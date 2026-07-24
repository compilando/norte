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

/// Como [`render`] pero a 80×24 (MAJOR-1 item d, H1 close): el gestor de
/// extensiones y el selector de tema necesitan más filas visibles que la
/// pantalla de 16 usada en el resto del archivo para pintar su lista
/// completa sin recorte vertical.
fn render_80x24(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
    terminal.draw(|f| ui::draw(f, app)).expect("draw");
    terminal.backend().to_string()
}

/// H1 T3 (#24): los hints de los overlays ya NO son estáticos — se
/// precomputan del efectivo `dialog` vigente (`main.rs`, `DialogHints::
/// build`). Los tests de render construyen `App` directamente (sin pasar
/// por `main`), así que replican el MISMO cómputo con el preset `orthodox`
/// real: el snapshot congela lo que el usuario vería de verdad, no una
/// cadena vacía.
fn default_dialog_hints() -> norte_tui::hints::DialogHints {
    use norte_tui::keymap::{COMMANDS, DIALOG_COMMANDS, Effective, Screen, presets};
    let (_, preset) = presets()
        .into_iter()
        .find(|(n, _)| *n == "orthodox")
        .expect("preset orthodox");
    let known: Vec<&str> = COMMANDS
        .iter()
        .copied()
        .chain(DIALOG_COMMANDS.iter().copied())
        .collect();
    let eff = Effective::build_for(&preset, &[], &known, Screen::Dialog).expect("dialog efectivo");
    norte_tui::hints::DialogHints::build(&eff)
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
    app.dialog_hints = default_dialog_hints();
    app
}

#[test]
fn snapshot_navegacion() {
    insta::assert_snapshot!(render(&app_base()));
}

/// Quick search en modo filtro (spec 2026-07-18): el pane izquierdo lista
/// SOLO los matches, con la línea de input `/{query} n/m` al pie y el
/// cursor sobre la selección filtrada; el derecho sigue intacto.
#[test]
fn snapshot_quick_search_filtro() {
    let mut app = app_base();
    let pane = app.focused_mut();
    pane.quick_start(norte_tui::nav::Mode::Filter);
    pane.quick_char('s');
    insta::assert_snapshot!(render(&app));
}

/// Diálogo de búsqueda viva (`Alt+F7`, liveSearch T6): campo de nombre con
/// texto (cursor `_`), toggles regex/case y la raíz del walk. El `cwd` lleva
/// un override bidi: sale ENMASCARADO y con el badge (jamás bidi crudo en el
/// borde, spec §6) — verifica el saneado del modal.
#[test]
fn snapshot_search_dialog() {
    let mut app = app_base();
    // Fija el cwd hostil del pane con foco reconstruyéndolo (mismo listado y
    // cursor que `app_base`): el `Pane` ya no expone `dir` como campo — su
    // estado puro vive en `norte_frontend::PaneState` (#82).
    let entries = app.panes[0].entries().to_vec();
    app.panes[0] = Pane::new(vp("file:///casa/evil%E2%80%AEdir"), entries);
    app.panes[0].move_down(1);
    app.open_search_dialog();
    let dialog = app.search_dialog.as_mut().expect("diálogo abierto");
    for c in "*.rs".chars() {
        dialog.push_char(c);
    }
    insta::assert_snapshot!(render(&app));
}

/// Pane virtual de búsqueda viva (`Alt+F7`, liveSearch T6): el pane con foco
/// lista los HITS que van llegando (nombre plano, `VPath` completo bajo el
/// capó) y la barra pinta `search-status-running` («buscando…»); el otro pane
/// sigue normal.
#[test]
fn snapshot_search_pane_virtual() {
    let mut app = app_base();
    let raiz = vp("file:///casa");
    let pane = app.focused_mut();
    pane.begin_search(raiz.clone());
    pane.extend_listing(vec![
        entry(
            &vp("file:///casa/src"),
            b"main.rs",
            EntryKind::File,
            Some(120),
        ),
        entry(
            &vp("file:///casa/docs"),
            "a\u{00F1}o.txt".as_bytes(),
            EntryKind::File,
            Some(88),
        ),
    ]);
    insta::assert_snapshot!(render(&app));
}

/// Popup de historial (spec 2026-07-18, `Alt+↓`): dirs del pane con foco,
/// más reciente primero, con el cursor arriba.
#[test]
fn snapshot_popup_historial() {
    let mut app = app_base();
    app.history[0].push(vp("file:///casa/docs"));
    app.history[0].push(vp("file:///proyectos"));
    app.open_nav_popup(norte_tui::app::NavPopupKind::History);
    insta::assert_snapshot!(render(&app));
}

/// Popup de hotlist (`Ctrl+D`): entrada válida con `name — path`, entrada
/// INVÁLIDA con su aviso (degradación por entrada, no revienta), y el
/// footer de teclas `[enter]/[a]/[d]/[esc]`.
#[test]
fn snapshot_popup_hotlist() {
    let mut app = app_base();
    app.hotlist = vec![
        norte_tui::config::HotlistItem {
            name: "trabajo".into(),
            target: Ok(vp("file:///home/o/work")),
        },
        norte_tui::config::HotlistItem {
            name: "rota".into(),
            target: Err("err-invalid-path".into()),
        },
    ];
    app.open_nav_popup(norte_tui::app::NavPopupKind::Hotlist);
    insta::assert_snapshot!(render(&app));
}

/// MAJOR-1 item (d), H1 close: a 80 columnas, el hint GENERADO del selector
/// de tema (`app.theme`, F9) ya no se corta a mitad de palabra — el box
/// ahora crece con su footer (ver `ui::draw_theme_picker`). Se pin-ea Y se
/// verifica en directo que ninguna etiqueta quedó partida.
#[test]
fn snapshot_theme_picker_80x24() {
    let mut app = app_base();
    app.open_theme_picker();
    let texto = render_80x24(&app);
    insta::assert_snapshot!(texto.clone());
    let hint = &app.dialog_hints.picker;
    assert!(
        !hint.is_empty(),
        "el preset orthodox liga confirm/cancel al picker"
    );
    assert!(
        texto.contains(hint.as_str()),
        "el hint generado debe caber ENTERO, sin cortes: hint={hint:?}\n{texto}"
    );
}

/// MAJOR-1 item (d): igual que el selector de tema, para el gestor de
/// extensiones (`app.extensions`, M4-P3) — su hint tras (a)+(b) (labels
/// cortas + sin flechas) más el sizing por footer de (c) deben caber
/// enteros a 80 columnas.
#[test]
fn snapshot_extensions_80x24() {
    let mut app = app_base();
    app.extensions = Some(norte_tui::app::ExtensionManager {
        plugins: vec![norte_proto::methods::PluginInfo {
            id: "org.norte.demo".into(),
            name: "Demo".into(),
            publisher: "norte".into(),
            version: "1.0.0".into(),
            category: "previewer".into(),
            capabilities: vec!["fs-read".into()],
            approved: false,
            enabled: true,
            description: None,
            commands: Vec::new(),
        }],
        errors: Vec::new(),
        cursor: 0,
    });
    let texto = render_80x24(&app);
    insta::assert_snapshot!(texto.clone());
    let hint = &app.dialog_hints.extensions;
    assert!(
        !hint.is_empty(),
        "el preset orthodox liga approve/toggle-enabled/cancel a extensiones"
    );
    assert!(
        texto.contains(hint.as_str()),
        "el hint generado debe caber ENTERO, sin cortes: hint={hint:?}\n{texto}"
    );
}

/// P1: la description de un plugin (manifest `[plugin]`, cap 280 chars) es
/// texto de TERCEROS — una segunda línea bajo la fila del plugin, pero
/// hostil (override RTL, corpus `rtl_override`) NUNCA se pinta cruda. Mismo
/// criterio de enmascarado que `render_enmascara_nombre_hostil`
/// (`extensions.rs`), a nivel de snapshot completo.
#[test]
fn snapshot_extensions_description_hostil_80x24() {
    let hostil = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "rtl_override")
        .expect("fixture del corpus");
    let descripcion = String::from_utf8_lossy(&hostil.bytes).into_owned();
    let mut app = app_base();
    app.extensions = Some(norte_tui::app::ExtensionManager {
        plugins: vec![norte_proto::methods::PluginInfo {
            id: "org.norte.demo".into(),
            name: "Demo".into(),
            publisher: "norte".into(),
            version: "1.0.0".into(),
            category: "previewer".into(),
            capabilities: vec!["fs-read".into()],
            approved: true,
            enabled: true,
            description: Some(descripcion),
            commands: Vec::new(),
        }],
        errors: Vec::new(),
        cursor: 0,
    });
    let texto = render_80x24(&app);
    // El check es sobre el CARÁCTER inyectado, no "ningún hazard en toda la
    // pantalla" — `to_string()` del backend une líneas con `\n`, que
    // `is_terminal_hazard` (correctamente) también marca como control: un
    // check ciego sobre TODO el render daría un falso positivo por el
    // formato del propio buffer, no por texto hostil filtrado.
    assert!(
        !texto.contains('\u{202E}'),
        "el override RTL de la description se pintó crudo: {texto}"
    );
    assert!(
        texto.contains('\u{FFFD}'),
        "la description hostil debe enmascararse a U+FFFD: {texto}"
    );
    insta::assert_snapshot!(texto);
}

/// BAJA-3: los items largos del popup de navegación van con elipsis MEDIA
/// (cabeza + cola, como los modales de rutas), no truncado derecho: dos
/// entradas de historial con un prefijo común más ancho que el popup deben
/// rendir displays DISTINTOS — la cola (el nombre, lo que identifica la
/// ruta ante un humano) sobrevive.
#[test]
fn popup_items_largos_con_elipsis_media_siguen_distinguibles() {
    let mut app = app_base();
    let prefijo = "x".repeat(70); // > 62 celdas interiores del popup
    app.history[0].push(vp(&format!("file:///{prefijo}/uno.txt")));
    app.history[0].push(vp(&format!("file:///{prefijo}/dos.txt")));
    app.open_nav_popup(norte_tui::app::NavPopupKind::History);
    let texto = render(&app);
    assert!(
        texto.contains("uno.txt") && texto.contains("dos.txt"),
        "las colas distintas sobreviven al recorte (elipsis media): {texto}"
    );
    assert!(texto.contains('…'), "el recorte se marca: {texto}");
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
            name_encoding: None,
        },
    });
    insta::assert_snapshot!(render(&app));
}

/// MINOR-1 (H1 close): `modal_width` medía en `chars`, no en celdas de
/// terminal — un cuerpo con CJK (2 celdas por char) desbordaba la caja. Un
/// nombre japonés en el `to` del modal de copia pin-ea el fit correcto: la
/// caja debe caber en el frame de 80 columnas sin que ratatui recorte el
/// borde ni el path.
#[test]
fn snapshot_modal_confirm_transfer_cjk() {
    let mut app = app_base();
    app.modal = Some(Modal::ConfirmTransfer {
        kind: TransferKind::Copy,
        from: vp("file:///casa/notas.txt"),
        to: vp("file:///otro/日本語のファイル名.txt"),
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
        // 32 hex (128 bits) como produce main.rs — el modal debe caberlo.
        hash_abbrev: "ab12cd34ef56ab78ab12cd34ef56ab78".into(),
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

/// Command palette (`Ctrl+P`/vim `:`, H1 T4): filtrada a "principio" deja
/// DOS filas visibles (`cursor.top`/`viewer.top` — ambas "ir al principio"
/// en ES) con su chord real del preset orthodox — el MISMO builder que usa
/// el binario (`norte_tui::palette::build_rows`), no una copia del formato.
#[test]
fn snapshot_palette_abierta() {
    let mut app = app_base();
    let presets = norte_tui::keymap::presets();
    let (_, preset) = presets.iter().find(|(n, _)| *n == "orthodox").unwrap();
    let build = |screen| {
        norte_tui::keymap::Effective::build_for(preset, &[], norte_tui::keymap::COMMANDS, screen)
            .unwrap()
    };
    let rows = norte_tui::palette::build_rows(
        &build(norte_tui::keymap::Screen::Browse),
        &build(norte_tui::keymap::Screen::Viewer),
    );
    let mut palette = norte_tui::app::Palette::new(rows);
    for c in "principio".chars() {
        palette.push_char(c);
    }
    app.palette = Some(palette);
    insta::assert_snapshot!(render(&app));
}

/// P1: una fila de comando de plugin (`palette::plugin_rows`) filtrada a
/// SOLO ella — con un título hostil (override RTL, corpus `rtl_override`),
/// para pinchar que ni el título ni el prefijo `[extension]` se pintan
/// crudos, y que la fila queda distinguible de un built-in.
#[test]
fn snapshot_palette_fila_de_plugin_hostil() {
    let hostil = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "rtl_override")
        .expect("fixture del corpus");
    let titulo = String::from_utf8_lossy(&hostil.bytes).into_owned();
    let mut app = app_base();
    let plugin = norte_proto::methods::PluginInfo {
        id: "org.evil.demo".into(),
        name: "Evil".into(),
        publisher: "evil".into(),
        version: "1.0.0".into(),
        category: "command".into(),
        capabilities: Vec::new(),
        approved: true,
        enabled: true,
        description: None,
        commands: vec![norte_proto::methods::PluginCommandInfo {
            id: "run".into(),
            title: titulo,
        }],
    };
    // Sin query: `plugin_rows` sobre UN plugin con UN comando ya deja una
    // sola fila — "filtrada a solo ella" por construcción, no por texto
    // tecleado (el título hostil no tiene por qué contener nada buscable).
    let rows = norte_tui::palette::plugin_rows(std::slice::from_ref(&plugin));
    let palette = norte_tui::app::Palette::new(rows);
    app.palette = Some(palette);
    let texto = render(&app);
    // Ver comentario equivalente en `snapshot_extensions_description_hostil_80x24`:
    // el check es sobre el CARÁCTER inyectado, no sobre "ningún control en
    // toda la pantalla" (los saltos de línea de `to_string()` también son
    // controles, falso positivo si se escanea el buffer entero).
    assert!(
        !texto.contains('\u{202E}'),
        "el override RTL del título se pintó crudo: {texto}"
    );
    assert!(
        texto.contains('\u{FFFD}'),
        "el título hostil debe enmascararse a U+FFFD: {texto}"
    );
    insta::assert_snapshot!(texto);
}
