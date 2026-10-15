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
use norte_frontend::search_status::Outcome;
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
        if let Some((dir, _)) = name.rsplit_once('/') {
            // Idempotent for a second file in the same folder.
            let _ = mem.mkdir(&vp(&format!("mem:///{dir}"))).await;
        }
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
    let mut stream = futures::stream::pending();
    let mut events = norte_tui::console::Console::detached(&mut stream);
    searchpanel::on_key(
        app,
        backend,
        &mut events,
        work,
        resolver,
        KeyModifiers::NONE,
        code,
    )
    .await;
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

/// F7 ("search here") cancels the run at once; the panel stops saying it
/// is searching even if nothing relaunches after the pause.
#[tokio::test]
async fn f7_ends_the_run_it_cancels() {
    tokio::time::pause();
    let (mut app, backend, mut work, mut resolver, mem) =
        harness_with_tree(&[("a.txt", "aguja\n")]).await;
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_mins(1)));
    app.toggle_search_panel();
    type_in(&mut app, &backend, &mut work, &mut resolver, "ag").await;
    tokio::time::sleep(DEBOUNCE).await;
    searchpanel::tick(&mut app, &backend, &mut work).await;
    assert!(panel(&app).is_running());
    key(&mut app, &backend, &mut work, &mut resolver, KeyCode::F(7)).await;
    assert!(work.search_panel.is_none(), "cancelled");
    assert!(!panel(&app).is_running());
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

// ── A result opens where it matched (Task 8) ────────────────────────────

/// Types `text` into `field`, searches now and drains the run; the keyboard
/// ends in the results with the cursor on the first row.
async fn search_for(
    app: &mut App,
    backend: &Backend,
    work: &mut InFlight,
    resolver: &mut Resolver,
    field: SearchField,
    text: &str,
) {
    app.toggle_search_panel();
    app.search_panel_focus = SearchFocus::Field(field);
    type_in(app, backend, work, resolver, text).await;
    key(app, backend, work, resolver, KeyCode::Enter).await;
    searchpanel::tick(app, backend, work).await;
    loop {
        let hits = work.search_panel.as_mut().expect("run").rx.recv().await;
        let end = hits.is_none();
        searchpanel::drain(app, &mut work.search_panel, hits);
        if end {
            break;
        }
    }
    app.search_panel_focus = SearchFocus::Tree;
}

/// Enter on a matching line opens the viewer on that line, lit.
#[tokio::test]
async fn enter_on_a_line_opens_the_viewer_at_it() {
    let (mut app, backend, mut work, mut resolver, _mem) =
        harness_with_tree(&[("notes", "uno\ndos\naguja aqui\ncuatro\n")]).await;
    let content = SearchField::Content;
    search_for(
        &mut app,
        &backend,
        &mut work,
        &mut resolver,
        content,
        "aguja",
    )
    .await;
    // folder, file, line.
    key(&mut app, &backend, &mut work, &mut resolver, KeyCode::Down).await;
    key(&mut app, &backend, &mut work, &mut resolver, KeyCode::Down).await;
    key(&mut app, &backend, &mut work, &mut resolver, KeyCode::Enter).await;
    let v = app.viewer.as_ref().expect("the viewer opened");
    assert_eq!(v.highlight_line(), Some(2));
    assert_eq!(v.scroll, 2);
    assert_eq!(v.highlight_chars(2), vec![(0, 5)]);
}

/// The mouse's second press on a result goes through `activate`, the same
/// path as Enter: on a line, the viewer opens there.
#[tokio::test]
async fn activating_by_the_mouse_opens_the_line_like_enter() {
    let (mut app, backend, mut work, mut resolver, _mem) =
        harness_with_tree(&[("notes", "uno\ndos\naguja aqui\ncuatro\n")]).await;
    let content = SearchField::Content;
    search_for(
        &mut app,
        &backend,
        &mut work,
        &mut resolver,
        content,
        "aguja",
    )
    .await;
    assert!(!app.search_panel_click(2), "the first press selects");
    assert!(app.search_panel_click(2), "the second activates");
    let mut stream = futures::stream::pending();
    let mut events = norte_tui::console::Console::detached(&mut stream);
    searchpanel::activate(&mut app, &backend, &mut events, &mut work).await;
    let v = app.viewer.as_ref().expect("the viewer opened");
    assert_eq!(v.highlight_line(), Some(2));
}

/// One key through the whole chain, the way the loop sends it.
async fn chain_key(
    app: &mut App,
    backend: &Backend,
    work: &mut InFlight,
    resolvers: &mut [Resolver; 3],
    code: KeyCode,
) {
    let cfg = norte_tui::config::load(&norte_config::Layers { dirs: vec![] }).expect("config");
    let mut cap = norte_tui::mouse::Capture::new();
    let mut stream = futures::stream::pending();
    let mut events = norte_tui::console::Console::detached(&mut stream);
    let [browse, viewer, dialog] = resolvers;
    norte_tui::keys::on_key(
        app,
        backend,
        &mut cap,
        &mut events,
        browse,
        viewer,
        dialog,
        &mut Vec::new(),
        norte_i18n::Lang::Es,
        norte_tui::nav::Mode::default(),
        norte_tui::config::ConfirmQuit::default(),
        &cfg,
        None,
        None,
        work,
        crossterm::event::KeyEvent::new(code, KeyModifiers::NONE),
    )
    .await;
}

/// While the viewer opened from a line is up its keys are the viewer's
/// (scroll, close); closing it gives the keys back to the panel.
#[tokio::test]
async fn the_viewer_over_the_panel_takes_the_keys_and_returns_them() {
    for close in [KeyCode::Esc, KeyCode::Char('q'), KeyCode::F(3)] {
        let (mut app, backend, mut work, mut resolver, _mem) =
            harness_with_tree(&[("notes", "uno\ndos\naguja aqui\ncuatro\n")]).await;
        search_for(
            &mut app,
            &backend,
            &mut work,
            &mut resolver,
            SearchField::Content,
            "aguja",
        )
        .await;
        for code in [KeyCode::Down, KeyCode::Down, KeyCode::Enter] {
            key(&mut app, &backend, &mut work, &mut resolver, code).await;
        }
        let at = panel(&app).cursor();
        let scroll = app.viewer.as_ref().expect("the viewer opened").scroll;
        let src = presets::source("orthodox").expect("the preset exists");
        let kf = norte_frontend::keymap::parse_keymap(src).expect("the preset parses");
        let known: Vec<&str> = CATALOGUE.iter().map(|d| d.name).collect();
        let build = |s| Resolver::new(Effective::build_for(&kf, &[], &known, s).expect("merges"));
        let mut resolvers = [
            build(Screen::Browse),
            build(Screen::Viewer),
            build(Screen::Dialog),
        ];
        chain_key(&mut app, &backend, &mut work, &mut resolvers, KeyCode::Up).await;
        assert_eq!(app.viewer.as_ref().expect("still open").scroll, scroll - 1);
        assert_eq!(panel(&app).cursor(), at, "the results cursor did not move");
        chain_key(&mut app, &backend, &mut work, &mut resolvers, close).await;
        assert!(app.viewer.is_none(), "{close:?} closes the viewer");
        assert_eq!(app.key_owner(), KeyOwner::Search);
    }
}

/// Enter on a file goes to its folder with the cursor on it, and the keys
/// are the panes' again.
#[tokio::test]
async fn enter_on_a_file_goes_to_its_folder() {
    let (mut app, backend, mut work, mut resolver, _mem) =
        harness_with_tree(&[("d/a.txt", "aguja\n"), ("d/b.txt", "nada\n")]).await;
    let name = SearchField::Name;
    search_for(&mut app, &backend, &mut work, &mut resolver, name, "b.txt").await;
    // folder `d`, then its file.
    key(&mut app, &backend, &mut work, &mut resolver, KeyCode::Down).await;
    key(&mut app, &backend, &mut work, &mut resolver, KeyCode::Enter).await;
    assert_eq!(app.focused().dir(), &vp("mem:///d"));
    assert_eq!(
        app.focused().cursor_entry().map(|e| e.path.clone()),
        Some(vp("mem:///d/b.txt"))
    );
    assert_eq!(app.key_owner(), KeyOwner::Panes);
    assert!(app.viewer.is_none());
}

/// A match past the viewer's cap opens a window around it, numbered from
/// its true line.
#[tokio::test]
async fn a_match_past_the_cap_opens_a_window() {
    use norte_frontend::viewer::ViewerTarget;
    let line = "x".repeat(99);
    let mut text = String::new();
    for _ in 0..3000 {
        text.push_str(&line);
        text.push('\n');
    }
    let at = text.len() as u64;
    text.push_str("aguja\nfin\n");
    assert!(at > 256 * 1024);
    let (mut app, backend, _work, _resolver, _mem) = harness_with_tree(&[("big", &text)]).await;
    let mut stream = futures::stream::pending();
    let mut events = norte_tui::console::Console::detached(&mut stream);
    let target = ViewerTarget {
        line: 3001,
        offset: Some(at),
        preview: "aguja".to_owned(),
        ranges: vec![(0, 5)],
    };
    norte_tui::viewer_open::open_viewer_at(
        &mut app,
        &backend,
        &mut events,
        vp("mem:///big"),
        target,
    )
    .await;
    let v = app.viewer.as_ref().expect("the viewer opened");
    assert!(v.first_line() > 1, "a window, not the head");
    let lit = v.highlight_line().expect("the target line is lit");
    assert_eq!(v.first_line() + lit as u64, 3001);
    assert_eq!(v.highlight_chars(lit), vec![(0, 5)]);
}

/// A file an approved previewer claims (`.txt`, syntax highlighting) still
/// opens aimed from a result: the raw view, lit, as in the window — a
/// styled preview has no lit line.
#[tokio::test]
async fn a_result_skips_the_previewer_and_opens_lit() {
    let (mut app, backend, mut work, mut resolver, _mem) =
        harness_with_tree(&[("notes.txt", "uno\ndos\naguja aqui\ncuatro\n")]).await;
    search_for(
        &mut app,
        &backend,
        &mut work,
        &mut resolver,
        SearchField::Content,
        "aguja",
    )
    .await;
    for code in [KeyCode::Down, KeyCode::Down, KeyCode::Enter] {
        key(&mut app, &backend, &mut work, &mut resolver, code).await;
    }
    let v = app.viewer.as_ref().expect("the viewer opened");
    assert_eq!(v.preview_plugin(), None, "the raw view");
    assert_eq!(v.highlight_line(), Some(2));
}

/// A target with no byte offset on a line the head does not hold opens at
/// the top and says so — the shared viewer's rule, the window's too.
#[tokio::test]
async fn a_line_not_held_without_an_offset_opens_at_the_top_and_says_so() {
    use norte_frontend::viewer::ViewerTarget;
    let (mut app, backend, _work, _resolver, _mem) =
        harness_with_tree(&[("notes", "uno\ndos\naguja aqui\ncuatro\n")]).await;
    let mut stream = futures::stream::pending();
    let mut events = norte_tui::console::Console::detached(&mut stream);
    let target = ViewerTarget {
        line: 500,
        offset: None,
        preview: "aguja".to_owned(),
        ranges: vec![(0, 5)],
    };
    norte_tui::viewer_open::open_viewer_at(
        &mut app,
        &backend,
        &mut events,
        vp("mem:///notes"),
        target,
    )
    .await;
    let v = app.viewer.as_ref().expect("the viewer opened");
    assert_eq!((v.scroll, v.highlight_line()), (0, None));
    assert_eq!(
        app.message.as_deref(),
        Some(norte_i18n::t("viewer-target-past-cap").as_str())
    );
}

/// The viewer paints the lit span.
#[tokio::test]
async fn snapshot_viewer_lights_the_match() {
    let (mut app, backend, mut work, mut resolver, _mem) =
        harness_with_tree(&[("notes", "uno\ndos\naguja aqui\ncuatro\n")]).await;
    let content = SearchField::Content;
    search_for(
        &mut app,
        &backend,
        &mut work,
        &mut resolver,
        content,
        "aguja",
    )
    .await;
    for code in [KeyCode::Down, KeyCode::Down, KeyCode::Enter] {
        key(&mut app, &backend, &mut work, &mut resolver, code).await;
    }
    app.render_now_ms = Some(0);
    let mut terminal = Terminal::new(TestBackend::new(60, 8)).expect("terminal");
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    insta::assert_snapshot!(terminal.backend().to_string());
}

// ── Open as listing (phase B, Task 6) ───────────────────────────────────

/// `n` files over ten folders: a walk still streaming after its first
/// batch (nobody drains its channel, so it stalls on the full buffer).
async fn harness_with_big_tree(n: usize) -> (App, Backend, InFlight, Resolver) {
    let (app, backend, work, resolver, mem) = harness_with_tree(&[]).await;
    for i in 0..n {
        let dir = vp(&format!("mem:///d{}", i % 10));
        let _ = mem.mkdir(&dir).await;
        let mut sink = mem
            .write(&vp(&format!("mem:///d{}/f{i:05}.txt", i % 10)))
            .await
            .expect("write opens");
        sink.write(Bytes::from_static(b"x")).await.expect("chunk");
        sink.commit().await.expect("commit");
    }
    (app, backend, work, resolver)
}

/// Types a name query, searches now and drains the panel's FIRST batch.
async fn search_one_batch(
    app: &mut App,
    backend: &Backend,
    work: &mut InFlight,
    resolver: &mut Resolver,
    name: &str,
) {
    app.toggle_search_panel();
    type_in(app, backend, work, resolver, name).await;
    key(app, backend, work, resolver, KeyCode::Enter).await;
    searchpanel::tick(app, backend, work).await;
    let hits = work.search_panel.as_mut().expect("run").rx.recv().await;
    assert!(hits.is_some(), "a first batch");
    searchpanel::drain(app, &mut work.search_panel, hits);
}

/// The results slot the panel opened, if any.
fn results_slot(app: &App) -> Option<norte_frontend::layout::SlotId> {
    let slot = app.focused_slot();
    app.panes
        .browser(slot)
        .is_some_and(Pane::is_results)
        .then_some(slot)
}

/// F9 in the panel opens a results tab with the panel's files; a live run
/// MOVES to the tab (C9): the panel watches along while it shows that
/// query, its next query does not cancel the tab's run, and the tab keeps
/// receiving to the end.
#[tokio::test]
async fn f9_opens_the_listing_and_hands_over_the_live_run() {
    let total = 1_000;
    let (mut app, backend, mut work, mut resolver) = harness_with_big_tree(total).await;
    search_one_batch(&mut app, &backend, &mut work, &mut resolver, "*.txt").await;
    let epoch = work.search_panel.as_ref().expect("live").epoch;
    let shown = panel(&app).listing().expect("a tree").entries.len();
    assert!(shown > 0 && shown < total, "still streaming: {shown}");
    assert_eq!(app.search_panel_focus, SearchFocus::Field(SearchField::Name));
    key(&mut app, &backend, &mut work, &mut resolver, KeyCode::F(9)).await;

    let slot = results_slot(&app).expect("a results tab");
    let tab = app.panes.browser(slot).expect("tab");
    assert_eq!(tab.entries().len(), shown);
    assert_eq!(tab.results().expect("set").search().name, "*.txt");
    assert_eq!(app.key_owner(), KeyOwner::Panes, "focus back to the panes");
    assert!(work.search_panel.is_none(), "the run left the panel");
    let run = work.search.get(slot).expect("the tab adopted the run");
    assert_eq!(run.panel_epoch, Some(epoch));
    let progress = run.task.progress();

    // The panel watches along: a batch of the tab's run lands in both.
    let hits = work.search.get_mut(slot).expect("run").rx.recv().await;
    let batch = hits.as_ref().map_or(0, |h| h.entries.len());
    norte_tui::jobs::drain_search(&mut app, &mut work.search, slot, hits);
    assert_eq!(
        app.panes.browser(slot).expect("tab").entries().len(),
        shown + batch
    );
    assert_eq!(panel(&app).tree().iter().map(|f| f.files.len()).sum::<usize>(), shown + batch);

    // The panel's next query is a NEW run, and does not cancel the tab's.
    app.open_search_panel(); // the keys back in the panel
    assert_eq!(app.key_owner(), KeyOwner::Search);
    app.search_panel_focus = SearchFocus::Field(SearchField::Name);
    key(&mut app, &backend, &mut work, &mut resolver, KeyCode::Backspace).await;
    key(&mut app, &backend, &mut work, &mut resolver, KeyCode::Enter).await;
    searchpanel::tick(&mut app, &backend, &mut work).await;
    let new = work.search_panel.as_ref().expect("a new panel run");
    assert_ne!(new.epoch, epoch);
    assert!(
        matches!(
            progress.borrow().state,
            norte_proto::TaskState::Running | norte_proto::TaskState::Completed { .. }
        ),
        "the tab's run goes on: {:?}",
        progress.borrow().state
    );
    loop {
        let hits = work.search.get_mut(slot).expect("run").rx.recv().await;
        let end = hits.is_none();
        norte_tui::jobs::drain_search(&mut app, &mut work.search, slot, hits);
        if end {
            break;
        }
    }
    let tab = app.panes.browser(slot).expect("tab");
    assert_eq!(tab.entries().len(), total);
    assert_eq!(tab.results().expect("set").outcome(), &Outcome::Done);
}

/// F9 works from the tree focus as well as from an input, through the
/// whole key chain (the shared-chord branch must not take it).
#[tokio::test]
async fn f9_works_from_the_tree() {
    let (mut app, backend, mut work, mut resolver, _mem) =
        harness_with_tree(&[("d/a.txt", "aguja\n"), ("d/b.txt", "nada\n")]).await;
    search_for(
        &mut app,
        &backend,
        &mut work,
        &mut resolver,
        SearchField::Content,
        "aguja",
    )
    .await;
    assert_eq!(app.search_panel_focus, SearchFocus::Tree);
    let src = presets::source("orthodox").expect("the preset exists");
    let kf = norte_frontend::keymap::parse_keymap(src).expect("the preset parses");
    let known: Vec<&str> = CATALOGUE.iter().map(|d| d.name).collect();
    let build = |s| Resolver::new(Effective::build_for(&kf, &[], &known, s).expect("merges"));
    let mut resolvers = [
        build(Screen::Browse),
        build(Screen::Viewer),
        build(Screen::Dialog),
    ];
    chain_key(&mut app, &backend, &mut work, &mut resolvers, KeyCode::F(9)).await;
    assert!(app.menu.is_none(), "not the menu");
    let slot = results_slot(&app).expect("a results tab");
    let tab = app.panes.browser(slot).expect("tab");
    assert_eq!(tab.entries().len(), 1);
    let set = tab.results().expect("set");
    assert_eq!(set.outcome(), &Outcome::Done, "the finished run's outcome");
    assert_eq!(
        set.match_of(&vp("mem:///d/a.txt")).and_then(|m| m.line),
        Some(1),
        "the panel's match context came along"
    );
    assert!(work.search.get(slot).is_none(), "no run to adopt");
}

/// With nothing shown, F9 says so and opens nothing.
#[tokio::test]
async fn f9_with_nothing_to_list_says_so() {
    let (mut app, backend, mut work, mut resolver) = harness().await;
    app.toggle_search_panel();
    let before = app.focused_slot();
    key(&mut app, &backend, &mut work, &mut resolver, KeyCode::F(9)).await;
    assert_eq!(
        app.message.as_deref(),
        Some(norte_i18n::t("search-panel-nothing-to-list").as_str())
    );
    assert_eq!(app.focused_slot(), before, "no tab");
    assert_eq!(app.key_owner(), KeyOwner::Search, "the keys stay");
}

/// The tab that adopted the run is closed: its run is cancelled, and the
/// panel that handed it over stops saying it is searching instead of
/// waiting forever on a run nobody drains.
#[tokio::test]
async fn closing_the_adopting_tab_stops_the_panel_watching() {
    let (mut app, backend, mut work, mut resolver) = harness_with_big_tree(1_000).await;
    search_one_batch(&mut app, &backend, &mut work, &mut resolver, "*.txt").await;
    key(&mut app, &backend, &mut work, &mut resolver, KeyCode::F(9)).await;
    let slot = results_slot(&app).expect("a results tab");
    let progress = work.search.get(slot).expect("adopted").task.progress();
    assert!(panel(&app).is_running(), "watching along");
    app.tab_close();
    norte_tui::refresh::reap_search_runs(&app, &mut work.search);
    assert!(work.search.get(slot).is_none(), "the run left with its tab");
    // Cancelled, or already done filling its channel: either way it ends.
    assert!(wait_terminal(progress).await.is_terminal());
    searchpanel::tick(&mut app, &backend, &mut work).await;
    assert!(!panel(&app).is_running(), "the panel let go of it");
    assert!(panel(&app).listing().is_some(), "what it found stays");
}

/// The status sentence is chosen per count: Fluent never matches a plural
/// on string args ("1 ficheros en 1 carpetas").
#[test]
fn the_status_says_one_and_many() {
    use norte_i18n::Lang::{En, Es};
    let run = |lang, key, files, folders| searchpanel::run_text(lang, key, files, folders);
    assert_eq!(run(Es, "search-panel-done", 1, 1), "1 fichero en 1 carpeta");
    assert_eq!(
        run(Es, "search-panel-done", 3, 3),
        "3 ficheros en 3 carpetas"
    );
    assert_eq!(
        run(Es, "search-panel-running", 1, 3),
        "1 fichero en 3 carpetas · buscando…"
    );
    assert_eq!(
        run(Es, "search-panel-cancelled", 1, 1),
        "1 fichero · detenida"
    );
    assert_eq!(run(En, "search-panel-done", 1, 1), "1 file in 1 folder");
    assert_eq!(run(En, "search-panel-done", 3, 3), "3 files in 3 folders");
    assert_eq!(
        run(En, "search-panel-running", 3, 1),
        "3 files in 1 folder · searching…"
    );
    assert_eq!(run(En, "search-panel-cancelled", 1, 1), "1 file · stopped");
}
