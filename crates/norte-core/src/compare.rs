//! La Task de `fs.compare` (C6): envuelve el motor de `norte-compare` en el
//! framework de tasks del core y convierte su flujo de filas en LOTES
//! acotados y coalescidos ([`CompareRowsBatch`]).
//!
//! Aquí no se decide nada sobre la comparación: los veredictos, las
//! confianzas y los errores-como-fila son del motor. Lo que este módulo
//! aporta es lo que el motor deliberadamente no sabe — la cancelación de la
//! Task, el progreso, y que un millón de filas no puede convertirse en un
//! millón de frames.
//!
//! **Regla dura 4 NO aplica**: comparar no muta nada, no escribe un byte y no
//! tiene undo posible, así que no hay entrada de journal que crear. Está dicho
//! aquí para que una revisión posterior no pida una que no significaría nada.
//!
//! El coalescing es el mismo que el de `fs.search` (`search.rs`), y a
//! propósito: dos contratos de lote distintos para dos feeds idénticos serían
//! dos cosas que mantener sincronizadas a mano.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use norte_compare::{CompareError, CompareOptions};
use norte_proto::methods::{COMPARE_ROWS_MAX_BATCH, CompareRow, CompareRowsBatch};
use norte_proto::{Error, TaskId, VPath};
use norte_vfs::Provider;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Flush por tiempo del lote de filas: el mismo intervalo (y el mismo motivo)
/// que el de `fs.search` — el panel gotea en vivo aunque el lote no se llene.
const FLUSH_INTERVAL: Duration = Duration::from_millis(100);

/// Lote en construcción.
struct Batch {
    task_id: TaskId,
    rows: Vec<CompareRow>,
}

impl Batch {
    fn new(task_id: TaskId) -> Self {
        Self {
            task_id,
            rows: Vec::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    fn len(&self) -> usize {
        self.rows.len()
    }

    /// Extrae el lote acumulado, dejando el buffer vacío.
    fn take(&mut self) -> CompareRowsBatch {
        CompareRowsBatch {
            task_id: self.task_id,
            rows: std::mem::take(&mut self.rows),
        }
    }
}

/// Desenlace de un [`flush`] (calca el de `search.rs`).
enum FlushOutcome {
    /// Enviado (o nada que enviar): sigue la comparación. Lleva CUÁNTAS filas
    /// salieron, que es lo que cuenta el progreso — un lote que no llegó a
    /// enviarse no puede aparecer en `entries_done`.
    Continue(u64),
    /// El receptor murió (el dueño se fue): termina limpio.
    ReceiverGone,
    /// Cancelado mientras el `send` estaba bloqueado por backpressure:
    /// termina como `Cancelled` (regla dura 3).
    Cancelled,
}

/// Cómo empareja la PAREJA de lados, calculado por QUIEN CONOCE LAS DOS
/// RAÍCES — `norte_compare::compare` ya no lo calcula sola (#153, ADR 0051):
/// antes lo hacía internamente, en el primer listado, contra
/// `Provider::capabilities()` sin path — un mismo `LocalProvider` sirviendo
/// `/home` (ext4) y `/mnt/usb` (exFAT) contestaba la MISMA respuesta para los
/// dos, y las colisiones de plegado del segundo mount se perdían en silencio.
///
/// El `stat` de cada raíz fuerza el sondeo de `norte-vfs-local` ANTES de leer
/// `capabilities()`, que si no es exacta solo tras la primera operación async
/// (el sondeo corre ahí; antes es el default del OS vía `cfg!(target_os)`).
/// **Esto no cierra el hueco de #153 por sí solo** — `Provider::capabilities`
/// sigue sin tomar path, así que dos raíces servidas por el MISMO provider
/// siguen compartiendo una `Capabilities` — pero es el paso que no exige
/// tocar el trait `Provider` (una query por-path es la forma de #164 y quiere
/// su propia ADR), y es DONDE puede crecer sin volver a tocar
/// `norte-compare`: quien conoce las dos raíces es quien puede, mañana,
/// resolver el mount real de cada una.
///
/// Los errores de `stat` se ignoran a propósito: una raíz que no existe sigue
/// fallando su propio `list` dentro del motor de comparación, con su fila de
/// error — aquí solo interesa el efecto secundario del sondeo.
///
/// Esto asume que un provider deja su sondeo de capacidades EN EL ESTADO QUE
/// SEA (probado u honestamente sin probar) aunque la operación que lo
/// disparó falle — cierto hoy de `norte-vfs-local::ensure_caps`, que corre
/// antes que el `stat` pueda fallar, pero no es parte del contrato de
/// `Provider`. Un provider futuro cuyo sondeo solo completase en el camino
/// de ÉXITO degradaría en silencio a `default_capabilities()` para la
/// comparación entera; el `trace!` de abajo es la única señal si eso pasa.
pub(crate) async fn probed_sides(
    left: &dyn Provider,
    left_root: &VPath,
    right: &dyn Provider,
    right_root: &VPath,
) -> norte_compare::Sides {
    let (left_probe, right_probe) = tokio::join!(left.stat(left_root), right.stat(right_root));
    if let Err(e) = left_probe {
        tracing::trace!(error = %e, "probed_sides: stat de la raíz izquierda falló (ignorado a propósito)");
    }
    if let Err(e) = right_probe {
        tracing::trace!(error = %e, "probed_sides: stat de la raíz derecha falló (ignorado a propósito)");
    }
    norte_compare::Sides::from_capabilities(left.capabilities(), right.capabilities())
}

/// Envía el lote pendiente (si lo hay). Un `send` bloqueado por backpressure
/// (canal lleno + receptor lento) NO ignora la cancelación: se hace `select`
/// contra el token.
async fn flush(
    tx: &mpsc::Sender<CompareRowsBatch>,
    batch: &mut Batch,
    cancel: &CancellationToken,
) -> FlushOutcome {
    if batch.is_empty() {
        return FlushOutcome::Continue(0);
    }
    let lot = batch.take();
    let rows = u64::try_from(lot.rows.len()).unwrap_or(u64::MAX);
    tokio::select! {
        biased;
        () = cancel.cancelled() => FlushOutcome::Cancelled,
        r = tx.send(lot) => match r {
            Ok(()) => FlushOutcome::Continue(rows),
            Err(_) => FlushOutcome::ReceiverGone,
        },
    }
}

/// Cuerpo de la Task: drena el flujo de [`norte_compare::compare`] y emite
/// lotes por `tx`. Lectura pura: sin journal, sin mutaciones.
///
/// - **Los providers entran por valor** y el flujo se construye AQUÍ DENTRO:
///   `CompareStream<'a>` toma prestados los DOS providers, así que el
///   préstamo tiene que nacer dentro del `async` que lo consume, no fuera.
/// - **Cancelación** (regla dura 3): el motor comprueba el token por
///   directorio y por chunk hasheado, y el `flush` lo comprueba también
///   mientras espera sitio en el canal. El único `Err` del flujo es
///   [`CompareError::Cancelled`] y significa exactamente eso: la Task acaba
///   `Cancelled`, no `Failed`. Todo fallo REAL —un directorio ilegible, uno
///   desmesurado, una lectura rota a mitad de hash— es una FILA.
/// - **Progreso**: `entries_done` cuenta las filas ENVIADAS, y se incrementa
///   al confirmarse el `flush`, no al acumular la fila en el lote. Es contrato
///   (C1): sin un `max_hits` contra el que contar, es la única señal con la
///   que un cliente detecta que se le perdió una notificación `compare.rows`,
///   así que contar filas que se quedaron en un lote descartado (por
///   cancelación, o porque el receptor desapareció) le haría denunciar una
///   pérdida que no hubo. `bytes_done` se queda a cero — con el rung de hash
///   apagado no se lee un byte, y una barra de bytes que pinta cero para
///   siempre miente más que no estar. `current` tampoco se toca: llevaría un
///   `VPath` del árbol comparado a un broadcast que ven todos los humanos
///   conectados, y el gate de esta Task es por RAÍZ.
/// - **Coalescing**: las filas se acumulan hasta [`COMPARE_ROWS_MAX_BATCH`] o
///   se drenan cada [`FLUSH_INTERVAL`], lo que ocurra antes.
///
/// # Errors
/// [`Error::Cancelled`] si se canceló. Nada más: la comparación no tiene otro
/// final prematuro, y no hay nada que limpiar porque no escribió nada.
pub async fn run_compare(
    left: Arc<dyn Provider>,
    left_root: VPath,
    right: Arc<dyn Provider>,
    right_root: VPath,
    opts: CompareOptions,
    tx: mpsc::Sender<CompareRowsBatch>,
    ctx: &crate::scheduler::TaskCtx,
) -> Result<(), Error> {
    let task_id = ctx.progress.snapshot().task_id;
    let mut batch = Batch::new(task_id);
    let mut last_flush = Instant::now();

    let sides = probed_sides(left.as_ref(), &left_root, right.as_ref(), &right_root).await;
    // Lo que este ACTOR no puede recorrer (#209): el gate de lectura del
    // daemon mira las dos RAÍCES, así que comparar `$HOME` contra otra cosa es
    // legítimo y arrastraba el directorio de estado del daemon con ello —
    // `journal.db`, los spools de sync y, con el rung de hash encendido, un
    // oráculo de igualdad sobre sus bytes. Es la mitad que #165 dejó abierta,
    // y sale del MISMO sitio que las exclusiones del walk de `fs.search`.
    let excluded = crate::policy::walk_exclusions(&ctx.actor);
    let mut stream = norte_compare::compare(
        left.as_ref(),
        &left_root,
        right.as_ref(),
        &right_root,
        opts,
        sides,
        excluded,
        ctx.cancel.clone(),
    );

    while let Some(item) = stream.next().await {
        match item {
            Ok(row) => batch.rows.push(row),
            // El ÚNICO error del flujo. Se emite una vez y el flujo termina;
            // lo acumulado se descarta (el receptor ya no lo necesita: la
            // comparación no llegó a contestar).
            Err(CompareError::Cancelled) => return Err(Error::Cancelled),
            // `CompareError` es `#[non_exhaustive]`: una variante futura NO es
            // una cancelación y no puede tratarse como tal — una Task que dice
            // `Cancelled` cuando en realidad falló miente sobre lo que pasó, y
            // un plan de sincronización que lea esas filas se lo creería.
            Err(other) => {
                tracing::error!(error = %other, "fs.compare: final inesperado del motor");
                return Err(Error::Internal { panic: false });
            }
        }
        if batch.len() >= COMPARE_ROWS_MAX_BATCH || last_flush.elapsed() >= FLUSH_INTERVAL {
            match flush(&tx, &mut batch, &ctx.cancel).await {
                FlushOutcome::Continue(rows) => {
                    ctx.progress.update(|p| p.entries_done += rows);
                    last_flush = Instant::now();
                }
                // El receptor desapareció a mitad: la comparación NO terminó,
                // y decir `Completed` haría que quien compara filas recibidas
                // contra `entries_done` diera por buena una respuesta a
                // medias. `Cancelled` es lo que de verdad pasó (lo pidiera
                // quien lo pidiera), y no hay nada que limpiar.
                FlushOutcome::ReceiverGone | FlushOutcome::Cancelled => {
                    return Err(Error::Cancelled);
                }
            }
        }
    }

    match flush(&tx, &mut batch, &ctx.cancel).await {
        FlushOutcome::Continue(rows) => {
            ctx.progress.update(|p| p.entries_done += rows);
            Ok(())
        }
        FlushOutcome::ReceiverGone | FlushOutcome::Cancelled => Err(Error::Cancelled),
    }
}
