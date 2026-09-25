//! Modal state tests (phase 5) + H1 T2 (issue #24): dialog keys are now
//! RESOLVED against the keymap's `dialog` context and the resulting command
//! is filtered by the specific modal's ALLOWLIST (`app::dialog_action`) —
//! SECURITY semantics (what confirms, what denies, what is inert) live in
//! code; only the key→command assignment is rebindable. `Modal::TrustLuaInit`
//! is the only exception (decision 8 of the H1 plan): it is still resolved
//! with `app::trust_lua_key` over the raw `crossterm::event::KeyCode`.

use norte_core::TransferOptions;
use norte_proto::{CollisionPolicy, VPath};
use norte_tui::app::{DialogOutcome, Modal, Trail, TransferKind, dialog_action};
use norte_tui::keymap::{
    COMMANDS, Chord, Count, DIALOG_COMMANDS, Effective, KeyCode, Mods, Resolution, Resolver,
    Screen, parse_keymap,
};
use norte_tui::tasks::RetrySpec;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

fn confirm() -> Modal {
    Modal::ConfirmTransfer {
        kind: TransferKind::Copy,
        items: vec![vp("file:///a")],
        to: vp("file:///b"),
        space: None,
        confine: None,
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
            paths_total: 0,
            ttl_ms: 60_000,
            detail: norte_proto::methods::ApprovalDetail::default(),
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

fn ask_secret_con(text: &str) -> Modal {
    let mut input = norte_tui::app::TypedSecret::default();
    for c in text.chars() {
        input.push(c);
    }
    Modal::AskSecret {
        conn: "rosetta".into(),
        endpoint: "s3://s3.eu-west-1.amazonaws.com".into(),
        input,
        dir: vp("s3://bucket/"),
        pane: 0,
        trail: Trail::Record,
    }
}

fn ask_secret() -> Modal {
    ask_secret_con("hunter2")
}

/// Safety pin H1 T2: Enter (`dialog.confirm`) is INERT on an agent
/// approval — approving an agent mutation is not a
// TODO(translation): review — source comment was cut off mid-sentence before translation; restore the missing rationale from history if available.
#[test]
fn approval_ignores_confirm() {
    assert_eq!(dialog_action(&approval(), "dialog.confirm"), None);
}

/// Safety pin H1 T2: same principle for the SSH host key TOFU (#45) — Enter
/// never trusts an unverified key.
#[test]
fn trust_host_ignores_confirm() {
    assert_eq!(dialog_action(&trust_host(), "dialog.confirm"), None);
}

/// #325 and the contrast with the one above: the password dialog DOES
/// accept Enter — whoever answers just typed the data — and does NOT
/// accept `dialog.approve`: handing over a password is not approving
/// anything, and offering the approve key here would suggest it is good
/// for that.
#[test]
fn ask_secret_accepts_confirm_and_not_approve() {
    assert_eq!(
        dialog_action(&ask_secret(), "dialog.confirm"),
        Some(DialogOutcome::Confirmed)
    );
    assert_eq!(dialog_action(&ask_secret(), "dialog.approve"), None);
    assert_eq!(dialog_action(&ask_secret(), "dialog.deny"), None);
    assert_eq!(
        dialog_action(&ask_secret(), "dialog.cancel"),
        Some(DialogOutcome::Cancelled)
    );
}

/// #325: with an EMPTY field, confirming is INERT — the dialog stays. Both
/// halves matter: submitting the empty string would reproduce what #320
/// closed (an empty secret leaves the provider picking up credentials from
/// the environment), and closing would force redoing the whole navigation
/// over one extra Enter, which in a field where what is typed is not shown
/// is the easy mistake to make. Cancel is still live: leaving IS an answer.
///
/// (Control mutation: removing the `is_empty` guard makes the first
/// assertion return `Confirmed`.)
#[test]
fn ask_secret_empty_does_not_confirm_but_does_cancel() {
    assert_eq!(dialog_action(&ask_secret_con(""), "dialog.confirm"), None);
    assert_eq!(
        dialog_action(&ask_secret_con(""), "dialog.cancel"),
        Some(DialogOutcome::Cancelled)
    );
}

/// #325: PASTING into the password field works.
///
/// It is the case that matters most and the one that had been left out:
/// pasting from a password manager is how most people answer this dialog,
/// and without the arm in `route_paste` nothing happened and nothing said
/// so — not a character, not a message. `route_paste`'s contract literally
/// says an overlay that keeps a key also has to keep the paste, "or the two
/// surfaces diverge."
///
/// (Control mutation: removing `AskSecret`'s arm from `route_paste` leaves
/// the field empty and the second assertion turns red.)
#[test]
fn pasting_fills_the_password_field() {
    let dir = vp("file:///x");
    let mut app = norte_tui::app::App::new(
        norte_tui::app::Pane::new(dir.clone(), Vec::new()),
        norte_tui::app::Pane::new(dir, Vec::new()),
    );
    app.modal = Some(ask_secret_con(""));
    norte_tui::paste::route_paste(&mut app, "de-un-gestor");

    let Some(Modal::AskSecret { input, .. }) = &app.modal else {
        panic!("the modal is still open: {:?}", app.modal);
    };
    assert_eq!(
        input.chars(),
        "de-un-gestor".chars().count(),
        "the paste enters whole"
    );
    // And pasting does NOT confirm: an Enter is still needed.
    assert!(matches!(app.modal, Some(Modal::AskSecret { .. })));
}

/// #325: the modal's `Debug` does NOT carry the password. It is where it
/// would leak silently — `tracing`, a panic message, an `assert_eq!`'s diff
/// — and the wrapper exists exactly for that (rule 10).
#[test]
fn the_modal_debug_does_not_carry_the_secret() {
    let mut input = norte_tui::app::TypedSecret::default();
    for c in "hunter2".chars() {
        input.push(c);
    }
    let modal = Modal::AskSecret {
        conn: "rosetta".into(),
        endpoint: "s3://s3.eu-west-1.amazonaws.com".into(),
        input,
        dir: vp("s3://bucket/"),
        pane: 0,
        trail: Trail::Record,
    };
    let painted = format!("{modal:?}");
    assert!(
        !painted.contains("hunter2"),
        "the modal's Debug leaked the password: {painted}"
    );
    // And the connection's name DOES show, which is what makes Debug useful.
    assert!(painted.contains("rosetta"), "{painted}");
}

/// Safety pin H1 T2: a collision has no dangerous default — neither
/// `dialog.confirm` (Enter) nor `dialog.deny` (`n`, which on a collision is
/// not a valid policy) do anything; only overwrite/skip/rename/newer/cancel.
#[test]
fn collision_ignores_confirm_and_deny() {
    assert_eq!(dialog_action(&collision(), "dialog.confirm"), None);
    assert_eq!(dialog_action(&collision(), "dialog.deny"), None);
}

/// Safety pin H1 T2: in confirmations (delete/transfer), both
/// `dialog.confirm` (Enter) and `dialog.approve` (`y`) accept — same
/// behavior as before H1.
#[test]
fn confirm_accepts_confirm_and_approve() {
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
fn confirmation_accepts_and_cancels() {
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
    // Command outside the allowlist: inert (the dialog stays open).
    assert_eq!(dialog_action(&confirm(), "dialog.overwrite"), None);
    let delete = Modal::ConfirmDelete {
        items: vec![vp("file:///x")],
        permanent: false,
    };
    assert_eq!(
        dialog_action(&delete, "dialog.confirm"),
        Some(DialogOutcome::Confirmed)
    );
    assert_eq!(
        dialog_action(&delete, "dialog.cancel"),
        Some(DialogOutcome::Cancelled)
    );
}

#[test]
fn collision_chooses_policy_or_cancels() {
    let cases = [
        ("dialog.overwrite", CollisionPolicy::Overwrite),
        ("dialog.skip", CollisionPolicy::Skip),
        ("dialog.rename", CollisionPolicy::RenameAuto),
        // The collision switched `n`→`w` (dialog.newer, H1 plan decision
        // 5): `n` is now `dialog.deny`, inert on this modal (pin above,
        // `collision_ignores_confirm_and_deny`).
        ("dialog.newer", CollisionPolicy::Newer),
    ];
    for (cmd, policy) in cases {
        assert_eq!(
            dialog_action(&collision(), cmd),
            Some(DialogOutcome::Retry(policy)),
            "command {cmd}"
        );
    }
    assert_eq!(
        dialog_action(&collision(), "dialog.cancel"),
        Some(DialogOutcome::Cancelled)
    );
}

/// Agent approval (M3-3b T5): `dialog.approve` approves, `dialog.deny` and
/// `dialog.cancel` DENY (closing is denying, fail-safe); `dialog.confirm`
/// is inert (pin `approval_ignores_confirm`).
#[test]
fn approval_approves_with_approve_and_denies_with_deny_cancel() {
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
    // Command outside this modal's allowlist.
    assert_eq!(dialog_action(&approval(), "dialog.overwrite"), None);
}

/// H1 T2 integration (issue #24, the "payoff" of turning the keymap into
/// data): a real TOML keymap — `orthodox` preset + a user layer — rebinds
/// `dialog.approve`'s `y` to `dialog.deny`, is RESOLVED with
/// `norte-frontend`'s engine (the same path as the run loop), and
/// `dialog_action` obeys the resolved command, not the physical key: agent
/// approval is now DENIED.
#[test]
fn a_user_layer_rebinds_dialog_and_dialog_action_obeys_it() {
    let preset = presets_orthodox();
    let layer =
        parse_keymap("[dialog]\nprepend_keymap = [{ on = [\"y\"], run = \"dialog.deny\" }]\n")
            .expect("valid layer");
    // The TUI passes the UNION of COMMANDS ∪ DIALOG_COMMANDS to
    // Screen::Dialog (T1 confirmed: build_for_impl validates the WHOLE
    // merged effective, including [global], not just the specific section).
    let known: Vec<&str> = COMMANDS
        .iter()
        .copied()
        .chain(DIALOG_COMMANDS.iter().copied())
        .collect();
    let eff = Effective::build_for(&preset, &[layer], &known, Screen::Dialog)
        .expect("the effective builds with the rebound layer");
    let mut resolver = Resolver::new(eff);
    let res = resolver.push(Chord::new(Mods::default(), KeyCode::Char('y')));
    assert_eq!(
        res,
        Resolution::Run {
            command: "dialog.deny".to_owned(),
            count: Count::None
        }
    );
    let Resolution::Run { command: cmd, .. } = res else {
        unreachable!()
    };
    // The RESOLVED command (not the key) decides the outcome: `y` now
    // denies the agent approval, pure data, with no code touched.
    assert_eq!(
        dialog_action(&approval(), &cmd),
        Some(DialogOutcome::Cancelled)
    );
}

/// review MINOR-3 (H1 close): `ALLOW_APPROVAL` excludes `dialog.confirm` ON
/// PURPOSE — out of the box, Enter NEVER approves an agent mutation (H1
/// plan decision 2). But if a user EXPLICITLY rebinds `enter` to
/// `dialog.approve` in their OWN keymap layer, Enter DOES approve — this
/// test documents that hole as ACCEPTED, not as a bug: security semantics
/// still live in `dialog_action` (the RESOLVED command decides, never the
/// physical key), and reaching this requires a hand-written user layer — no
/// factory preset brings it — so it is informed consent, not a key firing
/// on its own.
#[test]
fn explicit_rebind_of_enter_to_approve_is_informed_consent() {
    let preset = presets_orthodox();
    let layer = parse_keymap(
        "[dialog]\nprepend_keymap = [{ on = [\"enter\"], run = \"dialog.approve\" }]\n",
    )
    .expect("valid layer");
    let known: Vec<&str> = COMMANDS
        .iter()
        .copied()
        .chain(DIALOG_COMMANDS.iter().copied())
        .collect();
    let eff = Effective::build_for(&preset, &[layer], &known, Screen::Dialog)
        .expect("the effective builds with the rebound layer");
    let mut resolver = Resolver::new(eff);
    let res = resolver.push(Chord::new(Mods::default(), KeyCode::Enter));
    assert_eq!(
        res,
        Resolution::Run {
            command: "dialog.approve".to_owned(),
            count: Count::None
        }
    );
    let Resolution::Run { command: cmd, .. } = res else {
        unreachable!()
    };
    assert_eq!(
        dialog_action(&approval(), &cmd),
        Some(DialogOutcome::Confirmed),
        "an EXPLICIT rebind of enter to dialog.approve does approve: informed consent"
    );
}

/// The embedded `orthodox` preset, already parsed (helper for the previous
/// integration test).
fn presets_orthodox() -> norte_tui::keymap::KeymapFile {
    norte_tui::keymap::presets()
        .into_iter()
        .find(|(n, _)| *n == "orthodox")
        .expect("orthodox preset")
        .1
}

/// A two-pane App over the same dir, with no entries (fixture for the
/// M4-IA state tests).
fn app() -> norte_tui::app::App {
    use norte_tui::app::{App, Pane};
    let dir = vp("file:///x");
    App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    )
}

/// An AI rename plan with one pair and an applicable BATCH plan (allowlist
/// test fixture: the case where confirming DOES have something to submit).
fn ai_plan() -> Modal {
    ai_plan_con(batch_plan(true))
}

/// The same fixture with whatever batch plan is passed in: `None` = still
/// in flight, `Some(not executable)` = the core stopped it with verdicts.
fn ai_plan_con(plan: norte_frontend::BatchPlan) -> Modal {
    Modal::AiRenamePlan {
        dir: vp("file:///x"),
        entries: vec![norte_proto::methods::AiRenameEntry {
            from: "a".into(),
            to: "b".into(),
        }],
        offset: 0,
        // A single pair: it is seen whole as soon as the modal opens.
        seen: norte_frontend::AI_RENAME_PAIR_LIMIT,
        plan,
    }
}

/// A `fs.rename_batch_plan` batch plan with whatever verdict is asked for,
/// already in the "the core answered" state.
fn batch_plan(executable: bool) -> norte_frontend::BatchPlan {
    norte_frontend::BatchPlan::Ready(Box::new(norte_proto::methods::FsRenameBatchPlanResult {
        steps: Vec::new(),
        collisions: Vec::new(),
        executable,
        plan_hash: norte_proto::methods::PlanHash::parse(&"0".repeat(64)).expect("64 hex"),
    }))
}

/// M4-IA: the instruction prompt follows `Mkdir`'s discipline (#104 review
/// MINOR-1) — confirming does NOT close; `ai_rename_set_error` leaves a
/// SYNCHRONOUS diagnostic while keeping what was typed (audit INFO-7: in
/// the real flow model failures arrive async with the prompt already closed
/// and go to the bar); only `ai_rename_submitted` closes it.
#[test]
fn ai_rename_instruction_keeps_text_after_failure() {
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
        other => panic!("unexpected modal: {other:?}"),
    }
    app.ai_rename_submitted();
    assert!(app.modal.is_none());
}

/// M4-IA: confirming with an empty instruction returns nothing and leaves
/// the diagnostic under the field (the modal stays open).
#[test]
fn ai_rename_confirm_empty_does_not_return_and_leaves_a_diagnostic() {
    let mut app = app();
    app.open_ai_rename();
    assert!(app.ai_rename_confirm().is_none());
    assert!(matches!(
        &app.modal,
        Some(Modal::AiRenameInstruction { error: Some(_), .. })
    ));
}

/// M4-IA: `AiRenamePlan` is a decision surface over content the human
/// initiated and REVIEWED — it uses `ALLOW_CONFIRM` (Enter confirms, like a
/// delete), NOT the agent-approval allowlist: `dialog.confirm` confirming
/// here is exactly what `ALLOW_APPROVAL` forbids (pin
/// `approval_ignores_confirm`), and collision commands are inert. The
/// instruction prompt is free text: it never goes through `dialog_action`.
#[test]
fn ai_plan_confirms_as_a_confirmation_not_as_an_agent_approval() {
    for cmd in ["dialog.confirm", "dialog.approve"] {
        assert_eq!(
            dialog_action(&ai_plan(), cmd),
            Some(DialogOutcome::Confirmed),
            "command {cmd}"
        );
    }
    for cmd in ["dialog.cancel", "dialog.deny"] {
        assert_eq!(
            dialog_action(&ai_plan(), cmd),
            Some(DialogOutcome::Cancelled),
            "command {cmd}"
        );
    }
    // Outside THIS modal's allowlist: inert. up/down INCLUDED (audit
    // MAJOR-3): the window's scroll is routed by the run loop, it is never
    // an outcome — scrolling neither confirms nor cancels.
    assert_eq!(dialog_action(&ai_plan(), "dialog.overwrite"), None);
    assert_eq!(dialog_action(&ai_plan(), "dialog.up"), None);
    assert_eq!(dialog_action(&ai_plan(), "dialog.down"), None);
}

/// §17: confirming is DISABLED with no APPLICABLE batch plan — with no
/// plan there is no approved `plan_hash` to submit, and with verdicts the
/// core would execute nothing. Cancel is still live in both cases: a modal
/// you could not leave would be worse than one that does not apply.
///
/// (Control mutation: removing `dialog_action`'s gate puts `Some(Confirmed)`
/// on the first two rounds and breaks this test.)
#[test]
fn ai_plan_does_not_confirm_without_an_applicable_batch() {
    for plan in [
        norte_frontend::BatchPlan::Pending,
        norte_frontend::BatchPlan::Failed,
        batch_plan(false),
    ] {
        let modal = ai_plan_con(plan);
        for cmd in ["dialog.confirm", "dialog.approve"] {
            assert_eq!(
                dialog_action(&modal, cmd),
                None,
                "{cmd} cannot confirm a batch that cannot be executed: {modal:?}"
            );
        }
        for cmd in ["dialog.cancel", "dialog.deny"] {
            assert_eq!(
                dialog_action(&modal, cmd),
                Some(DialogOutcome::Cancelled),
                "cancel ALWAYS works: {modal:?}"
            );
        }
    }
    // Free text (like Mkdir/MarkPattern): the run loop intercepts it BEFORE.
    let prompt = Modal::AiRenameInstruction {
        instruction: String::new(),
        error: None,
    };
    assert_eq!(dialog_action(&prompt, "dialog.confirm"), None);
    assert_eq!(dialog_action(&prompt, "dialog.approve"), None);
}

/// §17: `fs.rename_batch_plan`'s answer arrives ASYNCHRONOUSLY (the modal
/// opens `Pending` and gets filled in), so it has to land on the open modal
/// — and only if that modal is still waiting. An answer never overwrites an
/// already-resolved plan, and with no modal it lands nowhere.
///
/// (Control mutation: removing the `Pending` guard makes the second round
/// overwrite and breaks this test.)
#[test]
fn the_batch_plan_only_fills_the_modal_that_was_waiting_for_it() {
    let mut app = app();
    // No modal: the answer is dropped, and it SAYS SO.
    assert!(!app.settle_ai_batch_plan(&batch_plan(true)));

    app.modal = Some(ai_plan_con(norte_frontend::BatchPlan::Pending));
    assert!(app.settle_ai_batch_plan(&batch_plan(true)));
    let Some(Modal::AiRenamePlan { plan, .. }) = &app.modal else {
        panic!("unexpected modal: {:?}", app.modal);
    };
    assert!(plan.confirmable(), "the plan landed: {plan:?}");

    // Already resolved: a second answer does NOT overwrite it.
    assert!(!app.settle_ai_batch_plan(&norte_frontend::BatchPlan::Failed));
    let Some(Modal::AiRenamePlan { plan, .. }) = &app.modal else {
        panic!("unexpected modal: {:?}", app.modal);
    };
    assert!(
        plan.confirmable(),
        "a late answer does not degrade it: {plan:?}"
    );

    // Another modal on top: also not (the run loop looks for it in its stash).
    app.modal = Some(confirm());
    assert!(!app.settle_ai_batch_plan(&batch_plan(true)));
}

/// A plan that has not been READ cannot be approved.
///
/// The terminal only required the core to accept it, so a plan with two
/// hundred renames could be signed off after seeing the first ten — and the
/// ones that matter could be on row one hundred eighty. The window already
/// required it: the same question with two answers, on the surface where
/// it costs the most.
#[test]
fn an_unread_plan_is_not_approved() {
    let long: Vec<norte_proto::methods::AiRenameEntry> = (1..=40)
        .map(|i| norte_proto::methods::AiRenameEntry {
            from: format!("f{i}"),
            to: format!("t{i}"),
        })
        .collect();
    let mut app = app();
    app.modal = Some(Modal::AiRenamePlan {
        dir: vp("file:///x"),
        entries: long.clone(),
        offset: 0,
        seen: norte_frontend::AI_RENAME_PAIR_LIMIT,
        plan: batch_plan(true),
    });
    assert_eq!(
        dialog_action(
            app.modal.as_ref().expect("there is a modal"),
            "dialog.confirm"
        ),
        None,
        "the core accepts it, but the reader stayed on the first window"
    );

    // Scroll all the way down: the watermark rises, and going back up does
    // NOT un-read what was already read.
    for _ in 0..long.len() {
        app.ai_plan_scroll(true);
    }
    for _ in 0..long.len() {
        app.ai_plan_scroll(false);
    }
    assert_eq!(
        dialog_action(
            app.modal.as_ref().expect("there is a modal"),
            "dialog.confirm"
        ),
        Some(DialogOutcome::Confirmed),
        "seen in full, and going back up does not un-read it"
    );
}

/// Audit MAJOR-3: the plan's scroll clamps the window to `[0, len - 5]`
/// (never overshoots or goes negative) and advances/retreats one at a time
/// with stable numbering.
#[test]
fn the_plans_scroll_clamps_at_both_ends() {
    let offset_de = |app: &norte_tui::app::App| match &app.modal {
        Some(Modal::AiRenamePlan { offset, .. }) => *offset,
        other => panic!("unexpected modal: {other:?}"),
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
        seen: norte_frontend::AI_RENAME_PAIR_LIMIT,
        plan: batch_plan(true),
    });
    app.ai_plan_scroll(false);
    assert_eq!(offset_de(&app), 0, "it does not go below zero");
    for _ in 0..10 {
        app.ai_plan_scroll(true);
    }
    assert_eq!(offset_de(&app), 2, "clamps at len - window (7 - 5)");
    app.ai_plan_scroll(false);
    assert_eq!(offset_de(&app), 1);
}

/// Approvals queue like collisions (they never overwrite an open modal) and
/// have PRIORITY over them: an approval expires by TTL on the daemon; a
/// collision waits as long as it takes.
#[test]
fn approvals_queue_with_priority_over_collisions() {
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

    // With a modal open, nothing changes.
    app.open_next_pending();
    assert_eq!(
        app.modal,
        Some(confirm()),
        "the open modal is not overwritten"
    );

    // Once closed, the APPROVAL comes out before the collision queued first.
    app.modal = None;
    app.open_next_pending();
    assert!(matches!(app.modal, Some(Modal::ApproveAgentOp { .. })));
    app.modal = None;
    app.open_next_pending();
    assert!(matches!(app.modal, Some(Modal::Collision { .. })));
    app.modal = None;
    app.open_next_pending();
    assert_eq!(app.modal, None, "empty queues");
}

#[test]
fn collisions_queue_and_never_step_on_a_modal() {
    use norte_tui::app::{App, Pane};
    let dir = vp("file:///x");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );
    app.modal = Some(confirm());
    app.pending_collisions.push_back(retry());
    app.pending_collisions.push_back(retry());

    // With a modal open, nothing changes.
    app.open_next_collision();
    assert_eq!(
        app.modal,
        Some(confirm()),
        "the open modal is not overwritten"
    );

    // Once closed, the collisions come out in order.
    app.modal = None;
    app.open_next_collision();
    assert!(matches!(app.modal, Some(Modal::Collision { .. })));
    app.modal = None;
    app.open_next_collision();
    assert!(matches!(app.modal, Some(Modal::Collision { .. })));
    app.modal = None;
    app.open_next_collision();
    assert_eq!(app.modal, None, "empty queue");
}

/// Semantic hits fixture (M4-IA-2): `n` distinct paths, descending score.
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

/// M4-IA-2: the query prompt follows the AI instruction one's discipline —
/// confirming does NOT close (returns the trimmed query); only
/// `semantic_submitted` closes it after spawning.
#[test]
fn semantic_query_modal_edits_and_confirms() {
    let mut app = app();
    app.open_semantic_search();
    for c in "facturas 2024".chars() {
        app.semantic_push(c);
    }
    assert_eq!(app.semantic_confirm().as_deref(), Some("facturas 2024"));
    assert!(
        matches!(app.modal, Some(Modal::SemanticQuery { .. })),
        "confirming does not close it: the submit does"
    );
    app.semantic_submitted();
    assert!(app.modal.is_none());
}

/// M4-IA-2: confirming with an empty query returns nothing and leaves the
/// diagnostic under the field (the modal stays open); `semantic_set_error`
/// keeps what was typed.
#[test]
fn an_empty_semantic_query_does_not_confirm() {
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
        other => panic!("unexpected modal: {other:?}"),
    }
}

/// M4-IA-2: the hits cursor clamps at both ends and the WINDOW follows it
/// (scrolls down past the bottom edge, up past the top one).
#[test]
fn semantic_hits_cursor_scroll_clampa() {
    let state = |app: &norte_tui::app::App| match &app.modal {
        Some(Modal::SemanticHits { offset, cursor, .. }) => (*offset, *cursor),
        other => panic!("unexpected modal: {other:?}"),
    };
    let mut app = app();
    app.modal = Some(semantic_hits(12));
    app.semantic_cursor(false);
    assert_eq!(state(&app), (0, 0), "it does not go below zero");
    for _ in 0..99 {
        app.semantic_cursor(true);
    }
    assert_eq!(
        state(&app),
        (2, 11),
        "the cursor clamps at len-1 and the window follows it (12 - 10)"
    );
    for _ in 0..99 {
        app.semantic_cursor(false);
    }
    assert_eq!(state(&app), (0, 0), "the window scrolls back up with it");
}

/// M4-IA-2: `SemanticHits` confirms as a decision (`ALLOW_CONFIRM`, Enter
/// navigates) and up/down are INERT for `dialog_action` (the run loop
/// routes the cursor, it is never an outcome); the query prompt is free
/// text and never goes through here.
#[test]
fn semantic_hits_confirms_as_a_decision_and_the_cursor_is_inert() {
    for cmd in ["dialog.confirm", "dialog.approve"] {
        assert_eq!(
            dialog_action(&semantic_hits(1), cmd),
            Some(DialogOutcome::Confirmed),
            "command {cmd}"
        );
    }
    for cmd in ["dialog.cancel", "dialog.deny"] {
        assert_eq!(
            dialog_action(&semantic_hits(1), cmd),
            Some(DialogOutcome::Cancelled),
            "command {cmd}"
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
