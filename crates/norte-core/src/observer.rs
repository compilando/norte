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
    /// Nodo movido a la papelera (RECUPERABLE — el undo de M3 lo restaura;
    /// régimen distinto a `Removed`, ADR 0009).
    Trashed {
        /// Path original (víctima).
        path: &'a VPath,
        /// Destino recuperable en una papelera LÓGICA (`.norte-trash/<id>`,
        /// fase 9) → `reversal_ref`. `None` si es papelera NATIVA del OS o
        /// "vanish" (sin ruta estable; el handle se resuelve en el undo M3-2).
        dest: Option<&'a VPath>,
    },
    /// Nodo renombrado dentro de un provider.
    Renamed {
        /// Path original.
        from: &'a VPath,
        /// Path nuevo.
        to: &'a VPath,
        /// Lote al que pertenece el rename (`fs.rename_batch`): la etiqueta que
        /// agrupa n entradas del journal para deshacerlas juntas. `None` para
        /// un rename suelto — que es todo lo que hay fuera del ejecutor de
        /// lotes.
        ///
        /// El lote vive en ESTA variante y no en el contexto de la task porque
        /// el ejecutor de lotes solo emite renames.
        ///
        /// OBLIGACIÓN DEL EJECUTOR: cada paso llama a `Provider::rename`
        /// DIRECTAMENTE. Si en su lugar pasara por el camino de move con
        /// política de colisión, una sobrescritura emitiría un
        /// [`Mutation::Removed`] —clasificado `Irreversible`— que se quedaría
        /// FUERA del grupo: un borrado permanente dentro de una operación que
        /// el wire anuncia como una unidad deshacible, y que un undo por lote
        /// ni siquiera vería para bloquearse. El planificador ya rechaza el
        /// plan entero ante cualquier colisión, así que el ejecutor nunca tiene
        /// motivo para sobrescribir nada.
        batch: Option<i64>,
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
