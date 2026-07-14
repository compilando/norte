//! Costura del journal (M3): TODA mutación que ejecuta el core pasa por un
//! [`MutationObserver`] ANTES de considerarse completa (regla dura 4). En M0
//! el observador es no-op; el journal se enchufará aquí sin tocar el engine.

use async_trait::async_trait;
use norte_proto::{Error, VPath};

use crate::journal::Actor;

/// Una mutación observable del VFS.
#[derive(Debug)]
pub enum Mutation<'a> {
    /// Nodo creado (archivo commiteado o dir).
    Created(&'a VPath),
    /// Nodo eliminado PERMANENTEMENTE (irreversible).
    Removed(&'a VPath),
    /// Nodo movido a la papelera (RECUPERABLE — el undo de M3 usa el
    /// restore del OS; régimen distinto a `Removed`, ADR 0009).
    Trashed(&'a VPath),
    /// Nodo renombrado dentro de un provider.
    Renamed {
        /// Path original.
        from: &'a VPath,
        /// Path nuevo.
        to: &'a VPath,
    },
}

/// Receptor de mutaciones. M3 lo implementa el journal (con undo); hasta
/// entonces, un observador no-op interno.
#[async_trait]
pub trait MutationObserver: Send + Sync {
    /// Notifica una mutación ya aplicada con éxito. Async y falible: el journal
    /// await-ea el insert antes de que la op se considere completa, y su fallo
    /// PROPAGA (regla 4 — la op falla si su entrada no quedó durable).
    ///
    /// # Errors
    /// El error del sink (p. ej. fallo de escritura del journal).
    async fn on_mutation(&self, mutation: &Mutation<'_>, actor: &Actor) -> Result<(), Error>;
}

/// Observador que no hace nada (M0 / tests sin journal).
pub(crate) struct NoopObserver;

#[async_trait]
impl MutationObserver for NoopObserver {
    async fn on_mutation(&self, _mutation: &Mutation<'_>, _actor: &Actor) -> Result<(), Error> {
        Ok(())
    }
}
