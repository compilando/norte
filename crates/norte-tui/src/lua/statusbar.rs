//! Hook de statusbar (M4 Lua, task 7): `norte.ui.statusbar(fn)` deja que un
//! `init.lua` pinte la barra de estado. A diferencia de un comando (task 5),
//! esta llamada es SÍNCRONA y se dispara en CADA vuelta de render — un bucle
//! infinito bloquearía el TUI entero — así que el presupuesto de
//! instrucciones no CEDE (yield, como el de un comando): al agotarse ABORTA
//! la llamada (`Err`) y `LuaHost::statusbar` deshabilita el hook para el
//! resto de la vida de este host (se re-habilita solo con un `LuaHost`
//! nuevo, hot-reload, task 8).
//!
//! Contexto heredado de T5 (spec de la task): los hooks de mlua son POR
//! THREAD. `Lua::set_hook` instala en el estado PRINCIPAL; `Function::call`
//! SÍNCRONO corre en ese mismo estado (a diferencia de `call_async`, que
//! corre en una corrutina propia) — por eso el hook del estado principal SÍ
//! dispara aquí. `driver.rs` usa `Thread::set_hook` en la corrutina de cada
//! comando; ambos hooks son independientes (thread distinto) y no
//! interfieren entre sí.

use mlua::{Function, HookTriggers, Lua};

/// Presupuesto de instrucciones de una llamada al hook de statusbar. Se
/// dispara en CADA vuelta de render (a diferencia de un comando, que corre
/// una vez): un tope bajo evita que un script lento se perciba como
/// congelación antes de deshabilitarse.
const STATUSBAR_BUDGET: u32 = 50_000;

/// Snapshot del estado visible para el hook. `PartialEq` es la clave del
/// cache de [`super::LuaHost::statusbar`]: dos snapshots iguales no
/// reinvocan el script.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusInput {
    /// Bytes wire del cwd del pane con foco.
    pub cwd: Vec<u8>,
    /// Índice de la entrada bajo el cursor.
    pub selected: usize,
    /// Bytes totales de la selección marcada.
    pub selected_bytes: u64,
    /// Número de entradas del listado actual.
    pub entries: usize,
    /// Tareas en vuelo.
    pub tasks: usize,
}

/// RAII: `remove_hook` SIEMPRE al salir (retorno normal o `?` temprano). El
/// hook vive en una ranura del estado Lua COMPARTIDA con el resto del host
/// (comandos incluidos, `driver.rs`); dejarlo puesto tras un error o un
/// panic-catch contaminaría cualquier llamada síncrona posterior sobre el
/// mismo estado.
struct HookGuard<'a>(&'a Lua);

impl Drop for HookGuard<'_> {
    fn drop(&mut self) {
        self.0.remove_hook();
    }
}

/// Invoca `f` con el snapshot `input` bajo presupuesto de instrucciones.
/// SÍNCRONA a propósito: el hook de statusbar es una llamada bloqueante
/// corta del hilo de render, no un comando de fondo (eso es `driver.rs`).
///
/// El valor de retorno es el string CRUDO del script, sin sanear — el
/// caller ([`super::LuaHost::statusbar`]) lo pasa por
/// `crate::app::detail_for_bar` (enmascara bidi/control + tope de longitud)
/// antes de mostrarlo.
///
/// # Errors
/// Presupuesto agotado, error de runtime del script (incluyendo que el
/// script no haya devuelto algo coercionable a string), o cualquier intento
/// de ceder el control (p. ej. llamar una API async desde este contexto
/// síncrono produce un error de mlua) — todos indistinguibles para el
/// caller: cualquiera deshabilita el hook.
pub(super) fn call_hook(lua: &Lua, f: &Function, input: &StatusInput) -> mlua::Result<String> {
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(STATUSBAR_BUDGET),
        |_, _| {
            Err(mlua::Error::RuntimeError(
                "statusbar: presupuesto de instrucciones agotado".to_string(),
            ))
        },
    );
    // Guard ANTES de cualquier operación falible: si `create_table`/`set`
    // fallan (sin memoria; no ocurre en la práctica) o `f.call` erra, el
    // hook se quita igual al salir por el `?` temprano.
    let _guard = HookGuard(lua);

    let table = lua.create_table()?;
    table.set("cwd", lua.create_string(&input.cwd)?)?;
    table.set("selected", input.selected)?;
    table.set("selected_bytes", input.selected_bytes)?;
    table.set("entries", input.entries)?;
    table.set("tasks", input.tasks)?;

    let out: mlua::String = f.call(table)?;
    Ok(String::from_utf8_lossy(&out.as_bytes()).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lua::{Layer, LuaHost};

    fn input() -> StatusInput {
        StatusInput {
            cwd: b"mem:///d".to_vec(),
            selected: 2,
            selected_bytes: 10,
            entries: 5,
            tasks: 0,
        }
    }

    #[test]
    fn hook_pinta_y_cachea() {
        let h = LuaHost::new().unwrap();
        h.eval_layer(
            b"norte.ui.statusbar(function(s) return s.selected .. ' sel' end)",
            Layer::User,
        )
        .unwrap();
        assert_eq!(h.statusbar(&input()).as_deref(), Some("2 sel"));

        // Mismo input = cache (se comprueba que una función con contador
        // global de Lua solo corre una vez para el mismo snapshot).
        let h2 = LuaHost::new().unwrap();
        h2.eval_layer(
            b"n = 0; norte.ui.statusbar(function(s) n = n + 1 return tostring(n) end)",
            Layer::User,
        )
        .unwrap();
        assert_eq!(h2.statusbar(&input()).as_deref(), Some("1"));
        assert_eq!(h2.statusbar(&input()).as_deref(), Some("1"), "cacheado");
    }

    #[test]
    fn presupuesto_excedido_deshabilita_sin_panic() {
        let h = LuaHost::new().unwrap();
        h.eval_layer(
            b"norte.ui.statusbar(function() while true do end end)",
            Layer::User,
        )
        .unwrap();
        assert_eq!(
            h.statusbar(&input()),
            None,
            "excedido -> None + deshabilitado"
        );
        assert_eq!(h.statusbar(&input()), None, "sigue deshabilitado");
        assert!(
            h.statusbar_error().is_some(),
            "el error queda para la barra"
        );
    }

    #[test]
    fn salida_hostil_enmascarada() {
        let h = LuaHost::new().unwrap();
        h.eval_layer(
            "norte.ui.statusbar(function() return 'a\u{202E}b' end)".as_bytes(),
            Layer::User,
        )
        .unwrap();
        let s = h.statusbar(&input()).unwrap();
        assert!(!s.contains('\u{202E}'), "sin bidi: {s}");
    }

    /// Regresión: un `eval_layer` posterior sobre un host YA VIVO (p. ej. la
    /// capa `Project`, evaluada tras resolver el modal TOFU — task 8 — en un
    /// host que ya venía sirviendo `statusbar()` con las capas
    /// `System`/`User`) que redefine el hook NO debe servir la respuesta
    /// cacheada del hook viejo si el `StatusInput` no cambió entretanto.
    #[test]
    fn redefinir_el_hook_invalida_el_cache_previo() {
        let h = LuaHost::new().unwrap();
        h.eval_layer(
            b"norte.ui.statusbar(function() return 'v1' end)",
            Layer::User,
        )
        .unwrap();
        assert_eq!(h.statusbar(&input()).as_deref(), Some("v1"));

        h.eval_layer(
            b"norte.ui.statusbar(function() return 'v2' end)",
            Layer::Project,
        )
        .unwrap();
        assert_eq!(
            h.statusbar(&input()).as_deref(),
            Some("v2"),
            "el cache del hook viejo no debe sobrevivir a la redefinicion"
        );
    }
}
