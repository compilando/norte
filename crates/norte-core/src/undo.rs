//! Undo de sesión (M3-2): ejecuta la `Reversal` persistida de cada entrada del
//! journal en orden LIFO, sin pisar el trabajo del usuario (estricto), y
//! appendea una entrada compensatoria (append-only, la cadena sigue íntegra).

use norte_proto::{ConflictKind, Error, VPath};
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
pub(crate) async fn revert_entry(
    provider: &dyn Provider,
    journal: &SqliteJournal,
    entry: &JournalEntry,
    actor: &Actor,
) -> Result<Reverted, Error> {
    let path = wire(&entry.path)?;
    match entry.reversal.as_str() {
        "irreversible" => Ok(Reverted::SkippedIrreversible),

        // Undo de un Created: borrar el nodo creado (si sigue existiendo).
        "delete" => {
            match provider.stat(&path).await {
                Ok(_) => {}
                // Ya no está: estado inesperado → bloquea (no finge éxito).
                Err(e) => return Ok(Reverted::Blocked(e)),
            }
            if let Err(e) = provider.remove(&path).await {
                return Ok(Reverted::Blocked(e));
            }
            journal
                .journal()
                .record_undoing(
                    "removed",
                    &entry.path,
                    None,
                    Reversal::Irreversible,
                    None,
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
