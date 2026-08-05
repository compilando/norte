//! Which help page belongs to where the reader is standing (H3c).
//!
//! The mapping context→page lives in the CORPUS (a topic's `context:` front
//! matter). What lives here is the other half: the closed vocabulary of
//! context ids — which places the app has — and the rule for deciding which
//! one the reader is in right now. The corpus cannot own that half: it does
//! not know what a modal is.
//!
//! The compile anchor below is the point of the module. `modal_context`
//! matches on [`Modal`] exhaustively, with no wildcard arm, so a new modal
//! variant does not compile until someone extends the map — which is the
//! moment to decide which page explains it, rather than months later when a
//! reader presses `F1` over it and gets the index.
//!
//! Variants that ask the same KIND of question may share one id (the delete
//! and the transfer confirmation are one page). That sharing is a decision
//! recorded in the `match`; it is never the residue of a wildcard.

use crate::app::{App, Modal};

/// Every context id the TUI can be in.
///
/// The documentation gate cross-checks this against the corpus in both
/// directions: an id no page claims is a finding, and a page claiming an id
/// that is not here is a finding. That is why the list is closed and hand
/// written — derived from the corpus it would agree with itself by
/// construction and catch nothing.
pub const CONTEXTS: &[&str] = &[
    "browse",
    "viewer",
    "dialog.confirm",
    "dialog.collision",
    "dialog.approval",
    "dialog.trust-host",
    "dialog.trust-lua",
    "dialog.quit",
    "dialog.mark-pattern",
    "dialog.transfer-name",
    "dialog.mkdir",
    "dialog.ai-rename",
    "dialog.semantic-search",
];

/// The context of `modal`.
///
/// Exhaustive on purpose — see the module docs. The groupings are decisions:
///
/// - the delete and the transfer confirmation are the same question ("shall I
///   touch these files?") and share `dialog.confirm`; quitting is NOT that
///   question — it mutates nothing — so it keeps its own id;
/// - the AI rename prompt and its reviewable plan are two steps of one
///   feature, as are the semantic query and its hits: one page each explains
///   both steps, and splitting them would ask the corpus for a page about
///   half a flow.
///
/// The security dialogs never share: a host-key TOFU, a project `init.lua`
/// TOFU and an agent approval are three different things to be careful
/// about, and a reader who presses `F1` over one of them must not be handed
/// prose about another.
fn modal_context(modal: &Modal) -> &'static str {
    match modal {
        Modal::ConfirmDelete { .. } | Modal::ConfirmTransfer { .. } => "dialog.confirm",
        Modal::ConfirmQuit => "dialog.quit",
        Modal::Collision { .. } => "dialog.collision",
        Modal::ApproveAgentOp { .. } => "dialog.approval",
        Modal::TrustHostKey { .. } => "dialog.trust-host",
        Modal::TrustLuaInit { .. } => "dialog.trust-lua",
        Modal::MarkPattern { .. } => "dialog.mark-pattern",
        Modal::TransferName { .. } => "dialog.transfer-name",
        Modal::Mkdir { .. } => "dialog.mkdir",
        Modal::AiRenameInstruction { .. } | Modal::AiRenamePlan { .. } => "dialog.ai-rename",
        Modal::SemanticQuery { .. } | Modal::SemanticHits { .. } => "dialog.semantic-search",
    }
}

/// May `F1` open a help page OVER this modal? (review H3c MINOR-3)
///
/// Exhaustive and wildcard-free for the same reason as `modal_context`: this
/// is a decision about a security-relevant property, and a new modal variant
/// must not be able to inherit an answer nobody chose.
///
/// `false` for the SIX variants the run loop intercepts before the `dialog`
/// keymap ever resolves — the five free-text editors (`Modal::Mkdir`,
/// `Modal::MarkPattern`, `Modal::AiRenameInstruction`, `Modal::SemanticQuery`,
/// `Modal::TransferName`) plus the project `init.lua` TOFU
/// (`Modal::TrustLuaInit`). They were already excluded, but only as the residue
/// of that interception 3000 lines away in `main`: moving one onto the `dialog`
/// keymap — a plausible cleanup — would have opened a help page over a text
/// field the reader is typing into, and over a trust prompt that has NO TTL to
/// bound how long it stays unanswerable.
///
/// The free-text ones cannot resolve `app.help` at all without reinterpreting
/// what is being typed, which is exactly what they avoid; their pages are
/// reachable from the index. `TrustLuaInit` has no `dialog_action` allowlist
/// either (H1 decision 8).
///
/// ```
/// use norte_tui::help_context::help_over_modal_allowed;
/// let escribiendo = norte_tui::app::Modal::Mkdir {
///     name: "nuevo".into(),
///     error: None,
/// };
/// assert!(!help_over_modal_allowed(&escribiendo));
/// assert!(help_over_modal_allowed(&norte_tui::app::Modal::ConfirmQuit));
/// ```
#[must_use]
pub fn help_over_modal_allowed(modal: &Modal) -> bool {
    match modal {
        Modal::TrustLuaInit { .. }
        | Modal::MarkPattern { .. }
        | Modal::Mkdir { .. }
        | Modal::AiRenameInstruction { .. }
        | Modal::SemanticQuery { .. }
        | Modal::TransferName { .. } => false,
        Modal::ConfirmDelete { .. }
        | Modal::ConfirmTransfer { .. }
        | Modal::ConfirmQuit
        | Modal::Collision { .. }
        | Modal::ApproveAgentOp { .. }
        | Modal::TrustHostKey { .. }
        | Modal::AiRenamePlan { .. }
        | Modal::SemanticHits { .. } => true,
    }
}

/// Where the reader is: the topmost thing on screen, because that is what
/// they are looking at and what they need explained.
///
/// A modal beats the viewer and the viewer beats the panes. The other
/// overlays (palette, settings, theme and column pickers, the extension
/// manager) answer `browse` today: the palette gets its own bridge — `F1` on
/// a row opens the page for THAT command, which is a better answer than a
/// page about the palette — and the rest have no page yet.
///
/// The returned id is always one of [`CONTEXTS`].
#[must_use]
pub fn help_context(app: &App) -> &'static str {
    if let Some(modal) = app.modal.as_ref() {
        return modal_context(modal);
    }
    if app.viewer.is_some() {
        return "viewer";
    }
    "browse"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, Modal, Trail, TransferKind};
    use norte_proto::VPath;

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire de test")
    }

    fn app_en_pane() -> App {
        let d = vp("file:///x");
        App::new(
            crate::app::Pane::new(d.clone(), Vec::new()),
            crate::app::Pane::new(d, Vec::new()),
        )
    }

    fn collision_modal_de_test() -> Modal {
        Modal::Collision {
            retry: crate::tasks::RetrySpec {
                kind: TransferKind::Move,
                from: vp("file:///a"),
                to: vp("file:///b"),
                opts: norte_core::TransferOptions::default(),
                name_encoding: None,
            },
        }
    }

    fn approval_modal_de_test() -> Modal {
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

    fn visor_de_test() -> crate::viewer::Viewer {
        crate::viewer::Viewer::new(vp("file:///x/a.txt"), b"hola\n".to_vec(), false)
    }

    /// Uno de CADA variante de [`Modal`], para cruzar el mapa con
    /// [`CONTEXTS`]. No es exhaustiva por compilador (eso lo hace el `match`
    /// de `modal_context`): su trabajo es que ningún id del vocabulario se
    /// quede sin modal que lo produzca, ni al revés.
    fn un_modal_de_cada_variante() -> Vec<Modal> {
        vec![
            Modal::ConfirmDelete {
                items: vec![vp("file:///x/a")],
                permanent: false,
            },
            Modal::ConfirmTransfer {
                kind: TransferKind::Copy,
                items: vec![vp("file:///x/a")],
                to: vp("file:///y"),
            },
            collision_modal_de_test(),
            approval_modal_de_test(),
            Modal::TrustHostKey {
                host: "h".into(),
                port: Some(22),
                algo: "ssh-ed25519".into(),
                fingerprint: "SHA256:AAAA".into(),
                dir: vp("sftp://h/"),
                pane: 0,
                trail: Trail::Record,
            },
            Modal::TrustLuaInit {
                path: "repo/.norte/init.lua".into(),
                hash_abbrev: "ab12cd34ef56ab78ab12cd34ef56ab78".into(),
            },
            Modal::ConfirmQuit,
            Modal::MarkPattern {
                mark: true,
                pattern: "*.rs".into(),
                error: None,
            },
            Modal::TransferName {
                kind: TransferKind::Copy,
                from: vp("file:///x/a"),
                to_dir: vp("file:///y"),
                name: "a".into(),
                original: b"a".to_vec(),
                touched: false,
                from_marks: false,
                enc: None,
                error: None,
            },
            Modal::Mkdir {
                name: "nuevo".into(),
                error: None,
            },
            Modal::AiRenameInstruction {
                instruction: "en snake_case".into(),
                error: None,
            },
            Modal::AiRenamePlan {
                dir: vp("file:///x"),
                entries: vec![norte_proto::methods::AiRenameEntry {
                    from: "a".into(),
                    to: "b".into(),
                }],
                offset: 0,
            },
            Modal::SemanticQuery {
                query: "facturas".into(),
                error: None,
            },
            Modal::SemanticHits {
                hits: vec![norte_proto::methods::SemanticHit {
                    path: vp("file:///x/a"),
                    score: 0.9,
                }],
                offset: 0,
                cursor: 0,
            },
        ]
    }

    #[test]
    fn el_pane_es_el_contexto_por_defecto() {
        assert_eq!(help_context(&app_en_pane()), "browse");
    }

    #[test]
    fn un_modal_gana_al_pane_y_cada_uno_tiene_el_suyo() {
        let mut app = app_en_pane();
        app.modal = Some(collision_modal_de_test());
        assert_eq!(help_context(&app), "dialog.collision");
        app.modal = Some(approval_modal_de_test());
        assert_eq!(
            help_context(&app),
            "dialog.approval",
            "la aprobación de un agente no se explica con la página de copiar"
        );
    }

    #[test]
    fn el_visor_gana_al_pane_y_pierde_contra_un_modal() {
        // El orden importa: lo que está ENCIMA es lo que el lector está
        // mirando, y es de eso de lo que necesita que le hablen.
        let mut app = app_en_pane();
        app.viewer = Some(visor_de_test());
        assert_eq!(help_context(&app), "viewer");
        app.modal = Some(collision_modal_de_test());
        assert_eq!(help_context(&app), "dialog.collision");
    }

    #[test]
    fn el_vocabulario_no_tiene_duplicados_ni_huecos() {
        // Un id repetido haría que dos modales compartieran página sin que
        // nadie lo hubiera decidido; uno vacío abriría la nada.
        let mut vistos = std::collections::BTreeSet::new();
        for id in CONTEXTS {
            assert!(!id.is_empty(), "id vacío en el vocabulario");
            assert!(vistos.insert(*id), "id duplicado: {id}");
        }
    }

    /// El mapa y el vocabulario se pinan MUTUAMENTE: un `match` que devuelva
    /// un id que no está en [`CONTEXTS`] deja a la puerta de documentación
    /// sin nada que comprobar (F1 abriría el índice y nadie se quejaría), y
    /// un id en `CONTEXTS` que ningún modal produce hace que la puerta pida
    /// una página para una pantalla que no existe.
    #[test]
    fn cada_id_de_dialogo_lo_produce_un_modal_y_esta_en_el_vocabulario() {
        let mut producidos = std::collections::BTreeSet::new();
        for modal in un_modal_de_cada_variante() {
            let id = modal_context(&modal);
            assert!(
                CONTEXTS.contains(&id),
                "{id} sale del mapa pero no está en CONTEXTS"
            );
            producidos.insert(id);
        }
        let declarados: std::collections::BTreeSet<&str> = CONTEXTS
            .iter()
            .copied()
            .filter(|id| id.starts_with("dialog."))
            .collect();
        assert_eq!(
            producidos, declarados,
            "todo id `dialog.*` del vocabulario lo produce algún modal, y al revés"
        );
    }
}
