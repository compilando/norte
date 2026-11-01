//! The path line (`pane.edit-path`, ADR 0190): the focused pane's path,
//! editable over its top border, all of it selected when it opens.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, Pane};
use norte_tui::path_edit::{self, Outcome};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn entry(dir: &VPath, name: &str, kind: EntryKind) -> Entry {
    Entry {
        attrs: std::collections::BTreeMap::new(),
        path: dir.join(Segment::new(name.as_bytes().to_vec()).unwrap()),
        kind,
        size: Some(1),
        mtime_ms: None,
    }
}

fn app_at(wire: &str, entries: &[(&str, EntryKind)]) -> App {
    let vp = VPath::parse(wire).expect("wire");
    let es: Vec<Entry> = entries.iter().map(|(n, k)| entry(&vp, n, *k)).collect();
    let mut app = App::new(Pane::new(vp.clone(), es), Pane::new(vp, Vec::new()));
    app.set_focus(0);
    app
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn typed(app: &mut App, s: &str) {
    for c in s.chars() {
        assert_eq!(path_edit::on_key(app, key(KeyCode::Char(c))), Outcome::Kept);
    }
}

fn text(app: &App) -> &str {
    app.path_edit.as_ref().expect("open").text()
}

/// The line opens on the pane's own path, all selected, and Esc puts it
/// away without going anywhere.
#[test]
fn it_opens_on_the_panes_path_and_esc_closes_it() {
    let mut app = app_at("sftp://host/srv/www", &[]);
    path_edit::open(&mut app);
    let edit = app.path_edit.as_ref().expect("open");
    assert_eq!(edit.text(), "sftp://host/srv/www");
    assert!(edit.all_selected());
    assert_eq!(
        path_edit::on_key(&mut app, key(KeyCode::Esc)),
        Outcome::Kept
    );
    assert!(app.path_edit.is_none());
}

/// Typing replaces the selected path; Enter hands the loop the folder and
/// the text, and the line closes.
#[test]
fn typing_replaces_and_enter_goes_there() {
    let mut app = app_at("sftp://host/srv/www", &[]);
    path_edit::open(&mut app);
    typed(&mut app, "sftp://host/etc");
    assert_eq!(
        path_edit::on_key(&mut app, key(KeyCode::Enter)),
        Outcome::Go {
            dir: VPath::parse("sftp://host/etc").unwrap(),
            text: "sftp://host/etc".to_owned(),
        }
    );
    assert!(app.path_edit.is_none());
}

/// A text that names no folder says why and keeps the line, with what was
/// typed, open.
#[test]
fn enter_on_a_text_that_is_not_a_path_keeps_the_line() {
    let _ = norte_i18n::force(norte_i18n::Lang::En);
    let mut app = app_at("sftp://host/srv", &[]);
    path_edit::open(&mut app);
    typed(&mut app, "relative/dir");
    assert_eq!(
        path_edit::on_key(&mut app, key(KeyCode::Enter)),
        Outcome::Kept
    );
    assert_eq!(text(&app), "relative/dir");
    assert_eq!(
        app.message.as_deref(),
        Some(norte_i18n::t("msg-goto-bad-path").as_str())
    );
}

/// A navigation that fails gives the line back with the typed text, not
/// selected: the reader fixes one letter instead of retyping the path.
#[test]
fn a_failed_navigation_gives_the_text_back() {
    let mut app = app_at("sftp://host/srv", &[]);
    path_edit::reopen(&mut app, "sftp://host/srvv".to_owned());
    let edit = app.path_edit.as_ref().expect("open");
    assert_eq!(edit.text(), "sftp://host/srvv");
    assert!(!edit.all_selected());
}

/// Ctrl+Insert copies the line (Ctrl+C quits norte) and keeps it open.
#[test]
fn ctrl_insert_copies_the_line() {
    let mut app = app_at("sftp://host/srv", &[]);
    path_edit::open(&mut app);
    let ctrl_ins = KeyEvent::new(KeyCode::Insert, KeyModifiers::CONTROL);
    assert_eq!(
        path_edit::on_key(&mut app, ctrl_ins),
        Outcome::Copy("sftp://host/srv".to_owned())
    );
    assert!(app.path_edit.is_some());
}

/// Tab completes a FOLDER of the pane; a file with the same start is not
/// a place to go.
#[test]
fn tab_completes_a_folder_and_not_a_file() {
    let mut app = app_at(
        "sftp://host/srv",
        &[("www", EntryKind::Dir), ("wwwlog.txt", EntryKind::File)],
    );
    path_edit::open(&mut app);
    assert_eq!(
        path_edit::on_key(&mut app, key(KeyCode::End)),
        Outcome::Kept
    );
    typed(&mut app, "/w");
    assert_eq!(
        path_edit::on_key(&mut app, key(KeyCode::Tab)),
        Outcome::Kept
    );
    assert_eq!(text(&app), "sftp://host/srv/www/");
}

/// The line is painted over the focused pane's top border, with the
/// terminal's cursor at its end: what the reader edits is on screen.
#[test]
fn the_line_is_painted_over_the_focused_panes_border() {
    let mut app = app_at("sftp://host/srv", &[]);
    path_edit::open(&mut app);
    assert_eq!(
        path_edit::on_key(&mut app, key(KeyCode::End)),
        Outcome::Kept
    );
    let (w, h) = (100, 20);
    let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
    let area = ratatui::layout::Rect::new(0, 0, w, h);
    norte_tui::ui::before_frame(&mut app, area);
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    let geo = norte_tui::ui::pane_geometry(&app, area).expect("panes painted");
    let pane = geo.first().expect("focused pane placed");
    let buf = terminal.backend().buffer();
    let row: String = (pane.x..pane.x + pane.width)
        .map(|x| buf[(x, pane.y)].symbol().to_owned())
        .collect();
    assert!(row.contains(" sftp://host/srv "), "{row:?}");
    let cursor = terminal.get_cursor_position().expect("cursor");
    let at = pane.x + 2 + u16::try_from("sftp://host/srv".len()).unwrap();
    assert_eq!((cursor.x, cursor.y), (at, pane.y));
}
