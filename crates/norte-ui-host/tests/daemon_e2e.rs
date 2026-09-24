//! The host against a REAL daemon.
//!
//! The other tests use a table backend, which is what makes them
//! deterministic. This one does the opposite on purpose: it brings up a real
//! daemon over a temporary socket, connects the SDK, and checks that what the
//! host projects comes out of a JSON-RPC round trip — initial listing,
//! navigation, and the session written on close.
//!
//! No screen and no Node: the host is useful to a headless test before any
//! renderer exists, which was phase 2's condition.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use norte_client::RemoteBackend;
use norte_core::Engine;
use norte_core::daemon::{Daemon, DaemonConfig};
use norte_proto::VPath;
use norte_proto::methods::ClientInfo;
use norte_testkit::MemProvider;
use norte_ui_host::action::UiAction;
use norte_ui_host::dto::{SlotView, UiUpdate};
use norte_ui_host::{UiHost, UiHostOptions, UiSubscription, Update, ViewSnapshot};
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("test wire")
}

struct TestDaemon {
    socket: std::path::PathBuf,
    /// The memory provider BEHIND the daemon, so a test can seed more than
    /// the little startup tree.
    mem: Arc<MemProvider>,
    _run: tokio::task::JoinHandle<Result<(), norte_core::daemon::DaemonError>>,
    _dir: tempfile::TempDir,
}

async fn write(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write opens");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk goes in");
    sink.commit().await.expect("commit publishes");
}

/// A daemon over an in-memory provider with a little tree inside.
async fn daemon() -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let mem = Arc::new(MemProvider::new());
    mem.mkdir(&vp("mem:///casa")).await.expect("mkdir casa");
    mem.mkdir(&vp("mem:///casa/docs"))
        .await
        .expect("mkdir docs");
    write(&mem, "mem:///casa/notas.txt", b"hola").await;
    write(&mem, "mem:///casa/docs/a.md", b"# a").await;

    let socket = dir.path().join("d.sock");
    let engine = Arc::new(Engine::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let d = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: Some(dir.path().to_path_buf()),
            state_dir: Some(dir.path().to_path_buf()),
        },
    )
    .await
    .expect("bind");
    TestDaemon {
        socket,
        mem,
        _run: tokio::spawn(d.run()),
        _dir: dir,
    }
}

async fn host_against(d: &TestDaemon) -> (UiHost, ViewSnapshot) {
    let backend = RemoteBackend::connect(
        d.socket.clone(),
        None,
        ClientInfo {
            name: "ui-host-e2e".into(),
            version: "0.0.0".into(),
        },
    )
    .await
    .expect("connects");
    UiHost::start(UiHostOptions {
        backend: Arc::new(backend),
        initial_dir: vp("mem:///casa"),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        // The `..` row turned off: these tests reason about listing indices,
        // and one more row at the start would shift them all without saying
        // anything about what they test.
        settings: {
            let mut cfg = norte_ui_host::default_settings();
            cfg.common.ui_parent_entry = Some(false);
            cfg
        },
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::default_columns(),
        effects: norte_ui_host::commands::Effects::Full,
        log_ring: None,
    })
    .await
    .expect("starts")
}

fn listing(snap: &ViewSnapshot) -> &norte_ui_host::dto::BrowserSlotView {
    let SlotView::Browser(b) = snap
        .slots
        .iter()
        .find(|s| matches!(s, SlotView::Browser(_)))
        .expect("there is a listing")
    else {
        unreachable!("filtered above")
    };
    b
}

async fn next_snapshot(sub: &mut UiSubscription) -> ViewSnapshot {
    for _ in 0..20 {
        let next = tokio::time::timeout(Duration::from_secs(5), sub.recv())
            .await
            .expect("a snapshot before the deadline")
            .expect("the host is still alive");
        if let Update::Message(m) = next
            && let UiUpdate::Snapshot(s) = m.payload
        {
            return *s;
        }
    }
    panic!("no snapshot ever arrived");
}

/// The initial listing comes from the daemon, sorted by the shared layer.
#[tokio::test]
async fn the_initial_listing_arrives_from_the_daemon() {
    let d = daemon().await;
    let (_h, snap) = host_against(&d).await;
    let b = listing(&snap);
    let names: Vec<&str> = b.rows.iter().map(|r| r.display_name.as_str()).collect();
    assert_eq!(names, vec!["docs", "notas.txt"], "directories first");
    assert!(b.path_display.ends_with("/casa"));
}

/// Really navigating: entering a directory and going back, against the
/// daemon.
#[tokio::test]
async fn navigating_against_the_daemon() {
    let d = daemon().await;
    let (h, snap) = host_against(&d).await;
    let docs = listing(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("docs is there")
        .key;
    let generation = listing(&snap).generation;
    let mut sub = h.subscribe();

    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs,
        generation,
    })
    .await
    .expect("host alive");
    let inside = next_snapshot(&mut sub).await;
    assert!(listing(&inside).path_display.ends_with("/casa/docs"));
    assert_eq!(listing(&inside).rows.len(), 1);

    h.dispatch(UiAction::History {
        slot_id: 1,
        back: true,
    })
    .await
    .expect("host alive");
    let outside = next_snapshot(&mut sub).await;
    assert!(listing(&outside).path_display.ends_with("/casa"));
}

/// The session is written on close, and the daemon returns it in the host's
/// next life: it is proof that the document crosses the whole wire.
#[tokio::test]
async fn the_session_survives_closing() {
    let d = daemon().await;
    let (h, snap) = host_against(&d).await;
    let docs = listing(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("docs is there")
        .key;
    let generation = listing(&snap).generation;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs,
        generation,
    })
    .await
    .expect("host alive");
    next_snapshot(&mut sub).await;
    let report = h.shutdown().await.expect("shuts down");
    assert!(!report.incomplete, "the owner wrote");

    // Another life of the host, against the SAME daemon.
    let (_h2, another) = host_against(&d).await;
    assert!(
        listing(&another).path_display.ends_with("/casa/docs"),
        "starts where the previous life left it: {}",
        listing(&another).path_display
    );
}

/// Configured columns arrive WITH their value.
///
/// The Tauri spike showed them empty against a real daemon, and the table
/// backend did not catch it: its entries are built by hand and always carry
/// a size. What crosses the wire is a different thing.
#[tokio::test]
async fn cells_carry_a_value_against_the_daemon() {
    let d = daemon().await;
    let (_h, snap) = host_against(&d).await;
    let b = listing(&snap);
    let file = b
        .rows
        .iter()
        .find(|r| r.display_name == "notas.txt")
        .expect("the file is there");
    let size = file
        .cells
        .iter()
        .find(|c| c.column == "size")
        .expect("the size column is configured");
    assert!(
        size.text.is_some(),
        "a file with a size carries its cell: {:?}",
        file.cells
    );
}

/// What ANOTHER client of the same daemon launches shows up on this board,
/// marked as foreign, and this window can stop it.
///
/// This is the proof the table backend cannot give: there, foreign tasks are
/// pushed by the test itself through a channel it opens. Here the task is
/// born on another connection, the daemon broadcasts it, and the SDK decides
/// it is foreign — which is the path a copy launched from the TUI while the
/// window is open really travels.
#[tokio::test]
async fn another_clients_task_is_visible_and_can_be_stopped() {
    let d = daemon().await;
    let (h, _snap) = host_against(&d).await;
    let mut sub = h.subscribe();

    // The "other frontend": another human connection to the same daemon.
    let other = RemoteBackend::connect(
        d.socket.clone(),
        None,
        ClientInfo {
            name: "otro-frontend-e2e".into(),
            version: "0.0.0".into(),
        },
    )
    .await
    .expect("connects");
    // The copy has to still be ALIVE when its first progress is broadcast:
    // the SDK does not announce a task that already arrived terminal as
    // foreign — there would be nothing to subscribe to — so an instant copy
    // against an in-memory provider would prove nothing. The provider is
    // slowed down on purpose.
    write(
        &d.mem,
        "mem:///casa/grande.bin",
        &vec![7u8; 4 * 1024 * 1024],
    )
    .await;
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(30)));
    let task = other
        .transfer(
            norte_client::Transfer::Copy,
            &vp("mem:///casa/grande.bin"),
            &vp("mem:///casa/copia.bin"),
            norte_client::TransferOptions::default(),
        )
        .await
        .expect("queues the copy");

    // THIS window's board shows it, and says it is not its own.
    let mut seen = None;
    for _ in 0..40 {
        let next = tokio::time::timeout(Duration::from_secs(5), sub.recv())
            .await
            .expect("an update before the deadline")
            .expect("the host is still alive");
        let tasks = match next {
            Update::Message(m) => match m.payload {
                UiUpdate::Snapshot(s) => s.tasks.clone(),
                UiUpdate::Patch(p) => p
                    .changes
                    .iter()
                    .find_map(|c| match c {
                        norte_ui_host::dto::ViewChange::Tasks { tasks, .. } => Some(tasks.clone()),
                        _ => None,
                    })
                    .unwrap_or_default(),
                UiUpdate::Notice(_) => Vec::new(),
            },
            Update::Lagged => Vec::new(),
        };
        if let Some(t) = tasks.iter().find(|t| t.task_id == task.id().get()) {
            seen = Some(t.clone());
            break;
        }
    }
    let seen = seen.expect("the other client's task reached the board");
    assert!(seen.foreign, "and the board says it is foreign: {seen:?}");

    // And it can be cancelled from here: it is the same session, so stopping
    // it is legitimate — and a board that shows it without being able to
    // touch it would be a window watching it burn.
    let ack = h
        .dispatch(UiAction::CancelTask {
            task_id: seen.task_id,
        })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, norte_ui_host::bridge::ActionAck::Applied { .. }),
        "{ack:?}"
    );
}

/// #270 — `norte_ui_host::backend::transferir` sends
/// `TransferOptions { on_collision, ..Default::default() }`, i.e.
/// `SymlinkPolicy::Preserve` and `ResumePolicy::Off`. That these two arrive
/// that way was ASSERTED by a comment and nothing else: the host's test
/// double implements `HostBackend` directly and does not go through this
/// impl.
///
/// `Preserve` is what stops a copy from DEREFERENCING a hostile symlink from
/// the source, so it is checked by its effect and not by serialization: the
/// link is still a link on the other side, with its target intact. If the
/// default fell off the path, the destination would be a file with the
/// target's content — which is exactly the leak `Preserve` exists to
/// prevent.
#[tokio::test]
async fn a_host_copy_preserves_the_sources_symlinks() {
    use norte_proto::CollisionPolicy;
    use norte_ui_host::backend::HostBackend;

    let d = daemon().await;
    d.mem
        .mkdir(&vp("mem:///casa/src"))
        .await
        .expect("mkdir src");
    write(&d.mem, "mem:///casa/src/real.txt", b"secreto").await;
    d.mem
        .symlink(
            &vp("mem:///casa/src/enlace"),
            b"real.txt",
            norte_vfs::SymlinkKind::File,
        )
        .await
        .expect("symlink");

    let backend = RemoteBackend::connect(
        d.socket.clone(),
        None,
        ClientInfo {
            name: "ui-host-e2e-symlink".into(),
            version: "0.0.0".into(),
        },
    )
    .await
    .expect("connects");

    let task = backend
        .copy(
            vp("mem:///casa/src"),
            vp("mem:///casa/dst"),
            CollisionPolicy::Fail,
            false,
        )
        .await
        .expect("queues the copy");
    let mut progress = task.progress;
    loop {
        let state = progress.borrow_and_update().state.clone();
        if state.is_terminal() {
            assert_eq!(state, norte_proto::TaskState::Completed, "{state:?}");
            break;
        }
        tokio::time::timeout(Duration::from_secs(10), progress.changed())
            .await
            .expect("the copy finishes before the deadline")
            .expect("the progress channel is still alive");
    }

    let copied = d
        .mem
        .stat(&vp("mem:///casa/dst/enlace"))
        .await
        .expect("the link reached the destination");
    assert_eq!(
        copied.kind,
        norte_proto::EntryKind::Symlink,
        "the copy DEREFERENCED the link: `SymlinkPolicy::Preserve` did not \
         reach the wire"
    );
}
