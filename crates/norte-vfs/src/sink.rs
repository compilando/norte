//! [`ByteSink`]: escritura transaccional de un archivo. La invariante de
//! cancelación limpia del proyecto ("destino limpio o `.norte-partial`,
//! jamás un archivo a medias sin marcar") vive AQUÍ, como contrato del sink —
//! no es heroísmo del copy engine.

use async_trait::async_trait;
use bytes::Bytes;
use norte_proto::Error;

/// Destino de escritura transaccional devuelto por
/// [`Provider::write`](crate::Provider::write).
///
/// Contrato (verificado por la suite contractual):
/// - Los bytes van a un staging propio del provider (p. ej.
///   `.norte-partial.<hash>.<pid>-<seq>` en FS reales — nombre corto que NO
///   deriva del nombre final, que puede rozar `NAME_MAX`); el path final NO
///   existe ni cambia hasta [`Self::commit`].
/// - [`Self::commit`] publica el contenido completo en el path final, de
///   forma atómica si el backend puede (`RENAME_ATOMIC`).
/// - [`Self::abort`] elimina todo rastro del staging; es la vía de la
///   cancelación y del fallo.
/// - Soltar el sink sin commit equivale a un abort best-effort: un provider
///   DEBE intentar limpiar en `Drop`, pero solo `abort()` explícito garantiza
///   la limpieza (Drop no puede hacer I/O async de forma fiable).
#[async_trait]
pub trait ByteSink: Send {
    /// Añade un chunk al staging. Errores típicos: [`Error::NoSpace`],
    /// [`Error::Io`].
    async fn write(&mut self, chunk: Bytes) -> Result<(), Error>;

    /// Publica el contenido en el path final y consume el sink.
    async fn commit(self: Box<Self>) -> Result<(), Error>;

    /// Elimina el staging sin publicar nada y consume el sink.
    /// Idempotente respecto a un staging ya desaparecido.
    async fn abort(self: Box<Self>) -> Result<(), Error>;

    /// Suelta el staging SIN publicar y SIN borrar, durabilizándolo, para
    /// que un [`Provider::open_resumable`](crate::Provider::open_resumable)
    /// posterior lo reencuentre y REANUDE (ADR 0012). Es la vía de la
    /// cancelación/fallo cuando el caller pidió resume.
    ///
    /// Default: [`abort`](Self::abort) — un provider sin reanudación NO
    /// deja parcial (degrada a destino limpio, coherente con `resume=Off`).
    async fn keep(self: Box<Self>) -> Result<(), Error> {
        self.abort().await
    }
}
