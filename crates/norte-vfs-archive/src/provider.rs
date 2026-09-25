//! [`ArchiveProvider`]: the read-only `Provider` for compressed archives.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use norte_proto::{
    ArchiveRef, ByteRange, Capabilities, CapabilityFlags, ConflictKind, Entry, EntryKind, Error,
    VPath,
};
use norte_vfs::{ByteSink, ByteStream, EntryStream, Provider};

use crate::blocking::ProviderReader;
use crate::index::{ArchiveIndex, InnerPath, Limits, Locator};

/// Supported container format (proto's `ARCHIVE_FORMATS` whitelist).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Plain tar (ustar/GNU/pax). Passthrough read (contiguous data).
    Tar,
    /// zip stored+deflate. Read by decompressing in a blocking thread.
    Zip,
    /// tar.gz/tgz (ADR 0028, #55): OPAQUE gz layer over tar — sequential
    /// index (`entries()`, no `Seek`) and forward-decode read (discards up
    /// to the offset with a fresh decoder per read).
    TarGz,
}

impl Format {
    fn token(self) -> &'static str {
        match self {
            Self::Tar => "tar",
            Self::Zip => "zip",
            Self::TarGz => "tar+gz",
        }
    }
}

/// ONE container's cached index. Since #59 it's ONLY the index (the zip
/// locator is self-contained: no archive object is retained); the struct
/// keeps its name to minimize churn.
#[derive(Clone)]
struct CachedContainer {
    index: Arc<ArchiveIndex>,
}

/// A minimal LRU index cache: key = the outer's canonical wire. Fixed cap
/// (ADR 0018): RAII — when the provider dies, everything dies with it.
struct IndexCache {
    map: HashMap<String, CachedContainer>,
    /// Usage order (the last one is the most recent).
    order: Vec<String>,
}

const CACHE_CAP: usize = 8;

/// Ceiling on CONCURRENT `tar+gz` forward-decode reads per provider
/// (FIX-2, security MAJOR, #55 review). The discard up to `offset` in
/// `read_entry_gz` can pin a `spawn_blocking` thread for MINUTES (up to
/// `Limits::max_decompressed_bytes` of real inflate) — N concurrent deep
/// reads are N threads simultaneously blocked on tokio's `spawn_blocking`
/// pool, which is SHARED by the daemon's WHOLE runtime (the journal, other
/// providers, background tasks…), not exclusive to this provider: without
/// a ceiling, a client firing many deep reads of a large `tar+gz` starves
/// the entire blocking pool (`DoS`). The [`tokio::sync::Semaphore`] sets a
/// hard ceiling: EXCESS reads get QUEUED (wait their turn), never rejected.
const GZ_READ_CONCURRENCY: usize = 4;

/// The outer container's `(mtime_ms, size)`: the invalidation currency
/// throughout this module's cache/spool.
type Generation = (Option<i64>, Option<u64>);

/// Spool heat threshold (#95.1): on the
/// [`SPOOL_HEAT_THRESHOLD`]-th gz read of the same container (same
/// generation) the decompressed spool gets built.
const SPOOL_HEAT_THRESHOLD: u32 = 2;

/// Ceiling on the heat map's entries. It's just a heuristic: when full, an
/// arbitrary entry gets evicted.
const SPOOL_HEAT_CAP: usize = 32;

/// Sentinel in the heat map: a NON-spoolable container (its decompressed
/// size exceeds `Limits::spool_max_bytes`) — the build isn't retried until
/// it changes generation. `saturating_add` pins it here.
const SPOOL_UNSPOOLABLE: u32 = u32::MAX;

/// ONE hot `tar+gz` container's spool (#95.1): its whole gz stream
/// DECOMPRESSED into a temporary file, so repeated reads are local O(1)
/// seeks instead of O(offset) forward-decode.
struct Spool {
    /// The outer container's canonical wire (same key as `IndexCache`).
    key: String,
    /// The container's generation when spooled — the invalidation key.
    generation: Generation,
    /// A [`tempfile::tempfile()`] file: ANONYMOUS — born already unlinked,
    /// the OS reclaims the space when the last descriptor dies and it NEVER
    /// has a pathname (zero attack surface via a staging name). Behind a
    /// `Mutex` because the fd's cursor is SHARED (a `try_clone` is a dup:
    /// same offset) — every chunk re-seeks to an ABSOLUTE position under
    /// the lock, so two concurrent reads of the spool don't step on each other.
    file: Arc<Mutex<std::fs::File>>,
    /// The spool's total decompressed bytes.
    len: u64,
}

/// The spool's shared state (#95.1). Lives in an `Arc` because the
/// `spawn_blocking` threads that build/install the spool need `'static`.
struct SpoolState {
    /// A SINGLE slot per provider (v1): the last hot container wins —
    /// another container that heats up REPLACES the previous one.
    slot: tokio::sync::Mutex<Option<Spool>>,
    /// Heat per container: number of gz reads of the generation seen. A
    /// new generation resets the counter (and un-marks a non-spoolable
    /// one); old generations' entries get pruned this way, opportunistically.
    heat: Mutex<HashMap<String, (Generation, u32)>>,
    /// Build in progress (the container's key). Competitors do NOT wait:
    /// they fall to forward-decode — only one thread pays for the build.
    building: Mutex<Option<String>>,
}

impl SpoolState {
    fn new() -> Self {
        Self {
            slot: tokio::sync::Mutex::new(None),
            heat: Mutex::new(HashMap::new()),
            building: Mutex::new(None),
        }
    }

    /// Adds a read to `key`'s heat and returns the resulting count.
    fn bump_heat(&self, key: &str, generation: Generation) -> u32 {
        let mut heat = self.heat.lock().expect("heat lock is healthy");
        if heat.len() >= SPOOL_HEAT_CAP
            && !heat.contains_key(key)
            && let Some(victim) = heat.keys().next().cloned()
        {
            heat.remove(&victim);
        }
        let e = heat.entry(key.to_owned()).or_insert((generation, 0));
        if e.0 != generation {
            *e = (generation, 0);
        }
        e.1 = e.1.saturating_add(1);
        e.1
    }

    /// Negative-cache: `key`'s decompressed size exceeds the budget —
    /// don't retry the build for as long as this generation lasts.
    fn mark_unspoolable(&self, key: &str, generation: Generation) {
        self.heat
            .lock()
            .expect("heat lock is healthy")
            .insert(key.to_owned(), (generation, SPOOL_UNSPOOLABLE));
    }

    /// Claims the build flag for `key`. `None` = another build is in
    /// progress (the caller falls to forward-decode, never waits).
    fn try_claim_build(self: &Arc<Self>, key: &str) -> Option<SpoolBuildClaim> {
        let mut building = self.building.lock().expect("building lock is healthy");
        if building.is_some() {
            return None;
        }
        *building = Some(key.to_owned());
        Some(SpoolBuildClaim {
            state: Arc::clone(self),
        })
    }
}

/// RAII for the build flag (#95.1): clears it on drop no matter what —
/// success, abort, a thread panic, or a `spawn_blocking` closure dropped
/// without running (runtime shutdown). Only ONE can exist at a time (the
/// `None → Some` transition happens under the lock), so clearing without
/// comparing is correct.
struct SpoolBuildClaim {
    state: Arc<SpoolState>,
}

impl SpoolBuildClaim {
    fn state(&self) -> &SpoolState {
        &self.state
    }
}

impl Drop for SpoolBuildClaim {
    fn drop(&mut self) {
        *self
            .state
            .building
            .lock()
            .expect("building lock is healthy") = None;
    }
}

impl IndexCache {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: Vec::new(),
        }
    }

    fn touch(&mut self, key: &str) {
        self.order.retain(|k| k != key);
        self.order.push(key.to_owned());
    }

    fn get(
        &mut self,
        key: &str,
        generation: (Option<i64>, Option<u64>),
    ) -> Option<CachedContainer> {
        let hit = self.map.get(key)?;
        // Unknown mtime = ALWAYS stale (ADR 0018): with no validator no
        // cache is worth anything.
        if hit.index.generation != generation || generation.0.is_none() {
            self.map.remove(key);
            self.order.retain(|k| k != key);
            return None;
        }
        let hit = hit.clone();
        self.touch(key);
        Some(hit)
    }

    fn put(&mut self, key: &str, container: CachedContainer) {
        if self.map.len() >= CACHE_CAP
            && !self.map.contains_key(key)
            && let Some(evict) = self.order.first().cloned()
        {
            self.map.remove(&evict);
            self.order.retain(|k| k != &evict);
        }
        self.map.insert(key.to_owned(), container);
        self.touch(key);
    }
}

/// A read-only provider serving the content of compressed archives that
/// live on ANOTHER provider (composition, ADR 0018 B2). One instance
/// serves ONE composite scheme (`tar+file`, `tar+sftp`…) over ONE inner
/// provider. Nesting caveat (#56): a NESTED layer's cache generation is
/// the (mtime, size) of the entry INSIDE the outer archive — replacing the
/// outer container with entries of identical metadata can serve a stale
/// inner index until eviction; complete-read CRCs (#59) and fail-loud
/// short-reads are the belt.
pub struct ArchiveProvider {
    scheme: String,
    format: Format,
    inner: Arc<dyn Provider>,
    limits: Limits,
    cache: Mutex<IndexCache>,
    /// Single-flight for index construction (#61): one builder per key;
    /// concurrent ones wait on the lock and re-read the cache. The map is
    /// pruned when the last interested party drops its Arc.
    building: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Concurrency ceiling for reads with a decompressed DISCARD: `tar+gz`
    /// forward-decode (FIX-2, #55 review) and, since #59, zip's RANGED
    /// deflate (skip>0 — deflate has no seek). See [`GZ_READ_CONCURRENCY`].
    /// `Tar` and discard-free reads don't go through here.
    gz_read_permits: Arc<tokio::sync::Semaphore>,
    /// Spool for hot tar.gz files (#95.1): single slot + heat + build flag.
    /// In an `Arc` for the blocking threads (see [`SpoolState`]).
    spool: Arc<SpoolState>,
}

/// Wraps a passthrough stream with a DELIVERED-vs-PROMISED counter (#97):
/// the index promised `expected` bytes — if the inner stream ends early
/// (a container truncated/mutated under our feet, pread semantics with no
/// error) or delivers extra (a lying inner provider), the consumer gets
/// `Error::Corrupt`, never silently short or extra data. An `Err` from the
/// inner side propagates verbatim and cuts the stream short. `container` =
/// the container's ALREADY-REDACTED display (for traces).
fn expect_exact(inner: ByteStream, expected: u64, container: String) -> ByteStream {
    // State: the inner stream lives in an Option — in terminal states it's
    // DROPPED immediately (review m1: a remote inner side could pin
    // buffers/a connection slot until the caller drops the wrapper).
    futures::stream::unfold(
        (Some(inner), 0u64, container),
        move |(stream, got, container)| async move {
            let mut stream = stream?;
            match stream.next().await {
                Some(Ok(chunk)) => {
                    let got = got + chunk.len() as u64;
                    if got > expected {
                        tracing::warn!(
                            got,
                            expected,
                            %container,
                            "tar passthrough delivers EXTRA bytes"
                        );
                        return Some((Err(Error::Corrupt), (None, got, container)));
                    }
                    Some((Ok(chunk), (Some(stream), got, container)))
                }
                Some(Err(e)) => Some((Err(e), (None, got, container))),
                None => {
                    if got < expected {
                        tracing::warn!(
                            got,
                            expected,
                            %container,
                            "tar passthrough short: container truncated/mutated under the read"
                        );
                        return Some((Err(Error::Corrupt), (None, got, container)));
                    }
                    None
                }
            }
        },
    )
    // Review M1: Unfold PANICS if polled after Ready(None) — fused, like
    // the rest of this crate's ByteStreams (poll_fn/iter/empty).
    .fuse()
    .boxed()
}

impl ArchiveProvider {
    /// A provider with the default limits. `scheme` is the full composite
    /// one (`tar+file`); it must start with the format's token.
    ///
    /// # Panics
    /// If `scheme` doesn't start with `<format>+` — a wiring error, not a
    /// data one (the core composes the scheme from this same token).
    #[must_use]
    pub fn new(inner: Arc<dyn Provider>, format: Format, scheme: impl Into<String>) -> Self {
        Self::with_limits(inner, format, scheme, Limits::default())
    }

    /// Like [`Self::new`] with custom limits (bomb tests; future config).
    ///
    /// # Panics
    /// See [`Self::new`].
    #[must_use]
    pub fn with_limits(
        inner: Arc<dyn Provider>,
        format: Format,
        scheme: impl Into<String>,
        limits: Limits,
    ) -> Self {
        let scheme = scheme.into();
        assert!(
            scheme.starts_with(&format!("{}+", format.token())),
            "composite scheme `{scheme}` doesn't match format {format:?}"
        );
        Self {
            scheme,
            format,
            inner,
            limits,
            cache: Mutex::new(IndexCache::new()),
            building: Mutex::new(HashMap::new()),
            gz_read_permits: Arc::new(tokio::sync::Semaphore::new(GZ_READ_CONCURRENCY)),
            spool: Arc::new(SpoolState::new()),
        }
    }

    /// Validates the path against this provider and splits it apart (ADR 0018).
    fn split(&self, p: &VPath) -> Result<ArchiveRef, Error> {
        if p.scheme() != self.scheme {
            return Err(Error::InvalidPath);
        }
        match p.archive_split() {
            Ok(Some(aref)) if aref.format == self.format.token() => Ok(aref),
            _ => Err(Error::InvalidPath),
        }
    }

    /// Stats the container on the inner side: it must be a file.
    async fn outer_stat(&self, aref: &ArchiveRef) -> Result<Entry, Error> {
        let e = self.inner.stat(&aref.outer).await?;
        if e.kind != EntryKind::File {
            return Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            });
        }
        Ok(e)
    }

    /// The container's index, from cache or rebuilt (`spawn_blocking`).
    async fn index_for(&self, aref: &ArchiveRef) -> Result<CachedContainer, Error> {
        let outer = self.outer_stat(aref).await?;
        let generation = (outer.mtime_ms, outer.size);
        let key = aref.outer.to_wire();
        {
            let mut cache = self.cache.lock().expect("cache lock is healthy");
            if let Some(hit) = cache.get(&key, generation) {
                return Ok(hit);
            }
        }
        // MINOR-4 (#61): with no mtime nothing is cacheable — `get()`
        // would always throw it away as stale (`IndexCache::get`'s rule).
        // Single-flight only pays off when the coalesced work gets
        // REUSED; here there's no possible reuse, so going through the
        // lock would only serialize N builds behind one with no benefit.
        // Direct path, in parallel, like pre-B2.
        if generation.0.is_none() {
            let container_len = outer.size.unwrap_or(0);
            return self.build_blocking(aref, generation, container_len).await;
        }

        // RAII (MAJOR-1, #61): `slot` is declared BEFORE `_build_guard` on
        // purpose — Rust drops locals in reverse declaration order, so on
        // any exit (return, `?`, panic, future CANCELLATION) the guard
        // releases the mutex first and the slot gets pruned from the map
        // (or is left for the next interested party) afterward, without
        // the race of two finalists watching each other's Arc that the
        // manual `prune_building` had.
        let slot = BuildingSlot::new(self, &key);
        let _build_guard = slot.shared().lock_owned().await;
        // MINOR-3 (#61): re-stat UNDER the lock. The stat above only
        // serves the fast path (a hot cache); a waiter may have waited on
        // the lock long enough that its generation went stale — using the
        // old one here would overwrite (or fail to overwrite) a fresh
        // entry the previous builder already put in with the CURRENT
        // generation.
        let outer = self.outer_stat(aref).await?;
        let generation = (outer.mtime_ms, outer.size);
        // Double-check: another caller may have built while we waited.
        {
            let mut cache = self.cache.lock().expect("cache lock is healthy");
            if let Some(hit) = cache.get(&key, generation) {
                return Ok(hit);
            }
        }
        let container_len = outer.size.unwrap_or(0);
        let container = self.build_blocking(aref, generation, container_len).await?;
        // With no mtime there's no validator: get() would always call it
        // stale — don't spend an LRU slot on an unrecoverable index.
        if generation.0.is_some() {
            self.cache
                .lock()
                .expect("cache lock is healthy")
                .put(&key, container.clone());
        }
        Ok(container)
    }

    /// Builds the index on a `spawn_blocking` thread. No cache nor
    /// single-flight of its own: shared by `index_for`'s locked path and
    /// MINOR-4's unknown-generation shortcut.
    async fn build_blocking(
        &self,
        aref: &ArchiveRef,
        generation: (Option<i64>, Option<u64>),
        container_len: u64,
    ) -> Result<CachedContainer, Error> {
        let reader = ProviderReader::new(
            tokio::runtime::Handle::current(),
            Arc::clone(&self.inner),
            aref.outer.clone(),
            container_len,
        );
        let limits = self.limits;
        let format = self.format;
        // Rule 3: if this future dies (the caller cancels), the guard
        // arms the flag and the blocking thread cuts short at the loop's
        // next entry.
        let cancel = Arc::new(AtomicBool::new(false));
        let mut guard = CancelOnDrop::new(Arc::clone(&cancel));
        let joined = crate::blocking::spawn_blocking(move || match format {
            Format::Tar => {
                crate::tar_format::build_index(reader, container_len, generation, &limits, &cancel)
            }
            Format::Zip => {
                crate::zip_format::build_index(reader, container_len, generation, &limits, &cancel)
            }
            Format::TarGz => {
                // No `container_len` as the locator's bound (ADR 0028):
                // that size is the COMPRESSED one and bounds nothing
                // about the decompressed stream — truncation is detected
                // in the read.
                crate::targz_format::build_index_gz(reader, generation, &limits, &cancel)
            }
        })
        .await;
        guard.disarm();
        let index = joined
            .map_err(|e| {
                if e.is_panic() {
                    Error::Internal { panic: true }
                } else {
                    // Runtime shutting down: cancellation, not a bug (spec §17.7).
                    Error::Cancelled
                }
            })
            .and_then(|r| r)?;
        Ok(CachedContainer {
            index: Arc::new(index),
        })
    }

    fn inner_key(aref: &ArchiveRef) -> InnerPath {
        aref.inner.iter().map(|s| s.as_bytes().to_vec()).collect()
    }

    /// zip read (#59): decompression in a blocking thread → bounded
    /// channel → stream; a FRESH reader per read, a self-contained locator
    /// — no retained archive nor CD re-parse. Dropping the stream = the
    /// send fails (delivery phase) or the closed-channel check cuts the
    /// DISCARD short (rust MAJOR-1 from review #59) = the thread ends
    /// (rule 3).
    async fn read_zip(
        &self,
        aref: &ArchiveRef,
        plan: crate::zip_format::ReadPlan,
    ) -> Result<norte_vfs::ByteStream, Error> {
        let handle = tokio::runtime::Handle::current();
        let inner = Arc::clone(&self.inner);
        let outer_path = aref.outer.clone();
        let outer_len = plan.container_len;
        // rust MAJOR-1 (#59 review): a RANGED deflate discards O(skip) by
        // decompressing in the blocking thread (deflate has no seek) —
        // the same pool starvation as gz forward-decode (#55 FIX-2): it
        // shares its semaphore. stored and discard-free reads don't pay it.
        let permit = if plan.method == 8 && plan.skip > 0 {
            Some(
                Arc::clone(&self.gz_read_permits)
                    .acquire_owned()
                    .await
                    .map_err(|_| Error::Cancelled)?,
            )
        } else {
            None
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        // The JoinHandle is dropped on purpose: the thread's lifetime is
        // governed by the channel, not the caller — the orphan left after
        // a drop is bounded by the channel (4 chunks) during delivery AND
        // by the closed-channel check during discard.
        drop(crate::blocking::spawn_blocking(move || {
            let _permit = permit; // released when the thread ends
            let reader = ProviderReader::new(handle, inner, outer_path, outer_len);
            crate::zip_format::read_entry(reader, &plan, &tx);
        }));
        Ok(futures::stream::poll_fn(move |cx| rx.poll_recv(cx)).boxed())
    }

    /// tar.gz read (#95.1). Three paths:
    ///
    /// 1. **Spool hit** (a hot container, same generation): serves the
    ///    span with local seeks of the tempfile — no decompression, NO
    ///    semaphore (pins nothing).
    /// 2. **A read crossing the heat threshold**: builds the spool INSIDE
    ///    the same blocking thread (under the gz permit the read is
    ///    already paying for) and serves the span from it. Competitors
    ///    during the build fall to path 3, they never wait.
    /// 3. **Forward-decode** (cold, zero budget, no mtime, non-spoolable
    ///    or someone else's build in progress): the usual path — discards
    ///    up to the offset with a fresh decoder (ADR 0028), under the
    ///    FIX-2 semaphore.
    ///
    /// Dropping the stream = the send fails / `is_closed` cuts it short =
    /// the thread ends (rule 3), on all three paths.
    async fn read_gz(
        &self,
        aref: &ArchiveRef,
        cached: &CachedContainer,
        offset: u64,
        req_off: u64,
        req_len: u64,
    ) -> Result<norte_vfs::ByteStream, Error> {
        let key = aref.outer.to_wire();
        let generation = cached.index.generation;
        // ABSOLUTE position of the span in the decompressed stream.
        let start = offset + req_off;

        // Path 1: a current spool. Unknown mtime = NEVER spool (with no
        // validator no cache is worth anything — same criterion as IndexCache).
        if generation.0.is_some() {
            let mut slot = self.spool.slot.lock().await;
            if let Some(s) = slot.as_ref()
                && s.key == key
            {
                if s.generation == generation {
                    let file = Arc::clone(&s.file);
                    let spool_len = s.len;
                    drop(slot);
                    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
                    drop(crate::blocking::spawn_blocking(move || {
                        serve_from_spool(&file, spool_len, start, req_len, &tx);
                    }));
                    return Ok(futures::stream::poll_fn(move |cx| rx.poll_recv(cx)).boxed());
                }
                // Same container, old generation: it no longer serves
                // anyone — drop it right away (frees the disk) and let
                // the heat start fresh for the new generation.
                tracing::debug!(
                    container = %aref.outer.display_lossy(),
                    "spool discarded: the container changed generation"
                );
                *slot = None;
            }
        }

        // Heat + build decision (path 2 vs 3).
        let claim = if generation.0.is_some() && self.limits.spool_max_bytes > 0 {
            let n = self.spool.bump_heat(&key, generation);
            if n >= SPOOL_HEAT_THRESHOLD && n != SPOOL_UNSPOOLABLE {
                self.spool.try_claim_build(&key)
            } else {
                None
            }
        } else {
            None
        };

        // FIX-2 (security MAJOR, #55 review): discard/decompression can
        // pin the blocking thread for minutes — bound the aggregate
        // concurrency with the semaphore (see [`GZ_READ_CONCURRENCY`]).
        // EXCESS reads get QUEUED here (await), never rejected. The
        // spool's build runs under the SAME permit as the read that triggers it.
        let permit = Arc::clone(&self.gz_read_permits)
            .acquire_owned()
            .await
            .map_err(|_| {
                // The semaphore is never `close()`d during this
                // provider's lifetime (no caller closes it) — unreachable
                // in practice; an explicit fail-safe instead of an
                // `expect` that could panic if something changes.
                Error::Cancelled
            })?;
        let reader = ProviderReader::new(
            tokio::runtime::Handle::current(),
            Arc::clone(&self.inner),
            aref.outer.clone(),
            generation.1.unwrap_or(0),
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        if let Some(claim) = claim {
            let job = SpoolBuildJob {
                claim,
                key,
                generation,
                budget: self.limits.spool_max_bytes,
                start,
                len: req_len,
                container: aref.outer.display_lossy(),
            };
            drop(crate::blocking::spawn_blocking(move || {
                let _permit = permit; // released when the thread ends
                build_spool_and_serve(job, reader, &tx);
            }));
        } else {
            drop(crate::blocking::spawn_blocking(move || {
                let _permit = permit; // released when the thread ends
                crate::targz_format::read_entry_gz(reader, start, req_len, &tx);
            }));
        }
        Ok(futures::stream::poll_fn(move |cx| rx.poll_recv(cx)).boxed())
    }
}

/// Parameters for the spool's build+serve (#95.1), bundled for the
/// blocking thread.
struct SpoolBuildJob {
    claim: SpoolBuildClaim,
    key: String,
    generation: Generation,
    budget: u64,
    /// Absolute position of the requested span in the decompressed stream.
    start: u64,
    /// Length of the requested span.
    len: u64,
    /// The container's ALREADY-redacted display (traces only).
    container: String,
}

/// Builds the spool (a COMPLETE forward-decode of the container into the
/// anonymous tempfile) and, if it succeeds, installs it as the provider's
/// single slot and serves the requested span from it. Any abort degrades
/// honestly: a dead receiver = nothing to serve; over-budget =
/// negative-cache + forward-decode; a build failure = forward-decode (if
/// the container is really broken, the re-read will fail with the correct
/// error via the usual path). The build flag is released by `job.claim`'s
/// drop on EVERY path (RAII).
fn build_spool_and_serve(
    job: SpoolBuildJob,
    reader: ProviderReader,
    tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, Error>>,
) {
    use crate::targz_format::{SpoolAbort, read_entry_gz, spool_gz};
    let mut file = match tempfile::tempfile() {
        Ok(f) => f,
        Err(e) => {
            // No tempfile means no spool; the read continues via forward-decode.
            tracing::warn!(
                error = %e,
                container = %job.container,
                "no tempfile for the tar.gz spool; forward-decode"
            );
            drop(job.claim);
            return read_entry_gz(reader, job.start, job.len, tx);
        }
    };
    // A FRESH reader for the build (`Clone` resets the position and block
    // cache); the original is kept for the fallback if the build aborts.
    let probe = || tx.is_closed();
    match spool_gz(reader.clone(), job.budget, &probe, &mut file) {
        Ok(len) => {
            let file = Arc::new(Mutex::new(file));
            let spool = Spool {
                key: job.key,
                generation: job.generation,
                file: Arc::clone(&file),
                len,
            };
            // blocking_lock: we're on a `spawn_blocking` thread, never
            // inside the async runtime (where it would panic).
            *job.claim.state().slot.blocking_lock() = Some(spool);
            tracing::debug!(
                bytes = len,
                container = %job.container,
                "tar.gz spool built and installed"
            );
            drop(job.claim);
            serve_from_spool(&file, len, job.start, job.len, tx);
        }
        Err(SpoolAbort::Cancelled) => {
            // Dead receiver: discards the partial without noise (rule 3).
            // The claim's drop releases the flag; the tempfile's drop, the disk.
            tracing::debug!(
                container = %job.container,
                "tar.gz spool build cancelled (receiver dead)"
            );
        }
        Err(SpoolAbort::OverBudget) => {
            job.claim.state().mark_unspoolable(&job.key, job.generation);
            tracing::warn!(
                budget = job.budget,
                container = %job.container,
                "decompressed size exceeds spool_max_bytes: container not spoolable"
            );
            drop(job.claim);
            read_entry_gz(reader, job.start, job.len, tx);
        }
        Err(SpoolAbort::Io(e)) => {
            tracing::warn!(
                error = %e,
                container = %job.container,
                "tar.gz spool build failed; forward-decode"
            );
            drop(job.claim);
            read_entry_gz(reader, job.start, job.len, tx);
        }
    }
}

/// Serves `[start, start+len)` of the spool file in 64 KiB chunks over the
/// bounded channel. Every chunk re-seeks to an ABSOLUTE position under the
/// file's lock (the fd's cursor is shared — see [`Spool::file`]). A short
/// read = fail-loud `Corrupt`: the index promised bytes the spool doesn't
/// have (the container and the spool don't match) — never silently short
/// data. The caller already trimmed the span against the ENTRY's size
/// (pread semantics), same as in `read_entry_gz`.
fn serve_from_spool(
    file: &Mutex<std::fs::File>,
    spool_len: u64,
    start: u64,
    len: u64,
    tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, Error>>,
) {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let send_err = |e: Error| {
        // Best effort: if the receiver died, there's nobody to tell.
        let _ = tx.blocking_send(Err(e));
    };
    if start.checked_add(len).is_none_or(|end| end > spool_len) {
        tracing::warn!(start, len, spool_len, "requested span outside the spool");
        return send_err(Error::Corrupt);
    }
    let mut buf = vec![0u8; 64 * 1024];
    let mut pos = start;
    let mut remaining = len;
    while remaining > 0 {
        if tx.is_closed() {
            tracing::debug!("spool read cancelled (receiver dead)");
            return;
        }
        let want = buf
            .len()
            .min(usize::try_from(remaining).unwrap_or(buf.len()));
        let got = {
            let mut f = file.lock().expect("spool file lock is healthy");
            f.seek(SeekFrom::Start(pos))
                .and_then(|_| f.read(&mut buf[..want]))
        };
        match got {
            // EOF before serving the promised span: a short read of the spool.
            Ok(0) => return send_err(Error::Corrupt),
            Ok(n) => {
                pos += n as u64;
                remaining -= n as u64;
                if tx
                    .blocking_send(Ok(bytes::Bytes::copy_from_slice(&buf[..n])))
                    .is_err()
                {
                    tracing::debug!("spool read cancelled (receiver dead)");
                    return;
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "IO on the spool file");
                return send_err(Error::Io { retryable: true });
            }
        }
    }
}

/// Single-flight's RAII guard (MAJOR-1, #61): registers (or reuses) the
/// key's lock when created and, when it dies (success, error, or
/// CANCELLATION at any `await` — Rust's drop runs just the same), drops
/// its Arc and removes the map entry if it's left as the sole owner. No
/// manual pruning per call site and no race between two finalists each
/// seeing the other's Arc (both counted 3 and nobody deleted it).
struct BuildingSlot<'a> {
    provider: &'a ArchiveProvider,
    key: String,
    lock: Option<Arc<tokio::sync::Mutex<()>>>,
}

impl<'a> BuildingSlot<'a> {
    fn new(provider: &'a ArchiveProvider, key: &str) -> Self {
        let lock = {
            let mut building = provider.building.lock().expect("building lock is healthy");
            Arc::clone(
                building
                    .entry(key.to_owned())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
            )
        };
        Self {
            provider,
            key: key.to_owned(),
            lock: Some(lock),
        }
    }

    /// The lock's Arc for `lock_owned` (the guard keeps ITS OWN Arc and is
    /// dropped before the slot — declaration order in `index_for`).
    fn shared(&self) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(self.lock.as_ref().expect("slot alive until the drop"))
    }
}

impl Drop for BuildingSlot<'_> {
    fn drop(&mut self) {
        let mut building = self
            .provider
            .building
            .lock()
            .expect("building lock is healthy");
        drop(self.lock.take()); // drops OUR Arc before counting
        if let Some(l) = building.get(&self.key)
            && Arc::strong_count(l) == 1
        {
            building.remove(&self.key);
        }
    }
}

/// Arms an `AtomicBool` if its owner dies without disarming it: the
/// cancellation signal to the indexing blocking thread (rule 3).
struct CancelOnDrop {
    flag: Arc<AtomicBool>,
    armed: bool,
}

impl CancelOnDrop {
    fn new(flag: Arc<AtomicBool>) -> Self {
        Self { flag, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.flag.store(true, Ordering::Relaxed);
        }
    }
}

/// The zip format's attrs catalogue (#108 block 2): everything comes from
/// the already-indexed CD — zero I/O per query.
fn zip_catalog() -> &'static [norte_proto::AttrInfo] {
    use norte_proto::{AttrHint, AttrInfo, AttrType};
    static CAT: std::sync::LazyLock<Vec<AttrInfo>> = std::sync::LazyLock::new(|| {
        vec![
            AttrInfo {
                id: "archive.method".to_owned(),
                label: "Method".to_owned(),
                ty: AttrType::Text,
                hint: AttrHint::Opaque,
            },
            AttrInfo {
                id: "archive.packed_size".to_owned(),
                label: "Packed".to_owned(),
                ty: AttrType::Uint,
                hint: AttrHint::Size,
            },
            AttrInfo {
                id: "archive.crc32".to_owned(),
                label: "CRC-32".to_owned(),
                ty: AttrType::Uint,
                hint: AttrHint::Opaque,
            },
        ]
    });
    &CAT
}

#[async_trait]
impl Provider for ArchiveProvider {
    fn scheme(&self) -> &str {
        &self.scheme
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
        self.stat_with(p, &norte_vfs::ListOptions::default()).await
    }

    async fn stat_with(&self, p: &VPath, opt: &norte_vfs::ListOptions) -> Result<Entry, Error> {
        let aref = self.split(p)?;
        let cached = self.index_for(&aref).await?;
        cached
            .index
            .entry_for(p, &Self::inner_key(&aref), &opt.attrs)
    }

    /// Catalogue PER FORMAT (#108 block 2): zip keeps method/crc/packed in
    /// its CD; tar/tar.gz have no per-entry method nor CRC (and a solid
    /// gz stream's per-entry packed size means nothing) → empty.
    fn attrs(&self) -> &[norte_proto::AttrInfo] {
        match self.format {
            Format::Zip => zip_catalog(),
            Format::Tar | Format::TarGz => &[],
        }
    }

    /// Listing of a dir of the virtual tree.
    ///
    /// CONTRACT (ADR 0018 C2): container entries whose name doesn't map to
    /// valid `VPath` segments (`..`, `.`, empty, NUL, absolute, a `!`
    /// component) do NOT appear — they're omitted at index time with
    /// `tracing::warn!` and count in the index's `skipped`. Duplicates:
    /// the last one wins; a file-vs-dir conflict: the dir wins (a file at
    /// an ancestor position gets promoted to a dir).
    ///
    /// Since #59 zip's CD is parsed with OUR OWN parser: distinct raw
    /// names that decode the same do NOT collapse (H1 closed) and the
    /// Info-ZIP 0x7075 extra is ignored by design — it never substitutes
    /// the name nor kills the archive (H3 closed).
    async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
        self.list_with(p, &norte_vfs::ListOptions::default()).await
    }

    async fn list_with(
        &self,
        p: &VPath,
        opt: &norte_vfs::ListOptions,
    ) -> Result<EntryStream, Error> {
        let aref = self.split(p)?;
        let cached = self.index_for(&aref).await?;
        let index = &cached.index;
        let key = Self::inner_key(&aref);
        if !key.is_empty() {
            match index.nodes.get(&key) {
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
        for name in index.children.get(&key).into_iter().flatten() {
            let seg = norte_proto::Segment::new(name.clone())
                .expect("the index only contains valid segments");
            let child_path = p.join(seg);
            let mut child_key = key.clone();
            child_key.push(name.clone());
            entries.push(index.entry_for(&child_path, &child_key, &opt.attrs));
        }
        Ok(futures::stream::iter(entries).boxed())
    }

    /// Total omitted from `p`'s CONTAINER's index (#93): the ones the
    /// internal index's `skipped` counts (hostile names/per-entry limits,
    /// [`Self::list`]'s contract). Reuses the cached index — after a
    /// `list` it's a cheap query.
    async fn list_skipped(&self, p: &VPath) -> Result<Option<u64>, Error> {
        let aref = self.split(p)?;
        let cached = self.index_for(&aref).await?;
        Ok(Some(cached.index.skipped))
    }

    async fn read(&self, p: &VPath, range: Option<ByteRange>) -> Result<ByteStream, Error> {
        let aref = self.split(p)?;
        let cached = self.index_for(&aref).await?;
        let index = &cached.index;
        let key = Self::inner_key(&aref);
        if key.is_empty() {
            return Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            });
        }
        let node = index.nodes.get(&key).ok_or(Error::NotFound)?;
        if node.kind != EntryKind::File {
            return Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            });
        }
        let Some(locator) = &node.locator else {
            // Listable but unreadable (unsupported method, encrypted…).
            return Err(Error::Unsupported);
        };
        // The requested range is trimmed to the ENTRY's span (pread semantics).
        let entry_size = node.size.unwrap_or(0);
        let req_off = range.map_or(0, |r| r.offset);
        if req_off >= entry_size {
            return Ok(futures::stream::empty().boxed());
        }
        let available = entry_size - req_off;
        let req_len = range
            .and_then(|r| r.len)
            .map_or(available, |l| l.min(available));
        if req_len == 0 {
            return Ok(futures::stream::empty().boxed());
        }
        match *locator {
            Locator::Tar { offset, .. } => {
                // Passthrough: contiguous, uncompressed data. With a
                // delivered-vs-promised counter (#97): a container
                // truncated/mutated UNDER the read ends short with pread
                // semantics and no error — the exact kind of silence zip
                // (#95.4) and targz (FIX-1 #55) already make fail-loud.
                let inner = self
                    .inner
                    .read(
                        &aref.outer,
                        Some(ByteRange {
                            offset: offset + req_off,
                            len: Some(req_len),
                        }),
                    )
                    .await?;
                Ok(expect_exact(inner, req_len, aref.outer.display_lossy()))
            }
            Locator::Zip {
                header_offset,
                method,
                crc32,
                comp_size,
                uncomp_size,
            } => {
                self.read_zip(
                    &aref,
                    crate::zip_format::ReadPlan {
                        header_offset,
                        method,
                        crc32,
                        comp_size,
                        uncomp_size,
                        // The size from the SAME generation as the index:
                        // a coherent view even if the container changes.
                        container_len: cached.index.generation.1.unwrap_or(0),
                        skip: req_off,
                        take: req_len,
                    },
                )
                .await
            }
            Locator::Gz { offset, .. } => {
                self.read_gz(&aref, &cached, offset, req_off, req_len).await
            }
        }
    }

    async fn read_link(&self, p: &VPath) -> Result<Vec<u8>, Error> {
        let aref = self.split(p)?;
        let cached = self.index_for(&aref).await?;
        let index = &cached.index;
        let key = Self::inner_key(&aref);
        if key.is_empty() {
            return Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            });
        }
        let node = index.nodes.get(&key).ok_or(Error::NotFound)?;
        match (&node.kind, &node.link_target) {
            (EntryKind::Symlink, Some(target)) => Ok(target.clone()),
            (EntryKind::Symlink, None) => Err(Error::Corrupt),
            _ => Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            }),
        }
    }

    // ---------- mutations: READ_ONLY (ADR 0018 E2) ----------

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

#[cfg(test)]
mod tests {
    //! Inline (not `tests/`): needs access to the private `building` field
    //! to verify MAJOR-1's RAII (#61) leaves no orphans.
    use std::time::Duration;

    use bytes::Bytes;
    use norte_proto::Segment;
    use norte_testkit::{MemProvider, ZipSmith};

    use super::*;

    async fn seed_zip(bytes: &[u8]) -> (Arc<ArchiveProvider>, VPath, Arc<MemProvider>) {
        let mem = Arc::new(MemProvider::new());
        let path = MemProvider::root().join(Segment::new(b"f.zip".to_vec()).expect("seg"));
        let mut sink = mem.write(&path).await.expect("write");
        sink.write(Bytes::copy_from_slice(bytes))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
        let root = VPath::archive_compose("zip", &path, &[]).expect("compose");
        let provider = Arc::new(ArchiveProvider::with_limits(
            Arc::clone(&mem) as Arc<dyn Provider>,
            Format::Zip,
            "zip+mem",
            Limits::default(),
        ));
        (provider, root, mem)
    }

    /// MINOR-6 (#61): the first builder gets cancelled (its task aborted)
    /// while a second caller is already queued behind the same `building`
    /// lock — the RAII `BuildingSlot` must release the lock on the
    /// cancellation's drop (with no manual pruning involved) so the waiter
    /// picks up the baton and completes its OWN build cleanly, and
    /// `building` must be empty at the end (no orphans, MAJOR-1).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_cancelled_builder_leaves_no_orphan_and_the_waiter_completes() {
        let bytes = ZipSmith::new().file(b"a.txt", b"hi").build();
        let (provider, root, mem) = seed_zip(&bytes).await;
        // Every Mem operation (including the container's `stat` and every
        // block read) takes a while; gives plenty of margin to abort the
        // first one while it's still inside the build (rule: no blind
        // sleeps, this is only used to give the blocking thread real time
        // between our poll and the abort).
        mem.faults()
            .set_latency_per_op(Some(Duration::from_millis(40)));

        let p1 = Arc::clone(&provider);
        let root1 = root.clone();
        let first = tokio::spawn(async move {
            let _ = p1.list(&root1).await;
        });

        // Waits (no blind sleep: cooperative polling) for the first
        // builder to have REGISTERED its slot — confirms it's inside the
        // locked path before aborting it.
        loop {
            if !provider
                .building
                .lock()
                .expect("building lock is healthy")
                .is_empty()
            {
                break;
            }
            tokio::task::yield_now().await;
        }

        let p2 = Arc::clone(&provider);
        let root2 = root.clone();
        let second = tokio::spawn(async move { p2.list(&root2).await });

        // Lets the second one get queued behind the same lock before
        // cutting the first one short mid-build.
        tokio::time::sleep(Duration::from_millis(5)).await;
        first.abort();
        let _ = first.await; // drains the abort: its stack's drop already ran

        let result = second.await.expect("join on the waiter");
        match result {
            Ok(mut stream) => {
                let entries: Vec<_> = stream.by_ref().collect().await;
                assert!(
                    entries.iter().all(Result::is_ok),
                    "the waiter lists the whole content after the first \
                     one's cancellation"
                );
            }
            Err(e) => panic!(
                "the waiter must complete its own build cleanly after the \
                 first one's cancellation, failed with {e:?}"
            ),
        }

        assert!(
            provider
                .building
                .lock()
                .expect("building lock is healthy")
                .is_empty(),
            "no orphans in `building` after cancelling the first builder \
             (MAJOR-1: RAII with no manual pruning)"
        );
    }

    /// MAJOR-1 (#61), direct reproduction of the leak: if ALL interested
    /// parties for a key get cancelled (nobody survives to do the
    /// "success" pruning), the pre-fix manual `prune_building` never runs
    /// for that entry — a permanent orphan. RAII doesn't depend on anyone
    /// "winning": each `BuildingSlot` gets pruned on ITS OWN drop, no
    /// matter what. Verified against the pre-fix code (see the report):
    /// with manual pruning this leaves `building` with 1 entry; with
    /// RAII, always empty.
    #[tokio::test(flavor = "multi_thread")]
    async fn all_interested_parties_cancelled_leaves_no_orphan() {
        for _ in 0..20 {
            let bytes = ZipSmith::new().file(b"a.txt", b"hi").build();
            let (provider, root, mem) = seed_zip(&bytes).await;
            mem.faults()
                .set_latency_per_op(Some(Duration::from_millis(10)));

            let p1 = Arc::clone(&provider);
            let root1 = root.clone();
            let first = tokio::spawn(async move {
                let _ = p1.list(&root1).await;
            });
            // Cooperative wait (no blind sleep) for the builder to
            // register its slot before adding waiters behind it.
            loop {
                if !provider
                    .building
                    .lock()
                    .expect("building lock is healthy")
                    .is_empty()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }

            let mut waiters = Vec::new();
            for _ in 0..8 {
                let p = Arc::clone(&provider);
                let r = root.clone();
                waiters.push(tokio::spawn(async move { p.list(&r).await }));
            }
            // Lets the 8 queue up behind the same lock before cutting ALL
            // of them short mid-flight (the "client disconnected" scenario
            // under load — with no survivor left to prune at the end).
            tokio::time::sleep(Duration::from_millis(2)).await;
            first.abort();
            let _ = first.await;
            for w in &waiters {
                w.abort();
            }
            for w in waiters {
                let _ = w.await;
            }

            assert!(
                provider
                    .building
                    .lock()
                    .expect("building lock is healthy")
                    .is_empty(),
                "orphan in `building` when ALL interested parties get \
                 cancelled (MAJOR-1)"
            );
        }
    }
}
