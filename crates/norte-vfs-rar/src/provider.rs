//! [`RarProvider`]: the read-only `Provider` for a `.rar`, served by an
//! external program.
//!
//! It does not compose over another provider: it holds the archive's LOCAL
//! PATH. Who gets to mount a `rar` —only over `file://`— is decided by the
//! engine's dispatch, which is where it is known what is on the other side
//! of the inner scheme.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use norte_proto::{
    ByteRange, Capabilities, CapabilityFlags, ConflictKind, Entry, EntryKind, Error, VPath,
};
use norte_vfs::{ByteSink, ByteStream, EntryStream, Provider};
use tokio_util::sync::CancellationToken;

use crate::delegate::Delegate;
use crate::index::{ArchiveIndex, InnerPath};
use crate::listing::Listing;
use crate::{LIST_TIMEOUT, RarLimits};

/// The provider for ONE `.rar` archive.
pub struct RarProvider {
    archive: PathBuf,
    delegate: Delegate,
    limits: RarLimits,
    /// Cached index and its generation. One archive, one slot.
    cache: Mutex<Option<Arc<ArchiveIndex>>>,
    /// The index arrives already set and is NOT rebuilt: only tests that pin
    /// policy without a `.rar` to back it use this.
    pinned: bool,
}

impl RarProvider {
    /// A provider for the `.rar` living at `archive`, served by `delegate`.
    #[must_use]
    pub fn new(archive: PathBuf, delegate: Delegate, limits: RarLimits) -> Self {
        Self {
            archive,
            delegate,
            limits,
            cache: Mutex::new(None),
            pinned: false,
        }
    }

    /// A provider with the index already set: for tests that pin policy (an
    /// encrypted entry, an ambiguous name) without needing a `.rar` that
    /// genuinely contains them.
    #[must_use]
    pub fn with_index_for_test(index: ArchiveIndex) -> Self {
        Self {
            archive: PathBuf::from("/dev/null"),
            delegate: Delegate::SevenZip(PathBuf::from("/nonexistent/7z")),
            limits: RarLimits::default(),
            cache: Mutex::new(Some(Arc::new(index))),
            pinned: true,
        }
    }

    /// Splits the path apart and checks it talks about THIS provider.
    fn split(p: &VPath) -> Result<InnerPath, Error> {
        match p.archive_split() {
            Ok(Some(aref)) if aref.format == "rar" => {
                Ok(aref.inner.iter().map(|s| s.as_bytes().to_vec()).collect())
            }
            _ => Err(Error::InvalidPath),
        }
    }

    /// `(mtime_ms, size)` of the `.rar`: the invalidation currency.
    ///
    /// `metadata` is blocking I/O, so it goes to `spawn_blocking` (rule 2).
    /// Without an mtime nothing gets cached: a stale index would show files
    /// that are no longer there.
    async fn generation(&self) -> Result<(Option<i64>, Option<u64>), Error> {
        let path = self.archive.clone();
        let meta = tokio::task::spawn_blocking(move || std::fs::metadata(&path))
            .await
            .map_err(|_| Error::Internal { panic: true })?
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => Error::NotFound,
                std::io::ErrorKind::PermissionDenied => Error::PermissionDenied,
                _ => Error::Io { retryable: false },
            })?;
        if !meta.is_file() {
            return Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            });
        }
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_millis()).ok());
        Ok((mtime_ms, Some(meta.len())))
    }

    /// The index, from cache or rebuilt by calling the delegate.
    async fn index(&self) -> Result<Arc<ArchiveIndex>, Error> {
        if self.pinned {
            let cache = self.cache.lock().expect("cache lock is sound");
            return Ok(Arc::clone(cache.as_ref().expect("pinned index")));
        }
        let generation = self.generation().await?;
        if generation.0.is_some() {
            let cache = self.cache.lock().expect("cache lock is sound");
            if let Some(hit) = cache.as_ref().filter(|i| i.generation == generation) {
                return Ok(Arc::clone(hit));
            }
        }
        let argv = self.delegate.list_argv(&self.archive);
        let stdout = self
            .delegate
            .run_capture(&argv, LIST_TIMEOUT)
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "the rar listing failed");
                Error::from(e)
            })?;
        let Listing { entries, skipped } = match self.delegate {
            Delegate::SevenZip(_) => crate::parse_7z_slt(&stdout),
            Delegate::Unrar(_) => crate::parse_unrar_vt(&stdout),
        };
        let index = Arc::new(ArchiveIndex::build(
            entries,
            &self.limits,
            generation,
            skipped,
        ));
        if generation.0.is_some() {
            *self.cache.lock().expect("cache lock is sound") = Some(Arc::clone(&index));
        }
        Ok(index)
    }
}

#[async_trait]
impl Provider for RarProvider {
    fn scheme(&self) -> &'static str {
        "rar+file"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            flags: CapabilityFlags::READ_ONLY
                | CapabilityFlags::CASE_SENSITIVE
                | CapabilityFlags::CASE_PRESERVING,
            max_path: None,
        }
    }

    async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        let inner = Self::split(p)?;
        self.index().await?.entry_for(p, &inner)
    }

    /// Listing of a dir in the virtual tree.
    ///
    /// CONTRACT (ADR 0018 C2, plus one rule that is RAR's own): entries
    /// whose name does not map to `VPath` segments do NOT appear (`..`,
    /// `.`, empty, NUL, absolute, `!` component), NOR do the ones the
    /// delegate's line-based listing cannot carry (a name with `\n` or
    /// `\r`). All of them are counted in [`Provider::list_skipped`].
    async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
        let inner = Self::split(p)?;
        let index = self.index().await?;
        if !inner.is_empty() {
            match index.node(&inner) {
                None => return Err(Error::NotFound),
                Some(n) if n.kind != EntryKind::Dir => {
                    return Err(Error::Conflict {
                        conflict: ConflictKind::TypeMismatch,
                    });
                }
                Some(_) => {}
            }
        }
        let mut entries = Vec::new();
        for name in index.children.get(&inner).into_iter().flatten() {
            let seg = norte_proto::Segment::new(name.clone())
                .expect("the index only contains valid segments");
            let child_path = p.join(seg);
            let mut child_key = inner.clone();
            child_key.push(name.clone());
            entries.push(index.entry_for(&child_path, &child_key));
        }
        Ok(futures::stream::iter(entries).boxed())
    }

    async fn list_skipped(&self, p: &VPath) -> Result<Option<u64>, Error> {
        Self::split(p)?;
        Ok(Some(self.index().await?.skipped()))
    }

    /// Reads ONE entry by making the delegate spit it out through `stdout`.
    ///
    /// Two explicit refusals before starting anything:
    ///
    /// - an **encrypted** entry gets listed but not read: the password
    ///   cannot be asked for (the child has `stdin` deliberately closed) and
    ///   pretending the file is empty would be worse;
    /// - a name the delegate would treat as a **pattern** and that reaches
    ///   another entry is refused: the wrong stream is indistinguishable
    ///   from the right one.
    ///
    /// `range` is served by discarding from the stream, because a pipe has
    /// no seek: the same thing is asked of the delegate and it is cut off as
    /// soon as there is enough —killing the child—, instead of waiting for
    /// it to finish decompressing.
    async fn read(&self, p: &VPath, range: Option<ByteRange>) -> Result<ByteStream, Error> {
        let inner = Self::split(p)?;
        let index = self.index().await?;
        if inner.is_empty() {
            return Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            });
        }
        let node = index.node(&inner).ok_or(Error::NotFound)?;
        if node.kind != EntryKind::File {
            return Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            });
        }
        let name = inner.join(&b'/');
        if node.encrypted {
            tracing::warn!(
                name = ?String::from_utf8_lossy(&name),
                "encrypted entry: it gets listed, but reading it would demand a password nobody can type"
            );
            return Err(Error::Unsupported);
        }
        index.addressable(&name).map_err(Error::from)?;
        let argv = self.delegate.read_argv(&self.archive, &name);
        // The token belongs to the stream: dropping it kills the child (rule 3).
        let cancel = CancellationToken::new();
        let guard = cancel.clone().drop_guard();
        let stream = self
            .delegate
            .run_stream(&argv, cancel)
            .await
            .map_err(Error::from)?;
        Ok(apply_range(stream, range, guard).boxed())
    }

    async fn write(&self, _p: &VPath) -> Result<Box<dyn ByteSink>, Error> {
        Err(Error::Unsupported)
    }

    async fn mkdir(&self, _p: &VPath) -> Result<(), Error> {
        Err(Error::Unsupported)
    }

    async fn remove(&self, _p: &VPath) -> Result<(), Error> {
        Err(Error::Unsupported)
    }

    async fn rename(&self, _from: &VPath, _to: &VPath) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
}

/// Trims the delegate's stream to the requested `range` and cuts it off as
/// soon as it is served. `guard` kills the child when dropped: it travels
/// INSIDE the stream so that an early cutoff and the consumer's drop have
/// the same effect.
fn apply_range(
    stream: ByteStream,
    range: Option<ByteRange>,
    guard: tokio_util::sync::DropGuard,
) -> impl futures::Stream<Item = Result<bytes::Bytes, Error>> + Send {
    let (to_skip, left) = match range {
        Some(r) => (r.offset, r.len),
        None => (0, None),
    };
    // The trimming state lives in the unfold's STATE, not captured by the
    // async block: an `async move` would copy the counters into every chunk
    // and the trim would be reapplied from scratch — `unused_assignments`
    // is what gave the bug away.
    futures::stream::unfold(
        (Some(stream), guard, to_skip, left),
        |(stream, guard, mut to_skip, mut left)| async move {
            let mut stream = stream?;
            if left == Some(0) {
                return None; // served: dropping the guard kills the child
            }
            loop {
                let chunk = match stream.next().await {
                    Some(Ok(c)) => c,
                    Some(Err(e)) => return Some((Err(e), (None, guard, to_skip, left))),
                    None => return None,
                };
                let chunk = if to_skip >= chunk.len() as u64 {
                    to_skip -= chunk.len() as u64;
                    continue;
                } else {
                    let start = usize::try_from(to_skip).unwrap_or(usize::MAX);
                    to_skip = 0;
                    chunk.slice(start..)
                };
                let chunk = match left {
                    Some(n) if (chunk.len() as u64) > n => {
                        let take = usize::try_from(n).unwrap_or(usize::MAX);
                        left = Some(0);
                        chunk.slice(..take)
                    }
                    Some(n) => {
                        left = Some(n - chunk.len() as u64);
                        chunk
                    }
                    None => chunk,
                };
                return Some((Ok(chunk), (Some(stream), guard, to_skip, left)));
            }
        },
    )
}

impl std::fmt::Debug for RarProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RarProvider")
            .field("archive", &self.archive)
            .field("delegate", &self.delegate)
            .finish_non_exhaustive()
    }
}
