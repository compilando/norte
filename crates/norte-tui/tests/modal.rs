//! Tests del estado modal (fase 5) + H1 T2 (issue #24): las teclas de
//! diálogo ahora se RESUELVEN contra el contexto `dialog` del keymap y el
//! comando resultante se filtra por el ALLOWLIST del modal concreto
//! (`app::dialog_action`) — la semántica de SEGURIDAD (qué confirma, qué
//! deniega, qué es inerte) vive en código; solo la asignación tecla→comando
//! es rebindeable. `Modal::TrustLuaInit` es la única excepción (decisión 8
//! del plan H1): sigue resuelta con `app::trust_lua_key` sobre el
//! `crossterm::event::KeyCode` crudo.

use norte_core::TransferOptions;
use norte_proto::{CollisionPolicy, VPath};
use norte_tui::app::{DialogOutcome, Modal, Trail, TransferKind, dialog_action};
use norte_tui::keymap::{
    COMMANDS, Chord, DIALOG_COMMANDS, Effective, KeyCode, Mods, Resolution, Resolver, Screen,
    parse_keymap,
};
use norte_tui::tasks::RetrySpec;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

fn confirm() -> Modal {
    Modal::ConfirmTransfer {
        kind: TransferKind::Copy,
        items: vec![vp("file:///a")],
        to: vp("file:///b"),
    }
}

fn retry() -> RetrySpec {
    RetrySpec {
        kind: TransferKind::Move,
        from: vp("file:///a"),
        to: vp("file:///b"),
        opts: TransferOptions::default(),
        name_encoding: None,
    }
}

fn collision() -> Modal {
    Modal::Collision { retry: retry() }
}

fn approval() -> Modal {
    Modal::ApproveAgentOp {
        req: norte_proto::methods::PolicyApprovalRequired {
            approval_id: 7,
            session: Some("s1".into()),
            op: "copy".into(),
            paths: vec!["mem:///proj/a".into(), "mem:///proj/b".into()],
            ttl_ms: 60_000,
        },
    }
}

fn trust_host() -> Modal {
    Modal::TrustHostKey {
        host: "h".into(),
        port: Some(22),
        algo: "ssh-ed25519".into(),
        fingerprint: "SHA256:AAAA".into(),
        dir: vp("sftp://h/"),
        pane: 0,
        trail: Trail::Record,
    }
}

/// Safety pin H1 T2: Enter (`dialog.confirm`) es INERTE sobre una
/// aprobación de agente — aprobar una mutación de agente no es una
/// respuesta inocua que Enter deba disparar sola (decisión 2 del plan).
#[test]
fn aprobacion_ignora_confirm() {
    assert_eq!(dialog_action(&approval(), "dialog.confirm"), None);
}

/// Safety pin H1 T2: mismo principio para el TOFU de host key SSH (#45) —
/// Enter jamás confía en una clave sin verificar.
#[test]
fn trust_host_ignora_confirm() {
    assert_eq!(dialog_action(&trust_host(), "dialog.confirm"), None);
}

/// Safety pin H1 T2: la colisión no tiene default peligroso — ni
/// `dialog.confirm` (Enter) ni `dialog.deny` (`n`, que en la colisión no es
/// una política válida) hacen nada; solo overwrite/skip/rename/newer/cancel.
#[test]
fn colision_ignora_confirm_y_deny() {
    assert_eq!(dialog_action(&collision(), "dialog.confirm"), None);
    assert_eq!(dialog_action(&collision(), "dialog.deny"), None);
}

/// Safety pin H1 T2: en las confirmaciones (borrado/transferencia), tanto
/// `dialog.confirm` (Enter) como `dialog.approve` (`y`) aceptan — mismo
/// comportamiento que antes de H1.
#[test]
fn confirm_acepta_confirm_y_approve() {
    assert_eq!(
        dialog_action(&confirm(), "dialog.confirm"),
        Some(DialogOutcome::Confirmed)
    );
    assert_eq!(
        dialog_action(&confirm(), "dialog.approve"),
        Some(DialogOutcome::Confirmed)
    );
}

#[test]
fn confirmacion_acepta_y_cancela() {
    for cmd in ["dialog.confirm", "dialog.approve"] {
        assert_eq!(
            dialog_action(&confirm(), cmd),
            Some(DialogOutcome::Confirmed)
        );
    }
    for cmd in ["dialog.cancel", "dialog.deny"] {
        assert_eq!(
            dialog_action(&confirm(), cmd),
            Some(DialogOutcome::Cancelled)
        );
    }
    // Comando fuera del allowlist: inerte (el diálogo sigue abierto).
    assert_eq!(dialog_action(&confirm(), "dialog.overwrite"), None);
    let borrar = Modal::ConfirmDelete {
        items: vec![vp("file:///x")],
        permanent: false,
    };
    assert_eq!(
        dialog_action(&borrar, "dialog.confirm"),
        Some(DialogOutcome::Confirmed)
    );
    assert_eq!(
        dialog_action(&borrar, "dialog.cancel"),
        Some(DialogOutcome::Cancelled)
    );
}

#[test]
fn colision_elige_politica_o_cancela() {
    let casos = [
        ("dialog.overwrite", CollisionPolicy::Overwrite),
        ("dialog.skip", CollisionPolicy::Skip),
        ("dialog.rename", CollisionPolicy::RenameAuto),
        // La colisión cambió `n`→`w` (dialog.newer, decisión 5 del plan
        // H1): `n` ahora es `dialog.deny`, inerte en este modal (pin
        // arriba, `colision_ignora_confirm_y_deny`).
        ("dialog.newer", CollisionPolicy::Newer),
    ];
    for (cmd, policy) in casos {
        assert_eq!(
            dialog_action(&collision(), cmd),
            Some(DialogOutcome::Retry(policy)),
            "comando {cmd}"
        );
    }
    assert_eq!(
        dialog_action(&collision(), "dialog.cancel"),
        Some(DialogOutcome::Cancelled)
    );
}

/// Aprobación de agente (M3-3b T5): `dialog.approve` aprueba, `dialog.deny`
/// y `dialog.cancel` DENIEGAN (cerrar es denegar, fail-safe);
/// `dialog.confirm` es inerte (pin `aprobacion_ignora_confirm`).
#[test]
fn aprobacion_aprueba_con_approve_y_deniega_con_deny_cancel() {
    assert_eq!(
        dialog_action(&approval(), "dialog.approve"),
        Some(DialogOutcome::Confirmed)
    );
    for cmd in ["dialog.deny", "dialog.cancel"] {
        assert_eq!(
            dialog_action(&approval(), cmd),
            Some(DialogOutcome::Cancelled)
        );
    }
    // Comando fuera del allowlist de este modal.
    assert_eq!(dialog_action(&approval(), "dialog.overwrite"), None);
}

/// Integración H1 T2 (issue #24, el "payoff" de datificar el keymap): un
/// keymap TOML real —preset `orthodox` + capa de usuario— rebindea `y` de
/// `dialog.approve` a `dialog.deny`, se RESUELVE con el motor de
/// `norte-frontend` (el mismo camino que el run loop) y `dialog_action`
/// obedece el comando resuelto, no la tecla física: la aprobación de agente
/// ahora la DENIEGA.
#[test]
fn una_capa_de_usuario_rebindea_dialog_y_dialog_action_lo_obedece() {
    let preset = presets_orthodox();
    let layer =
        parse_keymap("[dialog]\nprepend_keymap = [{ on = [\"y\"], run = \"dialog.deny\" }]\n")
            .expect("capa válida");
    // El TUI pasa la UNIÓN de COMMANDS ∪ DIALOG_COMMANDS a Screen::Dialog
    // (T1 confirmó: build_for_impl valida TODO el efectivo fusionado,
    // incluido [global], no solo la sección específica).
    let known: Vec<&str> = COMMANDS
        .iter()
        .copied()
        .chain(DIALOG_COMMANDS.iter().copied())
        .collect();
    let eff = Effective::build_for(&preset, &[layer], &known, Screen::Dialog)
        .expect("el efectivo construye con la capa rebindeada");
    let mut resolver = Resolver::new(eff);
    let res = resolver.push(Chord::new(Mods::default(), KeyCode::Char('y')));
    assert_eq!(res, Resolution::Run("dialog.deny".to_owned()));
    let Resolution::Run(cmd) = res else {
        unreachable!()
    };
    // El comando RESUELTO (no la tecla) decide el desenlace: `y` ahora
    // deniega la aprobación de agente, dato puro, sin tocar código.
    assert_eq!(
        dialog_action(&approval(), &cmd),
        Some(DialogOutcome::Cancelled)
    );
}

/// review MINOR-3 (H1 close): `ALLOW_APPROVAL` excluye `dialog.confirm` A
/// PROPÓSITO — de fábrica, Enter NUNCA aprueba una mutación de agente
/// (decisión 2 del plan H1). Pero si un usuario rebindea EXPLÍCITAMENTE
/// `enter` a `dialog.approve` en su PROPIA capa de keymap, Enter SÍ aprueba
/// — este test documenta ese agujero como ACEPTADO, no como bug: la
/// semántica de seguridad sigue viviendo en `dialog_action` (el comando
/// RESUELTO decide, nunca la tecla física), y llegar aquí exige una capa de
/// usuario escrita a mano — ningún preset de fábrica la trae — así que es
/// consentimiento informado, no una tecla que se dispara sola.
#[test]
fn rebind_explicito_de_enter_a_approve_es_consentimiento_informado() {
    let preset = presets_orthodox();
    let layer = parse_keymap(
        "[dialog]\nprepend_keymap = [{ on = [\"enter\"], run = \"dialog.approve\" }]\n",
    )
    .expect("capa válida");
    let known: Vec<&str> = COMMANDS
        .iter()
        .copied()
        .chain(DIALOG_COMMANDS.iter().copied())
        .collect();
    let eff = Effective::build_for(&preset, &[layer], &known, Screen::Dialog)
        .expect("el efectivo construye con la capa rebindeada");
    let mut resolver = Resolver::new(eff);
    let res = resolver.push(Chord::new(Mods::default(), KeyCode::Enter));
    assert_eq!(res, Resolution::Run("dialog.approve".to_owned()));
    let Resolution::Run(cmd) = res else {
        unreachable!()
    };
    assert_eq!(
        dialog_action(&approval(), &cmd),
        Some(DialogOutcome::Confirmed),
        "un rebind EXPLÍCITO de enter a dialog.approve sí aprueba: consentimiento informado"
    );
}

/// El preset `orthodox` embebido, ya parseado (helper del test de
/// integración anterior).
fn presets_orthodox() -> norte_tui::keymap::KeymapFile {
    norte_tui::keymap::presets()
        .into_iter()
        .find(|(n, _)| *n == "orthodox")
        .expect("preset orthodox")
        .1
}

/// App de dos panes sobre el mismo dir, sin entradas (fixture de los tests
/// de estado M4-IA).
fn app() -> norte_tui::app::App {
    use norte_tui::app::{App, Pane};
    let dir = vp("file:///x");
    App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    )
}

/// Plan de rename IA con una pareja (fixture del test de allowlist).
fn ai_plan() -> Modal {
    Modal::AiRenamePlan {
        dir: vp("file:///x"),
        entries: vec![norte_proto::methods::AiRenameEntry {
            from: "a".into(),
            to: "b".into(),
        }],
        offset: 0,
    }
}

/// M4-IA: el prompt de instrucción sigue la disciplina de `Mkdir` (#104
/// review MINOR-1) — confirmar NO cierra; `ai_rename_set_error` deja un
/// diagnóstico SÍNCRONO conservando lo tecleado (audit INFO-7: en el flujo
/// real los fallos del modelo llegan async con el prompt ya cerrado y van
/// a la barra); solo `ai_rename_submitted` cierra.
#[test]
fn ai_rename_instruccion_conserva_texto_tras_fallo() {
    let mut app = app();
    app.open_ai_rename();
    for c in "kebab".chars() {
        app.ai_rename_push(c);
    }
    assert_eq!(app.ai_rename_confirm().as_deref(), Some("kebab"));
    app.ai_rename_set_error("boom".into());
    match &app.modal {
        Some(Modal::AiRenameInstruction { instruction, error }) => {
            assert_eq!(instruction, "kebab");
            assert_eq!(error.as_deref(), Some("boom"));
        }
        other => panic!("modal inesperado: {other:?}"),
    }
    app.ai_rename_submitted();
    assert!(app.modal.is_none());
}

/// M4-IA: confirmar con la instrucción vacía no devuelve nada y deja el
/// diagnóstico bajo el campo (el modal sigue abierto).
#[test]
fn ai_rename_confirm_vacio_no_devuelve_y_deja_diagnostico() {
    let mut app = app();
    app.open_ai_rename();
    assert!(app.ai_rename_confirm().is_none());
    assert!(matches!(
        &app.modal,
        Some(Modal::AiRenameInstruction { error: Some(_), .. })
    ));
}

/// M4-IA: `AiRenamePlan` es una superficie de decisión sobre contenido
/// iniciado y REVISADO por el humano — usa `ALLOW_CONFIRM` (Enter confirma,
/// como un delete), NO el allowlist de aprobación de agentes: que
/// `dialog.confirm` confirme aquí es exactamente lo que `ALLOW_APPROVAL`
/// prohíbe (pin `aprobacion_ignora_confirm`), y los comandos de colisión
/// son inertes. El prompt de instrucción es texto libre: jamás pasa por
/// `dialog_action`.
#[test]
fn plan_ia_confirma_como_confirmacion_no_como_aprobacion_de_agente() {
    for cmd in ["dialog.confirm", "dialog.approve"] {
        assert_eq!(
            dialog_action(&ai_plan(), cmd),
            Some(DialogOutcome::Confirmed),
            "comando {cmd}"
        );
    }
    for cmd in ["dialog.cancel", "dialog.deny"] {
        assert_eq!(
            dialog_action(&ai_plan(), cmd),
            Some(DialogOutcome::Cancelled),
            "comando {cmd}"
        );
    }
    // Fuera del allowlist de ESTE modal: inerte. up/down INCLUIDOS (audit
    // MAJOR-3): el scroll de la ventana lo enruta el run loop, jamás es un
    // desenlace — scrollear no confirma ni cancela.
    assert_eq!(dialog_action(&ai_plan(), "dialog.overwrite"), None);
    assert_eq!(dialog_action(&ai_plan(), "dialog.up"), None);
    assert_eq!(dialog_action(&ai_plan(), "dialog.down"), None);
    // Texto libre (como Mkdir/MarkPattern): el run loop lo intercepta ANTES.
    let prompt = Modal::AiRenameInstruction {
        instruction: String::new(),
        error: None,
    };
    assert_eq!(dialog_action(&prompt, "dialog.confirm"), None);
    assert_eq!(dialog_action(&prompt, "dialog.approve"), None);
}

/// Audit MAJOR-3: el scroll del plan clampa la ventana a `[0, len - 5]`
/// (jamás pasa de largo ni se hace negativo) y avanza/retrocede de una en
/// una con numeración estable.
#[test]
fn scroll_del_plan_clampa_en_ambos_extremos() {
    let offset_de = |app: &norte_tui::app::App| match &app.modal {
        Some(Modal::AiRenamePlan { offset, .. }) => *offset,
        other => panic!("modal inesperado: {other:?}"),
    };
    let mut app = app();
    app.modal = Some(Modal::AiRenamePlan {
        dir: vp("file:///x"),
        entries: (1..=7)
            .map(|i| norte_proto::methods::AiRenameEntry {
                from: format!("f{i}"),
                to: format!("t{i}"),
            })
            .collect(),
        offset: 0,
    });
    app.ai_plan_scroll(false);
    assert_eq!(offset_de(&app), 0, "no retrocede bajo cero");
    for _ in 0..10 {
        app.ai_plan_scroll(true);
    }
    assert_eq!(offset_de(&app), 2, "clampa en len - ventana (7 - 5)");
    app.ai_plan_scroll(false);
    assert_eq!(offset_de(&app), 1);
}

/// Las aprobaciones hacen cola como las colisiones (jamás pisan un modal
/// abierto) y tienen PRIORIDAD sobre ellas: una aprobación vence por TTL en
/// el daemon; una colisión espera lo que haga falta.
#[test]
fn las_aprobaciones_hacen_cola_con_prioridad_sobre_colisiones() {
    use norte_tui::app::{App, Pane};
    let dir = vp("file:///x");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );
    let Modal::ApproveAgentOp { req } = approval() else {
        unreachable!()
    };
    app.modal = Some(confirm());
    app.pending_collisions.push_back(retry());
    app.pending_approvals.push_back(req);

    // Con un modal abierto, nada cambia.
    app.open_next_pending();
    assert_eq!(app.modal, Some(confirm()), "el modal abierto no se pisa");

    // Al cerrarse, la APROBACIÓN sale antes que la colisión encolada primero.
    app.modal = None;
    app.open_next_pending();
    assert!(matches!(app.modal, Some(Modal::ApproveAgentOp { .. })));
    app.modal = None;
    app.open_next_pending();
    assert!(matches!(app.modal, Some(Modal::Collision { .. })));
    app.modal = None;
    app.open_next_pending();
    assert_eq!(app.modal, None, "colas vacías");
}

#[test]
fn las_colisiones_hacen_cola_y_jamas_pisan_un_modal() {
    use norte_tui::app::{App, Pane};
    let dir = vp("file:///x");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );
    app.modal = Some(confirm());
    app.pending_collisions.push_back(retry());
    app.pending_collisions.push_back(retry());

    // Con un modal abierto, nada cambia.
    app.open_next_collision();
    assert_eq!(app.modal, Some(confirm()), "el modal abierto no se pisa");

    // Al cerrarse, las colisiones salen en orden.
    app.modal = None;
    app.open_next_collision();
    assert!(matches!(app.modal, Some(Modal::Collision { .. })));
    app.modal = None;
    app.open_next_collision();
    assert!(matches!(app.modal, Some(Modal::Collision { .. })));
    app.modal = None;
    app.open_next_collision();
    assert_eq!(app.modal, None, "cola vacía");
}

/// Hits semánticos de fixture (M4-IA-2): `n` paths distintos, score
/// descendente.
fn semantic_hits(n: u16) -> Modal {
    Modal::SemanticHits {
        hits: (1..=n)
            .map(|i| norte_proto::methods::SemanticHit {
                path: vp(&format!("file:///d/f{i}")),
                score: 1.0 - f64::from(i) / 100.0,
            })
            .collect(),
        offset: 0,
        cursor: 0,
    }
}

/// M4-IA-2: el prompt de consulta sigue la disciplina del de instrucción IA
/// — confirmar NO cierra (devuelve la consulta trimmed); solo
/// `semantic_submitted` cierra tras spawnear.
#[test]
fn semantic_query_modal_edita_y_confirma() {
    let mut app = app();
    app.open_semantic_search();
    for c in "facturas 2024".chars() {
        app.semantic_push(c);
    }
    assert_eq!(app.semantic_confirm().as_deref(), Some("facturas 2024"));
    assert!(
        matches!(app.modal, Some(Modal::SemanticQuery { .. })),
        "confirmar no cierra: cierra el submit"
    );
    app.semantic_submitted();
    assert!(app.modal.is_none());
}

/// M4-IA-2: confirmar con la consulta vacía no devuelve nada y deja el
/// diagnóstico bajo el campo (el modal sigue abierto); `semantic_set_error`
/// conserva lo tecleado.
#[test]
fn semantic_query_vacia_no_confirma() {
    let mut app = app();
    app.open_semantic_search();
    assert!(app.semantic_confirm().is_none());
    assert!(matches!(
        &app.modal,
        Some(Modal::SemanticQuery { error: Some(_), .. })
    ));
    app.semantic_push('q');
    app.semantic_set_error("boom".into());
    match &app.modal {
        Some(Modal::SemanticQuery { query, error }) => {
            assert_eq!(query, "q");
            assert_eq!(error.as_deref(), Some("boom"));
        }
        other => panic!("modal inesperado: {other:?}"),
    }
}

/// M4-IA-2: el cursor de hits clampa en ambos extremos y la VENTANA le
/// sigue (baja al pasar del borde inferior, sube al pasar del superior).
#[test]
fn semantic_hits_cursor_scroll_clampa() {
    let estado = |app: &norte_tui::app::App| match &app.modal {
        Some(Modal::SemanticHits { offset, cursor, .. }) => (*offset, *cursor),
        other => panic!("modal inesperado: {other:?}"),
    };
    let mut app = app();
    app.modal = Some(semantic_hits(12));
    app.semantic_cursor(false);
    assert_eq!(estado(&app), (0, 0), "no retrocede bajo cero");
    for _ in 0..99 {
        app.semantic_cursor(true);
    }
    assert_eq!(
        estado(&app),
        (2, 11),
        "cursor clampa en len-1 y la ventana lo sigue (12 - 10)"
    );
    for _ in 0..99 {
        app.semantic_cursor(false);
    }
    assert_eq!(estado(&app), (0, 0), "la ventana vuelve a subir con él");
}

/// M4-IA-2: `SemanticHits` confirma como decisión (`ALLOW_CONFIRM`, Enter
/// navega) y up/down son INERTES para `dialog_action` (el run loop enruta el
/// cursor, jamás es un desenlace); el prompt de consulta es texto libre y
/// nunca pasa por aquí.
#[test]
fn semantic_hits_confirma_como_decision_y_cursor_es_inerte() {
    for cmd in ["dialog.confirm", "dialog.approve"] {
        assert_eq!(
            dialog_action(&semantic_hits(1), cmd),
            Some(DialogOutcome::Confirmed),
            "comando {cmd}"
        );
    }
    for cmd in ["dialog.cancel", "dialog.deny"] {
        assert_eq!(
            dialog_action(&semantic_hits(1), cmd),
            Some(DialogOutcome::Cancelled),
            "comando {cmd}"
        );
    }
    assert_eq!(dialog_action(&semantic_hits(1), "dialog.up"), None);
    assert_eq!(dialog_action(&semantic_hits(1), "dialog.down"), None);
    let prompt = Modal::SemanticQuery {
        query: String::new(),
        error: None,
    };
    assert_eq!(dialog_action(&prompt, "dialog.confirm"), None);
    assert_eq!(dialog_action(&prompt, "dialog.approve"), None);
}
