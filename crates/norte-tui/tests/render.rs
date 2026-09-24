//! Render smoke test: the frame paints both panes, highlights focus and
//! marks non-UTF8 names. Real snapshot tests (insta) = phase 10.

use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, Pane, sort_entries};
use norte_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

/// H1 T3 (#24): overlay hints are precomputed from the effective `dialog`
/// in force (`main.rs`, `DialogHints::build`). This test builds `App`
/// directly (without going through `main`), so it replicates the SAME
/// computation with the real `orthodox` preset.
fn default_dialog_hints() -> norte_tui::hints::DialogHints {
    use norte_tui::keymap::{COMMANDS, DIALOG_COMMANDS, Effective, Screen, presets};
    let (_, preset) = presets()
        .into_iter()
        .find(|(n, _)| *n == "orthodox")
        .expect("orthodox preset");
    let known: Vec<&str> = COMMANDS
        .iter()
        .copied()
        .chain(DIALOG_COMMANDS.iter().copied())
        .collect();
    let eff = Effective::build_for(&preset, &[], &known, Screen::Dialog).expect("effective dialog");
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
            // é in latin-1: non-UTF8 → lossy + badge.
            path: dir.join(Segment::new(vec![0xE9]).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        },
    ];
    sort_entries(&mut entries);
    let app = App::new(Pane::new(dir.clone(), entries), Pane::new(dir, Vec::new()));

    let mut terminal = Terminal::new(TestBackend::new(60, 10)).expect("test terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");

    let content = terminal.backend().to_string();
    assert!(content.contains("/docs"), "dir with marker: {content}");
    assert!(
        content.contains('\u{FFFD}') && content.contains("! "),
        "non-UTF8 lossy AND with a prefix badge: {content}"
    );
    assert!(
        content.contains("1/2"),
        "cursor position in status: {content}"
    );
}

/// #117-follow-up: a `plugin:` column configured in `[ui.columns]` paints a
/// header (sanitized `plugin/column` label) and a cell (value from the
/// pane's side-map, arrived through the async fetch); an entry with no
/// value stays blank — never fabricated.
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
    // Audit F3: a value with a RAW RLO stuck directly into the side-map
    // (simulating an ingest that stopped sanitizing) — `plugin_cell`'s
    // defensive re-mask is the last line and must show in the FRAME (in
    // ratatui a raw bidi character disappears silently, it does not "look
    // weird").
    per_path.insert(
        dir.join(Segment::new(b"b.txt".to_vec()).unwrap()),
        "x\u{202E}y".to_owned(),
    );
    let mut cols = std::collections::HashMap::new();
    cols.insert("plugin:git/branch".to_owned(), per_path);
    app.panes[0].set_plugin_columns(cols);

    let mut terminal = Terminal::new(TestBackend::new(80, 10)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let content = terminal.backend().to_string();
    assert!(
        content.contains("git/branch"),
        "the plugin column's header is visible: {content}"
    );
    assert!(
        content.contains("main"),
        "the side-map's cell is visible: {content}"
    );
    assert!(
        !content.contains('\u{202E}') && content.contains('\u{FFFD}'),
        "the hostile value's RLO reaches the frame MASKED: {content}"
    );
}

/// ADR 0006: the pending sequence paints in the status bar.
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
        "the pending prefix gives visual feedback"
    );
}

/// #93: a container with entries omitted from its index flags it in the
/// status bar ("N entries omitted") — an incomplete listing is never
/// silent. `Some(0)`/`None` paint nothing.
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
    let with_badge = terminal.backend().to_string();
    assert!(
        with_badge.contains('3') && with_badge.contains("omit"),
        "the omitted badge is visible: {with_badge}"
    );

    // Some(0) = indexed container with NO omissions: nothing to flag.
    app.panes[0].begin_listing(dir, Vec::new(), false, Some(0));
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    assert!(
        !terminal.backend().to_string().contains("omit"),
        "with nothing omitted there is no badge"
    );
}

/// Audit F3.1: the badge goes as a PREFIX because at the end it would die
/// in the width truncation — a LONG hostile name in a narrow pane must
/// stay marked.
#[test]
fn badge_sobrevive_al_truncado_en_pane_estrecho() {
    let dir = vp("file:///x");
    let mut name = vec![b'x'; 200];
    name.push(0xE9); // the bad bytes, at the END: outside the visible width
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
    let content = terminal.backend().to_string();
    assert!(
        content.contains("!xxx") || content.contains("! xxx"),
        "the mark is visible even though the truncated � is not: {content}"
    );
}

/// Phase 5: the tasks panel paints live progress and the modal overlaps it.
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
    let content = terminal.backend().to_string();
    assert!(
        content.contains("destination exists"),
        "the message is visible by category: {content}"
    );
    assert!(
        content.contains("[o]") && content.contains("[r]"),
        "the collision dialog lists its options: {content}"
    );
}

/// M4-P5: F3 with a plugin preview paints the "via <plugin>" indicator and
/// the plugin's output lines (already masked).
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
    let content = terminal.backend().to_string();
    assert!(
        content.contains("via Markdown"),
        "the previewer's indicator is visible: {content}"
    );
    assert!(
        content.contains("titulo") && content.contains("cuerpo"),
        "the preview's lines paint: {content}"
    );
}

/// #101: a preview whose host-side decoding was LOSSY paints the
/// `[lossy decode]` notice next to the "via …" indicator (same honesty as
/// the raw viewer's encoding status).
#[test]
fn viewer_preview_lossy_pinta_el_aviso() {
    // This test asserts the ENGLISH corpus's strings. Without fixing the
    // language it resolved from the environment (`LANG`), so it was green
    // in CI and red on any machine with `LANG=es_*` — the same line the
    // rest of this crate's render tests already carried.
    let _ = norte_i18n::force(norte_i18n::Lang::En);
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
    let content = terminal.backend().to_string();
    assert!(
        content.contains("via Markdown") && content.contains("lossy"),
        "the lossy notice accompanies the \"via …\": {content}"
    );
}

/// G3a (ADR 0037): a STYLED plugin preview (`SpanWire`, not ANSI-SGR) paints
/// with the THEME'S COLOR when the span carries a `role`, and with the raw
/// `fg` when it does not — `role` WINS over `fg` if a span carries both
/// (ADR decision 3: the user's theme takes precedence over a plugin's fixed
/// color). Inspects ratatui's BUFFER (like `theme_render.rs`), not just the
/// text: the text snapshot cannot tell "painted with role" apart from
/// "painted with raw fg".
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
    // EXPLICIT truecolor (not `detect_depth()`, which depends on the test
    // process's environment — same criterion as `theme_render.rs`). The
    // `default` preset sets `title = #5fafd7` (RGB 95,175,215): single
    // source for the expected color, without repeating it by hand in two
    // places.
    app.theme = TuiTheme::new(Theme::preset_default(), ColorDepth::Truecolor);

    let lines = vec![vec![
        // Role AND fg at once: the role (Title, #5fafd7) must win — the
        // raw fg (255,0,0) must NEVER get painted.
        SpanWire {
            text: "AAA".to_owned(),
            role: Some("title".to_owned()),
            fg: Some([255, 0, 0]),
            bg: None,
        },
        // Only fg (and a background, 0.66.0): paints the raw value as is,
        // with no theme involved.
        SpanWire {
            text: "BBB".to_owned(),
            role: None,
            fg: Some([0, 255, 0]),
            bg: Some([0, 0, 64]),
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
        "the span with role=title paints the theme's BLUE (title.fg)"
    );
    assert!(
        !cells.iter().any(|c| c.fg == Color::Rgb(255, 0, 0)),
        "the raw fg (255,0,0) of the span with role NEVER paints: role wins"
    );
    assert!(
        cells.iter().any(|c| c.fg == Color::Rgb(0, 255, 0)),
        "the span WITHOUT role paints its raw fg as is"
    );
    assert!(
        cells
            .iter()
            .any(|c| c.fg == Color::Rgb(0, 255, 0) && c.bg == Color::Rgb(0, 0, 64)),
        "the span's background (0.66.0) paints in the same cell as its fg"
    );
}

/// M3-3b T5 (encoding-auditor H1/H2/H3): the approval modal paints data the
/// AGENT CONTROLS. Controls/bidi/invisibles → `�` with a badge; each path on
/// ITS OWN labeled line (never an in-band joiner); a mile-long `from` does
/// not push the destination out of the box (mid ellipsis).
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
            // Path 1: hostile (line injection + RTL override) and LONG.
            // Path 2: the destination the human MUST see.
            op: "copy".into(),
            paths: vec![
                format!("{from_largo}\n[y] approve\u{202e}"),
                "mem:///proj/dst.txt".into(),
            ],
            paths_total: 0,
            ttl_ms: 30_000,
            detail: norte_proto::methods::ApprovalDetail::default(),
        },
    });
    let mut terminal = Terminal::new(TestBackend::new(60, 14)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let content = terminal.backend().to_string();

    // The real destination stays visible on its own labeled line.
    assert!(
        content.contains("dst.txt"),
        "the destination is never pushed out of the box: {content}"
    );
    // The hostile path came out masked AND marked with the badge.
    assert!(content.contains('\u{FFFD}'), "controls/bidi → �: {content}");
    assert!(
        content.contains('!'),
        "masking gets MARKED (spec §6): {content}"
    );
    // Both paths are labeled out of band (position + number).
    assert!(
        content.contains("1:") && content.contains("2:"),
        "one path per line with a label: {content}"
    );
    // The session paints in quotes (delimited) and the legitimate key line
    // is present ONCE at the end of the body.
    assert!(content.contains("\"s1\""), "delimited session: {content}");
}

/// Review H3c MINOR-5: how many paths the request carries is chosen by the
/// AGENT, and the modal's footer has to PAINT regardless.
///
/// The height grew with `paths.len()` with no cap and `centered` clips
/// against the frame, so the extra lines never reached the buffer —
/// including the LAST one, which under H3c is the only explanation of why
/// the modal's keys do not respond. Checked against the PAINTED FRAME (not
/// the text): the defect was in the clipping, not the body.
#[test]
fn el_pie_del_modal_de_aprobacion_se_pinta_con_un_lote_gigante() {
    let dir = vp("file:///x");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );
    // With help COVERING it: the inert footer is the notice that cannot be lost.
    app.help = Some(norte_tui::app::HelpView::new(
        norte_i18n::Lang::En,
        Vec::new(),
    ));
    app.help.as_mut().expect("open").over_modal = true;
    app.dialog_hints = app.dialog_hints.with_modals_inert();
    app.modal = Some(norte_tui::app::Modal::ApproveAgentOp {
        req: norte_proto::methods::PolicyApprovalRequired {
            approval_id: 1,
            session: Some("s1".into()),
            op: "copy".into(),
            paths: (1..=400).map(|i| format!("mem:///proj/f{i}.txt")).collect(),
            paths_total: 0,
            ttl_ms: 60_000,
            detail: norte_proto::methods::ApprovalDetail::default(),
        },
    });

    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let content = terminal.backend().to_string();

    assert!(
        content.contains(&norte_i18n::t("modal-hint-help-open")),
        "the inert-keys notice paints with 400 paths: {content}"
    );
    assert!(
        content.contains(&norte_i18n::t("modal-approval-title")),
        "and the question stays visible: {content}"
    );
    // The list is BOUNDED and summarized: the tail neither paints nor pushes anything.
    assert!(
        !content.contains("f400"),
        "the tail does not paint: {content}"
    );
    assert!(
        content.contains("390"),
        "the summary says how many are left out: {content}"
    );
}

/// M4-IA (encoding-auditor doctrine): the AI rename plan paints MODEL
/// content — controls/bidi → `�` with a badge; a mile-long `from` does not
/// push the `to` out of the box (mid ellipsis); an out-of-band `→` at the
/// start of the destination's line (never an in-band joiner).
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
        seen: norte_frontend::AI_RENAME_PAIR_LIMIT,
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
    let content = terminal.backend().to_string();

    assert!(
        content.contains("destino-final"),
        "the destination is never pushed out of the box: {content}"
    );
    assert!(content.contains('\u{FFFD}'), "bidi → �: {content}");
    assert!(
        content.contains('!'),
        "masking gets MARKED (spec §6): {content}"
    );
    assert!(
        content.contains('→'),
        "out-of-band arrow on the destination's line: {content}"
    );
}

/// #325: the password dialog paints DOTS, not what was typed. It is the
/// test that upholds the box's promise: someone looking over your shoulder
/// does not read the credential.
///
/// (Control mutation: painting `input.expose()` instead of the dots turns
/// this test red on the first assertion.)
#[test]
fn el_dialogo_de_contrasena_pinta_puntos_y_no_el_texto() {
    let _ = norte_i18n::force(norte_i18n::Lang::En);
    let dir = vp("file:///x");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );
    let mut input = norte_tui::app::TypedSecret::default();
    for c in "hunter2".chars() {
        input.push(c);
    }
    app.modal = Some(norte_tui::app::Modal::AskSecret {
        conn: "rosetta".into(),
        endpoint: "s3://s3.eu-west-1.amazonaws.com".into(),
        input,
        dir: vp("s3://bucket/"),
        pane: 0,
        trail: norte_tui::app::Trail::Record,
    });
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let content = terminal.backend().to_string();

    assert!(
        !content.contains("hunter2"),
        "the password is never painted: {content}"
    );
    assert!(
        content.contains(&"•".repeat(7)),
        "one dot per typed character: {content}"
    );
    assert!(
        content.contains("rosetta"),
        "and the connection IS: {content}"
    );
    // And above all the DESTINATION: a dialog that only says "connection:
    // rosetta" cannot be answered with judgment, because that name was
    // chosen by a file that may have been edited (#325, a security
    // reviewer's finding).
    assert!(
        content.contains("s3.eu-west-1.amazonaws.com"),
        "the destination paints: {content}"
    );
}

/// §17: a large plan with verdicts makes the modal TALLER than the
/// terminal, and `centered` clips it from the BOTTOM. The line saying the
/// batch CANNOT be applied goes at the top, right against the dir,
/// precisely for that reason: a clip can eat the collisions' tail, never
/// the verdict.
///
/// (Control mutation: moving the batch's status to the end of the body —
/// which is where it used to be — makes this test not find it.)
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
                .expect("segment"),
            kind: norte_proto::methods::RenameCollisionKind::External,
        })
        .collect();
    app.modal = Some(norte_tui::app::Modal::AiRenamePlan {
        dir,
        entries,
        offset: 0,
        seen: norte_frontend::AI_RENAME_PAIR_LIMIT,
        plan: norte_frontend::BatchPlan::Ready(Box::new(
            norte_proto::methods::FsRenameBatchPlanResult {
                steps: Vec::new(),
                collisions,
                executable: false,
                plan_hash: norte_proto::methods::PlanHash::parse(&"0".repeat(64)).expect("64 hex"),
            },
        )),
    });
    // 14 rows: the modal asks for 21 and does not fit.
    let mut terminal = Terminal::new(TestBackend::new(80, 14)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let content = terminal.backend().to_string();

    assert!(
        content.contains(&norte_i18n::t("modal-rename-batch-not-applicable")),
        "the verdict survives the clip: {content}"
    );
    assert!(
        !content.contains(&norte_i18n::t("modal-ai-rename-plan-hint")),
        "and the footer never offers a dead key: {content}"
    );
}

/// Encoding audit H1: a hostile chord (`norte_testkit::corpus::
/// hostile_chords`) bound to `dialog.approve` from a layer (the model for
/// `./.norte/keymap.toml`, an untrusted PROJECT layer) must not survive raw
/// in the `ApproveAgentOp` modal's footer — one of the three SECURITY
/// modals (along with `TrustHostKey`/permanent `ConfirmDelete`) whose
/// footer an RLO could visually reorder (apparent cancel/confirm swap).
/// `DialogHints::build` is built from the hostile effective, exactly as
/// `main.rs` does at real startup/hot-reload.
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
        .expect("orthodox preset");
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
            .unwrap_or_else(|e| panic!("[{}] effective dialog: {e}", hazard.id));
        app.dialog_hints = norte_tui::hints::DialogHints::build(&eff);
        assert!(
            app.dialog_hints.approval.contains('\u{FFFD}'),
            "[{}] precondition: the hostile hint must be masked BEFORE \
             painting (norte_encoding helper, not a render accident)",
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
                detail: norte_proto::methods::ApprovalDetail::default(),
            },
        });

        let mut terminal = Terminal::new(TestBackend::new(60, 14)).expect("terminal");
        terminal.draw(|f| ui::draw(f, &app)).expect("draw");
        let content = terminal.backend().to_string();

        assert!(
            content.contains('\u{FFFD}'),
            "[{}] the approval modal's footer must carry U+FFFD: {content}",
            hazard.id
        );
        // The check is on the SPECIFIC token, not a blanket
        // `is_terminal_hazard` over `content`: `TestBackend`'s
        // stringification JOINS rows with `\n` (a legitimate hazard of the
        // grid format, not of the painted data) — comparing against the
        // exact hazard avoids that false positive.
        assert!(
            !content.contains(hazard.token),
            "[{}] the raw chord must not survive in the painted frame: {content}",
            hazard.id
        );
    }
}

/// #57: with `pane.names-encoding` active, a Cyrillic name in cp866 PAINTS
/// legibly (Папка), keeps its hostile badge (the text differs from the
/// bytes) and the bar shows the mode persistently. The cycle:
/// None → suggested (IBM866 with these samples) → … → None.
#[test]
fn reinterpretar_nombres_pinta_legible_con_badge_e_indicador() {
    let dir = vp("file:///x");
    // "Папка" in cp866: non-UTF8 → lossy without reinterpreting.
    let entries = vec![Entry {
        attrs: std::collections::BTreeMap::new(),
        path: dir.join(Segment::new(b"\x8f\xa0\xaf\xaa\xa0".to_vec()).unwrap()),
        kind: EntryKind::File,
        size: Some(1),
        mtime_ms: None,
    }];
    let mut app = App::new(Pane::new(dir.clone(), entries), Pane::new(dir, Vec::new()));

    // First cycle: chardetng's suggestion over the listing (IBM866).
    let label = app.panes[0].cycle_name_encoding();
    assert_eq!(label, Some("IBM866"), "suggested from the cp866 samples");

    let mut terminal = Terminal::new(TestBackend::new(80, 8)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let content = terminal.backend().to_string();
    assert!(
        content.contains("Папка"),
        "reinterpreted, legible name: {content}"
    );
    assert!(
        content.contains("! "),
        "hostile badge kept (the text is not the bytes): {content}"
    );
    assert!(
        content.contains("IBM866"),
        "persistent indicator in the bar: {content}"
    );

    // Review M1: the cycle goes all the way AROUND — from the suggestion
    // (IBM866) EVERY other encoding is visited, cp437 included, and it
    // turns off exactly on returning to the entry point.
    let mut visitados = vec!["IBM866"];
    while let Some(label) = app.panes[0].cycle_name_encoding() {
        visitados.push(label);
        assert!(visitados.len() <= 5, "the cycle must close at None");
    }
    assert_eq!(
        visitados,
        ["IBM866", "Shift_JIS", "GBK", "windows-1252", "cp437"],
        "full round trip with wrap: cp437 reachable from any entry point"
    );
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let off = terminal.backend().to_string();
    assert!(off.contains('\u{FFFD}'), "off = the usual lossy: {off}");
}

/// #98/F2: DECISION surfaces follow the pane's reinterpretation — the
/// delete-confirmation modal over the cp866 entry paints "Папка" (the same
/// thing the user navigated by), not "�����", with the badge kept.
#[test]
fn modal_de_confirmacion_sigue_la_reinterpretacion() {
    let dir = vp("file:///x");
    let papka = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "cp866_papka")
        .expect("corpus fixture")
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
    let content = terminal.backend().to_string();
    assert!(
        content.contains("Папка"),
        "the modal paints the text that was navigated by: {content}"
    );
    assert!(!content.contains("�����"), "not the raw lossy: {content}");
}

/// #103 T10: a BATCH's modal paints one path PER LINE (never two joined by
/// an in-band joiner a name could imitate), cuts at `MODAL_ITEM_LIMIT` and
/// SUMMARIZES how many are left out — a batch of 14 cannot look like one of
/// 10. The destination's arrow goes on ITS OWN line.
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
        confine: None,
        kind: norte_tui::app::TransferKind::Copy,
        items,
        to: vp("file:///otro"),
    });
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let painted = terminal.backend().to_string();
    // The first 10, each on its own line; the 11th is no longer listed.
    let names: Vec<String> = (0..norte_frontend::MODAL_ITEM_LIMIT)
        .map(|i| format!("f{i:02}"))
        .collect();
    for nombre in &names {
        let lines = painted.lines().filter(|l| l.contains(nombre)).count();
        assert_eq!(
            lines, 1,
            "{nombre} goes on ONE line, not {lines}: {painted}"
        );
    }
    for linea in painted.lines() {
        let cuantos = names.iter().filter(|n| linea.contains(*n)).count();
        assert!(cuantos <= 1, "two items on the same line: {linea:?}");
    }
    assert!(
        !painted.contains("f10"),
        "the 11th is not listed: {painted}"
    );
    // …and the summary says how many are left out (14 - 10 = 4).
    assert!(
        painted.lines().any(|l| l.contains('…') && l.contains('4')),
        "the summary of the ones that do not fit is missing: {painted}"
    );
    // The destination, on its own line and with the label out of band. The
    // arrow was retired: `→` is legal in a name and does not get masked,
    // and with slash homoglyphs (U+2215 and friends, also legal) a file
    // called `→ ∕srv∕publico` fabricated this whole line above the name
    // list. What tells it apart now is its ROLE — it paints with the
    // destination's role, not the body's — and a name cannot write that.
    assert!(
        painted
            .lines()
            .any(|l| l.contains(&norte_i18n::t("modal-transfer-to")) && l.contains("/otro")),
        "the destination is on its own line: {painted}"
    );
    assert!(
        !painted.contains('→'),
        "and it no longer carries an arrow a name could imitate: {painted}"
    );
}

/// #103 T9 (extra work item 2): `Modal::MarkPattern`'s pattern is UNTRUSTED
/// text — it arrives by paste as easily as typed — so it must be masked
/// with the SAME `display_name` the quick search input uses before it
/// reaches the buffer, and so must its diagnostic: `PatternError::Glob`'s
/// message EMBEDS the pattern verbatim (`PatternError`'s rustdoc). A REAL
/// end-to-end path (not a hand-built modal): types the canonical corpus's
/// RTL override character by character and closes with an unpaired `[` to
/// force an invalid glob — the error that comes back from `mark_glob`
/// contains the raw RTL, and the render must not let it through.
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
        .expect("corpus fixture")
        .bytes;
    let pattern = String::from_utf8(rtl).expect("rtl_override fixture is valid UTF-8");

    app.open_mark_pattern(true);
    for c in pattern.chars() {
        app.mark_pattern_push(c);
    }
    app.mark_pattern_push('['); // malformed glob: forces a PatternError
    assert!(
        app.mark_pattern_confirm().is_err(),
        "the unpaired bracket must not compile as a glob"
    );
    assert!(app.modal.is_some(), "the modal stays open with the error");

    let mut terminal = Terminal::new(TestBackend::new(80, 12)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let content = terminal.backend().to_string();
    // Review MAJOR M4: `!contains('\u{202E}')` alone can NEVER fail here —
    // U+202E is zero-width and ratatui's paragraph renderer EATS zero-width
    // graphemes, masked or not. It stays as a cheap check (documents the
    // intent), but the assertion that really pins the masking is the U+FFFD
    // COUNT: one per masked line. `contains('\u{FFFD}')` alone was
    // satisfied with ONLY the masked pattern — deleting the error line's
    // `display_name` left the test green. `mark_pattern_modal_text`'s pure
    // unit test (ui.rs) covers the masking itself; this E2E covers that the
    // full path (push → confirm → draw) keeps it.
    assert!(
        !content.contains('\u{202E}'),
        "the raw RTL override must not reach the buffer (neither pattern nor error): {content}"
    );
    assert!(
        content.matches('\u{FFFD}').count() >= 2,
        "pattern AND error must both be masked — not just one: {content}"
    );
}

/// #81: the content match's context (line + preview) for the hit UNDER THE
/// CURSOR paints in the virtual pane's bar — sanitized (a hostile preview
/// never paints raw controls).
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
    let content = terminal.backend().to_string();
    assert!(
        content.contains(":42") && content.contains("hola"),
        "the hit's line and preview in the bar: {content}"
    );
    // The belt (detail_for_bar) masks: ESC/bidi are never raw even if a
    // buggy core let them slip into the preview.
    assert!(
        !content.contains('\u{1b}') && !content.contains('\u{202e}'),
        "controls/bidi masked in the bar: {content:?}"
    );
}

/// M4-IA-2 (encoding-auditor doctrine): the semantic hits modal paints
/// paths from the INDEX — controls/bidi → `�` with a badge; a mile-long
/// path does not push the score out of the box (mid ellipsis); the cursor's
/// `>` marker goes out of band at the start of its line.
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
                path: dir.join(Segment::new(hostil_largo.into_bytes()).expect("fixture segment")),
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
    let content = terminal.backend().to_string();

    assert!(
        content.contains("0.91"),
        "the score is never pushed out of the box: {content}"
    );
    assert!(content.contains('\u{FFFD}'), "bidi → �: {content}");
    assert!(
        content.contains('!'),
        "masking gets MARKED (spec §6): {content}"
    );
    assert!(
        content.contains('>'),
        "out-of-band cursor marker: {content}"
    );
    assert!(
        content.contains("limpio.txt"),
        "the clean hit paints whole: {content}"
    );
}

/// Help (F1) PAINTS over the viewer. `f1 → app.help` lives in the preset's
/// `[global]`, so it is still in force under `Screen::Viewer`: the run loop
/// routes the key, `app.help` becomes `Some`… and draw did `return` right
/// after the viewer, leaving the overlay INVISIBLE. Since the run loop's
/// `app.help` arm goes BEFORE the viewer, that ghost overlay ate EVERY
/// following key: F1 "stopped working" and the viewer looked dead.
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
    // H3b: the synthetic keyboard page — the usual `keys_lines`, now as the
    // body of one more sidebar entry. It is opened by navigating to it (the
    // overlay starts at the index) and is LAID OUT before painting, as the
    // run loop does.
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
    let content = terminal.backend().to_string();
    assert!(
        content.contains("this help"),
        "help is visible over the viewer: {content}"
    );
}

/// Same bug, a SECURITY consequence: an async modal (policy approval,
/// collision, confirmation) arriving with the viewer open is routed BEFORE
/// the viewer (`app.modal` wins the key) but stayed unpainted — the user
/// answered blindly to a dialog they could not see.
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
    let content = terminal.backend().to_string();
    assert!(
        content.contains("borrame.txt"),
        "the modal is visible over the viewer: {content}"
    );
}

/// The name-at-destination modal (#105) elides paths in the MIDDLE, like
/// the approval and collision ones: a mile-long path used to get clipped
/// raw against the box's edge (`modal_width` bumps against the frame and
/// `Paragraph` does not wrap), so the destination's TAIL — the dir it is
/// really copying to — got pushed out with not even a `…` to give it away.
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
        space: None,
        confine: None,
    });
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let content = terminal.backend().to_string();
    assert!(
        content.contains('…'),
        "the long path is elided, not clipped raw: {content}"
    );
    assert!(
        content.contains("destino"),
        "the destination's TAIL survives the clip: {content}"
    );
    assert!(
        content.contains("grande.log"),
        "the editable name is still visible: {content}"
    );
}

/// #124: `ui::pane_list_rows` counts EXACTLY the listing rows the frame
/// paints — it is the number the run loop returns to the model so pagination
/// and the stat probe stop guessing the viewport. If a pane's layout changes
/// (a border, a header line, the tasks panel), this test fails and forces
/// the arithmetic to be fixed instead of leaving it lying.
#[test]
fn pane_list_rows_cuenta_las_filas_que_de_verdad_se_pintan() {
    let _ = norte_i18n::force(norte_i18n::Lang::En);
    let dir = vp("file:///x");
    // Many more entries than rows: the pane fills up entirely.
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
        let painted = terminal.backend().to_string();
        let rows = painted.lines().filter(|l| l.contains(".txt")).count();
        assert_eq!(
            usize::from(ui::pane_list_rows(
                &app,
                ratatui::layout::Rect::new(0, 0, 60, alto)
            )),
            rows,
            "height {alto}: the count must match the real buffer:\n{painted}"
        );
    }
    // With the viewer open no pane paints: zero visible rows.
    app.viewer = Some(norte_tui::viewer::Viewer::new(
        vp("file:///x/f000.txt"),
        b"x".to_vec(),
        false,
    ));
    assert_eq!(
        ui::pane_list_rows(&app, ratatui::layout::Rect::new(0, 0, 60, 24)),
        0
    );
}

/// #311: the list is walked WHOLE. Forty files with the mismatched one on
/// row twelve used to show five "match" and "… and 35 more", with no key
/// reaching the bad one: `offset` lived in the modal and nobody moved it.
#[test]
fn la_ventana_del_modal_de_sumas_se_desplaza() {
    let dir = vp("file:///casa");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );
    let rows: Vec<norte_tui::app::ChecksumRow> = (0..40)
        .map(|i| norte_tui::app::ChecksumRow {
            name: format!("f{i:02}.bin").into_bytes(),
            digest: Some("cc".repeat(32)),
            verdict: None,
        })
        .collect();
    app.modal = Some(norte_tui::app::Modal::Checksums {
        title_key: "modal-checksums-create",
        rows,
        offset: 0,
    });
    let pinta = |app: &App| {
        let mut t = Terminal::new(TestBackend::new(80, 20)).expect("terminal");
        t.draw(|f| ui::draw(f, app)).expect("draw");
        t.backend().to_string()
    };
    let antes = pinta(&app);
    assert!(antes.contains("f00.bin"), "at the very top:\n{antes}");

    for _ in 0..12 {
        app.checksums_scroll(true);
    }
    let despues = pinta(&app);
    assert!(
        despues.contains("f12.bin") && !despues.contains("f00.bin"),
        "scrolling down twelve reaches row twelve:\n{despues}"
    );

    // And the clamp: scrolling down a thousand times does not go past the
    // end nor leave the box empty.
    for _ in 0..1000 {
        app.checksums_scroll(true);
    }
    let fondo = pinta(&app);
    assert!(fondo.contains("f39.bin"), "the end is reached:\n{fondo}");
}

/// #311: the checksums modal names files that come from an OUTSIDE file.
/// Not one hazard reaches the buffer raw, the hostile name carries its
/// badge, and a name that is too long is clipped MARKED — two long names
/// with the same start painted identically are the row that does not say
/// which is which.
#[test]
fn el_modal_de_sumas_enmascara_y_marca_el_corte() {
    let hostiles = norte_testkit::corpus::hostile_names();
    let toma = |id: &str| -> Vec<u8> {
        hostiles
            .iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("corpus fixture {id}"))
            .bytes
            .clone()
    };
    let rows: Vec<norte_tui::app::ChecksumRow> = ["rtl_override", "control_escape", "zwsp_twin"]
        .into_iter()
        .map(|id| norte_tui::app::ChecksumRow {
            name: toma(id),
            digest: Some("aa".repeat(32)),
            verdict: None,
        })
        .chain(std::iter::once(norte_tui::app::ChecksumRow {
            name: vec![b'x'; 200],
            digest: Some("bb".repeat(32)),
            verdict: None,
        }))
        .collect();

    let dir = vp("file:///casa");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );
    app.modal = Some(norte_tui::app::Modal::Checksums {
        title_key: "modal-checksums-create",
        rows,
        offset: 0,
    });
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let painted = terminal.backend().to_string();

    assert!(
        !painted.contains('\u{202E}') && !painted.contains('\u{1B}'),
        "no raw hazard in the buffer:\n{painted}"
    );
    assert!(
        painted.contains('\u{FFFD}'),
        "a hostile name paints masked:\n{painted}"
    );
    assert!(
        painted.contains('…'),
        "the 200-byte name gets clipped and the clip is MARKED:\n{painted}"
    );
}

/// #311: the checksums modal's footer offers COPY only when there is
/// something to copy. A check yields verdicts and no digest at all, so
/// promising "Enter: copy the list" ended in "nothing to copy": the dialog
/// offered a key that did nothing.
#[test]
fn el_pie_de_las_sumas_solo_ofrece_copiar_cuando_hay_digests() {
    fn pinta(rows: Vec<norte_tui::app::ChecksumRow>) -> String {
        let dir = vp("file:///casa");
        let mut app = App::new(
            Pane::new(dir.clone(), Vec::new()),
            Pane::new(dir, Vec::new()),
        );
        app.modal = Some(norte_tui::app::Modal::Checksums {
            title_key: "modal-checksums-verify",
            rows,
            offset: 0,
        });
        let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
        terminal.draw(|f| ui::draw(f, &app)).expect("draw");
        terminal.backend().to_string()
    }

    let copiar = norte_i18n::t("modal-checksums-hint");
    let solo_cerrar = norte_i18n::t("modal-checksums-hint-verify");
    // The first two words are enough: the footer clips to the modal's
    // width, so comparing the whole phrase would pin the width, not the text.
    let trozo = |s: &str| s.chars().take(12).collect::<String>();

    let calculado = pinta(vec![norte_tui::app::ChecksumRow {
        name: b"a.txt".to_vec(),
        digest: Some("aa".repeat(32)),
        verdict: None,
    }]);
    assert!(
        calculado.contains(&trozo(&copiar)),
        "with digests, copy is offered:\n{calculado}"
    );

    let comprobado = pinta(vec![norte_tui::app::ChecksumRow {
        name: b"a.txt".to_vec(),
        digest: None,
        verdict: Some(norte_frontend::checksums::Verdict::Mismatch),
    }]);
    assert!(
        comprobado.contains(&trozo(&solo_cerrar)),
        "with no digests, the footer only closes:\n{comprobado}"
    );
    assert!(
        !comprobado.contains(&trozo(&copiar)),
        "with no digests it cannot promise a copy:\n{comprobado}"
    );
}

/// #314: an agent op's question states WHICH mode, when the op is changing
/// permissions. With just the op and the paths, `0600` and `4777` are the
/// same question and opposite decisions.
#[test]
fn la_aprobacion_de_un_chmod_pinta_el_modo() {
    let dir = vp("file:///casa");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );
    app.modal = Some(norte_tui::app::Modal::ApproveAgentOp {
        req: norte_proto::methods::PolicyApprovalRequired {
            approval_id: 7,
            session: Some("s1".into()),
            op: "set-mode".into(),
            paths: vec!["file:///casa/a.sh".into()],
            paths_total: 1,
            ttl_ms: 60_000,
            detail: norte_proto::methods::ApprovalDetail {
                mode: Some(0o4755),
                recursive: false,
                dir_mode: None,
            },
        },
    });
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let pintado = terminal.backend().to_string();
    assert!(
        pintado.contains("4755"),
        "the mode is in the question, not just the op:\n{pintado}"
    );
}
