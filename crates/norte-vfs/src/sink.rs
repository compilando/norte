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

    /// Fills the still EMPTY staging with all of `src`, from its current
    /// offset to its end, without passing the bytes through the caller
    /// (ADR 0165): a reflink or an in-kernel copy, when the sink can.
    ///
    /// `progress` is called with the bytes copied so far; returning
    /// `false` stops the copy, and the call answers
    /// `Some(Err(Error::Cancelled))`. The staging contract is unchanged:
    /// nothing is published until [`Self::commit`].
    ///
    /// `None` = this sink cannot, and NOTHING was written: the caller
    /// streams instead. Default: `None`.
    ///
    /// ```
    /// # use norte_vfs::ByteSink;
    /// # async fn demo(sink: &mut dyn ByteSink, src: std::fs::File) {
    /// let progress = std::sync::Arc::new(|_done: u64| true);
    /// match sink.fill_from(src, progress).await {
    ///     None => { /* stream it chunk by chunk */ }
    ///     Some(Ok(copied)) => { let _ = copied; /* commit */ }
    ///     Some(Err(_e)) => { /* abort */ }
    /// }
    /// # }
    /// ```
    async fn fill_from(
        &mut self,
        src: std::fs::File,
        progress: FillProgress,
    ) -> Option<Result<u64, Error>> {
        let _ = (src, progress);
        None
    }
}

/// What [`ByteSink::fill_from`] reports to: bytes copied so far, and
/// whether to go on.
///
/// ```
/// let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
/// let s = std::sync::Arc::clone(&stop);
/// let progress: norte_vfs::FillProgress =
///     std::sync::Arc::new(move |_done| !s.load(std::sync::atomic::Ordering::Relaxed));
/// assert!(progress(10));
/// stop.store(true, std::sync::atomic::Ordering::Relaxed);
/// assert!(!progress(20), "false stops the copy");
/// ```
pub type FillProgress = std::sync::Arc<dyn Fn(u64) -> bool + Send + Sync>;
