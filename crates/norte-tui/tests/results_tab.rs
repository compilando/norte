//! Search, phase B (Task 4): the Alt+F7 dialog opens a RESULTS TAB instead of
//! taking over the focused pane. The run is filed under the tab's slot, and
//! it dies with the tab (Review Focus 1); transfers from and into a results
//! tab follow C4 and C5; Enter opens the hit's folder in a new tab (C10).

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt as _;
use norte_core::Engine;
use norte_core::backend::Backend;
use norte_frontend::results::{MAX_HITS, ResultSet, SavedSearch};
use norte_frontend::search::SearchForm;
use norte_i18n::t;
use norte_proto::{Entry, EntryKind, VPath};
use norte_testkit::MemProvider;
use norte_tui::app::{App, Modal, Pane, TransferKind};
use norte_tui::jobs::{InFlight, drain_search, launch_results, open_hit_folder};
use norte_tui::refresh::reap_search_runs;
use norte_tui::ui;
use norte_vfs::Provider;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn vp(wire: &str) -> VPath {
    // Fixed language: the assertions compare localized text.
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("valid wire")
}

/// An app over the embedded engine's memory tree, with `files` in it.
async fn harness_with_tree(files: &[(&str, &str)]) -> (App, Backend, InFlight, Arc<MemProvider>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    for (name, content) in files {
        write_file(&mem, name, content).await;
    }
    let root = vp("mem:///");
    let app = App::new(Pane::new(root.clone(), vec![]), Pane::new(root, vec![]));
    (
        app,
        Backend::Embedded(Arc::new(engine)),
        InFlight::default(),
        mem,
    )
}

async fn write_file(mem: &MemProvider, name: &str, content: &str) {
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

/// `n` files spread over ten folders: a walk that is still going when the
/// test cancels it (nobody drains its channel, so it stalls on the full
/// buffer).
async fn harness_with_big_tree(n: usize) -> (App, Backend, InFlight, Arc<MemProvider>) {
    let (app, backend, work, mem) = harness_with_tree(&[]).await;
    for i in 0..n {
        write_file(&mem, &format!("d{}/f{i:05}.txt", i % 10), "x").await;
    }
    (app, backend, work, mem)
}

async fn read_all(mem: &MemProvider, path: &VPath) -> Vec<u8> {
    let mut stream = mem.read(path, None).await.expect("read opens");
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk"));
    }
    out
}

/// The task's terminal state, through its progress `watch` (a `TaskRef` is
/// not `Clone`).
async fn wait_terminal(
    mut rx: tokio::sync::watch::Receiver<norte_proto::TaskProgress>,
) -> norte_proto::TaskState {
    tokio::time::timeout(std::time::Duration::from_mins(1), async {
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

fn saved_content(root: &VPath, content: &str) -> SavedSearch {
    let mut s = SavedSearch::from_form(&SearchForm::new(), root.clone());
    content.clone_into(&mut s.content);
    s
}

fn saved_name(root: &VPath, name: &str) -> SavedSearch {
    let mut s = SavedSearch::from_form(&SearchForm::new(), root.clone());
    name.clone_into(&mut s.name);
    s
}

/// Drains `slot`'s run until its channel closes.
async fn drain_until_closed(
    app: &mut App,
    work: &mut InFlight,
    slot: norte_frontend::layout::SlotId,
) {
    loop {
        let hits = work.search.get_mut(slot).expect("run").rx.recv().await;
        let end = hits.is_none();
        drain_search(app, &mut work.search, slot, hits);
        if end {
            break;
        }
    }
}

fn console_stream() -> futures::stream::Pending<std::io::Result<crossterm::event::Event>> {
    futures::stream::pending()
}

/// Puts the focused pane's cursor on `path`.
fn cursor_on(app: &mut App, path: &VPath) {
    let i = app
        .focused()
        .entries()
        .iter()
        .position(|e| &e.path == path)
        .expect("the hit is listed");
    app.focused_mut().set_cursor(i);
}

/// Alt+F7's Enter opens a TAB: the pane that was focused keeps its folder,
/// a new tab shows the hits with the 🔍 title, and the run is filed under
/// the NEW slot.
#[tokio::test]
async fn the_dialog_opens_a_results_tab() {
    let (mut app, backend, mut work, _r) =
        harness_with_tree(&[("a/x.txt", "aguja\n"), ("b/y.txt", "aguja\n")]).await;
    let before = app.focused().dir().clone();
    let old_slot = app.focused_slot();
    let search = saved_content(&before, "aguja");
    let slot = launch_results(&mut app, &backend, &mut work.search, search, None)
        .await
        .expect("tab");
    assert_ne!(slot, old_slot);
    assert_eq!(app.focused_slot(), slot, "the new tab is focused");
    assert_eq!(
        app.panes.browser(old_slot).expect("old").dir(),
        &before,
        "the folder tab is untouched"
    );
    assert!(
        !app.panes.browser(old_slot).expect("old").is_results(),
        "and it is still a folder"
    );
    drain_until_closed(&mut app, &mut work, slot).await;
    let tab = app.panes.browser(slot).expect("tab");
    assert!(tab.is_results());
    assert_eq!(tab.entries().len(), 2);
    let strip = ui::tab_strip_for(&app, app.focus()).expect("a group");
    assert!(
        strip.titles.iter().any(|t| t.starts_with("🔍 aguja (2)")),
        "{:?}",
        strip.titles
    );
}

/// Review Focus 1: closing the tab cancels its run; navigating it to a
/// folder cancels its run and no late batch lands in the folder listing.
#[tokio::test]
async fn closing_or_leaving_the_tab_cancels_its_run() {
    let (mut app, backend, mut work, _r) = harness_with_big_tree(2_000).await;
    let root = app.focused().dir().clone();
    let slot = launch_results(
        &mut app,
        &backend,
        &mut work.search,
        saved_name(&root, "*"),
        None,
    )
    .await
    .expect("tab");
    let progress = work.search.get(slot).expect("run").task.progress();
    app.tab_close();
    reap_search_runs(&app, &mut work.search);
    assert!(work.search.get(slot).is_none(), "the run left with its tab");
    assert_eq!(
        wait_terminal(progress).await,
        norte_proto::TaskState::Cancelled
    );

    let slot = launch_results(
        &mut app,
        &backend,
        &mut work.search,
        saved_name(&root, "*"),
        None,
    )
    .await
    .expect("tab");
    let progress = work.search.get(slot).expect("run").task.progress();
    app.panes
        .browser_mut(slot)
        .expect("tab")
        .set_listing(root.clone(), vec![]);
    reap_search_runs(&app, &mut work.search);
    assert!(work.search.get(slot).is_none(), "nothing left to drain");
    assert_eq!(
        wait_terminal(progress).await,
        norte_proto::TaskState::Cancelled
    );
    let tab = app.panes.browser(slot).expect("tab");
    assert!(!tab.is_results());
    assert!(tab.entries().is_empty(), "no late batch in the folder");
}

/// C10: Enter on a hit opens its folder in a NEW tab with the cursor on
/// it; the results tab is still there.
#[tokio::test]
async fn enter_opens_the_hit_folder_in_a_new_tab() {
    let (mut app, backend, mut work, _r) = harness_with_tree(&[
        ("a/x.txt", "aguja\n"),
        ("b/w.txt", "nada\n"),
        ("b/y.txt", "aguja\n"),
        ("b/z.txt", "nada\n"),
    ])
    .await;
    let root = app.focused().dir().clone();
    let results = launch_results(
        &mut app,
        &backend,
        &mut work.search,
        saved_content(&root, "aguja"),
        None,
    )
    .await
    .expect("tab");
    drain_until_closed(&mut app, &mut work, results).await;
    let hit = vp("mem:///b/y.txt");
    cursor_on(&mut app, &hit);

    let mut stream = console_stream();
    let mut events = norte_tui::console::Console::detached(&mut stream);
    open_hit_folder(
        &mut app,
        &backend,
        &mut events,
        &mut work.fill,
        &mut work.decorate,
        &mut work.probed,
        &mut work.search,
    )
    .await;

    let folder = app.focused_slot();
    assert_ne!(folder, results, "a NEW tab has the focus");
    let (tabs, active) = app.layout.tabs_of(folder).expect("a group");
    assert_eq!(
        tabs.len(),
        3,
        "the folder tab, the results and the hit's folder"
    );
    assert_eq!(tabs[active], folder);
    assert_eq!(app.focused().dir(), &vp("mem:///b"));
    assert!(!app.focused().is_results());
    assert_eq!(
        app.focused().selected().map(|e| e.path.clone()),
        Some(hit),
        "the cursor is on the hit"
    );
    assert!(
        app.panes
            .browser(results)
            .expect("still there")
            .is_results(),
        "the results tab stays"
    );
}

/// Review Focus 2 / C4 / C5: from a results tab into the ROOT is allowed
/// unless a marked hit already lives there; INTO a results tab refuses.
#[tokio::test]
async fn transfer_rules_for_results_tabs() {
    let (mut app, backend, mut work, _r) = harness_with_tree(&[
        ("a/x.txt", "aguja\n"),
        ("b/y.txt", "aguja\n"),
        ("r.txt", "aguja\n"),
    ])
    .await;
    let root = app.focused().dir().clone();
    // The other side is a folder tab at the root too, with one file listed
    // so that a transfer FROM it is not empty.
    let r_entry = Entry {
        attrs: std::collections::BTreeMap::new(),
        path: vp("mem:///r.txt"),
        kind: EntryKind::File,
        size: Some(6),
        mtime_ms: None,
    };
    app.panes[1].set_listing(root.clone(), vec![r_entry]);
    let slot = launch_results(
        &mut app,
        &backend,
        &mut work.search,
        saved_content(&root, "aguja"),
        None,
    )
    .await
    .expect("tab");
    drain_until_closed(&mut app, &mut work, slot).await;
    assert_eq!(app.focus(), 0, "the results tab is on the left");
    assert_eq!(app.panes[1].dir(), &root, "the destination is the root");

    // Mark the two hits in subfolders.
    for p in ["mem:///a/x.txt", "mem:///b/y.txt"] {
        cursor_on(&mut app, &vp(p));
        app.focused_mut().toggle_mark();
    }
    app.open_transfer(TransferKind::Copy, 0, 1, None);
    match &app.modal {
        Some(Modal::ConfirmTransfer { items, to, .. }) => {
            assert_eq!(items.len(), 2);
            assert_eq!(to, &root, "into the search root");
        }
        other => panic!("expected the transfer confirm, got {other:?}"),
    }
    app.modal = None;

    // Plus one that already lives in the root: refused, no modal.
    cursor_on(&mut app, &vp("mem:///r.txt"));
    app.focused_mut().toggle_mark();
    app.message = None;
    app.open_transfer(TransferKind::Copy, 0, 1, None);
    assert!(app.modal.is_none(), "no modal");
    assert_eq!(app.message, Some(t("results-already-there")));

    // A results tab as the DESTINATION: refused before anything else.
    app.message = None;
    app.set_focus(1);
    app.open_transfer(TransferKind::Copy, 1, 0, None);
    assert!(app.modal.is_none(), "no modal");
    assert_eq!(app.message, Some(t("msg-pane-not-a-location")));
}

/// Review Focus 3: two hits with the same name copied to one folder — the
/// second job meets the first's file and goes through the collision queue;
/// nothing is overwritten.
#[tokio::test]
async fn same_named_hits_meet_the_collision_policy() {
    let (mut app, backend, _work, mem) =
        harness_with_tree(&[("a/n.txt", "A"), ("b/n.txt", "B")]).await;
    mem.mkdir(&vp("mem:///dest")).await.expect("dest");
    let items = [vp("mem:///a/n.txt"), vp("mem:///b/n.txt")];
    norte_tui::mutations::submit_transfers(
        &mut app,
        &backend,
        TransferKind::Copy,
        &items,
        &vp("mem:///dest"),
        norte_core::TransferOptions::default(),
    )
    .await;

    let mut stream = console_stream();
    let mut events = norte_tui::console::Console::detached(&mut stream);
    let collisions = |app: &App| {
        app.pending_collisions.len()
            + usize::from(matches!(app.modal, Some(Modal::Collision { .. })))
    };
    for _ in 0..500 {
        let _ = norte_tui::refresh::on_tick(&mut app, &backend, &mut events).await;
        if collisions(&app) > 0 && !app.board.has_active() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(collisions(&app), 1, "the second copy is a collision");
    let landed = read_all(&mem, &vp("mem:///dest/n.txt")).await;
    assert!(
        landed == b"A" || landed == b"B",
        "one of the two landed whole: {landed:?}"
    );
    // The one that collided is the OTHER one, and it was not written.
    let retry_from = match &app.modal {
        Some(Modal::Collision { retry }) => retry.from.clone(),
        _ => app.pending_collisions[0].from.clone(),
    };
    let expected_winner: &[u8] = if retry_from == items[0] { b"B" } else { b"A" };
    assert_eq!(landed, expected_winner, "nothing was overwritten");
}

/// Runs the tick (and the refresh it triggers) until the board has no live
/// task, then once more the refresh a finished mutation fires: the order in
/// which concurrent tasks finish is not fixed, and the last refresh is the
/// one that must leave the tab settled.
async fn settle_board(app: &mut App, backend: &Backend) {
    let mut stream = console_stream();
    let mut events = norte_tui::console::Console::detached(&mut stream);
    for _ in 0..500 {
        let _ = norte_tui::refresh::on_tick(app, backend, &mut events).await;
        if !app.board.has_active() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(!app.board.has_active(), "the board settles");
    let _ = norte_tui::refresh::on_tick(app, backend, &mut events).await;
    let _ = norte_tui::refresh::refresh_panes(app, backend, &mut events).await;
}

fn hit_paths(app: &App, slot: norte_frontend::layout::SlotId) -> Vec<VPath> {
    let mut v: Vec<VPath> = app
        .panes
        .browser(slot)
        .expect("tab")
        .real_entries()
        .iter()
        .map(|e| e.path.clone())
        .collect();
    v.sort();
    v
}

fn expected_of(app: &App, slot: norte_frontend::layout::SlotId) -> Vec<VPath> {
    app.panes
        .browser(slot)
        .and_then(Pane::results)
        .expect("still a results tab")
        .expected()
}

/// C2: `pane.refresh` on a results tab re-runs the SAME query and picks
/// up a file created since.
#[tokio::test]
async fn refresh_re_runs_the_query() {
    let (mut app, backend, mut work, mem) = harness_with_tree(&[("a/x.txt", "aguja\n")]).await;
    let root = app.focused().dir().clone();
    let slot = launch_results(
        &mut app,
        &backend,
        &mut work.search,
        saved_content(&root, "aguja"),
        None,
    )
    .await
    .expect("tab");
    drain_until_closed(&mut app, &mut work, slot).await;
    assert_eq!(app.focused().entries().len(), 1);
    write_file(&mem, "b/y.txt", "aguja\n").await;

    let cfg = norte_tui::config::load(&norte_config::Layers { dirs: vec![] }).expect("config");
    let mut cap = norte_tui::mouse::Capture::new();
    let mut stream = console_stream();
    let mut events = norte_tui::console::Console::detached(&mut stream);
    assert_eq!(app.focused_slot(), slot, "the tab has the focus");
    let _ = norte_tui::dispatch::dispatch(
        &mut app,
        &backend,
        &mut events,
        &[],
        norte_i18n::Lang::Es,
        norte_tui::nav::Mode::default(),
        norte_tui::config::ConfirmQuit::default(),
        &cfg,
        norte_tui::keymap::Command::PaneRefresh,
    )
    .await;
    // The turn header launches what dispatch left requested.
    norte_tui::turn::drain_pending(&mut app, &backend, &mut cap, &mut events, &mut work).await;
    drain_until_closed(&mut app, &mut work, slot).await;

    let tab = app.panes.browser(slot).expect("tab");
    assert!(tab.is_results(), "still a results tab");
    assert_eq!(
        hit_paths(&app, slot),
        vec![vp("mem:///a/x.txt"), vp("mem:///b/y.txt")],
        "the same query, run again, finds the new file"
    );
    assert_eq!(
        app.panes
            .browser(slot)
            .and_then(Pane::results)
            .map(|r| r.outcome().clone()),
        Some(norte_frontend::search_status::Outcome::Done)
    );
}

/// C11: deleting a hit from a results tab removes it from the tab when
/// the task finishes; the other hits stay; nothing is re-listed.
#[tokio::test]
async fn a_deleted_hit_leaves_the_tab() {
    let (mut app, backend, mut work, _mem) =
        harness_with_tree(&[("a/x.txt", "aguja\n"), ("b/y.txt", "aguja\n")]).await;
    let root = app.focused().dir().clone();
    let slot = launch_results(
        &mut app,
        &backend,
        &mut work.search,
        saved_content(&root, "aguja"),
        None,
    )
    .await
    .expect("tab");
    drain_until_closed(&mut app, &mut work, slot).await;
    let x = vp("mem:///a/x.txt");
    cursor_on(&mut app, &x);
    app.focused_mut().toggle_mark();
    norte_tui::mutations::submit_deletes(&mut app, &backend, std::slice::from_ref(&x), true).await;
    assert_eq!(
        expected_of(&app, slot),
        vec![x.clone()],
        "the source is expected"
    );

    settle_board(&mut app, &backend).await;

    let tab = app.panes.browser(slot).expect("tab");
    assert!(tab.is_results(), "still a results tab, not a folder");
    assert_eq!(
        hit_paths(&app, slot),
        vec![vp("mem:///b/y.txt")],
        "the deleted hit left"
    );
    assert!(expected_of(&app, slot).is_empty(), "nothing left to check");
}

/// C11: a moved hit leaves; a hit whose move FAILED stays.
#[tokio::test]
async fn a_moved_hit_leaves_and_a_failed_move_stays() {
    let (mut app, backend, mut work, _mem) = harness_with_tree(&[
        ("a/x.txt", "aguja\n"),
        ("b/y.txt", "aguja\n"),
        // Already in the destination, under the name of the second hit.
        ("dest/y.txt", "otro\n"),
    ])
    .await;
    let root = app.focused().dir().clone();
    let slot = launch_results(
        &mut app,
        &backend,
        &mut work.search,
        saved_content(&root, "aguja"),
        None,
    )
    .await
    .expect("tab");
    drain_until_closed(&mut app, &mut work, slot).await;
    let (x, y) = (vp("mem:///a/x.txt"), vp("mem:///b/y.txt"));
    assert_eq!(hit_paths(&app, slot), vec![x.clone(), y.clone()]);
    for p in [&x, &y] {
        cursor_on(&mut app, p);
        app.focused_mut().toggle_mark();
    }
    norte_tui::mutations::submit_transfers(
        &mut app,
        &backend,
        TransferKind::Move,
        &[x.clone(), y.clone()],
        &vp("mem:///dest"),
        norte_core::TransferOptions::default(),
    )
    .await;
    assert_eq!(expected_of(&app, slot), vec![x.clone(), y.clone()]);

    settle_board(&mut app, &backend).await;
    // The second move met `dest/y.txt`: the collision dialog asks, and the
    // reader answers "skip" — the same retry `on_dialog_key` sends.
    app.open_next_pending();
    let retry = match app.modal.take() {
        Some(Modal::Collision { retry }) => retry,
        other => panic!("expected the collision dialog, got {other:?}"),
    };
    assert_eq!(retry.from, y);
    let opts = norte_core::TransferOptions {
        on_collision: norte_proto::CollisionPolicy::Skip,
        ..retry.opts
    };
    let _ = norte_tui::mutations::submit_transfer(
        &mut app, &backend, retry.kind, retry.from, retry.to, opts,
    )
    .await;
    settle_board(&mut app, &backend).await;

    assert!(app.panes.browser(slot).expect("tab").is_results());
    assert_eq!(
        hit_paths(&app, slot),
        vec![y],
        "the moved hit left; the skipped one stays"
    );
    assert!(expected_of(&app, slot).is_empty(), "nothing left to check");
}

/// Review Focus 4: the 🔍 title is two cells wide; the click zones of the
/// tab strip are measured with the painted width, so a click on the third
/// tab selects the third tab. The query is hostile and mixes CJK (two
/// cells), an emoji (two cells) and a combining mark (zero cells).
#[test]
fn the_results_tab_title_measures_its_emoji() {
    let root = vp("file:///casa");
    let mut app = App::new(
        Pane::new(root.clone(), vec![]),
        Pane::new(root.clone(), vec![]),
    );
    app.chrome.panel_bar_position = Some(norte_config::PanelBarPosition::Top);
    // [casa, tercera] and then the results between them.
    app.tab_new();
    let third = app.focused_slot();
    app.panes
        .browser_mut(third)
        .expect("tab")
        .set_listing(vp("file:///tercera"), vec![]);
    app.tab_goto(1);
    let mut saved = SavedSearch::from_form(&SearchForm::new(), root.clone());
    saved.content = "\u{1b}[31m漢字🎉e\u{301}\u{202e}x".to_owned();
    let hit = Entry {
        attrs: std::collections::BTreeMap::new(),
        path: vp("file:///casa/sub/x.txt"),
        kind: EntryKind::File,
        size: Some(1),
        mtime_ms: None,
    };
    let results = app.open_results_tab(ResultSet::new(saved, MAX_HITS), vec![hit]);
    let (tabs, active) = app.layout.tabs_of(results).expect("group");
    assert_eq!(tabs, vec![tabs[0], results, third]);
    assert_eq!(active, 1);

    let (w, h) = (100, 30);
    let area = ratatui::layout::Rect::new(0, 0, w, h);
    let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
    ui::before_frame(&mut app, area);
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    // Close the frame like the run loop: the mouse resolves against the
    // zones of the frame just painted.
    let geometry = ui::pane_geometry(&app, area);
    let zones = norte_tui::mouse::FrameZones {
        tabs: ui::tab_zones(&app, area),
        menus: ui::menu_zones(&app, area),
        panels: ui::panel_zones(&app, area),
        keys: ui::key_zones(&app, area),
        modal: ui::modal_zones(&app, area),
        places: ui::places_zones(&app, area),
        tree: ui::tree_zones(&app, area),
        extensions: ui::extension_zones(&app, area),
        help: ui::help_zones(&app, area),
        session: ui::session_zone(&app, area),
        status_items: ui::status_item_zones(&app, area),
        borders: ui::resize_borders(&app, area),
        slots: ui::panel_slots(&app, area),
    };
    norte_tui::mouse::after_frame(&mut app, geometry, zones);
    let strip = ui::tab_strip_for(&app, 0).expect("strip");
    assert!(
        strip.titles[1].starts_with("! 🔍 "),
        "masked, so marked hostile: {:?}",
        strip.titles
    );
    let row = ui::tab_zones(&app, area)
        .first()
        .map(|z| z.row)
        .expect("zones");
    // The painted cells of the bar's row, by column.
    let buf = terminal.backend().buffer();
    let cells: Vec<String> = (0..w)
        .map(|x| {
            buf.cell((x, row))
                .map(|c| c.symbol().to_owned())
                .unwrap_or_default()
        })
        .collect();
    let painted: String = cells.concat();
    assert!(!painted.contains('\u{1b}'), "no ESC painted: {painted:?}");
    assert!(!painted.contains('\u{202e}'), "no RLO painted: {painted:?}");
    assert!(painted.contains('🔍'), "{painted:?}");
    // A wide char takes its cell and blanks the next one in the buffer.
    assert!(painted.contains("漢 字"), "{painted:?}");
    assert!(
        cells.iter().any(|c| c == "e\u{301}"),
        "the combining mark rides on its base cell: {painted:?}"
    );
    // The third title's first cell, found in the BUFFER (not in the zones).
    let x = (0..cells.len())
        .find(|&x| {
            "tercera"
                .chars()
                .enumerate()
                .all(|(k, c)| cells.get(x + k).is_some_and(|s| s == &c.to_string()))
        })
        .expect("the third title is painted");
    click(&mut app, x, row);
    let (_, active) = app.layout.tabs_of(third).expect("group");
    assert_eq!(
        active, 2,
        "the click on the third title selects the third tab"
    );
    // And the LAST painted cell of the results title (its `)`) selects the
    // results tab: a zone measured in chars instead of cells would end
    // three cells early (🔍 +1, 漢字 +2, 🎉 +1, the combining mark −1), and
    // this click would fall into the third tab's zone.
    let lens = cells.iter().position(|c| c == "🔍").expect("the lens");
    let close = (lens..cells.len())
        .find(|&x| cells[x] == ")")
        .expect("the results title ends in `)`");
    click(&mut app, close, row);
    let (_, active) = app.layout.tabs_of(third).expect("group");
    assert_eq!(
        active, 1,
        "the click on the results title's last cell selects the results tab"
    );
}

/// A left click at painted column `x` of `row`.
fn click(app: &mut App, x: usize, row: u16) {
    let _ = norte_tui::mouse::handle(
        app,
        crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: u16::try_from(x).expect("fits"),
            row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        },
    );
}

/// C5 (review of Task 4): a split's pieces go to the other visible pane;
/// when that pane is a results tab they would land in its search root, so
/// it refuses — on opening and on confirming.
#[tokio::test]
async fn a_split_into_a_results_tab_is_refused() {
    let (mut app, backend, mut work, _r) =
        harness_with_tree(&[("a/x.txt", "aguja\n"), ("r.txt", "nada\n")]).await;
    let root = app.focused().dir().clone();
    let r_entry = Entry {
        attrs: std::collections::BTreeMap::new(),
        path: vp("mem:///r.txt"),
        kind: EntryKind::File,
        size: Some(5),
        mtime_ms: None,
    };
    app.panes[1].set_listing(root.clone(), vec![r_entry]);
    let slot = launch_results(
        &mut app,
        &backend,
        &mut work.search,
        saved_content(&root, "aguja"),
        None,
    )
    .await
    .expect("tab");
    drain_until_closed(&mut app, &mut work, slot).await;
    assert_eq!(app.focus(), 0, "the results tab is on the left");
    app.set_focus(1);
    cursor_on(&mut app, &vp("mem:///r.txt"));
    assert_eq!(app.split_dest_pane(), 0, "the pieces would go to the tab");

    app.message = None;
    app.open_split();
    assert!(app.modal.is_none(), "no dialog");
    assert_eq!(app.message, Some(t("msg-pane-not-a-location")));

    // A dialog already open when the other side became a results tab.
    app.modal = Some(Modal::Split {
        size: "10M".to_owned(),
        error: None,
    });
    assert!(app.split_confirm().is_none(), "nothing to submit");
    match &app.modal {
        Some(Modal::Split { error, .. }) => {
            assert_eq!(
                error.as_deref(),
                Some(t("msg-pane-not-a-location").as_str())
            );
        }
        other => panic!("the dialog stays with its error, got {other:?}"),
    }
}

/// C5 (review of Task 4): packing from a results tab writes the archive
/// next to the FIRST marked hit, like combine-files writes next to its
/// first piece — not into the search root. The names inside stay relative
/// to the root, as the Folder column shows them.
#[tokio::test]
async fn packing_from_a_results_tab_writes_next_to_the_first_hit() {
    let (mut app, backend, mut work, _r) =
        harness_with_tree(&[("a/x.txt", "aguja\n"), ("b/y.txt", "aguja\n")]).await;
    let root = app.focused().dir().clone();
    let slot = launch_results(
        &mut app,
        &backend,
        &mut work.search,
        saved_content(&root, "aguja"),
        None,
    )
    .await
    .expect("tab");
    drain_until_closed(&mut app, &mut work, slot).await;
    for p in ["mem:///a/x.txt", "mem:///b/y.txt"] {
        cursor_on(&mut app, &vp(p));
        app.focused_mut().toggle_mark();
    }
    let first = app.focused().marked_paths()[0].clone();
    app.open_pack();
    assert!(
        matches!(app.modal, Some(Modal::Pack { .. })),
        "{:?}",
        app.message
    );
    let params = app.pack_confirm().expect("params");
    assert_eq!(
        params.dest.parent(),
        first.parent(),
        "next to the first marked hit"
    );
    assert_ne!(params.dest.parent().as_ref(), Some(&root), "not the root");
    assert_eq!(params.base, root, "names relative to the search root");
}

/// C10 (review of Task 4): Esc while the hit's folder is still listing
/// closes the new tab, as a failed listing does — an empty tab at the
/// folder would read as an empty folder.
#[tokio::test]
async fn cancelling_the_hit_folder_listing_closes_the_new_tab() {
    let (mut app, backend, mut work, mem) = harness_with_tree(&[("a/x.txt", "aguja\n")]).await;
    let root = app.focused().dir().clone();
    let results = launch_results(
        &mut app,
        &backend,
        &mut work.search,
        saved_content(&root, "aguja"),
        None,
    )
    .await
    .expect("tab");
    drain_until_closed(&mut app, &mut work, results).await;
    cursor_on(&mut app, &vp("mem:///a/x.txt"));
    // A listing that takes a minute, and a console whose stream ends at
    // once: the wait is abandoned, as with Esc.
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_mins(1)));
    let mut stream = futures::stream::empty();
    let mut events = norte_tui::console::Console::detached(&mut stream);
    open_hit_folder(
        &mut app,
        &backend,
        &mut events,
        &mut work.fill,
        &mut work.decorate,
        &mut work.probed,
        &mut work.search,
    )
    .await;
    let (tabs, active) = app.layout.tabs_of(results).expect("a group");
    assert_eq!(
        tabs.len(),
        2,
        "the folder tab and the results, nothing else"
    );
    assert_eq!(tabs[active], results, "back on the results tab");
    assert!(app.focused().is_results());
}

/// Review Focus 1 (review of Task 4): a batch that arrives after the tab
/// became a folder does not land in it; the run is dropped.
#[tokio::test]
async fn a_late_batch_does_not_land_in_the_folder() {
    let (mut app, backend, mut work, _r) =
        harness_with_tree(&[("a/x.txt", "aguja\n"), ("b/y.txt", "aguja\n")]).await;
    let root = app.focused().dir().clone();
    let slot = launch_results(
        &mut app,
        &backend,
        &mut work.search,
        saved_content(&root, "aguja"),
        None,
    )
    .await
    .expect("tab");
    let batch = work
        .search
        .get_mut(slot)
        .expect("run")
        .rx
        .recv()
        .await
        .expect("a batch of hits");
    assert!(!batch.entries.is_empty());
    app.panes
        .browser_mut(slot)
        .expect("tab")
        .set_listing(root.clone(), vec![]);
    drain_search(&mut app, &mut work.search, slot, Some(batch));
    let tab = app.panes.browser(slot).expect("tab");
    assert!(!tab.is_results());
    assert!(tab.entries().is_empty(), "the late batch did not land");
    assert!(work.search.get(slot).is_none(), "the run was dropped");
}
