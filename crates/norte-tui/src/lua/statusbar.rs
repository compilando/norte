//! Hook de statusbar (M4 Lua, task 7): `norte.ui.statusbar(fn)` deja que un
//! `init.lua` pinte la barra de estado. A diferencia de un comando (task 5),
//! esta llamada es SÍNCRONA y se dispara en CADA vuelta de render — un bucle
//! infinito bloquearía el TUI entero — así que el presupuesto de
//! instrucciones no CEDE (yield, como el de un comando): al agotarse ABORTA
//! la llamada (`Err`) y `LuaHost::statusbar` deshabilita el hook para el
//! resto de la vida de este host (se re-habilita solo con un `LuaHost`
//! nuevo, hot-reload, task 8).
//!
//! Contexto heredado de T5 (spec de la task, corregido tras revisión — ver
//! `docs/superpowers/plans/2026-07-17-m4-lua-scripting.md` T7): `Lua::set_hook`
//! instala en el estado PRINCIPAL; `Function::call` SÍNCRONO corre en ese
//! mismo estado (a diferencia de `call_async`, que corre en una corrutina
//! propia) — por eso el hook del estado principal SÍ dispara aquí.
//!
//! **CORRECCIÓN (spec-reviewer, reproducido fuera del repo con mlua 0.10.5):**
//! la afirmación original de que el hook del driver («por thread») y el de
//! esta barra («estado principal») son independientes es FALSA. mlua guarda
//! `hook_callback`/`hook_thread` en una única ranura de `ExtraData`
//! COMPARTIDA por el estado principal y TODAS las corrutinas — la propia
//! doc de mlua lo dice: «cannot have more than one hook function set at a
//! time». El trampolín en C (`hook_proc`, `state/raw.rs::set_thread_hook`)
//! comprueba `hook_thread == state` en cada disparo; si no coincide, se
//! autodesarma (`lua_sethook(state, None, 0, 0)`) SIN llamar al callback.
//! Si esta barra llama `Lua::set_hook`/`remove_hook` mientras el driver
//! tiene una corrutina viva con su propio `Thread::set_hook` (cancelación,
//! regla 3), la próxima vez que el hook del driver dispare se encuentra con
//! `hook_thread` apuntando a OTRO estado y se apaga solo — un bucle Lua puro
//! en vuelo queda INCANCELABLE (ni token ni timeout lo matan; el driver
//! tendría que esperar el timeout duro y ABANDONAR el future).
//!
//! Por eso `LuaHost` lleva `run_active: Rc<Cell<bool>>` (compartido con
//! `driver.rs`: `invoke_with_timeout` lo enciende al arrancar, `RunGuard` lo
//! apaga en su `Drop`, TODOS los caminos incluido el abandono). Mientras
//! `run_active` es `true`, [`super::LuaHost::statusbar`] JAMÁS llama a esta
//! función — ni de lejos toca `set_hook`/`remove_hook` — devuelve el valor
//! cacheado si el `StatusInput` coincide o `None` si no: la barra se
//! CONGELA durante un comando Lua, a cambio de no poder desarmar jamás la
//! cancelación de un run en vuelo.

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
///
/// # Invariante del caller
/// [`super::LuaHost::statusbar`] NUNCA llama a esta función mientras
/// `run_active` esté encendido (un comando Lua en vuelo): la ranura de hook
/// de mlua es ÚNICA por instancia — compartida entre el estado principal y
/// TODAS las corrutinas — y `set_hook`/`remove_hook` aquí desarmaría en
/// silencio el hook de cancelación del driver (ver el módulo).
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

    // ---- Regresión spec-reviewer (task 7, ranura de hook única) --------

    /// Setup compartido de las regresiones `run_active`: un `Backend`
    /// embebido sobre `MemProvider` con `mem:///a` escrito, listo para un
    /// `copy` que se puede volver lento con `faults().set_latency_per_op`
    /// (mismo patrón que `tests/lua_driver.rs::cancelar_mata_el_script_y_sus_tasks`).
    async fn backend_con_origen() -> (
        norte_core::backend::Backend,
        std::sync::Arc<norte_testkit::MemProvider>,
    ) {
        use norte_vfs::Provider;
        let engine = norte_core::Engine::new();
        let mem = std::sync::Arc::new(norte_testkit::MemProvider::new());
        engine.register_provider(std::sync::Arc::clone(&mem) as std::sync::Arc<dyn Provider>);
        let vp = norte_proto::VPath::parse("mem:///a").expect("wire");
        let mut sink = mem.write(&vp).await.unwrap();
        sink.write(bytes::Bytes::from_static(b"x")).await.unwrap();
        sink.commit().await.unwrap();
        (
            norte_core::backend::Backend::Embedded(std::sync::Arc::new(engine)),
            mem,
        )
    }

    fn ctx_mem() -> crate::lua::PaneCtx {
        let vp = |w: &str| norte_proto::VPath::parse(w).expect("wire");
        crate::lua::PaneCtx {
            cwd: vp("mem:///"),
            other_cwd: vp("mem:///"),
            selection: vec![],
            current: None,
        }
    }

    /// Regresión: `statusbar()` con un comando en vuelo (latencia inyectada
    /// en la copia — la Task NO ha terminado, el run está de verdad "en
    /// vuelo", no resuelto en microsegundos) NUNCA debe ejecutar el hook: un
    /// contador global de Lua debe seguir en `0` (visto por el `None`, ya
    /// que sin cache previo la única salida honesta con un run vivo es
    /// `None`). Al terminar el run, `statusbar()` vuelve a funcionar y
    /// ejecuta el hook de verdad.
    ///
    /// Sin el guard `run_active` este test es rojo: nada impide que
    /// `statusbar()` llame a Lua durante el run, así que devolvería
    /// `Some("1")` en vez de `None` (verificado manualmente quitando el
    /// guard).
    #[tokio::test]
    async fn statusbar_no_toca_lua_con_run_en_vuelo() {
        let (backend, mem) = backend_con_origen().await;
        mem.faults()
            .set_latency_per_op(Some(std::time::Duration::from_millis(200)));

        let h = LuaHost::new().unwrap();
        h.eval_layer(
            b"n = 0\n\
              norte.ui.statusbar(function() n = n + 1 return tostring(n) end)\n\
              norte.command('copia', function()\n\
                norte.fs.copy('mem:///a', 'mem:///b')\n\
              end)",
            Layer::User,
        )
        .unwrap();

        let run = h
            .invoke(
                "copia",
                backend.clone(),
                ctx_mem(),
                tokio_util::sync::CancellationToken::new(),
            )
            .expect("existe");

        let probe = async {
            // Deja que la copia arranque de verdad (entre en la latencia
            // inyectada) antes de sondear la barra.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            assert_eq!(
                h.statusbar(&input()),
                None,
                "run en vuelo: jamás debe ejecutar el hook (ni tocar Lua)"
            );
        };

        let (outcome, ()) = tokio::join!(run, probe);
        assert!(
            matches!(outcome, crate::lua::RunOutcome::Ok { .. }),
            "{outcome:?}"
        );

        // Tras terminar el run: statusbar() vuelve a ejecutar de verdad (el
        // contador avanza a 1 — es la PRIMERA ejecución real).
        assert_eq!(h.statusbar(&input()).as_deref(), Some("1"));
    }

    /// Reproducción del reviewer: un comando con una copia lenta seguida de
    /// un bucle Lua puro; SI, mientras el run está en vuelo, algo llamara
    /// `Lua::set_hook`/`remove_hook` (statusbar SIN el guard), el hook de
    /// cancelación del driver quedaría desarmado en silencio (ranura de
    /// hook única de mlua) y el bucle sería INCANCELABLE — el test usa un
    /// timeout corto (`invoke_with_timeout`) para que, si el bug reaparece,
    /// falle rápido con `TimedOut` en vez de agotar el timeout por defecto
    /// (5 min).
    ///
    /// Verificado en rojo quitando temporalmente el `if self.run_active.get()`
    /// de `LuaHost::statusbar`: el outcome pasa a `TimedOut` (la cancelación
    /// no llega a tiempo porque el hook del driver quedó inerte).
    #[tokio::test]
    async fn cancelar_sigue_funcionando_tras_statusbar_durante_run() {
        let (backend, mem) = backend_con_origen().await;
        mem.faults()
            .set_latency_per_op(Some(std::time::Duration::from_millis(200)));

        let h = LuaHost::new().unwrap();
        h.eval_layer(
            b"norte.ui.statusbar(function() return 'ok' end)\n\
              norte.command('loop', function()\n\
                local ok = norte.fs.copy('mem:///a', 'mem:///b')\n\
                while true do end\n\
              end)",
            Layer::User,
        )
        .unwrap();
        // Un hook DE VERDAD registrado: sin esto, `statusbar()` corta camino
        // en `registry.statusbar.clone()?` (None) y NUNCA llega a tocar
        // `Lua::set_hook`/`remove_hook` — la interferencia que este test
        // reproduce exige que la barra intente ejecutar el hook de verdad.
        assert!(
            h.statusbar(&input()).is_some(),
            "precondición: el hook existe y corre en frío (sin run en vuelo)"
        );
        // Input DISTINTO al de la precondición: si coincidiera, el cache de
        // la línea de arriba serviría la respuesta sin tocar Lua durante el
        // run, y este test no probaría nada — necesitamos que el intento de
        // ejecutar el hook sea REAL.
        let during_input = StatusInput {
            selected: 99,
            ..input()
        };

        let token = tokio_util::sync::CancellationToken::new();
        let run = h
            .invoke_with_timeout(
                "loop",
                backend.clone(),
                ctx_mem(),
                token.clone(),
                std::time::Duration::from_secs(2),
            )
            .expect("existe");

        let cancel = async {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            // El punto del reviewer: llamar a statusbar() MIENTRAS el run
            // está en vuelo, con un input SIN cache (fuerza el intento real
            // de ejecutar el hook). Con el guard, no-op (`None`, sin tocar
            // Lua); es justo lo que este test verifica indirectamente vía
            // el outcome de la cancelación de abajo.
            assert_eq!(
                h.statusbar(&during_input),
                None,
                "no debe tocar Lua con el run en vuelo"
            );
            token.cancel();
        };

        let (outcome, ()) = tokio::join!(run, cancel);
        assert!(
            matches!(outcome, crate::lua::RunOutcome::Cancelled),
            "{outcome:?}"
        );
    }

    /// El hook de statusbar llamando a una API que solo existe DURANTE un
    /// run (`norte.fs`, instalada por `install_fs` en cada `invoke`, task
    /// 4/5) revienta y deshabilita el hook. Con el guard `run_active` de
    /// este fix, el caso "el hook llama de verdad a `norte.fs.stat` mientras
    /// hay un run en vuelo" es INALCANZABLE: `statusbar()` nunca ejecuta el
    /// hook con un run vivo, así que `norte.fs` JAMÁS está instalado cuando
    /// el hook corre. Lo que SÍ es alcanzable — y es lo que prueba este
    /// test — es el camino de error genérico: en el estado normal (sin run)
    /// `norte.fs` no existe, así que la llamada revienta por indexar `nil`
    /// — un mensaje de error distinto al de una llamada async real, pero
    /// con el MISMO desenlace (deshabilita + `statusbar_error()` con Some)
    /// que cualquier otro fallo del hook.
    #[test]
    fn fs_async_desde_el_hook_deshabilita() {
        let h = LuaHost::new().unwrap();
        h.eval_layer(
            b"norte.ui.statusbar(function() return norte.fs.stat('mem:///x') end)",
            Layer::User,
        )
        .unwrap();
        assert_eq!(
            h.statusbar(&input()),
            None,
            "norte.fs no existe fuera de un run -> revienta -> deshabilitado"
        );
        assert!(
            h.statusbar_error().is_some(),
            "el detalle del fallo queda para la barra"
        );
    }

    /// Regresión (re-review de la task 7): si el caller (T8 o cualquier
    /// futuro) crea un `CommandRun` y lo dropea SIN pollearlo ni una vez, el
    /// cuerpo de la `async fn run_command` JAMÁS empieza a ejecutarse — así
    /// que ningún guard interno de esa función corre nunca. Si `run_active`
    /// dependiera de un guard construido DENTRO del future, quedaría
    /// atascado en `true` para siempre (la barra congelada hasta el próximo
    /// hot-reload). `CommandRun` debe apagarlo estructuralmente en su propio
    /// `Drop`, sin depender de que se pollee.
    #[tokio::test]
    async fn command_run_dropeado_sin_pollear_no_congela_la_statusbar() {
        let (backend, _mem) = backend_con_origen().await;
        let h = LuaHost::new().unwrap();
        h.eval_layer(
            b"norte.ui.statusbar(function() return 'ok' end)\n\
              norte.command('loop', function() while true do end end)",
            Layer::User,
        )
        .unwrap();

        let run = h
            .invoke(
                "loop",
                backend,
                ctx_mem(),
                tokio_util::sync::CancellationToken::new(),
            )
            .expect("existe");
        drop(run); // JAMÁS polleado: el cuerpo de la async fn nunca corrió.

        assert_eq!(
            h.statusbar(&input()).as_deref(),
            Some("ok"),
            "un CommandRun dropeado sin pollear no debe dejar run_active atascado"
        );
    }
}
