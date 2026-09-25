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

use crate::app::{App, Modal, ReportKind};

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
    "dialog.ask-secret",
    "dialog.trust-lua",
    "dialog.plugin-approval",
    "dialog.quit",
    "dialog.mark-pattern",
    "dialog.transfer-name",
    "dialog.transfer-dest",
    "dialog.mkdir",
    "dialog.command-line",
    "dialog.ai-rename",
    "dialog.semantic-search",
    // #139: an entry's properties are "what does the listing tell me about
    // this", which is what the columns page is about — not a decision
    // dialog, so it shares no id with any of the ones that ask something.
    "dialog.properties",
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
        // Uninstalling an extension is a delete that asks: same page as the
        // delete.
        // Undoing up to a point is also a consequence being accepted, so it
        // shares the confirm page.
        Modal::ConfirmDelete { .. }
        | Modal::ConfirmPluginUninstall { .. }
        | Modal::ConfirmUndoAfter { .. }
        | Modal::ConfirmTransfer { .. } => "dialog.confirm",
        Modal::ConfirmQuit => "dialog.quit",
        Modal::Collision { .. } => "dialog.collision",
        Modal::ApproveAgentOp { .. } => "dialog.approval",
        // Granting capabilities does NOT share a page with approving an
        // agent's operation: they are two different things to be careful
        // about, and whoever presses F1 over one must not get prose about the
        // other.
        Modal::ConfirmPluginApproval { .. } => "dialog.plugin-approval",
        Modal::TrustHostKey { .. } => "dialog.trust-host",
        // #325: its OWN id and not the TOFU's, even though today the same
        // page (`remote`, where connections and their secrets live) explains
        // both: they are two different questions — a host key to compare, a
        // password to type — and sharing an id would tie the second to the
        // first's page forever. `F1` over it opens nothing
        // (`help_over_modal_allowed`), so this id is reached today through
        // help's index, not through the key.
        Modal::AskSecret { .. } => "dialog.ask-secret",
        Modal::TrustLuaInit { .. } => "dialog.trust-lua",
        Modal::MarkPattern { .. } => "dialog.mark-pattern",
        Modal::TransferName { .. } => "dialog.transfer-name",
        // Creating a file shares a page with creating a directory, same as in
        // the window: both are the dialog that asks you to type a name, and
        // the corpus has ONE page that talks about that.
        // Saving as a profile shares a page with creating: both are the
        // dialog that asks you to type a name.
        Modal::Mkdir { .. } | Modal::EditNew { .. } | Modal::ProfileSaveAs { .. } => "dialog.mkdir",
        Modal::TransferDest { .. } => "dialog.transfer-dest",
        Modal::CommandLine { .. } => "dialog.command-line",
        // Organize shares a page with the AI-rename plan: it is the same
        // deal — a plan reviewed whole before it applies — with one more
        // freedom, and splitting the prose would force repeating it.
        Modal::AiRenameInstruction { .. }
        | Modal::AiRenamePlan { .. }
        | Modal::OrganizePlan { .. } => "dialog.ai-rename",
        // The batch template shares a page with rename, which is where it is
        // explained what a reviewable plan is and what can be undone.
        Modal::RenameBatchPattern { .. } => "dialog.rename",
        // A report goes to the page of whatever produced it: an undo's, to
        // the confirm-undo page (`ConfirmUndoAfter`'s); a batch's, to the
        // rename page, which explains what can be undone.
        Modal::Report { kind, .. } => match kind {
            ReportKind::Undo => "dialog.confirm",
            ReportKind::Batch => "dialog.rename",
        },
        Modal::SemanticQuery { .. } | Modal::SemanticHits { .. } => "dialog.semantic-search",
        // Checksums share a page with properties: both are READ-ONLY panels
        // about what is under the cursor.
        // And chmod (#314): it is the dialog that CHANGES what that panel
        // shows, and what needs explaining — what an octal mode is, what it
        // applies to — is explained alongside it.
        Modal::Properties { .. } | Modal::Checksums { .. } | Modal::Chmod { .. } => {
            "dialog.properties"
        }
        // #132: the two file-writing dialogs share a page — something is
        // typed and confirmed, and what needs explaining (what format comes
        // from the name, what suffixes the size understands) is the same.
        Modal::Pack { .. } | Modal::Split { .. } => "dialog.archive",
    }
}

/// May `F1` open a help page OVER this modal? (review H3c MINOR-3)
///
/// Exhaustive and wildcard-free for the same reason as `modal_context`: this
/// is a decision about a security-relevant property, and a new modal variant
/// must not be able to inherit an answer nobody chose.
///
/// `false` for the SEVEN variants the run loop intercepts before the `dialog`
/// keymap ever resolves — the six free-text editors (`Modal::Mkdir`,
/// `Modal::MarkPattern`, `Modal::CommandLine`, `Modal::AiRenameInstruction`,
/// `Modal::SemanticQuery`, `Modal::TransferName`) plus the project `init.lua` TOFU
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
/// let typing = norte_tui::app::Modal::Mkdir {
///     name: "new".into(),
///     error: None,
/// };
/// assert!(!help_over_modal_allowed(&typing));
/// assert!(help_over_modal_allowed(&norte_tui::app::Modal::ConfirmQuit));
/// ```
#[must_use]
pub fn help_over_modal_allowed(modal: &Modal) -> bool {
    match modal {
        Modal::TrustLuaInit { .. }
        | Modal::MarkPattern { .. }
        | Modal::Mkdir { .. }
        | Modal::ProfileSaveAs { .. }
        | Modal::EditNew { .. }
        | Modal::TransferDest { .. }
        | Modal::CommandLine { .. }
        | Modal::AiRenameInstruction { .. }
        | Modal::RenameBatchPattern { .. }
        | Modal::SemanticQuery { .. }
        | Modal::TransferName { .. }
        // #132: the two file-writing ones are free-text editors, and the run
        // loop intercepts them before the `dialog` keymap same as the others.
        // `F1` over them would type an f into the name.
        | Modal::Pack { .. }
        | Modal::Split { .. }
        // #314: chmod, for the same reason — `F1` over it would type an f
        // that is not even an octal digit.
        | Modal::Chmod { .. }
        // #325: a PASSWORD is being typed. `F1` over it would write an f
        // inside it, and — worse than the ones above — the field does not
        // show it, so the user would not see the extra character they just
        // put in their credential.
        | Modal::AskSecret { .. } => false,
        Modal::ConfirmDelete { .. }
        | Modal::ConfirmPluginUninstall { .. }
        | Modal::ConfirmUndoAfter { .. }
        | Modal::OrganizePlan { .. }
        | Modal::ConfirmTransfer { .. }
        | Modal::ConfirmQuit
        | Modal::Collision { .. }
        | Modal::ApproveAgentOp { .. }
        | Modal::ConfirmPluginApproval { .. }
        | Modal::TrustHostKey { .. }
        | Modal::AiRenamePlan { .. }
        // #139: it is read, not written — F1 over it steals no key from
        // anyone.
        | Modal::Properties { .. }
        | Modal::Report { .. }
        | Modal::Checksums { .. }
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
        VPath::parse(wire).expect("test wire")
    }

    fn app_in_a_pane() -> App {
        let d = vp("file:///x");
        App::new(
            crate::app::Pane::new(d.clone(), Vec::new()),
            crate::app::Pane::new(d, Vec::new()),
        )
    }

    fn test_collision_modal() -> Modal {
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

    fn test_approval_modal() -> Modal {
        Modal::ApproveAgentOp {
            req: norte_proto::methods::PolicyApprovalRequired {
                approval_id: 7,
                session: Some("s1".into()),
                op: "copy".into(),
                paths: vec!["mem:///proj/a".into(), "mem:///proj/b".into()],
                paths_total: 0,
                ttl_ms: 60_000,
                detail: norte_proto::methods::ApprovalDetail::default(),
            },
        }
    }

    fn test_viewer() -> crate::viewer::Viewer {
        crate::viewer::Viewer::new(vp("file:///x/a.txt"), b"hello\n".to_vec(), false)
    }

    /// One of EACH [`Modal`] variant, to cross-check the map against
    /// [`CONTEXTS`]. It is not exhaustive by compiler (that is
    /// `modal_context`'s `match`): its job is that no id in the vocabulary is
    /// left without a modal that produces it, nor the other way around.
    // A LITERAL list of variants: it grows with the enum, and that is what
    // makes a new modal with no help context a compile failure.
    #[expect(
        clippy::too_many_lines,
        reason = "literal list of variants: a new modal with no help is a compile error"
    )]
    fn one_modal_of_each_variant() -> Vec<Modal> {
        vec![
            Modal::ConfirmDelete {
                items: vec![vp("file:///x/a")],
                permanent: false,
            },
            Modal::ConfirmPluginApproval {
                id: "org.acme.demo".to_owned(),
                name: "Demo".to_owned(),
                name_hostile: false,
                caps: vec![("read files".to_owned(), false)],
                digest: None,
            },
            Modal::ConfirmPluginUninstall {
                id: "org.acme.demo".to_owned(),
                name: "Demo".to_owned(),
                name_hostile: false,
            },
            Modal::ConfirmTransfer {
                kind: TransferKind::Copy,
                items: vec![vp("file:///x/a")],
                to: vp("file:///y"),
                space: None,
                confine: None,
            },
            test_collision_modal(),
            test_approval_modal(),
            Modal::TrustHostKey {
                host: "h".into(),
                port: Some(22),
                algo: "ssh-ed25519".into(),
                fingerprint: "SHA256:AAAA".into(),
                dir: vp("sftp://h/"),
                pane: 0,
                trail: Trail::Record,
            },
            Modal::AskSecret {
                conn: "rosetta".into(),
                endpoint: "s3://s3.eu-west-1.amazonaws.com".into(),
                input: crate::app::TypedSecret::default(),
                dir: vp("s3://bucket/"),
                pane: 0,
                trail: Trail::Record,
            },
            Modal::TrustLuaInit {
                path: "repo/.norte/init.lua".into(),
                hash_abbrev: "ab12cd34ef56ab78ab12cd34ef56ab78".into(),
            },
            Modal::ConfirmQuit,
            Modal::Properties {
                entry: Box::new(norte_proto::Entry {
                    path: norte_proto::VPath::parse("file:///d").expect("vpath"),
                    kind: norte_proto::EntryKind::Dir,
                    size: None,
                    mtime_ms: None,
                    attrs: std::collections::BTreeMap::new(),
                }),
                size_task: None,
                size: None,
            },
            Modal::MarkPattern {
                mark: true,
                pattern: "*.rs".into(),
                error: None,
            },
            Modal::TransferDest {
                kind: TransferKind::Copy,
                input: "file:///y".into(),
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
                space: None,
                confine: None,
            },
            Modal::Mkdir {
                name: "new".into(),
                error: None,
            },
            Modal::CommandLine {
                command: "make test".into(),
                error: None,
            },
            Modal::AiRenameInstruction {
                instruction: "in snake_case".into(),
                error: None,
            },
            Modal::AiRenamePlan {
                dir: vp("file:///x"),
                entries: vec![norte_proto::methods::AiRenameEntry {
                    from: "a".into(),
                    to: "b".into(),
                }],
                offset: 0,
                seen: norte_frontend::AI_RENAME_PAIR_LIMIT,
                plan: norte_frontend::BatchPlan::Pending,
            },
            Modal::SemanticQuery {
                query: "invoices".into(),
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
    fn the_pane_is_the_default_context() {
        assert_eq!(help_context(&app_in_a_pane()), "browse");
    }

    #[test]
    fn a_modal_beats_the_pane_and_each_has_its_own() {
        let mut app = app_in_a_pane();
        app.modal = Some(test_collision_modal());
        assert_eq!(help_context(&app), "dialog.collision");
        app.modal = Some(test_approval_modal());
        assert_eq!(
            help_context(&app),
            "dialog.approval",
            "an agent's approval is not explained by the copy page"
        );
    }

    #[test]
    fn the_viewer_beats_the_pane_and_loses_to_a_modal() {
        // Order matters: what is ON TOP is what the reader is looking at, and
        // that is what they need explained.
        let mut app = app_in_a_pane();
        app.viewer = Some(test_viewer());
        assert_eq!(help_context(&app), "viewer");
        app.modal = Some(test_collision_modal());
        assert_eq!(help_context(&app), "dialog.collision");
    }

    #[test]
    fn the_vocabulary_has_no_duplicates_or_gaps() {
        // A repeated id would make two modals share a page without anyone
        // having decided so; an empty one would open nothing.
        let mut seen = std::collections::BTreeSet::new();
        for id in CONTEXTS {
            assert!(!id.is_empty(), "empty id in the vocabulary");
            assert!(seen.insert(*id), "duplicate id: {id}");
        }
    }

    /// The map and the vocabulary pin each OTHER: a `match` that returns an
    /// id not in [`CONTEXTS`] leaves the documentation gate with nothing to
    /// check (F1 would open the index and nobody would complain), and an id
    /// in `CONTEXTS` that no modal produces makes the gate ask for a page for
    /// a screen that does not exist.
    #[test]
    fn every_dialog_id_is_produced_by_a_modal_and_is_in_the_vocabulary() {
        let mut produced = std::collections::BTreeSet::new();
        for modal in one_modal_of_each_variant() {
            let id = modal_context(&modal);
            assert!(
                CONTEXTS.contains(&id),
                "{id} comes out of the map but is not in CONTEXTS"
            );
            produced.insert(id);
        }
        let declared: std::collections::BTreeSet<&str> = CONTEXTS
            .iter()
            .copied()
            .filter(|id| id.starts_with("dialog."))
            .collect();
        assert_eq!(
            produced, declared,
            "every `dialog.*` id in the vocabulary is produced by some modal, and vice versa"
        );
    }
}
