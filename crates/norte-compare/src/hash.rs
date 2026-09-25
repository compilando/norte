//! The expensive rung: the sha256 of ONE file, read in streaming through
//! [`Provider::read`].
//!
//! Private on purpose. The cascade does not know how to hash — it receives
//! the result already made, in [`HashOutcome`](crate::cascade::HashOutcome)
//! — and whoever uses the engine from outside asks for the rung with
//! [`CompareOptions::with_hash`](crate::CompareOptions::with_hash), not by
//! calling here. Publishing this would offer a second way to hash a file,
//! and the real one — the copy engine's and the index's — does not live in
//! this crate.
//!
//! # What is read, and what is not
//!
//! The WHOLE file, in whatever chunks the provider gives, and never more
//! than one chunk is materialized at a time: a 40 GB file costs 40 GB of
//! network or disk, not memory. A range would not do — the hash is of the
//! whole content — and a `read` with `range: None` is what every provider
//! implements.
//!
//! # Cancellation
//!
//! The token is checked **once per chunk**, not once per file (hard rule
//! 3): if it were checked per file, cancelling a comparison stuck on a huge
//! file would wait for it to finish reading entirely. That is exactly the
//! case where a user cancels.

use futures::StreamExt;
use norte_proto::VPath;
use norte_vfs::{ByteStream, Provider};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

/// A raw sha256. Never shown to anyone: only compared against another.
pub(crate) type Digest256 = [u8; 32];

/// Why there is no digest.
///
/// The two reasons are treated VERY differently above: a broken read is an
/// error row and the walk continues; a cancellation ends the whole stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HashFailure {
    /// The provider could not open the file, or could not finish reading it.
    Read,
    /// The token fired halfway through the read. There is nothing to clean
    /// up: not a single byte has been written.
    Cancelled,
}

/// `path`'s sha256, read in streaming.
pub(crate) async fn sha256_of(
    provider: &dyn Provider,
    path: &VPath,
    cancel: &CancellationToken,
) -> Result<Digest256, HashFailure> {
    // Before OPENING: with the token already fired, the provider is not
    // asked for a read nobody is going to use (and an empty file has no
    // chunk to check it on afterward).
    if cancel.is_cancelled() {
        return Err(HashFailure::Cancelled);
    }
    let stream = provider
        .read(path, None)
        .await
        .map_err(|_| HashFailure::Read)?;
    fold_digest(stream, cancel).await
}

/// The loop that consumes the byte stream: separated from [`sha256_of`] so
/// the token check can be tested without a cooperating provider.
async fn fold_digest(
    mut stream: ByteStream,
    cancel: &CancellationToken,
) -> Result<Digest256, HashFailure> {
    let mut hasher = Sha256::new();
    while let Some(chunk) = stream.next().await {
        // PER CHUNK. See the module header's cancellation note.
        if cancel.is_cancelled() {
            return Err(HashFailure::Cancelled);
        }
        hasher.update(&chunk.map_err(|_| HashFailure::Read)?);
    }
    Ok(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use norte_proto::{Error, Segment};
    use norte_testkit::MemProvider;

    use super::*;

    fn path(name: &str) -> VPath {
        MemProvider::root().join(Segment::new(name.as_bytes().to_vec()).expect("segment"))
    }

    async fn seed(mem: &MemProvider, name: &str, content: &[u8]) {
        let mut sink = mem.write(&path(name)).await.expect("write");
        sink.write(Bytes::copy_from_slice(content))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }

    fn chunks(items: Vec<Result<Bytes, Error>>) -> ByteStream {
        futures::stream::iter(items).boxed()
    }

    /// Same content, same digest; a different byte, a different digest. And
    /// the chunking does not count: the hash is of the content, not of how
    /// it arrived.
    #[tokio::test]
    async fn the_digest_is_of_the_content_and_not_the_chunking() {
        let mem = MemProvider::new();
        seed(&mem, "a", b"hello").await;
        seed(&mem, "b", b"hello").await;
        seed(&mem, "c", b"hellO").await;
        let cancel = CancellationToken::new();
        let a = sha256_of(&mem, &path("a"), &cancel).await.expect("a");
        let b = sha256_of(&mem, &path("b"), &cancel).await.expect("b");
        let c = sha256_of(&mem, &path("c"), &cancel).await.expect("c");
        assert_eq!(a, b);
        assert_ne!(a, c);

        // The same content split into two chunks gives the same digest.
        let split = fold_digest(
            chunks(vec![
                Ok(Bytes::from_static(b"he")),
                Ok(Bytes::from_static(b"llo")),
            ]),
            &cancel,
        )
        .await
        .expect("digest");
        assert_eq!(split, a);
    }

    /// A file that cannot be opened is [`HashFailure::Read`], not a panic
    /// nor a zero-byte digest.
    #[tokio::test]
    async fn a_file_that_does_not_exist_is_a_read_failure() {
        let mem = MemProvider::new();
        let outcome = sha256_of(&mem, &path("does-not-exist"), &CancellationToken::new()).await;
        assert_eq!(outcome, Err(HashFailure::Read));
    }

    /// An error HALFWAY through the stream too: half a file hashed is not a
    /// digest, it is a lie the size of a file.
    #[tokio::test]
    async fn a_stream_that_breaks_halfway_is_a_read_failure() {
        let outcome = fold_digest(
            chunks(vec![
                Ok(Bytes::from_static(b"he")),
                Err(Error::Io { retryable: false }),
            ]),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(outcome, Err(HashFailure::Read));
    }

    /// The token is checked PER CHUNK, and this test is the only thing that
    /// proves it.
    ///
    /// The stream fires the token on delivering its first chunk: a loop that
    /// only checked the token at the start of the file would happily return
    /// the digest of both chunks. With 40 GB instead of eight bytes, that
    /// difference is half an hour of waiting after pressing cancel.
    #[tokio::test]
    async fn the_token_is_checked_once_per_chunk() {
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        let stream = futures::stream::iter(vec![
            Ok(Bytes::from_static(b"aaaa")),
            Ok(Bytes::from_static(b"bbbb")),
        ])
        .inspect(move |_| trigger.cancel())
        .boxed();
        assert_eq!(
            fold_digest(stream, &cancel).await,
            Err(HashFailure::Cancelled)
        );
    }

    /// With the token already fired, the file is not even opened.
    #[tokio::test]
    async fn an_already_fired_token_does_not_open_the_file() {
        let mem = MemProvider::new();
        seed(&mem, "a", b"hello").await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(
            sha256_of(&mem, &path("a"), &cancel).await,
            Err(HashFailure::Cancelled)
        );
        assert_eq!(mem.faults().read_calls(), 0, "nothing was opened");
    }
}
