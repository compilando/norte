//! E2E of M3's EXIT CRITERION (spec §M3): "Claude Code manages a real
//! directory under `ask` policy, with full-session undo". Everything
//! in-process (a UDS daemon in a tempdir + the MCP bridge + a human client):
//! the whole flow `request_scope` → grant → copy(ask) → approval → decide →
//! completed → undo → reverted, and the scope boundary that stays closed.
#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use norte_core::daemon::{Client, Daemon, DaemonApprovalResolver, DaemonConfig};
use norte_core::{Engine, PolicyConfig, ScopeRegistry, ScopedPolicy};
use norte_mcp::bridge::Bridge;
use norte_proto::VPath;
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write opens");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

/// `tools/call` through the bridge → `(text, is_error)`.
async fn call_tool(b: &Bridge, name: &str, args: serde_json::Value) -> (String, bool) {
    let req = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": name, "arguments": args},
    });
    let out = b.handle_line(&req.to_string()).await.expect("response");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let content = v["result"]["content"][0]["text"]
        .as_str()
        .expect("content")
        .to_owned();
    (content, v["result"]["isError"].as_bool().expect("isError"))
}

/// M3's exit criterion.
/// REAL daemon with an in-memory journal + `ask` policy + approval router,
/// with `mem:///proj/informe.txt` seeded. Returns `(tempdir, socket, mem)`.
async fn spawn_ask_daemon() -> (tempfile::TempDir, std::path::PathBuf, Arc<MemProvider>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let scopes = ScopeRegistry::new();
    let cfg = PolicyConfig::parse("[[rule]]\naction=\"ask\"").expect("policy");
    let policy = ScopedPolicy::new(scopes.clone(), cfg);
    let approvals = Arc::new(DaemonApprovalResolver::new(Duration::from_secs(30)));
    let journal = Arc::new(norte_core::SqliteJournal::new(
        norte_core::Journal::open_in_memory()
            .await
            .expect("journal"),
    ));
    let engine = Arc::new(
        Engine::with_journal(journal).with_policy(Arc::new(policy), Arc::clone(&approvals) as _),
    );
    // `sync.plan`'s spool, under the socket's same tempdir (ADR 0049):
    // without it, `sync.plan` answers `Unsupported` (see norte-core's
    // `daemon.rs`, which documents the same requirement).
    engine.set_spool(norte_core::sync::Spool::new(dir.path()));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&mem, "mem:///proj/informe.txt", b"datos importantes").await;
    let daemon = Daemon::bind_with_policy(
        engine,
        scopes,
        approvals,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: Some(dir.path().to_path_buf()),
            state_dir: None,
        },
    )
    .await
    .expect("bind");
    tokio::spawn(daemon.run());
    (dir, socket, mem)
}

/// Waits for `task_id`'s terminal via `task.list` (with a cap).
async fn wait_undo_terminal(human: &Client, task_id: norte_proto::TaskId) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let listed: norte_proto::methods::TaskListResult = human
            .call(
                norte_proto::methods::TASK_LIST,
                &norte_proto::methods::TaskListParams {},
            )
            .await
            .expect("task.list");
        if let Some(t) = listed.tasks.iter().find(|t| t.task_id == task_id)
            && t.state.is_terminal()
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the undo did not finish"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Waits for the undo's terminal and checks its report (#71): 1 reverted,
/// nothing skipped or blocked — the "done" stops being blind.
async fn assert_undo_report_clean(human: &Client, task_id: norte_proto::TaskId) {
    wait_undo_terminal(human, task_id).await;
    let report: norte_proto::methods::PolicyUndoReportResult = human
        .call(
            norte_proto::methods::POLICY_UNDO_REPORT,
            &norte_proto::methods::PolicyUndoReportParams { task_id },
        )
        .await
        .expect("undo_report");
    assert_eq!(report.undone, 1, "the reverted copy is counted");
    assert_eq!(report.skipped_created_no_trash, 0);
    assert!(report.blocked.is_none(), "no blocking: {report:?}");
}

/// M3's exit criterion, end to end.
#[tokio::test]
async fn criterio_de_salida_m3_agente_bajo_ask_con_undo() {
    let (_dir, socket, mem) = spawn_ask_daemon().await;

    // --- The HUMAN: a User connection that grants, approves and undoes ---
    let mut human = Client::connect(&socket).await.expect("human connect");
    human
        .initialize(norte_proto::methods::ClientInfo {
            name: "tui".into(),
            version: "0".into(),
        })
        .await
        .expect("human initialize");

    // --- The AGENT: the MCP bridge declaring the session ---
    let agent = Bridge::connect(&socket, "claude")
        .await
        .expect("agent connect");

    // 1) The agent asks for scope over its project.
    let (out, err) = call_tool(
        &agent,
        "request_scope",
        serde_json::json!({"roots": ["mem:///proj"], "ops": ["copy"], "ttl_ms": 60000}),
    )
    .await;
    assert!(!err, "{out}");
    let req_id = serde_json::from_str::<serde_json::Value>(&out).expect("json")["request_id"]
        .as_u64()
        .expect("request_id");

    // 2) The human grants it.
    let _: norte_proto::methods::GrantScopeResult = human
        .call(
            norte_proto::methods::POLICY_GRANT_SCOPE,
            &norte_proto::methods::GrantScopeParams { request_id: req_id },
        )
        .await
        .expect("grant");

    // 3) The agent copies — it stays SUSPENDED in the `ask` (spawned: the
    //    human must still be served in order to approve).
    let copy = tokio::spawn(async move {
        let r = call_tool(
            &agent,
            "copy",
            serde_json::json!({"from": "mem:///proj/informe.txt", "to": "mem:///proj/copia.txt"}),
        )
        .await;
        (agent, r)
    });

    // 4) The human gets `policy.approval_required` and approves.
    let approval_id = loop {
        let n = human.notification().await.expect("live channel");
        if n.method == norte_proto::methods::POLICY_APPROVAL_REQUIRED {
            let req: norte_proto::methods::PolicyApprovalRequired =
                serde_json::from_value(n.params.expect("params")).expect("shape");
            assert_eq!(req.op, "copy");
            assert_eq!(req.session.as_deref(), Some("claude"));
            break req.approval_id;
        }
    };
    let _: norte_proto::methods::PolicyDecideResult = human
        .call(
            norte_proto::methods::POLICY_DECIDE,
            &norte_proto::methods::PolicyDecideParams {
                approval_id,
                approve: true,
            },
        )
        .await
        .expect("decide approve");

    // 5) The copy proceeds; the destination exists.
    let (agent, (out, err)) = tokio::time::timeout(Duration::from_secs(5), copy)
        .await
        .expect("does not hang")
        .expect("join");
    assert!(!err, "approved must complete: {out}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&out).expect("json")["state"],
        "completed"
    );
    assert!(mem.stat(&vp("mem:///proj/copia.txt")).await.is_ok());

    // 6) The human undoes the agent's WHOLE session → the copy is reverted,
    //    the original stays intact.
    let undone: norte_proto::methods::PolicyUndoSessionResult = human
        .call(
            norte_proto::methods::POLICY_UNDO_SESSION,
            &norte_proto::methods::PolicyUndoSessionParams {
                session: "claude".into(),
            },
        )
        .await
        .expect("undo_session");
    // 6b) Terminal + report (#71): 1 reverted, nothing skipped or blocked.
    assert_undo_report_clean(&human, undone.task_id).await;
    assert!(
        matches!(
            mem.stat(&vp("mem:///proj/copia.txt")).await,
            Err(norte_proto::Error::NotFound)
        ),
        "the agent's copy was reverted"
    );
    assert!(
        mem.stat(&vp("mem:///proj/informe.txt")).await.is_ok(),
        "the original was never touched"
    );

    // 7) Outside scope is STILL closed: the grant was not a blank check.
    let (out, err) = call_tool(
        &agent,
        "copy",
        serde_json::json!({"from": "mem:///proj/informe.txt", "to": "mem:///fuera.txt"}),
    )
    .await;
    assert!(err, "outside scope must fail");
    assert!(
        out.contains("out-of-scope") && out.contains("request_scope"),
        "actionable denial: {out}"
    );
}

/// A miniature #155: the bridge can OPEN the arm that drains notifications,
/// and it opens it ONCE. Without this, `compare` and `sync_plan` have
/// nowhere to receive their rows from.
///
/// And the arm belongs to the AGENT, not to a user: checked with the read
/// gate, which requires a live scope for `Actor::Agent` and requires nothing
/// for `Actor::User`. If the arm were opened with the bare `connect`, the
/// listing below would succeed — and the bridge would have laundered the
/// actor.
#[tokio::test]
async fn el_puente_abre_su_brazo_de_streams_una_sola_vez() {
    let (_dir, socket, _mem) = spawn_ask_daemon().await;
    let agent = Bridge::connect(&socket, "claude")
        .await
        .expect("agent connect");

    let first = agent.streams().await.expect("first arm");
    let second = agent.streams().await.expect("second arm");
    assert!(
        std::ptr::eq(first, second),
        "the arm opens lazily but ONCE: two connections per session \
         would be two conn_ids and no benefit"
    );

    // The arm declares `agent_session`: without a granted scope, the
    // daemon's read gate blocks it. A bare `connect` (User actor) would list.
    let denied = first.list(&vp("mem:///proj")).await;
    assert!(
        matches!(denied, Err(norte_proto::Error::PolicyDenied { .. })),
        "the arm has to be an AGENT connection (no scope, blocked), was {denied:?}"
    );
}

/// Requests and grants READ scope for `roots` (test helper): `compare` goes
/// through the same `read_gate` as `list`/`stat` (see
/// `el_puente_abre_su_brazo_de_streams_una_sola_vez`), so a read-only tool
/// needs a granted scope just like a mutating one — the op chosen (`copy`)
/// is irrelevant to `covers_read`, which only looks at the root.
async fn grant_read_scope(agent: &Bridge, human: &Client, roots: &[&str]) {
    let (out, err) = call_tool(
        agent,
        "request_scope",
        serde_json::json!({"roots": roots, "ops": ["copy"], "ttl_ms": 60000}),
    )
    .await;
    assert!(!err, "{out}");
    let req_id = serde_json::from_str::<serde_json::Value>(&out).expect("json")["request_id"]
        .as_u64()
        .expect("request_id");
    let _: norte_proto::methods::GrantScopeResult = human
        .call(
            norte_proto::methods::POLICY_GRANT_SCOPE,
            &norte_proto::methods::GrantScopeParams { request_id: req_id },
        )
        .await
        .expect("grant");
}

/// The agent sees WHAT differs, with the wire's vocabulary and not with
/// translated labels: a tool result that changes with the operator's
/// language is not a contract.
#[tokio::test]
async fn compare_devuelve_las_filas_con_valores_de_wire() {
    let (_dir, socket, mem) = spawn_ask_daemon().await;
    mem.mkdir(&vp("mem:///a")).await.expect("mkdir a");
    mem.mkdir(&vp("mem:///b")).await.expect("mkdir b");
    write_file(&mem, "mem:///a/x.txt", b"hola").await;

    let mut human = Client::connect(&socket).await.expect("human connect");
    human
        .initialize(norte_proto::methods::ClientInfo {
            name: "tui".into(),
            version: "0".into(),
        })
        .await
        .expect("human initialize");
    let agent = Bridge::connect(&socket, "claude")
        .await
        .expect("agent connect");
    grant_read_scope(&agent, &human, &["mem:///a", "mem:///b"]).await;

    let (out, err) = call_tool(
        &agent,
        "compare",
        serde_json::json!({"left": "mem:///a", "right": "mem:///b"}),
    )
    .await;
    assert!(!err, "{out}");
    let payload: serde_json::Value = serde_json::from_str(&out).expect("json");
    let rows = payload["rows"].as_array().expect("rows");
    assert!(!rows.is_empty(), "x.txt is on only one side");
    // The verdict travels as a WIRE value (`only_left`, verified against
    // `CompareVerdict`'s serde), not as `left_only` — the plan guessed it
    // wrong.
    assert!(
        rows.iter().any(|f| f["verdict"] == "only_left"),
        "the verdict travels as a wire value: {payload}"
    );
    assert_eq!(payload["truncated"], false, "{payload}");
    assert_eq!(payload["complete"], true, "{payload}");
}

/// The row cap cuts the comparison, CANCELS it, and SAYS so: a silent
/// truncation would be worse than the cap — a model reading it as complete
/// would report two trees as equal without having seen them whole.
#[tokio::test]
async fn compare_con_limit_bajo_trunca_y_cancela() {
    let (_dir, socket, mem) = spawn_ask_daemon().await;
    // A tree that is SLOW on purpose: over five plain files the walk ends
    // before the cancellation arrives and the task comes out `Completed`,
    // so the test proved nothing about the cancellation (it uncovered this
    // itself when it started looking at the daemon's state). With one
    // `list` per subdirectory there are ~20 operations still ahead of the
    // cutoff.
    seed_slow_pair(&mem, 12, Duration::from_millis(50)).await;

    let mut human = Client::connect(&socket).await.expect("human connect");
    human
        .initialize(norte_proto::methods::ClientInfo {
            name: "tui".into(),
            version: "0".into(),
        })
        .await
        .expect("human initialize");
    let agent = Bridge::connect(&socket, "claude")
        .await
        .expect("agent connect");
    grant_read_scope(&agent, &human, &["mem:///a", "mem:///b"]).await;

    let (out, err) = call_tool(
        &agent,
        "compare",
        serde_json::json!({"left": "mem:///a", "right": "mem:///b", "limit": 2}),
    )
    .await;
    assert!(!err, "{out}");
    let payload: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(
        payload["rows"].as_array().expect("rows").len(),
        2,
        "{payload}"
    );
    assert_eq!(payload["truncated"], true, "{payload}");
    assert_eq!(
        payload["complete"], false,
        "truncated is never complete: {payload}"
    );
    // And the DAEMON's task was really cancelled (hard rule 3): without
    // this the test only proved the payload says so.
    let task_id = norte_proto::TaskId::new(payload["task_id"].as_u64().expect("task_id"));
    assert_eq!(
        wait_task_state(&human, task_id).await,
        norte_proto::TaskState::Cancelled,
        "truncating CANCELS the walk, not just stops reading it: {payload}"
    );
}

/// Rule 1 in `compare` (encoding-auditor, task 2 review): a hostile name
/// that is born as BYTES in the provider travels all the way to the tool's
/// row as a faithful `to_wire()`, never lossy — full norte-testkit corpus,
/// same as `nombre_hostil_round_trip_byte_fiel_por_el_puente` covers
/// `list_dir`.
#[tokio::test]
async fn compare_nombre_hostil_viaja_como_wire_fiel() {
    let (_dir, socket, mem) = spawn_ask_daemon().await;
    mem.mkdir(&vp("mem:///a")).await.expect("mkdir a");
    mem.mkdir(&vp("mem:///b")).await.expect("mkdir b");

    let corpus = norte_testkit::corpus::hostile_names();
    for n in &corpus {
        let seg = norte_proto::Segment::new(n.bytes.clone()).expect("corpus segment");
        let src = vp("mem:///a").join(seg);
        let mut sink = mem.write(&src).await.expect("write");
        sink.write(Bytes::from_static(b"x")).await.expect("chunk");
        sink.commit().await.expect("commit");
    }

    let mut human = Client::connect(&socket).await.expect("human connect");
    human
        .initialize(norte_proto::methods::ClientInfo {
            name: "tui".into(),
            version: "0".into(),
        })
        .await
        .expect("human initialize");
    let agent = Bridge::connect(&socket, "claude")
        .await
        .expect("agent connect");
    grant_read_scope(&agent, &human, &["mem:///a", "mem:///b"]).await;

    // Generous cap: the corpus does not come close to the default (500),
    // and this tests the path WITHOUT truncation (truncation has its own
    // test already).
    let (out, err) = call_tool(
        &agent,
        "compare",
        serde_json::json!({"left": "mem:///a", "right": "mem:///b", "limit": 1000}),
    )
    .await;
    assert!(!err, "{out}");
    let payload: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(payload["truncated"], false, "{payload}");
    let rows = payload["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), corpus.len(), "one row per name: {payload}");

    for n in &corpus {
        let seg = norte_proto::Segment::new(n.bytes.clone()).expect("corpus segment");
        let src = vp("mem:///a").join(seg);
        let row = rows
            .iter()
            .find(|f| {
                f["left"]["path"]
                    .as_str()
                    .is_some_and(|w| VPath::parse(w).is_ok_and(|p| p == src))
            })
            .unwrap_or_else(|| {
                panic!(
                    "{}: compare does not carry the faithful row: {payload}",
                    n.id
                )
            });
        // Exists only on the left: the wire verdict says so (`only_left`,
        // not `left_only` — the plan guessed it wrong). UNLESS two corpus
        // entries collapse to the same matching key on THIS side (e.g.
        // NFC/NFD, `nfd_e_acute` against its composed form): there the
        // verdict is `ambiguous` with `side: left` — still ONE faithful row
        // per name, which is what this test checks.
        let verdict = row["verdict"].as_str().expect("verdict");
        assert!(
            verdict == "only_left" || verdict == "ambiguous",
            "{}: unexpected verdict: {row}",
            n.id
        );
        if verdict == "ambiguous" {
            assert_eq!(row["side"], "left", "{}: {row}", n.id);
        }
        let wire = row["left"]["path"].as_str().expect("path");
        assert!(
            !wire.contains('\u{FFFD}'),
            "{}: lossy on the wire: {wire}",
            n.id
        );
    }
}

/// The agent sees WHAT a synchronisation would do, and the tool's
/// description tells it the hash is of no use to anyone else.
#[tokio::test]
async fn sync_plan_devuelve_los_pasos_y_no_aplica_nada() {
    let (_dir, socket, mem) = spawn_ask_daemon().await;
    mem.mkdir(&vp("mem:///src")).await.expect("mkdir src");
    mem.mkdir(&vp("mem:///dst")).await.expect("mkdir dst");
    write_file(&mem, "mem:///src/nuevo.txt", b"hola").await;

    let mut human = Client::connect(&socket).await.expect("human connect");
    human
        .initialize(norte_proto::methods::ClientInfo {
            name: "tui".into(),
            version: "0".into(),
        })
        .await
        .expect("human initialize");
    let agent = Bridge::connect(&socket, "claude")
        .await
        .expect("agent connect");
    grant_read_scope(&agent, &human, &["mem:///src", "mem:///dst"]).await;

    let (out, err) = call_tool(
        &agent,
        "sync_plan",
        serde_json::json!({"source": "mem:///src", "dest": "mem:///dst", "mode": "update"}),
    )
    .await;
    assert!(!err, "{out}");
    let payload: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(
        payload["steps"].as_array().expect("steps").len(),
        1,
        "{payload}"
    );
    assert_eq!(payload["truncated"], false, "{payload}");
    assert_eq!(payload["complete"], true, "{payload}");
    assert!(
        payload["counts"].is_object(),
        "the totals the core does know"
    );
    assert!(payload.get("dest_trash").is_some(), "{payload}");
    assert!(payload["blockers"].is_array(), "{payload}");
    // The hash does NOT travel: it is of no use to anyone outside the
    // streams connection.
    assert!(payload.get("plan_hash").is_none(), "{payload}");
    // The `task_id` DOES: the bridge's two connections are the same agent
    // actor, so the tools one can observe and cancel the task the streams
    // arm opened (`task_status`).
    let task_id = payload["task_id"].as_u64().expect("task_id");
    assert!(task_id > 0, "{payload}");
    let (state, err) = call_tool(
        &agent,
        "task_status",
        serde_json::json!({"task_id": task_id}),
    )
    .await;
    assert!(!err, "{state}");
    let state: serde_json::Value = serde_json::from_str(&state).expect("json");
    assert_eq!(state["state"], "completed", "{state}");
    // And the steps match what the plan's counters say there are.
    assert_eq!(payload["steps_total"], 1, "{payload}");

    // And the destination stays empty: planning does not write.
    assert!(
        mem.stat(&vp("mem:///dst/nuevo.txt")).await.is_err(),
        "sync_plan applies nothing"
    );
}

/// There is no apply tool, and that is the decision, not an oversight.
#[tokio::test]
async fn no_existe_una_tool_de_aplicar() {
    let (_dir, socket, _mem) = spawn_ask_daemon().await;
    let agent = Bridge::connect(&socket, "claude")
        .await
        .expect("agent connect");
    let out = agent
        .handle_line(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
        .await
        .expect("response");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let defs = v["result"]["tools"].as_array().expect("tools");
    assert!(
        defs.iter().any(|t| t["name"] == "sync_plan"),
        "sync_plan has to be there: {v}"
    );
    assert!(
        !defs.iter().any(|t| t["name"] == "sync_apply"),
        "applying is a human's action in their own client (spec 3 §2.1): {v}"
    );
}

/// `mode` absent or of an illegal type/value is an error, NEVER a silent
/// default — same precedent as `tool_delete::mode` (spec 3, task 3): the
/// wire has no neutral value between `update` and `mirror` either.
#[tokio::test]
async fn sync_plan_mode_malformado_o_ausente_es_error_no_default() {
    let (_dir, socket, mem) = spawn_ask_daemon().await;
    mem.mkdir(&vp("mem:///src")).await.expect("mkdir src");
    mem.mkdir(&vp("mem:///dst")).await.expect("mkdir dst");

    let mut human = Client::connect(&socket).await.expect("human connect");
    human
        .initialize(norte_proto::methods::ClientInfo {
            name: "tui".into(),
            version: "0".into(),
        })
        .await
        .expect("human initialize");
    let agent = Bridge::connect(&socket, "claude")
        .await
        .expect("agent connect");
    grant_read_scope(&agent, &human, &["mem:///src", "mem:///dst"]).await;

    // Absent: does NOT silently fall back to `update`.
    let (out, err) = call_tool(
        &agent,
        "sync_plan",
        serde_json::json!({"source": "mem:///src", "dest": "mem:///dst"}),
    )
    .await;
    assert!(err, "{out}");
    assert!(out.contains("mode"), "{out}");

    // Illegal type.
    let (out, err) = call_tool(
        &agent,
        "sync_plan",
        serde_json::json!({"source": "mem:///src", "dest": "mem:///dst", "mode": 7}),
    )
    .await;
    assert!(err, "{out}");
    assert!(out.contains("invalid mode"), "{out}");

    // Illegal string value (neither of the wire's two).
    let (out, err) = call_tool(
        &agent,
        "sync_plan",
        serde_json::json!({"source": "mem:///src", "dest": "mem:///dst", "mode": "obliterate"}),
    )
    .await;
    assert!(err, "{out}");
    assert!(out.contains("invalid mode"), "{out}");
}

/// The step cap cuts the plan, CANCELS it, and the payload says so without
/// inventing anything: `sync.plan_done` does not arrive after cancelling
/// (`run_sync_plan` does not emit it on the error path), so
/// `counts`/`dest_trash`/`blockers` have to stay ABSENT — never zero, which
/// a model would read as "no blockers" when it really is not known.
#[tokio::test]
async fn sync_plan_con_limit_bajo_trunca_y_no_trae_lo_que_no_supo() {
    let (_dir, socket, mem) = spawn_ask_daemon().await;
    // A SLOW source for the same reason as in
    // `compare_con_limit_bajo_trunca_y_cancela`: over five plain files the
    // plan ends before the cancellation arrives. The planner DOES descend
    // orphans on the source side, so an empty destination does not save it
    // the walk.
    seed_tree(&mem, "mem:///src", 12).await;
    mem.mkdir(&vp("mem:///dst")).await.expect("mkdir dst");
    slow_down(&mem, Duration::from_millis(50));

    let mut human = Client::connect(&socket).await.expect("human connect");
    human
        .initialize(norte_proto::methods::ClientInfo {
            name: "tui".into(),
            version: "0".into(),
        })
        .await
        .expect("human initialize");
    let agent = Bridge::connect(&socket, "claude")
        .await
        .expect("agent connect");
    grant_read_scope(&agent, &human, &["mem:///src", "mem:///dst"]).await;

    let (out, err) = call_tool(
        &agent,
        "sync_plan",
        serde_json::json!({"source": "mem:///src", "dest": "mem:///dst", "mode": "update", "limit": 2}),
    )
    .await;
    assert!(!err, "{out}");
    let payload: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(
        payload["steps"].as_array().expect("steps").len(),
        2,
        "{payload}"
    );
    assert_eq!(payload["truncated"], true, "{payload}");
    assert_eq!(
        payload["complete"], false,
        "truncated is never complete: {payload}"
    );
    for absent in [
        "counts",
        "dest_trash",
        "blockers",
        "blockers_total",
        "executable",
    ] {
        assert!(
            payload.get(absent).is_none(),
            "{absent} cannot be invented after truncating: {payload}"
        );
    }
    assert!(payload.get("plan_hash").is_none(), "{payload}");
    // And the DAEMON's task was really cancelled (hard rule 3).
    let task_id = norte_proto::TaskId::new(payload["task_id"].as_u64().expect("task_id"));
    assert_eq!(
        wait_task_state(&human, task_id).await,
        norte_proto::TaskState::Cancelled,
        "truncating CANCELS the plan, not just stops reading it: {payload}"
    );

    // And the destination stays empty: truncated does not apply anything either.
    for i in 0..12 {
        assert!(
            mem.stat(&vp(&format!("mem:///dst/d{i}"))).await.is_err(),
            "sync_plan applies nothing, not even truncated"
        );
    }
}

/// BLOCKER: `criteria: []` is NOT "the wire default". It turns off all
/// three rungs, and then `compare` calls two trees equal without having
/// compared them and `sync_plan` —with its default `on_unknown: copy`—
/// plans an `Overwrite` per file. Rejected in BOTH tools, with the same
/// criterion as `mode`: an empty argument cannot be the short way to ask
/// for the whole destination to be rewritten.
#[tokio::test]
async fn criteria_vacia_es_error_en_las_dos_tools() {
    let (_dir, socket, mem) = spawn_ask_daemon().await;
    mem.mkdir(&vp("mem:///a")).await.expect("mkdir a");
    mem.mkdir(&vp("mem:///b")).await.expect("mkdir b");
    write_file(&mem, "mem:///a/x.txt", b"hola").await;
    write_file(&mem, "mem:///b/x.txt", b"adios").await;

    let mut human = Client::connect(&socket).await.expect("human connect");
    human
        .initialize(norte_proto::methods::ClientInfo {
            name: "tui".into(),
            version: "0".into(),
        })
        .await
        .expect("human initialize");
    let agent = Bridge::connect(&socket, "claude")
        .await
        .expect("agent connect");
    grant_read_scope(&agent, &human, &["mem:///a", "mem:///b"]).await;

    let (out, err) = call_tool(
        &agent,
        "compare",
        serde_json::json!({"left": "mem:///a", "right": "mem:///b", "criteria": []}),
    )
    .await;
    assert!(err, "an empty criteria list must not pass: {out}");
    assert!(out.contains("criteria"), "{out}");

    let (out, err) = call_tool(
        &agent,
        "sync_plan",
        serde_json::json!({
            "source": "mem:///a", "dest": "mem:///b",
            "mode": "update", "criteria": []
        }),
    )
    .await;
    assert!(err, "and even less so in sync_plan: {out}");
    assert!(out.contains("criteria"), "{out}");

    // And without `criteria` it DOES compare: x.txt's difference shows up.
    let (out, err) = call_tool(
        &agent,
        "compare",
        serde_json::json!({"left": "mem:///a", "right": "mem:///b"}),
    )
    .await;
    assert!(!err, "{out}");
    let payload: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert!(
        payload["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .any(|f| f["verdict"] == "different"),
        "the wire default DOES compare: {payload}"
    );
}

/// `limit: 0` is a nonsensical argument, not "zero rows on purpose": without
/// the rejection it would come out `truncated: true` with an empty list,
/// indistinguishable from a genuinely truncated tree.
#[tokio::test]
async fn limit_cero_es_error_en_las_dos_tools() {
    let (_dir, socket, mem) = spawn_ask_daemon().await;
    mem.mkdir(&vp("mem:///a")).await.expect("mkdir a");
    mem.mkdir(&vp("mem:///b")).await.expect("mkdir b");

    let mut human = Client::connect(&socket).await.expect("human connect");
    human
        .initialize(norte_proto::methods::ClientInfo {
            name: "tui".into(),
            version: "0".into(),
        })
        .await
        .expect("human initialize");
    let agent = Bridge::connect(&socket, "claude")
        .await
        .expect("agent connect");
    grant_read_scope(&agent, &human, &["mem:///a", "mem:///b"]).await;

    for (name, args) in [
        (
            "compare",
            serde_json::json!({"left": "mem:///a", "right": "mem:///b", "limit": 0}),
        ),
        (
            "sync_plan",
            serde_json::json!({
                "source": "mem:///a", "dest": "mem:///b", "mode": "update", "limit": 0
            }),
        ),
    ] {
        let (out, err) = call_tool(&agent, name, args).await;
        assert!(err, "{name}: {out}");
        assert!(out.contains("limit"), "{name}: {out}");
    }
}

/// Seeds a tree of `dirs` sibling subdirectories with one file in each. With
/// per-operation latency (see [`slow_down`]) every subdirectory costs a
/// `list`, so the daemon's walk lasts long enough to be observed alive and
/// acted on WITHOUT guessing timings: `Faults` is deterministic, not
/// probabilistic.
async fn seed_tree(mem: &MemProvider, root: &str, dirs: usize) {
    mem.mkdir(&vp(root)).await.expect("mkdir root");
    for i in 0..dirs {
        mem.mkdir(&vp(&format!("{root}/d{i}")))
            .await
            .expect("mkdir child");
        write_file(mem, &format!("{root}/d{i}/f.txt"), b"x").await;
    }
}

/// Fixed per-operation provider latency, applied AFTER seeding (seeding with
/// it on would only make the test longer).
fn slow_down(mem: &MemProvider, latency: Duration) {
    mem.faults().set_latency_per_op(Some(latency));
}

/// Two IDENTICAL, slow trees: the walk has to cover both sides.
async fn seed_slow_pair(mem: &MemProvider, dirs: usize, latency: Duration) {
    seed_tree(mem, "mem:///a", dirs).await;
    seed_tree(mem, "mem:///b", dirs).await;
    slow_down(mem, latency);
}

/// Waits for a LIVE Task of class `kind` to exist and returns its id. The
/// human sees all of them (the daemon's visibility criterion).
async fn wait_live_task(human: &Client, kind: norte_proto::TaskKind) -> norte_proto::TaskId {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let listed: norte_proto::methods::TaskListResult = human
            .call(
                norte_proto::methods::TASK_LIST,
                &norte_proto::methods::TaskListParams {},
            )
            .await
            .expect("task.list");
        if let Some(t) = listed
            .tasks
            .iter()
            .find(|t| t.kind == kind && !t.state.is_terminal())
        {
            return t.task_id;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no live {kind:?} task"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Waits for `task_id`'s terminal and returns its state.
async fn wait_task_state(human: &Client, task_id: norte_proto::TaskId) -> norte_proto::TaskState {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let listed: norte_proto::methods::TaskListResult = human
            .call(
                norte_proto::methods::TASK_LIST,
                &norte_proto::methods::TaskListParams {},
            )
            .await
            .expect("task.list");
        if let Some(t) = listed.tasks.iter().find(|t| t.task_id == task_id)
            && t.state.is_terminal()
        {
            return t.state.clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "task {task_id:?} did not finish"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// MAJOR: an agent that ABANDONS the tool (`notifications/cancelled`, or its
/// transport dying) used to leave the walk running. `TaskRef` has no
/// `Drop`, and dropping the local `rx` only removes the client's route: the
/// daemon's pump keeps sending batches over a live connection and never
/// sees a `ReceiverGone`. With `criteria: ["hash"]` that is reading both
/// WHOLE trees for nobody, and repeating it fills `MAX_LIVE_TASKS_AGENTS`
/// without leaving a trace in the journal (nothing mutates). The bridge
/// already cancelled on truncation: it is the same fact.
#[tokio::test]
async fn una_tool_abandonada_cancela_el_walk_del_daemon() {
    let (_dir, socket, mem) = spawn_ask_daemon().await;
    seed_slow_pair(&mem, 12, Duration::from_millis(50)).await;

    let mut human = Client::connect(&socket).await.expect("human connect");
    human
        .initialize(norte_proto::methods::ClientInfo {
            name: "tui".into(),
            version: "0".into(),
        })
        .await
        .expect("human initialize");
    let agent = Arc::new(Bridge::connect(&socket, "claude").await.expect("agent"));
    grant_read_scope(&agent, &human, &["mem:///a", "mem:///b"]).await;

    let tool = {
        let agent = Arc::clone(&agent);
        tokio::spawn(async move {
            call_tool(
                &agent,
                "compare",
                serde_json::json!({"left": "mem:///a", "right": "mem:///b"}),
            )
            .await
        })
    };
    // Abandoned with the task ALREADY alive: no guessing timings.
    let task_id = wait_live_task(&human, norte_proto::TaskKind::Compare).await;
    tool.abort();

    assert_eq!(
        wait_task_state(&human, task_id).await,
        norte_proto::TaskState::Cancelled,
        "dropping the tool's future has to cancel the walk"
    );
}

/// A third party (the human governing the daemon) cancels the comparison
/// halfway. The tool answers with what it drained, and SAYS so:
/// `complete: false` and `state: "cancelled"`. Without the state, a model
/// looking only at `rows` would read "no differences" into a list that is
/// only halfway there.
#[tokio::test]
async fn una_comparacion_cancelada_por_un_tercero_no_finge_estar_completa() {
    let (_dir, socket, mem) = spawn_ask_daemon().await;
    seed_slow_pair(&mem, 12, Duration::from_millis(50)).await;

    let mut human = Client::connect(&socket).await.expect("human connect");
    human
        .initialize(norte_proto::methods::ClientInfo {
            name: "tui".into(),
            version: "0".into(),
        })
        .await
        .expect("human initialize");
    let agent = Arc::new(Bridge::connect(&socket, "claude").await.expect("agent"));
    grant_read_scope(&agent, &human, &["mem:///a", "mem:///b"]).await;

    let tool = {
        let agent = Arc::clone(&agent);
        tokio::spawn(async move {
            call_tool(
                &agent,
                "compare",
                serde_json::json!({"left": "mem:///a", "right": "mem:///b"}),
            )
            .await
        })
    };
    let task_id = wait_live_task(&human, norte_proto::TaskKind::Compare).await;
    let _: norte_proto::methods::TaskCancelResult = human
        .call(
            norte_proto::methods::TASK_CANCEL,
            &norte_proto::methods::TaskCancelParams { task_id },
        )
        .await
        .expect("task.cancel");

    let (out, err) = tool.await.expect("the tool answers");
    assert!(!err, "{out}");
    let payload: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(
        payload["truncated"], false,
        "not truncated by the cap: {payload}"
    );
    assert_eq!(payload["timed_out"], false, "{payload}");
    assert_eq!(payload["state"], "cancelled", "{payload}");
    assert_eq!(
        payload["complete"], false,
        "a comparison cut short is not complete: {payload}"
    );
}

/// MAJOR: a connection's 17th retained plan comes out of the daemon as
/// `OVERLOADED` with a clear message, and it used to reach the agent as
/// "internal error (panic: false)" — which is exactly the text that makes
/// it retry. The bridge has ONE streams connection for the whole process,
/// cannot apply and has no method to discard, so the cap is reached in
/// normal use.
#[tokio::test]
async fn el_tope_de_planes_retenidos_no_sale_como_error_interno() {
    let (_dir, socket, mem) = spawn_ask_daemon().await;
    mem.mkdir(&vp("mem:///dst")).await.expect("mkdir dst");
    // 17 DIFFERENT sources: every plan needs its own digest, or the spool
    // would not retain seventeen.
    let mut roots: Vec<String> = vec!["mem:///dst".to_owned()];
    for i in 0..17 {
        let root = format!("mem:///src{i}");
        mem.mkdir(&vp(&root)).await.expect("mkdir src");
        write_file(&mem, &format!("{root}/f{i}.txt"), b"x").await;
        roots.push(root);
    }

    let mut human = Client::connect(&socket).await.expect("human connect");
    human
        .initialize(norte_proto::methods::ClientInfo {
            name: "tui".into(),
            version: "0".into(),
        })
        .await
        .expect("human initialize");
    let agent = Bridge::connect(&socket, "claude")
        .await
        .expect("agent connect");
    let refs: Vec<&str> = roots.iter().map(String::as_str).collect();
    grant_read_scope(&agent, &human, &refs).await;

    let mut last = String::new();
    let mut failure = None;
    for i in 0..17 {
        let (out, err) = call_tool(
            &agent,
            "sync_plan",
            serde_json::json!({
                "source": format!("mem:///src{i}"), "dest": "mem:///dst", "mode": "update"
            }),
        )
        .await;
        last = out;
        if err {
            failure = Some(i);
            break;
        }
    }
    let i = failure.unwrap_or_else(|| panic!("the 16-plan cap had to trigger: {last}"));
    assert_eq!(i, 16, "triggers on the 17th, not before: {last}");
    assert!(
        !last.contains("internal error"),
        "\"internal error\" is what makes an agent retry: {last}"
    );
    assert!(last.contains("retained plans"), "{last}");
}
