//! Tests del estado modal (fase 5): confirmaciones y diálogo de colisión.
//! Teclas de diálogo HARDCODEADAS (semántica del diálogo, no keymap).

use crossterm::event::KeyCode;
use norte_core::TransferOptions;
use norte_proto::{CollisionPolicy, VPath};
use norte_tui::app::{DialogOutcome, Modal, TransferKind, dialog_key};
use norte_tui::tasks::RetrySpec;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

fn confirm() -> Modal {
    Modal::ConfirmTransfer {
        kind: TransferKind::Copy,
        from: vp("file:///a"),
        to: vp("file:///b"),
    }
}

fn retry() -> RetrySpec {
    RetrySpec {
        kind: TransferKind::Move,
        from: vp("file:///a"),
        to: vp("file:///b"),
        opts: TransferOptions::default(),
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

#[test]
fn confirmacion_acepta_y_cancela() {
    for code in [KeyCode::Enter, KeyCode::Char('y')] {
        assert_eq!(dialog_key(&confirm(), code), DialogOutcome::Confirmed);
    }
    for code in [KeyCode::Esc, KeyCode::Char('n')] {
        assert_eq!(dialog_key(&confirm(), code), DialogOutcome::Cancelled);
    }
    // Tecla irrelevante: sigue abierto.
    assert_eq!(
        dialog_key(&confirm(), KeyCode::Char('z')),
        DialogOutcome::Open
    );
    let borrar = Modal::ConfirmDelete {
        target: vp("file:///x"),
        permanent: false,
    };
    assert_eq!(
        dialog_key(&borrar, KeyCode::Enter),
        DialogOutcome::Confirmed
    );
    assert_eq!(dialog_key(&borrar, KeyCode::Esc), DialogOutcome::Cancelled);
}

#[test]
fn colision_elige_politica_o_cancela() {
    let casos = [
        ('o', CollisionPolicy::Overwrite),
        ('s', CollisionPolicy::Skip),
        ('r', CollisionPolicy::RenameAuto),
        ('n', CollisionPolicy::Newer),
    ];
    for (tecla, policy) in casos {
        assert_eq!(
            dialog_key(&collision(), KeyCode::Char(tecla)),
            DialogOutcome::Retry(policy),
            "tecla {tecla}"
        );
    }
    assert_eq!(
        dialog_key(&collision(), KeyCode::Esc),
        DialogOutcome::Cancelled
    );
    // Enter NO confirma una colisión (no hay default peligroso).
    assert_eq!(
        dialog_key(&collision(), KeyCode::Enter),
        DialogOutcome::Open
    );
}

/// Aprobación de agente (M3-3b T5): `y` aprueba, `n`/Esc DENIEGAN (cerrar es
/// denegar, fail-safe) y Enter NO aprueba — aprobar una mutación de agente no
/// es respuesta inocua que merezca default (mismo principio que la colisión).
#[test]
fn aprobacion_aprueba_con_y_deniega_con_n_esc_y_enter_no_es_default() {
    assert_eq!(
        dialog_key(&approval(), KeyCode::Char('y')),
        DialogOutcome::Confirmed
    );
    for code in [KeyCode::Char('n'), KeyCode::Esc] {
        assert_eq!(dialog_key(&approval(), code), DialogOutcome::Cancelled);
    }
    for code in [KeyCode::Enter, KeyCode::Char('z')] {
        assert_eq!(dialog_key(&approval(), code), DialogOutcome::Open);
    }
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
