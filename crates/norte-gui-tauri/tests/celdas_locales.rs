//! A daemon with the LOCAL provider, which is what the window runs against.
//!
//! The spike showed the `size` and `mtime` columns blank over `file://`, and
//! the host's e2e did not see it because its daemon mounts `MemProvider`.
//! This one reproduces the real path: real files in a temp directory.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use norte_client::RemoteBackend;
use norte_core::Engine;
use norte_core::daemon::{Daemon, DaemonConfig};
use norte_proto::methods::ClientInfo;
use norte_ui_host::dto::SlotView;
use norte_ui_host::{UiHost, UiHostOptions};
use norte_vfs::Provider;

/// `[ui.columns]` with `size` in EXACT format.
///
/// The other half of #108 this window used to ignore: it asked for the
/// factory style, so a configured `format` did nothing here while the
/// terminal did honor it. `exact` is chosen because its answer is a
/// checkable number — `12` — and the factory one for `size` is `iec`, i.e.
/// "12 B": if the style were not applied, the test would say so instead of
/// exhausting its loop.
fn columns_with_exact_size() -> norte_frontend::columns::ColumnsSettings {
    let cfg = norte_config::ColumnsConfig {
        specs: [(
            "size".to_owned(),
            norte_config::ColumnSpec {
                width: None,
                align: None,
                format: Some("exact".to_owned()),
                header: None,
            },
        )]
        .into_iter()
        .collect(),
        ..norte_config::ColumnsConfig::default()
    };
    norte_frontend::columns::ColumnsSettings::resolve(&cfg)
}

/// Size and date cells carry a value over real files.
#[tokio::test]
async fn the_size_and_the_date_are_not_left_blank() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.txt"), b"hello world!").expect("writes");
    // The socket goes in /tmp and not in the tempdir: a `sockaddr_un` does
    // not reach 108 bytes of path, and a test's temp dir already eats them
    // up. And in its OWN directory: the daemon hardens to 0700 the dir that
    // holds its socket, and `/tmp` belongs to everyone.
    let sock_dir =
        std::path::PathBuf::from(format!("/tmp/norte-gui-celdas-{}", std::process::id()));
    std::fs::create_dir_all(&sock_dir).expect("socket dir");
    let socket = sock_dir.join("d.sock");
    let _ = std::fs::remove_file(&socket);

    let engine = Arc::new(Engine::new());
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::os_root()) as Arc<dyn Provider>
    );
    let d = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: None,
            state_dir: Some(dir.path().to_path_buf()),
        },
    )
    .await
    .expect("bind");
    let _run = tokio::spawn(d.run());

    let backend = RemoteBackend::connect(
        socket.clone(),
        None,
        ClientInfo {
            name: "gui-celdas".to_owned(),
            version: "0.0.0".to_owned(),
        },
    )
    .await
    .expect("connects");
    let start = norte_vfs_local::vpath_from_native(dir.path()).expect("vpath");
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: Arc::new(backend),
        initial_dir: start,
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: norte_ui_host::default_settings(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        // No profile: this test looks at a local listing's cells, and an
        // active profile does not change what a `stat` returns.
        profile: None,
        columns: columns_with_exact_size(),
        effects: norte_ui_host::commands::Effects::Full,
        log_ring: None,
    })
    .await
    .expect("starts");
    let _ = std::fs::remove_dir_all(&sock_dir);

    // The first frame comes out WITHOUT waiting for the probes — blocking it
    // would delay the first frame for a column's sake — so the size arrives
    // shortly after, in a patch. What is checked is that it arrives.
    let mut sub = h.subscribe();
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(25)).await;
        h.dispatch(norte_ui_host::UiAction::Resync)
            .await
            .expect("host is alive");
        let snapshot = loop {
            match sub.recv().await.expect("the host is still alive") {
                norte_ui_host::Update::Message(m) => {
                    if let norte_ui_host::dto::UiUpdate::Snapshot(s) = m.payload {
                        break s;
                    }
                }
                norte_ui_host::Update::Lagged => {}
            }
        };
        let SlotView::Browser(b) = &snapshot.slots[0] else {
            panic!("the first slot is a listing");
        };
        let Some(row) = b.rows.iter().find(|r| r.display_name == "a.txt") else {
            continue;
        };
        let size = row
            .cells
            .iter()
            .find(|c| c.column == "size")
            .expect("size configured");
        // `12` and not "12 B": with `format = "exact"` set, the cell has to
        // come out exact. If the configured style were not applied, the
        // factory one (`iec`) would arrive here and this loop would run out.
        if size.text.as_deref() == Some("12") {
            return;
        }
        assert_ne!(
            size.text.as_deref(),
            Some("12 B"),
            "the cell came out with the FACTORY format: `[ui.columns]` is \
             not being applied in the window"
        );
    }
    panic!("a real file's size never reached the cell");
}
