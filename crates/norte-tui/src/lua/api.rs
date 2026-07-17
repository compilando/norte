//! `LuaHost`: estado Lua + registro de comandos/statusbar. Se reconstruye
//! ENTERO en hot-reload (jamás estado a medias); un comando en vuelo retiene
//! el estado viejo vía sus handles clonados (mlua es un handle Rc).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use mlua::{Function, Lua};
use norte_core::backend::Backend;

use super::fs::{self, PaneCtx, RunCancellers};

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
    /// Payload diagnóstico, NUNCA se pinta crudo: el caller lo enruta por
    /// una clave Fluent + `detail_for_bar` (patrón #73) para localizarlo y
    /// sanearlo antes de mostrarlo.
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

    /// Se llamó a [`LuaHost::eval_layer`] mientras otra carga ya estaba en
    /// curso en el mismo host. Hoy no hay forma de disparar esto desde Lua
    /// (ningún binding invoca `eval_layer` desde dentro del runtime), pero
    /// el guard existe para cuando `driver.rs` (task 5) lo exponga.
    #[error("eval_layer no es reentrante: ya hay una carga en curso")]
    Reentrant,
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
    // Rc: el driver (task 5) clona el handle del registry para invoke.
    registry: Rc<RefCell<Registry>>,
    /// Guard de reentrada: `true` mientras `eval_layer` está en curso.
    loading: Cell<bool>,
}

/// RAII: repone `loading` a `false` al salir de `eval_layer` por cualquier
/// vía (retorno normal o cualquiera de los `?` tempranos).
struct LoadingGuard<'a>(&'a Cell<bool>);

impl Drop for LoadingGuard<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
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

        Ok(Self {
            lua,
            registry,
            loading: Cell::new(false),
        })
    }

    /// Evalúa el código fuente de un `init.lua` como perteneciente a `layer`.
    ///
    /// Semántica:
    /// - NO es reentrante: si ya hay una carga en curso en este host,
    ///   devuelve [`LuaLoadError::Reentrant`] sin tocar nada.
    /// - Durante la evaluación, `norte.command(name, f)` registra en un
    ///   *staging* nuevo (no en el registro real); un nombre inválido o
    ///   duplicado EN EL MISMO STAGING es error inmediato.
    /// - Si la carga falla (sintaxis, runtime, o `norte.command` rechazó
    ///   algo), el registro real queda intacto — el staging se descarta.
    /// - Si la carga tiene éxito, el staging se fusiona con el registro
    ///   real nombre a nombre:
    ///   - Si el nombre no existía, o existía en una capa estrictamente
    ///     ANTERIOR, la nueva definición se instala (en el segundo caso con
    ///     un [`LuaWarning`] de pisado).
    ///   - Si existía en la MISMA capa (reload), se instala sin warning.
    ///   - Si existía en una capa estrictamente POSTERIOR (p. ej. se
    ///     re-evalúa `System` después de que `User` ya definiera el mismo
    ///     nombre), la nueva definición se IGNORA — la precedencia nunca se
    ///     invierte — y se emite un [`LuaWarning`] explicando el descarte.
    /// - Tras evaluar (éxito o error), la ranura global `norte.command`
    ///   vuelve a apuntar a una función que devuelve error si se llama fuera
    ///   de una carga. Además, la propia clausura de esta llamada queda
    ///   invalidada por una bandera compartida: si el script capturó una
    ///   referencia (`local c = norte.command`) y la invoca DESPUÉS de que
    ///   `eval_layer` retorne (p. ej. desde el cuerpo de un comando ya
    ///   registrado), la llamada falla igual — nunca escribe en un staging
    ///   huérfano.
    ///
    /// # Errors
    /// Cualquier error de sintaxis o runtime de Lua, incluyendo los que
    /// `norte.command` genera para nombres inválidos o duplicados, y
    /// [`LuaLoadError::Reentrant`] si ya hay una carga en curso.
    pub fn eval_layer(&self, source: &[u8], layer: Layer) -> Result<Vec<LuaWarning>, LuaLoadError> {
        if self.loading.get() {
            return Err(LuaLoadError::Reentrant);
        }
        self.loading.set(true);
        let _guard = LoadingGuard(&self.loading);

        let staging: Rc<RefCell<Vec<(String, Function)>>> = Rc::new(RefCell::new(Vec::new()));
        // Bandera de "sesión de carga activa": la clausura instalada abajo
        // la comprueba en cada llamada, no solo la ranura global. Así, una
        // referencia capturada por el script (`local c = norte.command`) y
        // invocada más tarde — p. ej. desde el cuerpo de un comando ya
        // registrado — muere con la carga igual que la ranura global.
        let active = Rc::new(Cell::new(true));

        let staging_for_closure = Rc::clone(&staging);
        let active_for_closure = Rc::clone(&active);
        let command_fn = self
            .lua
            .create_function(move |_lua, (name, f): (String, Function)| {
                if !active_for_closure.get() {
                    return Err(mlua::Error::RuntimeError(
                        "norte.command solo se puede llamar durante la carga de init.lua"
                            .to_string(),
                    ));
                }
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

        // Pase lo que pase: (1) la clausura de esta llamada deja de aceptar
        // comandos aunque conserve una referencia viva (Rc compartido); (2)
        // la ranura global `norte.command` vuelve a apuntar a una función
        // que rechaza cualquier llamada fuera de una carga en curso.
        //
        // OJO: construimos `restore` como un `Result` SIN propagarlo aquí
        // (nada de `?` en esta zona) para no enmascarar `exec_result` — si
        // ambos fallan, el error de la carga real es el que importa.
        active.set(false);
        let restore = self
            .lua
            .create_function(command_outside_load)
            .and_then(|f| norte.set("command", f));
        exec_result.map_err(LuaLoadError::Lua)?;
        restore.map_err(LuaLoadError::Lua)?;

        // INVARIANT: desde aquí hasta que se suelta `registry`, jamás se
        // llama a Lua (ni `exec`, ni se invoca una `Function`) — el borrow
        // mutable del registro debe quedar libre antes de volver a tocar el
        // runtime, o una reentrada lo encontraría prestado.
        let mut warnings = Vec::new();
        let mut registry = self.registry.borrow_mut();
        for (name, f) in staging.borrow_mut().drain(..) {
            match registry.commands.get(&name) {
                Some((prev_layer, _)) if *prev_layer > layer => {
                    // Una capa anterior (p. ej. System re-evaluada) no puede
                    // pisar a una posterior ya establecida (p. ej. User): la
                    // precedencia nunca se invierte. Se descarta con aviso.
                    warnings.push(LuaWarning {
                        detail: format!(
                            "comando {name} ignorado: ya definido por una capa posterior"
                        ),
                    });
                }
                Some((prev_layer, _)) if *prev_layer < layer => {
                    warnings.push(LuaWarning {
                        detail: format!("comando {name} redefinido por una capa posterior"),
                    });
                    registry.commands.insert(name, (layer, f));
                }
                // `None` (nombre nuevo) o misma capa (reload): instala sin
                // warning.
                _ => {
                    registry.commands.insert(name, (layer, f));
                }
            }
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

    /// SOLO para tests del crate: instala `norte.fs`/`norte.pane`/
    /// `norte.ui.message` con cancellers/messages frescos (sin driver ni
    /// token: nada cancela) y evalúa `src` como chunk async. El camino real
    /// de ejecución es `invoke` (task 5) — mismo `install_fs`, este helper
    /// solo ahorra el driver en los tests de bindings.
    ///
    /// Conversión del retorno del chunk: entero → ese `i64`; `true` → 1;
    /// nil/nada/otro → 0.
    ///
    /// # Errors
    /// Cualquier error de instalación de los bindings o de evaluación del
    /// chunk (sintaxis o runtime).
    #[doc(hidden)]
    pub async fn run_script_for_test(
        &self,
        backend: Backend,
        ctx: PaneCtx,
        src: &[u8],
    ) -> Result<i64, LuaLoadError> {
        let cancellers: RunCancellers = Rc::default();
        let messages: Rc<RefCell<Vec<String>>> = Rc::default();
        // El helper es un run completo: sus bindings se CIERRAN al terminar
        // (mismo contrato que `invoke`) — un stash desde aquí también muere.
        let closed: Rc<Cell<bool>> = Rc::default();
        fs::install_fs(
            &self.lua,
            backend,
            ctx,
            cancellers,
            messages,
            Rc::clone(&closed),
        )?;
        let result: mlua::Result<mlua::Value> = self.lua.load(src).eval_async().await;
        closed.set(true);
        Ok(match result? {
            mlua::Value::Integer(i) => i,
            mlua::Value::Boolean(true) => 1,
            _ => 0,
        })
    }

    /// Recupera la `Function` registrada bajo `name`, si existe. La usa el
    /// driver (`invoke`) — y los tests, para invocar una clausura capturada
    /// de una carga cerrada. El borrow del registro se SUELTA antes de
    /// devolver (invariant: jamás llamar a Lua con el registro prestado).
    pub(super) fn command_fn(&self, name: &str) -> Option<Function> {
        self.registry
            .borrow()
            .commands
            .get(name)
            .map(|(_, f)| f.clone())
    }

    /// Handle clonado del estado Lua (mlua es un handle `Rc` barato). Para
    /// el driver: hook de instrucciones + `install_fs` por invocación.
    pub(super) fn lua_handle(&self) -> Lua {
        self.lua.clone()
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

    #[test]
    fn referencia_capturada_al_staging_muere_con_la_carga() {
        let h = host();
        h.eval_layer(
            b"local c = norte.command\n\
              norte.command('trigger', function() c('ghost', function() end) end)",
            Layer::User,
        )
        .expect("carga ok");

        let trigger = h.command_fn("trigger").expect("trigger registrado");
        let result: mlua::Result<()> = trigger.call(());
        assert!(
            result.is_err(),
            "la referencia capturada a norte.command debe fallar tras cerrar la carga"
        );
        assert!(
            !h.commands().contains(&"ghost".to_string()),
            "no debe colarse en el registro"
        );
    }

    #[test]
    fn capa_anterior_reevaluada_no_pisa_a_la_posterior() {
        let h = host();
        h.eval_layer(b"norte.command('x', function() end)", Layer::User)
            .expect("user define x primero");
        let before = h.command_fn("x").expect("x registrado por User");

        let w = h
            .eval_layer(b"norte.command('x', function() end)", Layer::System)
            .expect("system se re-evalua despues, no es error de carga");
        assert_eq!(
            w.len(),
            1,
            "debe avisar de que la redefinicion de una capa anterior se ignora"
        );

        let after = h.command_fn("x").expect("x sigue registrado");
        assert_eq!(
            before, after,
            "la definicion de User (posterior) no puede ser pisada por System (anterior)"
        );
        assert_eq!(h.commands().len(), 1);
    }

    #[test]
    fn reload_de_la_misma_capa_pisa_sin_warning() {
        let h = host();
        h.eval_layer(b"norte.command('x', function() end)", Layer::User)
            .expect("primera carga");
        let w = h
            .eval_layer(b"norte.command('x', function() end)", Layer::User)
            .expect("reload de la misma capa");
        assert!(w.is_empty(), "recargar la misma capa no debe avisar");
        assert_eq!(h.commands().len(), 1);
    }

    #[test]
    fn norte_command_via_ranura_global_tras_la_carga_falla() {
        let h = host();
        h.eval_layer(
            b"norte.command('trigger2', function() norte.command('ghost2', function() end) end)",
            Layer::User,
        )
        .expect("carga ok");

        let trigger = h.command_fn("trigger2").expect("trigger2 registrado");
        let result: mlua::Result<()> = trigger.call(());
        assert!(
            result.is_err(),
            "norte.command (via ranura global) fuera de una carga debe fallar"
        );
        assert!(!h.commands().contains(&"ghost2".to_string()));
    }

    #[test]
    fn nombre_valido_con_charset_completo_y_longitud_maxima() {
        let h = host();
        // Cubre minuscula, digito, '.', '_' y '-'; exactamente 64 bytes.
        let name: String = "a.b_c-9".chars().cycle().take(64).collect();
        assert_eq!(name.len(), 64);

        let src = format!("norte.command('{name}', function() end)");
        let w = h
            .eval_layer(src.as_bytes(), Layer::User)
            .expect("charset completo y longitud 64 son validos");
        assert!(w.is_empty());
        assert!(h.commands().contains(&name));
    }

    #[test]
    fn eval_layer_no_es_reentrante() {
        let h = host();
        // No hay hoy binding que dispare esto desde dentro de Lua; se
        // fuerza el estado directamente para probar el guard en sí.
        h.loading.set(true);
        let err = h.eval_layer(b"norte.command('x', function() end)", Layer::User);
        assert!(matches!(err, Err(LuaLoadError::Reentrant)));
        h.loading.set(false);
    }
}
