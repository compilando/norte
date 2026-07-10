//! Costura del journal (M3): TODA mutación que ejecuta el core pasa por un
//! [`MutationObserver`] ANTES de considerarse completa (regla dura 4). En M0
//! el observador es no-op; el journal se enchufará aquí sin tocar el engine.

use norte_proto::VPath;

/// Una mutación observable del VFS.
#[derive(Debug)]
pub enum Mutation<'a> {
    /// Nodo creado (archivo commiteado o dir).
    Created(&'a VPath),
    /// Nodo eliminado.
    Removed(&'a VPath),
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
pub trait MutationObserver: Send + Sync {
    /// Notifica una mutación ya aplicada con éxito.
    fn on_mutation(&self, mutation: &Mutation<'_>);
}

/// Observador que no hace nada (M0).
pub(crate) struct NoopObserver;

impl MutationObserver for NoopObserver {
    fn on_mutation(&self, _mutation: &Mutation<'_>) {}
}
