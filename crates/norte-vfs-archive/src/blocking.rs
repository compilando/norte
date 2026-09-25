//! Sync→async bridge: `Read + Seek` over the inner provider's
//! `Provider::read(range)`, for archive format parsers that run in
//! `spawn_blocking` (rule 2 / ADR 0002: the blocking thread may block on
//! `block_on`; the runtime never does).
//!
//! Also [`spawn_blocking`]: the only door to that thread, because it
//! preserves the caller's span (ADR 0127).

use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

use futures::StreamExt;
use norte_proto::{ByteRange, VPath};
use norte_vfs::Provider;

/// Like [`tokio::task::spawn_blocking`], but the closure runs inside the
/// span that was active at call time: whatever the parsers log keeps
/// hanging off their task (ADR 0127). The crate's `clippy.toml` forbids
/// calling it directly.
#[allow(
    clippy::disallowed_methods,
    reason = "the only place allowed to call it: this is where the span gets added"
)]
pub(crate) fn spawn_blocking<F, R>(f: F) -> tokio::task::JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let span = tracing::Span::current();
    tokio::task::spawn_blocking(move || span.in_scope(f))
}

/// Read block size: parsers do local bursts (headers, central directory)
/// — a block amortizes round-trips to the inner provider.
const BLOCK: u64 = 256 * 1024;

/// Sync reader positioned over a file of the inner provider, with a cache
/// of the last block. ONLY for `spawn_blocking` threads.
///
/// Runtime note: `Handle::block_on` doesn't drive a `current_thread`
/// runtime's IO/time drivers unless its thread is inside
/// `Runtime::block_on`. Irrelevant in the daemon (multi-thread); pure
/// inner providers (Mem) don't need them either.
pub(crate) struct ProviderReader {
    handle: tokio::runtime::Handle,
    inner: Arc<dyn Provider>,
    path: VPath,
    len: u64,
    pos: u64,
    /// (block offset, bytes) — the last block read.
    block: Option<(u64, Vec<u8>)>,
}

impl ProviderReader {
    /// `len` comes from the container's `stat`, which the caller already
    /// did (and which governs the index's invalidation: same generation,
    /// same view).
    pub(crate) fn new(
        handle: tokio::runtime::Handle,
        inner: Arc<dyn Provider>,
        path: VPath,
        len: u64,
    ) -> Self {
        Self {
            handle,
            inner,
            path,
            len,
            pos: 0,
            block: None,
        }
    }

    fn fetch_block(&mut self, block_off: u64) -> std::io::Result<()> {
        let want = BLOCK.min(self.len.saturating_sub(block_off));
        let range = ByteRange {
            offset: block_off,
            len: Some(want),
        };
        let inner = Arc::clone(&self.inner);
        let path = self.path.clone();
        let bytes: Result<Vec<u8>, norte_proto::Error> = self.handle.block_on(async move {
            let mut stream = inner.read(&path, Some(range)).await?;
            let mut out = Vec::with_capacity(usize::try_from(want).unwrap_or(0));
            while let Some(chunk) = stream.next().await {
                out.extend_from_slice(&chunk?);
            }
            Ok(out)
        });
        let bytes = bytes.map_err(std::io::Error::other)?;
        self.block = Some((block_off, bytes));
        Ok(())
    }
}

impl Clone for ProviderReader {
    /// An independent reader over the SAME container: position at 0 and
    /// an EMPTY block cache (cloning doesn't drag along up to 256 KiB of
    /// block).
    fn clone(&self) -> Self {
        Self {
            handle: self.handle.clone(),
            inner: Arc::clone(&self.inner),
            path: self.path.clone(),
            len: self.len,
            pos: 0,
            block: None,
        }
    }
}

impl Read for ProviderReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let block_off = self.pos - (self.pos % BLOCK);
        let hit = self
            .block
            .as_ref()
            .is_some_and(|(off, _)| *off == block_off);
        if !hit {
            self.fetch_block(block_off)?;
        }
        let (off, bytes) = self.block.as_ref().expect("freshly loaded block");
        let start = usize::try_from(self.pos - off).map_err(std::io::Error::other)?;
        if start >= bytes.len() {
            // The inner provider returned less than expected (container
            // mutated under our feet): clean EOF; generation-based
            // invalidation will do the rest on the next operation.
            return Ok(0);
        }
        let n = buf.len().min(bytes.len() - start);
        buf[..n].copy_from_slice(&bytes[start..start + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for ProviderReader {
    fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
        let target: i128 = match from {
            SeekFrom::Start(o) => i128::from(o),
            SeekFrom::End(d) => i128::from(self.len) + i128::from(d),
            SeekFrom::Current(d) => i128::from(self.pos) + i128::from(d),
        };
        let target =
            u64::try_from(target).map_err(|_| std::io::Error::other("seek before byte 0"))?;
        self.pos = target;
        Ok(self.pos)
    }
}

/// Recovers the INNER provider's [`norte_proto::Error`] if this
/// `io::Error` wraps one ([`ProviderReader::fetch_block`] puts it inside
/// `io::Error::other`, and `targz_format`'s `CountingReader` does the same
/// with the bomb's `Cancelled`/`Corrupt`): a network drop mid-parse is
/// genuine IO from the inner provider and MUST propagate verbatim —
/// `Corrupt` stays reserved for a genuinely broken format (#58).
///
/// FIX-3 (rust MINOR-2, #55 review): walks the WHOLE `source()` chain, not
/// just the outermost `io::Error`. Parsers like `tar`/`flate2` can
/// re-wrap the inner reader's error inside their own type (or inside a
/// NEW `io::Error` that in turn wraps the original) before it gets here —
/// a single-level downcast would miss it and disguise it as `Corrupt`. At
/// each hop of the chain it's checked (a) whether the link itself IS a
/// `norte_proto::Error`, and (b) whether it's an `io::Error` whose
/// `get_ref()` wraps one.
pub(crate) fn inner_proto_error(e: &std::io::Error) -> Option<norte_proto::Error> {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(e);
    // Defensive ceiling: no real error chain in this crate nests more
    // than a handful of levels; cuts off a pathological cycle instead of
    // hanging.
    for _ in 0..16 {
        let err = current?;
        if let Some(proto_err) = err.downcast_ref::<norte_proto::Error>() {
            return Some(proto_err.clone());
        }
        if let Some(io_err) = err.downcast_ref::<std::io::Error>()
            && let Some(inner) = io_err.get_ref()
            && let Some(proto_err) = inner.downcast_ref::<norte_proto::Error>()
        {
            return Some(proto_err.clone());
        }
        current = err.source();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use norte_proto::Segment;
    use norte_testkit::MemProvider;

    async fn seed(content: &[u8]) -> (Arc<dyn Provider>, VPath) {
        let mem = MemProvider::new();
        let path = MemProvider::root().join(Segment::new(b"f.bin".to_vec()).expect("seg"));
        let mut sink = mem.write(&path).await.expect("write");
        sink.write(Bytes::copy_from_slice(content))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
        (Arc::new(mem), path)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn the_blocking_thread_runs_inside_the_callers_span() {
        let dispatch = tracing::Dispatch::new(tracing_subscriber::registry());
        let _guard = tracing::dispatcher::set_default(&dispatch);
        let span = tracing::info_span!("task", task_id = 7);
        let outside = span.id().expect("span with subscriber");
        let inside = {
            let _e = span.enter();
            let d = dispatch.clone();
            spawn_blocking(move || {
                tracing::dispatcher::with_default(&d, || tracing::Span::current().id())
            })
        }
        .await
        .expect("the closure returns");
        assert_eq!(inside, Some(outside));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reads_with_seek_and_crosses_blocks() {
        // > BLOCK to force two blocks.
        let content: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let (mem, path) = seed(&content).await;
        let handle = tokio::runtime::Handle::current();
        let len = content.len() as u64;
        let content2 = content.clone();
        spawn_blocking(move || {
            let mut r = ProviderReader::new(handle, mem, path, len);
            // A read that CROSSES the block boundary (256 KiB).
            r.seek(SeekFrom::Start(262_100)).expect("seek");
            let mut buf = [0u8; 100];
            r.read_exact(&mut buf).expect("read_exact");
            assert_eq!(&buf[..], &content2[262_100..262_200]);
            // SeekFrom::End and reading the tail.
            r.seek(SeekFrom::End(-5)).expect("seek end");
            let mut tail = Vec::new();
            r.read_to_end(&mut tail).expect("tail");
            assert_eq!(tail, &content2[content2.len() - 5..]);
            // Past-EOF: Ok(0).
            r.seek(SeekFrom::Start(len + 10)).expect("seek past");
            let mut b = [0u8; 4];
            assert_eq!(r.read(&mut b).expect("read past-EOF"), 0);
        })
        .await
        .expect("blocking thread");
    }
}
