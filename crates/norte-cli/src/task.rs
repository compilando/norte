//! El manejo de Ctrl+C y el bucle de progreso compartidos por todas las Tasks
//! que corre `norte` (`cp`/`mv`/`rm`/`undo`/`sync`/`index`).

use std::process::ExitCode;

use norte_core::backend::TaskRef;
use norte_proto::TaskState;

use crate::EXIT_CANCELLED;

/// Arma el manejador de Ctrl+C de una Task: cancela por su
/// [`norte_core::backend::TaskCanceller`] (regla dura 3) en vez de dejar que
/// el SO mate el proceso con el SIGINT por defecto.
///
/// Compartido entre [`run_task`] y `sync.plan` (#180): antes de esto solo
/// `run_task` lo armaba, así que un Ctrl+C durante el drenaje de
/// `sync_plan_show_apply` —que no pasa por `run_task`— no tenía manejador
/// alguno y el proceso moría por SIGINT sin correr ningún `Drop`. Eso importa
/// aquí más que en `cp`/`mv`/`rm`: un `.part` de spool solo se limpia si
/// `SpoolWriter::finish`/`Drop` llega a ejecutarse, y ninguno de los dos
/// corre cuando el SO termina el proceso por señal en vez de por un retorno
/// normal.
///
/// El llamante tiene que `abort()` el `JoinHandle` devuelto en cuanto la Task
/// termina — si no, el `ctrl_c()` de dentro se queda esperando para siempre.
/// El vigilante de SIGINT de TODO `norte sync`, con objetivo intercambiable.
///
/// Existe porque [`watch_ctrl_c`] por fase es incorrecto y esta rama lo
/// demostró (revisión de rama de W2, BLOCKER-1). Registrar `ctrl_c()` en tokio
/// es **de proceso y permanente**: la doc de tokio lo dice con todas las letras
/// —«even if this `Signal` instance is dropped, subsequent `SIGINT` deliveries
/// will end up captured by Tokio, and the default platform behavior will NOT be
/// reset»—, así que abortar la task que esperaba NO devuelve la señal al SO.
///
/// Con un vigilante por fase, cada hueco ENTRE fases queda con un SIGINT que
/// tokio se traga y que ya no mata el proceso. El hueco que importa es el
/// prompt `[y/N]`: el sitio donde un humano se sienta minutos decidiendo si
/// borra un subárbol, y donde antes de #180 `Ctrl+C` sí funcionaba porque
/// todavía no se había registrado nada.
///
/// Un solo vigilante para todo el mandato, y las fases le van poniendo su
/// cancelador. Sin cancelador puesto, el `Ctrl+C` sale con 130 él mismo, que es
/// lo que hacía el SO. Y `take()` en vez de leer: el PRIMER `Ctrl+C` cancela la
/// task, el SEGUNDO sale — el mismo pacto que el doble `Esc` de los paneles.
pub(crate) struct SigintGate {
    estado: std::sync::Arc<std::sync::Mutex<SigintState<norte_core::backend::TaskCanceller>>>,
    _handle: tokio::task::JoinHandle<()>,
}

/// Qué hacer con un `Ctrl+C`, decidido SOLO por el estado del vigilante.
///
/// Se separa de la puerta para poder probarlo sin señales ni procesos: el
/// hueco que cierra es de milisegundos y no se reproduce a mano.
#[derive(Debug, PartialEq, Eq)]
enum SigintAction {
    /// Hay Task viva: cancelarla (regla dura 3).
    Cancel,
    /// Hay una Task NACIENDO: apuntar el `Ctrl+C` y cancelarla en cuanto
    /// exista. Salir aquí mataría el proceso sin que corriese el `Drop` que
    /// borra el `.part` del spool — que es justo lo que #180 arregló y esta
    /// ventana volvía a abrir.
    Defer,
    /// No hay nada vivo ni naciendo (el prompt `[y/N]`, el plan en pantalla):
    /// se hace lo que haría el SO.
    Exit,
}

/// El estado del vigilante. Genérico en el cancelador para que los tests no
/// necesiten un `TaskRef` de verdad.
struct SigintState<C> {
    objetivo: Option<C>,
    /// Hay una Task pedida cuyo handle todavía no ha vuelto.
    naciendo: bool,
    /// Llegó un `Ctrl+C` mientras nacía.
    pendiente: bool,
}

impl<C> Default for SigintState<C> {
    fn default() -> Self {
        Self {
            objetivo: None,
            naciendo: false,
            pendiente: false,
        }
    }
}

impl<C> SigintState<C> {
    /// Decide qué hacer con la señal, llevándose el cancelador si lo hay.
    fn on_signal(&mut self) -> (SigintAction, Option<C>) {
        if let Some(c) = self.objetivo.take() {
            return (SigintAction::Cancel, Some(c));
        }
        if self.naciendo {
            self.pendiente = true;
            return (SigintAction::Defer, None);
        }
        (SigintAction::Exit, None)
    }

    /// Se ha PEDIDO una Task: desde aquí y hasta [`Self::apunta_a`], un
    /// `Ctrl+C` se aparca en vez de matar el proceso.
    fn naciendo(&mut self) {
        self.naciendo = true;
    }

    /// La Task ya existe. Devuelve el cancelador si hay que usarlo YA porque
    /// el `Ctrl+C` llegó mientras nacía.
    fn apunta_a(&mut self, canceller: C) -> Option<C> {
        self.naciendo = false;
        if std::mem::take(&mut self.pendiente) {
            return Some(canceller);
        }
        self.objetivo = Some(canceller);
        None
    }

    /// Ya no hay Task viva: el siguiente `Ctrl+C` sale con 130.
    fn suelta(&mut self) {
        self.objetivo = None;
        self.naciendo = false;
        self.pendiente = false;
    }
}

impl SigintGate {
    /// Arma el vigilante. Una vez por mandato, nunca por fase.
    pub(crate) fn arm() -> Self {
        let estado: std::sync::Arc<
            std::sync::Mutex<SigintState<norte_core::backend::TaskCanceller>>,
        > = std::sync::Arc::default();
        let visto = std::sync::Arc::clone(&estado);
        let handle = tokio::spawn(async move {
            loop {
                if tokio::signal::ctrl_c().await.is_err() {
                    break;
                }
                // INVARIANTE: el Mutex nunca se envenena — bajo el lock solo
                // se mueven Options y bools, sin panic posible.
                let (accion, canceller) = visto.lock().unwrap().on_signal();
                match accion {
                    SigintAction::Cancel => {
                        eprintln!("\n{}", norte_i18n::t("cli-cancelling"));
                        if let Some(c) = canceller {
                            c.cancel();
                        }
                    }
                    // La Task todavía no ha vuelto: se aparca y `apunta_a` la
                    // cancela en cuanto exista. Salir aquí sería `exit(130)`
                    // sin `Drop`, y el `.part` del spool quedaría huérfano.
                    SigintAction::Defer => eprintln!("\n{}", norte_i18n::t("cli-cancelling")),
                    SigintAction::Exit => {
                        eprintln!();
                        std::process::exit(130);
                    }
                }
            }
        });
        Self {
            estado,
            _handle: handle,
        }
    }

    /// Se ha PEDIDO una Task. Llamar ANTES de arrancarla: entre la petición y
    /// el handle hay una ventana en la que un `Ctrl+C` mataba el proceso en
    /// crudo, saltándose el `Drop` que borra el `.part` del spool.
    pub(crate) fn naciendo(&self) {
        self.estado.lock().unwrap().naciendo();
    }

    /// Esta Task es la que un `Ctrl+C` cancela a partir de ahora — y si la
    /// señal ya llegó mientras nacía, se la cancela AQUÍ.
    pub(crate) fn apunta_a(&self, task: &TaskRef) {
        // INVARIANTE: como arriba.
        let ya = self.estado.lock().unwrap().apunta_a(task.canceller());
        if let Some(c) = ya {
            c.cancel();
        }
    }

    /// Ya no hay Task viva: el siguiente `Ctrl+C` sale con 130.
    pub(crate) fn suelta(&self) {
        // INVARIANTE: como arriba.
        self.estado.lock().unwrap().suelta();
    }
}

fn watch_ctrl_c(task: &TaskRef) -> tokio::task::JoinHandle<()> {
    let canceller = task.canceller();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\n{}", norte_i18n::t("cli-cancelling"));
            canceller.cancel();
        }
    })
}

/// El bucle compartido entre [`run_task`] y `sync.apply`: pinta progreso en
/// stderr, arma [`watch_ctrl_c`], y devuelve el [`TaskState`] terminal SIN
/// traducirlo a código de salida ni a mensaje.
///
/// La traducción vive en cada llamante a propósito (#187): para `run_task`
/// —`cp`/`mv`/`rm`/`undo`— un `Cancelled` ES «destino limpio». Para
/// `sync.apply` no lo es: lo aplicado hasta el corte se queda, journalizado,
/// y el único frontend que puede decir cuánto es el que pide `sync.report`.
/// Colapsar los dos casos en la rama `Cancelled` de un único traductor es
/// exactamente cómo el CLI se quedó siendo el único de los tres frontends que
/// no podía decirlo.
pub(crate) async fn drive_task(
    task: TaskRef,
    show_bytes: bool,
    sigint: Option<&SigintGate>,
) -> TaskState {
    // Con puerta —`norte sync`, que tiene varias fases y un prompt entre
    // ellas— se le APUNTA. Sin ella —`cp`/`mv`/`rm`/`undo`, un solo mandato
    // que sale en cuanto la Task termina— basta el vigilante de siempre: el
    // hueco que `SigintGate` cierra no existe ahí, porque no hay nada después.
    let sig = sigint.map_or_else(
        || Some(watch_ctrl_c(&task)),
        |g| {
            g.apunta_a(&task);
            None
        },
    );

    let mut rx = task.progress();
    loop {
        let snap = rx.borrow_and_update().clone();
        render(&snap, show_bytes);
        if snap.state.is_terminal() {
            break;
        }
        if rx.changed().await.is_err() {
            break;
        }
    }
    match (&sig, sigint) {
        (Some(h), _) => h.abort(),
        (None, Some(g)) => g.suelta(),
        (None, None) => {}
    }
    let final_state = rx.borrow().state.clone();
    eprintln!();
    final_state
}

/// Corre una Task pintando progreso en stderr; Ctrl-C cancela cooperativamente
/// (la task deja destino limpio o `.norte-partial`, regla dura 3).
pub(crate) async fn run_task(task: TaskRef, show_bytes: bool) -> ExitCode {
    match drive_task(task, show_bytes, None).await {
        TaskState::Completed => ExitCode::SUCCESS,
        TaskState::Cancelled => {
            eprintln!("{}", norte_i18n::t("cli-cancelled-clean"));
            ExitCode::from(EXIT_CANCELLED)
        }
        TaskState::Failed { error } => {
            eprintln!(
                "{}",
                norte_i18n::ta("cli-final-error", &[("error", &error.to_string())])
            );
            ExitCode::FAILURE
        }
        other => {
            eprintln!(
                "{}",
                norte_i18n::ta("cli-unexpected-state", &[("state", &format!("{other:?}"))])
            );
            ExitCode::FAILURE
        }
    }
}

fn render(p: &norte_proto::TaskProgress, show_bytes: bool) {
    let entries = match p.entries_total {
        Some(t) => format!("{}/{t}", p.entries_done),
        None => format!("{}/?", p.entries_done),
    };
    if show_bytes {
        let bytes = match p.bytes_total {
            Some(t) if t > 0 => {
                let pct = p.bytes_done.saturating_mul(100) / t;
                format!("{} / {t} bytes ({pct}%)", p.bytes_done)
            }
            _ => format!("{} bytes", p.bytes_done),
        };
        eprint!("\r{bytes} — {entries} entradas   ");
    } else {
        eprint!("\r{entries} entradas   ");
    }
}

#[cfg(test)]
mod sigint_gate_tests {
    use super::{SigintAction, SigintState};

    /// Regresión del hueco que `just test` destapó bajo carga: la Task nace
    /// DENTRO de `sync_plan`, y el `.part` del spool con ella. Un `Ctrl+C` en
    /// esa ventana salía por `process::exit(130)` — sin `Drop`, y por tanto
    /// con el `.part` huérfano que #180 existía para evitar. El código de
    /// salida no distinguía los dos casos: 130 en los dos.
    #[test]
    fn una_senal_mientras_la_task_nace_no_mata_el_proceso() {
        let mut estado = SigintState::<&str>::default();
        estado.naciendo();
        let (accion, canceller) = estado.on_signal();
        assert_eq!(accion, SigintAction::Defer, "jamás Exit mientras nace");
        assert!(canceller.is_none());
        // Y en cuanto la Task existe, se la cancela YA: la señal no se pierde.
        assert_eq!(estado.apunta_a("canceller"), Some("canceller"));
    }

    #[test]
    fn con_task_viva_la_senal_cancela_una_sola_vez() {
        let mut estado = SigintState::<&str>::default();
        assert_eq!(estado.apunta_a("canceller"), None);
        assert_eq!(
            estado.on_signal(),
            (SigintAction::Cancel, Some("canceller"))
        );
        // El SEGUNDO Ctrl+C sale, que es el pacto del doble Esc.
        assert_eq!(estado.on_signal(), (SigintAction::Exit, None));
    }

    #[test]
    fn sin_nada_vivo_la_senal_sale_como_haria_el_so() {
        let mut estado = SigintState::<&str>::default();
        assert_eq!(estado.on_signal(), (SigintAction::Exit, None));
    }

    /// Soltar la Task borra también un `Ctrl+C` aparcado: si la que nacía ya
    /// terminó, cancelar a la SIGUIENTE sería cancelar lo que nadie pidió.
    #[test]
    fn soltar_olvida_la_senal_aparcada() {
        let mut estado = SigintState::<&str>::default();
        estado.naciendo();
        assert_eq!(estado.on_signal().0, SigintAction::Defer);
        estado.suelta();
        assert_eq!(estado.apunta_a("otra"), None, "no se cancela la siguiente");
    }
}
