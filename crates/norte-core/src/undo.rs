//! Undo de sesión (M3-2): ejecuta la `Reversal` persistida de cada entrada del
//! journal en orden LIFO y appendea una entrada compensatoria (append-only, la
//! cadena sigue íntegra).
//!
//! **No-clobber (estricto).** `RenameBack`/`RestoreTrash` exigen destino LIBRE
//! antes de actuar (nunca sobrescriben). El undo de un `Created` es el caso
//! sutil: el journal no guarda identidad del nodo, así que se deshace SOLO vía
//! PAPELERA (recuperable) — sin capability `TRASH` la entrada se salta con
//! contador propio en el [`UndoReport`], jamás un `remove` permanente (#65):
//! lo que hoy vive en ese path puede ser un fichero que el usuario editó tras
//! la creación. Deuda: identidad (`node_id`/hash) en el `Created` para un undo
//! con verificación real.
//!
//! **Tres unidades, tres contratos.** [`undo_units`] agrupa por `batch_id` para
//! los tres, y [`revert_unit`] reparte:
//!
//! - una mutación SUELTA → [`revert_entry`];
//! - un lote de `fs.rename_batch` → [`revert_batch`], entero o nada (media
//!   permutación deshecha no es ningún estado);
//! - un lote de `sync.apply` → [`revert_sync_batch`], lo que se pueda y con
//!   nombres para lo que no (media sincronización deshecha SÍ es un estado: el
//!   árbol de antes con parte de los ficheros ya devueltos).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use norte_proto::{CapabilityFlags, ConflictKind, Error, TaskId, TaskKind, VPath};
use norte_vfs::Provider;
use tokio_util::sync::CancellationToken;

use crate::journal::{Actor, JournalEntry, NewEntry, Reversal, SqliteJournal};
use crate::progress::ProgressReporter;
use crate::rename::exec::{BatchJournal, BatchReport, PlannedStep};
use crate::rename::plan::{NameCaps, name_key};

/// Tope de unidades denegadas que el informe LISTA (#171). Las demás solo
/// cuentan.
///
/// El mismo número y el mismo motivo que los topes de `sync`: una lista sin
/// tope viaja por el wire y se queda en memoria del cliente, y con una policy
/// que deniegue por defecto son tantas filas como unidades tenga la sesión.
pub use norte_proto::methods::UNDO_MAX_DENIED_REPORTED;

/// Resultado de un [`crate::Engine::undo_session`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UndoReport {
    /// Entradas revertidas con éxito (compensación appendeada). Cuenta
    /// ENTRADAS, no unidades: un lote de `fs.rename_batch` aporta todas las
    /// suyas de golpe, porque se revierte entero o nada, y uno de `sync.apply`
    /// aporta las que de verdad volvieron. El progreso de la Task, en cambio,
    /// avanza por unidades — para el humano un lote es UN paso del undo.
    pub undone: u64,
    /// Entradas `Irreversible` encontradas y saltadas (no hay nada que pisar).
    pub skipped_irreversible: u64,
    /// Reversas de `Created` saltadas porque el provider NO tiene `TRASH`
    /// (#65): deshacerlas sería un borrado PERMANENTE de «lo que hoy vive en
    /// ese path» — sin `node_id` en el `Created`, puede ser trabajo del humano
    /// posterior a la creación. El nodo se queda; quien quiera borrarlo lo
    /// pide explícito (`fs.delete`).
    pub skipped_created_no_trash: u64,
    /// Primer paso bloqueado (drift/conflicto): `seq` original + motivo. La
    /// sesión para ahí (estricto).
    ///
    /// **No es lo mismo que [`Self::denied`]**, y confundirlos sería leer el
    /// informe al revés: esto dice «paré aquí y el árbol quedó consistente»;
    /// aquello dice «esta unidad no se tocó y el undo siguió con las demás».
    pub blocked: Option<(i64, Error)>,
    /// Unidades que la POLICY denegó, con el `seq` de su primera entrada y el
    /// motivo (#171). El undo NO para: bloquea esa unidad y sigue.
    ///
    /// Acotada a [`UNDO_MAX_DENIED_REPORTED`]; [`Self::denied_total`] las
    /// cuenta todas. Un undo de medio millón de entradas bajo una policy que
    /// deniega por defecto llenaría la memoria del cliente con la lista, que
    /// es el mismo fallo que #196 en el otro extremo.
    pub denied: Vec<(i64, Error)>,
    /// Cuántas unidades denegó la policy en total, recortadas o no.
    pub denied_total: u64,
    /// **Deshacer un lote se quedó a medias.** El ejecutor no pudo devolver
    /// algún paso de undo que ya había aplicado, así que el directorio NO
    /// volvió a como estaba: aquí está el paso concreto, con nombres.
    ///
    /// Cuando esto es `Some`, la Task termina `Failed` y no `Completed`: un
    /// undo que dice «hecho» promete un árbol restaurado, y este no lo está.
    /// Míralo aunque la Task haya fallado — el error solo cuenta la causa.
    pub batch_stuck: Option<crate::rename::StuckStep>,
    /// Reversas del undo de un lote que se APLICARON pero cuya compensación no
    /// se pudo escribir. Cada una deja una entrada que sigue pareciendo
    /// pendiente aunque su efecto ya volvió: un undo posterior la encontrará y
    /// se bloqueará ahí. Es la única señal de eso.
    pub compensations_lost: u64,
    /// **Lo que NO volvió**, por ruta y en bytes de wire (regla 1), recortado a
    /// [`UNDO_MAX_UNREVERTED_PATHS`].
    ///
    /// Lo llena el undo de un lote de SINCRONIZACIÓN (`sync.apply`), que
    /// revierte lo que puede en vez de rehusar
    /// entero: sin esta lista, un `skipped_irreversible: 3` sobre un lote de
    /// diez mil pasos es un número sin sitio donde mirar. Una ruta entra aquí
    /// por una de tres razones, y los contadores son los que las distinguen:
    /// la entrada era `irreversible` ([`Self::skipped_irreversible`]), era un
    /// `created` en un destino sin papelera
    /// ([`Self::skipped_created_no_trash`], #65), o su reversa se topó con
    /// drift ([`Self::blocked`], que nombra la primera).
    ///
    /// **Los contadores NO la parten en tres.** `skipped_irreversible` y
    /// `skipped_created_no_trash` son exactos, pero de las entradas con drift
    /// solo la primera queda en `blocked` y no hay contador para las demás:
    /// una lista de cinco con `skipped_irreversible: 1` significa «una
    /// irreversible y CUATRO que no volvieron por otra cosa», no cuatro
    /// bloqueos identificables. Y está recortada, así que sumar tampoco vale
    /// a partir de [`UNDO_MAX_UNREVERTED_PATHS`].
    ///
    /// El camino de renombrado no la usa — ese lote vuelve entero o no se
    /// toca, así que no hay «lo que no volvió».
    ///
    /// **Hoy no sale del proceso**: `PolicyUndoReportResult` no lleva este
    /// campo, así que un cliente remoto ve los contadores y no las rutas.
    /// Ponerlo en el wire es un cambio de `norte-proto` con sus goldens, y
    /// pide antes redactar la authority como hace
    /// [`crate::engine::span_path`](crate::Engine) — un `VPath` de wire puede
    /// llevar userinfo (regla 10).
    pub unreverted_paths: Vec<Vec<u8>>,
}

/// Cuántas rutas caben en [`UndoReport::unreverted_paths`].
///
/// El mismo criterio que las listas del wire (`SYNC_MAX_FAILURES_REPORTED` y
/// compañía): una muestra que quepa en una pantalla, con el contador al lado
/// diciendo la verdad entera. Un lote de sincronización puede tener medio
/// millón de pasos y este reporte vive en memoria del daemon.
pub const UNDO_MAX_UNREVERTED_PATHS: usize = 64;

/// Reconstruye un `VPath` desde los bytes `to_wire` guardados en el journal.
fn wire(bytes: &[u8]) -> Result<VPath, Error> {
    let s = std::str::from_utf8(bytes).map_err(|_| Error::InvalidPath)?;
    VPath::parse(s).map_err(|_| Error::InvalidPath)
}

/// `true` si `p` NO existe (libre) en `provider`.
///
/// Compartida con el rollback del ejecutor de lotes
/// (`crate::rename::exec`): el primitivo del no-clobber tiene que ser UNO, o
/// las dos copias dejan de tratar igual un `stat` que falla por otra cosa.
pub(crate) async fn is_free(provider: &dyn Provider, p: &VPath) -> Result<bool, Error> {
    match provider.stat(p).await {
        Err(Error::NotFound) => Ok(true),
        Ok(_) => Ok(false),
        Err(e) => Err(e),
    }
}

const OCCUPIED: Error = Error::Conflict {
    conflict: ConflictKind::Exists,
};

/// ¿`a` y `b` son el MISMO nodo? (#274)
///
/// Sin identidad —un provider que no la da— contesta `false`: lo que se decide
/// con esto es si se puede renombrar sobre lo que ocupa el origen, y ante la
/// duda no se toca nada.
async fn mismo_nodo(provider: &dyn Provider, a: &VPath, b: &VPath) -> Result<bool, Error> {
    let ida = provider.node_id(a, norte_vfs::FollowLinks::No).await?;
    let vuelta = provider.node_id(b, norte_vfs::FollowLinks::No).await?;
    Ok(matches!((ida, vuelta), (Some(x), Some(y)) if x == y))
}

/// Renombra `de` a `a` pasando por un nombre intermedio, para cuando las dos
/// rutas son el mismo nodo y el rename del provider no puede pisar (#274).
///
/// El gemelo de `ops::rename_de_ortografia` para el camino del undo. Vive
/// aparte y no comparte código con él porque aquí no hay `TaskCtx`, ni
/// observer, ni reintentos: deshacer ya corre dentro de su propia task y lo
/// que emite el journal es la compensación de arriba.
async fn rename_por_rodeo(provider: &dyn Provider, de: &VPath, a: &VPath) -> Result<(), Error> {
    for n in 0..1000u32 {
        let mut name = crate::rename::naming::TEMP_PREFIX.to_vec();
        name.extend_from_slice(format!("case-undo-{n}").as_bytes());
        let seg = norte_proto::Segment::new(name).map_err(|_| Error::InvalidPath)?;
        let paso = de.with_file_name(seg).ok_or(Error::InvalidPath)?;
        if !is_free(provider, &paso).await? {
            continue;
        }
        provider.rename(de, &paso).await?;
        if let Err(e) = provider.rename(&paso, a).await {
            // La vuelta atrás, o el fichero se queda con el nombre del rodeo y
            // el lector no tiene dónde buscarlo.
            if let Err(vuelta) = provider.rename(&paso, de).await {
                tracing::error!(
                    error = %e,
                    vuelta = %vuelta,
                    quedo = %crate::engine::span_path(&paso),
                    "deshacer un cambio de ortografía no pudo terminar ni volver"
                );
            }
            return Err(e);
        }
        return Ok(());
    }
    Err(OCCUPIED)
}

/// Los permisos POSIX que `p` tiene AHORA, o `None` si no se pueden leer
/// (#314).
///
/// `None` no es un fallo del que actúa: hay providers que no publican
/// `posix.mode`, y entonces lo honesto es registrar la mutación como
/// irreversible y decirlo, en vez de guardar un modo inventado que un undo
/// aplicaría después como si fuera el de antes.
///
/// Los doce bits de permiso, sin los de clase de nodo: es lo único que
/// `set_mode` acepta, y devolver `st_mode` entero haría que la reversa
/// intentara cambiar de qué clase es el nodo.
pub(crate) async fn modo_actual(provider: &dyn Provider, p: &VPath) -> Option<u32> {
    let req = norte_vfs::AttrRequest::sanitized(vec!["posix.mode".to_owned()]);
    let opt = norte_vfs::ListOptions { attrs: req };
    let entry = provider.stat_with(p, &opt).await.ok()?;
    match entry.attrs.get("posix.mode")? {
        norte_proto::AttrValue::Uint(m) => u32::try_from(*m)
            .ok()
            .map(|m| m & norte_proto::methods::MODE_PERMISSION_BITS),
        _ => None,
    }
}

/// ¿Tiene `dir` algún hijo?
///
/// Mira SOLO el primer ítem del listado: la pregunta es «¿está vacío?», y un
/// directorio con cien mil entradas la contesta con la primera. Nunca
/// materializa el listado, así que no tiene tope que reventar.
async fn has_children(provider: &dyn Provider, dir: &VPath) -> Result<bool, Error> {
    use futures::StreamExt as _;
    let mut listing = provider.list(dir).await?;
    match listing.next().await {
        Some(item) => {
            item?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Resultado de intentar revertir UNA unidad de undo (una entrada suelta o un
/// lote entero).
pub(crate) enum Reverted {
    /// Revertida y compensada.
    Done,
    /// `Irreversible`: saltada.
    SkippedIrreversible,
    /// Reversa de `Created` en provider sin `TRASH`: saltada, el nodo se
    /// queda (#65). Sin compensación (no hubo efecto); un undo posterior
    /// volverá a encontrarla — honesto.
    SkippedNoTrash,
    /// Bloqueada por drift/conflicto. `seq` es la entrada CONCRETA que no se
    /// pudo revertir — en un lote, la del paso que se atascó, no la del lote
    /// entero: es la que el humano tiene que ir a mirar.
    Blocked {
        /// La entrada bloqueada.
        seq: i64,
        /// Por qué.
        error: Error,
    },
    /// La unidad YA se contó ella misma en el [`UndoReport`] y la sesión
    /// sigue.
    ///
    /// Es lo que devuelve [`revert_sync_batch`], que revierte parte de un lote
    /// y salta el resto: `Done` haría que el llamante sumara TODAS las
    /// entradas de la unidad a `undone`, y `SkippedIrreversible` sumaría una
    /// por un lote que quizá revirtió nueve mil. Quien reparte entre los
    /// contadores es quien sabe qué le pasó a cada entrada.
    Accounted,
    /// El undo de un LOTE se aplicó a medias y tampoco pudo desandarse: el
    /// árbol NO volvió. A diferencia de `Blocked`, esto FALLA la Task — decir
    /// `Completed` sobre un directorio a medio revertir es la única mentira
    /// que este módulo no se puede permitir. El detalle (qué paso, con qué
    /// nombres) va en [`UndoReport::batch_stuck`].
    Stuck {
        /// La entrada cuyo paso se quedó aplicado.
        seq: i64,
        /// Por qué no pudo volver.
        error: Error,
    },
}

impl Reverted {
    /// Atajo del caso bloqueado, que se construye en once sitios.
    fn blocked(seq: i64, error: Error) -> Self {
        Self::Blocked { seq, error }
    }
}

/// Parte las entradas LIFO en UNIDADES de undo: una entrada suelta, o TODAS
/// las que comparten un `batch_id`.
///
/// **Un lote es UNA unidad**, y tiene que llegar a la task como un solo ítem.
/// Qué se hace con esa unidad depende de quién la escribió: un lote de
/// renombrado se revierte entero o no se toca (media permutación deshecha es
/// el estado que toda esta funcionalidad existe para impedir), y uno de
/// sincronización revierte lo que puede ([`revert_sync_batch`]). El agrupado
/// es el mismo para los dos, y por eso vive aquí y no en ninguno de ellos.
///
/// El agrupado es GLOBAL, no por entradas contiguas. El scheduler corre hasta
/// cuatro tasks por provider y el undo se encola con una clave propia, así que
/// otra mutación del MISMO actor puede aterrizar entre dos entradas del lote;
/// agrupar por contigüidad partiría ese lote en dos unidades y la primera lo
/// dejaría a medias. La unidad se ancla donde apareció su primer miembro (el
/// `seq` mayor), de modo que el orden LIFO entre unidades se conserva y los
/// miembros que quedan por debajo se ADELANTAN.
///
/// **Lo que ese adelanto cuesta.** Un LIFO estricto por entradas siempre puede
/// deshacerse; agrupar no. Si la mutación intercalada ocupa un nombre que el
/// lote necesita para volver, [`feasible`] lo ve y la unidad queda bloqueada —
/// y, siendo el undo estricto, la sesión para ahí y no llega a la intercalada
/// que lo desatascaría. Ejemplo: `seq 1` (lote) `a → tmp`, `seq 2` (suelta)
/// `x → a`, `seq 3` (lote) `tmp → b`. Deshacer el lote pide `a` libre y lo
/// ocupa el antiguo `x`; por entradas (3, 2, 1) habría salido. Se acepta a
/// sabiendas: un bloqueo honesto es preferible a media permutación deshecha,
/// que es el estado que esta función existe para impedir. Sale a mano.
///
/// No hay riesgo de cruzar actores: la lista viene de
/// [`crate::journal::Journal::revertible_for`], que ya filtra por actor, así
/// que un `batch_id` compartido por dos actores (imposible hoy: el lote lo
/// escribe una sola task con un solo actor) tampoco los uniría.
pub(crate) fn undo_units(entries: Vec<JournalEntry>) -> Vec<Vec<JournalEntry>> {
    let mut units: Vec<Vec<JournalEntry>> = Vec::new();
    let mut at: HashMap<i64, usize> = HashMap::new();
    for e in entries {
        match e.batch_id {
            Some(b) => {
                if let Some(&i) = at.get(&b) {
                    units[i].push(e);
                } else {
                    at.insert(b, units.len());
                    units.push(vec![e]);
                }
            }
            None => units.push(vec![e]),
        }
    }
    units
}

/// Ejecuta la reversa de `entry` sobre `provider` (verificada, estricta) y, si
/// tiene éxito, appendea la compensación con `actor` y `undoes_seq = entry.seq`.
///
/// `batch` es el lote de la COMPENSACIÓN, no el de la entrada deshecha:
/// `Some(id)` cuando esta reversa forma parte del undo de un lote —el de
/// sincronización, [`revert_sync_batch`]— y `None` para una mutación suelta.
/// Es una etiqueta de AGRUPACIÓN, para que el audit y cualquier lectura del
/// journal vean el undo de un lote como un lote; no lo vuelve deshacible
/// (`revertible_for` filtra `undoes_seq IS NULL`, así que una compensación no
/// se revierte nunca).
///
/// # Errors
/// Solo por fallo al PERSISTIR la compensación en el journal (regla 4) — y
/// entonces el EFECTO ya ocurrió: el nodo se movió y el journal no lo sabe. Un
/// conflicto/drift del FS NO es error: se devuelve `Reverted::Blocked`.
// Dispatch lineal por `Reversal` (4 ramas): más claro junto que fragmentado.
#[expect(
    clippy::too_many_lines,
    reason = "Dispatch lineal por `Reversal` (4 ramas): más claro junto que fragmentado"
)]
pub(crate) async fn revert_entry(
    provider: &dyn Provider,
    journal: &SqliteJournal,
    entry: &JournalEntry,
    actor: &Actor,
    batch: Option<i64>,
    cancel: &CancellationToken,
) -> Result<Reverted, Error> {
    let path = wire(&entry.path)?;
    match entry.reversal.as_str() {
        "irreversible" => Ok(Reverted::SkippedIrreversible),

        // Undo de un Created: quitar el nodo creado (si sigue existiendo).
        //
        // OJO (seguridad): el journal NO guarda identidad del nodo creado
        // (node_id/hash), así que no podemos distinguir «el fichero que el
        // agente creó» de «ese fichero con contenido que el usuario editó
        // DESPUÉS» (una edición de contenido no genera un nuevo `Created`).
        // Por eso el undo va SIEMPRE por PAPELERA (recuperable): si el usuario
        // había modificado el nodo, su trabajo queda en la papelera en vez de
        // destruido. Sin capability `TRASH` (sftp/object con `logical_trash`
        // OFF — el caso común remoto) NO se cae a `remove` permanente: la
        // entrada se SALTA y el nodo se queda (#65). Deuda: identidad en el
        // `Created` para un undo con verificación real.
        "delete" => {
            // El stat va PRIMERO: un drift (el nodo ya no está) bloquea
            // SIEMPRE — clasificarlo como skip-por-no-trash tragaría la
            // señal de divergencia que el modo estricto valora.
            let node = match provider.stat(&path).await {
                Ok(node) => node,
                // Ya no está: estado inesperado → bloquea (no finge éxito).
                Err(e) => return Ok(Reverted::blocked(entry.seq, e)),
            };
            // Un DIRECTORIO creado solo se deshace VACÍO. La papelera se lleva
            // el subárbol entero, así que un directorio con contenido que el
            // journal no explica —un fichero que el usuario metió dentro
            // después, o un hijo del mismo lote cuya reversa no pudo volver—
            // desaparecería de la vista sin que nada lo nombrara. El orden
            // `seq` descendente lo deja vacío por construcción cuando todo va
            // bien; esto convierte esa suposición en una comprobación.
            if node.kind == norte_proto::EntryKind::Dir {
                match has_children(provider, &path).await {
                    Ok(true) => {
                        tracing::info!(
                            seq = entry.seq,
                            "el directorio creado tiene contenido que este undo no puso: se deja",
                        );
                        return Ok(Reverted::blocked(entry.seq, OCCUPIED));
                    }
                    Ok(false) => {}
                    Err(e) => return Ok(Reverted::blocked(entry.seq, e)),
                }
            }
            if !provider
                .capabilities()
                .flags
                .contains(CapabilityFlags::TRASH)
            {
                return Ok(Reverted::SkippedNoTrash);
            }
            // Id del trash compensatorio (#99): el `seq` del evento deshecho
            // (único) hace de contador; DENTRO de un `undo_session` el `now_ms`
            // se computa una vez por reversa, así que el reintento de
            // `trash_retrying` converge. (Entre invocaciones SEPARADAS de undo
            // el `now_ms` difiere: solo `seq` es estable — acotado, sin pérdida.)
            // Se enruta por `trash_retrying` para que un transitorio-tras-efecto
            // no pierda el `reversal_ref` también en el camino del undo.
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
            let comp_trash_id = norte_vfs::trash::TrashId::new(now_ms, entry.seq.unsigned_abs());
            let (comp_op, comp_reversal, comp_ref) =
                match crate::ops::trash_retrying(provider, &path, &comp_trash_id, cancel).await {
                    Ok(dest) => ("trashed", Reversal::RestoreTrash, dest),
                    // La cancelación NO es un bloqueo: la Task tiene que
                    // terminar `Cancelled` (regla 3), no `Completed` con un
                    // motivo. El token es el de la Task, así que la escalera
                    // de reintentos no sigue corriendo tras un corte.
                    Err(Error::Cancelled) => return Err(Error::Cancelled),
                    Err(e) => return Ok(Reverted::blocked(entry.seq, e)),
                };
            let comp_ref_bytes = comp_ref.as_ref().map(|d| d.to_wire().into_bytes());
            if let Err(e) = journal
                .journal()
                .record_entry(&NewEntry {
                    op: comp_op,
                    path: &entry.path,
                    path_to: None,
                    reversal: comp_reversal,
                    reversal_ref: comp_ref_bytes.as_deref(),
                    actor,
                    undoes_seq: Some(entry.seq),
                    batch_id: batch,
                })
                .await
            {
                // El nodo YA está en la papelera y el journal no lo sabe: el
                // log es lo único que queda para contestar «¿dónde ha ido mi
                // fichero?». Va con las dos rutas, redactadas (regla 10), como
                // hace `sync::exec` al enterrar.
                tracing::error!(
                    seq = entry.seq,
                    error = %e,
                    enterrado = %crate::engine::span_path(&path),
                    destino = comp_ref.as_ref().map(crate::engine::span_path).unwrap_or_default(),
                    "reversa aplicada sin compensar: el nodo está en la papelera y el journal no lo registra",
                );
                return Err(Error::from(e));
            }
            Ok(Reverted::Done)
        }

        // Undo de un Renamed: devolver el nodo de `path`(destino) a
        // `path_to`(origen). El origen debe estar LIBRE.
        //
        // TOCTOU is_free→rename: en local lo cierra `renameat2(NOREPLACE)`;
        // en sftp (posix-rename clobbering) y object (copy+delete) la ventana
        // existe — deuda de providers remotos, ventana estrecha.
        "rename_back" => {
            let Some(from_bytes) = entry.path_to.as_deref() else {
                return Ok(Reverted::blocked(entry.seq, Error::InvalidPath));
            };
            let from = wire(from_bytes)?;
            // Deshacer un cambio de ORTOGRAFÍA (#274) no puede pedir que el
            // origen esté libre: en el volumen que pliega —el único donde ese
            // rename ocurre— `stat("Foo.txt")` encuentra el `foo.txt` que
            // acabamos de crear, así que `is_free` dice «ocupado» SIEMPRE y el
            // undo se bloqueaba de forma garantizada. Y un `Blocked` estrangula
            // el LIFO: deja varado todo lo anterior de la sesión (#128).
            //
            // Lo que desempata es la identidad: si lo que ocupa el origen es el
            // MISMO nodo que estamos devolviendo, no hay nada que respetar ahí
            // — es él. Entonces se renombra por el rodeo, igual que se hizo a
            // la ida, porque el rename del provider tampoco puede pisar.
            let ocupa_el_mismo = match is_free(provider, &from).await {
                Ok(true) => false,
                Ok(false) => match mismo_nodo(provider, &path, &from).await {
                    Ok(true) => true,
                    Ok(false) => return Ok(Reverted::blocked(entry.seq, OCCUPIED)),
                    Err(e) => return Ok(Reverted::blocked(entry.seq, e)),
                },
                Err(e) => return Ok(Reverted::blocked(entry.seq, e)),
            };
            let vuelta = if ocupa_el_mismo {
                rename_por_rodeo(provider, &path, &from).await
            } else {
                provider.rename(&path, &from).await
            };
            if let Err(e) = vuelta {
                return Ok(Reverted::blocked(entry.seq, e));
            }
            // Compensación: renamed inverso (destino=origen, origen=destino).
            journal
                .journal()
                .record_entry(&NewEntry {
                    op: "renamed",
                    path: from_bytes,
                    path_to: Some(&entry.path),
                    reversal: Reversal::RenameBack,
                    reversal_ref: None,
                    actor,
                    undoes_seq: Some(entry.seq),
                    batch_id: batch,
                })
                .await
                .map_err(Error::from)?;
            Ok(Reverted::Done)
        }

        // Undo de un ModeChanged (#314): devolver los permisos que tenía.
        //
        // El modo anterior viene en `reversal_ref`, en ASCII decimal (ver
        // `Reversal::SetModeBack`). Sin él —o ilegible— la entrada estaría
        // clasificada `Irreversible` y no llegaría aquí; que llegue igual es
        // un journal corrupto, y entonces se BLOQUEA en vez de inventarse un
        // modo. No hay comprobación de «está libre» que hacer: esto no crea ni
        // mueve nada, solo devuelve doce bits a lo que haya en esa ruta.
        //
        // «Lo que haya», y no «el nodo que cambió»: el journal no guarda la
        // identidad del nodo —el mismo hueco que el brazo de `delete` razona
        // más arriba—, así que si aquello se borró y alguien creó otra cosa
        // con ese nombre, esta reversa le pone los permisos del anterior. El
        // techo del daño es más bajo que el de un borrado, pero conviene no
        // fingir una garantía que no se comprueba.
        "set_mode_back" => {
            let Some(anterior) = entry
                .reversal_ref
                .as_deref()
                .and_then(|b| std::str::from_utf8(b).ok())
                .and_then(|s| s.parse::<u32>().ok())
            else {
                return Ok(Reverted::blocked(entry.seq, Error::InvalidPath));
            };
            // Un modo corrupto en el journal no se le pasa al provider: los
            // bits de clase de nodo no son un permiso, y el trait manda
            // rechazarlos en vez de recortarlos.
            if anterior & !norte_proto::methods::MODE_PERMISSION_BITS != 0 {
                return Ok(Reverted::blocked(entry.seq, Error::InvalidPath));
            }
            // El modo de AHORA, para que la compensación diga la verdad sobre
            // lo que deshizo. Si no se puede leer NO se inventa: la
            // compensación queda irreversible, que es lo mismo que hace la
            // mutación original cuando no pudo leer el suyo. Caer al `path_to`
            // —el modo que se PIDIÓ— prometería un rehacer hacia un valor que
            // nadie llegó a observar, y `chmod(2)` puede haberlo cambiado por
            // el camino (limpia setgid en silencio).
            let actual = modo_actual(provider, &path).await;
            if let Err(e) = provider.set_mode(&path, anterior).await {
                return Ok(Reverted::blocked(entry.seq, e));
            }
            journal
                .journal()
                .record_entry(&NewEntry {
                    op: "mode_changed",
                    path: &entry.path,
                    path_to: Some(anterior.to_string().as_bytes()),
                    reversal: if actual.is_some() {
                        Reversal::SetModeBack
                    } else {
                        Reversal::Irreversible
                    },
                    reversal_ref: actual.map(|m| m.to_string().into_bytes()).as_deref(),
                    actor,
                    undoes_seq: Some(entry.seq),
                    batch_id: batch,
                })
                .await
                .map_err(Error::from)?;
            Ok(Reverted::Done)
        }

        // Undo de un Trashed: restaurar al original. El original debe estar LIBRE.
        "restore_trash" => {
            match is_free(provider, &path).await {
                Ok(true) => {}
                Ok(false) => return Ok(Reverted::blocked(entry.seq, OCCUPIED)),
                Err(e) => return Ok(Reverted::blocked(entry.seq, e)),
            }
            let res = match entry.reversal_ref.as_deref() {
                // Papelera que NOMBRÓ su destino (lógica, o la freedesktop de
                // `norte-vfs-local`): se restaura desde esa ruta exacta, sin
                // adivinar. `restore_from` y no `rename` porque una papelera
                // puede tener metadatos al lado del payload que se van con él
                // (el `.trashinfo` de freedesktop); el default del trait ES el
                // rename, así que la lógica no cambia de comportamiento.
                Some(dest_bytes) => {
                    let dest = wire(dest_bytes)?;
                    provider.restore_from(&dest, &path).await
                }
                // Papelera nativa: restore por ruta original.
                None => provider.restore_trashed(&path).await,
            };
            if let Err(e) = res {
                return Ok(Reverted::blocked(entry.seq, e));
            }
            // Compensación: el nodo reapareció en `path` (una creación).
            journal
                .journal()
                .record_entry(&NewEntry {
                    op: "created",
                    path: &entry.path,
                    path_to: None,
                    reversal: Reversal::Delete,
                    reversal_ref: None,
                    actor,
                    undoes_seq: Some(entry.seq),
                    batch_id: batch,
                })
                .await
                .map_err(Error::from)?;
            Ok(Reverted::Done)
        }

        // Etiqueta de reversa desconocida (journal de un core más nuevo): trata
        // como bloqueo honesto, no adivines.
        _ => Ok(Reverted::blocked(entry.seq, Error::Unsupported)),
    }
}

/// El paso inverso de una entrada de lote, con el `seq` que compensa.
struct Inverse {
    /// Nombre base que el nodo tiene AHORA (el `path` de la entrada).
    from: Vec<u8>,
    /// Nombre base al que vuelve (el `path_to` de la entrada).
    to: Vec<u8>,
    /// La entrada que este paso revierte.
    seq: i64,
}

/// La cadena inversa de un lote: por cada entrada, de donde el nodo está AHORA
/// a donde estaba. En el mismo orden LIFO que llega, que es justo el orden en
/// que hay que aplicarla.
///
/// Comprueba de paso las dos precondiciones estructurales de un lote: es todo
/// `rename_back` y vive todo en UN directorio (así lo escribe
/// [`crate::Engine::rename_batch`]). Un journal que diga otra cosa está
/// corrupto o viene de un core que no conocemos: se bloquea, no se adivina —
/// `Err` es siempre un `Reverted::Blocked` con el `seq` culpable, jamás un
/// error que mate la sesión entera.
fn inverse_chain(unit: &[JournalEntry], dir: &VPath) -> Result<Vec<Inverse>, Reverted> {
    let mut steps: Vec<Inverse> = Vec::with_capacity(unit.len());
    for e in unit {
        let bad = || Reverted::blocked(e.seq, Error::InvalidPath);
        if e.reversal.as_str() != Reversal::RenameBack.as_str() {
            tracing::error!(seq = e.seq, "entrada de lote con reversa que no es rename");
            return Err(Reverted::blocked(e.seq, Error::Unsupported));
        }
        let now = wire(&e.path).map_err(|_| bad())?;
        let before = wire(e.path_to.as_deref().ok_or_else(bad)?).map_err(|_| bad())?;
        if now.parent().as_ref() != Some(dir) || before.parent().as_ref() != Some(dir) {
            tracing::error!(seq = e.seq, "entrada de lote fuera del directorio del lote");
            return Err(bad());
        }
        let (Some(from), Some(to)) = (now.file_name(), before.file_name()) else {
            return Err(bad());
        };
        steps.push(Inverse {
            from: from.as_bytes().to_vec(),
            to: to.as_bytes().to_vec(),
            seq: e.seq,
        });
    }
    Ok(steps)
}

/// ¿Se puede aplicar TODA la cadena inversa contra el listado actual?
///
/// Simulación pura sobre las claves de comparación del directorio
/// ([`name_key`]): un paso necesita su origen presente y su destino libre, y
/// cada paso libera y ocupa nombres para el siguiente. Es lo que hace que el
/// lote sea todo-o-nada: se responde ANTES de tocar el provider, así que un
/// miembro irreversible deja el lote entero sin tocar en vez de a medias.
///
/// Un rename que solo cambia el CASO (`Foo → foo` en un directorio que no
/// distingue) tiene origen y destino con la misma clave: ni libera ni ocupa, y
/// su destino «ocupado» es él mismo.
///
/// **Responde TODO-O-NADA, no no-clobber.** Es una foto del listado, y entre
/// la foto y el primer `rename` cabe otra task (el undo se encola con clave
/// propia, así que el scheduler no lo serializa contra un `fs.*` del mismo
/// directorio). Quien impide pisar es el `rename` del provider: atómico en
/// local (`renameat2(NOREPLACE)`), check-then-act en sftp y object — la misma
/// ventana que ya tiene la IDA del lote, que también planifica sobre un
/// listado y luego renombra a pelo. Cerrarla es tarea de las dos mitades a la
/// vez, no de esta sola.
///
/// **Bloqueo en dos niveles, no uno solo (#128).** Un directorio que NO
/// normaliza (ext4) puede tener `café` NFC y `café` NFD como dos ficheros
/// distintos; ambos comparten [`name_key`] pero no comparten bytes. Un lote
/// que movió el NFD y cuyo undo quiere restaurarlo se topaba con el gemelo
/// NFC y se leía como "destino ocupado" — un fichero que el rename jamás
/// tocaría bloqueando la reversión de otro, y con LIFO estricto eso se
/// llevaba por delante el undo de todo lo anterior en la sesión también.
///
/// Por eso esta función rastrea DOS colecciones: `occupied_bytes` (los
/// nombres EXACTOS presentes) decide el bloqueo, y `occupied_keys` (las
/// claves [`name_key`] presentes, con su cuenta — dos ficheros pueden
/// compartir clave) decide si el ORIGEN de un paso sigue estando ahí, el
/// mismo criterio de antes. Un gemelo que solo comparte clave YA NO bloquea
/// el destino; un ocupante que comparte los BYTES exactos sigue bloqueando
/// igual que siempre.
///
/// Esto es seguro — y no meramente optimista — porque lo que de verdad
/// impide pisar es el `rename` del provider, no esta simulación, y los tres
/// providers comparan bytes exactos en ese punto: local con
/// `renameat2(RENAME_NOREPLACE)` (atómico, compara la entrada de directorio
/// tal cual), sftp con un `stat` previo sobre la ruta remota tal cual
/// (`provider.rs`, `rename`: `if self.exists(&to_r)`), y object con
/// `ensure_absent` sobre la key exacta (`provider.rs`, `rename`) — ninguno
/// de los tres hace un `stat`/lookup consciente de folding NFC/NFD. Relajar
/// el bloqueo a bytes exactos no abre, pues, una ventana que el `rename`
/// real fuera a colar: si los bytes coinciden con algo real, el provider lo
/// rechaza igual (atómico en local, con la misma ventana TOCTOU de siempre
/// en sftp/object — esta función nunca prometió cerrarla, ver más arriba).
///
/// Un plegado por MAYÚSCULAS/minúsculas es distinto: en un directorio que de
/// verdad no distingue caso, dos nombres que solo difieren en caso no pueden
/// ser dos entradas separadas — son el mismo fichero, y el propio sistema de
/// ficheros ya resuelve la búsqueda plegando caso antes de llegar al
/// `rename`. Un listado real de un directorio así nunca puede traer dos
/// bytes distintos bajo la misma clave por esa vía; el caso que sí ocurre de
/// verdad, y el único que este cambio relaja, es NFC/NFD en un directorio
/// que no normaliza.
fn feasible(steps: &[Inverse], listing: &[Vec<u8>], caps: NameCaps) -> Result<(), (usize, Error)> {
    let mut occupied_bytes: HashSet<Vec<u8>> = listing.iter().cloned().collect();
    let mut occupied_keys: HashMap<Vec<u8>, u32> = HashMap::new();
    for n in listing {
        *occupied_keys
            .entry(name_key(n, caps).into_owned())
            .or_insert(0) += 1;
    }
    for (i, s) in steps.iter().enumerate() {
        let fk = name_key(&s.from, caps).into_owned();
        if !occupied_keys.contains_key(&fk) {
            return Err((i, Error::NotFound));
        }
        if s.from != s.to {
            // El bloqueo es por BYTES exactos, no por clave: un gemelo que
            // solo comparte `name_key` no es el fichero que este paso
            // tocaría (ver rustdoc de la función).
            if occupied_bytes.contains(&s.to) {
                return Err((i, OCCUPIED));
            }
            // El paso vacía Y ocupa en los DOS niveles: dejar uno desfasado
            // haría que el paso SIGUIENTE de esta misma simulación viera un
            // origen que ya no está, o un destino libre que en realidad
            // sigue ocupado.
            occupied_bytes.remove(&s.from);
            occupied_bytes.insert(s.to.clone());
            let tk = name_key(&s.to, caps).into_owned();
            if fk != tk {
                if let Some(count) = occupied_keys.get_mut(&fk) {
                    // Ya se comprobó `occupied_keys.contains_key(&fk)` más
                    // arriba en esta MISMA iteración, y nada entre medias lo
                    // toca: la cuenta no puede ser 0 aquí. `saturating_sub`
                    // solo evita que una futura rotura de ese invariante
                    // vuelva esto un underflow silencioso; el `debug_assert`
                    // es lo que la haría RUIDOSA en tests.
                    debug_assert!(*count > 0, "clave {fk:?} contada en cero");
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        occupied_keys.remove(&fk);
                    }
                }
                *occupied_keys.entry(tk).or_insert(0) += 1;
            }
        }
    }
    Ok(())
}

/// Revierte un LOTE (`fs.rename_batch`) como UNA unidad: entero, o nada.
///
/// `unit` son las entradas revertibles que comparten un `batch_id`, en orden
/// LIFO. La reversa es la cadena inversa de sus pasos —cada entrada dice de
/// dónde a dónde fue el nodo, así que deshacerlas en LIFO devuelve el
/// directorio exactamente a donde estaba, temporales incluidos—, y se ejecuta
/// por el MISMO ejecutor que la ida ([`crate::rename::exec::run`]): un fallo a
/// mitad del undo desanda lo que el undo llevaba aplicado, en vez de dejar el
/// lote medio revertido.
///
/// **Todo-o-nada.** La cadena se simula primero contra el listado ACTUAL
/// ([`feasible`]); si un solo paso no cabe, no se aplica ninguno y la unidad
/// vuelve `Blocked` con el `seq` del paso que estorba. Es la propiedad por la
/// que existe esta función: media permutación deshecha es peor que ninguna.
///
/// Cada paso lleva `undoes: Some(seq)` de la entrada que revierte, así que
/// ninguna compensación queda con pinta de mutación nueva revertible, y todas
/// van bajo un `batch_id` FRESCO: el undo de un lote es a su vez un lote.
///
/// Lo que el ejecutor no pudo desandar (`stuck`) y las compensaciones que se
/// perdieron por el camino se vuelcan en `report`: son detalle del lote que el
/// [`UndoReport`] de la sesión no tenía dónde contar, y callarlos dejaría al
/// llamante creyendo que el árbol volvió.
///
/// # Errors
/// SOLO [`Error::Cancelled`], cuando el token se cancela entre pasos y el
/// ejecutor ya desandó lo suyo (regla 3). Cualquier otro fallo —conflicto,
/// provider, journal, listado— vuelve como `Blocked`/`Stuck`, para que el
/// reporte diga DÓNDE en vez de morir con un error pelado y una sesión entera
/// sin intentar.
///
/// # Panics
/// Solo por envenenamiento de un `Mutex` de reporte (otro hilo panicó
/// sosteniéndolo), mismo criterio que el resto del core.
#[tracing::instrument(
    skip_all,
    fields(task_id = %task_id, batch = unit.len(), seq = unit.first().map_or(0, |e| e.seq))
)]
pub(crate) async fn revert_batch(
    provider: &dyn Provider,
    journal: &Arc<SqliteJournal>,
    unit: &[JournalEntry],
    actor: &Actor,
    cancel: &CancellationToken,
    task_id: TaskId,
    report: &Mutex<UndoReport>,
) -> Result<Reverted, Error> {
    let Some(first) = unit.first() else {
        // Imposible: `undo_units` jamás produce una unidad vacía.
        return Ok(Reverted::Done);
    };
    // La unidad viene de `revertible_for`, que filtra por actor, y de
    // `undo_units`, que agrupa por `batch_id`. Si alguna vez dejara de ser
    // cierto, este ejecutor revertiría MEDIO lote creyéndolo entero — que es
    // justo lo que no puede pasar. Se afirma donde se consume.
    debug_assert!(
        unit.iter().all(|e| e.batch_id == first.batch_id
            && e.actor_kind == first.actor_kind
            && e.actor_id == first.actor_id),
        "una unidad de undo es UN lote de UN actor",
    );
    let Some(dir) = wire(&first.path).ok().and_then(|p| p.parent()) else {
        return Ok(Reverted::blocked(first.seq, Error::InvalidPath));
    };
    let steps = match inverse_chain(unit, &dir) {
        Ok(steps) => steps,
        Err(blocked) => return Ok(blocked),
    };

    // Se le pregunta al DIRECTORIO, no al provider (ADR 0054): el lote se
    // planificó con el plegado de este directorio, y deshacerlo con otro es lo
    // que hace que `feasible` declare viable un paso inverso que el filesystem
    // va a colapsar. Un fallo aquí bloquea la unidad y lo dice, como el fallo
    // de listar de abajo — jamás mata la sesión de undo.
    let caps = match provider.capabilities_at(&dir).await {
        Ok(caps) => caps,
        Err(error) => return Ok(Reverted::blocked(first.seq, error)),
    };
    if caps.flags.contains(CapabilityFlags::READ_ONLY) {
        return Ok(Reverted::blocked(first.seq, Error::Unsupported));
    }
    let name_caps = NameCaps::from_capabilities(caps);
    // Un fallo al listar NO mata la sesión: bloquea esta unidad y el reporte
    // dice cuál. Morir aquí devolvería un `UndoReport` vacío, sin `seq` ni
    // motivo, y con todo lo más viejo de la sesión sin intentar siquiera —
    // incluido el caso tonto de un directorio que desde entonces creció por
    // encima del tope planificable.
    let listing = match crate::engine::list_base_names(provider, &dir).await {
        Ok(names) => names,
        Err(error) => return Ok(Reverted::blocked(first.seq, error)),
    };
    if let Err((i, error)) = feasible(&steps, &listing, name_caps) {
        // El paso i no cabe → NADA se aplica. El `seq` es el de ESE paso.
        tracing::info!(
            seq = steps[i].seq,
            error = %error,
            "el lote no se puede deshacer entero: se deja intacto",
        );
        return Ok(Reverted::blocked(steps[i].seq, error));
    }

    let planned: Vec<PlannedStep> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            Ok(PlannedStep {
                from: dir.join(seg(&s.from)?),
                to: dir.join(seg(&s.to)?),
                // El «par» de un undo es la entrada que revierte: así el
                // `failed_pair` del reporte del ejecutor se indexa de vuelta
                // en `steps` y sale un `seq`. Saturar el índice rompería esa
                // correspondencia, así que se rechaza en vez de aliasear (el
                // tope de pares del wire lo deja inalcanzable, pero el
                // acoplamiento queda dicho).
                pair_index: u32::try_from(i).map_err(|_| Error::LimitExceeded {
                    limit: Error::LIMIT_ENTRIES.into(),
                })?,
                undoes: Some(s.seq),
            })
        })
        .collect::<Result<_, Error>>()?;

    let batch_id = match journal.journal().alloc_batch().await {
        Ok(id) => id,
        Err(e) => return Ok(Reverted::blocked(first.seq, Error::from(e))),
    };
    let recorder = BatchJournal {
        journal: Arc::clone(journal),
        actor: actor.clone(),
        batch_id,
    };
    // El ejecutor publica progreso paso a paso y el undo lo cuenta por
    // unidades: darle el reporter de la task le dejaría reescribir su
    // `entries_total`. Este emisor existe solo para satisfacer la firma; sus
    // snapshots no los recibe nadie (el receptor se descarta aquí mismo).
    let (progress, _rx) = ProgressReporter::new(task_id, TaskKind::Undo);
    let batch_report = Mutex::new(BatchReport::default());
    let outcome = crate::rename::exec::run(
        provider,
        &recorder,
        &planned,
        cancel,
        &progress,
        &batch_report,
    )
    .await;

    let snapshot = batch_report.lock().expect("batch report lock").clone();
    interpret(outcome, &snapshot, &steps, first.seq, report)
}

/// Traduce lo que hizo el ejecutor a un [`Reverted`], volcando en `report` el
/// detalle que el [`UndoReport`] de la sesión no tenía dónde contar.
///
/// Un `seq` cualquiera no vale: el reporte del ejecutor habla de `pair_index`,
/// que aquí se indexa de vuelta en `steps` para nombrar la ENTRADA concreta.
///
/// # Panics
/// Solo por envenenamiento del `Mutex` del reporte.
fn interpret(
    outcome: Result<(), Error>,
    snapshot: &BatchReport,
    steps: &[Inverse],
    fallback_seq: i64,
    report: &Mutex<UndoReport>,
) -> Result<Reverted, Error> {
    let seq_of = |i: u32| {
        steps
            .get(i as usize)
            .map_or(fallback_seq, |s: &Inverse| s.seq)
    };
    if snapshot.compensations_lost > 0 {
        tracing::error!(
            lost = snapshot.compensations_lost,
            "reversas del undo aplicadas sin compensar: la sesión se bloqueará aquí",
        );
        report.lock().expect("undo report lock").compensations_lost += snapshot.compensations_lost;
    }
    if let Some(s) = snapshot.stuck.clone() {
        tracing::error!(
            pair_index = s.pair_index,
            still_applied = s.still_applied,
            "deshacer el lote se quedó a medias: el árbol NO volvió del todo",
        );
        let (seq, error) = (seq_of(s.pair_index), s.error.clone());
        report.lock().expect("undo report lock").batch_stuck = Some(s);
        // El árbol NO volvió: esto FALLA la task. `Blocked` la dejaría
        // `Completed`, prometiendo un árbol restaurado — y en cancelación es
        // exactamente la mentira que el ejecutor evita devolviendo un error
        // distinto de `Cancelled` cuando su rollback se atasca.
        return Ok(Reverted::Stuck { seq, error });
    }
    match outcome {
        Ok(()) => Ok(Reverted::Done),
        // Cancelación: el ejecutor ya desandó lo que llevaba, y la task tiene
        // que terminar `Cancelled` como cualquier otra (regla 3).
        Err(Error::Cancelled) => Err(Error::Cancelled),
        // Lo demás es un bloqueo LIMPIO: sin `stuck`, el ejecutor devolvió el
        // árbol exactamente a como estaba, así que el LIFO estricto para aquí
        // con el `seq` del paso que falló.
        Err(error) => Ok(Reverted::blocked(
            snapshot.failed_pair.map_or(fallback_seq, seq_of),
            error,
        )),
    }
}

/// ¿Es esta unidad un lote de SINCRONIZACIÓN (`sync.apply`) y no uno de
/// renombrado (`fs.rename_batch`)?
///
/// El journal no lleva columna que diga qué método escribió un lote —y esta
/// tarea no cambia su esquema—, así que la distinción sale de la FORMA de las
/// entradas, que es lo que de verdad decide qué undo se puede aplicar:
///
/// - un lote de renombrado es TODO `renamed`/`rename_back` (así lo escribe
///   [`crate::rename::exec::run`], y su undo devuelve la permutación entera o
///   ninguna parte de ella);
/// - un lote de sincronización son las TRES formas de la tabla de
///   `crate::sync::exec`, y solo esas: `created` (reversa `delete` o
///   `irreversible`), `trashed` (`restore_trash`) y `removed`
///   (`irreversible`).
///
/// **La comprobación es POSITIVA, y ahí está su valor de seguridad.** «No es
/// un lote de renombrados» no es «es uno de sincronización»: con el criterio
/// negativo, reescribir la columna `reversal` de un lote de renombrados a
/// `delete` lo pasaba del camino que lo REHÚSA ([`revert_batch`] bloquea todo
/// lo que no sea `rename_back`) a uno que manda a la papelera el `path` de
/// cada entrada —que en un renombrado es el DESTINO—. Exigiendo también el
/// `op` hace falta reescribir dos columnas, y una forma futura que este core
/// no conozca cae del lado que rehúsa en vez del que actúa.
///
/// Una unidad MIXTA tampoco es de sincronización, por lo mismo: cae en
/// [`revert_batch`], que la bloquea sin adivinar.
fn is_sync_unit(unit: &[JournalEntry]) -> bool {
    !unit.is_empty()
        && unit.iter().all(|e| {
            e.batch_id.is_some()
                && matches!(e.op.as_str(), "created" | "trashed" | "removed")
                && matches!(
                    e.reversal.as_str(),
                    "delete" | "restore_trash" | "irreversible"
                )
        })
}

/// ¿Es esta unidad un lote de PERMISOS (#315)?
///
/// Un `fs.set_mode` recursivo agrupa sus n nodos bajo un lote para que una
/// auditoría pueda leer UNA acción donde el humano hizo una. Pero no es un
/// lote de renombrados —no hay permutación que deshacer entera ni temporales
/// que atravesar— ni uno de sincronización: cada entrada lleva su propio modo
/// anterior y se deshace sola.
fn is_mode_unit(unit: &[JournalEntry]) -> bool {
    !unit.is_empty()
        && unit
            .iter()
            .all(|e| e.batch_id.is_some() && e.op == "mode_changed")
}

/// Deshace un lote de permisos: entrada a entrada, en LIFO, y lo que no se
/// pueda se DICE (#315).
///
/// Al revés que un lote de renombrados, aquí «entero o nada» sería peor: los
/// modos son independientes —ninguno depende de que otro haya vuelto ya— y un
/// árbol de cien mil ficheros donde uno solo no se puede tocar volvería entero
/// menos ése, que es exactamente lo que el lector quiere. El `Blocked` de un
/// nodo detiene la SESIÓN igual que en cualquier otra unidad; lo que no hace es
/// tirar los que ya volvieron.
async fn revert_mode_batch(
    provider: &dyn Provider,
    journal: &Arc<SqliteJournal>,
    unit: &[JournalEntry],
    actor: &Actor,
    cancel: &CancellationToken,
    report: &Mutex<UndoReport>,
) -> Result<Reverted, Error> {
    // TODAS las entradas del lote tienen que vivir en el mismo provider, y se
    // comprueba antes de tocar nada. Sin esto, un `batch_id` con rutas de dos
    // hosts —que `fs.set_mode` acepta, porque resuelve un provider POR RUTA—
    // hace que el llamante resuelva UN provider a partir de la primera entrada
    // y ejecute las demás contra él: la política evaluó una máquina y el efecto
    // cae en otra. Es la misma guarda que `revert_sync_batch` tiene y por el
    // mismo motivo.
    let Some(primera) = unit.first() else {
        return Ok(Reverted::Accounted);
    };
    // Y NO `one_provider`, aunque la pregunta sea la misma: aquella mira
    // también `reversal_ref` como si fuera una ruta, y en un `mode_changed` ese
    // campo es el MODO anterior en ASCII decimal (`journal.rs`). Pasarlo por
    // `wire` bloquearía cada lote de permisos con un `InvalidPath` que no
    // significa nada.
    let origen = match wire(&primera.path) {
        Ok(p) => p,
        Err(e) => return Ok(Reverted::blocked(primera.seq, e)),
    };
    for e in unit {
        match wire(&e.path) {
            Ok(p) if p.scheme() == origen.scheme() && p.authority() == origen.authority() => {}
            Ok(_) => {
                // Un lote con rutas de dos máquinas: el llamante resuelve UN
                // provider a partir de la primera entrada, así que ejecutar
                // aplicaría las demás contra otro host. La política evaluó una
                // máquina y el efecto caería en otra.
                return Ok(Reverted::blocked(e.seq, Error::InvalidPath));
            }
            Err(err) => return Ok(Reverted::blocked(e.seq, err)),
        }
    }
    // El lote de la COMPENSACIÓN es propio: las entradas que este undo escribe
    // son otra acción, y mezclarlas con el lote original haría que un segundo
    // undo creyera que forman parte de él.
    let batch = journal.journal().alloc_batch().await.ok();
    // De la HOJA a la raíz: el recorrido de ida fue de arriba abajo, así que
    // devolver primero el directorio podría dejarlo sin bit de ejecución con
    // sus hijos todavía por revertir — y entonces no se llega a ellos. El
    // orden no se hereda del `ORDER BY` de la consulta: se pone aquí, como
    // hace `revert_sync_batch`.
    let mut orden: Vec<&JournalEntry> = unit.iter().collect();
    orden.sort_by_key(|e| std::cmp::Reverse(e.seq));
    let mut hechas: u64 = 0;
    let mut bloqueada: Option<Reverted> = None;
    for entry in orden {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        match revert_entry(provider, journal, entry, actor, batch, cancel).await? {
            Reverted::Done => hechas += 1,
            // **NO es entero-o-nada**, al revés que un lote de renombrados: los
            // modos son independientes —ninguno depende de que otro haya vuelto
            // ya—, así que un árbol de cien mil ficheros donde uno no se puede
            // tocar vuelve entero menos ése, que es lo que el lector quiere.
            // Media permutación deshecha sí sería un estado inválido; media
            // reversión de permisos no lo es.
            otro => {
                note_unreverted(report, &entry.path);
                if bloqueada.is_none() {
                    bloqueada = Some(otro);
                }
            }
        }
    }
    // El recuento lo lleva ESTA función y no el llamante, que sumaría los
    // miembros de la unidad entera: aquí cada nodo puede volver o no.
    report.lock().expect("report lock sano").undone += hechas;
    Ok(bloqueada.unwrap_or(Reverted::Accounted))
}

/// Revierte UNA unidad de undo, sea de la clase que sea.
///
/// El único sitio donde se decide qué undo le toca a una unidad, y vive aquí
/// —junto a las tres funciones a las que reparte— y no en
/// [`crate::Engine::undo_session_for`], que solo tiene que saber qué contar.
/// Lo decide la FORMA de las entradas ([`is_sync_unit`]), no el tamaño de la
/// unidad: un lote de sincronización de un paso también necesita el camino que
/// sabe saltarse lo irreversible.
///
/// # Errors
/// Las de [`revert_entry`], [`revert_batch`] y [`revert_sync_batch`]: la
/// cancelación (regla 3) y el fallo al persistir una compensación (regla 4).
pub(crate) async fn revert_unit(
    provider: &dyn Provider,
    journal: &Arc<SqliteJournal>,
    unit: &[JournalEntry],
    actor: &Actor,
    cancel: &CancellationToken,
    task_id: TaskId,
    report: &Mutex<UndoReport>,
) -> Result<Reverted, Error> {
    if is_sync_unit(unit) {
        return revert_sync_batch(provider, journal, unit, actor, cancel, task_id, report).await;
    }
    if is_mode_unit(unit) {
        return revert_mode_batch(provider, journal, unit, actor, cancel, report).await;
    }
    match unit.split_first() {
        // Unidad de una: el camino de siempre, intacto.
        Some((entry, [])) => revert_entry(provider, journal, entry, actor, None, cancel).await,
        // Unidad de varias: un lote de renombrados, entero o nada.
        _ => revert_batch(provider, journal, unit, actor, cancel, task_id, report).await,
    }
}

/// Todas las entradas de la unidad viven en el MISMO provider (scheme y
/// authority), o la unidad no se toca.
///
/// `Err` es siempre un [`Reverted::Blocked`] con el `seq` culpable, nunca un
/// error que mate la sesión: mismo criterio que [`inverse_chain`].
fn one_provider(unit: &[JournalEntry], first: &JournalEntry) -> Result<(), Reverted> {
    let origin = wire(&first.path).map_err(|e| Reverted::blocked(first.seq, e))?;
    for e in unit {
        let refs = [Some(e.path.as_slice()), e.reversal_ref.as_deref()];
        for bytes in refs.into_iter().flatten() {
            let path = wire(bytes).map_err(|err| Reverted::blocked(e.seq, err))?;
            if path.scheme() != origin.scheme() || path.authority() != origin.authority() {
                tracing::error!(
                    seq = e.seq,
                    "entrada de un lote que apunta a otro provider que el resto del lote",
                );
                return Err(Reverted::blocked(e.seq, Error::InvalidPath));
            }
        }
    }
    Ok(())
}

/// Las rutas cuya restauración este undo NO puede acertar, y que por eso no se
/// tocan ni por un lado ni por el otro.
///
/// Una sobrescritura con papelera deja `trashed(P)` + `created(P)`. Con una
/// papelera LÓGICA el `trashed` guarda en `reversal_ref` el payload exacto y
/// restaurarlo es un rename sin ambigüedad. Con la papelera NATIVA del sistema
/// no hay handle: `restore_trashed(P)` elige entre los ítems cuya ruta
/// ORIGINAL es `P` el más reciente (`norte-vfs-local`, `restore_trashed`) — y
/// para cuando el undo llega ahí, el más reciente es el que él mismo acaba de
/// enterrar al deshacer el `created`. Restauraría el fichero NUEVO y dejaría
/// el original del usuario dentro de la papelera, contándolo como éxito.
///
/// Deshacer eso de verdad pide que `trash()` devuelva el identificador del
/// ítem, que es deuda de `norte-vfs` (anotada en `norte-vfs-local`). Hasta
/// entonces la pareja se deja INTACTA —el fichero sincronizado se queda donde
/// está y el original sigue en la papelera, de donde el usuario lo saca a
/// mano— y el informe da la ruta. Enterrar lo nuevo y no restaurar lo viejo
/// dejaría la ruta VACÍA, que es peor que no tocar nada.
fn ambiguous_restores(unit: &[JournalEntry]) -> HashSet<&[u8]> {
    let buried: HashSet<&[u8]> = unit
        .iter()
        .filter(|e| {
            e.reversal.as_str() == Reversal::RestoreTrash.as_str() && e.reversal_ref.is_none()
        })
        .map(|e| e.path.as_slice())
        .collect();
    unit.iter()
        .filter(|e| {
            e.reversal.as_str() == Reversal::Delete.as_str() && buried.contains(e.path.as_slice())
        })
        .map(|e| e.path.as_slice())
        .collect()
}

/// Anota una ruta que NO volvió, con tope.
///
/// El contador que le corresponda ya lo llevó quien llama: esto es la muestra
/// con nombres, no la cuenta.
///
/// # Panics
/// INVARIANTE: el `Mutex` del reporte solo se envenena si otro hilo entró en
/// pánico sosteniéndolo, que es irrecuperable — mismo criterio que el resto de
/// los locks del core.
fn note_unreverted(report: &Mutex<UndoReport>, path: &[u8]) {
    let mut report = report.lock().expect("undo report lock");
    if report.unreverted_paths.len() < UNDO_MAX_UNREVERTED_PATHS {
        report.unreverted_paths.push(path.to_vec());
    }
}

/// Revierte un lote de SINCRONIZACIÓN (`sync.apply`): lo que se pueda, en
/// orden de `seq` DESCENDENTE, nombrando lo que no.
///
/// **No es [`revert_batch`], y la diferencia es el contrato entero.** Media
/// permutación deshecha no es ningún estado, así que un lote de renombrado
/// vuelve entero o no se toca. Media sincronización deshecha SÍ es un estado:
/// es el árbol de antes con parte de los ficheros ya devueltos. Así que aquí
/// una entrada irreversible no rehúsa el lote —lo diría todo-o-nada, y
/// dejaría 9 999 pasos reversibles secuestrados por uno que no lo es—, sino
/// que se salta, se cuenta y se nombra en
/// [`UndoReport::unreverted_paths`].
///
/// # Por qué el orden es `seq` descendente
/// Es el que devuelve
/// [`revertible_for`](crate::journal::Journal::revertible_for), y aquí se
/// vuelve a imponer para que la propiedad viva en la función que depende de
/// ella. Lo que ese orden resuelve, con las formas que
/// [`crate::sync::exec`] emite:
///
/// - **La pareja de un `Overwrite` con papelera LÓGICA** (`trashed` y luego
///   `created`): borra lo creado ANTES de restaurar lo enterrado, que es la
///   única secuencia en la que el `restore_trash` encuentra su ruta libre.
///   Sobre una papelera NATIVA la ruta libre no basta y la pareja no se toca:
///   ver [`ambiguous_restores`].
/// - **Un `CreateDir` y las copias de dentro**: el walk es pre-orden, así que
///   el directorio se journaliza ANTES que sus hijos y al revés se vacía antes
///   de mandarlo a la papelera. Que quede vacío no se SUPONE: la reversa de un
///   `created` que es un directorio comprueba que no tenga hijos y se bloquea
///   si los tiene, porque la papelera se llevaría también lo que hubiera
///   dentro.
///
/// El resto de las formas (`Copy`, `DeleteTree`, las irreversibles) tocan una
/// ruta cada una y no se ordenan entre sí.
///
/// # Lo que este undo NO puede
/// - Una entrada `irreversible` no tiene reversa que ejecutar: es un
///   `Overwrite` o un `DeleteTree` sobre un destino sin papelera, y lo que
///   había ya no está en ningún sitio. Se cuenta en
///   [`UndoReport::skipped_irreversible`].
/// - Un `created` en un provider SIN papelera se salta también (#65): su
///   reversa es un borrado, el journal no guarda identidad del nodo, y borrar
///   permanente «lo que hoy viva en esa ruta» puede destruir trabajo posterior
///   del humano. O sea: **en un destino sin papelera, una `Copy` tampoco se
///   deshace**, aunque el plan la enseñe con reversa `Delete`. Se cuenta en
///   [`UndoReport::skipped_created_no_trash`].
/// - Una sobrescritura cuya papelera no da destino recuperable: la pareja se
///   queda intacta, con su ruta nombrada ([`ambiguous_restores`]).
/// - Un drift (la ruta cambió bajo el undo) bloquea ESA entrada, se anota la
///   primera en [`UndoReport::blocked`] y la sesión para ahí — pero las demás
///   entradas del lote se intentan igual, que es la regla de arriba. Seguir es
///   seguro porque cada reversa comprueba lo suyo antes de actuar, y donde esa
///   comprobación no bastaba se ha añadido: un directorio creado no se
///   entierra si tiene hijos, y una sobrescritura ambigua no se toca. Lo que
///   NO promete es independencia entre las dos mitades de una sobrescritura:
///   si la primera entierra lo creado y la segunda no logra restaurar lo
///   enterrado, la ruta se queda VACÍA —todo recuperable de la papelera, todo
///   nombrado en el informe, pero vacía—.
///
/// # Errors
/// SOLO [`Error::Cancelled`] (regla 3, el token se mira entre entradas) y el
/// error del JOURNAL cuando una compensación no se pudo persistir después de
/// que su efecto ya ocurriera (regla 4). Lo segundo suma
/// [`UndoReport::compensations_lost`] y para la Task: con el journal fallando,
/// seguir tocando el árbol es escribir mutaciones que nadie registra.
///
/// # Panics
/// Solo por envenenamiento del `Mutex` del reporte, mismo criterio que el
/// resto del core.
#[tracing::instrument(
    skip_all,
    fields(task_id = %task_id, batch = unit.len(), seq = unit.first().map_or(0, |e| e.seq))
)]
pub(crate) async fn revert_sync_batch(
    provider: &dyn Provider,
    journal: &Arc<SqliteJournal>,
    unit: &[JournalEntry],
    actor: &Actor,
    cancel: &CancellationToken,
    task_id: TaskId,
    report: &Mutex<UndoReport>,
) -> Result<Reverted, Error> {
    let Some(first) = unit.first() else {
        // Imposible: `undo_units` jamás produce una unidad vacía.
        return Ok(Reverted::Accounted);
    };
    // La unidad viene de `revertible_for` (filtra por actor) y de `undo_units`
    // (agrupa por `batch_id`). Se afirma donde se consume, como en
    // `revert_batch`.
    debug_assert!(
        unit.iter().all(|e| e.batch_id == first.batch_id
            && e.actor_kind == first.actor_kind
            && e.actor_id == first.actor_id),
        "una unidad de undo es UN lote de UN actor",
    );
    // Todas las entradas tienen que vivir en UN provider, y se comprueba de
    // verdad. El llamante resuelve UNO (por la primera ruta) y lo usa para
    // todas; `revert_batch` lo tenía gratis —`inverse_chain` exige que todas
    // cuelguen del mismo directorio, y el padre de un `VPath` lleva scheme y
    // authority—, pero aquí no hay directorio común. Sin esto, un `batch_id`
    // manipulado ejecuta la ruta de un host contra el provider de otro: la
    // policy evaluó una máquina y el efecto cae en otra. De paso, esta pasada
    // es la que hace que un `Err` de `revert_entry` signifique SOLO «la
    // compensación no se pudo persistir»: los `wire()` que allí devuelven
    // `Err(InvalidPath)` ya no pueden fallar.
    if let Err(refused) = one_provider(unit, first) {
        return Ok(refused);
    }
    // El orden que la corrección necesita se impone AQUÍ (ver el rustdoc), no
    // se hereda del `ORDER BY` de quien listó las entradas.
    let mut ordered: Vec<&JournalEntry> = unit.iter().collect();
    ordered.sort_by_key(|e| std::cmp::Reverse(e.seq));
    let ambiguous = ambiguous_restores(unit);

    // Deshacer un lote se LEE como un lote: un `batch_id` FRESCO para todas las
    // compensaciones, cada una con su `undoes_seq`. Un id pedido y no usado
    // (lote entero irreversible) se pierde sin más — son etiquetas de
    // agrupación, no un contador auditable.
    let batch_id = match journal.journal().alloc_batch().await {
        Ok(id) => id,
        Err(e) => return Ok(Reverted::blocked(first.seq, Error::from(e))),
    };

    let mut blocked: Option<(i64, Error)> = None;
    for entry in ordered {
        // Regla 3: entre entradas. Lo compensado se queda compensado y el
        // resto del lote sigue siendo deshacible en el siguiente undo.
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        // La pareja que una papelera NATIVA no sabe deshacer: ni se toca.
        if ambiguous.contains(entry.path.as_slice()) {
            if entry.reversal.as_str() == Reversal::RestoreTrash.as_str() {
                tracing::error!(
                    seq = entry.seq,
                    "sobrescritura sobre papelera sin destino recuperable: el undo no puede \
                     distinguir el fichero enterrado del que él mismo enterraría, así que no \
                     toca ninguno de los dos — el original sigue en la papelera",
                );
                // Se nombra UNA vez por pareja: la entrada `trashed` es la del
                // fichero que el usuario quiere de vuelta.
                note_unreverted(report, &entry.path);
            }
            if blocked.is_none() {
                blocked = Some((entry.seq, Error::Unsupported));
            }
            continue;
        }
        match revert_entry(provider, journal, entry, actor, Some(batch_id), cancel).await {
            Ok(Reverted::Done) => {
                report.lock().expect("undo report lock").undone += 1;
            }
            Ok(Reverted::SkippedIrreversible) => {
                report
                    .lock()
                    .expect("undo report lock")
                    .skipped_irreversible += 1;
                note_unreverted(report, &entry.path);
            }
            Ok(Reverted::SkippedNoTrash) => {
                report
                    .lock()
                    .expect("undo report lock")
                    .skipped_created_no_trash += 1;
                note_unreverted(report, &entry.path);
            }
            // Drift en UNA entrada. Se nombra la primera (la de `seq` mayor, o
            // sea la mutación más reciente) y se sigue: las demás no dependen
            // de ella, y cada reversa comprueba lo suyo antes de actuar.
            Ok(Reverted::Blocked { seq, error }) => {
                tracing::info!(seq, error = %error, "una entrada del lote de sync no volvió");
                note_unreverted(report, &entry.path);
                if blocked.is_none() {
                    blocked = Some((seq, error));
                }
            }
            // `revert_entry` no lo produce (es del ejecutor de renombrados).
            // Si algún día lo hiciera, un árbol a medio devolver NO se trata
            // como un salto: se propaga tal cual y la Task falla.
            Ok(stuck @ Reverted::Stuck { .. }) => return Ok(stuck),
            Ok(Reverted::Accounted) => {
                debug_assert!(false, "`revert_entry` no contabiliza por su cuenta");
            }
            // Regla 4: el efecto ocurrió y su compensación NO quedó durable.
            // La entrada sigue pareciendo pendiente y un undo posterior se
            // bloqueará ahí; es la única señal.
            Err(e) => {
                tracing::error!(
                    seq = entry.seq,
                    error = %e,
                    "reversa aplicada sin compensar en un lote de sync: el journal no la tiene",
                );
                report.lock().expect("undo report lock").compensations_lost += 1;
                return Err(e);
            }
        }
    }

    match blocked {
        // Estricto en la SESIÓN: el lote hizo lo que pudo y el LIFO para aquí,
        // porque lo anterior a un drift ya no se puede prometer.
        Some((seq, error)) => Ok(Reverted::blocked(seq, error)),
        None => Ok(Reverted::Accounted),
    }
}

/// Un nombre base como `Segment`, o [`Error::InvalidPath`].
///
/// Los nombres salen de un `VPath` del journal, así que ya eran segmentos
/// legales; esto vuelve a comprobarlo en el único punto que los reconstruye
/// (regla 6: la garantía se comprueba, no se asume).
fn seg(b: &[u8]) -> Result<norte_proto::Segment, Error> {
    norte_proto::Segment::new(b.to_vec()).map_err(|_| Error::InvalidPath)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(seq: i64, batch: Option<i64>) -> JournalEntry {
        JournalEntry {
            seq,
            ts_ms: 0,
            entry_hash: Vec::new(),
            actor_kind: "user".to_owned(),
            actor_id: None,
            op: "renamed".to_owned(),
            path: Vec::new(),
            path_to: None,
            reversal: "rename_back".to_owned(),
            reversal_ref: None,
            undoes_seq: None,
            batch_id: batch,
        }
    }

    fn shape(units: &[Vec<JournalEntry>]) -> Vec<Vec<i64>> {
        units
            .iter()
            .map(|u| u.iter().map(|e| e.seq).collect())
            .collect()
    }

    /// Mutaciones sueltas: una unidad cada una, en el mismo orden LIFO.
    #[test]
    fn lone_entries_stay_one_unit_each() {
        let units = undo_units(vec![entry(3, None), entry(2, None), entry(1, None)]);
        assert_eq!(shape(&units), vec![vec![3], vec![2], vec![1]]);
    }

    /// Un lote contiguo llega entero, y en orden LIFO por dentro.
    #[test]
    fn a_contiguous_batch_is_one_unit() {
        let units = undo_units(vec![
            entry(4, None),
            entry(3, Some(7)),
            entry(2, Some(7)),
            entry(1, Some(7)),
        ]);
        assert_eq!(shape(&units), vec![vec![4], vec![3, 2, 1]]);
    }

    /// LA propiedad: una mutación de otra task colada EN MEDIO del lote no lo
    /// parte. Con agrupado por contigüidad esto daría `[[3], [2], [1]]` y la
    /// primera unidad dejaría la permutación a medias.
    #[test]
    fn an_interleaved_entry_does_not_split_the_batch() {
        let units = undo_units(vec![entry(3, Some(7)), entry(2, None), entry(1, Some(7))]);
        assert_eq!(shape(&units), vec![vec![3, 1], vec![2]]);
    }

    /// Dos lotes distintos entrelazados: cada uno entero, cada uno el suyo.
    #[test]
    fn two_interleaved_batches_stay_apart() {
        let units = undo_units(vec![
            entry(4, Some(8)),
            entry(3, Some(9)),
            entry(2, Some(8)),
            entry(1, Some(9)),
        ]);
        assert_eq!(shape(&units), vec![vec![4, 2], vec![3, 1]]);
    }

    /// Una entrada con la forma que escribe `sync.apply`: sin `path_to`, con
    /// lote, y con una de sus tres reversas.
    fn sync_entry(seq: i64, batch: Option<i64>, op: &str, reversal: &str) -> JournalEntry {
        JournalEntry {
            op: op.to_owned(),
            path: format!("mem:///d/{seq}").into_bytes(),
            reversal: reversal.to_owned(),
            ..entry(seq, batch)
        }
    }

    /// Un lote de renombrados NO va por el camino de sincronización: su undo es
    /// todo-o-nada y esa propiedad no se puede perder por un dispatch.
    #[test]
    fn a_rename_batch_is_not_a_sync_unit() {
        let unit = vec![entry(2, Some(7)), entry(1, Some(7))];
        assert!(!is_sync_unit(&unit));
    }

    /// Un lote de sincronización de UNA entrada sigue siendo un lote: el
    /// dispatch mira la forma, no el tamaño.
    #[test]
    fn a_sync_batch_of_one_is_still_a_sync_unit() {
        let unit = vec![sync_entry(1, Some(7), "created", "delete")];
        assert!(is_sync_unit(&unit));
    }

    /// Las tres formas que el ejecutor de sync emite, juntas.
    #[test]
    fn the_three_shapes_of_a_sync_batch_are_a_sync_unit() {
        let unit = vec![
            sync_entry(3, Some(7), "removed", "irreversible"),
            sync_entry(2, Some(7), "created", "delete"),
            sync_entry(1, Some(7), "trashed", "restore_trash"),
        ];
        assert!(is_sync_unit(&unit));
    }

    /// Una unidad MIXTA no es de sincronización: quien colara un `created` en
    /// un lote de renombrados elegiría, si no, el undo permisivo para una
    /// permutación — y media permutación deshecha es justo lo que no puede
    /// pasar. Cae en `revert_batch`, que la bloquea.
    #[test]
    fn a_mixed_unit_is_not_a_sync_unit() {
        let unit = vec![
            sync_entry(2, Some(7), "created", "delete"),
            entry(1, Some(7)),
        ];
        assert!(!is_sync_unit(&unit));
    }

    /// Una mutación suelta (un `fs.copy`) no lleva lote y sigue por el camino
    /// de siempre, con su compensación sin `batch_id`.
    #[test]
    fn a_lone_entry_without_a_batch_is_not_a_sync_unit() {
        let unit = vec![sync_entry(1, None, "created", "delete")];
        assert!(!is_sync_unit(&unit));
    }

    /// Reescribir SOLO la columna `reversal` de un lote de renombrados no basta
    /// para comprarle el undo permisivo: el `op` sigue diciendo `renamed`, y
    /// con el criterio negativo («no hay ningún `rename_back`») esa unidad
    /// habría ido a mandar a la papelera el DESTINO de cada renombrado.
    #[test]
    fn a_rename_batch_with_a_rewritten_reversal_is_still_not_a_sync_unit() {
        let unit = vec![
            sync_entry(2, Some(7), "renamed", "delete"),
            sync_entry(1, Some(7), "renamed", "delete"),
        ];
        assert!(!is_sync_unit(&unit));
    }

    /// La pareja de una sobrescritura sobre papelera NATIVA (sin
    /// `reversal_ref`) no se toca por ninguno de sus dos lados.
    #[test]
    fn an_overwrite_pair_without_a_trash_reference_is_ambiguous() {
        let unit = vec![
            JournalEntry {
                path: b"mem:///d/a.txt".to_vec(),
                ..sync_entry(2, Some(7), "created", "delete")
            },
            JournalEntry {
                path: b"mem:///d/a.txt".to_vec(),
                ..sync_entry(1, Some(7), "trashed", "restore_trash")
            },
        ];
        let ambiguous = ambiguous_restores(&unit);
        assert_eq!(ambiguous.len(), 1);
        assert!(ambiguous.contains(b"mem:///d/a.txt".as_slice()));
    }

    /// Con papelera LÓGICA el `trashed` sabe de dónde sacar el fichero, así que
    /// no hay ambigüedad y la pareja se deshace entera.
    #[test]
    fn an_overwrite_pair_with_a_trash_reference_is_not_ambiguous() {
        let unit = vec![
            JournalEntry {
                path: b"mem:///d/a.txt".to_vec(),
                ..sync_entry(2, Some(7), "created", "delete")
            },
            JournalEntry {
                path: b"mem:///d/a.txt".to_vec(),
                reversal_ref: Some(b"mem:///d/.norte-trash/1/a.txt".to_vec()),
                ..sync_entry(1, Some(7), "trashed", "restore_trash")
            },
        ];
        assert!(ambiguous_restores(&unit).is_empty());
    }

    /// Un `trashed` sin `reversal_ref` que NADIE vuelve a crear (un
    /// `DeleteTree`) se restaura sin ambigüedad: el undo no entierra nada en
    /// esa ruta, así que el ítem más reciente de la papelera sigue siendo el
    /// suyo.
    #[test]
    fn a_lone_native_trash_entry_is_not_ambiguous() {
        let unit = vec![sync_entry(1, Some(7), "trashed", "restore_trash")];
        assert!(ambiguous_restores(&unit).is_empty());
    }

    /// Una entrada que apunta a OTRO provider bloquea la unidad antes de tocar
    /// nada: el llamante resuelve un solo provider para todas.
    #[test]
    fn a_unit_that_spans_two_providers_is_refused() {
        let first = JournalEntry {
            path: b"sftp://a/x".to_vec(),
            ..sync_entry(2, Some(7), "created", "delete")
        };
        let other = JournalEntry {
            path: b"sftp://b/x".to_vec(),
            ..sync_entry(1, Some(7), "created", "delete")
        };
        let unit = vec![first.clone(), other];
        let refused = one_provider(&unit, &first).expect_err("bloqueada");
        assert!(matches!(
            refused,
            Reverted::Blocked {
                seq: 1,
                error: Error::InvalidPath
            }
        ));
    }

    /// Y el `reversal_ref` cuenta igual: es la ruta que el `restore_trash`
    /// EJECUTA sobre el provider de la unidad.
    #[test]
    fn a_trash_reference_on_another_provider_is_refused() {
        let first = JournalEntry {
            path: b"mem:///d/a".to_vec(),
            reversal_ref: Some(b"sftp://b/trash/a".to_vec()),
            ..sync_entry(1, Some(7), "trashed", "restore_trash")
        };
        let unit = vec![first.clone()];
        assert!(one_provider(&unit, &first).is_err());
    }

    /// Ni una unidad vacía, que además `undo_units` no produce.
    #[test]
    fn an_empty_unit_is_not_a_sync_unit() {
        assert!(!is_sync_unit(&[]));
    }

    /// La lista de rutas tiene tope; el CONTADOR no. Un lote de medio millón de
    /// pasos irreversibles no puede llevarse la memoria del daemon por delante.
    #[test]
    fn the_unreverted_path_list_is_capped() {
        let report = Mutex::new(UndoReport::default());
        for i in 0..(UNDO_MAX_UNREVERTED_PATHS * 3) {
            note_unreverted(&report, format!("mem:///d/{i}").as_bytes());
        }
        let r = report.lock().expect("lock");
        assert_eq!(r.unreverted_paths.len(), UNDO_MAX_UNREVERTED_PATHS);
        assert_eq!(r.unreverted_paths[0], b"mem:///d/0", "recorta por la COLA");
    }

    const SENSITIVE: NameCaps = NameCaps {
        fold: norte_encoding::FoldMode::None,
    };

    fn inv(from: &[u8], to: &[u8], seq: i64) -> Inverse {
        Inverse {
            from: from.to_vec(),
            to: to.to_vec(),
            seq,
        }
    }

    /// La cadena inversa de una permutación (con su temporal) cabe entera.
    #[test]
    fn the_inverse_chain_of_a_permutation_is_feasible() {
        let steps = vec![
            inv(b"b", b".norte-rename-0", 3),
            inv(b"a", b"b", 2),
            inv(b".norte-rename-0", b"a", 1),
        ];
        assert!(feasible(&steps, &[b"a".to_vec(), b"b".to_vec()], SENSITIVE).is_ok());
    }

    /// Un ocupante en el destino de UN paso invalida la cadena ENTERA, y dice
    /// cuál: es lo que convierte el undo en todo-o-nada.
    #[test]
    fn an_occupied_destination_kills_the_whole_chain() {
        let steps = vec![inv(b"y", b"b", 2), inv(b"x", b"a", 1)];
        let listing = [b"x".to_vec(), b"y".to_vec(), b"a".to_vec()];
        let (i, e) = feasible(&steps, &listing, SENSITIVE).expect_err("bloqueado");
        assert_eq!(i, 1, "el segundo paso es el que no cabe");
        assert_eq!(e, OCCUPIED);
    }

    /// Un origen que ya no está (alguien borró o movió el fichero) también
    /// bloquea el lote entero.
    #[test]
    fn a_vanished_source_kills_the_whole_chain() {
        let steps = vec![inv(b"y", b"b", 2)];
        let (i, e) = feasible(&steps, &[b"x".to_vec()], SENSITIVE).expect_err("bloqueado");
        assert_eq!(i, 0);
        assert_eq!(e, Error::NotFound);
    }

    /// Deshacer un rename de solo-caso en un directorio que no distingue: el
    /// destino «ocupado» es el propio origen, y eso no es un conflicto.
    #[test]
    fn undoing_a_case_only_rename_is_not_a_conflict() {
        let insensitive = NameCaps {
            fold: norte_encoding::FoldMode::Simple,
        };
        let steps = vec![inv(b"foo", b"Foo", 1)];
        assert!(feasible(&steps, &[b"foo".to_vec()], insensitive).is_ok());
    }
}
