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

/// A lazy local listing carries no size for rows never painted (#52): the
/// order measures them before it selects, or `size>` would skip them.
#[tokio::test]
async fn sel_by_size_measures_rows_the_listing_never_measured() {
    use norte_vfs::Provider;
    let engine = norte_core::Engine::new();
    let mem = Arc::new(norte_testkit::MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let b = Backend::Embedded(Arc::new(engine));
    let mut sink = mem.write(&vp("mem:///big.bin")).await.expect("write");
    sink.write(bytes::Bytes::from(vec![0u8; 4096]))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
    let d = vp("mem:///");
    let mut lazy = entry(&d, "big.bin");
    lazy.size = None;
    let mut app = App::new(
        Pane::new(d, vec![lazy]),
        Pane::new(vp("mem:///o"), Vec::new()),
    );
    app.set_focus(0);
    norte_tui::orders::run(&mut app, &b, &order("sel size>1K")).await;
    assert_eq!(marked(&app), ["big.bin"]);
}

/// An order that fails, or whose `sel` matches nothing, gives back the marks
/// the pane had before the box — not the preview's, not none.
#[tokio::test]
async fn a_failed_or_empty_order_gives_the_marks_back() {
    let b = backend();
    for line in [":sel ext:pdf | cp >nowhere\\x00", ":sel ext:zzz | trash"] {
        let mut app = app_with("mem:///d", &["a.pdf", "b.txt"], "mem:///o");
        app.focused_mut().toggle_mark();
        let before = marked(&app);
        typed(&mut app, line);
        let order = app
            .goto
            .as_ref()
            .and_then(norte_frontend::goto::Goto::order)
            .and_then(Result::ok);
        app.goto = None;
        if let Some(o) = order {
            norte_tui::orders::run(&mut app, &b, &o).await;
        }
        assert_eq!(marked(&app), before, "{line}");
    }
}

/// A glob that does not compile carries the pattern; it must not reach the
/// status line raw (an RLO or an ESC typed as `\xHH`).
#[tokio::test]
async fn a_bad_glob_never_paints_its_pattern_raw() {
    let mut app = app_with("mem:///d", &["a"], "mem:///o");
    let b = backend();
    norte_tui::orders::run(&mut app, &b, &order(r"sel \xe2\x80\xae\x1b[")).await;
    let msg = app.message.clone().expect("said why");
    assert!(
        msg.chars().all(|c| !norte_encoding::is_terminal_hazard(c)),
        "{msg:?}"
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
