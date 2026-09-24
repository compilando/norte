//! Adapter [`PluginProvider`] (#30 stage 2b, ADR 0032): exposes a WASM
//! guest-provider as a normal [`norte_vfs::Provider`]. It REASSEMBLES the
//! trait's streams from the guest's BOUNDED calls — `list` paginating until
//! the cursor is exhausted, `read` reading by range until EOF. Mutations are
//! DELEGATED to the guest: `write` projects the transactional [`ByteSink`]
//! onto the guest's `writer` resource (staging → commit/abort), and
//! `mkdir`/`remove`/`rename` call its functions. A read-only guest responds
//! [`Error::Unsupported`] on all of them and the adapter propagates it;
//! `trash`/`symlink` are not in the WIT interface → Unsupported directly.
//!
//! Every call to the guest is SYNCHRONOUS (wasmtime) and serialized by a
//! `Mutex`; it runs inside `spawn_blocking` so it does not block the async
//! executor (rule 2). `PluginProvider` keeps the [`PluginRuntime`] alive (its
//! epoch ticker governs the guest's CPU deadline).
//!
//! **`[config]` (P2 Task 4a) — documented deferral:** [`PluginProvider::set_settings`]
//! exists (the same contract as [`norte_plugin_host::PluginInstance::set_settings`]/
//! [`ProviderInstance::set_settings`], used by `command`/`previewer` since
//! Task 3/4a), but NO production caller invokes it today. This is a
//! structural reason, not an oversight: a `PluginProvider` is NEVER built
//! from the plugin catalog ([`norte_plugin_host::Catalog`]/
//! `norte_core::plugins::PluginRegistry`) — there is no `plugin.toml` or
//! `[config]` to resolve. The one real provider today (FTP, `ftp_plugin.rs`)
//! is built from `ConnectionSpec`/`connections.toml` (a configuration
//! subsystem that is TOTALLY separate, with no `[config]` schema). The
//! method is ready for the day a provider IS born from a plugin manifest
//! with its own `[config]`, without having to touch more than that one
//! wiring point.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::Bytes;
use futures::stream::{self, StreamExt};
use norte_plugin_host::{
    Capabilities as HostCaps, PluginRuntime, ProviderInstance, RuntimeError, provider_iface,
};
use norte_vfs::proto::{
    ByteRange, Capabilities, CapabilityFlags, ConflictKind, Entry, EntryKind, Error, Segment, VPath,
};
use norte_vfs::{ByteSink, ByteStream, EntryStream, Provider, SymlinkKind};
use tokio::sync::Mutex;

/// Per-operation timeout for the guest (M2, ADR 0033): a call blocked on a
/// wasip2 socket is not cut off by the epoch deadline (that only throttles
/// the guest's CPU). On expiry, the op fails and the provider is marked
/// dead. 30 s = the order of magnitude of connect.
const OP_TIMEOUT: Duration = Duration::from_secs(30);

/// A VFS provider backed by a WASM plugin that exports the WIT `provider`
/// interface (#30). See the module docs.
pub struct PluginProvider {
    /// Keeps the epoch ticker alive (the guest's CPU deadline).
    _runtime: PluginRuntime,
    /// The guest instance; the `Mutex` serializes its synchronous calls.
    inst: Arc<Mutex<ProviderInstance>>,
    /// The scheme this provider serves (e.g. `mem`, `ftp`).
    scheme: String,
    /// Capabilities cached at construction (`Provider::capabilities` is sync).
    caps: Capabilities,
    /// `true` once an op has timed out (M2): the hung `spawn_blocking` thread
    /// holds the `Mutex` forever, so every future op fails fast without
    /// touching it.
    dead: Arc<AtomicBool>,
    /// Per-op timeout (overridden in tests so they don't wait 30 s).
    op_timeout: Duration,
}

impl PluginProvider {
    /// Instantiates the `wasm` guest under `host_caps` and caches its
    /// capabilities.
    ///
    /// # Errors
    /// [`RuntimeError`] if the artifact fails to instantiate or the call to
    /// `capabilities` traps.
    pub fn new(
        runtime: PluginRuntime,
        wasm: &norte_plugin_host::WasmArtifact,
        host_caps: HostCaps,
        scheme: impl Into<String>,
    ) -> Result<Self, RuntimeError> {
        let inst = runtime.instantiate_provider(wasm, host_caps)?;
        Self::from_instance(runtime, inst, scheme)
    }

    /// Like [`Self::new`] but instantiating the guest from the BYTES of an
    /// EMBEDDED component (ADR 0033): the FTP guest is `include_bytes!`-ed
    /// into `norte-core` because the `wasm32-wasip2` target may be missing
    /// on the build host.
    ///
    /// # Errors
    /// [`RuntimeError`] if the bytes fail to instantiate or `capabilities`
    /// traps.
    pub fn from_bytes(
        runtime: PluginRuntime,
        bytes: &[u8],
        host_caps: HostCaps,
        scheme: impl Into<String>,
    ) -> Result<Self, RuntimeError> {
        let inst = runtime.instantiate_provider_bytes(bytes, host_caps)?;
        Self::from_instance(runtime, inst, scheme)
    }

    /// Caches the guest's capabilities and assembles the adapter.
    fn from_instance(
        runtime: PluginRuntime,
        mut inst: ProviderInstance,
        scheme: impl Into<String>,
    ) -> Result<Self, RuntimeError> {
        let guest = inst.capabilities()?;
        // Projects the flags the guest declares honestly (absent ones are
        // simply left unset → capability absent). Symlinks/trash/server-copy
        // do not travel over the WIT interface → the adapter leaves them out
        // (Unsupported).
        let mut flags = CapabilityFlags::empty();
        if guest.read_only {
            flags |= CapabilityFlags::READ_ONLY;
        }
        if guest.case_sensitive {
            flags |= CapabilityFlags::CASE_SENSITIVE;
        }
        if guest.case_preserving {
            flags |= CapabilityFlags::CASE_PRESERVING;
        }
        Ok(Self {
            _runtime: runtime,
            inst: Arc::new(Mutex::new(inst)),
            scheme: scheme.into(),
            caps: Capabilities {
                flags,
                max_path: None,
            },
            dead: Arc::new(AtomicBool::new(false)),
            op_timeout: OP_TIMEOUT,
        })
    }

    /// Sets a short per-op timeout (M2 tests). Doc-hidden: not production API.
    #[doc(hidden)]
    #[must_use]
    pub fn with_op_timeout(mut self, timeout: Duration) -> Self {
        self.op_timeout = timeout;
        self
    }

    /// Installs the `[config]` values (P2 Task 4a) that the guest will see
    /// via `host-config::get`/`all` — the same contract as
    /// [`norte_plugin_host::PluginInstance::set_settings`]: call it BEFORE
    /// any operation that invokes the guest. See the documented deferral in
    /// the module docs: NO production caller uses it today (providers are
    /// not born from a plugin manifest), but the plumbing exists and is
    /// safe to call — even with an empty map, which is the default behavior
    /// when it is never called at all.
    ///
    /// `spawn_blocking` is not needed: it is a pure in-memory write to the
    /// `Store` (it neither compiles nor runs the guest), unlike
    /// [`Self::configure`] or other ops.
    pub async fn set_settings(&self, settings: BTreeMap<String, String>) {
        let mut guard = self.inst.lock().await;
        guard.set_settings(settings);
    }

    /// Configures the guest-provider's connection (#30 stage 3c): the
    /// endpoint ALREADY resolved by the host, credentials and base. Called
    /// ONCE after construction, before using the provider. A connectionless
    /// guest (mem) implements it as a no-op.
    ///
    /// # Errors
    /// The guest's logical error (mapped) or a runtime failure.
    pub async fn configure(
        &self,
        endpoint: String,
        user: String,
        password: String,
        base: String,
    ) -> Result<(), Error> {
        use norte_plugin_host::provider_iface::ProviderConfig;
        self.call(move |g| {
            g.configure(&ProviderConfig {
                endpoint,
                user,
                password,
                base,
            })
            .map_err(|e| map_runtime_error(&e))?
            .map_err(map_vfs_error)
        })
        .await
    }

    /// The raw segments of `p` (the path the guest understands). Only the
    /// SEGMENTS cross over to the guest; the authority (`ftp://user@host:port/…`,
    /// which the engine does include when routing a remote connection) is
    /// NOT projected — the guest is already bound to ONE connection via
    /// `configure`, so the path it cares about is relative to that root. A
    /// local scheme-only provider (mem) carries no authority and it makes
    /// no difference. (There used to be a `debug_assert!` for an absent
    /// authority: that was a FALSE invariant — FTP paths routed by the
    /// engine DO carry an authority and it panicked in debug; encoding H1.)
    fn segments(p: &VPath) -> Vec<Vec<u8>> {
        p.segments().map(<[u8]>::to_vec).collect()
    }

    /// Runs a call to the guest under the lock + timeout (M2). See
    /// [`run_guarded`].
    async fn call<T, F>(&self, f: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(&mut ProviderInstance) -> Result<T, Error> + Send + 'static,
    {
        run_guarded(&self.inst, &self.dead, self.op_timeout, f).await
    }
}

/// Runs `f` against the guest under the ASYNC lock + a per-op timeout (M2).
///
/// The `Mutex` is tokio's: waiting for the lock is `.await` (CANCELABLE), so
/// an op that times out while waiting for the lock only drops its future —
/// it does NOT park a thread. Only the `spawn_blocking` that runs the HUNG
/// guest can leak ONE thread (non-cancelable wasip2 socket I/O), and only
/// one: the rest wait asynchronously. On expiry `dead` is set and every
/// future op fails fast without touching the lock. The `OwnedMutexGuard` is
/// moved to the blocking thread and dropped there after the guest runs, so
/// the lock stays held for exactly as long as the guest is running.
async fn run_guarded<T, F>(
    inst: &Arc<Mutex<ProviderInstance>>,
    dead: &Arc<AtomicBool>,
    op_timeout: Duration,
    f: F,
) -> Result<T, Error>
where
    T: Send + 'static,
    F: FnOnce(&mut ProviderInstance) -> Result<T, Error> + Send + 'static,
{
    // Provider already dead from a previous timeout (M2): fail fast without
    // waiting for the lock (which the hung thread holds forever).
    if dead.load(Ordering::Relaxed) {
        return Err(Error::ProviderUnavailable { retryable: true });
    }
    let inst = Arc::clone(inst);
    let work = async move {
        let mut guard = inst.lock_owned().await; // ASYNC wait, cancelable
        crate::blocking::spawn_blocking(move || {
            let r = f(&mut guard);
            drop(guard); // releases the lock on this thread after the guest
            r
        })
        .await
    };
    let Ok(join) = tokio::time::timeout(op_timeout, work).await else {
        // The guest is still hung on the socket (M2): mark the provider dead
        // so future ops don't block (an accepted leak of ONE thread per
        // stuck socket; the human reconnects). The thread carries its
        // request's span (ADR 0127), so that span never closes either: the
        // cost is bounded by that same thread, and whatever it logs still
        // says who it belongs to.
        dead.store(true, Ordering::Relaxed);
        return Err(Error::ProviderUnavailable { retryable: true });
    };
    join.map_err(|_| Error::Internal { panic: true })?
}

/// Translates the guest's logical error into the protocol's taxonomy.
/// `other` falls to non-panic `Internal` (a coarse category, without
/// inventing detail); `conflict`/`no-space` (reachable on the write path)
/// are mapped faithfully.
fn map_vfs_error(e: provider_iface::VfsError) -> Error {
    use provider_iface::VfsError as V;
    match e {
        V::NotFound => Error::NotFound,
        V::PermissionDenied => Error::PermissionDenied,
        V::Unsupported => Error::Unsupported,
        V::InvalidPath => Error::InvalidPath,
        V::Io => Error::Io { retryable: false },
        V::Corrupt => Error::Corrupt,
        V::CursorExpired => Error::CursorExpired,
        // Transient remote condition (TCP/server down): retryable — the
        // scheduler retries instead of failing hard (rust review m1; the WIT
        // enum does not carry the `retryable` flag, so it is fixed when
        // mapping back).
        V::ProviderUnavailable => Error::ProviderUnavailable { retryable: true },
        V::Loop => Error::Loop,
        V::Conflict => Error::Conflict {
            conflict: ConflictKind::Unknown,
        },
        V::NoSpace => Error::NoSpace,
        V::Other => Error::Internal { panic: false },
    }
}

/// Failure of the guest's runtime.
///
/// Only a TRAP is panic-class (the guest crashed); a controlled rejection
/// —a return-value cap, instantiation— is a NON-panic internal failure.
///
/// An exhausted BUDGET is neither of the two (#211): the deadline is
/// measured on the wall clock, so a loaded machine eats it with a plugin
/// that is merely slow, and saying "the plugin crashed" is the one answer
/// guaranteed to be false. It goes to `ProviderUnavailable { retryable: true }`,
/// which is what really happened: it wasn't given enough time, and next
/// time it might be.
pub(crate) fn map_runtime_error(e: &RuntimeError) -> Error {
    match e {
        RuntimeError::Deadline => Error::ProviderUnavailable { retryable: true },
        // The binary is not the approved one (ADR 0142): the same thing
        // `connect` says when it detects it itself — there is no permission
        // for THAT code.
        RuntimeError::DigestMismatch => Error::PermissionDenied,
        other => Error::Internal {
            panic: matches!(other, RuntimeError::Trap(_)),
        },
    }
}

/// Cap on the number of entries the adapter reassembles from a `list`
/// before failing fail-loud — a hostile guest cannot hang the host by
/// paginating forever.
const MAX_LIST_ENTRIES: usize = 1_000_000;

fn map_kind(k: provider_iface::EntryKind) -> EntryKind {
    use provider_iface::EntryKind as K;
    match k {
        K::File => EntryKind::File,
        K::Dir => EntryKind::Dir,
        K::Symlink => EntryKind::Symlink,
        K::Other => EntryKind::Other,
    }
}

#[async_trait::async_trait]
impl Provider for PluginProvider {
    fn scheme(&self) -> &str {
        &self.scheme
    }

    fn capabilities(&self) -> Capabilities {
        self.caps
    }

    async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        let segs = Self::segments(p);
        let path = p.clone();
        self.call(move |g| {
            let e = g
                .stat(&segs)
                .map_err(|e| map_runtime_error(&e))?
                .map_err(map_vfs_error)?;
            Ok(Entry {
                attrs: std::collections::BTreeMap::new(),
                path,
                kind: map_kind(e.kind),
                size: e.size,
                mtime_ms: None,
            })
        })
        .await
    }

    async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
        let segs = Self::segments(p);
        let dir = p.clone();
        // EAGER reassembly: every page of the cursor is drained and a stream
        // is returned over the Vec (stage 2 de-risk; lazy = a later
        // optimization). A contract's tree is small.
        let entries: Vec<Entry> = self
            .call(move |g| {
                let mut out = Vec::new();
                let mut cursor: Option<Vec<u8>> = None;
                loop {
                    let page = g
                        .list_dir(&segs, cursor.as_deref())
                        .map_err(|e| map_runtime_error(&e))?
                        .map_err(map_vfs_error)?;
                    for e in page.entries {
                        // The WIT segment model is MORE permissive than
                        // `VPath`: a name with `/` (or NUL, `.`/`..`) is
                        // valid as bytes but NOT as a `Segment`. It is
                        // SKIPPED with a warn — it has no representable path
                        // to live at (the same criterion as the archive
                        // providers, #93). Stage-2b debt: count it and
                        // expose it via `list_skipped`.
                        let name = e.name;
                        let Ok(seg) = Segment::new(name.clone()) else {
                            // Without the name and at `debug`, for the same
                            // reason as its `zip_cd` twin: it is a byte
                            // string chosen by the plugin, of a length it
                            // decides, and since roadmap item 9 the log
                            // persists to disk.
                            tracing::debug!("provider name not representable as VPath: skipped");
                            continue;
                        };
                        out.push(Entry {
                            attrs: std::collections::BTreeMap::new(),
                            path: dir.join(seg),
                            kind: map_kind(e.kind),
                            size: e.size,
                            mtime_ms: None,
                        });
                        // Anti-DoS cap: a hostile guest cannot reassemble
                        // forever.
                        if out.len() > MAX_LIST_ENTRIES {
                            return Err(Error::LimitExceeded {
                                limit: Error::LIMIT_ENTRIES.to_owned(),
                            });
                        }
                    }
                    match page.next_cursor {
                        Some(c) => cursor = Some(c),
                        None => break,
                    }
                }
                Ok(out)
            })
            .await?;
        Ok(stream::iter(entries.into_iter().map(Ok)).boxed())
    }

    async fn read(&self, p: &VPath, range: Option<ByteRange>) -> Result<ByteStream, Error> {
        let segs = Self::segments(p);
        let (offset, limit) = match range {
            None => (0u64, None),
            Some(r) => (r.offset, r.len),
        };
        // LAZY reassembly: every poll reads ONE bounded chunk from the guest
        // inside spawn_blocking — the whole file is never buffered (rule 2 +
        // a memory cap: a hostile guest cannot inflate the host's RAM, every
        // call is bounded by `want` and by the epoch deadline). The open
        // error (nonexistent file, dir) arrives as the stream's first item.
        // State: (instance, segments, offset, bytes read so far).
        let inst = Arc::clone(&self.inst);
        let dead = Arc::clone(&self.dead);
        let timeout = self.op_timeout;
        let stream = stream::try_unfold(
            (inst, dead, timeout, segs, offset, 0u64),
            move |(inst, dead, timeout, segs, off, done)| async move {
                const CHUNK: u64 = 64 * 1024;
                let want = match limit {
                    Some(l) => {
                        let remaining = l.saturating_sub(done);
                        if remaining == 0 {
                            return Ok(None);
                        }
                        remaining.min(CHUNK)
                    }
                    None => CHUNK,
                };
                let segs2 = segs.clone();
                // Same async lock + timeout + dead as `call` (M2): a hung
                // read leaks at most ONE thread, it does not park the ones
                // waiting.
                let chunk: Vec<u8> = run_guarded(&inst, &dead, timeout, move |g| {
                    g.read(&segs2, off, want)
                        .map_err(|e| map_runtime_error(&e))?
                        .map_err(map_vfs_error)
                })
                .await?;
                if chunk.is_empty() {
                    return Ok(None); // EOF
                }
                // A hostile guest that returns MORE than `want` gets
                // truncated: the requested range rules (the `ByteRange`
                // contract).
                let mut chunk = chunk;
                if chunk.len() as u64 > want {
                    chunk.truncate(usize::try_from(want).unwrap_or(usize::MAX));
                }
                let n = chunk.len() as u64;
                Ok(Some((
                    Bytes::from(chunk),
                    (inst, dead, timeout, segs, off.saturating_add(n), done + n),
                )))
            },
        );
        Ok(stream.boxed())
    }

    // ---- mutations: DELEGATED to the guest (#30 stage 2b-write). A
    // read-only guest responds Unsupported on each one and the adapter
    // propagates it; a writable one does the work. ----

    async fn write(&self, p: &VPath) -> Result<Box<dyn ByteSink>, Error> {
        let segs = Self::segments(p);
        // Opens the guest's transactional writer (its own staging; the
        // final path does not exist until commit — the ByteSink contract).
        let handle = self
            .call(move |g| {
                g.open_writer(&segs)
                    .map_err(|e| map_runtime_error(&e))?
                    .map_err(map_vfs_error)
            })
            .await?;
        Ok(Box::new(PluginByteSink {
            inst: Arc::clone(&self.inst),
            dead: Arc::clone(&self.dead),
            op_timeout: self.op_timeout,
            writer: Some(handle),
        }))
    }

    async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
        let segs = Self::segments(p);
        self.call(move |g| {
            g.make_dir(&segs)
                .map_err(|e| map_runtime_error(&e))?
                .map_err(map_vfs_error)
        })
        .await
    }

    async fn remove(&self, p: &VPath) -> Result<(), Error> {
        let segs = Self::segments(p);
        self.call(move |g| {
            g.remove(&segs)
                .map_err(|e| map_runtime_error(&e))?
                .map_err(map_vfs_error)
        })
        .await
    }

    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
        let src = Self::segments(from);
        let dst = Self::segments(to);
        self.call(move |g| {
            g.rename(&src, &dst)
                .map_err(|e| map_runtime_error(&e))?
                .map_err(map_vfs_error)
        })
        .await
    }

    // `trash`/`symlink` are not in the WIT `provider` interface (stage 2): a
    // guest does not offer them → Unsupported directly.
    async fn trash(
        &self,
        _p: &VPath,
        _id: &norte_vfs::trash::TrashId,
    ) -> Result<Option<VPath>, Error> {
        Err(Error::Unsupported)
    }

    async fn symlink(
        &self,
        _link: &VPath,
        _target: &[u8],
        _kind: SymlinkKind,
    ) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
}

/// `ByteSink` (#30 stage 2b-write) backed by a `writer` resource of the
/// guest: `write` appends a chunk, `commit`/`abort` publish or discard and
/// release the handle (unless the guest traps — the trap poisons the
/// instance, which is discarded). Dropping the sink without commit/abort
/// fires a best-effort `abort`+drop in [`Drop`] (the `ByteSink` contract;
/// synchronous, with `try_lock`).
struct PluginByteSink {
    inst: Arc<Mutex<ProviderInstance>>,
    /// Death flag shared with the `PluginProvider` (M2).
    dead: Arc<AtomicBool>,
    /// Per-op timeout (inherited from the provider).
    op_timeout: Duration,
    /// `Some` while the handle has not been released yet; `commit`/`abort`/`Drop`
    /// take it.
    writer: Option<norte_plugin_host::WriterHandle>,
}

impl Drop for PluginByteSink {
    fn drop(&mut self) {
        // Best-effort (the `ByteSink` contract): dropping without
        // commit/abort cleans up the guest's staging. Synchronous (the
        // guest calls are too), with `try_lock` — it does not block on the
        // mutex: if it were held, the handle is ceded to the store's
        // resource table until the provider dies.
        //
        // It is NOT async, so it CANNOT be wrapped in `tokio::time::timeout`
        // (M2): `writer_abort` emits a control `rm` over the wasip2 socket.
        // If the provider is already DEAD from a previous timeout, it is
        // skipped — the hung thread holds the state and cleaning up is
        // futile. A server that is ALIVE-but-stuck could still block this
        // `Drop` on the socket (the same non-cancelable-I/O reason as the
        // leak accepted in M2); residual debt.
        if self.dead.load(Ordering::Relaxed) {
            self.writer.take(); // release the handle without touching the socket
            return;
        }
        if let Some(w) = self.writer.take()
            && let Ok(mut g) = self.inst.try_lock()
        {
            let _ = g.writer_abort(w);
            let _ = g.writer_drop(w);
        }
    }
}

impl PluginByteSink {
    /// Runs an op on the writer under the lock + timeout (M2): delegates to
    /// [`run_guarded`], the same path as [`PluginProvider::call`].
    async fn call<F>(
        inst: &Arc<Mutex<ProviderInstance>>,
        dead: &Arc<AtomicBool>,
        op_timeout: Duration,
        f: F,
    ) -> Result<(), Error>
    where
        F: FnOnce(&mut ProviderInstance) -> Result<(), Error> + Send + 'static,
    {
        run_guarded(inst, dead, op_timeout, f).await
    }
}

#[async_trait::async_trait]
impl ByteSink for PluginByteSink {
    async fn write(&mut self, chunk: Bytes) -> Result<(), Error> {
        let Some(w) = self.writer else {
            return Err(Error::Internal { panic: false }); // used after being consumed
        };
        Self::call(&self.inst, &self.dead, self.op_timeout, move |g| {
            g.writer_write(w, &chunk)
                .map_err(|e| map_runtime_error(&e))?
                .map_err(map_vfs_error)
        })
        .await
    }

    async fn commit(mut self: Box<Self>) -> Result<(), Error> {
        let Some(w) = self.writer.take() else {
            return Err(Error::Internal { panic: false });
        };
        Self::call(&self.inst, &self.dead, self.op_timeout, move |g| {
            let r = g
                .writer_commit(w)
                .map_err(|e| map_runtime_error(&e))?
                .map_err(map_vfs_error);
            // The handle is released after a logical commit (OK or
            // VfsError); if the guest TRAPPED, `?` has already returned and
            // the poisoned instance is discarded.
            let _ = g.writer_drop(w);
            r
        })
        .await
    }

    async fn abort(mut self: Box<Self>) -> Result<(), Error> {
        let Some(w) = self.writer.take() else {
            return Err(Error::Internal { panic: false });
        };
        Self::call(&self.inst, &self.dead, self.op_timeout, move |g| {
            let r = g
                .writer_abort(w)
                .map_err(|e| map_runtime_error(&e))?
                .map_err(map_vfs_error);
            let _ = g.writer_drop(w);
            r
        })
        .await
    }
}
