//! The disk map's "measuring the directory…" notice leaves the status bar
//! when the measurement ends (landing shots, 2026-10-08): it stayed under a
//! map whose title already said done.

use std::sync::Arc;

use norte_proto::VPath;
use norte_testkit::MemProvider;
use norte_tui::app::{App, Pane};
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("test wire")
}

/// An engine over a memory tree with one folder in it.
async fn backend() -> norte_core::backend::Backend {
    let engine = norte_core::Engine::new();
    let mem = Arc::new(MemProvider::new());
    mem.mkdir(&vp("mem:///a")).await.expect("mkdir");
    engine.register_provider(mem as Arc<dyn Provider>);
    norte_core::backend::Backend::Embedded(Arc::new(engine))
}

fn app() -> App {
    let d = vp("mem:///");
    App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
}

/// Landed: the notice goes.
#[tokio::test]
async fn the_measuring_notice_goes_when_the_map_lands() {
    let backend = backend().await;
    let mut app = app();
    let mut work = norte_tui::jobs::InFlight::default();
    app.open_disk_map();
    norte_tui::jobs::launch_disk_map(&mut app, &backend, &mut work).await;
    let started = norte_i18n::t("msg-disk-map-started");
    assert_eq!(app.message.as_deref(), Some(started.as_str()));
    let run = work.disk_map.as_mut().expect("running");
    let res = (&mut run.handle).await;
    norte_tui::jobs::harvest_disk_map(&mut app, &mut work, res);
    assert_eq!(app.message, None, "the notice is gone once the map landed");
}

/// Closed while measuring: cancelled, and the notice goes with it.
#[tokio::test]
async fn the_measuring_notice_goes_when_the_map_is_closed() {
    let backend = backend().await;
    let mut app = app();
    let mut work = norte_tui::jobs::InFlight::default();
    app.open_disk_map();
    norte_tui::jobs::launch_disk_map(&mut app, &backend, &mut work).await;
    while app.disk_map_slot().is_some() {
        app.toggle_disk_map();
    }
    norte_tui::jobs::tend_disk_map(&mut app, &mut work);
    assert!(work.disk_map.is_none(), "cancelled");
    assert_eq!(app.message, None);
}

/// Only THAT notice: a message said after it stays.
#[test]
fn another_message_is_left_alone() {
    let mut app = app();
    app.message = Some("something else".to_owned());
    app.disk_map_settled();
    assert_eq!(app.message.as_deref(), Some("something else"));
}
