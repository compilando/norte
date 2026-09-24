//! M2 phase 3 matrix: `Backend::Remote` against a real daemon (a socket in a
//! tempdir) — the SAME surface as the embedded one, foreign tasks, resync via
//! `task.list` and reconnection with a warning.
#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use norte_core::Engine;
use norte_core::backend::remote::RemoteBackend;
use norte_core::backend::{Backend, ConnEvent, TaskRef};
use norte_core::daemon::{Daemon, DaemonConfig};
use norte_proto::methods::{ClientInfo, FsSearchParams, SearchHits};
use norte_proto::{ByteRange, CapabilityFlags, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;
use tokio::sync::mpsc;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid test wire")
}

/// Drains the hits channel until it CLOSES (with a time cap: if the route were
/// never retired, this would hang). Returns the display paths, sorted.
async fn drain_search(mut rx: mpsc::Receiver<SearchHits>) -> Vec<String> {
    let mut got = Vec::new();
    while let Some(hits) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("a batch or the channel closing before the timeout")
    {
        for e in hits.entries {
            got.push(e.path.display_lossy());
        }
    }
    got.sort();
    got
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write opens");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk goes in");
    sink.commit().await.expect("commit publishes");
}

struct TestDaemon {
    socket: PathBuf,
    /// Keeps the daemon's `run()` alive during the test.
    _run: tokio::task::JoinHandle<Result<(), norte_core::daemon::DaemonError>>,
    _dir: tempfile::TempDir,
    mem: Arc<MemProvider>,
}

async fn spawn_daemon_with(dir: tempfile::TempDir, mem: Arc<MemProvider>) -> TestDaemon {
    let socket = dir.path().join("d.sock");
    let engine = Arc::new(Engine::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let daemon = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: std::time::Duration::from_mins(2),
            plugins_dir: Some(dir.path().to_path_buf()),
            state_dir: None,
        },
    )
    .await
    .expect("bind");
    let run = tokio::spawn(daemon.run());
    TestDaemon {
        socket,
        _run: run,
        _dir: dir,
        mem,
    }
}

async fn spawn_daemon() -> TestDaemon {
    spawn_daemon_with(
        tempfile::tempdir().expect("tempdir"),
        Arc::new(MemProvider::new()),
    )
    .await
}

async fn remote(d: &TestDaemon) -> RemoteBackend {
    RemoteBackend::connect(
        d.socket.clone(),
        None,
        ClientInfo {
            name: "backend-test".into(),
            version: "0.0.0".into(),
        },
    )
    .await
    .expect("connect")
}

/// Waits for a task's terminal state via its watch.
async fn join_ref(task: TaskRef) -> TaskState {
    tokio::time::timeout(Duration::from_secs(5), task.join())
        .await
        .expect("terminal before the timeout")
}

/// #93: a container's `skipped` reaches BOTH `Backend` modes — the remote one
/// carries it in `fs.list`'s first page; the embedded one queries the
/// provider. With nothing skipped: `None` (the badge is not painted).
#[tokio::test]
async fn list_skipped_reaches_both_modes() {
    let mem = Arc::new(MemProvider::new().with_list_skipped(3));
    let d = spawn_daemon_with(tempfile::tempdir().expect("tempdir"), Arc::clone(&mem)).await;
    write_file(&d.mem, "mem:///a.txt", b"x").await;

    let remote_b = Backend::Remote(remote(&d).await);
    let (entries, skipped) = remote_b
        .list_with_skipped(&vp("mem:///"))
        .await
        .expect("remote list");
    assert_eq!(entries.len(), 1);
    assert_eq!(skipped, Some(3), "remote: it travels in fs.list's page");

    let engine = Arc::new(Engine::new());
    engine.register_provider(mem as Arc<dyn Provider>);
    let embedded = Backend::Embedded(engine);
    let (_, skipped) = embedded
        .list_with_skipped(&vp("mem:///"))
        .await
        .expect("embedded list");
    assert_eq!(skipped, Some(3), "embedded: it queries the provider");

    // A provider with nothing skipped: None in both modes.
    let d2 = spawn_daemon().await;
    write_file(&d2.mem, "mem:///b.txt", b"y").await;
    let remote2 = Backend::Remote(remote(&d2).await);
    let (_, skipped) = remote2
        .list_with_skipped(&vp("mem:///"))
        .await
        .expect("remote list, nothing skipped");
    assert_eq!(skipped, None);
}

// ---------- unified surface ----------

#[tokio::test]
async fn remote_copy_list_read_capabilities_like_the_embedded_one() {
    let d = spawn_daemon().await;
    write_file(&d.mem, "mem:///src.bin", b"remote-content").await;
    let backend = Backend::Remote(remote(&d).await);

    // list
    let entries = backend.list(&vp("mem:///")).await.expect("list");
    assert_eq!(entries.len(), 1);

    // copy as a TaskRef with progress up to terminal
    let task = backend
        .copy(
            &vp("mem:///src.bin"),
            &vp("mem:///dst.bin"),
            norte_core::TransferOptions::default(),
        )
        .await
        .expect("copy");
    assert_eq!(join_ref(task).await, TaskState::Completed);

    // read with a range (viewer)
    let bytes = backend
        .read(
            &vp("mem:///dst.bin"),
            Some(ByteRange {
                offset: 10,
                len: Some(6),
            }),
        )
        .await
        .expect("read");
    assert_eq!(bytes, b"ntent-"[..].to_vec().as_slice());

    // capabilities (F8's gating)
    let caps = backend
        .capabilities(&vp("mem:///"))
        .await
        .expect("capabilities");
    assert!(caps.flags.contains(CapabilityFlags::TRASH));

    // stat (fs.stat, M4 Lua T3)
    let entry = backend.stat(&vp("mem:///dst.bin")).await.expect("stat");
    assert_eq!(entry.size, Some(14));
}

/// **#295's whole seam, over the socket and without a single frontend line**
/// (ADR 0073): the SDK retains the anchor of what it LISTS and returns it
/// alone in the `transfer`. This exercises what no engine test can see — that
/// `fs.list` carries it over the wire and that `fs.copy` carries it back.
///
/// The destination gets replaced by ANOTHER node with the same name (delete
/// and recreate, which is what an attacker who cannot write inside would do).
/// The core has nothing to tell it apart with; the client does, because it was
/// not looking at THAT node.
#[tokio::test]
async fn the_sdk_anchors_the_destination_it_listed_and_the_copy_checks_it() {
    let d = spawn_daemon().await;
    d.mem.mkdir(&vp("mem:///d")).await.expect("destination");
    write_file(&d.mem, "mem:///src.bin", b"content").await;
    let backend = Backend::Remote(remote(&d).await);

    // Listing is what anchors: the client looks at `d/` and retains its identity.
    backend
        .list(&vp("mem:///d"))
        .await
        .expect("list of the destination");
    let task = backend
        .copy(
            &vp("mem:///src.bin"),
            &vp("mem:///d/copy.bin"),
            norte_core::TransferOptions::default(),
        )
        .await
        .expect("copy");
    assert_eq!(
        join_ref(task).await,
        TaskState::Completed,
        "the directory is still the one that was listed"
    );

    // The swap: same NAME, different NODE.
    d.mem
        .remove(&vp("mem:///d/copy.bin"))
        .await
        .expect("empty it");
    d.mem
        .remove(&vp("mem:///d"))
        .await
        .expect("delete the destination");
    d.mem
        .mkdir(&vp("mem:///d"))
        .await
        .expect("recreate the destination");

    let task = backend
        .copy(
            &vp("mem:///src.bin"),
            &vp("mem:///d/other.bin"),
            norte_core::TransferOptions::default(),
        )
        .await
        .expect("enqueues");
    assert!(
        matches!(
            join_ref(task).await,
            TaskState::Failed {
                error: norte_proto::Error::Conflict {
                    conflict: norte_proto::ConflictKind::EscapesRoot
                }
            }
        ),
        "the retained anchor no longer names this directory"
    );

    // And refreshing the pane is the fix: re-listing re-anchors.
    backend.list(&vp("mem:///d")).await.expect("re-list");
    let task = backend
        .copy(
            &vp("mem:///src.bin"),
            &vp("mem:///d/other.bin"),
            norte_core::TransferOptions::default(),
        )
        .await
        .expect("copy");
    assert_eq!(join_ref(task).await, TaskState::Completed);
}

/// A destination this client NEVER listed sends no anchor, and that is NOT a
/// failure: it is a `norte cp` with a hand-typed path, which behaves as it did
/// in 0.53.
#[tokio::test]
async fn a_destination_nobody_listed_copies_without_an_anchor() {
    let d = spawn_daemon().await;
    d.mem.mkdir(&vp("mem:///d")).await.expect("destination");
    write_file(&d.mem, "mem:///src.bin", b"content").await;
    let backend = Backend::Remote(remote(&d).await);

    let task = backend
        .copy(
            &vp("mem:///src.bin"),
            &vp("mem:///d/copy.bin"),
            norte_core::TransferOptions::default(),
        )
        .await
        .expect("copy");
    assert_eq!(join_ref(task).await, TaskState::Completed);
}

/// A clone of `Backend::Remote` shares the connection/watches but CANNOT
/// steal the one-shot channels (`take_foreign_tasks`, `take_conn_events`,
/// `take_approvals`) from the original owner: if the clone took them, the
/// owning TUI would be left without a channel and policy's `ask`s would
/// silently expire to `deny` (a MAJOR from the rust-reviewer over e408373).
/// Order matters: the clone tries to steal BEFORE the owner claims its own.
/// #104: fs.mkdir over the wire — a remote Task up to terminal, and the dir exists.
#[tokio::test]
async fn remote_mkdir_as_a_task() {
    let d = spawn_daemon().await;
    let backend = Backend::Remote(remote(&d).await);
    let task = backend.mkdir(&vp("mem:///wire-dir")).await.expect("mkdir");
    assert_eq!(join_ref(task).await, TaskState::Completed);
    assert!(d.mem.stat(&vp("mem:///wire-dir")).await.is_ok());
}

/// `fs.create` OVER THE WIRE (#290): the dispatcher's arm, `parse_params` and
/// the Task up to terminal, none of which the in-process path runs.
#[tokio::test]
async fn remote_create_as_a_task() {
    let d = spawn_daemon().await;
    let backend = Backend::Remote(remote(&d).await);
    let task = backend
        .create_file(&vp("mem:///wire-new.txt"))
        .await
        .expect("create");
    assert_eq!(join_ref(task).await, TaskState::Completed);
    let e = d
        .mem
        .stat(&vp("mem:///wire-new.txt"))
        .await
        .expect("is there");
    assert_eq!(e.size, Some(0), "and EMPTY: creating invents no content");
}

/// And over the wire, an occupied destination is also a conflict — not a
/// silent truncation.
#[tokio::test]
async fn remote_create_over_something_that_exists_is_a_conflict() {
    let d = spawn_daemon().await;
    write_file(&d.mem, "mem:///wire-taken.txt", b"what matters").await;
    let backend = Backend::Remote(remote(&d).await);
    let task = backend
        .create_file(&vp("mem:///wire-taken.txt"))
        .await
        .expect("enqueues");
    assert!(
        matches!(
            join_ref(task).await,
            TaskState::Failed {
                error: norte_proto::Error::Conflict { .. }
            }
        ),
        "a taken name is a conflict"
    );
    let e = d
        .mem
        .stat(&vp("mem:///wire-taken.txt"))
        .await
        .expect("still there");
    assert_eq!(e.size, Some(12), "with its bytes intact");
}

#[tokio::test]
async fn a_clone_does_not_steal_the_owners_channels() {
    let d = spawn_daemon().await;
    let mut backend = Backend::Remote(remote(&d).await);
    let mut clone = backend.clone();

    assert!(clone.take_foreign_tasks().is_none());
    assert!(clone.take_conn_events().is_none());
    assert!(clone.take_approvals().is_none());

    assert!(backend.take_foreign_tasks().is_some());
    assert!(backend.take_conn_events().is_some());
    assert!(backend.take_approvals().is_some());
}

/// Two frontends, the same session: backend B sees the task queued by backend
/// A as FOREIGN — phase 3's criterion.
#[tokio::test]
async fn two_backends_see_the_same_tasks() {
    let d = spawn_daemon().await;
    write_file(&d.mem, "mem:///big.bin", &vec![0xAB; 100_000]).await;
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(10)));

    let a = Backend::Remote(remote(&d).await);
    let mut b = Backend::Remote(remote(&d).await);
    let mut foreign = b.take_foreign_tasks().expect("foreign channel");

    let own = a
        .copy(
            &vp("mem:///big.bin"),
            &vp("mem:///copy.bin"),
            norte_core::TransferOptions::default(),
        )
        .await
        .expect("A's copy");
    let own_id = own.id();

    let seen = tokio::time::timeout(Duration::from_secs(5), foreign.recv())
        .await
        .expect("a foreign task before the timeout")
        .expect("live channel");
    assert_eq!(seen.id(), own_id, "B sees A's task");
    assert_eq!(join_ref(seen).await, TaskState::Completed);
}

/// Remote cancellation works from the `TaskRef` (the same gesture as the
/// embedded one).
#[tokio::test]
async fn remote_cancel_from_the_task_ref() {
    let d = spawn_daemon().await;
    write_file(&d.mem, "mem:///big.bin", &vec![0xCD; 200_000]).await;
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(20)));
    let backend = Backend::Remote(remote(&d).await);

    let task = backend
        .copy(
            &vp("mem:///big.bin"),
            &vp("mem:///copy.bin"),
            norte_core::TransferOptions::default(),
        )
        .await
        .expect("copy");
    task.cancel();
    assert_eq!(join_ref(task).await, TaskState::Cancelled);
}

/// A backend that connects LATE sees the live tasks through `task.list`'s resync.
#[tokio::test]
async fn resync_on_connecting_sees_ongoing_tasks() {
    let d = spawn_daemon().await;
    write_file(&d.mem, "mem:///big.bin", &vec![0xEE; 200_000]).await;
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(15)));
    let a = Backend::Remote(remote(&d).await);
    let own = a
        .copy(
            &vp("mem:///big.bin"),
            &vp("mem:///copy.bin"),
            norte_core::TransferOptions::default(),
        )
        .await
        .expect("A's copy");

    // B arrives late and still sees it (via task.list, not via broadcast).
    let mut b = Backend::Remote(remote(&d).await);
    let mut foreign = b.take_foreign_tasks().expect("foreign channel");
    let seen = tokio::time::timeout(Duration::from_secs(5), foreign.recv())
        .await
        .expect("resync before the timeout")
        .expect("live channel");
    assert_eq!(seen.id(), own.id());
}

/// Binds a daemon on a SPECIFIC socket (to be able to revive it on the same
/// path after shutting it down).
async fn bind_at(
    mem: Arc<MemProvider>,
    socket: PathBuf,
) -> (
    tokio_util::sync::CancellationToken,
    tokio::task::JoinHandle<Result<(), norte_core::daemon::DaemonError>>,
) {
    let engine = Arc::new(Engine::new());
    engine.register_provider(mem as Arc<dyn Provider>);
    // The socket's directory acts as the plugins root: a caller's tempdir,
    // never the real `~/.config` (see `DaemonConfig::plugins_dir`).
    let plugins_dir = socket.parent().map(std::path::Path::to_path_buf);
    let daemon = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket),
            idle_timeout: None,
            listing_ttl: std::time::Duration::from_mins(2),
            plugins_dir,
            state_dir: None,
        },
    )
    .await
    .expect("bind");
    let shutdown = daemon.shutdown_token();
    (shutdown, tokio::spawn(daemon.run()))
}

/// Reconnection with a warning: when the daemon dies, `Lost` arrives; when
/// another one comes back on the SAME socket, `Restored` — and operations work
/// again.
#[tokio::test]
async fn reconnection_warns_and_recovers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let mem = Arc::new(MemProvider::new());
    write_file(&mem, "mem:///f", b"x").await;
    let (shutdown1, run1) = bind_at(Arc::clone(&mem), socket.clone()).await;

    let mut backend = Backend::Remote(
        RemoteBackend::connect(
            socket.clone(),
            None,
            ClientInfo {
                name: "backend-test".into(),
                version: "0.0.0".into(),
            },
        )
        .await
        .expect("connect"),
    );
    let mut events = backend.take_conn_events().expect("events channel");
    assert!(backend.list(&vp("mem:///")).await.is_ok());

    // Shut down the daemon (graceful closes the connections): a Lost warning.
    shutdown1.cancel();
    tokio::time::timeout(Duration::from_secs(5), run1)
        .await
        .expect("shutdown")
        .expect("join")
        .expect("run ok");
    let ev = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("Lost before the timeout")
        .expect("live channel");
    assert_eq!(ev, ConnEvent::Lost);
    // While it is down, the taxonomy is honest.
    assert!(matches!(
        backend.list(&vp("mem:///")).await,
        Err(norte_proto::Error::ProviderUnavailable { retryable: true })
    ));

    // A new daemon on the SAME socket: Restored and operational.
    let (_shutdown2, _run2) = bind_at(Arc::clone(&mem), socket.clone()).await;
    let ev = tokio::time::timeout(Duration::from_secs(15), events.recv())
        .await
        .expect("Restored before the timeout (backoff ≤5 s)")
        .expect("live channel");
    assert_eq!(ev, ConnEvent::Restored);
    let entries = backend
        .list(&vp("mem:///"))
        .await
        .expect("operational after reconnecting");
    assert_eq!(entries.len(), 1);
}

/// The AGENT arm is still an agent after reconnecting.
///
/// `establish` also runs on every reconnection, and the daemon sets the actor
/// at the handshake without remembering the previous connection's: if the
/// session did not travel there, the backend would come back as `Actor::User`
/// — allow-all — and nothing would go red, because the failure's symptom is
/// that reads START working. Hence the assertion after `Restored` is the one
/// that matters: an `Ok` there is the actor whitewashing itself.
#[tokio::test]
async fn the_agent_arm_is_still_an_agent_after_reconnecting() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let mem = Arc::new(MemProvider::new());
    write_file(&mem, "mem:///f", b"x").await;
    let (shutdown1, run1) = bind_at(Arc::clone(&mem), socket.clone()).await;

    let mut agent = Backend::Remote(
        RemoteBackend::connect_as_agent(
            socket.clone(),
            ClientInfo {
                name: "backend-test".into(),
                version: "0.0.0".into(),
            },
            "agent.session".into(),
        )
        .await
        .expect("connect_as_agent"),
    );
    let mut events = agent.take_conn_events().expect("events channel");
    // Without a granted scope, the agent read gate blocks it. A `User` over
    // this same daemon lists fine — `reconexion_avisa_y_recupera` checks
    // that, and it is this test's control.
    assert!(
        matches!(
            agent.list(&vp("mem:///")).await,
            Err(norte_proto::Error::PolicyDenied { .. })
        ),
        "from the start the connection has to already be an agent's"
    );

    // The daemon dies…
    shutdown1.cancel();
    tokio::time::timeout(Duration::from_secs(5), run1)
        .await
        .expect("shutdown")
        .expect("join")
        .expect("run ok");
    let ev = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("Lost before the timeout")
        .expect("live channel");
    assert_eq!(ev, ConnEvent::Lost);

    // …and another one comes back on the SAME socket: the arm reconnects on its own.
    let (_shutdown2, _run2) = bind_at(Arc::clone(&mem), socket.clone()).await;
    let ev = tokio::time::timeout(Duration::from_secs(15), events.recv())
        .await
        .expect("Restored before the timeout (backoff ≤5 s)")
        .expect("live channel");
    assert_eq!(ev, ConnEvent::Restored);
    let after_reconnecting = agent.list(&vp("mem:///")).await;
    assert!(
        matches!(
            after_reconnecting,
            Err(norte_proto::Error::PolicyDenied { .. })
        ),
        "after reconnecting it is STILL an agent; an Ok here is the actor \
         whitewashed to User, and it was {after_reconnecting:?}"
    );
}

/// Errors from the daemon arrive as TAXONOMY (the frontends' contract), not as
/// a transport error.
#[tokio::test]
async fn remote_errors_are_taxonomy() {
    let d = spawn_daemon().await;
    let backend = Backend::Remote(remote(&d).await);
    assert_eq!(
        backend
            .list(&vp("mem:///does-not-exist"))
            .await
            .unwrap_err(),
        norte_proto::Error::NotFound
    );
    assert_eq!(
        backend
            .read(&vp("mem:///does-not-exist"), None)
            .await
            .unwrap_err(),
        norte_proto::Error::NotFound
    );
    // move of something that does not exist: NotFound from the source's stat.
    let task = backend
        .move_(
            &vp("mem:///does-not-exist"),
            &vp("mem:///x"),
            norte_core::TransferOptions::default(),
        )
        .await
        .expect("the task is queued");
    assert!(matches!(join_ref(task).await, TaskState::Failed { .. }));
}

/// A remote `fs.read` over several calls: a range bigger than the per-call cap
/// is re-requested with the offset advanced until everything is joined.
#[tokio::test]
async fn remote_read_multichunk() {
    let d = spawn_daemon().await;
    // > 8 MiB (FS_READ_MAX_CHUNK): forces at least two calls.
    let size = usize::try_from(norte_proto::methods::FS_READ_MAX_CHUNK).unwrap() + 4096;
    let content: Vec<u8> = (0..size).map(|i| u8::try_from(i % 251).unwrap()).collect();
    write_file(&d.mem, "mem:///big.bin", &content).await;
    let backend = Backend::Remote(remote(&d).await);

    let bytes = backend
        .read(&vp("mem:///big.bin"), None)
        .await
        .expect("complete multichunk read");
    assert_eq!(bytes.len(), size);
    assert_eq!(bytes, content, "the chunks join byte-exact");
}

/// A remote delete to the trash is a task like any other.
#[tokio::test]
async fn remote_delete_to_trash() {
    let d = spawn_daemon().await;
    write_file(&d.mem, "mem:///victim", b"x").await;
    let backend = Backend::Remote(remote(&d).await);
    let task = backend
        .delete(&vp("mem:///victim"), norte_proto::DeleteMode::Trash)
        .await
        .expect("delete");
    assert_eq!(join_ref(task).await, TaskState::Completed);
    assert_eq!(
        d.mem.stat(&vp("mem:///victim")).await.unwrap_err(),
        norte_proto::Error::NotFound
    );
}

/// rust-reviewer's B1: if the daemon DIES halfway through one of our tasks,
/// its `join()` does NOT hang — reconnection reconciles the orphan as
/// `Failed{ProviderUnavailable}`.
#[tokio::test]
async fn an_in_flight_task_does_not_hang_if_the_daemon_dies() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let mem = Arc::new(MemProvider::new());
    write_file(&mem, "mem:///big.bin", &vec![0xAB; 500_000]).await;
    // High latency: the copy is still alive when we kill the daemon.
    mem.faults()
        .set_latency_per_op(Some(Duration::from_millis(50)));
    let (shutdown1, run1) = bind_at(Arc::clone(&mem), socket.clone()).await;

    let backend = Backend::Remote(
        RemoteBackend::connect(
            socket.clone(),
            None,
            ClientInfo {
                name: "backend-test".into(),
                version: "0.0.0".into(),
            },
        )
        .await
        .expect("connect"),
    );
    let task = backend
        .copy(
            &vp("mem:///big.bin"),
            &vp("mem:///copy.bin"),
            norte_core::TransferOptions::default(),
        )
        .await
        .expect("queued copy");

    // Kill the daemon with the task alive (hard: cancels and closes right away).
    shutdown1.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), run1).await;

    // A NEW, empty daemon on the same socket: the reconnection does not find
    // it → reconciled as Failed, join NEVER hangs.
    let (_shutdown2, _run2) = bind_at(Arc::clone(&mem), socket.clone()).await;
    let state = tokio::time::timeout(Duration::from_secs(20), task.join())
        .await
        .expect("join answers, it never hangs");
    assert!(
        matches!(state, TaskState::Failed { .. } | TaskState::Cancelled),
        "the orphan resolves, it does not hang: {state:?}"
    );
}

// ---------- remote fs.search (live search T5) ----------

/// Remote `Backend::search` = the same surface as the embedded one: hits
/// arrive through the `search.hits` notification, `RemoteBackend`'s pump
/// routes them by `task_id` to the `rx` that `search` returns, and `rx` closes
/// at the terminal (a graceful route retirement). Same tree and same result as
/// `embedded_search_hits_stream`.
#[tokio::test]
async fn remote_search_like_the_embedded_one() {
    let d = spawn_daemon().await;
    write_file(&d.mem, "mem:///a.rs", b"").await;
    write_file(&d.mem, "mem:///b.txt", b"").await;
    d.mem.mkdir(&vp("mem:///sub")).await.expect("mkdir");
    write_file(&d.mem, "mem:///sub/c.rs", b"").await;
    let backend = Backend::Remote(remote(&d).await);

    let (task, rx) = backend
        .search(FsSearchParams {
            name_glob: Some("*.rs".into()),
            ..FsSearchParams::new(vp("mem:///"))
        })
        .await
        .expect("search");

    let got = drain_search(rx).await;
    assert_eq!(
        got,
        vec![
            vp("mem:///a.rs").display_lossy(),
            vp("mem:///sub/c.rs").display_lossy()
        ]
    );
    assert_eq!(join_ref(task).await, TaskState::Completed);
}

/// M1 (encoding, MEDIUM): a hostile name crosses the WIRE byte-EXACT. A file
/// named `[0xFF, 0xFE]` (non-UTF-8, never decodable) seeded in the tree comes
/// back through the real daemon with its bytes intact — the same assert as the
/// embedded one, now crossing JSON-RPC serialization (hard rule §1: names =
/// bytes, UTF-8 is never assumed).
#[tokio::test]
async fn remote_search_preserves_a_non_utf8_name_byte_exact() {
    let d = spawn_daemon().await;
    let seg = norte_proto::Segment::new(vec![0xFF, 0xFE]).expect("valid segment");
    let hostile = MemProvider::root().join(seg);
    let mut sink = d.mem.write(&hostile).await.expect("write opens");
    sink.write(Bytes::new()).await.expect("empty chunk");
    sink.commit().await.expect("commit publishes");
    let backend = Backend::Remote(remote(&d).await);

    let (task, mut rx) = backend
        .search(FsSearchParams {
            name_glob: Some("*".into()),
            ..FsSearchParams::new(vp("mem:///"))
        })
        .await
        .expect("search");

    let mut names: Vec<Vec<u8>> = Vec::new();
    while let Some(hits) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("a batch or the close before the timeout")
    {
        for e in hits.entries {
            names.push(e.path.file_name().expect("has a name").as_bytes().to_vec());
        }
    }
    assert_eq!(join_ref(task).await, TaskState::Completed);
    assert!(
        names.iter().any(|n| n.as_slice() == [0xFF, 0xFE]),
        "the non-UTF-8 name survived the wire byte-exact: {names:?}"
    );
}

/// Two CONCURRENT searches on the SAME connection do not mix their batches:
/// routing by `task_id` delivers only ITS OWN hits to each `rx` (disjoint
/// subtrees and globs → zero observable overlap if the routing is correct).
#[tokio::test]
async fn remote_two_searches_do_not_cross() {
    let d = spawn_daemon().await;
    d.mem.mkdir(&vp("mem:///da")).await.expect("mkdir da");
    d.mem.mkdir(&vp("mem:///db")).await.expect("mkdir db");
    write_file(&d.mem, "mem:///da/a1.rs", b"").await;
    write_file(&d.mem, "mem:///da/a2.rs", b"").await;
    write_file(&d.mem, "mem:///db/b1.txt", b"").await;
    let backend = Backend::Remote(remote(&d).await);

    let (t1, rx1) = backend
        .search(FsSearchParams {
            name_glob: Some("*.rs".into()),
            ..FsSearchParams::new(vp("mem:///da"))
        })
        .await
        .expect("search da");
    let (t2, rx2) = backend
        .search(FsSearchParams {
            name_glob: Some("*.txt".into()),
            ..FsSearchParams::new(vp("mem:///db"))
        })
        .await
        .expect("search db");

    let g1 = drain_search(rx1).await;
    let g2 = drain_search(rx2).await;
    assert_eq!(
        g1,
        vec![
            vp("mem:///da/a1.rs").display_lossy(),
            vp("mem:///da/a2.rs").display_lossy()
        ],
        "rx1 only sees da's .rs files"
    );
    assert_eq!(
        g2,
        vec![vp("mem:///db/b1.txt").display_lossy()],
        "rx2 only sees db's .txt file"
    );
    assert_eq!(join_ref(t1).await, TaskState::Completed);
    assert_eq!(join_ref(t2).await, TaskState::Completed);
}

// ---------- approval router through the backend (M3-3b T5) ----------

/// A daemon with an `ask` rule + approval router (the pattern from tests/daemon.rs).
async fn spawn_daemon_ask() -> TestDaemon {
    use norte_core::daemon::DaemonApprovalResolver;
    use norte_core::{PolicyConfig, ScopeRegistry, ScopedPolicy};
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let scopes = ScopeRegistry::new();
    let cfg = PolicyConfig::parse("[[rule]]\naction=\"ask\"").expect("policy cfg");
    let policy = ScopedPolicy::new(scopes.clone(), cfg);
    let approvals = Arc::new(DaemonApprovalResolver::new(Duration::from_secs(30)));
    let engine = Arc::new(Engine::new().with_policy(Arc::new(policy), Arc::clone(&approvals) as _));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let daemon = norte_core::daemon::Daemon::bind_with_policy(
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
        _run: run,
        _dir: dir,
        mem,
    }
}

/// A raw AGENT connection with a copy scope over `mem:///proj` already granted
/// (a request by the agent + a grant by an ephemeral human).
async fn agent_with_scope(d: &TestDaemon, session: &str) -> norte_core::daemon::Client {
    use norte_proto::methods::{
        self, GrantScopeParams, GrantScopeResult, InitializeParams, RequestScopeParams,
        RequestScopeResult,
    };
    let agent = norte_core::daemon::Client::connect(&d.socket)
        .await
        .expect("connect agent");
    let _: methods::InitializeResult = agent
        .call(
            methods::INITIALIZE,
            &InitializeParams {
                client_info: ClientInfo {
                    name: "agent".into(),
                    version: "0.0.0".into(),
                },
                protocol_version: methods::PROTOCOL_VERSION.into(),
                encodings: vec!["json".into()],
                agent_session: Some(session.into()),
            },
        )
        .await
        .expect("initialize agent");
    let req: RequestScopeResult = agent
        .call(
            methods::POLICY_REQUEST_SCOPE,
            &RequestScopeParams {
                session: session.into(),
                roots: vec![vp("mem:///proj")],
                ops: vec!["copy".into()],
                ttl_ms: 60_000,
            },
        )
        .await
        .expect("request_scope");
    let mut human = norte_core::daemon::Client::connect(&d.socket)
        .await
        .expect("connect human");
    human
        .initialize(ClientInfo {
            name: "granter".into(),
            version: "0.0.0".into(),
        })
        .await
        .expect("initialize human");
    let _: GrantScopeResult = human
        .call(
            methods::POLICY_GRANT_SCOPE,
            &GrantScopeParams {
                request_id: req.request_id,
            },
        )
        .await
        .expect("grant_scope");
    agent
}

/// Launches the agent's fs.copy on its own task (it ends up suspended at the Ask).
fn spawn_agent_copy(
    agent: norte_core::daemon::Client,
) -> tokio::task::JoinHandle<
    Result<norte_proto::methods::FsTaskResult, norte_core::daemon::ClientError>,
> {
    use norte_proto::methods::{self, FsCopyParams};
    tokio::spawn(async move {
        agent
            .call::<_, methods::FsTaskResult>(
                methods::FS_COPY,
                &FsCopyParams {
                    from: vp("mem:///proj/src.txt"),
                    to: vp("mem:///proj/dst.txt"),
                    on_collision: norte_proto::CollisionPolicy::default(),
                    symlinks: norte_proto::SymlinkPolicy::default(),
                    resume: norte_proto::ResumePolicy::default(),
                    verify: norte_proto::VerifyPolicy::default(),
                    dest_anchor: None,
                    queued: false,
                },
            )
            .await
    })
}

/// T5's LIVE path: `RemoteBackend`'s pump routes `policy.approval_required`
/// to the `take_approvals` channel, and `Backend::policy_decide(approve)`
/// unblocks the agent's copy.
#[tokio::test]
async fn the_backend_receives_an_approval_and_deciding_approves() {
    let d = spawn_daemon_ask().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/src.txt", b"hello").await;
    let mut backend = Backend::Remote(remote(&d).await);
    let mut approvals = backend.take_approvals().expect("approvals channel");

    let agent = agent_with_scope(&d, "s1").await;
    let copy = spawn_agent_copy(agent);

    let req = tokio::time::timeout(Duration::from_secs(5), approvals.recv())
        .await
        .expect("the approval arrives")
        .expect("live channel");
    assert_eq!(req.op, "copy");
    assert_eq!(req.session.as_deref(), Some("s1"));

    backend
        .policy_decide(req.approval_id, true)
        .await
        .expect("decide approve");
    let res = tokio::time::timeout(Duration::from_secs(5), copy)
        .await
        .expect("does not hang")
        .expect("join");
    assert!(res.expect("approved, it proceeds").task_id.get() > 0);
}

/// T5's RESYNC path: a `RemoteBackend` that connects AFTER the broadcast
/// receives the pending one via `policy.pending` (`ttl_ms` 0 = unknown) and
/// can deny it.
#[tokio::test]
async fn a_late_backend_resyncs_pending_ones_and_denies() {
    let d = spawn_daemon_ask().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/src.txt", b"hello").await;

    let agent = agent_with_scope(&d, "s1").await;
    let copy = spawn_agent_copy(agent);
    // Synchronizes: waits for the Ask to be REGISTERED in the daemon before
    // connecting the backend (without this, the pending one could reach it
    // via broadcast with a real ttl and the resync assert would be flaky —
    // the rust-reviewer's MAJOR-2). Polls with a raw human against
    // `policy.pending`.
    {
        let mut probe = norte_core::daemon::Client::connect(&d.socket)
            .await
            .expect("connect probe");
        probe
            .initialize(ClientInfo {
                name: "probe".into(),
                version: "0.0.0".into(),
            })
            .await
            .expect("initialize probe");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let listed: norte_proto::methods::PolicyPendingResult = probe
                .call(norte_proto::methods::POLICY_PENDING, &serde_json::json!({}))
                .await
                .expect("policy.pending");
            if !listed.pending.is_empty() {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the Ask never became pending"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // The probe dies here: when the backend connects, the resync is the
        // only possible path for the already-registered pending one.
    }

    // The frontend connects LATE: the pending one reaches it via the resync.
    let mut backend = Backend::Remote(remote(&d).await);
    let mut approvals = backend.take_approvals().expect("approvals channel");
    let req = tokio::time::timeout(Duration::from_secs(5), approvals.recv())
        .await
        .expect("the resync delivers the pending one")
        .expect("live channel");
    assert_eq!(req.op, "copy");
    assert_eq!(req.ttl_ms, 0, "unknown TTL in the resync");

    backend
        .policy_decide(req.approval_id, false)
        .await
        .expect("decide deny");
    let res = tokio::time::timeout(Duration::from_secs(5), copy)
        .await
        .expect("does not hang")
        .expect("join");
    let err = res.expect_err("denied");
    assert!(
        matches!(
            err,
            norte_core::daemon::ClientError::Rpc(ref rpc)
                if matches!(rpc.data, Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved")
        ),
        "PolicyDenied not-approved, was {err:?}"
    );
}

// ---------- #74: an abandoned submit → rpc.cancel for the in-flight dispatch ------

/// A hanging connector with a drop probe: `cancelled` turns on when the
/// dial's future is DROPPED halfway (the #47 pool's cancellation).
struct ProbedHangingConnector {
    started: std::sync::atomic::AtomicUsize,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}

struct DropProbe(Arc<std::sync::atomic::AtomicBool>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl norte_core::connect::RemoteConnector for ProbedHangingConnector {
    async fn connect(
        &self,
        _s: &str,
        _a: &str,
    ) -> Result<norte_core::connect::Connected, norte_core::connect::DialError> {
        self.started
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _probe = DropProbe(self.cancelled.clone());
        std::future::pending().await
    }
    async fn trust_host_key(
        &self,
        _h: &str,
        _p: Option<u16>,
        _f: &str,
    ) -> Result<(), norte_proto::Error> {
        Ok(())
    }
    async fn provide_secret(&self, _c: &str, _s: &str) -> Result<(), norte_proto::Error> {
        Ok(())
    }
}

/// #74: dropping the future of a remote submit IN FLIGHT sends `rpc.cancel`
/// (the backend's drop-based guard, the #72 pattern) — the daemon's dispatch
/// dies PRE-effect (here, cancelling the #47 dial) and the Task never gets
/// born orphaned with no canceller. This is the Lua driver's window that
/// ABANDONS the run with the submit in flight.
#[tokio::test]
async fn an_abandoned_submit_sends_rpc_cancel_and_kills_the_dispatch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let engine = Arc::new(Engine::new());
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let conn = Arc::new(ProbedHangingConnector {
        started: std::sync::atomic::AtomicUsize::new(0),
        cancelled: cancelled.clone(),
    });
    engine.set_connector(conn.clone());
    let daemon = Daemon::bind(
        engine,
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
    let _run = tokio::spawn(daemon.run());
    let backend = Backend::Remote(
        RemoteBackend::connect(
            socket,
            None,
            ClientInfo {
                name: "backend-test".into(),
                version: "0.0.0".into(),
            },
        )
        .await
        .expect("connect"),
    );

    let b2 = backend.clone();
    let submit = tokio::spawn(async move {
        b2.copy(
            &vp("sftp://h/a"),
            &vp("sftp://h/b"),
            norte_core::TransferOptions::default(),
        )
        .await
    });
    // The dispatch is IN the dial (fs.copy in flight, no response).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while conn.started.load(std::sync::atomic::Ordering::SeqCst) == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the dial never started"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // The caller ABANDONS the submit's future (e.g. the Lua driver's grace
    // period ran out): the guard must send rpc.cancel with the in-flight id.
    submit.abort();
    let _ = submit.await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !cancelled.load(std::sync::atomic::Ordering::SeqCst) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the daemon's dispatch is still alive: the abandonment did not send rpc.cancel (#74)"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// #248: an ABANDONED `fs.list` does not keep the connection stuck.
///
/// `serve_connection` dispatches serially, so a listing the client stopped
/// waiting for — the session startup's five-second budget (#235), or any
/// dropped future — kept running against a hung provider, and EVERYTHING
/// behind it waited its turn, dying in its own 30 s `CALL_TIMEOUT`. The TUI
/// started, showed up, and served no purpose without saying why.
///
/// This test checks BOTH halves, which are two different changes: that the
/// client SENDS `rpc.cancel` when abandoning a read (reads used to go through
/// `call_timed`, with no guard), and that the daemon LISTENS for it for
/// `fs.list` (before only mutations were on the cancelable arm, so the
/// notice arrived and cut nothing).
#[tokio::test]
async fn an_abandoned_listing_does_not_keep_the_connection_stuck() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let engine = Arc::new(Engine::new());
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let conn = Arc::new(ProbedHangingConnector {
        started: std::sync::atomic::AtomicUsize::new(0),
        cancelled: cancelled.clone(),
    });
    engine.set_connector(conn.clone());
    let daemon = Daemon::bind(
        engine,
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
    let _run = tokio::spawn(daemon.run());
    let backend = Backend::Remote(
        RemoteBackend::connect(
            socket,
            None,
            ClientInfo {
                name: "backend-test".into(),
                version: "0.0.0".into(),
            },
        )
        .await
        .expect("connect"),
    );

    let b2 = backend.clone();
    let listing = tokio::spawn(async move { b2.list(&vp("sftp://h/dir")).await });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while conn.started.load(std::sync::atomic::Ordering::SeqCst) == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the dial never started"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // The caller stops waiting: this is what `tokio::time::timeout` does when
    // it expires, which is how this case really arrives.
    listing.abort();
    let _ = listing.await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !cancelled.load(std::sync::atomic::Ordering::SeqCst) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the listing's dispatch is still alive: the abandonment did not cut it (#248)"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // And the connection KEEPS SERVING: this is the half that was noticeable.
    // Without the cut, this request waited behind the hung listing and died
    // at 30 s with `ProviderUnavailable`.
    let next = tokio::time::timeout(Duration::from_secs(5), backend.list(&vp("mem:///")))
        .await
        .expect("the connection answers instead of staying queued");
    assert!(
        next.is_err() || next.is_ok(),
        "what matters is that it ANSWERED, not what it answered"
    );
}

// ---------- remote sync.plan / sync.apply (0.40.0, ADR 0049) ----------

/// A daemon with a journal and a spool: what is needed to plan and apply.
async fn spawn_daemon_sync() -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let journal = Arc::new(norte_core::SqliteJournal::new(
        norte_core::Journal::open_in_memory()
            .await
            .expect("journal"),
    ));
    let engine = Arc::new(Engine::with_journal(journal));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    engine.set_spool(norte_core::sync::Spool::new(dir.path()));
    let daemon = Daemon::bind(
        engine,
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
        _run: run,
        _dir: dir,
        mem,
    }
}

/// The whole cycle through the REMOTE arm, which is where the routing lives:
/// the plan's two events — `sync.steps` and `sync.plan_done` — travel over the
/// SAME `rx`, the closing one comes last, and the hash it carries runs.
///
/// Sharing a channel is not an implementation detail: with two channels, the
/// order between a batch and the closing event would depend on how the
/// runtime wakes up two receivers, and a client could approve the hash of a
/// plan that was still arriving.
#[tokio::test]
async fn remote_sync_plan_and_apply_like_the_embedded_one() {
    let d = spawn_daemon_sync().await;
    d.mem.mkdir(&vp("mem:///s")).await.expect("mkdir s");
    d.mem.mkdir(&vp("mem:///d")).await.expect("mkdir d");
    for i in 0..300 {
        write_file(&d.mem, &format!("mem:///s/f{i}.txt"), b"x").await;
    }
    let backend = Backend::Remote(remote(&d).await);

    let (task, mut rx) = backend
        .sync_plan(norte_proto::methods::SyncPlanParams {
            source: vp("mem:///s"),
            dest: vp("mem:///d"),
            mode: norte_proto::methods::SyncMode::Update,
            compare: norte_proto::methods::SyncCompareOptions::default(),
            on_unknown: norte_proto::methods::OnUnknown::Copy,
            include: None,
        })
        .await
        .expect("remote sync.plan");

    let mut steps = 0usize;
    let mut done = None;
    while let Some(event) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("an event or the channel closing before the timeout")
    {
        match event {
            norte_core::sync::SyncPlanEvent::Steps(b) => {
                assert!(
                    done.is_none(),
                    "a batch AFTER closing: the order is a SINGLE channel's queue"
                );
                steps += b.steps.len();
            }
            norte_core::sync::SyncPlanEvent::Done(d) => {
                assert!(done.is_none(), "two closings for one plan");
                done = Some(d);
            }
        }
    }
    assert_eq!(join_ref(task).await, TaskState::Completed);
    let done = done.expect("the plan closed with its hash");
    assert_eq!(steps, 300);
    assert_eq!(done.counts.copy, 300);

    let applying = backend
        .sync_apply(&done.plan_hash)
        .await
        .expect("sync.apply");
    let id = applying.id();
    assert_eq!(join_ref(applying).await, TaskState::Completed);
    let report = backend.sync_report(id).await.expect("sync.report");
    assert_eq!(report.done, 300);
    assert_eq!(report.failed, 0, "{:?}", report.failures);
    assert!(report.batch_id.is_some());

    // And the plan was spent: the same hash does not run again.
    assert!(matches!(
        backend.sync_apply(&done.plan_hash).await,
        Err(norte_proto::Error::PlanStale)
    ));
}

/// A daemon WITHOUT a spool answers `Unsupported` over the wire and the remote
/// arm delivers it as is: fail-closed also through the socket, and
/// distinguishable from a real failure.
#[tokio::test]
async fn remote_sync_plan_without_a_spool_is_unsupported() {
    let d = spawn_daemon().await;
    d.mem.mkdir(&vp("mem:///s")).await.expect("mkdir s");
    d.mem.mkdir(&vp("mem:///d")).await.expect("mkdir d");
    let backend = Backend::Remote(remote(&d).await);

    assert!(matches!(
        backend
            .sync_plan(norte_proto::methods::SyncPlanParams {
                source: vp("mem:///s"),
                dest: vp("mem:///d"),
                mode: norte_proto::methods::SyncMode::Update,
                compare: norte_proto::methods::SyncCompareOptions::default(),
                on_unknown: norte_proto::methods::OnUnknown::Copy,
                include: None,
            })
            .await,
        Err(norte_proto::Error::Unsupported)
    ));
}

/// Roadmap item 10: after a HANDOVER the client starts the next one, and
/// after a STOP it revives nothing. They are the same closed connection; the
/// only thing that tells them apart is the notification.
///
/// `spawn_cmd` is not a daemon: it is a `touch`, which is what lets the
/// decision be OBSERVED without setting up a second real process. What is
/// tested is exactly that — whether the client decides to start something or
/// not — and the start itself is the same path the first connection already
/// uses.
#[tokio::test]
async fn after_a_handover_the_next_one_starts_and_after_a_stop_it_does_not() {
    async fn run(mode: norte_proto::methods::ShutdownMode) -> bool {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("d.sock");
        let witness = dir.path().join("started");
        let mem = Arc::new(MemProvider::new());
        let (_shutdown, run) = bind_at(Arc::clone(&mem), socket.clone()).await;

        let backend = RemoteBackend::connect(
            socket.clone(),
            Some(vec![
                std::ffi::OsString::from("/usr/bin/touch"),
                witness.clone().into_os_string(),
            ]),
            ClientInfo {
                name: "backend-test".into(),
                version: "0.0.0".into(),
            },
        )
        .await
        .expect("connect");
        // The FIRST connection started nothing (there was a daemon), so the
        // witness can only appear via the reconnection.
        assert!(!witness.exists(), "the first connection starts nothing");

        let mut client = norte_core::daemon::Client::connect(&socket)
            .await
            .expect("control client");
        client
            .initialize(ClientInfo {
                name: "control".into(),
                version: "0.0.0".into(),
            })
            .await
            .expect("initialize");
        let _: norte_proto::methods::DaemonShutdownResult = client
            .call(
                norte_proto::methods::DAEMON_SHUTDOWN,
                &norte_proto::methods::DaemonShutdownParams {
                    graceful: true,
                    mode,
                },
            )
            .await
            .expect("shutdown accepted");
        tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .expect("shutdown")
            .expect("join")
            .expect("run ok");

        // Give the backend time for a reconnection with its backoff.
        for _ in 0..40 {
            if witness.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let started = witness.exists();
        drop(backend);
        started
    }

    assert!(
        run(norte_proto::methods::ShutdownMode::Handover).await,
        "a handover DOES authorize starting the next one"
    );
    assert!(
        !run(norte_proto::methods::ShutdownMode::Stop).await,
        "a stop does NOT: the user stopped it, and nobody revives it"
    );
}
