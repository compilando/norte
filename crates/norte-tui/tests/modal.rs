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
use norte_tui::app::{DialogOutcome, Modal, TransferKind, dialog_action};
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
