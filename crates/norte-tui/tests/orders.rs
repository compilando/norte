//! The `:` box in the terminal (ADR 0185): the live `sel`, Esc putting the
//! marks back, and an order going through the doors F5/F7/F8 use.

use std::sync::Arc;

use norte_core::backend::Backend;
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, Modal, Pane};

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("vpath")
}

fn entry(dir: &VPath, name: &str) -> Entry {
    Entry {
        attrs: std::collections::BTreeMap::new(),
        path: dir.join(Segment::new(name.as_bytes().to_vec()).expect("segment")),
        kind: EntryKind::File,
        size: Some(10),
        mtime_ms: Some(0),
    }
}

fn app_with(here: &str, names: &[&str], other: &str) -> App {
    let d = vp(here);
    let es = names.iter().map(|n| entry(&d, n)).collect();
    let mut app = App::new(Pane::new(d, es), Pane::new(vp(other), Vec::new()));
    app.set_focus(0);
    app
}

fn backend() -> Backend {
    let engine = norte_core::Engine::new();
    engine.register_provider(Arc::new(norte_testkit::MemProvider::new()));
    Backend::Embedded(Arc::new(engine))
}

fn typed(app: &mut App, query: &str) {
    norte_tui::goto::open(app, &[], &[], query);
}

fn marked(app: &App) -> Vec<String> {
    app.focused()
        .marked_entries()
        .iter()
        .map(|e| String::from_utf8_lossy(e.path.file_name().expect("name").as_bytes()).into_owned())
        .collect()
}

fn order(line: &str) -> norte_orders::Order {
    norte_orders::parse(line).expect("parse")
}

#[test]
fn typing_sel_lights_the_rows_before_enter() {
    let mut app = app_with("mem:///d", &["a.pdf", "b.txt"], "mem:///o");
    typed(&mut app, ":sel ext:pdf");
    assert_eq!(marked(&app), ["a.pdf"]);
    assert!(app.order_preview.contains('1'), "{}", app.order_preview);
}

#[test]
fn esc_restores_the_marks_from_before_the_box() {
    let mut app = app_with("mem:///d", &["a.pdf", "b.txt"], "mem:///o");
    typed(&mut app, ":sel *");
    assert_eq!(marked(&app).len(), 2);
    norte_tui::orders::cancel(&mut app);
    assert!(marked(&app).is_empty(), "no marks before, none after");
    assert!(app.order_preview.is_empty());
}

#[test]
fn leaving_order_mode_gives_the_marks_back() {
    let mut app = app_with("mem:///d", &["a.pdf", "b.txt"], "mem:///o");
    typed(&mut app, ":sel ext:pdf");
    typed(&mut app, "");
    assert!(marked(&app).is_empty());
    assert!(app.order_marks_before.is_none());
}

#[tokio::test]
async fn enter_copies_the_matched_rows_to_the_other_pane() {
    let mut app = app_with("mem:///d", &["a.pdf", "b.txt"], "mem:///o");
    let b = backend();
    let next = norte_tui::orders::run(&mut app, &b, &order("sel ext:pdf | cp >other")).await;
    assert!(next.is_none());
    assert_eq!(marked(&app), ["a.pdf"]);
    assert!(
        matches!(
            app.modal,
            Some(Modal::TransferName { .. } | Modal::ConfirmTransfer { .. })
        ),
        "the F5 confirmation opens: {:?}",
        app.modal.as_ref().map(std::mem::discriminant)
    );
}

#[tokio::test]
async fn order_copy_reuses_the_f5_guard() {
    let mut app = app_with("mem:///d", &["a.pdf"], "mem:///d");
    let b = backend();
    norte_tui::orders::run(&mut app, &b, &order("sel * | cp >here")).await;
    assert!(app.modal.is_none(), "nothing is copied onto itself");
    assert_eq!(
        app.message.as_deref(),
        Some(norte_i18n::t("msg-transfer-dest-same").as_str())
    );
}

#[tokio::test]
async fn zero_matches_copies_nothing() {
    let mut app = app_with("mem:///d", &["a.txt"], "mem:///o");
    let b = backend();
    norte_tui::orders::run(&mut app, &b, &order("sel ext:pdf | cp >other")).await;
    assert!(app.modal.is_none(), "the cursor row is not copied");
    assert_eq!(
        app.message.as_deref(),
        Some(norte_i18n::t("msg-order-nothing-matched").as_str())
    );
}

#[tokio::test]
async fn a_catalogue_stage_runs_like_its_key() {
    let mut app = app_with("mem:///d", &["a"], "mem:///o");
    let b = backend();
    let next = norte_tui::orders::run(&mut app, &b, &order("pane.swap")).await;
    assert_eq!(
        next,
        Some(norte_frontend::goto::Action::Command("pane.swap".into()))
    );
}

#[tokio::test]
async fn log_opens_the_timeline() {
    let mut app = app_with("mem:///d", &["a"], "mem:///o");
    let b = backend();
    let next = norte_tui::orders::run(&mut app, &b, &order("log")).await;
    assert_eq!(
        next,
        Some(norte_frontend::goto::Action::Command(
            "layout.timeline".into()
        ))
    );
}

#[tokio::test]
async fn undo_with_an_empty_journal_says_so_and_asks_nothing() {
    let mut app = app_with("mem:///d", &["a"], "mem:///o");
    let b = backend();
    norte_tui::orders::run(&mut app, &b, &order("undo 2")).await;
    assert!(app.modal.is_none());
    assert!(app.message.is_some());
}

#[tokio::test]
async fn cd_comes_back_as_the_boxs_own_navigation() {
    let mut app = app_with("mem:///d", &["a"], "mem:///o");
    let b = backend();
    let next = norte_tui::orders::run(&mut app, &b, &order("cd sub")).await;
    assert_eq!(
        next,
        Some(norte_frontend::goto::Action::Ir(vp("mem:///d/sub")))
    );
}
