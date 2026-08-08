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

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use norte_proto::{CapabilityFlags, ConflictKind, Error, TaskId, TaskKind, VPath};
use norte_vfs::Provider;
use tokio_util::sync::CancellationToken;

use crate::journal::{Actor, JournalEntry, Reversal, SqliteJournal};
use crate::progress::ProgressReporter;
use crate::rename::exec::{BatchJournal, BatchReport, PlannedStep};
use crate::rename::plan::{NameCaps, name_key};

/// Resultado de un [`crate::Engine::undo_session`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UndoReport {
    /// Entradas revertidas con éxito (compensación appendeada). Cuenta
    /// ENTRADAS, no unidades: un lote (`fs.rename_batch`) aporta todas las
    /// suyas de golpe, porque se revierte entero o nada. El progreso de la
    /// Task, en cambio, avanza por unidades — para el humano un lote es UN
    /// paso del undo.
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
    pub blocked: Option<(i64, Error)>,
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
}

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
/// **Un lote es UNA unidad, se revierte entero o no se toca.** Media
/// permutación deshecha es el estado que toda esta funcionalidad existe para
/// impedir, así que el lote tiene que llegar a la task como un solo ítem.
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
/// # Errors
/// Solo por fallo al PERSISTIR la compensación en el journal (regla 4). Un
/// conflicto/drift del FS NO es error: se devuelve `Reverted::Blocked`.
// Dispatch lineal por `Reversal` (4 ramas): más claro junto que fragmentado.
#[allow(clippy::too_many_lines)]
pub(crate) async fn revert_entry(
    provider: &dyn Provider,
    journal: &SqliteJournal,
    entry: &JournalEntry,
    actor: &Actor,
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
            match provider.stat(&path).await {
                Ok(_) => {}
                // Ya no está: estado inesperado → bloquea (no finge éxito).
                Err(e) => return Ok(Reverted::blocked(entry.seq, e)),
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
            let (comp_op, comp_reversal, comp_ref) = match crate::ops::trash_retrying(
                provider,
                &path,
                &comp_trash_id,
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
            {
                Ok(dest) => ("trashed", Reversal::RestoreTrash, dest),
                Err(e) => return Ok(Reverted::blocked(entry.seq, e)),
            };
            let comp_ref_bytes = comp_ref.map(|d| d.to_wire().into_bytes());
            journal
                .journal()
                .record_undoing(
                    comp_op,
                    &entry.path,
                    None,
                    comp_reversal,
                    comp_ref_bytes.as_deref(),
                    actor,
                    Some(entry.seq),
                )
                .await
                .map_err(Error::from)?;
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
            match is_free(provider, &from).await {
                Ok(true) => {}
                Ok(false) => return Ok(Reverted::blocked(entry.seq, OCCUPIED)),
                Err(e) => return Ok(Reverted::blocked(entry.seq, e)),
            }
            if let Err(e) = provider.rename(&path, &from).await {
                return Ok(Reverted::blocked(entry.seq, e));
            }
            // Compensación: renamed inverso (destino=origen, origen=destino).
            journal
                .journal()
                .record_undoing(
                    "renamed",
                    from_bytes,
                    Some(&entry.path),
                    Reversal::RenameBack,
                    None,
                    actor,
                    Some(entry.seq),
                )
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
                // Papelera lógica: mover el payload de vuelta al original.
                Some(dest_bytes) => {
                    let dest = wire(dest_bytes)?;
                    provider.rename(&dest, &path).await
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
                .record_undoing(
                    "created",
                    &entry.path,
                    None,
                    Reversal::Delete,
                    None,
                    actor,
                    Some(entry.seq),
                )
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
fn feasible(steps: &[Inverse], listing: &[Vec<u8>], caps: NameCaps) -> Result<(), (usize, Error)> {
    let mut occupied: HashSet<Vec<u8>> = listing
        .iter()
        .map(|n| name_key(n, caps).into_owned())
        .collect();
    for (i, s) in steps.iter().enumerate() {
        let fk = name_key(&s.from, caps).into_owned();
        let tk = name_key(&s.to, caps).into_owned();
        if !occupied.contains(&fk) {
            return Err((i, Error::NotFound));
        }
        if fk != tk {
            if occupied.contains(&tk) {
                return Err((i, OCCUPIED));
            }
            occupied.remove(&fk);
            occupied.insert(tk);
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

    let caps = provider.capabilities();
    if caps.flags.contains(CapabilityFlags::READ_ONLY) {
        return Ok(Reverted::blocked(first.seq, Error::Unsupported));
    }
    let name_caps = NameCaps {
        case_sensitive: caps.flags.contains(CapabilityFlags::CASE_SENSITIVE),
    };
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

    const SENSITIVE: NameCaps = NameCaps {
        case_sensitive: true,
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
            case_sensitive: false,
        };
        let steps = vec![inv(b"foo", b"Foo", 1)];
        assert!(feasible(&steps, &[b"foo".to_vec()], insensitive).is_ok());
    }
}
