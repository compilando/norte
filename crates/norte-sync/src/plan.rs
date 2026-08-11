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
    CompareConfidence, CompareRow, CompareVerdict, OnUnknown, RelPath, Side, StepReversal,
    SyncBlocker, SyncMode, SyncReason, SyncStep, SyncStepKind,
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
/// # Las dos ortografías de una misma pareja
/// `rel` sale de los bytes del ORIGEN. `norte-compare` empareja por una clave
/// PLEGADA —NFC siempre, mayúsculas cuando alguno de los dos lados no
/// distingue— así que dos entradas con bytes DISTINTOS se emparejan sin que la
/// fila lo diga (`reason: None`): un `café` NFC contra un `café` NFD, un
/// `README` contra el `readme` de un APFS. La fila trae las DOS `Entry`, así
/// que aquí se ve, y lo que sale es un [`SyncStep::dest_rel`] poblado con la
/// ruta del destino cuando sus bytes no son los del origen. El ejecutor escribe
/// entonces sobre el fichero que EXISTE en vez de crear un segundo al lado, y
/// la papelera que su reversa promete entierra algo de verdad (issue #152).
///
/// El destino NO se renombra a la ortografía del origen: eso convertiría cada
/// sincronización macOS↔Linux en un baile de renombrados.
///
/// **Y solo vale para las filas EMPAREJADAS.** Lo que existe únicamente en el
/// origen no trae entrada del destino, así que no hay segunda ortografía que
/// leer: un fichero nuevo dentro de un directorio que los dos lados deletrean
/// distinto sale con `dest_rel: None` y su `rel` cuelga del nombre del ORIGEN,
/// que es el mismo fichero duplicado un nivel más arriba. Cerrarlo pide
/// memoria —una pila de prefijos `(origen, destino)` de los directorios
/// emparejados que difieren, alimentada por unas filas `Same` que hoy no
/// producen paso alguno—, y esa pila es estado nuevo del transductor. Se fija
/// con un test (`a_copy_under_a_folder_the_two_sides_spell_differently_…`)
/// para que la tarea 9 no lo herede sin saberlo, y sigue anotado en el issue
/// #152.
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
        let (source, dest) = match self.opts.source_side {
            Side::Left => (row.left.as_ref(), row.right.as_ref()),
            _ => (row.right.as_ref(), row.left.as_ref()),
        };

        // Una fila de error no se traduce a nada que actúe: se traduce a un
        // `Skip` que la NOMBRA. Va antes que el veredicto porque no tiene
        // ninguno que valga (`Error` no es `Same` ni `Different`).
        if row.verdict == CompareVerdict::Error {
            return self.absorb_error(row, source, dest);
        }

        // El motivo del `Skip`, cuando el veredicto acaba en uno. Lo pone quien
        // decide la clase, que es el único que lo sabe.
        let mut skip_reason: Option<SyncReason> = None;
        // La entrada del destino con la que ESTA fila emparejó, que es la única
        // de la que puede salir una segunda ortografía. Un huérfano del origen
        // no emparejó con nada, así que se anula ahí abajo en vez de confiar en
        // que la fila traiga `None`: una que dijera «solo en el origen» y a la
        // vez trajera lado del destino —lo que `CompareRow::sides_are_consistent`
        // llama contradictoria— mandaría la copia a un nombre que nadie
        // emparejó, dentro del árbol aprobado. Se cierra, no se confía.
        let mut paired_dest = dest;
        let kind = if row.verdict == source_orphan {
            paired_dest = None;
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
                //
                // Una diferencia es una diferencia con la confianza que sea:
                // `on_unknown` no desempata aquí, desempata en `Same` — «parece
                // igual pero nadie lo puede prometer»— y no en «es distinto».
                CompareVerdict::Different | CompareVerdict::TypeMismatch => SyncStepKind::Overwrite,
                // Dos lados que se tienen por iguales no producen NADA… salvo
                // cuando esa igualdad no la respalda nadie
                // ([`CompareConfidence::Unknown`]: un provider que no da tamaño
                // ni fecha, un symlink sin destino legible, un socket). Ahí sí
                // hay una elección que hacer, y es del usuario.
                CompareVerdict::Same => {
                    if row.confidence != CompareConfidence::Unknown {
                        return Ok(());
                    }
                    // Solo el `Copy` EXPLÍCITO escribe. `OnUnknown` es
                    // `#[non_exhaustive]`, así que el comodín es obligatorio, y
                    // que caiga del lado de no tocar nada es deliberado: una
                    // política que este binario no entiende no puede autorizar
                    // una sobrescritura, y el `Skip` se ve en el plan antes de
                    // aprobarlo.
                    if matches!(self.opts.on_unknown, OnUnknown::Copy) {
                        SyncStepKind::Overwrite
                    } else {
                        skip_reason = Some(SyncReason::UnknownConfidence);
                        SyncStepKind::Skip
                    }
                }
                // `Ambiguous` y `Unknown` todavía no: son de la tarea 5. Que
                // hoy no produzcan NADA no es inocuo y por eso se escribe: una
                // colisión del origen —dos ortografías que el destino no puede
                // distinguir— sale hoy del plan sin paso y sin bloqueo, o sea
                // sin que nadie la vea. La tarea 5 la convierte en un `Skip`
                // con `AmbiguousSource` (o en un bloqueo, si es del destino), y
                // de eso depende que dos nombres del origen no acaben
                // escribiéndose uno encima del otro.
                _ => return Ok(()),
            }
        };

        let Some(entry) = source else {
            // `Different`/`TypeMismatch` sin lado de origen: la misma
            // contradicción que arriba.
            return Ok(());
        };
        let rel = rel_under(&self.opts.source_root, &entry.path)?;
        if rel.is_root() && kind != SyncStepKind::Skip {
            // La fila NOMBRA la raíz, no algo bajo ella. Un paso así actúa sobre
            // el árbol entero del destino. Un `Skip` sí puede nombrarla: no
            // actúa, y decir «no toqué la raíz, y por qué» es informar, no
            // apuntar a un blanco.
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
        //
        // Un `Skip` tampoco lo lleva, y por el mismo motivo: no escribe nada.
        let size = if entry.kind == EntryKind::Dir || kind == SyncStepKind::Skip {
            None
        } else {
            entry.size
        };
        let dest_rel = self.dest_rel_of(&rel, paired_dest)?;
        self.push(row, kind, rel, dest_rel, size, skip_reason);
        Ok(())
    }

    /// Una fila [`CompareVerdict::Error`] es un [`SyncStepKind::Skip`] que
    /// nombra lo que no se pudo leer.
    ///
    /// El motivo es SIEMPRE [`SyncReason::Unreadable`], y el vocabulario está
    /// cerrado a propósito: `Unreadable`, `ReadFailed` y `DirTooLarge` son tres
    /// maneras de que el walk no pudiera contestar por esa entrada, y ninguna
    /// autoriza a escribir sobre ella. (La tarea 5 saca de aquí el caso que sí
    /// es distinto: un `DirTooLarge` del DESTINO no es un paso saltado sino un
    /// bloqueo del plan entero, porque no se sabe qué hay en ese directorio.)
    ///
    /// El `rel` sale del lado que la fila trae: el walk emite la entrada del
    /// lado que falló y `None` en el otro cuando fue un listado, y las dos
    /// cuando lo que falló fue hidratar una pareja ya emparejada. Una fila sin
    /// ninguna de las dos no nombra nada y no produce paso.
    ///
    /// Este `Skip` SÍ puede nombrar la raíz: es exactamente lo que sale cuando
    /// el walk no pudo listar la propia raíz de la comparación, y decirlo es
    /// mucho mejor que morir o que callar un árbol entero.
    ///
    /// # El `rel` de una fila que solo tiene lado del destino
    /// Se mide contra `dest_root`, que es la única raíz de la que cuelga, y el
    /// paso no lleva nada que lo diga: `SyncStep` no tiene lado, y añadirle uno
    /// por dos formas que no escriben no lo valía. La consecuencia es de
    /// presentación y hay que conocerla — un panel que ancle todo `rel` al lado
    /// del origen pintará ahí un nombre que en el origen no existe—, y está
    /// escrita también en el rustdoc de [`SyncStep::rel`], que es donde la
    /// buscará quien consuma el plan. La tarea 5 hereda la misma convención con
    /// el [`SyncStepKind::DeleteTree`] de `Mirror`.
    fn absorb_error(
        &mut self,
        row: &CompareRow,
        source: Option<&Entry>,
        dest: Option<&Entry>,
    ) -> Result<(), SyncError> {
        let (entry, root) = match (source, dest) {
            (Some(s), _) => (s, &self.opts.source_root),
            (None, Some(d)) => (d, &self.opts.dest_root),
            (None, None) => return Ok(()),
        };
        let rel = rel_under(root, &entry.path)?;
        let dest_rel = self.dest_rel_of(&rel, dest)?;
        self.push(
            row,
            SyncStepKind::Skip,
            rel,
            dest_rel,
            None,
            Some(SyncReason::Unreadable),
        );
        Ok(())
    }

    /// Cómo se llama en el DESTINO lo que `rel` nombra en el origen, cuando no
    /// se llama igual.
    ///
    /// [`Some`] solo cuando las dos rutas relativas difieren BYTE A BYTE —lo
    /// hace el `Eq` de `Segment`, sin `to_str`, sin normalizar y sin plegar
    /// (regla dura 1)—, que es la regla normativa del campo.
    ///
    /// Se comparan las rutas ENTERAS y no solo el último segmento, y eso es más
    /// de lo que el nombre del campo sugiere: la clave de emparejamiento pliega
    /// en CADA nivel, así que un `café/x.txt` del origen puede colgar de un
    /// `café` NFD en el destino aunque `x.txt` se llame igual en los dos. Pegar
    /// `rel` sobre `dest_root` nombraría entonces un directorio que en ext4 no
    /// existe, exactamente igual que en el caso del nombre suelto.
    fn dest_rel_of(
        &self,
        rel: &RelPath,
        dest: Option<&Entry>,
    ) -> Result<Option<RelPath>, SyncError> {
        let Some(dest) = dest else { return Ok(None) };
        let dest_rel = rel_under(&self.opts.dest_root, &dest.path)?;
        Ok((dest_rel != *rel).then_some(dest_rel))
    }

    /// Empaqueta el paso y le pone su `id`. La reversa es función de la clase y
    /// de la papelera, salvo en un `Skip`, que no tiene y debe un motivo.
    fn push(
        &mut self,
        row: &CompareRow,
        kind: SyncStepKind,
        rel: RelPath,
        dest_rel: Option<RelPath>,
        size: Option<u64>,
        skip_reason: Option<SyncReason>,
    ) {
        let (reversal, reason) = match skip_reason {
            Some(why) => (None, Some(why)),
            None => reversal_for(kind, self.opts.dest_has_trash),
        };
        let step = SyncStep {
            id: self.next_id,
            kind,
            rel,
            dest_rel,
            size,
            criterion: row.criterion,
            confidence: row.confidence,
            reversal,
            reason,
        };
        debug_assert!(
            step.shape_is_consistent(),
            "paso con forma imposible: {step:?}"
        );
        self.pending.push_back(PlanItem::Step(step));
        self.next_id += 1;
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
    use norte_proto::methods::{CompareCriterion, CompareReason};

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

    /// Los BYTES de cada segmento. La forma wire no sirve para comparar dos
    /// ortografías: NFC y NFD son UTF-8 válido, así que el códec las deja tal
    /// cual y las dos cadenas se pintan IGUAL (regla dura 1 — se comparan
    /// bytes).
    fn bytes_of(rel: &RelPath) -> Vec<&[u8]> {
        rel.segments().iter().map(Segment::as_bytes).collect()
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
        assert_eq!(
            s.reason, None,
            "un paso reversible no tiene nada que justificar"
        );
        assert_eq!(s.dest_rel, None, "el destino se llama igual");
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
    async fn the_same_hostile_name_on_both_sides_never_invents_a_second_spelling() {
        // El corpus hostil ENTERO otra vez, ahora emparejado consigo mismo: si
        // la comparación de las dos rutas relativas dejara de ser byte a byte
        // —una normalización, un plegado, un `to_str` de más—, alguno de los 47
        // saldría con `dest_rel` poblado y el ejecutor escribiría en otro sitio
        // por un nombre que es EL MISMO.
        for name in norte_testkit::corpus::hostile_names() {
            let items = run(
                vec![row(
                    CompareVerdict::Different,
                    CompareCriterion::Size,
                    CompareConfidence::Certain,
                    Some(entry_at(
                        &source_root(),
                        &name.bytes,
                        EntryKind::File,
                        Some(2),
                    )),
                    Some(entry_at(
                        &dest_root(),
                        &name.bytes,
                        EntryKind::File,
                        Some(1),
                    )),
                )],
                opts_update(),
            )
            .await;
            assert_eq!(one_step(&items).dest_rel, None, "{}", name.id);
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

    // ---------- las dos ortografías de una pareja (issue #152) ----------

    #[tokio::test]
    async fn a_pair_whose_two_names_differ_in_bytes_names_the_destination_entry_too() {
        // La clave de emparejamiento de `norte-compare` pliega (NFC siempre,
        // mayúsculas si algún lado no distingue), así que dos entradas con
        // bytes DISTINTOS salen en una fila `Different` sin marca alguna. Si el
        // paso solo llevara el `rel` del origen, el ejecutor pegaría un `café`
        // NFC sobre `dest_root` y en ext4 escribiría un SEGUNDO fichero al lado
        // del que se quería sobrescribir — con una reversa que promete sacar de
        // la papelera algo que nadie enterró.
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
            "el rel sigue siendo el del ORIGEN: es de ahí de donde se lee"
        );
        assert_eq!(
            s.dest_rel.as_ref().expect("dos ortografías").segments()[0].as_bytes(),
            b"cafe\xcc\x81",
            "y el destino se escribe donde ESTÁ, sin renombrarlo a NFC"
        );
        assert_eq!(
            s.size,
            Some(10),
            "los bytes que se mueven son los del origen"
        );
        assert!(s.shape_is_consistent());
    }

    #[tokio::test]
    async fn a_pair_that_is_spelt_the_same_carries_no_dest_rel() {
        // El caso COMÚN, y por eso la clave viaja ausente: medio millón de
        // pasos no pagan una ruta repetida.
        let items = run(
            vec![row(
                CompareVerdict::Different,
                CompareCriterion::Size,
                CompareConfidence::Certain,
                Some(src_file("a.txt", 10)),
                Some(dst_file("a.txt", 9)),
            )],
            opts_update(),
        )
        .await;
        assert_eq!(one_step(&items).dest_rel, None);
    }

    #[tokio::test]
    async fn a_case_folded_pair_writes_over_the_name_the_destination_really_has() {
        // `README` contra el `readme` de un APFS: la clave los empareja porque
        // el destino no distingue caja. Escribir `README` ahí es escribir sobre
        // `readme` de todas formas — pero el plan tiene que DECIRLO, porque el
        // journal y la papelera nombran la entrada que existe.
        let items = run(
            vec![row(
                CompareVerdict::Different,
                CompareCriterion::Mtime,
                CompareConfidence::Probable,
                Some(src_file("README", 10)),
                Some(dst_file("readme", 9)),
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.rel, rel("README"));
        assert_eq!(s.dest_rel, Some(rel("readme")));
    }

    #[tokio::test]
    async fn a_pair_under_a_folder_spelt_differently_names_the_whole_destination_path() {
        // La clave pliega en CADA nivel, así que la diferencia puede estar en un
        // ancestro y no en el nombre: `café/x.txt` contra `café(NFD)/x.txt`. El
        // último segmento es idéntico y aun así son dos rutas distintas — pegar
        // `rel` sobre el destino nombraría un directorio que en ext4 no existe.
        let bajo = |root: &VPath, dir: &[u8]| Entry {
            path: root
                .join(Segment::new(dir.to_vec()).expect("segment"))
                .join(Segment::new(b"x.txt".to_vec()).expect("segment")),
            kind: EntryKind::File,
            size: Some(3),
            mtime_ms: None,
            attrs: std::collections::BTreeMap::default(),
        };
        let items = run(
            vec![row(
                CompareVerdict::Different,
                CompareCriterion::Size,
                CompareConfidence::Certain,
                Some(bajo(&source_root(), "café".as_bytes())),
                Some(bajo(&dest_root(), b"cafe\xcc\x81")),
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(bytes_of(&s.rel), vec!["café".as_bytes(), b"x.txt"]);
        assert_eq!(
            bytes_of(s.dest_rel.as_ref().expect("dos ortografías")),
            vec![b"cafe\xcc\x81".as_slice(), b"x.txt"],
            "la ruta ENTERA, no solo el último segmento"
        );
    }

    #[tokio::test]
    async fn something_only_on_the_source_never_carries_a_dest_rel() {
        // No hay entrada en el destino: no hay segunda ortografía que nombrar, y
        // un `dest_rel` inventado ahí mandaría la copia a otro sitio.
        let items = run(
            vec![
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(src_file("a.txt", 10)),
                    None,
                ),
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(src_dir("sub")),
                    None,
                ),
            ],
            opts_update(),
        )
        .await;
        assert!(steps_of(&items).iter().all(|s| s.dest_rel.is_none()));
    }

    #[tokio::test]
    async fn a_copy_under_a_folder_the_two_sides_spell_differently_still_takes_the_source_spelling()
    {
        // DEUDA, fijada aquí a propósito (issue #152, la mitad que este cambio
        // NO cierra). Los dos directorios `café` emparejan —la clave normaliza—
        // y el walk baja por ellos, así que un fichero que solo está en el
        // origen llega como huérfano: sin lado del destino no hay segunda
        // ortografía que leer, y su `rel` cuelga del `café` del ORIGEN. Sobre
        // ext4 eso crea un SEGUNDO directorio al lado del que ya estaba.
        //
        // La fila del par de directorios, que es la única que sabe las dos
        // ortografías, es `Same` y no produce paso: cerrarlo pide una pila de
        // prefijos, o sea estado nuevo. Cuando la haya, este test cambia.
        let nuevo = Entry {
            path: source_root()
                .join(Segment::new("café".as_bytes().to_vec()).expect("segment"))
                .join(Segment::new(b"nuevo.txt".to_vec()).expect("segment")),
            kind: EntryKind::File,
            size: Some(4),
            mtime_ms: None,
            attrs: std::collections::BTreeMap::default(),
        };
        let items = run(
            vec![
                // El par de directorios: empareja, y no produce nada.
                row(
                    CompareVerdict::Same,
                    CompareCriterion::Kind,
                    CompareConfidence::Certain,
                    Some(entry_at(
                        &source_root(),
                        "café".as_bytes(),
                        EntryKind::Dir,
                        None,
                    )),
                    Some(entry_at(
                        &dest_root(),
                        b"cafe\xcc\x81",
                        EntryKind::Dir,
                        None,
                    )),
                ),
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(nuevo),
                    None,
                ),
            ],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Copy);
        assert_eq!(bytes_of(&s.rel), vec!["café".as_bytes(), b"nuevo.txt"]);
        assert_eq!(
            s.dest_rel, None,
            "hoy el plan no lleva la ortografía del directorio del destino"
        );
    }

    #[tokio::test]
    async fn a_contradictory_orphan_row_cannot_redirect_the_copy() {
        // «Solo en el origen» Y con lado del destino: la fila se contradice
        // (`sides_are_consistent`). Si de ahí saliera un `dest_rel`, la copia
        // iría a un nombre que nadie emparejó. Se cierra en el transductor, no
        // se confía en que las filas vengan bien formadas.
        let items = run(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(src_file("a.txt", 10)),
                Some(dst_file("OTRA-COSA.txt", 1)),
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Copy);
        assert_eq!(s.rel, rel("a.txt"));
        assert_eq!(s.dest_rel, None);
    }

    #[tokio::test]
    async fn a_destination_entry_outside_the_destination_root_ends_the_plan() {
        // Si el `dest_rel` se calculara mal, el paso escribiría fuera del árbol
        // aprobado. Se cierra igual que el lado del origen: el plan muere.
        let fuera = entry_at(&vpath("file:///otro"), b"a.txt", EntryKind::File, Some(9));
        let out = run_raw(
            vec![row(
                CompareVerdict::Different,
                CompareCriterion::Size,
                CompareConfidence::Certain,
                Some(src_file("a.txt", 10)),
                Some(fuera),
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

    // ---------- confianza desconocida ----------

    #[tokio::test]
    async fn unknown_confidence_copies_by_default() {
        let items = run(
            vec![row(
                CompareVerdict::Same,
                CompareCriterion::Mtime,
                CompareConfidence::Unknown,
                Some(src_file("a", 1)),
                Some(dst_file("a", 1)),
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Overwrite);
        assert_eq!(
            s.confidence,
            CompareConfidence::Unknown,
            "el informe tiene que poder decir que copió porque nadie pudo asegurar nada"
        );
        assert_eq!(s.reversal, Some(StepReversal::RestoreTrash));
        assert_eq!(s.size, Some(1));
        assert!(s.shape_is_consistent());
    }

    #[tokio::test]
    async fn unknown_confidence_skips_when_asked_to() {
        let opts = SyncOptions {
            on_unknown: OnUnknown::Skip,
            ..opts_update()
        };
        let items = run(
            vec![row(
                CompareVerdict::Same,
                CompareCriterion::Mtime,
                CompareConfidence::Unknown,
                Some(src_file("a", 1)),
                Some(dst_file("a", 1)),
            )],
            opts,
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Skip);
        assert_eq!(s.reversal, None);
        assert_eq!(s.reason, Some(SyncReason::UnknownConfidence));
        assert_eq!(
            s.size, None,
            "un `Skip` no mueve bytes, y `counts.bytes` los suma"
        );
        assert_eq!(s.confidence, CompareConfidence::Unknown);
        assert!(s.shape_is_consistent());
    }

    #[tokio::test]
    async fn a_difference_is_overwritten_whatever_on_unknown_says() {
        // `on_unknown` desempata «parece igual y nadie lo puede prometer». Una
        // fila que dice DISTINTO no tiene empate que romper.
        for on_unknown in [OnUnknown::Copy, OnUnknown::Skip] {
            let opts = SyncOptions {
                on_unknown,
                ..opts_update()
            };
            let items = run(
                vec![row(
                    CompareVerdict::Different,
                    CompareCriterion::Mtime,
                    CompareConfidence::Unknown,
                    Some(src_file("a", 1)),
                    Some(dst_file("a", 2)),
                )],
                opts,
            )
            .await;
            assert_eq!(
                one_step(&items).kind,
                SyncStepKind::Overwrite,
                "{on_unknown:?}"
            );
        }
    }

    #[tokio::test]
    async fn on_unknown_does_not_decide_what_is_missing_from_the_destination() {
        // No copiar lo que no está porque «no se pudo verificar» sería saltarse
        // un hecho CIERTO: no hay nada que verificar donde no hay nada.
        let opts = SyncOptions {
            on_unknown: OnUnknown::Skip,
            ..opts_update()
        };
        let items = run(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Unknown,
                Some(src_file("a.txt", 10)),
                None,
            )],
            opts,
        )
        .await;
        assert_eq!(one_step(&items).kind, SyncStepKind::Copy);
    }

    #[tokio::test]
    async fn a_certain_same_is_never_a_skip_step() {
        // El volumen de los `Skip` lo acota lo RARO que sea el árbol, no lo
        // grande: mil parejas idénticas producen cero elementos.
        let rows: Vec<_> = (0..1000)
            .map(|i| {
                let name = format!("f{i}.txt");
                row(
                    CompareVerdict::Same,
                    CompareCriterion::Size,
                    CompareConfidence::Certain,
                    Some(src_file(&name, 10)),
                    Some(dst_file(&name, 10)),
                )
            })
            .collect();
        assert!(run(rows, opts_update()).await.is_empty());
    }

    // ---------- filas de error ----------

    /// Una fila de error tal como la emite el walk: motivo y lado obligatorios,
    /// confianza `Unknown`, y la entrada solo del lado que falló.
    fn error_row(
        reason: CompareReason,
        side: Side,
        left: Option<Entry>,
        right: Option<Entry>,
    ) -> CompareRow {
        CompareRow {
            reason: Some(reason),
            side: Some(side),
            ..row(
                CompareVerdict::Error,
                CompareCriterion::Presence,
                CompareConfidence::Unknown,
                left,
                right,
            )
        }
    }

    #[tokio::test]
    async fn an_error_row_becomes_a_skip_that_names_the_read_that_failed() {
        let items = run(
            vec![error_row(
                CompareReason::Unreadable,
                Side::Left,
                Some(src_dir("secreto")),
                None,
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Skip);
        assert_eq!(s.reason, Some(SyncReason::Unreadable));
        assert_eq!(s.rel, rel("secreto"));
        assert_eq!(s.reversal, None);
        assert_eq!(s.size, None);
        assert!(s.shape_is_consistent());
    }

    #[tokio::test]
    async fn every_way_the_walk_can_fail_a_row_is_a_skip_and_none_of_them_writes() {
        // Las tres son «el walk no pudo contestar por esta entrada», y ninguna
        // autoriza a escribir encima. (La tarea 5 saca de aquí el `DirTooLarge`
        // del DESTINO, que es un bloqueo del plan entero.)
        for reason in [
            CompareReason::Unreadable,
            CompareReason::ReadFailed,
            CompareReason::DirTooLarge,
            CompareReason::Unknown,
        ] {
            let items = run(
                vec![error_row(
                    reason,
                    Side::Left,
                    Some(src_file("x", 1)),
                    Some(dst_file("x", 1)),
                )],
                opts_update(),
            )
            .await;
            let s = one_step(&items);
            assert_eq!(s.kind, SyncStepKind::Skip, "{reason:?}");
            assert_eq!(s.reason, Some(SyncReason::Unreadable), "{reason:?}");
        }
    }

    #[tokio::test]
    async fn an_error_row_that_only_has_a_destination_entry_still_names_it() {
        // Un listado del DESTINO que no se dejó leer: la fila trae su entrada y
        // nada del origen. El `rel` sale de la raíz del destino, que es la que
        // le corresponde — medirla contra la del origen nombraría otra cosa.
        let items = run(
            vec![error_row(
                CompareReason::Unreadable,
                Side::Right,
                None,
                Some(dst_dir("privado")),
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Skip);
        assert_eq!(s.rel, rel("privado"));
        assert_eq!(s.dest_rel, None, "el rel YA es el del destino");
    }

    #[tokio::test]
    async fn an_error_row_that_names_the_root_is_a_skip_at_the_root() {
        // Es la fila que sale cuando el walk no pudo listar la PROPIA raíz. Un
        // paso que actúa sobre ella mata el plan (`RootIsNotAStep`); un `Skip`
        // no actúa, así que aquí sí puede llevarla — y decir «no pude mirar el
        // árbol» es mucho mejor que callarlo o que morir.
        let raiz = Entry {
            path: source_root(),
            kind: EntryKind::Dir,
            size: None,
            mtime_ms: None,
            attrs: std::collections::BTreeMap::default(),
        };
        let items = run(
            vec![error_row(
                CompareReason::Unreadable,
                Side::Left,
                Some(raiz),
                None,
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Skip);
        assert!(s.rel.is_root());
        assert_eq!(s.reason, Some(SyncReason::Unreadable));
    }

    #[tokio::test]
    async fn an_error_row_with_no_entry_on_either_side_produces_nothing() {
        // No hay ruta que medir, así que no hay paso que nombrar. El walk no
        // produce filas así; una fabricada a mano no puede colar un paso sin
        // `rel`.
        let items = run(
            vec![error_row(CompareReason::Unreadable, Side::Left, None, None)],
            opts_update(),
        )
        .await;
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn an_error_row_that_failed_to_hydrate_a_pair_keeps_both_spellings() {
        // El otro origen de una fila de error: la pareja SÍ se emparejó y lo que
        // falló fue leer lo que la cascada necesitaba, así que la fila trae los
        // dos lados. El `Skip` nombra los dos, porque el panel los pinta.
        let nfc = entry_at(&source_root(), "café".as_bytes(), EntryKind::File, None);
        let nfd = entry_at(&dest_root(), b"cafe\xcc\x81", EntryKind::File, None);
        let items = run(
            vec![error_row(
                CompareReason::Unreadable,
                Side::Right,
                Some(nfc),
                Some(nfd),
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Skip);
        assert_eq!(bytes_of(&s.rel), vec!["café".as_bytes()]);
        assert_eq!(
            bytes_of(s.dest_rel.as_ref().expect("dos ortografías")),
            vec![b"cafe\xcc\x81".as_slice()]
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
