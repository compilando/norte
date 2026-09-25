//! A refreshed listing asks the plugins for its decorations again.
//!
//! The decorations live in a map by path, so after a refresh the rows that
//! were already there kept their icon and the NEW ones had none: while a
//! copy landed in `~/Backup`, the watcher refreshed the pane and
//! `fjord-timelapse.mp4` appeared bare. A second client that opened on the
//! still-empty `Backup` had no icon at all, since everything it ever showed
//! arrived by refresh.

use std::sync::Arc;

use norte_core::Engine;
use norte_core::backend::Backend;
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, Pane};
use norte_tui::fill::Fill;
use norte_tui::jobs::SearchRun;
use norte_tui::navigate::{Cd, settle_cd};
use norte_tui::panel::{SLOT_LEFT, SLOT_RIGHT};
use norte_tui::probes::{DecorateFetch, Probed};

fn vp(w: &str) -> VPath {
    VPath::parse(w).expect("test wire")
}

fn file(dir: &VPath, name: &str) -> Entry {
    Entry {
        attrs: std::collections::BTreeMap::new(),
        path: dir.join(Segment::new(name.as_bytes().to_vec()).expect("segment")),
        kind: EntryKind::File,
        size: Some(1),
        mtime_ms: None,
    }
}

fn backend() -> Backend {
    Backend::Embedded(Arc::new(Engine::new()))
}

/// Pane 0 refreshed with a new file; pane 1 untouched.
fn refreshed_app() -> App {
    let dir = vp("mem:///Backup");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir.clone(), vec![file(&dir, "old.txt")]),
    );
    app.panes[0].refresh_listing(vec![file(&dir, "fjord-timelapse.mp4")]);
    app
}

/// `Ctrl+R` files its outcome as `Cd::Refreshed` through `settle_cd`.
#[tokio::test]
async fn a_refresh_through_settle_cd_requests_decorations() {
    let mut app = refreshed_app();
    let mut fill = norte_frontend::layout::BySlot::<Fill>::new();
    let mut decorate = norte_frontend::layout::BySlot::<DecorateFetch>::new();
    let mut probed = Probed::new();
    let mut search: Option<SearchRun> = None;
    settle_cd(
        &mut app,
        &backend(),
        &mut fill,
        &mut decorate,
        &mut probed,
        &mut search,
        Cd::Refreshed([true, false]),
    );
    assert!(
        decorate.get(SLOT_LEFT).is_some(),
        "the refreshed pane asks for its decorations"
    );
    assert!(
        decorate.get(SLOT_RIGHT).is_none(),
        "the pane that was not refreshed does not"
    );
}

/// The watcher, a mutation and the columns picker refresh through
/// `after_panes_refresh` instead.
#[tokio::test]
async fn a_refresh_through_the_ritual_requests_decorations() {
    let mut app = refreshed_app();
    let mut fill = norte_frontend::layout::BySlot::<Fill>::new();
    let mut decorate = norte_frontend::layout::BySlot::<DecorateFetch>::new();
    let mut probed = Probed::new();
    let mut search: Option<SearchRun> = None;
    norte_tui::refresh::after_panes_refresh(
        &mut app,
        &backend(),
        [true, false],
        &mut fill,
        &mut decorate,
        &mut probed,
        &mut search,
    );
    assert!(decorate.get(SLOT_LEFT).is_some());
    assert!(decorate.get(SLOT_RIGHT).is_none());
}
