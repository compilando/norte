//! Snapshots del render (fase 10, insta): la forma EXACTA de cada pantalla
//! queda congelada — cualquier cambio visual es un diff consciente
//! (`cargo insta review`). Determinista: idioma fijo, datos fijos.

use norte_core::TransferOptions;
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, Modal, Pane, Trail, TransferKind, sort_entries};
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

/// #210: en la barra de estado la RUTA cede, y el contador `pos/total` no.
///
/// Con una ruta larga, ratatui recortaba la cola: lo que quedaba a la vista
/// era un dígito suelto del total, que se lee como cualquier otra cosa. Ahora
/// la ruta se elipsa por el medio —el principio dice dónde estás y el final
/// qué carpeta es— y todo lo que viene detrás sobrevive entero.
/// #149: el aviso de espacio se pinta DEBAJO del destino y ENCIMA de las
/// teclas — lo último que se lee antes de decidir.
///
/// Y solo cuando lo hay: que quepa, que el destino no sepa decir cuánto le
/// queda o que no se sepa cuánto se va a mover se callan las tres, porque un
/// «sí cabe» en cada copia enseña a no leer la línea.
#[test]
fn el_modal_de_transferencia_pinta_el_aviso_de_espacio() {
    let dir = vp("file:///casa");
    let pintar = |space: Option<String>| {
        let mut app = App::new(
            Pane::new(dir.clone(), Vec::new()),
            Pane::new(dir.clone(), Vec::new()),
        );
        app.dialog_hints = default_dialog_hints();
        app.modal = Some(Modal::ConfirmTransfer {
            kind: TransferKind::Copy,
            items: vec![vp("file:///casa/a.bin"), vp("file:///casa/b.bin")],
            to: vp("file:///medios"),
            space,
        });
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
        terminal.draw(|f| ui::draw(f, &app)).expect("draw");
        terminal.backend().to_string()
    };

    let aviso = norte_frontend::space::warning(
        Some(4_200_000_000),
        Some(1_100_000_000),
        norte_i18n::active(),
    )
    .expect("no cabe: hay aviso");
    let con = pintar(Some(aviso.clone()));
    let lineas: Vec<&str> = con.lines().collect();
    let fila = |aguja: &str| {
        lineas
            .iter()
            .position(|l| l.contains(aguja))
            .unwrap_or_else(|| panic!("falta {aguja:?} en:\n{con}"))
    };
    let destino = fila("medios");
    let avisada = fila(aviso.split_whitespace().next().expect("primera palabra"));
    assert!(avisada > destino, "el aviso va debajo del destino:\n{con}");

    // Sin aviso, ni rastro de él.
    let sin = pintar(None);
    assert!(!sin.contains(&aviso), "cuando cabe no se dice nada:\n{sin}");
}

#[test]
fn la_barra_de_estado_recorta_la_ruta_y_no_el_contador() {
    let hondo = vp(&format!(
        "file:///{}",
        ["carpeta-con-nombre-larguisimo"; 6].join("/")
    ));
    let mut app = App::new(
        Pane::new(
            hondo.clone(),
            (0..42)
                .map(|i| {
                    entry(
                        &hondo,
                        format!("f{i:03}.txt").as_bytes(),
                        EntryKind::File,
                        Some(1),
                    )
                })
                .collect(),
        ),
        Pane::new(hondo, Vec::new()),
    );
    app.focused_mut().set_cursor(7);

    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
    ui::before_frame(&mut app, ratatui::layout::Rect::new(0, 0, 80, 24));
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let barra = terminal
        .backend()
        .to_string()
        .lines()
        .last()
        .expect("barra de estado")
        .to_owned();

    assert!(
        barra.contains("8/42"),
        "el contador entero, que es lo que dice cuánto hay: {barra:?}"
    );
    assert!(
        barra.contains('…'),
        "y la ruta cede por el medio: {barra:?}"
    );
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
    app.open_columns_picker(&[]);
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
    app.open_columns_picker(&[]);
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
            has_help: false,
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
            has_help: false,
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
        space: None,
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

/// H3c: mientras una página de ayuda ABIERTA DESDE el modal lo tapa, la ayuda
/// se queda las teclas (`HelpView::over_modal`) y los verbos del modal son
/// INERTES. Un pie que siguiera ofreciéndolos mentiría — `y` y `n` no harían
/// nada — así que dice lo que es verdad: cierra la ayuda para responder.
///
/// Lo que NO cambia: la caja y la pregunta siguen visibles, encima de la ayuda
/// (el modal se pinta el último). Esconder la pregunta es el defecto que H1
/// arregló pintándolo así, y esto no lo deshace.
///
/// Las aserciones van contra el hint GENERADO del modal (`DialogHints`) y no
/// contra etiquetas sueltas: `[Enter] confirmar` sale también en el pie de la
/// PROPIA ayuda, donde sí está vivo, así que buscar la etiqueta a secas en el
/// frame confundiría dos pies distintos.
#[test]
fn el_pie_del_modal_no_ofrece_verbos_inertes_bajo_la_ayuda() {
    // Idioma fijo ANTES del primer `t()`: el catálogo se resuelve una vez, y
    // `vp` (que es quien lo fuerza en el resto del archivo) todavía no ha
    // corrido aquí.
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    let etiqueta = |cmd: &str| norte_i18n::t(&norte_tui::keymap::dialog_hint_id(cmd));
    let aviso = norte_i18n::t("modal-hint-help-open");
    let verbos = default_dialog_hints().approval;

    let mut app = app_base();
    app.modal = Some(Modal::ApproveAgentOp {
        req: norte_proto::methods::PolicyApprovalRequired {
            approval_id: 1,
            session: Some("s1".into()),
            op: "copy".into(),
            paths: vec!["mem:///a".into()],
            paths_total: 0,
            ttl_ms: 60_000,
        },
    });
    open_help_over_modal(&mut app);
    let tapado = render(&app);

    assert!(
        tapado.contains(&aviso),
        "el pie tiene que decir por qué las teclas del modal no responden:\n{tapado}"
    );
    assert!(
        !tapado.contains(&verbos),
        "el pie sigue ofreciendo los verbos inertes ({verbos:?}):\n{tapado}"
    );
    // Y ni sueltas: `aprobar`/`denegar` solo puede pintarlas este modal (el pie
    // de la ayuda lista los suyos, que sí responden).
    for cmd in ["dialog.approve", "dialog.deny"] {
        assert!(
            !tapado.contains(&etiqueta(cmd)),
            "{cmd} está inerte y el pie lo sigue ofreciendo:\n{tapado}"
        );
    }
    // Y la pregunta NO se esconde: el título y la ruta que se aprueba siguen
    // ahí, encima de la página (el modal se pinta el último, H1).
    assert!(
        tapado.contains(&norte_i18n::t("modal-approval-title")),
        "la pregunta tiene que seguir a la vista:\n{tapado}"
    );
    assert!(
        tapado.contains("mem:///a"),
        "…y la ruta con ella:\n{tapado}"
    );

    // Cerrada la ayuda, el modal recupera sus verbos: la tecla vuelve a hacer
    // lo que el pie dice.
    app.help = None;
    let visible = render(&app);
    assert!(
        !visible.contains(&aviso),
        "sin ayuda por encima no hay nada que cerrar:\n{visible}"
    );
    assert!(
        visible.contains(&verbos),
        "los verbos vuelven al pie en cuanto la ayuda se cierra:\n{visible}"
    );
}

/// El mismo pie honesto en TODOS los modales con hint generado, no solo en la
/// aprobación: la mentira es idéntica en una confirmación de borrado, en una
/// colisión y en una host key sin confiar.
///
/// El par de aserciones por modal es lo que le da fuerza: con la ayuda cerrada
/// su hint generado se pinta ENTERO (si no, la mitad de abajo no probaría nada),
/// y con la ayuda encima no queda ni rastro de él.
#[test]
fn ningun_modal_con_hint_generado_ofrece_verbos_bajo_la_ayuda() {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    let aviso = norte_i18n::t("modal-hint-help-open");
    let hints = default_dialog_hints();
    let modales = [
        (
            Modal::ConfirmDelete {
                items: vec![vp("file:///casa/notas.txt")],
                permanent: true,
            },
            hints.confirm.clone(),
        ),
        (
            Modal::ConfirmTransfer {
                kind: TransferKind::Copy,
                items: vec![vp("file:///casa/notas.txt")],
                to: vp("file:///otro"),
                space: None,
            },
            hints.confirm.clone(),
        ),
        (Modal::ConfirmQuit, hints.confirm.clone()),
        (
            Modal::Collision {
                retry: RetrySpec {
                    kind: TransferKind::Copy,
                    from: vp("file:///casa/notas.txt"),
                    to: vp("file:///otro/notas.txt"),
                    opts: TransferOptions::default(),
                    name_encoding: None,
                },
            },
            hints.collision.clone(),
        ),
        (
            Modal::TrustHostKey {
                host: "ejemplo.org".into(),
                port: Some(22),
                algo: "ssh-ed25519".into(),
                fingerprint: "SHA256:abc".into(),
                dir: vp("sftp://ejemplo.org/casa"),
                pane: 0,
                trail: Trail::Record,
            },
            hints.trust_host.clone(),
        ),
    ];
    for (modal, verbos) in modales {
        let mut app = app_base();
        app.modal = Some(modal);

        // Sin ayuda: el pie generado se pinta entero.
        let solo = render(&app);
        assert!(
            solo.contains(&verbos),
            "este modal no pinta su hint entero, así que la otra mitad del test \
             no probaría nada ({verbos:?}):\n{solo}"
        );

        // Con la ayuda encima: ni un verbo, y el aviso en su lugar.
        open_help_over_modal(&mut app);
        let tapado = render(&app);
        assert!(
            tapado.contains(&aviso),
            "este modal no dice por qué sus teclas no responden:\n{tapado}"
        );
        assert!(
            !tapado.contains(&verbos),
            "este modal sigue ofreciendo verbos inertes ({verbos:?}):\n{tapado}"
        );
    }
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
        pane: 0,
        trail: Trail::Record,
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
        pane: 0,
        trail: Trail::Record,
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
        pane: 0,
        trail: Trail::Record,
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
    // H3d: y los hechos del contexto se congelan igual que en el binario
    // (`open_contextual_help`), así que estos snapshots registran lo que un
    // lector ve DESDE `app_base` — con `file:///casa` escribible, nada
    // atenuado por el backend, y las filas de `nav.enter`/`pane.view` decididas
    // por lo que hay bajo el cursor.
    app.freeze_help_facts();
    app.help = Some(norte_tui::app::HelpView::new(norte_i18n::Lang::Es, lines));
    refresh_help(app);
}

/// La misma ayuda, pero abierta ENCIMA de un modal (H3c, `over_modal`): la que
/// se queda las teclas, con lo que los verbos del modal quedan inertes hasta
/// que se cierre.
fn open_help_over_modal(app: &mut App) {
    open_help(app);
    app.help.as_mut().expect("la ayuda se abrió").over_modal = true;
    refresh_help(app);
}

/// H3b: el overlay se maqueta para el frame sobre el que va a pintarse (lo
/// hace el run loop en cada vuelta), y la geometría sale de la MISMA función
/// que usa el pintor. 80×16 es el frame de [`render`].
fn refresh_help(app: &mut App) {
    refresh_help_en(app, 80, 16);
}

/// [`refresh_help`] sobre un frame de `w`×`h`: la geometría del overlay ya no
/// depende solo del frame — la lateral se dimensiona a los títulos del corpus
/// del idioma abierto — así que el idioma sale del propio modelo, como en el
/// run loop.
fn refresh_help_en(app: &mut App, w: u16, h: u16) {
    let Some(lang) = app.help.as_ref().map(|v| v.state.lang()) else {
        return;
    };
    let (ancho, alto) = ui::help_body_size(ratatui::layout::Rect::new(0, 0, w, h), lang);
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

/// H3b, pie ADAPTATIVO: el pie de la ayuda ofrece sus CINCO verbos imprimibles
/// y deja que el ancho decida cuántos se pintan (`ui::fit_hint_groups` tira
/// grupos ENTEROS por la cola y marca la pérdida con `…`).
///
/// Antes excluía `[enter]`/`[esc]` SIEMPRE, para honrar un frame de 80: en un
/// terminal de 113 columnas el pie se quedaba medio vacío con las dos teclas
/// universales escondidas sin motivo. La exclusión fija cede ahora al mecanismo
/// que ya existía.
///
/// Lo que se pincha es el invariante, no un ancho concreto: a 113 caben los
/// cinco (y sin `…`, porque no se perdió nada); a 80 sobreviven los del
/// principio del ranking — los que el lector NO puede adivinar — y el `…` dice
/// que hubo recorte. En ningún ancho aparece medio grupo.
#[test]
fn el_pie_de_la_ayuda_se_adapta_al_ancho() {
    let pie_a = |w: u16, h: u16| {
        let mut app = app_base();
        open_help(&mut app);
        refresh_help_en(&mut app, w, h);
        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
        terminal.draw(|f| ui::draw(f, &app)).expect("draw");
        terminal
            .backend()
            .to_string()
            .lines()
            .nth(help_footer_row(w, h))
            .expect("el pie cae dentro del frame")
            .to_owned()
    };

    let ancho = pie_a(124, 16);
    for verbo in [
        "índice ↔ texto",
        "bajar",
        "subir",
        "filtrar",
        "atrás",
        "confirmar",
        "cancelar",
    ] {
        assert!(
            ancho.contains(verbo),
            "en un frame ancho caben los siete grupos, y `{verbo}` falta: {ancho:?}"
        );
    }
    assert!(
        !ancho.contains('…'),
        "…y sin marca de pérdida, porque no se perdió nada: {ancho:?}"
    );

    let estrecho = pie_a(80, 16);
    // Los que sobreviven son la CABEZA del ranking: cómo se pasa al texto y
    // cómo se baja por él, que es lo que nadie adivina en una pantalla que no
    // se parece a ninguna otra del programa.
    for verbo in ["índice ↔ texto", "bajar"] {
        assert!(
            estrecho.contains(verbo),
            "a 80 columnas sobreviven los verbos que el lector no puede \
             adivinar, y `{verbo}` falta: {estrecho:?}"
        );
    }
    assert!(
        estrecho.contains('…'),
        "y el recorte se MARCA — un pie recortado en silencio miente: {estrecho:?}"
    );
    // Grupo entero o nada: ningún corchete queda huérfano.
    for pie in [&ancho, &estrecho] {
        assert_eq!(
            pie.matches('[').count(),
            pie.matches(']').count(),
            "medio grupo `[chord] etiqueta` en el pie: {pie:?}"
        );
    }
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
///
/// El corte VERTICAL no cambió al dimensionar la lateral por contenido: el
/// idioma que pide ahora `help_body_size` decide el reparto de ANCHO y nada
/// más, así que aquí sirve cualquiera.
fn help_footer_row(w: u16, h: u16) -> usize {
    let alto_caja = h.saturating_sub(2).max(6).min(h);
    let arriba = (h - alto_caja) / 2;
    let (_, alto_cuerpo) =
        ui::help_body_size(ratatui::layout::Rect::new(0, 0, w, h), norte_i18n::Lang::Es);
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
/// H3e: la página de un plugin HOSTIL, compuesta — lateral y cuerpo a la vez.
///
/// Los tests unitarios de `help_render` pinchan las CADENAS (que el título se
/// enmascara, que la insignia aparece); esto pincha la MAQUETA, que es donde
/// vivía el fallo que la revisión de seguridad encontró: la insignia se
/// apagaba sola cuando el plugin no declaraba publicador, y la página quedaba
/// con la forma exacta —título, regla, cuerpo— de una del manual. Un snapshot
/// registra esa forma; un test de cadenas, no.
///
/// El plugin es lo peor que se puede mandar por el wire y sigue siendo legal:
/// `name` con override RTL (corpus `rtl_override`), `publisher` VACÍO — el
/// manifiesto exige el campo pero no que tenga contenido — y una página que se
/// hace pasar por la documentación de la app.
#[test]
fn snapshot_ayuda_pagina_de_plugin_hostil() {
    let hostil = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "rtl_override")
        .expect("fixture del corpus");
    let nombre = String::from_utf8_lossy(&hostil.bytes).into_owned();
    let mut app = app_base();
    open_help(&mut app);
    let plugin = norte_proto::methods::PluginInfo {
        id: "org.evil.demo".into(),
        name: nombre,
        publisher: String::new(),
        version: "1.0.0".into(),
        category: "command".into(),
        capabilities: Vec::new(),
        approved: false,
        enabled: false,
        description: None,
        commands: vec![norte_proto::methods::PluginCommandInfo {
            id: "run".into(),
            title: "Aprobar es seguro".into(),
        }],
        columns: Vec::new(),
        has_help: true,
    };
    app.freeze_help_plugins(std::slice::from_ref(&plugin));
    let view = app.help.as_mut().expect("overlay abierto");
    view.state.open(&norte_help::TopicId::new("org.evil.demo"));
    // La página que un plugin firmaría para que la lea quien va a aprobarlo, y
    // que `main::extensions_help` pone a una tecla del gestor de extensiones.
    let parsed = norte_help::parse_untrusted(
        "+++\nid = \"org.evil.demo\"\ntitle = \"Aprobar extensiones\"\n\
         commands = [\"plugin:org.evil.demo:run\"]\n+++\n\
         Las extensiones del catálogo est\u{202E}án auditadas.\u{200B}"
            .as_bytes(),
        "org.evil.demo",
        None,
    );
    view.state.install_plugin_topic(parsed.topic);
    refresh_help(&mut app);
    let texto = render(&app);
    // Mismo barrido POR LÍNEA que `snapshot_ayuda_filtro_hostil`, y por la
    // misma razón (los `\n` del volcado son controles).
    for (n, linea) in texto.lines().enumerate() {
        assert!(
            !linea.chars().any(norte_encoding::is_terminal_hazard),
            "la página de plugin pintó un hazard de terminal (fila {n}): \
             {linea:?}\n{texto}"
        );
    }
    // Lo que el snapshot NO puede afirmar por sí solo: que la página se
    // DECLARA de un tercero incluso sin publicador que nombrar.
    assert!(
        texto.contains(&norte_i18n::t_in(
            norte_i18n::Lang::Es,
            "help-plugin-origin"
        )),
        "sin marca de procedencia la página se lee como del manual:\n{texto}"
    );
    insta::assert_snapshot!(texto);
}

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

/// Como [`render`] pero sobre un frame de `w`×`h`, maquetando la ayuda para
/// ESE frame: la geometría del overlay depende de las dos dimensiones y el
/// pre-render tiene que medir lo mismo que el pintor.
fn render_ayuda(app: &mut App, w: u16, h: u16) -> String {
    refresh_help_en(app, w, h);
    let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
    terminal.draw(|f| ui::draw(f, app)).expect("draw");
    terminal.backend().to_string()
}

/// La lateral se dimensiona a SU CONTENIDO, entre dos topes.
///
/// Antes era una constante de 24 celdas y a 113 columnas recortaba cinco de
/// las nueve filas del corpus español — con 90 celdas de prosa al lado, que es
/// más ancho del que se lee de un vistazo. Ahora pide lo que miden sus filas
/// (sangría incluida, en CELDAS) y se queda entre el suelo de siempre y un
/// tercio largo del frame.
///
/// Los dos extremos que se fijan aquí son los dos que pueden romperse por
/// separado: que el contenido MANDE (un corpus con títulos más largos ensancha
/// la lateral) y que el tope AGUANTE (en un frame estrecho no se la come).
#[test]
fn la_lateral_de_la_ayuda_se_dimensiona_a_sus_titulos() {
    use norte_i18n::Lang;
    use ratatui::layout::Rect;
    use unicode_width::UnicodeWidthStr;

    // Lo que miden las filas del corpus: la sangría de dos celdas más el
    // título más ancho, o la cabecera de grupo más ancha si ganase.
    let ancho_pedido = |lang| {
        norte_help::topics(lang)
            .iter()
            .map(|t| 2 + t.title.width())
            .max()
            .expect("el corpus trae temas")
    };
    let es = ancho_pedido(Lang::Es);
    let en = ancho_pedido(Lang::En);
    assert_ne!(
        es, en,
        "los dos corpus miden lo mismo ({es}); con títulos igual de anchos, \
         una lateral de ancho CONSTANTE pasaría lo de abajo y este test no \
         distinguiría «manda el contenido» de «siempre lo mismo». Cámbiale el \
         título a un tema o compara contra otro par."
    );

    // Frame ANCHO: manda el contenido, y dos corpus distintos dan dos anchos
    // distintos. Una constante pasaría lo de abajo y fallaría aquí.
    let ancho = Rect::new(0, 0, 160, 40);
    assert_eq!(
        usize::from(ui::help_sidebar_width(ancho, Lang::Es)),
        es,
        "con sitio de sobra la lateral pide exactamente lo que mide su fila \
         más ancha"
    );
    assert_eq!(usize::from(ui::help_sidebar_width(ancho, Lang::En)), en);

    // Frame ESTRECHO: el tope del 35 % del frame gana, y el suelo histórico
    // de 24 celdas se respeta — ni una lateral que se come la prosa ni una
    // más angosta que la de antes.
    for w in [80u16, 100, 113] {
        let lateral = ui::help_sidebar_width(Rect::new(0, 0, w, 40), Lang::Es);
        assert!(
            lateral >= 24,
            "a {w} columnas la lateral encogió por debajo del ancho que tenía \
             fijo ({lateral})"
        );
        assert!(
            u32::from(lateral) * 100 <= u32::from(w) * 35,
            "a {w} columnas la lateral se pasa del 35 % del frame ({lateral})"
        );
    }
    // …y el tope es lo que MUERDE a 80 columnas: la lateral pedía 39.
    assert_eq!(
        ui::help_sidebar_width(Rect::new(0, 0, 80, 40), Lang::Es),
        28
    );

    // Y el cuerpo tiene medida tipográfica: la prosa no crece con el terminal
    // más allá de lo que se lee de un vistazo. Una celda menos que la medida:
    // la última columna del cuerpo es su barra de scroll.
    let (cuerpo, _) = ui::help_body_size(Rect::new(0, 0, 200, 40), Lang::Es);
    assert_eq!(cuerpo, 71, "la prosa se corta en su medida, no en el borde");
}

/// …y con sitio, NINGÚN título sale recortado.
///
/// La comprobación de arriba es aritmética; ésta es sobre el frame pintado, que
/// es donde se ve si la sangría, el canalón o la elipsis se comieron una celda
/// de más. A 120 columnas caben las nueve filas del corpus español enteras.
#[test]
fn a_120_columnas_ningun_titulo_de_la_ayuda_sale_recortado() {
    let mut app = app_base();
    open_help(&mut app);
    let texto = render_ayuda(&mut app, 120, 36);
    for tema in norte_help::topics(norte_i18n::Lang::Es) {
        assert!(
            texto.lines().any(|l| l.contains(&tema.title)),
            "el título {:?} no aparece entero en la lateral:\n{texto}",
            tema.title
        );
    }
    assert!(
        !texto
            .lines()
            .any(|l| l.contains("…") && l.contains("  SFTP")),
        "…y sin elipsis en la fila más larga:\n{texto}"
    );
}

/// El pie dice DÓNDE está el lector, con el mismo idioma que el visor
/// (`{primera visible}/{total}`), y se calla cuando la página cabe entera.
///
/// No es adorno: las filas ejecutables de un tema se pintan DETRÁS de toda su
/// prosa, así que en una página larga no entran en el primer render y sin el
/// indicador nada dice que estén ahí.
#[test]
fn el_pie_de_la_ayuda_situa_al_lector_solo_cuando_hace_falta() {
    let mut app = app_base();
    open_help(&mut app);

    // 80×16: el índice no cabe ni de lejos en las 12 filas del cuerpo.
    let texto = render_ayuda(&mut app, 80, 16);
    let view = app.help.as_ref().expect("overlay abierto");
    let total = view.body().0.len();
    let (_, alto) = ui::help_body_size(ratatui::layout::Rect::new(0, 0, 80, 16), view.state.lang());
    assert!(total > alto, "el índice no cabe en {alto} filas ({total})");
    let pie = texto
        .lines()
        .nth(help_footer_row(80, 16))
        .expect("el pie cae dentro del frame");
    // Pegado al borde derecho de la caja: el volcado del backend entrecomilla
    // cada fila, así que el ancla es el `│` de la caja y no el fin de línea.
    assert!(
        pie.contains(&format!("1/{total} │")),
        "el pie sitúa al lector en la primera línea, a la DERECHA: {pie:?}"
    );

    // Y sigue al scroll. El foco entra en el cuerpo para que `page_down`
    // desplace (con el foco en la lateral mueve el cursor de temas), lo que de
    // paso hace que `refresh` REVELE la acción con foco: da igual cuánto se
    // mueva el cuerpo — lo que se fija es que el pie dice la línea que de
    // verdad está arriba, no que se movieran cinco.
    app.help.as_mut().expect("overlay").state.toggle_focus();
    app.help.as_mut().expect("overlay").state.page_down(5);
    let texto = render_ayuda(&mut app, 80, 16);
    let scroll = app.help.as_ref().expect("overlay").state.body_scroll();
    assert!(scroll > 0, "el cuerpo se desplazó");
    let pie = texto
        .lines()
        .nth(help_footer_row(80, 16))
        .expect("el pie cae dentro del frame");
    assert!(
        pie.contains(&format!("{}/{total} │", scroll + 1)),
        "el indicador va con el scroll ({scroll}): {pie:?}"
    );

    // Frame de sobra: la página entra entera y el indicador SOBRA — un `1/9`
    // sobre nueve líneas visibles no informa de nada. El alto va holgado a
    // propósito: el índice CRECE con cada página que H3h escribe, y un frame
    // ajustado convertiría "escribir una página" en "arreglar este test".
    let texto = render_ayuda(&mut app, 120, 90);
    let view = app.help.as_ref().expect("overlay abierto");
    let total = view.body().0.len();
    let (_, alto) =
        ui::help_body_size(ratatui::layout::Rect::new(0, 0, 120, 90), view.state.lang());
    assert!(total <= alto, "la página cabe en {alto} filas ({total})");
    let pie = texto
        .lines()
        .nth(help_footer_row(120, 90))
        .expect("el pie cae dentro del frame");
    assert!(
        !pie.contains(&format!("/{total}")),
        "con la página entera a la vista el pie no dice nada: {pie:?}"
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
/// H3d de punta a punta, sobre el FRAME: con los dos panes dentro de un zip
/// (`READ_ONLY` por construcción del scheme, ADR 0018), las filas de la página
/// de copiado que ESCRIBEN salen con su razón al lado, no solo atenuadas.
///
/// Es lo único que ata la cadena completa —`freeze_help_facts` → la tabla
/// compartida → `row_line`— a lo que se ve: los tres tramos tienen su test
/// unitario, y ninguno se rompería si el congelado dejara de llamarse al abrir.
///
/// Se recorre el cuerpo con el foco (como `snapshot_ayuda_cuerpo_con_foco`)
/// porque las filas ejecutables van TRAS la prosa: sin desplazar, la razón
/// existe y no está en el frame. El frame es de 100×30 — un terminal real, no
/// uno holgado a medida — y la razón sale ENTERA porque `row_line` la presupuesta
/// antes que la etiqueta: quien cede es el nombre del comando, que ya está en la
/// prosa de arriba y en la columna del chord.
#[test]
fn la_ayuda_dentro_de_un_zip_pinta_la_razon_del_veto() {
    let dentro = vp("zip+file:///a.zip/!");
    let mut app = App::new(
        Pane::new(
            dentro.clone(),
            vec![entry(&dentro, b"leeme.txt", EntryKind::File, Some(3))],
        ),
        Pane::new(dentro, Vec::new()),
    );
    app.dialog_hints = default_dialog_hints();
    open_help(&mut app);
    let view = app.help.as_mut().expect("overlay abierto");
    view.state.open(&norte_help::TopicId::new("copying"));
    view.state.toggle_focus();
    // Y se MUEVE por las filas ejecutables: desde que `Tab` respeta dónde está
    // el lector (deja la vista quieta y trae el cursor a ella), llegar a las
    // filas de comando es lo que hace quien las quiere ver — un movimiento del
    // cursor, no un efecto secundario de cambiar de columna.
    view.state.up();
    refresh_help_en(&mut app, 100, 30);

    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let texto = terminal.backend().to_string();
    let buffer = terminal.backend().buffer().clone();

    let razon = norte_i18n::t_in(norte_i18n::Lang::Es, "reason-read-only");
    assert!(
        texto.contains(&razon),
        "la fila vetada tiene que decir POR QUÉ ({razon}):\n{texto}"
    );

    // Y la OTRA mitad de la función: la fila está ATENUADA. Decir la razón
    // sobre una fila que se sigue pintando como pulsable es media feature, y
    // es la mitad que el ojo lee primero — el volcado de texto no lleva
    // estilos, así que esto va celda a celda.
    //
    // Se comprueban los DOS tramos que `row_line` decide por separado: el
    // chord (`Mark` si se puede pulsar, `Info` si no — una fila apagada no
    // puede vestir de tecla) y el texto. Solo el color de FRENTE: el fondo se
    // lo pone el bloque del overlay y no dice nada de la disponibilidad.
    let filas = row_texts(&buffer);
    let estilos = all_row_styles(&buffer);
    let y = filas
        .iter()
        .position(|f| f.contains(&razon))
        .expect("la fila con la razón cae dentro del frame");
    let fg_de = |x: usize| estilos[y][x].fg.expect("cada celda pintada tiene frente");
    let en = |aguja: &str| -> usize {
        let byte = filas[y].find(aguja).expect("el trozo está en la fila");
        filas[y][..byte].chars().count()
    };
    let atenuado = app.theme.role(norte_theme::Role::Info).fg;
    let normal = app.theme.role(norte_theme::Role::Regular).fg;
    let tecla = app.theme.role(norte_theme::Role::Mark).fg;
    assert!(
        atenuado != normal && atenuado != tecla,
        "el tema tiene que distinguir los tres roles o esto no prueba nada"
    );

    let x_razon = en(&razon);
    for x in x_razon..x_razon + razon.chars().count() {
        assert_eq!(
            Some(fg_de(x)),
            atenuado,
            "la fila dice la razón pero se pinta como si se pudiera pulsar: {:?}",
            filas[y]
        );
    }
    let x_chord = en("F5");
    assert_eq!(
        Some(fg_de(x_chord)),
        atenuado,
        "el chord de una fila vetada no puede seguir vestido de tecla: {:?}",
        filas[y]
    );
    assert_ne!(Some(fg_de(x_chord)), tecla);
}

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
        has_help: false,
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

/// El panel de diferencias (`Shift+F2`, 2026-08-11-directory-comparison.md).
///
/// La suite del modelo vive en `norte-frontend` y no pinta nada; lo que este
/// snapshot congela es la COMPOSICIÓN, que es lo único que se rompe en
/// silencio: que las dos caras quepan, que las dos marcas queden entre ellas,
/// que un nombre no-UTF8 salga enmascarado y BADGEADO en las dos, y que el pie
/// diga el estado, el lado activo y las cinco cuentas.
///
/// Hay una fila de cada categoría a propósito, incluida la pareja que solo
/// difiere en la CONFIANZA (`= !` frente a `= ~`): esa distinción es el motivo
/// de existir de todo el ítem, y un render que la perdiera seguiría siendo
/// verde en todas las demás aserciones.
/// Una fila de comparación de test: los dos lados salen de las dos raíces del
/// snapshot, y `reason` se rellena solo donde el wire lo exige
/// (`CompareRow::reason_is_consistent`).
#[allow(clippy::fn_params_excessive_bools)]
fn fila_compare(
    id: u64,
    nombre: &[u8],
    verdict: norte_proto::methods::CompareVerdict,
    criterion: norte_proto::methods::CompareCriterion,
    confidence: norte_proto::methods::CompareConfidence,
    izquierda: bool,
    derecha: bool,
) -> norte_proto::methods::CompareRow {
    use norte_proto::methods::{CompareReason, CompareRow, CompareVerdict};
    let izq = vp("file:///casa");
    let der = vp("file:///otro");
    CompareRow {
        id,
        left: izquierda.then(|| entry(&izq, nombre, EntryKind::File, Some(1024))),
        right: derecha.then(|| entry(&der, nombre, EntryKind::File, Some(2048))),
        verdict,
        criterion,
        confidence,
        newer: None,
        reason: matches!(verdict, CompareVerdict::Ambiguous | CompareVerdict::Error)
            .then_some(CompareReason::Unreadable),
        side: None,
        paired_under: None,
    }
}

#[test]
fn snapshot_compare_pane() {
    use norte_proto::methods::{CompareConfidence, CompareCriterion, CompareVerdict};

    let izq = vp("file:///casa");
    let der = vp("file:///otro");
    let fila = fila_compare;

    let mut view = norte_tui::app::CompareView::new(izq, der, 0, None, None);
    view.pane.extend(vec![
        // Probado por el hash, y solo sugerido por la fecha: DOS respuestas.
        fila(
            1,
            b"probado.bin",
            CompareVerdict::Same,
            CompareCriterion::Hash,
            CompareConfidence::Certain,
            true,
            true,
        ),
        fila(
            2,
            b"supuesto.bin",
            CompareVerdict::Same,
            CompareCriterion::Mtime,
            CompareConfidence::Probable,
            true,
            true,
        ),
        // Un provider que no puede decirlo (un .zip): respuesta, no fallo.
        fila(
            3,
            b"en-archivo.txt",
            CompareVerdict::Same,
            CompareCriterion::Mtime,
            CompareConfidence::Unknown,
            true,
            true,
        ),
        fila(
            4,
            b"distinto.txt",
            CompareVerdict::Different,
            CompareCriterion::Size,
            CompareConfidence::Certain,
            true,
            true,
        ),
        fila(
            5,
            &[0xE9, b'.', b'd', b'a', b't'],
            CompareVerdict::OnlyLeft,
            CompareCriterion::Presence,
            CompareConfidence::Certain,
            true,
            false,
        ),
        fila(
            6,
            b"solo-derecha",
            CompareVerdict::OnlyRight,
            CompareCriterion::Presence,
            CompareConfidence::Certain,
            false,
            true,
        ),
        fila(
            7,
            b"clase-distinta",
            CompareVerdict::TypeMismatch,
            CompareCriterion::Kind,
            CompareConfidence::Certain,
            true,
            true,
        ),
        // C6, hallazgo 7: un `Error` puede no traer NINGÚN lado.
        fila(
            8,
            b"ilegible",
            CompareVerdict::Error,
            CompareCriterion::Presence,
            CompareConfidence::Unknown,
            false,
            false,
        ),
    ]);
    view.state = norte_tui::app::CompareState::Done;
    view.pane.select(4);

    let mut app = app_base();
    app.compare = Some(view);
    insta::assert_snapshot!(render_80x24(&app));
}

/// Un paso de plan de test.
fn paso_sync(
    id: u64,
    kind: norte_proto::methods::SyncStepKind,
    rel: &str,
    size: Option<u64>,
    reversal: norte_proto::methods::StepReversal,
) -> norte_proto::methods::SyncStep {
    use norte_proto::methods::{
        CompareConfidence, CompareCriterion, RelPath, StepReversal, SyncReason, SyncStep,
    };
    let paso = SyncStep {
        id,
        kind,
        rel: RelPath::parse_wire(rel).expect("rel"),
        dest_rel: None,
        size,
        criterion: CompareCriterion::Size,
        confidence: CompareConfidence::Certain,
        reversal: Some(reversal),
        // `shape_is_consistent`: un paso IRREVERSIBLE debe decir por qué, y
        // aquí el porqué es siempre el mismo — el destino no puede devolverlo.
        reason: (reversal == StepReversal::Irreversible).then_some(SyncReason::NoTrashOnTarget),
    };
    assert!(
        paso.shape_is_consistent(),
        "paso de test mal formado: {paso:?}"
    );
    paso
}

fn cierre_sync(
    counts: norte_proto::methods::SyncCounts,
    dest_trash: norte_proto::methods::DestTrash,
) -> norte_proto::methods::SyncPlanDone {
    norte_proto::methods::SyncPlanDone {
        task_id: norte_proto::TaskId::new(1),
        plan_hash: norte_proto::methods::PlanHash::from_digest(&[7u8; 32]),
        counts,
        blockers: Vec::new(),
        blockers_total: 0,
        executable: true,
        dest_trash,
    }
}

/// El panel de sincronización de un `Update` contra un destino cuya papelera
/// SÍ dice dónde entierra las cosas: el caso normal en Linux, y el único que
/// una máquina de desarrollo puede producir de verdad (`os_root` declara
/// `trash_restorable` siempre).
///
/// Lo que este snapshot congela es la COMPOSICIÓN a 80 columnas: que el título
/// quepa con las dos raíces y la flecha del SENTIDO, que el resumen no se coma
/// la lista, que las tres marcas de cada paso queden separadas y que la línea
/// de teclas no se corte a media palabra — que es exactamente el fallo que la
/// línea del panel de diferencias tenía al añadirle estas teclas.
#[test]
fn snapshot_sync_pane_update_con_papelera() {
    use norte_proto::methods::{DestTrash, StepReversal, SyncCounts, SyncMode, SyncStepKind};

    let mut view = norte_tui::app::SyncView::new(
        norte_proto::TaskId::new(1),
        SyncMode::Update,
        vp("file:///casa"),
        vp("file:///otro"),
        None,
        None,
    );
    let pasos = vec![
        paso_sync(
            1,
            SyncStepKind::CreateDir,
            "sub",
            None,
            StepReversal::Delete,
        ),
        paso_sync(
            2,
            SyncStepKind::Copy,
            "sub/c.txt",
            Some(2048),
            StepReversal::Delete,
        ),
        paso_sync(
            3,
            SyncStepKind::Overwrite,
            "a.txt",
            Some(4096),
            StepReversal::RestoreTrash,
        ),
        paso_sync(
            4,
            SyncStepKind::Copy,
            "nuevo.txt",
            None,
            StepReversal::Delete,
        ),
    ];
    let counts = SyncCounts {
        create_dir: 1,
        copy: 2,
        overwrite: 1,
        delete_tree: 0,
        skip: 0,
        irreversible: 0,
        bytes: 6144,
        unmeasured_steps: 1,
        unknown_kind: 0,
    };
    view.state =
        norte_frontend::sync::SyncState::ready(pasos, cierre_sync(counts, DestTrash::Restorable));
    view.run = norte_tui::app::SyncRunState::Done;

    let mut app = app_base();
    app.sync = Some(view);
    insta::assert_snapshot!(render_80x24(&app));
}

/// Y el caso que esta máquina NO puede producir: un `Mirror` que borra un árbol
/// contra un destino SIN papelera, con la segunda pregunta abierta.
///
/// `norte-vfs-local` declara `TRASH` siempre y, con la raíz en `/`, contesta
/// `trash_restorable` siempre — así que `Opaque` y `Absent` son alcanzables en
/// macOS y en Windows, y aquí solo por construcción. Este snapshot es lo que
/// hace que la frase se lea, y lo que impide que un cambio en el resumen deje
/// «no se puede deshacer nada» fuera de pantalla en el único caso donde no
/// leerlo cuesta datos.
#[test]
fn snapshot_sync_pane_mirror_sin_papelera() {
    use norte_proto::methods::{DestTrash, StepReversal, SyncCounts, SyncMode, SyncStepKind};

    let mut view = norte_tui::app::SyncView::new(
        norte_proto::TaskId::new(1),
        SyncMode::Mirror,
        vp("file:///casa"),
        vp("file:///otro"),
        None,
        None,
    );
    let pasos = vec![
        paso_sync(
            1,
            SyncStepKind::Copy,
            "nuevo.txt",
            Some(64),
            StepReversal::Delete,
        ),
        paso_sync(
            2,
            SyncStepKind::Overwrite,
            "a.txt",
            Some(4096),
            StepReversal::Irreversible,
        ),
        paso_sync(
            3,
            SyncStepKind::DeleteTree,
            "arbol-sobrante",
            None,
            StepReversal::Irreversible,
        ),
        paso_sync(
            4,
            SyncStepKind::DeleteTree,
            "sobra.txt",
            None,
            StepReversal::Irreversible,
        ),
    ];
    let counts = SyncCounts {
        create_dir: 0,
        copy: 1,
        overwrite: 1,
        delete_tree: 2,
        skip: 0,
        irreversible: 3,
        bytes: 4160,
        unmeasured_steps: 0,
        unknown_kind: 0,
    };
    view.state =
        norte_frontend::sync::SyncState::ready(pasos, cierre_sync(counts, DestTrash::Absent));
    view.run = norte_tui::app::SyncRunState::Done;
    view.confirming = view
        .state
        .plan()
        .expect("plan")
        .confirmation(norte_i18n::active());
    assert!(
        view.confirming.is_some(),
        "un mirror que borra dos árboles sin papelera TIENE que preguntar dos veces"
    );

    let mut app = app_base();
    app.sync = Some(view);
    insta::assert_snapshot!(render_80x24(&app));
}
