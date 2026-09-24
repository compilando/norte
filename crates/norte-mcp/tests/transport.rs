//! #67: the bridge's transport is NOT serial — a suspended tool (a policy
//! ask / a long task) does not hold up `ping` or `notifications/cancelled`.
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
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

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

/// In-process `ask` daemon (same harness as `e2e_m3`).
async fn spawn_ask_daemon() -> (tempfile::TempDir, std::path::PathBuf, Arc<MemProvider>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let scopes = ScopeRegistry::new();
    let cfg = PolicyConfig::parse("[[rule]]\naction=\"ask\"").expect("policy");
    let approvals = Arc::new(DaemonApprovalResolver::new(Duration::from_secs(30)));
    let engine = Arc::new(Engine::new().with_policy(
        Arc::new(ScopedPolicy::new(scopes.clone(), cfg)),
        Arc::clone(&approvals) as _,
    ));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&mem, "mem:///proj/a.txt", b"datos").await;
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

/// Starts the bridge's transport over a duplex pair and returns the "MCP
/// client" ends: (line writer, line reader).
async fn spawn_transport(
    socket: &std::path::Path,
) -> (
    tokio::io::WriteHalf<tokio::io::DuplexStream>,
    BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
) {
    let bridge = Bridge::connect(socket, "claude").await.expect("bridge");
    let (client_side, server_side) = tokio::io::duplex(64 * 1024);
    let (srv_r, srv_w) = tokio::io::split(server_side);
    tokio::spawn(async move {
        let _ = norte_mcp::bridge::serve_transport(bridge, BufReader::new(srv_r), srv_w).await;
    });
    let (cli_r, cli_w) = tokio::io::split(client_side);
    (cli_w, BufReader::new(cli_r))
}

async fn send_line(w: &mut tokio::io::WriteHalf<tokio::io::DuplexStream>, v: &serde_json::Value) {
    w.write_all(v.to_string().as_bytes()).await.expect("write");
    w.write_all(b"\n").await.expect("nl");
}

async fn read_json(
    r: &mut BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
) -> serde_json::Value {
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(3), r.read_line(&mut line))
        .await
        .expect("response before the timeout")
        .expect("io");
    serde_json::from_str(&line).expect("json")
}

/// Grants copy scope to the `claude` session via the transport itself + a
/// human going directly over the wire.
async fn grant_scope_via_transport(
    w: &mut tokio::io::WriteHalf<tokio::io::DuplexStream>,
    r: &mut BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    socket: &std::path::Path,
) -> Client {
    let mut human = Client::connect(socket).await.expect("human");
    human
        .initialize(norte_proto::methods::ClientInfo {
            name: "tui".into(),
            version: "0".into(),
        })
        .await
        .expect("human init");
    send_line(
        w,
        &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
            "name":"request_scope",
            "arguments":{"roots":["mem:///proj"],"ops":["copy"],"ttl_ms":60_000}}}),
    )
    .await;
    let resp = read_json(r).await;
    let text = resp["result"]["content"][0]["text"].as_str().expect("text");
    let req_id = serde_json::from_str::<serde_json::Value>(text).expect("json")["request_id"]
        .as_u64()
        .expect("request_id");
    let _: norte_proto::methods::GrantScopeResult = human
        .call(
            norte_proto::methods::POLICY_GRANT_SCOPE,
            &norte_proto::methods::GrantScopeParams { request_id: req_id },
        )
        .await
        .expect("grant");
    human
}

/// DETERMINISTIC wait for the copy to be genuinely suspended in its Ask:
/// polls `policy.pending` until it sees it non-empty and returns the
/// `approval_id`. Never a fixed sleep: "the copy has already reached the
/// Ask" is daemon state, and 100 ms over a local UDS was a bet that lost
/// under load (wave W10).
async fn esperar_ask(human: &Client) -> u64 {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let res: norte_proto::methods::PolicyPendingResult = human
            .call(norte_proto::methods::POLICY_PENDING, &serde_json::json!({}))
            .await
            .expect("policy.pending");
        if let Some(p) = res.pending.first() {
            return p.approval_id;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the Ask never showed up in policy.pending"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn copy_call(id: u64) -> serde_json::Value {
    serde_json::json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
        "name":"copy",
        "arguments":{"from":"mem:///proj/a.txt","to":"mem:///proj/b.txt"}}})
}

/// #67: with a `copy` SUSPENDED in the ask, a concurrent `ping` answers just
/// the same — the MCP client's keep-alive does not declare the bridge dead.
#[tokio::test]
async fn ping_responde_con_un_tool_suspendido_en_vuelo() {
    let (_dir, socket, _mem) = spawn_ask_daemon().await;
    let (mut w, mut r) = spawn_transport(&socket).await;
    let human = grant_scope_via_transport(&mut w, &mut r, &socket).await;

    // The copy stays suspended in the Ask (nobody decides).
    send_line(&mut w, &copy_call(10)).await;
    esperar_ask(&human).await;
    // The ping MUST answer even while the copy is still in flight.
    send_line(
        &mut w,
        &serde_json::json!({"jsonrpc":"2.0","id":11,"method":"ping"}),
    )
    .await;
    let resp = read_json(&mut r).await;
    assert_eq!(
        resp["id"], 11,
        "the ping answers BEFORE the copy resolves: {resp}"
    );
}

/// #67: `notifications/cancelled` abandons the in-flight tool WITHOUT a
/// response (MCP spec); the transport stays alive for what comes next.
#[tokio::test]
async fn cancelled_abandona_el_tool_en_vuelo_sin_respuesta() {
    let (_dir, socket, _mem) = spawn_ask_daemon().await;
    let (mut w, mut r) = spawn_transport(&socket).await;
    let human = grant_scope_via_transport(&mut w, &mut r, &socket).await;

    send_line(&mut w, &copy_call(10)).await;
    esperar_ask(&human).await;
    send_line(
        &mut w,
        &serde_json::json!({"jsonrpc":"2.0","method":"notifications/cancelled",
            "params":{"requestId":10}}),
    )
    .await;
    // After cancelling, a ping answers and NOTHING arrives for id 10 first.
    send_line(
        &mut w,
        &serde_json::json!({"jsonrpc":"2.0","id":12,"method":"ping"}),
    )
    .await;
    let resp = read_json(&mut r).await;
    assert_eq!(
        resp["id"], 12,
        "the first thing after the cancel is the ping — id 10 does not answer: {resp}"
    );
}

/// #72 (e2e): the agent cancels its tools/call suspended in an Ask → the
/// bridge forwards rpc.cancel → the daemon WITHDRAWS the Ask. The human can
/// NO LONGER approve-to-execute: policy.pending stays empty and a late
/// policy.decide does not create the destination. (≠ #67, which only
/// abandoned the local wait, leaving the Ask zombie until the TTL.)
#[tokio::test]
async fn cancel_del_agente_retira_el_ask_el_humano_no_ejecuta() {
    let (_dir, socket, mem) = spawn_ask_daemon().await;
    let (mut w, mut r) = spawn_transport(&socket).await;
    let human = grant_scope_via_transport(&mut w, &mut r, &socket).await;

    // The agent launches the copy: it stays suspended in the Ask (nobody
    // has decided yet).
    send_line(&mut w, &copy_call(10)).await;

    let approval_id = esperar_ask(&human).await;

    // The agent CANCELS its in-flight tools/call.
    send_line(
        &mut w,
        &serde_json::json!({"jsonrpc":"2.0","method":"notifications/cancelled",
            "params":{"requestId":10}}),
    )
    .await;

    // The Ask must be WITHDRAWN: policy.pending goes back to empty. Polled
    // with a deadline.
    {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let res: norte_proto::methods::PolicyPendingResult = human
                .call(norte_proto::methods::POLICY_PENDING, &serde_json::json!({}))
                .await
                .expect("policy.pending");
            if res.pending.is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the Ask stayed ZOMBIE: the rpc.cancel was not forwarded (#72)"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    // A LATE policy.decide must not execute anything (the pending one no
    // longer exists): either Err or Ok is acceptable — what matters is the
    // effect on the FS.
    let _ = human
        .call::<_, norte_proto::methods::PolicyDecideResult>(
            norte_proto::methods::POLICY_DECIDE,
            &norte_proto::methods::PolicyDecideParams {
                approval_id,
                approve: true,
            },
        )
        .await;

    // Lets any wrongful execution surface before checking the FS.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        matches!(
            mem.stat(&vp("mem:///proj/b.txt")).await,
            Err(norte_proto::Error::NotFound)
        ),
        "b.txt must not exist after the Ask was withdrawn"
    );

    // The transport is still alive after the whole exchange.
    send_line(
        &mut w,
        &serde_json::json!({"jsonrpc":"2.0","id":12,"method":"ping"}),
    )
    .await;
    let resp = read_json(&mut r).await;
    assert_eq!(
        resp["id"], 12,
        "the transport is still alive after the cancel: {resp}"
    );
}

/// #67 (rule 3): the peer's EOF with a SUSPENDED tool ends the transport
/// cleanly — cancels what is in flight, drains the writer, returns within a
/// timeout (never hangs on the writer's join).
#[tokio::test]
async fn eof_con_tool_en_vuelo_termina_limpio() {
    let (_dir, socket, _mem) = spawn_ask_daemon().await;
    let bridge = Bridge::connect(&socket, "claude").await.expect("bridge");
    let (client_side, server_side) = tokio::io::duplex(64 * 1024);
    let (srv_r, srv_w) = tokio::io::split(server_side);
    let served = tokio::spawn(async move {
        norte_mcp::bridge::serve_transport(bridge, BufReader::new(srv_r), srv_w).await
    });
    let (cli_r, mut cli_w) = tokio::io::split(client_side);
    let mut r = BufReader::new(cli_r);

    // Grants scope and launches a copy that stays suspended in the Ask.
    let human = grant_scope_via_transport(&mut cli_w, &mut r, &socket).await;
    send_line(&mut cli_w, &copy_call(10)).await;
    esperar_ask(&human).await;

    // The peer DIES (drops its end) with the copy in flight.
    drop(cli_w);
    drop(r);

    // serve_transport must RETURN soon — not hang waiting on the Ask.
    let out = tokio::time::timeout(Duration::from_secs(3), served)
        .await
        .expect("serve_transport does not hang after EOF with a tool in flight")
        .expect("join");
    assert!(out.is_ok(), "clean teardown: {out:?}");
}
