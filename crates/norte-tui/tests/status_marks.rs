//! #103: la barra de estado anuncia cuántas entradas están marcadas y cuánto
//! pesan, y advierte cuando un refresh se comió marcas silenciosamente. Ver
//! `draw_status` en `crates/norte-tui/src/ui.rs`.

use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, Pane};
use norte_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

/// Entries con un `size` explícito (bytes), todas ficheros. Nombre distinto
/// de `app_with_entries` (task 9) a propósito: esa construye desde nombres
/// puros sin tamaño.
fn app_with_sized_entries(files: Vec<(&str, u64)>) -> App {
    let dir = vp("file:///casa");
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

/// Como [`status_text`], con el ancho como parámetro — necesario para pinear
/// el recorte de la barra a un ancho estrecho (review MAJOR M3).
fn status_text_at(app: &App, width: u16) -> String {
    // Review MINOR: the file asserts English literals below, but
    // `norte_i18n` resolves from the environment — this test suite is the
    // only gate (`just ci`), and it must not fail on a Spanish-locale box.
    let _ = norte_i18n::force(norte_i18n::Lang::En);
    let mut terminal = Terminal::new(TestBackend::new(width, 16)).expect("terminal de test");
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

/// #103: `marked_bytes` deliberadamente NO cuenta directorios (nada aquí
/// recorre un árbol). Un fichero de 10 bytes + un directorio marcados NO
/// deben leerse como el bare "2 marked, 10 B" — eso se lee como un tamaño de
/// transferencia y no lo es. La barra debe nombrar el directorio aparte.
#[test]
fn the_status_bar_names_marked_directories_separately() {
    let dir = vp("file:///casa");
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
/// the TAIL. `omitidas` (an incomplete listing — "jamás es silencioso" per
/// its own comment) and `nombres` (the name-reinterpretation badge — "el
/// usuario debe saberlo en todo momento") are warnings; `marked`/`pruned`
/// is an informational counter. The warnings must be ordered ahead of the
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
