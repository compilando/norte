//! Modales de confirmación y resolución de conflicto (máquina de estado PURA,
//! testeable sin GPUI). El handler de `main.rs` enruta la tecla aquí con
//! [`on_key`] y actúa sobre el [`ModalOutcome`]; el render pinta el [`Modal`].
//!
//! Aquí también viven los tipos de OPERACIÓN mutante ([`TransferKind`],
//! [`PendingOp`]) que `session.rs` ejecuta: un modal confirmado produce
//! `PendingOp`s que la GUI manda por el canal de comandos.

use norte_core::TransferOptions;
use norte_proto::{CollisionPolicy, ConflictKind, DeleteMode, VPath};

/// Copia o movimiento (la clase de una transferencia).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    /// Copia (origen intacto).
    Copy,
    /// Movimiento (origen desaparece al terminar).
    Move,
}

/// Una operación mutante concreta lista para mandar al daemon.
#[derive(Debug, Clone, PartialEq)]
pub enum PendingOp {
    /// Copia/movimiento de `from` a `to` (ambos ABSOLUTOS; `to` ya incluye el
    /// nombre destino) con la política de colisión de `opts`.
    Transfer {
        /// Copia o movimiento.
        kind: TransferKind,
        /// Origen (path absoluto de la entrada).
        from: VPath,
        /// Destino absoluto (dir destino + nombre del origen).
        to: VPath,
        /// Opciones (colisión, symlinks, resume).
        opts: TransferOptions,
    },
    /// Borrado de `path` (papelera o permanente).
    Delete {
        /// Entrada a borrar.
        path: VPath,
        /// Papelera (default) o permanente.
        mode: DeleteMode,
    },
}

/// Modal activo. Uno a la vez; el render lo pinta como overlay.
#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    /// Confirmar copia/movimiento de `items` al dir `to`.
    ConfirmTransfer {
        /// Copia o movimiento.
        kind: TransferKind,
        /// Orígenes (paths absolutos; marcas o el target del cursor).
        items: Vec<VPath>,
        /// DIRECTORIO destino (el `dir` del pane inactivo).
        to: VPath,
    },
    /// Confirmar borrado de `items`; `permanent` toggleable.
    ConfirmDelete {
        /// Entradas a borrar.
        items: Vec<VPath>,
        /// `false` = papelera (default), `true` = permanente.
        permanent: bool,
    },
    /// Un transfer chocó con el destino: reintentar/saltar/cancelar.
    ConflictResolve {
        /// La transferencia que falló (para reemitir).
        pending: PendingTransfer,
        /// Subtipo de conflicto (para pintar el motivo).
        conflict: ConflictKind,
    },
}

/// La transferencia que originó un conflicto (para reemitir con otra política).
#[derive(Debug, Clone, PartialEq)]
pub struct PendingTransfer {
    /// Copia o movimiento.
    pub kind: TransferKind,
    /// Origen.
    pub from: VPath,
    /// Destino absoluto.
    pub to: VPath,
}

/// Qué debe hacer el handler tras una tecla en el modal.
#[derive(Debug, Clone, PartialEq)]
pub enum ModalOutcome {
    /// Tecla irrelevante: no pasa nada (el modal sigue abierto).
    Ignored,
    /// El estado del modal cambió (p. ej. toggle permanent): re-render, sigue abierto.
    StayOpen,
    /// Cierra el modal sin hacer nada.
    Dismiss,
    /// Cierra el modal y manda estas operaciones al daemon.
    Submit(Vec<PendingOp>),
}

/// Destino absoluto de un item copiado/movido a `to_dir`: `to_dir` + nombre del
/// origen. `None` si el origen no tiene nombre (raíz — no debería marcarse).
#[must_use]
pub fn dest_for(to_dir: &VPath, item: &VPath) -> Option<VPath> {
    item.file_name().map(|n| to_dir.join(n.clone()))
}

/// Enruta una tecla (nombre GPUI) al modal, mutándolo si hace falta. `main.rs`
/// llama a esto cuando hay un modal abierto, ANTES del mapeo de navegación.
pub fn on_key(modal: &mut Modal, key: &str) -> ModalOutcome {
    match modal {
        Modal::ConfirmTransfer { kind, items, to } => match key {
            "y" => {
                let ops = items
                    .iter()
                    .filter_map(|from| {
                        dest_for(to, from).map(|dest| PendingOp::Transfer {
                            kind: *kind,
                            from: from.clone(),
                            to: dest,
                            opts: TransferOptions::default(),
                        })
                    })
                    .collect();
                ModalOutcome::Submit(ops)
            }
            "n" | "escape" => ModalOutcome::Dismiss,
            _ => ModalOutcome::Ignored,
        },
        Modal::ConfirmDelete { items, permanent } => match key {
            "y" => {
                let mode = if *permanent {
                    DeleteMode::Permanent
                } else {
                    DeleteMode::Trash
                };
                let ops = items
                    .iter()
                    .map(|path| PendingOp::Delete {
                        path: path.clone(),
                        mode,
                    })
                    .collect();
                ModalOutcome::Submit(ops)
            }
            "p" | "tab" => {
                *permanent = !*permanent;
                ModalOutcome::StayOpen
            }
            "n" | "escape" => ModalOutcome::Dismiss,
            _ => ModalOutcome::Ignored,
        },
        Modal::ConflictResolve { pending, .. } => {
            let policy = match key {
                "o" => CollisionPolicy::Overwrite,
                "s" => CollisionPolicy::Skip,
                "c" | "escape" => return ModalOutcome::Dismiss,
                _ => return ModalOutcome::Ignored,
            };
            ModalOutcome::Submit(vec![PendingOp::Transfer {
                kind: pending.kind,
                from: pending.from.clone(),
                to: pending.to.clone(),
                opts: TransferOptions {
                    on_collision: policy,
                    ..Default::default()
                },
            }])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(s: &str) -> VPath {
        VPath::parse(s).unwrap()
    }

    #[test]
    fn dest_for_une_dir_destino_con_nombre_origen() {
        assert_eq!(
            dest_for(&vp("mem:///dst"), &vp("mem:///src/foo.txt")),
            Some(vp("mem:///dst/foo.txt"))
        );
    }

    #[test]
    fn dest_for_preserva_bytes_no_utf8_del_nombre_origen() {
        // Origen con bytes 0xFF 0xFE en el nombre (inválido como UTF-8).
        let src = vp("mem:///src/%FF%FE.bin");
        let dest = dest_for(&vp("mem:///dst"), &src).expect("tiene nombre");
        assert_eq!(
            dest.file_name().unwrap().as_bytes(),
            &[0xFF, 0xFE, b'.', b'b', b'i', b'n'],
        );
        assert_eq!(dest.parent().as_ref(), Some(&vp("mem:///dst")));
    }

    #[test]
    fn dest_for_raiz_sin_nombre_es_none_sin_panic() {
        assert_eq!(dest_for(&vp("mem:///dst"), &vp("mem:///")), None);
    }

    #[test]
    fn confirm_transfer_y_produce_una_op_por_item() {
        let mut m = Modal::ConfirmTransfer {
            kind: TransferKind::Copy,
            items: vec![vp("mem:///src/a"), vp("mem:///src/b")],
            to: vp("mem:///dst"),
        };
        let out = on_key(&mut m, "y");
        assert_eq!(
            out,
            ModalOutcome::Submit(vec![
                PendingOp::Transfer {
                    kind: TransferKind::Copy,
                    from: vp("mem:///src/a"),
                    to: vp("mem:///dst/a"),
                    opts: TransferOptions::default(),
                },
                PendingOp::Transfer {
                    kind: TransferKind::Copy,
                    from: vp("mem:///src/b"),
                    to: vp("mem:///dst/b"),
                    opts: TransferOptions::default(),
                },
            ])
        );
    }

    #[test]
    fn confirm_transfer_n_o_escape_descartan() {
        let mut m = Modal::ConfirmTransfer {
            kind: TransferKind::Move,
            items: vec![vp("mem:///src/a")],
            to: vp("mem:///dst"),
        };
        assert_eq!(on_key(&mut m, "n"), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "escape"), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "x"), ModalOutcome::Ignored);
    }

    #[test]
    fn confirm_delete_togglea_permanent_y_confirma_con_el_modo() {
        let mut m = Modal::ConfirmDelete {
            items: vec![vp("mem:///a")],
            permanent: false,
        };
        assert_eq!(on_key(&mut m, "p"), ModalOutcome::StayOpen);
        assert_eq!(
            on_key(&mut m, "y"),
            ModalOutcome::Submit(vec![PendingOp::Delete {
                path: vp("mem:///a"),
                mode: DeleteMode::Permanent,
            }])
        );
        // "tab" es alias de "p" para el toggle.
        assert_eq!(on_key(&mut m, "tab"), ModalOutcome::StayOpen);
        // "n"/"escape" descartan sin importar el estado de `permanent`.
        assert_eq!(on_key(&mut m, "n"), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "escape"), ModalOutcome::Dismiss);
    }

    #[test]
    fn confirm_delete_default_es_papelera() {
        let mut m = Modal::ConfirmDelete {
            items: vec![vp("mem:///a")],
            permanent: false,
        };
        assert_eq!(
            on_key(&mut m, "y"),
            ModalOutcome::Submit(vec![PendingOp::Delete {
                path: vp("mem:///a"),
                mode: DeleteMode::Trash,
            }])
        );
    }

    #[test]
    fn conflict_overwrite_reemite_con_overwrite() {
        let mut m = Modal::ConflictResolve {
            pending: PendingTransfer {
                kind: TransferKind::Copy,
                from: vp("mem:///src/a"),
                to: vp("mem:///dst/a"),
            },
            conflict: ConflictKind::Exists,
        };
        let out = on_key(&mut m, "o");
        match out {
            ModalOutcome::Submit(ops) => {
                assert_eq!(ops.len(), 1);
                match &ops[0] {
                    PendingOp::Transfer { opts, .. } => {
                        assert_eq!(opts.on_collision, CollisionPolicy::Overwrite);
                    }
                    other => panic!("esperaba Transfer, vino {other:?}"),
                }
            }
            other => panic!("esperaba Submit, vino {other:?}"),
        }

        // "s" reemite con Skip (cubre el otro brazo de política).
        let out = on_key(&mut m, "s");
        match out {
            ModalOutcome::Submit(ops) => {
                assert_eq!(ops.len(), 1);
                match &ops[0] {
                    PendingOp::Transfer { opts, .. } => {
                        assert_eq!(opts.on_collision, CollisionPolicy::Skip);
                    }
                    other => panic!("esperaba Transfer, vino {other:?}"),
                }
            }
            other => panic!("esperaba Submit, vino {other:?}"),
        }
    }

    #[test]
    fn conflict_cancelar_descarta() {
        let mut m = Modal::ConflictResolve {
            pending: PendingTransfer {
                kind: TransferKind::Copy,
                from: vp("mem:///src/a"),
                to: vp("mem:///dst/a"),
            },
            conflict: ConflictKind::Exists,
        };
        assert_eq!(on_key(&mut m, "c"), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "escape"), ModalOutcome::Dismiss);
    }
}
