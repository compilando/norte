//! E2E del CRITERIO DE SALIDA de M3 (spec §M3): "Claude Code gestiona un
//! directorio real bajo policy `ask`, con undo de sesión completa". Todo
//! in-process (daemon UDS en tempdir + puente MCP + cliente humano): el
//! flujo entero `request_scope` → grant → copy(ask) → approval → decide →
//! completed → undo → revertido, y la frontera de scope que sigue cerrada.
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
    VPath::parse(wire).expect("wire válido")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write abre");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

/// `tools/call` por el puente → `(texto, is_error)`.
async fn call_tool(b: &mut Bridge, name: &str, args: serde_json::Value) -> (String, bool) {
    let req = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": name, "arguments": args},
    });
    let out = b.handle_line(&req.to_string()).await.expect("respuesta");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let content = v["result"]["content"][0]["text"]
        .as_str()
        .expect("content")
        .to_owned();
    (content, v["result"]["isError"].as_bool().expect("isError"))
}

/// El criterio de salida de M3, extremo a extremo.
/// Daemon REAL con journal in-memory + policy `ask` + approval router, con
/// `mem:///proj/informe.txt` sembrado. Devuelve `(tempdir, socket, mem)`.
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
        },
    )
    .await
    .expect("bind");
    tokio::spawn(daemon.run());
    (dir, socket, mem)
}

/// Espera el terminal de `task_id` por `task.list` (con tope).
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
        assert!(tokio::time::Instant::now() < deadline, "el undo no terminó");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// El criterio de salida de M3, extremo a extremo.
#[tokio::test]
async fn criterio_de_salida_m3_agente_bajo_ask_con_undo() {
    let (_dir, socket, mem) = spawn_ask_daemon().await;

    // --- El HUMANO: conexión User que concede, aprueba y deshace ---
    let mut human = Client::connect(&socket).await.expect("connect humano");
    human
        .initialize(norte_proto::methods::ClientInfo {
            name: "tui".into(),
            version: "0".into(),
        })
        .await
        .expect("initialize humano");

    // --- El AGENTE: puente MCP declarando la sesión ---
    let mut agent = Bridge::connect(&socket, "claude")
        .await
        .expect("connect agente");

    // 1) El agente pide scope para su proyecto.
    let (out, err) = call_tool(
        &mut agent,
        "request_scope",
        serde_json::json!({"roots": ["mem:///proj"], "ops": ["copy"], "ttl_ms": 60000}),
    )
    .await;
    assert!(!err, "{out}");
    let req_id = serde_json::from_str::<serde_json::Value>(&out).expect("json")["request_id"]
        .as_u64()
        .expect("request_id");

    // 2) El humano lo concede.
    let _: norte_proto::methods::GrantScopeResult = human
        .call(
            norte_proto::methods::POLICY_GRANT_SCOPE,
            &norte_proto::methods::GrantScopeParams { request_id: req_id },
        )
        .await
        .expect("grant");

    // 3) El agente copia — queda SUSPENDIDA en el `ask` (spawn: el humano debe
    //    seguir atendido para aprobar).
    let copy = tokio::spawn(async move {
        let r = call_tool(
            &mut agent,
            "copy",
            serde_json::json!({"from": "mem:///proj/informe.txt", "to": "mem:///proj/copia.txt"}),
        )
        .await;
        (agent, r)
    });

    // 4) El humano recibe `policy.approval_required` y aprueba.
    let approval_id = loop {
        let n = human.notification().await.expect("canal vivo");
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

    // 5) La copia procede; el destino existe.
    let (mut agent, (out, err)) = tokio::time::timeout(Duration::from_secs(5), copy)
        .await
        .expect("no cuelga")
        .expect("join");
    assert!(!err, "aprobada debe completar: {out}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&out).expect("json")["state"],
        "completed"
    );
    assert!(mem.stat(&vp("mem:///proj/copia.txt")).await.is_ok());

    // 6) El humano deshace la sesión COMPLETA del agente → la copia se revierte,
    //    el original sigue intacto.
    let undone: norte_proto::methods::PolicyUndoSessionResult = human
        .call(
            norte_proto::methods::POLICY_UNDO_SESSION,
            &norte_proto::methods::PolicyUndoSessionParams {
                session: "claude".into(),
            },
        )
        .await
        .expect("undo_session");
    wait_undo_terminal(&human, undone.task_id).await;
    assert!(
        matches!(
            mem.stat(&vp("mem:///proj/copia.txt")).await,
            Err(norte_proto::Error::NotFound)
        ),
        "la copia del agente se revirtió"
    );
    assert!(
        mem.stat(&vp("mem:///proj/informe.txt")).await.is_ok(),
        "el original jamás se tocó"
    );

    // 7) Fuera de scope SIGUE cerrado: la concesión no fue un cheque en blanco.
    let (out, err) = call_tool(
        &mut agent,
        "copy",
        serde_json::json!({"from": "mem:///proj/informe.txt", "to": "mem:///fuera.txt"}),
    )
    .await;
    assert!(err, "fuera del scope debe fallar");
    assert!(
        out.contains("out-of-scope") && out.contains("request_scope"),
        "denegación accionable: {out}"
    );
}
