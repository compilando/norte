//! Integración policy↔engine (M3-3a): el gate PRE-efecto decide allow/ask/deny
//! por operación; el actor real llega al journal; el undo pasa por la policy.

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

/// Engine con journal + `ScopedPolicy` y un resolver dado.
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
        .expect("denegada");
    assert!(
        matches!(err, Error::PolicyDenied { ref rule } if rule == "out-of-scope"),
        "policy fuera-de-scope: {err:?}"
    );
    assert!(
        matches!(mem.stat(&vp("mem:///dst.txt")).await, Err(Error::NotFound)),
        "no tocó el FS"
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
    // El journal registró la creación con actor AGENT (cierra deuda M2 de M3-2).
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
        .expect("aprobada procede");
    assert_eq!(h.join().await, TaskState::Completed);
    assert!(mem.stat(&vp("mem:///dst.txt")).await.is_ok());
}

#[tokio::test]
async fn ask_denied_by_resolver_blocks_the_op() {
    let cfg = PolicyConfig::parse("[[rule]]\naction=\"ask\"").expect("cfg");
    // DenyAll resolver → Ask se resuelve Denied.
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
        .expect("denegada");
    assert!(
        matches!(err, Error::PolicyDenied { ref rule } if rule == "not-approved"),
        "Ask denegado por el resolver: {err:?}"
    );
    assert!(matches!(
        mem.stat(&vp("mem:///dst.txt")).await,
        Err(Error::NotFound)
    ));
}

#[tokio::test]
async fn user_bypasses_policy() {
    // Sin scope ni reglas, un User copia igual (no se sandboxea).
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
    // copy permitido, delete denegado. Un agente crea (copy allow); el undo de
    // ese Created es un delete → la policy lo bloquea.
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
    assert_eq!(r.undone, 0, "el undo (delete) lo bloquea la policy");
    assert!(
        matches!(&r.blocked, Some((_, Error::PolicyDenied { rule })) if rule == "policy-rule"),
        "bloqueado por policy: {:?}",
        r.blocked
    );
    // No pisó: dst sigue existiendo (el undo no llegó a borrarlo).
    assert!(mem.stat(&vp("mem:///dst.txt")).await.is_ok());
}

/// El ejemplo commiteado de policy (`docs/policy-example.toml`) parsea SIEMPRE
/// (M3-4 T4): si la sintaxis de reglas cambia, este test lo delata — el
/// ejemplo jamás se pudre.
#[test]
fn policy_example_toml_parsea() {
    let cfg = norte_core::PolicyConfig::parse(include_str!("../../../docs/policy-example.toml"))
        .expect("el ejemplo de docs/ parsea");
    assert!(!cfg.rules.is_empty(), "trae al menos la regla ask");
}
