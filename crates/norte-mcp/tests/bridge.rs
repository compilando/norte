//! MCP bridge integration (M3-4 T5) against a REAL daemon in a tempdir:
//! handshake, tools/list, and every tool forwarded with the daemon's
//! governance (scope + policy) in place. The bridge is driven via
//! `handle_line` — no subprocesses or stdio.
#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use norte_core::approval::DenyAll;
use norte_core::daemon::{Daemon, DaemonConfig, DaemonError};
use norte_core::{Engine, OpSet, PolicyConfig, Scope, ScopeRegistry, ScopedPolicy};
use norte_mcp::bridge::Bridge;
use norte_proto::VPath;
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid test wire")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write opens");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk goes in");
    sink.commit().await.expect("commit publishes");
}

struct TestDaemon {
    socket: PathBuf,
    _run: tokio::task::JoinHandle<Result<(), DaemonError>>,
    _dir: tempfile::TempDir,
    mem: Arc<MemProvider>,
    /// The SAME registry the gate queries: tests grant directly (the
    /// request+grant round trip over the wire is already covered by the
    /// daemon's own tests and T7's E2E).
    scopes: ScopeRegistry,
}

/// Daemon with an in-memory journal + an `allow` rule + shared scopes
/// (`norte-core/tests/daemon.rs`'s pattern, copied — a crate does not import
/// test helpers from another's tests).
async fn spawn_daemon_allow() -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let scopes = ScopeRegistry::new();
    let cfg = PolicyConfig::parse("[[rule]]\naction=\"allow\"").expect("policy cfg");
    let policy = ScopedPolicy::new(scopes.clone(), cfg);
    let journal = Arc::new(norte_core::SqliteJournal::new(
        norte_core::Journal::open_in_memory()
            .await
            .expect("journal"),
    ));
    let engine =
        Arc::new(Engine::with_journal(journal).with_policy(Arc::new(policy), Arc::new(DenyAll)));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let daemon = Daemon::bind_with_scopes(
        engine,
        scopes.clone(),
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
    let run = tokio::spawn(daemon.run());
    TestDaemon {
        socket,
        _run: run,
        _dir: dir,
        mem,
        scopes,
    }
}

/// Grants `session` all of `mem:///proj` (directly into the shared registry).
fn grant_proj(d: &TestDaemon, session: &str) {
    d.scopes.grant(
        session,
        Scope::forever(vec![vp("mem:///proj")], OpSet::all()),
    );
}

/// `tools/call` through the bridge; returns `(text, is_error)` from the MCP
/// content.
async fn call_tool(b: &Bridge, name: &str, args: serde_json::Value) -> (String, bool) {
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 42,
        "method": "tools/call",
        "params": {"name": name, "arguments": args},
    });
    let out = b
        .handle_line(&req.to_string())
        .await
        .expect("one response per request");
    let v: serde_json::Value = serde_json::from_str(&out).expect("JSON response");
    assert_eq!(v["id"], 42, "the id is echoed verbatim");
    let content = v["result"]["content"][0]["text"]
        .as_str()
        .expect("text content")
        .to_owned();
    let is_error = v["result"]["isError"].as_bool().expect("isError present");
    (content, is_error)
}

#[tokio::test]
async fn initialize_ping_y_tools_list() {
    let d = spawn_daemon_allow().await;
    let b = Bridge::connect(&d.socket, "claude").await.expect("connect");

    let out = b
        .handle_line(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#)
        .await
        .expect("response");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(v["result"]["serverInfo"]["name"], "norte-mcp");

    // Notifications (no id) NEVER produce a response (JSON-RPC).
    assert!(
        b.handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await
            .is_none()
    );

    let out = b
        .handle_line(r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#)
        .await
        .expect("response");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["result"], serde_json::json!({}));

    let out = b
        .handle_line(r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#)
        .await
        .expect("response");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let names: Vec<&str> = v["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|t| t["name"].as_str().expect("name"))
        .collect();
    for expected in [
        "list_dir",
        "stat",
        "read_file",
        "copy",
        "move",
        "delete",
        "task_status",
        "request_scope",
    ] {
        assert!(names.contains(&expected), "missing tool {expected}");
    }

    // Unknown method → JSON-RPC error -32601; broken JSON → -32700.
    let out = b
        .handle_line(r#"{"jsonrpc":"2.0","id":4,"method":"resources/list"}"#)
        .await
        .expect("response");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["error"]["code"], -32601);
    let out = b.handle_line("{this is not json").await.expect("response");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["error"]["code"], -32700);
    assert_eq!(v["id"], serde_json::Value::Null);
}

#[tokio::test]
async fn list_dir_and_stat_read_under_scope() {
    // READS go through the scope gate just like mutations do (#80): an agent
    // reads ONLY under a granted scope. Granted here beforehand.
    let d = spawn_daemon_allow().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/a.txt", b"hola").await;
    grant_proj(&d, "claude");
    let b = Bridge::connect(&d.socket, "claude").await.expect("connect");

    let (out, err) = call_tool(&b, "list_dir", serde_json::json!({"path": "mem:///proj"})).await;
    assert!(!err, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("payload json");
    assert_eq!(v["entries"][0]["path"], "mem:///proj/a.txt");
    assert_eq!(v["entries"][0]["kind"], "file");

    let (out, err) = call_tool(&b, "stat", serde_json::json!({"path": "mem:///proj/a.txt"})).await;
    assert!(!err, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("payload json");
    assert_eq!(v["size"], 4);
}

/// #80: an agent WITHOUT scope receives the ACTIONABLE denial (mentions
/// `request_scope`) on reads, just as it already does on `copy`.
#[tokio::test]
async fn reads_without_scope_are_actionable() {
    let d = spawn_daemon_allow().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/a.txt", b"hola").await;
    let b = Bridge::connect(&d.socket, "claude").await.expect("connect");

    for (tool, args) in [
        ("list_dir", serde_json::json!({"path": "mem:///proj"})),
        ("stat", serde_json::json!({"path": "mem:///proj/a.txt"})),
        (
            "read_file",
            serde_json::json!({"path": "mem:///proj/a.txt"}),
        ),
    ] {
        let (out, err) = call_tool(&b, tool, args).await;
        assert!(err, "{tool}: must fail without scope");
        assert!(
            out.contains("out-of-scope") && out.contains("request_scope"),
            "{tool}: actionable error, was: {out}"
        );
    }
}

#[tokio::test]
async fn read_file_text_and_binary_faithful() {
    let d = spawn_daemon_allow().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/texto.txt", b"hola \xc3\xb1").await;
    write_file(&d.mem, "mem:///proj/crudo.bin", &[0x68, 0xE9, 0x00, 0xFF]).await;
    grant_proj(&d, "claude");
    let b = Bridge::connect(&d.socket, "claude").await.expect("connect");

    let (out, err) = call_tool(
        &b,
        "read_file",
        serde_json::json!({"path": "mem:///proj/texto.txt"}),
    )
    .await;
    assert!(!err, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("payload");
    assert_eq!(v["text"], "hola ñ");
    assert!(v.get("base64").is_none(), "valid UTF-8: no base64");
    assert_eq!(v["eof"], true);

    let (out, err) = call_tool(
        &b,
        "read_file",
        serde_json::json!({"path": "mem:///proj/crudo.bin"}),
    )
    .await;
    assert!(!err, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("payload");
    let faithful = base64::engine::general_purpose::STANDARD
        .decode(v["base64"].as_str().expect("base64 present"))
        .expect("valid base64");
    assert_eq!(faithful, vec![0x68, 0xE9, 0x00, 0xFF], "exact bytes");
    assert!(v["text"].as_str().expect("text").contains('\u{FFFD}'));
}

#[tokio::test]
async fn copy_with_scope_completes_and_out_of_scope_is_actionable() {
    let d = spawn_daemon_allow().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/src.txt", b"hola").await;
    let b = Bridge::connect(&d.socket, "claude").await.expect("connect");

    // No scope: the tool error is ACTIONABLE (mentions request_scope).
    let (out, err) = call_tool(
        &b,
        "copy",
        serde_json::json!({"from": "mem:///proj/src.txt", "to": "mem:///proj/dst.txt"}),
    )
    .await;
    assert!(err, "must fail without scope");
    assert!(
        out.contains("out-of-scope") && out.contains("request_scope"),
        "actionable for the agent: {out}"
    );

    // With scope: completes and the file exists.
    grant_proj(&d, "claude");
    let (out, err) = call_tool(
        &b,
        "copy",
        serde_json::json!({"from": "mem:///proj/src.txt", "to": "mem:///proj/dst.txt"}),
    )
    .await;
    assert!(!err, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("payload");
    assert_eq!(v["state"], "completed");
    assert!(d.mem.stat(&vp("mem:///proj/dst.txt")).await.is_ok());

    // task_status for the task that just finished (retained among recents).
    let task_id = v["task_id"].as_u64().expect("task_id");
    let (out, err) = call_tool(&b, "task_status", serde_json::json!({"task_id": task_id})).await;
    assert!(!err, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("payload");
    assert_eq!(v["state"], "completed");
}

#[tokio::test]
async fn delete_default_trash_y_permanent_explicito() {
    let d = spawn_daemon_allow().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/victima.txt", b"x").await;
    grant_proj(&d, "claude");
    let b = Bridge::connect(&d.socket, "claude").await.expect("connect");

    // Default trash: the testkit's logical trash accepts it — completes and
    // the victim disappears from view (RECOVERABLE: the journal records a
    // Trashed with a destination; M3-2's undo restores from there).
    let (out, err) = call_tool(
        &b,
        "delete",
        serde_json::json!({"path": "mem:///proj/victima.txt"}),
    )
    .await;
    assert!(!err, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("payload");
    assert_eq!(v["state"], "completed");
    assert!(matches!(
        d.mem.stat(&vp("mem:///proj/victima.txt")).await,
        Err(norte_proto::Error::NotFound)
    ));

    // Explicit permanent: deletes (the harness's allow policy permits it;
    // with the example policy this would be an ask/deny).
    write_file(&d.mem, "mem:///proj/victima2.txt", b"x").await;
    let (out, err) = call_tool(
        &b,
        "delete",
        serde_json::json!({"path": "mem:///proj/victima2.txt", "mode": "permanent"}),
    )
    .await;
    assert!(!err, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("payload");
    assert_eq!(v["state"], "completed");
    assert!(matches!(
        d.mem.stat(&vp("mem:///proj/victima2.txt")).await,
        Err(norte_proto::Error::NotFound)
    ));

    // Made-up mode → tool error, without calling the daemon.
    let (out, err) = call_tool(
        &b,
        "delete",
        serde_json::json!({"path": "mem:///proj/x", "mode": "shred"}),
    )
    .await;
    assert!(err);
    assert!(out.contains("invalid mode"), "{out}");
}

#[tokio::test]
async fn request_scope_returns_id_and_a_human_hint() {
    let d = spawn_daemon_allow().await;
    let b = Bridge::connect(&d.socket, "claude").await.expect("connect");
    let (out, err) = call_tool(
        &b,
        "request_scope",
        serde_json::json!({"roots": ["mem:///proj"], "ops": ["copy"], "ttl_ms": 60000}),
    )
    .await;
    assert!(!err, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("payload");
    assert!(v["request_id"].is_u64());
    assert!(
        v["hint"]
            .as_str()
            .expect("hint")
            .contains("norte policy grant"),
        "the agent knows what to ask the human: {out}"
    );
}

#[tokio::test]
async fn invalid_vpath_is_a_local_tool_error() {
    let d = spawn_daemon_allow().await;
    let b = Bridge::connect(&d.socket, "claude").await.expect("connect");
    let (out, err) = call_tool(&b, "stat", serde_json::json!({"path": "no-es-un-vpath"})).await;
    assert!(err);
    assert!(out.contains("invalid VPath"), "{out}");
    // Unknown tool → tool error (not protocol error).
    let (out, err) = call_tool(&b, "write_file", serde_json::json!({})).await;
    assert!(err);
    assert!(out.contains("unknown tool"), "{out}");
}

#[tokio::test]
async fn an_illegal_session_is_rejected_in_connect() {
    let d = spawn_daemon_allow().await;
    let Err(err) = Bridge::connect(&d.socket, "con espacios").await else {
        panic!("the daemon validates the charset in the handshake");
    };
    assert!(matches!(err, norte_mcp::bridge::BridgeError::Daemon(_)));
}

/// The `move` tool (rust M3: the only one without coverage, and now
/// serializes ITS OWN `FsMoveParams`): moves within scope and the source
/// disappears.
#[tokio::test]
async fn move_renames_within_scope() {
    let d = spawn_daemon_allow().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/viejo.txt", b"hola").await;
    grant_proj(&d, "claude");
    let b = Bridge::connect(&d.socket, "claude").await.expect("connect");

    let (out, err) = call_tool(
        &b,
        "move",
        serde_json::json!({"from": "mem:///proj/viejo.txt", "to": "mem:///proj/nuevo.txt"}),
    )
    .await;
    assert!(!err, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("payload");
    assert_eq!(v["state"], "completed");
    assert!(d.mem.stat(&vp("mem:///proj/nuevo.txt")).await.is_ok());
    assert!(matches!(
        d.mem.stat(&vp("mem:///proj/viejo.txt")).await,
        Err(norte_proto::Error::NotFound)
    ));
}

/// Rule 1 on the NEW surface (encoding H1): a hostile name that IS BORN as
/// bytes in the provider travels to the agent as wire form (%XX, never
/// lossy); the agent ECHOES it in copy and the destination has THE SAME
/// bytes. Full norte-testkit corpus.
#[tokio::test]
async fn hostile_name_round_trips_byte_faithful_through_the_bridge() {
    let d = spawn_daemon_allow().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    d.mem.mkdir(&vp("mem:///dst")).await.expect("mkdir dst");
    d.scopes.grant(
        "claude",
        Scope::forever(vec![vp("mem:///proj"), vp("mem:///dst")], OpSet::all()),
    );
    let b = Bridge::connect(&d.socket, "claude").await.expect("connect");

    for n in norte_testkit::corpus::hostile_names() {
        // Source = BYTES, never going through a String at any point.
        let seg = norte_proto::Segment::new(n.bytes.clone()).expect("corpus segment");
        let src = vp("mem:///proj").join(seg);
        let mut sink = d.mem.write(&src).await.expect("write");
        sink.write(Bytes::from_static(b"payload"))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");

        // 1) list_dir emits the faithful wire form (never U+FFFD).
        let (out, err) =
            call_tool(&b, "list_dir", serde_json::json!({"path": "mem:///proj"})).await;
        assert!(!err, "{}: {out}", n.id);
        let v: serde_json::Value = serde_json::from_str(&out).expect("payload");
        let wire = v["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .filter_map(|e| e["path"].as_str())
            .find(|w| VPath::parse(w).is_ok_and(|p| p == src))
            .unwrap_or_else(|| panic!("{}: the listing does not carry the faithful wire", n.id))
            .to_owned();
        assert!(
            !wire.contains('\u{FFFD}'),
            "{}: lossy on the wire: {wire}",
            n.id
        );

        // 2) The agent ECHOES that string in copy → same bytes at the destination.
        let dst_wire = format!(
            "mem:///dst/{}",
            wire.strip_prefix("mem:///proj/").expect("child of proj")
        );
        let (out, err) = call_tool(
            &b,
            "copy",
            serde_json::json!({"from": wire, "to": dst_wire}),
        )
        .await;
        assert!(!err, "{}: {out}", n.id);
        let dst = VPath::parse(&dst_wire).expect("dst wire");
        let entry = d
            .mem
            .stat(&dst)
            .await
            .unwrap_or_else(|e| panic!("{}: destination does not exist: {e}", n.id));
        assert_eq!(
            entry.path.file_name().expect("name").as_bytes(),
            n.bytes.as_slice(),
            "{}: the name's bytes differ after the round trip",
            n.id
        );
        d.mem.remove(&src).await.expect("clean up src");
        d.mem.remove(&dst).await.expect("clean up dst");
    }
}

/// Encoding H2: a range that SPLITS a multibyte character makes text look
/// binary — the chunk falls to lossy MARKED + byte-exact base64 (never a
/// loss; the tool's description says to reassemble via base64).
#[tokio::test]
async fn read_file_multibyte_boundary_falls_back_to_faithful_base64() {
    let d = spawn_daemon_allow().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    // "año…": offset=1,len=1 cuts the ñ (0xC3 0xB1) → chunk [0xC3].
    write_file(&d.mem, "mem:///proj/texto.txt", "año 2026\n".as_bytes()).await;
    grant_proj(&d, "claude"); // #80: reading requires scope
    let b = Bridge::connect(&d.socket, "claude").await.expect("connect");
    let (out, err) = call_tool(
        &b,
        "read_file",
        serde_json::json!({"path": "mem:///proj/texto.txt", "offset": 1, "len": 1}),
    )
    .await;
    assert!(!err, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("payload");
    let faithful = base64::engine::general_purpose::STANDARD
        .decode(v["base64"].as_str().expect("base64 present"))
        .expect("valid base64");
    assert_eq!(faithful, [0xC3], "exact byte of the split ñ");
    assert_eq!(v["text"], "\u{FFFD}");
}

/// Single criterion for args (sec MINOR-1 / enc H3): an argument PRESENT
/// with an illegal type is a tool error — never a silent degradation.
#[tokio::test]
async fn badly_typed_args_are_an_error_not_a_degradation() {
    let d = spawn_daemon_allow().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/f.txt", b"x").await;
    grant_proj(&d, "claude");
    let b = Bridge::connect(&d.socket, "claude").await.expect("connect");

    // Numeric mode: does NOT silently fall back to trash.
    let (out, err) = call_tool(
        &b,
        "delete",
        serde_json::json!({"path": "mem:///proj/f.txt", "mode": 123}),
    )
    .await;
    assert!(err);
    assert!(out.contains("invalid mode"), "{out}");
    assert!(d.mem.stat(&vp("mem:///proj/f.txt")).await.is_ok(), "intact");

    // Float offset: does NOT read from 0 pretending to apply it.
    let (out, err) = call_tool(
        &b,
        "read_file",
        serde_json::json!({"path": "mem:///proj/f.txt", "offset": 2.5}),
    )
    .await;
    assert!(err);
    assert!(out.contains("offset"), "{out}");

    // String limit: error, not ignored.
    let (out, err) = call_tool(
        &b,
        "list_dir",
        serde_json::json!({"path": "mem:///proj", "limit": "muchos"}),
    )
    .await;
    assert!(err);
    assert!(out.contains("limit"), "{out}");
}
