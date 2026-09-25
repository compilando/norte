use super::*;

#[tokio::test]
async fn initialize_negotiates_and_is_mandatory() {
    let d = spawn_daemon(None).await;
    // Without initialize: any method is NOT_INITIALIZED.
    let c = Client::connect(&d.socket).await.expect("connect");
    let err = c
        .call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: None,
                cursor: None,
                attrs: Vec::new(),
            },
        )
        .await
        .expect_err("initialize first");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::NOT_INITIALIZED),
        other => panic!("expected Rpc, got {other:?}"),
    }
    // With initialize, it works.
    let _c2 = connected_client(&d).await;
}

/// The daemon's N-1 version, DERIVED from `PROTOCOL_VERSION`.
///
/// Hand-written (it used to be `"0.38.2"`), the window this test says it
/// checks turned into "some old version" on the first bump and went red on
/// the second, blaming whatever change happened to pass by. In 0.x the minor
/// is the effective major, so N-1 is minor minus one.
pub(super) fn n_minus_one() -> String {
    let (major, rest) = methods::PROTOCOL_VERSION.split_once('.').expect("semver");
    let (minor, _) = rest.split_once('.').expect("semver");
    let minor: u64 = minor.parse().expect("numeric minor");
    assert_eq!(major, "0", "outside 0.x the major defines the N-1 window");
    assert!(minor > 0, "0.0.x has no N-1 to ask for");
    format!("{major}.{}.2", minor - 1)
}

#[tokio::test]
async fn initialize_rejects_an_incompatible_version() {
    let d = spawn_daemon(None).await;
    let c = Client::connect(&d.socket).await.expect("connect");
    let err = c
        .call::<_, methods::InitializeResult>(
            methods::INITIALIZE,
            &InitializeParams {
                client_info: client_info(),
                protocol_version: "0.1.0".into(),
                encodings: vec!["json".into()],
                agent_session: None,
            },
        )
        .await
        .expect_err("0.1.0 is neither N nor N-1");
    match err {
        ClientError::Rpc(rpc) => {
            // OUR OWN code: the upgrade signal is never parsed out of a message.
            assert_eq!(rpc.code, codes::VERSION_MISMATCH);
            assert!(norte_core::daemon::is_version_mismatch(&ClientError::Rpc(
                rpc
            )));
        }
        other => panic!("expected Rpc, got {other:?}"),
    }
    // N-1 DOES get in.
    let c2 = Client::connect(&d.socket).await.expect("connect");
    let ok: methods::InitializeResult = c2
        .call(
            methods::INITIALIZE,
            &InitializeParams {
                client_info: client_info(),
                protocol_version: n_minus_one(),
                encodings: vec![],
                agent_session: None,
            },
        )
        .await
        .expect("N-1 accepted");
    assert_eq!(ok.protocol_version, methods::PROTOCOL_VERSION);
}

#[tokio::test]
async fn initialize_rejects_an_unknown_encoding() {
    let d = spawn_daemon(None).await;
    let c = Client::connect(&d.socket).await.expect("connect");
    let err = c
        .call::<_, methods::InitializeResult>(
            methods::INITIALIZE,
            &InitializeParams {
                client_info: client_info(),
                protocol_version: methods::PROTOCOL_VERSION.into(),
                encodings: vec!["msgpack".into()],
                agent_session: None,
            },
        )
        .await
        .expect_err("only json in M2");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
}

/// `agent_session` is validated fail-closed at the handshake (encoding-auditor
/// H1 from T5): controls, bidi, empty or kilometric → `INVALID_PARAMS`. The id
/// travels to the journal, logs and approval modals — never freely chosen by
/// the agent.
#[tokio::test]
async fn a_hostile_agent_session_is_rejected_at_initialize() {
    let d = spawn_daemon(None).await;
    let hostile = [
        "s1\nmem:///fake",       // line injection
        "s1\u{202e}ypoc",        // RTL override
        "s1\u{1b}]0;pwned\u{7}", // OSC/ANSI
        "",                      // empty
        &"a".repeat(65),         // too long
        "con espacios",          // outside the charset
    ];
    for session in hostile {
        let c = Client::connect(&d.socket).await.expect("connect");
        let err = c
            .call::<_, methods::InitializeResult>(
                methods::INITIALIZE,
                &InitializeParams {
                    client_info: client_info(),
                    protocol_version: methods::PROTOCOL_VERSION.into(),
                    encodings: vec!["json".into()],
                    agent_session: Some(session.into()),
                },
            )
            .await
            .expect_err("hostile session rejected");
        assert!(
            matches!(err, ClientError::Rpc(ref rpc) if rpc.code == codes::INVALID_PARAMS),
            "expected INVALID_PARAMS for {session:?}, got {err:?}"
        );
    }
    // The full legal charset passes.
    let c = Client::connect(&d.socket).await.expect("connect");
    let ok: methods::InitializeResult = c
        .call(
            methods::INITIALIZE,
            &InitializeParams {
                client_info: client_info(),
                protocol_version: methods::PROTOCOL_VERSION.into(),
                encodings: vec!["json".into()],
                agent_session: Some("Agente.Claude_01-x".into()),
            },
        )
        .await
        .expect("valid session");
    assert_eq!(ok.protocol_version, methods::PROTOCOL_VERSION);
}

/// `connection.trust_host_key` (0.7.0, phase 6e) exists in the dispatch and
/// reaches the engine: with no connector configured it answers the
/// `Unsupported` taxonomy over the wire — not `METHOD_NOT_FOUND` (that would
/// mean the handler is missing).
#[tokio::test]
async fn trust_host_key_reaches_the_engine() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::ConnectionTrustHostKeyResult>(
            methods::CONNECTION_TRUST_HOST_KEY,
            &methods::ConnectionTrustHostKeyParams {
                host: "h.example".into(),
                port: Some(22),
                algo: "ssh-ed25519".into(),
                fingerprint: "SHA256:abc".into(),
            },
        )
        .await
        .expect_err("no connector: Unsupported");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert!(
                matches!(rpc.data, Some(norte_proto::Error::Unsupported)),
                "Unsupported taxonomy, was {:?}",
                rpc.data
            );
        }
        other => panic!("expected Rpc, got {other:?}"),
    }
}

/// #325, the twin of the one above: `connection.provide_secret` exists in the
/// dispatch and reaches the engine. With no connector it answers
/// `Unsupported` over the wire; a `METHOD_NOT_FOUND` would mean the handler
/// is missing.
#[tokio::test]
async fn provide_secret_reaches_the_engine() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::ConnectionProvideSecretResult>(
            methods::CONNECTION_PROVIDE_SECRET,
            &methods::ConnectionProvideSecretParams {
                conn: "rosetta".into(),
                secret: "s3cr3t".into(),
            },
        )
        .await
        .expect_err("no connector: Unsupported");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert!(
                matches!(rpc.data, Some(norte_proto::Error::Unsupported)),
                "Unsupported taxonomy, was {:?}",
                rpc.data
            );
        }
        other => panic!("expected Rpc, got {other:?}"),
    }
}

#[tokio::test]
async fn an_unknown_method_is_method_not_found() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, serde_json::Value>("fs.inventado", &serde_json::json!({}))
        .await
        .expect_err("the method does not exist");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::METHOD_NOT_FOUND));
}

// ---------- lifecycle ----------

#[tokio::test]
async fn daemon_shutdown_graceful_waits_and_shuts_down() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let _: DaemonShutdownResult = c
        .call(
            methods::DAEMON_SHUTDOWN,
            &DaemonShutdownParams {
                graceful: true,
                ..Default::default()
            },
        )
        .await
        .expect("shutdown accepted");
    let joined = tokio::time::timeout(Duration::from_secs(5), d.run)
        .await
        .expect("run() finishes")
        .expect("clean join");
    joined.expect("shutdown with no error");
    assert!(!d.socket.exists(), "the socket is removed from the FS");
}

/// When `run()` returns, the connections have already CLOSED — with
/// whatever they had to say already written.
///
/// Whoever calls `run()` is usually a `main` that returns right after, and
/// dropping the runtime takes down with it every task still alive. The one
/// writing `daemon.shutdown`'s response used to be one of them: `norte
/// daemon stop` used to fail now and then with "connection closed with the
/// request in flight" on a daemon that had in fact stopped. It is checked on
/// another open connection because it is observable without waiting: if
/// `run()` already drained, its read gives EOF right away; if not, there is
/// still nothing to read.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_returns_with_the_connections_already_closed() {
    for _ in 0..30 {
        run_returns_with_the_connections_already_closed_once().await;
    }
}

async fn run_returns_with_the_connections_already_closed_once() {
    let d = spawn_daemon(None).await;
    let other = tokio::net::UnixStream::connect(&d.socket)
        .await
        .expect("connect");
    let c = connected_client(&d).await;
    let _: DaemonShutdownResult = c
        .call(
            methods::DAEMON_SHUTDOWN,
            &DaemonShutdownParams {
                graceful: true,
                ..Default::default()
            },
        )
        .await
        .expect("shutdown accepted");
    tokio::time::timeout(Duration::from_secs(5), d.run)
        .await
        .expect("run() finishes")
        .expect("clean join")
        .expect("shutdown with no error");
    let mut buf = [0u8; 16];
    match other.try_read(&mut buf) {
        Ok(0) => {}
        read => panic!("the connection is still open when run() returns: {read:?}"),
    }
}

/// Waits for a `daemon.going_away` and returns whether it says to come back.
pub(super) async fn going_away(c: &mut Client) -> bool {
    loop {
        let n = tokio::time::timeout(Duration::from_secs(5), c.notification())
            .await
            .expect("notification before the timeout")
            .expect("connection alive");
        if n.method == methods::DAEMON_GOING_AWAY {
            let p: methods::DaemonGoingAway =
                serde_json::from_value(n.params.expect("params")).expect("DaemonGoingAway");
            return p.reconnect;
        }
    }
}

/// A HANDOVER says to come back, and says so BEFORE it stops accepting.
#[tokio::test]
async fn a_handover_says_to_come_back() {
    let d = spawn_daemon(None).await;
    let mut c = connected_client(&d).await;
    let _: DaemonShutdownResult = c
        .call(
            methods::DAEMON_SHUTDOWN,
            &DaemonShutdownParams {
                graceful: true,
                mode: methods::ShutdownMode::Handover,
            },
        )
        .await
        .expect("handover accepted");

    assert!(going_away(&mut c).await, "a handover says to come back");
}

/// And an ordinary stop says the OPPOSITE. That is what separates "the daemon
/// stopped" from "the connection dropped", which are not the same thing to
/// whoever reads it — and it is what stops a client from resurrecting what the
/// user just stopped.
#[tokio::test]
async fn an_ordinary_stop_says_not_to_come_back() {
    let d = spawn_daemon(None).await;
    let mut c = connected_client(&d).await;
    let _: DaemonShutdownResult = c
        .call(methods::DAEMON_SHUTDOWN, &DaemonShutdownParams::default())
        .await
        .expect("stop accepted");

    assert!(!going_away(&mut c).await, "a stop says not to come back");
}

/// A handover with a LIVE task is refused, in the response, while there is
/// still someone to answer to — and it touches nothing: the daemon keeps
/// accepting.
///
/// The refusal comes BEFORE, not after waiting for the tasks, because
/// `daemon.shutdown`'s response goes out right away: a refusal decided
/// minutes later would have nobody to tell it to, and by then the listener
/// would have already stopped accepting — "refusing" would mean accepting
/// again, a state machine nobody asked for.
#[tokio::test]
async fn a_handover_with_a_live_task_is_refused_and_touches_nothing() {
    let mem = MemProvider::new();
    write_file(&mem, "mem:///src.bin", &vec![7u8; 256 * 1024]).await;
    // Per-op latency: the copy stays alive while the handover is requested,
    // deterministically and without sleeping blindly.
    mem.faults()
        .set_latency_per_op(Some(Duration::from_millis(50)));
    let d = spawn_daemon_mem(None, Duration::from_mins(2), mem).await;
    let c = connected_client(&d).await;
    let _: FsTaskResult = c
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///src.bin"),
                to: vp("mem:///dst.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
                queued: false,
            },
        )
        .await
        .expect("copy launched");

    let err = c
        .call::<_, DaemonShutdownResult>(
            methods::DAEMON_SHUTDOWN,
            &DaemonShutdownParams {
                graceful: true,
                mode: methods::ShutdownMode::Handover,
            },
        )
        .await
        .expect_err("not with a live copy");
    assert!(
        matches!(&err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST),
        "{err:?}"
    );

    // And it touched nothing: the daemon is still up and serving.
    let _: FsStatResult = c
        .call(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("mem:///"),
                attrs: Vec::new(),
            },
        )
        .await
        .expect("the daemon keeps accepting after refusing the handover");
}

/// **The socket is removed BEFORE draining, not after**, and that only
/// matters since handover exists.
///
/// On shutdown, the daemon drops the listener and waits for its tasks. If the
/// socket file is still there during that wait, a client that reconnects
/// gets `ECONNREFUSED`, starts the replacement — something it never did
/// BEFORE this phase — the replacement deletes the stale path and binds its
/// own... and the old daemon, once it finishes draining, deletes the
/// REPLACEMENT's socket. That one is left listening on a nameless inode, and
/// since the startup permission is single-use, nobody ever brings it back
/// up.
///
/// The test pins the order: with a live task — i.e. in the middle of
/// draining — the path no longer exists.
#[tokio::test]
async fn the_socket_is_removed_before_draining() {
    let mem = MemProvider::new();
    write_file(&mem, "mem:///src.bin", &vec![7u8; 4 * 1024 * 1024]).await;
    // HIGH per-op latency: draining takes seconds, so "the socket left" and
    // "the daemon finished" cannot be confused.
    mem.faults()
        .set_latency_per_op(Some(Duration::from_millis(200)));
    let mut d = spawn_daemon_mem(None, Duration::from_mins(2), mem).await;
    let c = connected_client(&d).await;
    let _: FsTaskResult = c
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///src.bin"),
                to: vp("mem:///dst.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
                queued: false,
            },
        )
        .await
        .expect("copy launched");

    // Graceful stop: it enters draining with the copy alive.
    let _: DaemonShutdownResult = c
        .call(methods::DAEMON_SHUTDOWN, &DaemonShutdownParams::default())
        .await
        .expect("stop accepted");

    // The path has to disappear WHILE draining is still happening. The two
    // halves are the assertion: without the second, a fast drain would pass
    // the test with the deletion at the end, exactly what breaks handover.
    let mut removed = false;
    for _ in 0..50 {
        if !d.socket.exists() {
            removed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(removed, "the socket is still there during draining");
    assert!(
        futures::FutureExt::now_or_never(&mut d.run).is_none(),
        "and the daemon has NOT finished yet: if it already finished, this test does not          distinguish early deletion from late"
    );
}

pub(super) async fn spawn_daemon_at(socket: PathBuf) -> TestDaemon {
    // The parent tempdir is owned by the caller; here, an empty guard.
    let dir = tempfile::tempdir().expect("tempdir guard");
    let engine = Arc::new(Engine::new());
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let daemon = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: std::time::Duration::from_mins(2),
            plugins_dir: Some(dir.path().to_path_buf()),
            state_dir: None,
        },
    )
    .await
    .expect("bind");
    let run = tokio::spawn(daemon.run());
    TestDaemon {
        socket,
        run,
        dir,
        mem,
    }
}

// ---------- raw protocol (hostile frames) ----------

/// Reads ONE whole line from the stream (UDS is a stream: one read can be
/// partial — rust-reviewer m4).
pub(super) async fn read_frame(s: &mut tokio::net::UnixStream) -> serde_json::Value {
    use tokio::io::AsyncReadExt;
    let mut decoder = norte_proto::wire::FrameDecoder::new();
    let mut buf = vec![0u8; 4096];
    loop {
        if let Some(frame) = decoder.next_frame() {
            return serde_json::from_slice(&frame).expect("JSON response");
        }
        let n = tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf))
            .await
            .expect("response before the timeout")
            .expect("read");
        assert_ne!(n, 0, "connection closed while waiting for a response");
        decoder.push(&buf[..n]).expect("reasonable frame");
    }
}

/// Hostile frames directly over the socket, without the Client: broken JSON
/// = -32700; valid JSON that is not an envelope = -32600; a request with an
/// illegally-typed id = -32600 (NEVER silence); null params on
/// daemon.shutdown (the golden's canonical shape) works.
#[tokio::test]
async fn raw_hostile_frames_and_canonical_shapes() {
    use tokio::io::AsyncWriteExt;
    let d = spawn_daemon(None).await;

    let mut s = tokio::net::UnixStream::connect(&d.socket)
        .await
        .expect("raw connect");

    s.write_all(b"esto no es json\n").await.expect("write");
    let resp = read_frame(&mut s).await;
    assert_eq!(resp["error"]["code"], serde_json::json!(-32700));
    assert_eq!(resp["id"], serde_json::Value::Null);

    // Valid JSON, invalid envelope: -32600, not -32700 (guardian M2).
    s.write_all(b"{\"foo\":1}\n").await.expect("write");
    let resp = read_frame(&mut s).await;
    assert_eq!(resp["error"]["code"], serde_json::json!(-32600));

    // Illegal id (negative): -32600, never swallowed as a notification (M3).
    s.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":-1,\"method\":\"fs.stat\",\"params\":null}\n")
        .await
        .expect("write");
    let resp = read_frame(&mut s).await;
    assert_eq!(resp["error"]["code"], serde_json::json!(-32600));

    // initialize + daemon.shutdown with null params (canonical golden, M1).
    //
    // The version is INTERPOLATED from `PROTOCOL_VERSION` and not hand
    // written: pinned here (it used to be `"0.38.0"`), the frame would age
    // without anyone touching it and this test would turn red two bumps
    // later, blaming whatever change happened to pass by. What it tests is
    // the raw frame, not the N/N-1 window — `version_compatible` in
    // `norte-proto` handles that.
    let hello = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{{\"client_info\":{{\"name\":\"raw\",\"version\":\"0\"}},\"protocol_version\":\"{}\",\"encodings\":[\"json\"]}}}}\n",
        methods::PROTOCOL_VERSION
    );
    s.write_all(hello.as_bytes()).await.expect("write");
    let resp = read_frame(&mut s).await;
    assert!(resp["result"]["protocol_version"].is_string(), "{resp}");
    s.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"daemon.shutdown\",\"params\":null}\n")
        .await
        .expect("write");
    let resp = read_frame(&mut s).await;
    assert!(resp["error"].is_null(), "null params accepted: {resp}");
}

/// A repeated initialize = a protocol error (pinned decision, guardian m4) —
/// and the connection stays alive and usable.
#[tokio::test]
async fn a_repeated_initialize_is_invalid_request() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::InitializeResult>(
            methods::INITIALIZE,
            &InitializeParams {
                client_info: client_info(),
                protocol_version: methods::PROTOCOL_VERSION.into(),
                encodings: vec!["json".into()],
                agent_session: None,
            },
        )
        .await
        .expect_err("re-initialize rejected");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));
    let _: FsListResult = c
        .call(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: None,
                cursor: None,
                attrs: Vec::new(),
            },
        )
        .await
        .expect("the connection is still alive");
}

// ---------- L2: the UI session over the socket ----------

/// The session goes and comes back, and the revision goes up. The first human
/// connection that asks keeps it.
#[tokio::test]
async fn session_get_and_put_over_the_socket() {
    // With `state_dir`, because `owner` means "this gets saved": a daemon
    // with nowhere to write answers no, and rightly so.
    let state = tempfile::tempdir().expect("tempdir");
    let d = spawn_daemon_with_state(state.path()).await;
    let c = connected_client(&d).await;
    let g: methods::SessionGetResult = c
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert_eq!(g.session.revision, 0);
    assert_eq!(g.session.version, 0, "no schema until someone writes");
    assert!(g.owner, "the first human connection keeps it");

    let body = serde_json::json!({ "version": 1, "slots": {} });
    let p: methods::SessionPutResult = c
        .call(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: 0,
                body: body.clone(),
            },
        )
        .await
        .expect("session.put");
    assert_eq!(p.revision, 1);

    let g2: methods::SessionGetResult = c
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert_eq!(g2.session.body, body, "the same document comes back");
    assert_eq!(g2.session.version, 1);
    assert_eq!(g2.session.revision, 1);
}

/// `session.release` (phase 9) releases ownership WITHOUT disconnecting, and
/// what it releases is ownership, not the content.
///
/// The three things it has to demonstrate, and none is obvious:
///
/// - the owner receives `released: true` and stops being one;
/// - the BODY stays where it was — exactly what the other frontend is going
///   to read — with its revision;
/// - the next human connection takes it, which is what makes handoff
///   possible.
#[tokio::test]
async fn session_release_gives_up_ownership_and_keeps_the_body() {
    let state = tempfile::tempdir().expect("tempdir");
    let daemon = spawn_daemon_with_state(state.path()).await;
    let client = connected_client(&daemon).await;
    let g: methods::SessionGetResult = client
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(g.owner, "the first human connection keeps it");

    let body = serde_json::json!({ "version": 1, "slots": {} });
    let p: methods::SessionPutResult = client
        .call(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: 0,
                body: body.clone(),
            },
        )
        .await
        .expect("session.put");
    assert_eq!(p.revision, 1);

    let r: methods::SessionReleaseResult = client
        .call(methods::SESSION_RELEASE, &serde_json::json!({}))
        .await
        .expect("session.release");
    assert!(r.released, "it was the owner");

    // Releasing TWICE answers `false` the second time: it was no longer
    // theirs, and that has to be distinguishable from a failure.
    let r2: methods::SessionReleaseResult = client
        .call(methods::SESSION_RELEASE, &serde_json::json!({}))
        .await
        .expect("session.release");
    assert!(!r2.released, "it was no longer the owner");

    // And another connection takes it, with the body intact: that is the
    // handoff.
    let client2 = connected_client(&daemon).await;
    let g2: methods::SessionGetResult = client2
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(g2.owner, "the session was free and it takes it");
    assert_eq!(g2.session.body, body, "what was released was ownership");
    assert_eq!(g2.session.revision, 1);
}

/// An AGENT does not release anyone's session: it has no screen, and the
/// answer is the same as `session.get`'s — `INVALID_REQUEST` before looking
/// at anything.
#[tokio::test]
async fn session_release_is_denied_to_an_agent() {
    let state = tempfile::tempdir().expect("tempdir");
    let daemon = spawn_daemon_with_state(state.path()).await;
    let human = connected_client(&daemon).await;
    let g: methods::SessionGetResult = human
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(g.owner);

    let agent = connected_agent(&daemon, "claude").await;
    let err = agent
        .call::<_, methods::SessionReleaseResult>(methods::SESSION_RELEASE, &serde_json::json!({}))
        .await
        .expect_err("an agent has no session");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_REQUEST),
        other => panic!("expected Rpc, got {other:?}"),
    }
    // And the human's STAYS theirs: the agent's attempt did not touch it.
    let r: methods::SessionReleaseResult = human
        .call(methods::SESSION_RELEASE, &serde_json::json!({}))
        .await
        .expect("session.release");
    assert!(r.released, "the human was still the owner");
}

/// A stale revision over the wire is the `Conflict` taxonomy in `data`, not a
/// transport error: the client distinguishes "read again" from "the daemon
/// broke".
#[tokio::test]
async fn a_stale_session_put_is_conflict() {
    // WITH `state_dir`: since #237's review, a core that does not persist
    // refuses the whole `put`, so a revision conflict can only be triggered
    // where writing actually happens.
    let state = tempfile::tempdir().expect("tmp");
    let d = spawn_daemon_with_state(state.path()).await;
    let c = connected_client(&d).await;
    let _: methods::SessionGetResult = c
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    let params = methods::SessionPutParams {
        version: 1,
        revision: 0,
        body: serde_json::json!({}),
    };
    let _: methods::SessionPutResult = c
        .call(methods::SESSION_PUT, &params)
        .await
        .expect("the first one gets in");
    let err = c
        .call::<_, methods::SessionPutResult>(methods::SESSION_PUT, &params)
        .await
        .expect_err("the revision is no longer current");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert_eq!(
                rpc.data,
                Some(Error::Conflict {
                    conflict: norte_proto::ConflictKind::StaleRevision
                }),
                "{:?}",
                rpc.data
            );
        }
        other => panic!("expected Rpc, got {other:?}"),
    }
}

/// Above the cap: `LimitExceeded` with ITS token, and the stored session
/// stays as it was.
#[tokio::test]
async fn session_put_over_the_cap_is_limit_exceeded() {
    // WITH `state_dir`, for the same reason as the test above: the cap is
    // checked after ownership, and with no writer it is never reached.
    let state = tempfile::tempdir().expect("tmp");
    let d = spawn_daemon_with_state(state.path()).await;
    let c = connected_client(&d).await;
    let _: methods::SessionGetResult = c
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    let fat = serde_json::json!({ "x": "y".repeat(methods::SESSION_BODY_MAX + 1) });
    let err = c
        .call::<_, methods::SessionPutResult>(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: 0,
                body: fat,
            },
        )
        .await
        .expect_err("does not fit");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert!(
                matches!(
                    rpc.data,
                    Some(Error::LimitExceeded { ref limit }) if limit == Error::LIMIT_SESSION_BODY
                ),
                "LimitExceeded session-body, was {:?}",
                rpc.data
            );
        }
        other => panic!("expected Rpc, got {other:?}"),
    }
    let g: methods::SessionGetResult = c
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert_eq!(g.session.revision, 0, "nothing was written");
}

/// The SAME daemon's second client receives a copy and runs detached: its
/// `put` is refused and the owner's session stays intact.
#[tokio::test]
async fn the_second_client_receives_a_copy_and_does_not_write() {
    let state = tempfile::tempdir().expect("tempdir");
    let d = spawn_daemon_with_state(state.path()).await;
    let one = connected_client(&d).await;
    let g1: methods::SessionGetResult = one
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(g1.owner);
    let _: methods::SessionPutResult = one
        .call(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: 0,
                body: serde_json::json!({ "who": "one" }),
            },
        )
        .await
        .expect("the owner writes");

    let two = connected_client(&d).await;
    let g2: methods::SessionGetResult = two
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(!g2.owner, "the second one runs detached");
    assert_eq!(
        g2.session.body["who"],
        serde_json::json!("one"),
        "it receives a COPY"
    );
    let err = two
        .call::<_, methods::SessionPutResult>(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: 1,
                body: serde_json::json!({ "who": "two" }),
            },
        )
        .await
        .expect_err("whoever is not the owner does not write");
    // With a taxonomy and not prose: the client distinguishes "it's not
    // yours" from "your params are wrong" without reading English — and it
    // is the same denial the embedded arm gives.
    match err {
        ClientError::Rpc(ref rpc) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert_eq!(rpc.data, Some(Error::PermissionDenied), "{:?}", rpc.data);
        }
        ref other => panic!("expected Rpc, got {other:?}"),
    }
    let g3: methods::SessionGetResult = one
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert_eq!(g3.session.body["who"], serde_json::json!("one"));
}

/// The owner leaving RELEASES the session: the next human connection takes
/// it. Without this, a client that dies leaves the screen hostage until a
/// handover.
#[tokio::test]
async fn the_session_frees_up_when_the_owner_dies() {
    let state = tempfile::tempdir().expect("tempdir");
    let d = spawn_daemon_with_state(state.path()).await;
    let one = connected_client(&d).await;
    let g1: methods::SessionGetResult = one
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(g1.owner);
    drop(one);

    // The disconnection is processed on the server; retried until seen. The
    // limit is one of TIME, not a number of loops: under load, "50 yields"
    // is a race that gets lost and an intermittent red.
    let free = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let two = connected_client(&d).await;
            let g: methods::SessionGetResult = two
                .call(methods::SESSION_GET, &serde_json::json!({}))
                .await
                .expect("session.get");
            if g.owner {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        free.is_ok(),
        "the session stayed hostage to a dead connection"
    );
}

/// A daemon with NOWHERE to write does not claim to be in charge: `owner:
/// false`, and the client sees itself detached instead of writing a screen
/// every second that will never reach any disk.
#[tokio::test]
async fn with_no_state_dir_nobody_is_the_owner() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let g: methods::SessionGetResult = c
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(!g.owner, "with no writer there is no owner to promise");
}

/// A daemon with its own `state_dir`: the one that persists the UI session
/// (L2). The directory is set by the test, and that is why no test touches
/// the real state.
pub(super) async fn spawn_daemon_with_state(state: &std::path::Path) -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let engine = Arc::new(Engine::new());
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let daemon = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: Some(dir.path().to_path_buf()),
            state_dir: Some(state.to_path_buf()),
        },
    )
    .await
    .expect("bind");
    let run = tokio::spawn(daemon.run());
    TestDaemon {
        socket,
        run,
        dir,
        mem,
    }
}

/// Handover is the event this exists for: a daemon leaves, and what the
/// client had put is on disk by the time the next one starts.
#[tokio::test]
async fn the_session_survives_a_handover() {
    let state = tempfile::tempdir().expect("tmp");
    let d = spawn_daemon_with_state(state.path()).await;
    let c = connected_client(&d).await;
    let _: methods::SessionGetResult = c
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    let _: methods::SessionPutResult = c
        .call(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: 0,
                body: serde_json::json!({ "dir": "file:///casa" }),
            },
        )
        .await
        .expect("session.put");
    let _: DaemonShutdownResult = c
        .call(
            methods::DAEMON_SHUTDOWN,
            &DaemonShutdownParams {
                graceful: true,
                mode: methods::ShutdownMode::Handover,
            },
        )
        .await
        .expect("daemon.shutdown");
    drop(c);
    // The flush and releasing the lock happen INSIDE `run`: waiting for it
    // is waiting for exactly what the successor needs to find done.
    d.run.await.expect("join").expect("clean shutdown");

    let d2 = spawn_daemon_with_state(state.path()).await;
    let c2 = connected_client(&d2).await;
    let g: methods::SessionGetResult = c2
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert_eq!(g.session.body["dir"], serde_json::json!("file:///casa"));
    assert_eq!(g.session.revision, 1, "the revision survives too");
    assert_eq!(g.session.version, 1);
}

/// #237: a daemon that starts while ANOTHER core holds the session lock used
/// to never retry it.
///
/// `session_persists` used to be computed once at bind time, so it answered
/// `owner: false` for its whole life — also hours after the other process
/// had left and the file had been free ever since. The embedded arm already
/// retried (#234); this is the daemon's.
///
/// And taking it late ADOPTS the disk document: what the other core wrote
/// after this one started is what is current, and serving the old copy with
/// the new number would lose it without anything noticing.
#[tokio::test]
async fn a_detached_daemon_takes_the_session_once_it_frees_up() {
    let state = tempfile::tempdir().expect("tmp");
    let one = spawn_daemon_with_state(state.path()).await;
    let c1 = connected_client(&one).await;
    let g1: methods::SessionGetResult = c1
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(g1.owner, "the first one has the lock");

    // The second one starts WITH the lock taken: it runs detached.
    let two = spawn_daemon_with_state(state.path()).await;
    let c2 = connected_client(&two).await;
    let g2: methods::SessionGetResult = c2
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(!g2.owner, "with another's lock, detached");

    // The first one writes AFTER the second one has started: this is what
    // the second one has to adopt, and it cannot have read it at birth.
    let _: methods::SessionPutResult = c1
        .call(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: 0,
                body: serde_json::json!({ "who": "the first one" }),
            },
        )
        .await
        .expect("the owner writes");
    let _: DaemonShutdownResult = c1
        .call(
            methods::DAEMON_SHUTDOWN,
            &DaemonShutdownParams {
                graceful: true,
                mode: methods::ShutdownMode::Stop,
            },
        )
        .await
        .expect("daemon.shutdown");
    drop(c1);
    one.run.await.expect("join").expect("clean shutdown");

    // The limit is one of TIME, not a number of loops: the writer retries on
    // its tick, and counting loops under load is an intermittent red.
    let taken = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let g: methods::SessionGetResult = c2
                .call(methods::SESSION_GET, &serde_json::json!({}))
                .await
                .expect("session.get");
            if g.owner {
                return g;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the second one never took a session that had been free");

    assert_eq!(
        taken.session.body["who"],
        serde_json::json!("the first one"),
        "and it adopts the document the other one left, not the one it had at birth"
    );
    assert_eq!(taken.session.revision, 1, "with its revision");
}

/// **ADR 0059's promise, end to end**: a session written by a NEWER binary is
/// not read and — what matters — not overwritten.
///
/// Without the gate in the writer, this used to break in a second and in
/// silence: the future's file would not load, the core would start at
/// revision 0, the client's first `put` would be accepted, and the next
/// flush would publish over it. Losing a newer binary's session against an
/// older one cannot be recovered, so the assertion is about the file's
/// BYTES.
#[tokio::test]
async fn a_session_from_the_future_is_not_overwritten_over_the_socket() {
    let state = tempfile::tempdir().expect("tmp");
    let future = methods::Session {
        version: norte_core::ui_session::disk::SCHEMA_VERSION + 1,
        revision: 7,
        body: serde_json::json!({ "from": "a newer binary" }),
    };
    norte_core::ui_session::disk::write(state.path(), &future).expect("write the future one");
    let file = norte_core::ui_session::disk::path(state.path());
    let before = std::fs::read(&file).expect("read");

    let d = spawn_daemon_with_state(state.path()).await;
    let c = connected_client(&d).await;
    let g: methods::SessionGetResult = c
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(!g.owner, "not read, so not written either");
    assert_eq!(g.session.revision, 0, "starts from the configured default");
    // And a client that IGNORES `owner` does not overwrite it either. It used
    // to be accepted in memory and the body died with the process; since
    // #237's review it is refused outright, which is what the embedded arm
    // already did — and what has to happen once the writer can take the lock
    // late.
    let err = c
        .call::<_, methods::SessionPutResult>(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: 0,
                body: serde_json::json!({ "yo": "el binario viejo" }),
            },
        )
        .await
        .expect_err("over a session from the future, nothing is written, not even in memory");
    match err {
        ClientError::Rpc(ref rpc) => {
            assert_eq!(rpc.data, Some(Error::PermissionDenied), "{:?}", rpc.data);
        }
        ref other => panic!("expected Rpc, got {other:?}"),
    }
    let _: DaemonShutdownResult = c
        .call(
            methods::DAEMON_SHUTDOWN,
            &DaemonShutdownParams {
                graceful: true,
                mode: methods::ShutdownMode::Stop,
            },
        )
        .await
        .expect("daemon.shutdown");
    drop(c);
    d.run.await.expect("join").expect("clean shutdown");

    assert_eq!(
        std::fs::read(&file).expect("read"),
        before,
        "the future file has to stay byte for byte as it was"
    );
}

/// The core that does not hold the lock serves the screen and does NOT write
/// it: two cores over the same state do not overwrite each other.
#[tokio::test]
async fn a_detached_core_does_not_write_someone_elses_state() {
    let state = tempfile::tempdir().expect("tmp");
    let owner = spawn_daemon_with_state(state.path()).await;
    let c = connected_client(&owner).await;
    let _: methods::SessionGetResult = c
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    let _: methods::SessionPutResult = c
        .call(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: 0,
                body: serde_json::json!({ "who": "the owner" }),
            },
        )
        .await
        .expect("session.put");

    // The second core starts WITH the screen — the lock decides who writes,
    // not who reads — even though it is not on disk yet.
    let detached = spawn_daemon_with_state(state.path()).await;
    let c2 = connected_client(&detached).await;
    let g2: methods::SessionGetResult = c2
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(!g2.owner, "the second core runs detached");
    // The revision comes from ITS OWN `get` and is not assumed to be zero:
    // the detached core loads whatever is on disk, and by the time it
    // starts, the owner may have already flushed — its writer's first tick
    // is immediate. Fixing zero here would assert who won that race, and
    // under load it lost: intermittent red in a test that is not about
    // revisions.
    // And since #237's review, a detached core's `put` is REFUSED, same as
    // in the embedded arm: accepting it in memory stopped being harmless
    // once the writer could take the lock late — the detached, accepted body
    // survived adoption and got published over someone else's screen.
    let err = c2
        .call::<_, methods::SessionPutResult>(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: g2.session.revision,
                body: serde_json::json!({ "who": "the detached one" }),
            },
        )
        .await
        .expect_err("a detached core does not write, not even in memory");
    match err {
        ClientError::Rpc(ref rpc) => {
            assert_eq!(rpc.data, Some(Error::PermissionDenied), "{:?}", rpc.data);
        }
        ref other => panic!("expected Rpc, got {other:?}"),
    }
    let _: DaemonShutdownResult = c2
        .call(
            methods::DAEMON_SHUTDOWN,
            &DaemonShutdownParams {
                graceful: true,
                mode: methods::ShutdownMode::Stop,
            },
        )
        .await
        .expect("daemon.shutdown");
    drop(c2);
    detached.run.await.expect("join").expect("clean shutdown");

    // It has left NOTHING of its own on disk. The file may already exist —
    // the owner flushes every second, and this test does not race that
    // clock — but whatever it says belongs to HER. The assertion is not "no
    // file exists", which would depend on the tick, but "the file is not the
    // detached one's", which depends on nothing.
    let file = norte_core::ui_session::disk::path(state.path());
    let who = |path: &std::path::Path| -> Option<String> {
        let raw = std::fs::read(path).ok()?;
        let s: methods::Session = serde_json::from_slice(&raw).ok()?;
        Some(s.body["who"].to_string())
    };
    if let Some(q) = who(&file) {
        assert_eq!(
            q, "\"the owner\"",
            "a detached core wrote another one's state"
        );
    }

    // And once the owner shuts down, the file is unambiguously theirs: their
    // final flush is what stands.
    let _: DaemonShutdownResult = c
        .call(
            methods::DAEMON_SHUTDOWN,
            &DaemonShutdownParams {
                graceful: true,
                mode: methods::ShutdownMode::Stop,
            },
        )
        .await
        .expect("daemon.shutdown");
    drop(c);
    owner.run.await.expect("join").expect("clean shutdown");
    assert_eq!(
        who(&file).as_deref(),
        Some("\"the owner\""),
        "the final flush is the owner's"
    );
}

// ---------------------------------------------------------------------------
// The daemon's log over the wire (#328, ADR 0092).
// ---------------------------------------------------------------------------

/// A daemon with a log ring mounted, and the ring.
///
/// The ring is THE SAME object on both sides — the daemon serves it, the
/// test's subscriber writes into it — because in the real process it is too:
/// the binary is the one that mounts the log, and the daemon only serves it.
pub(super) async fn spawn_daemon_with_ring() -> (TestDaemon, norte_config::logring::LogRing) {
    spawn_daemon_with_ring_of(norte_config::logring::RING_DEFAULT).await
}

/// The same, with a ring of whatever size the test asks for.
///
/// A SMALL ring is the only way to reach overflow without emitting two
/// thousand lines, and overflow is what makes `lost` checkable.
pub(super) async fn spawn_daemon_with_ring_of(
    cap: usize,
) -> (TestDaemon, norte_config::logring::LogRing) {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let engine = Arc::new(Engine::new());
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let ring = norte_config::logring::LogRing::new(cap);
    let daemon = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: Some(dir.path().to_path_buf()),
            state_dir: None,
        },
    )
    .await
    .expect("bind")
    .with_log_ring(ring.clone());
    let run = tokio::spawn(daemon.run());
    (
        TestDaemon {
            socket,
            run,
            dir,
            mem,
        },
        ring,
    )
}

/// Routes THIS thread's lines to the ring while the guard lives.
///
/// A GLOBAL subscriber can only be installed once per process, and these
/// tests need their own; the scoped one solves it, same as `with_lines` in
/// `norte-ui-host`. The daemon runs on the same thread (`#[tokio::test]`'s
/// runtime is single-threaded), so its lines go in too — exactly what
/// happens in the real process.
///
/// Through the `tracing` layer and not by feeding lines by hand: the filter
/// that layer goes through is where `suppaftp`'s cap lives, and a shortcut
/// that skipped it would test a path that does not exist.
pub(super) fn toward_the_ring(
    ring: &norte_config::logring::LogRing,
) -> tracing::subscriber::DefaultGuard {
    use tracing_subscriber::layer::SubscriberExt as _;
    let s = tracing_subscriber::registry().with(norte_config::logring::ring_layer(ring));
    tracing::subscriber::set_default(s)
}

/// THE proof of this work: the cap stays alive on the other side of the
/// socket.
///
/// `suppaftp` writes `PASS <password>` at TRACE (#43, rule 10), and the
/// ring's level is raised FROM the interface. If raising the level via
/// `log.level` let a third-party target through, a keystroke in a panel
/// would put a password on screen.
///
/// **Covers the `tracing` path, not the `log` bridge's.** `suppaftp` does not
/// emit `tracing` events: it emits `log::trace!`, and `tracing-log` dispatches
/// them with the static `target` `"log"`. That other path is already pinned
/// in `norte-config`
/// (`logring::tests::la_contrasena_no_entra_ni_por_el_puente_de_log`), and the
/// cap is the SAME function for both, so repeating it over the socket would
/// prove the same thing twice. What this test adds is that raising the level
/// OVER THE WIRE does not lift it; the name `suppaftp` is here because it is
/// the target the allowlist names, not because this is its real path.
#[tokio::test]
async fn raising_the_level_over_the_wire_does_not_lift_the_cap() {
    let (d, ring) = spawn_daemon_with_ring().await;
    let _guard = toward_the_ring(&ring);
    let c = connected_client(&d).await;

    let level: methods::LogLevelResult = c
        .call(methods::LOG_LEVEL, &serde_json::json!({ "level": "trace" }))
        .await
        .expect("the human raises the level");
    assert_eq!(
        level.level, "trace",
        "the daemon answers the one that RESULTED"
    );

    tracing::trace!(target: "suppaftp", "PASS real-secret");
    tracing::trace!(target: "hyper::proto", "raw header");
    tracing::trace!(target: "norte_core::connect", "this one");

    let r: methods::LogTailResult = c
        .call(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": null, "max": 500 }),
        )
        .await
        .expect("the human reads the log");
    let messages: Vec<_> = r.lines.iter().map(|l| l.message.as_str()).collect();
    assert!(messages.iter().any(|m| m.contains("this one")));
    assert!(!messages.iter().any(|m| m.contains("real-secret")));
    assert!(!messages.iter().any(|m| m.contains("raw header")));
}

/// The cursor survives two calls and neither repeats nor skips lines.
#[tokio::test]
async fn the_cursor_chains_two_calls() {
    let (d, ring) = spawn_daemon_with_ring().await;
    let _guard = toward_the_ring(&ring);
    let c = connected_client(&d).await;

    tracing::info!(target: "norte_core::prueba", "first");
    let a: methods::LogTailResult = c
        .call(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": null, "max": 500 }),
        )
        .await
        .expect("first round");
    assert!(a.lines.iter().any(|l| l.message.contains("first")));
    tracing::info!(target: "norte_core::prueba", "second");
    let b: methods::LogTailResult = c
        .call(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": a.next, "max": 500 }),
        )
        .await
        .expect("second round");
    assert!(b.lines.iter().any(|l| l.message.contains("second")));
    assert!(!b.lines.iter().any(|l| l.message.contains("first")));
    // With no cursor, no gap is asserted: nobody lost what they never
    // expected.
    assert_eq!(a.lost, 0);
    assert_eq!(b.lost, 0, "a cursor that is caught up lost nothing");
    // The level and the history's background travel with the lines, so the
    // panel can say "this is everything there is" and at what level it was
    // captured.
    assert_eq!(a.level, "info", "the ring starts at INFO");
    assert_eq!(
        a.capacity,
        u32::try_from(norte_config::logring::RING_DEFAULT).expect("fits"),
    );
}

/// A cursor that fell behind receives the gap COUNTED, not a zero.
///
/// It is the only thing that makes polling honest, and it is the case none
/// of the other tests touch: they all ask while caught up or with no cursor,
/// so the `lost` that travels over the wire always came out zero —
/// substituting that count with a literal `0` in the daemon would have left
/// all of them green. The failure this prevents is concrete: a panel polls,
/// the machine stalls for thirty seconds with the daemon maxed out, the
/// panel asks again and is answered a log with a gap and no explanation — a
/// missing line is indistinguishable from an event that never happened.
///
/// The EXACT number is asserted, not `> 0`: a `lost` that only has to be
/// positive is satisfied by any badly-made count, and this number feeds a
/// gap marker that says how many.
#[tokio::test]
async fn a_cursor_that_fell_behind_receives_the_gap_counted() {
    const CAP: usize = 64;
    const EMITTED: u64 = 100;

    let (d, ring) = spawn_daemon_with_ring_of(CAP).await;
    let _guard = toward_the_ring(&ring);
    let c = connected_client(&d).await;

    tracing::info!(target: "norte_core::prueba", "the last one this client saw");
    let a: methods::LogTailResult = c
        .call(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": null, "max": 500 }),
        )
        .await
        .expect("first round");
    assert_eq!(a.lost, 0, "with no cursor, no gap is asserted");

    // The client stays still while the daemon keeps working, and the ring
    // wraps around underneath its cursor.
    for i in 0..EMITTED {
        tracing::info!(target: "norte_core::prueba", "while you weren't looking: {i}");
    }

    let b: methods::LogTailResult = c
        .call(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": a.next, "max": 500 }),
        )
        .await
        .expect("second round");

    // That exactly the test's own lines went in and nothing else is what
    // makes the number below legible: without this, a different `lost`
    // would not say whether the count is wrong or the daemon logged on its
    // own.
    assert_eq!(
        b.next - a.next,
        EMITTED,
        "only the test's lines went in between the two rounds"
    );
    // Of the 100 that went in, the ring only keeps 64: the first 36 — exactly
    // the ones this cursor was expecting — fell off the back.
    assert_eq!(
        b.lost,
        EMITTED - u64::try_from(CAP).expect("fits"),
        "the gap is counted, not silenced"
    );
    assert_eq!(b.lines.len(), CAP, "and the whole ring arrives");
    assert!(
        !b.lines
            .iter()
            .any(|l| l.message.contains("the last one this client saw")),
        "that one had already fallen off: it is what the gap counts"
    );
    // And the next round, caught up, does not carry over the previous gap:
    // `lost` is about THIS cursor, not everything the ring ever dropped.
    let c2: methods::LogTailResult = c
        .call(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": b.next, "max": 500 }),
        )
        .await
        .expect("third round");
    assert_eq!(c2.lost, 0, "a caught-up cursor lost nothing");
}

/// `max` is capped on the server: asking for a million does not send a
/// million.
#[tokio::test]
async fn the_server_caps_max() {
    let (d, ring) = spawn_daemon_with_ring().await;
    let _guard = toward_the_ring(&ring);
    let c = connected_client(&d).await;

    for i in 0..1200 {
        tracing::info!(target: "norte_core::prueba", "l{i}");
    }
    let r: methods::LogTailResult = c
        .call(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": 0, "max": 100_000 }),
        )
        .await
        .expect("asking for more is not an error");
    // EXACTLY a thousand, which here is deterministic: 1200 lines emitted
    // into a ring of 2000, so none fell off and the trim is the only thing
    // limiting it. A `<=` would have passed just as well with a server
    // answering a single line, or none.
    assert_eq!(
        r.lines.len(),
        1000,
        "the server trims to its cap, no more no less"
    );
    // And what did not fit is NOT lost: it is still there after `next`.
    let next: methods::LogTailResult = c
        .call(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": r.next, "max": 500 }),
        )
        .await
        .expect("the next round picks up the rest");
    assert!(!next.lines.is_empty(), "lines were left after the trim");
}

/// `max: 0` is a params error, not an empty page.
///
/// A panel polling with zero would receive an empty list every round with
/// the cursor stuck, and on screen that reads as "nothing is happening"
/// instead of the programming error it is. Same criterion as
/// `FsListParams::limit`.
#[tokio::test]
async fn max_zero_is_not_an_empty_page() {
    let (d, ring) = spawn_daemon_with_ring().await;
    let _guard = toward_the_ring(&ring);
    let c = connected_client(&d).await;

    let err = c
        .call::<_, methods::LogTailResult>(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": null, "max": 0 }),
        )
        .await
        .expect_err("zero is not a page");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_PARAMS),
        other => panic!("expected Rpc, got {other:?}"),
    }
}

/// A daemon with NO ring says it does not have one, and does not answer an
/// empty list.
///
/// It is the difference that lets the panel degrade to its local log WHILE
/// SAYING why: an empty log and an absent log cannot be read the same way
/// (#326). It is also the daemon compiled without the `logging` feature, and
/// the one that started when there was already another subscriber
/// installed.
#[tokio::test]
async fn with_no_ring_the_log_does_not_exist_instead_of_being_empty() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;

    for (method, params) in [
        (
            methods::LOG_TAIL,
            serde_json::json!({ "cursor": null, "max": 10 }),
        ),
        (methods::LOG_LEVEL, serde_json::json!({ "level": "debug" })),
    ] {
        let err = c
            .call::<_, serde_json::Value>(method, &params)
            .await
            .expect_err("this daemon has no log to serve");
        match err {
            ClientError::Rpc(rpc) => assert!(
                matches!(rpc.data, Some(norte_proto::Error::Unsupported)),
                "{method}: expected Unsupported, was {:?}",
                rpc.data
            ),
            other => panic!("expected Rpc, got {other:?}"),
        }
    }
}

/// A level outside the vocabulary is NOT degraded to the default one.
///
/// Accepting what is not understood and setting something else would leave
/// the reader believing it asked for something nobody did.
///
/// And it is rejected with `INVALID_PARAMS`, a DIFFERENT error from the one
/// the ringless daemon gives (`Unsupported`, see
/// `with_no_ring_the_log_does_not_exist_instead_of_being_empty`). With a
/// single code, a client could not distinguish "this daemon has no log" from
/// "I sent a typo", and the two call for different answers: the first
/// degrades to the local ring forever, the second gets corrected and
/// retried.
#[tokio::test]
async fn an_unknown_level_is_rejected_and_the_ring_does_not_move() {
    let (d, ring) = spawn_daemon_with_ring().await;
    let _guard = toward_the_ring(&ring);
    let c = connected_client(&d).await;

    let err = c
        .call::<_, methods::LogLevelResult>(
            methods::LOG_LEVEL,
            &serde_json::json!({ "level": "very-verbose" }),
        )
        .await
        .expect_err("that level does not exist");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(
            rpc.code,
            codes::INVALID_PARAMS,
            "a typo is not a missing capability"
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }
    assert_eq!(
        ring.level(),
        norte_config::logline::LogLevel::Info,
        "the ring stays where it was"
    );

    // And lowering does not lower: asking for less verbosity than current
    // answers the current one, which is the honest answer, not a failure.
    let _: methods::LogLevelResult = c
        .call(methods::LOG_LEVEL, &serde_json::json!({ "level": "debug" }))
        .await
        .expect("raises to debug");
    let r: methods::LogLevelResult = c
        .call(methods::LOG_LEVEL, &serde_json::json!({ "level": "error" }))
        .await
        .expect("asking for less is not an error");
    assert_eq!(r.level, "debug", "the ring NEVER lowers");
}
