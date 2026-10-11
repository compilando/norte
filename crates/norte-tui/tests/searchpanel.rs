//! The search panel inside `App`: opening it rooted at the focused folder,
//! giving the keyboard to it and taking it back.

use std::sync::Arc;

use bytes::Bytes;
use crossterm::event::{KeyCode, KeyModifiers};
use norte_core::Engine;
use norte_core::backend::Backend;
use norte_frontend::keymap::{CATALOGUE, Effective, Resolver, Screen, presets};
use norte_frontend::search::SearchField;
use norte_frontend::search_panel::{DEBOUNCE, Due, SearchPanel};
use norte_proto::methods::{MatchInfo, SearchHits};
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_testkit::MemProvider;
use norte_tui::app::{App, KeyOwner, Pane};
use norte_tui::jobs::InFlight;
use norte_tui::searchpanel::{self, SearchFocus};
use norte_vfs::Provider;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn vp(wire: &str) -> VPath {
    // Fixed language: the snapshot freezes localized text.
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("valid wire")
}

fn entries(dir: &VPath) -> Vec<Entry> {
    (0..3)
        .map(|i| Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir
                .join(Segment::new(format!("f{i:02}").into_bytes()).expect("segment"))
                .clone(),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        })
        .collect()
}

fn test_app() -> App {
    let dir = vp("file:///casa");
    let mut app = App::new(
        Pane::new(dir.clone(), entries(&dir)),
        Pane::new(dir.clone(), entries(&dir)),
    );
    app.chrome.panel_bar_position = Some(norte_config::PanelBarPosition::Top);
    app
}

/// The command opens the panel at the focused listing's folder with the
/// keyboard in it; again closes it and gives the keys back.
#[test]
fn layout_search_opens_rooted_at_the_focused_folder_and_closes() {
    let mut app = test_app();
    let dir = app.focused().dir().clone();
    app.toggle_search_panel();
    let slot = app.search_panel_slot().expect("open");
    assert_eq!(app.panes.search_panel(slot).expect("model").root(), &dir);
    assert_eq!(app.key_owner(), KeyOwner::Search);
    app.toggle_search_panel();
    assert!(app.search_panel_slot().is_none());
    assert_eq!(app.key_owner(), KeyOwner::Panes);
}

/// Docked on the left by default (ADR 0173).
#[test]
fn snapshot_search_panel_opens_on_the_left() {
    let mut app = test_app();
    app.render_now_ms = Some(0);
    app.toggle_search_panel();
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    insta::assert_snapshot!(terminal.backend().to_string());
}

// ── Typing, running, painting (Task 7) ──────────────────────────────────

/// The `dialog` screen's resolver, the one that resolves inside a panel.
fn dialog_resolver() -> Resolver {
    let known: Vec<&str> = CATALOGUE.iter().map(|d| d.name).collect();
    let src = presets::source("orthodox").expect("the preset exists");
    let kf = norte_frontend::keymap::parse_keymap(src).expect("the preset parses");
    Resolver::new(Effective::build_for(&kf, &[], &known, Screen::Dialog).expect("merges"))
}

/// An app over the embedded engine's memory tree, with `files` in it.
async fn harness_with_tree(
    files: &[(&str, &str)],
) -> (App, Backend, InFlight, Resolver, Arc<MemProvider>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    for (name, content) in files {
        let mut sink = mem
            .write(&vp(&format!("mem:///{name}")))
            .await
            .expect("write opens");
        sink.write(Bytes::copy_from_slice(content.as_bytes()))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }
    let root = vp("mem:///");
    let app = App::new(Pane::new(root.clone(), vec![]), Pane::new(root, vec![]));
    (
        app,
        Backend::Embedded(Arc::new(engine)),
        InFlight::default(),
        dialog_resolver(),
        mem,
    )
}

async fn harness() -> (App, Backend, InFlight, Resolver) {
    let (app, backend, work, resolver, _) = harness_with_tree(&[]).await;
    (app, backend, work, resolver)
}

async fn key(
    app: &mut App,
    backend: &Backend,
    work: &mut InFlight,
    resolver: &mut Resolver,
    code: KeyCode,
) {
    searchpanel::on_key(app, backend, work, resolver, KeyModifiers::NONE, code).await;
}

async fn type_in(
    app: &mut App,
    backend: &Backend,
    work: &mut InFlight,
    resolver: &mut Resolver,
    text: &str,
) {
    for c in text.chars() {
        key(app, backend, work, resolver, KeyCode::Char(c)).await;
    }
}

/// The task's terminal state, through its progress `watch` (a `TaskRef` is
/// not `Clone`).
async fn wait_terminal(
    mut rx: tokio::sync::watch::Receiver<norte_proto::TaskProgress>,
) -> norte_proto::TaskState {
    tokio::time::timeout(std::time::Duration::from_mins(10), async {
        loop {
            let state = rx.borrow_and_update().state.clone();
            if state.is_terminal() {
                return state;
            }
            if rx.changed().await.is_err() {
                return rx.borrow().state.clone();
            }
        }
    })
    .await
    .expect("the task ends")
}

fn panel(app: &App) -> &SearchPanel {
    app.panes
        .search_panel(app.search_panel_slot().expect("open"))
        .expect("model")
}

/// Typing goes to the focused input and arms the pause; F2 toggles regex.
#[tokio::test]
async fn typing_writes_the_focused_input_and_f2_toggles_regex() {
    let (mut app, backend, mut work, mut resolver) = harness().await;
    app.toggle_search_panel();
    for code in [KeyCode::Char('a'), KeyCode::Char('g'), KeyCode::F(2)] {
        key(&mut app, &backend, &mut work, &mut resolver, code).await;
    }
    let p = panel(&app);
    assert_eq!((p.form().name.as_str(), p.form().regex), ("ag", true));
    key(
        &mut app,
        &backend,
        &mut work,
        &mut resolver,
        KeyCode::Backspace,
    )
    .await;
    assert_eq!(panel(&app).form().name, "a");
}

/// Tab walks name → content → results → name; with the filters open it
/// goes through them; ↓ on the last input reaches the results; Esc gives
/// the keyboard back.
#[tokio::test]
async fn tab_walks_the_inputs_and_the_results() {
    let (mut app, backend, mut work, mut resolver) = harness().await;
    app.toggle_search_panel();
    assert!(searchpanel::typing(&app));
    let mut seen = vec![app.search_panel_focus];
    for _ in 0..3 {
        key(&mut app, &backend, &mut work, &mut resolver, KeyCode::Tab).await;
        seen.push(app.search_panel_focus);
    }
    assert_eq!(
        seen,
        vec![
            SearchFocus::Field(SearchField::Name),
            SearchFocus::Field(SearchField::Content),
            SearchFocus::Tree,
            SearchFocus::Field(SearchField::Name),
        ]
    );
    // F8 unfolds the filters: Tab now crosses them, in their order.
    key(&mut app, &backend, &mut work, &mut resolver, KeyCode::F(8)).await;
    assert!(panel(&app).filters_open());
    let mut seen = Vec::new();
    for _ in 0..7 {
        key(&mut app, &backend, &mut work, &mut resolver, KeyCode::Tab).await;
        seen.push(app.search_panel_focus);
    }
    assert_eq!(
        seen,
        vec![
            SearchFocus::Field(SearchField::Content),
            SearchFocus::Field(SearchField::Exclude),
            SearchFocus::Field(SearchField::MinSize),
            SearchFocus::Field(SearchField::MaxSize),
            SearchFocus::Field(SearchField::Days),
            SearchFocus::Field(SearchField::Encoding),
            SearchFocus::Tree,
        ]
    );
    // Typing in a filter writes that filter.
    app.search_panel_focus = SearchFocus::Field(SearchField::Days);
    type_in(&mut app, &backend, &mut work, &mut resolver, "7").await;
    assert_eq!(panel(&app).form().days, "7");
    // ↓ on the last input → the results.
    app.search_panel_focus = SearchFocus::Field(SearchField::Encoding);
    key(&mut app, &backend, &mut work, &mut resolver, KeyCode::Down).await;
    assert_eq!(app.search_panel_focus, SearchFocus::Tree);
    assert!(!searchpanel::typing(&app));
    // From the results, a letter is not typed anywhere.
    key(
        &mut app,
        &backend,
        &mut work,
        &mut resolver,
        KeyCode::Char('x'),
    )
    .await;
    assert_eq!(panel(&app).form().name, "");
    key(&mut app, &backend, &mut work, &mut resolver, KeyCode::Esc).await;
    assert_eq!(app.key_owner(), KeyOwner::Panes);
}

/// Enter ends the pause now; the run's batches land in the tree, and its
/// end says how it went.
#[tokio::test]
async fn enter_searches_now_and_the_hits_land() {
    let (mut app, backend, mut work, mut resolver, _mem) =
        harness_with_tree(&[("a.txt", "uno\naguja\n"), ("b.txt", "nada\n")]).await;
    app.toggle_search_panel();
    key(&mut app, &backend, &mut work, &mut resolver, KeyCode::Tab).await;
    type_in(&mut app, &backend, &mut work, &mut resolver, "aguja").await;
    key(&mut app, &backend, &mut work, &mut resolver, KeyCode::Enter).await;
    searchpanel::tick(&mut app, &backend, &mut work).await;
    assert!(work.search_panel.is_some(), "launched");
    assert!(app.needs_frame, "a launch repaints");
    loop {
        let hits = work.search_panel.as_mut().expect("run").rx.recv().await;
        let end = hits.is_none();
        searchpanel::drain(&mut app, &mut work.search_panel, hits);
        if end {
            break;
        }
    }
    assert!(work.search_panel.is_none(), "the finished run is dropped");
    let p = panel(&app);
    assert_eq!(p.tree().len(), 1);
    assert_eq!(p.tree()[0].files.len(), 1);
    assert_eq!(p.tree()[0].files[0].matches[0].line, 2);
    assert!(matches!(
        p.status(),
        norte_frontend::search_panel::PanelStatus::Run {
            key: "search-panel-done",
            files: 1,
            ..
        }
    ));
}

/// In the results: ↓ moves, ← folds the folder, Enter unfolds it again.
#[tokio::test]
async fn the_results_move_and_fold() {
    let (mut app, backend, mut work, mut resolver) = harness().await;
    app.toggle_search_panel();
    {
        let slot = app.search_panel_slot().expect("open");
        let p = app.panes.search_panel_mut(slot).expect("model");
        let t0 = std::time::Instant::now();
        p.edit(SearchField::Content, "aguja".to_owned(), t0);
        let Some(Due::Launch { epoch, .. }) = p.due(t0 + DEBOUNCE, 0) else {
            panic!("launch")
        };
        p.apply_hits(epoch, &hits_with_51_lines());
    }
    app.search_panel_focus = SearchFocus::Tree;
    key(&mut app, &backend, &mut work, &mut resolver, KeyCode::Down).await;
    key(&mut app, &backend, &mut work, &mut resolver, KeyCode::Down).await;
    assert_eq!(panel(&app).cursor(), 2);
    key(&mut app, &backend, &mut work, &mut resolver, KeyCode::Left).await;
    assert_eq!(panel(&app).cursor(), 0, "← climbs to the folder");
    key(&mut app, &backend, &mut work, &mut resolver, KeyCode::Left).await;
    assert_eq!(panel(&app).rows().len(), 1, "and folds it");
    key(&mut app, &backend, &mut work, &mut resolver, KeyCode::Enter).await;
    assert!(panel(&app).rows().len() > 1, "Enter on a folder unfolds it");
    key(&mut app, &backend, &mut work, &mut resolver, KeyCode::Tab).await;
    assert_eq!(
        app.search_panel_focus,
        SearchFocus::Field(SearchField::Name)
    );
}

/// Review Focus 2: a second query after the pause cancels the first run.
#[tokio::test]
async fn a_second_query_cancels_the_first_run() {
    tokio::time::pause();
    let (mut app, backend, mut work, mut resolver, mem) =
        harness_with_tree(&[("a.txt", "aguja\n")]).await;
    // The walk stalls on its first step: only a cancel ends it as Cancelled.
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_mins(1)));
    app.toggle_search_panel();
    type_in(&mut app, &backend, &mut work, &mut resolver, "ag").await;
    tokio::time::sleep(DEBOUNCE).await;
    searchpanel::tick(&mut app, &backend, &mut work).await;
    let first = work
        .search_panel
        .as_ref()
        .expect("launched")
        .task
        .progress();
    type_in(&mut app, &backend, &mut work, &mut resolver, "u").await;
    tokio::time::sleep(DEBOUNCE).await;
    searchpanel::tick(&mut app, &backend, &mut work).await;
    let second = work.search_panel.as_ref().expect("relaunched");
    assert_ne!(second.task.id(), first.borrow().task_id, "a new run");
    assert_eq!(
        wait_terminal(first).await,
        norte_proto::TaskState::Cancelled
    );
}

/// Review Focus 5: closing the panel cancels the run.
#[tokio::test]
async fn closing_the_panel_cancels_its_run() {
    tokio::time::pause();
    let (mut app, backend, mut work, mut resolver, mem) =
        harness_with_tree(&[("a.txt", "aguja\n")]).await;
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_mins(1)));
    app.toggle_search_panel();
    type_in(&mut app, &backend, &mut work, &mut resolver, "ag").await;
    tokio::time::sleep(DEBOUNCE).await;
    searchpanel::tick(&mut app, &backend, &mut work).await;
    let task = work
        .search_panel
        .as_ref()
        .expect("launched")
        .task
        .progress();
    searchpanel::close(&mut app, &mut work);
    assert!(work.search_panel.is_none());
    assert!(app.search_panel_slot().is_none());
    assert_eq!(wait_terminal(task).await, norte_proto::TaskState::Cancelled);
}

/// Closing it any other way (its command, closing the slot) cancels too:
/// the next turn's tick finds the run without its panel.
#[tokio::test]
async fn a_run_without_its_panel_is_cancelled_on_the_next_tick() {
    tokio::time::pause();
    let (mut app, backend, mut work, mut resolver, mem) =
        harness_with_tree(&[("a.txt", "aguja\n")]).await;
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_mins(1)));
    app.toggle_search_panel();
    type_in(&mut app, &backend, &mut work, &mut resolver, "ag").await;
    key(&mut app, &backend, &mut work, &mut resolver, KeyCode::Enter).await;
    searchpanel::tick(&mut app, &backend, &mut work).await;
    let task = work
        .search_panel
        .as_ref()
        .expect("launched")
        .task
        .progress();
    app.toggle_search_panel();
    searchpanel::tick(&mut app, &backend, &mut work).await;
    assert!(work.search_panel.is_none());
    assert_eq!(wait_terminal(task).await, norte_proto::TaskState::Cancelled);
}

fn hit_file(path: &str) -> Entry {
    Entry {
        attrs: std::collections::BTreeMap::new(),
        path: vp(path),
        kind: EntryKind::File,
        size: Some(10),
        mtime_ms: None,
    }
}

/// One file with 51 matching lines and 3 more uncounted: 50 shown and a
/// `+4 more` row.
fn hits_with_51_lines() -> SearchHits {
    let lines: Vec<MatchInfo> = (1..=51)
        .map(|n| MatchInfo {
            line: Some(n * 2),
            offset: Some(n * 20),
            preview: Some(format!("línea {n}: la aguja está aquí")),
            ranges: Some(vec![(13, 18)]),
        })
        .collect();
    SearchHits {
        task_id: norte_proto::TaskId::new(1),
        entries: vec![hit_file("file:///casa/notas.txt")],
        matches: Some(vec![lines[0].clone()]),
        all_matches: Some(vec![lines]),
        more: Some(vec![3]),
    }
}

/// The panel with results: a highlighted line, folded filters, a `+N` row.
#[test]
fn snapshot_search_panel_with_results() {
    let mut app = test_app();
    app.render_now_ms = Some(0);
    app.toggle_search_panel();
    let slot = app.search_panel_slot().expect("open");
    let p = app.panes.search_panel_mut(slot).expect("model");
    let t0 = std::time::Instant::now();
    p.edit(SearchField::Content, "aguja".to_owned(), t0);
    let Some(Due::Launch { epoch, .. }) = p.due(t0 + DEBOUNCE, 0) else {
        panic!("launch")
    };
    p.apply_hits(epoch, &hits_with_51_lines());
    p.finished(epoch, norte_frontend::search_status::Outcome::Done);
    // The cursor near the end: the window follows it to the `+N` row.
    p.end();
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    insta::assert_snapshot!(terminal.backend().to_string());
}

/// A hostile file name is masked, never painted raw.
#[test]
fn snapshot_search_panel_masks_a_hostile_name() {
    let mut app = test_app();
    app.render_now_ms = Some(0);
    app.toggle_search_panel();
    let slot = app.search_panel_slot().expect("open");
    let p = app.panes.search_panel_mut(slot).expect("model");
    let t0 = std::time::Instant::now();
    p.edit(SearchField::Name, "mal".to_owned(), t0);
    let Some(Due::Launch { epoch, .. }) = p.due(t0 + DEBOUNCE, 0) else {
        panic!("launch")
    };
    let dir = vp("file:///casa");
    let hostile = dir
        .join(Segment::new(b"mal\x1b[31m\xff.txt".to_vec()).expect("segment"))
        .clone();
    p.apply_hits(
        epoch,
        &SearchHits {
            task_id: norte_proto::TaskId::new(1),
            entries: vec![Entry {
                path: hostile,
                ..hit_file("file:///casa/x")
            }],
            matches: None,
            all_matches: None,
            more: None,
        },
    );
    let mut terminal = Terminal::new(TestBackend::new(100, 16)).expect("terminal");
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    let screen = terminal.backend().to_string();
    assert!(
        !screen.contains('\u{1b}'),
        "no raw escape reaches the screen"
    );
    insta::assert_snapshot!(screen);
}
