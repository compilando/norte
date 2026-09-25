//! [`ByteSink`]: transactional write of a file. The project's clean
//! cancellation invariant ("clean destination or `.norte-partial`, never
//! an unmarked half-done file") lives HERE, as the sink's contract — it's
//! not heroics on the copy engine's part.

use async_trait::async_trait;
use bytes::Bytes;
use norte_proto::Error;

/// Transactional write destination returned by
/// [`Provider::write`](crate::Provider::write).
///
/// Contract (verified by the contract suite):
/// - The bytes go to a staging file of the provider's own (e.g.
///   `.norte-partial.<hash>.<pid>-<seq>` on real FSes — a short name that
///   does NOT derive from the final name, which may brush `NAME_MAX`); the
///   final path neither exists nor changes until [`Self::commit`].
/// - [`Self::commit`] publishes the complete content at the final path,
///   atomically if the backend can (`RENAME_ATOMIC`).
/// - [`Self::abort`] removes every trace of the staging; it's the path
///   cancellation and failure take.
/// - Dropping the sink without a commit is equivalent to a best-effort
///   abort: a provider MUST try to clean up in `Drop`, but only an
///   explicit `abort()` guarantees the cleanup (Drop can't reliably do
///   async I/O).
#[async_trait]
pub trait ByteSink: Send {
    /// Adds a chunk to the staging. Typical errors: [`Error::NoSpace`],
    /// [`Error::Io`].
    async fn write(&mut self, chunk: Bytes) -> Result<(), Error>;

    /// Publishes the content at the final path and consumes the sink.
    async fn commit(self: Box<Self>) -> Result<(), Error>;

    /// Removes the staging without publishing anything and consumes the
    /// sink. Idempotent with respect to a staging that's already gone.
    async fn abort(self: Box<Self>) -> Result<(), Error>;

    /// Drops the staging WITHOUT publishing and WITHOUT deleting it,
    /// durabilizing it, so a later
    /// [`Provider::open_resumable`](crate::Provider::open_resumable) finds
    /// it again and RESUMES it (ADR 0012). It's the path cancellation/
    /// failure takes when the caller asked for resume.
    ///
    /// Default: [`abort`](Self::abort) — a provider without resume leaves
    /// NO partial behind (degrades to a clean destination, consistent
    /// with `resume=Off`).
    async fn keep(self: Box<Self>) -> Result<(), Error> {
        self.abort().await
    }
}
