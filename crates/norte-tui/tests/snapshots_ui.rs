//! Snapshots del render (fase 10, insta): la forma EXACTA de cada pantalla
//! queda congelada — cualquier cambio visual es un diff consciente
//! (`cargo insta review`). Determinista: idioma fijo, datos fijos.

use norte_core::TransferOptions;
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, Modal, Pane, TransferKind, sort_entries};
use norte_tui::tasks::RetrySpec;
use norte_tui::ui;
use norte_tui::viewer::Viewer;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn vp(wire: &str) -> VPath {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("wire válido")
}

fn entry(dir: &VPath, name: &[u8], kind: EntryKind, size: Option<u64>) -> Entry {
    Entry {
        path: dir.join(Segment::new(name.to_vec()).unwrap()),
        kind,
        size,
        mtime_ms: None,
    }
}

fn render(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
    terminal.draw(|f| ui::draw(f, app)).expect("draw");
    terminal.backend().to_string()
}

fn app_base() -> App {
    let izq = vp("file:///casa");
    let der = vp("file:///otro");
    let mut entries = vec![
        entry(&izq, b"docs", EntryKind::Dir, None),
        entry(&izq, b"src", EntryKind::Dir, None),
        entry(&izq, b"notas.txt", EntryKind::File, Some(420)),
        entry(
            &izq,
            &[0xE9, b'.', b'd', b'a', b't'],
            EntryKind::File,
            Some(7),
        ),
        entry(&izq, b"enlace", EntryKind::Symlink, None),
    ];
    sort_entries(&mut entries);
    let mut app = App::new(
        Pane::new(izq, entries),
        Pane::new(
            der.clone(),
            vec![entry(&der, b"cosa", EntryKind::File, Some(1))],
        ),
    );
    app.focused_mut().move_down(1);
    app
}

#[test]
fn snapshot_navegacion() {
    insta::assert_snapshot!(render(&app_base()));
}

#[test]
fn snapshot_modal_colision() {
    let mut app = app_base();
    app.modal = Some(Modal::Collision {
        retry: RetrySpec {
            kind: TransferKind::Copy,
            from: vp("file:///casa/notas.txt"),
            to: vp("file:///otro/notas.txt"),
            opts: TransferOptions::default(),
        },
    });
    insta::assert_snapshot!(render(&app));
}

#[test]
fn snapshot_modal_papelera_y_permanente() {
    let mut app = app_base();
    app.modal = Some(Modal::ConfirmDelete {
        target: vp("file:///casa/notas.txt"),
        permanent: false,
    });
    let papelera = render(&app);
    app.modal = Some(Modal::ConfirmDelete {
        target: vp("file:///casa/notas.txt"),
        permanent: true,
    });
    let permanente = render(&app);
    insta::assert_snapshot!(format!("{papelera}\n===\n{permanente}"));
}

/// TOFU Lua (M4, ADR 0026): la forma exacta del modal de confianza del
/// `./.norte/init.lua` queda congelada — path saneado + sha256 abreviado +
/// aviso de que corre con los permisos del usuario.
#[test]
fn snapshot_modal_trust_lua_init() {
    let mut app = app_base();
    app.modal = Some(Modal::TrustLuaInit {
        path: "repo/.norte/init.lua".into(),
        hash_abbrev: "ab12cd34".into(),
    });
    insta::assert_snapshot!(render(&app));
}

/// TOFU (#45): el modal muestra el fingerprint para comparar, y un host
/// HOSTIL (bidi override) del servidor remoto se ENMASCARA — jamás pinta el
/// byte crudo que podría spoofear la barra. No es snapshot: asserts directos.
#[test]
fn modal_trust_host_muestra_fingerprint_y_enmascara_host_hostil() {
    let mut app = app_base();
    app.modal = Some(Modal::TrustHostKey {
        host: "evil\u{202E}host".into(),
        port: Some(22),
        algo: "ssh-ed25519".into(),
        fingerprint: "SHA256:abc123XYZ".into(),
        dir: vp("sftp://evilhost/"),
    });
    let texto = render(&app);
    assert!(
        texto.contains("SHA256:abc123XYZ"),
        "el fingerprint se muestra para comparar: {texto}"
    );
    assert!(
        !texto.contains('\u{202E}'),
        "el override bidi del host NO llega al render: {texto:?}"
    );
    assert!(
        texto.contains("ssh-ed25519"),
        "el algoritmo se muestra: {texto}"
    );
    assert!(
        texto.contains('!'),
        "el host hostil lleva el badge que AVISA al usuario: {texto}"
    );
}

/// El fingerprint hostil (el server intenta ocultar chars) se enmascara Y
/// lleva badge: el usuario ve que la huella fue manipulada, no la aprueba a
/// ciegas. Y un SHA256 canónico (50 chars) cabe entero SIN elipsis: lo
/// mostrado == lo que se confía.
#[test]
fn modal_trust_host_fingerprint_hostil_y_sha256_completo() {
    let mut app = app_base();
    app.modal = Some(Modal::TrustHostKey {
        host: "h".into(),
        port: None,
        algo: "ssh-ed25519".into(),
        fingerprint: "SHA256:sp\u{202E}oof".into(),
        dir: vp("sftp://h/"),
    });
    let texto = render(&app);
    assert!(
        !texto.contains('\u{202E}'),
        "el bidi del fingerprint NO llega al render: {texto:?}"
    );
    assert!(
        texto.contains('!'),
        "fingerprint manipulado → badge: {texto}"
    );

    // Un SHA256 real (7 + 43 = 50 chars) cabe entero, sin truncar.
    app.modal = Some(Modal::TrustHostKey {
        host: "h".into(),
        port: None,
        algo: "ssh-ed25519".into(),
        fingerprint: "SHA256:oXf6dQ7pC3vN2mK9tR1sB4jW8yZ0aL5eH6gU3iO7wA".into(),
        dir: vp("sftp://h/"),
    });
    let texto = render(&app);
    assert!(
        texto.contains("SHA256:oXf6dQ7pC3vN2mK9tR1sB4jW8yZ0aL5eH6gU3iO7wA"),
        "el SHA256 canónico se muestra COMPLETO (sin elipsis): {texto}"
    );
    assert!(!texto.contains('…'), "no se trunca: {texto}");
}

#[test]
fn snapshot_viewer_texto_y_hex() {
    let mut app = app_base();
    app.viewer = Some(Viewer::new(
        vp("file:///casa/notas.txt"),
        b"a\xF1o 2026\nsegunda l\xEDnea\n".to_vec(),
        false,
    ));
    let texto = render(&app);
    let mut v = Viewer::new(
        vp("file:///casa/logo.png"),
        b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec(),
        false,
    );
    v.scroll_down(0);
    app.viewer = Some(v);
    let hex = render(&app);
    insta::assert_snapshot!(format!("{texto}\n===\n{hex}"));
}

#[test]
fn snapshot_ayuda() {
    let mut app = app_base();
    // El MISMO builder que usa el binario (no una copia del formato):
    // ambas pantallas, desde el preset orthodox real.
    let presets = norte_tui::keymap::presets();
    let (_, preset) = presets.iter().find(|(n, _)| *n == "orthodox").unwrap();
    let build = |screen| {
        norte_tui::keymap::Effective::build_for(preset, &[], norte_tui::keymap::COMMANDS, screen)
            .unwrap()
    };
    let lines = norte_tui::help::build(
        &build(norte_tui::keymap::Screen::Browse),
        &build(norte_tui::keymap::Screen::Viewer),
    );
    app.help = Some(norte_tui::app::Help { lines, scroll: 0 });
    let arriba = render(&app);
    // Scrolleada: el viewport empieza más abajo (sección Viewer visible).
    app.help.as_mut().unwrap().scroll_down(24);
    insta::assert_snapshot!(format!("{arriba}\n===\n{}", render(&app)));
}
