//! Sincronización de directorios: el plan retenido y su ejecución (spec
//! `docs/superpowers/specs/2026-08-11-directory-sync-design.md`, ADR 0049).
//!
//! El PLANIFICADOR no vive aquí: es `norte-sync`, un transductor puro sobre las
//! filas de `norte-compare` que no toca un provider. Lo que vive aquí es todo
//! lo que necesita un daemon para que ese plan se pueda **aprobar** y
//! **ejecutar**:
//!
//! - [`spool`] — el plan aprobado, retenido en un fichero atado a la conexión
//!   que lo produjo. Es lo que hace que `sync.apply` no lleve más que un hash y
//!   que lo que se ejecuta sea, por la FORMA del wire, lo que un humano vio.
//! - `run_sync_plan` (privado) — la Task de `sync.plan`: mete el flujo de
//!   `norte_compare::compare` por el transductor, TEE cada elemento al spool y
//!   al lote que viaja al cliente, y cierra con un [`SyncPlanDone`].
//!
//! El ejecutor y el lote del journal llegan en la tarea 9 del plan.
//!
//! **Regla dura 4 NO aplica a `sync.plan`**: planificar no escribe un byte en
//! ninguno de los dos árboles y no tiene undo posible. Lo que escribe es
//! `sync.apply`, que sí es un lote del journal. Está dicho aquí para que una
//! revisión posterior no pida una entrada que no significaría nada.

pub mod spool;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use norte_proto::methods::{
    RelPath, SYNC_STEPS_MAX_BATCH, SyncCompareOptions, SyncPlanDone, SyncStep, SyncStepKind,
    SyncStepsBatch,
};
use norte_proto::{Error, TaskId};
use norte_sync::{PlanItem, SyncError, SyncOptions};
use norte_vfs::Provider;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub use spool::{
    PlanOutcome, SPOOL_DIR_NAME, SPOOL_FORMAT, Spool, SpoolError, SpoolHeader, SpoolReader,
    SpoolSummary, SpoolWriter, SweepReport,
};

/// Flush por tiempo del lote de pasos: el mismo intervalo (y el mismo motivo)
/// que el de `fs.compare` y `fs.search` — el diálogo gotea en vivo aunque el
/// lote no se llene.
const FLUSH_INTERVAL: Duration = Duration::from_millis(100);

/// Lo que la Task de `sync.plan` emite por su canal, en orden.
///
/// **Un solo canal para las dos notificaciones, y ese es el punto.** El
/// contrato de `sync.plan_done` es que CIERRA el plan: llega después del último
/// `sync.steps` y no antes. Con dos canales ese orden dependería de cómo el
/// runtime despierte dos receptores; con uno es la propia cola FIFO quien lo
/// garantiza, y la bomba del daemon no tiene que reordenar nada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncPlanEvent {
    /// Un lote de pasos, acotado por
    /// [`SYNC_STEPS_MAX_BATCH`].
    Steps(SyncStepsBatch),
    /// El cierre del plan. Como mucho UNO por Task, y siempre el último.
    Done(SyncPlanDone),
}

/// Filtro de [`SyncPlanParams::include`](norte_proto::methods::SyncPlanParams::include).
///
/// # Filtra la SALIDA del transductor, jamás su entrada
/// Filtrar las FILAS reabre #152: la ortografía que el destino le da a una
/// carpeta viaja en la fila de la carpeta, que es `Same` y no produce paso
/// alguno, así que un plan que solo viera las filas seleccionadas compondría
/// rutas de destino con la ortografía del ORIGEN. Está enunciado como norma en
/// el rustdoc de `norte_sync::plan` y aquí es donde se cumple: el transductor ve
/// el árbol entero y esto recorta lo que sale.
///
/// # Un ancestro seleccionado arrastra su subárbol
/// Seleccionar una carpeta en el panel de diferencias significa sincronizarla,
/// y un huérfano descendido produce un `CreateDir` más un paso por descendiente:
/// con igualdad exacta la carpeta se crearía vacía. Así que la pertenencia es
/// por PREFIJO de segmentos, nunca por prefijo de cadena (`café` no puede
/// arrastrar a `cafétière`) — la comparación se hace sobre la forma wire, que
/// separa segmentos por `/` y percent-encodea todo lo demás, así que un `/`
/// dentro de ella solo puede ser un separador.
///
/// # Los BLOQUEOS no se filtran
/// Un bloqueo no es un paso: dice por qué el plan no se puede ejecutar, y los
/// hay cuyo alcance es el árbol entero (`DestReadOnly` cuelga de la raíz, que
/// ninguna selección nombra). Recortarlos por la selección convertiría un
/// destino de solo lectura en un plan ejecutable, así que pasan todos y
/// `executable` sigue hablando de la comparación completa.
/// # Un `CreateDir` que un paso elegido necesita se queda
/// El arrastre es de arriba abajo, y el plan lo necesita también al revés. Un
/// huérbano del origen produce, en pre-orden, un `CreateDir nueva` y después un
/// `Copy nueva/a.txt`; el panel deja seleccionar la FILA del fichero. Con solo
/// el arrastre descendente el `CreateDir` se cae y queda un plan `executable`
/// cuya única copia va a un directorio que no existe — y encima rompe la regla
/// que `SyncStepsBatch::steps` publica («un `CreateDir` precede a toda copia
/// dentro de él»). Así que un `CreateDir` cuya `rel` sea ancestro ESTRICTO de
/// algo seleccionado se queda. Solo esa clase: un `DeleteTree` en un ancestro
/// borraría justo el subárbol que se pidió sincronizar.
#[derive(Debug, Clone)]
struct IncludeFilter {
    /// La raíz venía en la lista: todo entra y no hay nada que mirar.
    everything: bool,
    /// Las rutas pedidas en forma wire (lossless: percent-encoding sobre los
    /// bytes crudos, sin `to_str` y sin plegar nada — regla dura 1).
    wire: HashSet<String>,
    /// Los ancestros ESTRICTOS de lo pedido, para los `CreateDir`.
    ancestors: HashSet<String>,
}

impl IncludeFilter {
    /// Construye el filtro. Una lista VACÍA no es «todo»: es una selección de
    /// cero rutas, y produce un plan sin pasos. Quien no quiera filtrar manda
    /// el campo ausente.
    fn new(list: &[RelPath]) -> Self {
        let wire: HashSet<String> = list.iter().map(RelPath::to_wire).collect();
        // El cierre de ancestros es ≤ (rutas × profundidad), o sea acotado por
        // `SYNC_MAX_INCLUDE`: se paga una vez y deja `covers` en una consulta de
        // hash por paso.
        let mut ancestors = HashSet::new();
        for path in &wire {
            for (i, _) in path.match_indices('/') {
                ancestors.insert(path[..i].to_owned());
            }
        }
        Self {
            everything: list.iter().any(RelPath::is_root),
            wire,
            ancestors,
        }
    }

    /// ¿Cae `rel` en la selección, por sí misma o por un ancestro?
    fn covers(&self, rel: &RelPath) -> bool {
        if self.everything {
            return true;
        }
        let wire = rel.to_wire();
        if self.wire.contains(&wire) {
            return true;
        }
        // Cada `/` de la forma wire cierra exactamente un ancestro, y no hay
        // otro sitio donde pueda aparecer.
        wire.match_indices('/')
            .any(|(i, _)| self.wire.contains(&wire[..i]))
    }

    /// ¿Es `rel` un ancestro estricto de algo seleccionado? (Ver la nota del
    /// tipo: solo decide sobre un `CreateDir`.)
    fn is_needed_ancestor(&self, rel: &RelPath) -> bool {
        self.ancestors.contains(&rel.to_wire())
    }

    /// ¿Sobrevive este paso a la selección?
    fn keeps(&self, step: &SyncStep) -> bool {
        self.covers(&step.rel)
            || (step.kind == SyncStepKind::CreateDir && self.is_needed_ancestor(&step.rel))
    }
}

/// Lote de pasos en construcción.
struct Batch {
    task_id: TaskId,
    steps: Vec<SyncStep>,
}

impl Batch {
    fn new(task_id: TaskId) -> Self {
        Self {
            task_id,
            steps: Vec::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    fn len(&self) -> usize {
        self.steps.len()
    }

    /// Extrae el lote acumulado, dejando el buffer vacío.
    fn take(&mut self) -> SyncStepsBatch {
        SyncStepsBatch {
            task_id: self.task_id,
            steps: std::mem::take(&mut self.steps),
        }
    }
}

/// Desenlace de un [`flush`] (calca el de `compare.rs`).
enum FlushOutcome {
    /// Enviado (o nada que enviar): sigue el plan. Lleva CUÁNTOS pasos
    /// salieron, que es lo que cuenta el progreso.
    Continue(u64),
    /// El receptor murió (el dueño se fue): termina limpio.
    ReceiverGone,
    /// Cancelado mientras el `send` estaba bloqueado por backpressure.
    Cancelled,
}

/// Envía el lote pendiente (si lo hay). Un `send` bloqueado por backpressure NO
/// ignora la cancelación: se hace `select` contra el token (regla dura 3).
async fn flush(
    tx: &mpsc::Sender<SyncPlanEvent>,
    batch: &mut Batch,
    cancel: &CancellationToken,
) -> FlushOutcome {
    if batch.is_empty() {
        return FlushOutcome::Continue(0);
    }
    let lot = batch.take();
    let steps = u64::try_from(lot.steps.len()).unwrap_or(u64::MAX);
    tokio::select! {
        biased;
        () = cancel.cancelled() => FlushOutcome::Cancelled,
        r = tx.send(SyncPlanEvent::Steps(lot)) => match r {
            Ok(()) => FlushOutcome::Continue(steps),
            Err(_) => FlushOutcome::ReceiverGone,
        },
    }
}

/// Todo lo que la Task de `sync.plan` necesita y no puede derivar.
///
/// Es una struct y no siete argumentos porque siete argumentos son siete sitios
/// donde equivocarse de orden entre dos `Arc<dyn Provider>` que el compilador no
/// distingue.
pub(crate) struct SyncPlanJob {
    /// Provider de la raíz de ORIGEN.
    pub source: Arc<dyn Provider>,
    /// Provider de la raíz de DESTINO. Puede ser el mismo objeto.
    pub dest: Arc<dyn Provider>,
    /// Las dos raíces, el modo, `on_unknown` y las capacidades del destino.
    pub opts: SyncOptions,
    /// Con qué se compara. Va al hash y a la cabecera del spool: un plan hecho
    /// con `hash` encendido no es el mismo que uno hecho solo con tamaños.
    pub compare: SyncCompareOptions,
    /// La selección del llamante, ya validada contra
    /// [`SYNC_MAX_INCLUDE`](norte_proto::methods::SYNC_MAX_INCLUDE).
    pub include: Option<Vec<RelPath>>,
    /// EL spool del daemon (clonado, jamás construido por segunda vez).
    pub spool: Spool,
    /// La conexión dueña del plan. Es la mitad de la llave con la que después
    /// se podrá abrir.
    pub conn_id: u64,
}

/// Cuerpo de la Task de `sync.plan`: compara, transduce, retiene y emite.
///
/// El flujo es `norte_compare::compare` → `norte_sync::plan` → (spool, lote).
/// Cada elemento que sobrevive al `include` se empuja al
/// [`SpoolWriter`](spool::SpoolWriter) **y** al lote en la misma iteración: el
/// writer hashea, cuenta y escribe en la misma llamada, así que no hay forma de
/// retener una secuencia y enseñar otra (el `plan_hash` resume EXACTAMENTE lo
/// que el humano ve).
///
/// - **`descend_orphans` lo fija el llamante al lado del ORIGEN** (lo hace
///   [`Engine::sync_plan_as`](crate::Engine::sync_plan_as)), no el cliente: quien
///   aprueba necesita cuántos ficheros y cuántos bytes, y el ejecutor un paso por
///   fichero. Un huérfano del DESTINO es un `DeleteTree` entero y descenderlo
///   compra listados que no cambian un solo paso.
/// - **Cancelación** (regla dura 3): el token es el MISMO para el walk y para el
///   transductor —lo exige el rustdoc de `norte_sync::plan`—, y el `flush` lo
///   vuelve a mirar mientras espera sitio en el canal. Un plan cancelado se
///   cierra con [`PlanOutcome::Interrupted`], que borra el `.part`: **no deja
///   nada aprobable**.
/// - **Solo el brazo `None` del flujo cierra con [`PlanOutcome::Ended`]**. El
///   digest parcial de un plan cortado por la mitad es indistinguible del de uno
///   completo más corto, así que cerrarlo produciría un `plan_hash` válido para
///   un plan que dice sincronizar un árbol que se recorrió un tercio.
/// - **Un lote que no se entrega también interrumpe.** Si el dueño se fue, el
///   plan que retuviéramos sería uno que nadie llegó a ver entero; y soltar el
///   canal es además lo que para el walk.
/// - **Progreso**: `entries_done` cuenta PASOS emitidos y se incrementa al
///   confirmarse el `flush`, jamás al acumular en el lote — es la señal con la
///   que un cliente detecta un `sync.steps` perdido, y contar pasos que se
///   quedaron en un lote descartado le haría denunciar una pérdida que no hubo.
///   Con precisión: «confirmado» es que el lote ENTRÓ en el canal de la Task,
///   no que el frame llegara al cliente. Los dos números solo se separan cuando
///   la bomba del daemon no puede entregar, y ese camino termina el plan sin
///   `sync.plan_done`, así que nadie puede aprobar sobre una cuenta de más.
///   `bytes_done` se queda a cero: planificar no escribe. `current` tampoco se
///   toca (llevaría un `VPath` a un broadcast que ven todos los humanos
///   conectados, y el gate de esta Task es por RAÍZ).
///
/// # Errors
/// [`Error::Cancelled`] si se canceló o si el dueño dejó de recibir;
/// [`Error::Io`] si el spool no se pudo escribir; [`Error::Internal`] si el
/// transductor terminó de una forma que este cableado no puede producir.
#[tracing::instrument(skip_all, fields(conn_id = job.conn_id))]
pub(crate) async fn run_sync_plan(
    job: SyncPlanJob,
    tx: mpsc::Sender<SyncPlanEvent>,
    ctx: &crate::scheduler::TaskCtx,
) -> Result<(), Error> {
    let SyncPlanJob {
        source,
        dest,
        opts,
        compare,
        include,
        spool,
        conn_id,
    } = job;
    let task_id = ctx.progress.snapshot().task_id;
    let include = include.as_deref().map(IncludeFilter::new);

    let mut writer = spool
        .create(conn_id, &opts, &compare)
        .await
        .map_err(|e| spool_error(&e))?;
    let mut batch = Batch::new(task_id);
    let mut last_flush = Instant::now();

    // El flujo se construye AQUÍ DENTRO: `compare` toma prestados los DOS
    // providers, así que el préstamo tiene que nacer dentro del `async` que lo
    // consume.
    let rows = norte_compare::compare(
        source.as_ref(),
        &opts.source_root,
        dest.as_ref(),
        &opts.dest_root,
        compare_options(&compare, &opts),
        ctx.cancel.clone(),
    );
    // Fijado en la pila: el flujo del transductor no es `Unpin` (su `Unfold`
    // guarda el `async` que lo produce), y aquí se sondea desde un bucle.
    let mut items = std::pin::pin!(norte_sync::plan(rows, opts.clone(), ctx.cancel.clone()));

    // `Ok(())` = el flujo llegó a `None`. Cualquier otra cosa interrumpe, y la
    // interrupción NO puede salir por la misma puerta que el final (ver la nota
    // del tipo `PlanOutcome`).
    let ended: Result<(), Error> = loop {
        // El tick de tiempo va en un `select!` y no colgado de la llegada de un
        // paso, que es lo que hace `fs.compare`. Allí no importa —toda pareja es
        // una fila—; aquí el transductor no emite NADA por una fila `Same`, así
        // que un plan que produce tres pasos y después recorre doscientos mil
        // ficheros idénticos dejaría esos tres en el buffer durante todo el
        // walk. El flujo es `FusedStream` justamente para poder ir en un
        // `select!` (nota de la tarea 3).
        let next = tokio::select! {
            biased;
            item = items.next() => item,
            () = tokio::time::sleep_until(last_flush + FLUSH_INTERVAL) => {
                match flush(&tx, &mut batch, &ctx.cancel).await {
                    FlushOutcome::Continue(steps) => {
                        ctx.progress.update(|p| p.entries_done += steps);
                        last_flush = Instant::now();
                        continue;
                    }
                    FlushOutcome::ReceiverGone | FlushOutcome::Cancelled => {
                        break Err(Error::Cancelled);
                    }
                }
            }
        };
        let Some(item) = next else {
            break Ok(());
        };
        let item = match item {
            Ok(item) => item,
            Err(SyncError::Cancelled) => break Err(Error::Cancelled),
            // `SyncError` es `#[non_exhaustive]` y las otras variantes son
            // fallos de CABLEADO (un modo que este binario no planifica, un
            // origen que no nombra lado, una fila fuera de su raíz). Este
            // llamante construye las opciones él mismo, así que ninguna es
            // alcanzable desde el wire; decir `Cancelled` por ellas mentiría
            // sobre lo que pasó.
            // Se loguea la CLASE, no el `Display`: `OutsideRoot` y
            // `RootIsNotAStep` formatean rutas, y las rutas que llegan por esa
            // vía son exactamente las que eligió un provider que devuelve filas
            // fuera de la raíz que se le pidió listar — o sea, atacante. El
            // resto de este módulo redacta, y `read_gate` fija el criterio:
            // jamás el path en la traza.
            Err(other) => {
                tracing::error!(
                    class = sync_error_class(&other),
                    "sync.plan: final inesperado del transductor"
                );
                break Err(Error::Internal { panic: false });
            }
        };
        if let (PlanItem::Step(step), Some(filter)) = (&item, include.as_ref())
            && !filter.keeps(step)
        {
            continue;
        }
        // Hashea, cuenta y escribe en la MISMA llamada: el lote de abajo se
        // lleva exactamente lo mismo.
        if let Err(e) = writer.push(&item).await {
            tracing::error!(error = %e, "sync.plan: el spool no admitió un elemento");
            break Err(spool_error(&e));
        }
        if let PlanItem::Step(step) = item {
            batch.steps.push(step);
            if batch.len() >= SYNC_STEPS_MAX_BATCH {
                match flush(&tx, &mut batch, &ctx.cancel).await {
                    FlushOutcome::Continue(steps) => {
                        ctx.progress.update(|p| p.entries_done += steps);
                        last_flush = Instant::now();
                    }
                    // El dueño se fue: retener un plan que nadie vio entero es
                    // exactamente lo que el diálogo de aprobación existe para
                    // impedir. Y soltar el canal es lo que para el walk.
                    FlushOutcome::ReceiverGone | FlushOutcome::Cancelled => {
                        break Err(Error::Cancelled);
                    }
                }
            }
        }
    };
    if let Err(e) = ended {
        // NO `Ended`: borra el `.part` y no hay plan que aplicar.
        let _ = writer.finish(PlanOutcome::Interrupted).await;
        return Err(e);
    }
    close_plan(writer, &spool, conn_id, task_id, &mut batch, &tx, ctx).await
}

/// Cierra un plan que llegó al final de su flujo: último lote, terminador del
/// spool y `sync.plan_done`.
///
/// Es lo que decide si el plan queda RETENIDO, así que las tres formas de que no
/// deba quedarlo están juntas aquí:
///
/// 1. **El último lote no se entrega.** Un plan que nadie vio entero no se
///    aprueba, que es lo que el diálogo existe para impedir.
/// 2. **El canal está cerrado.** `flush` devuelve `Continue(0)` sin tocarlo
///    cuando el lote está vacío, así que un plan sobre dos árboles idénticos
///    —cero pasos— jamás se enteraría por esa vía.
/// 3. **`finish` dice que no.** Lo autoritativo: la conexión se desmontó
///    mientras el plan se cerraba, o alguien está aplicando un plan idéntico.
///
/// Y una cuarta, ya con el plan retenido: si el aviso no llega, se retira. El
/// plan tiene que estar cerrado ANTES de mandarlo —al revés dejaría al cliente
/// con un hash que todavía no se puede abrir—, así que la única forma de no
/// dejar un plan que nadie va a aplicar ni a recoger es deshacerlo.
async fn close_plan(
    writer: SpoolWriter,
    spool: &Spool,
    conn_id: u64,
    task_id: TaskId,
    batch: &mut Batch,
    tx: &mpsc::Sender<SyncPlanEvent>,
    ctx: &crate::scheduler::TaskCtx,
) -> Result<(), Error> {
    match flush(tx, batch, &ctx.cancel).await {
        FlushOutcome::Continue(steps) => ctx.progress.update(|p| p.entries_done += steps),
        FlushOutcome::ReceiverGone | FlushOutcome::Cancelled => {
            let _ = writer.finish(PlanOutcome::Interrupted).await;
            return Err(Error::Cancelled);
        }
    }
    if tx.is_closed() {
        let _ = writer.finish(PlanOutcome::Interrupted).await;
        return Err(Error::Cancelled);
    }
    let summary = match writer.finish(PlanOutcome::Ended).await {
        Ok(summary) => summary,
        Err(SpoolError::Interrupted) => return Err(Error::Cancelled),
        Err(e) => return Err(spool_error(&e)),
    };
    // Se CONSTRUYE desde el resumen, no se recalcula: `executable` y los
    // contadores se derivan en un solo sitio ([`SpoolWriter::finish`]), y quien
    // los derive por su cuenta tarde o temprano los derivará distinto.
    let plan_hash = summary.plan_hash.clone();
    let done = SyncPlanDone {
        task_id,
        plan_hash: summary.plan_hash,
        counts: summary.counts,
        blockers: summary.blockers,
        blockers_total: summary.blockers_total,
        executable: summary.executable,
    };
    // El dueño podría recalcular el digest por su cuenta —`PlanHasher` no lleva
    // clave y recibió todos los lotes—, así que «nadie sabe el hash» no es la
    // razón por la que esto es seguro: la razón es que el plan deja de estar
    // retenido, y que `sync.apply` sigue exigiendo scope de escritura.
    if tx.send(SyncPlanEvent::Done(done)).await.is_err() {
        tracing::debug!(
            conn = conn_id,
            "sync.plan_done sin dueño: se retira el plan"
        );
        let _ = spool.remove(conn_id, &plan_hash).await;
        return Err(Error::Cancelled);
    }
    Ok(())
}

/// La CLASE de un fallo del transductor, para la traza.
///
/// Existe para no formatear el error: `OutsideRoot` y `RootIsNotAStep` llevan
/// rutas en su `Display`, y las rutas que llegan por ahí son las que eligió un
/// provider que devuelve filas fuera de la raíz que se le pidió listar. Un
/// vocabulario cerrado dice lo mismo para diagnosticar y no escribe en el log de
/// un operador algo que no controla (regla 10, mismo criterio que `read_gate`).
fn sync_error_class(e: &SyncError) -> &'static str {
    match e {
        SyncError::Cancelled => "cancelled",
        SyncError::SourceSideUnknown => "source-side-unknown",
        SyncError::OutsideRoot { .. } => "outside-root",
        SyncError::RootIsNotAStep { .. } => "root-is-not-a-step",
        SyncError::ModeNotPlanned(_) => "mode-not-planned",
        SyncError::Compare(_) => "compare",
        // `SyncError` es `#[non_exhaustive]`.
        _ => "unknown",
    }
}

/// Traduce el fallo del spool a la taxonomía del wire.
///
/// Solo [`SpoolError::Io`] es un fallo de I/O de verdad; lo demás, en el camino
/// de ESCRITURA, solo puede ser un writer ya cerrado o un registro por encima
/// del tope — un fallo de este core, no del disco.
fn spool_error(e: &SpoolError) -> Error {
    match e {
        SpoolError::Io(_) => Error::Io { retryable: false },
        _ => Error::Internal { panic: false },
    }
}

/// Las opciones del MOTOR de comparación a partir de las del wire.
///
/// `follow_symlinks` y `descend_orphans` no son del llamante en `sync.plan`
/// (rechazados aguas arriba con `-32602`): aquí se fijan, y `descend_orphans` al
/// lado del ORIGEN.
fn compare_options(wire: &SyncCompareOptions, opts: &SyncOptions) -> norte_compare::CompareOptions {
    norte_compare::CompareOptions {
        criteria: wire.criteria,
        max_depth: wire.max_depth,
        mtime_tolerance_ms: wire.mtime_tolerance_ms,
        follow_symlinks: false,
        descend_orphans: Some(opts.source_side),
    }
}

#[cfg(test)]
mod tests {
    use norte_proto::methods::{RelPath, SyncStepKind};

    use super::IncludeFilter;

    fn rel(wire: &str) -> RelPath {
        RelPath::parse_wire(wire).expect("rel")
    }

    /// Un paso mínimo: solo `kind` y `rel` importan para el filtro.
    fn step(kind: norte_proto::methods::SyncStepKind, wire: &str) -> super::SyncStep {
        super::SyncStep {
            id: 1,
            kind,
            rel: rel(wire),
            dest_rel: None,
            size: None,
            criterion: norte_proto::methods::CompareCriterion::Presence,
            confidence: norte_proto::methods::CompareConfidence::Certain,
            reversal: Some(norte_proto::methods::StepReversal::Delete),
            reason: None,
        }
    }

    #[test]
    fn una_seleccion_arrastra_su_subarbol_y_no_a_su_vecino() {
        let f = IncludeFilter::new(&[rel("caf%C3%A9")]);
        assert!(f.covers(&rel("caf%C3%A9")), "la propia carpeta");
        assert!(f.covers(&rel("caf%C3%A9/x.txt")), "lo de dentro");
        // Por prefijo de CADENA, `cafétière` colgaría de `café` (regla dura 1).
        assert!(!f.covers(&rel("caf%C3%A9ti%C3%A8re")));
        assert!(!f.covers(&rel("otra")));
    }

    #[test]
    fn la_raiz_en_la_lista_lo_incluye_todo_y_la_lista_vacia_nada() {
        let todo = IncludeFilter::new(&[RelPath::default()]);
        assert!(todo.covers(&rel("a/b/c")));
        let nada = IncludeFilter::new(&[]);
        assert!(!nada.covers(&rel("a")));
        assert!(!nada.covers(&RelPath::default()));
    }

    #[test]
    fn la_pertenencia_es_por_bytes_sin_plegar_ni_normalizar() {
        // NFC en la lista, NFD en el paso: dos nombres distintos.
        let f = IncludeFilter::new(&[rel("caf%C3%A9")]);
        assert!(!f.covers(&rel("cafe%CC%81")));
        // Y la caja tampoco se pliega.
        let g = IncludeFilter::new(&[rel("README")]);
        assert!(!g.covers(&rel("readme")));
    }

    #[test]
    fn seleccionar_un_fichero_conserva_el_createdir_que_necesita() {
        // El panel deja elegir la FILA del fichero; sin su `CreateDir` el plan
        // copiaría dentro de un directorio que no existe.
        let f = IncludeFilter::new(&[rel("nueva/a.txt")]);
        assert!(f.keeps(&step(SyncStepKind::CreateDir, "nueva")));
        assert!(f.keeps(&step(SyncStepKind::Copy, "nueva/a.txt")));
        // Pero SOLO esa clase: un borrado del ancestro se llevaría por delante
        // justo lo que se pidió sincronizar.
        assert!(!f.keeps(&step(SyncStepKind::DeleteTree, "nueva")));
        // Y el ancestro tiene que ser ancestro de ALGO elegido.
        assert!(!f.keeps(&step(SyncStepKind::CreateDir, "otra")));
    }

    #[test]
    fn un_nombre_que_no_es_utf8_entra_por_su_forma_wire() {
        let f = IncludeFilter::new(&[rel("informe%FF%FE.dat")]);
        assert!(f.covers(&rel("informe%FF%FE.dat")));
        assert!(!f.covers(&rel("informe%FE%FF.dat")));
    }
}
