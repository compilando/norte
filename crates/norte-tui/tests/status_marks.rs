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
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal de test");
    terminal.draw(|f| ui::draw(f, app)).expect("draw");
    terminal.backend().to_string()
}

#[test]
fn the_status_bar_reports_marks_only_when_there_are_any() {
    let mut app = app_with_sized_entries(vec![("a", 10), ("b", 20)]);
    assert!(!status_text(&app).contains("marked"));
    app.focused_mut().mark_all();
    let text = status_text(&app);
    assert!(text.contains('2'), "count: {text}");
    assert!(text.contains("30 B"), "size: {text}");
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
