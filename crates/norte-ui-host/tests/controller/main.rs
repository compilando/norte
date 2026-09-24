//! The controller: a single writer, and what that guarantees.
//!
//! These tests need no daemon. The backend is a deterministic table, which is
//! exactly what the plan asked for: the host has to be useful to a headless
//! test before any renderer exists.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use norte_proto::VPath;
use norte_ui_host::action::UiAction;
use norte_ui_host::bridge::{ActionAck, RowKey, StaleAction};
use norte_ui_host::controller::{UiHost, UiHostOptions, Update};
use norte_ui_host::dto::{SlotView, UiNotice, UiUpdate};

#[path = "../backend_fake/mod.rs"]
// One test binary, many files (wave W10): each `mod` is a section of the old
// 22,000-line `controller.rs`, and the `use x::*` below share between
// sections the `pub(super)` helpers that were already used throughout the
// file. One integration file per section would be one binary per section —
// a full crate link each (CLAUDE.md's disk budget).
mod backend_fake;
use backend_fake::Fake;

mod attributes_processes;
mod base;
mod columns;
mod compare;
mod corpus;
mod gestos;
mod handoff;
mod help;
mod ir_a;
mod keys_palette;
mod layouts;
mod log;
mod organize;
mod panels;
mod plugin_panes;
mod rename;
mod revisiones;
mod search;
mod settings_extensions;
mod settings_write;
mod splash;
mod sync;
mod timeline;
mod transfers;
mod visor;

use attributes_processes::*;
use base::*;
use columns::*;
use compare::*;
use corpus::*;
use gestos::*;
use help::*;
use keys_palette::*;
use layouts::*;
use panels::*;
use rename::*;
use revisiones::*;
use search::*;
use settings_extensions::*;
use sync::*;
use transfers::*;

/// How long ONE update is waited for before the test is given up as hung.
///
/// It is a relief ceiling, not a measurement: it turns a hang into a failure
/// with a message. It used to be 500 ms, and under the whole gate's load
/// (6,000 tests in parallel plus the networked e2e ones) `next_revision`
/// lost it now and then in pre-push — the same family as `snapshot_until`, which
/// already waited fifteen seconds for the same reason. No test uses it as a
/// signal for "nothing is arriving".
const WAIT_MAX: std::time::Duration = std::time::Duration::from_secs(15);

/// Column settings with these ids, for every scheme.
fn columns_of(ids: &[&str]) -> norte_frontend::columns::ColumnsSettings {
    let cfg = norte_config::ColumnsConfig {
        default_columns: Some(ids.iter().map(|s| (*s).to_owned()).collect()),
        ..norte_config::ColumnsConfig::default()
    };
    norte_frontend::columns::ColumnsSettings::resolve(&cfg)
}

fn dir() -> VPath {
    VPath::parse("mem:///casa").expect("test vpath")
}

/// A test host's configuration: the factory one, with the `..` row TURNED
/// OFF.
///
/// Off on purpose and not by oversight. These tests reason about listing
/// indices — row 0 is the first entry — and one more row at the start would
/// shift them all without saying anything about what each one tests. The row
/// has its own tests, and they are the ones that turn it on.
fn test_settings() -> norte_frontend::config::FrontendConfig {
    let mut cfg = norte_ui_host::default_settings();
    cfg.common.ui_parent_entry = Some(false);
    cfg
}

async fn host(names: Vec<&'static str>) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend: Fake::con(&names),
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: test_settings(),
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

/// Starting produces EXACTLY one snapshot, and it describes a screen that
/// already exists: the listing was requested before publishing it.
#[tokio::test]
async fn starting_gives_a_snapshot_with_the_listing_inside() {
    let (_h, snap) = host(vec!["b.txt", "a.txt"]).await;
    let SlotView::Browser(b) = &snap.slots[0] else {
        panic!("the first slot is a listing");
    };
    assert_eq!(b.rows.len(), 2);
    // Sorted with the SHARED comparator, not the host's own.
    assert_eq!(b.rows[0].display_name, "a.txt");
    assert_eq!(b.cursor, Some(RowKey(0)));
}

/// Two handles to the host are still ONE writer: actions apply in order and
/// the sequence does not skip.
#[tokio::test]
async fn two_handles_one_writer_and_sequences_do_not_skip() {
    let (h, _snap) = host(vec!["a", "b", "c", "d"]).await;
    let h2 = h.clone();
    let mut sub = h.subscribe();

    for _ in 0..3 {
        let ack = h2
            .dispatch(UiAction::MoveCursor {
                slot_id: 1,
                delta: 1,
            })
            .await
            .expect("host alive");
        assert!(matches!(ack, ActionAck::Applied { .. }));
    }

    let mut seen = Vec::new();
    for _ in 0..3 {
        match sub.recv().await.expect("there is an update") {
            Update::Message(m) => seen.push(m.sequence),
            Update::Lagged => panic!("there should be no lag with three messages"),
        }
    }
    assert_eq!(seen, vec![1, 2, 3], "one sequence per action, with no gaps");
}

/// A click on a row that no longer exists mutates nothing, and says so.
#[tokio::test]
async fn a_row_that_no_longer_exists_is_a_race_not_an_error() {
    let (h, snap) = host(vec!["a"]).await;
    let ack = h
        .dispatch(UiAction::SelectRow {
            generation: listing(&snap).generation,
            slot_id: 1,
            key: RowKey(99),
        })
        .await
        .expect("host alive");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Generation
        }
    );
}

/// A slow subscriber does NOT grow the host's memory: it finds out it fell
/// behind and requests a snapshot.
#[tokio::test]
async fn a_slow_subscriber_finds_out_and_requests_a_snapshot() {
    let (h, _snap) = host(vec!["a", "b", "c"]).await;
    let mut sub = h.subscribe();
    // Many more updates than the buffer has slots.
    for _ in 0..200 {
        let _ = h
            .dispatch(UiAction::MoveCursor {
                slot_id: 1,
                delta: 1,
            })
            .await;
    }
    match sub.recv().await.expect("something arrives") {
        Update::Lagged => {}
        Update::Message(m) => panic!("should warn about the lag, not give {:?}", m.sequence),
    }
    // And the recovery is a full snapshot.
    let ack = h.dispatch(UiAction::Resync).await.expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }));
}

/// A subscriber leaving does not stop the host.
#[tokio::test]
async fn if_the_subscriber_leaves_the_host_keeps_going() {
    let (h, _snap) = host(vec!["a"]).await;
    drop(h.subscribe());
    let ack = h
        .dispatch(UiAction::MoveCursor {
            slot_id: 1,
            delta: 1,
        })
        .await
        .expect("the host stays alive with nobody listening");
    assert!(matches!(ack, ActionAck::Applied { .. }));
}

/// Shutting down reports whether something was left unfinished, and
/// afterward the host no longer accepts anything.
#[tokio::test]
async fn shutting_down_reports_and_closes() {
    let (h, _snap) = host(vec!["a"]).await;
    let mut sub = h.subscribe();
    let report = h.shutdown().await.expect("shuts down");
    assert!(!report.incomplete);
    let last = sub.recv().await.expect("the last message arrives");
    match last {
        Update::Message(m) => assert!(matches!(
            m.payload,
            UiUpdate::Notice(UiNotice::Shutdown { .. })
        )),
        Update::Lagged => panic!("no lag here"),
    }
    assert!(
        h.dispatch(UiAction::Resync).await.is_err(),
        "a shut-down host accepts no more actions"
    );
}

/// The visible window bounds what travels: requesting forty rows of a large
/// listing sends forty, not the listing.
#[tokio::test]
async fn only_the_visible_window_travels() {
    let names: Vec<&'static str> = vec!["f"; 500];
    let (h, _snap) = host(names).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 10,
        count: 40,
    })
    .await
    .expect("host alive");
    let Update::Message(m) = sub.recv().await.expect("arrives") else {
        panic!("no lag");
    };
    let UiUpdate::Patch(p) = &m.payload else {
        panic!("a patch");
    };
    match &p.changes[0] {
        norte_ui_host::dto::ViewChange::Rows {
            rows,
            first_visible,
            ..
        } => {
            assert_eq!(rows.len(), 40, "only the window");
            assert_eq!(*first_visible, 10);
        }
        other => panic!("expected rows: {other:?}"),
    }
}

/// The same tree, unwrapped: for tests that need to touch its channels
/// before starting the host.
fn tree_as_fake() -> Fake {
    let mut f = Fake::default();
    f.put(
        "mem:///casa",
        vec![
            (b"docs".to_vec(), true),
            (b"notas.txt".to_vec(), false),
            (vec![0x63, 0x61, 0x66, 0xC3, 0x28], false),
        ],
    );
    f.put(
        "mem:///casa/docs",
        vec![(b"a.md".to_vec(), false), (b"b.md".to_vec(), false)],
    );
    f
}

/// A two-level tree for really navigating.
fn fake_tree() -> Arc<Fake> {
    let mut f = Fake::default();
    f.put(
        "mem:///casa",
        vec![
            (b"docs".to_vec(), true),
            (b"notas.txt".to_vec(), false),
            // A name that is NOT UTF-8: it has to survive as bytes and reach
            // the renderer marked, never rejected nor silenced.
            (vec![0x63, 0x61, 0x66, 0xC3, 0x28], false),
        ],
    );
    f.put(
        "mem:///casa/docs",
        vec![(b"a.md".to_vec(), false), (b"b.md".to_vec(), false)],
    );
    Arc::new(f)
}

async fn host_tree(backend: Arc<Fake>) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: test_settings(),
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

fn listing(snap: &norte_ui_host::ViewSnapshot) -> &norte_ui_host::dto::BrowserSlotView {
    let SlotView::Browser(b) = &snap.slots[0] else {
        panic!("the first slot is a listing");
    };
    b
}

/// Waits for the next snapshot (a navigation sends one).
async fn next_snapshot(sub: &mut norte_ui_host::UiSubscription) -> norte_ui_host::ViewSnapshot {
    loop {
        match sub.recv().await.expect("the host is still alive") {
            Update::Message(m) => {
                if let UiUpdate::Snapshot(s) = m.payload {
                    return *s;
                }
            }
            // Falling behind does not break the wait: it means "request a
            // snapshot", and a snapshot is exactly what is being waited for.
            Update::Lagged => {}
        }
    }
}
