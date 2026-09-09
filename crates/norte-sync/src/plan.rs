//! El transductor: filas de comparación entran, pasos de plan salen.
//!
//! Una fila produce CERO o UN elemento: un paso, o un bloqueo, o nada. Dos
//! árboles idénticos producen cero pasos, no un millón de `Skip`: lo NOTABLE se
//! emite, lo aburrido no.
//!
//! El orden es el del walk, que es pre-orden, así que un `CreateDir` precede
//! siempre a lo que va dentro de él sin que aquí haya que ordenar nada. Ese
//! pre-orden no es solo una comodidad de presentación: los dos únicos estados
//! que el transductor guarda —el prefijo podado por un solape y las ortografías
//! de los directorios emparejados— se apoyan en que el padre llegue ANTES que
//! sus hijos.
//!
//! Y en eso, y en NADA más: el walk emite las filas por DIRECTORIO —todas las de
//! un nivel, y después las de cada subdirectorio— así que entre la fila de una
//! carpeta y las de sus hijos se cuelan todas sus hermanas. Un estado que
//! suponga «los hijos vienen justo detrás» se rompe con el primer hermano, en
//! silencio y sobre el árbol de un usuario.

use std::collections::{BTreeMap, VecDeque};
use std::pin::Pin;

use futures::stream::{self, FusedStream, Stream, StreamExt};
use norte_compare::CompareError;
use norte_proto::methods::{
    CompareConfidence, CompareReason, CompareRow, CompareVerdict, OnUnknown, RelPath, Side,
    StepReversal, SyncBlocker, SyncBlockerKind, SyncMode, SyncReason, SyncStep, SyncStepKind,
};
use norte_proto::{Entry, EntryKind, VPath};
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
    Step {
        /// Lo que viaja por el wire y lo que entra en el `plan_hash`.
        step: SyncStep,
        /// Lo que la comparación vio en el destino, para los pasos que lo van a
        /// destruir. Ver [`DestWitness`].
        dest: Option<DestWitness>,
    },
    /// Una razón para que el plan no sea ejecutable.
    Blocker(SyncBlocker),
}

/// Lo que la comparación vio en la entrada del DESTINO sobre la que un paso
/// destructivo va a caer.
///
/// # Por qué existe, y por qué no está en el wire
/// El ejecutor tiene que revalidar antes de destruir: entre que un humano
/// aprueba un plan y que se aplica pasan hasta diez minutos, y un `stat` que
/// compare el destino con **lo que el plan anotó de él** es lo único que hay
/// entre ese TTL y un fichero perdido. Nada en [`SyncStep`] sirve de referencia:
/// [`SyncStep::size`] son los bytes que el paso MUEVE, o sea los del ORIGEN, y
/// no hay campo alguno que describa el estado previo del destino.
///
/// No viaja por el wire porque nadie del otro lado lo necesita —el panel pinta
/// el paso, no la foto del destino— y porque ponerlo ahí sería publicar una
/// segunda descripción del árbol de destino con sus tamaños y sus fechas. Viaja
/// en el spool, que es de este proceso, y no entra en el `plan_hash`: es de
/// dónde SALIÓ la conclusión, no la conclusión.
///
/// # Lo que puede y lo que no
/// Un provider que lista sin tamaño ni fecha —`file://` es uno— deja los dos
/// campos en `None`, y entonces la revalidación se queda en «sigue existiendo y
/// sigue siendo de la misma clase». Es menos, y es honesto: fingir un cero sería
/// declarar un conflicto en cada paso. Una entrada de una pareja SÍ viene
/// hidratada (la cascada necesita tamaño y fecha para decidir), así que el caso
/// que importa —el [`SyncStepKind::Overwrite`]— la trae poblada.
///
/// ```
/// use norte_proto::{Entry, EntryKind, VPath};
/// use norte_sync::DestWitness;
///
/// let entry = Entry {
///     path: VPath::parse("file:///destino/a.txt").expect("path"),
///     kind: EntryKind::File,
///     size: Some(1234),
///     mtime_ms: Some(1_726_000_000_000),
///     attrs: Default::default(),
/// };
/// let foto = DestWitness::of(&entry);
/// assert_eq!(foto.kind, EntryKind::File);
/// assert_eq!(foto.size, Some(1234));
///
/// // Un provider que no mide deja las dos en `None`, jamás un cero fingido: la
/// // revalidación se queda entonces en «existe y es de la misma clase».
/// let parco = Entry { size: None, mtime_ms: None, ..entry };
/// assert_eq!(DestWitness::of(&parco).size, None);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DestWitness {
    /// La clase que tenía. Un cambio de clase es siempre un conflicto: el paso
    /// se aprobó sobre un fichero y ahora hay un directorio, o al revés.
    pub kind: EntryKind,
    /// El tamaño que tenía; `None` = el provider no lo dijo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// La fecha que tenía, en ms; `None` = el provider no la dijo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime_ms: Option<i64>,
    /// Cuántas entradas tenía su PRIMER NIVEL, para un directorio (#176).
    ///
    /// `None` = no se contó: no es un directorio, o tenía más de las que se
    /// cuentan sin que contar sea el trabajo. Un `None` **no** relaja nada por
    /// su cuenta — la revalidación solo compara lo que las dos fotos traen,
    /// igual que con el tamaño y la fecha.
    ///
    /// Existe porque el `stat` de un directorio solo se mueve cuando cambian
    /// sus hijos DIRECTOS, así que un `DeleteTree` revalidaba limpio con un
    /// subárbol que había ganado cien ficheros dos niveles más abajo. El
    /// recuento no cierra ese caso —sigue sin ver un nieto— y sí caza el
    /// corriente: alguien metió algo ahí mientras el humano decidía. Es la
    /// comprobación más floja del paso con más radio de acción, y ahora es
    /// menos floja.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entries: Option<u64>,
}

impl DestWitness {
    /// La foto de `entry`, sin recuento de hijos: quien pueda contarlos —el
    /// que tiene provider— lo añade con [`Self::with_entries`].
    #[must_use]
    pub fn of(entry: &Entry) -> Self {
        Self {
            kind: entry.kind,
            size: entry.size,
            mtime_ms: entry.mtime_ms,
            entries: None,
        }
    }

    /// La misma foto, con el recuento del primer nivel (#176).
    #[must_use]
    pub fn with_entries(self, entries: Option<u64>) -> Self {
        Self { entries, ..self }
    }
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
/// # El solape lo decide ESTE flujo, no solo el llamante
/// Copiar `/a` sobre `/a/sub` copia un árbol dentro de sí mismo. La
/// comprobación ESTRUCTURAL de las dos raíces es del llamante (`sync.plan` la
/// hace con `Error::OverlappingRoots`) y aquí no se supone hecha: en cuanto una
/// fila trae una ruta del origen que llega a la raíz del DESTINO —o al revés—
/// sale un [`SyncBlockerKind::OverlapDetected`] y ese subárbol entero se poda,
/// sin un solo paso.
///
/// Lo que este flujo NO puede ver es que dos [`VPath`] distintos nombren un
/// mismo árbol (un symlink, una raíz SFTP bajo dos authorities, un archivo
/// abierto por dos caminos): eso pide canonicalizar, que cuesta un viaje por
/// comparación y no todo provider lo ofrece (ADR 0048). Y al revés, dos raíces
/// IGUALES no se toman por solape: el transductor no sabe de providers, así que
/// dos providers distintos que deletreen igual su raíz —dos `mem:///` de un
/// test, mismamente— llegarían aquí indistinguibles de un árbol contra sí mismo.
/// Ese caso no escribe de más de todas formas: un árbol comparado consigo mismo
/// da filas `Same`, o sea cero pasos. Lo peligroso es la CONTENCIÓN, que es lo
/// que se poda.
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
/// **Y lo que solo está en el ORIGEN hereda la ortografía de su carpeta.** Un
/// fichero nuevo dentro de un directorio que los dos lados deletrean distinto no
/// trae entrada del destino, así que no hay segunda ortografía que LEER de la
/// fila — pero sí hay una que RECORDAR: la fila del par de directorios, que es
/// `Same` y no produce paso, es la que la sabe. El transductor anota
/// `(origen → destino)` de cada par de directorios cuyas dos rutas difieren y
/// resuelve cada fila por su ancestro más profundo, así que `café/nuevo.txt` sale
/// con `dest_rel = café(NFD)/nuevo.txt` y el ejecutor escribe DENTRO del
/// directorio que existe en vez de crear un segundo `café` al lado (la otra
/// mitad del issue #152).
///
/// La comparación de ancestros es por SEGMENTOS y por bytes, nunca por prefijo
/// de cadena: `café` no es prefijo de `cafétière`.
///
/// # El flujo tiene que venir COMPLETO
/// De ahí salen las dos exigencias que la firma no puede imponer. `rows` debe
/// traer, por cada fila, todas las de sus ANCESTROS — porque la ortografía de
/// una carpeta viaja en la fila de la carpeta, que es `Same` y no produce paso—;
/// y debe venir en el orden del walk, que es pre-orden. Un llamante que quiera
/// planificar solo una selección (`include` de `sync.plan`) tiene que filtrar el
/// flujo de SALIDA, nunca el de entrada: quitar la fila `Same` de `café` deja
/// `café/nuevo.txt` sin `dest_rel` y reabre el #152 justo por donde se cerró.
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
///     dest_trash_restorable: true,
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
///     paired_under: None,
/// };
///
/// let items: Vec<_> = futures::executor::block_on(
///     plan(futures::stream::iter(vec![Ok(fila)]), opts, CancellationToken::new()).collect(),
/// );
/// let PlanItem::Step { step: paso, .. } = items[0].as_ref().expect("sin error") else {
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
    // Los dos fallos de CABLEADO se deciden aquí y no por fila: un plan cuyo
    // modo este binario no sabe planificar, o cuyo origen no nombra ningún lado,
    // no puede terminar vacío-y-aprobable solo porque la comparación no
    // produjera filas.
    let mirroring = matches!(opts.mode, SyncMode::Mirror);
    let fatal = if !matches!(opts.mode, SyncMode::Update | SyncMode::Mirror) {
        Some(SyncError::ModeNotPlanned(opts.mode))
    } else if opts.source_side == Side::Unknown {
        Some(SyncError::SourceSideUnknown)
    } else {
        None
    };
    // Un destino que no admite escritura bloquea el plan ENTERO, y se dice antes
    // de pedir la primera fila: el bloqueo no depende de que el árbol tenga nada
    // dentro, así que un origen vacío tiene que producirlo igual. Y ninguna fila
    // se llega a mirar — no hay nada que un árbol pueda decir que cambie el
    // resultado, y arrastrar el walk entero por él cuesta minutos de red.
    let mut pending = VecDeque::new();
    if !opts.dest_writable && fatal.is_none() {
        pending.push_back(PlanItem::Blocker(SyncBlocker {
            rel: RelPath::default(),
            kind: SyncBlockerKind::DestReadOnly,
            // No es de un sitio: es del árbol entero, y el árbol es el DESTINO
            // (convenio de plan: origen `Left`, destino `Right`).
            side: Some(Side::Right),
        }));
    }
    let source_is_left = opts.source_side == Side::Left;
    let transducer = Transducer {
        rows: Box::pin(rows),
        opts,
        cancel,
        pending,
        next_id: 0,
        finished: false,
        overlap: None,
        spellings: BTreeMap::new(),
        fatal,
        source_is_left,
        mirroring,
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
    /// El subárbol que un solape podó, cuando lo hubo.
    ///
    /// UNO solo, y no un conjunto, porque lo que se guarda no es la ruta de la
    /// fila que lo destapó sino la RAÍZ que se alcanzó, que es la que contiene
    /// todo el subárbol solapado. El pre-orden hace lo demás: el padre llega
    /// antes que sus hijos, así que la primera fila que entra en el subárbol lo
    /// levanta y todas las demás caen dentro del mismo prefijo.
    overlap: Option<Overlap>,
    /// Cómo se deletrea en el DESTINO cada directorio emparejado cuyas dos
    /// ortografías NO coinciden byte a byte.
    ///
    /// Un MAPA y no una pila, y esa es la única forma que funciona: el walk
    /// emite las filas por DIRECTORIO —todas las de un nivel y después, una a
    /// una, las de sus subdirectorios—, así que entre el par `café` y su hijo
    /// `café/nuevo.txt` se cuelan todos los hermanos de `café`. Una pila que se
    /// desapilase con el primer hermano perdería la traducción justo antes de
    /// necesitarla (o peor, con dos niveles: se quedaría con la del abuelo y
    /// nombraría un directorio que no existe en ninguno de los dos lados). Un
    /// mapa solo necesita que el padre llegue ANTES que sus hijos, que es lo que
    /// el pre-orden sí garantiza.
    ///
    /// La clave es la ruta del ORIGEN y el valor la del DESTINO, ENTERAS: la
    /// entrada más profunda que sea ancestro de una fila ya lleva dentro la
    /// traducción de todas sus ancestras, así que se resuelve con UNA búsqueda.
    ///
    /// Cuesta memoria proporcional al número de directorios emparejados que se
    /// deletrean distinto —cero en un árbol homogéneo, uno por carpeta acentuada
    /// en una sincronización macOS↔Linux—, y es de todas formas mucho menos que
    /// el plan que este flujo produce y que quien lo consume retiene entero.
    /// Sin él, lo que solo está en el origen sale nombrando una carpeta que en el
    /// destino no existe (issue #152).
    spellings: BTreeMap<RelPath, RelPath>,
    /// El fallo de CABLEADO que termina el plan en cuanto se pida el primer
    /// elemento, si lo hay.
    ///
    /// Se decide al construir y no al absorber la primera fila: un plan cuyo
    /// modo o cuyo origen vienen mal no puede salir vacío-y-aprobable solo
    /// porque los dos árboles estuvieran vacíos.
    fatal: Option<SyncError>,
    /// ¿Es el lado IZQUIERDO de las filas el origen? Resuelto una vez, aquí, en
    /// vez de volver a interpretar [`SyncOptions::source_side`] por fila.
    source_is_left: bool,
    /// ¿Borra este plan lo que sobra en el destino ([`SyncMode::Mirror`])?
    mirroring: bool,
}

/// El solape que el walk encontró: qué raíz se alcanzó y de qué lado venía la
/// ruta que la alcanzó.
///
/// El lado importa. Si una ruta del ORIGEN llegó a `dest_root`, lo que hay que
/// podar son las filas cuyo lado de ORIGEN cae ahí dentro; las del destino caen
/// todas bajo `dest_root` por definición —de ahí cuelga el árbol entero— y
/// podarlas también se llevaría por delante el plan entero en vez del subárbol.
#[derive(Debug, Clone)]
struct Overlap {
    /// La raíz alcanzada: todo lo que esté en ella o por debajo se poda.
    prefix: VPath,
    /// `true` si se miran las rutas del ORIGEN, `false` si las del destino.
    from_source: bool,
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
            // calculado. Es el mismo contrato que `norte_compare::walk`. Hoy una
            // fila produce como mucho un elemento, así que lo pendiente es a lo
            // sumo el bloqueo sembrado al construir — pero el contrato es sobre
            // lo que SALE, no sobre cuántos quepan.
            if self.cancel.is_cancelled() {
                self.finished = true;
                self.pending.clear();
                return Some(Err(SyncError::Cancelled));
            }
            // El cableado se comprobó al construir: termina el flujo sin mirar
            // una sola fila, y también cuando no hay ninguna.
            if let Some(fatal) = self.fatal.take() {
                self.finished = true;
                self.pending.clear();
                return Some(Err(fatal));
            }
            if let Some(item) = self.pending.pop_front() {
                return Some(Ok(item));
            }
            if !self.opts.dest_writable {
                // El bloqueo ya salió (lo sembró `plan`) y no hay nada más que
                // decir: ninguna fila puede cambiar que el destino no se deje
                // escribir. No se pide ni una, y soltar el flujo para de paso el
                // walk que lo alimenta.
                self.finished = true;
                return None;
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

    /// Traduce UNA fila a cero o un elemento en `pending`.
    ///
    /// El modo, el lado del origen y la escritura del destino ya se resolvieron
    /// al construir el transductor: aquí no hay ninguna opción que interpretar,
    /// solo la fila.
    fn absorb(&mut self, row: &CompareRow) -> Result<(), SyncError> {
        let (source_orphan, dest_orphan) = if self.source_is_left {
            (CompareVerdict::OnlyLeft, CompareVerdict::OnlyRight)
        } else {
            (CompareVerdict::OnlyRight, CompareVerdict::OnlyLeft)
        };
        let (source, dest) = if self.source_is_left {
            (row.left.as_ref(), row.right.as_ref())
        } else {
            (row.right.as_ref(), row.left.as_ref())
        };
        let mirroring = self.mirroring;

        // El solape, antes que cualquier otra cosa: dentro del subárbol podado
        // no se planifica nada, ni siquiera un `Skip`.
        if self.overlap_prunes(source, dest) {
            return Ok(());
        }
        if let Some(blocker) = self.overlap_reached(source, dest)? {
            self.pending.push_back(PlanItem::Blocker(blocker));
            return Ok(());
        }

        // El `rel` del origen se calcula UNA vez por fila, y antes de las
        // salidas tempranas: la pila de ortografías se alimenta también de filas
        // que no producen paso (el par de directorios es `Same`).
        let source_rel = match source {
            Some(entry) => Some(rel_under(&self.opts.source_root, &entry.path)?),
            None => None,
        };
        if let Some(rel) = source_rel.as_ref() {
            self.remember_spelling(rel, source, dest)?;
        }

        // Una fila de error no se traduce a nada que actúe: se traduce a un
        // `Skip` que la NOMBRA —o, si es un directorio del destino que no cabe,
        // a un bloqueo—. Va antes que el veredicto porque no tiene ninguno que
        // valga (`Error` no es `Same` ni `Different`).
        if row.verdict == CompareVerdict::Error {
            return self.absorb_error(row, source, dest, source_rel);
        }
        // Y una colisión de nombres tampoco tiene veredicto que mapear: es un
        // `Skip` si colisionó el origen y un BLOQUEO si colisionó el destino.
        if row.verdict == CompareVerdict::Ambiguous {
            return self.absorb_ambiguous(row, source, dest, source_rel);
        }
        // Y una pareja que solo se sostiene sobre una transformación NO
        // INYECTIVA no se toca (#207, ADR 0053): `K.txt` con U+212A KELVIN
        // SIGN contra `K.txt` con la `K` ASCII son dos ficheros para ext4 y
        // uno para Unicode, así que el `Overwrite` que salía de aquí escribía
        // los bytes de uno encima del OTRO. Es la pérdida de datos de #152, y
        // el 0.42.0 puso el dato en el wire justo para poder pararla aquí.
        //
        // El criterio es `names_one_text()` y no la variante concreta: se
        // salta toda transformación de la que este binario no pueda afirmar
        // que nombra un solo texto, incluida una que nombre un daemon más
        // nuevo. `CaseFold` y `Normalization` SIGUEN actuando — son las
        // parejas para las que la clave existe, y negarlas rompería el caso
        // macOS↔Linux al que sirve.
        //
        // Va antes del veredicto porque no depende de él: lo que no es de
        // fiar es la PAREJA, y una pareja que no es de fiar no se sobrescribe
        // ni se declara igual.
        if row.paired_under.is_some_and(|t| !t.names_one_text()) {
            return self.absorb_non_injective(row, source, dest, source_rel);
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
            // Lo que sobra en el destino: bajo `Update` no se borra nada —ese es
            // el motivo de que el modo exista—, bajo `Mirror` es UN
            // `DeleteTree`.
            if !mirroring {
                return Ok(());
            }
            return self.absorb_dest_orphan(row, dest);
        } else {
            match row.verdict {
                // Un desajuste de CLASE con un directorio de por medio no es un
                // `Overwrite`: `Overwrite` significa normativamente «a la
                // papelera y COPIAR bytes», el paso no lleva `EntryKind` con el
                // que distinguirlo, y el subárbol implicado ni siquiera está en
                // el plan —el walk no desciende un par que no es de dos
                // directorios—. Cambiar un árbol por un fichero es un cambio
                // estructural destructivo que esta spec no prometió: lo decide
                // un humano, y por eso es un bloqueo.
                //
                // Un fichero contra un symlink sí se sobrescribe: eso es
                // sustituir bytes, que es exactamente lo que el paso dice.
                CompareVerdict::TypeMismatch => {
                    if let Some(side) = directory_side(source, dest) {
                        return self.absorb_type_mismatch_dir(side, source_rel, dest);
                    }
                    SyncStepKind::Overwrite
                }
                // Una diferencia es una diferencia con la confianza que sea:
                // `on_unknown` no desempata aquí, desempata en `Same` — «parece
                // igual pero nadie lo puede prometer»— y no en «es distinto».
                CompareVerdict::Different => SyncStepKind::Overwrite,
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
                // Un veredicto que este decodificador no conoce (un daemon una
                // versión por delante) no produce nada: no hay tabla que
                // aplicarle y adivinarla escribiría. `CompareVerdict::Ambiguous`
                // NO cae aquí — lo atiende `absorb_ambiguous`, arriba.
                _ => return Ok(()),
            }
        };

        let (Some(entry), Some(rel)) = (source, source_rel) else {
            // `Different`/`TypeMismatch` sin lado de origen: la misma
            // contradicción que arriba.
            return Ok(());
        };
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
        // (un par de directorios que se tienen por iguales sin que nadie lo
        // pueda asegurar es lo segundo), y el `size` que un provider le ponga a
        // un directorio no son bytes que se vayan a escribir.
        //
        // Un `Skip` tampoco lo lleva, y por el mismo motivo: no escribe nada.
        let size = if entry.kind == EntryKind::Dir || kind == SyncStepKind::Skip {
            None
        } else {
            entry.size
        };
        let dest_rel = self.dest_rel_of(&rel, paired_dest)?;
        self.push(row, kind, rel, dest_rel, size, skip_reason, paired_dest);
        Ok(())
    }

    /// Una fila [`CompareVerdict::Error`] es un [`SyncStepKind::Skip`] que
    /// nombra lo que no se pudo leer.
    ///
    /// El motivo es SIEMPRE [`SyncReason::Unreadable`], y el vocabulario está
    /// cerrado a propósito: `Unreadable`, `ReadFailed` y `DirTooLarge` son tres
    /// maneras de que el walk no pudiera contestar por esa entrada, y ninguna
    /// autoriza a escribir sobre ella.
    ///
    /// # El único error que no es un `Skip`
    /// Un directorio del DESTINO por encima de
    /// [`COMPARE_MAX_DIR_ENTRIES`](norte_proto::methods::COMPARE_MAX_DIR_ENTRIES)
    /// no es una entrada que se salta: es un trozo del destino cuyo contenido
    /// NADIE ha visto, y planificar escrituras dentro de él es escribir a
    /// ciegas. Sale como
    /// [`SyncBlockerKind::DirTooLarge`](norte_proto::methods::SyncBlockerKind::DirTooLarge),
    /// y el mismo tope del lado del ORIGEN sigue siendo un `Skip`: no saber qué
    /// hay en un directorio del origen solo significa que de ahí no se copia.
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
    /// buscará quien consuma el plan. El [`SyncStepKind::DeleteTree`] de
    /// `Mirror` sigue la misma convención.
    fn absorb_error(
        &mut self,
        row: &CompareRow,
        source: Option<&Entry>,
        dest: Option<&Entry>,
        source_rel: Option<RelPath>,
    ) -> Result<(), SyncError> {
        if row.reason == Some(CompareReason::DirTooLarge)
            && self.speaks_for_the_destination(row, source, dest)
        {
            let rel = match dest {
                Some(entry) => rel_under(&self.opts.dest_root, &entry.path)?,
                None => source_rel.unwrap_or_default(),
            };
            self.push_blocker(rel, SyncBlockerKind::DirTooLarge, Some(Side::Right));
            return Ok(());
        }
        let rel = match (source_rel, dest) {
            (Some(rel), _) => rel,
            (None, Some(entry)) => rel_under(&self.opts.dest_root, &entry.path)?,
            (None, None) => return Ok(()),
        };
        let dest_rel = self.dest_rel_of(&rel, dest)?;
        self.push(
            row,
            SyncStepKind::Skip,
            rel,
            dest_rel,
            None,
            Some(SyncReason::Unreadable),
            dest,
        );
        Ok(())
    }

    /// Un [`CompareVerdict::TypeMismatch`] con un DIRECTORIO de por medio:
    /// bloqueo, no paso.
    ///
    /// `side` nombra el lado que tiene el directorio, y el `rel` se mide contra
    /// la raíz de ESE lado —igual que hacen `AmbiguousDest` y `DirTooLarge`—:
    /// si el árbol que no se va a tocar está en el destino, nombrarlo con la
    /// ortografía del origen pintaría una ruta que allí no existe, que es el
    /// mismo agujero que [`SyncStep::dest_rel`] tapa en los pasos.
    ///
    /// Un `TypeMismatch` trae SIEMPRE las dos entradas; los `None` de abajo son
    /// por si una fila fabricada a mano no las trae, y entonces nombra lo que
    /// haya. Un bloqueo sin sitio sigue bloqueando.
    fn absorb_type_mismatch_dir(
        &mut self,
        side: Side,
        source_rel: Option<RelPath>,
        dest: Option<&Entry>,
    ) -> Result<(), SyncError> {
        let dest_rel = match dest {
            Some(entry) => Some(rel_under(&self.opts.dest_root, &entry.path)?),
            None => None,
        };
        let (first, second) = if side == Side::Right {
            (dest_rel, source_rel)
        } else {
            (source_rel, dest_rel)
        };
        let rel = first.or(second).unwrap_or_default();
        self.push_blocker(rel, SyncBlockerKind::TypeMismatchDir, Some(side));
        Ok(())
    }

    /// Una fila [`CompareVerdict::Ambiguous`]: dos nombres de UN lado que
    /// colapsan a la misma clave de emparejamiento.
    ///
    /// Los dos lados no son el mismo problema, y solo uno puede perder datos:
    ///
    /// - **En el ORIGEN** no se sabe cuál de los dos ficheros copiar, así que no
    ///   se copia ninguno: un [`SyncStepKind::Skip`] con
    ///   [`SyncReason::AmbiguousSource`], y el resto del plan sigue en pie.
    /// - **En el DESTINO** escribir ahí es escribir sobre uno de dos ficheros
    ///   sin saber cuál: bloqueo, y el plan entero deja de ser ejecutable.
    ///
    /// Esto es lo que impide que dos ortografías del origen que el destino
    /// pliega a una se sobrescriban entre ellas: sin `Skip` ni bloqueo la
    /// colisión sale del plan SIN QUE NADIE LA VEA, que es exactamente la
    /// colisión que ADR 0048 dice que una sincronización tiene que ver antes de
    /// escribir nada.
    ///
    /// Una fila que no nombra lado —o que nombra uno que este decodificador no
    /// conoce— se decide por la entrada que trae, y el empate cae del lado del
    /// DESTINO: fallar hacia el bloqueo cuesta un plan que hay que rehacer,
    /// fallar hacia el `Skip` cuesta un fichero.
    fn absorb_ambiguous(
        &mut self,
        row: &CompareRow,
        source: Option<&Entry>,
        dest: Option<&Entry>,
        source_rel: Option<RelPath>,
    ) -> Result<(), SyncError> {
        if self.speaks_for_the_destination(row, source, dest) {
            let rel = match dest {
                Some(entry) => rel_under(&self.opts.dest_root, &entry.path)?,
                None => source_rel.unwrap_or_default(),
            };
            self.push_blocker(rel, SyncBlockerKind::AmbiguousDest, Some(Side::Right));
            return Ok(());
        }
        // Sin entrada del origen no hay nada que nombrar, y un `Skip` sin `rel`
        // no informa de nada.
        let Some(rel) = source_rel else { return Ok(()) };
        // Una colisión es de UN lado, así que la fila no trae pareja: el
        // `dest_rel`, si sale, sale de la carpeta que los envuelve.
        let dest_rel = self.dest_rel_of(&rel, None)?;
        self.push(
            row,
            SyncStepKind::Skip,
            rel,
            dest_rel,
            None,
            Some(SyncReason::AmbiguousSource),
            None,
        );
        Ok(())
    }

    /// Una pareja que solo se sostiene sobre una transformación NO INYECTIVA:
    /// un [`SyncStepKind::Skip`] que la nombra (#207, ADR 0053).
    ///
    /// Molde de [`Self::absorb_ambiguous`] y por el mismo motivo: lo que falla
    /// no es el veredicto sino la PAREJA, así que no hay tabla de veredictos
    /// que aplicar — hay una fila que informar y un árbol que no se toca.
    ///
    /// A diferencia de una colisión, esta no tiene «lado»: la transformación
    /// junta un nombre de CADA lado, así que no existe el caso `AmbiguousDest`
    /// que allí es un bloqueo. Y el `rel` sale del origen cuando lo hay, que
    /// es de donde salen todos los `rel` de un paso que habla de una pareja.
    fn absorb_non_injective(
        &mut self,
        row: &CompareRow,
        source: Option<&Entry>,
        dest: Option<&Entry>,
        source_rel: Option<RelPath>,
    ) -> Result<(), SyncError> {
        // Sin nada que nombrar no hay `Skip` que informe. Con lado del destino
        // y sin lado del origen —una pareja no puede estar así, pero la fila
        // viene del wire— se nombra el destino antes que no decir nada.
        let rel = match (source_rel, dest) {
            (Some(rel), _) => rel,
            (None, Some(entry)) => rel_under(&self.opts.dest_root, &entry.path)?,
            (None, None) => return Ok(()),
        };
        // La segunda ortografía es EL punto de la fila: los dos nombres se
        // escriben distinto, y quien lea el plan tiene que poder ver los dos.
        let dest_rel = self.dest_rel_of(&rel, dest)?;
        let _ = source;
        self.push(
            row,
            SyncStepKind::Skip,
            rel,
            dest_rel,
            None,
            Some(SyncReason::NonInjectivePairing),
            None,
        );
        Ok(())
    }

    /// Un huérfano del DESTINO bajo [`SyncMode::Mirror`]: UN
    /// [`SyncStepKind::DeleteTree`], y no se desciende.
    ///
    /// Un movimiento a la papelera, una entrada de journal, una cosa que
    /// restaurar. Partirlo en cuarenta mil pasos empeora el undo y cuesta
    /// cuarenta mil listados para no aprender nada que el plan necesite (spec,
    /// «Mirror»). El plan cuenta con que la comparación NO descendió los
    /// huérfanos del destino —`sync.plan` fija `descend_orphans` al lado del
    /// origen—: si alguien la pidiera con el destino descendido, cada hijo
    /// traería su propio `DeleteTree` dentro de un árbol que su padre ya borra.
    ///
    /// El `rel` se mide contra `dest_root`, igual que el del `Skip` de un
    /// listado del destino ilegible: es la única raíz de la que cuelga.
    /// [`SyncStep::size`] va AUSENTE aunque el provider dé un tamaño — un
    /// borrado no mueve bytes, y [`SyncCounts::bytes`](norte_proto::methods::SyncCounts::bytes)
    /// es la suma de ese campo.
    fn absorb_dest_orphan(
        &mut self,
        row: &CompareRow,
        dest: Option<&Entry>,
    ) -> Result<(), SyncError> {
        let Some(entry) = dest else {
            // La fila se contradice: dice «solo en el destino» y no trae la
            // entrada del destino.
            return Ok(());
        };
        let rel = rel_under(&self.opts.dest_root, &entry.path)?;
        if rel.is_root() {
            // Un `DeleteTree` con `rel` vacío borra el árbol ENTERO del destino.
            return Err(SyncError::RootIsNotAStep {
                root: Box::new(self.opts.dest_root.clone()),
            });
        }
        self.push(
            row,
            SyncStepKind::DeleteTree,
            rel,
            None,
            None,
            None,
            Some(entry),
        );
        Ok(())
    }

    /// ¿Habla esta fila del lado del DESTINO?
    ///
    /// [`CompareRow::side`] manda, que es lo que el walk puebla en toda fila
    /// `Ambiguous` y en toda fila `Error`. Cuando no nombra ningún lado —o
    /// nombra un [`Side::Unknown`] que solo puede venir de un peer más nuevo—
    /// decide qué entrada trae la fila, y el empate cae del lado del destino:
    /// las dos formas que preguntan esto (`Ambiguous`, `DirTooLarge`) bloquean
    /// el plan si son del destino y solo saltan una entrada si son del origen,
    /// así que fallar hacia el destino falla hacia el lado que no escribe.
    fn speaks_for_the_destination(
        &self,
        row: &CompareRow,
        source: Option<&Entry>,
        dest: Option<&Entry>,
    ) -> bool {
        // `source_side` ya no puede ser `Unknown` aquí: `absorb` termina el plan
        // con `SourceSideUnknown` antes de llegar.
        let dest_side = match self.opts.source_side {
            Side::Left => Side::Right,
            _ => Side::Left,
        };
        match row.side {
            Some(side) if side == self.opts.source_side => false,
            Some(side) if side == dest_side => true,
            _ => dest.is_some() || source.is_none(),
        }
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
    ///
    /// Sin entrada del destino la fila no dice nada… pero la PILA sí: lo que
    /// solo está en el origen hereda la ortografía de la carpeta emparejada de
    /// la que cuelga (issue #152). Es el mismo dato, recordado en vez de leído.
    ///
    /// La regla normativa del campo se aplica en un solo sitio, aquí: `Some`
    /// únicamente cuando las dos rutas difieren, venga de donde venga la del
    /// destino.
    fn dest_rel_of(
        &self,
        rel: &RelPath,
        dest: Option<&Entry>,
    ) -> Result<Option<RelPath>, SyncError> {
        let dest_rel = match dest {
            Some(dest) => rel_under(&self.opts.dest_root, &dest.path)?,
            None => match self.spelt_at_the_destination(rel) {
                Some(dest_rel) => dest_rel,
                None => return Ok(None),
            },
        };
        Ok((dest_rel != *rel).then_some(dest_rel))
    }

    /// Cómo se escribe `rel` en el destino según la carpeta emparejada más
    /// PROFUNDA de la que cuelga, cuando esa carpeta se deletrea distinto.
    ///
    /// Se prueban los ancestros de dentro hacia fuera y se para en el primero
    /// que esté en el mapa: su valor es la ruta del destino ENTERA, así que ya
    /// lleva dentro la traducción de todas sus ancestras y no hay que componer
    /// nada. La comparación la hace el `Eq`/`Ord` de [`RelPath`], que es el de
    /// [`Segment`], que son BYTES (regla dura 1): `café` y `cafétière` son dos
    /// claves distintas por mucho que una empiece por los bytes de la otra.
    ///
    /// El propio `rel` NO se prueba: la fila de un directorio emparejado trae su
    /// pareja y no necesita que nadie se la recuerde.
    ///
    /// Cuando no hay ni una carpeta que difiera —el caso común, y el único en un
    /// árbol homogéneo— esto es una comparación y nada más.
    fn spelt_at_the_destination(&self, rel: &RelPath) -> Option<RelPath> {
        if self.spellings.is_empty() {
            return None;
        }
        let segments = rel.segments();
        for split in (1..segments.len()).rev() {
            let ancestor = RelPath::new(segments[..split].to_vec());
            if let Some(dest_dir) = self.spellings.get(&ancestor) {
                let mut translated = dest_dir.segments().to_vec();
                translated.extend_from_slice(&segments[split..]);
                return Some(RelPath::new(translated));
            }
        }
        None
    }

    /// Anota la carpeta de esta fila si es un par de directorios que los dos
    /// lados deletrean DISTINTO.
    ///
    /// Se llama en toda fila con lado de origen, incluidas las que no producen
    /// paso: la fila de un par de directorios idénticos es `Same`, y es
    /// justamente ella la que sabe las dos ortografías.
    ///
    /// Las dos claves de una anotación son siempre UTF-8 válido, y no por
    /// casualidad: `norte-compare` no pliega un nombre que no lo sea (su clave
    /// sale cruda), así que un nombre no-UTF8 solo empareja con otro
    /// byte-idéntico —y entonces no hay nada que anotar—. Lo que SÍ puede ser
    /// no-UTF8 es lo que cuelgue de la carpeta: el sufijo se copia byte a byte.
    fn remember_spelling(
        &mut self,
        source_rel: &RelPath,
        source: Option<&Entry>,
        dest: Option<&Entry>,
    ) -> Result<(), SyncError> {
        let (Some(source), Some(dest)) = (source, dest) else {
            return Ok(());
        };
        if source.kind != EntryKind::Dir || dest.kind != EntryKind::Dir {
            return Ok(());
        }
        let dest_rel = rel_under(&self.opts.dest_root, &dest.path)?;
        if dest_rel != *source_rel {
            self.spellings.insert(source_rel.clone(), dest_rel);
        }
        Ok(())
    }

    /// ¿Cae esta fila dentro del subárbol que un solape ya podó?
    ///
    /// Se mira SOLO el lado que alcanzó la otra raíz. El otro cuelga de ella
    /// entero —si el origen llegó a `dest_root`, todas las rutas del destino
    /// están bajo `dest_root` por definición—, así que mirarlo también podaría
    /// el plan entero en vez del subárbol.
    fn overlap_prunes(&self, source: Option<&Entry>, dest: Option<&Entry>) -> bool {
        let Some(overlap) = self.overlap.as_ref() else {
            return false;
        };
        let side = if overlap.from_source { source } else { dest };
        side.is_some_and(|entry| is_at_or_under(&overlap.prefix, &entry.path))
    }

    /// ¿Ha llegado el walk a la OTRA raíz?
    ///
    /// Una ruta del origen que está en `dest_root` o por debajo —o una del
    /// destino que está en `source_root` o por debajo— significa que las dos
    /// raíces nombran un mismo árbol y que copiar de una a otra copiaría un
    /// subárbol dentro de sí mismo. Sale UN bloqueo y se poda: lo que se recuerda
    /// no es la ruta de esta fila sino la RAÍZ alcanzada, que es la que contiene
    /// el subárbol entero, así que un solo prefijo vale para todo lo que venga
    /// detrás.
    ///
    /// Dos raíces IGUALES no cuentan: ver el rustdoc de [`plan`].
    fn overlap_reached(
        &mut self,
        source: Option<&Entry>,
        dest: Option<&Entry>,
    ) -> Result<Option<SyncBlocker>, SyncError> {
        if self.overlap.is_some() {
            // UNA vez. Con las dos raíces anidadas la comprobación del otro lado
            // es cierta para TODA fila —si el destino está dentro del origen,
            // toda ruta del destino está bajo la raíz del origen—, así que
            // volver a mirarla levantaría un bloqueo por fila y movería la poda
            // a una raíz que se traga el plan entero. Con uno basta: el plan ya
            // no es ejecutable.
            return Ok(None);
        }
        if self.opts.source_root == self.opts.dest_root {
            return Ok(None);
        }
        let reached = source
            .filter(|entry| is_at_or_under(&self.opts.dest_root, &entry.path))
            .map(|entry| (entry, &self.opts.source_root, &self.opts.dest_root, true))
            .or_else(|| {
                dest.filter(|entry| is_at_or_under(&self.opts.source_root, &entry.path))
                    .map(|entry| (entry, &self.opts.dest_root, &self.opts.source_root, false))
            });
        let Some((entry, walked_root, other_root, from_source)) = reached else {
            return Ok(None);
        };
        // El `rel` del bloqueo se mide contra la raíz por la que iba el walk,
        // que es donde el panel lo va a pintar.
        let rel = rel_under(walked_root, &entry.path)?;
        self.overlap = Some(Overlap {
            prefix: other_root.clone(),
            from_source,
        });
        Ok(Some(SyncBlocker {
            rel,
            kind: SyncBlockerKind::OverlapDetected,
            // El solape es de las DOS raíces a la vez: no hay un lado que
            // nombrar, y se omite en vez de inventar uno.
            side: None,
        }))
    }

    /// Encola un bloqueo. No lleva `id`: un bloqueo no es un paso, y nada lo
    /// ejecuta ni lo enumera.
    fn push_blocker(&mut self, rel: RelPath, kind: SyncBlockerKind, side: Option<Side>) {
        self.pending
            .push_back(PlanItem::Blocker(SyncBlocker { rel, kind, side }));
    }

    /// Empaqueta el paso y le pone su `id`. La reversa es función de la clase y
    /// de la papelera, salvo en un `Skip`, que no tiene y debe un motivo.
    ///
    /// `dest` es la entrada del DESTINO que la fila traía, cuando la traía. De
    /// ella sale el [`DestWitness`], y solo para las dos clases que van a
    /// destruirla: quien no destruye no tiene nada que revalidar, y un testigo
    /// por paso en un plan de medio millón es fichero de spool que nadie lee.
    // Ocho argumentos porque un paso tiene ocho cosas que decir. Agruparlos en
    // una struct intermedia solo movería el sitio donde equivocarse de campo, y
    // esto es privado del módulo: no es API que nadie más vaya a llamar.
    #[expect(
        clippy::too_many_arguments,
        reason = "privado del módulo, no es API: agrupar en un struct solo movería los campos"
    )]
    fn push(
        &mut self,
        row: &CompareRow,
        kind: SyncStepKind,
        rel: RelPath,
        dest_rel: Option<RelPath>,
        size: Option<u64>,
        skip_reason: Option<SyncReason>,
        dest: Option<&Entry>,
    ) {
        let (reversal, reason) = match skip_reason {
            Some(why) => (None, Some(why)),
            None => reversal_for(
                kind,
                self.opts.dest_has_trash,
                self.opts.dest_trash_restorable,
            ),
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
        let dest = match kind {
            SyncStepKind::Overwrite | SyncStepKind::DeleteTree => dest.map(DestWitness::of),
            _ => None,
        };
        self.pending.push_back(PlanItem::Step { step, dest });
        self.next_id += 1;
    }
}

/// Cómo vuelve atrás un paso, en función de su clase y de la papelera del
/// DESTINO.
///
/// Un `CreateDir` y un `Copy` no destruyen nada, así que se deshacen borrando
/// lo que crearon, con papelera o sin ella. Un `Overwrite` y un `DeleteTree`
/// entierran algo: con papelera se saca de ella, sin papelera no se saca de
/// ningún sitio y el plan tiene que decirlo ANTES de que nadie lo apruebe
/// (regla dura 4).
///
/// # Y una papelera que no dice dónde puso las cosas no sirve para NADA
/// Cuando el destino tiene papelera pero no la nombra
/// ([`SyncOptions::dest_trash_restorable`](crate::SyncOptions::dest_trash_restorable)
/// en `false`), el undo se queda sin `reversal_ref` y no puede acertar: ni
/// desentierra lo que se sobrescribió —casaría por ruta original y sacaría el
/// fichero que él mismo acaba de enterrar— ni deshace una creación, porque
/// deshacerla es enterrarla y eso pasa por la misma papelera (#65). Así que
/// **todos** los pasos salen `Irreversible`, no solo los destructivos.
///
/// Lo que este caso NO cambia es el de un destino SIN papelera, donde una
/// `Copy` sigue anunciándose reversible: ahí el undo se salta la entrada por
/// una razón distinta (borrar «lo que hoy viva en esa ruta» sin papelera puede
/// destruir trabajo posterior del humano) y lo cuenta en
/// `skipped_created_no_trash`. Esa asimetría es una decisión de la tarea 11 del
/// plan, no un descuido de esta.
fn reversal_for(
    kind: SyncStepKind,
    dest_has_trash: bool,
    dest_trash_restorable: bool,
) -> (Option<StepReversal>, Option<SyncReason>) {
    let actua = matches!(
        kind,
        SyncStepKind::CreateDir
            | SyncStepKind::Copy
            | SyncStepKind::Overwrite
            | SyncStepKind::DeleteTree
    );
    if actua && dest_has_trash && !dest_trash_restorable {
        return (
            Some(StepReversal::Irreversible),
            Some(SyncReason::NoTrashOnTarget),
        );
    }
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

/// Cuál de los dos lados de un [`CompareVerdict::TypeMismatch`] es el
/// DIRECTORIO, en el convenio de lados de un PLAN: [`Side::Left`] el origen,
/// [`Side::Right`] el destino (el mismo que usa
/// [`SyncBlocker::side`](norte_proto::methods::SyncBlocker::side), y NO el lado
/// de la comparación, que depende de en qué panel estuviera el usuario).
///
/// `None` cuando no hay ninguno: un fichero contra un symlink es una sustitución
/// de bytes y se sobrescribe.
///
/// Los dos no pueden serlo a la vez —dos directorios no desajustan de clase—,
/// así que el orden del `or` no esconde nada; si una fila fabricada a mano
/// dijera lo contrario, gana el origen y sale bloqueo igual.
fn directory_side(source: Option<&Entry>, dest: Option<&Entry>) -> Option<Side> {
    if source.is_some_and(|entry| entry.kind == EntryKind::Dir) {
        Some(Side::Left)
    } else if dest.is_some_and(|entry| entry.kind == EntryKind::Dir) {
        Some(Side::Right)
    } else {
        None
    }
}

/// ¿Está `path` EN `root` o por debajo?
///
/// Delegado en [`RelPath::under`], igual que [`rel_under`] y por el mismo
/// motivo — es la única implementación desde #172. La raíz misma cuenta como
/// contenida, que es lo que las dos llamantes de este módulo necesitan: si el
/// walk alcanzó la propia raíz contraria, el subárbol entero está dentro
/// igual (ver [`Transducer::overlap_reached`] y [`Transducer::overlap_prunes`]).
fn is_at_or_under(root: &VPath, path: &VPath) -> bool {
    RelPath::under(root, path).is_some()
}

/// La ruta de `path` RELATIVA a `root`, o el error que dice que no cuelga.
///
/// La comparación —scheme, authority y segmentos por sus bytes crudos— vive en
/// [`RelPath::under`], que es de `norte-proto` porque allí viven los tres tipos
/// que toca y porque la respuesta tiene que ser UNA: el filtro `include` del
/// core y el frontend que arma la petición preguntan lo mismo, y dos
/// implementaciones que difieran mandan un `include` que no selecciona lo que
/// el lector marcó. Esto solo le pone el error de este crate.
///
/// Lo que SÍ puede devolver es la raíz misma (`path == root`), que no es un
/// escape hacia arriba pero sí el blanco más destructivo del plan: lo rechaza
/// quien lo llama, que es el único que sabe si un `rel` vacío tiene sentido (un
/// `Skip` sí, un `Overwrite` no).
///
/// # Errors
/// [`SyncError::OutsideRoot`] cuando `path` no cuelga de `root` —otro scheme,
/// otra authority, u otra rama—.
///
/// ```
/// use norte_proto::VPath;
/// use norte_sync::rel_under;
/// let root = VPath::parse("file:///origen").expect("root");
/// let path = VPath::parse("file:///origen/sub/a.txt").expect("path");
/// assert_eq!(rel_under(&root, &path).expect("rel").to_wire(), "sub/a.txt");
/// // Por SEGMENTOS, no por prefijo de cadena: `…/ab` no cuelga de `…/a`.
/// let otro = VPath::parse("file:///origenes/a.txt").expect("path");
/// assert!(rel_under(&root, &otro).is_err());
/// ```
pub fn rel_under(root: &VPath, path: &VPath) -> Result<RelPath, SyncError> {
    RelPath::under(root, path).ok_or_else(|| SyncError::OutsideRoot {
        root: Box::new(root.clone()),
        path: Box::new(path.clone()),
    })
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    // `Segment` ya no lo usa el módulo: `rel_under` delega la comparación en
    // `RelPath::under`. Los tests sí, para construir rutas.
    use norte_proto::Segment;
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
            dest_trash_restorable: true,
            dest_writable: true,
        }
    }

    fn opts_mirror() -> SyncOptions {
        SyncOptions {
            mode: SyncMode::Mirror,
            ..opts_update()
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

    fn dst_link(name: &str) -> Entry {
        entry_at(&dest_root(), name.as_bytes(), EntryKind::Symlink, None)
    }

    /// Una entrada en una ruta ARBITRARIA, para los tests de solape: ahí lo
    /// interesante es justamente que la ruta no cuelgue de donde debería.
    fn entry_wire(wire: &str, kind: EntryKind, size: Option<u64>) -> Entry {
        Entry {
            path: vpath(wire),
            kind,
            size,
            mtime_ms: None,
            attrs: std::collections::BTreeMap::default(),
        }
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
            paired_under: None,
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
                PlanItem::Step { step, .. } => Some(step),
                PlanItem::Blocker(_) => None,
            })
            .collect()
    }

    fn one_step(items: &[PlanItem]) -> &SyncStep {
        let steps = steps_of(items);
        assert_eq!(steps.len(), 1, "se esperaba UN paso: {items:?}");
        steps[0]
    }

    /// (`rel`, `dest_rel`) de un paso, en BYTES.
    type Ortografias<'a> = (Vec<&'a [u8]>, Option<Vec<&'a [u8]>>);

    fn blockers_of(items: &[PlanItem]) -> Vec<&SyncBlocker> {
        items
            .iter()
            .filter_map(|i| match i {
                PlanItem::Blocker(b) => Some(b),
                PlanItem::Step { .. } => None,
            })
            .collect()
    }

    fn one_blocker(items: &[PlanItem]) -> &SyncBlocker {
        let blockers = blockers_of(items);
        assert_eq!(blockers.len(), 1, "se esperaba UN bloqueo: {items:?}");
        blockers[0]
    }

    /// #207 (ADR 0053): una pareja que solo se sostiene sobre una
    /// transformación NO INYECTIVA no se sobrescribe.
    ///
    /// `K.txt` con U+212A KELVIN SIGN contra `K.txt` con la `K` ASCII: Unicode
    /// los declara canónicamente equivalentes, ext4 los guarda como DOS
    /// ficheros. Sin esto, un `Different` corriente salía como `Overwrite` y
    /// escribía los bytes de uno encima del otro — la pérdida de datos de
    /// #152, con el dato ya en el wire desde 0.42.0 y nadie leyéndolo.
    #[tokio::test]
    async fn una_pareja_no_inyectiva_no_se_sobrescribe() {
        use norte_proto::methods::PairTransform;

        let mut fila = row(
            CompareVerdict::Different,
            CompareCriterion::Size,
            CompareConfidence::Certain,
            Some(src_file("K.txt", 10)),
            Some(dst_file("K.txt", 20)),
        );
        fila.paired_under = Some(PairTransform::NormalizationSingleton);
        let items = run(vec![fila], opts_update()).await;
        let steps = steps_of(&items);
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].kind, SyncStepKind::Skip, "jamás un Overwrite");
        assert_eq!(steps[0].reason, Some(SyncReason::NonInjectivePairing));
    }

    /// Y una transformación que este binario NO conoce cae del mismo lado: el
    /// criterio es `names_one_text()`, no la variante. Un daemon una versión
    /// por delante no puede autorizar una sobrescritura por omisión.
    #[tokio::test]
    async fn una_transformacion_desconocida_tampoco_actua() {
        use norte_proto::methods::PairTransform;

        // `#[serde(other)]`: la variante que este binario usa para «no la
        // conozco». Se construye por el mismo camino por el que llegaría del
        // wire — el `Unknown` del enum.
        let desconocida = PairTransform::Unknown;
        assert!(!desconocida.names_one_text());
        let mut fila = row(
            CompareVerdict::Different,
            CompareCriterion::Size,
            CompareConfidence::Certain,
            Some(src_file("x.txt", 10)),
            Some(dst_file("x.txt", 20)),
        );
        fila.paired_under = Some(desconocida);
        let items = run(vec![fila], opts_update()).await;
        assert_eq!(steps_of(&items)[0].kind, SyncStepKind::Skip);
    }

    /// Las CORRIENTES siguen actuando: `CaseFold` y `Normalization` son las
    /// parejas para las que la clave de emparejamiento existe, y negarlas
    /// rompería el caso macOS↔Linux al que sirve.
    #[tokio::test]
    async fn una_pareja_nfc_nfd_sigue_sobrescribiendo() {
        use norte_proto::methods::PairTransform;

        for transform in [PairTransform::Normalization, PairTransform::CaseFold] {
            let mut fila = row(
                CompareVerdict::Different,
                CompareCriterion::Size,
                CompareConfidence::Certain,
                Some(src_file("cafe.txt", 10)),
                Some(dst_file("cafe.txt", 20)),
            );
            fila.paired_under = Some(transform);
            let items = run(vec![fila], opts_update()).await;
            assert_eq!(
                steps_of(&items)[0].kind,
                SyncStepKind::Overwrite,
                "{transform:?} nombra UN texto y sí actúa"
            );
        }
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

    /// Cuatro filas que producen las cuatro clases de paso que ACTÚAN. Es la
    /// entrada de los tests de reversa: la matriz completa en una llamada.
    fn una_fila_de_cada_clase() -> Vec<CompareRow> {
        vec![
            // Copy: solo en el origen.
            row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(src_file("nuevo.txt", 10)),
                None,
            ),
            // CreateDir: un directorio solo en el origen.
            row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(src_dir("nueva")),
                None,
            ),
            // Overwrite: distinto a los dos lados.
            row(
                CompareVerdict::Different,
                CompareCriterion::Size,
                CompareConfidence::Certain,
                Some(src_file("a.txt", 10)),
                Some(dst_file("a.txt", 9)),
            ),
            // DeleteTree (solo en Mirror): huérfano del destino.
            row(
                CompareVerdict::OnlyRight,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                None,
                Some(dst_file("sobra.txt", 1)),
            ),
        ]
    }

    /// **Una papelera que no dice dónde puso las cosas no deshace NADA.** Ni la
    /// sobrescritura (el undo sacaría el fichero que él mismo enterró) ni la
    /// copia (deshacerla es enterrarla, y eso pasa por la misma papelera, #65).
    /// Es lo que le pasaba a `file://` en Linux antes de que la papelera
    /// freedesktop nombrara su destino.
    #[tokio::test]
    async fn nothing_is_reversible_when_the_destination_cannot_restore() {
        let opts = SyncOptions {
            dest_has_trash: true,
            dest_trash_restorable: false,
            ..opts_mirror()
        };
        let items = run(una_fila_de_cada_clase(), opts).await;
        let steps = steps_of(&items);
        assert!(steps.len() >= 4, "las cuatro clases: {items:?}");
        for s in steps {
            if s.kind == SyncStepKind::Skip {
                continue;
            }
            assert_eq!(s.reversal, Some(StepReversal::Irreversible), "{s:?}");
            assert_eq!(s.reason, Some(SyncReason::NoTrashOnTarget), "{s:?}");
            assert!(s.shape_is_consistent(), "{s:?}");
        }
    }

    /// Y con una papelera que SÍ nombra su destino, la tabla de reversas es la
    /// de siempre: no se marca irreversible de más.
    #[tokio::test]
    async fn a_restorable_trash_keeps_the_promises_the_table_makes() {
        let opts = SyncOptions {
            dest_has_trash: true,
            dest_trash_restorable: true,
            ..opts_mirror()
        };
        let items = run(una_fila_de_cada_clase(), opts).await;
        let steps = steps_of(&items);
        assert!(
            steps
                .iter()
                .any(|s| s.reversal == Some(StepReversal::RestoreTrash))
        );
        assert!(
            steps
                .iter()
                .any(|s| s.reversal == Some(StepReversal::Delete))
        );
        assert!(
            !steps
                .iter()
                .any(|s| s.reversal == Some(StepReversal::Irreversible)),
            "{items:?}"
        );
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
    async fn a_type_mismatch_between_a_file_and_a_symlink_overwrites_and_says_so() {
        // Sustituir un symlink por un fichero (o al revés) es sustituir bytes,
        // que es exactamente lo que `Overwrite` significa. Con un DIRECTORIO de
        // por medio no lo es, y eso es un bloqueo (más abajo).
        let items = run(
            vec![row(
                CompareVerdict::TypeMismatch,
                CompareCriterion::Kind,
                CompareConfidence::Certain,
                Some(src_file("x", 1)),
                Some(dst_link("x")),
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Overwrite);
        assert_eq!(s.criterion, CompareCriterion::Kind);
        assert!(blockers_of(&items).is_empty());
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
    async fn a_wiring_error_ends_the_plan_even_with_no_rows_at_all() {
        // Los dos fallos de cableado se deciden al construir, no al absorber la
        // primera fila. Si se decidieran por fila, dos árboles vacíos —o una
        // comparación que no produjo ninguna— darían un plan VACÍO y aprobable
        // para un modo que este binario no sabe planificar: exactamente el fallo
        // silencioso que las dos variantes existen para evitar.
        let sin_lado = SyncOptions {
            source_side: Side::Unknown,
            ..opts_update()
        };
        assert_eq!(
            run_raw(vec![], sin_lado, CancellationToken::new()).await,
            vec![Err(SyncError::SourceSideUnknown)]
        );
    }

    #[tokio::test]
    async fn a_read_only_destination_does_not_hide_a_wiring_error() {
        // Un destino inmutable no es excusa para tragarse un cableado mal hecho:
        // el bloqueo del árbol no llega a salir, porque no hay plan que bloquear.
        let opts = SyncOptions {
            source_side: Side::Unknown,
            dest_writable: false,
            ..opts_update()
        };
        assert_eq!(
            run_raw(vec![], opts, CancellationToken::new()).await,
            vec![Err(SyncError::SourceSideUnknown)]
        );
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

    // ---------- un desajuste de clase con un directorio de por medio ----------

    #[tokio::test]
    async fn a_type_mismatch_whose_source_is_a_directory_blocks_instead_of_overwriting() {
        // `Overwrite` significa «a la papelera y copiar BYTES» y el paso no
        // lleva `EntryKind` con el que decir otra cosa, así que el ejecutor no
        // podría distinguirlo de sobrescribir un fichero — y el subárbol del
        // directorio ni siquiera está en el plan, porque el walk no desciende un
        // par que no es de dos directorios. Cambiar un fichero por un árbol lo
        // decide un humano.
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
        let b = one_blocker(&items);
        assert_eq!(b.kind, SyncBlockerKind::TypeMismatchDir);
        assert_eq!(b.rel, rel("build"));
        assert_eq!(
            b.side,
            Some(Side::Left),
            "el árbol está en el ORIGEN (convenio de plan: origen=Left)"
        );
        assert!(
            steps_of(&items).is_empty(),
            "y no sale además un paso que lo haga de todas formas"
        );
    }

    #[tokio::test]
    async fn a_type_mismatch_whose_destination_is_a_directory_blocks_too() {
        // El caso peor de los dos: aquí lo que se destruiría es un árbol del
        // DESTINO, y `Overwrite` lo enterraría entero con una sola línea del
        // plan.
        let items = run(
            vec![row(
                CompareVerdict::TypeMismatch,
                CompareCriterion::Kind,
                CompareConfidence::Certain,
                Some(src_file("build", 4)),
                Some(dst_dir("build")),
            )],
            opts_update(),
        )
        .await;
        let b = one_blocker(&items);
        assert_eq!(b.kind, SyncBlockerKind::TypeMismatchDir);
        assert_eq!(b.side, Some(Side::Right));
        assert!(steps_of(&items).is_empty());
    }

    #[tokio::test]
    async fn a_blocker_about_the_destination_names_the_destination_s_spelling() {
        // El `rel` de un bloqueo se mide contra la raíz del lado del que HABLA,
        // igual que en `AmbiguousDest` y `DirTooLarge`. Con una pareja plegada
        // —un `café` NFC del origen contra el `café` NFD del destino— nombrarlo
        // con la ortografía del origen pintaría una ruta que en el destino no
        // existe: el mismo agujero que `dest_rel` tapa en los pasos.
        let items = run(
            vec![row(
                CompareVerdict::TypeMismatch,
                CompareCriterion::Kind,
                CompareConfidence::Certain,
                Some(entry_at(
                    &source_root(),
                    "café".as_bytes(),
                    EntryKind::File,
                    Some(1),
                )),
                Some(entry_at(
                    &dest_root(),
                    b"cafe\xcc\x81",
                    EntryKind::Dir,
                    None,
                )),
            )],
            opts_update(),
        )
        .await;
        let b = one_blocker(&items);
        assert_eq!(b.side, Some(Side::Right));
        assert_eq!(
            b.rel.segments()[0].as_bytes(),
            b"cafe\xcc\x81",
            "el árbol que no se toca está en el destino, y se llama así ALLÍ"
        );
    }

    #[tokio::test]
    async fn a_type_mismatch_with_a_directory_blocks_under_mirror_as_well() {
        // El modo no cambia lo que un desajuste con directorio significa.
        let items = run(
            vec![row(
                CompareVerdict::TypeMismatch,
                CompareCriterion::Kind,
                CompareConfidence::Certain,
                Some(src_dir("build")),
                Some(dst_file("build", 4)),
            )],
            opts_mirror(),
        )
        .await;
        assert_eq!(one_blocker(&items).kind, SyncBlockerKind::TypeMismatchDir);
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

    /// Una entrada colgando de una carpeta, del lado que se le diga.
    fn under(
        root: &VPath,
        dirs: &[&[u8]],
        name: &[u8],
        kind: EntryKind,
        size: Option<u64>,
    ) -> Entry {
        let mut path = root.clone();
        for dir in dirs {
            path = path.join(Segment::new(dir.to_vec()).expect("segment"));
        }
        Entry {
            path: path.join(Segment::new(name.to_vec()).expect("segment")),
            kind,
            size,
            mtime_ms: None,
            attrs: std::collections::BTreeMap::default(),
        }
    }

    /// La fila del par de directorios que los dos lados deletrean distinto: es
    /// `Same` y no produce paso, y es la ÚNICA que sabe las dos ortografías.
    fn dir_pair(source: &[u8], dest: &[u8]) -> CompareRow {
        row(
            CompareVerdict::Same,
            CompareCriterion::Kind,
            CompareConfidence::Certain,
            Some(entry_at(&source_root(), source, EntryKind::Dir, None)),
            Some(entry_at(&dest_root(), dest, EntryKind::Dir, None)),
        )
    }

    #[tokio::test]
    async fn a_copy_under_a_folder_the_two_sides_spell_differently_takes_the_destination_spelling()
    {
        // La otra mitad del issue #152. Los dos directorios `café` emparejan —la
        // clave normaliza— y el walk baja por ellos, así que un fichero que solo
        // está en el origen llega como huérfano: su fila no trae lado del
        // destino y no hay segunda ortografía que LEER. Pero la fila del par de
        // directorios, que llegó antes por ser el walk pre-orden, sí la sabía: se
        // recuerda en una pila y se aplica aquí. Sin ella el ejecutor crearía un
        // SEGUNDO `café` al lado del que ya estaba, sobre ext4.
        let items = run(
            vec![
                dir_pair("café".as_bytes(), b"cafe\xcc\x81"),
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(under(
                        &source_root(),
                        &["café".as_bytes()],
                        b"nuevo.txt",
                        EntryKind::File,
                        Some(4),
                    )),
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
            bytes_of(s.dest_rel.as_ref().expect("la ortografía de la carpeta")),
            vec![b"cafe\xcc\x81".as_slice(), b"nuevo.txt"],
            "se escribe DENTRO del directorio que existe"
        );
        assert!(s.shape_is_consistent());
    }

    #[tokio::test]
    async fn a_directory_created_under_a_differently_spelt_folder_inherits_it_too() {
        // No solo las copias: un `CreateDir` que cuelgue de la carpeta también
        // tiene que crearse DENTRO de la que existe, o el subárbol entero nace
        // en el sitio equivocado.
        let items = run(
            vec![
                dir_pair("café".as_bytes(), b"cafe\xcc\x81"),
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(under(
                        &source_root(),
                        &["café".as_bytes()],
                        b"sub",
                        EntryKind::Dir,
                        None,
                    )),
                    None,
                ),
            ],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::CreateDir);
        assert_eq!(
            bytes_of(s.dest_rel.as_ref().expect("la ortografía de la carpeta")),
            vec![b"cafe\xcc\x81".as_slice(), b"sub"]
        );
    }

    #[tokio::test]
    async fn the_deepest_folder_wins_and_carries_the_ones_above_it() {
        // Dos niveles que difieren: la cima de la pila guarda la ruta del
        // destino ENTERA, así que traducir con ella sola ya lleva dentro la
        // traducción de sus ancestras.
        let profundo = Entry {
            path: source_root()
                .join(Segment::new("café".as_bytes().to_vec()).expect("segment"))
                .join(Segment::new("RESUMÉ".as_bytes().to_vec()).expect("segment"))
                .join(Segment::new(b"nuevo.txt".to_vec()).expect("segment")),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
            attrs: std::collections::BTreeMap::default(),
        };
        let par_interior = row(
            CompareVerdict::Same,
            CompareCriterion::Kind,
            CompareConfidence::Certain,
            Some(under(
                &source_root(),
                &["café".as_bytes()],
                "RESUMÉ".as_bytes(),
                EntryKind::Dir,
                None,
            )),
            Some(under(
                &dest_root(),
                &[b"cafe\xcc\x81"],
                b"resume\xcc\x81",
                EntryKind::Dir,
                None,
            )),
        );
        let items = run(
            vec![
                dir_pair("café".as_bytes(), b"cafe\xcc\x81"),
                par_interior,
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(profundo),
                    None,
                ),
            ],
            opts_update(),
        )
        .await;
        assert_eq!(
            bytes_of(
                one_step(&items)
                    .dest_rel
                    .as_ref()
                    .expect("las dos carpetas")
            ),
            vec![b"cafe\xcc\x81".as_slice(), b"resume\xcc\x81", b"nuevo.txt"]
        );
    }

    #[tokio::test]
    async fn a_sibling_of_the_folder_inherits_nothing_and_the_folder_survives_it() {
        // El orden que el walk emite DE VERDAD: la fila de la carpeta, después
        // TODAS sus hermanas, y solo entonces las de dentro. Una pila que se
        // desapilara con la primera hermana perdería la ortografía justo antes
        // de necesitarla — que es lo que hacía la primera versión de esto, con
        // los tres tests hechos a mano en el único orden que el walk no produce.
        //
        // Y al revés: lo de FUERA de la carpeta no puede heredarla, o `otro.txt`
        // acabaría dentro de `café` en el destino.
        let items = run(
            vec![
                dir_pair("café".as_bytes(), b"cafe\xcc\x81"),
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(src_file("otro.txt", 1)),
                    None,
                ),
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(under(
                        &source_root(),
                        &["café".as_bytes()],
                        b"nuevo.txt",
                        EntryKind::File,
                        Some(4),
                    )),
                    None,
                ),
            ],
            opts_update(),
        )
        .await;
        let steps = steps_of(&items);
        assert_eq!(steps.len(), 2);
        assert_eq!(
            steps[0].dest_rel, None,
            "lo de fuera de la carpeta no hereda su ortografía"
        );
        assert_eq!(
            bytes_of(steps[1].dest_rel.as_ref().expect("la ortografía sobrevive")),
            vec![b"cafe\xcc\x81".as_slice(), b"nuevo.txt"],
            "…y la carpeta sigue sabiendo la suya cuando por fin llegan sus hijos"
        );
    }

    #[tokio::test]
    async fn a_deeper_folder_is_remembered_even_when_a_sibling_comes_between() {
        // El caso que la pila no solo perdía sino que MENTÍA: con `café` y
        // `café/RESUMÉ` deletreados distinto los dos, la fila de `café/zz.txt`
        // se cuela entre `RESUMÉ` y sus hijos. Desapilando, `RESUMÉ` se pierde y
        // `café/RESUMÉ/nuevo.txt` sale traducido a `café(NFD)/RESUMÉ/nuevo.txt`
        // — una ruta que no existe en ninguno de los dos lados.
        let par_interior = row(
            CompareVerdict::Same,
            CompareCriterion::Kind,
            CompareConfidence::Certain,
            Some(under(
                &source_root(),
                &["café".as_bytes()],
                "RESUMÉ".as_bytes(),
                EntryKind::Dir,
                None,
            )),
            Some(under(
                &dest_root(),
                &[b"cafe\xcc\x81"],
                b"RESUME\xcc\x81",
                EntryKind::Dir,
                None,
            )),
        );
        let items = run(
            vec![
                dir_pair("café".as_bytes(), b"cafe\xcc\x81"),
                par_interior,
                // La hermana que se cuela.
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(under(
                        &source_root(),
                        &["café".as_bytes()],
                        b"zz.txt",
                        EntryKind::File,
                        Some(1),
                    )),
                    None,
                ),
                // Y solo ahora, lo que hay dentro de `RESUMÉ`.
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(under(
                        &source_root(),
                        &["café".as_bytes(), "RESUMÉ".as_bytes()],
                        b"nuevo.txt",
                        EntryKind::File,
                        Some(1),
                    )),
                    None,
                ),
            ],
            opts_update(),
        )
        .await;
        let steps = steps_of(&items);
        assert_eq!(steps.len(), 2);
        assert_eq!(
            bytes_of(steps[0].dest_rel.as_ref().expect("la carpeta de fuera")),
            vec![b"cafe\xcc\x81".as_slice(), b"zz.txt"]
        );
        assert_eq!(
            bytes_of(steps[1].dest_rel.as_ref().expect("las dos carpetas")),
            vec![b"cafe\xcc\x81".as_slice(), b"RESUME\xcc\x81", b"nuevo.txt"]
        );
    }

    #[tokio::test]
    async fn a_non_utf8_name_under_a_folded_folder_travels_byte_for_byte() {
        // La mezcla: la carpeta empareja porque la clave normaliza a NFC —lo que
        // solo puede pasar con nombres UTF-8—, y lo que cuelga de ella NO es
        // UTF-8 y jamás se pliega. El sufijo se copia crudo (regla dura 1).
        let items = run(
            vec![
                dir_pair("café".as_bytes(), b"cafe\xcc\x81"),
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(under(
                        &source_root(),
                        &["café".as_bytes()],
                        b"informe\xff\xfe.dat",
                        EntryKind::File,
                        Some(1),
                    )),
                    None,
                ),
            ],
            opts_update(),
        )
        .await;
        assert_eq!(
            bytes_of(
                one_step(&items)
                    .dest_rel
                    .as_ref()
                    .expect("la ortografía de la carpeta")
            ),
            vec![b"cafe\xcc\x81".as_slice(), b"informe\xff\xfe.dat"]
        );
    }

    #[tokio::test]
    async fn a_sibling_whose_name_starts_with_the_same_bytes_inherits_nothing() {
        // `café` son los primeros bytes de `cafétière`, así que un `starts_with`
        // de cadena sobre la ruta daría por bueno el prefijo y mandaría
        // `cafétière/x.txt` a `café(NFD)tière/x.txt` — un directorio que no
        // existe en ninguno de los dos lados. Se compara por SEGMENTOS.
        let items = run(
            vec![
                dir_pair("café".as_bytes(), b"cafe\xcc\x81"),
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(under(
                        &source_root(),
                        &["cafétière".as_bytes()],
                        b"x.txt",
                        EntryKind::File,
                        Some(2),
                    )),
                    None,
                ),
            ],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(
            bytes_of(&s.rel),
            vec!["cafétière".as_bytes(), b"x.txt".as_slice()]
        );
        assert_eq!(s.dest_rel, None);
    }

    #[tokio::test]
    async fn a_folder_pair_spelt_the_same_records_nothing() {
        // El caso común: si la carpeta se llama igual en los dos lados, lo que
        // cuelgue de ella tampoco tiene segunda ortografía. Nada que apilar y
        // nada que traducir.
        let items = run(
            vec![
                dir_pair(b"sub", b"sub"),
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(under(
                        &source_root(),
                        &[b"sub"],
                        b"nuevo.txt",
                        EntryKind::File,
                        Some(4),
                    )),
                    None,
                ),
            ],
            opts_update(),
        )
        .await;
        assert_eq!(one_step(&items).dest_rel, None);
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

    // ---------- Mirror: lo que sobra en el destino ----------

    #[tokio::test]
    async fn mirror_turns_a_destination_orphan_into_one_delete_tree() {
        let items = run(
            vec![row(
                CompareVerdict::OnlyRight,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                None,
                Some(dst_dir("stale")),
            )],
            opts_mirror(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::DeleteTree);
        assert_eq!(s.rel, rel("stale"));
        assert_eq!(s.reversal, Some(StepReversal::RestoreTrash));
        assert_eq!(s.reason, None);
        assert_eq!(
            s.dest_rel, None,
            "el `rel` de un borrado YA es el del destino"
        );
        assert!(s.shape_is_consistent());
    }

    /// **La invariante de la que depende un painter, pinneada donde se
    /// produce.** `norte_frontend::sync::render_failure` decide el ancla de
    /// una fila de fallo con la ÚNICA prueba que queda en el wire: si el
    /// informe manda `dest_rel`, `rel` es la mitad del ORIGEN. Un
    /// `SyncFailure` no llevaba clase, así que esa regla solo era correcta
    /// mientras un `DeleteTree` —cuyo `rel` cuelga del DESTINO— no trajera
    /// nunca `dest_rel`. Hoy no lo trae, y `anchor_of` lo sabe porque para un
    /// PASO sí tiene la clase y la mira primero.
    ///
    /// **0.42.0 (#195) le pone la clase al fallo, y este test SE QUEDA.** Con
    /// `SyncFailure::kind` en el wire, el ancla deja de deducirse y se lee, así
    /// que el painter ya no depende de esta invariante — pero un `DeleteTree`
    /// que empezara a llevar `dest_rel` seguiría contradiciendo lo que el campo
    /// dice de sí mismo («la ruta del destino CUANDO no se deletrea como
    /// `rel`»), y este test cuesta cero segundos. Es la guarda barata de una
    /// propiedad del planificador, no ya el andamio de un frontend.
    ///
    /// Sin este test, añadir `dest_rel` a un `DeleteTree` —algo razonable el
    /// día que se quiera enseñar la ortografía del destino— cambiaría en
    /// silencio el ancla de la fila hostil más común de un `Mirror`: un
    /// borrado denegado por permisos, que pasaría a decir «del origen» y
    /// mandaría al operador a arreglar el árbol equivocado (revisión de rama
    /// de C2, rust MINOR-1).
    #[tokio::test]
    async fn un_delete_tree_jamas_lleva_dest_rel() {
        let items = run(
            vec![
                row(
                    CompareVerdict::OnlyRight,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    None,
                    Some(dst_dir("subarbol")),
                ),
                row(
                    CompareVerdict::OnlyRight,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    None,
                    Some(dst_file("suelto.bin", 10)),
                ),
            ],
            opts_mirror(),
        )
        .await;
        let borrados: Vec<_> = steps_of(&items)
            .into_iter()
            .filter(|s| s.kind == SyncStepKind::DeleteTree)
            .collect();
        assert_eq!(borrados.len(), 2, "los dos huérfanos del destino");
        for s in borrados {
            assert_eq!(
                s.dest_rel, None,
                "un DeleteTree no lleva dest_rel: render_failure lee esa \
                 ausencia como «esta ruta no es del origen»"
            );
        }
    }

    #[tokio::test]
    async fn a_deletion_is_one_step_for_the_whole_tree_and_moves_no_bytes() {
        // UN movimiento a la papelera, UNA entrada de journal, UNA cosa que
        // restaurar (spec). Y `size` ausente aunque el provider lo sepa:
        // `counts.bytes` es la suma de ese campo, y un borrado no mueve ninguno.
        let items = run(
            vec![row(
                CompareVerdict::OnlyRight,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                None,
                Some(dst_file("gone.bin", 4096)),
            )],
            opts_mirror(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::DeleteTree);
        assert_eq!(s.size, None);
    }

    #[tokio::test]
    async fn a_delete_without_a_trash_is_irreversible_and_says_why() {
        let opts = SyncOptions {
            dest_has_trash: false,
            ..opts_mirror()
        };
        let items = run(
            vec![row(
                CompareVerdict::OnlyRight,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                None,
                Some(dst_dir("stale")),
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
    async fn mirror_copies_exactly_what_update_copies() {
        // `Mirror` AÑADE una regla; no cambia ninguna de las que ya había.
        let filas = || {
            vec![
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(src_file("a.txt", 10)),
                    None,
                ),
                row(
                    CompareVerdict::Different,
                    CompareCriterion::Size,
                    CompareConfidence::Certain,
                    Some(src_file("b.txt", 10)),
                    Some(dst_file("b.txt", 9)),
                ),
                row(
                    CompareVerdict::Same,
                    CompareCriterion::Hash,
                    CompareConfidence::Certain,
                    Some(src_file("c.txt", 1)),
                    Some(dst_file("c.txt", 1)),
                ),
            ]
        };
        let como_update: Vec<_> = steps_of(&run(filas(), opts_update()).await)
            .iter()
            .map(|s| (s.kind, s.rel.to_wire()))
            .collect();
        let como_mirror: Vec<_> = steps_of(&run(filas(), opts_mirror()).await)
            .iter()
            .map(|s| (s.kind, s.rel.to_wire()))
            .collect();
        assert_eq!(como_update, como_mirror);
    }

    #[tokio::test]
    async fn update_never_emits_a_delete_tree() {
        // Todos los veredictos, una vez cada uno, bajo `Update`.
        let rows = vec![
            row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(src_file("a.txt", 1)),
                None,
            ),
            row(
                CompareVerdict::OnlyRight,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                None,
                Some(dst_file("b.txt", 1)),
            ),
            row(
                CompareVerdict::Different,
                CompareCriterion::Size,
                CompareConfidence::Certain,
                Some(src_file("c.txt", 2)),
                Some(dst_file("c.txt", 1)),
            ),
            row(
                CompareVerdict::Same,
                CompareCriterion::Hash,
                CompareConfidence::Certain,
                Some(src_file("d.txt", 1)),
                Some(dst_file("d.txt", 1)),
            ),
            row(
                CompareVerdict::TypeMismatch,
                CompareCriterion::Kind,
                CompareConfidence::Certain,
                Some(src_file("e", 1)),
                Some(dst_link("e")),
            ),
            ambiguous_row(
                Side::Left,
                CompareReason::CaseFold,
                Some(src_file("F", 1)),
                None,
            ),
            error_row(
                CompareReason::Unreadable,
                Side::Left,
                Some(src_dir("g")),
                None,
            ),
            row(
                CompareVerdict::Unknown,
                CompareCriterion::Unknown,
                CompareConfidence::Unrecognised,
                Some(src_file("h.txt", 1)),
                Some(dst_file("h.txt", 1)),
            ),
        ];
        let items = run(rows, opts_update()).await;
        assert!(
            steps_of(&items)
                .iter()
                .all(|s| s.kind != SyncStepKind::DeleteTree),
            "`Update` no borra NUNCA: {items:?}"
        );
    }

    #[tokio::test]
    async fn mirror_will_not_delete_the_destination_root_itself() {
        // Un `DeleteTree` con `rel` vacío borra el árbol ENTERO del destino. Sale
        // de un llamante cuyas raíces son más profundas que las de la
        // comparación, y mata el plan.
        let raiz = Entry {
            path: dest_root(),
            kind: EntryKind::Dir,
            size: None,
            mtime_ms: None,
            attrs: std::collections::BTreeMap::default(),
        };
        let out = run_raw(
            vec![row(
                CompareVerdict::OnlyRight,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                None,
                Some(raiz),
            )],
            opts_mirror(),
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(
            out.as_slice(),
            [Err(SyncError::RootIsNotAStep { .. })]
        ));
    }

    #[tokio::test]
    async fn a_destination_orphan_outside_the_destination_root_ends_the_plan() {
        let out = run_raw(
            vec![row(
                CompareVerdict::OnlyRight,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                None,
                Some(entry_wire("file:///otro/x.txt", EntryKind::File, Some(1))),
            )],
            opts_mirror(),
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(
            out.as_slice(),
            [Err(SyncError::OutsideRoot { .. })]
        ));
    }

    // ---------- colisiones de nombre ----------

    /// Una fila `Ambiguous` tal como la emite el walk (forma normativa de
    /// `CompareVerdict::Ambiguous`): UNA fila por entrada implicada, con la
    /// entrada en el campo de SU lado y el otro en `None`.
    fn ambiguous_row(
        side: Side,
        reason: CompareReason,
        left: Option<Entry>,
        right: Option<Entry>,
    ) -> CompareRow {
        CompareRow {
            reason: Some(reason),
            side: Some(side),
            ..row(
                CompareVerdict::Ambiguous,
                CompareCriterion::Presence,
                CompareConfidence::Unknown,
                left,
                right,
            )
        }
    }

    #[tokio::test]
    async fn an_ambiguous_source_is_skipped_and_the_rest_of_the_plan_stands() {
        // No se sabe cuál de los dos ficheros copiar, así que no se copia
        // ninguno — y se DICE. Sin este `Skip` la colisión saldría del plan sin
        // paso y sin bloqueo, o sea sin que nadie la viera.
        let items = run(
            vec![
                ambiguous_row(
                    Side::Left,
                    CompareReason::CaseFold,
                    Some(src_file("README", 1)),
                    None,
                ),
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(src_file("a.txt", 10)),
                    None,
                ),
            ],
            opts_update(),
        )
        .await;
        let steps = steps_of(&items);
        assert_eq!(steps[0].kind, SyncStepKind::Skip);
        assert_eq!(steps[0].reason, Some(SyncReason::AmbiguousSource));
        assert_eq!(steps[0].rel, rel("README"));
        assert_eq!(steps[0].reversal, None);
        assert_eq!(steps[0].size, None);
        assert!(steps[0].shape_is_consistent());
        assert_eq!(
            steps[1].kind,
            SyncStepKind::Copy,
            "una colisión no para el plan"
        );
        assert!(blockers_of(&items).is_empty());
    }

    #[tokio::test]
    async fn two_source_spellings_the_destination_folds_together_never_overwrite_each_other() {
        // Por qué el `Skip` de arriba es lo que hace SEGURO lo que antes solo
        // era inofensivo: el destino no distingue caja, así que `README` y
        // `readme` del origen apuntan al mismo fichero de allí. Si alguno saliera
        // como `Copy`, el segundo escribiría encima del primero dentro del árbol
        // aprobado.
        let items = run(
            vec![
                ambiguous_row(
                    Side::Left,
                    CompareReason::CaseFold,
                    Some(src_file("README", 1)),
                    None,
                ),
                ambiguous_row(
                    Side::Left,
                    CompareReason::CaseFold,
                    Some(src_file("readme", 2)),
                    None,
                ),
            ],
            opts_update(),
        )
        .await;
        let steps = steps_of(&items);
        assert_eq!(steps.len(), 2, "una fila por entrada, ninguna deduplicada");
        assert!(steps.iter().all(|s| s.kind == SyncStepKind::Skip));
        assert!(
            steps
                .iter()
                .all(|s| s.reason == Some(SyncReason::AmbiguousSource))
        );
    }

    #[tokio::test]
    async fn an_ambiguous_destination_blocks_the_plan() {
        // Escribir ahí es escribir sobre uno de dos ficheros sin saber cuál.
        let items = run(
            vec![ambiguous_row(
                Side::Right,
                CompareReason::Normalization,
                None,
                Some(dst_file("README", 1)),
            )],
            opts_update(),
        )
        .await;
        let b = one_blocker(&items);
        assert_eq!(b.kind, SyncBlockerKind::AmbiguousDest);
        assert_eq!(b.rel, rel("README"));
        assert_eq!(b.side, Some(Side::Right));
        assert!(steps_of(&items).is_empty());
    }

    #[tokio::test]
    async fn an_ambiguous_row_that_does_not_name_its_side_falls_towards_the_blocker() {
        // El walk siempre nombra el lado. Una fila que no lo hiciera se decide
        // por la entrada que trae, y el EMPATE cae del lado del destino: fallar
        // hacia el bloqueo cuesta rehacer un plan, fallar hacia el `Skip` cuesta
        // un fichero.
        let sin_lado = CompareRow {
            side: None,
            ..ambiguous_row(
                Side::Right,
                CompareReason::CaseFold,
                Some(src_file("README", 1)),
                Some(dst_file("readme", 1)),
            )
        };
        let items = run(vec![sin_lado], opts_update()).await;
        assert_eq!(one_blocker(&items).kind, SyncBlockerKind::AmbiguousDest);

        // Y con entrada SOLO del origen no hay colisión del destino posible: ahí
        // la evidencia es concluyente y sale el `Skip`.
        let solo_origen = CompareRow {
            side: None,
            ..ambiguous_row(
                Side::Left,
                CompareReason::CaseFold,
                Some(src_file("README", 1)),
                None,
            )
        };
        let items = run(vec![solo_origen], opts_update()).await;
        assert_eq!(one_step(&items).reason, Some(SyncReason::AmbiguousSource));
    }

    #[tokio::test]
    async fn the_side_of_a_collision_is_the_row_s_and_not_the_panel_s() {
        // Con el origen a la DERECHA, una colisión del lado izquierdo es del
        // DESTINO y bloquea, aunque el bloqueo se reporte como `Right` (convenio
        // de plan: origen `Left`, destino `Right`).
        let opts = SyncOptions {
            source_root: dest_root(),
            dest_root: source_root(),
            source_side: Side::Right,
            ..opts_update()
        };
        let items = run(
            vec![ambiguous_row(
                Side::Left,
                CompareReason::CaseFold,
                Some(src_file("README", 1)),
                None,
            )],
            opts,
        )
        .await;
        let b = one_blocker(&items);
        assert_eq!(b.kind, SyncBlockerKind::AmbiguousDest);
        assert_eq!(b.side, Some(Side::Right));
    }

    // ---------- bloqueos del árbol entero ----------

    #[tokio::test]
    async fn a_read_only_destination_blocks_before_a_single_step() {
        let opts = SyncOptions {
            dest_writable: false,
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
        let b = one_blocker(&items);
        assert_eq!(b.kind, SyncBlockerKind::DestReadOnly);
        assert!(b.rel.is_root(), "no es de un sitio: es del árbol entero");
        assert_eq!(b.side, Some(Side::Right));
        assert!(
            steps_of(&items).is_empty(),
            "no se planifican escrituras contra un árbol que las rehúsa"
        );
    }

    #[tokio::test]
    async fn a_read_only_destination_does_not_even_look_at_the_rows() {
        // No hay nada que un árbol pueda decir que cambie el resultado, y
        // arrastrar el walk entero por él cuesta minutos contra una red. La fila
        // de este test mataría el plan con `OutsideRoot` si se llegara a mirar.
        let opts = SyncOptions {
            dest_writable: false,
            ..opts_update()
        };
        let out = run_raw(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(entry_wire("file:///otro/x.txt", EntryKind::File, Some(1))),
                None,
            )],
            opts,
            CancellationToken::new(),
        )
        .await;
        assert_eq!(out.len(), 1, "solo el bloqueo: {out:?}");
        assert!(out[0].is_ok());
    }

    #[tokio::test]
    async fn a_read_only_destination_blocks_even_when_the_comparison_is_empty() {
        // El bloqueo no depende de que haya filas: sale antes de pedir la
        // primera. Un origen vacío contra un destino de solo lectura es un plan
        // que no se puede ejecutar, no un plan vacío que se aprueba solo.
        let opts = SyncOptions {
            dest_writable: false,
            ..opts_update()
        };
        let items = run(vec![], opts).await;
        assert_eq!(one_blocker(&items).kind, SyncBlockerKind::DestReadOnly);
    }

    #[tokio::test]
    async fn a_destination_directory_over_the_entry_limit_blocks() {
        // No se sabe qué hay en ese directorio, y el plan iba a escribir dentro.
        let items = run(
            vec![error_row(
                CompareReason::DirTooLarge,
                Side::Right,
                None,
                Some(dst_dir("fotos")),
            )],
            opts_update(),
        )
        .await;
        let b = one_blocker(&items);
        assert_eq!(b.kind, SyncBlockerKind::DirTooLarge);
        assert_eq!(b.rel, rel("fotos"));
        assert_eq!(b.side, Some(Side::Right));
        assert!(steps_of(&items).is_empty());
    }

    #[tokio::test]
    async fn the_same_limit_on_the_source_is_still_only_a_skip() {
        // No saber qué hay en un directorio del ORIGEN solo significa que de ahí
        // no se copia nada. No hay nada que perder en el destino.
        let items = run(
            vec![error_row(
                CompareReason::DirTooLarge,
                Side::Left,
                Some(src_dir("fotos")),
                None,
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Skip);
        assert_eq!(s.reason, Some(SyncReason::Unreadable));
        assert!(blockers_of(&items).is_empty());
    }

    // ---------- el solape que encuentra el walk ----------

    #[tokio::test]
    async fn reaching_the_other_root_prunes_and_blocks() {
        // `/a` contra `/a/sub`: dos `VPath` distintos nombrando un mismo árbol.
        // Copiar el primero sobre el segundo copia un subárbol dentro de sí
        // mismo. La comprobación estructural del daemon se puede derrotar con un
        // symlink; esta no depende de ella.
        let opts = SyncOptions {
            source_root: vpath("file:///a"),
            dest_root: vpath("file:///a/sub"),
            ..opts_update()
        };
        let items = run(
            vec![
                // El walk llegó a la raíz del destino…
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(entry_wire("file:///a/sub", EntryKind::Dir, None)),
                    None,
                ),
                // …y a todo lo que hay debajo.
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(entry_wire("file:///a/sub/x.txt", EntryKind::File, Some(1))),
                    None,
                ),
            ],
            opts,
        )
        .await;
        let b = one_blocker(&items);
        assert_eq!(b.kind, SyncBlockerKind::OverlapDetected);
        assert_eq!(b.rel, rel("sub"), "medido contra la raíz por la que iba");
        assert_eq!(b.side, None, "el solape es de las DOS raíces");
        assert!(
            steps_of(&items).is_empty(),
            "el subárbol se poda, no se copia dentro de sí mismo"
        );
    }

    #[tokio::test]
    async fn what_is_outside_the_overlap_is_still_planned() {
        // Se poda el SUBÁRBOL, no el plan: el resto del árbol sigue saliendo, y
        // el bloqueo es lo que impide aprobarlo.
        let opts = SyncOptions {
            source_root: vpath("file:///a"),
            dest_root: vpath("file:///a/sub"),
            ..opts_update()
        };
        let items = run(
            vec![
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(entry_wire("file:///a/sub/x.txt", EntryKind::File, Some(1))),
                    None,
                ),
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(entry_wire("file:///a/otro.txt", EntryKind::File, Some(1))),
                    None,
                ),
                // Y una fila EMPAREJADA, que es la que destapa si la poda mira
                // el lado que no debe: toda ruta del destino cuelga de
                // `dest_root` por definición, así que podar por ella se llevaría
                // el plan entero en vez del subárbol.
                row(
                    CompareVerdict::Different,
                    CompareCriterion::Size,
                    CompareConfidence::Certain,
                    Some(entry_wire("file:///a/par.txt", EntryKind::File, Some(2))),
                    Some(entry_wire(
                        "file:///a/sub/par.txt",
                        EntryKind::File,
                        Some(1),
                    )),
                ),
            ],
            opts,
        )
        .await;
        assert_eq!(one_blocker(&items).kind, SyncBlockerKind::OverlapDetected);
        let steps: Vec<_> = steps_of(&items)
            .iter()
            .map(|s| (s.kind, s.rel.to_wire()))
            .collect();
        assert_eq!(
            steps,
            vec![
                (SyncStepKind::Copy, "otro.txt".to_owned()),
                (SyncStepKind::Overwrite, "par.txt".to_owned()),
            ]
        );
    }

    #[tokio::test]
    async fn the_overlap_is_reported_once_however_deep_the_subtree_is() {
        // Se guarda la RAÍZ alcanzada y no la ruta de la fila que lo destapó, así
        // que un solo prefijo se traga el subárbol entero: cien filas dentro no
        // son cien bloqueos.
        let opts = SyncOptions {
            source_root: vpath("file:///a"),
            dest_root: vpath("file:///a/sub"),
            ..opts_update()
        };
        let mut rows = vec![row(
            CompareVerdict::OnlyLeft,
            CompareCriterion::Presence,
            CompareConfidence::Certain,
            Some(entry_wire("file:///a/sub", EntryKind::Dir, None)),
            None,
        )];
        for i in 0..100 {
            rows.push(row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(entry_wire(
                    &format!("file:///a/sub/d{i}/f{i}.txt"),
                    EntryKind::File,
                    Some(1),
                )),
                None,
            ));
        }
        let items = run(rows, opts).await;
        assert_eq!(blockers_of(&items).len(), 1);
        assert!(steps_of(&items).is_empty());
    }

    #[tokio::test]
    async fn a_destination_row_that_reaches_the_source_root_is_overlap_too() {
        // El espejo: la raíz del ORIGEN está dentro de la del destino, así que
        // quien llega a la otra raíz es una fila del lado del destino.
        let opts = SyncOptions {
            source_root: vpath("file:///a/sub"),
            dest_root: vpath("file:///a"),
            ..opts_update()
        };
        let items = run(
            vec![row(
                CompareVerdict::OnlyRight,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                None,
                Some(entry_wire("file:///a/sub/x.txt", EntryKind::File, Some(1))),
            )],
            opts,
        )
        .await;
        assert_eq!(one_blocker(&items).kind, SyncBlockerKind::OverlapDetected);
        assert!(steps_of(&items).is_empty());
    }

    #[tokio::test]
    async fn a_root_whose_bytes_prefix_the_other_is_not_an_overlap() {
        // `file:///a/su` son un prefijo de CADENA de `file:///a/sub` y no lo son
        // de segmentos. Por cadena, todo el árbol del origen «llegaría» a la raíz
        // del destino y el plan entero se bloquearía por nada (regla dura 1).
        let opts = SyncOptions {
            source_root: vpath("file:///a"),
            dest_root: vpath("file:///a/su"),
            ..opts_update()
        };
        let items = run(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(entry_wire("file:///a/sub", EntryKind::Dir, None)),
                None,
            )],
            opts,
        )
        .await;
        assert!(blockers_of(&items).is_empty());
        assert_eq!(one_step(&items).kind, SyncStepKind::CreateDir);
    }

    #[tokio::test]
    async fn two_providers_that_spell_their_root_the_same_are_not_an_overlap() {
        // El transductor no sabe de providers: dos `mem:///` de dos providers
        // distintos llegan aquí indistinguibles de un árbol contra sí mismo. Se
        // planifica, y no se pierde nada por ello — un árbol comparado consigo
        // mismo da filas `Same`, o sea cero pasos. Lo peligroso es la CONTENCIÓN.
        let raiz = vpath("mem:///");
        let opts = SyncOptions {
            source_root: raiz.clone(),
            dest_root: raiz,
            ..opts_update()
        };
        let items = run(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(entry_wire("mem:///a.txt", EntryKind::File, Some(1))),
                None,
            )],
            opts,
        )
        .await;
        assert!(blockers_of(&items).is_empty());
        assert_eq!(one_step(&items).kind, SyncStepKind::Copy);
    }

    #[tokio::test]
    async fn nothing_at_all_is_planned_under_a_pruned_subtree() {
        // Ni un `Skip`, ni un bloqueo de otra clase: dentro del subárbol podado
        // el plan no dice nada, porque nada de lo que hay ahí es del árbol que se
        // quiso sincronizar.
        let opts = SyncOptions {
            source_root: vpath("file:///a"),
            dest_root: vpath("file:///a/sub"),
            ..opts_update()
        };
        let items = run(
            vec![
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(entry_wire("file:///a/sub", EntryKind::Dir, None)),
                    None,
                ),
                CompareRow {
                    reason: Some(CompareReason::Unreadable),
                    side: Some(Side::Left),
                    ..row(
                        CompareVerdict::Error,
                        CompareCriterion::Presence,
                        CompareConfidence::Unknown,
                        Some(entry_wire("file:///a/sub/secreto", EntryKind::Dir, None)),
                        None,
                    )
                },
            ],
            opts,
        )
        .await;
        assert_eq!(items.len(), 1, "solo el bloqueo del solape: {items:?}");
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
    async fn real_compare_rows_carry_the_destination_spelling_down_the_tree() {
        // El test que ninguna prueba de mesa puede dar: el orden de las filas lo
        // pone el WALK, y el walk las emite por directorio —todas las de un
        // nivel y después las de cada subdirectorio—, así que entre `café` y su
        // hijo se cuelan sus hermanas y entre `café/RESUMÉ` y el suyo también.
        // La primera versión de esto era una pila, pasaba los cinco tests hechos
        // a mano y fallaba aquí: `nuevo.txt` salía sin `dest_rel` o, peor, con
        // uno que nombraba una carpeta que no existe en ninguno de los dos lados.
        use norte_compare::{CompareOptions, Sides, compare};
        use norte_vfs::Provider as _;
        let origen = norte_testkit::MemProvider::new();
        let destino = norte_testkit::MemProvider::new();
        // Origen en NFC; destino en NFD, que es lo que devuelve un macOS. La
        // clave de emparejamiento normaliza a NFC SIEMPRE, así que las dos
        // carpetas emparejan aunque sus bytes difieran.
        seed(
            &origen,
            &["café".as_bytes(), "RESUMÉ".as_bytes(), b"nuevo.txt"],
            b"nuevo",
        )
        .await;
        seed(&origen, &["café".as_bytes(), b"zz.txt"], b"hermana").await;
        seed(
            &destino,
            &[b"cafe\xcc\x81", b"RESUME\xcc\x81", b"viejo.txt"],
            b"viejo",
        )
        .await;

        let raiz = norte_testkit::MemProvider::root();
        let sides = Sides::from_capabilities(origen.capabilities(), destino.capabilities());
        let rows = compare(
            &origen,
            &raiz,
            &destino,
            &raiz,
            CompareOptions::cheap(),
            sides,
            Vec::new(),
            CancellationToken::new(),
        );
        let opts = SyncOptions {
            source_root: raiz.clone(),
            dest_root: raiz,
            ..opts_update()
        };
        let items: Vec<PlanItem> = plan(rows, opts, CancellationToken::new())
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.expect("ninguna fila del walk se sale de su raíz"))
            .collect();

        let copias: Vec<Ortografias<'_>> = steps_of(&items)
            .iter()
            .filter(|s| s.kind == SyncStepKind::Copy)
            .map(|s| (bytes_of(&s.rel), s.dest_rel.as_ref().map(bytes_of)))
            .collect();
        assert_eq!(
            copias,
            vec![
                // El orden es el del walk: primero el nivel de `café` entero
                // —la hermana incluida— y solo después lo que hay dentro de
                // `RESUMÉ`. Que `zz.txt` se cuele en medio es justamente lo que
                // rompía la primera versión de esto.
                (
                    vec!["café".as_bytes(), b"zz.txt"],
                    Some(vec![b"cafe\xcc\x81".as_slice(), b"zz.txt"])
                ),
                (
                    vec!["café".as_bytes(), "RESUMÉ".as_bytes(), b"nuevo.txt"],
                    Some(vec![
                        b"cafe\xcc\x81".as_slice(),
                        b"RESUME\xcc\x81",
                        b"nuevo.txt"
                    ])
                ),
            ],
            "cada copia entra en la carpeta que EXISTE en el destino"
        );
    }

    #[tokio::test]
    async fn real_compare_rows_plan_without_a_single_outside_root() {
        // Las 17 pruebas de mesa construyen sus filas a mano, así que ninguna
        // toca el contrato que cruza los dos crates: TODA ruta que el walk
        // emite cuelga de la raíz que se le dio. Si deja de cumplirse, el plan
        // entero muere con `OutsideRoot` — y solo se ve aquí.
        use norte_compare::{CompareOptions, Sides, compare};
        use norte_vfs::Provider as _;
        let origen = norte_testkit::MemProvider::new();
        let destino = norte_testkit::MemProvider::new();
        seed(&origen, &[b"sub", b"informe\xff\xfe.dat"], b"nuevo").await;
        seed(&origen, &[b"raiz.txt"], b"nuevo").await;
        seed(&destino, &[b"raiz.txt"], b"viejo mas largo").await;

        let raiz = norte_testkit::MemProvider::root();
        let sides = Sides::from_capabilities(origen.capabilities(), destino.capabilities());
        let rows = compare(
            &origen,
            &raiz,
            &destino,
            &raiz,
            CompareOptions {
                descend_orphans: Some(Side::Left),
                ..CompareOptions::cheap()
            },
            sides,
            Vec::new(),
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
