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
    /// An open-ended read of the container, kept between blocks while the
    /// parser reads forward (#397): `(offset of its next byte, stream)`.
    /// Opening one per block re-did every byte before it when the
    /// container is itself compressed — a tar inside a deflated zip entry
    /// decompressed from byte 0 for every 256 KiB.
    stream: Option<(u64, norte_vfs::ByteStream)>,
    /// What the stream already gave past the last block's end.
    carry: Vec<u8>,
    /// Where the previous block ended: a block starting there is a
    /// sequential read, the only kind worth an open-ended stream (a
    /// random one would read ahead for nothing).
    last_end: Option<u64>,
    /// How far a forward seek is read through instead of reopened:
    /// [`SKIP_BY_READING`] when the container is itself an archive's entry
    /// (reopening decompresses from its start), 0 for a plain file, where a
    /// seek is cheap and reading through a tar's data to its next header
    /// would read the whole archive.
    skip_by_reading: u64,
    /// Reads opened on the inner provider, for the tests.
    #[cfg(test)]
    opens: usize,
}

/// How far ahead a forward seek is served by reading and discarding from
/// the open stream instead of a new read: over a compressed container that
/// is always cheaper than starting over, and over a plain one it bounds
/// what is read for nothing.
const SKIP_BY_READING: u64 = 8 * 1024 * 1024;

/// A fetched block, the stream to keep (if any) and what it read past it.
type Fetched = (Vec<u8>, Option<(u64, norte_vfs::ByteStream)>, Vec<u8>);

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
        let skip_by_reading = if norte_proto::scheme_archive_format(path.scheme()).is_some() {
            SKIP_BY_READING
        } else {
            0
        };
        Self {
            handle,
            inner,
            path,
            len,
            pos: 0,
            block: None,
            stream: None,
            carry: Vec::new(),
            last_end: None,
            skip_by_reading,
            #[cfg(test)]
            opens: 0,
        }
    }

    fn fetch_block(&mut self, block_off: u64) -> std::io::Result<()> {
        let want = usize::try_from(BLOCK.min(self.len.saturating_sub(block_off)))
            .map_err(std::io::Error::other)?;
        // The open stream serves this block if it is at it, or a little
        // behind it (a short forward seek): the gap is read and dropped.
        let stream_at = self.stream.as_ref().map(|(next, _)| *next);
        let reuse = stream_at
            .is_some_and(|next| next <= block_off && block_off - next <= self.skip_by_reading);
        let sequential = self.last_end == Some(block_off);
        if !reuse {
            self.stream = None;
            self.carry.clear();
        }
        let open_ended = reuse || sequential;
        let taken = self.stream.take();
        let mut carry = std::mem::take(&mut self.carry);
        let inner = Arc::clone(&self.inner);
        let path = self.path.clone();
        #[cfg(test)]
        if taken.is_none() {
            self.opens += 1;
        }
        let fetched: Result<Fetched, norte_proto::Error> = self.handle.block_on(async move {
            let (mut at, mut stream) = if let Some(s) = taken {
                s
            } else {
                // Not sequential (yet): exactly the block, as always.
                let len = (!open_ended).then_some(want as u64);
                let range = ByteRange {
                    offset: block_off,
                    len,
                };
                (block_off, inner.read(&path, Some(range)).await?)
            };
            // Drop what lies between the stream and this block.
            let mut gap = usize::try_from(block_off - at).unwrap_or(usize::MAX);
            let dropped = gap.min(carry.len());
            carry.drain(..dropped);
            gap -= dropped;
            let mut out = Vec::with_capacity(want);
            let from_carry = want.min(carry.len());
            out.extend(carry.drain(..from_carry));
            while out.len() < want {
                let Some(chunk) = stream.next().await else {
                    break;
                };
                let chunk = chunk?;
                let mut piece: &[u8] = &chunk;
                let skip = gap.min(piece.len());
                piece = &piece[skip..];
                gap -= skip;
                let need = want - out.len();
                if piece.len() > need {
                    out.extend_from_slice(&piece[..need]);
                    carry.extend_from_slice(&piece[need..]);
                } else {
                    out.extend_from_slice(piece);
                }
            }
            at = block_off + out.len() as u64;
            let keep = open_ended.then_some((at, stream));
            Ok((out, keep, carry))
        });
        let (bytes, stream, carry) = fetched.map_err(std::io::Error::other)?;
        self.last_end = Some(block_off + bytes.len() as u64);
        self.stream = stream;
        self.carry = carry;
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
            stream: None,
            carry: Vec::new(),
            last_end: None,
            skip_by_reading: self.skip_by_reading,
            #[cfg(test)]
            opens: 0,
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

    /// #397: a forward read keeps ONE open read of the container instead
    /// of one per 256 KiB block — over a deflated zip entry, each new read
    /// decompressed again from byte 0. Short forward seeks ride the same
    /// stream; a seek back starts over, bounded, and the bytes are right in
    /// every case.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_forward_read_keeps_one_stream_open() {
        let content: Vec<u8> = (0..4_000_000u32).map(|i| (i % 251) as u8).collect();
        let (mem, path) = seed(&content).await;
        let handle = tokio::runtime::Handle::current();
        let len = content.len() as u64;
        spawn_blocking(move || {
            let mut r = ProviderReader::new(handle, mem, path, len);
            let mut all = Vec::new();
            r.read_to_end(&mut all).expect("whole");
            assert_eq!(all, content);
            assert_eq!(r.opens, 2, "one bounded read, then one stream for the rest");

            // A plain container: a skip ahead is a seek, never a read
            // through what lies between (a tar's data up to its next header).
            let mut plain = r.clone();
            let mut buf = vec![0u8; 300_000];
            plain.read_exact(&mut buf).expect("two blocks");
            plain
                .seek(SeekFrom::Current(1_000_000))
                .expect("skip ahead");
            plain.read_exact(&mut buf).expect("after the skip");
            assert_eq!(&buf[..], &content[1_300_000..1_600_000]);
            // 2 before; after the skip, a bounded read of the block it
            // lands in, and a stream again once reading is sequential.
            assert_eq!(
                plain.opens, 4,
                "the skip reopened instead of reading through"
            );

            // A container that is itself an archive's entry: reopening would
            // decompress from its start, so a short skip reads through.
            let mut r = r.clone();
            r.skip_by_reading = SKIP_BY_READING;
            r.read_exact(&mut buf).expect("two blocks");
            r.seek(SeekFrom::Current(1_000_000)).expect("skip ahead");
            r.read_exact(&mut buf).expect("after the skip");
            assert_eq!(&buf[..], &content[1_300_000..1_600_000]);
            let after_skip = r.opens;
            assert_eq!(after_skip, 2, "a short skip ahead reads through");

            r.seek(SeekFrom::Start(10)).expect("back");
            let mut b = [0u8; 20];
            r.read_exact(&mut b).expect("back read");
            assert_eq!(&b[..], &content[10..30]);
            assert_eq!(r.opens, after_skip + 1, "a seek back opens again");
        })
        .await
        .expect("blocking thread");
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
