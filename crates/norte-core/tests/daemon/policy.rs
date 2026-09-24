use super::*;

/// M3-3b Task 2: the actor is fixed by the connection. An agent client with
/// no scope sees its `fs.copy` denied by policy (`PolicyDenied` taxonomy, NOT
/// `PermissionDenied`) and the FS stays intact; a human client (no
/// `agent_session`) copies with no gate.
#[tokio::test]
async fn an_agent_with_no_scope_sees_policy_denied_a_human_copies() {
    let d = spawn_daemon_policy().await;
    write_file(&d.mem, "mem:///src.txt", b"hola").await;
    let copy = |from: &str, to: &str| FsCopyParams {
        from: vp(from),
        to: vp(to),
        on_collision: norte_proto::CollisionPolicy::default(),
        symlinks: norte_proto::SymlinkPolicy::default(),
        resume: norte_proto::ResumePolicy::default(),
        verify: norte_proto::VerifyPolicy::default(),
        dest_anchor: None,
        queued: false,
    };

    // Agent with no scope: denied by policy, without touching the FS.
    let agent = connected_agent(&d, "s1").await;
    let err = agent
        .call::<_, FsTaskResult>(methods::FS_COPY, &copy("mem:///src.txt", "mem:///a.txt"))
        .await
        .expect_err("agent with no scope: denied");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert!(
                matches!(rpc.data, Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
                "PolicyDenied out-of-scope, was {:?}",
                rpc.data
            );
        }
        other => panic!("expected Rpc, got {other:?}"),
    }
    assert!(
        matches!(
            d.mem.stat(&vp("mem:///a.txt")).await,
            Err(norte_proto::Error::NotFound)
        ),
        "the PRE-effect gate did not touch the destination"
    );

    // Human (no agent_session): copy accepted, the Task gets queued.
    let human = connected_client(&d).await;
    let res: FsTaskResult = human
        .call(methods::FS_COPY, &copy("mem:///src.txt", "mem:///h.txt"))
        .await
        .expect("a human copies with no gate");
    assert!(res.task_id.get() > 0);
}

/// V2 (#131, `host.volumes`): the same server-side criterion as
/// `an_agent_with_no_scope_sees_policy_denied_a_human_copies`, for an
/// operation with NO concept of scope — the mount table is not a path under
/// anyone's tree, so an agent sees it forbidden CATEGORICALLY (never
/// `out-of-scope`: there is no scope to ask for here, and asking for one
/// would not change the answer). `PolicyDenied` with the coarse `not-approved`
/// category from the closed vocabulary — same criterion as
/// `ai.rename_plan`/`index.embed`/`index.search_semantic` — never a transport
/// failure that distinguishes "forbidden" from "not implemented". A daemon
/// with NO `ScopedPolicy` installed (a plain `spawn_daemon`) is enough:
/// `host.volumes`'s gate does not go through the engine nor `policy.toml`, it
/// is a pure actor check BEFORE parsing the params (security review V2,
/// MAJOR applied: the gate used to run after `parse_params`, so an agent with
/// invalid params saw `INVALID_PARAMS` instead of `PolicyDenied` — an oracle
/// the agent controls with the shape of its own request).
/// `connection.list` is human-ONLY (#264), for the same reason as
/// `host.volumes`: the list names the user's servers, and a path scope has no
/// use for it at all.
///
/// And the gate runs BEFORE parsing, so an agent sees the same thing no
/// matter what it sends — it cannot distinguish "forbidden" from "bad params"
/// by fuzzing the shape of its own request.
#[tokio::test]
async fn connection_list_is_human_only() {
    let d = spawn_daemon(None).await;

    let agent = connected_agent(&d, "s1").await;
    for params in [
        serde_json::json!({}),
        serde_json::json!({"algo": "que no existe"}),
        serde_json::Value::Null,
    ] {
        let err = agent
            .call::<_, methods::ConnectionListResult>(methods::CONNECTION_LIST, &params)
            .await
            .expect_err("an agent does not list connections");
        match err {
            ClientError::Rpc(rpc) => assert!(
                matches!(
                    rpc.data,
                    Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved"
                ),
                "forbidden no matter what, was {:?}",
                rpc.data
            ),
            other => panic!("expected Rpc, got {other:?}"),
        }
    }

    // The human DOES, and with no params: absence is accepted (ADR 0004).
    // And the list is the daemon's tempdir's, not the machine's (#365): this
    // used to read whoever ran the suite's `~/.config/norte`, and it was
    // enough for them to have a connection norte could not read to turn it
    // red.
    let human = connected_client(&d).await;
    let r: methods::ConnectionListResult = human
        .call(methods::CONNECTION_LIST, &serde_json::Value::Null)
        .await
        .expect("the human lists with no gate");
    assert!(
        r.connections.is_empty() && r.unusable.is_empty(),
        "a test's daemon has no connections configured, no matter who has \
         some at home: {r:?}"
    );
}

/// **An entry the daemon cannot read does not hide the others** (#365).
///
/// The failure that uncovered this was twofold and each half hid the other:
/// the test read the machine's REAL config, and a single unusable entry made
/// the whole `connection.list` fail. The second half is the one that costs a
/// reader something — it loses the list of ALL its connections over one,
/// with an error that names none and points at nothing it can fix.
#[tokio::test]
async fn connection_list_does_not_fail_over_one_bad_entry() {
    let d = spawn_daemon(None).await;
    // The daemon resolves its `connections.toml` under its config root,
    // which in a test is its tempdir: that is why this can be written.
    std::fs::write(
        d.config_dir().join("connections.toml"),
        "[connections.buena]\nurl = \"sftp://servidor.example/datos\"\n\
         \n[connections.rota]\nurl = \"sftp://otro.example/\"\npassword = \"no va aquí\"\n",
    )
    .expect("write");

    let human = connected_client(&d).await;
    let r: methods::ConnectionListResult = human
        .call(methods::CONNECTION_LIST, &serde_json::Value::Null)
        .await
        .expect("a bad entry no longer aborts the call");

    assert_eq!(r.connections.len(), 1, "the good one is listed: {r:?}");
    assert_eq!(r.connections[0].name, "buena");
    assert_eq!(
        r.unusable.len(),
        1,
        "and the bad one comes out separately: {r:?}"
    );
    assert_eq!(
        r.unusable[0].name, "rota",
        "named, or the notice is useless"
    );
    assert!(
        r.unusable[0].reason.contains("password"),
        "and with the reason, which is the actionable part: {}",
        r.unusable[0].reason
    );
}

#[tokio::test]
async fn an_agent_sees_policy_denied_on_host_volumes_a_human_lists_it() {
    let d = spawn_daemon(None).await;
    let params = methods::HostVolumesParams {
        include_pseudo: false,
    };

    // Agent (with `agent_session`, no scope at all): forbidden.
    let agent = connected_agent(&d, "s1").await;
    let err = agent
        .call::<_, methods::HostVolumesResult>(methods::HOST_VOLUMES, &params)
        .await
        .expect_err("agent: host.volumes forbidden");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert!(
                matches!(
                    rpc.data,
                    Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved"
                ),
                "PolicyDenied not-approved, was {:?}",
                rpc.data
            );
        }
        other => panic!("expected Rpc, got {other:?}"),
    }

    // An agent with MALFORMED params (`include_pseudo` is not a bool): STILL
    // sees `PolicyDenied`, not `INVALID_PARAMS` — the gate runs before
    // parsing can even fail, so the answer does not depend on anything the
    // agent controls with the shape of its request.
    let err = agent
        .call::<_, methods::HostVolumesResult>(
            methods::HOST_VOLUMES,
            &serde_json::json!({"include_pseudo": "no-es-un-bool"}),
        )
        .await
        .expect_err("agent: invalid params are still forbidden, not INVALID_PARAMS");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert!(
                matches!(
                    rpc.data,
                    Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved"
                ),
                "PolicyDenied not-approved even with invalid params, was {:?}",
                rpc.data
            );
        }
        other => panic!("expected Rpc with PolicyDenied, got {other:?}"),
    }

    // Human (no `agent_session`): lists with no gate. The test machine runs
    // real Linux, so the `/` root has to show up — same criterion as
    // `enumerate_finds_the_real_root_filesystem` in `norte-core::volumes`.
    let human = connected_client(&d).await;
    let res: methods::HostVolumesResult = human
        .call(methods::HOST_VOLUMES, &params)
        .await
        .expect("human: host.volumes with no gate");
    assert!(
        res.volumes.iter().any(|v| v.mount.to_wire() == "file:///"),
        "expected to find the root among {:?}",
        res.volumes
    );

    // Human with ABSENT params (`null`): accepted as the default (ADR 0004),
    // same pattern as `task.list`/`plugin.list` — a `bool` with a default is
    // not "no legal params" when the client omits the whole object.
    let res_null: methods::HostVolumesResult = human
        .call(methods::HOST_VOLUMES, &serde_json::Value::Null)
        .await
        .expect("human: host.volumes with null params (ADR 0004)");
    assert!(
        res_null
            .volumes
            .iter()
            .any(|v| v.mount.to_wire() == "file:///"),
        "null params must behave like include_pseudo: false by default"
    );
}

/// M4 (ADR 0034, BLOCKER review): `index.build` and `index.query` gate READS
/// by actor just like `fs.search`. A scopeless agent CANNOT walk an arbitrary
/// tree (whose paths would go out via `task.progress`) nor query the
/// index — both return `PolicyDenied out-of-scope`. The gate runs BEFORE the
/// engine, so it does not matter that the test daemon has no index
/// installed.
#[tokio::test]
async fn a_scopeless_agent_cannot_index_build_nor_query() {
    let d = spawn_daemon_policy().await;
    let agent = connected_agent(&d, "s1").await;
    let assert_denied = |err: ClientError| match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
            "PolicyDenied out-of-scope, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    };
    // index.build out of scope → denied (never walks the tree).
    let err = agent
        .call::<_, FsTaskResult>(
            methods::INDEX_BUILD,
            &methods::IndexBuildParams {
                root: vp("mem:///"),
            },
        )
        .await
        .expect_err("index.build with no scope denied");
    assert_denied(err);
    // index.query out of scope → denied.
    let err = agent
        .call::<_, methods::IndexQueryResult>(
            methods::INDEX_QUERY,
            &methods::IndexQueryParams {
                root: vp("mem:///"),
                text: "x".into(),
                limit: 10,
            },
        )
        .await
        .expect_err("index.query with no scope denied");
    assert_denied(err);
}

/// M3-3b Task 3: scope round-trip. An agent requests (`request_scope`) —
/// without a grant its copy inside is still denied —; a human grants
/// #132, and `protocol-guardian` found it: **`archive.pack` cannot be a
/// laundering service.**
///
/// The read gate looks only at the request's ROOT and nothing else, so an
/// agent with a legitimate scope over a big tree could pack it whole — the
/// daemon's state directory included: `journal.db`, `secrets.age`,
/// `connections.toml` — and then read the archive entry by entry, over a
/// file that is within its own scope. An `fs.read` of any of those files is
/// denied; packing used to launder all of them.
///
/// What closes the hole are the SAME exclusions `fs.search` and `fs.compare`
/// already use (`policy::walk_exclusions`), applied to the walk.
#[tokio::test]
async fn an_agent_does_not_pack_what_it_cannot_walk() {
    let d = spawn_daemon_policy().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/visible.txt", b"esto se ve").await;

    let agent = connected_agent(&d, "s1").await;
    let req: RequestScopeResult = agent
        .call(
            methods::POLICY_REQUEST_SCOPE,
            &RequestScopeParams {
                session: "s1".into(),
                roots: vec![vp("mem:///proj")],
                ops: vec!["copy".into()],
                ttl_ms: 60_000,
            },
        )
        .await
        .expect("request_scope");
    let human = connected_client(&d).await;
    let _grant: GrantScopeResult = human
        .call(
            methods::POLICY_GRANT_SCOPE,
            &GrantScopeParams {
                request_id: req.request_id,
            },
        )
        .await
        .expect("grant_scope");

    // Inside the scope, packing proceeds: the gate is NOT a blanket "no".
    let ok: FsTaskResult = agent
        .call(
            methods::ARCHIVE_PACK,
            &methods::ArchivePackParams {
                sources: vec![vp("mem:///proj/visible.txt")],
                dest: vp("mem:///proj/out.zip"),
                format: methods::ArchiveFormat::Zip,
                level: None,
                base: vp("mem:///proj"),
            },
        )
        .await
        .expect("inside the scope it packs");
    assert!(ok.task_id.get() > 0);

    // And OUTSIDE it does not: a source with no scope is denied before
    // creating any Task at all.
    let err = agent
        .call::<_, FsTaskResult>(
            methods::ARCHIVE_PACK,
            &methods::ArchivePackParams {
                sources: vec![vp("mem:///otro")],
                dest: vp("mem:///proj/fuera.zip"),
                format: methods::ArchiveFormat::Zip,
                level: None,
                base: vp("mem:///"),
            },
        )
        .await
        .expect_err("outside the scope, no");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::PolicyDenied { .. })),
            "PolicyDenied, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }
}

/// (`grant_scope`) and then the copy INSIDE the scope proceeds, but OUTSIDE
/// it is still denied. Proves the registry is the SAME one the gate consults.
#[tokio::test]
async fn scope_request_grant_opens_the_border_and_only_inside() {
    let d = spawn_daemon_policy().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir proj");
    write_file(&d.mem, "mem:///proj/src.txt", b"hola").await;
    let copy = |from: &str, to: &str| FsCopyParams {
        from: vp(from),
        to: vp(to),
        on_collision: norte_proto::CollisionPolicy::default(),
        symlinks: norte_proto::SymlinkPolicy::default(),
        resume: norte_proto::ResumePolicy::default(),
        verify: norte_proto::VerifyPolicy::default(),
        dest_anchor: None,
        queued: false,
    };
    let assert_out_of_scope = |err: ClientError| match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
            "PolicyDenied out-of-scope, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    };

    let agent = connected_agent(&d, "s1").await;

    // 1) Requests scope for its own session: it stays pending.
    let req: RequestScopeResult = agent
        .call(
            methods::POLICY_REQUEST_SCOPE,
            &RequestScopeParams {
                session: "s1".into(),
                roots: vec![vp("mem:///proj")],
                ops: vec!["copy".into()],
                ttl_ms: 60_000,
            },
        )
        .await
        .expect("request_scope");

    // 2) With no grant, the copy INSIDE is still denied (border closed).
    let err = agent
        .call::<_, FsTaskResult>(
            methods::FS_COPY,
            &copy("mem:///proj/src.txt", "mem:///proj/dst.txt"),
        )
        .await
        .expect_err("pending, not yet granted");
    assert_out_of_scope(err);

    // 3) A human grants the request.
    let human = connected_client(&d).await;
    let _grant: GrantScopeResult = human
        .call(
            methods::POLICY_GRANT_SCOPE,
            &GrantScopeParams {
                request_id: req.request_id,
            },
        )
        .await
        .expect("grant_scope");

    // 4) Now the copy INSIDE the scope proceeds (border + allow rule).
    let ok: FsTaskResult = agent
        .call(
            methods::FS_COPY,
            &copy("mem:///proj/src.txt", "mem:///proj/dst.txt"),
        )
        .await
        .expect("inside the scope it proceeds");
    assert!(ok.task_id.get() > 0);

    // 5) But OUTSIDE the scope it is still denied (the grant is not a blank
    //    check: it only opens `mem:///proj`).
    let err_out = agent
        .call::<_, FsTaskResult>(
            methods::FS_COPY,
            &copy("mem:///proj/src.txt", "mem:///out.txt"),
        )
        .await
        .expect_err("outside the scope");
    assert_out_of_scope(err_out);
}

/// The request channel has a PER-CONNECTION sub-cap: one session requesting
/// with nobody granting does not exhaust the others' global cap.
#[tokio::test]
async fn request_scope_has_a_per_connection_sub_cap() {
    let d = spawn_daemon_policy().await;
    let agent = connected_agent(&d, "s1").await;
    let req = || RequestScopeParams {
        session: "s1".into(),
        roots: vec![vp("mem:///proj")],
        ops: vec!["copy".into()],
        ttl_ms: 60_000,
    };
    // MAX_PENDING_SCOPE_PER_CONN (16) requests get in; the 17th is OVERLOADED.
    for i in 0..16 {
        let _: RequestScopeResult = agent
            .call(methods::POLICY_REQUEST_SCOPE, &req())
            .await
            .unwrap_or_else(|e| panic!("request {i} within the cap: {e:?}"));
    }
    let err = agent
        .call::<_, RequestScopeResult>(methods::POLICY_REQUEST_SCOPE, &req())
        .await
        .expect_err("exceeds the sub-cap");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::OVERLOADED));
}

/// An ungranted scope request dies with the connection that created it: it
/// does not outlive its requester (anti-leak of the global channel). After
/// the agent disconnects, granting that `request_id` is `INVALID_PARAMS`.
#[tokio::test]
async fn a_pending_scope_is_cleaned_up_when_the_agent_disconnects() {
    let d = spawn_daemon_policy().await;
    let request_id = {
        let agent = connected_agent(&d, "s1").await;
        let r: RequestScopeResult = agent
            .call(
                methods::POLICY_REQUEST_SCOPE,
                &RequestScopeParams {
                    session: "s1".into(),
                    roots: vec![vp("mem:///proj")],
                    ops: vec!["copy".into()],
                    ttl_ms: 60_000,
                },
            )
            .await
            .expect("request_scope");
        r.request_id
        // `agent` is dropped here: its write half closes, the daemon sees
        // EOF and runs the cleanup of its pending entries.
    };
    // The cleanup is asynchronous on the daemon's side: it is POLLED until
    // the pending entry is gone, instead of sleeping a margin and asserting.
    // A fixed margin is a bet on the machine; this fails saying what it
    // expected.
    let human = connected_client(&d).await;
    until!("the dead agent's pending entry is cleaned up", {
        let r = human
            .call::<_, GrantScopeResult>(
                methods::POLICY_GRANT_SCOPE,
                &GrantScopeParams { request_id },
            )
            .await;
        // The condition IS the assertion: the loop only exits with the
        // expected TYPED error. Anything else — success, or another code —
        // keeps looping and ends up in the deadline's named failure, which
        // says what was expected instead of where it blew up.
        matches!(&r, Err(ClientError::Rpc(rpc)) if rpc.code == codes::INVALID_PARAMS)
    });
}

/// An agent cannot request scope for ANOTHER session (identity is fixed by
/// the connection, not the body); and a human cannot request scope (not
/// sandboxed).
#[tokio::test]
async fn request_scope_rejects_another_session_and_a_non_agent() {
    let d = spawn_daemon_policy().await;

    // Agent s1 requesting for s2 → INVALID_PARAMS (cannot fake its identity).
    let agent = connected_agent(&d, "s1").await;
    let err = agent
        .call::<_, RequestScopeResult>(
            methods::POLICY_REQUEST_SCOPE,
            &RequestScopeParams {
                session: "s2".into(),
                roots: vec![vp("mem:///proj")],
                ops: vec!["copy".into()],
                ttl_ms: 1000,
            },
        )
        .await
        .expect_err("another session");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));

    // A human (no agent_session) requesting scope → INVALID_REQUEST.
    let human = connected_client(&d).await;
    let err2 = human
        .call::<_, RequestScopeResult>(
            methods::POLICY_REQUEST_SCOPE,
            &RequestScopeParams {
                session: "whatever".into(),
                roots: vec![vp("mem:///proj")],
                ops: vec!["copy".into()],
                ttl_ms: 1000,
            },
        )
        .await
        .expect_err("a human does not request scope");
    assert!(matches!(err2, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));
}

/// An agent cannot GRANT (granting is a human act); and granting an unknown
/// `request_id` is `INVALID_PARAMS`.
#[tokio::test]
async fn grant_scope_is_human_and_an_unknown_id_fails() {
    let d = spawn_daemon_policy().await;

    // Agent trying to grant → INVALID_REQUEST.
    let agent = connected_agent(&d, "s1").await;
    let err = agent
        .call::<_, GrantScopeResult>(
            methods::POLICY_GRANT_SCOPE,
            &GrantScopeParams { request_id: 0 },
        )
        .await
        .expect_err("an agent does not grant");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));

    // A human granting an id that does not exist → INVALID_PARAMS.
    let human = connected_client(&d).await;
    let err2 = human
        .call::<_, GrantScopeResult>(
            methods::POLICY_GRANT_SCOPE,
            &GrantScopeParams { request_id: 999 },
        )
        .await
        .expect_err("unknown id");
    assert!(matches!(err2, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
}

// ---------- M3-3b Task 4: approval router (Ask round-trip) ----------

pub(super) fn copy_params(from: &str, to: &str) -> FsCopyParams {
    FsCopyParams {
        from: vp(from),
        to: vp(to),
        on_collision: norte_proto::CollisionPolicy::default(),
        symlinks: norte_proto::SymlinkPolicy::default(),
        resume: norte_proto::ResumePolicy::default(),
        verify: norte_proto::VerifyPolicy::default(),
        dest_anchor: None,
        queued: false,
    }
}

/// Grants the `agent`'s session a `copy` scope over `mem:///proj` with the
/// wire's round-trip (request + grant): the real path, not a shortcut.
pub(super) async fn grant_copy_scope(agent: &Client, human: &Client, session: &str) {
    let req: RequestScopeResult = agent
        .call(
            methods::POLICY_REQUEST_SCOPE,
            &RequestScopeParams {
                session: session.into(),
                roots: vec![vp("mem:///proj")],
                ops: vec!["copy".into()],
                ttl_ms: 60_000,
            },
        )
        .await
        .expect("request_scope");
    let _: GrantScopeResult = human
        .call(
            methods::POLICY_GRANT_SCOPE,
            &GrantScopeParams {
                request_id: req.request_id,
            },
        )
        .await
        .expect("grant_scope");
}

/// Next `policy.approval_required` from the notification stream (ignores
/// interleaved `task.progress`), with a wait cap.
pub(super) async fn next_approval(human: &mut Client) -> PolicyApprovalRequired {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let n = human.notification().await.expect("notif channel alive");
            if n.method == methods::POLICY_APPROVAL_REQUIRED {
                return serde_json::from_value::<PolicyApprovalRequired>(
                    n.params.expect("the notif carries params"),
                )
                .expect("PolicyApprovalRequired shape");
            }
        }
    })
    .await
    .expect("policy.approval_required arrives")
}

pub(super) fn assert_not_approved(err: ClientError) {
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved"),
            "PolicyDenied not-approved, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }
}

/// The Ask's E2E (M3-3b Task 4): an agent's copy under an `ask` rule
/// suspends, the human receives `policy.approval_required` with the context
/// (op, session, paths) and its `policy.decide approve` unblocks it.
#[tokio::test]
async fn an_approved_ask_unblocks_the_copy() {
    let d = spawn_daemon_ask(Duration::from_secs(30)).await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir proj");
    write_file(&d.mem, "mem:///proj/src.txt", b"hola").await;
    let agent = connected_agent(&d, "s1").await;
    let mut human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await;

    // The copy stays suspended in the Ask: it lives in its own task. It only
    // holds up ITS OWN connection's dispatch — the human is still served.
    let copy = tokio::spawn(async move {
        agent
            .call::<_, FsTaskResult>(
                methods::FS_COPY,
                &copy_params("mem:///proj/src.txt", "mem:///proj/dst.txt"),
            )
            .await
    });

    let notif = next_approval(&mut human).await;
    assert_eq!(notif.op, "copy");
    assert_eq!(notif.session.as_deref(), Some("s1"));
    assert!(
        notif.paths.iter().any(|p| p.contains("src.txt"))
            && notif.paths.iter().any(|p| p.contains("dst.txt")),
        "the display paths travel: {:?}",
        notif.paths
    );

    let _: PolicyDecideResult = human
        .call(
            methods::POLICY_DECIDE,
            &PolicyDecideParams {
                approval_id: notif.approval_id,
                approve: true,
            },
        )
        .await
        .expect("decide approve");
    let res = copy
        .await
        .expect("join")
        .expect("approved, the copy proceeds");
    assert!(res.task_id.get() > 0);
}

/// `policy.decide approve=false` denies: the copy answers `PolicyDenied`
/// `not-approved` and the destination stays intact (PRE-effect gate).
#[tokio::test]
async fn a_denied_ask_is_policy_denied_with_no_fs_touched() {
    let d = spawn_daemon_ask(Duration::from_secs(30)).await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir proj");
    write_file(&d.mem, "mem:///proj/src.txt", b"hola").await;
    let agent = connected_agent(&d, "s1").await;
    let mut human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await;

    let copy = tokio::spawn(async move {
        agent
            .call::<_, FsTaskResult>(
                methods::FS_COPY,
                &copy_params("mem:///proj/src.txt", "mem:///proj/dst.txt"),
            )
            .await
    });
    let notif = next_approval(&mut human).await;
    let _: PolicyDecideResult = human
        .call(
            methods::POLICY_DECIDE,
            &PolicyDecideParams {
                approval_id: notif.approval_id,
                approve: false,
            },
        )
        .await
        .expect("decide deny");
    assert_not_approved(copy.await.expect("join").expect_err("denied"));
    assert!(
        matches!(
            d.mem.stat(&vp("mem:///proj/dst.txt")).await,
            Err(norte_proto::Error::NotFound)
        ),
        "the destination was not touched"
    );

    // Re-deciding the same id: the decision consumed it. And since #279 the
    // reason TRAVELS — `already-decided`, not a silent `INVALID_PARAMS` —:
    // with two windows open that is exactly what happened, and telling
    // whoever clicked "your click didn't get through" sends them to retry
    // something already decided.
    let err = human
        .call::<_, PolicyDecideResult>(
            methods::POLICY_DECIDE,
            &PolicyDecideParams {
                approval_id: notif.approval_id,
                approve: true,
            },
        )
        .await
        .expect_err("id already decided");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::ApprovalGone { ref reason }) if reason == "already-decided"),
            "it had to say which of the three, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }

    // And an id this daemon never issued is a DIFFERENT thing: a stale modal
    // from before a restart, not a race between windows.
    let err = human
        .call::<_, PolicyDecideResult>(
            methods::POLICY_DECIDE,
            &PolicyDecideParams {
                approval_id: notif.approval_id.saturating_add(10_000),
                approve: true,
            },
        )
        .await
        .expect_err("id that does not exist");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::ApprovalGone { ref reason }) if reason == "unknown"),
            "an id never issued is `unknown`, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }
}

/// With no decision, the TTL expires and denies (`not-approved`): an absent
/// human does not leave the operation hanging. The notification announces
/// the real TTL.
#[tokio::test]
async fn an_ask_with_no_decision_expires_by_ttl() {
    let d = spawn_daemon_ask(Duration::from_millis(200)).await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir proj");
    write_file(&d.mem, "mem:///proj/src.txt", b"hola").await;
    let agent = connected_agent(&d, "s1").await;
    let mut human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await;

    let copy = tokio::spawn(async move {
        agent
            .call::<_, FsTaskResult>(
                methods::FS_COPY,
                &copy_params("mem:///proj/src.txt", "mem:///proj/dst.txt"),
            )
            .await
    });
    let notif = next_approval(&mut human).await;
    assert_eq!(notif.ttl_ms, 200, "the announced TTL is the router's");
    // Nobody decides: it expires.
    assert_not_approved(copy.await.expect("join").expect_err("expired TTL"));
}

/// `policy.pending` resync: a frontend that connects AFTER the broadcast sees
/// the pending entry. And the roles are respected: an agent neither decides
/// nor lists (`INVALID_REQUEST`) — it never self-approves.
#[tokio::test]
async fn pending_resync_and_an_agent_neither_decides_nor_lists() {
    let d = spawn_daemon_ask(Duration::from_secs(30)).await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir proj");
    write_file(&d.mem, "mem:///proj/src.txt", b"hola").await;
    let agent = connected_agent(&d, "s1").await;
    let mut human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await;

    let copy = tokio::spawn(async move {
        agent
            .call::<_, FsTaskResult>(
                methods::FS_COPY,
                &copy_params("mem:///proj/src.txt", "mem:///proj/dst.txt"),
            )
            .await
    });
    let notif = next_approval(&mut human).await;

    // A NEW frontend (connected after the broadcast) resyncs via pending.
    let late = connected_client(&d).await;
    let listed: PolicyPendingResult = late
        .call(methods::POLICY_PENDING, &serde_json::json!({}))
        .await
        .expect("policy.pending");
    assert_eq!(listed.pending.len(), 1);
    assert_eq!(listed.pending[0].approval_id, notif.approval_id);
    assert_eq!(listed.pending[0].op, "copy");
    assert_eq!(listed.pending[0].session.as_deref(), Some("s1"));

    // Another agent connection: neither decides nor lists (INVALID_REQUEST).
    let agent2 = connected_agent(&d, "s2").await;
    let err = agent2
        .call::<_, PolicyDecideResult>(
            methods::POLICY_DECIDE,
            &PolicyDecideParams {
                approval_id: notif.approval_id,
                approve: true,
            },
        )
        .await
        .expect_err("an agent does not decide");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));
    let err2 = agent2
        .call::<_, PolicyPendingResult>(methods::POLICY_PENDING, &serde_json::json!({}))
        .await
        .expect_err("an agent does not list");
    assert!(matches!(err2, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));

    // Deciding an id this daemon never issued. Since #279 it SAYS SO:
    // `unknown`, not "already decided". The sequence starts at a clock-seed
    // precisely so a stale modal cannot match by collision, so a 9999 falls
    // below the first possible id — and that is exactly the distinction it
    // has to make.
    let err3 = human
        .call::<_, PolicyDecideResult>(
            methods::POLICY_DECIDE,
            &PolicyDecideParams {
                approval_id: 9999,
                approve: true,
            },
        )
        .await
        .expect_err("unknown id");
    match err3 {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::ApprovalGone { ref reason }) if reason == "unknown"),
            "an id outside the issued range is `unknown`, was {:?}",
            rpc.data
        ),
        other => panic!("expected ApprovalGone, got {other:?}"),
    }

    // Unblocks and closes: denied, and the list is left empty.
    let _: PolicyDecideResult = human
        .call(
            methods::POLICY_DECIDE,
            &PolicyDecideParams {
                approval_id: notif.approval_id,
                approve: false,
            },
        )
        .await
        .expect("decide deny");
    assert_not_approved(copy.await.expect("join").expect_err("denied"));
    let listed: PolicyPendingResult = late
        .call(methods::POLICY_PENDING, &serde_json::json!({}))
        .await
        .expect("empty policy.pending");
    assert!(listed.pending.is_empty());
}

/// `policy.approval_required` goes ONLY to human connections (security
/// MAJOR-1): a subscribed agent must not passively enumerate OTHER sessions'
/// paths/ops — same criterion as `policy.pending`'s User-only gate.
#[tokio::test]
async fn approval_required_is_not_broadcast_to_agents() {
    let d = spawn_daemon_ask(Duration::from_secs(30)).await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir proj");
    write_file(&d.mem, "mem:///proj/src.txt", b"hola").await;
    let agent = connected_agent(&d, "s1").await;
    // The spy connects BEFORE the Ask: its subscription already exists when
    // broadcasting.
    let mut spy = connected_agent(&d, "s2").await;
    let mut human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await;

    let copy = tokio::spawn(async move {
        agent
            .call::<_, FsTaskResult>(
                methods::FS_COPY,
                &copy_params("mem:///proj/src.txt", "mem:///proj/dst.txt"),
            )
            .await
    });
    // The human DOES receive it (proof that the broadcast already went out)…
    let notif = next_approval(&mut human).await;
    // …and the spy agent does not get it within a generous later margin.
    let leaked = tokio::time::timeout(Duration::from_millis(400), async {
        loop {
            let n = spy.notification().await.expect("notif channel alive");
            if n.method == methods::POLICY_APPROVAL_REQUIRED {
                return;
            }
        }
    })
    .await
    .is_ok();
    assert!(!leaked, "an agent never sees another's approval_required");

    // Closes: denies and unblocks the suspended copy.
    let _: PolicyDecideResult = human
        .call(
            methods::POLICY_DECIDE,
            &PolicyDecideParams {
                approval_id: notif.approval_id,
                approve: false,
            },
        )
        .await
        .expect("decide deny");
    assert_not_approved(copy.await.expect("join").expect_err("denied"));
}

/// An AGENT does not hand over, just as it does not shut down: it is a human
/// governance act.
#[tokio::test]
async fn an_agent_cannot_hand_over() {
    let d = spawn_daemon(None).await;
    let c = connected_agent(&d, "sesion-de-prueba").await;
    let err = c
        .call::<_, DaemonShutdownResult>(
            methods::DAEMON_SHUTDOWN,
            &DaemonShutdownParams {
                graceful: true,
                mode: methods::ShutdownMode::Handover,
            },
        )
        .await
        .expect_err("an agent may not");
    assert!(
        matches!(&err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST),
        "{err:?}"
    );
}

#[tokio::test]
async fn the_daemon_shuts_down_only_from_idleness() {
    // Generous idle (1.2s): a frozen CI must not be able to shut the daemon
    // down before the client manages to connect (rust-reviewer m3).
    let d = spawn_daemon(Some(Duration::from_millis(1200))).await;
    {
        // A brief connection: while it is alive, no shutdown.
        //
        // Another timer OF THE SYSTEM UNDER TEST: it has to go past the
        // idleness span (1.2s) to be able to assert it did NOT shut down. It
        // is a negative assertion about someone else's deadline, so there is
        // no condition to poll: the proof is that at 1.5s it is still alive.
        let _c = connected_client(&d).await;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(
            !d.run.is_finished(),
            "with a live client it does not shut down"
        );
    }
    // Client gone: the idle timeout fires.
    let joined = tokio::time::timeout(Duration::from_secs(5), d.run)
        .await
        .expect("idle shutdown before the timeout")
        .expect("clean join");
    joined.expect("shutdown with no error");
}

#[tokio::test]
async fn two_daemons_do_not_share_a_socket() {
    let d = spawn_daemon(None).await;
    let engine = Arc::new(Engine::new());
    let err = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(d.socket.clone()),
            idle_timeout: None,
            listing_ttl: std::time::Duration::from_mins(2),
            plugins_dir: d.socket.parent().map(std::path::Path::to_path_buf),
            state_dir: None,
        },
    )
    .await
    .expect_err("the socket is alive");
    assert!(matches!(err, DaemonError::AlreadyRunning), "{err:?}");
}

// ---------- socket security ----------

#[tokio::test]
async fn bind_rejects_a_symlinked_dir() {
    let dir = tempfile::tempdir().expect("tempdir");
    let real = dir.path().join("real");
    std::fs::create_dir(&real).expect("mkdir");
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    let engine = Arc::new(Engine::new());
    let err = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(link.join("d.sock")),
            idle_timeout: None,
            listing_ttl: std::time::Duration::from_mins(2),
            plugins_dir: Some(dir.path().to_path_buf()),
            state_dir: None,
        },
    )
    .await
    .expect_err("symlinked dir rejected");
    assert!(matches!(err, DaemonError::InsecureDir { .. }), "{err:?}");
}

#[tokio::test]
async fn bind_hardens_the_dirs_and_the_sockets_mode() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let sock_dir = dir.path().join("laxo");
    std::fs::create_dir(&sock_dir).expect("mkdir");
    std::fs::set_permissions(&sock_dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let d = spawn_daemon_at(sock_dir.join("d.sock")).await;
    let md = std::fs::metadata(&sock_dir).expect("stat dir");
    assert_eq!(md.permissions().mode() & 0o777, 0o700, "dir hardened");
    let md = std::fs::metadata(&d.socket).expect("stat socket");
    assert_eq!(md.permissions().mode() & 0o777, 0o600, "socket 0600");
    let _ = d;
}

// ---------- M3-4 T4: journal in the daemon + policy.undo_session ----------

/// A daemon with a (in-memory) JOURNAL + `ScopedPolicy` with an `allow` rule +
/// a shared scope registry — the real M3-4 daemon's scenario (sole owner of
/// the journal, ADR 0024).
pub(super) async fn spawn_daemon_journal() -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let scopes = ScopeRegistry::new();
    let cfg = PolicyConfig::parse("[[rule]]\naction=\"allow\"").expect("policy cfg");
    let policy = ScopedPolicy::new(scopes.clone(), cfg);
    let journal = std::sync::Arc::new(norte_core::SqliteJournal::new(
        norte_core::Journal::open_in_memory()
            .await
            .expect("journal"),
    ));
    let engine =
        Arc::new(Engine::with_journal(journal).with_policy(Arc::new(policy), Arc::new(DenyAll)));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    engine.set_spool(norte_core::sync::Spool::new(dir.path()));
    let daemon = Daemon::bind_with_scopes(
        engine,
        scopes,
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
        run,
        dir,
        mem,
    }
}

/// Waits for `task_id`'s terminal state via `task.list` (resync retains
/// recent outcomes), with a cap.
pub(super) async fn wait_terminal(
    c: &Client,
    task_id: norte_proto::TaskId,
) -> norte_proto::TaskState {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let listed: methods::TaskListResult = c
            .call(methods::TASK_LIST, &methods::TaskListParams {})
            .await
            .expect("task.list");
        if let Some(t) = listed.tasks.iter().find(|t| t.task_id == task_id)
            && t.state.is_terminal()
        {
            return t.state.clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "task {task_id:?} never finished"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// M3-4 T4: a human undoes an agent's whole session over the wire. The agent
/// (with scope+allow) copies; `policy.undo_session` reverts it even though
/// the agent no longer has scope (executor=User).
#[tokio::test]
async fn policy_undo_session_reverts_the_agents_work() {
    let d = spawn_daemon_journal().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/src.txt", b"hola").await;

    // Ephemeral scope over the wire and the agent's copy.
    let agent = connected_agent(&d, "s1").await;
    let human = connected_client(&d).await;
    let req: RequestScopeResult = agent
        .call(
            methods::POLICY_REQUEST_SCOPE,
            &RequestScopeParams {
                session: "s1".into(),
                roots: vec![vp("mem:///proj")],
                ops: vec!["copy".into()],
                ttl_ms: 60_000,
            },
        )
        .await
        .expect("request_scope");
    let _: GrantScopeResult = human
        .call(
            methods::POLICY_GRANT_SCOPE,
            &GrantScopeParams {
                request_id: req.request_id,
            },
        )
        .await
        .expect("grant");
    let copied: FsTaskResult = agent
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///proj/src.txt"),
                to: vp("mem:///proj/dst.txt"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
                queued: false,
            },
        )
        .await
        .expect("the agent's copy");
    assert_eq!(
        wait_terminal(&human, copied.task_id).await,
        norte_proto::TaskState::Completed
    );
    assert!(d.mem.stat(&vp("mem:///proj/dst.txt")).await.is_ok());

    // The human undoes the agent's session.
    let undone: methods::PolicyUndoSessionResult = human
        .call(
            methods::POLICY_UNDO_SESSION,
            &methods::PolicyUndoSessionParams {
                session: "s1".into(),
            },
        )
        .await
        .expect("policy.undo_session");
    assert_eq!(
        wait_terminal(&human, undone.task_id).await,
        norte_proto::TaskState::Completed
    );
    assert!(
        matches!(
            d.mem.stat(&vp("mem:///proj/dst.txt")).await,
            Err(norte_proto::Error::NotFound)
        ),
        "the agent's copy was reverted"
    );
    assert!(
        d.mem.stat(&vp("mem:///proj/src.txt")).await.is_ok(),
        "the original is intact"
    );
}

/// `policy.undo_session`'s roles and validation: an agent does not call it
/// (`INVALID_REQUEST`) and a session with an illegal format is
/// `INVALID_PARAMS`.
#[tokio::test]
async fn policy_undo_session_roles_and_validation() {
    let d = spawn_daemon_journal().await;
    let agent = connected_agent(&d, "s1").await;
    let err = agent
        .call::<_, methods::PolicyUndoSessionResult>(
            methods::POLICY_UNDO_SESSION,
            &methods::PolicyUndoSessionParams {
                session: "s1".into(),
            },
        )
        .await
        .expect_err("an agent does not undo sessions this way");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));

    let human = connected_client(&d).await;
    let err2 = human
        .call::<_, methods::PolicyUndoSessionResult>(
            methods::POLICY_UNDO_SESSION,
            &methods::PolicyUndoSessionParams {
                session: "con espacios".into(),
            },
        )
        .await
        .expect_err("illegal session");
    assert!(matches!(err2, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
}

/// An AGENT (a connection with `agent_session`) CANNOT approve a plugin:
/// consenting to capabilities is a human security act → `INVALID_REQUEST`.
#[tokio::test]
async fn plugin_set_approval_by_an_agent_is_invalid_request() {
    let d = spawn_daemon_plugins().await;
    let agent = connected_agent(&d, "claude-01").await;
    let err = agent
        .call::<_, methods::PluginSetApprovalResult>(
            methods::PLUGIN_SET_APPROVAL,
            &methods::PluginSetApprovalParams {
                id: "org.norte.demo".into(),
                approved: true,
                expected_digest: None,
            },
        )
        .await
        .expect_err("an agent does not approve plugins");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));

    // And it did not touch the state: still unapproved for a human.
    let human = connected_client(&d).await;
    let list: methods::PluginListResult = human
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list");
    assert!(!list.plugins[0].approved, "the rejection left no trace");
}

/// Approving an UNKNOWN id is `INVALID_PARAMS` (state is not dirtied with
/// phantom plugins).
#[tokio::test]
async fn plugin_set_approval_of_an_unknown_id_is_invalid_params() {
    let d = spawn_daemon_plugins().await;
    let human = connected_client(&d).await;
    let err = human
        .call::<_, methods::PluginSetApprovalResult>(
            methods::PLUGIN_SET_APPROVAL,
            &methods::PluginSetApprovalParams {
                id: "org.norte.fantasma".into(),
                approved: true,
                expected_digest: None,
            },
        )
        .await
        .expect_err("unknown id");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
}

/// The anchor the human READ is the one that grants (#282).
///
/// The daemon used to anchor the digest IT had at write time, not the one
/// shown, so between the `plugin.list` the human saw and the confirming
/// `set_approval`, a different `plugin.toml` could fit. With
/// `expected_digest` the daemon refuses, and with the variant that means
/// "read it again": NOT `INVALID_PARAMS`, which is the code for "that plugin
/// does not exist" and would leave a client unable to distinguish the two
/// things.
#[tokio::test]
async fn plugin_set_approval_with_a_stale_anchor_is_refused() {
    let d = spawn_daemon_plugins().await;
    let human = connected_client(&d).await;
    let err = human
        .call::<_, methods::PluginSetApprovalResult>(
            methods::PLUGIN_SET_APPROVAL,
            &methods::PluginSetApprovalParams {
                id: "org.norte.demo".into(),
                approved: true,
                expected_digest: Some("no-es-el-ancla-de-nadie".into()),
            },
        )
        .await
        .expect_err("an anchor that does not match does not grant");
    assert!(
        matches!(&err, ClientError::Rpc(rpc) if rpc.code != codes::INVALID_PARAMS),
        "a stale anchor and an unknown id cannot share a code: {err:?}"
    );

    // And it granted nothing: the check has to be fail-closed.
    let list: methods::PluginListResult = human
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list");
    assert!(!list.plugins[0].approved, "the rejection left no trace");
}

/// REVOKING does not check the anchor, and that is deliberate: taking away a
/// permission grants nothing, and refusing the revocation over a stale
/// anchor would keep alive exactly the permission someone is trying to take
/// away.
#[tokio::test]
async fn revoking_is_not_refused_over_a_stale_anchor() {
    let d = spawn_daemon_plugins().await;
    let human = connected_client(&d).await;
    let _: methods::PluginSetApprovalResult = human
        .call(
            methods::PLUGIN_SET_APPROVAL,
            &methods::PluginSetApprovalParams {
                id: "org.norte.demo".into(),
                approved: true,
                expected_digest: None,
            },
        )
        .await
        .expect("approved first");

    let _: methods::PluginSetApprovalResult = human
        .call(
            methods::PLUGIN_SET_APPROVAL,
            &methods::PluginSetApprovalParams {
                id: "org.norte.demo".into(),
                approved: false,
                expected_digest: Some("no-es-el-ancla-de-nadie".into()),
            },
        )
        .await
        .expect("revoking does not look at the anchor");

    let list: methods::PluginListResult = human
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list");
    assert!(!list.plugins[0].approved, "the revocation applied");
}

/// An UNKNOWN id with an anchor is still `INVALID_PARAMS`: `manifest_digest`
/// returns `None` for both cases, and answering "the manifest changed" to
/// whoever named a plugin that does not exist is the wrong diagnosis for a
/// badly-written client's most common mistake.
#[tokio::test]
async fn an_unknown_id_with_an_anchor_is_not_confused_with_a_stale_one() {
    let d = spawn_daemon_plugins().await;
    let human = connected_client(&d).await;
    let err = human
        .call::<_, methods::PluginSetApprovalResult>(
            methods::PLUGIN_SET_APPROVAL,
            &methods::PluginSetApprovalParams {
                id: "org.norte.fantasma".into(),
                approved: true,
                expected_digest: Some("da-igual".into()),
            },
        )
        .await
        .expect_err("unknown id");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
}

/// Same criterion as `plugin.list`: reading documentation consents to
/// nothing, so an AGENT can also ask for the page.
#[tokio::test]
async fn plugin_help_is_open_to_an_agent() {
    let d = spawn_daemon_help_plugin(|_root, plugin_dir| {
        std::fs::write(plugin_dir.join("help.md"), "# Demo\n").expect("write help.md");
    })
    .await;
    let agent = connected_agent(&d, "claude-01").await;
    let help: methods::PluginHelpResult = agent
        .call(
            methods::PLUGIN_HELP,
            &methods::PluginHelpParams {
                id: "org.norte.demo".into(),
            },
        )
        .await
        .expect("an agent can read a plugin's page");
    assert!(help.markdown.contains("Demo"));
}

/// An AGENT (a connection with `agent_session`) CANNOT change a plugin
/// setting: it is USER data, same criterion as
/// `plugin_set_approval_by_an_agent_is_invalid_request` → `INVALID_REQUEST`,
/// and it leaves NO trace (the human still sees the default).
#[tokio::test]
async fn plugin_set_config_by_an_agent_is_invalid_request() {
    let d = spawn_daemon_config_plugin().await;
    let agent = connected_agent(&d, "claude-01").await;
    let err = agent
        .call::<_, methods::PluginSetConfigResult>(
            methods::PLUGIN_SET_CONFIG,
            &methods::PluginSetConfigParams {
                id: "org.norte.cfg".into(),
                key: "greeting".into(),
                value: "hola agente".into(),
            },
        )
        .await
        .expect_err("an agent does not change plugin settings");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));

    let human = connected_client(&d).await;
    let res: methods::PluginGetConfigResult = human
        .call(
            methods::PLUGIN_GET_CONFIG,
            &methods::PluginGetConfigParams {
                id: "org.norte.cfg".into(),
            },
        )
        .await
        .expect("get_config");
    assert_eq!(
        res.keys.iter().find(|k| k.key == "greeting").unwrap().value,
        "hola",
        "the rejection left no trace"
    );
}

/// Deliberately invalid TOML: the discoverer must report it in `errors`, not
/// bring down the catalogue. An unclosed `[[[` does not parse.
pub(super) const BROKEN_MANIFEST: &str = "no es toml [[[";

/// A daemon pointed at a `cfg` seeded with a VALID plugin (`org.norte.demo`)
/// and a BROKEN one (`rota`, invalid TOML). Also returns the `cfg` path so a
/// fresh `PluginRegistry` can be opened over it to check persistence.
pub(super) async fn spawn_daemon_plugins_ok_y_roto() -> (TestDaemon, PathBuf) {
    spawn_daemon_plugins_con(&[]).await
}

/// Like [`spawn_daemon_plugins_ok_y_roto`], plus whatever `(id, manifest)`
/// pairs are passed, seeded BEFORE starting up: the daemon discovers once, at
/// startup, and a test on its in-memory registry has to seed beforehand.
pub(super) async fn spawn_daemon_plugins_con(extra: &[(&str, &str)]) -> (TestDaemon, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = dir.path().join("cfg");
    let ok_dir = cfg.join("plugins").join("org.norte.demo");
    let broken_dir = cfg.join("plugins").join("rota");
    std::fs::create_dir_all(&ok_dir).expect("mkdir ok");
    std::fs::create_dir_all(&broken_dir).expect("mkdir broken");
    std::fs::write(ok_dir.join("plugin.toml"), DEMO_MANIFEST).expect("write ok manifest");
    std::fs::write(broken_dir.join("plugin.toml"), BROKEN_MANIFEST).expect("write broken manifest");
    for (id, manifest) in extra {
        let d = cfg.join("plugins").join(id);
        std::fs::create_dir_all(&d).expect("mkdir extra");
        std::fs::write(d.join("plugin.toml"), manifest).expect("write extra manifest");
    }

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
            plugins_dir: Some(cfg.clone()),
            state_dir: None,
        },
    )
    .await
    .expect("bind");
    let run = tokio::spawn(daemon.run());
    let d = TestDaemon {
        socket,
        run,
        dir,
        mem,
    };
    (d, cfg)
}

/// M4-P3's closing E2E: a COMPLETE round-trip of the extension manager over
/// the wire — discovery (valid + broken), human consent (approve +
/// enable), and DURABLE persistence re-read by a FRESH `PluginRegistry` (no
/// daemon). Ties together catalogue + state + masked errors in a single
/// flow.
#[tokio::test]
async fn the_plugin_manager_e2e_lists_governs_and_persists() {
    // `cfg` is the config root, seeded with a valid plugin and a broken one.
    let (d, cfg) = spawn_daemon_plugins_ok_y_roto().await;

    // 1) plugin.list: a valid one discovered (unapproved/disabled, fs-read
    //    capability visible) and a broken one reported by its BASENAME
    //    (never the absolute path, which would leak the user's home to an
    //    agent).
    let human = connected_client(&d).await;
    let list: methods::PluginListResult = human
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list");
    assert_eq!(list.plugins.len(), 1, "only the valid one loads");
    let p = &list.plugins[0];
    assert_eq!(p.id, "org.norte.demo");
    assert!(!p.approved, "born unapproved");
    assert!(!p.enabled, "born disabled");
    assert!(
        p.capabilities.iter().any(|c| c == "fs-read"),
        "the declared capability is exposed as a badge: {:?}",
        p.capabilities
    );
    assert_eq!(
        list.errors.len(),
        1,
        "the broken one is reported, not hidden"
    );
    let broken = &list.errors[0];
    assert_eq!(
        broken.dir, "rota",
        "only the basename, not the absolute path"
    );
    assert!(
        !broken.dir.contains('/'),
        "the reported dir is never a path: {}",
        broken.dir
    );

    // 2) The human approves and enables over the wire.
    let _: methods::PluginSetApprovalResult = human
        .call(
            methods::PLUGIN_SET_APPROVAL,
            &methods::PluginSetApprovalParams {
                id: "org.norte.demo".into(),
                approved: true,
                expected_digest: None,
            },
        )
        .await
        .expect("approve");
    let _: methods::PluginSetEnabledResult = human
        .call(
            methods::PLUGIN_SET_ENABLED,
            &methods::PluginSetEnabledParams {
                id: "org.norte.demo".into(),
                enabled: true,
            },
        )
        .await
        .expect("enable");

    // 3) plugin.list reflects it on the SAME daemon.
    let list: methods::PluginListResult = human
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list after consenting");
    assert!(list.plugins[0].approved, "approved is reflected");
    assert!(list.plugins[0].enabled, "enabled is reflected");

    // 4) DURABLE PERSISTENCE: a FRESH registry opened directly over `cfg`
    //    (no daemon) remembers both flags → `cfg/plugins-state.toml` was
    //    really written.
    let fresh = norte_core::PluginRegistry::discover(&cfg).expect("fresh discover");
    let persisted = fresh.list();
    assert_eq!(persisted.plugins.len(), 1);
    assert!(
        persisted.plugins[0].approved,
        "approved persisted in plugins-state.toml"
    );
    assert!(
        persisted.plugins[0].enabled,
        "enabled persisted in plugins-state.toml"
    );
    assert!(
        cfg.join("plugins-state.toml").exists(),
        "the state was written to disk"
    );

    // 5) An AGENT cannot approve (a human security act → INVALID_REQUEST).
    let agent = connected_agent(&d, "s1").await;
    let err = agent
        .call::<_, methods::PluginSetApprovalResult>(
            methods::PLUGIN_SET_APPROVAL,
            &methods::PluginSetApprovalParams {
                id: "org.norte.demo".into(),
                approved: false,
                expected_digest: None,
            },
        )
        .await
        .expect_err("an agent does not govern consent");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));
}

/// `plugin.uninstall` (0.71.0, ADR 0104): deletes the directory, withdraws
/// consent and — what the CLI could not do — forgets it in the daemon's
/// IN-MEMORY registry, which until now kept listing the deleted plugin until
/// a restart. An agent cannot; an id that is not there, either.
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "six steps over the SAME daemon: splitting them would lose the in-memory registry being tested"
)]
async fn plugin_uninstall_over_the_wire_deletes_forgets_and_withdraws_consent() {
    // And a broken one with a VALID id, for step 5: `rota` is not an id and
    // cannot be named over the wire.
    let (d, cfg) = spawn_daemon_plugins_con(&[("org.norte.rota", BROKEN_MANIFEST)]).await;
    let human = connected_client(&d).await;
    let _: methods::PluginSetApprovalResult = human
        .call(
            methods::PLUGIN_SET_APPROVAL,
            &methods::PluginSetApprovalParams {
                id: "org.norte.demo".into(),
                approved: true,
                expected_digest: None,
            },
        )
        .await
        .expect("approve");

    // 1) An AGENT does not uninstall: withdrawing a consent is as much the
    //    human's as giving it, and deleting files from their configuration,
    //    even more so.
    let agent = connected_agent(&d, "s1").await;
    let err = agent
        .call::<_, methods::PluginUninstallResult>(
            methods::PLUGIN_UNINSTALL,
            &methods::PluginUninstallParams {
                id: "org.norte.demo".into(),
            },
        )
        .await
        .expect_err("an agent does not uninstall");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));
    assert!(
        cfg.join("plugins").join("org.norte.demo").is_dir(),
        "and the directory is still there"
    );

    // 2) The human does, and the report says there was consent.
    let r: methods::PluginUninstallResult = human
        .call(
            methods::PLUGIN_UNINSTALL,
            &methods::PluginUninstallParams {
                id: "org.norte.demo".into(),
            },
        )
        .await
        .expect("desinstalar");
    assert!(r.was_approved, "it had consent, and it says so");
    assert!(
        !cfg.join("plugins").join("org.norte.demo").exists(),
        "the directory was deleted"
    );

    // 3) The SAME daemon no longer lists it: the in-memory registry forgot it.
    let list: methods::PluginListResult = human
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list after uninstalling");
    assert!(
        list.plugins.iter().all(|p| p.id != "org.norte.demo"),
        "still listed after uninstalling: {:?}",
        list.plugins.iter().map(|p| &p.id).collect::<Vec<_>>()
    );

    // 4) And consent left with it: a plugin installed later under the same
    //    id is born unapproved.
    let ok_dir = cfg.join("plugins").join("org.norte.demo");
    std::fs::create_dir_all(&ok_dir).expect("mkdir again");
    std::fs::write(ok_dir.join("plugin.toml"), DEMO_MANIFEST).expect("write manifest");
    let fresh = norte_core::PluginRegistry::discover(&cfg).expect("fresh discover");
    let reinstalled = fresh
        .list()
        .plugins
        .into_iter()
        .find(|p| p.id == "org.norte.demo")
        .expect("discovered again");
    assert!(!reinstalled.approved, "born unapproved");
    assert!(!reinstalled.enabled, "and disabled");

    // 5) A BROKEN plugin — listed in `errors`, not in `plugins` — uninstalls
    //    just the same, and the SAME daemon stops advertising it as "failed
    //    to load": the corpse used to leave `plugins` and stay in `errors`.
    let broken = cfg.join("plugins").join("org.norte.rota");
    let list: methods::PluginListResult = human
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list");
    assert!(
        list.errors.iter().any(|e| e.dir == "org.norte.rota"),
        "the daemon advertises it as broken: {:?}",
        list.errors
    );
    let r: methods::PluginUninstallResult = human
        .call(
            methods::PLUGIN_UNINSTALL,
            &methods::PluginUninstallParams {
                id: "org.norte.rota".into(),
            },
        )
        .await
        .expect("uninstall a broken one");
    assert!(!r.was_approved, "a broken one never had consent");
    assert!(!broken.exists(), "and its directory was deleted");
    let list: methods::PluginListResult = human
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list");
    assert!(
        list.errors.iter().all(|e| e.dir != "org.norte.rota"),
        "an uninstalled broken one is no longer advertised: {:?}",
        list.errors
    );

    // 6) What is not there, or is not an id, is INVALID_PARAMS — never a path.
    for id in ["org.norte.nunca", "../fuera"] {
        let err = human
            .call::<_, methods::PluginUninstallResult>(
                methods::PLUGIN_UNINSTALL,
                &methods::PluginUninstallParams { id: id.into() },
            )
            .await
            .expect_err("not there");
        assert!(
            matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS),
            "{id}"
        );
    }
}

// ---------- #66: actor gate on task.* and connection.trust_host_key ----------

/// An AGENT's `task.list` only shows ITS OWN tasks: the human's (live or
/// recent) carry `current` with other actors' paths (security-reviewer
/// NOTE-1 in M3-4 T5). The human still sees EVERYTHING.
#[tokio::test]
async fn an_agents_task_list_only_shows_its_own_tasks() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///humano.bin", &vec![0xAA; 3000]).await;
    write_file(&d.mem, "mem:///agente.bin", &vec![0xBB; 3000]).await;
    let mut human = connected_client(&d).await;
    let mut agent = connected_agent(&d, "sess-list").await;

    let ht: FsTaskResult = human
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///humano.bin"),
                to: vp("mem:///humano2.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
                queued: false,
            },
        )
        .await
        .expect("the human's copy");
    drain_task(&mut human, ht.task_id.get()).await;

    let at: FsTaskResult = agent
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///agente.bin"),
                to: vp("mem:///agente2.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
                queued: false,
            },
        )
        .await
        .expect("the agent's copy");
    drain_task(&mut agent, at.task_id.get()).await;

    let humans_list: methods::TaskListResult = human
        .call(methods::TASK_LIST, &methods::TaskListParams {})
        .await
        .expect("the human's task.list");
    let ids: Vec<u64> = humans_list.tasks.iter().map(|t| t.task_id.get()).collect();
    assert!(ids.contains(&ht.task_id.get()), "the human sees its task");
    assert!(ids.contains(&at.task_id.get()), "the human sees EVERYTHING");

    let agents_list: methods::TaskListResult = agent
        .call(methods::TASK_LIST, &methods::TaskListParams {})
        .await
        .expect("the agent's task.list");
    let ids: Vec<u64> = agents_list.tasks.iter().map(|t| t.task_id.get()).collect();
    assert!(ids.contains(&at.task_id.get()), "the agent sees its OWN");
    assert!(
        !ids.contains(&ht.task_id.get()),
        "the agent does NOT observe the human's tasks (paths in `current`)"
    );
}

/// `connection.trust_host_key` is a HUMAN trust decision (like
/// `grant_scope`/`decide`/`undo_session`): an agent does not bless
/// fingerprints.
#[tokio::test]
async fn trust_host_key_by_an_agent_is_invalid_request() {
    let d = spawn_daemon(None).await;
    let agent = connected_agent(&d, "sess-tofu").await;
    let err = agent
        .call::<_, methods::ConnectionTrustHostKeyResult>(
            methods::CONNECTION_TRUST_HOST_KEY,
            &methods::ConnectionTrustHostKeyParams {
                host: "example.com".into(),
                port: Some(22),
                algo: "ssh-ed25519".into(),
                fingerprint: "SHA256:AAAA".into(),
            },
        )
        .await
        .expect_err("an agent does not accept host keys");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));
}

/// #325: typing a password is a HUMAN act. An agent that could inject
/// session credentials would be choosing which identity the user acts under
/// on the remote host, so the gate is the same as TOFU's.
#[tokio::test]
async fn provide_secret_by_an_agent_is_invalid_request() {
    let d = spawn_daemon(None).await;
    let agent = connected_agent(&d, "sess-secreto").await;
    let err = agent
        .call::<_, methods::ConnectionProvideSecretResult>(
            methods::CONNECTION_PROVIDE_SECRET,
            &methods::ConnectionProvideSecretParams {
                conn: "rosetta".into(),
                secret: "no-deberia-llegar".into(),
            },
        )
        .await
        .expect_err("an agent does not provide connection secrets");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));
}

/// `task.progress`'s broadcast is the SAME leak as `task.list`: an agent
/// connection does not receive the progress (with `current`) of other
/// actors' tasks. Another human does keep seeing it (phase 3's basis).
#[tokio::test]
async fn a_humans_task_progress_does_not_reach_agent_connections() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///src.bin", &vec![0x11; 2000]).await;
    let human = connected_client(&d).await;
    let mut human2 = connected_client(&d).await;
    let mut agent = connected_agent(&d, "sess-espia").await;

    let task: FsTaskResult = human
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
        .expect("the human's fs.copy");
    // The other human drains up to the terminal: at that point EVERY frame of
    // the task has already been broadcast (synchronous try_send at the same
    // instant).
    let seen = drain_task(&mut human2, task.task_id.get()).await;
    assert_eq!(seen.last().expect("terminal").state, TaskState::Completed);
    let leaked = tokio::time::timeout(Duration::from_millis(200), agent.notification()).await;
    assert!(
        leaked.is_err(),
        "an agent does not receive task.progress of other actors' tasks: {leaked:?}"
    );
}

/// `daemon.shutdown` is also a human act: without this gate, an agent
/// bypasses `task.cancel`'s (a hard shutdown cancels EVERY task) and takes
/// down the session's daemon (security-reviewer MAJOR in #66).
#[tokio::test]
async fn daemon_shutdown_by_an_agent_is_invalid_request() {
    let d = spawn_daemon(None).await;
    let agent = connected_agent(&d, "sess-apagon").await;
    let err = agent
        .call::<_, DaemonShutdownResult>(
            methods::DAEMON_SHUTDOWN,
            &DaemonShutdownParams {
                graceful: false,
                ..Default::default()
            },
        )
        .await
        .expect_err("an agent does not shut down the daemon");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));
    // The daemon is still alive and serving.
    let c = connected_client(&d).await;
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
        .expect("the daemon did not shut down");
}

// ---------- #71: policy.undo_report ----------

/// `policy.undo_report` is User-ONLY (same barrier as the undo that
/// generates it): the report carries a journal seq and a block reason.
#[tokio::test]
async fn undo_report_by_an_agent_is_invalid_request() {
    let d = spawn_daemon(None).await;
    let agent = connected_agent(&d, "sess-report").await;
    let err = agent
        .call::<_, methods::PolicyUndoReportResult>(
            methods::POLICY_UNDO_REPORT,
            &methods::PolicyUndoReportParams {
                task_id: norte_proto::TaskId::new(1),
            },
        )
        .await
        .expect_err("an agent does not read undo reports");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));
}

// ---------- #70: per-class caps (reserved for the human) ----------

/// An AGENT's outcomes occupy at most half of the `recent` ring: a burst of
/// trivial agent tasks does NOT evict the human's terminals from
/// `task.list`'s resync (#70).
#[tokio::test]
async fn an_agents_terminals_do_not_displace_the_humans() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///h.bin", &[0xAA; 100]).await;
    let mut human = connected_client(&d).await;
    let mut agent = connected_agent(&d, "sess-ruido").await;

    // 1) The human completes ONE task.
    let ht: FsTaskResult = human
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///h.bin"),
                to: vp("mem:///h2.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
                queued: false,
            },
        )
        .await
        .expect("human copy");
    drain_task(&mut human, ht.task_id.get()).await;

    // 2) The agent completes MORE tasks than the whole ring (64).
    for i in 0..70u32 {
        let at: FsTaskResult = agent
            .call(
                methods::FS_COPY,
                &FsCopyParams {
                    from: vp("mem:///h.bin"),
                    to: vp(&format!("mem:///a{i}.bin")),
                    on_collision: norte_proto::CollisionPolicy::default(),
                    symlinks: norte_proto::SymlinkPolicy::default(),
                    resume: norte_proto::ResumePolicy::default(),
                    verify: norte_proto::VerifyPolicy::default(),
                    dest_anchor: None,
                    queued: false,
                },
            )
            .await
            .expect("the agent's copy");
        drain_task(&mut agent, at.task_id.get()).await;
    }

    // 3) The human's terminal is STILL in its resync.
    let listed: methods::TaskListResult = human
        .call(methods::TASK_LIST, &methods::TaskListParams {})
        .await
        .expect("task.list");
    assert!(
        listed.tasks.iter().any(|t| t.task_id == ht.task_id),
        "the agent's noise does not evict the human's outcome"
    );
}

/// Agents' LIVE tasks have a sub-cap: even if they exhaust it, the human can
/// still queue (#70). The agent that oversteps receives OVERLOADED.
#[tokio::test]
async fn agents_live_tasks_do_not_exhaust_the_humans_quota() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///src.bin", &[0xBB; 100]).await;
    // The agent's tasks stay alive: the first one blocked on latency, the
    // rest queued in the scheduler (registered = alive).
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_mins(2)));
    let human = connected_client(&d).await;
    let agent = connected_agent(&d, "sess-gloton").await;

    // The agent queues up to its sub-cap (384): all accepted.
    for i in 0..384u32 {
        let _: FsTaskResult = agent
            .call(
                methods::FS_COPY,
                &FsCopyParams {
                    from: vp("mem:///src.bin"),
                    to: vp(&format!("mem:///d{i}.bin")),
                    on_collision: norte_proto::CollisionPolicy::default(),
                    symlinks: norte_proto::SymlinkPolicy::default(),
                    resume: norte_proto::ResumePolicy::default(),
                    verify: norte_proto::VerifyPolicy::default(),
                    dest_anchor: None,
                    queued: false,
                },
            )
            .await
            .unwrap_or_else(|e| panic!("the agent's copy {i} accepted: {e:?}"));
    }
    // The agent's 385th: OVERLOADED (its class is full).
    let err = agent
        .call::<_, FsTaskResult>(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///src.bin"),
                to: vp("mem:///glutton.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
                queued: false,
            },
        )
        .await
        .expect_err("the agent sub-cap cuts it off");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::OVERLOADED));

    // The human CAN STILL queue: its reserve is not touched.
    let _: FsTaskResult = human
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///src.bin"),
                to: vp("mem:///humano.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
                queued: false,
            },
        )
        .await
        .expect("the human's reserve survives the greedy agent");
}

// ---------- #64: peer death during a suspended Ask ----------

/// #64: the requester's DEATH cancels its suspended Ask — the pending entry
/// does NOT stay a zombie until the TTL. The dispatch races against the
/// socket's life: when the peer dies, the gate's future is dropped and its
/// RAII guard removes the pending entry from the router.
#[tokio::test]
async fn a_peers_death_cancels_its_suspended_ask() {
    // LONG TTL on purpose: only the peer's death can clean up in time.
    let d = spawn_daemon_ask(Duration::from_secs(30)).await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir proj");
    write_file(&d.mem, "mem:///proj/src.txt", b"hola").await;
    let agent = connected_agent(&d, "s1").await;
    let mut human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await;

    let copy = tokio::spawn(async move {
        let _ = agent
            .call::<_, FsTaskResult>(
                methods::FS_COPY,
                &copy_params("mem:///proj/src.txt", "mem:///proj/dst.txt"),
            )
            .await;
        agent
    });
    let notif = next_approval(&mut human).await;
    let listed: PolicyPendingResult = human
        .call(methods::POLICY_PENDING, &serde_json::json!({}))
        .await
        .expect("policy.pending");
    assert!(
        listed
            .pending
            .iter()
            .any(|p| p.approval_id == notif.approval_id),
        "the pending entry exists while the requester is alive"
    );

    // The requester dies: aborting the task drops its Client → EOF.
    copy.abort();
    let _ = copy.await;

    // The pending entry disappears SOON — not at the TTL's 30s.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let listed: PolicyPendingResult = human
            .call(methods::POLICY_PENDING, &serde_json::json!({}))
            .await
            .expect("policy.pending");
        if !listed
            .pending
            .iter()
            .any(|p| p.approval_id == notif.approval_id)
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "ZOMBIE pending entry: the peer's death did not cancel its Ask (#64)"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // And the destination was never touched (the gate died BEFORE the effect).
    assert!(matches!(
        d.mem.stat(&vp("mem:///proj/dst.txt")).await,
        Err(Error::NotFound)
    ));
}

// ---------- #72: rpc.cancel of a tools/call suspended in an Ask ----------

/// #72: the agent WITHDRAWS (rpc.cancel) its own `fs.copy` suspended in an
/// Ask — the daemon fires that in-flight request's token, drops the dispatch
/// (PRE-effect gate: its guard cleans up the pending entry), answers
/// `Error::Cancelled` and NEVER approves (fail-closed). Unlike the peer's
/// death (#64), the agent's connection STAYS ALIVE and usable after the
/// withdrawal.
#[tokio::test]
async fn rpc_cancel_withdraws_the_suspended_ask_without_killing_the_connection() {
    // LONG TTL: only the rpc.cancel can withdraw the Ask in time.
    let d = spawn_daemon_ask(Duration::from_secs(30)).await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir proj");
    write_file(&d.mem, "mem:///proj/src.txt", b"hola").await;
    let agent = Arc::new(connected_agent(&d, "s1").await);
    let mut human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await;

    // The agent launches fs.copy and captures the assigned JSON-RPC id (the
    // one an rpc.cancel has to target). `call_tracked` invokes `on_id`
    // BEFORE waiting.
    let id_slot = Arc::new(std::sync::Mutex::new(None::<u64>));
    let agent_copy = Arc::clone(&agent);
    let slot = Arc::clone(&id_slot);
    let copy = tokio::spawn(async move {
        agent_copy
            .call_tracked::<_, FsTaskResult>(
                methods::FS_COPY,
                &copy_params("mem:///proj/src.txt", "mem:///proj/dst.txt"),
                move |id| *slot.lock().expect("id lock") = Some(id),
            )
            .await
    });

    // The human sees the Ask: the pending entry exists while the copy is
    // suspended.
    let notif = next_approval(&mut human).await;
    let listed: PolicyPendingResult = human
        .call(methods::POLICY_PENDING, &serde_json::json!({}))
        .await
        .expect("policy.pending");
    assert!(
        listed
            .pending
            .iter()
            .any(|p| p.approval_id == notif.approval_id),
        "the pending entry exists while the copy is suspended"
    );

    // The agent WITHDRAWS its suspended request (rpc.cancel, best-effort notify).
    let id = wait_for_id(&id_slot).await;
    agent
        .notify(
            methods::RPC_CANCEL,
            &methods::RpcCancelParams {
                id: norte_proto::wire::RequestId::Num(id),
            },
        )
        .expect("rpc.cancel notify");

    // The fs.copy answers Error::Cancelled (never approved: fail-closed).
    let res = copy.await.expect("copy join");
    match res {
        Err(ClientError::Rpc(rpc)) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert_eq!(rpc.data, Some(Error::Cancelled));
        }
        other => panic!("expected Cancelled, got {other:?}"),
    }

    // The pending entry is removed SOON — not at the TTL's 30s.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let listed: PolicyPendingResult = human
            .call(methods::POLICY_PENDING, &serde_json::json!({}))
            .await
            .expect("policy.pending");
        if !listed
            .pending
            .iter()
            .any(|p| p.approval_id == notif.approval_id)
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "ZOMBIE pending entry: the rpc.cancel did not withdraw the Ask (#72)"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The destination was never touched (the gate died BEFORE the effect).
    assert!(matches!(
        d.mem.stat(&vp("mem:///proj/dst.txt")).await,
        Err(Error::NotFound)
    ));

    // The agent's connection STAYS ALIVE after the rpc.cancel (≠ peer death):
    // another request is served normally.
    let st: FsStatResult = agent
        .call(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("mem:///proj/src.txt"),
                attrs: Vec::new(),
            },
        )
        .await
        .expect("the connection is still alive after the rpc.cancel");
    assert_eq!(st.entry.size, Some(4));
}

/// #72 (race A): `rpc.cancel` WINS over a later `policy.decide`. The agent
/// withdraws its suspended fs.copy; when the human later tries to approve it,
/// the pending entry no longer exists → `policy.decide` answers
/// `INVALID_PARAMS` (not a silent ok) and the destination is never touched.
#[tokio::test]
async fn cancel_wins_over_a_later_decide() {
    // LONG TTL: only the rpc.cancel can withdraw the Ask in time.
    let d = spawn_daemon_ask(Duration::from_secs(30)).await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir proj");
    write_file(&d.mem, "mem:///proj/src.txt", b"hola").await;
    let agent = Arc::new(connected_agent(&d, "s1").await);
    let mut human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await;

    let id_slot = Arc::new(std::sync::Mutex::new(None::<u64>));
    let agent_copy = Arc::clone(&agent);
    let slot = Arc::clone(&id_slot);
    let copy = tokio::spawn(async move {
        agent_copy
            .call_tracked::<_, FsTaskResult>(
                methods::FS_COPY,
                &copy_params("mem:///proj/src.txt", "mem:///proj/dst.txt"),
                move |id| *slot.lock().expect("id lock") = Some(id),
            )
            .await
    });

    // The human sees the Ask (the copy is suspended): synchronizes the race.
    let notif = next_approval(&mut human).await;

    // ACTION 1 (the one "first"): the agent WITHDRAWS the suspended request.
    let id = wait_for_id(&id_slot).await;
    agent
        .notify(
            methods::RPC_CANCEL,
            &methods::RpcCancelParams {
                id: norte_proto::wire::RequestId::Num(id),
            },
        )
        .expect("rpc.cancel notify");

    // The fs.copy answers Cancelled (fail-closed: never approved).
    match copy.await.expect("copy join") {
        Err(ClientError::Rpc(rpc)) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert_eq!(rpc.data, Some(Error::Cancelled));
        }
        other => panic!("expected Cancelled, got {other:?}"),
    }

    // ACTION 2 (arrives LATE): the human tries to approve the already-
    // withdrawn one. The pending entry does not exist → error, never a
    // silent ok. And since #279 it says which of the three forms:
    // `already-decided`, because that id DID exist and someone resolved it —
    // here, the requester itself withdrawing it. What it must not answer is
    // `unknown`, which would send whoever clicked off looking for a
    // restarted daemon that does not exist.
    let decide: Result<PolicyDecideResult, ClientError> = human
        .call(
            methods::POLICY_DECIDE,
            &PolicyDecideParams {
                approval_id: notif.approval_id,
                approve: true,
            },
        )
        .await;
    match decide {
        Err(ClientError::Rpc(rpc)) => assert!(
            matches!(rpc.data, Some(Error::ApprovalGone { ref reason }) if reason == "already-decided"),
            "deciding over a withdrawn pending entry has to say it was already resolved, was {:?}",
            rpc.data
        ),
        other => panic!("expected ApprovalGone, got {other:?}"),
    }

    // The destination was never executed.
    assert!(matches!(
        d.mem.stat(&vp("mem:///proj/dst.txt")).await,
        Err(Error::NotFound)
    ));
}

/// #72 (race B): `policy.decide` WINS over a later `rpc.cancel`. The human
/// approves before the withdrawal arrives; the copy proceeds as a governed
/// Task and the `rpc.cancel` of the ALREADY resolved request is a benign
/// no-op that does not disturb the agent's connection.
#[tokio::test]
async fn decide_wins_over_a_later_cancel() {
    let d = spawn_daemon_ask(Duration::from_secs(30)).await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir proj");
    write_file(&d.mem, "mem:///proj/src.txt", b"hola").await;
    let agent = Arc::new(connected_agent(&d, "s1").await);
    let mut human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await;

    let id_slot = Arc::new(std::sync::Mutex::new(None::<u64>));
    let agent_copy = Arc::clone(&agent);
    let slot = Arc::clone(&id_slot);
    let copy = tokio::spawn(async move {
        agent_copy
            .call_tracked::<_, FsTaskResult>(
                methods::FS_COPY,
                &copy_params("mem:///proj/src.txt", "mem:///proj/dst.txt"),
                move |id| *slot.lock().expect("id lock") = Some(id),
            )
            .await
    });

    // The human sees the Ask and APPROVES (the "first" action).
    let notif = next_approval(&mut human).await;
    let _: PolicyDecideResult = human
        .call(
            methods::POLICY_DECIDE,
            &PolicyDecideParams {
                approval_id: notif.approval_id,
                approve: true,
            },
        )
        .await
        .expect("decide approve");

    // Approved → the copy proceeds: joins Ok with an assigned task_id.
    let res = copy
        .await
        .expect("copy join")
        .expect("approved, the copy proceeds");
    assert!(res.task_id.get() > 0);

    // ACTION 2 (arrives LATE): rpc.cancel of the ALREADY resolved request.
    // Benign no-op — must NOT disturb the agent's connection.
    let id = wait_for_id(&id_slot).await;
    agent
        .notify(
            methods::RPC_CANCEL,
            &methods::RpcCancelParams {
                id: norte_proto::wire::RequestId::Num(id),
            },
        )
        .expect("rpc.cancel notify");

    // The agent's connection is still alive and serving: proof of the benign
    // no-op.
    let st: FsStatResult = agent
        .call(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("mem:///proj/src.txt"),
                attrs: Vec::new(),
            },
        )
        .await
        .expect("the connection is still alive after the rpc.cancel of a resolved request");
    assert_eq!(st.entry.size, Some(4));
}

/// #72 (backpressure/anti-DoS, security-reviewer MAJOR): while an fs.copy is
/// SUSPENDED in an Ask, the agent pipelines several more requests. The inner
/// loop buffers them (`pending_frames`) WITHOUT losing them; when the Ask is
/// withdrawn (rpc.cancel), ALL of them are processed after the outcome, in
/// the same arrival order (serial dispatch). Proves the deferred buffer
/// drains FIFO and that no request is left orphaned.
#[tokio::test]
async fn frames_pipelined_during_an_ask_are_processed_after_the_outcome() {
    let d = spawn_daemon_ask(Duration::from_secs(30)).await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir proj");
    write_file(&d.mem, "mem:///proj/src.txt", b"hola").await;
    let agent = Arc::new(connected_agent(&d, "s1").await);
    let mut human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await;

    // Launches the copy and captures its id; it suspends in the Ask.
    let id_slot = Arc::new(std::sync::Mutex::new(None::<u64>));
    let agent_copy = Arc::clone(&agent);
    let slot = Arc::clone(&id_slot);
    let copy = tokio::spawn(async move {
        agent_copy
            .call_tracked::<_, FsTaskResult>(
                methods::FS_COPY,
                &copy_params("mem:///proj/src.txt", "mem:///proj/dst.txt"),
                move |id| *slot.lock().expect("id lock") = Some(id),
            )
            .await
    });
    let _notif = next_approval(&mut human).await;
    let copy_id = wait_for_id(&id_slot).await;

    // With the copy suspended, the agent pipelines 5 fs.stat calls: the
    // daemon reads them off the socket and defers them (does not dispatch
    // them until the Ask resolves).
    let mut pipelined = Vec::new();
    for _ in 0..5u32 {
        let a = Arc::clone(&agent);
        pipelined.push(tokio::spawn(async move {
            a.call::<_, FsStatResult>(
                methods::FS_STAT,
                &FsStatParams {
                    path: vp("mem:///proj/src.txt"),
                    attrs: Vec::new(),
                },
            )
            .await
        }));
    }
    // Lets the 5 frames reach the daemon (they get buffered behind the copy).
    //
    // THIS `sleep` stays and there is no way to sharpen it: what is being
    // waited for is that the daemon has READ and DEFERRED them, and
    // deferring is exactly not answering anything — there is no observable
    // to poll. It holds up the test's MEANING, not its correctness: without
    // it, a frame that had not arrived before the cancel would be dispatched
    // through the normal path and the test would pass without having
    // exercised the deferral. Removing it does not turn it red; it empties
    // it.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // The agent withdraws the copy → the dispatch is freed; the 5 deferred
    // stats are processed right after.
    agent
        .notify(
            methods::RPC_CANCEL,
            &methods::RpcCancelParams {
                id: norte_proto::wire::RequestId::Num(copy_id),
            },
        )
        .expect("rpc.cancel notify");

    assert!(matches!(
        copy.await.expect("copy join"),
        Err(ClientError::Rpc(rpc)) if rpc.data == Some(Error::Cancelled)
    ));
    // None of the 5 deferred ones was lost: all answer Ok.
    for (i, h) in pipelined.into_iter().enumerate() {
        let st = h
            .await
            .expect("stat join")
            .unwrap_or_else(|e| panic!("deferred stat {i} should have answered Ok: {e:?}"));
        assert_eq!(st.entry.size, Some(4));
    }
}

/// Agents' read gate: with no scope, `fs.search` is `PolicyDenied`
/// out-of-scope; with a granted scope that covers the root, it proceeds.
#[tokio::test]
async fn a_scopeless_agent_does_not_search() {
    let d = spawn_daemon_policy().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/a.rs", b"x").await;
    let mut agent = connected_agent(&d, "s1").await;

    // 1) No scope: denied by the read gate.
    let err = agent
        .call::<_, FsTaskResult>(methods::FS_SEARCH, &search_by_name("mem:///proj", "*.rs"))
        .await
        .expect_err("without scope it does not search");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
            "PolicyDenied out-of-scope, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }

    // 2) Un humano concede scope sobre mem:///proj (round-trip request/grant).
    let req: RequestScopeResult = agent
        .call(
            methods::POLICY_REQUEST_SCOPE,
            &RequestScopeParams {
                session: "s1".into(),
                roots: vec![vp("mem:///proj")],
                ops: vec!["copy".into()],
                ttl_ms: 60_000,
            },
        )
        .await
        .expect("request_scope");
    let human = connected_client(&d).await;
    let _grant: GrantScopeResult = human
        .call(
            methods::POLICY_GRANT_SCOPE,
            &GrantScopeParams {
                request_id: req.request_id,
            },
        )
        .await
        .expect("grant_scope");

    // 3) Now the search under the scope proceeds.
    let task: FsTaskResult = agent
        .call(methods::FS_SEARCH, &search_by_name("mem:///proj", "*.rs"))
        .await
        .expect("with scope, it searches");
    let (hits, state) = drain_search(&mut agent, task.task_id.get()).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(hits.len(), 1);
}

/// A scope that covers `mem:///a` does NOT enable searching in `mem:///b`.
#[tokio::test]
async fn an_agents_scope_does_not_cover_the_root() {
    let d = spawn_daemon_policy().await;
    d.mem.mkdir(&vp("mem:///a")).await.expect("mkdir a");
    d.mem.mkdir(&vp("mem:///b")).await.expect("mkdir b");
    let agent = connected_agent(&d, "s1").await;

    let req: RequestScopeResult = agent
        .call(
            methods::POLICY_REQUEST_SCOPE,
            &RequestScopeParams {
                session: "s1".into(),
                roots: vec![vp("mem:///a")],
                ops: vec!["copy".into()],
                ttl_ms: 60_000,
            },
        )
        .await
        .expect("request_scope");
    let human = connected_client(&d).await;
    let _grant: GrantScopeResult = human
        .call(
            methods::POLICY_GRANT_SCOPE,
            &GrantScopeParams {
                request_id: req.request_id,
            },
        )
        .await
        .expect("grant_scope");

    // Scope on /a, search on /b → out-of-scope.
    let err = agent
        .call::<_, FsTaskResult>(methods::FS_SEARCH, &search_by_name("mem:///b", "*"))
        .await
        .expect_err("scope /a does not cover /b");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
            "PolicyDenied out-of-scope, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }
}

/// The gate: comparing READS two trees, so an agent needs live scope over
/// BOTH roots. One alone is not enough, and the denial says only the coarse
/// category.
#[tokio::test]
async fn fs_compare_an_agent_needs_scope_on_both_roots() {
    let d = spawn_daemon_policy().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir proj");
    d.mem
        .mkdir(&vp("mem:///proj/sub"))
        .await
        .expect("mkdir sub");
    d.mem.mkdir(&vp("mem:///otro")).await.expect("mkdir otro");
    let agent = connected_agent(&d, "s1").await;
    let human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await; // scope over mem:///proj

    // The right root falls outside the scope → denied.
    let err = agent
        .call::<_, FsTaskResult>(
            methods::FS_COMPARE,
            &compare_params("mem:///proj", "mem:///otro"),
        )
        .await
        .expect_err("the right one is out of scope");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
            "PolicyDenied out-of-scope, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }
    // And the other way around either: the gate looks at BOTH, not the first.
    let err = agent
        .call::<_, FsTaskResult>(
            methods::FS_COMPARE,
            &compare_params("mem:///otro", "mem:///proj"),
        )
        .await
        .expect_err("the left one is out of scope");
    assert!(matches!(err, ClientError::Rpc(_)), "was {err:?}");

    // Both under the scope → proceeds.
    let _: FsTaskResult = agent
        .call(
            methods::FS_COMPARE,
            &compare_params("mem:///proj", "mem:///proj/sub"),
        )
        .await
        .expect("both under the scope");
}

/// The hash rung READS CONTENT, and a scope that only grants `mkdir` covers
/// reading structure but not handling bytes: the cheap comparison passes and
/// the hashed one does not.
#[tokio::test]
async fn fs_compare_the_hash_rung_demands_content_scope() {
    let d = spawn_daemon_policy().await;
    d.mem.mkdir(&vp("mem:///data")).await.expect("mkdir data");
    d.mem.mkdir(&vp("mem:///data/l")).await.expect("mkdir l");
    d.mem.mkdir(&vp("mem:///data/r")).await.expect("mkdir r");
    write_file(&d.mem, "mem:///data/l/a.txt", b"x").await;
    write_file(&d.mem, "mem:///data/r/a.txt", b"x").await;
    let agent = connected_agent(&d, "s1").await;
    let human = connected_client(&d).await;

    // A `mkdir`-ONLY scope over mem:///data: reading yes, content no.
    let req: RequestScopeResult = agent
        .call(
            methods::POLICY_REQUEST_SCOPE,
            &RequestScopeParams {
                session: "s1".into(),
                roots: vec![vp("mem:///data")],
                ops: vec!["mkdir".into()],
                ttl_ms: 60_000,
            },
        )
        .await
        .expect("request_scope");
    let _: GrantScopeResult = human
        .call(
            methods::POLICY_GRANT_SCOPE,
            &GrantScopeParams {
                request_id: req.request_id,
            },
        )
        .await
        .expect("grant_scope");

    // Cheap: passes (the read gate is op-independent).
    let _: FsTaskResult = agent
        .call(
            methods::FS_COMPARE,
            &compare_params("mem:///data/l", "mem:///data/r"),
        )
        .await
        .expect("with no hash it proceeds");

    // With hash: denied — reading structure is not reading bytes.
    let mut p = compare_params("mem:///data/l", "mem:///data/r");
    p.criteria.hash = true;
    let err = agent
        .call::<_, FsTaskResult>(methods::FS_COMPARE, &p)
        .await
        .expect_err("the hash demands content scope");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
            "PolicyDenied out-of-scope, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }
}

/// And the side that ALLOWS, which is the one that can really break silently:
/// with a `copy` scope over the root, the TWO doors (read + content) compose
/// and the hashed comparison proceeds to completion. Without this test, a
/// `content_gate` that always denied would pass the one above.
#[tokio::test]
async fn fs_compare_with_a_copy_scope_the_hash_proceeds() {
    let d = spawn_daemon_policy().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir proj");
    d.mem.mkdir(&vp("mem:///proj/l")).await.expect("mkdir l");
    d.mem.mkdir(&vp("mem:///proj/r")).await.expect("mkdir r");
    write_file(&d.mem, "mem:///proj/l/a.txt", b"x").await;
    write_file(&d.mem, "mem:///proj/r/a.txt", b"x").await;
    let mut agent = connected_agent(&d, "s1").await;
    let human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await; // `copy` scope over /proj

    let mut p = compare_params("mem:///proj/l", "mem:///proj/r");
    p.criteria.hash = true;
    let task: FsTaskResult = agent
        .call(methods::FS_COMPARE, &p)
        .await
        .expect("with a copy scope the hash proceeds");
    let (batches, state) = drain_compare(&mut agent, task.task_id.get()).await;
    assert_eq!(state, TaskState::Completed);
    let rows: Vec<_> = batches.into_iter().flat_map(|b| b.rows).collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].verdict, methods::CompareVerdict::Same);
    assert_eq!(rows[0].criterion, methods::CompareCriterion::Hash);
}

/// The gate: planning READS two trees, so an agent needs live scope over
/// BOTH roots — and the gate runs BEFORE validating params, so bad params
/// tell it nothing either.
#[tokio::test]
async fn sync_plan_an_agent_needs_scope_on_both_roots() {
    let d = spawn_daemon_policy().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir proj");
    d.mem
        .mkdir(&vp("mem:///proj/sub"))
        .await
        .expect("mkdir sub");
    d.mem.mkdir(&vp("mem:///proj/l")).await.expect("mkdir l");
    d.mem.mkdir(&vp("mem:///proj/r")).await.expect("mkdir r");
    d.mem.mkdir(&vp("mem:///otro")).await.expect("mkdir otro");
    let agent = connected_agent(&d, "s1").await;
    let human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await; // scope over mem:///proj

    for p in [
        sync_params("mem:///proj", "mem:///otro"),
        sync_params("mem:///otro", "mem:///proj"),
    ] {
        let err = agent
            .call::<_, FsTaskResult>(methods::SYNC_PLAN, &p)
            .await
            .expect_err("one of the two is out of scope");
        match err {
            ClientError::Rpc(rpc) => assert!(
                matches!(rpc.data, Some(Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
                "PolicyDenied out-of-scope, was {:?}",
                rpc.data
            ),
            other => panic!("expected Rpc, got {other:?}"),
        }
    }

    // The gate goes FIRST: impossible params still answer denied, not
    // "and on top of that your request was wrong".
    let mut p = sync_params("mem:///otro", "mem:///proj");
    p.compare.follow_symlinks = true;
    let err = agent
        .call::<_, FsTaskResult>(methods::SYNC_PLAN, &p)
        .await
        .expect_err("out of scope");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PolicyDenied { .. })),
            "the gate goes before params validation, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }

    // With both under the scope, it proceeds.
    let _: FsTaskResult = agent
        .call(
            methods::SYNC_PLAN,
            &sync_params("mem:///proj/l", "mem:///proj/r"),
        )
        .await
        .expect("both under the scope");
}

/// The hash rung READS CONTENT here too: a scope that only grants `mkdir`
/// plans cheaply and unhashed.
#[tokio::test]
async fn sync_plan_the_hash_rung_demands_content_scope() {
    let d = spawn_daemon_policy().await;
    d.mem.mkdir(&vp("mem:///data")).await.expect("mkdir data");
    d.mem.mkdir(&vp("mem:///data/l")).await.expect("mkdir l");
    d.mem.mkdir(&vp("mem:///data/r")).await.expect("mkdir r");
    write_file(&d.mem, "mem:///data/l/a.txt", b"x").await;
    write_file(&d.mem, "mem:///data/r/a.txt", b"x").await;
    let agent = connected_agent(&d, "s1").await;
    let human = connected_client(&d).await;

    let req: RequestScopeResult = agent
        .call(
            methods::POLICY_REQUEST_SCOPE,
            &RequestScopeParams {
                session: "s1".into(),
                roots: vec![vp("mem:///data")],
                ops: vec!["mkdir".into()],
                ttl_ms: 60_000,
            },
        )
        .await
        .expect("request_scope");
    let _: GrantScopeResult = human
        .call(
            methods::POLICY_GRANT_SCOPE,
            &GrantScopeParams {
                request_id: req.request_id,
            },
        )
        .await
        .expect("grant_scope");

    let _: FsTaskResult = agent
        .call(
            methods::SYNC_PLAN,
            &sync_params("mem:///data/l", "mem:///data/r"),
        )
        .await
        .expect("sin hash procede");

    let mut p = sync_params("mem:///data/l", "mem:///data/r");
    p.compare.criteria.hash = true;
    let err = agent
        .call::<_, FsTaskResult>(methods::SYNC_PLAN, &p)
        .await
        .expect_err("the hash demands content scope");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
            "PolicyDenied out-of-scope, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }
}

/// Asserts a `-32602` (params the server refuses without creating a Task).
pub(super) fn assert_invalid_params<T: std::fmt::Debug>(r: Result<T, ClientError>) {
    match r {
        Err(ClientError::Rpc(rpc)) => assert_eq!(rpc.code, codes::INVALID_PARAMS, "{rpc:?}"),
        other => panic!("expected INVALID_PARAMS, got {other:?}"),
    }
}

/// Asserts that a read gives `PolicyDenied` out-of-scope (#80 helper).
pub(super) async fn assert_read_denied(
    agent: &Client,
    method: &str,
    params: &impl serde::Serialize,
) {
    let err = agent
        .call::<_, serde_json::Value>(method, params)
        .await
        .expect_err("with no scope it does not read");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
            "{method}: PolicyDenied out-of-scope, was {:?}",
            rpc.data
        ),
        other => panic!("{method}: expected Rpc, got {other:?}"),
    }
}

/// #80: READS (`list`/`read`/`stat`/`capabilities`) gate by scope for agents,
/// just like mutations. With no scope, `PolicyDenied`; with a scope that
/// covers the root (op-independent: a `copy` grant is enough), OK.
#[tokio::test]
async fn a_scopeless_agent_does_not_read_with_scope_it_does() {
    let d = spawn_daemon_policy().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/a.txt", b"hola").await;
    let agent = connected_agent(&d, "s1").await;

    // 1) With no scope: all four reads denied.
    assert_read_denied(
        &agent,
        methods::FS_LIST,
        &FsListParams {
            path: vp("mem:///proj"),
            limit: None,
            cursor: None,
            attrs: Vec::new(),
        },
    )
    .await;
    assert_read_denied(
        &agent,
        methods::FS_STAT,
        &FsStatParams {
            path: vp("mem:///proj/a.txt"),
            attrs: Vec::new(),
        },
    )
    .await;
    assert_read_denied(
        &agent,
        methods::FS_READ,
        &methods::FsReadParams {
            path: vp("mem:///proj/a.txt"),
            range: None,
        },
    )
    .await;
    assert_read_denied(
        &agent,
        methods::FS_CAPABILITIES,
        &methods::FsCapabilitiesParams {
            path: vp("mem:///proj"),
        },
    )
    .await;

    // 2) A human grants scope over mem:///proj (op copy — covers reads).
    let human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await;

    // 3) Now the four reads proceed.
    let list: FsListResult = agent
        .call(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///proj"),
                limit: None,
                cursor: None,
                attrs: Vec::new(),
            },
        )
        .await
        .expect("with scope, it lists");
    assert_eq!(list.entries.len(), 1);
    let stat: FsStatResult = agent
        .call(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("mem:///proj/a.txt"),
                attrs: Vec::new(),
            },
        )
        .await
        .expect("with scope, it stats");
    assert_eq!(stat.entry.size, Some(4));
    let read: methods::FsReadResult = agent
        .call(
            methods::FS_READ,
            &methods::FsReadParams {
                path: vp("mem:///proj/a.txt"),
                range: None,
            },
        )
        .await
        .expect("with scope, it reads");
    assert!(!read.content_b64.is_empty(), "read something with scope");
    let _caps: methods::FsCapabilitiesResult = agent
        .call(
            methods::FS_CAPABILITIES,
            &methods::FsCapabilitiesParams {
                path: vp("mem:///proj"),
            },
        )
        .await
        .expect("with scope, capabilities");
}

/// #80 (CRITICAL bypass closed): `plugin.preview` READS the file with the
/// daemon's authority — without a gate it would be the side door to
/// `fs.read`. A scopeless agent does not preview; with scope, it proceeds
/// (no matching previewer = `None`, not an error, but it PASSES the gate).
#[tokio::test]
async fn a_scopeless_agent_does_not_preview() {
    let d = spawn_daemon_policy().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/nota.txt", b"secreto").await;
    let agent = connected_agent(&d, "s1").await;

    assert_read_denied(
        &agent,
        methods::PLUGIN_PREVIEW,
        &methods::PluginPreviewParams {
            path: vp("mem:///proj/nota.txt"),
        },
    )
    .await;

    let human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await;
    // With scope: passes the gate (no previewer installed → preview None, not
    // an error).
    let res: methods::PluginPreviewResult = agent
        .call(
            methods::PLUGIN_PREVIEW,
            &methods::PluginPreviewParams {
                path: vp("mem:///proj/nota.txt"),
            },
        )
        .await
        .expect("with scope, the gate lets it through");
    assert!(res.preview.is_none());
}

/// #80: a HUMAN (User) reads with no scope — not sandboxed, symmetric with
/// mutations (User = allow-all).
#[tokio::test]
async fn a_human_reads_with_no_scope() {
    let d = spawn_daemon_policy().await;
    d.mem.mkdir(&vp("mem:///x")).await.expect("mkdir");
    write_file(&d.mem, "mem:///x/f.txt", b"hi").await;
    let human = connected_client(&d).await;

    let list: FsListResult = human
        .call(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///x"),
                limit: None,
                cursor: None,
                attrs: Vec::new(),
            },
        )
        .await
        .expect("a human lists with no scope");
    assert_eq!(list.entries.len(), 1);
}

// ---------- #44: connection.degraded (humans only) ----------

/// A trivial remote provider: answers ANY path with a directory. It is the
/// stand-in for the session the fake connector establishes (same criterion
/// as `connect.rs`'s `EcoProvider`); its only job is for the connect to
/// SUCCEED — the `fs.stat` result does not matter, that the notice was
/// broadcast before the response does.
pub(super) struct EcoProvider;

#[async_trait]
impl Provider for EcoProvider {
    fn scheme(&self) -> &'static str {
        "ftp"
    }
    fn capabilities(&self) -> norte_proto::Capabilities {
        norte_proto::Capabilities {
            flags: norte_proto::CapabilityFlags::empty(),
            max_path: None,
        }
    }
    async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        Ok(Entry {
            attrs: std::collections::BTreeMap::new(),
            path: p.clone(),
            kind: EntryKind::Dir,
            size: None,
            mtime_ms: None,
        })
    }
    async fn list(&self, _p: &VPath) -> Result<norte_vfs::EntryStream, Error> {
        Err(Error::Unsupported)
    }
    async fn read(
        &self,
        _p: &VPath,
        _range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, Error> {
        Err(Error::Unsupported)
    }
    async fn write(&self, _p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
        Err(Error::Unsupported)
    }
    async fn mkdir(&self, _p: &VPath) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
    async fn remove(&self, _p: &VPath) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
    async fn rename(&self, _from: &VPath, _to: &VPath) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
}

/// A fake connector that ALWAYS degrades: every connect returns a live
/// provider ([`EcoProvider`]) plus a `TlsAuthRejected` warning for
/// `backup.example`.
pub(super) struct DegradingConnector {
    mem: Arc<MemProvider>,
}

#[async_trait]
impl norte_core::connect::RemoteConnector for DegradingConnector {
    async fn connect(
        &self,
        scheme: &str,
        _authority: &str,
    ) -> Result<norte_core::connect::Connected, norte_core::connect::DialError> {
        // The live provider answers `stat`; `mem` stays as a witness that the
        // connector can hold its own if it ever needed to.
        let _ = &self.mem;
        Ok(norte_core::connect::Connected {
            provider: Arc::new(EcoProvider) as Arc<dyn Provider>,
            warnings: vec![norte_core::connect::ConnectionWarning {
                scheme: scheme.to_owned(),
                host: "backup.example".to_owned(),
                reason: norte_core::connect::ConnectionWarningReason::TlsAuthRejected,
            }],
        })
    }
    async fn trust_host_key(
        &self,
        _h: &str,
        _p: Option<u16>,
        _f: &str,
    ) -> Result<(), norte_proto::Error> {
        Ok(())
    }
    async fn provide_secret(&self, _c: &str, _s: &str) -> Result<(), norte_proto::Error> {
        Ok(())
    }
}

/// A daemon whose engine has a [`DegradingConnector`] injected: any access to
/// `ftp://backup.example/…` establishes a degraded session.
pub(super) async fn spawn_daemon_degrading() -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let engine = Arc::new(Engine::new());
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    engine.set_connector(Arc::new(DegradingConnector {
        mem: Arc::clone(&mem),
    }));
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
    .expect("bind");
    let run = tokio::spawn(daemon.run());
    TestDaemon {
        socket,
        run,
        dir,
        mem,
    }
}

/// Next `connection.degraded` in the stream (ignores other notifs), with a cap.
pub(super) async fn next_degraded(c: &mut Client) -> ConnectionDegraded {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let n = c.notification().await.expect("notif channel alive");
            if n.method == methods::CONNECTION_DEGRADED {
                return serde_json::from_value::<ConnectionDegraded>(
                    n.params.expect("the notif carries params"),
                )
                .expect("ConnectionDegraded shape");
            }
        }
    })
    .await
    .expect("connection.degraded arrives")
}

/// #44: when a remote session degrades, the human receives
/// `connection.degraded` (scheme/host/reason from the closed vocabulary); an
/// agent connection does NOT — it is security info for the user, not for the
/// agent (same criterion as `policy.*`).
#[tokio::test]
async fn connection_degradation_only_to_humans() {
    let d = spawn_daemon_degrading().await;
    let mut human = connected_client(&d).await;
    let mut agent = connected_agent(&d, "s1").await;

    // The human triggers the lazy connect to the degraded session. The notice
    // is broadcast SYNCHRONOUSLY inside the dispatch, BEFORE writing this
    // `fs.stat`'s response: by the time `call` returns, the broadcast has
    // already happened (same argument as
    // `a_humans_task_progress_does_not_reach_agent_connections`).
    let _stat: FsStatResult = human
        .call(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("ftp://backup.example/"),
                attrs: Vec::new(),
            },
        )
        .await
        .expect("fs.stat triggers the degraded connect");

    let deg = next_degraded(&mut human).await;
    assert_eq!(deg.scheme, "ftp");
    assert_eq!(deg.host, "backup.example");
    assert_eq!(deg.reason, "tls-auth-rejected");
    assert_eq!(deg.detail, None);

    // The agent does NOT receive it. The broadcast was synchronous and prior
    // to the response already received: no deferred path is left to deliver
    // it late → a short cap with no frame is robust (not flaky).
    let leaked = tokio::time::timeout(Duration::from_millis(200), agent.notification()).await;
    assert!(
        leaked.is_err(),
        "an agent does not receive connection.degraded: {leaked:?}"
    );
}

/// A connector that ALWAYS fails with an accountable cause (#322), and with
/// userinfo in the authority to prove it does not leak.
pub(super) struct FailingConnector;

#[async_trait]
impl norte_core::connect::RemoteConnector for FailingConnector {
    async fn connect(
        &self,
        _scheme: &str,
        _authority: &str,
    ) -> Result<norte_core::connect::Connected, norte_core::connect::DialError> {
        Err(norte_core::connect::DialError {
            error: Error::PermissionDenied,
            causa: Some(Box::new(norte_core::connect::Causa {
                conn: Some("rosetta".into()),
                reason: norte_core::connect::ConnectionFailureReason::SecretEmpty,
                detail: Some("el secreto de «rosetta» está definido pero VACÍO".into()),
            })),
        })
    }
    async fn trust_host_key(
        &self,
        _h: &str,
        _p: Option<u16>,
        _f: &str,
    ) -> Result<(), norte_proto::Error> {
        Ok(())
    }
    async fn provide_secret(&self, _c: &str, _s: &str) -> Result<(), norte_proto::Error> {
        Ok(())
    }
}

/// A daemon whose engine cannot connect to anything remote.
pub(super) async fn spawn_daemon_failing() -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let engine = Arc::new(Engine::new());
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    engine.set_connector(Arc::new(FailingConnector));
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
    .expect("bind");
    let run = tokio::spawn(daemon.run());
    TestDaemon {
        socket,
        run,
        dir,
        mem,
    }
}

/// #322: the human receives `connection.failed` with the reason; the agent
/// does NOT.
///
/// And it crosses a real socket, which is the half no unit test covers: the
/// frame is encoded, broadcast and the SDK on the other side decodes it. A
/// typo in the method comparison would be invisible without this.
#[tokio::test]
async fn connection_failure_only_to_humans_and_with_no_userinfo() {
    let d = spawn_daemon_failing().await;
    let mut human = connected_client(&d).await;
    let mut agent = connected_agent(&d, "s1").await;

    // With a USER in the authority: what goes before the `@` must not leak.
    let err = human
        .call::<_, FsStatResult>(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("sftp://alice@maquina.example/x"),
                attrs: Vec::new(),
            },
        )
        .await
        .expect_err("the connect fails");
    let _ = err;

    let failure = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let n = human.notification().await.expect("notif channel alive");
            if n.method == methods::CONNECTION_FAILED {
                return serde_json::from_value::<norte_proto::methods::ConnectionFailed>(
                    n.params.expect("the notif carries params"),
                )
                .expect("ConnectionFailed shape");
            }
        }
    })
    .await
    .expect("connection.failed arrives");

    assert_eq!(failure.scheme, "sftp");
    assert_eq!(
        failure.host, "maquina.example",
        "the userinfo does NOT leak (rule 10)"
    );
    assert!(!failure.host.contains('@'), "not a trace of alice");
    assert_eq!(failure.reason, "secret-empty");
    assert_eq!(failure.conn.as_deref(), Some("rosetta"));
    assert!(failure.detail.is_some());

    // The agent does not receive it: it is a sentence to read, and an agent
    // decides by category — which already reaches it in its operation's
    // error.
    let leaked = tokio::time::timeout(Duration::from_millis(200), agent.notification()).await;
    assert!(
        leaked.is_err(),
        "an agent does not receive connection.failed: {leaked:?}"
    );
}

/// **`ai.rename_plan` answers an agent the SAME no matter what** (#122): AI is
/// human-only, and the gate goes BEFORE parsing.
///
/// It used to answer `out-of-scope` to whoever was outside and `not-approved`
/// to whoever was inside, and checked the instruction's size and the params'
/// validity before anything else. So a FORBIDDEN method answered different
/// things depending on what the agent sent: that is an oracle about the
/// human's tree — "does this directory exist?", "does my scope cover it?" —
/// served through a door that is supposed to be closed.
///
/// Both agents are asserted together on purpose: what has to hold is not a
/// specific category, it is that **the two answers are the same**.
#[tokio::test]
async fn ai_rename_plan_tells_every_agent_the_same_thing() {
    let d = spawn_daemon_policy().await;
    let no_scope = connected_agent(&d, "s1").await;
    let err_without = no_scope
        .call::<_, methods::AiRenamePlanResult>(
            methods::AI_RENAME_PLAN,
            &methods::AiRenamePlanParams {
                dir: vp("mem:///"),
                instruction: "x".into(),
                names: Vec::new(),
            },
        )
        .await
        .expect_err("agent denied");

    // The same agent, now WITH a live read scope over another directory.
    let human = connected_client(&d).await;
    grant_copy_scope(&no_scope, &human, "s1").await;
    let err_with = no_scope
        .call::<_, methods::AiRenamePlanResult>(
            methods::AI_RENAME_PLAN,
            &methods::AiRenamePlanParams {
                dir: vp("mem:///proj"),
                instruction: "x".into(),
                names: Vec::new(),
            },
        )
        .await
        .expect_err("agent denied the same");

    for (who, err) in [("no scope", err_without), ("with scope", err_with)] {
        match err {
            ClientError::Rpc(rpc) => assert!(
                matches!(rpc.data, Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved"),
                "[{who}] had to be `not-approved` and was {:?}",
                rpc.data
            ),
            other => panic!("[{who}] expected Rpc, got {other:?}"),
        }
    }
}

/// MINOR-1 (security review M4-IA): AI is human-ONLY — an agent WITH a live
/// read scope passes `read_gate` but is denied just the same
/// (`not-approved`, closed vocabulary): it does not burn the provider's quota
/// nor push basenames + instruction off the machine with no trace (the read
/// path does not journal).
#[tokio::test]
async fn an_agent_with_scope_still_cannot_ai_rename_plan() {
    let d = spawn_daemon_policy().await;
    let agent = connected_agent(&d, "s1").await;
    let human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await;
    let err = agent
        .call::<_, methods::AiRenamePlanResult>(
            methods::AI_RENAME_PLAN,
            &methods::AiRenamePlanParams {
                dir: vp("mem:///proj"),
                instruction: "x".into(),
                names: Vec::new(),
            },
        )
        .await
        .expect_err("agent with scope: AI is still forbidden");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved"),
            "PolicyDenied not-approved, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }
}

// ---------- index.embed / index.search_semantic (M4-IA-2) ----------

/// A daemon with an in-memory index + a fake embeddings provider (M4-IA-2):
/// `[ai]` enabled with a `fake` provider declared as `embed_provider`.
/// `delay` delays every `embed` to leave the request IN FLIGHT (rpc.cancel).
pub(super) async fn spawn_daemon_embed(delay: Option<Duration>) -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let index = norte_core::Index::open_memory()
        .await
        .expect("in-memory index");
    let engine = Arc::new(Engine::new().with_index(Arc::new(index)));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let mut fake = norte_ai::fake::FakeEmbed::new(8);
    if let Some(d) = delay {
        fake = fake.with_delay(d);
    }
    engine.set_ai_embed_provider(Arc::new(fake));
    engine.set_ai_config(norte_core::ai::AiConfig {
        enabled: true,
        embed_provider: Some("fake".into()),
        providers: vec![norte_core::ai::AiProviderConfig {
            name: "fake".into(),
            kind: "ollama".into(),
            model: "fake-model".into(),
            base_url: None,
        }],
        ..Default::default()
    });
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
    .expect("bind");
    let run = tokio::spawn(daemon.run());
    TestDaemon {
        socket,
        run,
        dir,
        mem,
    }
}

/// M4-IA-2 security: embeddings are human-ONLY — an agent connection sees
/// `PolicyDenied not-approved` on `index.embed` AND on
/// `index.search_semantic` BEFORE any read gate or engine (the content
/// prefixes / the query would leave the process, the same criterion as
/// `ai.rename_plan`).
#[tokio::test]
async fn an_agent_cannot_embed_nor_semantic() {
    let d = spawn_daemon_embed(None).await;
    let agent = connected_agent(&d, "s1").await;
    let assert_not_approved = |err: ClientError| match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved"),
            "PolicyDenied not-approved, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    };
    let err = agent
        .call::<_, FsTaskResult>(
            methods::INDEX_EMBED,
            &methods::IndexEmbedParams {
                root: vp("mem:///"),
            },
        )
        .await
        .expect_err("agent: index.embed forbidden");
    assert_not_approved(err);
    let err = agent
        .call::<_, methods::IndexSearchSemanticResult>(
            methods::INDEX_SEARCH_SEMANTIC,
            &methods::IndexSearchSemanticParams {
                root: None,
                query: "x".into(),
                k: 5,
            },
        )
        .await
        .expect_err("agent: index.search_semantic forbidden");
    assert_not_approved(err);
}

/// The veto to an agent precedes the PARSING of params (security audit
/// M4-IA-2): with MALFORMED params the response is still
/// `PolicyDenied not-approved`, never `INVALID_PARAMS`. So the agent cannot
/// distinguish "bad schema" from "forbidden" — nothing it sends changes what
/// it sees.
#[tokio::test]
async fn an_agent_with_malformed_params_sees_policy_denied_not_invalid_params() {
    let d = spawn_daemon_embed(None).await;
    let agent = connected_agent(&d, "s1").await;
    let assert_not_approved = |err: ClientError| match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved"),
            "PolicyDenied not-approved (never INVALID_PARAMS), was {rpc:?}"
        ),
        other => panic!("expected Rpc, got {other:?}"),
    };
    let err = agent
        .call::<_, serde_json::Value>(methods::INDEX_EMBED, &serde_json::json!({ "root": 42 }))
        .await
        .expect_err("agent: index.embed forbidden despite bad params");
    assert_not_approved(err);
    let err = agent
        .call::<_, serde_json::Value>(
            methods::INDEX_SEARCH_SEMANTIC,
            &serde_json::json!({ "query": [] }),
        )
        .await
        .expect_err("agent: index.search_semantic forbidden despite bad params");
    assert_not_approved(err);
}

/// #80: `fs.rename_batch_plan` is a directory READ in disguise — its
/// verdicts say which names exist —, so it goes through the same `read_gate`
/// as `fs.list`. A scopeless agent gets `PolicyDenied`, never a plan, and
/// never the difference between "that file is there" and "it is not".
#[tokio::test]
async fn a_scopeless_agent_cannot_rename_batch_plan() {
    let d = spawn_daemon_policy().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/secret.txt", b"x").await;
    let agent = connected_agent(&d, "s1").await;

    let err = agent
        .call::<_, methods::FsRenameBatchPlanResult>(
            methods::FS_RENAME_BATCH_PLAN,
            &methods::FsRenameBatchPlanParams {
                dir: vp("mem:///proj"),
                pairs: vec![pair(b"secret.txt", b"other.txt")],
            },
        )
        .await
        .expect_err("scopeless agent denied");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
            "PolicyDenied out-of-scope, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }

    // And the verdict is the SAME for a name that does not exist: the error
    // does not distinguish what is inside the directory from what is not.
    let err = agent
        .call::<_, methods::FsRenameBatchPlanResult>(
            methods::FS_RENAME_BATCH_PLAN,
            &methods::FsRenameBatchPlanParams {
                dir: vp("mem:///proj"),
                pairs: vec![pair(b"does-not-exist.txt", b"other.txt")],
            },
        )
        .await
        .expect_err("scopeless agent denied");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
            "PolicyDenied out-of-scope, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }
}

/// #80 on the twin that MUTATES, which is where the oracle was worse:
/// executing starts by planning, and `plan_hash` is deterministic and
/// computable offline — without this gate, a scopeless agent would send the
/// hash of the hypothesis "X exists" and would distinguish the denial (it
/// existed) from `PlanStale` (it did not), an exact bit per request. With it,
/// all FOUR combinations (directory present / absent, hash matching / not)
/// answer the same thing.
#[tokio::test]
async fn a_scopeless_agent_cannot_rename_batch_nor_use_it_as_an_oracle() {
    let d = spawn_daemon_policy().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/secret.txt", b"x").await;
    let agent = connected_agent(&d, "s1").await;

    let zeros = methods::PlanHash::parse(&"0".repeat(64)).expect("hash");
    let ones = methods::PlanHash::parse(&"1".repeat(64)).expect("hash");
    let cases = [
        // (directory, source name): exists/does not exist, in the two
        // combinations the oracle would use to tell the worlds apart.
        (vp("mem:///proj"), b"secret.txt".to_vec()),
        (vp("mem:///proj"), b"does-not-exist.txt".to_vec()),
        (vp("mem:///no-such-dir"), b"secret.txt".to_vec()),
    ];
    let mut responses = Vec::new();
    for (dir, from) in cases {
        for hash in [&zeros, &ones] {
            let err = agent
                .call::<_, FsTaskResult>(
                    methods::FS_RENAME_BATCH,
                    &methods::FsRenameBatchParams {
                        dir: dir.clone(),
                        pairs: vec![pair(&from, b"other.txt")],
                        plan_hash: hash.clone(),
                    },
                )
                .await
                .expect_err("scopeless agent denied");
            match err {
                ClientError::Rpc(rpc) => {
                    assert!(
                        matches!(rpc.data, Some(Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
                        "PolicyDenied out-of-scope, was {:?}",
                        rpc.data
                    );
                    responses.push((rpc.code, rpc.message));
                }
                other => panic!("expected Rpc, got {other:?}"),
            }
        }
    }
    assert!(
        responses.windows(2).all(|w| w[0] == w[1]),
        "all six responses have to be THE SAME: {responses:?}",
    );
    // And nothing was touched.
    assert!(d.mem.stat(&vp("mem:///proj/secret.txt")).await.is_ok());
}

/// An agent can READ both trees (its scope covers them: `covers_read` is root
/// membership, without looking at the op) and can therefore PLAN — but
/// applying writes, and its scope does not carry `copy`. The gate runs over
/// the roots that come out of the plan, on apply, and denies.
///
/// That it can plan but not apply is exactly the split being sought: the
/// plan mutates nothing, applying does.
#[tokio::test]
async fn applying_with_no_write_permission_on_the_destination_is_denied() {
    let d = spawn_daemon_journal().await;
    d.mem.mkdir(&vp("mem:///s")).await.expect("mkdir s");
    d.mem.mkdir(&vp("mem:///d")).await.expect("mkdir d");
    write_file(&d.mem, "mem:///s/a.txt", b"aaa").await;

    let human = connected_client(&d).await;
    let mut agent = connected_agent(&d, "s-sync").await;
    // Scope over the root of both trees, but ONLY for `mkdir`: reading is in
    // (reading is root membership), copying is not.
    let req: RequestScopeResult = agent
        .call(
            methods::POLICY_REQUEST_SCOPE,
            &RequestScopeParams {
                session: "s-sync".into(),
                roots: vec![vp("mem:///")],
                ops: vec!["mkdir".into()],
                ttl_ms: 60_000,
            },
        )
        .await
        .expect("request_scope");
    let _: GrantScopeResult = human
        .call(
            methods::POLICY_GRANT_SCOPE,
            &GrantScopeParams {
                request_id: req.request_id,
            },
        )
        .await
        .expect("grant_scope");

    let task: FsTaskResult = agent
        .call(methods::SYNC_PLAN, &sync_params("mem:///s", "mem:///d"))
        .await
        .expect("planning is reading, and reading it can");
    let (_batches, done, state) = drain_sync(&mut agent, task.task_id.get()).await;
    assert_eq!(state, TaskState::Completed);
    let done = done.expect("the plan closed");

    let err = agent
        .call::<_, FsTaskResult>(
            methods::SYNC_APPLY,
            &methods::SyncApplyParams {
                plan_hash: done.plan_hash,
            },
        )
        .await
        .expect_err("writing is not in its scope");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
            "PolicyDenied out-of-scope, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }
    // And the destination stays empty: the gate runs BEFORE touching a byte.
    assert!(
        d.mem.stat(&vp("mem:///d/a.txt")).await.is_err(),
        "a denied plan writes nothing"
    );
}

/// An agent has no screen to save: `session.*` is `INVALID_REQUEST`, the same
/// criterion as `daemon.shutdown` and `policy.pending`.
#[tokio::test]
async fn session_is_human_only() {
    let d = spawn_daemon(None).await;
    let agent = connected_agent(&d, "a1").await;
    let err = agent
        .call::<_, methods::SessionGetResult>(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect_err("an agent does not read anyone's screen");
    assert_rpc_code(&err, codes::INVALID_REQUEST);
    let err = agent
        .call::<_, methods::SessionPutResult>(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: 0,
                body: serde_json::json!({}),
            },
        )
        .await
        .expect_err("nor does it write it");
    assert_rpc_code(&err, codes::INVALID_REQUEST);
}

/// An agent does not read the daemon's log: it carries paths, connection
/// names and activity of OTHER sessions — an existence oracle outside its
/// scope. And it is told it is forbidden, not that it is empty.
///
/// With the ring deliberately mounted: that way what refuses is the actor
/// gate and not the absence of a log, which would answer something else.
#[tokio::test]
async fn an_agent_does_not_read_the_log() {
    let (d, ring) = spawn_daemon_with_ring().await;
    let _guard = toward_the_ring(&ring);
    let agent = connected_agent(&d, "a1").await;

    let forbidden = |err: ClientError| match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(
                rpc.data,
                Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved"
            ),
            "forbidden, not empty nor malformed: {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    };

    // And whatever happens with the params: the gate runs BEFORE parsing, so
    // an agent cannot distinguish "forbidden" from "bad params" by fuzzing
    // its own request — not even the `max: 0` that would give a human
    // INVALID_PARAMS.
    for params in [
        serde_json::json!({ "cursor": null, "max": 10 }),
        serde_json::json!({ "max": 0 }),
        serde_json::json!({ "something": "that does not exist" }),
        serde_json::Value::Null,
    ] {
        forbidden(
            agent
                .call::<_, methods::LogTailResult>(methods::LOG_TAIL, &params)
                .await
                .expect_err("an agent does not read the log"),
        );
    }
    forbidden(
        agent
            .call::<_, methods::LogLevelResult>(
                methods::LOG_LEVEL,
                &serde_json::json!({ "level": "debug" }),
            )
            .await
            .expect_err("an agent does not raise the verbosity of someone else's job"),
    );
}

/// An agent does not read the timeline NOR undo to a point (phase 7).
///
/// The journal is the complete list of everything touched on the machine,
/// with source and destination: for an agent with a bounded scope it is an
/// existence oracle over everything outside its enclosure, and it also shows
/// what the other sessions did. And `undo_after` reverts the HUMAN's work —
/// an agent that could request it would erase the trace of its own actions.
///
/// The same thing is checked as in the log and for the same reason: that it
/// is FORBIDDEN and not empty, and that the gate runs BEFORE parsing, so an
/// agent cannot distinguish "forbidden" from "bad params" by probing shapes.
#[tokio::test]
async fn an_agent_does_not_read_the_timeline_nor_undo() {
    let d = spawn_daemon(None).await;
    let agent = connected_agent(&d, "a1").await;

    let forbidden = |err: ClientError| match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(
                rpc.data,
                Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved"
            ),
            "forbidden, not empty nor malformed: {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    };

    for params in [
        serde_json::json!({ "limit": 10 }),
        serde_json::json!({ "limit": 0 }),
        serde_json::json!({ "before_seq": 3, "limit": 5, "actor_kind": "user" }),
        serde_json::json!({ "something": "that does not exist" }),
        serde_json::Value::Null,
    ] {
        forbidden(
            agent
                .call::<_, methods::JournalListResult>(methods::JOURNAL_LIST, &params)
                .await
                .expect_err("an agent does not read the journal"),
        );
    }

    for params in [
        serde_json::json!({ "seq": 1 }),
        serde_json::json!({ "seq": -1 }),
        serde_json::Value::Null,
    ] {
        forbidden(
            agent
                .call::<_, methods::PolicyUndoSessionResult>(methods::JOURNAL_UNDO_AFTER, &params)
                .await
                .expect_err("an agent does not undo the human's work"),
        );
    }
}

/// #294 — the SDK RETAINS the version the peer declared during the handshake.
///
/// Without it a client cannot know that the check it just requested was not
/// performed: it sends `expected_digest` (#282), an old daemon ignores it as
/// ADR 0004 dictates, grants without checking, and nothing tells the client.
/// The `InitializeResult` was being discarded, and it is an answer already
/// paid for.
#[tokio::test]
async fn the_sdk_retains_the_peers_version() {
    let d = spawn_daemon_plugins().await;
    let backend = norte_client::RemoteBackend::connect(
        d.socket.clone(),
        None,
        norte_proto::methods::ClientInfo {
            name: "version-peer".into(),
            version: "0.0.0".into(),
        },
    )
    .await
    .expect("connects");

    assert_eq!(
        backend.peer_protocol_version().as_deref(),
        Some(norte_proto::PROTOCOL_VERSION),
        "the handshake version is the one the daemon declares"
    );

    // And with it the anchor IS sent: this daemon understands it.
    let list = backend.plugins_list().await.expect("plugin.list");
    let anchor = list.plugins[0]
        .manifest_digest
        .clone()
        .expect("the catalog carries the anchor");
    backend
        .plugins_set_approval("org.norte.demo", true, Some(&anchor))
        .await
        .expect("a 0.53 peer checks the anchor and grants");
}
