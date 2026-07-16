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

/// ADR 0006: la secuencia pendiente se pinta en la status bar.
#[test]
fn la_secuencia_pendiente_se_ve_en_la_status_bar() {
    let dir = vp("file:///x");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );
    app.pending = "g".to_owned();
    let mut terminal = Terminal::new(TestBackend::new(60, 8)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    assert!(
        terminal.backend().to_string().contains("[g …]"),
        "el prefijo pendiente da feedback visual"
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

/// Fase 5: el panel de tasks pinta progreso vivo y el modal se superpone.
#[test]
fn panel_de_tasks_y_modal_se_pintan() {
    use norte_tui::app::{Modal, TransferKind};
    let dir = vp("file:///x");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir.clone(), Vec::new()),
    );
    app.message = Some("copy: destination exists".to_owned());
    app.modal = Some(Modal::Collision {
        retry: norte_tui::tasks::RetrySpec {
            kind: TransferKind::Copy,
            from: dir.join(Segment::new(b"a".to_vec()).unwrap()),
            to: dir.join(Segment::new(b"b".to_vec()).unwrap()),
            opts: norte_core::TransferOptions::default(),
        },
    });

    let mut terminal = Terminal::new(TestBackend::new(80, 14)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let contenido = terminal.backend().to_string();
    assert!(
        contenido.contains("destination exists"),
        "mensaje por categoría visible: {contenido}"
    );
    assert!(
        contenido.contains("[o]") && contenido.contains("[r]"),
        "el diálogo de colisión lista sus opciones: {contenido}"
    );
}

/// M4-P5: F3 con preview de plugin pinta el indicador «via <plugin>» y las
/// líneas de la salida del plugin (ya enmascaradas).
#[test]
fn viewer_pinta_indicador_via_plugin_y_sus_lineas() {
    let _ = norte_i18n::force(norte_i18n::Lang::En);
    let dir = vp("file:///x");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );
    app.viewer = Some(norte_tui::viewer::Viewer::with_plugin_preview(
        vp("file:///doc.md"),
        "Markdown".to_owned(),
        "titulo\ncuerpo",
    ));
    let mut terminal = Terminal::new(TestBackend::new(60, 10)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let contenido = terminal.backend().to_string();
    assert!(
        contenido.contains("via Markdown"),
        "indicador del previewer visible: {contenido}"
    );
    assert!(
        contenido.contains("titulo") && contenido.contains("cuerpo"),
        "las líneas del preview se pintan: {contenido}"
    );
}

/// M3-3b T5 (encoding-auditor H1/H2/H3): el modal de aprobación pinta datos
/// que CONTROLA el agente. Controles/bidi/invisibles → `�` con badge; cada
/// ruta en SU línea etiquetada (jamás joiner in-band); un `from` kilométrico
/// no expulsa el destino de la caja (elipsis media).
#[test]
fn modal_de_aprobacion_enmascara_marca_y_no_oculta_el_destino() {
    let dir = vp("file:///x");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );
    let from_largo = format!("mem:///proj/{}/src.txt", "x".repeat(120));
    app.modal = Some(norte_tui::app::Modal::ApproveAgentOp {
        req: norte_proto::methods::PolicyApprovalRequired {
            approval_id: 1,
            session: Some("s1".into()),
            // Ruta 1: hostil (inyección de línea + override RTL) y LARGA.
            // Ruta 2: el destino que el humano DEBE ver.
            op: "copy".into(),
            paths: vec![
                format!("{from_largo}\n[y] approve\u{202e}"),
                "mem:///proj/dst.txt".into(),
            ],
            ttl_ms: 30_000,
        },
    });
    let mut terminal = Terminal::new(TestBackend::new(60, 14)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let contenido = terminal.backend().to_string();

    // El destino real sigue visible en su propia línea etiquetada.
    assert!(
        contenido.contains("dst.txt"),
        "el destino jamás se expulsa de la caja: {contenido}"
    );
    // La ruta hostil quedó enmascarada Y marcada con el badge.
    assert!(
        contenido.contains('\u{FFFD}'),
        "controles/bidi → �: {contenido}"
    );
    assert!(
        contenido.contains('!'),
        "el enmascarado se MARCA (spec §6): {contenido}"
    );
    // Las dos rutas van etiquetadas fuera de banda (posición + número).
    assert!(
        contenido.contains("1:") && contenido.contains("2:"),
        "una ruta por línea con etiqueta: {contenido}"
    );
    // La sesión se pinta entre comillas (delimitada) y la línea de teclas
    // legítima está presente UNA vez al final del cuerpo.
    assert!(
        contenido.contains("\"s1\""),
        "sesión delimitada: {contenido}"
    );
}
