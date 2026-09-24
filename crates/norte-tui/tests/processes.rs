//! The processes panel: that keys ARRIVE.
//!
//! The tests that existed asserted `key_owner()`, and that is exactly what
//! left the #243 hole invisible: `KeyOwner` was set, the focus border
//! painted, and not one key reached the panel — the arrows moved the
//! listing behind it and F8 opened the delete dialog over its selection.
//!
//! Here the key enters the way it really does: preset → the `dialog`
//! screen's `Effective` → `Resolver` → command → the panel's dispatch.
//!
//! And what the panel SAYS, which is the other half: a row that only
//! carries the class and a task number tells nothing, and a finished one
//! that stays forever turns the panel into a history.

use norte_frontend::keymap::{CATALOGUE, Chord, Effective, Resolution, Resolver, Screen, presets};
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{ALLOW_PROCESSES, App, KeyOwner, Pane};

fn vp(wire: &str) -> VPath {
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
    App::new(
        Pane::new(dir.clone(), entries(&dir)),
        Pane::new(dir.clone(), entries(&dir)),
    )
}

/// A preset's effective keymap for the `dialog` screen, the one that
/// resolves while the keyboard is inside a panel.
fn dialog(preset: &str) -> Effective {
    let known: Vec<&str> = CATALOGUE.iter().map(|d| d.name).collect();
    let src = presets::source(preset).expect("the preset exists");
    let kf = norte_frontend::keymap::parse_keymap(src).expect("the preset parses");
    Effective::build_for(&kf, &[], &known, Screen::Dialog).expect("the preset merges")
}

/// The key SEQUENCE a preset binds to `cmd` on the `dialog` screen,
/// available there.
fn key_for(eff: &Effective, cmd: &str) -> Vec<Chord> {
    eff.bindings_all_seq()
        .into_iter()
        .find(|(_, c, avail)| {
            *c == cmd && matches!(avail, norte_frontend::keymap::Availability::Here)
        })
        .map_or_else(
            || panic!("no preset binds {cmd} in dialog"),
            |(seq, _, _)| seq.to_vec(),
        )
}

/// Feeds a key through the real path and returns what the panel says.
fn pulsar(app: &mut App, resolver: &mut Resolver, seq: Vec<Chord>) -> Option<String> {
    let mut last = None;
    for chord in seq {
        match resolver.push(chord) {
            Resolution::Run { command, .. } => last = app.processes_command(&command),
            Resolution::Pending(_) | Resolution::Counting(_) => {}
            other => panic!("the key does not resolve to a command: {other:?}"),
        }
    }
    last
}

/// Enter on the panel CANCELS, which is what the CHANGELOG and both help
/// topics had been promising with no implementation behind it. With no
/// tasks it says there are none — what it cannot do is fall through to the
/// listing behind it.
#[test]
fn confirming_acts_on_the_pane_and_not_on_the_listing() {
    let eff = dialog("orthodox");
    let mut resolver = Resolver::new(eff.clone());
    let mut app = test_app();
    app.toggle_processes();
    assert_eq!(
        app.key_owner(),
        KeyOwner::Processes,
        "the panel has the keys"
    );

    let msg = pulsar(&mut app, &mut resolver, key_for(&eff, "dialog.confirm"));
    assert_eq!(
        msg,
        Some(norte_i18n::t("msg-no-tasks")),
        "the PANEL answers: with no tasks, there is nothing to cancel"
    );
    assert!(
        app.modal.is_none(),
        "and it opens nothing from the listing behind it"
    );
}

/// Escape releases the keyboard without closing the panel, and its own key
/// closes it from within: the third press of open → focus → close.
#[test]
fn cancelling_releases_the_keyboard_and_its_key_closes_from_inside() {
    let eff = dialog("orthodox");
    let mut resolver = Resolver::new(eff.clone());
    let mut app = test_app();

    app.toggle_processes();
    pulsar(&mut app, &mut resolver, key_for(&eff, "dialog.cancel"));
    assert_eq!(app.key_owner(), KeyOwner::Panes, "the keyboard returns");
    assert!(
        app.processes_slot().is_some(),
        "but the panel is still open"
    );

    app.toggle_processes();
    assert_eq!(app.key_owner(), KeyOwner::Processes);
    // By command and not by key: NO preset today binds `layout.processes`
    // — the panel opens from the menu or the palette (#228) — and what this
    // test pins is that the panel DISPATCHES its own key if someone binds
    // it. Without that, binding it would give a panel that opens and does
    // not close, which is the hole the places sidebar already had.
    assert!(app.processes_command("layout.processes").is_none());
    assert!(app.processes_slot().is_none(), "closed from within");
    assert_eq!(app.key_owner(), KeyOwner::Panes);
}

/// `Tab` returns the keyboard to the listings without closing the panel.
///
/// Opening a panel with keyboard support cannot cost you the key that
/// switches panes, always. The panel eats whatever is not in its
/// allowlist, so `Tab` was dead while it was open.
///
/// Both verbs are tested because that key has two names depending on the
/// screen: on the dialog one, presets bind `tab` to `dialog.pane`, and on
/// the navigation one it is `pane.switch`. This fix's first version only
/// accepted the second, and so it did NOTHING with the presets as they
/// ship — the suite passed and the key stayed dead. Piloting it in tmux
/// exposed it.
#[test]
fn tab_returns_the_keyboard_without_closing_the_panel() {
    for verb in ["dialog.pane", "pane.switch"] {
        let mut app = test_app();
        app.toggle_processes();
        assert_eq!(app.key_owner(), KeyOwner::Processes);

        assert!(app.processes_command(verb).is_none());
        assert_eq!(
            app.key_owner(),
            KeyOwner::Panes,
            "\"{verb}\" returns the keyboard"
        );
        assert!(
            app.processes_slot().is_some(),
            "and the panel is still open: leaving is not closing"
        );
    }
}

/// The cursor's movement is IN the allowlist AND dispatched: without both,
/// the `▶` stays on row 0 forever while the arrows move another list.
#[test]
fn the_pane_dispatches_its_entire_vocabulary() {
    for cmd in [
        "dialog.up",
        "dialog.down",
        "dialog.confirm",
        "dialog.cancel",
        "layout.processes",
    ] {
        assert!(
            ALLOW_PROCESSES.contains(&cmd),
            "{cmd} outside the allowlist"
        );
    }
    // And what is NOT its own stays inert in here: the panel does not
    // delete files.
    let mut app = test_app();
    app.toggle_processes();
    assert!(app.processes_command("pane.delete").is_none());
    assert!(app.modal.is_none(), "F8 opens nothing from this panel");
}

/// Whichever preset binds `layout.processes` has to bind it on BOTH
/// screens: with the key only in `browse`, opening the panel would make it
/// disappear — the keyboard switches to resolving through `dialog` — and
/// the panel would stay open with no key to close it. It is the lesson the
/// places sidebar left, written before any preset binds it (today none
/// does: it opens from the menu or the palette, #228).
#[test]
fn the_preset_that_binds_layout_processes_binds_it_on_both_screens() {
    let known: Vec<&str> = CATALOGUE.iter().map(|d| d.name).collect();
    for name in presets::NAMES {
        let src = presets::source(name).expect("the preset exists");
        let kf = norte_frontend::keymap::parse_keymap(src).expect("the preset parses");
        let binds = |screen| {
            Effective::build_for(&kf, &[], &known, screen)
                .expect("the preset merges")
                .bindings()
                .iter()
                .any(|(_, cmd)| *cmd == "layout.processes")
        };
        assert_eq!(
            binds(Screen::Browse),
            binds(Screen::Dialog),
            "{name} binds layout.processes on one screen and not the other"
        );
    }
}

/// A live task on the board, with its operand.
fn task(id: u64, kind: norte_proto::TaskKind, current: &str) -> norte_core::backend::TaskRef {
    let progress = norte_proto::TaskProgress {
        task_id: norte_proto::TaskId::new(id),
        kind,
        state: norte_proto::TaskState::Running,
        bytes_done: 1,
        bytes_total: Some(4),
        entries_done: 0,
        entries_total: None,
        current: Some(vp(current)),
        unreadable: None,
        unvisited: None,
    };
    // The sender is dropped: `watch` keeps the last published value, which
    // is all this test looks at.
    let (_tx, rx) = tokio::sync::watch::channel(progress);
    norte_core::backend::TaskRef::synthetic_for_tests(norte_proto::TaskId::new(id), rx)
}

fn paint(app: &App, width: u16, alto: u16) -> String {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, alto))
        .expect("test terminal");
    terminal
        .draw(|f| norte_tui::ui::draw(f, app))
        .expect("draw");
    terminal.backend().to_string()
}

/// The row says WHAT kind of work and ON WHAT. The task id is not painted:
/// eighteen digits identify nothing for whoever is looking and eat the
/// width the name needs.
///
/// Both surfaces at once — the panel and the strip below — because they
/// paint the same board and already diverged once over the percentage.
#[test]
fn the_row_says_the_class_and_what_it_acts_on_without_the_id() {
    let mut app = test_app();
    app.board.push(
        &task(
            7_318_349_021,
            norte_proto::TaskKind::Copy,
            "file:///casa/foto.jpg",
        ),
        None,
    );
    app.toggle_processes();
    let screen = paint(&app, 80, 24);

    assert!(screen.contains("copy"), "the class: {screen}");
    assert!(screen.contains("foto.jpg"), "the operand: {screen}");
    assert!(
        !screen.contains("7318349021"),
        "the task id is not painted: {screen}"
    );
}

/// The strip below (the "log") paints the same even with the panel closed:
/// it is the surface that is ALWAYS visible, and it used to only say
/// "copy" and a number.
#[test]
fn the_strip_names_the_operand_with_the_pane_closed() {
    let mut app = test_app();
    app.board.push(
        &task(42, norte_proto::TaskKind::Delete, "file:///casa/borrame"),
        None,
    );
    let screen = paint(&app, 80, 24);
    assert!(screen.contains("delete"), "the class: {screen}");
    assert!(screen.contains("borrame"), "the operand: {screen}");
}

/// An operand that does not fit is clipped in the MIDDLE — the tail is what
/// identifies a file — and what goes AFTER it stays: `ratatui` clips the
/// line to the width with no warning, so a row that overruns by one cell
/// loses its status past the edge and nobody notices. It happened: the `✓`
/// got eaten by the frame.
#[test]
fn a_long_path_does_not_overflow_the_row() {
    let mut app = test_app();
    let long = "file:///casa/".to_owned() + &"tramo/".repeat(30) + "final.txt";
    app.board
        .push(&task(3, norte_proto::TaskKind::Copy, &long), None);
    app.toggle_processes();
    let screen = paint(&app, 60, 20);
    for line in screen.lines() {
        // `TestBackend::to_string` quotes each row; the width is what is inside.
        let row = line.trim_matches('"');
        assert!(
            row.chars().count() <= 60,
            "a row overran the width: {row:?}"
        );
    }
    assert!(screen.contains('…'), "clipped: {screen}");
    // The task's row: the name's tail, the bar AND the status, in that
    // order and within the width.
    let row = screen
        .lines()
        .find(|l| l.contains("final.txt"))
        .unwrap_or_else(|| panic!("no row names the operand: {screen}"));
    assert!(
        row.contains("25%"),
        "the status was not eaten by the edge: {row:?}"
    );
    assert!(screen.contains("final.txt"), "the tail survives: {screen}");
}
