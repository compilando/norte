//! Phase 6e (ADR 0015 A/G): the Engine resolves REMOTE providers on demand
//! via a [`RemoteConnector`] — cached by `scheme://authority`, with the TOFU
//! flow (`HostKeyUnknown` → `trust_host_key`) going through the connector.
//! `connections.toml`'s matching logic is tested separately (a module unit
//! test); here goes the Engine↔connector contract with a FAKE connector.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use norte_core::Engine;
use norte_core::connect::{
    Connected, ConnectionObserver, ConnectionWarning, ConnectionWarningReason, DialError,
    RemoteConnector,
};
use norte_proto::{Entry, EntryKind, Error, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

/// A trivial provider that answers ANY scheme (the connection's fake).
struct EchoProvider;

#[async_trait]
impl Provider for EchoProvider {
    fn scheme(&self) -> &'static str {
        "sftp"
    }
    fn capabilities(&self) -> norte_proto::Capabilities {
        norte_proto::Capabilities {
            flags: norte_proto::CapabilityFlags::empty(),
            max_path: None,
        }
    }
    async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        Ok(Entry {
            attrs: std::collections::BTreeMap::new(),
            path: p.clone(),
            kind: EntryKind::Dir,
            size: None,
            mtime_ms: None,
        })
    }
    async fn list(&self, _p: &VPath) -> Result<norte_vfs::EntryStream, Error> {
        Err(Error::Unsupported)
    }
    async fn read(
        &self,
        _p: &VPath,
        _range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, Error> {
        Err(Error::Unsupported)
    }
    async fn write(&self, _p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
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

/// A fake connector: counts calls and can fail with `HostKeyUnknown` until
/// the key is trusted.
struct FakeConnector {
    connects: AtomicUsize,
    trusts: AtomicUsize,
    /// `true` = the first connect returns `HostKeyUnknown` until trust.
    tofu: std::sync::Mutex<bool>,
    /// Warnings the connect returns when establishing (#44: empty default).
    warn_on_connect: Vec<ConnectionWarning>,
    /// The last thing that arrived through `provide_secret` (#325).
    secret_given: std::sync::Mutex<Option<String>>,
}

impl FakeConnector {
    fn new(tofu: bool) -> Self {
        Self {
            connects: AtomicUsize::new(0),
            trusts: AtomicUsize::new(0),
            tofu: std::sync::Mutex::new(tofu),
            warn_on_connect: Vec::new(),
            secret_given: std::sync::Mutex::new(None),
        }
    }

    /// Like [`Self::new`] but every connect returns these warnings (#44).
    fn with_warnings(warnings: Vec<ConnectionWarning>) -> Self {
        Self {
            warn_on_connect: warnings,
            ..Self::new(false)
        }
    }
}

#[async_trait]
impl RemoteConnector for FakeConnector {
    async fn connect(&self, scheme: &str, authority: &str) -> Result<Connected, DialError> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        assert_eq!(scheme, "sftp");
        if *self.tofu.lock().unwrap() {
            return Err(DialError::from(Error::HostKeyUnknown {
                host: authority.to_string(),
                port: Some(22),
                algo: "ssh-ed25519".into(),
                fingerprint: "SHA256:xyz".into(),
            }));
        }
        Ok(Connected {
            provider: Arc::new(EchoProvider),
            warnings: self.warn_on_connect.clone(),
        })
    }

    async fn trust_host_key(
        &self,
        _host: &str,
        _port: Option<u16>,
        _fingerprint: &str,
    ) -> Result<(), Error> {
        self.trusts.fetch_add(1, Ordering::SeqCst);
        *self.tofu.lock().unwrap() = false;
        Ok(())
    }

    async fn provide_secret(&self, _conn: &str, secret: &str) -> Result<(), Error> {
        *self.secret_given.lock().unwrap() = Some(secret.to_string());
        Ok(())
    }
}

fn vp(s: &str) -> VPath {
    VPath::parse(s).expect("valid wire")
}

/// The remote provider is established ONCE and cached by authority: two stats
/// to the same host = one connect; another host = another connect.
#[tokio::test]
async fn it_connects_on_demand_and_caches_by_authority() {
    let engine = Engine::new();
    let conn = Arc::new(FakeConnector::new(false));
    engine.set_connector(conn.clone());

    engine
        .stat(&vp("sftp://a.example/x"))
        .await
        .expect("stat 1");
    engine
        .stat(&vp("sftp://a.example/y"))
        .await
        .expect("stat 2");
    assert_eq!(conn.connects.load(Ordering::SeqCst), 1, "cached");

    engine
        .stat(&vp("sftp://b.example/x"))
        .await
        .expect("stat 3");
    assert_eq!(conn.connects.load(Ordering::SeqCst), 2, "another authority");
}

/// Without a connector, a scheme with no provider is still Unsupported (M0/M1).
#[tokio::test]
async fn without_a_connector_it_is_still_unsupported() {
    let engine = Engine::new();
    let err = engine.stat(&vp("sftp://h/x")).await.unwrap_err();
    assert!(matches!(err, Error::Unsupported), "was {err:?}");
}

/// A provider registered by scheme (e.g. mem in tests, local file) takes
/// priority and does NOT trigger the connector.
#[tokio::test]
async fn a_registered_provider_does_not_trigger_the_connector() {
    let engine = Engine::new();
    let conn = Arc::new(FakeConnector::new(false));
    engine.set_connector(conn.clone());
    let mem = MemProvider::new();
    mem.mkdir(&vp("mem://x/d")).await.expect("mkdir");
    engine.register_provider(Arc::new(mem));

    engine.stat(&vp("mem://x/d")).await.expect("stat mem");
    assert_eq!(conn.connects.load(Ordering::SeqCst), 0);
}

/// Full TOFU flow through the Engine: first contact →
/// `HostKeyUnknown` (with fingerprint), `trust_host_key`, retry connects.
#[tokio::test]
async fn tofu_error_trust_and_retry() {
    let engine = Engine::new();
    let conn = Arc::new(FakeConnector::new(true));
    engine.set_connector(conn.clone());

    let err = engine.stat(&vp("sftp://h/x")).await.unwrap_err();
    let Error::HostKeyUnknown { fingerprint, .. } = &err else {
        panic!("expected HostKeyUnknown, was {err:?}");
    };
    assert_eq!(fingerprint, "SHA256:xyz");

    engine
        .trust_host_key("h", Some(22), "SHA256:xyz")
        .await
        .expect("trust");
    assert_eq!(conn.trusts.load(Ordering::SeqCst), 1);

    // Retry: now it connects (and a previous failure was NOT cached).
    engine
        .stat(&vp("sftp://h/x"))
        .await
        .expect("stat after trust");
}

/// #325: `provide_secret` reaches the connector with the secret as is, and
/// without a connector it is `Unsupported` instead of a panic (same as
/// `trust_host_key`).
#[tokio::test]
async fn provide_secret_reaches_the_connector() {
    let engine = Engine::new();
    assert!(matches!(
        engine.provide_secret("rosetta", "s3cr3t").await,
        Err(Error::Unsupported)
    ));

    let conn = Arc::new(FakeConnector::new(false));
    engine.set_connector(conn.clone());
    engine
        .provide_secret("rosetta", "s3cr3t")
        .await
        .expect("provide_secret");
    assert_eq!(
        conn.secret_given.lock().unwrap().as_deref(),
        Some("s3cr3t"),
        "the secret arrives intact: trimming or normalizing it is changing the password"
    );
}

/// A connector that never resolves: simulates a server that accepts TCP and
/// stays silent.
struct HangingConnector;

#[async_trait]
impl RemoteConnector for HangingConnector {
    async fn connect(&self, _s: &str, _a: &str) -> Result<Connected, DialError> {
        std::future::pending().await
    }
    async fn trust_host_key(&self, _h: &str, _p: Option<u16>, _f: &str) -> Result<(), Error> {
        std::future::pending().await
    }
    async fn provide_secret(&self, _c: &str, _s: &str) -> Result<(), Error> {
        Ok(())
    }
}

/// Establishing a connection has a TIMEOUT: a host that accepts TCP and stays
/// silent does not hang the operation forever (the connect happens BEFORE
/// there is a cancelable Task — rule 3 requires it not be indefinite).
#[tokio::test(start_paused = true)]
async fn a_hanging_connect_times_out() {
    let engine = Engine::new();
    engine.set_connector(Arc::new(HangingConnector));
    let err = engine.stat(&vp("sftp://h/x")).await.unwrap_err();
    assert!(
        matches!(err, Error::ProviderUnavailable { retryable: true }),
        "was {err:?}"
    );
}

/// Two concurrent requests to the same host: the SECOND one to register does
/// not overwrite the first (double-check under the write lock) — no two
/// indistinguishable live sessions are left.
#[tokio::test]
async fn concurrent_connect_does_not_duplicate_registration() {
    let engine = Arc::new(Engine::new());
    let conn = Arc::new(FakeConnector::new(false));
    engine.set_connector(conn.clone());
    let (px, py) = (vp("sftp://h/x"), vp("sftp://h/y"));
    let (a, b) = tokio::join!(engine.stat(&px), engine.stat(&py));
    a.expect("stat a");
    b.expect("stat b");
    // 1 or 2 connects may have fired (a race), but a third access ALWAYS
    // reuses the registered one (it does not reconnect).
    let before = conn.connects.load(Ordering::SeqCst);
    engine.stat(&vp("sftp://h/z")).await.expect("stat c");
    assert_eq!(conn.connects.load(Ordering::SeqCst), before);
}

/// A gated connector: `connect` counts the call and waits for the test to
/// open the gate — lets there be TWO pending waiters on the same dial.
struct GatedConnector {
    connects: AtomicUsize,
    gate: tokio::sync::Semaphore,
}

impl GatedConnector {
    fn new() -> Self {
        Self {
            connects: AtomicUsize::new(0),
            gate: tokio::sync::Semaphore::new(0),
        }
    }
}

#[async_trait]
impl RemoteConnector for GatedConnector {
    async fn connect(&self, _s: &str, _a: &str) -> Result<Connected, DialError> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        let _permit = self.gate.acquire().await.expect("gate alive");
        Ok(Connected {
            provider: Arc::new(EchoProvider),
            warnings: Vec::new(),
        })
    }
    async fn trust_host_key(&self, _h: &str, _p: Option<u16>, _f: &str) -> Result<(), Error> {
        Ok(())
    }
    async fn provide_secret(&self, _c: &str, _s: &str) -> Result<(), Error> {
        Ok(())
    }
}

/// #47: two concurrent requests to the same host wait for the SAME dial —
/// exactly ONE connect, never a transient duplicated session.
#[tokio::test]
async fn concurrent_connect_is_single_flight() {
    let engine = Arc::new(Engine::new());
    let conn = Arc::new(GatedConnector::new());
    engine.set_connector(conn.clone());

    let e1 = Arc::clone(&engine);
    let t1 = tokio::spawn(async move { e1.stat(&vp("sftp://h/x")).await });
    let e2 = Arc::clone(&engine);
    let t2 = tokio::spawn(async move { e2.stat(&vp("sftp://h/y")).await });
    // Wait for the dial to have started (both stats already in flight).
    while conn.connects.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }
    tokio::task::yield_now().await;
    conn.gate.add_permits(2);
    t1.await.expect("join").expect("stat 1");
    t2.await.expect("join").expect("stat 2");
    assert_eq!(conn.connects.load(Ordering::SeqCst), 1, "a single dial");
}

/// Marks `cancelled` when the connect's future is DROPPED halfway.
struct DropProbe(Arc<std::sync::atomic::AtomicBool>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A hanging connector that detects cancellation (drop of the in-flight future).
struct ProbedHangingConnector {
    started: AtomicUsize,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl RemoteConnector for ProbedHangingConnector {
    async fn connect(&self, _s: &str, _a: &str) -> Result<Connected, DialError> {
        self.started.fetch_add(1, Ordering::SeqCst);
        let _probe = DropProbe(self.cancelled.clone());
        std::future::pending().await
    }
    async fn trust_host_key(&self, _h: &str, _p: Option<u16>, _f: &str) -> Result<(), Error> {
        Ok(())
    }
    async fn provide_secret(&self, _c: &str, _s: &str) -> Result<(), Error> {
        Ok(())
    }
}

/// #47: if ALL waiters abandon (the future is dropped — e.g. `rpc.cancel`
/// drops the dispatch, #72), the in-flight dial is CANCELLED and the key ends
/// up clean: a later access marks again.
#[tokio::test]
async fn all_waiters_abandoning_cancels_the_dial() {
    let engine = Arc::new(Engine::new());
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let conn = Arc::new(ProbedHangingConnector {
        started: AtomicUsize::new(0),
        cancelled: cancelled.clone(),
    });
    engine.set_connector(conn.clone());

    let e1 = Arc::clone(&engine);
    let waiter = tokio::spawn(async move { e1.stat(&vp("sftp://h/x")).await });
    while conn.started.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }
    waiter.abort();
    let _ = waiter.await;
    // The job processes the cancellation in its own task: give it turns.
    for _ in 0..50 {
        if cancelled.load(Ordering::SeqCst) {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(cancelled.load(Ordering::SeqCst), "the dial was cancelled");

    // The key ended up clean: the next access marks again (it does not wait
    // on a zombie job).
    let e2 = Arc::clone(&engine);
    let again = tokio::spawn(async move { e2.stat(&vp("sftp://h/x")).await });
    for _ in 0..500 {
        if conn.started.load(Ordering::SeqCst) >= 2 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        conn.started.load(Ordering::SeqCst),
        2,
        "marks again after cleaning up"
    );
    again.abort();
    let _ = again.await;
}

/// A programmable connector: fails with `ProviderUnavailable` while
/// `failures` > 0, then connects.
struct FlakyConnector {
    connects: AtomicUsize,
    failures: AtomicUsize,
}

#[async_trait]
impl RemoteConnector for FlakyConnector {
    async fn connect(&self, _s: &str, _a: &str) -> Result<Connected, DialError> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        if self
            .failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |f| f.checked_sub(1))
            .is_ok()
        {
            return Err(Error::ProviderUnavailable { retryable: true }.into());
        }
        Ok(Connected {
            provider: Arc::new(EchoProvider),
            warnings: Vec::new(),
        })
    }
    async fn trust_host_key(&self, _h: &str, _p: Option<u16>, _f: &str) -> Result<(), Error> {
        Ok(())
    }
    async fn provide_secret(&self, _c: &str, _s: &str) -> Result<(), Error> {
        Ok(())
    }
}

/// #47: a transient dial failure enters negative-cache with exponential
/// backoff (1s → 2s → … capped at 30s): retrying inside the window answers the
/// cached error WITHOUT marking again.
#[tokio::test(start_paused = true)]
async fn a_transient_failure_enters_cooldown_with_backoff() {
    let engine = Engine::new();
    let conn = Arc::new(FlakyConnector {
        connects: AtomicUsize::new(0),
        failures: AtomicUsize::new(usize::MAX), // always fails
    });
    engine.set_connector(conn.clone());
    let p = vp("sftp://h/x");

    let err = engine.stat(&p).await.unwrap_err();
    assert!(matches!(
        err,
        Error::ProviderUnavailable { retryable: true }
    ));
    assert_eq!(conn.connects.load(Ordering::SeqCst), 1);

    // Inside the window (1s): cached, no dial.
    let err = engine.stat(&p).await.unwrap_err();
    assert!(matches!(
        err,
        Error::ProviderUnavailable { retryable: true }
    ));
    assert_eq!(conn.connects.load(Ordering::SeqCst), 1, "in cooldown");

    // Past the window: marks again (and fails again → 2s window).
    tokio::time::advance(std::time::Duration::from_millis(1100)).await;
    let _ = engine.stat(&p).await.unwrap_err();
    assert_eq!(conn.connects.load(Ordering::SeqCst), 2);

    // 1s later: the window is now 2s — still cached.
    tokio::time::advance(std::time::Duration::from_millis(1100)).await;
    let _ = engine.stat(&p).await.unwrap_err();
    assert_eq!(conn.connects.load(Ordering::SeqCst), 2, "doubled window");

    // One more second: it expires and marks again.
    tokio::time::advance(std::time::Duration::from_millis(1100)).await;
    let _ = engine.stat(&p).await.unwrap_err();
    assert_eq!(conn.connects.load(Ordering::SeqCst), 3);
}

/// #47: a connect that finally succeeds CLEARS the key's cooldown; the
/// session ends up cached (later accesses do not mark).
#[tokio::test(start_paused = true)]
async fn success_clears_the_cooldown() {
    let engine = Engine::new();
    let conn = Arc::new(FlakyConnector {
        connects: AtomicUsize::new(0),
        failures: AtomicUsize::new(1), // only the first one fails
    });
    engine.set_connector(conn.clone());
    let p = vp("sftp://h/x");

    let _ = engine.stat(&p).await.unwrap_err();
    assert_eq!(conn.connects.load(Ordering::SeqCst), 1);
    tokio::time::advance(std::time::Duration::from_millis(1100)).await;
    engine.stat(&p).await.expect("second dial connects");
    assert_eq!(conn.connects.load(Ordering::SeqCst), 2);
    // Cached: no more dials.
    engine.stat(&p).await.expect("cached");
    assert_eq!(conn.connects.load(Ordering::SeqCst), 2);
}

/// A provider that can be POISONED: after `poison`, every operation returns
/// `ProviderUnavailable` (a dead session: server restarted, network down).
struct FlipProvider {
    poisoned: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl Provider for FlipProvider {
    fn scheme(&self) -> &'static str {
        "sftp"
    }
    fn capabilities(&self) -> norte_proto::Capabilities {
        norte_proto::Capabilities {
            flags: norte_proto::CapabilityFlags::empty(),
            max_path: None,
        }
    }
    async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        if self.poisoned.load(Ordering::SeqCst) {
            return Err(Error::ProviderUnavailable { retryable: true });
        }
        Ok(Entry {
            attrs: std::collections::BTreeMap::new(),
            path: p.clone(),
            kind: EntryKind::Dir,
            size: None,
            mtime_ms: None,
        })
    }
    async fn list(&self, _p: &VPath) -> Result<norte_vfs::EntryStream, Error> {
        Err(Error::Unsupported)
    }
    async fn read(
        &self,
        _p: &VPath,
        _range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, Error> {
        Err(Error::Unsupported)
    }
    async fn write(&self, _p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
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

/// A connector that delivers a FRESH session per dial and keeps each one's
/// poison switch.
struct RevivingConnector {
    connects: AtomicUsize,
    poisons: std::sync::Mutex<Vec<Arc<std::sync::atomic::AtomicBool>>>,
}

#[async_trait]
impl RemoteConnector for RevivingConnector {
    async fn connect(&self, _s: &str, _a: &str) -> Result<Connected, DialError> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        let poisoned = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.poisons.lock().unwrap().push(poisoned.clone());
        Ok(Connected {
            provider: Arc::new(FlipProvider { poisoned }),
            warnings: Vec::new(),
        })
    }
    async fn trust_host_key(&self, _h: &str, _p: Option<u16>, _f: &str) -> Result<(), Error> {
        Ok(())
    }
    async fn provide_secret(&self, _c: &str, _s: &str) -> Result<(), Error> {
        Ok(())
    }
}

/// #47: a session that starts returning `ProviderUnavailable` gets EVICTED
/// from the cache — the next access reconnects (lazy reconnection), with no
/// process restart nor a zombie entry forever.
#[tokio::test]
async fn a_dead_session_is_evicted_and_reconnects() {
    let engine = Engine::new();
    let conn = Arc::new(RevivingConnector {
        connects: AtomicUsize::new(0),
        poisons: std::sync::Mutex::new(Vec::new()),
    });
    engine.set_connector(conn.clone());
    let p = vp("sftp://h/x");

    engine.stat(&p).await.expect("session 1 alive");
    assert_eq!(conn.connects.load(Ordering::SeqCst), 1);

    // The session dies: the error propagates AS IS to the caller…
    conn.poisons.lock().unwrap()[0].store(true, Ordering::SeqCst);
    let err = engine.stat(&p).await.unwrap_err();
    assert!(matches!(
        err,
        Error::ProviderUnavailable { retryable: true }
    ));

    // …and the key ended up evicted: the next access reconnects.
    engine.stat(&p).await.expect("reconnected");
    assert_eq!(conn.connects.load(Ordering::SeqCst), 2, "re-dial");
}

/// A connector with a fixed canonical form: `h` and `oscar@h` are the SAME identity.
struct CanonConnector {
    connects: AtomicUsize,
}

#[async_trait]
impl RemoteConnector for CanonConnector {
    async fn connect(&self, _s: &str, _a: &str) -> Result<Connected, DialError> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        Ok(Connected {
            provider: Arc::new(EchoProvider),
            warnings: Vec::new(),
        })
    }
    async fn canonical_authority(&self, _scheme: &str, authority: &str) -> Option<String> {
        assert!(authority == "h" || authority == "oscar@h");
        Some("oscar@h".to_string())
    }
    async fn trust_host_key(&self, _h: &str, _p: Option<u16>, _f: &str) -> Result<(), Error> {
        Ok(())
    }
    async fn provide_secret(&self, _c: &str, _s: &str) -> Result<(), Error> {
        Ok(())
    }
}

/// #47 (canonical dedup): `sftp://host` which inherits `oscar@` from the
/// config and `sftp://oscar@host` are the same identity — ONE session, in
/// either order.
#[tokio::test]
async fn canonical_dedup_does_not_open_a_second_session() {
    // Order 1: the userless form first.
    let engine = Engine::new();
    let conn = Arc::new(CanonConnector {
        connects: AtomicUsize::new(0),
    });
    engine.set_connector(conn.clone());
    engine.stat(&vp("sftp://h/x")).await.expect("stat 1");
    engine.stat(&vp("sftp://oscar@h/x")).await.expect("stat 2");
    assert_eq!(conn.connects.load(Ordering::SeqCst), 1, "one session");

    // Order 2: the canonical form first.
    let engine = Engine::new();
    let conn = Arc::new(CanonConnector {
        connects: AtomicUsize::new(0),
    });
    engine.set_connector(conn.clone());
    engine.stat(&vp("sftp://oscar@h/x")).await.expect("stat 1");
    engine.stat(&vp("sftp://h/x")).await.expect("stat 2");
    assert_eq!(conn.connects.load(Ordering::SeqCst), 1, "one session");
}

/// A session that serves ONE zip at `/a.zip` (stat+ranged read) and can be
/// poisoned — the minimum to compose an archive provider on top.
struct ZipHostProvider {
    zip: bytes::Bytes,
    poisoned: Arc<std::sync::atomic::AtomicBool>,
}

impl ZipHostProvider {
    fn check(&self) -> Result<(), Error> {
        if self.poisoned.load(Ordering::SeqCst) {
            return Err(Error::ProviderUnavailable { retryable: true });
        }
        Ok(())
    }
}

#[async_trait]
impl Provider for ZipHostProvider {
    fn scheme(&self) -> &'static str {
        "sftp"
    }
    fn capabilities(&self) -> norte_proto::Capabilities {
        norte_proto::Capabilities {
            flags: norte_proto::CapabilityFlags::empty(),
            max_path: None,
        }
    }
    async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        self.check()?;
        if p.segments().last() == Some(b"a.zip".as_slice()) {
            return Ok(Entry {
                attrs: std::collections::BTreeMap::new(),
                path: p.clone(),
                kind: EntryKind::File,
                size: Some(self.zip.len() as u64),
                mtime_ms: None,
            });
        }
        Ok(Entry {
            attrs: std::collections::BTreeMap::new(),
            path: p.clone(),
            kind: EntryKind::Dir,
            size: None,
            mtime_ms: None,
        })
    }
    async fn list(&self, _p: &VPath) -> Result<norte_vfs::EntryStream, Error> {
        self.check()?;
        Err(Error::Unsupported)
    }
    async fn read(
        &self,
        _p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, Error> {
        self.check()?;
        let total = self.zip.len() as u64;
        let (off, len) = match range {
            None => (0, total),
            Some(r) => {
                let off = r.offset.min(total);
                (off, r.len.unwrap_or(total - off).min(total - off))
            }
        };
        let (a, b) = (
            usize::try_from(off).expect("test: small range"),
            usize::try_from(off + len).expect("test: small range"),
        );
        let chunk = self.zip.slice(a..b);
        Ok(Box::pin(futures::stream::iter(vec![Ok(chunk)])))
    }
    async fn write(&self, _p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
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

/// A connector that serves, on dial N, a zip with the member `dial-N.txt`,
/// and keeps each session's poison switch.
struct ZipReviving {
    connects: AtomicUsize,
    poisons: std::sync::Mutex<Vec<Arc<std::sync::atomic::AtomicBool>>>,
}

#[async_trait]
impl RemoteConnector for ZipReviving {
    async fn connect(&self, _s: &str, _a: &str) -> Result<Connected, DialError> {
        let n = self.connects.fetch_add(1, Ordering::SeqCst) + 1;
        let member = format!("dial-{n}.txt");
        let zip = norte_testkit::ZipSmith::new()
            .file(member.as_bytes(), b"content")
            .build();
        let poisoned = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.poisons.lock().unwrap().push(poisoned.clone());
        Ok(Connected {
            provider: Arc::new(ZipHostProvider {
                zip: bytes::Bytes::from(zip),
                poisoned,
            }),
            warnings: Vec::new(),
        })
    }
    async fn trust_host_key(&self, _h: &str, _p: Option<u16>, _f: &str) -> Result<(), Error> {
        Ok(())
    }
    async fn provide_secret(&self, _c: &str, _s: &str) -> Result<(), Error> {
        Ok(())
    }
}

/// #62: evicting a session DRAGS DOWN the archive providers composed over it
/// (`zip+sftp://h` caches `sftp://h`'s Arc — leaving it would serve the index
/// of a dead connection).
#[tokio::test]
async fn evicting_a_session_drags_down_composite_archives() {
    use futures::StreamExt;

    let engine = Engine::new();
    let conn = Arc::new(ZipReviving {
        connects: AtomicUsize::new(0),
        poisons: std::sync::Mutex::new(Vec::new()),
    });
    engine.set_connector(conn.clone());

    let names = |entries: Vec<Result<Entry, Error>>| -> Vec<String> {
        entries
            .into_iter()
            .map(|e| {
                let e = e.expect("entry");
                String::from_utf8_lossy(e.path.segments().last().expect("segment")).into_owned()
            })
            .collect()
    };

    // Session 1: the zip+sftp composite serves dial 1's index.
    let inner = VPath::archive_compose("zip", &vp("sftp://h/a.zip"), &[]).expect("compose");
    let got = names(engine.list(&inner).await.expect("list 1").collect().await);
    assert_eq!(got, vec!["dial-1.txt".to_string()]);
    assert_eq!(conn.connects.load(Ordering::SeqCst), 1);

    // The session dies → a direct op evicts it…
    conn.poisons.lock().unwrap()[0].store(true, Ordering::SeqCst);
    let err = engine.stat(&vp("sftp://h/x")).await.unwrap_err();
    assert!(matches!(err, Error::ProviderUnavailable { .. }));

    // …and the COMPOSITE fell with it: the next list reconnects (dial 2) and
    // recomposes — it serves the NEW index, not the dead zip's.
    let got = names(engine.list(&inner).await.expect("list 2").collect().await);
    assert_eq!(
        got,
        vec!["dial-2.txt".to_string()],
        "composite dragged down"
    );
    assert_eq!(conn.connects.load(Ordering::SeqCst), 2);
}

/// `trust_host_key` without a configured connector is Unsupported, not a panic.
#[tokio::test]
async fn trust_without_a_connector_is_unsupported() {
    let engine = Engine::new();
    let err = engine
        .trust_host_key("h", Some(22), "SHA256:x")
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Unsupported), "was {err:?}");
}

/// A connection-warning observer that accumulates what it receives (#44).
struct RecordingObserver {
    seen: Arc<std::sync::Mutex<Vec<ConnectionWarning>>>,
}

impl ConnectionObserver for RecordingObserver {
    fn on_connection_warning(&self, warning: &ConnectionWarning) {
        self.seen.lock().expect("test lock").push(warning.clone());
    }
}

/// #44: a connect that degrades TLS delivers the warning to the installed
/// observer, EXACTLY once per establishment (the provider is cached afterward).
#[tokio::test]
async fn the_observer_receives_the_degradation_warning() {
    let engine = Engine::new();
    let warning = ConnectionWarning {
        scheme: "sftp".into(),
        host: "a.example".into(),
        reason: ConnectionWarningReason::TlsAuthRejected,
    };
    let conn = Arc::new(FakeConnector::with_warnings(vec![warning.clone()]));
    engine.set_connector(conn.clone());
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    engine.set_connection_observer(Arc::new(RecordingObserver { seen: seen.clone() }));

    // First access: establishes the connection and emits the warning.
    engine
        .stat(&vp("sftp://a.example/x"))
        .await
        .expect("stat 1");
    // Second access to the same host: cached, does NOT re-establish → no re-warning.
    engine
        .stat(&vp("sftp://a.example/y"))
        .await
        .expect("stat 2");

    let got = seen.lock().expect("test lock");
    assert_eq!(*got, vec![warning], "exactly one warning, once");
    assert_eq!(conn.connects.load(Ordering::SeqCst), 1, "cached");
}

/// An observer that notes FAILURES (#322).
struct FailureObserver {
    seen: Arc<std::sync::Mutex<Vec<norte_core::connect::ConnectionFailure>>>,
}

impl ConnectionObserver for FailureObserver {
    fn on_connection_warning(&self, _w: &ConnectionWarning) {}

    fn on_connection_failure(&self, f: &norte_core::connect::ConnectionFailure) {
        self.seen.lock().expect("test lock").push(f.clone());
    }
}

/// A connector that fails with a COUNTABLE cause: the job sets the detail.
struct CauseConnector;

#[async_trait]
impl RemoteConnector for CauseConnector {
    async fn connect(&self, _s: &str, _a: &str) -> Result<Connected, DialError> {
        Err(DialError {
            error: Error::PermissionDenied,
            cause: Some(Box::new(norte_core::connect::Cause {
                conn: Some("rosetta".into()),
                reason: norte_core::connect::ConnectionFailureReason::SecretEmpty,
                detail: Some("the secret for \"rosetta\" is set but EMPTY".into()),
            })),
        })
    }
    async fn trust_host_key(&self, _h: &str, _p: Option<u16>, _f: &str) -> Result<(), Error> {
        Ok(())
    }
    async fn provide_secret(&self, _c: &str, _s: &str) -> Result<(), Error> {
        Ok(())
    }
}

/// #322: a connect that fails with a countable cause delivers it to the
/// observer, with the detail only the job knows, and the caller STILL
/// receives its category.
///
/// Both halves matter. If the error stopped being `PermissionDenied`, this
/// would have changed the taxonomy — which is what decides — to fix a
/// presentation problem.
#[tokio::test]
async fn the_observer_receives_a_failures_reason() {
    let engine = Engine::new();
    engine.set_connector(Arc::new(CauseConnector));
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    engine.set_connection_observer(Arc::new(FailureObserver { seen: seen.clone() }));

    let err = engine
        .stat(&vp("sftp://user@a.example/x"))
        .await
        .expect_err("the connect fails");
    assert!(
        matches!(err, Error::PermissionDenied),
        "the taxonomy does not change: {err:?}"
    );

    let got = seen.lock().expect("test lock");
    assert_eq!(got.len(), 1, "one failure, one warning: {got:?}");
    let f = &got[0];
    assert_eq!(f.reason.wire(), "secret-empty");
    assert_eq!(f.conn.as_deref(), Some("rosetta"));
    assert_eq!(f.scheme, "sftp");
    assert_eq!(
        f.host, "a.example",
        "the authority goes WITHOUT userinfo (rule 10): {:?}",
        f.host
    );
    assert!(
        !f.host.contains('@'),
        "not a trace of the user: {:?}",
        f.host
    );
    assert_eq!(
        f.detail.as_deref(),
        Some("the secret for \"rosetta\" is set but EMPTY")
    );
}

/// A connector that ALWAYS fails with a cause that also enters cooldown.
///
/// `Agent` degrades to `ProviderUnavailable`, which is what triggers the
/// negative cache — the path where the reason used to be lost on retry.
struct FallenAgentConnector {
    attempts: AtomicUsize,
}

#[async_trait]
impl RemoteConnector for FallenAgentConnector {
    async fn connect(&self, _s: &str, _a: &str) -> Result<Connected, DialError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        Err(DialError {
            error: Error::ProviderUnavailable { retryable: false },
            cause: Some(Box::new(norte_core::connect::Cause {
                conn: None,
                reason: norte_core::connect::ConnectionFailureReason::Agent,
                detail: Some("the SSH agent is not responding".into()),
            })),
        })
    }
    async fn trust_host_key(&self, _h: &str, _p: Option<u16>, _f: &str) -> Result<(), Error> {
        Ok(())
    }
    async fn provide_secret(&self, _c: &str, _s: &str) -> Result<(), Error> {
        Ok(())
    }
}

/// #322: the reason REPEATS while the negative cache serves the error.
///
/// Without this, the explanation disappeared exactly when someone looks for
/// it: the human reads "the SSH agent could not authenticate", presses again
/// within the backoff window, and the second attempt is served from the cache
/// without marking — so it does not go through the observer — and the answer
/// goes back to being the bare category.
#[tokio::test]
async fn the_reason_repeats_on_retry_within_the_backoff() {
    let engine = Engine::new();
    let conn = Arc::new(FallenAgentConnector {
        attempts: AtomicUsize::new(0),
    });
    engine.set_connector(conn.clone());
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    engine.set_connection_observer(Arc::new(FailureObserver { seen: seen.clone() }));

    for _ in 0..2 {
        let e = engine
            .stat(&vp("sftp://a.example/x"))
            .await
            .expect_err("fails");
        assert!(matches!(e, Error::ProviderUnavailable { .. }), "was {e:?}");
    }

    assert_eq!(
        conn.attempts.load(Ordering::SeqCst),
        1,
        "the second attempt was served from the negative cache (otherwise this test proves nothing)"
    );
    let got = seen.lock().expect("test lock");
    assert_eq!(
        got.len(),
        2,
        "the why also accompanies the cached error: {got:?}"
    );
    assert!(
        got.iter()
            .all(|f| f.reason == norte_core::connect::ConnectionFailureReason::Agent)
    );
}

/// The vocabulary the core EMITS is exactly the one the proto declares.
///
/// In both directions, and that is the point. With `reason` as a bare
/// `&'static str`, renaming a value here put nothing red: the proto's goldens
/// froze a different copy, the notification kept going out, and every failure
/// started painting as "unknown reason" forever. The emitter has to live in
/// the same place as the contract.
#[test]
fn the_failure_vocabulary_is_the_protos() {
    use norte_core::connect::ConnectionFailureReason as R;
    let from_core: Vec<&str> = R::ALL.iter().map(|r| r.wire()).collect();
    let from_proto = norte_proto::methods::CONNECTION_FAILURE_REASONS;
    for w in &from_core {
        assert!(
            from_proto.contains(w),
            "the core emits {w:?} and the proto does not declare it"
        );
    }
    for w in from_proto {
        assert!(
            from_core.contains(w),
            "the proto declares {w:?} and the core cannot emit it"
        );
    }
    // And none repeated: two variants with the same string make one
    // indistinguishable from the other on the wire.
    let mut sorted = from_core.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), from_core.len(), "two variants, one string");
}

/// #44: a connect WITHOUT warnings never calls the observer.
#[tokio::test]
async fn the_observer_is_not_called_without_warnings() {
    let engine = Engine::new();
    let conn = Arc::new(FakeConnector::new(false));
    engine.set_connector(conn.clone());
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    engine.set_connection_observer(Arc::new(RecordingObserver { seen: seen.clone() }));

    engine.stat(&vp("sftp://a.example/x")).await.expect("stat");
    assert!(
        seen.lock().expect("test lock").is_empty(),
        "without degradation there is no warning"
    );
}

/// A sequential connector: call N follows the `steps` script (fails fast or
/// waits at the gate and connects).
struct SeqConnector {
    connects: AtomicUsize,
    gate: tokio::sync::Semaphore,
}

#[async_trait]
impl RemoteConnector for SeqConnector {
    async fn connect(&self, _s: &str, _a: &str) -> Result<Connected, DialError> {
        let n = self.connects.fetch_add(1, Ordering::SeqCst) + 1;
        if n == 1 {
            // An ACTIONABLE failure (no cooldown): the retry marks instantly.
            return Err(Error::PermissionDenied.into());
        }
        let _permit = self.gate.acquire().await.expect("gate alive");
        Ok(Connected {
            provider: Arc::new(EchoProvider),
            warnings: Vec::new(),
        })
    }
    async fn trust_host_key(&self, _h: &str, _p: Option<u16>, _f: &str) -> Result<(), Error> {
        Ok(())
    }
    async fn provide_secret(&self, _c: &str, _s: &str) -> Result<(), Error> {
        Ok(())
    }
}

/// BLOCKER review #47: a STALE waiter (its job already finished and
/// published) that gets dropped without being re-polled does NOT decrement
/// waiters of a NEW job under the same key — the guard carries the id of the
/// job it subscribed to. Without the fix, A's drop cancelled B's dial and B
/// saw `Internal{panic:true}`.
#[tokio::test]
async fn a_stale_guard_does_not_cancel_the_new_dial() {
    let engine = Arc::new(Engine::new());
    let conn = Arc::new(SeqConnector {
        connects: AtomicUsize::new(0),
        gate: tokio::sync::Semaphore::new(0),
    });
    engine.set_connector(conn.clone());
    let p = vp("sftp://h/x");

    // A: a single poll (job 1 spawned, A's guard alive); job 1 fails and
    // publishes WITHOUT A being re-polled.
    let mut fut_a = Box::pin(engine.stat(&p));
    assert!(futures::poll!(fut_a.as_mut()).is_pending(), "A subscribed");
    while conn.connects.load(Ordering::SeqCst) < 1 {
        tokio::task::yield_now().await;
    }
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }

    // B: starts job 2 (dial at the gate).
    let e2 = Arc::clone(&engine);
    let p2 = p.clone();
    let b = tokio::spawn(async move { e2.stat(&p2).await });
    while conn.connects.load(Ordering::SeqCst) < 2 {
        tokio::task::yield_now().await;
    }

    // A gets DROPPED with its stale guard: it must not touch job 2.
    drop(fut_a);
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    conn.gate.add_permits(1);
    let res = tokio::time::timeout(std::time::Duration::from_secs(5), b)
        .await
        .expect("B does not hang")
        .expect("join");
    res.expect("B connects: the stale guard did not cancel its dial");
}
