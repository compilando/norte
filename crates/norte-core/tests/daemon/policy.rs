use super::*;

/// M3-3b Task 2: el actor lo fija la conexión. Un cliente-agente sin scope ve
/// su `fs.copy` denegado por policy (taxonomía `PolicyDenied`, NO
/// `PermissionDenied`) y el FS queda intacto; un cliente humano (sin
/// `agent_session`) copia sin gate.
#[tokio::test]
async fn agente_sin_scope_ve_policy_denied_humano_copia() {
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

    // Agente sin scope: denegado por policy, sin tocar el FS.
    let agent = connected_agent(&d, "s1").await;
    let err = agent
        .call::<_, FsTaskResult>(methods::FS_COPY, &copy("mem:///src.txt", "mem:///a.txt"))
        .await
        .expect_err("agente sin scope: denegado");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert!(
                matches!(rpc.data, Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
                "PolicyDenied out-of-scope, fue {:?}",
                rpc.data
            );
        }
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
    assert!(
        matches!(
            d.mem.stat(&vp("mem:///a.txt")).await,
            Err(norte_proto::Error::NotFound)
        ),
        "el gate PRE-efecto no tocó el destino"
    );

    // Humano (sin agent_session): copia aceptada, la Task se encola.
    let human = connected_client(&d).await;
    let res: FsTaskResult = human
        .call(methods::FS_COPY, &copy("mem:///src.txt", "mem:///h.txt"))
        .await
        .expect("humano copia sin gate");
    assert!(res.task_id.get() > 0);
}

/// V2 (#131, `host.volumes`): mismo criterio server-side que
/// `agente_sin_scope_ve_policy_denied_humano_copia`, para una operación SIN
/// concepto de scope — la tabla de montaje no es un path bajo el árbol de
/// nadie, así que un agente la ve vedada CATEGÓRICAMENTE (nunca
/// `out-of-scope`: no hay scope que pedir para esto, y pedirle uno no
/// cambiaría la respuesta). `PolicyDenied` con la categoría gruesa
/// `not-approved` del vocabulario cerrado — mismo criterio que
/// `ai.rename_plan`/`index.embed`/`index.search_semantic` — jamás un fallo de
/// transporte que distinga "vedado" de "no implementado". Un daemon SIN
/// `ScopedPolicy` instalada (`spawn_daemon` liso) basta: el gate de
/// `host.volumes` no pasa por el engine ni por `policy.toml`, es una
/// comprobación de actor pura ANTES del parseo de params (security review
/// V2, MAJOR aplicado: el gate corría después de `parse_params`, así que un
/// agente con params inválidos veía `INVALID_PARAMS` en vez de
/// `PolicyDenied` — un oráculo que el agente controla con la forma de su
/// propia petición).
/// `connection.list` es SOLO del humano (#264), por lo mismo que
/// `host.volumes`: la lista nombra los servidores del usuario, y un scope de
/// rutas no lo necesita para nada.
///
/// Y el gate corre ANTES del parseo, así que un agente ve lo mismo mande lo
/// que mande — no puede distinguir «vedado» de «params malos» fuzzeando la
/// forma de su propia petición.
#[tokio::test]
async fn connection_list_es_solo_del_humano() {
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
            .expect_err("un agente no lista conexiones");
        match err {
            ClientError::Rpc(rpc) => assert!(
                matches!(
                    rpc.data,
                    Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved"
                ),
                "vedado pase lo que pase, fue {:?}",
                rpc.data
            ),
            other => panic!("esperaba Rpc, fue {other:?}"),
        }
    }

    // El humano SÍ, y sin params: la ausencia se acepta (ADR 0004). Cuántas
    // haya depende de la máquina; lo que se sostiene es que contesta.
    let human = connected_client(&d).await;
    let _: methods::ConnectionListResult = human
        .call(methods::CONNECTION_LIST, &serde_json::Value::Null)
        .await
        .expect("el humano lista sin gate");
}

#[tokio::test]
async fn agente_ve_policy_denied_en_host_volumes_humano_lo_lista() {
    let d = spawn_daemon(None).await;
    let params = methods::HostVolumesParams {
        include_pseudo: false,
    };

    // Agente (con `agent_session`, sin scope alguno): vedado.
    let agent = connected_agent(&d, "s1").await;
    let err = agent
        .call::<_, methods::HostVolumesResult>(methods::HOST_VOLUMES, &params)
        .await
        .expect_err("agente: host.volumes vedado");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert!(
                matches!(
                    rpc.data,
                    Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved"
                ),
                "PolicyDenied not-approved, fue {:?}",
                rpc.data
            );
        }
        other => panic!("esperaba Rpc, fue {other:?}"),
    }

    // Agente con params MAL FORMADOS (`include_pseudo` no es un bool): SIGUE
    // viendo `PolicyDenied`, no `INVALID_PARAMS` — el gate corre antes de
    // que el parseo pueda fallar, así que la respuesta no depende de nada
    // que el agente controle con la forma de su petición.
    let err = agent
        .call::<_, methods::HostVolumesResult>(
            methods::HOST_VOLUMES,
            &serde_json::json!({"include_pseudo": "no-es-un-bool"}),
        )
        .await
        .expect_err("agente: params inválidos siguen vedados, no INVALID_PARAMS");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert!(
                matches!(
                    rpc.data,
                    Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved"
                ),
                "PolicyDenied not-approved incluso con params inválidos, fue {:?}",
                rpc.data
            );
        }
        other => panic!("esperaba Rpc con PolicyDenied, fue {other:?}"),
    }

    // Humano (sin `agent_session`): lista sin gate. La máquina de test corre
    // Linux de verdad, así que la raíz `/` tiene que aparecer — mismo criterio
    // que `enumerate_finds_the_real_root_filesystem` en `norte-core::volumes`.
    let human = connected_client(&d).await;
    let res: methods::HostVolumesResult = human
        .call(methods::HOST_VOLUMES, &params)
        .await
        .expect("humano: host.volumes sin gate");
    assert!(
        res.volumes.iter().any(|v| v.mount.to_wire() == "file:///"),
        "se esperaba encontrar la raíz entre {:?}",
        res.volumes
    );

    // Humano con params AUSENTES (`null`): se aceptan como el default (ADR
    // 0004), mismo patrón que `task.list`/`plugin.list` — un `bool` con
    // default no es "sin params legales" cuando el cliente omite el objeto
    // entero.
    let res_null: methods::HostVolumesResult = human
        .call(methods::HOST_VOLUMES, &serde_json::Value::Null)
        .await
        .expect("humano: host.volumes con params null (ADR 0004)");
    assert!(
        res_null
            .volumes
            .iter()
            .any(|v| v.mount.to_wire() == "file:///"),
        "params null debe comportarse como include_pseudo: false por default"
    );
}

/// M4 (ADR 0034, review BLOCKER): `index.build` e `index.query` gatean la
/// LECTURA por actor igual que `fs.search`. Un agente sin scope NO puede caminar
/// un árbol arbitrario (cuyos paths saldrían por `task.progress`) ni consultar el
/// índice — ambos devuelven `PolicyDenied out-of-scope`. El gate corre ANTES del
/// engine, así que no importa que el daemon de test no tenga índice instalado.
#[tokio::test]
async fn agente_sin_scope_no_puede_index_build_ni_query() {
    let d = spawn_daemon_policy().await;
    let agent = connected_agent(&d, "s1").await;
    let assert_denied = |err: ClientError| match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
            "PolicyDenied out-of-scope, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    };
    // index.build fuera de scope → denegado (jamás camina el árbol).
    let err = agent
        .call::<_, FsTaskResult>(
            methods::INDEX_BUILD,
            &methods::IndexBuildParams {
                root: vp("mem:///"),
            },
        )
        .await
        .expect_err("index.build sin scope denegado");
    assert_denied(err);
    // index.query fuera de scope → denegado.
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
        .expect_err("index.query sin scope denegado");
    assert_denied(err);
}

/// M3-3b Task 3: round-trip de scope. Un agente pide (`request_scope`) — sin
/// concesión su copia dentro sigue denegada —; un humano concede
/// #132, y lo encontró `protocol-guardian`: **`archive.pack` no puede ser un
/// lavadero.**
///
/// El gate de lectura mira la RAÍZ de la petición y nada más, así que un
/// agente con un scope legítimo sobre un árbol grande podía empaquetarlo
/// entero —directorio de estado del daemon incluido: `journal.db`,
/// `secrets.age`, `connections.toml`— y luego leerse el archivo entrada por
/// entrada, sobre un fichero que está en su propio scope. Un `fs.read` de
/// cualquiera de esos ficheros se deniega; el empaquetado los blanqueaba
/// todos.
///
/// Lo que cierra el agujero son las MISMAS exclusiones que ya usan `fs.search`
/// y `fs.compare` (`policy::walk_exclusions`), aplicadas al recorrido.
#[tokio::test]
async fn un_agente_no_empaqueta_lo_que_no_puede_recorrer() {
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

    // Dentro del scope, empaquetar procede: el gate NO es un «no» a todo.
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
        .expect("dentro del scope se empaqueta");
    assert!(ok.task_id.get() > 0);

    // Y FUERA no: una fuente sin scope se deniega antes de crear Task alguna.
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
        .expect_err("fuera del scope no");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::PolicyDenied { .. })),
            "PolicyDenied, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// (`grant_scope`) y entonces la copia DENTRO del scope procede, pero FUERA
/// sigue denegada. Prueba que el registro es el MISMO que consulta el gate.
#[tokio::test]
async fn scope_request_grant_abre_la_frontera_y_solo_dentro() {
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
            "PolicyDenied out-of-scope, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    };

    let agent = connected_agent(&d, "s1").await;

    // 1) Pide scope para su propia sesión: queda pendiente.
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

    // 2) Sin concesión, la copia DENTRO sigue denegada (frontera cerrada).
    let err = agent
        .call::<_, FsTaskResult>(
            methods::FS_COPY,
            &copy("mem:///proj/src.txt", "mem:///proj/dst.txt"),
        )
        .await
        .expect_err("pendiente aún no concede");
    assert_out_of_scope(err);

    // 3) Un humano concede la petición.
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

    // 4) Ahora la copia DENTRO del scope procede (frontera + regla allow).
    let ok: FsTaskResult = agent
        .call(
            methods::FS_COPY,
            &copy("mem:///proj/src.txt", "mem:///proj/dst.txt"),
        )
        .await
        .expect("dentro del scope procede");
    assert!(ok.task_id.get() > 0);

    // 5) Pero FUERA del scope sigue denegada (la concesión no es un cheque en
    //    blanco: solo abre `mem:///proj`).
    let err_out = agent
        .call::<_, FsTaskResult>(
            methods::FS_COPY,
            &copy("mem:///proj/src.txt", "mem:///out.txt"),
        )
        .await
        .expect_err("fuera del scope");
    assert_out_of_scope(err_out);
}

/// El canal de peticiones tiene sub-cap POR CONEXIÓN: una sola sesión que pide
/// sin que nadie conceda no agota el tope global de las demás.
#[tokio::test]
async fn request_scope_sub_cap_por_conexion() {
    let d = spawn_daemon_policy().await;
    let agent = connected_agent(&d, "s1").await;
    let req = || RequestScopeParams {
        session: "s1".into(),
        roots: vec![vp("mem:///proj")],
        ops: vec!["copy".into()],
        ttl_ms: 60_000,
    };
    // MAX_PENDING_SCOPE_PER_CONN (16) peticiones entran; la 17ª es OVERLOADED.
    for i in 0..16 {
        let _: RequestScopeResult = agent
            .call(methods::POLICY_REQUEST_SCOPE, &req())
            .await
            .unwrap_or_else(|e| panic!("petición {i} dentro del cap: {e:?}"));
    }
    let err = agent
        .call::<_, RequestScopeResult>(methods::POLICY_REQUEST_SCOPE, &req())
        .await
        .expect_err("supera el sub-cap");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::OVERLOADED));
}

/// Una petición de scope sin conceder muere con la conexión que la creó: no
/// sobrevive a su peticionario (anti-fuga del canal global). Tras desconectar
/// el agente, conceder ese `request_id` es `INVALID_PARAMS`.
#[tokio::test]
async fn pending_scope_se_limpia_al_desconectar_el_agente() {
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
        // `agent` se dropea aquí: su mitad de escritura cierra, el daemon ve
        // EOF y ejecuta la limpieza de sus pendientes.
    };
    // La limpieza es asíncrona del lado del daemon: se PREGUNTA hasta que la
    // pendiente no está, en vez de dormir un margen y afirmar. Un margen fijo
    // es una apuesta sobre la máquina; esto falla diciendo qué esperaba.
    let human = connected_client(&d).await;
    hasta!("la pendiente del agente muerto se limpia", {
        let r = human
            .call::<_, GrantScopeResult>(
                methods::POLICY_GRANT_SCOPE,
                &GrantScopeParams { request_id },
            )
            .await;
        // La condición ES la aserción: se sale del bucle solo con el error
        // TIPADO que se espera. Cualquier otra cosa —éxito, u otro código—
        // sigue dando vueltas y acaba en el fallo con nombre del plazo, que
        // dice qué se esperaba en vez de dónde reventó.
        matches!(&r, Err(ClientError::Rpc(rpc)) if rpc.code == codes::INVALID_PARAMS)
    });
}

/// Un agente no puede pedir scope para OTRA sesión (la identidad la fija la
/// conexión, no el cuerpo); y un humano no puede pedir scope (no se sandboxea).
#[tokio::test]
async fn request_scope_rechaza_sesion_ajena_y_no_agente() {
    let d = spawn_daemon_policy().await;

    // Agente s1 pidiendo para s2 → INVALID_PARAMS (no falsea su identidad).
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
        .expect_err("sesión ajena");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));

    // Humano (sin agent_session) pidiendo scope → INVALID_REQUEST.
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
        .expect_err("humano no pide scope");
    assert!(matches!(err2, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));
}

/// Un agente no puede CONCEDER (grant es acto humano); y conceder un
/// `request_id` desconocido es `INVALID_PARAMS`.
#[tokio::test]
async fn grant_scope_es_humano_y_id_desconocido_falla() {
    let d = spawn_daemon_policy().await;

    // Agente intentando conceder → INVALID_REQUEST.
    let agent = connected_agent(&d, "s1").await;
    let err = agent
        .call::<_, GrantScopeResult>(
            methods::POLICY_GRANT_SCOPE,
            &GrantScopeParams { request_id: 0 },
        )
        .await
        .expect_err("agente no concede");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));

    // Humano concediendo un id que no existe → INVALID_PARAMS.
    let human = connected_client(&d).await;
    let err2 = human
        .call::<_, GrantScopeResult>(
            methods::POLICY_GRANT_SCOPE,
            &GrantScopeParams { request_id: 999 },
        )
        .await
        .expect_err("id desconocido");
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

/// Concede a la sesión del `agent` un scope de `copy` sobre `mem:///proj` con
/// el round-trip del wire (request + grant): el camino real, no un atajo.
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

/// Siguiente `policy.approval_required` del stream de notificaciones (ignora
/// `task.progress` intercaladas), con tope de espera.
pub(super) async fn next_approval(human: &mut Client) -> PolicyApprovalRequired {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let n = human.notification().await.expect("canal de notifs vivo");
            if n.method == methods::POLICY_APPROVAL_REQUIRED {
                return serde_json::from_value::<PolicyApprovalRequired>(
                    n.params.expect("la notif lleva params"),
                )
                .expect("shape de PolicyApprovalRequired");
            }
        }
    })
    .await
    .expect("policy.approval_required llega")
}

pub(super) fn assert_not_approved(err: ClientError) {
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved"),
            "PolicyDenied not-approved, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// E2E del Ask (M3-3b Task 4): la copia del agente bajo regla `ask` se
/// suspende, el humano recibe `policy.approval_required` con el contexto (op,
/// sesión, rutas) y su `policy.decide approve` la desbloquea.
#[tokio::test]
async fn ask_aprobado_desbloquea_la_copia() {
    let d = spawn_daemon_ask(Duration::from_secs(30)).await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir proj");
    write_file(&d.mem, "mem:///proj/src.txt", b"hola").await;
    let agent = connected_agent(&d, "s1").await;
    let mut human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await;

    // La copia queda suspendida en el Ask: vive en su propia task. Solo
    // retiene el dispatch de SU conexión — el humano sigue atendido.
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
        "las rutas de display viajan: {:?}",
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
        .expect("aprobada, la copia procede");
    assert!(res.task_id.get() > 0);
}

/// `policy.decide approve=false` deniega: la copia responde `PolicyDenied`
/// `not-approved` y el destino queda intacto (gate PRE-efecto).
#[tokio::test]
async fn ask_denegado_es_policy_denied_sin_tocar_el_fs() {
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
    assert_not_approved(copy.await.expect("join").expect_err("denegada"));
    assert!(
        matches!(
            d.mem.stat(&vp("mem:///proj/dst.txt")).await,
            Err(norte_proto::Error::NotFound)
        ),
        "el destino no se tocó"
    );

    // Re-decidir el mismo id: la decisión lo consumió. Y desde #279 el motivo
    // VIAJA — `already-decided`, no un `INVALID_PARAMS` mudo—: con dos
    // ventanas abiertas eso es exactamente lo que ha pasado, y decirle a quien
    // pulsó «tu clic no llegó» le manda a reintentar algo ya decidido.
    let err = human
        .call::<_, PolicyDecideResult>(
            methods::POLICY_DECIDE,
            &PolicyDecideParams {
                approval_id: notif.approval_id,
                approve: true,
            },
        )
        .await
        .expect_err("id ya decidido");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::ApprovalGone { ref reason }) if reason == "already-decided"),
            "tenía que decir cuál de las tres, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }

    // Y un id que este daemon no ha emitido nunca es OTRA cosa: un modal
    // rancio de antes de un reinicio, no una carrera entre ventanas.
    let err = human
        .call::<_, PolicyDecideResult>(
            methods::POLICY_DECIDE,
            &PolicyDecideParams {
                approval_id: notif.approval_id.saturating_add(10_000),
                approve: true,
            },
        )
        .await
        .expect_err("id que no existe");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::ApprovalGone { ref reason }) if reason == "unknown"),
            "un id jamás emitido es `unknown`, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// Sin decisión, el TTL vence y deniega (`not-approved`): un humano ausente no
/// deja la operación colgada. La notificación anuncia el TTL real.
#[tokio::test]
async fn ask_sin_decision_vence_por_ttl() {
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
    assert_eq!(notif.ttl_ms, 200, "el TTL anunciado es el del router");
    // Nadie decide: vence.
    assert_not_approved(copy.await.expect("join").expect_err("TTL vencido"));
}

/// `policy.pending` resync: un frontend que conecta DESPUÉS del broadcast ve
/// la pendiente. Y los roles se respetan: un agente ni decide ni lista
/// (`INVALID_REQUEST`) — jamás se auto-aprueba.
#[tokio::test]
async fn pending_resync_y_un_agente_ni_decide_ni_lista() {
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

    // Un frontend NUEVO (conectó tras el broadcast) resincroniza por pending.
    let late = connected_client(&d).await;
    let listed: PolicyPendingResult = late
        .call(methods::POLICY_PENDING, &serde_json::json!({}))
        .await
        .expect("policy.pending");
    assert_eq!(listed.pending.len(), 1);
    assert_eq!(listed.pending[0].approval_id, notif.approval_id);
    assert_eq!(listed.pending[0].op, "copy");
    assert_eq!(listed.pending[0].session.as_deref(), Some("s1"));

    // Otra conexión de agente: ni decide ni lista (INVALID_REQUEST).
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
        .expect_err("un agente no decide");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));
    let err2 = agent2
        .call::<_, PolicyPendingResult>(methods::POLICY_PENDING, &serde_json::json!({}))
        .await
        .expect_err("un agente no lista");
    assert!(matches!(err2, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));

    // Decidir un id que este daemon no ha emitido nunca. Desde #279 lo DICE:
    // `unknown`, no «ya se decidió». La secuencia arranca en una semilla del
    // reloj precisamente para que un modal rancio no acierte por colisión, así
    // que un 9999 cae por debajo del primer id posible — y eso es exactamente
    // lo que hay que saber distinguir.
    let err3 = human
        .call::<_, PolicyDecideResult>(
            methods::POLICY_DECIDE,
            &PolicyDecideParams {
                approval_id: 9999,
                approve: true,
            },
        )
        .await
        .expect_err("id desconocido");
    match err3 {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::ApprovalGone { ref reason }) if reason == "unknown"),
            "un id fuera del rango emitido es `unknown`, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba ApprovalGone, fue {other:?}"),
    }

    // Desbloquea y cierra: denegada, y la lista queda vacía.
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
    assert_not_approved(copy.await.expect("join").expect_err("denegada"));
    let listed: PolicyPendingResult = late
        .call(methods::POLICY_PENDING, &serde_json::json!({}))
        .await
        .expect("policy.pending vacío");
    assert!(listed.pending.is_empty());
}

/// `policy.approval_required` va SOLO a conexiones humanas (security MAJOR-1):
/// un agente suscrito no debe enumerar pasivamente rutas/ops de OTRAS sesiones
/// — mismo criterio que el gate User-only de `policy.pending`.
#[tokio::test]
async fn approval_required_no_se_difunde_a_agentes() {
    let d = spawn_daemon_ask(Duration::from_secs(30)).await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir proj");
    write_file(&d.mem, "mem:///proj/src.txt", b"hola").await;
    let agent = connected_agent(&d, "s1").await;
    // El espía conecta ANTES del Ask: su suscripción ya existe al difundir.
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
    // El humano SÍ la recibe (prueba de que el broadcast ya salió)…
    let notif = next_approval(&mut human).await;
    // …y al agente espía no le llega en un margen holgado posterior.
    let leaked = tokio::time::timeout(Duration::from_millis(400), async {
        loop {
            let n = spy.notification().await.expect("canal de notifs vivo");
            if n.method == methods::POLICY_APPROVAL_REQUIRED {
                return;
            }
        }
    })
    .await
    .is_ok();
    assert!(!leaked, "un agente jamás ve el approval_required de otro");

    // Cierra: deniega y desbloquea la copia suspendida.
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
    assert_not_approved(copy.await.expect("join").expect_err("denegada"));
}

/// Un AGENTE no releva, igual que no apaga: es un acto de gobierno humano.
#[tokio::test]
async fn un_agente_no_puede_relevar() {
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
        .expect_err("un agente no");
    assert!(
        matches!(&err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST),
        "{err:?}"
    );
}

#[tokio::test]
async fn daemon_se_apaga_solo_por_inactividad() {
    // Idle holgado (1,2 s): una CI congelada no puede apagar el daemon
    // antes de que el cliente llegue a conectar (m3 del rust-reviewer).
    let d = spawn_daemon(Some(Duration::from_millis(1200))).await;
    {
        // Una conexión breve: mientras vive, no hay apagado.
        //
        // Otro temporizador DEL SISTEMA BAJO PRUEBA: hay que pasar del plazo
        // de inactividad (1,2 s) para poder afirmar que NO se apagó. Es una
        // aserción negativa sobre un plazo ajeno, así que no hay condición que
        // sondear: la prueba es que a los 1,5 s siga vivo.
        let _c = connected_client(&d).await;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(!d.run.is_finished(), "con cliente vivo no se apaga");
    }
    // Cliente fuera: el idle timeout dispara.
    let joined = tokio::time::timeout(Duration::from_secs(5), d.run)
        .await
        .expect("idle shutdown antes del timeout")
        .expect("join limpio");
    joined.expect("apagado sin error");
}

#[tokio::test]
async fn dos_daemons_no_comparten_socket() {
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
    .expect_err("el socket está vivo");
    assert!(matches!(err, DaemonError::AlreadyRunning), "{err:?}");
}

// ---------- seguridad del socket ----------

#[tokio::test]
async fn bind_rechaza_dir_symlink() {
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
    .expect_err("dir symlink rechazado");
    assert!(matches!(err, DaemonError::InsecureDir { .. }), "{err:?}");
}

#[tokio::test]
async fn bind_endurece_el_modo_del_dir_y_del_socket() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let sock_dir = dir.path().join("laxo");
    std::fs::create_dir(&sock_dir).expect("mkdir");
    std::fs::set_permissions(&sock_dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let d = spawn_daemon_at(sock_dir.join("d.sock")).await;
    let md = std::fs::metadata(&sock_dir).expect("stat dir");
    assert_eq!(md.permissions().mode() & 0o777, 0o700, "dir endurecido");
    let md = std::fs::metadata(&d.socket).expect("stat socket");
    assert_eq!(md.permissions().mode() & 0o777, 0o600, "socket 0600");
    let _ = d;
}

// ---------- M3-4 T4: journal en el daemon + policy.undo_session ----------

/// Daemon con JOURNAL (in-memory) + `ScopedPolicy` con regla `allow` + registro
/// de scopes compartido — el escenario del daemon real de M3-4 (dueño único
/// del journal, ADR 0024).
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
        _dir: dir,
        mem,
    }
}

/// Espera el estado terminal de `task_id` vía `task.list` (resync retiene
/// desenlaces recientes), con tope.
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
            "la task {task_id:?} nunca terminó"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// M3-4 T4: un humano deshace por el wire la sesión completa de un agente.
/// El agente (con scope+allow) copia; `policy.undo_session` la revierte
/// aunque el agente ya no tenga scope (ejecutor=User).
#[tokio::test]
async fn policy_undo_session_revierte_lo_del_agente() {
    let d = spawn_daemon_journal().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/src.txt", b"hola").await;

    // Scope efímero por wire y copia del agente.
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
        .expect("copia del agente");
    assert_eq!(
        wait_terminal(&human, copied.task_id).await,
        norte_proto::TaskState::Completed
    );
    assert!(d.mem.stat(&vp("mem:///proj/dst.txt")).await.is_ok());

    // El humano deshace la sesión del agente.
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
        "la copia del agente se revirtió"
    );
    assert!(
        d.mem.stat(&vp("mem:///proj/src.txt")).await.is_ok(),
        "el original intacto"
    );
}

/// Roles y validación de `policy.undo_session`: un agente no lo llama
/// (`INVALID_REQUEST`) y una sesión con formato ilegal es `INVALID_PARAMS`.
#[tokio::test]
async fn policy_undo_session_roles_y_validacion() {
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
        .expect_err("un agente no deshace sesiones por esta vía");
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
        .expect_err("sesión ilegal");
    assert!(matches!(err2, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
}

/// Un AGENTE (conexión con `agent_session`) NO puede aprobar un plugin:
/// consentir capabilities es acto humano de seguridad → `INVALID_REQUEST`.
#[tokio::test]
async fn plugin_set_approval_agente_es_invalid_request() {
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
        .expect_err("un agente no aprueba plugins");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));

    // Y no tocó el estado: sigue sin aprobar para un humano.
    let human = connected_client(&d).await;
    let list: methods::PluginListResult = human
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list");
    assert!(!list.plugins[0].approved, "el rechazo no dejó rastro");
}

/// Aprobar un id DESCONOCIDO es `INVALID_PARAMS` (no se ensucia el estado con
/// plugins fantasma).
#[tokio::test]
async fn plugin_set_approval_id_desconocido_es_invalid_params() {
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
        .expect_err("id desconocido");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
}

/// El ancla que el humano LEYÓ es la que se concede (#282).
///
/// El daemon anclaba el digest que ÉL tenía al escribir, no el que se enseñó,
/// así que entre el `plugin.list` que vio el humano y el `set_approval` que
/// confirma cabía un `plugin.toml` distinto. Con `expected_digest` el daemon
/// rehúsa, y con la variante que significa «vuelve a leerlo»: NO
/// `INVALID_PARAMS`, que es el código de «ese plugin no existe» y dejaría a un
/// cliente sin poder distinguir las dos cosas.
#[tokio::test]
async fn plugin_set_approval_con_ancla_rancia_se_rehusa() {
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
        .expect_err("un ancla que no casa no concede");
    assert!(
        matches!(&err, ClientError::Rpc(rpc) if rpc.code != codes::INVALID_PARAMS),
        "un ancla rancia y un id desconocido no pueden compartir código: {err:?}"
    );

    // Y no concedió nada: la comprobación tiene que ser fail-closed.
    let list: methods::PluginListResult = human
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list");
    assert!(!list.plugins[0].approved, "el rechazo no dejó rastro");
}

/// REVOCAR no comprueba el ancla, y es deliberado: quitar un permiso no
/// concede nada, y rehusar la revocación por un ancla rancia dejaría vivo
/// justo el permiso que alguien intenta quitar.
#[tokio::test]
async fn revocar_no_se_rehusa_por_un_ancla_rancia() {
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
        .expect("aprobada primero");

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
        .expect("revocar no mira el ancla");

    let list: methods::PluginListResult = human
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list");
    assert!(!list.plugins[0].approved, "la revocación se aplicó");
}

/// Un id DESCONOCIDO con ancla sigue siendo `INVALID_PARAMS`: `manifest_digest`
/// devuelve `None` para los dos casos, y contestar «el manifiesto cambió» a
/// quien nombró un plugin que no existe es un diagnóstico equivocado sobre el
/// error más común de un cliente mal escrito.
#[tokio::test]
async fn un_id_desconocido_con_ancla_no_se_confunde_con_una_rancia() {
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
        .expect_err("id desconocido");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
}

/// Mismo criterio que `plugin.list`: leer documentación no consiente nada, así
/// que un AGENTE también puede pedir la página.
#[tokio::test]
async fn plugin_help_esta_abierto_a_un_agente() {
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
        .expect("un agente puede leer la página de un plugin");
    assert!(help.markdown.contains("Demo"));
}

/// Un AGENTE (conexión con `agent_session`) NO puede cambiar un ajuste de
/// plugin: es dato de USUARIO, mismo criterio que
/// `plugin_set_approval_agente_es_invalid_request` → `INVALID_REQUEST`, y
/// NO deja rastro (el humano sigue viendo el default).
#[tokio::test]
async fn plugin_set_config_agente_es_invalid_request() {
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
        .expect_err("un agente no cambia ajustes de plugin");
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
        "el rechazo no dejó rastro"
    );
}

/// TOML deliberadamente inválido: el descubridor debe reportarlo en `errors`,
/// no tumbar el catálogo. Un `[[[` sin cerrar no parsea.
pub(super) const BROKEN_MANIFEST: &str = "no es toml [[[";

/// Daemon apuntado a un `cfg` sembrado con un plugin VÁLIDO (`org.norte.demo`)
/// y uno ROTO (`rota`, TOML inválido). Devuelve también la ruta `cfg` para
/// poder abrir un `PluginRegistry` fresco sobre ella y comprobar persistencia.
pub(super) async fn spawn_daemon_plugins_ok_y_roto() -> (TestDaemon, PathBuf) {
    spawn_daemon_plugins_con(&[]).await
}

/// Como [`spawn_daemon_plugins_ok_y_roto`], más los `(id, manifiesto)` que
/// se pasen, sembrados ANTES de arrancar: el daemon descubre una vez, al
/// arrancar, y un test sobre su registro en memoria tiene que sembrar antes.
pub(super) async fn spawn_daemon_plugins_con(extra: &[(&str, &str)]) -> (TestDaemon, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = dir.path().join("cfg");
    let ok_dir = cfg.join("plugins").join("org.norte.demo");
    let rota_dir = cfg.join("plugins").join("rota");
    std::fs::create_dir_all(&ok_dir).expect("mkdir ok");
    std::fs::create_dir_all(&rota_dir).expect("mkdir rota");
    std::fs::write(ok_dir.join("plugin.toml"), DEMO_MANIFEST).expect("write manifest ok");
    std::fs::write(rota_dir.join("plugin.toml"), BROKEN_MANIFEST).expect("write manifest roto");
    for (id, manifiesto) in extra {
        let d = cfg.join("plugins").join(id);
        std::fs::create_dir_all(&d).expect("mkdir extra");
        std::fs::write(d.join("plugin.toml"), manifiesto).expect("write manifest extra");
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
        _dir: dir,
        mem,
    };
    (d, cfg)
}

/// E2E de cierre M4-P3: round-trip COMPLETO del gestor de extensiones por el
/// wire — descubrimiento (válido + roto), consentimiento humano (aprobar +
/// activar), y persistencia DURABLE releída por un `PluginRegistry` FRESCO
/// (sin daemon). Ata catálogo + estado + errores enmascarados en un solo flujo.
#[tokio::test]
async fn plugin_gestor_e2e_lista_gobierna_y_persiste() {
    // `cfg` es la raíz de config, sembrada con un plugin válido y uno roto.
    let (d, cfg) = spawn_daemon_plugins_ok_y_roto().await;

    // 1) plugin.list: un válido descubierto (sin aprobar/activar, capability
    //    fs-read visible) y un roto reportado por su BASENAME (jamás la ruta
    //    absoluta, que filtraría el home del usuario a un agente).
    let human = connected_client(&d).await;
    let list: methods::PluginListResult = human
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list");
    assert_eq!(list.plugins.len(), 1, "solo el válido carga");
    let p = &list.plugins[0];
    assert_eq!(p.id, "org.norte.demo");
    assert!(!p.approved, "nace sin aprobar");
    assert!(!p.enabled, "nace sin activar");
    assert!(
        p.capabilities.iter().any(|c| c == "fs-read"),
        "la capability declarada se expone como badge: {:?}",
        p.capabilities
    );
    assert_eq!(list.errors.len(), 1, "el roto se reporta, no desaparece");
    let broken = &list.errors[0];
    assert_eq!(broken.dir, "rota", "solo el basename, no la ruta absoluta");
    assert!(
        !broken.dir.contains('/'),
        "el dir reportado nunca es una ruta: {}",
        broken.dir
    );

    // 2) El humano aprueba y activa por el wire.
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
        .expect("aprobar");
    let _: methods::PluginSetEnabledResult = human
        .call(
            methods::PLUGIN_SET_ENABLED,
            &methods::PluginSetEnabledParams {
                id: "org.norte.demo".into(),
                enabled: true,
            },
        )
        .await
        .expect("activar");

    // 3) plugin.list lo refleja en el MISMO daemon.
    let list: methods::PluginListResult = human
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list tras consentir");
    assert!(list.plugins[0].approved, "aprobado se refleja");
    assert!(list.plugins[0].enabled, "activado se refleja");

    // 4) PERSISTENCIA DURABLE: un registro FRESCO abierto directamente sobre
    //    `cfg` (sin daemon) recuerda ambos flags → se escribió
    //    `cfg/plugins-state.toml` de verdad.
    let fresco = norte_core::PluginRegistry::discover(&cfg).expect("discover fresco");
    let persistido = fresco.list();
    assert_eq!(persistido.plugins.len(), 1);
    assert!(
        persistido.plugins[0].approved,
        "approved persistió en plugins-state.toml"
    );
    assert!(
        persistido.plugins[0].enabled,
        "enabled persistió en plugins-state.toml"
    );
    assert!(
        cfg.join("plugins-state.toml").exists(),
        "el estado se escribió a disco"
    );

    // 5) Un AGENTE no puede aprobar (acto humano de seguridad → INVALID_REQUEST).
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
        .expect_err("un agente no gobierna consentimiento");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));
}

/// `plugin.uninstall` (0.71.0, ADR 0104): borra el directorio, retira el
/// consentimiento y —lo que la CLI no podía— lo olvida en el registro EN
/// MEMORIA del daemon, que hasta aquí seguía listando lo borrado hasta
/// reiniciar. Un agente no puede; un id que no está, tampoco.
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "seis pasos sobre el MISMO daemon: partirlos es perder el registro en memoria que se prueba"
)]
async fn plugin_uninstall_por_el_wire_borra_olvida_y_retira_el_consentimiento() {
    // Y un roto con id VÁLIDO, para el paso 5: `rota` no es un id y no se
    // puede nombrar por el wire.
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
        .expect("aprobar");

    // 1) Un AGENTE no desinstala: retirar un consentimiento es tan del humano
    //    como darlo, y borrar ficheros de su configuración, más.
    let agent = connected_agent(&d, "s1").await;
    let err = agent
        .call::<_, methods::PluginUninstallResult>(
            methods::PLUGIN_UNINSTALL,
            &methods::PluginUninstallParams {
                id: "org.norte.demo".into(),
            },
        )
        .await
        .expect_err("un agente no desinstala");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));
    assert!(
        cfg.join("plugins").join("org.norte.demo").is_dir(),
        "y el directorio sigue"
    );

    // 2) El humano sí, y el informe dice que había consentimiento.
    let r: methods::PluginUninstallResult = human
        .call(
            methods::PLUGIN_UNINSTALL,
            &methods::PluginUninstallParams {
                id: "org.norte.demo".into(),
            },
        )
        .await
        .expect("desinstalar");
    assert!(r.was_approved, "tenía consentimiento, y se dice");
    assert!(
        !cfg.join("plugins").join("org.norte.demo").exists(),
        "el directorio se borró"
    );

    // 3) El MISMO daemon ya no lo lista: el registro en memoria lo olvidó.
    let list: methods::PluginListResult = human
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list tras desinstalar");
    assert!(
        list.plugins.iter().all(|p| p.id != "org.norte.demo"),
        "sigue listado tras desinstalar: {:?}",
        list.plugins.iter().map(|p| &p.id).collect::<Vec<_>>()
    );

    // 4) Y el consentimiento se fue con él: un plugin instalado después bajo
    //    el mismo id nace sin aprobar.
    let ok_dir = cfg.join("plugins").join("org.norte.demo");
    std::fs::create_dir_all(&ok_dir).expect("mkdir de nuevo");
    std::fs::write(ok_dir.join("plugin.toml"), DEMO_MANIFEST).expect("write manifest");
    let fresco = norte_core::PluginRegistry::discover(&cfg).expect("discover fresco");
    let reinstalado = fresco
        .list()
        .plugins
        .into_iter()
        .find(|p| p.id == "org.norte.demo")
        .expect("vuelve a descubrirse");
    assert!(!reinstalado.approved, "nace sin aprobar");
    assert!(!reinstalado.enabled, "y apagado");

    // 5) Un plugin ROTO —listado en `errors`, no en `plugins`— se desinstala
    //    igual, y el MISMO daemon deja de anunciarlo como «no cargó»: el
    //    cadáver salía de `plugins` y se quedaba en `errors`.
    let rota = cfg.join("plugins").join("org.norte.rota");
    let list: methods::PluginListResult = human
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list");
    assert!(
        list.errors.iter().any(|e| e.dir == "org.norte.rota"),
        "el daemon lo anuncia como roto: {:?}",
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
        .expect("desinstalar un roto");
    assert!(!r.was_approved, "un roto nunca tuvo consentimiento");
    assert!(!rota.exists(), "y su directorio se borró");
    let list: methods::PluginListResult = human
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list");
    assert!(
        list.errors.iter().all(|e| e.dir != "org.norte.rota"),
        "un roto desinstalado no se sigue anunciando: {:?}",
        list.errors
    );

    // 6) Lo que no está, o no es un id, es INVALID_PARAMS — nunca una ruta.
    for id in ["org.norte.nunca", "../fuera"] {
        let err = human
            .call::<_, methods::PluginUninstallResult>(
                methods::PLUGIN_UNINSTALL,
                &methods::PluginUninstallParams { id: id.into() },
            )
            .await
            .expect_err("no está");
        assert!(
            matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS),
            "{id}"
        );
    }
}

// ---------- #66: gate de actor en task.* y connection.trust_host_key ----------

/// `task.list` de un AGENTE solo muestra SUS tasks: las del humano (vivas o
/// recientes) llevan `current` con paths ajenos (NOTA-1 del security-reviewer
/// en M3-4 T5). El humano sigue viéndolo TODO.
#[tokio::test]
async fn task_list_de_agente_solo_muestra_sus_tasks() {
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
        .expect("copia del humano");
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
        .expect("copia del agente");
    drain_task(&mut agent, at.task_id.get()).await;

    let del_humano: methods::TaskListResult = human
        .call(methods::TASK_LIST, &methods::TaskListParams {})
        .await
        .expect("task.list humano");
    let ids: Vec<u64> = del_humano.tasks.iter().map(|t| t.task_id.get()).collect();
    assert!(ids.contains(&ht.task_id.get()), "el humano ve su task");
    assert!(ids.contains(&at.task_id.get()), "el humano ve TODO");

    let del_agente: methods::TaskListResult = agent
        .call(methods::TASK_LIST, &methods::TaskListParams {})
        .await
        .expect("task.list agente");
    let ids: Vec<u64> = del_agente.tasks.iter().map(|t| t.task_id.get()).collect();
    assert!(ids.contains(&at.task_id.get()), "el agente ve la SUYA");
    assert!(
        !ids.contains(&ht.task_id.get()),
        "el agente NO observa las tasks del humano (paths en `current`)"
    );
}

/// `connection.trust_host_key` es una decisión de confianza HUMANA (como
/// `grant_scope`/`decide`/`undo_session`): un agente no bendice fingerprints.
#[tokio::test]
async fn trust_host_key_de_agente_es_invalid_request() {
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
        .expect_err("un agente no acepta host keys");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));
}

/// #325: teclear una contraseña es un acto HUMANO. Un agente que pudiera
/// inyectar credenciales de sesión elegiría con qué identidad actúa el
/// usuario en el host remoto, así que el gate es el mismo que el del TOFU.
#[tokio::test]
async fn provide_secret_de_agente_es_invalid_request() {
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
        .expect_err("un agente no entrega secretos de conexión");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));
}

/// El broadcast de `task.progress` es el MISMO leak que `task.list`: una
/// conexión de agente no recibe el progreso (con `current`) de tasks ajenas.
/// Otro humano sí lo sigue viendo (base de la fase 3).
#[tokio::test]
async fn progreso_de_task_humana_no_llega_a_conexiones_agente() {
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
        .expect("fs.copy del humano");
    // El otro humano drena hasta el terminal: en ese punto TODOS los frames
    // de la task ya se difundieron (try_send síncrono en el mismo instante).
    let seen = drain_task(&mut human2, task.task_id.get()).await;
    assert_eq!(seen.last().expect("terminal").state, TaskState::Completed);
    let colado = tokio::time::timeout(Duration::from_millis(200), agent.notification()).await;
    assert!(
        colado.is_err(),
        "un agente no recibe task.progress de tasks ajenas: {colado:?}"
    );
}

/// `daemon.shutdown` también es acto humano: sin este gate, un agente
/// bypasea el de `task.cancel` (el hard-shutdown cancela TODAS las tasks)
/// y tumba el daemon de la sesión (MAJOR del security-reviewer en #66).
#[tokio::test]
async fn daemon_shutdown_de_agente_es_invalid_request() {
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
        .expect_err("un agente no apaga el daemon");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));
    // El daemon sigue vivo y sirviendo.
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
        .expect("el daemon no se apagó");
}

// ---------- #71: policy.undo_report ----------

/// `policy.undo_report` es SOLO-User (misma barrera que el undo que lo
/// genera): el informe lleva seq del journal y motivo de bloqueo.
#[tokio::test]
async fn undo_report_de_agente_es_invalid_request() {
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
        .expect_err("un agente no lee informes de undo");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));
}

// ---------- #70: topes por clase (reserva para el humano) ----------

/// Los desenlaces de un AGENTE ocupan como mucho la mitad del anillo
/// `recent`: una ráfaga de tasks triviales de agente NO expulsa los
/// terminales del humano del resync de `task.list` (#70).
#[tokio::test]
async fn terminales_de_agente_no_desplazan_los_del_humano() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///h.bin", &[0xAA; 100]).await;
    let mut human = connected_client(&d).await;
    let mut agent = connected_agent(&d, "sess-ruido").await;

    // 1) El humano completa UNA task.
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
        .expect("copia humana");
    drain_task(&mut human, ht.task_id.get()).await;

    // 2) El agente completa MÁS tasks que el anillo entero (64).
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
            .expect("copia del agente");
        drain_task(&mut agent, at.task_id.get()).await;
    }

    // 3) El terminal del humano SIGUE en su resync.
    let listed: methods::TaskListResult = human
        .call(methods::TASK_LIST, &methods::TaskListParams {})
        .await
        .expect("task.list");
    assert!(
        listed.tasks.iter().any(|t| t.task_id == ht.task_id),
        "el ruido del agente no expulsa el desenlace del humano"
    );
}

/// Las tasks VIVAS de agentes tienen sub-tope: aunque lo agoten, el humano
/// sigue pudiendo encolar (#70). El agente que se pasa recibe OVERLOADED.
#[tokio::test]
async fn tasks_vivas_de_agente_no_agotan_el_cupo_del_humano() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///src.bin", &[0xBB; 100]).await;
    // Las tasks del agente quedan vivas: la primera bloqueada en latencia,
    // el resto encoladas en el scheduler (registradas = vivas).
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_mins(2)));
    let human = connected_client(&d).await;
    let agent = connected_agent(&d, "sess-gloton").await;

    // El agente encola hasta su sub-tope (384): todas aceptadas.
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
            .unwrap_or_else(|e| panic!("copia {i} del agente aceptada: {e:?}"));
    }
    // La 385ª del agente: OVERLOADED (su clase está llena).
    let err = agent
        .call::<_, FsTaskResult>(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///src.bin"),
                to: vp("mem:///glotón.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
                queued: false,
            },
        )
        .await
        .expect_err("el sub-tope de agentes corta");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::OVERLOADED));

    // El humano SIGUE pudiendo encolar: su reserva no se toca.
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
        .expect("la reserva del humano sobrevive al agente glotón");
}

// ---------- #64: muerte del peer durante un Ask suspendido ----------

/// #64: la MUERTE del peticionario cancela su Ask suspendido — la pendiente
/// NO queda zombi hasta el TTL. El dispatch se racea contra la vida del
/// socket: al morir el peer se dropea el future del gate y su guard RAII
/// retira la pendiente del router.
#[tokio::test]
async fn muerte_del_peer_cancela_su_ask_suspendido() {
    // TTL LARGO a propósito: solo la muerte del peer puede limpiar a tiempo.
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
        "la pendiente existe mientras el peticionario vive"
    );

    // Muere el peticionario: abortar la task dropea su Client → EOF.
    copy.abort();
    let _ = copy.await;

    // La pendiente desaparece PRONTO — no a los 30s del TTL.
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
            "pendiente ZOMBI: la muerte del peer no canceló su Ask (#64)"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Y el destino jamás se tocó (el gate murió ANTES del efecto).
    assert!(matches!(
        d.mem.stat(&vp("mem:///proj/dst.txt")).await,
        Err(Error::NotFound)
    ));
}

// ---------- #72: rpc.cancel de un tools/call suspendido en un Ask ----------

/// #72: el agente RETIRA (rpc.cancel) su propia fs.copy suspendida en un Ask —
/// el daemon dispara el token de esa request en vuelo, dropea el dispatch
/// (gate PRE-efecto: su guard limpia la pendiente), responde `Error::Cancelled`
/// y JAMÁS aprueba (fail-closed). A diferencia de la muerte del peer (#64), la
/// conexión del agente SIGUE VIVA y usable tras la retirada.
#[tokio::test]
async fn rpc_cancel_retira_el_ask_suspendido_sin_matar_la_conexion() {
    // TTL LARGO: solo el rpc.cancel puede retirar el Ask a tiempo.
    let d = spawn_daemon_ask(Duration::from_secs(30)).await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir proj");
    write_file(&d.mem, "mem:///proj/src.txt", b"hola").await;
    let agent = Arc::new(connected_agent(&d, "s1").await);
    let mut human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await;

    // El agente lanza fs.copy y captura el id JSON-RPC asignado (el que un
    // rpc.cancel debe apuntar). `call_tracked` invoca `on_id` ANTES de esperar.
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

    // El humano ve el Ask: la pendiente existe mientras la copia se suspende.
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
        "la pendiente existe mientras la copia se suspende"
    );

    // El agente RETIRA su request suspendida (rpc.cancel, best-effort notify).
    let id = esperar_id(&id_slot).await;
    agent
        .notify(
            methods::RPC_CANCEL,
            &methods::RpcCancelParams {
                id: norte_proto::wire::RequestId::Num(id),
            },
        )
        .expect("rpc.cancel notify");

    // La fs.copy responde Error::Cancelled (jamás aprobada: fail-closed).
    let res = copy.await.expect("join de la copia");
    match res {
        Err(ClientError::Rpc(rpc)) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert_eq!(rpc.data, Some(Error::Cancelled));
        }
        other => panic!("esperaba Cancelled, fue {other:?}"),
    }

    // La pendiente se retira PRONTO — no a los 30s del TTL.
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
            "pendiente ZOMBI: el rpc.cancel no retiró el Ask (#72)"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // El destino jamás se tocó (el gate murió ANTES del efecto).
    assert!(matches!(
        d.mem.stat(&vp("mem:///proj/dst.txt")).await,
        Err(Error::NotFound)
    ));

    // La conexión del agente SIGUE VIVA tras el rpc.cancel (≠ muerte del peer):
    // otra request se atiende con normalidad.
    let st: FsStatResult = agent
        .call(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("mem:///proj/src.txt"),
                attrs: Vec::new(),
            },
        )
        .await
        .expect("la conexión sigue viva tras el rpc.cancel");
    assert_eq!(st.entry.size, Some(4));
}

/// #72 (carrera A): el `rpc.cancel` GANA a un `policy.decide` posterior. El
/// agente retira su fs.copy suspendida; cuando el humano intenta aprobarla
/// después, la pendiente ya no existe → `policy.decide` responde
/// `INVALID_PARAMS` (no un ok silencioso) y el destino jamás se toca.
#[tokio::test]
async fn cancel_gana_a_un_decide_posterior() {
    // TTL LARGO: solo el rpc.cancel puede retirar el Ask a tiempo.
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

    // El humano ve el Ask (la copia está suspendida): sincroniza la carrera.
    let notif = next_approval(&mut human).await;

    // ACCIÓN 1 (única "primera"): el agente RETIRA la request suspendida.
    let id = esperar_id(&id_slot).await;
    agent
        .notify(
            methods::RPC_CANCEL,
            &methods::RpcCancelParams {
                id: norte_proto::wire::RequestId::Num(id),
            },
        )
        .expect("rpc.cancel notify");

    // La fs.copy responde Cancelled (fail-closed: jamás aprobada).
    match copy.await.expect("join de la copia") {
        Err(ClientError::Rpc(rpc)) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert_eq!(rpc.data, Some(Error::Cancelled));
        }
        other => panic!("esperaba Cancelled, fue {other:?}"),
    }

    // ACCIÓN 2 (llega TARDE): el humano intenta aprobar la ya-retirada. La
    // pendiente no existe → error, jamás un ok silencioso. Y desde #279 dice
    // cuál de las tres formas: `already-decided`, porque ese id SÍ existió y
    // alguien lo resolvió —aquí, el propio peticionario retirándolo—. Lo que
    // no puede contestar es `unknown`, que mandaría a quien pulsó a buscar un
    // daemon reiniciado que no existe.
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
            "decide sobre una pendiente retirada tiene que decir que ya se resolvió, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba ApprovalGone, fue {other:?}"),
    }

    // El destino jamás se ejecutó.
    assert!(matches!(
        d.mem.stat(&vp("mem:///proj/dst.txt")).await,
        Err(Error::NotFound)
    ));
}

/// #72 (carrera B): el `policy.decide` GANA a un `rpc.cancel` posterior. El
/// humano aprueba antes de que llegue la retirada; la copia procede como Task
/// gobernada y el `rpc.cancel` de la request YA resuelta es un no-op benigno
/// que no perturba la conexión del agente.
#[tokio::test]
async fn decide_gana_a_un_cancel_posterior() {
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

    // El humano ve el Ask y APRUEBA (acción "primera").
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

    // Aprobada → la copia procede: joins Ok con task_id asignado.
    let res = copy
        .await
        .expect("join de la copia")
        .expect("aprobada, la copia procede");
    assert!(res.task_id.get() > 0);

    // ACCIÓN 2 (llega TARDE): rpc.cancel de la request YA resuelta. No-op
    // benigno — NO debe perturbar la conexión del agente.
    let id = esperar_id(&id_slot).await;
    agent
        .notify(
            methods::RPC_CANCEL,
            &methods::RpcCancelParams {
                id: norte_proto::wire::RequestId::Num(id),
            },
        )
        .expect("rpc.cancel notify");

    // La conexión del agente sigue viva y atiende: prueba del no-op benigno.
    let st: FsStatResult = agent
        .call(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("mem:///proj/src.txt"),
                attrs: Vec::new(),
            },
        )
        .await
        .expect("la conexión sigue viva tras el rpc.cancel de una request resuelta");
    assert_eq!(st.entry.size, Some(4));
}

/// #72 (backpressure/anti-DoS, MAJOR del security-reviewer): mientras una
/// fs.copy está SUSPENDIDA en un Ask, el agente hace pipeline de varias
/// requests más. El inner loop las bufferiza (`pending_frames`) SIN perderlas;
/// cuando el Ask se retira (rpc.cancel), TODAS se procesan tras el desenlace,
/// en el mismo orden de llegada (dispatch serial). Prueba que el búfer de
/// diferidos drena FIFO y que ninguna request queda huérfana.
#[tokio::test]
async fn frames_pipelined_durante_un_ask_se_procesan_tras_el_desenlace() {
    let d = spawn_daemon_ask(Duration::from_secs(30)).await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir proj");
    write_file(&d.mem, "mem:///proj/src.txt", b"hola").await;
    let agent = Arc::new(connected_agent(&d, "s1").await);
    let mut human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await;

    // Lanza la copia y captura su id; se suspende en el Ask.
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
    let copy_id = esperar_id(&id_slot).await;

    // Con la copia suspendida, el agente pipelinea 5 fs.stat: el daemon las
    // lee del socket y las difiere (no las despacha hasta que el Ask resuelva).
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
    // Deja que los 5 frames lleguen al daemon (se bufferizan tras la copia).
    //
    // ESTE `sleep` se queda y no hay forma de afinarlo: lo que se espera es
    // que el daemon los haya LEÍDO y DIFERIDO, y diferir es exactamente no
    // contestar nada — no hay observable que sondear. Sostiene el SIGNIFICADO
    // del test, no su corrección: sin él, un frame que no hubiera llegado
    // antes del cancel se despacharía por el camino normal y el test pasaría
    // sin haber ejercitado el diferido. Quitarlo no lo pone rojo; lo vacía.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // El agente retira la copia → se libera el dispatch; los 5 stats diferidos
    // se procesan a continuación.
    agent
        .notify(
            methods::RPC_CANCEL,
            &methods::RpcCancelParams {
                id: norte_proto::wire::RequestId::Num(copy_id),
            },
        )
        .expect("rpc.cancel notify");

    assert!(matches!(
        copy.await.expect("join copia"),
        Err(ClientError::Rpc(rpc)) if rpc.data == Some(Error::Cancelled)
    ));
    // Ninguno de los 5 diferidos se perdió: todos responden Ok.
    for (i, h) in pipelined.into_iter().enumerate() {
        let st = h
            .await
            .expect("join stat")
            .unwrap_or_else(|e| panic!("stat diferido {i} debía responder Ok: {e:?}"));
        assert_eq!(st.entry.size, Some(4));
    }
}

/// Gate de lectura de agentes: sin scope, `fs.search` es `PolicyDenied`
/// out-of-scope; con un scope concedido que cubre el root, procede.
#[tokio::test]
async fn agente_fuera_de_scope_no_busca() {
    let d = spawn_daemon_policy().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/a.rs", b"x").await;
    let mut agent = connected_agent(&d, "s1").await;

    // 1) Sin scope: denegado por el gate de lectura.
    let err = agent
        .call::<_, FsTaskResult>(methods::FS_SEARCH, &search_by_name("mem:///proj", "*.rs"))
        .await
        .expect_err("sin scope no busca");
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

    // 3) Ahora la búsqueda bajo el scope procede.
    let task: FsTaskResult = agent
        .call(methods::FS_SEARCH, &search_by_name("mem:///proj", "*.rs"))
        .await
        .expect("con scope busca");
    let (hits, state) = drain_search(&mut agent, task.task_id.get()).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(hits.len(), 1);
}

/// Un scope que cubre `mem:///a` NO habilita buscar en `mem:///b`.
#[tokio::test]
async fn agente_scope_no_cubre_root() {
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

    // Scope en /a, búsqueda en /b → out-of-scope.
    let err = agent
        .call::<_, FsTaskResult>(methods::FS_SEARCH, &search_by_name("mem:///b", "*"))
        .await
        .expect_err("scope /a no cubre /b");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
            "PolicyDenied out-of-scope, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// El gate: comparar LEE dos árboles, así que un agente necesita scope vivo
/// sobre AMBAS raíces. Con una sola no basta, y la denegación dice únicamente
/// la categoría gruesa.
#[tokio::test]
async fn fs_compare_agente_necesita_scope_en_ambas_raices() {
    let d = spawn_daemon_policy().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir proj");
    d.mem
        .mkdir(&vp("mem:///proj/sub"))
        .await
        .expect("mkdir sub");
    d.mem.mkdir(&vp("mem:///otro")).await.expect("mkdir otro");
    let agent = connected_agent(&d, "s1").await;
    let human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await; // scope sobre mem:///proj

    // La raíz derecha cae fuera del scope → denegado.
    let err = agent
        .call::<_, FsTaskResult>(
            methods::FS_COMPARE,
            &compare_params("mem:///proj", "mem:///otro"),
        )
        .await
        .expect_err("la derecha está fuera de scope");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
            "PolicyDenied out-of-scope, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
    // Y en el otro sentido tampoco: el gate mira las DOS, no la primera.
    let err = agent
        .call::<_, FsTaskResult>(
            methods::FS_COMPARE,
            &compare_params("mem:///otro", "mem:///proj"),
        )
        .await
        .expect_err("la izquierda está fuera de scope");
    assert!(matches!(err, ClientError::Rpc(_)), "fue {err:?}");

    // Las dos bajo el scope → procede.
    let _: FsTaskResult = agent
        .call(
            methods::FS_COMPARE,
            &compare_params("mem:///proj", "mem:///proj/sub"),
        )
        .await
        .expect("ambas bajo el scope");
}

/// El rung de hash LEE CONTENIDO, y un scope que solo concede `mkdir` cubre la
/// lectura de estructura pero no el manejo de bytes: la comparación barata
/// pasa y la hasheada no.
#[tokio::test]
async fn fs_compare_el_rung_de_hash_exige_scope_de_contenido() {
    let d = spawn_daemon_policy().await;
    d.mem.mkdir(&vp("mem:///data")).await.expect("mkdir data");
    d.mem.mkdir(&vp("mem:///data/l")).await.expect("mkdir l");
    d.mem.mkdir(&vp("mem:///data/r")).await.expect("mkdir r");
    write_file(&d.mem, "mem:///data/l/a.txt", b"x").await;
    write_file(&d.mem, "mem:///data/r/a.txt", b"x").await;
    let agent = connected_agent(&d, "s1").await;
    let human = connected_client(&d).await;

    // Scope de SOLO `mkdir` sobre mem:///data: lectura sí, contenido no.
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

    // Barata: pasa (el gate de lectura es op-independiente).
    let _: FsTaskResult = agent
        .call(
            methods::FS_COMPARE,
            &compare_params("mem:///data/l", "mem:///data/r"),
        )
        .await
        .expect("sin hash procede");

    // Con hash: denegada — leer estructura no es leer bytes.
    let mut p = compare_params("mem:///data/l", "mem:///data/r");
    p.criteria.hash = true;
    let err = agent
        .call::<_, FsTaskResult>(methods::FS_COMPARE, &p)
        .await
        .expect_err("el hash exige scope de contenido");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
            "PolicyDenied out-of-scope, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// Y el lado que PERMITE, que es el que de verdad puede romperse en silencio:
/// con un scope de `copy` sobre la raíz, las DOS puertas (lectura + contenido)
/// se componen y la comparación hasheada procede hasta terminar. Sin este
/// test, un `content_gate` que denegara siempre pasaría el de arriba.
#[tokio::test]
async fn fs_compare_con_scope_de_copy_el_hash_procede() {
    let d = spawn_daemon_policy().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir proj");
    d.mem.mkdir(&vp("mem:///proj/l")).await.expect("mkdir l");
    d.mem.mkdir(&vp("mem:///proj/r")).await.expect("mkdir r");
    write_file(&d.mem, "mem:///proj/l/a.txt", b"x").await;
    write_file(&d.mem, "mem:///proj/r/a.txt", b"x").await;
    let mut agent = connected_agent(&d, "s1").await;
    let human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await; // scope `copy` sobre /proj

    let mut p = compare_params("mem:///proj/l", "mem:///proj/r");
    p.criteria.hash = true;
    let task: FsTaskResult = agent
        .call(methods::FS_COMPARE, &p)
        .await
        .expect("con scope de copy el hash procede");
    let (batches, state) = drain_compare(&mut agent, task.task_id.get()).await;
    assert_eq!(state, TaskState::Completed);
    let rows: Vec<_> = batches.into_iter().flat_map(|b| b.rows).collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].verdict, methods::CompareVerdict::Same);
    assert_eq!(rows[0].criterion, methods::CompareCriterion::Hash);
}

/// El gate: planificar LEE dos árboles, así que un agente necesita scope vivo
/// sobre AMBAS raíces — y el gate va ANTES de validar params, así que unos
/// params malos tampoco le dicen nada.
#[tokio::test]
async fn sync_plan_agente_necesita_scope_en_ambas_raices() {
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
    grant_copy_scope(&agent, &human, "s1").await; // scope sobre mem:///proj

    for p in [
        sync_params("mem:///proj", "mem:///otro"),
        sync_params("mem:///otro", "mem:///proj"),
    ] {
        let err = agent
            .call::<_, FsTaskResult>(methods::SYNC_PLAN, &p)
            .await
            .expect_err("una de las dos está fuera de scope");
        match err {
            ClientError::Rpc(rpc) => assert!(
                matches!(rpc.data, Some(Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
                "PolicyDenied out-of-scope, fue {:?}",
                rpc.data
            ),
            other => panic!("esperaba Rpc, fue {other:?}"),
        }
    }

    // El gate va PRIMERO: unos params imposibles siguen contestando denegado, no
    // «además tu petición estaba mal».
    let mut p = sync_params("mem:///otro", "mem:///proj");
    p.compare.follow_symlinks = true;
    let err = agent
        .call::<_, FsTaskResult>(methods::SYNC_PLAN, &p)
        .await
        .expect_err("fuera de scope");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PolicyDenied { .. })),
            "el gate va antes que la validación de params, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }

    // Con las dos bajo el scope, procede.
    let _: FsTaskResult = agent
        .call(
            methods::SYNC_PLAN,
            &sync_params("mem:///proj/l", "mem:///proj/r"),
        )
        .await
        .expect("ambas bajo el scope");
}

/// El rung de hash LEE CONTENIDO también aquí: un scope que solo concede
/// `mkdir` planifica barato y no hasheado.
#[tokio::test]
async fn sync_plan_el_rung_de_hash_exige_scope_de_contenido() {
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
        .expect_err("el hash exige scope de contenido");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
            "PolicyDenied out-of-scope, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// Asevera un `-32602` (params que el server rehúsa sin crear Task).
pub(super) fn assert_invalid_params<T: std::fmt::Debug>(r: Result<T, ClientError>) {
    match r {
        Err(ClientError::Rpc(rpc)) => assert_eq!(rpc.code, codes::INVALID_PARAMS, "{rpc:?}"),
        other => panic!("esperaba INVALID_PARAMS, fue {other:?}"),
    }
}

/// Asevera que una lectura da `PolicyDenied` out-of-scope (helper de #80).
pub(super) async fn assert_read_denied(
    agent: &Client,
    method: &str,
    params: &impl serde::Serialize,
) {
    let err = agent
        .call::<_, serde_json::Value>(method, params)
        .await
        .expect_err("sin scope no lee");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
            "{method}: PolicyDenied out-of-scope, fue {:?}",
            rpc.data
        ),
        other => panic!("{method}: esperaba Rpc, fue {other:?}"),
    }
}

/// #80: las LECTURAS (`list`/`read`/`stat`/`capabilities`) gatean por scope
/// para agentes, igual que las mutaciones. Sin scope da `PolicyDenied`; con un
/// scope que cubre la raíz (op-independiente: un grant de `copy` basta) da OK.
#[tokio::test]
async fn agente_sin_scope_no_lee_y_con_scope_si() {
    let d = spawn_daemon_policy().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/a.txt", b"hola").await;
    let agent = connected_agent(&d, "s1").await;

    // 1) Sin scope: los cuatro reads denegados.
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

    // 2) Un humano concede scope sobre mem:///proj (op copy — cubre lectura).
    let human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await;

    // 3) Ahora los cuatro reads proceden.
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
        .expect("con scope lista");
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
        .expect("con scope statea");
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
        .expect("con scope lee");
    assert!(!read.content_b64.is_empty(), "leyó algo con scope");
    let _caps: methods::FsCapabilitiesResult = agent
        .call(
            methods::FS_CAPABILITIES,
            &methods::FsCapabilitiesParams {
                path: vp("mem:///proj"),
            },
        )
        .await
        .expect("con scope capabilities");
}

/// #80 (bypass CRÍTICO cerrado): `plugin.preview` LEE el archivo con la
/// autoridad del daemon — sin gate sería la puerta lateral a `fs.read`. Un
/// agente sin scope no previsualiza; con scope, procede (sin previewer casando
/// = `None`, no error, pero PASA el gate).
#[tokio::test]
async fn agente_sin_scope_no_preview() {
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
    // Con scope: pasa el gate (sin previewer instalado → preview None, no error).
    let res: methods::PluginPreviewResult = agent
        .call(
            methods::PLUGIN_PREVIEW,
            &methods::PluginPreviewParams {
                path: vp("mem:///proj/nota.txt"),
            },
        )
        .await
        .expect("con scope el gate deja pasar");
    assert!(res.preview.is_none());
}

/// #80: un HUMANO (User) lee sin scope — no se sandboxea, simetría con las
/// mutaciones (User = allow-all).
#[tokio::test]
async fn humano_lee_sin_scope() {
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
        .expect("humano lista sin scope");
    assert_eq!(list.entries.len(), 1);
}

// ---------- #44: connection.degraded (solo humanos) ----------

/// Provider remoto trivial: responde a CUALQUIER path con un directorio. Es el
/// stand-in de la sesión establecida por el conector falso (mismo criterio que
/// el `EcoProvider` de `connect.rs`); su único cometido es que el connect
/// TENGA ÉXITO — el resultado del `fs.stat` no importa, sí que el aviso se haya
/// difundido antes del response.
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

/// Conector falso que SIEMPRE degrada: cada connect devuelve un provider vivo
/// ([`EcoProvider`]) más un aviso `TlsAuthRejected` para `backup.example`.
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
        // El provider vivo responde `stat`; `mem` queda como testigo de que el
        // conector puede sostener uno propio si hiciera falta.
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

/// Daemon cuyo engine tiene inyectado un [`DegradingConnector`]: cualquier
/// acceso a `ftp://backup.example/…` establece una sesión degradada.
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
        _dir: dir,
        mem,
    }
}

/// Siguiente `connection.degraded` del stream (ignora otras notifs), con tope.
pub(super) async fn next_degraded(c: &mut Client) -> ConnectionDegraded {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let n = c.notification().await.expect("canal de notifs vivo");
            if n.method == methods::CONNECTION_DEGRADED {
                return serde_json::from_value::<ConnectionDegraded>(
                    n.params.expect("la notif lleva params"),
                )
                .expect("shape de ConnectionDegraded");
            }
        }
    })
    .await
    .expect("connection.degraded llega")
}

/// #44: al degradarse una sesión remota, el humano recibe `connection.degraded`
/// (scheme/host/reason del vocabulario cerrado); una conexión de agente NO —
/// es info de seguridad para el usuario, no para el agente (mismo criterio que
/// `policy.*`).
#[tokio::test]
async fn degradacion_de_conexion_solo_a_humanos() {
    let d = spawn_daemon_degrading().await;
    let mut human = connected_client(&d).await;
    let mut agent = connected_agent(&d, "s1").await;

    // El humano dispara el connect perezoso a la sesión degradada. El aviso se
    // difunde de forma SÍNCRONA dentro del dispatch, ANTES de escribir el
    // response de este `fs.stat`: cuando el `call` retorna, el broadcast ya
    // ocurrió (mismo argumento que `progreso_de_task_humana_no_llega_a_...`).
    let _stat: FsStatResult = human
        .call(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("ftp://backup.example/"),
                attrs: Vec::new(),
            },
        )
        .await
        .expect("fs.stat dispara el connect degradado");

    let deg = next_degraded(&mut human).await;
    assert_eq!(deg.scheme, "ftp");
    assert_eq!(deg.host, "backup.example");
    assert_eq!(deg.reason, "tls-auth-rejected");
    assert_eq!(deg.detail, None);

    // El agente NO la recibe. El broadcast fue síncrono y previo al response ya
    // recibido: no queda ningún camino diferido que se la entregue tarde → un
    // tope corto sin frame es robusto (no flaky).
    let colado = tokio::time::timeout(Duration::from_millis(200), agent.notification()).await;
    assert!(
        colado.is_err(),
        "un agente no recibe connection.degraded: {colado:?}"
    );
}

/// Conector que SIEMPRE falla con causa contable (#322), y con userinfo en la
/// authority para probar que no sale.
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

/// Daemon cuyo engine no puede conectar con nada remoto.
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
        _dir: dir,
        mem,
    }
}

/// #322: el humano recibe `connection.failed` con el motivo; el agente NO.
///
/// Y cruza el socket de verdad, que es la mitad que ninguna prueba de unidad
/// cubre: el marco se codifica, se difunde y el SDK del otro lado lo decodea.
/// Un error de dedo en la comparación del método sería invisible sin esto.
#[tokio::test]
async fn fallo_de_conexion_solo_a_humanos_y_sin_userinfo() {
    let d = spawn_daemon_failing().await;
    let mut human = connected_client(&d).await;
    let mut agent = connected_agent(&d, "s1").await;

    // Con USUARIO en la authority: lo que va delante del `@` no puede salir.
    let err = human
        .call::<_, FsStatResult>(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("sftp://alice@maquina.example/x"),
                attrs: Vec::new(),
            },
        )
        .await
        .expect_err("el connect falla");
    let _ = err;

    let fallo = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let n = human.notification().await.expect("canal de notifs vivo");
            if n.method == methods::CONNECTION_FAILED {
                return serde_json::from_value::<norte_proto::methods::ConnectionFailed>(
                    n.params.expect("la notif lleva params"),
                )
                .expect("shape de ConnectionFailed");
            }
        }
    })
    .await
    .expect("connection.failed llega");

    assert_eq!(fallo.scheme, "sftp");
    assert_eq!(
        fallo.host, "maquina.example",
        "el userinfo NO sale (regla 10)"
    );
    assert!(!fallo.host.contains('@'), "ni rastro de alice");
    assert_eq!(fallo.reason, "secret-empty");
    assert_eq!(fallo.conn.as_deref(), Some("rosetta"));
    assert!(fallo.detail.is_some());

    // El agente no la recibe: es una frase para leer, y un agente decide por
    // categoría — que ya le llega en el error de su operación.
    let colado = tokio::time::timeout(Duration::from_millis(200), agent.notification()).await;
    assert!(
        colado.is_err(),
        "un agente no recibe connection.failed: {colado:?}"
    );
}

/// **`ai.rename_plan` le contesta lo MISMO a un agente pase lo que pase**
/// (#122): la IA es solo del humano, y el gate va ANTES del parseo.
///
/// Antes contestaba `out-of-scope` a quien estaba fuera y `not-approved` a
/// quien estaba dentro, y comprobaba el tamaño de la instrucción y la validez
/// de los params antes que nada. O sea que un método VEDADO respondía cosas
/// distintas según lo que el agente mandara: eso es un oráculo sobre el árbol
/// del humano —«¿existe este directorio?», «¿lo cubre mi scope?»— servido por
/// una puerta que se supone cerrada.
///
/// Se afirman los dos agentes juntos a propósito: lo que hay que sostener no
/// es una categoría concreta, es que **las dos respuestas sean iguales**.
#[tokio::test]
async fn ai_rename_plan_le_dice_lo_mismo_a_todo_agente() {
    let d = spawn_daemon_policy().await;
    let sin_scope = connected_agent(&d, "s1").await;
    let err_sin = sin_scope
        .call::<_, methods::AiRenamePlanResult>(
            methods::AI_RENAME_PLAN,
            &methods::AiRenamePlanParams {
                dir: vp("mem:///"),
                instruction: "x".into(),
                names: Vec::new(),
            },
        )
        .await
        .expect_err("agente denegado");

    // El mismo agente, ahora CON scope de lectura vivo sobre otro directorio.
    let human = connected_client(&d).await;
    grant_copy_scope(&sin_scope, &human, "s1").await;
    let err_con = sin_scope
        .call::<_, methods::AiRenamePlanResult>(
            methods::AI_RENAME_PLAN,
            &methods::AiRenamePlanParams {
                dir: vp("mem:///proj"),
                instruction: "x".into(),
                names: Vec::new(),
            },
        )
        .await
        .expect_err("agente denegado igual");

    for (quien, err) in [("sin scope", err_sin), ("con scope", err_con)] {
        match err {
            ClientError::Rpc(rpc) => assert!(
                matches!(rpc.data, Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved"),
                "[{quien}] tenía que ser `not-approved` y fue {:?}",
                rpc.data
            ),
            other => panic!("[{quien}] esperaba Rpc, fue {other:?}"),
        }
    }
}

/// MINOR-1 (security review M4-IA): la IA es SOLO para el humano — un agente
/// CON scope de lectura VIVO pasa el `read_gate` pero se deniega igualmente
/// (`not-approved`, vocabulario cerrado): no quema cuota del proveedor ni
/// empuja basenames + instrucción fuera de la máquina sin rastro (el path de
/// lectura no journaliza).
#[tokio::test]
async fn agente_con_scope_tampoco_puede_ai_rename_plan() {
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
        .expect_err("agente con scope: la IA sigue vedada");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved"),
            "PolicyDenied not-approved, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

// ---------- index.embed / index.search_semantic (M4-IA-2) ----------

/// Daemon con índice en memoria + proveedor de embeddings fake (M4-IA-2):
/// `[ai]` habilitado con un proveedor `fake` declarado como `embed_provider`.
/// `delay` retrasa cada `embed` para dejar la request EN VUELO (rpc.cancel).
pub(super) async fn spawn_daemon_embed(delay: Option<Duration>) -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let index = norte_core::Index::open_memory()
        .await
        .expect("index memoria");
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
        _dir: dir,
        mem,
    }
}

/// M4-IA-2 security: los embeddings son SOLO para el humano — una conexión de
/// agente ve `PolicyDenied not-approved` en `index.embed` Y en
/// `index.search_semantic` ANTES de cualquier gate de lectura o engine (los
/// prefijos de contenido / la query saldrían del proceso, mismo criterio que
/// `ai.rename_plan`).
#[tokio::test]
async fn agente_no_puede_embed_ni_semantic() {
    let d = spawn_daemon_embed(None).await;
    let agent = connected_agent(&d, "s1").await;
    let assert_not_approved = |err: ClientError| match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved"),
            "PolicyDenied not-approved, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    };
    let err = agent
        .call::<_, FsTaskResult>(
            methods::INDEX_EMBED,
            &methods::IndexEmbedParams {
                root: vp("mem:///"),
            },
        )
        .await
        .expect_err("agente: index.embed vedado");
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
        .expect_err("agente: index.search_semantic vedado");
    assert_not_approved(err);
}

/// El veto al agente precede al PARSEO de params (security audit M4-IA-2): con
/// params MALFORMADOS la respuesta sigue siendo `PolicyDenied not-approved` y
/// nunca `INVALID_PARAMS`. Así el agente no distingue "schema malo" de
/// "vedado" — nada de lo que envía cambia lo que ve.
#[tokio::test]
async fn agente_con_params_malformados_ve_policy_denied_no_invalid_params() {
    let d = spawn_daemon_embed(None).await;
    let agent = connected_agent(&d, "s1").await;
    let assert_not_approved = |err: ClientError| match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved"),
            "PolicyDenied not-approved (nunca INVALID_PARAMS), fue {rpc:?}"
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    };
    let err = agent
        .call::<_, serde_json::Value>(methods::INDEX_EMBED, &serde_json::json!({ "root": 42 }))
        .await
        .expect_err("agente: index.embed vedado pese a params malos");
    assert_not_approved(err);
    let err = agent
        .call::<_, serde_json::Value>(
            methods::INDEX_SEARCH_SEMANTIC,
            &serde_json::json!({ "query": [] }),
        )
        .await
        .expect_err("agente: index.search_semantic vedado pese a params malos");
    assert_not_approved(err);
}

/// #80: `fs.rename_batch_plan` es una LECTURA de directorio disfrazada — sus
/// veredictos dicen qué nombres existen —, así que pasa por el mismo
/// `read_gate` que `fs.list`. Un agente sin scope recibe `PolicyDenied`, jamás
/// un plan, y jamás la diferencia entre «ese fichero está» y «no está».
#[tokio::test]
async fn agente_sin_scope_no_puede_rename_batch_plan() {
    let d = spawn_daemon_policy().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/secreto.txt", b"x").await;
    let agent = connected_agent(&d, "s1").await;

    let err = agent
        .call::<_, methods::FsRenameBatchPlanResult>(
            methods::FS_RENAME_BATCH_PLAN,
            &methods::FsRenameBatchPlanParams {
                dir: vp("mem:///proj"),
                pairs: vec![pair(b"secreto.txt", b"otro.txt")],
            },
        )
        .await
        .expect_err("agente sin scope denegado");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
            "PolicyDenied out-of-scope, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }

    // Y el veredicto es el MISMO para un nombre que no existe: el error no
    // distingue lo que hay dentro del directorio de lo que no.
    let err = agent
        .call::<_, methods::FsRenameBatchPlanResult>(
            methods::FS_RENAME_BATCH_PLAN,
            &methods::FsRenameBatchPlanParams {
                dir: vp("mem:///proj"),
                pairs: vec![pair(b"no-existe.txt", b"otro.txt")],
            },
        )
        .await
        .expect_err("agente sin scope denegado");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
            "PolicyDenied out-of-scope, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// #80 en el gemelo que MUTA, que es donde el oráculo era peor: ejecutar
/// empieza por planificar, y el `plan_hash` es determinista y calculable
/// offline — sin este gate, un agente sin scope mandaría el hash de la
/// hipótesis «X existe» y distinguiría la denegación (existía) de `PlanStale`
/// (no existía), un bit exacto por petición. Con él, las CUATRO combinaciones
/// (directorio que está / que no está, hash que casa / que no) contestan lo
/// mismo.
#[tokio::test]
async fn agente_sin_scope_no_puede_rename_batch_ni_como_oraculo() {
    let d = spawn_daemon_policy().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/secreto.txt", b"x").await;
    let agent = connected_agent(&d, "s1").await;

    let ceros = methods::PlanHash::parse(&"0".repeat(64)).expect("hash");
    let unos = methods::PlanHash::parse(&"1".repeat(64)).expect("hash");
    let casos = [
        // (directorio, nombre de origen): existe/no existe, en las dos
        // combinaciones que el oráculo usaría para separar los mundos.
        (vp("mem:///proj"), b"secreto.txt".to_vec()),
        (vp("mem:///proj"), b"no-existe.txt".to_vec()),
        (vp("mem:///no-hay"), b"secreto.txt".to_vec()),
    ];
    let mut respuestas = Vec::new();
    for (dir, from) in casos {
        for hash in [&ceros, &unos] {
            let err = agent
                .call::<_, FsTaskResult>(
                    methods::FS_RENAME_BATCH,
                    &methods::FsRenameBatchParams {
                        dir: dir.clone(),
                        pairs: vec![pair(&from, b"otro.txt")],
                        plan_hash: hash.clone(),
                    },
                )
                .await
                .expect_err("agente sin scope denegado");
            match err {
                ClientError::Rpc(rpc) => {
                    assert!(
                        matches!(rpc.data, Some(Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
                        "PolicyDenied out-of-scope, fue {:?}",
                        rpc.data
                    );
                    respuestas.push((rpc.code, rpc.message));
                }
                other => panic!("esperaba Rpc, fue {other:?}"),
            }
        }
    }
    assert!(
        respuestas.windows(2).all(|w| w[0] == w[1]),
        "las seis respuestas tienen que ser LA MISMA: {respuestas:?}",
    );
    // Y no se tocó nada.
    assert!(d.mem.stat(&vp("mem:///proj/secreto.txt")).await.is_ok());
}

/// Un agente puede LEER los dos árboles (su scope los cubre: `covers_read` es
/// membresía de raíz, sin mirar la op) y por tanto puede PLANIFICAR — pero
/// aplicar escribe, y su scope no trae `copy`. El gate corre sobre las raíces
/// que salen del plan, al aplicar, y deniega.
///
/// Que planifique y no pueda aplicar es exactamente el reparto que se busca: el
/// plan no muta nada, la aplicación sí.
#[tokio::test]
async fn aplicar_sin_permiso_de_escritura_sobre_el_destino_se_deniega() {
    let d = spawn_daemon_journal().await;
    d.mem.mkdir(&vp("mem:///s")).await.expect("mkdir s");
    d.mem.mkdir(&vp("mem:///d")).await.expect("mkdir d");
    write_file(&d.mem, "mem:///s/a.txt", b"aaa").await;

    let human = connected_client(&d).await;
    let mut agent = connected_agent(&d, "s-sync").await;
    // Scope sobre la raíz de los dos árboles, pero SOLO para `mkdir`: leer entra
    // (la lectura es membresía de raíz), copiar no.
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
        .expect("planificar es leer, y leer sí puede");
    let (_batches, done, state) = drain_sync(&mut agent, task.task_id.get()).await;
    assert_eq!(state, TaskState::Completed);
    let done = done.expect("el plan cerró");

    let err = agent
        .call::<_, FsTaskResult>(
            methods::SYNC_APPLY,
            &methods::SyncApplyParams {
                plan_hash: done.plan_hash,
            },
        )
        .await
        .expect_err("escribir no está en su scope");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
            "PolicyDenied out-of-scope, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
    // Y el destino sigue vacío: el gate corre ANTES de tocar un byte.
    assert!(
        d.mem.stat(&vp("mem:///d/a.txt")).await.is_err(),
        "un plan denegado no escribe"
    );
}

/// Un agente no tiene pantalla que guardar: `session.*` es `INVALID_REQUEST`,
/// el mismo criterio que `daemon.shutdown` y `policy.pending`.
#[tokio::test]
async fn session_es_de_humanos() {
    let d = spawn_daemon(None).await;
    let agente = connected_agent(&d, "a1").await;
    let err = agente
        .call::<_, methods::SessionGetResult>(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect_err("un agente no lee la pantalla de nadie");
    assert_rpc_code(&err, codes::INVALID_REQUEST);
    let err = agente
        .call::<_, methods::SessionPutResult>(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: 0,
                body: serde_json::json!({}),
            },
        )
        .await
        .expect_err("ni la escribe");
    assert_rpc_code(&err, codes::INVALID_REQUEST);
}

/// Un agente no lee el registro del daemon: lleva rutas, nombres de conexión y
/// actividad de OTRAS sesiones, o sea un oráculo de existencia fuera de su
/// scope. Y se le dice que está vedado, no que está vacío.
///
/// Con anillo montado a propósito: así lo que refusa es el gate de actor y no
/// la ausencia de registro, que contestaría otra cosa.
#[tokio::test]
async fn un_agente_no_lee_el_registro() {
    let (d, anillo) = spawn_daemon_con_anillo().await;
    let _guard = hacia_el_anillo(&anillo);
    let agent = connected_agent(&d, "a1").await;

    let vedado = |err: ClientError| match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(
                rpc.data,
                Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved"
            ),
            "vedado, no vacío ni mal-formado: {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    };

    // Y pase lo que pase con los params: el gate corre ANTES del parseo, así
    // que un agente no distingue «vedado» de «params malos» fuzzeando su
    // propia petición — ni siquiera el `max: 0` que a un humano le daría
    // INVALID_PARAMS.
    for params in [
        serde_json::json!({ "cursor": null, "max": 10 }),
        serde_json::json!({ "max": 0 }),
        serde_json::json!({ "algo": "que no existe" }),
        serde_json::Value::Null,
    ] {
        vedado(
            agent
                .call::<_, methods::LogTailResult>(methods::LOG_TAIL, &params)
                .await
                .expect_err("un agente no lee el registro"),
        );
    }
    vedado(
        agent
            .call::<_, methods::LogLevelResult>(
                methods::LOG_LEVEL,
                &serde_json::json!({ "level": "debug" }),
            )
            .await
            .expect_err("un agente no sube la verbosidad de un trabajo ajeno"),
    );
}

/// Un agente no lee la línea de tiempo NI deshace hasta un punto (fase 7).
///
/// El journal es la lista completa de lo que se ha tocado en la máquina, con
/// origen y destino: para un agente con scope acotado es un oráculo de
/// existencia sobre todo lo que hay fuera de su recinto, y además le enseña
/// lo que hicieron las demás sesiones. Y `undo_after` revierte trabajo del
/// HUMANO — un agente que pudiera pedirlo borraría la huella de lo suyo.
///
/// Se comprueba lo mismo que en el registro y por el mismo motivo: que está
/// VEDADO y no vacío, y que el gate corre ANTES del parseo, para que un
/// agente no pueda distinguir «prohibido» de «params malos» probando formas.
#[tokio::test]
async fn un_agente_no_lee_la_linea_de_tiempo_ni_deshace() {
    let d = spawn_daemon(None).await;
    let agent = connected_agent(&d, "a1").await;

    let vedado = |err: ClientError| match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(
                rpc.data,
                Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved"
            ),
            "vedado, no vacío ni mal-formado: {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    };

    for params in [
        serde_json::json!({ "limit": 10 }),
        serde_json::json!({ "limit": 0 }),
        serde_json::json!({ "before_seq": 3, "limit": 5, "actor_kind": "user" }),
        serde_json::json!({ "algo": "que no existe" }),
        serde_json::Value::Null,
    ] {
        vedado(
            agent
                .call::<_, methods::JournalListResult>(methods::JOURNAL_LIST, &params)
                .await
                .expect_err("un agente no lee el journal"),
        );
    }

    for params in [
        serde_json::json!({ "seq": 1 }),
        serde_json::json!({ "seq": -1 }),
        serde_json::Value::Null,
    ] {
        vedado(
            agent
                .call::<_, methods::PolicyUndoSessionResult>(methods::JOURNAL_UNDO_AFTER, &params)
                .await
                .expect_err("un agente no deshace el trabajo del humano"),
        );
    }
}

/// #294 — el SDK RETIENE la versión que el peer declaró en el handshake.
///
/// Sin ella un cliente no puede saber que la comprobación que acaba de pedir
/// no se hizo: manda `expected_digest` (#282), un daemon viejo lo ignora como
/// manda ADR 0004, concede sin comprobar, y nada se lo dice. El
/// `InitializeResult` se tiraba, que es una respuesta ya pagada.
#[tokio::test]
async fn el_sdk_retiene_la_version_del_peer() {
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
    .expect("conecta");

    assert_eq!(
        backend.peer_protocol_version().as_deref(),
        Some(norte_proto::PROTOCOL_VERSION),
        "la versión del handshake es la que el daemon declara"
    );

    // Y con ella el ancla SÍ se manda: este daemon la entiende.
    let list = backend.plugins_list().await.expect("plugin.list");
    let ancla = list.plugins[0]
        .manifest_digest
        .clone()
        .expect("el catálogo trae el ancla");
    backend
        .plugins_set_approval("org.norte.demo", true, Some(&ancla))
        .await
        .expect("un peer 0.53 comprueba el ancla y concede");
}
