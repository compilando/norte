//! El transductor: filas de comparación entran, pasos de plan salen.
//!
//! Una fila produce CERO o UN elemento —de momento; el modo `Mirror` y los
//! bloqueos de la tarea 5 no cambian esa forma, solo la tabla—. Dos árboles
//! idénticos producen cero pasos, no un millón de `Skip`: lo NOTABLE se emite,
//! lo aburrido no.
//!
//! El orden es el del walk, que es pre-orden, así que un `CreateDir` precede
//! siempre a lo que va dentro de él sin que aquí haya que ordenar nada.

use std::collections::VecDeque;
use std::pin::Pin;

use futures::stream::{self, FusedStream, Stream, StreamExt};
use norte_compare::CompareError;
use norte_proto::methods::{
    CompareRow, CompareVerdict, RelPath, Side, StepReversal, SyncBlocker, SyncMode, SyncReason,
    SyncStep, SyncStepKind,
};
use norte_proto::{Entry, EntryKind, Segment, VPath};
use tokio_util::sync::CancellationToken;

use crate::{SyncError, SyncOptions};

/// Lo que el flujo del plan lleva: un paso que se va a ejecutar, o una razón
/// por la que el plan entero no se puede aprobar.
///
/// Los dos van por el MISMO flujo y no por dos canales: un bloqueo aparece
/// donde el walk lo encontró, así que el panel puede enseñarlo en su sitio, y
/// quien acumula el plan no tiene que reconciliar dos secuencias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanItem {
    /// Un paso del plan.
    Step(SyncStep),
    /// Una razón para que el plan no sea ejecutable.
    Blocker(SyncBlocker),
}

/// Planifica: transduce el flujo de filas en un flujo de pasos.
///
/// `rows` es el flujo que devuelve [`norte_compare::compare`] sobre las MISMAS
/// dos raíces que trae `opts`. Nada en la firma lo puede exigir y todo depende
/// de ello: si las raíces no coinciden byte a byte, cada `rel` que salga de
/// aquí nombra otra cosa (de ahí [`SyncError::OutsideRoot`] y
/// [`SyncError::RootIsNotAStep`], que es como se entera).
///
/// `cancel` es el token de la Task (regla dura 3) y debe ser **el mismo** que
/// se le dio a la comparación: se comprueba una vez POR FILA, antes de pedir la
/// siguiente, así que un token distinto no corta nada hasta que aguas arriba
/// produzca. Con el mismo token, el corte es inmediato porque el walk también
/// lo ve.
///
/// No abre nada, no lista nada y no escribe nada.
///
/// El flujo está FUSIONADO ([`FusedStream`]): pedirle otro elemento después del
/// final devuelve `None` en vez de entrar en pánico, que es lo que hace el
/// `Unfold` crudo de `futures`. Un `select!` sobre él es legal.
///
/// El flujo termina en cuanto emite un [`Err`]: una cancelación, o un fallo del
/// llamante ([`SyncError::SourceSideUnknown`], [`SyncError::OutsideRoot`],
/// [`SyncError::RootIsNotAStep`], [`SyncError::ModeNotPlanned`]). Todo lo demás
/// que sale mal en un árbol es un paso o un bloqueo.
///
/// # Un plan no es atómico mientras se produce
/// Los pasos salen ANTES de que se sepa si el plan va a morir, así que quien
/// los acumule verá pasos de un plan que termina en error. Es inocuo porque
/// entonces no llega `sync.plan_done` y `sync.apply` no lleva nada más que un
/// `plan_hash` que nunca se emitió — pero quien guarde los pasos no puede
/// suponer lo contrario.
///
/// # La suposición que este transductor hereda y no puede comprobar
/// `rel` sale de los bytes del ORIGEN. `norte-compare` empareja por una clave
/// PLEGADA —NFC siempre, mayúsculas cuando alguno de los dos lados no
/// distingue— así que dos entradas con bytes DISTINTOS pueden emparejarse sin
/// que la fila lo diga (`reason: None`). Cuando eso pasa, el `rel` de un
/// `Overwrite` no nombra la entrada del destino: pegado sobre `dest_root` crea
/// un fichero NUEVO al lado del que se quería sobrescribir, y la reversa
/// prometida no se cumple porque no se enterró nada. Arreglarlo pide un campo
/// más en el paso (el nombre del destino) o un motivo nuevo para saltarlo: las
/// dos cosas son decisiones de wire, y están anotadas en
/// <https://github.com/compilando/norte/issues/152>.
///
/// ```
/// use futures::StreamExt;
/// use norte_proto::VPath;
/// use norte_proto::methods::{
///     CompareConfidence, CompareCriterion, CompareRow, CompareVerdict,
/// };
/// use norte_proto::{Entry, EntryKind};
/// use norte_sync::{OnUnknown, PlanItem, Side, SyncMode, SyncOptions, SyncStepKind, plan};
/// use tokio_util::sync::CancellationToken;
///
/// let opts = SyncOptions {
///     source_root: VPath::parse("file:///origen").expect("path"),
///     dest_root: VPath::parse("file:///destino").expect("path"),
///     mode: SyncMode::Update,
///     on_unknown: OnUnknown::Copy,
///     source_side: Side::Left,
///     dest_has_trash: true,
///     dest_writable: true,
/// };
/// let fila = CompareRow {
///     id: 0,
///     left: Some(Entry {
///         path: VPath::parse("file:///origen/informe%FF%FE.dat").expect("path"),
///         kind: EntryKind::File,
///         size: Some(1234),
///         mtime_ms: None,
///         attrs: std::collections::BTreeMap::default(),
///     }),
///     right: None,
///     verdict: CompareVerdict::OnlyLeft,
///     criterion: CompareCriterion::Presence,
///     confidence: CompareConfidence::Certain,
///     newer: None,
///     reason: None,
///     side: None,
/// };
///
/// let items: Vec<_> = futures::executor::block_on(
///     plan(futures::stream::iter(vec![Ok(fila)]), opts, CancellationToken::new()).collect(),
/// );
/// let PlanItem::Step(paso) = items[0].as_ref().expect("sin error") else {
///     panic!("un paso");
/// };
/// assert_eq!(paso.kind, SyncStepKind::Copy);
/// // Y el nombre no-UTF8 llega entero, relativo a las raíces (regla dura 1).
/// assert_eq!(paso.rel.to_wire(), "informe%FF%FE.dat");
/// ```
pub fn plan<'a, S>(
    rows: S,
    opts: SyncOptions,
    cancel: CancellationToken,
) -> impl FusedStream<Item = Result<PlanItem, SyncError>> + 'a
where
    S: Stream<Item = Result<CompareRow, CompareError>> + 'a,
{
    let transducer = Transducer {
        rows: Box::pin(rows),
        opts,
        cancel,
        pending: VecDeque::new(),
        next_id: 0,
        finished: false,
    };
    stream::unfold(transducer, |mut t| async move {
        let item = t.step().await?;
        Some((item, t))
    })
    // `Unfold` no es fusionable por sí solo y `FusedStream` no es un auto-trait
    // que se filtre por el `impl Trait`: sin esto, un llamante que lo sondee una
    // vez de más —lo hace cualquier bucle con `select!` y un `tick` de flush—
    // se lleva un pánico DESPUÉS de haber planificado bien.
    .fuse()
}

/// El estado del transductor: el flujo de entrada, lo que una fila dejó a
/// medio emitir y el contador de `id`.
struct Transducer<S> {
    /// El flujo de filas, ya fijado (`Box::pin`) para poder pedirle la
    /// siguiente sin exigirle `Unpin` al llamante.
    rows: Pin<Box<S>>,
    /// Lo que el plan necesita y las filas no traen.
    opts: SyncOptions,
    /// El token de la Task.
    cancel: CancellationToken,
    /// Lo que la última fila produjo y aún no se ha entregado.
    pending: VecDeque<PlanItem>,
    /// El siguiente `id` de paso. Monótono dentro de UN plan.
    next_id: u64,
    /// Una vez `true` el flujo no vuelve a producir nada.
    finished: bool,
}

impl<S> Transducer<S>
where
    S: Stream<Item = Result<CompareRow, CompareError>>,
{
    /// El siguiente elemento del plan, o `None` cuando se acabó.
    async fn step(&mut self) -> Option<Result<PlanItem, SyncError>> {
        loop {
            if self.finished {
                return None;
            }
            // La comprobación va ANTES de vaciar lo pendiente, y lo pendiente se
            // tira: ningún paso sale después del corte, ni siquiera uno ya
            // calculado. Es el mismo contrato que `norte_compare::walk`, y
            // empieza a importar en cuanto una fila produzca más de un elemento
            // (tarea 5: un `Skip` y un bloqueo de la misma fila).
            if self.cancel.is_cancelled() {
                self.finished = true;
                self.pending.clear();
                return Some(Err(SyncError::Cancelled));
            }
            if let Some(item) = self.pending.pop_front() {
                return Some(Ok(item));
            }
            match self.rows.next().await {
                None => {
                    self.finished = true;
                    return None;
                }
                Some(Err(CompareError::Cancelled)) => {
                    self.finished = true;
                    return Some(Err(SyncError::Cancelled));
                }
                Some(Err(other)) => {
                    self.finished = true;
                    return Some(Err(SyncError::Compare(other)));
                }
                Some(Ok(row)) => {
                    if let Err(e) = self.absorb(&row) {
                        self.finished = true;
                        return Some(Err(e));
                    }
                }
            }
        }
    }

    /// Traduce UNA fila a cero o más elementos en `pending`.
    fn absorb(&mut self, row: &CompareRow) -> Result<(), SyncError> {
        // `SyncMode` es `#[non_exhaustive]`, así que el comodín es obligatorio;
        // y es justo lo que hace falta, porque un modo que este planificador no
        // sabe planificar NO puede degradar al que sí sabe.
        if self.opts.mode != SyncMode::Update {
            return Err(SyncError::ModeNotPlanned(self.opts.mode));
        }
        let (source_orphan, dest_orphan) = match self.opts.source_side {
            Side::Left => (CompareVerdict::OnlyLeft, CompareVerdict::OnlyRight),
            Side::Right => (CompareVerdict::OnlyRight, CompareVerdict::OnlyLeft),
            Side::Unknown => return Err(SyncError::SourceSideUnknown),
        };
        let source: Option<&Entry> = match self.opts.source_side {
            Side::Left => row.left.as_ref(),
            _ => row.right.as_ref(),
        };

        let kind = if row.verdict == source_orphan {
            let Some(entry) = source else {
                // La fila se contradice a sí misma (ver
                // `CompareRow::sides_are_consistent`): dice «solo en el origen»
                // y no trae la entrada del origen. No hay nada que copiar y no
                // hay `rel` que calcular, así que no produce paso. Las filas de
                // `norte-compare` nunca son así.
                return Ok(());
            };
            if entry.kind == EntryKind::Dir {
                // Un directorio huérfano es UN `CreateDir`, jamás una copia
                // recursiva: lo que hay dentro llega en sus propias filas
                // cuando la comparación se pidió con `descend_orphans`, y si no
                // se pidió, el plan dice exactamente lo que va a hacer.
                SyncStepKind::CreateDir
            } else {
                SyncStepKind::Copy
            }
        } else if row.verdict == dest_orphan {
            // Lo que sobra en el destino es asunto de `Mirror` (tarea 5). Bajo
            // `Update` no se borra nada, y ese es el motivo de que el modo
            // exista.
            return Ok(());
        } else {
            match row.verdict {
                // OJO, deuda conocida: un `TypeMismatch` cuyo ORIGEN es un
                // directorio (un `build/` contra un `build` fichero) sale
                // también como `Overwrite`, y `Overwrite` significa
                // normativamente «a la papelera y COPIAR bytes». El paso no
                // lleva `EntryKind`, así que el ejecutor no lo puede distinguir,
                // y el walk no desciende un par no-directorio: el subárbol
                // entero se queda fuera del plan. Decirlo bien pide vocabulario
                // que el wire 0.40.0 no tiene (un paso «borra y crea
                // directorio», o el `EntryKind` en el paso). Se pinta aquí y se
                // fija con un test para que la tarea 9 no lo herede sin saberlo.
                CompareVerdict::Different | CompareVerdict::TypeMismatch => SyncStepKind::Overwrite,
                // `Same` no produce nada. `Ambiguous`, `Error` y `Unknown`
                // todavía no: son de las tareas 4 y 5.
                _ => return Ok(()),
            }
        };

        let Some(entry) = source else {
            // `Different`/`TypeMismatch` sin lado de origen: la misma
            // contradicción que arriba.
            return Ok(());
        };
        let rel = rel_under(&self.opts.source_root, &entry.path)?;
        if rel.is_root() {
            // La fila NOMBRA la raíz, no algo bajo ella. Un paso así actúa sobre
            // el árbol entero del destino.
            return Err(SyncError::RootIsNotAStep {
                root: Box::new(self.opts.source_root.clone()),
            });
        }
        // El tamaño se copia TAL CUAL: un provider perezoso —`file://` entre
        // ellos— lista sin él, y un cero fingido en el diálogo de aprobación
        // es peor que un «no se sabe» (ADR 0048; los contadores lo suman
        // aparte).
        //
        // La guarda mira la clase de la ENTRADA y no la del paso: un directorio
        // no mueve bytes lo llame el plan `CreateDir` o lo llame `Overwrite`
        // (un `TypeMismatch` con directorio en el origen es lo segundo), y el
        // `size` que un provider le ponga a un directorio no son bytes que se
        // vayan a escribir.
        let size = if entry.kind == EntryKind::Dir {
            None
        } else {
            entry.size
        };
        let (reversal, reason) = reversal_for(kind, self.opts.dest_has_trash);
        self.pending.push_back(PlanItem::Step(SyncStep {
            id: self.next_id,
            kind,
            rel,
            size,
            criterion: row.criterion,
            confidence: row.confidence,
            reversal,
            reason,
        }));
        self.next_id += 1;
        Ok(())
    }
}

/// Cómo vuelve atrás un paso, en función de su clase y de si el DESTINO tiene
/// papelera.
///
/// Un `CreateDir` y un `Copy` no destruyen nada, así que se deshacen borrando
/// lo que crearon, con papelera o sin ella. Un `Overwrite` y un `DeleteTree`
/// entierran algo: con papelera se saca de ella, sin papelera no se saca de
/// ningún sitio y el plan tiene que decirlo ANTES de que nadie lo apruebe
/// (regla dura 4).
fn reversal_for(
    kind: SyncStepKind,
    dest_has_trash: bool,
) -> (Option<StepReversal>, Option<SyncReason>) {
    match kind {
        SyncStepKind::CreateDir | SyncStepKind::Copy => (Some(StepReversal::Delete), None),
        SyncStepKind::Overwrite | SyncStepKind::DeleteTree => {
            if dest_has_trash {
                (Some(StepReversal::RestoreTrash), None)
            } else {
                (
                    Some(StepReversal::Irreversible),
                    Some(SyncReason::NoTrashOnTarget),
                )
            }
        }
        // Un `Skip` no tiene reversa (no hizo nada) y su motivo lo pone quien
        // lo emite, que es el único que lo sabe. `Unknown` no lo emite este
        // crate nunca.
        _ => (None, None),
    }
}

/// La ruta de `path` RELATIVA a `root`, en bytes.
///
/// Compara scheme, authority y luego los segmentos UNO A UNO por sus bytes
/// crudos: sin `to_str`, sin lossy, sin normalizar y sin plegar mayúsculas
/// (regla dura 1). Que la comparación sea por SEGMENTOS y no por prefijo de
/// cadena es lo que impide que `…/ab` cuelgue de `…/a`.
///
/// La authority también se compara BYTE A BYTE, y es deliberado aunque un
/// hostname DNS no distinga mayúsculas: para `mem://` y para un id de conexión
/// de object storage la authority es un testigo opaco, y plegarla juntaría dos
/// conexiones distintas. `sftp://NAS/…` contra `sftp://nas/…` falla, y falla
/// CERRADO —un plan menos, nunca una escritura de más—.
///
/// El resultado no se puede escapar de la raíz porque el tipo no lo permite:
/// [`Segment`] rechaza `/`, `.`, `..` y el NUL, y los bytes que aquí entran
/// salieron ya validados de un [`VPath`], así que ni se pierde ni se gana
/// ninguno. Lo que SÍ puede devolver es la raíz misma (`path == root`), que no
/// es un escape hacia arriba pero sí el blanco más destructivo del plan: lo
/// rechaza quien lo llama, que es el único que sabe si un `rel` vacío tiene
/// sentido (un `Skip` sí, un `Overwrite` no).
///
/// Los bytes no se copian de más: se recorren los dos iteradores en paralelo,
/// sin materializar ningún `Vec` de segmentos por fila.
fn rel_under(root: &VPath, path: &VPath) -> Result<RelPath, SyncError> {
    let outside = || SyncError::OutsideRoot {
        root: Box::new(root.clone()),
        path: Box::new(path.clone()),
    };
    if path.scheme() != root.scheme() || path.authority() != root.authority() {
        return Err(outside());
    }
    let mut rest = path.segments();
    for root_segment in root.segments() {
        if rest.next() != Some(root_segment) {
            return Err(outside());
        }
    }
    let rest = rest
        .map(Segment::new)
        .collect::<Result<Vec<_>, _>>()
        // Inalcanzable: cada uno de estos bytes salió de un `Segment` que un
        // `VPath` ya validó. Se mapea en vez de `expect` (regla dura 6).
        .map_err(|_| outside())?;
    Ok(RelPath::new(rest))
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use norte_proto::methods::{
        CompareConfidence, CompareCriterion, OnUnknown, SyncMode, SyncStepKind,
    };

    use super::*;

    fn vpath(wire: &str) -> VPath {
        VPath::parse(wire).expect("path")
    }

    fn source_root() -> VPath {
        vpath("file:///origen")
    }

    fn dest_root() -> VPath {
        vpath("file:///destino")
    }

    fn opts_update() -> SyncOptions {
        SyncOptions {
            source_root: source_root(),
            dest_root: dest_root(),
            mode: SyncMode::Update,
            on_unknown: OnUnknown::Copy,
            source_side: Side::Left,
            dest_has_trash: true,
            dest_writable: true,
        }
    }

    fn entry_at(root: &VPath, name: &[u8], kind: EntryKind, size: Option<u64>) -> Entry {
        Entry {
            path: root.join(Segment::new(name).expect("segment")),
            kind,
            size,
            mtime_ms: None,
            attrs: std::collections::BTreeMap::default(),
        }
    }

    fn src_file(name: &str, size: u64) -> Entry {
        entry_at(&source_root(), name.as_bytes(), EntryKind::File, Some(size))
    }

    fn dst_file(name: &str, size: u64) -> Entry {
        entry_at(&dest_root(), name.as_bytes(), EntryKind::File, Some(size))
    }

    fn src_dir(name: &str) -> Entry {
        entry_at(&source_root(), name.as_bytes(), EntryKind::Dir, None)
    }

    fn dst_dir(name: &str) -> Entry {
        entry_at(&dest_root(), name.as_bytes(), EntryKind::Dir, None)
    }

    fn row(
        verdict: CompareVerdict,
        criterion: CompareCriterion,
        confidence: CompareConfidence,
        left: Option<Entry>,
        right: Option<Entry>,
    ) -> CompareRow {
        CompareRow {
            id: 0,
            left,
            right,
            verdict,
            criterion,
            confidence,
            newer: None,
            reason: None,
            side: None,
        }
    }

    fn rel(wire: &str) -> RelPath {
        RelPath::parse_wire(wire).expect("rel")
    }

    async fn run_raw(
        rows: Vec<CompareRow>,
        opts: SyncOptions,
        cancel: CancellationToken,
    ) -> Vec<Result<PlanItem, SyncError>> {
        plan(stream::iter(rows.into_iter().map(Ok)), opts, cancel)
            .collect()
            .await
    }

    async fn run(rows: Vec<CompareRow>, opts: SyncOptions) -> Vec<PlanItem> {
        run_raw(rows, opts, CancellationToken::new())
            .await
            .into_iter()
            .map(|r| r.expect("sin error"))
            .collect()
    }

    fn steps_of(items: &[PlanItem]) -> Vec<&SyncStep> {
        items
            .iter()
            .filter_map(|i| match i {
                PlanItem::Step(s) => Some(s),
                PlanItem::Blocker(_) => None,
            })
            .collect()
    }

    fn one_step(items: &[PlanItem]) -> &SyncStep {
        let steps = steps_of(items);
        assert_eq!(steps.len(), 1, "se esperaba UN paso: {items:?}");
        steps[0]
    }

    #[tokio::test]
    async fn only_on_the_source_becomes_a_copy() {
        let items = run(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(src_file("a.txt", 10)),
                None,
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Copy);
        assert_eq!(s.rel, rel("a.txt"));
        assert_eq!(s.size, Some(10));
        assert_eq!(s.reversal, Some(StepReversal::Delete));
        assert_eq!(s.reason, None);
        assert_eq!(s.criterion, CompareCriterion::Presence);
        assert!(s.shape_is_consistent());
    }

    #[tokio::test]
    async fn a_directory_only_on_the_source_becomes_create_dir() {
        let items = run(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(src_dir("sub")),
                None,
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::CreateDir);
        assert_eq!(s.rel, rel("sub"));
        assert_eq!(s.reversal, Some(StepReversal::Delete));
        assert_eq!(s.size, None, "crear un directorio no mueve bytes");
    }

    #[tokio::test]
    async fn a_descended_orphan_is_one_step_per_row_and_never_a_recursive_copy() {
        // Lo que la tarea 2 hace posible: el contenedor primero y sus hijos
        // detrás, cada uno en su fila. El plan no «optimiza» el directorio a
        // una copia recursiva — el ejecutor journaliza y aísla fallos POR PASO.
        let deep = Entry {
            path: source_root()
                .join(Segment::new(b"sub".to_vec()).expect("segment"))
                .join(Segment::new(b"1.txt".to_vec()).expect("segment")),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
            attrs: std::collections::BTreeMap::default(),
        };
        let items = run(
            vec![
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(src_dir("sub")),
                    None,
                ),
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(deep),
                    None,
                ),
            ],
            opts_update(),
        )
        .await;
        let steps = steps_of(&items);
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].kind, SyncStepKind::CreateDir);
        assert_eq!(steps[1].kind, SyncStepKind::Copy);
        assert_eq!(
            steps[1].rel,
            rel("sub/1.txt"),
            "el rel lleva los dos niveles"
        );
        assert_eq!(
            steps[1].size, None,
            "una fila huérfana no se hidrata: `None` viaja como `None`, jamás como 0"
        );
        assert_eq!(steps[0].id, 0);
        assert_eq!(steps[1].id, 1, "los id son monótonos dentro del plan");
    }

    #[tokio::test]
    async fn different_becomes_overwrite_and_keeps_the_criterion_that_decided_it() {
        let items = run(
            vec![row(
                CompareVerdict::Different,
                CompareCriterion::Mtime,
                CompareConfidence::Probable,
                Some(src_file("a.txt", 10)),
                Some(dst_file("a.txt", 9)),
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Overwrite);
        assert_eq!(s.criterion, CompareCriterion::Mtime);
        assert_eq!(
            s.confidence,
            CompareConfidence::Probable,
            "el informe tiene que poder decir POR QUÉ sobrescribió"
        );
        assert_eq!(s.size, Some(10), "los bytes son los del ORIGEN");
        assert_eq!(s.reversal, Some(StepReversal::RestoreTrash));
        assert!(s.shape_is_consistent());
    }

    #[tokio::test]
    async fn an_overwrite_without_a_trash_is_irreversible_and_says_why() {
        let opts = SyncOptions {
            dest_has_trash: false,
            ..opts_update()
        };
        let items = run(
            vec![row(
                CompareVerdict::Different,
                CompareCriterion::Size,
                CompareConfidence::Certain,
                Some(src_file("a.txt", 10)),
                Some(dst_file("a.txt", 9)),
            )],
            opts,
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.reversal, Some(StepReversal::Irreversible));
        assert_eq!(s.reason, Some(SyncReason::NoTrashOnTarget));
        assert!(s.shape_is_consistent());
    }

    #[tokio::test]
    async fn a_copy_is_reversible_even_without_a_trash() {
        let opts = SyncOptions {
            dest_has_trash: false,
            ..opts_update()
        };
        let items = run(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(src_file("a.txt", 10)),
                None,
            )],
            opts,
        )
        .await;
        // No se destruyó nada: deshacer es borrar lo que se creó.
        assert_eq!(one_step(&items).reversal, Some(StepReversal::Delete));
    }

    #[tokio::test]
    async fn same_produces_nothing_at_all() {
        let items = run(
            vec![row(
                CompareVerdict::Same,
                CompareCriterion::Hash,
                CompareConfidence::Certain,
                Some(src_file("a.txt", 10)),
                Some(dst_file("a.txt", 10)),
            )],
            opts_update(),
        )
        .await;
        assert!(
            items.is_empty(),
            "un árbol idéntico no puede producir un millón de no-ops"
        );
    }

    #[tokio::test]
    async fn only_on_the_destination_produces_nothing_under_update() {
        let items = run(
            vec![row(
                CompareVerdict::OnlyRight,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                None,
                Some(dst_file("gone.txt", 3)),
            )],
            opts_update(),
        )
        .await;
        assert!(items.is_empty(), "`Update` no borra nunca");
    }

    #[tokio::test]
    async fn a_type_mismatch_overwrites_and_says_so() {
        let items = run(
            vec![row(
                CompareVerdict::TypeMismatch,
                CompareCriterion::Kind,
                CompareConfidence::Certain,
                Some(src_file("x", 1)),
                Some(dst_dir("x")),
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Overwrite);
        assert_eq!(s.criterion, CompareCriterion::Kind);
    }

    #[tokio::test]
    async fn the_source_can_be_the_right_side() {
        // La dirección la tradujo el frontend UNA vez; aquí es un hecho, y el
        // espejo tiene que dar el mismo plan con los lados cambiados.
        let opts = SyncOptions {
            source_root: dest_root(),
            dest_root: source_root(),
            source_side: Side::Right,
            ..opts_update()
        };
        let items = run(
            vec![
                row(
                    CompareVerdict::OnlyRight,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    None,
                    Some(dst_file("a.txt", 10)),
                ),
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(src_file("b.txt", 3)),
                    None,
                ),
            ],
            opts,
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Copy);
        assert_eq!(s.rel, rel("a.txt"));
    }

    #[tokio::test]
    async fn rel_is_relative_to_the_roots_and_keeps_every_byte() {
        // El corpus hostil ENTERO, no un nombre de muestra: si derivar el `rel`
        // pierde o traduce un byte, se ve aquí (regla dura 1).
        for name in norte_testkit::corpus::hostile_names() {
            let entry = entry_at(&source_root(), &name.bytes, EntryKind::File, Some(1));
            let items = run(
                vec![row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(entry),
                    None,
                )],
                opts_update(),
            )
            .await;
            let s = one_step(&items);
            assert_eq!(s.rel.segments().len(), 1, "{}", name.id);
            assert_eq!(
                s.rel.segments()[0].as_bytes(),
                name.bytes.as_slice(),
                "{} perdió bytes al hacerse relativo",
                name.id
            );
            // Y sigue siendo un `rel` legal en el wire, ida y vuelta.
            let back = RelPath::parse_wire(&s.rel.to_wire()).expect("wire");
            assert_eq!(back, s.rel, "{}", name.id);
        }
    }

    #[tokio::test]
    async fn a_root_whose_name_is_a_prefix_of_another_is_not_a_root() {
        // `…/origen2/x` NO cuelga de `…/origen`: se compara por SEGMENTOS, no
        // por prefijo de cadena.
        let intruso = entry_at(
            &vpath("file:///origen2"),
            b"x.txt",
            EntryKind::File,
            Some(1),
        );
        let out = run_raw(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(intruso),
                None,
            )],
            opts_update(),
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(
            out.as_slice(),
            [Err(SyncError::OutsideRoot { .. })]
        ));
    }

    #[tokio::test]
    async fn a_path_from_another_provider_is_not_under_the_root() {
        let ajeno = entry_at(&vpath("sftp://nas/origen"), b"x.txt", EntryKind::File, None);
        let out = run_raw(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(ajeno),
                None,
            )],
            opts_update(),
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(
            out.as_slice(),
            [Err(SyncError::OutsideRoot { .. })]
        ));
    }

    #[tokio::test]
    async fn a_row_that_contradicts_its_own_verdict_produces_nothing() {
        // «Solo en el origen» sin entrada de origen: no hay qué copiar ni de
        // dónde sacar un `rel`. Ninguna fila de `norte-compare` es así.
        let items = run(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                None,
                None,
            )],
            opts_update(),
        )
        .await;
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn a_source_side_that_names_no_side_ends_the_plan() {
        let opts = SyncOptions {
            source_side: Side::Unknown,
            ..opts_update()
        };
        let out = run_raw(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(src_file("a.txt", 1)),
                None,
            )],
            opts,
            CancellationToken::new(),
        )
        .await;
        assert_eq!(out, vec![Err(SyncError::SourceSideUnknown)]);
    }

    #[tokio::test]
    async fn cancellation_ends_the_stream_with_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let rows: Vec<_> = (0..64)
            .map(|_| {
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(src_file("a.txt", 1)),
                    None,
                )
            })
            .collect();
        let out = run_raw(rows, opts_update(), cancel).await;
        assert_eq!(
            out,
            vec![Err(SyncError::Cancelled)],
            "se corta en la fila siguiente, no al final"
        );
    }

    #[tokio::test]
    async fn a_cancelled_row_stream_cancels_the_plan() {
        // La cancelación puede venir de aguas arriba: el walk la vio primero.
        let out: Vec<_> = plan(
            stream::iter(vec![
                Ok(row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(src_file("a.txt", 1)),
                    None,
                )),
                Err(CompareError::Cancelled),
            ]),
            opts_update(),
            CancellationToken::new(),
        )
        .collect()
        .await;
        assert_eq!(out.len(), 2);
        assert!(out[0].is_ok());
        assert_eq!(out[1], Err(SyncError::Cancelled));
    }

    #[tokio::test]
    async fn polling_past_the_end_gives_none_instead_of_panicking() {
        // El `Unfold` crudo de `futures` entra en PÁNICO si se le sondea
        // después de `None`, y cualquier bucle con `select!` y un tick de flush
        // lo hace. El `.fuse()` de `plan()` es lo que lo impide.
        let mut s = Box::pin(plan(
            stream::iter(Vec::new()),
            opts_update(),
            CancellationToken::new(),
        ));
        assert!(s.next().await.is_none());
        assert!(s.next().await.is_none(), "y otra vez, sin pánico");
        assert!(s.is_terminated());
    }

    #[test]
    fn the_plan_stream_is_send_so_a_task_can_own_it() {
        // Se apoya en que `Send` se filtre por el `impl Trait`. Un `Rc` o un
        // `RefCell` dentro de `Transducer` compilaría aquí y rompería a
        // distancia, en la tarea 8, con un error ilegible.
        fn assert_send<T: Send>(_: &T) {}
        let s = plan(
            stream::iter(Vec::new()),
            opts_update(),
            CancellationToken::new(),
        );
        assert_send(&s);
    }

    #[tokio::test]
    async fn mirror_is_refused_and_never_degraded_to_update() {
        // El plan de `Update` es un SUBCONJUNTO del de `Mirror`: servirlo sería
        // dar por aprobado un espejo que no borra nada.
        let opts = SyncOptions {
            mode: SyncMode::Mirror,
            ..opts_update()
        };
        let out = run_raw(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(src_file("a.txt", 1)),
                None,
            )],
            opts,
            CancellationToken::new(),
        )
        .await;
        assert_eq!(out, vec![Err(SyncError::ModeNotPlanned(SyncMode::Mirror))]);
    }

    #[tokio::test]
    async fn a_row_that_names_the_root_itself_is_not_a_step() {
        // Sale de un llamante cuyas raíces son más profundas que las de la
        // comparación. Un paso con `rel` vacío actúa sobre el árbol ENTERO.
        let raiz = Entry {
            path: source_root(),
            kind: EntryKind::Dir,
            size: None,
            mtime_ms: None,
            attrs: std::collections::BTreeMap::default(),
        };
        let out = run_raw(
            vec![row(
                CompareVerdict::Different,
                CompareCriterion::Mtime,
                CompareConfidence::Probable,
                Some(raiz),
                Some(dst_dir("origen")),
            )],
            opts_update(),
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(
            out.as_slice(),
            [Err(SyncError::RootIsNotAStep { .. })]
        ));
    }

    #[tokio::test]
    async fn a_type_mismatch_whose_source_is_a_directory_is_still_one_overwrite() {
        // DEUDA, fijada aquí a propósito: `Overwrite` significa «copiar bytes»
        // y el paso no lleva `EntryKind`, así que la tarea 9 no puede
        // distinguirlo de sobrescribir un fichero — y el subárbol del
        // directorio no está en el plan, porque el walk no desciende un par que
        // no es de dos directorios. Cuando el wire gane vocabulario para
        // decirlo, este test cambia.
        let items = run(
            vec![row(
                CompareVerdict::TypeMismatch,
                CompareCriterion::Kind,
                CompareConfidence::Certain,
                Some(src_dir("build")),
                Some(dst_file("build", 4)),
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Overwrite);
        assert_eq!(
            s.size, None,
            "un directorio no mueve bytes, lo llame el plan como lo llame"
        );
    }

    #[tokio::test]
    async fn a_pair_whose_two_names_differ_in_bytes_takes_the_source_name() {
        // La clave de emparejamiento de `norte-compare` pliega (NFC siempre,
        // mayúsculas si algún lado no distingue), así que dos entradas con
        // bytes DISTINTOS pueden salir en una fila `Different` sin marca. El
        // `rel` sale del origen, con lo que pegado sobre el destino nombra un
        // fichero que no existe: se crea uno nuevo al lado.
        //
        // Se fija el comportamiento de HOY. Arreglarlo es una decisión de wire
        // (issue #152), no de este transductor.
        let nfc = entry_at(&source_root(), "café".as_bytes(), EntryKind::File, Some(10));
        let nfd = entry_at(&dest_root(), b"cafe\xcc\x81", EntryKind::File, Some(9));
        let items = run(
            vec![row(
                CompareVerdict::Different,
                CompareCriterion::Size,
                CompareConfidence::Certain,
                Some(nfc),
                Some(nfd),
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Overwrite);
        assert_eq!(
            s.rel.segments()[0].as_bytes(),
            "café".as_bytes(),
            "hoy el rel son los bytes del ORIGEN, y el destino se escribe NFD"
        );
    }

    // ---------- filas de verdad: compare → plan ----------

    /// Siembra un fichero en un `MemProvider`, creando los directorios del
    /// camino. Los nombres viajan en BYTES.
    async fn seed(mem: &norte_testkit::MemProvider, segments: &[&[u8]], content: &[u8]) {
        use norte_vfs::Provider as _;
        let segs: Vec<Segment> = segments
            .iter()
            .map(|s| Segment::new(*s).expect("segmento"))
            .collect();
        let (name, dirs) = segs.split_last().expect("camino no vacío");
        let mut at = norte_testkit::MemProvider::root();
        for dir in dirs {
            at = at.join(dir.clone());
            let _ = mem.mkdir(&at).await;
        }
        let mut sink = mem.write(&at.join(name.clone())).await.expect("write");
        sink.write(bytes::Bytes::copy_from_slice(content))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }

    #[tokio::test]
    async fn real_compare_rows_plan_without_a_single_outside_root() {
        // Las 17 pruebas de mesa construyen sus filas a mano, así que ninguna
        // toca el contrato que cruza los dos crates: TODA ruta que el walk
        // emite cuelga de la raíz que se le dio. Si deja de cumplirse, el plan
        // entero muere con `OutsideRoot` — y solo se ve aquí.
        use norte_compare::{CompareOptions, compare};
        let origen = norte_testkit::MemProvider::new();
        let destino = norte_testkit::MemProvider::new();
        seed(&origen, &[b"sub", b"informe\xff\xfe.dat"], b"nuevo").await;
        seed(&origen, &[b"raiz.txt"], b"nuevo").await;
        seed(&destino, &[b"raiz.txt"], b"viejo mas largo").await;

        let raiz = norte_testkit::MemProvider::root();
        let rows = compare(
            &origen,
            &raiz,
            &destino,
            &raiz,
            CompareOptions {
                descend_orphans: Some(Side::Left),
                ..CompareOptions::cheap()
            },
            CancellationToken::new(),
        );
        let opts = SyncOptions {
            source_root: raiz.clone(),
            dest_root: raiz,
            ..opts_update()
        };
        let out: Vec<_> = plan(rows, opts, CancellationToken::new()).collect().await;
        let items: Vec<PlanItem> = out
            .into_iter()
            .map(|r| r.expect("ninguna fila del walk se sale de su raíz"))
            .collect();

        // El walk es pre-orden y la mezcla va ordenada por clave, así que el
        // orden es un hecho y no hace falta ordenar: `raiz.txt` antes que
        // `sub`, y `sub` antes que lo que hay dentro.
        let salida: Vec<(SyncStepKind, String)> = steps_of(&items)
            .iter()
            .map(|s| (s.kind, s.rel.to_wire()))
            .collect();
        assert_eq!(
            salida,
            vec![
                (SyncStepKind::Overwrite, "raiz.txt".to_owned()),
                (SyncStepKind::CreateDir, "sub".to_owned()),
                (SyncStepKind::Copy, "sub/informe%FF%FE.dat".to_owned()),
            ],
            "un CreateDir precede siempre a lo que va dentro de él"
        );
    }
}
