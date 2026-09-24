use super::*;

// ---------- ai.rename_plan (M4-IA, ADR 0031) ----------

/// Proveedor de IA falso (copiado de `ai_rename.rs` — los binarios de test no
/// comparten código): devuelve un JSON canned en dos deltas. `delay` retrasa
/// la entrega para dejar la request EN VUELO (test de rpc.cancel, #72).
pub(super) struct FakeAi {
    reply: String,
    delay: Option<Duration>,
}

#[async_trait]
impl norte_ai::AiProvider for FakeAi {
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "firma del trait (&self→&str)"
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
        // Latencia SIMULADA del proveedor, no una espera del test: es lo que
        // abre la ventana en la que un `rpc.cancel` llega con la petición en
        // vuelo. Este `sleep` se queda, como el `retraso_ms` del doble de
        // `norte-ui-host`.
        if let Some(d) = self.delay {
            tokio::time::sleep(d).await;
        }
        // Entrega en DOS deltas para ejercitar el drenado del stream.
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

/// Daemon con proveedor de IA fake instalado y `[ai]` habilitado (M4-IA).
pub(super) async fn spawn_daemon_ai(reply: &str) -> TestDaemon {
    spawn_daemon_ai_delay(reply, None).await
}

/// Como [`spawn_daemon_ai`] pero el proveedor RETRASA su respuesta: la
/// request queda en vuelo hasta que un `rpc.cancel` la retire.
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
async fn ai_rename_plan_responde_por_el_socket() {
    let d = spawn_daemon_ai(r#"[{"from":"a.txt","to":"informe-a.txt"}]"#).await;
    write_file(&d.mem, "mem:///a.txt", b"x").await;
    let c = connected_client(&d).await;
    let plan: methods::AiRenamePlanResult = c
        .call(
            methods::AI_RENAME_PLAN,
            &methods::AiRenamePlanParams {
                dir: vp("mem:///"),
                instruction: "prefija informe-".into(),
                names: Vec::new(),
            },
        )
        .await
        .expect("ai.rename_plan");
    assert_eq!(plan.entries.len(), 1);
    assert_eq!(plan.entries[0].from, "a.txt");
    assert_eq!(plan.entries[0].to, "informe-a.txt");
}

/// Sin proveedor instalado el daemon responde `Unsupported`, no un panic ni
/// un error opaco (mismo contrato que el engine embebido).
#[tokio::test]
async fn ai_rename_plan_sin_proveedor_es_unsupported() {
    let d = spawn_daemon(None).await; // sin set_ai_provider
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
        .expect_err("sin proveedor → error");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::Unsupported)),
            "Unsupported, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// Y tampoco distingue por los PARAMS: unos ilegibles y una instrucción que
/// pasa del tope reciben la misma denegación que unos válidos. Si no, el
/// agente aprende dónde está el tope y qué forma tiene el params sin que nadie
/// le haya dejado llamar.
#[tokio::test]
async fn ai_rename_plan_no_distingue_params_malos_para_un_agente() {
    let d = spawn_daemon_policy().await;
    let agent = connected_agent(&d, "s1").await;

    let err = agent
        .call::<_, methods::AiRenamePlanResult>(
            methods::AI_RENAME_PLAN,
            &serde_json::json!({ "esto": "no es el params" }),
        )
        .await
        .expect_err("denegado");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved"),
            "params ilegibles tenían que dar `not-approved`, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
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
        .expect_err("denegado");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved"),
            "una instrucción enorme tenía que dar `not-approved`, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// MINOR-2 (security review M4-IA): la instrucción se capa server-side ANTES
/// de tocar engine o proveedor — los tokens de ENTRADA son el coste; el frame
/// de 16 MiB no es un límite.
#[tokio::test]
async fn instruccion_desmesurada_es_invalid_params() {
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
        .expect_err("instrucción de 5 KiB → error de protocolo");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_PARAMS),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// Una lista de `names` desmesurada es `-32602`, como la instrucción y como
/// un lote de rutas (#121): lo que acota es un filtro que corre por cada
/// entrada del listado, en una llamada DIRECTA que solo puede morir por
/// timeout.
#[tokio::test]
async fn names_por_encima_del_tope_es_invalid_params() {
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
        .expect_err("por encima del tope");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_PARAMS),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// **El daemon HONRA `names`**: sin este test, un handler que se comiera el
/// campo pasaba la suite entera — que es exactamente lo que pasó mientras se
/// escribía esto.
#[tokio::test]
async fn el_daemon_pide_el_plan_solo_sobre_los_nombres_pedidos() {
    let d = spawn_daemon_ai(r#"[{"from":"marcado.txt","to":"nuevo.txt"}]"#).await;
    let c = connected_client(&d).await;
    let plan: methods::AiRenamePlanResult = c
        .call(
            methods::AI_RENAME_PLAN,
            &methods::AiRenamePlanParams {
                dir: vp("mem:///"),
                instruction: "x".into(),
                // Un nombre que NO está en el listado del daemon: si el campo
                // se ignorara, el plan saldría del directorio entero y el
                // proveedor contestaría su plan de siempre.
                names: vec!["no-esta-en-el-listado.txt".into()],
            },
        )
        .await
        .expect("plan");
    assert!(
        plan.entries.is_empty(),
        "sobre nada que exista no se pregunta: {plan:?}"
    );
}

/// #72 sobre `ai.rename_plan`: la llamada al proveedor puede tardar — un
/// `rpc.cancel` dropea el dispatch en vuelo (el stream HTTP aborta con el
/// drop) y responde `Error::Cancelled` sin matar la conexión.
#[tokio::test]
async fn rpc_cancel_aborta_ai_rename_plan_en_vuelo() {
    // FakeAi con delay grande: la request queda EN VUELO hasta el cancel.
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

    let id = esperar_id(&id_slot).await;
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
        other => panic!("esperaba Cancelled, fue {other:?}"),
    }
}

/// Fail-loud EN LA RESPUESTA (no en el join de la Task): `index.embed` sobre
/// un root jamás construido con `index.build` es `NotFound` inmediato.
#[tokio::test]
async fn embed_sin_build_previo_es_not_found() {
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
        .expect_err("sin build previo → error");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert_eq!(rpc.data, Some(norte_proto::Error::NotFound));
        }
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// #72 sobre `index.search_semantic`: el embed de la query puede tardar — un
/// `rpc.cancel` dropea el dispatch en vuelo y responde `Error::Cancelled`
/// sin matar la conexión (espejo de `rpc_cancel_aborta_ai_rename_plan_en_vuelo`).
#[tokio::test]
async fn rpc_cancel_aborta_search_semantic_en_vuelo() {
    // FakeEmbed con delay grande: la request queda EN VUELO hasta el cancel.
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

    let id = esperar_id(&id_slot).await;
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
        other => panic!("esperaba Cancelled, fue {other:?}"),
    }
}

/// La query se capa server-side ANTES de tocar engine o proveedor (mismo
/// cinturón de 4 KiB que la instrucción de `ai.rename_plan`): los tokens de
/// ENTRADA son el coste; el frame de 16 MiB no es un límite.
#[tokio::test]
async fn semantic_query_gigante_es_invalid_params() {
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
        .expect_err("query de 5 KiB → error de protocolo");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_PARAMS),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// Pedir un `k` desmesurado NO es error: se recorta a `INDEX_SEMANTIC_MAX_K`
/// (mismo patrón que `FS_LIST_MAX_PAGE` — el contrato documentado del wire).
#[tokio::test]
async fn semantic_k_desmesurado_no_es_error() {
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
        .expect("k gigante se recorta, jamás error");
    assert!(
        r.hits.len() <= usize::try_from(methods::INDEX_SEMANTIC_MAX_K).expect("cabe"),
        "el clamp acota los hits"
    );
}
