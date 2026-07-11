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
