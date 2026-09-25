//! Lifecycle of the Engine's remote sessions (#47, an evolution of phase 6e
//! / ADR 0015): a [`SessionPool`] owns the provider cache and adds
//! single-flight connects, drop-cancelable dials, reconnect backoff and
//! eviction of dead sessions (dragging along composed archive providers,
//! #62).
//!
//! Lifecycle contract (ADR 0029):
//! - **Lazy**: no background health-checks or TTL. A session is evicted
//!   when an operation returns [`Error::ProviderUnavailable`]; the next
//!   access reconnects.
//! - **Single-flight**: two concurrent requests for the same key wait on
//!   the SAME dial (one session per key, no duplicate handshakes).
//! - **Drop-cancelable**: waiters carry an RAII guard; when the last one is
//!   dropped mid-dial, the connect is cancelled (composes with
//!   `rpc.cancel`, #72 — dropping the dispatch drops the waiter).
//! - **Backoff**: a dial's `ProviderUnavailable` failure enters a
//!   negative-cache (1s, ×2, 30s cap); errors actionable BY the user (TOFU,
//!   auth) are NEVER cached.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::Duration;

use norte_proto::Error;
use norte_vfs::Provider;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// Cap for ESTABLISHING a remote connection (dial+TOFU+auth+subsystem).
/// Generous on purpose: covers slow networks without hanging indefinitely.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// The Engine's provider cache + the remote sessions' lifecycle. Cheap to
/// clone (internal Arc): connect jobs and eviction wrappers reference the
/// interior via `Weak`.
#[derive(Clone)]
pub(crate) struct SessionPool {
    inner: Arc<PoolInner>,
}

struct PoolInner {
    /// Key: the scheme (`"file"`, process providers), `scheme://authority`
    /// (remote sessions; there can be several alias keys for the SAME Arc,
    /// canonical dedup) or `fmt+scheme://authority` (composed archives).
    providers: RwLock<HashMap<String, Arc<dyn Provider>>>,
    /// Dials in flight, by cache key (single-flight).
    connecting: Mutex<HashMap<String, ConnectJob>>,
    /// Negative-cache of dial failures (`ProviderUnavailable` only).
    cooldown: Mutex<HashMap<String, Cooldown>>,
    /// Unique job ids (a job only cleans up ITS entry in `connecting`).
    next_job: AtomicU64,
}

/// Shared result of an in-flight dial.
type DialResult = Option<Result<Arc<dyn Provider>, Error>>;

struct ConnectJob {
    id: u64,
    rx: watch::Receiver<DialResult>,
    waiters: usize,
    cancel: CancellationToken,
    /// Alias keys to register alongside the canonical one when publishing
    /// (the spawner's plus those of waiters who JOINED the in-flight dial
    /// with another form of the same identity — rust MINOR-1 from review
    /// #47).
    aliases: Vec<String>,
}

struct Cooldown {
    until: tokio::time::Instant,
    next: Duration,
    last_err: Error,
    /// The WHY of the last failure (#322), so it can be repeated.
    ///
    /// Without this, the explanation disappeared exactly when someone
    /// looked for it: the human reads "the SSH agent couldn't authenticate",
    /// presses again inside the backoff window, and the second attempt is
    /// served from this cache with no flag — so it never goes through the
    /// observer and the answer is the bare category again.
    last_cause: Option<Box<crate::connect::Cause>>,
}

const BACKOFF_INITIAL: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// Cap on negative-cache entries (security MAJOR-3 from review #47): the
/// caller picks the keys (an arbitrary `scheme://authority`) — with no cap,
/// enumerating authorities inflates memory. Same order as the daemon's
/// anti-DoS caps (256 listings, 256 scopes).
const COOLDOWN_MAX: usize = 256;

impl SessionPool {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(PoolInner {
                providers: RwLock::new(HashMap::new()),
                connecting: Mutex::new(HashMap::new()),
                cooldown: Mutex::new(HashMap::new()),
                next_job: AtomicU64::new(1),
            }),
        }
    }

    /// Registers a process provider under its scheme (overwrites the
    /// previous one).
    pub(crate) fn register_process(&self, provider: Arc<dyn Provider>) {
        let scheme = provider.scheme().to_owned();
        self.inner
            .providers
            .write()
            .expect("providers lock sound")
            .insert(scheme, provider);
    }

    /// Closes the session cached under `key` and the composites hanging off
    /// it (#140). Returns `true` if there was something to close.
    ///
    /// Dropping the `Arc` IS closing: the remote provider closes its
    /// transport in its own `Drop`, so removing it from the map is enough
    /// —as long as nobody else is holding it, and an in-flight operation
    /// is: that one finishes with the connection it already had, and
    /// that's correct. What no longer happens is a NEW request reusing it.
    ///
    /// Archive composites (`fmt+key`) are swept along with it for the same
    /// reason as in an eviction: the archive wrapper caches the session's
    /// `Arc`, and leaving it alive would serve the index of a dead
    /// connection (#62).
    pub(crate) fn close(&self, key: &str) -> bool {
        let mut providers = self.inner.providers.write().expect("providers lock sound");
        let composites: Vec<String> = providers
            .keys()
            .filter(|ck| {
                ck.len() > key.len() + 1
                    && ck.ends_with(key)
                    && ck.as_bytes()[ck.len() - key.len() - 1] == b'+'
            })
            .cloned()
            .collect();
        let had = providers.remove(key).is_some();
        for ck in composites {
            providers.remove(&ck);
        }
        had
    }

    /// The provider cached under `key`, if any.
    pub(crate) fn lookup(&self, key: &str) -> Option<Arc<dyn Provider>> {
        self.inner
            .providers
            .read()
            .expect("providers lock sound")
            .get(key)
            .map(Arc::clone)
    }

    /// Inserts a composite (archive) provider with a double-check: if
    /// another request registered first, theirs wins (the extra is only
    /// RAM). The composite enters WRAPPED in its own [`SessionProvider`]:
    /// if an eviction's sweep skips it (compose-vs-evict race, security
    /// MAJOR-2), a zombie composite over a dead session self-evicts on its
    /// first failed operation — never immortal.
    pub(crate) fn insert_composite(
        &self,
        key: String,
        provider: Arc<dyn Provider>,
    ) -> Arc<dyn Provider> {
        let wrapped: Arc<dyn Provider> = Arc::new(SessionProvider {
            inner: provider,
            key: key.clone(),
            pool: Arc::downgrade(&self.inner),
        });
        let mut providers = self.inner.providers.write().expect("providers lock sound");
        let entry = providers.entry(key).or_insert(wrapped);
        Arc::clone(entry)
    }

    /// Registers `alias_key` → `canonical_key`'s CURRENT session and
    /// returns it; `None` if the canonical one is no longer there (evicted
    /// between the caller's lookup and here — security MAJOR-2: never
    /// re-insert a dead Arc the caller is holding from an old lookup).
    pub(crate) fn alias_current(
        &self,
        canonical_key: &str,
        alias_key: String,
    ) -> Option<Arc<dyn Provider>> {
        let mut providers = self.inner.providers.write().expect("providers lock sound");
        let current = Arc::clone(providers.get(canonical_key)?);
        providers
            .entry(alias_key)
            .or_insert_with(|| Arc::clone(&current));
        Some(current)
    }

    /// Establishes (or waits for) `cache_key`'s remote session — the heart
    /// of the lifecycle (#47).
    ///
    /// Single-flight: if a dial is already in flight for the key, this call
    /// SUBSCRIBES and waits for its result. If not, it spawns the dial job
    /// (timeout [`CONNECT_TIMEOUT`]). Dropping this function's future drops
    /// the waiter; when the last one falls, the dial is cancelled.
    ///
    /// `alias`: an extra key to register the SAME session under once
    /// connected (canonical dedup: the form the caller asked for).
    ///
    /// # Errors
    /// The dial's; a recent `ProviderUnavailable` failure is answered from
    /// the negative-cache without flagging again (backoff).
    pub(crate) async fn connect_remote(
        &self,
        cache_key: String,
        alias: Option<String>,
        scheme: &str,
        authority: &str,
        connector: Arc<dyn crate::connect::RemoteConnector>,
        observer: Option<Arc<dyn crate::connect::ConnectionObserver>>,
    ) -> Result<Arc<dyn Provider>, Error> {
        // Backoff: a recent transient failure answers from cache (never
        // actionable errors — TOFU/auth never enter cooldown). Along the
        // way it PRUNES dead entries (COOLDOWN_MAX cap, sec MAJOR-3) and
        // DECAYS the backoff of a key that's gone a long time without
        // failing (rust MINOR-2: two failures hours apart aren't
        // consecutive).
        {
            let now = tokio::time::Instant::now();
            let mut cooldown = self.inner.cooldown.lock().expect("cooldown lock sound");
            cooldown.retain(|_, c| now < c.until + BACKOFF_MAX);
            if let Some(c) = cooldown.get_mut(&cache_key) {
                if now < c.until {
                    let err = c.last_err.clone();
                    let cause = c.last_cause.clone();
                    // Outside the lock: the observer is foreign code, and
                    // this is the same rule as the normal path.
                    drop(cooldown);
                    count_the_failure(observer.as_ref(), scheme, authority, cause);
                    return Err(err);
                }
                if now > c.until + c.next {
                    c.next = BACKOFF_INITIAL;
                }
            }
        }
        let (mut rx, _guard) = {
            let mut connecting = self.inner.connecting.lock().expect("connecting lock sound");
            // Double-check under the lock (fixed connecting→providers
            // order): a job may have published between the caller's fast
            // path and here — without this, the second caller would flag
            // one extra handshake.
            if let Some(prov) = self
                .inner
                .providers
                .read()
                .expect("providers lock sound")
                .get(&cache_key)
            {
                return Ok(Arc::clone(prov));
            }
            if let Some(job) = connecting.get_mut(&cache_key) {
                job.waiters += 1;
                // The waiter that JOINS contributes its alias form: it's
                // registered when published (rust MINOR-1).
                if let Some(a) = alias
                    && a != cache_key
                    && !job.aliases.contains(&a)
                {
                    job.aliases.push(a);
                }
                (
                    job.rx.clone(),
                    WaiterGuard {
                        pool: Arc::clone(&self.inner),
                        key: cache_key.clone(),
                        id: job.id,
                    },
                )
            } else {
                let (tx, rx) = watch::channel(None);
                let cancel = CancellationToken::new();
                let id = self.inner.next_job.fetch_add(1, Ordering::Relaxed);
                connecting.insert(
                    cache_key.clone(),
                    ConnectJob {
                        id,
                        rx: rx.clone(),
                        waiters: 1,
                        cancel: cancel.clone(),
                        aliases: alias.into_iter().filter(|a| *a != cache_key).collect(),
                    },
                );
                spawn_dial_job(DialJob {
                    pool: Arc::downgrade(&self.inner),
                    id,
                    cache_key: cache_key.clone(),
                    scheme: scheme.to_owned(),
                    authority: authority.to_owned(),
                    connector,
                    observer,
                    cancel,
                    tx,
                });
                (
                    rx,
                    WaiterGuard {
                        pool: Arc::clone(&self.inner),
                        key: cache_key.clone(),
                        id,
                    },
                )
            }
        };
        loop {
            if let Some(result) = rx.borrow_and_update().clone() {
                return result;
            }
            if rx.changed().await.is_err() {
                // The job died without publishing. With the per-id guard
                // (BLOCKER from the review), a job with live waiters can no
                // longer be cancelled by a stale guard: if the tx dropped
                // with no publish while we had a live waiter, the job
                // REALLY panicked.
                return Err(Error::Internal { panic: true });
            }
        }
    }
}

/// RAII waiter for an in-flight dial: when the LAST one falls, it cancels
/// the job and cleans up the entry (atomic under `connecting`'s lock — a
/// new waiter never sees an already-cancelled job). Carries the `id` of the
/// job it subscribed to: a stale guard (its job already finished) NEVER
/// decrements waiters of a new job under the same key (BLOCKER from review
/// #47).
struct WaiterGuard {
    pool: Arc<PoolInner>,
    key: String,
    id: u64,
}

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        let mut connecting = self.pool.connecting.lock().expect("connecting lock sound");
        if let Some(job) = connecting.get_mut(&self.key)
            && job.id == self.id
        {
            job.waiters -= 1;
            if job.waiters == 0 {
                job.cancel.cancel();
                connecting.remove(&self.key);
            }
        }
    }
}

/// Everything the dial job needs (spawned: outlives the waiters).
struct DialJob {
    pool: Weak<PoolInner>,
    id: u64,
    cache_key: String,
    scheme: String,
    authority: String,
    connector: Arc<dyn crate::connect::RemoteConnector>,
    observer: Option<Arc<dyn crate::connect::ConnectionObserver>>,
    cancel: CancellationToken,
    tx: watch::Sender<DialResult>,
}

/// #322: counts WHY it couldn't connect, if that can be counted.
///
/// This is the point where the cause —known by whoever caught the
/// `ConnectError`— meets the destination —known by this job—, and until now
/// the why stayed in the daemon's log: the human read "permission denied"
/// and nothing else.
///
/// Called OUTSIDE the pool's locks, like #44's notices: the observer is
/// foreign code and no lock is held for it while it runs.
fn count_the_failure(
    observer: Option<&Arc<dyn crate::connect::ConnectionObserver>>,
    scheme: &str,
    authority: &str,
    cause: Option<Box<crate::connect::Cause>>,
) {
    let (Some(obs), Some(cause)) = (observer, cause) else {
        return;
    };
    obs.on_connection_failure(&crate::connect::ConnectionFailure {
        conn: cause.conn,
        scheme: scheme.to_owned(),
        // The authority WITHOUT userinfo (rule 10): whatever comes before
        // the LAST `@` is the user, and it doesn't go out. Same "the last
        // one" criterion `Authority::new` and `parse_endpoint` use, so
        // `a@b@c` gives `c` in all three places. The port DOES stay: this
        // is a diagnostic, and which port couldn't be reached is part of
        // the answer.
        host: authority.rsplit('@').next().unwrap_or(authority).to_owned(),
        reason: cause.reason,
        detail: cause.detail,
    });
}

fn spawn_dial_job(job: DialJob) {
    crate::blocking::spawn(async move {
        let dialed: Option<Result<crate::connect::Connected, crate::connect::DialError>> = tokio::select! {
            () = job.cancel.cancelled() => None,
            r = tokio::time::timeout(
                CONNECT_TIMEOUT,
                job.connector.connect(&job.scheme, &job.authority),
            ) => Some(r.unwrap_or_else(|_| {
                tracing::warn!(scheme = %job.scheme, "timeout establishing the remote connection");
                Err(Error::ProviderUnavailable { retryable: true }.into())
            })),
        };
        let Some(pool) = job.pool.upgrade() else {
            return; // the Engine died: nothing to register
        };
        match dialed {
            // Abandoned: the last WaiterGuard already cancelled AND cleaned
            // up the entry under the lock — nothing left to do here.
            None => {}
            Some(Ok(connected)) => {
                // The session enters the pool WRAPPED: it self-evicts when
                // an operation finds it dead (ProviderUnavailable).
                let provider: Arc<dyn Provider> = Arc::new(SessionProvider {
                    inner: connected.provider,
                    key: job.cache_key.clone(),
                    pool: job.pool.clone(),
                });
                // Publishes ONLY if this job is still current (sec
                // MINOR-3: an abandoned job whose select! took the connect
                // arm doesn't clobber a replacement job's session). The
                // pool's FIXED lock order: connecting → providers →
                // cooldown.
                let mut connecting = pool.connecting.lock().expect("connecting lock sound");
                let current = connecting
                    .get(&job.cache_key)
                    .is_some_and(|j| j.id == job.id);
                if !current {
                    // Replaced/abandoned: the freshly born session is
                    // dropped (Drop closes it) and nobody listens to tx.
                    return;
                }
                let aliases = connecting
                    .get(&job.cache_key)
                    .map(|j| j.aliases.clone())
                    .unwrap_or_default();
                {
                    let mut providers = pool.providers.write().expect("providers lock sound");
                    providers.insert(job.cache_key.clone(), Arc::clone(&provider));
                    for a in aliases {
                        providers.insert(a, Arc::clone(&provider));
                    }
                }
                pool.cooldown
                    .lock()
                    .expect("cooldown lock sound")
                    .remove(&job.cache_key);
                connecting.remove(&job.cache_key);
                drop(connecting);
                // #44: notices ONCE per published establishment (outside
                // the locks: the observer is foreign code).
                if let Some(obs) = &job.observer {
                    for w in &connected.warnings {
                        obs.on_connection_warning(w);
                    }
                }
                let _ = job.tx.send(Some(Ok(provider)));
            }
            Some(Err(dial)) => {
                let crate::connect::DialError { error: e, cause } = dial;
                let mut connecting = pool.connecting.lock().expect("connecting lock sound");
                let current = connecting
                    .get(&job.cache_key)
                    .is_some_and(|j| j.id == job.id);
                if current {
                    // The cooldown is set BEFORE dropping the entry (under
                    // the same lock): a re-dial cannot sneak into the
                    // window without seeing the negative-cache. Only a
                    // CURRENT job escalates the backoff (an abandoned job
                    // that dies late doesn't punish the user's next
                    // attempt).
                    let mut cooldown = pool.cooldown.lock().expect("cooldown lock sound");
                    if matches!(e, Error::ProviderUnavailable { .. }) {
                        let now = tokio::time::Instant::now();
                        if cooldown.len() >= COOLDOWN_MAX
                            && !cooldown.contains_key(&job.cache_key)
                            && let Some(oldest) = cooldown
                                .iter()
                                .min_by_key(|(_, c)| c.until)
                                .map(|(k, _)| k.clone())
                        {
                            // Cap: the oldest entry falls (sec MAJOR-3).
                            cooldown.remove(&oldest);
                        }
                        let entry = cooldown.entry(job.cache_key.clone()).or_insert(Cooldown {
                            until: now,
                            next: BACKOFF_INITIAL,
                            last_err: e.clone(),
                            last_cause: None,
                        });
                        entry.until = now + entry.next;
                        entry.next = (entry.next * 2).min(BACKOFF_MAX);
                        entry.last_err = e.clone();
                        // #322: and the why, so it can be repeated while
                        // this entry serves the error with no new flag.
                        entry.last_cause.clone_from(&cause);
                    } else {
                        // Actionable error (TOFU, auth, path): no cooldown
                        // — the user fixes it and retries instantly.
                        cooldown.remove(&job.cache_key);
                    }
                    drop(cooldown);
                    connecting.remove(&job.cache_key);
                }
                drop(connecting);
                // #322: the why gets COUNTED, and ONLY if this job was
                // still current — the same criterion as #44's notices
                // above. An abandoned job (the human left) or one replaced
                // by another has nobody waiting on its answer, and its
                // notice would show up in someone's bar who never asked
                // for anything.
                //
                // Outside the locks, also like #44: the observer is
                // foreign code and no lock is held for it while it runs.
                if current {
                    count_the_failure(job.observer.as_ref(), &job.scheme, &job.authority, cause);
                }
                let _ = job.tx.send(Some(Err(e)));
            }
        }
    });
}

impl PoolInner {
    /// Evicts EVERY entry whose Arc is exactly the wrapper requesting it
    /// (ptr-driven: a newer session under any key is never clobbered, and
    /// an orphaned alias whose canonical already fell is ALSO swept — sec
    /// MAJOR-2: without this, an alias re-inserted by a race stayed dead
    /// forever). Drags along composed archive providers over the swept
    /// keys AND over `key` (`{fmt}+{key}`, #62 — the archive wrapper caches
    /// the session's Arc: leaving it would serve an index from a dead
    /// connection).
    fn evict_session(&self, key: &str, wrapper_ptr: *const ()) {
        let mut providers = self.providers.write().expect("providers lock sound");
        let mut swept_keys: Vec<String> = providers
            .iter()
            .filter(|(_, v)| Arc::as_ptr(v).cast::<()>() == wrapper_ptr)
            .map(|(k, _)| k.clone())
            .collect();
        if swept_keys.is_empty() {
            // Stale eviction (a new session already lives under these
            // keys): no session sweep, but composites over `key` wrapping
            // THIS wrapper get cleaned up via their own wrapper
            // (insert_composite wraps — they self-evict on failure).
            return;
        }
        for k in &swept_keys {
            providers.remove(k);
        }
        // Base for the composite sweep: the swept keys + the wrapper's
        // canonical one (covers a swept alias whose canonical was already
        // gone).
        if !swept_keys.iter().any(|k| k == key) {
            swept_keys.push(key.to_owned());
        }
        let composite_keys: Vec<String> = providers
            .keys()
            .filter(|ck| {
                swept_keys.iter().any(|k| {
                    ck.len() > k.len() + 1 && ck.ends_with(k) && {
                        // exact `+{k}` suffix (the composite scheme is
                        // `fmt+scheme`): avoids false positives.
                        ck.as_bytes()[ck.len() - k.len() - 1] == b'+'
                    }
                })
            })
            .cloned()
            .collect();
        for ck in &composite_keys {
            providers.remove(ck);
        }
        tracing::warn!(
            key = %redact_key(key),
            swept = swept_keys.len(),
            composites = composite_keys.len(),
            "remote session evicted (ProviderUnavailable); the next access reconnects"
        );
    }
}

/// `scheme://user@host` → `scheme://host` for logs (rule 10: userinfo never
/// enters the log — same convention as `ConnectionWarning.host`). The cut
/// uses the LAST `@` (rule from #46: whatever's before is userinfo).
fn redact_key(key: &str) -> String {
    match key.split_once("://") {
        Some((scheme, auth)) => match auth.rfind('@') {
            Some(i) => format!("{scheme}://{}", &auth[i + 1..]),
            None => key.to_owned(),
        },
        None => key.to_owned(),
    }
}

/// Wraps the cached remote session: if an operation returns
/// [`Error::ProviderUnavailable`], it self-evicts from the pool (the
/// session is dead; the next access reconnects). The error propagates AS
/// IS.
///
/// v1: only the `Result`s of CALLS evict — an error inside an already
/// delivered stream (`list`/`read`) or sink doesn't reach here (the next
/// direct call on the dead session does evict).
struct SessionProvider {
    inner: Arc<dyn Provider>,
    /// The CANONICAL key it lives under in the pool.
    key: String,
    pool: Weak<PoolInner>,
}

impl SessionProvider {
    fn observe<T>(&self, r: Result<T, Error>) -> Result<T, Error> {
        if let Err(Error::ProviderUnavailable { .. }) = &r
            && let Some(pool) = self.pool.upgrade()
        {
            pool.evict_session(&self.key, std::ptr::from_ref(self).cast::<()>());
        }
        r
    }
}

#[async_trait::async_trait]
impl Provider for SessionProvider {
    fn scheme(&self) -> &str {
        self.inner.scheme()
    }
    fn capabilities(&self) -> norte_proto::Capabilities {
        self.inner.capabilities()
    }
    async fn capabilities_at(
        &self,
        p: &norte_proto::VPath,
    ) -> Result<norte_proto::Capabilities, Error> {
        self.observe(self.inner.capabilities_at(p).await)
    }
    fn attrs(&self) -> &[norte_proto::AttrInfo] {
        self.inner.attrs()
    }
    async fn open_root(
        &self,
        root: &norte_proto::VPath,
    ) -> Result<Box<dyn norte_vfs::ConfinedRoot>, Error> {
        // `Unsupported` does NOT evict: it's the honest answer of a backend
        // that doesn't know how to confine (sftp, object, archive), not the
        // symptom of a dead session. Any other error does, like the rest of
        // the wrapper.
        match self.inner.open_root(root).await {
            Err(Error::Unsupported) => Err(Error::Unsupported),
            other => self.observe(other),
        }
    }
    async fn stat(&self, p: &norte_proto::VPath) -> Result<norte_proto::Entry, Error> {
        self.observe(self.inner.stat(p).await)
    }
    async fn stat_with(
        &self,
        p: &norte_proto::VPath,
        opt: &norte_vfs::ListOptions,
    ) -> Result<norte_proto::Entry, Error> {
        self.observe(self.inner.stat_with(p, opt).await)
    }
    async fn list(&self, p: &norte_proto::VPath) -> Result<norte_vfs::EntryStream, Error> {
        self.observe(self.inner.list(p).await)
    }
    async fn list_with(
        &self,
        p: &norte_proto::VPath,
        opt: &norte_vfs::ListOptions,
    ) -> Result<norte_vfs::EntryStream, Error> {
        self.observe(self.inner.list_with(p, opt).await)
    }
    async fn list_skipped(&self, p: &norte_proto::VPath) -> Result<Option<u64>, Error> {
        self.observe(self.inner.list_skipped(p).await)
    }
    async fn read(
        &self,
        p: &norte_proto::VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, Error> {
        self.observe(self.inner.read(p, range).await)
    }
    async fn node_id(
        &self,
        p: &norte_proto::VPath,
        follow: norte_vfs::FollowLinks,
    ) -> Result<Option<norte_vfs::NodeId>, Error> {
        self.observe(self.inner.node_id(p, follow).await)
    }
    async fn read_link(&self, p: &norte_proto::VPath) -> Result<Vec<u8>, Error> {
        self.observe(self.inner.read_link(p).await)
    }
    async fn trash(
        &self,
        p: &norte_proto::VPath,
        id: &norte_vfs::trash::TrashId,
    ) -> Result<Option<norte_proto::VPath>, Error> {
        self.observe(self.inner.trash(p, id).await)
    }
    fn trash_restorable(&self) -> bool {
        // Property of the wrapped provider, no I/O to observe.
        self.inner.trash_restorable()
    }
    async fn restore_from(
        &self,
        dest: &norte_proto::VPath,
        original: &norte_proto::VPath,
    ) -> Result<(), Error> {
        self.observe(self.inner.restore_from(dest, original).await)
    }
    async fn gc_partials(
        &self,
        dir: &norte_proto::VPath,
        older_than: Duration,
    ) -> Result<usize, Error> {
        self.observe(self.inner.gc_partials(dir, older_than).await)
    }
    async fn restore_trashed(&self, original: &norte_proto::VPath) -> Result<(), Error> {
        self.observe(self.inner.restore_trashed(original).await)
    }
    async fn symlink(
        &self,
        link: &norte_proto::VPath,
        target: &[u8],
        kind: norte_vfs::SymlinkKind,
    ) -> Result<(), Error> {
        self.observe(self.inner.symlink(link, target, kind).await)
    }
    async fn write(&self, p: &norte_proto::VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
        self.observe(self.inner.write(p).await)
    }
    async fn open_resumable(
        &self,
        p: &norte_proto::VPath,
    ) -> Result<(Box<dyn norte_vfs::ByteSink>, u64), Error> {
        self.observe(self.inner.open_resumable(p).await)
    }
    async fn partial_digest(
        &self,
        p: &norte_proto::VPath,
        len: u64,
    ) -> Result<Option<[u8; 32]>, Error> {
        self.observe(self.inner.partial_digest(p, len).await)
    }
    async fn mkdir(&self, p: &norte_proto::VPath) -> Result<(), Error> {
        self.observe(self.inner.mkdir(p).await)
    }
    async fn remove(&self, p: &norte_proto::VPath) -> Result<(), Error> {
        self.observe(self.inner.remove(p).await)
    }
    async fn rename(
        &self,
        from: &norte_proto::VPath,
        to: &norte_proto::VPath,
    ) -> Result<(), Error> {
        self.observe(self.inner.rename(from, to).await)
    }
    async fn copy_native(
        &self,
        from: &norte_proto::VPath,
        to: &norte_proto::VPath,
    ) -> Option<Result<(), Error>> {
        self.inner
            .copy_native(from, to)
            .await
            .map(|r| self.observe(r))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::VPath;

    fn pu() -> Error {
        Error::ProviderUnavailable { retryable: true }
    }

    /// A provider whose EVERYTHING returns `ProviderUnavailable` (dead
    /// session): pins that the wrapper delegates and evicts on EVERY trait
    /// method.
    struct AllPu;

    #[async_trait::async_trait]
    impl Provider for AllPu {
        // The signature is fixed by the trait (&self → &str): the literal
        // is the test's.
        #[expect(
            clippy::unnecessary_literal_bound,
            reason = "The signature is fixed by the trait (&self → &str): the literal is the test's"
        )]
        fn scheme(&self) -> &str {
            "sftp"
        }
        fn capabilities(&self) -> norte_proto::Capabilities {
            norte_proto::Capabilities {
                flags: norte_proto::CapabilityFlags::empty(),
                max_path: None,
            }
        }
        async fn capabilities_at(&self, _p: &VPath) -> Result<norte_proto::Capabilities, Error> {
            Err(pu())
        }
        async fn open_root(
            &self,
            _root: &VPath,
        ) -> Result<Box<dyn norte_vfs::ConfinedRoot>, Error> {
            Err(pu())
        }
        fn attrs(&self) -> &[norte_proto::AttrInfo] {
            static ONE: std::sync::LazyLock<Vec<norte_proto::AttrInfo>> =
                std::sync::LazyLock::new(|| {
                    vec![norte_proto::AttrInfo {
                        id: "allpu.x".into(),
                        label: "x".into(),
                        ty: norte_proto::AttrType::Bool,
                        hint: norte_proto::AttrHint::Opaque,
                    }]
                });
            &ONE
        }
        // `true` so the trait's default (`false`) CANNOT pass the
        // delegation assertion.
        fn trash_restorable(&self) -> bool {
            true
        }
        async fn stat(&self, _p: &VPath) -> Result<norte_proto::Entry, Error> {
            Err(pu())
        }
        async fn stat_with(
            &self,
            _p: &VPath,
            _opt: &norte_vfs::ListOptions,
        ) -> Result<norte_proto::Entry, Error> {
            Err(pu())
        }
        async fn list(&self, _p: &VPath) -> Result<norte_vfs::EntryStream, Error> {
            Err(pu())
        }
        async fn list_with(
            &self,
            _p: &VPath,
            _opt: &norte_vfs::ListOptions,
        ) -> Result<norte_vfs::EntryStream, Error> {
            Err(pu())
        }
        async fn list_skipped(&self, _p: &VPath) -> Result<Option<u64>, Error> {
            Err(pu())
        }
        async fn read(
            &self,
            _p: &VPath,
            _r: Option<norte_proto::ByteRange>,
        ) -> Result<norte_vfs::ByteStream, Error> {
            Err(pu())
        }
        async fn node_id(
            &self,
            _p: &VPath,
            _f: norte_vfs::FollowLinks,
        ) -> Result<Option<norte_vfs::NodeId>, Error> {
            Err(pu())
        }
        async fn read_link(&self, _p: &VPath) -> Result<Vec<u8>, Error> {
            Err(pu())
        }
        async fn trash(
            &self,
            _p: &VPath,
            _id: &norte_vfs::trash::TrashId,
        ) -> Result<Option<VPath>, Error> {
            Err(pu())
        }
        async fn gc_partials(&self, _d: &VPath, _o: Duration) -> Result<usize, Error> {
            Err(pu())
        }
        async fn restore_trashed(&self, _o: &VPath) -> Result<(), Error> {
            Err(pu())
        }
        async fn symlink(
            &self,
            _l: &VPath,
            _t: &[u8],
            _k: norte_vfs::SymlinkKind,
        ) -> Result<(), Error> {
            Err(pu())
        }
        async fn write(&self, _p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
            Err(pu())
        }
        async fn open_resumable(
            &self,
            _p: &VPath,
        ) -> Result<(Box<dyn norte_vfs::ByteSink>, u64), Error> {
            Err(pu())
        }
        async fn partial_digest(&self, _p: &VPath, _l: u64) -> Result<Option<[u8; 32]>, Error> {
            Err(pu())
        }
        async fn mkdir(&self, _p: &VPath) -> Result<(), Error> {
            Err(pu())
        }
        async fn remove(&self, _p: &VPath) -> Result<(), Error> {
            Err(pu())
        }
        async fn rename(&self, _f: &VPath, _t: &VPath) -> Result<(), Error> {
            Err(pu())
        }
        async fn copy_native(&self, _f: &VPath, _t: &VPath) -> Option<Result<(), Error>> {
            Some(Err(pu()))
        }
    }

    fn wrapper(pool: &SessionPool, key: &str) -> Arc<dyn Provider> {
        Arc::new(SessionProvider {
            inner: Arc::new(AllPu),
            key: key.to_owned(),
            pool: Arc::downgrade(&pool.inner),
        })
    }

    fn insert(pool: &SessionPool, key: &str, w: &Arc<dyn Provider>) {
        pool.inner
            .providers
            .write()
            .expect("test lock")
            .insert(key.to_owned(), Arc::clone(w));
    }

    fn contains(pool: &SessionPool, key: &str) -> bool {
        pool.inner
            .providers
            .read()
            .expect("test lock")
            .contains_key(key)
    }

    /// sec MAJOR-2 (review #47): an ORPHANED alias (its canonical already
    /// fell out of the map) also gets swept — the eviction is ptr-driven,
    /// with no per-key gate — and drags along the composites hanging off
    /// that form (#62).
    #[tokio::test]
    async fn an_orphaned_alias_is_swept_ptr_driven() {
        let pool = SessionPool::new();
        let w = wrapper(&pool, "sftp://oscar@h");
        // Lives ONLY under the alias form; the canonical isn't in the map.
        insert(&pool, "sftp://h", &w);
        let comp: Arc<dyn Provider> = Arc::new(AllPu);
        insert(&pool, "zip+sftp://h", &comp);

        let _ = w.stat(&VPath::parse("sftp://h/x").expect("wire")).await;
        assert!(!contains(&pool, "sftp://h"), "alias swept");
        assert!(!contains(&pool, "zip+sftp://h"), "composite dragged along");
    }

    /// `alias_current` with the canonical missing does NOT re-insert
    /// anything (a dead Arc the caller is holding from an old lookup never
    /// comes back to the map).
    #[test]
    fn alias_current_with_no_canonical_is_none() {
        let pool = SessionPool::new();
        assert!(
            pool.alias_current("sftp://oscar@h", "sftp://h".to_owned())
                .is_none()
        );
        assert!(!contains(&pool, "sftp://h"));
    }

    /// A STALE eviction (another Arc already lives under the key) touches
    /// nothing.
    #[tokio::test]
    async fn a_stale_eviction_does_not_clobber_the_new_session() {
        let pool = SessionPool::new();
        let old = wrapper(&pool, "sftp://h");
        let new_session = wrapper(&pool, "sftp://h");
        insert(&pool, "sftp://h", &new_session);
        // The OLD one (already out of the map) fails and requests eviction:
        // it doesn't sweep.
        let _ = old.stat(&VPath::parse("sftp://h/x").expect("wire")).await;
        assert!(contains(&pool, "sftp://h"), "the new session survives");
    }

    /// rust MINOR-3 (review #47): the ENTIRE trait surface delegates to the
    /// interior and OBSERVES the error — every method over a dead session
    /// evicts the key. If a new trait method isn't delegated, this test is
    /// the reminder (alongside the comment on the trait).
    #[tokio::test]
    async fn the_wrapper_observes_every_method() {
        let pool = SessionPool::new();
        let key = "sftp://h";
        let p = VPath::parse("sftp://h/x").expect("wire");
        let w = wrapper(&pool, key);

        macro_rules! evicts {
            ($call:expr) => {{
                insert(&pool, key, &w);
                let _ = $call;
                assert!(!contains(&pool, key), stringify!($call));
            }};
        }

        evicts!(w.capabilities_at(&p).await);
        evicts!(w.open_root(&p).await);
        evicts!(w.stat(&p).await);
        evicts!(w.stat_with(&p, &norte_vfs::ListOptions::default()).await);
        evicts!(w.list(&p).await);
        evicts!(w.list_with(&p, &norte_vfs::ListOptions::default()).await);
        evicts!(w.list_skipped(&p).await);
        evicts!(w.read(&p, None).await);
        evicts!(w.node_id(&p, norte_vfs::FollowLinks::No).await);
        evicts!(w.read_link(&p).await);
        evicts!(w.trash(&p, &norte_vfs::trash::TrashId::new(0, 0)).await);
        evicts!(w.gc_partials(&p, Duration::from_secs(1)).await);
        evicts!(w.restore_trashed(&p).await);
        evicts!(w.symlink(&p, b"t", norte_vfs::SymlinkKind::File).await);
        evicts!(w.write(&p).await);
        evicts!(w.open_resumable(&p).await);
        evicts!(w.partial_digest(&p, 0).await);
        evicts!(w.mkdir(&p).await);
        evicts!(w.remove(&p).await);
        evicts!(w.rename(&p, &p).await);
        evicts!(w.restore_from(&p, &p).await);
        evicts!(w.copy_native(&p, &p).await);

        // Methods with no `Result` (they don't evict): pinned pass-through
        // — the gap this test used to have with `capabilities` doesn't
        // repeat with `attrs`.
        assert_eq!(w.attrs().len(), 1, "attrs() delegates to the interior");
        assert!(
            w.trash_restorable(),
            "trash_restorable() delegates to the interior"
        );
    }
}
