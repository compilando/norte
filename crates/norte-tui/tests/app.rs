//! Tests for the TUI's pure state (phase 3 M1): navigation, cd, sort and
//! display of hostile names. Zero terminal: the state is a pure machine
//! over `Entry`s.

use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{Pane, display_name, sort_entries};

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid test wire")
}

fn entry(dir: &VPath, name: &[u8], kind: EntryKind) -> Entry {
    Entry {
        attrs: std::collections::BTreeMap::new(),
        path: dir.join(Segment::new(name.to_vec()).expect("valid segment")),
        kind,
        size: (kind == EntryKind::File).then_some(42),
        mtime_ms: None,
    }
}

fn pane_with(names: &[(&[u8], EntryKind)]) -> Pane {
    let dir = vp("file:///base");
    let mut entries: Vec<Entry> = names.iter().map(|(n, k)| entry(&dir, n, *k)).collect();
    sort_entries(&mut entries);
    Pane::new(dir, entries)
}

#[test]
fn sort_puts_dirs_first_and_by_bytes() {
    let dir = vp("file:///base");
    let mut entries = vec![
        entry(&dir, b"zeta.txt", EntryKind::File),
        entry(&dir, b"alfa.txt", EntryKind::File),
        entry(&dir, b"carpeta", EntryKind::Dir),
        entry(&dir, b"Alfa", EntryKind::Dir),
        entry(&dir, b"enlace", EntryKind::Symlink),
    ];
    sort_entries(&mut entries);
    let names: Vec<&[u8]> = entries
        .iter()
        .map(|e| e.path.file_name().unwrap().as_bytes())
        .collect();
    // Dirs first (byte order: uppercase before), then the rest.
    assert_eq!(
        names,
        vec![
            b"Alfa".as_slice(),
            b"carpeta",
            b"alfa.txt",
            b"enlace",
            b"zeta.txt"
        ]
    );
}

#[test]
fn cursor_navigates_with_caps() {
    let mut p = pane_with(&[
        (b"a", EntryKind::File),
        (b"b", EntryKind::File),
        (b"c", EntryKind::File),
    ]);
    assert_eq!(p.cursor(), 0);
    p.move_up(1);
    assert_eq!(p.cursor(), 0, "top cap");
    p.move_down(1);
    assert_eq!(p.cursor(), 1);
    p.move_down(100);
    assert_eq!(p.cursor(), 2, "bottom cap");
    p.move_to_end();
    assert_eq!(p.cursor(), 2);
    p.move_to_start();
    assert_eq!(p.cursor(), 0);
}

#[test]
fn cursor_in_empty_pane_does_not_crash() {
    let mut p = pane_with(&[]);
    p.move_down(1);
    p.move_up(1);
    p.move_to_end();
    assert_eq!(p.cursor(), 0);
    assert!(p.selected().is_none());
}

#[test]
fn selected_returns_the_entry_under_the_cursor() {
    let mut p = pane_with(&[(b"a", EntryKind::File), (b"dir", EntryKind::Dir)]);
    // After sort: [dir, a].
    assert_eq!(
        p.selected().unwrap().path.file_name().unwrap().as_bytes(),
        b"dir"
    );
    p.move_down(1);
    assert_eq!(
        p.selected().unwrap().path.file_name().unwrap().as_bytes(),
        b"a"
    );
}

#[test]
fn display_marks_everything_lost_and_neutralizes_controls() {
    // Clean UTF-8 name: identical and with no badge.
    let (text, hostile) = display_name(b"normal.txt");
    assert_eq!(text, "normal.txt");
    assert!(!hostile);

    // Spec §6 property: badge EXACTLY when the painted text differs from
    // the real name (lossy, masked controls or bidi).
    for n in norte_testkit::corpus::hostile_names() {
        let (text, hostile) = display_name(&n.bytes);
        assert!(!text.is_empty(), "{}: display never empty", n.id);
        let identical = text.as_bytes() == n.bytes.as_slice();
        assert_eq!(
            hostile, !identical,
            "{}: badge exactly when the display differs from the real name",
            n.id
        );
        // Never raw controls or a bidi override toward the terminal:
        // ratatui would silently DROP them (visible name ≠ real one) and a
        // direct frontend would run ANSI / reorder RTL.
        assert!(
            !text.chars().any(|c| c.is_control()
                || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')),
            "{}: no Cc or Cf-bidi in the display",
            n.id
        );
        if !identical {
            assert!(
                text.contains('\u{FFFD}'),
                "{}: the loss is visible (spec §6: lossy marked)",
                n.id
            );
        }
    }

    // A file REALLY named � (valid UTF-8) carries no badge: telling it
    // apart from a lossy one depends on the badge, not the glyph.
    let (text, hostile) = display_name("\u{FFFD}".as_bytes());
    assert_eq!(text, "\u{FFFD}");
    assert!(!hostile);
}

#[test]
fn sort_groups_normalization_variants_together() {
    // spec §6.1: unicode_compare = nfc by default FOR SORTING (the bytes
    // are never mutated). NFC and NFD of the same name end up adjacent.
    let dir = vp("file:///base");
    let mut entries = vec![
        entry(&dir, &[0xC3, 0xA9], EntryKind::File), // é NFC
        entry(&dir, b"zzz", EntryKind::File),
        entry(&dir, &[0x65, 0xCC, 0x81], EntryKind::File), // é NFD
        entry(&dir, b"aaa", EntryKind::File),
    ];
    sort_entries(&mut entries);
    let names: Vec<&[u8]> = entries
        .iter()
        .map(|e| e.path.file_name().unwrap().as_bytes())
        .collect();
    // é (U+00E9) sorts after 'z' by its NFC key; what matters: BOTH variants
    // end up ADJACENT (same key, tie broken by raw bytes: NFD 0x65… <
    // NFC 0xC3…). With no NFC key, "zzz" would split the pair.
    assert_eq!(names[0], b"aaa");
    assert_eq!(names[1], b"zzz");
    assert_eq!(names[2], &[0x65, 0xCC, 0x81][..]);
    assert_eq!(names[3], &[0xC3, 0xA9][..]);
}

#[test]
fn path_display_flags_paths_with_hostile_segments() {
    use norte_tui::app::path_display;
    let clean = vp("file:///casa/docs");
    let (text, hostile) = path_display(&clean);
    assert!(text.contains("docs"));
    assert!(!hostile);

    let feo = clean.join(Segment::new(vec![0xE9]).unwrap());
    let (_, hostile) = path_display(&feo);
    assert!(hostile, "a non-UTF8 segment marks the whole path");
}

#[test]
fn tab_toggles_focus_between_the_two_panes() {
    use norte_tui::app::App;
    let mut app = App::new(pane_with(&[]), pane_with(&[]));
    assert_eq!(app.focus(), 0);
    app.switch_focus();
    assert_eq!(app.focus(), 1);
    app.switch_focus();
    assert_eq!(app.focus(), 0);
    app.focused_mut().move_down(1);
    assert!(!app.quit);
}

/// #81 (review MAJOR-4): re-launching the search with NO cd in between does
/// not drag previews from the earlier one — a name-only hit from query B
/// would paint query A's :line.
#[test]
fn begin_search_clears_previous_previews() {
    let mut p = pane_with(&[(b"a.rs", EntryKind::File)]);
    let root = vp("file:///casa");
    p.begin_search(root.clone());
    p.search_matches.insert(
        root.join(norte_proto::Segment::new(b"a.rs".to_vec()).unwrap()),
        norte_proto::methods::MatchInfo {
            line: Some(7),
            preview: Some("vieja".into()),
        },
    );
    p.begin_search(root);
    assert!(
        p.search_matches.is_empty(),
        "the map clears when the search restarts"
    );
}
