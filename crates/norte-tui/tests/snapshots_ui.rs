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
        attrs: std::collections::BTreeMap::new(),
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

/// G3b (ADR 0037): badge de decorator de plugin, TRAS el hueco del badge de
/// nombre hostil — `docs` lleva un badge con ROL reconocido (color del
/// tema); `src` lleva un badge HOSTIL (control embebido, más largo que el
/// tope de 8 chars) que debe llegar ya ENMASCARADO y TRUNCADO (nunca el
/// control crudo, nunca más de 8 chars); `notas.txt` no lleva decoración —
/// su fila se ve exactamente igual que antes de G3b (sin span extra).
#[test]
fn snapshot_decoracion_de_plugin_badge_hostil_enmascarado() {
    let mut app = app_base();
    let pane = app.focused_mut();
    let by_name = |entries: &[Entry], name: &[u8]| -> VPath {
        entries
            .iter()
            .find(|e| e.path.file_name().is_some_and(|n| n.as_bytes() == name))
            .expect("entrada del fixture")
            .path
            .clone()
    };
    let entries = pane.entries().to_vec();
    let mut decorations = std::collections::HashMap::new();
    decorations.insert(
        by_name(&entries, b"docs"),
        norte_frontend::sanitize_decoration(&norte_proto::methods::DecorationWire {
            badge: Some("M".to_string()),
            role: Some("warning".to_string()),
        }),
    );
    decorations.insert(
        by_name(&entries, b"src"),
        norte_frontend::sanitize_decoration(&norte_proto::methods::DecorationWire {
            badge: Some("A\nBCDEFGHIJ".to_string()),
            role: None,
        }),
    );
    pane.set_decorations(decorations);
    insta::assert_snapshot!(render(&app));
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

/// #108 7a: el picker de columnas a 80×24 — checkbox por fila, flecha del
/// sort en la columna vigente, y una fila de id OPACO hostil (config del
/// usuario con RLO incrustado) pintada ENMASCARADA, jamás cruda (#73).
/// Mismas garantías ruidosas que `snapshot_theme_picker_80x24`: el hint
/// generado presente y entero (sin truncado silencioso del footer).
#[test]
fn snapshot_columns_picker_80x24() {
    let mut app = app_base();
    // Config hostil ANTES de abrir: el picker parte del set resuelto.
    let cfg = norte_config::ColumnsConfig {
        default_columns: Some(vec![
            "name".into(),
            "size".into(),
            "mtime".into(),
            "attr:x\u{202E}evil".into(),
        ]),
        ..Default::default()
    };
    app.columns = norte_frontend::columns::ColumnsSettings::resolve(&cfg);
    app.open_columns_picker();
    let texto = render_80x24(&app);
    let hint = &app.dialog_hints.columns;
    assert!(
        !hint.is_empty(),
        "el preset orthodox liga toggle/sort/confirm/cancel al picker"
    );
    assert!(
        texto.contains(hint.as_str()),
        "el hint generado debe caber ENTERO, sin cortes: hint={hint:?}\n{texto}"
    );
    assert!(
        !texto.contains('\u{202E}'),
        "el RLO de la config jamás llega crudo al terminal:\n{texto}"
    );
    insta::assert_snapshot!(texto);
}

/// #117 encoding-audit L1: un id de config KILOMÉTRICO que no parsea se
/// enseña en el picker CAPADO a `HEADER_MAX_CHARS` (paridad GUI) — sin el
/// cap el overlay entero se ensancharía hasta el frame por un solo id.
#[test]
fn picker_capa_un_id_opaco_kilometrico() {
    let mut app = app_base();
    let kilometrico = "x".repeat(60); // no parsea: ni builtin ni attr:/plugin:
    let cfg = norte_config::ColumnsConfig {
        default_columns: Some(vec!["name".into(), kilometrico.clone()]),
        ..Default::default()
    };
    app.columns = norte_frontend::columns::ColumnsSettings::resolve(&cfg);
    app.open_columns_picker();
    let texto = render_80x24(&app);
    let cap = norte_frontend::columns::HEADER_MAX_CHARS;
    assert!(
        texto.contains(&"x".repeat(cap)),
        "la fila capada debe verse:\n{texto}"
    );
    assert!(
        !texto.contains(&"x".repeat(cap + 1)),
        "jamás más de {cap} chars del id opaco:\n{texto}"
    );
}

/// #108 7b: `[[ui.columns.spec]]` vivo en el pane — `size` con formato SI
/// («1.5 kB», no «1.5 KiB»), cabecera custom `Peso` (sustituye a «Tamaño»)
/// y ancho fijo 9; `kind` alineado a la IZQUIERDA (contenido tras el
/// separador, relleno a la derecha — el default derecho queda pineado por
/// `snapshot_navegacion`). La cabecera hostil no se re-pina aquí: el choke
/// point es `ColumnsSettings::resolve` (unit test en norte-frontend).
#[test]
fn snapshot_columns_spec_size_si_header_custom_kind_izquierda() {
    let izq = vp("file:///casa");
    let der = vp("file:///otro");
    let mut entries = vec![
        entry(&izq, b"docs", EntryKind::Dir, None),
        entry(&izq, b"grande.bin", EntryKind::File, Some(1500)),
        entry(&izq, b"notas.txt", EntryKind::File, Some(420)),
    ];
    sort_entries(&mut entries);
    let mut app = App::new(
        Pane::new(izq, entries),
        Pane::new(
            der.clone(),
            vec![entry(&der, b"cosa", EntryKind::File, Some(1))],
        ),
    );
    app.dialog_hints = default_dialog_hints();
    // `kind` no está en el set por defecto: lista explícita — con el width
    // fijo 9 del spec de size, 10+9+10+9 = 38 celdas casan EXACTAS en el
    // interior del pane y `kind` no se descarta.
    let mut cfg = norte_config::ColumnsConfig {
        default_columns: Some(vec![
            "name".into(),
            "size".into(),
            "mtime".into(),
            "kind".into(),
        ]),
        ..Default::default()
    };
    cfg.specs.insert(
        "size".into(),
        norte_config::ColumnSpec {
            format: Some("si".into()),
            header: Some("Peso".into()),
            width: Some(norte_config::WidthChoice::Fixed(9)),
            ..Default::default()
        },
    );
    cfg.specs.insert(
        "kind".into(),
        norte_config::ColumnSpec {
            align: Some(norte_config::AlignChoice::Left),
            ..Default::default()
        },
    );
    app.columns = norte_frontend::columns::ColumnsSettings::resolve(&cfg);
    let texto = render(&app);
    assert!(texto.contains("Peso"), "cabecera custom del spec:\n{texto}");
    assert!(
        texto.contains("1.5 kB") && !texto.contains("KiB"),
        "size en SI, no IEC:\n{texto}"
    );
    insta::assert_snapshot!(texto);
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
            columns: Vec::new(),
        }],
        errors: Vec::new(),
        cursor: 0,
        config: None,
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
            columns: Vec::new(),
        }],
        errors: Vec::new(),
        cursor: 0,
        config: None,
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

/// G3c: el panel de `[config]` de un plugin (drill-down del gestor de
/// extensiones) — dos claves (`bool` seleccionada, `enum` con una
/// description HOSTIL) se pintan sin bytes crudos, la seleccionada
/// resaltada.
#[test]
fn snapshot_plugin_config_panel_80x24() {
    use norte_frontend::plugin_config::{PluginConfigState, sanitize_config_keys};
    let hostil = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "rtl_override")
        .expect("fixture del corpus");
    let desc_hostil = String::from_utf8_lossy(&hostil.bytes).into_owned();
    let mut app = app_base();
    let wire_keys = vec![
        norte_proto::methods::PluginConfigKeyWire {
            key: "verbose".into(),
            kind: "bool".into(),
            default: "false".into(),
            min: None,
            max: None,
            values: Vec::new(),
            description: None,
            value: "true".into(),
        },
        norte_proto::methods::PluginConfigKeyWire {
            key: "mode".into(),
            kind: "enum".into(),
            default: "fast".into(),
            min: None,
            max: None,
            values: vec!["fast".into(), "thorough".into()],
            description: Some(desc_hostil),
            value: "fast".into(),
        },
    ];
    app.extensions = Some(norte_tui::app::ExtensionManager {
        plugins: Vec::new(),
        errors: Vec::new(),
        cursor: 0,
        config: Some(norte_tui::app::PluginConfigPanel {
            plugin_id: "org.norte.demo".into(),
            plugin_name: "Demo".into(),
            state: PluginConfigState::new(sanitize_config_keys(&wire_keys)),
        }),
    });
    let texto = render_80x24(&app);
    assert!(
        !texto.contains('\u{202E}'),
        "el override RTL de la description se pintó crudo: {texto}"
    );
    assert!(
        texto.contains('\u{FFFD}'),
        "la description hostil debe enmascararse a U+FFFD: {texto}"
    );
    assert!(texto.contains("verbose: true"));
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
        items: vec![vp("file:///casa/notas.txt")],
        to: vp("file:///otro/日本語のファイル名.txt"),
    });
    insta::assert_snapshot!(render(&app));
}

#[test]
fn snapshot_modal_papelera_y_permanente() {
    let mut app = app_base();
    app.modal = Some(Modal::ConfirmDelete {
        items: vec![vp("file:///casa/notas.txt")],
        permanent: false,
    });
    let papelera = render(&app);
    app.modal = Some(Modal::ConfirmDelete {
        items: vec![vp("file:///casa/notas.txt")],
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

/// Abre el overlay de ayuda (H3b) tal cual lo hace el binario: el cheatsheet
/// sintético de la entrada `keys` y el resolver de chords salen de los MISMOS
/// builders (`norte_tui::help::build` y `TuiChords::new`, no una copia del
/// formato) sobre el preset orthodox real, y AMBOS en el idioma del resto de
/// la UI (`vp` fuerza ES arriba). El resolver también, no solo el corpus: es
/// quien pone la etiqueta de cada fila ejecutable (`ChordResolver::label`), y
/// el default de `App` resuelve el idioma del ENTORNO — con él, un lector
/// español leería prosa española con las filas etiquetadas en inglés.
fn open_help(app: &mut App) {
    let presets = norte_tui::keymap::presets();
    let (_, preset) = presets.iter().find(|(n, _)| *n == "orthodox").unwrap();
    let build = |screen| {
        norte_tui::keymap::Effective::build_for(preset, &[], norte_tui::keymap::COMMANDS, screen)
            .unwrap()
    };
    // #113: la sección de diálogos sale del efectivo `dialog`, cuyo
    // vocabulario une COMMANDS y DIALOG_COMMANDS (como `build_keymaps`).
    let dialog_known: Vec<&str> = norte_tui::keymap::COMMANDS
        .iter()
        .copied()
        .chain(norte_tui::keymap::DIALOG_COMMANDS.iter().copied())
        .collect();
    let dialog = norte_tui::keymap::Effective::build_for(
        preset,
        &[],
        &dialog_known,
        norte_tui::keymap::Screen::Dialog,
    )
    .unwrap();
    let browse = build(norte_tui::keymap::Screen::Browse);
    let viewer = build(norte_tui::keymap::Screen::Viewer);
    let lines = norte_tui::help::build(&browse, &viewer, &dialog);
    app.help_chords = std::sync::Arc::new(norte_tui::help::TuiChords::new(
        &browse,
        &viewer,
        &dialog,
        norte_i18n::Lang::Es,
    ));
    app.help = Some(norte_tui::app::HelpView::new(norte_i18n::Lang::Es, lines));
    refresh_help(app);
}

/// H3b: el overlay se maqueta para el frame sobre el que va a pintarse (lo
/// hace el run loop en cada vuelta), y la geometría sale de la MISMA función
/// que usa el pintor. 80×16 es el frame de [`render`].
fn refresh_help(app: &mut App) {
    let (ancho, alto) = ui::help_body_size(ratatui::layout::Rect::new(0, 0, 80, 16));
    app.refresh_help(ancho, alto);
}

/// Como [`render`], pero devuelve el BUFFER: el volcado de texto no lleva
/// estilos, así que un resalte solo se puede pinchar celda a celda.
fn render_buffer(app: &App) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
    terminal.draw(|f| ui::draw(f, app)).expect("draw");
    terminal.backend().buffer().clone()
}

#[test]
fn snapshot_ayuda() {
    let mut app = app_base();
    open_help(&mut app);
    // Arriba: el índice del corpus, donde abre el overlay.
    let arriba = render(&app);
    // Abajo: la página de teclado sintética — el cheatsheet de siempre,
    // ahora una entrada más de la lateral.
    app.help
        .as_mut()
        .unwrap()
        .state
        .open(&norte_help::TopicId::new(norte_frontend::help::KEYS_ID));
    refresh_help(&mut app);
    insta::assert_snapshot!(format!("{arriba}\n===\n{}", render(&app)));
}

/// Fila del frame en la que cae el PIE del overlay de ayuda, para un frame de
/// `w`×`h`.
///
/// Calca la aritmética de `ui::help_layout`, que es privada: la caja va
/// centrada con dos filas de margen vertical, su borde se come una fila, y el
/// pie es la ÚLTIMA fila interior — justo debajo del cuerpo, cuya altura sí
/// publica `ui::help_body_size`. Hace falta para que la aserción de que el
/// filtro se pinta apunte al pie y no al frame entero: ver
/// [`snapshot_ayuda_filtro_hostil`].
fn help_footer_row(w: u16, h: u16) -> usize {
    let alto_caja = h.saturating_sub(2).max(6).min(h);
    let arriba = (h - alto_caja) / 2;
    let (_, alto_cuerpo) = ui::help_body_size(ratatui::layout::Rect::new(0, 0, w, h));
    usize::from(arriba + 1) + alto_cuerpo
}

/// H3b, lección H1 sobre una superficie PINTADA nueva: el filtro de la ayuda
/// empareja los bytes CRUDOS a propósito (`filter_raw` — un needle con un
/// override bidi tiene que encontrar el tema que el lector ve en pantalla), y
/// lo único que puede pintarse es `filter_display`. El pie del overlay es el
/// eco de ese texto tecleado: un `U+202E` que llegue ahí reordena visualmente
/// la línea entera.
///
/// El pie NO es la única entrada libre del draw, y decirlo era falso (review
/// MEDIA): la lateral pinta títulos del corpus y el cuerpo pinta encabezados,
/// celdas de tabla y bloques de código, todos SIN enmascarar y a propósito
/// —son texto del binario— pero sin más red que la puerta de charset del
/// corpus (`no_shipped_topic_carries_a_terminal_hazard`, en `norte-help`) y el
/// catálogo Fluent. Lo que sí es cierto del pie es que es la única entrada
/// TECLEADA, y por eso es la única que se enmascara en el pintor.
///
/// El needle arranca con el token del fixture canónico `rlo`
/// (`norte_testkit::corpus::hostile_chords`) — llega por paste tan fácil como
/// a mano — y con él delante ningún tema casa: la lateral queda vacía y el
/// cuerpo sigue enseñando lo que se estaba leyendo, que es justo el contrato
/// del modelo. Lo que se pincha aquí es el FRAME, no el modelo (que tiene su
/// propio test en `norte_frontend::help`): un snapshot a secas registraría el
/// hazard tan contento.
///
/// El barrido es POR LÍNEA y no sobre el `to_string()` entero: el backend une
/// las filas con `\n`, que `is_terminal_hazard` marca (correctamente) como
/// control — un check ciego sobre todo el buffer daría un falso positivo por
/// el formato del propio volcado. Ver el mismo comentario en
/// `snapshot_extensions_description_hostil_80x24`.
///
/// La aserción anti-vacuidad va acotada al PIE, no al frame (review MEDIA):
/// `app_base` siembra una entrada `\xE9.dat` que se pinta con su propio
/// `U+FFFD` en el pane de detrás, así que un `texto.contains('\u{FFFD}')`
/// sobre todo el frame pasaría aunque el pie no pintase absolutamente nada.
/// Hoy el overlay tapa esa fila a 80×16 y da igual; la maquetación cambió en
/// esta misma fase, así que «hoy da igual» no es un sitio donde apoyarse.
#[test]
fn snapshot_ayuda_filtro_hostil() {
    let rlo = norte_testkit::corpus::hostile_chords()
        .into_iter()
        .find(|c| c.id == "rlo")
        .expect("fixture del corpus");
    let mut app = app_base();
    open_help(&mut app);
    let view = app.help.as_mut().expect("overlay abierto");
    view.state.start_filter();
    for c in std::iter::once(rlo.token).chain("copiar".chars()) {
        view.state.push_char(c);
    }
    refresh_help(&mut app);
    let texto = render(&app);
    for (n, linea) in texto.lines().enumerate() {
        assert!(
            !linea.chars().any(norte_encoding::is_terminal_hazard),
            "el pie del overlay de ayuda pintó un hazard de terminal \
             (fila {n}, fixture {}): {linea:?}\n{texto}",
            rlo.id
        );
    }
    let pie = texto
        .lines()
        .nth(help_footer_row(80, 16))
        .expect("el pie cae dentro del frame");
    assert!(
        pie.contains('\u{FFFD}'),
        "y el filtro SÍ se pinta, enmascarado a U+FFFD — sin esto el test \
         pasaría igual con un pie que no pintase nada:\n{pie:?}\n{texto}"
    );
    assert!(
        pie.contains("copiar"),
        "el resto del needle llega al pie tal cual: el enmascarado es del \
         hazard, no del texto:\n{pie:?}"
    );
    insta::assert_snapshot!(texto);
}

/// La compañera del test de arriba, por el otro camino: un título HOSTIL que
/// llega a la LATERAL.
///
/// El de arriba solo ejercita el pie, y encima con un needle que no casa con
/// nada: la lateral sale vacía y ninguna cadena hostil recorre jamás el camino
/// del título. Este lo recorre, y con la fixture canónica del caso LEGÍTIMO —
/// `bidi_isolate_url` de `norte_testkit::corpus::hostile_titles`, que es
/// `U+2066`…`U+2069` alrededor de un `sftp://`, la forma CORRECTA de meter un
/// tramo LTR en prosa RTL y a la vez cuatro hazards de terminal seguidos.
///
/// El punto de entrada es real: la etiqueta de la entrada sintética `keys` la
/// resuelve el frontend (`t("help-topic-keys")`) y viaja al modelo como título
/// de fila, exactamente igual que un título del corpus o —H3f en adelante— el
/// de un manifiesto de plugin.
///
/// Lo que se afirma es lo que de verdad pasa, no lo que uno querría: el pintor
/// de la lateral NO enmascara, así que el título sale VERBATIM (hasta el
/// recorte por la derecha) y los aislantes bidi llegan al terminal. No es un
/// bug del pintor —el texto es de confianza por construcción— pero sí es la
/// razón por la que la puerta de charset del corpus es LOAD-BEARING y no un
/// cinturón de más: quítala y esto es una inyección a un `.md` de distancia.
#[test]
fn ayuda_un_titulo_hostil_llega_crudo_a_la_lateral() {
    let bidi = norte_testkit::corpus::hostile_titles()
        .into_iter()
        .find(|t| t.id == "bidi_isolate_url")
        .expect("fixture del corpus");
    let mut app = app_base();
    open_help(&mut app);
    // Mismo constructor que usa `HelpView::new`; lo único que cambia es la
    // etiqueta, que aquí es la fixture en vez del catálogo Fluent.
    let view = app.help.as_mut().expect("overlay abierto");
    view.state = norte_frontend::help::HelpState::new(norte_i18n::Lang::Es, bidi.text.to_owned());
    // La entrada sintética es la ÚLTIMA de la lateral y a 80×16 no cabe: se
    // filtra por su id para dejarla sola, que es además el camino por el que
    // el modelo empareja un título (`filter_raw` contra los bytes crudos).
    view.state.start_filter();
    for c in norte_frontend::help::KEYS_ID.chars() {
        view.state.push_char(c);
    }
    refresh_help(&mut app);
    let texto = render(&app);

    let fila = texto
        .lines()
        .find(|l| l.contains('\u{2066}'))
        .unwrap_or_else(|| {
            panic!(
                "el título de la fila sintética `keys` no llegó a la lateral: \
                 el camino que este test existe para recorrer no se recorrió\n{texto}"
            )
        });
    // Verbatim hasta el recorte: el prefijo del título, aislante incluido,
    // sale tal cual. `right_ellipsis` corta por la DERECHA, así que la cabeza
    // sobrevive entera.
    let cabeza: String = bidi.text.chars().take(10).collect();
    assert!(
        fila.contains(&cabeza),
        "la lateral pinta el título VERBATIM (recortado por la derecha): \
         {fila:?} debería empezar por {cabeza:?}"
    );
    assert!(
        fila.chars().any(norte_encoding::is_terminal_hazard),
        "…y sin enmascarar: si esto se pone rojo es que alguien añadió un \
         filtro en el pintor de la lateral, lo cual está BIEN — actualiza este \
         test y la nota de `draw_help` a la vez: {fila:?}"
    );
}

/// Texto de cada fila del buffer, una entrada por fila del frame.
fn row_texts(buf: &ratatui::buffer::Buffer) -> Vec<String> {
    (buf.area.top()..buf.area.bottom())
        .map(|y| {
            (buf.area.left()..buf.area.right())
                .map(|x| buf[(x, y)].symbol())
                .collect()
        })
        .collect()
}

/// Estilos de cada fila del buffer, celda a celda.
fn all_row_styles(buf: &ratatui::buffer::Buffer) -> Vec<Vec<ratatui::style::Style>> {
    (buf.area.top()..buf.area.bottom())
        .map(|y| {
            (buf.area.left()..buf.area.right())
                .map(|x| buf[(x, y)].style())
                .collect()
        })
        .collect()
}

/// H3b: el cuerpo CON EL FOCO. `copying` trae cinco comandos y tres enlaces,
/// y sus filas se pintan al FINAL de la página, detrás de toda la prosa: mover
/// el foco al cuerpo y dar un paso obliga a que la fila activa esté a la vez
/// RESALTADA y VISIBLE. Eso es lo que pincha este test, y la razón de que
/// `HelpView::refresh` llame a `reveal` — sin él el resalte viviría fuera de la
/// ventana y el lector movería un cursor que no ve.
///
/// El snapshot congela el recorte (qué líneas quedaron dentro); el resalte NO
/// puede salir de él —el volcado del backend es texto pelado, sin estilos— así
/// que va aparte, celda a celda.
///
/// La aserción es POSITIVA, y esa es la mitad que faltaba (review MAJOR).
/// Toda la maquinaria del foco descansa en un invariante que cruza dos
/// crates: la acción *i* de `HelpState::actions` se pinta en la línea
/// `Rendered::action_lines[i]`. Las dos mitades están fijadas por separado
/// (`help_render.rs` barre el mapa sobre todo el corpus, el modelo tiene sus
/// propios tests), pero el sitio donde se ENCUENTRAN es este, y aquí solo se
/// comparaban DOS filas por desigualdad: desplaza el mapa una posición y se
/// resalta `f5` mientras el modelo dice `f6` — los vectores siguen siendo
/// distintos, el test sigue pasando, y el usuario ve bajo el cursor una fila
/// que dice `pane.copy` mientras `Enter` despacha `pane.move`. Una etiqueta
/// mentirosa sobre una superficie que MUTA ficheros.
///
/// Se localiza la fila resaltada sin nombrarla: se pinta el MISMO frame con y
/// sin foco en el cuerpo y se diferencian los estilos fila a fila. La única
/// que cambia es la que lleva el resalte, y de ella se afirma que su TEXTO
/// nombra el comando que el modelo dice tener enfocado — con el chord y la
/// etiqueta que da el propio resolver, no una copia del formato. Comparar
/// contra el estilo concreto del tema ataría el test a la paleta.
#[test]
fn snapshot_ayuda_cuerpo_con_foco() {
    use norte_help::ChordResolver;

    let mut app = app_base();
    open_help(&mut app);
    let view = app.help.as_mut().expect("overlay abierto");
    view.state.open(&norte_help::TopicId::new("copying"));
    view.state.toggle_focus();
    assert_eq!(
        view.state.focus(),
        norte_frontend::help::Focus::Body,
        "el tema tiene filas ejecutables, así que el foco SÍ entra"
    );
    // Un paso: la SEGUNDA fila, para que esto no pueda pasar con un pintor que
    // resalte siempre la primera.
    view.state.down();
    let Some(norte_frontend::help::Action::Run(comando)) = view.state.action().cloned() else {
        panic!("la fila con foco es una fila ejecutable");
    };
    assert_eq!(
        comando, "pane.move",
        "la fila con foco es la de `pane.move`"
    );
    refresh_help(&mut app);

    let scroll = app.help.as_ref().unwrap().state.body_scroll();
    assert!(
        scroll > 0,
        "las filas van tras la prosa: revelarlas OBLIGA a desplazar el cuerpo \
         (scroll={scroll})"
    );
    let con_foco = render_buffer(&app);
    let texto = render(&app);

    // Y el resalte es DEL FOCO, no de la fila: devuelto el foco a la lateral,
    // el cursor del cuerpo sigue existiendo pero ya no es el que mueven las
    // flechas, y ninguna fila queda marcada. El mismo gesto sirve de PATRÓN
    // para localizar la fila resaltada: entre los dos frames no cambia nada
    // más (el `reveal` ya no mueve el scroll, que este test acaba de fijar).
    app.help.as_mut().unwrap().state.toggle_focus();
    refresh_help(&mut app);
    let sin_foco = render_buffer(&app);

    let estilos_con = all_row_styles(&con_foco);
    let estilos_sin = all_row_styles(&sin_foco);
    let distintas: Vec<usize> = (0..estilos_con.len())
        .filter(|&y| estilos_con[y] != estilos_sin[y])
        .collect();
    assert_eq!(
        distintas.len(),
        1,
        "exactamente UNA fila del frame cambia al quitar el foco del cuerpo; \
         cambiaron {distintas:?}"
    );

    // Y esa fila es la del comando que el MODELO dice tener enfocado. El
    // chord y la etiqueta salen del resolver que usa el pintor, así que esto
    // no puede pasar con una copia del formato de fila que se haya quedado
    // atrás.
    let resolver = std::sync::Arc::clone(&app.help_chords);
    let chord = resolver
        .chord(&comando)
        .unwrap_or_else(|| panic!("{comando} tiene chord en el preset orthodox"));
    let etiqueta = resolver.label(&comando);
    let fila = &row_texts(&con_foco)[distintas[0]];
    assert!(
        fila.contains(&chord) && fila.contains(&etiqueta),
        "la fila resaltada tiene que ser la de `{comando}` ({chord} / \
         {etiqueta}), no otra: {fila:?}"
    );
    // …y NO la de su vecina. Un mapa desplazado una posición resaltaría
    // `pane.copy` mientras `Enter` despacha `pane.move`.
    let vecino = "pane.copy";
    let chord_vecino = resolver
        .chord(vecino)
        .unwrap_or_else(|| panic!("{vecino} tiene chord en el preset orthodox"));
    assert!(
        !fila.contains(&resolver.label(vecino)) && !fila.contains(&chord_vecino),
        "la fila resaltada es la de la acción VECINA: el mapa acción→línea \
         está desplazado: {fila:?}"
    );

    insta::assert_snapshot!(texto);
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
        columns: Vec::new(),
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

fn cfg_vacia() -> norte_tui::config::LoadedConfig {
    norte_tui::config::load(&norte_config::Layers { dirs: vec![] }).expect("config vacía carga")
}

/// Overlay de ajustes (S3), abierto sobre la config VACÍA (valores default
/// de S2) — el MISMO builder que usa el binario
/// (`norte_tui::settings::build_rows`), no una copia a mano. A 80×24: el
/// catálogo completo (9 filas generales + la informativa de Plugins, más
/// dos cabeceras de sección) no cabe en las 16 filas del resto del archivo.
#[test]
fn snapshot_settings_abierta() {
    let mut app = app_base();
    let settings =
        norte_tui::app::Settings::new(norte_tui::settings::build_rows(&cfg_vacia(), &[]));
    app.settings = Some(settings);
    insta::assert_snapshot!(render_80x24(&app));
}

/// Revisión S, M3: con el overlay de ajustes Y un modal AMBOS abiertos (el
/// enrutado de teclas ya trata al modal como AUTORITATIVO en este caso,
/// `modal_preempts_settings`), el modal debe pintarse ENCIMA — antes se
/// pintaba antes que `draw_settings` en `ui::draw`, así que el overlay lo
/// tapaba visualmente aunque las teclas seguían yendo al modal. Pin: el
/// título del modal ("papelera") es visible en el snapshot, no enterrado
/// bajo la lista de ajustes.
#[test]
fn snapshot_modal_pinta_encima_del_overlay_de_ajustes() {
    let mut app = app_base();
    let settings =
        norte_tui::app::Settings::new(norte_tui::settings::build_rows(&cfg_vacia(), &[]));
    app.settings = Some(settings);
    app.modal = Some(Modal::ConfirmDelete {
        items: vec![vp("file:///casa/notas.txt")],
        permanent: false,
    });
    insta::assert_snapshot!(render_80x24(&app));
}

/// Mismo caso que arriba, con la PALETTE en vez del overlay de ajustes
/// (`modal_preempts_palette`) — la otra mitad de la clase H1 MINOR-4.
#[test]
fn snapshot_modal_pinta_encima_de_la_palette() {
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
    app.palette = Some(norte_tui::app::Palette::new(rows));
    app.modal = Some(Modal::ConfirmDelete {
        items: vec![vp("file:///casa/notas.txt")],
        permanent: false,
    });
    insta::assert_snapshot!(render(&app));
}

/// Filtrada + EDITANDO (S3): filtra a la fila `Text` `ui.font` (el espacio
/// final la aísla de `ui.font-size`, ver `app::settings_tests`), `activate`
/// abre el buffer de edición y se teclea un valor — pincha que la lista
/// filtrada, la descripción y el footer de edición (`settings-edit-hint`)
/// se pintan juntos sin pisarse.
#[test]
fn snapshot_settings_filtrada_y_editando_texto() {
    let mut app = app_base();
    let mut settings =
        norte_tui::app::Settings::new(norte_tui::settings::build_rows(&cfg_vacia(), &[]));
    for c in "ui.font ".chars() {
        settings.push_char(c);
    }
    settings.activate(&[], &[]);
    for c in "Fira Code".chars() {
        settings.edit_push_char(c);
    }
    app.settings = Some(settings);
    insta::assert_snapshot!(render_80x24(&app));
}
