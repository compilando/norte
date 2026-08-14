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

/// H1 T3 (#24): los hints de los overlays se precomputan del efectivo
/// `dialog` vigente (`main.rs`, `DialogHints::build`). Este test construye
/// `App` directamente (sin pasar por `main`), así que replica el MISMO
/// cómputo con el preset `orthodox` real.
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

#[test]
fn frame_pinta_panes_y_badge_no_utf8() {
    let dir = vp("file:///casa");
    let mut entries = vec![
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(b"docs".to_vec()).unwrap()),
            kind: EntryKind::Dir,
            size: None,
            mtime_ms: None,
        },
        Entry {
            attrs: std::collections::BTreeMap::new(),
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

/// #117-follow-up: una columna `plugin:` configurada en `[ui.columns]`
/// pinta cabecera (etiqueta `plugin/columna` saneada) y celda (valor del
/// side-map del pane, llegado por el fetch asíncrono); una entrada sin
/// valor queda en blanco — jamás fabricado.
#[test]
fn columna_plugin_configurada_pinta_cabecera_y_celda() {
    let dir = vp("file:///x");
    let mut entries = vec![
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(b"a.txt".to_vec()).unwrap()),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
        },
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(b"b.txt".to_vec()).unwrap()),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
        },
    ];
    sort_entries(&mut entries);
    let mut app = App::new(
        Pane::new(dir.clone(), entries),
        Pane::new(dir.clone(), Vec::new()),
    );
    app.columns = norte_frontend::columns::ColumnsSettings::resolve(&norte_config::ColumnsConfig {
        default_columns: Some(vec!["name".into(), "plugin:git/branch".into()]),
        ..Default::default()
    });
    let mut per_path = std::collections::HashMap::new();
    per_path.insert(
        dir.join(Segment::new(b"a.txt".to_vec()).unwrap()),
        "main".to_owned(),
    );
    // Audit F3: un valor con RLO CRUDO metido directamente en el side-map
    // (simulando un ingest que dejó de sanear) — el re-mask defensivo de
    // `plugin_cell` es la última línea y debe verse en el FRAME (en
    // ratatui un bidi crudo desaparece en silencio, no "se ve raro").
    per_path.insert(
        dir.join(Segment::new(b"b.txt".to_vec()).unwrap()),
        "x\u{202E}y".to_owned(),
    );
    let mut cols = std::collections::HashMap::new();
    cols.insert("plugin:git/branch".to_owned(), per_path);
    app.panes[0].set_plugin_columns(cols);

    let mut terminal = Terminal::new(TestBackend::new(80, 10)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let contenido = terminal.backend().to_string();
    assert!(
        contenido.contains("git/branch"),
        "cabecera de la columna plugin visible: {contenido}"
    );
    assert!(
        contenido.contains("main"),
        "celda del side-map visible: {contenido}"
    );
    assert!(
        !contenido.contains('\u{202E}') && contenido.contains('\u{FFFD}'),
        "el RLO del valor hostil llega ENMASCARADO al frame: {contenido}"
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

/// #93: un contenedor con entradas omitidas de su índice lo señaliza en la
/// status bar («N entradas omitidas») — un listado incompleto jamás es
/// silencioso. `Some(0)`/`None` no pintan nada.
#[test]
fn omitidas_del_contenedor_se_ven_en_la_status_bar() {
    let dir = vp("file:///x");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir.clone(), Vec::new()),
    );
    app.panes[0].begin_listing(dir.clone(), Vec::new(), false, Some(3));
    let mut terminal = Terminal::new(TestBackend::new(80, 8)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let con_badge = terminal.backend().to_string();
    assert!(
        con_badge.contains('3') && con_badge.contains("omit"),
        "badge de omitidas visible: {con_badge}"
    );

    // Some(0) = contenedor indexado SIN omisiones: nada que señalizar.
    app.panes[0].begin_listing(dir, Vec::new(), false, Some(0));
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    assert!(
        !terminal.backend().to_string().contains("omit"),
        "sin omitidas no hay badge"
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
        attrs: std::collections::BTreeMap::new(),
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
    app.dialog_hints = default_dialog_hints();
    app.message = Some("copy: destination exists".to_owned());
    app.modal = Some(Modal::Collision {
        retry: norte_tui::tasks::RetrySpec {
            kind: TransferKind::Copy,
            from: dir.join(Segment::new(b"a".to_vec()).unwrap()),
            to: dir.join(Segment::new(b"b".to_vec()).unwrap()),
            opts: norte_core::TransferOptions::default(),
            name_encoding: None,
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
        false,
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

/// #101: un preview cuya decodificación host-side fue LOSSY pinta el aviso
/// `[lossy decode]` junto al indicador «via …» (honestidad igual que el
/// status de encoding del viewer crudo).
#[test]
fn viewer_preview_lossy_pinta_el_aviso() {
    let dir = vp("file:///x");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );
    app.viewer = Some(norte_tui::viewer::Viewer::with_plugin_preview(
        vp("file:///doc.md"),
        "Markdown".to_owned(),
        "cuerpo",
        true,
    ));
    let mut terminal = Terminal::new(TestBackend::new(60, 10)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let contenido = terminal.backend().to_string();
    assert!(
        contenido.contains("via Markdown") && contenido.contains("lossy"),
        "el aviso lossy acompaña al «via …»: {contenido}"
    );
}

/// G3a (ADR 0037): un preview de plugin CON ESTILO (`SpanWire`, no ANSI-SGR)
/// pinta con el COLOR DEL TEMA cuando el span trae `role`, y con el `fg`
/// crudo cuando no — `role` GANA sobre `fg` si un span trae ambos (decisión
/// 3 del ADR: el tema del usuario tiene precedencia sobre el color fijo de
/// un plugin). Inspecciona el BUFFER de ratatui (como `theme_render.rs`),
/// no solo el texto: la snapshot de texto no distingue "pintado con role"
/// de "pintado con fg crudo".
#[test]
fn viewer_preview_styled_role_gana_a_fg_y_pinta_del_tema() {
    use norte_proto::methods::SpanWire;
    use norte_theme::{ColorDepth, Theme};
    use norte_tui::theme::TuiTheme;
    use ratatui::style::Color;

    let dir = vp("file:///x");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );
    // Truecolor EXPLÍCITO (no `detect_depth()`, que depende del entorno del
    // proceso de test — mismo criterio que `theme_render.rs`). El preset
    // `default` fija `title = #5fafd7` (RGB 95,175,215): fuente única del
    // color esperado, sin repetirlo a mano en dos sitios.
    app.theme = TuiTheme::new(Theme::preset_default(), ColorDepth::Truecolor);

    let lines = vec![vec![
        // Rol Y fg a la vez: el rol (Title, #5fafd7) debe ganar — el fg
        // crudo (255,0,0) NUNCA debe llegar a pintarse.
        SpanWire {
            text: "AAA".to_owned(),
            role: Some("title".to_owned()),
            fg: Some([255, 0, 0]),
        },
        // Solo fg: pinta el crudo tal cual, sin tema de por medio.
        SpanWire {
            text: "BBB".to_owned(),
            role: None,
            fg: Some([0, 255, 0]),
        },
    ]];
    app.viewer = Some(norte_tui::viewer::Viewer::with_plugin_preview_styled(
        vp("file:///doc.rs"),
        "Highlighter".to_owned(),
        &lines,
        false,
    ));

    let mut terminal = Terminal::new(TestBackend::new(60, 10)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let cells: Vec<_> = terminal.backend().buffer().content.iter().collect();

    assert!(
        cells.iter().any(|c| c.fg == Color::Rgb(0x5f, 0xaf, 0xd7)),
        "el span con role=title pinta el AZUL del tema (title.fg)"
    );
    assert!(
        !cells.iter().any(|c| c.fg == Color::Rgb(255, 0, 0)),
        "el fg crudo (255,0,0) del span con role NUNCA se pinta: role gana"
    );
    assert!(
        cells.iter().any(|c| c.fg == Color::Rgb(0, 255, 0)),
        "el span SIN role pinta su fg crudo tal cual"
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
            paths_total: 0,
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

/// Review H3c MINOR-5: cuántas rutas trae la petición lo elige el AGENTE, y el
/// pie del modal tiene que PINTARSE de todas formas.
///
/// El alto crecía con `paths.len()` sin tope y `centered` recorta contra el
/// frame, así que las líneas de sobra no llegaban al buffer — incluida la
/// ÚLTIMA, que bajo H3c es la única explicación de por qué las teclas del modal
/// no responden. Se comprueba sobre el FRAME pintado (no sobre el texto): el
/// defecto era del recorte, no del cuerpo.
#[test]
fn el_pie_del_modal_de_aprobacion_se_pinta_con_un_lote_gigante() {
    let dir = vp("file:///x");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );
    // Con una ayuda TAPÁNDOLO: el pie inerte es el aviso que no puede perderse.
    app.help = Some(norte_tui::app::HelpView::new(
        norte_i18n::Lang::En,
        Vec::new(),
    ));
    app.help.as_mut().expect("abierta").over_modal = true;
    app.dialog_hints = app.dialog_hints.with_modals_inert();
    app.modal = Some(norte_tui::app::Modal::ApproveAgentOp {
        req: norte_proto::methods::PolicyApprovalRequired {
            approval_id: 1,
            session: Some("s1".into()),
            op: "copy".into(),
            paths: (1..=400).map(|i| format!("mem:///proj/f{i}.txt")).collect(),
            paths_total: 0,
            ttl_ms: 60_000,
        },
    });

    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let contenido = terminal.backend().to_string();

    assert!(
        contenido.contains(&norte_i18n::t("modal-hint-help-open")),
        "el aviso de teclas inertes se pinta con 400 rutas: {contenido}"
    );
    assert!(
        contenido.contains(&norte_i18n::t("modal-approval-title")),
        "y la pregunta sigue a la vista: {contenido}"
    );
    // La lista está ACOTADA y resumida: la cola no se pinta ni empuja nada.
    assert!(
        !contenido.contains("f400"),
        "la cola no se pinta: {contenido}"
    );
    assert!(
        contenido.contains("390"),
        "el resumen dice cuántas quedan fuera: {contenido}"
    );
}

/// M4-IA (doctrina encoding-auditor): el plan de rename IA pinta contenido
/// del MODELO — controles/bidi → `�` con badge; un `from` kilométrico no
/// expulsa el `to` de la caja (elipsis media); `→` fuera de banda al inicio
/// de la línea del destino (jamás joiner in-band).
#[test]
fn modal_de_plan_ai_enmascara_y_no_oculta_el_destino() {
    let dir = vp("file:///x");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir.clone(), Vec::new()),
    );
    let from_largo = format!("{}\u{202e}oculto.txt", "x".repeat(120));
    app.modal = Some(norte_tui::app::Modal::AiRenamePlan {
        dir,
        entries: vec![norte_proto::methods::AiRenameEntry {
            from: from_largo,
            to: "destino-final.txt".into(),
        }],
        offset: 0,
        plan: norte_frontend::BatchPlan::Ready(Box::new(
            norte_proto::methods::FsRenameBatchPlanResult {
                steps: Vec::new(),
                collisions: Vec::new(),
                executable: true,
                plan_hash: norte_proto::methods::PlanHash::parse(&"0".repeat(64)).expect("64 hex"),
            },
        )),
    });
    let mut terminal = Terminal::new(TestBackend::new(60, 14)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let contenido = terminal.backend().to_string();

    assert!(
        contenido.contains("destino-final"),
        "el destino jamás se expulsa de la caja: {contenido}"
    );
    assert!(contenido.contains('\u{FFFD}'), "bidi → �: {contenido}");
    assert!(
        contenido.contains('!'),
        "el enmascarado se MARCA (spec §6): {contenido}"
    );
    assert!(
        contenido.contains('→'),
        "flecha fuera de banda en la línea del destino: {contenido}"
    );
}

/// §17: un plan grande con veredictos hace el modal MÁS ALTO que el
/// terminal, y `centered` lo recorta por ABAJO. La línea que dice que el
/// lote NO se puede aplicar va arriba, pegada al dir, precisamente por eso:
/// un recorte puede comerse la cola de las colisiones, jamás el veredicto.
///
/// (Mutación de control: mover el estado del lote al final del cuerpo — que
/// es donde estaba— hace que este test no lo encuentre.)
#[test]
fn el_veredicto_del_lote_sobrevive_a_un_terminal_corto() {
    let _ = norte_i18n::force(norte_i18n::Lang::En);
    let dir = vp("file:///x");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir.clone(), Vec::new()),
    );
    let entries: Vec<_> = (1..=8)
        .map(|i| norte_proto::methods::AiRenameEntry {
            from: format!("f{i}.txt"),
            to: format!("t{i}.txt"),
        })
        .collect();
    let collisions: Vec<_> = (0..8)
        .map(|i| norte_proto::methods::RenameCollision {
            pair_index: i,
            name: norte_proto::Segment::new(format!("t{}.txt", i + 1).into_bytes())
                .expect("segmento"),
            kind: norte_proto::methods::RenameCollisionKind::External,
        })
        .collect();
    app.modal = Some(norte_tui::app::Modal::AiRenamePlan {
        dir,
        entries,
        offset: 0,
        plan: norte_frontend::BatchPlan::Ready(Box::new(
            norte_proto::methods::FsRenameBatchPlanResult {
                steps: Vec::new(),
                collisions,
                executable: false,
                plan_hash: norte_proto::methods::PlanHash::parse(&"0".repeat(64)).expect("64 hex"),
            },
        )),
    });
    // 14 filas: el modal pide 21 y no cabe.
    let mut terminal = Terminal::new(TestBackend::new(80, 14)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let contenido = terminal.backend().to_string();

    assert!(
        contenido.contains(&norte_i18n::t("modal-rename-batch-not-applicable")),
        "el veredicto sobrevive al recorte: {contenido}"
    );
    assert!(
        !contenido.contains(&norte_i18n::t("modal-ai-rename-plan-hint")),
        "y el pie jamás ofrece una tecla muda: {contenido}"
    );
}

/// Encoding audit H1: un chord hostil (`norte_testkit::corpus::
/// hostile_chords`) ligado a `dialog.approve` desde una capa (el modelo
/// de `./.norte/keymap.toml`, capa de PROYECTO sin trust) no debe
/// sobrevivir crudo al pie del modal `ApproveAgentOp` — uno de los tres
/// modales de SEGURIDAD (junto a `TrustHostKey`/`ConfirmDelete`-permanente)
/// cuyo footer un RLO podría reordenar visualmente (cancel/confirm
/// intercambiados aparentes). `DialogHints::build` se construye del
/// efectivo hostil, exactamente como `main.rs` lo hace en el arranque/
/// hot-reload real.
#[test]
fn footer_de_aprobacion_enmascara_chord_hostil_de_una_capa() {
    use norte_tui::keymap::{COMMANDS, DIALOG_COMMANDS, Effective, Screen, parse_keymap, presets};

    let dir = vp("file:///x");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );

    let (_, preset) = presets()
        .into_iter()
        .find(|(n, _)| *n == "orthodox")
        .expect("preset orthodox");
    let known: Vec<&str> = COMMANDS
        .iter()
        .copied()
        .chain(DIALOG_COMMANDS.iter().copied())
        .collect();

    for hazard in norte_testkit::corpus::hostile_chords() {
        let token_esc = format!("\\u{:04X}", hazard.token as u32);
        let layer_src = format!(
            r#"
            [dialog]
            prepend_keymap = [{{ on = ["{token_esc}"], run = "dialog.approve" }}]
            "#,
        );
        let layer = parse_keymap(&layer_src).unwrap();
        let eff = Effective::build_for(&preset, &[layer], &known, Screen::Dialog)
            .unwrap_or_else(|e| panic!("[{}] efectivo dialog: {e}", hazard.id));
        app.dialog_hints = norte_tui::hints::DialogHints::build(&eff);
        assert!(
            app.dialog_hints.approval.contains('\u{FFFD}'),
            "[{}] precondición: el hint hostil debe enmascararse ANTES de \
             pintar (helper de norte_encoding, no un accidente del render)",
            hazard.id
        );

        app.modal = Some(norte_tui::app::Modal::ApproveAgentOp {
            req: norte_proto::methods::PolicyApprovalRequired {
                approval_id: 1,
                session: Some("s1".into()),
                op: "copy".into(),
                paths: vec!["mem:///proj/src.txt".into(), "mem:///proj/dst.txt".into()],
                paths_total: 0,
                ttl_ms: 30_000,
            },
        });

        let mut terminal = Terminal::new(TestBackend::new(60, 14)).expect("terminal");
        terminal.draw(|f| ui::draw(f, &app)).expect("draw");
        let contenido = terminal.backend().to_string();

        assert!(
            contenido.contains('\u{FFFD}'),
            "[{}] el pie del modal de aprobación debe llevar U+FFFD: {contenido}",
            hazard.id
        );
        // El check es del TOKEN concreto, no un blanket `is_terminal_hazard`
        // sobre `contenido`: la stringificación de `TestBackend` UNE filas
        // con `\n` (un hazard legítimo del formato de grilla, no del dato
        // pintado) — comparar contra el hazard exacto evita ese falso
        // positivo.
        assert!(
            !contenido.contains(hazard.token),
            "[{}] el chord crudo no debe sobrevivir en el frame pintado: {contenido}",
            hazard.id
        );
    }
}

/// #57: con `pane.names-encoding` activo, un nombre cirílico en cp866 se
/// PINTA legible (Папка), conserva su badge hostil (el texto difiere de los
/// bytes) y la barra indica el modo de forma persistente. El ciclo:
/// None → sugerido (IBM866 con estas muestras) → … → None.
#[test]
fn reinterpretar_nombres_pinta_legible_con_badge_e_indicador() {
    let dir = vp("file:///x");
    // "Папка" en cp866: no-UTF8 → lossy sin reinterpretar.
    let entries = vec![Entry {
        attrs: std::collections::BTreeMap::new(),
        path: dir.join(Segment::new(b"\x8f\xa0\xaf\xaa\xa0".to_vec()).unwrap()),
        kind: EntryKind::File,
        size: Some(1),
        mtime_ms: None,
    }];
    let mut app = App::new(Pane::new(dir.clone(), entries), Pane::new(dir, Vec::new()));

    // Primer ciclo: la sugerencia de chardetng sobre el listado (IBM866).
    let label = app.panes[0].cycle_name_encoding();
    assert_eq!(label, Some("IBM866"), "sugerido por las muestras cp866");

    let mut terminal = Terminal::new(TestBackend::new(80, 8)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let contenido = terminal.backend().to_string();
    assert!(
        contenido.contains("Папка"),
        "nombre reinterpretado legible: {contenido}"
    );
    assert!(
        contenido.contains("! "),
        "badge hostil conservado (el texto no son los bytes): {contenido}"
    );
    assert!(
        contenido.contains("IBM866"),
        "indicador persistente en la barra: {contenido}"
    );

    // M1 del review: el ciclo da la VUELTA COMPLETA — desde la sugerencia
    // (IBM866) se visitan TODOS los demás encodings, cp437 incluido, y se
    // apaga exactamente al regresar al punto de entrada.
    let mut visitados = vec!["IBM866"];
    while let Some(label) = app.panes[0].cycle_name_encoding() {
        visitados.push(label);
        assert!(visitados.len() <= 5, "el ciclo debe cerrarse en None");
    }
    assert_eq!(
        visitados,
        ["IBM866", "Shift_JIS", "GBK", "windows-1252", "cp437"],
        "vuelta completa con wrap: cp437 alcanzable desde cualquier entrada"
    );
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let apagado = terminal.backend().to_string();
    assert!(
        apagado.contains('\u{FFFD}'),
        "apagado = lossy de siempre: {apagado}"
    );
}

/// #98/F2: las superficies de DECISIÓN siguen la reinterpretación del pane —
/// el modal de confirmar borrado sobre la entrada cp866 pinta «Папка» (lo
/// mismo por lo que el usuario navegó), no «�����», con el badge conservado.
#[test]
fn modal_de_confirmacion_sigue_la_reinterpretacion() {
    let dir = vp("file:///x");
    let papka = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "cp866_papka")
        .expect("fixture del corpus")
        .bytes;
    let target = dir.join(Segment::new(papka).unwrap());
    let entries = vec![Entry {
        attrs: std::collections::BTreeMap::new(),
        path: target.clone(),
        kind: EntryKind::File,
        size: Some(1),
        mtime_ms: None,
    }];
    let mut app = App::new(Pane::new(dir.clone(), entries), Pane::new(dir, Vec::new()));
    assert_eq!(app.panes[0].cycle_name_encoding(), Some("IBM866"));
    app.modal = Some(norte_tui::app::Modal::ConfirmDelete {
        items: vec![target],
        permanent: false,
    });
    let mut terminal = Terminal::new(TestBackend::new(80, 12)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let contenido = terminal.backend().to_string();
    assert!(
        contenido.contains("Папка"),
        "el modal pinta el texto por el que se navegó: {contenido}"
    );
    assert!(
        !contenido.contains("�����"),
        "no el lossy crudo: {contenido}"
    );
}

/// #103 T10: el modal de un LOTE pinta una ruta POR LÍNEA (jamás dos
/// pegadas por un joiner in-band que un nombre pudiera imitar), se corta en
/// `MODAL_ITEM_LIMIT` y RESUME cuántas quedan fuera — un lote de 14 no puede
/// parecer uno de 10. La flecha del destino va en SU propia línea.
#[test]
fn el_modal_de_un_lote_pinta_una_ruta_por_linea_y_resume_el_resto() {
    let dir = vp("file:///casa");
    let items: Vec<VPath> = (0..14)
        .map(|i| dir.join(Segment::new(format!("f{i:02}").into_bytes()).unwrap()))
        .collect();
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(vp("file:///otro"), Vec::new()),
    );
    app.modal = Some(norte_tui::app::Modal::ConfirmTransfer {
        space: None,
        kind: norte_tui::app::TransferKind::Copy,
        items,
        to: vp("file:///otro"),
    });
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let pintado = terminal.backend().to_string();
    // Los 10 primeros, cada uno en su línea; el 11.º YA no se lista.
    let nombres: Vec<String> = (0..norte_frontend::MODAL_ITEM_LIMIT)
        .map(|i| format!("f{i:02}"))
        .collect();
    for nombre in &nombres {
        let lineas = pintado.lines().filter(|l| l.contains(nombre)).count();
        assert_eq!(
            lineas, 1,
            "{nombre} va en UNA línea, no {lineas}: {pintado}"
        );
    }
    for linea in pintado.lines() {
        let cuantos = nombres.iter().filter(|n| linea.contains(*n)).count();
        assert!(cuantos <= 1, "dos ítems en la misma línea: {linea:?}");
    }
    assert!(!pintado.contains("f10"), "el 11.º no se lista: {pintado}");
    // …y el resumen dice cuántos quedan fuera (14 - 10 = 4).
    assert!(
        pintado.lines().any(|l| l.contains('…') && l.contains('4')),
        "falta el resumen de los que no caben: {pintado}"
    );
    // El destino, en su propia línea y con la flecha fuera de banda.
    assert!(
        pintado
            .lines()
            .any(|l| l.contains('→') && l.contains("/otro")),
        "el destino va en su línea: {pintado}"
    );
}

/// #103 T9 (extra work item 2): el patrón de `Modal::MarkPattern` es texto
/// NO confiable — llega por paste tan fácil como tecleado — así que debe
/// enmascararse con el MISMO `display_name` que usa el input del quick
/// search antes de llegar al buffer, y lo mismo su diagnóstico: el mensaje
/// de `PatternError::Glob` EMBEBE el patrón verbatim (rustdoc de
/// `PatternError`). Camino REAL de punta a punta (no un modal a mano):
/// teclea el override RTL del corpus canónico carácter a carácter y cierra
/// con un `[` sin parear para forzar un glob inválido — el error que vuelve
/// de `mark_glob` contiene el RTL crudo, y el render no debe dejarlo pasar.
#[test]
fn mark_pattern_modal_enmascara_el_patron_hostil_y_su_error() {
    let dir = vp("file:///x");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );
    let rtl = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "rtl_override")
        .expect("fixture del corpus")
        .bytes;
    let pattern = String::from_utf8(rtl).expect("fixture rtl_override es UTF-8 válida");

    app.open_mark_pattern(true);
    for c in pattern.chars() {
        app.mark_pattern_push(c);
    }
    app.mark_pattern_push('['); // glob mal formado: fuerza PatternError
    assert!(
        app.mark_pattern_confirm().is_err(),
        "el corchete sin parear no debe compilar como glob"
    );
    assert!(app.modal.is_some(), "el modal queda abierto con el error");

    let mut terminal = Terminal::new(TestBackend::new(80, 12)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let contenido = terminal.backend().to_string();
    // Review MAJOR M4: `!contains('\u{202E}')` por sí sola NUNCA puede
    // fallar aquí — U+202E es de ancho cero y el renderer de párrafo de
    // ratatui se COME los grafemas de ancho cero, enmascarados o no. Se
    // conserva como comprobación barata (documenta la intención), pero la
    // aserción que de verdad pinea el enmascarado es el CONTEO de U+FFFD:
    // uno por línea enmascarada. `contains('\u{FFFD}')` a secas lo
    // satisfacía con SOLO el patrón enmascarado — borrar el `display_name`
    // de la línea de error dejaba el test en verde. El unit test puro de
    // `mark_pattern_modal_text` (ui.rs) cubre el enmascarado en sí; este
    // E2E cubre que la ruta completa (push → confirm → draw) lo conserva.
    assert!(
        !contenido.contains('\u{202E}'),
        "el override RTL crudo no debe llegar al buffer (patrón NI error): {contenido}"
    );
    assert!(
        contenido.matches('\u{FFFD}').count() >= 2,
        "patrón Y error deben enmascararse — no solo uno: {contenido}"
    );
}

/// #81: el contexto del match de contenido (línea + preview) del hit BAJO EL
/// CURSOR se pinta en la barra del pane virtual — saneado (un preview hostil
/// jamás pinta controles crudos).
#[test]
fn preview_del_match_bajo_el_cursor_en_la_barra() {
    let dir = vp("file:///casa");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir.clone(), Vec::new()),
    );
    let hit = Entry {
        attrs: std::collections::BTreeMap::new(),
        path: dir.join(Segment::new(b"main.rs".to_vec()).unwrap()),
        kind: EntryKind::File,
        size: Some(120),
        mtime_ms: None,
    };
    let pane = app.focused_mut();
    pane.begin_search(dir);
    pane.extend_listing(vec![hit.clone()]);
    pane.search_matches.insert(
        hit.path,
        norte_proto::methods::MatchInfo {
            line: Some(42),
            preview: Some("fn main() { hola }\u{1b}[31m\u{202e}".into()),
        },
    );
    let mut terminal = Terminal::new(TestBackend::new(80, 8)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let contenido = terminal.backend().to_string();
    assert!(
        contenido.contains(":42") && contenido.contains("hola"),
        "línea y preview del hit en la barra: {contenido}"
    );
    // El cinturón (detail_for_bar) enmascara: ESC/bidi jamás crudos aunque
    // un core buggy los colara en el preview.
    assert!(
        !contenido.contains('\u{1b}') && !contenido.contains('\u{202e}'),
        "controles/bidi enmascarados en la barra: {contenido:?}"
    );
}

/// M4-IA-2 (doctrina encoding-auditor): el modal de hits semánticos pinta
/// paths del ÍNDICE — controles/bidi → `�` con badge; un path kilométrico no
/// expulsa el score de la caja (elipsis media); el marcador `>` del cursor va
/// fuera de banda al inicio de su línea.
#[test]
fn modal_semantic_enmascara_hits_hostiles() {
    let dir = vp("file:///x");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir.clone(), Vec::new()),
    );
    let hostil_largo = format!("{}\u{202e}oculto.txt", "x".repeat(120));
    app.modal = Some(norte_tui::app::Modal::SemanticHits {
        hits: vec![
            norte_proto::methods::SemanticHit {
                path: dir
                    .join(Segment::new(hostil_largo.into_bytes()).expect("segmento del fixture")),
                score: 0.91,
            },
            norte_proto::methods::SemanticHit {
                path: vp("file:///x/limpio.txt"),
                score: 0.45,
            },
        ],
        offset: 0,
        cursor: 0,
    });
    let mut terminal = Terminal::new(TestBackend::new(70, 12)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let contenido = terminal.backend().to_string();

    assert!(
        contenido.contains("0.91"),
        "el score jamás se expulsa de la caja: {contenido}"
    );
    assert!(contenido.contains('\u{FFFD}'), "bidi → �: {contenido}");
    assert!(
        contenido.contains('!'),
        "el enmascarado se MARCA (spec §6): {contenido}"
    );
    assert!(
        contenido.contains('>'),
        "marcador de cursor fuera de banda: {contenido}"
    );
    assert!(
        contenido.contains("limpio.txt"),
        "el hit limpio se pinta entero: {contenido}"
    );
}

/// La ayuda (F1) se PINTA sobre el viewer. `f1 → app.help` vive en
/// `[global]` del preset, así que sigue vigente en `Screen::Viewer`: el
/// run loop enruta la tecla, `app.help` pasa a `Some`… y el draw hacía
/// `return` justo después del viewer, dejando el overlay INVISIBLE. Como
/// el brazo de `app.help` del run loop va ANTES del viewer, ese overlay
/// fantasma se comía TODAS las teclas siguientes: F1 "dejaba de
/// funcionar" y el viewer parecía muerto.
#[test]
fn la_ayuda_se_pinta_sobre_el_viewer() {
    let _ = norte_i18n::force(norte_i18n::Lang::En);
    let dir = vp("file:///x");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );
    app.viewer = Some(norte_tui::viewer::Viewer::new(
        vp("file:///x/notas.txt"),
        b"cuerpo del fichero\n".to_vec(),
        false,
    ));
    // H3b: la página de teclado sintética — el `keys_lines` de siempre, ahora
    // como cuerpo de una entrada más de la lateral. Se abre navegando a ella
    // (el overlay arranca en el índice) y se MAQUETA antes de pintar, como
    // hace el run loop.
    let mut help = norte_tui::app::HelpView::new(
        norte_i18n::Lang::En,
        vec![ratatui::text::Line::raw("  f1             this help")],
    );
    help.state
        .open(&norte_help::TopicId::new(norte_frontend::help::KEYS_ID));
    app.help = Some(help);
    let mut terminal = Terminal::new(TestBackend::new(70, 12)).expect("terminal");
    app.refresh_help(40, 8);
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let contenido = terminal.backend().to_string();
    assert!(
        contenido.contains("this help"),
        "la ayuda es visible sobre el viewer: {contenido}"
    );
}

/// Mismo fallo, consecuencia de SEGURIDAD: un modal asíncrono (aprobación
/// de policy, colisión, confirmación) llegado con el viewer abierto se
/// enruta ANTES que el viewer (`app.modal` gana la tecla) pero quedaba sin
/// pintar — el usuario respondía a ciegas a un diálogo que no veía.
#[test]
fn un_modal_se_pinta_sobre_el_viewer() {
    let _ = norte_i18n::force(norte_i18n::Lang::En);
    let dir = vp("file:///x");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );
    app.dialog_hints = default_dialog_hints();
    app.viewer = Some(norte_tui::viewer::Viewer::new(
        vp("file:///x/notas.txt"),
        b"cuerpo del fichero\n".to_vec(),
        false,
    ));
    app.modal = Some(norte_tui::app::Modal::ConfirmDelete {
        items: vec![vp("file:///x/borrame.txt")],
        permanent: true,
    });
    let mut terminal = Terminal::new(TestBackend::new(70, 12)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let contenido = terminal.backend().to_string();
    assert!(
        contenido.contains("borrame.txt"),
        "el modal es visible sobre el viewer: {contenido}"
    );
}

/// El modal de nombre en destino (#105) elide las rutas por el MEDIO, como
/// el de aprobación y el de colisión: una ruta kilométrica se recortaba a
/// pelo contra el borde de la caja (`modal_width` topa contra el frame y el
/// `Paragraph` no envuelve), así que la COLA del destino —el dir al que se
/// copia de verdad— quedaba expulsada sin ni siquiera un `…` que lo
/// delatara.
#[test]
fn el_modal_de_nombre_en_destino_elide_las_rutas() {
    let _ = norte_i18n::force(norte_i18n::Lang::En);
    let hondo = "/tmp/claude-1000/-home-oscar-work-wot-projects-high-norte/fd0480d7-a010-40be";
    let dir = vp(&format!("file://{hondo}/origen"));
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(vp(&format!("file://{hondo}/destino")), Vec::new()),
    );
    app.dialog_hints = default_dialog_hints();
    app.modal = Some(norte_tui::app::Modal::TransferName {
        kind: norte_tui::app::TransferKind::Copy,
        from: dir.join(Segment::new(b"grande.log".to_vec()).unwrap()),
        to_dir: vp(&format!("file://{hondo}/destino")),
        name: "grande.log".to_owned(),
        original: b"grande.log".to_vec(),
        touched: false,
        from_marks: false,
        enc: None,
        error: None,
    });
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let contenido = terminal.backend().to_string();
    assert!(
        contenido.contains('…'),
        "la ruta larga se elide, no se corta a pelo: {contenido}"
    );
    assert!(
        contenido.contains("destino"),
        "la COLA del destino sobrevive al recorte: {contenido}"
    );
    assert!(
        contenido.contains("grande.log"),
        "el nombre editable sigue visible: {contenido}"
    );
}

/// #124: `ui::pane_list_rows` cuenta EXACTAMENTE las filas de listado que el
/// frame pinta — es el número que el run loop devuelve al modelo para que la
/// paginación y la sonda de stat dejen de adivinar el viewport. Si el layout
/// del pane cambia (un borde, una línea de cabecera, el panel de tasks),
/// este test cae y obliga a corregir la aritmética en vez de dejarla
/// mintiendo.
#[test]
fn pane_list_rows_cuenta_las_filas_que_de_verdad_se_pintan() {
    let _ = norte_i18n::force(norte_i18n::Lang::En);
    let dir = vp("file:///x");
    // Muchas más entradas que filas: el pane se llena entero.
    let entries: Vec<Entry> = (0..60)
        .map(|i| Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir
                .join(Segment::new(format!("f{i:03}.txt").into_bytes()).unwrap())
                .clone(),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        })
        .collect();
    let mut app = App::new(Pane::new(dir.clone(), entries), Pane::new(dir, Vec::new()));
    app.render_now_ms = Some(0);
    for alto in [10u16, 16, 24] {
        let mut terminal = Terminal::new(TestBackend::new(60, alto)).expect("terminal");
        terminal.draw(|f| ui::draw(f, &app)).expect("draw");
        let pintado = terminal.backend().to_string();
        let filas = pintado.lines().filter(|l| l.contains(".txt")).count();
        assert_eq!(
            usize::from(ui::pane_list_rows(&app, alto)),
            filas,
            "alto {alto}: la cuenta debe ser la del buffer real:\n{pintado}"
        );
    }
    // Con el visor abierto no se pinta ningún pane: cero filas visibles.
    app.viewer = Some(norte_tui::viewer::Viewer::new(
        vp("file:///x/f000.txt"),
        b"x".to_vec(),
        false,
    ));
    assert_eq!(ui::pane_list_rows(&app, 24), 0);
}
