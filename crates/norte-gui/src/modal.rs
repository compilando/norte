//! Modales de confirmación y resolución de conflicto (máquina de estado PURA,
//! testeable sin GPUI). El handler de `main.rs` enruta la tecla aquí con
//! [`on_key`] y actúa sobre el [`ModalOutcome`]; el render pinta el [`Modal`].
//!
//! Aquí también viven los tipos de OPERACIÓN mutante ([`TransferKind`],
//! [`PendingOp`]) que `session.rs` ejecuta: un modal confirmado produce
//! `PendingOp`s que la GUI manda por el canal de comandos.

use norte_core::TransferOptions;
use norte_proto::{CollisionPolicy, ConflictKind, DeleteMode, VPath};

/// Parejas del plan IA visibles a la vez en [`Modal::AiRenamePlan`] (ventana
/// de scroll — paridad con `norte_tui::app::AI_RENAME_PAIR_LIMIT`, audit
/// MAJOR-3: el plan ENTERO es revisable por scroll, jamás se aplica una cola
/// invisible). Única fuente para el render (`modal_lines` en `main.rs`) y el
/// clamp del scroll de [`on_key`].
pub const AI_RENAME_PAIR_LIMIT: usize = 5;

/// Tope de caracteres de la instrucción de [`Modal::AiRenamePrompt`] (molde
/// TUI `MARK_PATTERN_MAX_CHARS`): un paste accidental no desborda el modal;
/// el límite REAL (4 KiB) lo pone el daemon.
pub const AI_INSTRUCTION_MAX_CHARS: usize = 256;

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
    /// Confirmar `app.quit` habiendo trabajo pendiente (revisión C2/G0
    /// IMPORTANT 3): tasks visibles en la franja y/o marcas activas se
    /// perderían de la vista (las tasks siguen en el daemon; las marcas son
    /// solo de sesión) si se cierra sin avisar. Contadores puramente
    /// informativos (para el título del modal, ver `modal_lines` en
    /// `main.rs`) — `on_key` no los necesita.
    ConfirmQuit {
        /// Tasks visibles en la franja (`task_progress.len()`).
        tasks: usize,
        /// Suma de marcas activas en ambos panes.
        marks: usize,
    },
    /// Prompt de instrucción del rename IA (M4-IA). Query en BYTES (molde
    /// `PaletteView`): push/backspace UTF-8-boundary-aware; se pinta
    /// enmascarada (`modal_lines`), jamás cruda.
    AiRenamePrompt {
        /// Dir del pane activo al abrir (viaja al daemon y luego al plan).
        dir: VPath,
        /// La instrucción tecleada hasta ahora, en bytes UTF-8.
        query: Vec<u8>,
    },
    /// Plan de rename IA revisable (M4-IA): superficie de DECISIÓN —
    /// `y`/`enter` aplica (tras el cinturón [`validate_ai_plan`]), `n`/Esc
    /// descarta, `up`/`down` desplazan la ventana de
    /// [`AI_RENAME_PAIR_LIMIT`] parejas.
    AiRenamePlan {
        /// Dir sobre el que se aplican los moves.
        dir: VPath,
        /// Parejas from→to del modelo (proto; UTF-8 garantizado por el
        /// engine, pero se re-valida y se pinta a la defensiva igual).
        entries: Vec<norte_proto::methods::AiRenameEntry>,
        /// Primera pareja visible de la ventana de scroll (paridad TUI
        /// audit MAJOR-3: sin esto, la cola de un plan largo se aplicaría
        /// sin poder verse).
        offset: usize,
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
    /// Confirma `app.quit` (revisión C2/G0 IMPORTANT 3): el caller (`main.rs`)
    /// debe cerrar la ventana (`cx.quit()`) — este módulo es puro y no puede
    /// hacerlo por sí mismo.
    Quit,
    /// Enter en [`Modal::AiRenamePrompt`] con instrucción no vacía: el caller
    /// cierra el modal, manda `SessionCmd::AiRenamePlan` y avisa en el banner
    /// (`msg-ai-rename-running`).
    RequestAiPlan {
        /// Dir del prompt (viaja tal cual a la sesión).
        dir: VPath,
        /// Instrucción ya recortada (trim), no vacía.
        instruction: String,
    },
    /// El plan falló el cinturón [`validate_ai_plan`] (paridad TUI audit
    /// MAJOR-2): un daemon hostil/roto mandó un segmento inválido — el caller
    /// cierra el modal SIN someter NADA y avisa (`msg-ai-rename-invalid-plan`).
    InvalidPlan,
}

/// Destino absoluto de un item copiado/movido a `to_dir`: `to_dir` + nombre del
/// origen. `None` si el origen no tiene nombre (raíz — no debería marcarse).
#[must_use]
pub fn dest_for(to_dir: &VPath, item: &VPath) -> Option<VPath> {
    item.file_name().map(|n| to_dir.join(n.clone()))
}

/// Valida TODAS las parejas del plan como [`norte_proto::Segment`] (cinturón
/// fail-loud, paridad con `norte_tui::main::validate_ai_plan`, audit
/// MAJOR-2): un plan bien formado del engine JAMÁS trae un segmento inválido
/// (el daemon los validó al armarlo), así que UN rechazo aquí delata un
/// daemon hostil/roto — `None` aborta el lote ENTERO, jamás un skip
/// silencioso que aplique "lo demás" de un plan adulterado.
#[must_use]
pub fn validate_ai_plan(
    entries: &[norte_proto::methods::AiRenameEntry],
) -> Option<Vec<(norte_proto::Segment, norte_proto::Segment)>> {
    entries
        .iter()
        .map(|e| {
            Some((
                norte_proto::Segment::new(e.from.as_bytes().to_vec()).ok()?,
                norte_proto::Segment::new(e.to.as_bytes().to_vec()).ok()?,
            ))
        })
        .collect()
}

/// El carácter único que teclea `key` — copia local de
/// `palette_view::typed_char` (misma justificación que la de ese fichero: un
/// helper puro de 4 líneas no amerita superficie `pub(crate)` compartida).
fn typed_char(key: &str, key_char: Option<&str>) -> Option<char> {
    if key == "space" {
        return Some(' ');
    }
    single_char(key_char).or_else(|| single_char(Some(key)))
}

fn single_char(s: Option<&str>) -> Option<char> {
    let s = s?;
    let mut it = s.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// Enruta una tecla (nombre GPUI) al modal, mutándolo si hace falta. `main.rs`
/// llama a esto cuando hay un modal abierto, ANTES del mapeo de navegación.
/// `key_char` solo lo consume [`Modal::AiRenamePrompt`] (tecleo libre, molde
/// `PaletteView`); el caller ya lo anula bajo ctrl/alt/platform.
pub fn on_key(modal: &mut Modal, key: &str, key_char: Option<&str>) -> ModalOutcome {
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
        // Los contadores son solo para el título (main.rs); "y" no los
        // necesita — confirma sin importar cuántos sean.
        Modal::ConfirmQuit { .. } => match key {
            "y" => ModalOutcome::Quit,
            "n" | "escape" => ModalOutcome::Dismiss,
            _ => ModalOutcome::Ignored,
        },
        Modal::AiRenamePrompt { dir, query } => match key {
            "enter" => {
                // La query es UTF-8 por construcción (`push_char`); lossy es
                // cinturón por si alguien construyera el modal con bytes
                // arbitrarios (el test hostil lo hace a propósito).
                let instruction = String::from_utf8_lossy(query).trim().to_owned();
                if instruction.is_empty() {
                    return ModalOutcome::StayOpen;
                }
                ModalOutcome::RequestAiPlan {
                    dir: dir.clone(),
                    instruction,
                }
            }
            "escape" => ModalOutcome::Dismiss,
            "backspace" => {
                // Retira el último carácter UTF-8 COMPLETO (molde
                // `PaletteView::backspace`), jamás un byte suelto.
                if query.is_empty() {
                    return ModalOutcome::Ignored;
                }
                let mut cut = query.len() - 1;
                while cut > 0 && (query[cut] & 0b1100_0000) == 0b1000_0000 {
                    cut -= 1;
                }
                query.truncate(cut);
                ModalOutcome::StayOpen
            }
            _ => {
                if let Some(c) = typed_char(key, key_char) {
                    if c.is_control()
                        || String::from_utf8_lossy(query).chars().count()
                            >= AI_INSTRUCTION_MAX_CHARS
                    {
                        return ModalOutcome::Ignored;
                    }
                    let mut buf = [0u8; 4];
                    query.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                    ModalOutcome::StayOpen
                } else {
                    ModalOutcome::Ignored
                }
            }
        },
        Modal::AiRenamePlan {
            dir,
            entries,
            offset,
        } => match key {
            // La convención "Enter jamás confirma" aplica a modales de
            // CONSENTIMIENTO/destructivos (aprobación de agente, quit); este
            // es un plan REVISADO por el humano — `y/Enter` aplica, igual
            // que la TUI (`modal-ai-rename-plan-hint`).
            "y" | "enter" => {
                // Cinturón fail-loud (paridad TUI audit MAJOR-2): TODAS las
                // parejas se validan ANTES de someter la primera.
                let Some(pairs) = validate_ai_plan(entries) else {
                    return ModalOutcome::InvalidPlan;
                };
                let ops = pairs
                    .into_iter()
                    .map(|(from, to)| PendingOp::Transfer {
                        kind: TransferKind::Move,
                        from: dir.join(from),
                        to: dir.join(to),
                        opts: TransferOptions::default(),
                    })
                    .collect();
                ModalOutcome::Submit(ops)
            }
            "n" | "escape" => ModalOutcome::Dismiss,
            "down" | "up" => {
                // El scroll JAMÁS confirma ni cancela (paridad TUI
                // `ai_plan_scroll`): ventana clampada a [0, len - ventana].
                let max = entries.len().saturating_sub(AI_RENAME_PAIR_LIMIT);
                *offset = if key == "down" {
                    (*offset + 1).min(max)
                } else {
                    offset.saturating_sub(1)
                };
                ModalOutcome::StayOpen
            }
            _ => ModalOutcome::Ignored,
        },
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
    fn dest_for_no_pliega_nfd_a_nfc() {
        let src = VPath::parse("mem:///")
            .unwrap()
            .join(norte_proto::Segment::new(vec![0x65, 0xCC, 0x81]).unwrap());
        let dest = dest_for(&vp("mem:///dst"), &src).expect("tiene nombre");
        assert_eq!(dest.file_name().unwrap().as_bytes(), &[0x65, 0xCC, 0x81]);
    }

    #[test]
    fn confirm_transfer_y_produce_una_op_por_item() {
        let mut m = Modal::ConfirmTransfer {
            kind: TransferKind::Copy,
            items: vec![vp("mem:///src/a"), vp("mem:///src/b")],
            to: vp("mem:///dst"),
        };
        let out = on_key(&mut m, "y", None);
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
        assert_eq!(on_key(&mut m, "n", None), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "escape", None), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "x", None), ModalOutcome::Ignored);
    }

    #[test]
    fn confirm_delete_togglea_permanent_y_confirma_con_el_modo() {
        let mut m = Modal::ConfirmDelete {
            items: vec![vp("mem:///a")],
            permanent: false,
        };
        assert_eq!(on_key(&mut m, "p", None), ModalOutcome::StayOpen);
        assert_eq!(
            on_key(&mut m, "y", None),
            ModalOutcome::Submit(vec![PendingOp::Delete {
                path: vp("mem:///a"),
                mode: DeleteMode::Permanent,
            }])
        );
        // "tab" es alias de "p" para el toggle.
        assert_eq!(on_key(&mut m, "tab", None), ModalOutcome::StayOpen);
        // "n"/"escape" descartan sin importar el estado de `permanent`.
        assert_eq!(on_key(&mut m, "n", None), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "escape", None), ModalOutcome::Dismiss);
    }

    #[test]
    fn confirm_delete_default_es_papelera() {
        let mut m = Modal::ConfirmDelete {
            items: vec![vp("mem:///a")],
            permanent: false,
        };
        assert_eq!(
            on_key(&mut m, "y", None),
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
        let out = on_key(&mut m, "o", None);
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
        let out = on_key(&mut m, "s", None);
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
        assert_eq!(on_key(&mut m, "c", None), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "escape", None), ModalOutcome::Dismiss);
    }

    /// Revisión C2/G0 IMPORTANT 3: "y" confirma la salida (el caller en
    /// `main.rs` hace `cx.quit()` al ver `ModalOutcome::Quit` — este módulo
    /// no puede tocar GPUI); "n"/Esc cancelan; cualquier otra tecla se
    /// ignora (el modal sigue abierto).
    #[test]
    fn confirm_quit_y_confirma_n_o_escape_cancelan() {
        let mut m = Modal::ConfirmQuit { tasks: 2, marks: 1 };
        assert_eq!(on_key(&mut m, "y", None), ModalOutcome::Quit);
        assert_eq!(on_key(&mut m, "n", None), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "escape", None), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "x", None), ModalOutcome::Ignored);
        // Pin explícito de la convención del modal de aprobación: Enter
        // JAMÁS confirma una acción destructiva/consentimiento.
        assert_eq!(on_key(&mut m, "enter", None), ModalOutcome::Ignored);
    }

    /// M4-IA: Enter en el prompt con texto pide el plan (instrucción
    /// recortada), vacío/espacios se queda abierto sin pedir nada, y Esc
    /// descarta.
    #[test]
    fn ai_prompt_enter_pide_plan_y_esc_descarta() {
        let mut m = Modal::AiRenamePrompt {
            dir: vp("mem:///docs"),
            query: Vec::new(),
        };
        // Vacío: StayOpen (nada viaja al daemon).
        assert_eq!(on_key(&mut m, "enter", None), ModalOutcome::StayOpen);
        // Solo espacios: sigue vacío tras el trim.
        assert_eq!(on_key(&mut m, "space", None), ModalOutcome::StayOpen);
        assert_eq!(on_key(&mut m, "enter", None), ModalOutcome::StayOpen);
        for c in "kebab".chars() {
            let buf = c.to_string();
            assert_eq!(
                on_key(&mut m, &buf, Some(&buf)),
                ModalOutcome::StayOpen,
                "teclear {c:?} debe quedarse abierto"
            );
        }
        assert_eq!(
            on_key(&mut m, "enter", None),
            ModalOutcome::RequestAiPlan {
                dir: vp("mem:///docs"),
                instruction: "kebab".into(),
            }
        );
        assert_eq!(on_key(&mut m, "escape", None), ModalOutcome::Dismiss);
    }

    /// M4-IA: backspace retira el último carácter UTF-8 COMPLETO (jamás un
    /// byte suelto — molde `PaletteView::backspace`).
    #[test]
    fn ai_prompt_backspace_respeta_fronteras_utf8() {
        let mut m = Modal::AiRenamePrompt {
            dir: vp("mem:///d"),
            query: Vec::new(),
        };
        assert_eq!(on_key(&mut m, "a", Some("a")), ModalOutcome::StayOpen);
        assert_eq!(on_key(&mut m, "ñ", Some("ñ")), ModalOutcome::StayOpen);
        assert_eq!(on_key(&mut m, "backspace", None), ModalOutcome::StayOpen);
        let Modal::AiRenamePrompt { query, .. } = &m else {
            unreachable!()
        };
        assert_eq!(query, b"a", "la ñ (2 bytes) se retiró entera");
    }

    /// M4-IA: `y` aplica el plan como moves `dir/from → dir/to` (opciones
    /// default) y `n` descarta.
    #[test]
    fn ai_plan_y_aplica_como_moves_y_n_descarta() {
        let entry = norte_proto::methods::AiRenameEntry {
            from: "a.txt".into(),
            to: "b.txt".into(),
        };
        let mut m = Modal::AiRenamePlan {
            dir: vp("mem:///docs"),
            entries: vec![entry],
            offset: 0,
        };
        assert_eq!(
            on_key(&mut m, "y", None),
            ModalOutcome::Submit(vec![PendingOp::Transfer {
                kind: TransferKind::Move,
                from: vp("mem:///docs/a.txt"),
                to: vp("mem:///docs/b.txt"),
                opts: TransferOptions::default(),
            }])
        );
        assert_eq!(on_key(&mut m, "n", None), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "escape", None), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "x", None), ModalOutcome::Ignored);
    }

    /// M4-IA cinturón fail-loud (paridad TUI audit MAJOR-2): UNA pareja
    /// inválida (aquí un `to` con `/`) aborta el plan ENTERO — `InvalidPlan`,
    /// cero ops — aunque el resto de parejas fuera legítimo.
    #[test]
    fn ai_plan_invalido_no_somete_nada() {
        let ok = norte_proto::methods::AiRenameEntry {
            from: "a.txt".into(),
            to: "b.txt".into(),
        };
        let evil = norte_proto::methods::AiRenameEntry {
            from: "c.txt".into(),
            to: "../evil".into(),
        };
        for entries in [vec![evil.clone()], vec![ok.clone(), evil.clone()]] {
            let mut m = Modal::AiRenamePlan {
                dir: vp("mem:///docs"),
                entries,
                offset: 0,
            };
            assert_eq!(on_key(&mut m, "y", None), ModalOutcome::InvalidPlan);
            assert_eq!(on_key(&mut m, "enter", None), ModalOutcome::InvalidPlan);
        }
    }

    /// M4-IA scroll (paridad TUI audit MAJOR-3): `down`/`up` desplazan la
    /// ventana clampada a `[0, len - AI_RENAME_PAIR_LIMIT]` y JAMÁS
    /// confirman ni cancelan.
    #[test]
    fn ai_plan_scroll_clampa_y_no_decide() {
        let entries: Vec<_> = (0..7)
            .map(|i| norte_proto::methods::AiRenameEntry {
                from: format!("f{i}.txt"),
                to: format!("t{i}.txt"),
            })
            .collect();
        let mut m = Modal::AiRenamePlan {
            dir: vp("mem:///d"),
            entries,
            offset: 0,
        };
        for expected in [1, 2, 2, 2] {
            assert_eq!(on_key(&mut m, "down", None), ModalOutcome::StayOpen);
            let Modal::AiRenamePlan { offset, .. } = &m else {
                unreachable!()
            };
            assert_eq!(*offset, expected, "clamp en len - ventana = 2");
        }
        for expected in [1, 0, 0] {
            assert_eq!(on_key(&mut m, "up", None), ModalOutcome::StayOpen);
            let Modal::AiRenamePlan { offset, .. } = &m else {
                unreachable!()
            };
            assert_eq!(*offset, expected);
        }
    }
}
