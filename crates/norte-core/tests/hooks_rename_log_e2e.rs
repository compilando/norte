//! `org.norte.rename-log` (`plugins/rename-log/`) builds, installs and, once
//! a rename lands in the journal, tells the human how many files it touched
//! (H1, ADR 0100). The source of the event is the journal's commit path, so
//! the same hook fires for a row written in-process and for a move done by a
//! human through the daemon — and the sentence reaches the frontend as
//! `plugin.notice`, attributed to the plugin.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use norte_core::hooks::{HookNoticeSink, spawn_dispatcher};
use norte_core::journal::{Actor, NewEntry, Reversal};
use norte_core::plugins::{PluginRegistry, install};
use norte_plugin_host::PluginRuntime;
use norte_proto::methods::PluginNotice;
use tokio::sync::mpsc;

const ID: &str = "org.norte.rename-log";

fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/rename-log")
}

fn build_plugin() -> Option<PathBuf> {
    if !target_installed("wasm32-wasip2") {
        eprintln!("SKIP: target wasm32-wasip2 no instalado");
        return None;
    }
    let target_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("wasm-guests");
    let status = Command::new(env!("CARGO"))
        .current_dir(plugin_dir())
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-wasip2",
            "--target-dir",
        ])
        .arg(&target_dir)
        .status()
        .expect("cargo for rename-log");
    assert!(status.success(), "rename-log did not build");
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join("rename_log.wasm");
    assert!(wasm.exists(), "missing {}", wasm.display());
    Some(wasm)
}

fn target_installed(target: &str) -> bool {
    Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l == target)
        })
}

fn install_and_consent(cfg: &Path, wasm: &Path) {
    let stage = cfg.join("stage");
    std::fs::create_dir_all(&stage).expect("stage");
    for name in ["plugin.toml", "help.md"] {
        std::fs::copy(plugin_dir().join(name), stage.join(name)).expect(name);
    }
    std::fs::copy(wasm, stage.join("plugin.wasm")).expect("wasm");
    assert_eq!(install(cfg, &stage, false).expect("installs").id, ID);
    let mut reg = PluginRegistry::discover(cfg).expect("discover");
    assert!(reg.set_approval(ID, true).expect("approve"));
    assert!(reg.set_enabled(ID, true).expect("enable"));
}

struct ChanSink(mpsc::UnboundedSender<PluginNotice>);

impl HookNoticeSink for ChanSink {
    fn notice(&self, n: PluginNotice) {
        let _ = self.0.send(n);
    }
}

async fn next_notice(rx: &mut mpsc::UnboundedReceiver<PluginNotice>) -> PluginNotice {
    tokio::time::timeout(Duration::from_secs(20), rx.recv())
        .await
        .expect("a notice before the timeout")
        .expect("the dispatcher is alive")
}

/// The journal's commit path is the source: three `renamed` rows written
/// in-process become one call to the guest, which says "renamed 3 files". A
/// `created` row is not the hook's business and produces nothing.
#[tokio::test]
async fn a_rename_in_the_journal_reaches_the_hook_and_the_hook_speaks() {
    let Some(wasm) = build_plugin() else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    install_and_consent(cfg.path(), &wasm);

    let (tx, mut rx) = mpsc::unbounded_channel();
    let sender = spawn_dispatcher(
        cfg.path().to_path_buf(),
        Arc::new(PluginRuntime::new().expect("runtime")),
        Arc::new(ChanSink(tx)),
    );
    let journal = norte_core::SqliteJournal::open(&cfg.path().join("journal.db"))
        .await
        .expect("journal");
    journal.set_hook_sender(sender);

    let actor = Actor::User;
    let batch = journal.journal().alloc_batch().await.expect("batch");
    for (from, to) in [("a", "1_a"), ("b", "1_b"), ("c", "1_c")] {
        journal
            .journal()
            .record_entry(&NewEntry {
                op: "renamed",
                path: format!("file:///d/{to}").as_bytes(),
                path_to: Some(format!("file:///d/{from}").as_bytes()),
                reversal: Reversal::RenameBack,
                reversal_ref: None,
                actor: &actor,
                undoes_seq: None,
                batch_id: Some(batch),
            })
            .await
            .expect("record");
    }
    let n = next_notice(&mut rx).await;
    assert_eq!(n.plugin_id, ID);
    assert_eq!(n.kind, "notify");
    // Three rows offered back to back are drained as one batch, so the guest
    // sees the group; under load the drain may split them, and then the
    // sentences add up to the same three.
    let mut total = count_of(n.text.as_deref());
    while total < 3 {
        total += count_of(next_notice(&mut rx).await.text.as_deref());
    }
    assert_eq!(total, 3);

    // A created row: no notice. Prove it with a renamed row right behind —
    // the next thing that arrives is about that one alone.
    journal
        .journal()
        .record(
            "created",
            b"file:///d/new",
            None,
            Reversal::Delete,
            None,
            &actor,
        )
        .await
        .expect("record");
    journal
        .journal()
        .record(
            "renamed",
            b"file:///d/z",
            Some(b"file:///d/y"),
            Reversal::RenameBack,
            None,
            &actor,
        )
        .await
        .expect("record");
    let n = next_notice(&mut rx).await;
    assert_eq!(n.text.as_deref(), Some("renamed 1 file"), "{n:?}");
    journal.close().await;
}

fn count_of(text: Option<&str>) -> usize {
    let t = text.expect("a notify carries text");
    let n = t
        .strip_prefix("renamed ")
        .and_then(|r| r.split(' ').next())
        .and_then(|d| d.parse::<usize>().ok());
    n.unwrap_or_else(|| panic!("unexpected sentence {t:?}"))
}

/// Over the wire: a human moves a file through the daemon, the daemon's
/// journal records the rename, the hook says so, and `plugin.notice` carries
/// the sentence to the frontend's channel, attributed to the plugin.
#[cfg(unix)]
#[tokio::test]
async fn a_move_through_the_daemon_becomes_a_plugin_notice() {
    use norte_core::Engine;
    use norte_core::backend::Backend;
    use norte_core::daemon::{Daemon, DaemonConfig};
    use norte_proto::methods::ClientInfo;
    use norte_vfs::Provider;
    use norte_vfs_local::LocalProvider;

    let Some(wasm) = build_plugin() else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    install_and_consent(cfg.path(), &wasm);

    let files = tempfile::tempdir().expect("files dir");
    std::fs::write(files.path().join("foto.jpg"), b"x").expect("write");
    // A rooted provider serves `file:///` FROM `files`: the wire path is
    // relative to the root, not the native one.
    let from = norte_proto::VPath::parse("file:///foto.jpg").expect("from");
    let to = norte_proto::VPath::parse("file:///2025_foto.jpg").expect("to");

    let journal = norte_core::SqliteJournal::open(&cfg.path().join("journal.db"))
        .await
        .expect("journal");
    let engine = Arc::new(Engine::with_journal(Arc::new(journal)));
    engine.register_provider(Arc::new(LocalProvider::rooted(files.path())) as Arc<dyn Provider>);
    let sock_dir = tempfile::tempdir().expect("tempdir daemon");
    let socket = sock_dir.path().join("d.sock");
    let daemon = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: Some(cfg.path().to_path_buf()),
            state_dir: None,
        },
    )
    .await
    .expect("bind");
    let _run = tokio::spawn(daemon.run());
    let remote = norte_core::backend::remote::RemoteBackend::connect(
        socket,
        None,
        ClientInfo {
            name: "hooks-rename-log-e2e".into(),
            version: "0.0.0".into(),
        },
    )
    .await
    .expect("connect");
    let mut notices = remote
        .take_plugin_notices()
        .expect("the first owner takes it");
    let backend = Backend::Remote(remote);

    let task = backend
        .move_(&from, &to, norte_core::TransferOptions::default())
        .await
        .expect("move is queued");
    let state = tokio::time::timeout(Duration::from_secs(10), task.join())
        .await
        .expect("terminal before the timeout");
    assert_eq!(
        state,
        norte_proto::TaskState::Completed,
        "the move finished"
    );
    assert!(files.path().join("2025_foto.jpg").exists());

    let n = next_notice(&mut notices).await;
    assert_eq!(n.plugin_id, ID);
    assert_eq!(n.kind, "notify");
    assert_eq!(n.text.as_deref(), Some("renamed 1 file"), "{n:?}");
}
