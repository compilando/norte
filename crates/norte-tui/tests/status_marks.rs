//! #103: the status bar announces how many entries are marked and how much
//! they weigh, and warns when a refresh silently ate marks. See
//! `draw_status` in `crates/norte-tui/src/ui.rs`.

use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, Pane};
use norte_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

/// Entries with an explicit `size` (bytes), all files. A name different
/// from `app_with_entries` (task 9) on purpose: that one builds from plain
/// names with no size.
fn app_with_sized_entries(files: Vec<(&str, u64)>) -> App {
    let dir = vp("file:///home");
    let entries = files
        .into_iter()
        .map(|(name, size)| Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(name.as_bytes().to_vec()).unwrap()),
            kind: EntryKind::File,
            size: Some(size),
            mtime_ms: None,
        })
        .collect();
    App::new(Pane::new(dir.clone(), entries), Pane::new(dir, Vec::new()))
}

fn status_text(app: &App) -> String {
    status_text_at(app, 80)
}

/// Like [`status_text`], with the width as a parameter — needed to pin the
/// bar's clipping at a narrow width (review MAJOR M3).
fn status_text_at(app: &App, width: u16) -> String {
    // Review MINOR: the file asserts English literals below, but
    // `norte_i18n` resolves from the environment — this test suite is the
    // only gate (`just ci`), and it must not fail on a Spanish-locale box.
    let _ = norte_i18n::force(norte_i18n::Lang::En);
    let mut terminal = Terminal::new(TestBackend::new(width, 16)).expect("test terminal");
    terminal.draw(|f| ui::draw(f, app)).expect("draw");
    terminal.backend().to_string()
}

#[test]
fn the_status_bar_reports_marks_only_when_there_are_any() {
    let mut app = app_with_sized_entries(vec![("a", 10), ("b", 20)]);
    assert!(!status_text(&app).contains("marked"));
    app.focused_mut().mark_all();
    let text = status_text(&app);
    // Review MINOR: `text.contains('2')` was vacuous — the bar already
    // reads `1/2` (cursor/total) regardless of marks. Assert the composed
    // sentence, like the sibling test below correctly does.
    assert!(text.contains("2 marked, 30 B"), "count+size: {text}");
}

/// #103: `marked_bytes` deliberately does NOT count directories (nothing
/// here walks a tree). A marked 10-byte file + a marked directory must NOT
/// read as the bare "2 marked, 10 B" — that reads as a transfer size, and
/// it is not one. The bar must name the directory separately.
#[test]
fn the_status_bar_names_marked_directories_separately() {
    let dir = vp("file:///home");
    let entries = vec![
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(b"a".to_vec()).unwrap()),
            kind: EntryKind::File,
            size: Some(10),
            mtime_ms: None,
        },
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(b"d".to_vec()).unwrap()),
            kind: EntryKind::Dir,
            size: None,
            mtime_ms: None,
        },
    ];
    let mut app = App::new(Pane::new(dir.clone(), entries), Pane::new(dir, Vec::new()));
    app.focused_mut().mark_all();

    let text = status_text(&app);
    // The correct render names the dir count ("... + 1 dirs"); the bare
    // fallback the plan warns against would end right after the size
    // instead, i.e. "2 marked, 10 B" followed by the next field's double
    // space rather than " + N dirs".
    assert!(
        text.contains("2 marked, 10 B + 1 dirs"),
        "must name the directory count instead of implying a bare total: {text}"
    );
}

/// Review MAJOR M3: the status line has no width budget, so ratatui clips
/// the TAIL. `omitidas` (an incomplete listing — "is never silent" per its
/// own comment) and `nombres` (the name-reinterpretation badge — "the user
/// must know it at all times") are warnings; `marked`/`pruned` is an
/// informational counter. The warnings must be ordered ahead of the
/// counter so a long path at a narrow width clips the counter first, not
/// the badge the user must always see. Pin: with marks present AND name
/// reinterpretation on, at 40 columns the encoding badge still appears.
#[test]
fn the_names_encoding_badge_survives_clipping_ahead_of_the_marked_summary() {
    let dir = vp("file:///d");
    let entries: Vec<Entry> = (0..10)
        .map(|i| Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(vec![b'a' + i]).unwrap()),
            kind: EntryKind::File,
            size: Some(100),
            mtime_ms: None,
        })
        .collect();
    let mut app = App::new(Pane::new(dir.clone(), entries), Pane::new(dir, Vec::new()));
    app.panes[0].cycle_name_encoding();
    app.focused_mut().mark_all();

    let text = status_text_at(&app, 40);
    assert!(
        text.contains("names:"),
        "the encoding badge must survive clipping ahead of the marked summary: {text}"
    );
}

/// K2a: a count typed halfway through is VISIBLE in the status bar. An
/// invisible count is a count that cannot be cancelled: a user who pressed
/// `5` by accident has no way of knowing the next key will be multiplied by
/// five. Walks the real path (resolver → `pending_display` → `app.pending`
/// → `draw_status`), not the literal.
#[test]
fn the_status_bar_shows_a_count_while_it_is_being_typed() {
    use norte_tui::keymap::{
        Effective, Resolution, Resolver, Screen, parse_chord, parse_keymap, pending_display,
    };

    let preset = parse_keymap(
        r"
counts = true

[pane]
keymap = [ { on = ['j'], run = 'cursor.down' } ]
",
    )
    .expect("test preset");
    let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse)
        .expect("test effective");
    let mut resolver = Resolver::new(eff);
    assert!(matches!(
        resolver.push(parse_chord("5").expect("chord")),
        Resolution::Counting(5)
    ));

    let mut app = app_with_sized_entries(vec![("a", 10), ("b", 20)]);
    assert!(
        // ` …]` and not `[`: the whole screen includes the menu bar's
        // layout buttons (`[|]`, ADR 0133).
        !status_text(&app).contains(" …]"),
        "without a count the bar does not open the segment"
    );
    app.pending = pending_display(&resolver);
    let text = status_text(&app);
    assert!(text.contains("[5 …]"), "the count must be visible: {text}");
}

/// #107: active hiding with entries set aside is announced in the bar — a
/// listing that shows less than there is is never silent (the same
/// discipline as `status-archive-skipped`). With hiding off, or with no
/// dotfiles in the dir, the bar stays quiet.
#[test]
fn the_status_bar_reports_hidden_entries_only_while_hiding() {
    let mut app = app_with_sized_entries(vec![(".env", 1), ("main.rs", 10)]);
    assert!(
        !status_text(&app).contains("hidden"),
        "default: everything is visible"
    );
    app.focused_mut().toggle_hidden();
    let text = status_text(&app);
    assert!(text.contains("1 hidden"), "badge: {text}");
    app.focused_mut().toggle_hidden();
    assert!(
        !status_text(&app).contains("hidden"),
        "once shown, it goes quiet"
    );
}
