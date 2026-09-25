use super::*;

// ---------- ai.rename_plan (M4-IA, ADR 0031) ----------

/// Fake AI provider (copied from `ai_rename.rs` — test binaries do not share
/// code): returns canned JSON in two deltas. `delay` delays delivery to leave
/// the request IN FLIGHT (rpc.cancel test, #72).
pub(super) struct FakeAi {
    reply: String,
    delay: Option<Duration>,
}

#[async_trait]
impl norte_ai::AiProvider for FakeAi {
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "the trait's signature (&self→&str)"
    )]
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self) -> norte_ai::AiCaps {
        norte_ai::AiCaps::STREAMING
    }
    fn is_local(&self) -> bool {
        true
    }
    async fn chat(
        &self,
        _req: norte_ai::ChatRequest,
    ) -> Result<norte_ai::ChatStream, norte_ai::AiError> {
        use futures::StreamExt as _;
        // SIMULATED provider latency, not a test wait: it is what opens the
        // window in which an `rpc.cancel` arrives with the request in
        // flight. This `sleep` stays, like `norte-ui-host`'s double's
        // `delay_ms`.
        if let Some(d) = self.delay {
            tokio::time::sleep(d).await;
        }
        // Delivered in TWO deltas to exercise draining the stream.
        let (a, b) = self.reply.split_at(self.reply.len() / 2);
        let items = vec![Ok(a.to_owned()), Ok(b.to_owned())];
        Ok(futures::stream::iter(items).boxed())
    }
    async fn list_models(&self) -> Result<Vec<norte_ai::ModelInfo>, norte_ai::AiError> {
        Ok(vec![norte_ai::ModelInfo {
            id: "fake".into(),
            context_window: None,
        }])
    }
}

/// A daemon with a fake AI provider installed and `[ai]` enabled (M4-IA).
pub(super) async fn spawn_daemon_ai(reply: &str) -> TestDaemon {
    spawn_daemon_ai_delay(reply, None).await
}

/// Like [`spawn_daemon_ai`] but the provider DELAYS its response: the
/// request stays in flight until an `rpc.cancel` withdraws it.
pub(super) async fn spawn_daemon_ai_slow(reply: &str, delay: Duration) -> TestDaemon {
    spawn_daemon_ai_delay(reply, Some(delay)).await
}

pub(super) async fn spawn_daemon_ai_delay(reply: &str, delay: Option<Duration>) -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let engine = Arc::new(Engine::new());
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    engine.set_ai_provider(Arc::new(FakeAi {
        reply: reply.to_owned(),
        delay,
    }));
    engine.set_ai_config(norte_core::ai::AiConfig {
        enabled: true,
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

#[tokio::test]
async fn ai_rename_plan_answers_over_the_socket() {
    let d = spawn_daemon_ai(r#"[{"from":"a.txt","to":"informe-a.txt"}]"#).await;
    write_file(&d.mem, "mem:///a.txt", b"x").await;
    let c = connected_client(&d).await;
    let plan: methods::AiRenamePlanResult = c
        .call(
            methods::AI_RENAME_PLAN,
            &methods::AiRenamePlanParams {
                dir: vp("mem:///"),
                instruction: "prefix informe-".into(),
                names: Vec::new(),
            },
        )
        .await
        .expect("ai.rename_plan");
    assert_eq!(plan.entries.len(), 1);
    assert_eq!(plan.entries[0].from, "a.txt");
    assert_eq!(plan.entries[0].to, "informe-a.txt");
}

/// With no provider installed the daemon answers `Unsupported`, not a panic
/// nor an opaque error (same contract as the embedded engine).
#[tokio::test]
async fn ai_rename_plan_with_no_provider_is_unsupported() {
    let d = spawn_daemon(None).await; // no set_ai_provider
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::AiRenamePlanResult>(
            methods::AI_RENAME_PLAN,
            &methods::AiRenamePlanParams {
                dir: vp("mem:///"),
                instruction: "x".into(),
                names: Vec::new(),
            },
        )
        .await
        .expect_err("no provider → error");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::Unsupported)),
            "Unsupported, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }
}

/// And it does not distinguish by the PARAMS either: unreadable ones and an
/// instruction over the cap receive the same denial as valid ones.
/// Otherwise the agent learns where the cap is and what shape the params
/// take without anyone having let it call at all.
#[tokio::test]
async fn ai_rename_plan_does_not_distinguish_bad_params_for_an_agent() {
    let d = spawn_daemon_policy().await;
    let agent = connected_agent(&d, "s1").await;

    let err = agent
        .call::<_, methods::AiRenamePlanResult>(
            methods::AI_RENAME_PLAN,
            &serde_json::json!({ "esto": "no es el params" }),
        )
        .await
        .expect_err("denied");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved"),
            "unreadable params had to give `not-approved`, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }

    let err = agent
        .call::<_, methods::AiRenamePlanResult>(
            methods::AI_RENAME_PLAN,
            &methods::AiRenamePlanParams {
                dir: vp("mem:///"),
                instruction: "x".repeat(64 * 1024),
                names: Vec::new(),
            },
        )
        .await
        .expect_err("denied");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved"),
            "a huge instruction had to give `not-approved`, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }
}

/// MINOR-2 (M4-IA security review): the instruction is capped server-side
/// BEFORE touching the engine or the provider — INPUT tokens are the cost;
/// the 16 MiB frame is not a limit.
#[tokio::test]
async fn oversized_instruction_is_invalid_params() {
    let d = spawn_daemon_ai("[]").await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::AiRenamePlanResult>(
            methods::AI_RENAME_PLAN,
            &methods::AiRenamePlanParams {
                dir: vp("mem:///"),
                instruction: "x".repeat(5 * 1024),
                names: Vec::new(),
            },
        )
        .await
        .expect_err("5 KiB instruction → protocol error");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_PARAMS),
        other => panic!("expected Rpc, got {other:?}"),
    }
}

/// An oversized `names` list is `-32602`, like the instruction and like a
/// path batch (#121): what caps it is a filter that runs for every listing
/// entry, in a DIRECT call that can only die by timeout.
#[tokio::test]
async fn names_above_the_cap_is_invalid_params() {
    let d = spawn_daemon_ai("[]").await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::AiRenamePlanResult>(
            methods::AI_RENAME_PLAN,
            &methods::AiRenamePlanParams {
                dir: vp("mem:///"),
                instruction: "x".into(),
                names: (0..=methods::AI_RENAME_NAMES_MAX)
                    .map(|i| format!("f{i}.txt"))
                    .collect(),
            },
        )
        .await
        .expect_err("above the cap");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_PARAMS),
        other => panic!("expected Rpc, got {other:?}"),
    }
}

/// **The daemon HONORS `names`**: without this test, a handler that swallowed
/// the field would pass the whole suite — which is exactly what happened
/// while this was being written.
#[tokio::test]
async fn the_daemon_asks_for_the_plan_only_over_the_requested_names() {
    let d = spawn_daemon_ai(r#"[{"from":"marcado.txt","to":"nuevo.txt"}]"#).await;
    let c = connected_client(&d).await;
    let plan: methods::AiRenamePlanResult = c
        .call(
            methods::AI_RENAME_PLAN,
            &methods::AiRenamePlanParams {
                dir: vp("mem:///"),
                instruction: "x".into(),
                // A name that is NOT in the daemon's listing: if the field
                // were ignored, the plan would come out of the whole
                // directory and the provider would answer its usual plan.
                names: vec!["not-in-the-listing.txt".into()],
            },
        )
        .await
        .expect("plan");
    assert!(
        plan.entries.is_empty(),
        "over nothing that exists, nothing is asked: {plan:?}"
    );
}

/// #72 over `ai.rename_plan`: the provider call can take a while — an
/// `rpc.cancel` drops the in-flight dispatch (the HTTP stream aborts on
/// drop) and answers `Error::Cancelled` without killing the connection.
#[tokio::test]
async fn rpc_cancel_aborts_ai_rename_plan_in_flight() {
    // FakeAi with a big delay: the request stays IN FLIGHT until the cancel.
    let d = spawn_daemon_ai_slow("[]", Duration::from_secs(30)).await;
    let c = Arc::new(connected_client(&d).await);

    let id_slot = Arc::new(std::sync::Mutex::new(None::<u64>));
    let caller = Arc::clone(&c);
    let slot = Arc::clone(&id_slot);
    let call = tokio::spawn(async move {
        caller
            .call_tracked::<_, methods::AiRenamePlanResult>(
                methods::AI_RENAME_PLAN,
                &methods::AiRenamePlanParams {
                    dir: vp("mem:///"),
                    instruction: "x".into(),
                    names: Vec::new(),
                },
                move |id| *slot.lock().expect("id lock") = Some(id),
            )
            .await
    });

    let id = wait_for_id(&id_slot).await;
    c.notify(
        methods::RPC_CANCEL,
        &methods::RpcCancelParams {
            id: norte_proto::wire::RequestId::Num(id),
        },
    )
    .expect("rpc.cancel notify");

    match call.await.expect("join") {
        Err(ClientError::Rpc(rpc)) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert_eq!(rpc.data, Some(Error::Cancelled));
        }
        other => panic!("expected Cancelled, got {other:?}"),
    }
}

/// Fail-loud IN THE RESPONSE (not in the Task's join): `index.embed` over a
/// root never built with `index.build` is an immediate `NotFound`.
#[tokio::test]
async fn embed_with_no_prior_build_is_not_found() {
    let d = spawn_daemon_embed(None).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, FsTaskResult>(
            methods::INDEX_EMBED,
            &methods::IndexEmbedParams {
                root: vp("mem:///nunca"),
            },
        )
        .await
        .expect_err("no prior build → error");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert_eq!(rpc.data, Some(norte_proto::Error::NotFound));
        }
        other => panic!("expected Rpc, got {other:?}"),
    }
}

/// #72 over `index.search_semantic`: embedding the query can take a while —
/// an `rpc.cancel` drops the in-flight dispatch and answers `Error::Cancelled`
/// without killing the connection (mirror of
/// `rpc_cancel_aborts_ai_rename_plan_in_flight`).
#[tokio::test]
async fn rpc_cancel_aborts_search_semantic_in_flight() {
    // FakeEmbed with a big delay: the request stays IN FLIGHT until the
    // cancel.
    let d = spawn_daemon_embed(Some(Duration::from_secs(30))).await;
    let c = Arc::new(connected_client(&d).await);

    let id_slot = Arc::new(std::sync::Mutex::new(None::<u64>));
    let caller = Arc::clone(&c);
    let slot = Arc::clone(&id_slot);
    let call = tokio::spawn(async move {
        caller
            .call_tracked::<_, methods::IndexSearchSemanticResult>(
                methods::INDEX_SEARCH_SEMANTIC,
                &methods::IndexSearchSemanticParams {
                    root: None,
                    query: "x".into(),
                    k: 5,
                },
                move |id| *slot.lock().expect("id lock") = Some(id),
            )
            .await
    });

    let id = wait_for_id(&id_slot).await;
    c.notify(
        methods::RPC_CANCEL,
        &methods::RpcCancelParams {
            id: norte_proto::wire::RequestId::Num(id),
        },
    )
    .expect("rpc.cancel notify");

    match call.await.expect("join") {
        Err(ClientError::Rpc(rpc)) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert_eq!(rpc.data, Some(Error::Cancelled));
        }
        other => panic!("expected Cancelled, got {other:?}"),
    }
}

/// The query is capped server-side BEFORE touching the engine or the
/// provider (the same 4 KiB belt as `ai.rename_plan`'s instruction): INPUT
/// tokens are the cost; the 16 MiB frame is not a limit.
#[tokio::test]
async fn giant_semantic_query_is_invalid_params() {
    let d = spawn_daemon_embed(None).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::IndexSearchSemanticResult>(
            methods::INDEX_SEARCH_SEMANTIC,
            &methods::IndexSearchSemanticParams {
                root: None,
                query: "x".repeat(5 * 1024),
                k: 5,
            },
        )
        .await
        .expect_err("5 KiB query → protocol error");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_PARAMS),
        other => panic!("expected Rpc, got {other:?}"),
    }
}

/// Asking for an oversized `k` is NOT an error: it is clamped to
/// `INDEX_SEMANTIC_MAX_K` (same pattern as `FS_LIST_MAX_PAGE` — the wire's
/// documented contract).
#[tokio::test]
async fn oversized_semantic_k_is_not_an_error() {
    let d = spawn_daemon_embed(None).await;
    let c = connected_client(&d).await;
    let r: methods::IndexSearchSemanticResult = c
        .call(
            methods::INDEX_SEARCH_SEMANTIC,
            &methods::IndexSearchSemanticParams {
                root: None,
                query: "x".into(),
                k: 100_000,
            },
        )
        .await
        .expect("a giant k gets clamped, never an error");
    assert!(
        r.hits.len() <= usize::try_from(methods::INDEX_SEMANTIC_MAX_K).expect("fits"),
        "the clamp bounds the hits"
    );
}
