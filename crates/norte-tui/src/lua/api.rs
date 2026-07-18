//! `LuaHost`: estado Lua + registro de comandos/statusbar. Se reconstruye
//! ENTERO en hot-reload (jamás estado a medias); un comando en vuelo retiene
//! el estado viejo vía sus handles clonados (mlua es un handle Rc).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use mlua::{Function, Lua};
use norte_core::backend::Backend;

use super::fs::{self, PaneCtx, RunCancellers};
use super::statusbar::{self, StatusInput};

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

    /// Se llamó a [`LuaHost::eval_layer`] con algún `CommandRun` de ESTE
    /// host en vuelo: la carga corre bajo un hook de presupuesto y la
    /// ranura de hook de mlua es ÚNICA por instancia — instalarlo pisaría
    /// en silencio el hook de cancelación de la corrutina del run (ver el
    /// módulo `statusbar`). Fail-closed: mejor rechazar la carga (visible)
    /// que un run incancelable.
    #[error("eval_layer con un run en vuelo: la carga pisaría el hook de cancelación")]
    RunInFlight,
}

/// Comandos ya confirmados en el registro (capa que los definió + la
/// función Lua invocable).
#[derive(Default)]
struct Registry {
    commands: HashMap<String, (Layer, Function)>,
    /// Hook de `norte.ui.statusbar`, si algún `init.lua` lo definió (task 7).
    /// A diferencia de `commands`, NO lleva capa: la semántica es «último
    /// eval que lo definió gana», sin precedencia de capa ni warning de
    /// pisado (documentado en `eval_layer`).
    statusbar: Option<Function>,
}

/// El anfitrión Lua del TUI. `!Send` — vive en el main task.
pub struct LuaHost {
    lua: Lua,
    // Rc: el driver (task 5) clona el handle del registry para invoke.
    registry: Rc<RefCell<Registry>>,
    /// Guard de reentrada: `true` mientras `eval_layer` está en curso.
    loading: Cell<bool>,
    /// `true` tras un fallo del hook de statusbar (presupuesto agotado o
    /// error de runtime): `statusbar()` devuelve `None` sin tocar Lua hasta
    /// que este host se reconstruya entero (hot-reload, task 8).
    statusbar_disabled: Cell<bool>,
    /// Detalle diagnóstico CRUDO del último fallo del hook (task 7): el
    /// wiring (task 8) lo consume UNA VEZ vía `statusbar_error()` para
    /// pintarlo en la barra saneado.
    statusbar_error: RefCell<Option<String>>,
    /// Cache de la última invocación: mismo `StatusInput` (`PartialEq`) =
    /// misma salida, sin reinvocar el script.
    statusbar_cache: RefCell<Option<(StatusInput, Option<String>)>>,
    /// Contador de comandos (`driver.rs`) en vuelo — CONTADOR, no booleano
    /// (spec-review 3): `invoke_with_timeout` lo INCREMENTA al construir el
    /// `CommandRun` devuelto (en AMBOS caminos, también el de error de
    /// `install_fs`, por simetría con el decremento) y el `Drop` de
    /// `CommandRun` lo DECREMENTA saturante (todo camino: retorno normal,
    /// cancelación o abandono por timeout, o ni siquiera pollearlo nunca).
    ///
    /// Un booleano NO basta: el patrón natural del caller (T8) `self.run =
    /// Some(host.invoke(...))` evalúa el run NUEVO (que enciende la
    /// protección) ANTES de dropear el run VIEJO que estaba en el slot (que
    /// la apagaría) — con un booleano, ese Drop del viejo pisaría el `true`
    /// recién puesto por el nuevo, dejándolo desprotegido. Con un contador,
    /// el incremento del nuevo y el decremento del viejo se compensan: solo
    /// llega a cero cuando NINGÚN `CommandRun` (viejo o nuevo) sigue vivo.
    ///
    /// Compartido por `Rc` con `driver.rs` (ver [`Self::run_active_handle`]):
    /// la ranura de hook de mlua (`ExtraData::hook_callback`/`hook_thread`)
    /// es ÚNICA por instancia — compartida entre el estado principal y TODAS
    /// las corrutinas, pese a que la API expone `Lua::set_hook` y
    /// `Thread::set_hook` como si fueran independientes. Mientras algún run
    /// esté en vuelo, su corrutina tiene un `Thread::set_hook` propio armado
    /// para cancelación (regla 3); si `statusbar()` llamara
    /// `Lua::set_hook`/`remove_hook` encima, el trampolín en C del driver se
    /// autodesarmaría en silencio la próxima vez que disparase (mismatch de
    /// `hook_thread`) — un bucle Lua puro en vuelo quedaría INCANCELABLE.
    /// Ver el módulo `statusbar` para el detalle completo.
    run_active: Rc<Cell<u32>>,
}

/// RAII: repone `loading` a `false` al salir de `eval_layer` por cualquier
/// vía (retorno normal o cualquiera de los `?` tempranos).
struct LoadingGuard<'a>(&'a Cell<bool>);

impl Drop for LoadingGuard<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

/// Presupuesto de instrucciones de UNA carga (`eval_layer`, rust-review
/// T8): un `init.lua` roto (`while true do end` en el top-level) NO puede
/// congelar el run loop del TUI (sin draw, sin Esc, terminal en raw mode al
/// matar el proceso). 10 M instrucciones es DELIBERADAMENTE generoso: una
/// carga legítima define comandos y poco más (miles de instrucciones, no
/// millones) — ni un init.lua barroco lo roza, y en hardware actual se
/// agota en decenas de ms, no en segundos.
const EVAL_BUDGET: u32 = 10_000_000;

/// Charset de nombres de comando (mismo espíritu que `agent_session`):
/// `[a-z0-9._-]{1,64}`. `pub(crate)`: el keymap (T8) valida con ESTA misma
/// función los bindings `lua:<nombre>` — una sola fuente, cero deriva.
pub(crate) fn valid_name(name: &str) -> bool {
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

/// Función instalada como `norte.ui.statusbar` fuera de una carga en curso:
/// mismo espíritu que [`command_outside_load`] — sin staging activo, un
/// intento de registrar el hook fuera de `eval_layer` es un error explícito.
fn statusbar_outside_load(_lua: &Lua, _f: Function) -> mlua::Result<()> {
    Err(mlua::Error::RuntimeError(
        "norte.ui.statusbar solo se puede llamar durante la carga de init.lua".to_string(),
    ))
}

/// Instala el staging temporal de `norte.ui.statusbar` para ESTA carga
/// (colgado de `ui`, mismo flag `active` que `norte.command` — una
/// referencia capturada por el script muere con la carga igual que él).
///
/// A diferencia de `norte.command`, redefinir el hook VARIAS veces dentro de
/// la MISMA carga no es un error: la última llamada dentro del staging
/// gana (un script puede reasignar su propio hook a placer mientras se
/// evalúa). El staging devuelto se fusiona con el registro real al final de
/// `eval_layer`, solo si la carga tuvo éxito.
fn stage_statusbar(
    lua: &Lua,
    ui: &mlua::Table,
    active: &Rc<Cell<bool>>,
) -> mlua::Result<Rc<RefCell<Option<Function>>>> {
    let staging: Rc<RefCell<Option<Function>>> = Rc::default();
    let staging_for_closure = Rc::clone(&staging);
    let active = Rc::clone(active);
    let f = lua.create_function(move |_lua, f: Function| {
        if !active.get() {
            return Err(mlua::Error::RuntimeError(
                "norte.ui.statusbar solo se puede llamar durante la carga de init.lua".to_string(),
            ));
        }
        *staging_for_closure.borrow_mut() = Some(f);
        Ok(())
    })?;
    ui.set("statusbar", f)?;
    Ok(staging)
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
        ui.set("statusbar", lua.create_function(statusbar_outside_load)?)?;
        norte.set("ui", ui)?;
        norte.set("command", lua.create_function(command_outside_load)?)?;
        lua.globals().set("norte", norte)?;

        Ok(Self {
            lua,
            registry,
            loading: Cell::new(false),
            statusbar_disabled: Cell::new(false),
            statusbar_error: RefCell::new(None),
            statusbar_cache: RefCell::new(None),
            run_active: Rc::new(Cell::new(0)),
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
    /// - `norte.ui.statusbar(f)` (task 7) sigue el mismo staging/bandera
    ///   `active` que `norte.command` (muere igual con la carga), pero su
    ///   fusión es MÁS SIMPLE: sin capas ni warnings. Si esta carga llamó a
    ///   `norte.ui.statusbar`, su función pisa a la que hubiera (de esta
    ///   misma capa o de otra) sin más; si no la llamó, el hook previo (si
    ///   lo hay) sobrevive intacto. Es decir: "el último `eval_layer` que
    ///   define el hook, gana", con independencia del orden de capas.
    ///
    /// # Precondición (rust-review T8)
    /// Ningún `CommandRun` de ESTE host en vuelo: la carga corre bajo un
    /// presupuesto de instrucciones ([`EVAL_BUDGET`], `Lua::set_hook`) y la
    /// ranura de hook de mlua es ÚNICA por instancia (ver `statusbar.rs`) —
    /// instalarlo desarmaría el hook de cancelación del run. Se COMPRUEBA
    /// (`run_active`, fail-closed → [`LuaLoadError::RunInFlight`]), no solo
    /// se documenta. Los callers del TUI la cumplen casi siempre por
    /// construcción: `load_lua` evalúa sobre un host recién nacido, y un
    /// run en vuelo a través de un hot-reload retiene el host VIEJO (otra
    /// instancia); el residual (la cola FIFO arranca un run en el host
    /// nuevo mientras el modal TOFU sigue abierto) cae aquí con error
    /// visible en vez de dejar un run incancelable.
    ///
    /// # Errors
    /// Cualquier error de sintaxis o runtime de Lua — incluyendo los que
    /// `norte.command` genera para nombres inválidos o duplicados, y el
    /// presupuesto de carga agotado ([`EVAL_BUDGET`]: un `while true do
    /// end` en el top-level muere con error, jamás congela el TUI) —,
    /// [`LuaLoadError::Reentrant`] si ya hay una carga en curso y
    /// [`LuaLoadError::RunInFlight`] si hay un run en vuelo.
    pub fn eval_layer(&self, source: &[u8], layer: Layer) -> Result<Vec<LuaWarning>, LuaLoadError> {
        if self.loading.get() {
            return Err(LuaLoadError::Reentrant);
        }
        if self.run_active.get() != 0 {
            return Err(LuaLoadError::RunInFlight);
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

        // Staging del hook de statusbar (task 7), ver `stage_statusbar`.
        let ui: mlua::Table = norte.get("ui").map_err(LuaLoadError::Lua)?;
        let statusbar_staging =
            stage_statusbar(&self.lua, &ui, &active).map_err(LuaLoadError::Lua)?;

        let layer_name = match layer {
            Layer::System => "init.lua (sistema)",
            Layer::User => "init.lua (usuario)",
            Layer::Project => "init.lua (proyecto)",
        };
        // Presupuesto de la carga (rust-review T8, [`EVAL_BUDGET`]): el hook
        // ERRA al primer disparo y el chunk muere con error de carga — un
        // init.lua roto jamás congela el run loop. Guard RAII (el MISMO
        // HookGuard de statusbar.rs, una sola pieza): remove_hook pase lo
        // que pase, también si exec() erra. Instalarlo es seguro porque no
        // hay run en vuelo (comprobado arriba: la ranura de hook es única
        // por instancia).
        let exec_result = {
            self.lua.set_hook(
                mlua::HookTriggers::new().every_nth_instruction(EVAL_BUDGET),
                |_, _| {
                    Err(mlua::Error::RuntimeError(
                        "init.lua: presupuesto de instrucciones de carga agotado".to_string(),
                    ))
                },
            );
            let _guard = statusbar::HookGuard(&self.lua);
            self.lua.load(source).set_name(layer_name).exec()
        };

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
        let restore_statusbar = self
            .lua
            .create_function(statusbar_outside_load)
            .and_then(|f| ui.set("statusbar", f));
        exec_result.map_err(LuaLoadError::Lua)?;
        restore.map_err(LuaLoadError::Lua)?;
        restore_statusbar.map_err(LuaLoadError::Lua)?;

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
        // Fusión del hook de statusbar (task 7): si ESTA carga lo definió,
        // pisa al anterior sin más — a diferencia de `commands`, aquí no hay
        // precedencia de capa ni warning; "último eval que lo define gana"
        // (documentado en el rustdoc de `eval_layer`). Si esta capa no llamó
        // a `norte.ui.statusbar`, el hook previo (de otra capa) sobrevive.
        //
        // El cache de `statusbar()` queda invalidado al cambiar el hook: sin
        // esto, un `eval_layer` posterior sobre un host YA VIVO (p. ej. la
        // capa `Project`, evaluada tras resolver el modal TOFU — task 8 — en
        // un host que ya venía sirviendo `statusbar()` con las capas
        // `System`/`User`) podría devolver la respuesta cacheada del hook
        // VIEJO si el `StatusInput` no cambió entretanto.
        if let Some(f) = statusbar_staging.borrow_mut().take() {
            registry.statusbar = Some(f);
            *self.statusbar_cache.borrow_mut() = None;
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

    /// Pinta el hook de statusbar del `init.lua` activo con el snapshot
    /// `input`, si hay uno registrado (task 7).
    ///
    /// Camino rápido: si el hook está deshabilitado (fallo previo) o no hay
    /// ninguno registrado, `None` inmediato sin tocar Lua. Si `input` es
    /// IGUAL (`PartialEq`) al de la última llamada exitosa, se devuelve la
    /// respuesta cacheada sin reinvocar el script — pensado para llamarse en
    /// cada vuelta de render.
    ///
    /// **Un comando en vuelo (`run_active != 0`) CONGELA la barra:** mientras
    /// CUALQUIER `CommandRun` de `driver.rs` siga vivo (contador, no
    /// booleano — ver el campo `run_active`), esta función JAMÁS toca Lua —
    /// ni de lejos `set_hook`/`remove_hook` — devuelve el cache si `input`
    /// coincide o `None` si no. La ranura de hook de mlua es ÚNICA por
    /// instancia (compartida entre el estado principal y TODAS las
    /// corrutinas); solaparse con el `Thread::set_hook` de cancelación del
    /// run en vuelo lo desarmaría en silencio — ver el módulo `statusbar` y
    /// el ADR/spec-review de la task 7 para el detalle completo.
    ///
    /// El caller (T8): dropea el `CommandRun` en cuanto tengas su
    /// `RunOutcome` — mientras lo retengas vivo (aunque ya haya resuelto),
    /// la barra sigue congelada.
    ///
    /// La llamada real (solo si NO hay run en vuelo) corre bajo un
    /// presupuesto de instrucciones (`statusbar::call_hook`) y es SÍNCRONA:
    /// si se agota el presupuesto, el script revienta en runtime, o devuelve
    /// algo que no coacciona a string, el hook queda DESHABILITADO para el
    /// resto de la vida de este host (hasta el próximo hot-reload, que
    /// reconstruye el `LuaHost` entero) y esta llamada devuelve `None`. El
    /// detalle del fallo queda disponible una vez vía
    /// [`Self::statusbar_error`].
    ///
    /// La salida en éxito pasa por `crate::app::detail_for_bar` — jamás
    /// bidi/controles crudos ni una barra desbordada por un string largo.
    #[must_use]
    pub fn statusbar(&self, input: &StatusInput) -> Option<String> {
        if self.statusbar_disabled.get() {
            return None;
        }
        if let Some((prev_input, prev_out)) = self.statusbar_cache.borrow().as_ref()
            && prev_input == input
        {
            return prev_out.clone();
        }
        if self.run_active.get() != 0 {
            // Algún run en vuelo (contador != 0): NUNCA tocar Lua (ver
            // rustdoc de arriba y el módulo `statusbar`). Sin cache que
            // coincida (comprobado justo encima), lo único honesto es
            // `None` — la barra se congela.
            return None;
        }
        // Borrow suelto ANTES de llamar a Lua (mismo invariant que
        // `command_fn`/`eval_layer`).
        let f = self.registry.borrow().statusbar.clone()?;
        match statusbar::call_hook(&self.lua, &f, input) {
            Ok(raw) => {
                let out = Some(crate::app::detail_for_bar(&raw));
                *self.statusbar_cache.borrow_mut() = Some((input.clone(), out.clone()));
                out
            }
            Err(e) => {
                // Deshabilitado: NO se cachea (documentado — `statusbar()`
                // vuelve a devolver `None` directo la próxima vez, sin pasar
                // por el cache).
                self.statusbar_disabled.set(true);
                *self.statusbar_error.borrow_mut() = Some(e.to_string());
                None
            }
        }
    }

    /// Detalle diagnóstico CRUDO del último fallo del hook de statusbar, si
    /// lo hay. CONSUME (`take`): el wiring (task 8) lo pinta en la barra
    /// saneado UNA sola vez.
    pub fn statusbar_error(&self) -> Option<String> {
        self.statusbar_error.borrow_mut().take()
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

    /// Handle compartido (mismo `Rc`, no una copia) del contador
    /// `run_active` (ver el campo). El driver lo INCREMENTA al construir un
    /// `CommandRun` y lo DECREMENTA en su `Drop` — el mismo `Rc` para que
    /// `statusbar()` vea el estado real, no una copia congelada.
    pub(super) fn run_active_handle(&self) -> Rc<Cell<u32>> {
        Rc::clone(&self.run_active)
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

    /// MAJOR (rust-review T8): un `init.lua` con un bucle infinito NO puede
    /// congelar la carga (correría en el run loop del TUI: sin draw, sin
    /// Esc, terminal en raw mode al matar el proceso). El presupuesto de
    /// instrucciones de `eval_layer` lo mata con error de carga; el host
    /// sigue usable después.
    #[test]
    fn init_lua_con_bucle_infinito_no_congela_la_carga() {
        let h = host();
        assert!(
            h.eval_layer(b"while true do end", Layer::User).is_err(),
            "presupuesto agotado = error de carga, jamás cuelgue"
        );
        h.eval_layer(b"norte.command('ok', function() end)", Layer::User)
            .expect("el host sigue usable tras agotar el presupuesto");
        assert_eq!(h.commands(), vec!["ok".to_string()]);
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

    /// La precondición «sin run en vuelo» se COMPRUEBA (fail-closed): con
    /// `run_active != 0`, cargar pisaría el hook de cancelación del run —
    /// se rechaza con error visible en vez de dejar un run incancelable.
    #[test]
    fn eval_layer_con_run_en_vuelo_se_rechaza() {
        let h = host();
        h.run_active.set(1);
        let err = h.eval_layer(b"norte.command('x', function() end)", Layer::User);
        assert!(matches!(err, Err(LuaLoadError::RunInFlight)));
        h.run_active.set(0);
        h.eval_layer(b"norte.command('x', function() end)", Layer::User)
            .expect("sin run en vuelo la carga vuelve a pasar");
    }
}
