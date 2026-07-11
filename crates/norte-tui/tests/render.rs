//! Smoke test del render: el frame pinta ambos panes, resalta el foco y
//! marca los nombres no-UTF8. Snapshot tests de verdad (insta) = fase 10.

use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, Pane, sort_entries};
use norte_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

#[test]
fn frame_pinta_panes_y_badge_no_utf8() {
    let dir = vp("file:///casa");
    let mut entries = vec![
        Entry {
            path: dir.join(Segment::new(b"docs".to_vec()).unwrap()),
            kind: EntryKind::Dir,
            size: None,
            mtime_ms: None,
        },
        Entry {
            // é en latin-1: no-UTF8 → lossy + badge.
            path: dir.join(Segment::new(vec![0xE9]).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        },
    ];
    sort_entries(&mut entries);
    let app = App::new(Pane::new(dir.clone(), entries), Pane::new(dir, Vec::new()));

    let mut terminal = Terminal::new(TestBackend::new(60, 10)).expect("terminal de test");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");

    let contenido = terminal.backend().to_string();
    assert!(contenido.contains("/docs"), "dir con marcador: {contenido}");
    assert!(
        contenido.contains('\u{FFFD}') && contenido.contains("! "),
        "no-UTF8 lossy Y con badge en prefijo: {contenido}"
    );
    assert!(
        contenido.contains("1/2"),
        "posición del cursor en status: {contenido}"
    );
}

/// F3.1 de la auditoría: el badge va en PREFIJO porque al final moriría en
/// el truncado por ancho — un nombre hostil LARGO en un pane estrecho debe
/// seguir marcado.
#[test]
fn badge_sobrevive_al_truncado_en_pane_estrecho() {
    let dir = vp("file:///x");
    let mut name = vec![b'x'; 200];
    name.push(0xE9); // los bytes malos, al FINAL: fuera del ancho visible
    let entries = vec![Entry {
        path: dir.join(Segment::new(name).unwrap()),
        kind: EntryKind::File,
        size: Some(1),
        mtime_ms: None,
    }];
    let app = App::new(Pane::new(dir.clone(), entries), Pane::new(dir, Vec::new()));

    let mut terminal = Terminal::new(TestBackend::new(40, 8)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let contenido = terminal.backend().to_string();
    assert!(
        contenido.contains("!xxx") || contenido.contains("! xxx"),
        "la marca es visible aunque el � truncado no lo sea: {contenido}"
    );
}
