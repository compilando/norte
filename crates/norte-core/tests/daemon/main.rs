//! Test-first matrix for M2's phase 2: a real UDS daemon (socket in a
//! tempdir) — handshake, dispatch, broadcast, cancellation, shutdown, socket
//! security. All against `MemProvider` (spec §12).
#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use norte_core::approval::DenyAll;
use norte_core::daemon::{
    Client, ClientError, Daemon, DaemonApprovalResolver, DaemonConfig, DaemonError,
};
use norte_core::{Engine, PolicyConfig, ScopeRegistry, ScopedPolicy};
use norte_proto::methods::{
    self, ClientInfo, ConnectionDegraded, DaemonShutdownParams, DaemonShutdownResult, FsCopyParams,
    FsListParams, FsListResult, FsSearchParams, FsStatParams, FsStatResult, FsTaskResult,
    GrantScopeParams, GrantScopeResult, InitializeParams, PolicyApprovalRequired,
    PolicyDecideParams, PolicyDecideResult, PolicyPendingResult, RequestScopeParams,
    RequestScopeResult, SearchHits, TaskCancelParams, TaskCancelResult,
};
use norte_proto::wire::codes;
use norte_proto::{Entry, EntryKind, Error, TaskProgress, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid test wire")
}

/// How long a daemon condition gets before it is given up as broken.
///
/// **Polling here is FINE, and that is the difference with `norte-ui-host`.**
/// There, the actor lives in the test's own process and one can wait for the
/// executor to go idle; here there is a real daemon on the other side of a
/// real socket, and there is no way to know it has finished thinking except
/// by asking it. What does NOT hold is sleeping a fixed span and asserting:
/// that is a bet on how long a loaded machine takes.
///
/// The span is a FAILURE budget, not a wait: on green it is not consumed.
const DEADLINE: Duration = Duration::from_secs(10);

/// Polls `cond` until it is true, and fails NAMING what it expected.
///
/// The pause between polls exists so as not to burn CPU against a socket; it
/// is not what makes the test valid — the condition does that — and that is
/// why the test does not get more fragile if the machine runs slow: it just
/// loops more times.
macro_rules! until {
    ($what_was_expected:expr, $cond:expr) => {{
        let deadline = tokio::time::Instant::now() + DEADLINE;
        loop {
            if $cond {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "never happened: {}",
                $what_was_expected
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }};
}

/// The request id the background task leaves in its slot, once it arrives.
///
/// Six tests used to wait for it with a `loop` with NO deadline: if the id
/// never arrived, the test hung instead of failing — and a hung test does
/// not say what it expected. It is the same defect as a blind `sleep`, wearing
/// a different face.
async fn wait_for_id(slot: &Arc<std::sync::Mutex<Option<u64>>>) -> u64 {
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        if let Some(id) = *slot.lock().expect("id lock") {
            return id;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the background request never published its id"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write opens");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk goes in");
    sink.commit().await.expect("commit publishes");
}

fn client_info() -> ClientInfo {
    ClientInfo {
        name: "test".into(),
        version: "0.0.0".into(),
    }
}

/// A live daemon over a socket in a tempdir. Also returns `run()`'s
/// `JoinHandle` so shutdown can be awaited.
struct TestDaemon {
    socket: PathBuf,
    run: tokio::task::JoinHandle<Result<(), DaemonError>>,
    /// The daemon's tempdir, which is also its config root: this is where the
    /// `connections.toml` that serves `connection.list` comes from (#365).
    /// Kept alive so it lives as long as the daemon AND so tests can write
    /// inside it.
    dir: tempfile::TempDir,
    mem: Arc<MemProvider>,
}

impl TestDaemon {
    /// THIS daemon's config root: a tempdir, never whoever runs the suite's
    /// real `~/.config` (#365).
    ///
    /// This is where a test can drop a `connections.toml` and count on the
    /// daemon reading that one. It did not used to exist; `connection.list`
    /// read the real config and the suite's color depended on the machine.
    fn config_dir(&self) -> &std::path::Path {
        self.dir.path()
    }
}

async fn spawn_daemon(idle: Option<Duration>) -> TestDaemon {
    spawn_daemon_ttl(idle, Duration::from_mins(2)).await
}

async fn spawn_daemon_ttl(idle: Option<Duration>, listing_ttl: Duration) -> TestDaemon {
    spawn_daemon_mem(idle, listing_ttl, MemProvider::new()).await
}

async fn spawn_daemon_mem(
    idle: Option<Duration>,
    listing_ttl: Duration,
    mem: MemProvider,
) -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let engine = Arc::new(Engine::new());
    let mem = Arc::new(mem);
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    // THE `sync.plan` spool, one per daemon and under its own tempdir (ADR
    // 0049). Without it `sync.plan` answers `Unsupported`.
    engine.set_spool(norte_core::sync::Spool::new(dir.path()));
    let daemon = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: idle,
            listing_ttl,
            plugins_dir: Some(dir.path().to_path_buf()),
            state_dir: None,
        },
    )
    .await
    .expect("bind");
    let run = tokio::spawn(daemon.run());
    TestDaemon {
        socket,
        run,
        dir,
        mem,
    }
}

async fn connected_client(d: &TestDaemon) -> Client {
    let mut c = Client::connect(&d.socket).await.expect("connect");
    let init = c.initialize(client_info()).await.expect("initialize");
    assert_eq!(init.protocol_version, methods::PROTOCOL_VERSION);
    assert_eq!(init.encodings, vec!["json".to_string()]);
    c
}

/// A daemon with `ScopedPolicy` installed: an EMPTY scope registry at
/// startup (grantable over the wire, `policy.grant_scope`) + an `allow` rule
/// (within scope is permitted). A `User` passes (not sandboxed); an `Agent`
/// with no scope is denied at the border before rules are even looked at.
/// The registry is SHARED between the engine's `ScopedPolicy` and the
/// daemon's `Shared` (M3-3b).
async fn spawn_daemon_policy() -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let scopes = ScopeRegistry::new();
    let cfg = PolicyConfig::parse("[[rule]]\naction=\"allow\"").expect("policy cfg");
    let policy = ScopedPolicy::new(scopes.clone(), cfg);
    let engine = Arc::new(Engine::new().with_policy(Arc::new(policy), Arc::new(DenyAll)));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    engine.set_spool(norte_core::sync::Spool::new(dir.path()));
    let daemon = Daemon::bind_with_scopes(
        engine,
        scopes,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: Some(dir.path().to_path_buf()),
            state_dir: None,
        },
    )
    .await
    .expect("bind");
    let run = tokio::spawn(daemon.run());
    TestDaemon {
        socket,
        run,
        dir,
        mem,
    }
}

/// A daemon with `ScopedPolicy` and an `ask` rule (M3-3b Task 4): within
/// scope, the gate suspends the mutation in the approval router — the SAME
/// `Arc` that receives `policy.decide` over the wire. Configurable approval
/// TTL (the timeout tests use a short one).
async fn spawn_daemon_ask(approval_ttl: Duration) -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let scopes = ScopeRegistry::new();
    let cfg = PolicyConfig::parse("[[rule]]\naction=\"ask\"").expect("policy cfg");
    let policy = ScopedPolicy::new(scopes.clone(), cfg);
    // Construction order (plan risk 1): the resolver is born BEFORE the
    // engine and the daemon.
    let approvals = Arc::new(DaemonApprovalResolver::new(approval_ttl));
    let engine = Arc::new(Engine::new().with_policy(Arc::new(policy), Arc::clone(&approvals) as _));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let daemon = Daemon::bind_with_policy(
        engine,
        scopes,
        approvals,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: Some(dir.path().to_path_buf()),
            state_dir: None,
        },
    )
    .await
    .expect("bind");
    let run = tokio::spawn(daemon.run());
    TestDaemon {
        socket,
        run,
        dir,
        mem,
    }
}

/// Opens a connection that declares `agent_session`: the server binds it to
/// an `Actor::Agent` and sandboxes its mutations (M3-3b).
async fn connected_agent(d: &TestDaemon, session: &str) -> Client {
    let c = Client::connect(&d.socket).await.expect("connect");
    let _init: methods::InitializeResult = c
        .call(
            methods::INITIALIZE,
            &InitializeParams {
                client_info: client_info(),
                protocol_version: methods::PROTOCOL_VERSION.into(),
                encodings: vec!["json".into()],
                agent_session: Some(session.into()),
            },
        )
        .await
        .expect("initialize agent");
    c
}

// ---------- handshake ----------

// One test binary, many files (wave W10): the old 9,000-line `daemon.rs`,
// grouped by the method family each test exercises. The helpers are
// `pub(super)` and travel between files via `use x::*`.

mod compare_sync;
mod fs_ops;
mod index_ai;
mod listing;
mod plugins;
mod policy;
mod rename;
mod search;
mod session;
mod tasks;

use compare_sync::*;
use fs_ops::*;
use listing::*;
use plugins::*;
use policy::*;
use rename::*;
use session::*;
use tasks::*;
