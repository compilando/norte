//! `LuaHost`: estado Lua + registro de comandos/statusbar. Se reconstruye
//! ENTERO en hot-reload (jamás estado a medias); un comando en vuelo retiene
//! el estado viejo vía sus handles clonados (mlua es un handle Rc).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use mlua::{Function, Lua};

/// Capa de origen de un `init.lua` (precedencia ASCENDENTE, ADR 0007).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    /// `/etc/norte` (o `ProgramData`).
    System,
    /// `~/.config/norte`.
    User,
    /// `./.norte` — SOLO tras trust (ADR 0026).
    Project,
}

/// Aviso no-fatal de carga (se muestra por barra, no aborta).
#[derive(Debug, Clone)]
pub struct LuaWarning {
    /// Detalle legible (el caller lo sanea antes de pintarlo).
    pub detail: String,
}

/// Error al cargar o evaluar una capa de `init.lua`.
#[derive(Debug, thiserror::Error)]
pub enum LuaLoadError {
    /// Error propagado tal cual desde el runtime Lua (sintaxis, runtime,
    /// nombre de comando inválido/duplicado — todos viajan como
    /// `mlua::Error::RuntimeError` desde `norte.command`).
    #[error(transparent)]
    Lua(#[from] mlua::Error),
}

/// Comandos ya confirmados en el registro (capa que los definió + la
/// función Lua invocable).
#[derive(Default)]
struct Registry {
    commands: HashMap<String, (Layer, Function)>,
}

/// El anfitrión Lua del TUI. `!Send` — vive en el main task.
pub struct LuaHost {
    lua: Lua,
    registry: Rc<RefCell<Registry>>,
}

/// Charset de nombres de comando (mismo espíritu que `agent_session`):
/// `[a-z0-9._-]{1,64}`.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

/// Función instalada como `norte.command` fuera de una carga en curso: no
/// hay staging activo, así que cualquier intento de registrar un comando
/// desde fuera de `eval_layer` es un error explícito (evita el bug sutil de
/// dejar el staging viejo capturado tras un `exec()` que falló).
fn command_outside_load(_lua: &Lua, _args: (String, Function)) -> mlua::Result<()> {
    Err(mlua::Error::RuntimeError(
        "norte.command solo se puede llamar durante la carga de init.lua".to_string(),
    ))
}

impl LuaHost {
    /// Crea un anfitrión nuevo: stdlib Lua completa (ADR 0026, sin sandbox —
    /// es config de usuario, no software de terceros) + tabla `norte` con
    /// subtabla `ui` vacía en globals.
    ///
    /// # Errors
    /// Si mlua falla al inicializar el estado o instalar las tablas base.
    pub fn new() -> mlua::Result<Self> {
        let lua = Lua::new();
        let registry = Rc::new(RefCell::new(Registry::default()));

        let norte = lua.create_table()?;
        let ui = lua.create_table()?;
        norte.set("ui", ui)?;
        norte.set("command", lua.create_function(command_outside_load)?)?;
        lua.globals().set("norte", norte)?;

        Ok(Self { lua, registry })
    }

    /// Evalúa el código fuente de un `init.lua` como perteneciente a `layer`.
    ///
    /// Semántica:
    /// - Durante la evaluación, `norte.command(name, f)` registra en un
    ///   *staging* nuevo (no en el registro real); un nombre inválido o
    ///   duplicado EN EL MISMO STAGING es error inmediato.
    /// - Si la carga falla (sintaxis, runtime, o `norte.command` rechazó
    ///   algo), el registro real queda intacto — el staging se descarta.
    /// - Si la carga tiene éxito, el staging se fusiona con el registro
    ///   real: una capa posterior siempre gana; si pisa un comando de una
    ///   capa estrictamente anterior se emite un [`LuaWarning`]. Re-evaluar
    ///   la MISMA capa (reload) pisa sin warning.
    /// - Tras evaluar (éxito o error), `norte.command` vuelve a apuntar a
    ///   una función que devuelve error si se llama fuera de una carga —
    ///   nunca queda capturado el staging de esta llamada.
    ///
    /// # Errors
    /// Cualquier error de sintaxis o runtime de Lua, incluyendo los que
    /// `norte.command` genera para nombres inválidos o duplicados.
    pub fn eval_layer(&self, source: &[u8], layer: Layer) -> Result<Vec<LuaWarning>, LuaLoadError> {
        let staging: Rc<RefCell<Vec<(String, Function)>>> = Rc::new(RefCell::new(Vec::new()));

        let staging_for_closure = Rc::clone(&staging);
        let command_fn = self
            .lua
            .create_function(move |_lua, (name, f): (String, Function)| {
                if !valid_name(&name) {
                    return Err(mlua::Error::RuntimeError(format!(
                        "nombre de comando inválido: {name:?} (esperado [a-z0-9._-]{{1,64}})"
                    )));
                }
                let mut staging = staging_for_closure.borrow_mut();
                if staging.iter().any(|(n, _)| n == &name) {
                    return Err(mlua::Error::RuntimeError(format!(
                        "comando {name} duplicado en la misma capa"
                    )));
                }
                staging.push((name, f));
                Ok(())
            })
            .map_err(LuaLoadError::Lua)?;

        let norte: mlua::Table = self.lua.globals().get("norte").map_err(LuaLoadError::Lua)?;
        norte
            .set("command", command_fn)
            .map_err(LuaLoadError::Lua)?;

        let layer_name = match layer {
            Layer::System => "init.lua (sistema)",
            Layer::User => "init.lua (usuario)",
            Layer::Project => "init.lua (proyecto)",
        };
        let exec_result = self.lua.load(source).set_name(layer_name).exec();

        // Pase lo que pase, `norte.command` deja de apuntar a este staging:
        // fuera de una carga en curso siempre es un error explícito.
        let restore = norte.set(
            "command",
            self.lua
                .create_function(command_outside_load)
                .map_err(LuaLoadError::Lua)?,
        );
        exec_result.map_err(LuaLoadError::Lua)?;
        restore.map_err(LuaLoadError::Lua)?;

        let mut warnings = Vec::new();
        let mut registry = self.registry.borrow_mut();
        for (name, f) in staging.borrow_mut().drain(..) {
            if let Some((prev_layer, _)) = registry.commands.get(&name)
                && *prev_layer < layer
            {
                warnings.push(LuaWarning {
                    detail: format!("comando {name} redefinido por una capa posterior"),
                });
            }
            registry.commands.insert(name, (layer, f));
        }

        Ok(warnings)
    }

    /// Nombres de los comandos registrados, ordenados.
    #[must_use]
    pub fn commands(&self) -> Vec<String> {
        let registry = self.registry.borrow();
        let mut names: Vec<String> = registry.commands.keys().cloned().collect();
        names.sort();
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> LuaHost {
        LuaHost::new().expect("lua arranca")
    }

    #[test]
    fn registra_y_lista_comandos() {
        let h = host();
        let w = h
            .eval_layer(b"norte.command('sel-up', function() end)", Layer::User)
            .expect("eval");
        assert!(w.is_empty());
        assert_eq!(h.commands(), vec!["sel-up".to_string()]);
    }

    #[test]
    fn nombre_invalido_es_error_de_carga() {
        let h = host();
        // Mayúsculas, espacios, vacío, >64: fuera (charset [a-z0-9._-]{1,64}).
        for bad in [
            "'Mal'",
            "'con espacio'",
            "''",
            &format!("'{}'", "a".repeat(65)),
        ] {
            let src = format!("norte.command({bad}, function() end)");
            assert!(h.eval_layer(src.as_bytes(), Layer::User).is_err(), "{bad}");
        }
    }

    #[test]
    fn duplicado_en_la_misma_capa_es_error() {
        let h = host();
        let src = b"norte.command('x', function() end)\nnorte.command('x', function() end)";
        assert!(h.eval_layer(src, Layer::User).is_err());
    }

    #[test]
    fn capa_posterior_pisa_con_warning() {
        let h = host();
        h.eval_layer(b"norte.command('x', function() end)", Layer::System)
            .expect("sistema");
        let w = h
            .eval_layer(b"norte.command('x', function() end)", Layer::User)
            .expect("usuario");
        assert_eq!(w.len(), 1, "warning de pisado");
        assert_eq!(h.commands().len(), 1);
    }

    #[test]
    fn un_error_de_evaluacion_no_envenena_el_host() {
        let h = host();
        assert!(h.eval_layer(b"esto no es lua (", Layer::System).is_err());
        h.eval_layer(b"norte.command('ok', function() end)", Layer::User)
            .expect("la capa siguiente carga");
        assert_eq!(h.commands(), vec!["ok".to_string()]);
    }
}
