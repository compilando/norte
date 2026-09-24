//! policy↔engine integration (M3-3a): the PRE-effect gate decides allow/ask/deny
//! per operation; the real actor reaches the journal; undo goes through the
//! policy too.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use norte_core::approval::{ApprovalOutcome, ApprovalRequest, ApprovalResolver, DenyAll};
use norte_core::journal::Actor;
use norte_core::policy::{OpSet, PolicyConfig, Scope, ScopeRegistry, ScopedPolicy};
use norte_core::{Engine, Journal, SqliteJournal};
use norte_proto::{Error, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(w: &str) -> VPath {
    VPath::parse(w).expect("wire")
}

async fn write_file(mem: &MemProvider, w: &str, c: &[u8]) {
    let mut s = mem.write(&vp(w)).await.expect("open");
    s.write(Bytes::copy_from_slice(c)).await.expect("chunk");
    s.commit().await.expect("commit");
}

struct AlwaysApprove;
#[async_trait]
impl ApprovalResolver for AlwaysApprove {
    async fn request(&self, _r: ApprovalRequest) -> ApprovalOutcome {
        ApprovalOutcome::Approved
    }
}

/// Engine with a journal + `ScopedPolicy` and a given resolver.
async fn engine_with_policy(
    config: PolicyConfig,
    scopes: ScopeRegistry,
    resolver: Arc<dyn ApprovalResolver>,
) -> (Engine, Arc<MemProvider>, Arc<SqliteJournal>) {
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("j"),
    ));
    let policy = Arc::new(ScopedPolicy::new(scopes, config));
    let engine = Engine::with_journal(Arc::clone(&journal)).with_policy(policy, resolver);
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem, journal)
}

fn agent() -> Actor {
    Actor::Agent {
        session: "s1".into(),
    }
}

fn full_scope() -> ScopeRegistry {
    let reg = ScopeRegistry::new();
    reg.grant("s1", Scope::forever(vec![vp("mem:///")], OpSet::all()));
    reg
}

#[tokio::test]
async fn agent_out_of_scope_copy_is_denied_without_touching_fs() {
    let (engine, mem, _j) = engine_with_policy(
        PolicyConfig::default(),
        ScopeRegistry::new(),
        Arc::new(DenyAll),
    )
    .await;
    write_file(&mem, "mem:///src.txt", b"x").await;
    let err = engine
        .copy_with_as(
            &vp("mem:///src.txt"),
            &vp("mem:///dst.txt"),
            norte_core::TransferOptions::default(),
            agent(),
        )
        .await
        .err()
        .expect("denied");
    assert!(
        matches!(err, Error::PolicyDenied { ref rule } if rule == "out-of-scope"),
        "out-of-scope policy: {err:?}"
    );
    assert!(
        matches!(mem.stat(&vp("mem:///dst.txt")).await, Err(Error::NotFound)),
        "did not touch the FS"
    );
}

#[tokio::test]
async fn agent_in_scope_with_allow_rule_proceeds_and_journals_actor() {
    let cfg = PolicyConfig::parse("[[rule]]\naction=\"allow\"").expect("cfg");
    let (engine, mem, journal) = engine_with_policy(cfg, full_scope(), Arc::new(DenyAll)).await;
    write_file(&mem, "mem:///src.txt", b"x").await;
    let h = engine
        .copy_with_as(
            &vp("mem:///src.txt"),
            &vp("mem:///dst.txt"),
            norte_core::TransferOptions::default(),
            agent(),
        )
        .await
        .expect("submit");
    assert_eq!(h.join().await, TaskState::Completed);
    // The journal recorded the creation with actor AGENT (closes M2 debt from M3-2).
    let es = journal.journal().entries().await.expect("entries");
    let created = es.iter().find(|e| e.op == "created").expect("created");
    assert_eq!(created.actor_kind, "agent");
    assert_eq!(created.actor_id.as_deref(), Some("s1"));
}

#[tokio::test]
async fn ask_rule_suspends_until_resolver_approves() {
    let cfg = PolicyConfig::parse("[[rule]]\naction=\"ask\"").expect("cfg");
    let (engine, mem, _j) = engine_with_policy(cfg, full_scope(), Arc::new(AlwaysApprove)).await;
    write_file(&mem, "mem:///src.txt", b"x").await;
    let h = engine
        .copy_with_as(
            &vp("mem:///src.txt"),
            &vp("mem:///dst.txt"),
            norte_core::TransferOptions::default(),
            agent(),
        )
        .await
        .expect("approved proceeds");
    assert_eq!(h.join().await, TaskState::Completed);
    assert!(mem.stat(&vp("mem:///dst.txt")).await.is_ok());
}

#[tokio::test]
async fn ask_denied_by_resolver_blocks_the_op() {
    let cfg = PolicyConfig::parse("[[rule]]\naction=\"ask\"").expect("cfg");
    // DenyAll resolver → Ask resolves to Denied.
    let (engine, mem, _j) = engine_with_policy(cfg, full_scope(), Arc::new(DenyAll)).await;
    write_file(&mem, "mem:///src.txt", b"x").await;
    let err = engine
        .copy_with_as(
            &vp("mem:///src.txt"),
            &vp("mem:///dst.txt"),
            norte_core::TransferOptions::default(),
            agent(),
        )
        .await
        .err()
        .expect("denied");
    assert!(
        matches!(err, Error::PolicyDenied { ref rule } if rule == "not-approved"),
        "Ask denied by the resolver: {err:?}"
    );
    assert!(matches!(
        mem.stat(&vp("mem:///dst.txt")).await,
        Err(Error::NotFound)
    ));
}

#[tokio::test]
async fn user_bypasses_policy() {
    // Without scope or rules, a User copies all the same (no sandboxing).
    let (engine, mem, _j) = engine_with_policy(
        PolicyConfig::default(),
        ScopeRegistry::new(),
        Arc::new(DenyAll),
    )
    .await;
    write_file(&mem, "mem:///src.txt", b"x").await;
    let h = engine
        .copy(&vp("mem:///src.txt"), &vp("mem:///dst.txt"))
        .await
        .expect("submit");
    assert_eq!(h.join().await, TaskState::Completed);
}

#[tokio::test]
async fn undo_of_agent_is_blocked_when_reverse_op_denied_by_policy() {
    // copy allowed, delete denied. An agent creates (copy allow); the undo of
    // that Created is a delete → the policy blocks it.
    let cfg = PolicyConfig::parse(
        "[[rule]]\nop=\"copy\"\naction=\"allow\"\n[[rule]]\nop=\"delete\"\naction=\"deny\"",
    )
    .expect("cfg");
    let (engine, mem, _j) = engine_with_policy(cfg, full_scope(), Arc::new(DenyAll)).await;
    write_file(&mem, "mem:///src.txt", b"x").await;
    let h = engine
        .copy_with_as(
            &vp("mem:///src.txt"),
            &vp("mem:///dst.txt"),
            norte_core::TransferOptions::default(),
            agent(),
        )
        .await
        .expect("copy");
    assert_eq!(h.join().await, TaskState::Completed);

    let (uh, report) = engine.undo_session(agent()).await.expect("undo submit");
    assert_eq!(uh.join().await, TaskState::Completed);
    let r = report.lock().expect("lock").clone();
    assert_eq!(r.undone, 0, "the undo (delete) is denied by policy");
    // #171: a policy denial is a ROW in the report, not a `blocked`.
    // `blocked` still means "I stopped because of drift and the tree stayed
    // consistent"; this means "this unit was not touched, and I kept going".
    assert!(r.blocked.is_none(), "not a block: {:?}", r.blocked);
    assert_eq!(r.denied_total, 1);
    assert!(
        matches!(r.denied.first(), Some((_, Error::PolicyDenied { rule })) if rule == "policy-rule"),
        "denied by policy: {:?}",
        r.denied
    );
    // Nothing was overwritten: dst still exists (undo never got to delete it).
    assert!(mem.stat(&vp("mem:///dst.txt")).await.is_ok());
}

/// **#171: a denied unit does not take the others down with it.**
///
/// Undo runs LIFO, so here the denied one is the FIRST one processed: if it
/// stopped right there — which is what it used to do — the `move` underneath
/// would never come back. It is the same rule as the forward executor: `Deny`
/// is a report row and the work continues.
#[tokio::test]
async fn a_denied_unit_does_not_stop_the_undo_of_the_others() {
    // `move` allowed, `delete` denied: the undo of a `Created` is a delete
    // (denied) and the undo of a `Moved` is a rename_back (allowed).
    let cfg = PolicyConfig::parse(
        "[[rule]]\nop=\"copy\"\naction=\"allow\"\n[[rule]]\nop=\"move\"\naction=\"allow\"\n[[rule]]\nop=\"delete\"\naction=\"deny\"",
    )
    .expect("cfg");
    let (engine, mem, _j) = engine_with_policy(cfg, full_scope(), Arc::new(DenyAll)).await;
    write_file(&mem, "mem:///source.txt", b"x").await;
    write_file(&mem, "mem:///other.txt", b"y").await;

    // 1) A move: its undo is a rename_back, ALLOWED.
    let h = engine
        .move_with_as(
            &vp("mem:///other.txt"),
            &vp("mem:///moved.txt"),
            norte_core::TransferOptions::default(),
            agent(),
        )
        .await
        .expect("move");
    assert_eq!(h.join().await, TaskState::Completed);

    // 2) A copy: its undo is a delete, DENIED. It comes after, so LIFO
    //    processes it FIRST.
    let h = engine
        .copy_with_as(
            &vp("mem:///source.txt"),
            &vp("mem:///copy.txt"),
            norte_core::TransferOptions::default(),
            agent(),
        )
        .await
        .expect("copy");
    assert_eq!(h.join().await, TaskState::Completed);

    let (uh, report) = engine.undo_session(agent()).await.expect("undo submit");
    assert_eq!(uh.join().await, TaskState::Completed);
    let r = report.lock().expect("lock").clone();

    assert_eq!(r.denied_total, 1, "the copy: its delete is denied");
    assert!(r.blocked.is_none());
    assert!(
        r.undone >= 1,
        "and the move underneath DID come back: {r:?}"
    );
    // The tree confirms it: the copy is still there and the move is back in place.
    assert!(mem.stat(&vp("mem:///copy.txt")).await.is_ok());
    assert!(mem.stat(&vp("mem:///other.txt")).await.is_ok());
    assert!(mem.stat(&vp("mem:///moved.txt")).await.is_err());
}

/// **Undoing a permission change asks for `set-mode`, not `delete`** (#314).
///
/// `set-mode` and `delete` are INDEPENDENT permissions, so asking for the
/// latter had both bad sides: an actor with `delete` could undo a chmod the
/// policy did not grant them, and one with `set-mode` could not undo their
/// own — and with strict LIFO that blocks the whole session behind it. It is
/// the same bug this file already fixed once for `move`.
#[tokio::test]
async fn undoing_a_chmod_asks_for_chmod_permission() {
    // TWO engines over the SAME journal and the same provider: in the first
    // the agent can change permissions, in the second it no longer can. This
    // is how to separate what the policy grants GOING FORWARD from what it
    // grants when undoing, which is exactly the distinction this test
    // measures — a permission expired or revoked between the two.
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("j"),
    ));
    let mem = Arc::new(MemProvider::new());
    let allows = PolicyConfig::parse("[[rule]]\nop=\"set-mode\"\naction=\"allow\"").expect("cfg");
    // Everything allowed EXCEPT `set-mode`: if the reversal asked for `delete`
    // — which IS allowed here — it would pass, and this test would not catch
    // the bug.
    let denies = PolicyConfig::parse(
        "[[rule]]\nop=\"delete\"\naction=\"allow\"\n\
         [[rule]]\nop=\"move\"\naction=\"allow\"\n\
         [[rule]]\nop=\"copy\"\naction=\"allow\"\n\
         [[rule]]\nop=\"mkdir\"\naction=\"allow\"\n\
         [[rule]]\nop=\"create\"\naction=\"allow\"\n\
         [[rule]]\nop=\"set-mode\"\naction=\"deny\"",
    )
    .expect("cfg");
    let engine_with = |cfg: PolicyConfig| {
        let e = Engine::with_journal(Arc::clone(&journal)).with_policy(
            Arc::new(ScopedPolicy::new(full_scope(), cfg)),
            Arc::new(DenyAll) as Arc<dyn ApprovalResolver>,
        );
        e.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
        e
    };

    let before = engine_with(allows);
    write_file(&mem, "mem:///a.sh", b"x").await;
    let h = before
        .set_mode_as(
            norte_proto::methods::FsSetModeParams {
                paths: vec![vp("mem:///a.sh")],
                mode: 0o700,
                recursive: false,
                dir_mode: None,
            },
            agent(),
        )
        .await
        .expect("with `set-mode` granted, the agent changes permissions");
    assert_eq!(h.join().await, TaskState::Completed);

    let after = engine_with(denies);
    let (uh, report) = after.undo_session(agent()).await.expect("undo submit");
    assert_eq!(uh.join().await, TaskState::Completed);
    let r = report.lock().expect("lock").clone();
    assert_eq!(
        r.denied_total, 1,
        "undoing a chmod asks for `set-mode`, and it is denied here: {r:?}"
    );
    assert_eq!(r.undone, 0, "and therefore nothing was undone");
}

/// **The question asked to the human states WHICH mode** (#314, 0.61.0).
///
/// `set-mode` is the first op where two requests with the same op and the same
/// paths mean opposite things — `0600` and `4777` — so without the mode the
/// human is not consenting to what they think they are. It is the same
/// argument `paths_total` makes for the count.
#[tokio::test]
async fn approving_a_chmod_states_the_mode() {
    use norte_core::approval::{ApprovalOutcome, ApprovalRequest, ApprovalResolver};

    /// A resolver that keeps what it was asked and denies.
    struct Snoop(std::sync::Mutex<Vec<norte_core::PolicyOp>>);
    #[async_trait::async_trait]
    impl ApprovalResolver for Snoop {
        async fn request(&self, req: ApprovalRequest) -> ApprovalOutcome {
            self.0.lock().expect("lock").push(req.op);
            ApprovalOutcome::Denied
        }
    }

    let cfg = PolicyConfig::parse("[[rule]]\nop=\"set-mode\"\naction=\"ask\"").expect("cfg");
    let snoop = Arc::new(Snoop(std::sync::Mutex::new(Vec::new())));
    let (engine, mem, _j) = engine_with_policy(
        cfg,
        full_scope(),
        Arc::clone(&snoop) as Arc<dyn ApprovalResolver>,
    )
    .await;
    write_file(&mem, "mem:///a.sh", b"x").await;

    let _ = engine
        .set_mode_as(
            norte_proto::methods::FsSetModeParams {
                paths: vec![vp("mem:///a.sh")],
                mode: 0o750,
                recursive: false,
                dir_mode: None,
            },
            agent(),
        )
        .await;

    let questions = snoop.0.lock().expect("lock").clone();
    assert_eq!(questions.len(), 1, "asked once: {questions:?}");
    assert!(
        matches!(questions[0], norte_core::PolicyOp::SetMode { mode, .. } if mode == 0o750),
        "and the question carries the MODE, not just the op: {:?}",
        questions[0]
    );
}

/// The committed policy example (`docs/policy-example.toml`) ALWAYS parses
/// (M3-4 T4): if the rule syntax changes, this test catches it — the example
/// never goes stale.
#[test]
fn policy_example_toml_parses() {
    let cfg = norte_core::PolicyConfig::parse(include_str!("../../../docs/policy-example.toml"))
        .expect("the example in docs/ parses");
    assert!(!cfg.rules.is_empty(), "brings at least the ask rule");
}
