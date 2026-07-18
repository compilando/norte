//! Ejecución de un comando: future !Send que el main loop pollea inline
//! (JAMÁS `tokio::spawn`). Cancelación en dos frentes (regla 3):
//! 1. las Tasks del engine lanzadas por el run (cancellers registrados);
//! 2. el propio script, vía hook de instrucciones (mata bucles Lua puros).
//!
//! Un script clavado en C (`os.execute`) no responde a ninguno: timeout duro
//! y el driver ABANDONA el future (drop); el estado Lua se tira en el
//! próximo reload. Los caminos de abandono (gracia/deadline) pueden dejar un
//! submit remoto en vuelo sin canceller registrado — deuda #74, documentada
//! en [`super::fs::install_fs`].

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};
use std::time::Duration;

use mlua::{HookTriggers, Lua, VmState};
use norte_core::backend::Backend;
use tokio_util::sync::CancellationToken;

use super::api::LuaHost;
use super::fs::{self, PaneCtx, RunCancellers};

/// Timeout duro por defecto de un run: contrato DOCUMENTADO de
/// [`LuaHost::invoke`] (por eso es `pub`, reexportado en `lua`);
/// configurable en v2 (ver spec).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_mins(5);

/// Gracia tras pedir cancelación: margen para que el hook de instrucciones
/// mate la corrutina; agotada, el driver abandona igual que con el timeout.
const CANCEL_GRACE: Duration = Duration::from_secs(5);

/// Cadencia del hook de instrucciones del run. Los hooks de Lua son POR
/// THREAD (corrutina): se instala en la corrutina del comando ANTES de
/// arrancarla — no vale `Lua::set_hook` a posteriori, que apunta al estado
/// principal y jamás dispararía dentro del run. Cada `HOOK_EVERY`
/// instrucciones el hook devuelve [`VmState::Yield`]: la corrutina cede el
/// control al executor (poll → `Pending` + wake inmediato en mlua), que es
/// lo que permite que los brazos de token/deadline del `select!` lleguen a
/// correr — sin él, un bucle Lua puro monopolizaría el poll para siempre y
/// ni cancelación ni timeout podrían disparar.
const HOOK_EVERY: u32 = 4096;

/// Desenlace de un run de comando Lua.
#[derive(Debug)]
pub enum RunOutcome {
    /// El comando terminó; mensajes de `norte.ui.message` acumulados.
    Ok {
        /// Mensajes del run, en orden (tope `MESSAGES_MAX`, ver `fs.rs`).
        messages: Vec<String>,
    },
    /// Error Lua (detalle diagnóstico CRUDO: el caller lo sanea y localiza
    /// antes de pintarlo, patrón #73 — jamás va a la barra tal cual).
    Err {
        /// `Display` del error de mlua, sin sanear.
        detail: String,
        /// Mensajes acumulados hasta el error.
        messages: Vec<String>,
    },
    /// Cancelado por el usuario (token). Cualquier error Lua posterior a la
    /// petición de cancelación cuenta como `Cancelled` (el hook mata el
    /// script con un error artificial).
    Cancelled,
    /// Timeout duro: el driver abandonó el future del run.
    TimedOut,
}

/// Future de un run de comando (!Send: mlua vive en el main task). Se
/// obtiene de [`LuaHost::invoke`] y el main loop lo pollea inline.
///
/// Lleva `run_active` como VALOR (no solo dentro del future que envuelve):
/// `CommandRun` existe desde el instante en que `invoke_with_timeout` lo
/// devuelve, se pollee alguna vez o no. Su [`Drop`] es la ÚNICA fuente de
/// verdad que DECREMENTA `run_active` — ver ahí el porqué (spec-review 2,
/// task 7): un guard construido DENTRO del future (como `RunGuard`, más
/// abajo) JAMÁS correría si el caller crea el `CommandRun` y lo dropea sin
/// pollear ni una vez (el cuerpo de una `async fn` no ejecuta nada hasta el
/// primer poll) — `run_active` quedaría atascado y la barra congelada hasta
/// el próximo hot-reload. Con el contador en el propio tipo, ese camino es
/// estructuralmente imposible: no depende de la disciplina del caller (T8)
/// de pollear hasta el final.
///
/// **Dropea el valor en cuanto tengas el [`RunOutcome`]** (spec-review 3):
/// mientras un `CommandRun` ya resuelto siga vivo en algún sitio (p. ej. un
/// slot `Option<CommandRun>` que aún no se ha puesto a `None`),
/// `LuaHost::statusbar` lo sigue contando como «en vuelo» y la barra
/// permanece congelada de más.
///
/// `run_active` es un CONTADOR (`Rc<Cell<u32>>`), no un booleano: el patrón
/// natural del caller `self.run = Some(host.invoke(...))` evalúa el RHS
/// (que INCREMENTA para el run nuevo) antes de dropear el valor viejo que
/// ocupaba el slot (que DECREMENTA) — con un booleano, ese decremento del
/// viejo pisaría el `true` que el nuevo acababa de encender, dejándolo sin
/// protección. Con un contador ambos se compensan.
pub struct CommandRun {
    future: Pin<Box<dyn Future<Output = RunOutcome>>>,
    run_active: Rc<Cell<u32>>,
}

impl Future for CommandRun {
    type Output = RunOutcome;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<RunOutcome> {
        self.future.as_mut().poll(cx)
    }
}

impl Drop for CommandRun {
    fn drop(&mut self) {
        // Cubre los TRES caminos: polleado hasta un desenlace terminal,
        // abandonado a medias (drop de un future `Pending`), o jamás
        // polleado (drop inmediato tras `invoke_with_timeout`, sin await
        // alguno — el cuerpo de `run_command` nunca llegó a ejecutarse, así
        // que ningún `RunGuard` interno corrió). Saturante: el contador
        // jamás debería llegar a 0 e intentar bajar más (cada `CommandRun`
        // decrementa como mucho una vez, en SU propio Drop), pero
        // `saturating_sub` es la defensa barata contra un futuro bug de
        // conteo — subdesbordar un `u32` en release sería peor (wrap a
        // `u32::MAX`, la barra JAMÁS se descongela).
        self.run_active.set(self.run_active.get().saturating_sub(1));
    }
}

/// RAII del run: pase lo que pase (retorno normal, cancelación, o ABANDONO
/// del future — el `Drop` corre también al dropear el future a medias):
/// - `remove_hook`: el hook del run vive en su corrutina (que muere con el
///   future), pero mlua guarda la clausura en una ranura POR ESTADO Lua
///   compartida; limpiarla evita que sobreviva al run (el estado Lua es
///   COMPARTIDO entre runs y con el statusbar);
/// - cierra el run (`closed = true`): los bindings fs stasheados mueren;
/// - cancela los cancellers registrados (doble-cancel inofensivo: token ya
///   cancelado o task terminal son no-op; cubre el abandono por timeout,
///   donde ninguna otra vía las cancelaría).
///
/// NO toca `run_active` (task 7, spec-review 2): esa responsabilidad vive
/// ahora en el `Drop` de [`CommandRun`] — este guard corre DENTRO del
/// future, así que un `CommandRun` jamás polleado lo dejaría sin ejecutar
/// jamás (ver el rustdoc de `CommandRun`).
struct RunGuard {
    lua: Lua,
    closed: Rc<Cell<bool>>,
    cancellers: RunCancellers,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        self.lua.remove_hook();
        self.closed.set(true);
        // Defensivo: un panic en Drop es abort. Hoy ningún borrow de
        // `cancellers` cruza un await (invariant de fs.rs), pero si un
        // cambio futuro lo rompiera y el future se abandonara con el borrow
        // vivo, este Drop NO debe rematar el proceso — mejor saltarse el
        // doble-cancel (best-effort) que abortar.
        if let Ok(cs) = self.cancellers.try_borrow() {
            for c in cs.iter() {
                c.cancel();
            }
        }
    }
}

/// Canales/flags compartidos de un run, agrupados en un solo valor para no
/// desbordar el número de argumentos de `run_command` (cada uno es un
/// `Rc`/`Rc<RefCell<_>>` barato de mover).
struct RunChannels {
    cancellers: RunCancellers,
    messages: Rc<RefCell<Vec<String>>>,
    closed: Rc<Cell<bool>>,
}

impl LuaHost {
    /// Arranca el comando `name` con el timeout por defecto. Ver
    /// [`LuaHost::invoke_with_timeout`].
    #[must_use]
    pub fn invoke(
        &self,
        name: &str,
        backend: Backend,
        ctx: PaneCtx,
        token: CancellationToken,
    ) -> Option<CommandRun> {
        self.invoke_with_timeout(name, backend, ctx, token, DEFAULT_TIMEOUT)
    }

    /// Arranca el comando `name`: instala bindings FRESCOS (`install_fs`,
    /// snapshot `ctx` y canales del run nuevos) y devuelve el future del
    /// run, o `None` si el comando no existe.
    ///
    /// El caller es responsable de la SERIALIZACIÓN: un run a la vez por
    /// host (el estado Lua es uno; dos runs concurrentes pisarían bindings y
    /// hook). La cola FIFO vive en el run loop del TUI (task 8), no aquí.
    ///
    /// Cancelación (regla 3): al cancelar `token` se cancelan las Tasks del
    /// engine registradas por el run Y el hook de instrucciones de la
    /// corrutina (instalado desde el arranque, ver [`HOOK_EVERY`]) pasa a
    /// errar — mata bucles Lua puros; si en [`CANCEL_GRACE`] el script no ha
    /// muerto (clavado en C, p. ej. `os.execute`), o si vence `timeout`, el
    /// driver ABANDONA el future (drop) — el guard interno limpia hook y
    /// cierra el run igualmente.
    #[must_use]
    pub fn invoke_with_timeout(
        &self,
        name: &str,
        backend: Backend,
        ctx: PaneCtx,
        token: CancellationToken,
        timeout: Duration,
    ) -> Option<CommandRun> {
        // Borrow del registro SUELTO antes de tocar Lua (invariant).
        let f = self.command_fn(name)?;
        let lua = self.lua_handle();
        let run_active = self.run_active_handle();
        let cancellers: RunCancellers = Rc::default();
        let messages: Rc<RefCell<Vec<String>>> = Rc::default();
        let closed: Rc<Cell<bool>> = Rc::default();

        if let Err(e) = fs::install_fs(
            &lua,
            backend,
            ctx,
            Rc::clone(&cancellers),
            Rc::clone(&messages),
            Rc::clone(&closed),
        ) {
            // Instalación a medias: se cierra el run (ningún binding parcial
            // sobrevive) y el run resuelve inmediato a Err — nunca panic. NO
            // hay hook alguno de por medio en este camino, pero `run_active`
            // se incrementa IGUAL, por simetría con el decremento
            // incondicional del `Drop` de `CommandRun`: cada `CommandRun`
            // que sale de aquí decrementa exactamente una vez al morir, así
            // que cada uno debe incrementar exactamente una vez al nacer —
            // hacerlo condicional rompería esa invariante de conteo.
            closed.set(true);
            let detail = e.to_string();
            run_active.set(run_active.get().saturating_add(1));
            return Some(CommandRun {
                future: Box::pin(async move {
                    RunOutcome::Err {
                        detail,
                        messages: Vec::new(),
                    }
                }),
                run_active,
            });
        }

        // El run arranca AQUÍ: se incrementa ANTES de devolver el valor, y
        // su decremento vive en el `Drop` de `CommandRun` (no en un guard
        // interno del future) — así que ni siquiera importa si el caller
        // pollea el valor devuelto o lo dropea de inmediato (ver el rustdoc
        // de `CommandRun`). Contador, no booleano (spec-review 3): el patrón
        // `self.run = Some(host.invoke(...))` incrementa para el run nuevo
        // ANTES de que el `Drop` del run viejo (que ocupaba el slot)
        // decremente — con un booleano, ese decremento pisaría el `true`
        // recién puesto.
        run_active.set(run_active.get().saturating_add(1));

        let channels = RunChannels {
            cancellers,
            messages,
            closed,
        };
        Some(CommandRun {
            future: Box::pin(run_command(lua, f, token, timeout, channels)),
            run_active,
        })
    }
}

/// Cuerpo del run: call pineado + `select!` en loop (no se puede «retomar»
/// un `call_async` seleccionado-fuera; el loop conserva el MISMO future del
/// call a través de la petición de cancelación).
///
/// El comando corre en una corrutina propia (`Thread`) con su hook de
/// instrucciones instalado ANTES de arrancar (los hooks son por thread, ver
/// [`HOOK_EVERY`]): en régimen normal el hook CEDE el control al executor;
/// tras la petición de cancelación (bandera compartida `cancel_flag`) pasa a
/// ERRAR y la corrutina muere en como mucho `HOOK_EVERY` instrucciones Lua.
async fn run_command(
    lua: Lua,
    f: mlua::Function,
    token: CancellationToken,
    timeout: Duration,
    channels: RunChannels,
) -> RunOutcome {
    let RunChannels {
        cancellers,
        messages,
        closed,
    } = channels;
    // El guard vive DENTRO del future: si el driver lo abandona (drop en los
    // brazos de gracia/deadline… o el caller dropea el CommandRun ya
    // polleado al menos una vez), el Drop corre igual y el estado Lua
    // compartido queda limpio. `run_active` NO vive aquí (ver el rustdoc de
    // `CommandRun`/`RunGuard`) — este guard nunca correría si el
    // `CommandRun` se dropea sin pollear ni una vez.
    let _guard = RunGuard {
        lua: lua.clone(),
        closed,
        cancellers: Rc::clone(&cancellers),
    };

    let take_messages = || std::mem::take(&mut *messages.borrow_mut());

    let thread = match lua.create_thread(f) {
        Ok(t) => t,
        Err(e) => {
            return RunOutcome::Err {
                detail: e.to_string(),
                messages: take_messages(),
            };
        }
    };
    // Frente 2 de la cancelación, armado desde el arranque: la bandera la
    // enciende el brazo del token; el hook la ve en el siguiente lote de
    // instrucciones. Mientras no esté encendida, el yield periódico devuelve
    // el control al select (imprescindible: sin él un `while true do end`
    // bloquearía este poll para siempre).
    let cancel_flag = Rc::new(Cell::new(false));
    {
        let cancel_flag = Rc::clone(&cancel_flag);
        thread.set_hook(
            HookTriggers::new().every_nth_instruction(HOOK_EVERY),
            move |_, _| {
                if cancel_flag.get() {
                    Err(mlua::Error::RuntimeError("cancelled".into()))
                } else {
                    Ok(VmState::Yield)
                }
            },
        );
    }

    let mut call = std::pin::pin!(thread.into_async::<()>(()));
    let mut cancel_requested = false;
    let mut deadline = std::pin::pin!(tokio::time::sleep(timeout));
    // Solo se pollea tras la petición de cancelación (guardado por el flag);
    // al activarse se resetea a «ahora + gracia».
    let mut grace = std::pin::pin!(tokio::time::sleep(CANCEL_GRACE));

    loop {
        tokio::select! {
            biased;
            r = &mut call => {
                break match r {
                    Ok(()) => RunOutcome::Ok { messages: take_messages() },
                    // Error posterior a la petición de cancelación: es la
                    // cancelación (el hook mata con "cancelled", pero
                    // CUALQUIER error post-cancel cuenta como tal).
                    Err(_) if cancel_requested => RunOutcome::Cancelled,
                    Err(e) => RunOutcome::Err {
                        detail: e.to_string(),
                        messages: take_messages(),
                    },
                };
            }
            () = token.cancelled(), if !cancel_requested => {
                cancel_requested = true;
                // Frente 1: las Tasks del engine lanzadas por el run.
                for c in cancellers.borrow().iter() {
                    c.cancel();
                }
                // Frente 2: el hook de la corrutina pasa a errar.
                cancel_flag.set(true);
                grace
                    .as_mut()
                    .reset(tokio::time::Instant::now() + CANCEL_GRACE);
            }
            // Gracia agotada (script clavado en C, inmune al hook): ABANDONA
            // el call en vuelo (drop al salir; un submit remoto puede quedar
            // sin canceller — deuda #74).
            () = &mut grace, if cancel_requested => break RunOutcome::Cancelled,
            // Timeout duro: ABANDONA igual (deuda #74 ídem). Si el usuario
            // ya había cancelado (cancel en t≈timeout, con la gracia aún
            // corriendo), el desenlace honesto es Cancelled, no TimedOut —
            // quien canceló no debe ver «se agotó el tiempo». La carrera es
            // difícil de forzar en test sin un script clavado en C (el hook
            // mata bucles Lua puros en microsegundos): solo el fix.
            () = &mut deadline => {
                break if cancel_requested {
                    RunOutcome::Cancelled
                } else {
                    RunOutcome::TimedOut
                };
            }
        }
    }
    // El guard limpia (remove_hook + closed + cancel) también en este camino.
}
