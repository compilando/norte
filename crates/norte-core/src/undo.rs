//! Undo de sesión (M3-2): ejecuta la `Reversal` persistida de cada entrada del
//! journal en orden LIFO y appendea una entrada compensatoria (append-only, la
//! cadena sigue íntegra).
//!
//! **No-clobber (estricto).** `RenameBack`/`RestoreTrash` exigen destino LIBRE
//! antes de actuar (nunca sobrescriben). El undo de un `Created` es el caso
//! sutil: el journal no guarda identidad del nodo, así que se deshace vía
//! PAPELERA (recuperable) cuando el provider la soporta, para no destruir de
//! forma irreversible un fichero que el usuario pudiera haber editado tras la
//! creación. Deuda: identidad (`node_id`/hash) en el `Created` para un undo con
//! verificación real.

use norte_proto::{CapabilityFlags, ConflictKind, Error, VPath};
use norte_vfs::Provider;

use crate::journal::{Actor, JournalEntry, Reversal, SqliteJournal};

/// Resultado de un [`crate::Engine::undo_session`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UndoReport {
    /// Entradas revertidas con éxito (compensación appendeada).
    pub undone: u64,
    /// Entradas `Irreversible` encontradas y saltadas (no hay nada que pisar).
    pub skipped_irreversible: u64,
    /// Primer paso bloqueado (drift/conflicto): `seq` original + motivo. La
    /// sesión para ahí (estricto).
    pub blocked: Option<(i64, Error)>,
}

/// Reconstruye un `VPath` desde los bytes `to_wire` guardados en el journal.
fn wire(bytes: &[u8]) -> Result<VPath, Error> {
    let s = std::str::from_utf8(bytes).map_err(|_| Error::InvalidPath)?;
    VPath::parse(s).map_err(|_| Error::InvalidPath)
}

/// `true` si `p` NO existe (libre) en `provider`.
async fn is_free(provider: &dyn Provider, p: &VPath) -> Result<bool, Error> {
    match provider.stat(p).await {
        Err(Error::NotFound) => Ok(true),
        Ok(_) => Ok(false),
        Err(e) => Err(e),
    }
}

const OCCUPIED: Error = Error::Conflict {
    conflict: ConflictKind::Exists,
};

/// Resultado de intentar revertir UNA entrada.
pub(crate) enum Reverted {
    /// Revertida y compensada.
    Done,
    /// `Irreversible`: saltada.
    SkippedIrreversible,
    /// Bloqueada por drift/conflicto (no se aplicó cambio de FS).
    Blocked(Error),
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
        // Por eso preferimos PAPELERA (recuperable) sobre `remove` permanente:
        // si el usuario había modificado el nodo, su trabajo queda recuperable
        // en la papelera en vez de destruido. Sin capability `TRASH` caemos a
        // `remove` permanente (documentado; deuda: identidad en el `Created`).
        "delete" => {
            match provider.stat(&path).await {
                Ok(_) => {}
                // Ya no está: estado inesperado → bloquea (no finge éxito).
                Err(e) => return Ok(Reverted::Blocked(e)),
            }
            let (comp_op, comp_reversal, comp_ref) = if provider
                .capabilities()
                .flags
                .contains(CapabilityFlags::TRASH)
            {
                match provider.trash(&path).await {
                    Ok(dest) => ("trashed", Reversal::RestoreTrash, dest),
                    Err(e) => return Ok(Reverted::Blocked(e)),
                }
            } else {
                if let Err(e) = provider.remove(&path).await {
                    return Ok(Reverted::Blocked(e));
                }
                ("removed", Reversal::Irreversible, None)
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
        "rename_back" => {
            let Some(from_bytes) = entry.path_to.as_deref() else {
                return Ok(Reverted::Blocked(Error::InvalidPath));
            };
            let from = wire(from_bytes)?;
            match is_free(provider, &from).await {
                Ok(true) => {}
                Ok(false) => return Ok(Reverted::Blocked(OCCUPIED)),
                Err(e) => return Ok(Reverted::Blocked(e)),
            }
            if let Err(e) = provider.rename(&path, &from).await {
                return Ok(Reverted::Blocked(e));
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
                Ok(false) => return Ok(Reverted::Blocked(OCCUPIED)),
                Err(e) => return Ok(Reverted::Blocked(e)),
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
                return Ok(Reverted::Blocked(e));
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
        _ => Ok(Reverted::Blocked(Error::Unsupported)),
    }
}
