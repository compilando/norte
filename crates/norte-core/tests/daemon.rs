//! Matriz test-first de la fase 2 de M2: daemon UDS de verdad (socket en
//! tempdir) — handshake, dispatch, broadcast, cancelación, shutdown,
//! seguridad del socket. Todo contra `MemProvider` (spec §12).
#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use norte_core::approval::DenyAll;
use norte_core::daemon::{
    Client, ClientError, Daemon, DaemonApprovalResolver, DaemonConfig, DaemonError,
};
use norte_core::{Engine, PolicyConfig, ScopeRegistry, ScopedPolicy};
use norte_proto::methods::{
    self, ClientInfo, DaemonShutdownParams, DaemonShutdownResult, FsCopyParams, FsListParams,
    FsListResult, FsStatParams, FsStatResult, FsTaskResult, GrantScopeParams, GrantScopeResult,
    InitializeParams, PolicyApprovalRequired, PolicyDecideParams, PolicyDecideResult,
    PolicyPendingResult, RequestScopeParams, RequestScopeResult, TaskCancelParams,
    TaskCancelResult,
};
use norte_proto::wire::codes;
use norte_proto::{TaskProgress, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido de test")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write abre");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk entra");
    sink.commit().await.expect("commit publica");
}

fn client_info() -> ClientInfo {
    ClientInfo {
        name: "test".into(),
        version: "0.0.0".into(),
    }
}

/// Daemon vivo sobre un socket en tempdir. Devuelve también el `JoinHandle`
/// de `run()` para poder esperar el apagado.
struct TestDaemon {
    socket: PathBuf,
    run: tokio::task::JoinHandle<Result<(), DaemonError>>,
    _dir: tempfile::TempDir,
    mem: Arc<MemProvider>,
}

async fn spawn_daemon(idle: Option<Duration>) -> TestDaemon {
    spawn_daemon_ttl(idle, Duration::from_mins(2)).await
}

async fn spawn_daemon_ttl(idle: Option<Duration>, listing_ttl: Duration) -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let engine = Arc::new(Engine::new());
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let daemon = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: idle,
            listing_ttl,
            plugins_dir: None,
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

async fn connected_client(d: &TestDaemon) -> Client {
    let mut c = Client::connect(&d.socket).await.expect("connect");
    let init = c.initialize(client_info()).await.expect("initialize");
    assert_eq!(init.protocol_version, methods::PROTOCOL_VERSION);
    assert_eq!(init.encodings, vec!["json".to_string()]);
    c
}

/// Daemon con `ScopedPolicy` instalada: registro de scopes VACÍO al arrancar
/// (concedible por el wire, `policy.grant_scope`) + una regla `allow` (dentro
/// de scope se permite). Un `User` pasa (no se sandboxea); un `Agent` sin scope
/// se deniega por frontera antes de mirar reglas. El registro se COMPARTE entre
/// el `ScopedPolicy` del engine y el `Shared` del daemon (M3-3b).
async fn spawn_daemon_policy() -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let scopes = ScopeRegistry::new();
    let cfg = PolicyConfig::parse("[[rule]]\naction=\"allow\"").expect("policy cfg");
    let policy = ScopedPolicy::new(scopes.clone(), cfg);
    let engine = Arc::new(Engine::new().with_policy(Arc::new(policy), Arc::new(DenyAll)));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let daemon = Daemon::bind_with_scopes(
        engine,
        scopes,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: None,
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

/// Daemon con `ScopedPolicy` y regla `ask` (M3-3b Task 4): dentro de scope, el
/// gate suspende la mutación en el router de aprobaciones — el MISMO `Arc` que
/// recibe `policy.decide` por el wire. TTL de aprobación configurable (los
/// tests de timeout usan uno corto).
async fn spawn_daemon_ask(approval_ttl: Duration) -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let scopes = ScopeRegistry::new();
    let cfg = PolicyConfig::parse("[[rule]]\naction=\"ask\"").expect("policy cfg");
    let policy = ScopedPolicy::new(scopes.clone(), cfg);
    // Orden del plan (riesgo 1): el resolver nace ANTES que engine y daemon.
    let approvals = Arc::new(DaemonApprovalResolver::new(approval_ttl));
    let engine = Arc::new(Engine::new().with_policy(Arc::new(policy), Arc::clone(&approvals) as _));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let daemon = Daemon::bind_with_policy(
        engine,
        scopes,
        approvals,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: None,
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

/// Abre una conexión que declara `agent_session`: el servidor la liga a un
/// `Actor::Agent` y sandboxea sus mutaciones (M3-3b).
async fn connected_agent(d: &TestDaemon, session: &str) -> Client {
    let c = Client::connect(&d.socket).await.expect("connect");
    let _init: methods::InitializeResult = c
        .call(
            methods::INITIALIZE,
            &InitializeParams {
                client_info: client_info(),
                protocol_version: methods::PROTOCOL_VERSION.into(),
                encodings: vec!["json".into()],
                agent_session: Some(session.into()),
            },
        )
        .await
        .expect("initialize agente");
    c
}

// ---------- handshake ----------

#[tokio::test]
async fn initialize_negocia_y_es_obligatorio() {
    let d = spawn_daemon(None).await;
    // Sin initialize: cualquier método es NOT_INITIALIZED.
    let c = Client::connect(&d.socket).await.expect("connect");
    let err = c
        .call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: None,
                cursor: None,
            },
        )
        .await
        .expect_err("initialize primero");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::NOT_INITIALIZED),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
    // Con initialize, funciona.
    let _c2 = connected_client(&d).await;
}

#[tokio::test]
async fn initialize_rechaza_version_incompatible() {
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
        .expect_err("0.1.0 no es N ni N-1 de 0.13.0");
    match err {
        ClientError::Rpc(rpc) => {
            // Código PROPIO: la señal de upgrade jamás se parsea de message.
            assert_eq!(rpc.code, codes::VERSION_MISMATCH);
            assert!(norte_core::daemon::is_version_mismatch(&ClientError::Rpc(
                rpc
            )));
        }
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
    // N-1 (0.12.x) SÍ entra.
    let c2 = Client::connect(&d.socket).await.expect("connect");
    let ok: methods::InitializeResult = c2
        .call(
            methods::INITIALIZE,
            &InitializeParams {
                client_info: client_info(),
                protocol_version: "0.12.2".into(),
                encodings: vec![],
                agent_session: None,
            },
        )
        .await
        .expect("N-1 aceptado");
    assert_eq!(ok.protocol_version, methods::PROTOCOL_VERSION);
}

#[tokio::test]
async fn initialize_rechaza_encoding_desconocido() {
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
        .expect_err("solo json en M2");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
}

// ---------- dispatch fs.* ----------

#[tokio::test]
async fn fs_list_y_stat_responden_por_el_socket() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///f.txt", b"hola").await;
    let c = connected_client(&d).await;

    let list: FsListResult = c
        .call(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: None,
                cursor: None,
            },
        )
        .await
        .expect("fs.list");
    assert_eq!(list.entries.len(), 1);

    let stat: FsStatResult = c
        .call(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("mem:///f.txt"),
            },
        )
        .await
        .expect("fs.stat");
    assert_eq!(stat.entry.size, Some(4));
}

// ---------- paginación de fs.list (ADR 0017) ----------

/// Una página de `fs.list` con `limit`/`cursor`.
async fn list_page(
    c: &Client,
    path: &str,
    limit: Option<u32>,
    cursor: Option<String>,
) -> FsListResult {
    c.call(
        methods::FS_LIST,
        &FsListParams {
            path: vp(path),
            limit,
            cursor,
        },
    )
    .await
    .expect("fs.list")
}

async fn seed(mem: &MemProvider, n: usize) {
    for i in 0..n {
        write_file(mem, &format!("mem:///f{i:03}.txt"), b"x").await;
    }
}

/// Paginar por cursor devuelve EXACTAMENTE las mismas entradas que el listado
/// completo, sin duplicar ni perder.
#[tokio::test]
async fn fs_list_paginado_concatena_igual_que_completo() {
    let d = spawn_daemon(None).await;
    seed(&d.mem, 5).await;
    let c = connected_client(&d).await;

    let completo = list_page(&c, "mem:///", None, None).await;
    assert_eq!(completo.entries.len(), 5);
    assert!(
        completo.next_cursor.is_none(),
        "sin cursor = listado completo"
    );

    // Páginas de 2.
    let mut acumulado = Vec::new();
    let mut cursor = None;
    loop {
        let page = list_page(&c, "mem:///", Some(2), cursor).await;
        assert!(page.entries.len() <= 2, "respeta el limit");
        acumulado.extend(page.entries);
        match page.next_cursor {
            Some(cur) => cursor = Some(cur),
            None => break,
        }
    }
    // Mismo conjunto de paths (el orden del provider puede variar).
    let mut a: Vec<_> = acumulado.iter().map(|e| e.path.clone()).collect();
    let mut b: Vec<_> = completo.entries.iter().map(|e| e.path.clone()).collect();
    a.sort();
    b.sort();
    assert_eq!(a, b, "paginado == completo");
}

/// `limit = 0` es error de params (evita páginas vacías en bucle).
#[tokio::test]
async fn fs_list_limit_cero_es_invalid_params() {
    let d = spawn_daemon(None).await;
    seed(&d.mem, 2).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: Some(0),
                cursor: None,
            },
        )
        .await
        .expect_err("limit 0");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
}

/// Un cursor desconocido (o no-numérico) → `CursorExpired`: el cliente reinicia.
#[tokio::test]
async fn fs_list_cursor_desconocido_es_cursor_expired() {
    let d = spawn_daemon(None).await;
    seed(&d.mem, 2).await;
    let c = connected_client(&d).await;
    for cur in ["999", "no-numerico"] {
        let err = c
            .call::<_, FsListResult>(
                methods::FS_LIST,
                &FsListParams {
                    path: vp("mem:///"),
                    limit: Some(1),
                    cursor: Some(cur.to_string()),
                },
            )
            .await
            .expect_err("cursor inválido");
        match err {
            ClientError::Rpc(rpc) => {
                assert_eq!(rpc.data, Some(norte_proto::Error::CursorExpired), "{cur}");
            }
            other => panic!("esperaba Rpc, fue {other:?}"),
        }
    }
}

/// Un cursor de OTRO path → `INVALID_PARAMS` (el cursor valida contra su dir).
#[tokio::test]
async fn fs_list_cursor_de_otro_path_es_invalid_params() {
    let d = spawn_daemon(None).await;
    seed(&d.mem, 3).await;
    let c = connected_client(&d).await;
    // Abre un listado de la raíz que RETIENE (limit 1, hay 3 entradas).
    let page = list_page(&c, "mem:///", Some(1), None).await;
    let cur = page.next_cursor.expect("retiene");
    // Continuarlo con OTRO path: el check de path va ANTES de listar, así que
    // el otro path ni siquiera necesita existir.
    let err = c
        .call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///otro"),
                limit: Some(1),
                cursor: Some(cur),
            },
        )
        .await
        .expect_err("cursor de otro path");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
}

/// LRU: al abrir el 9º listado retenido se expulsa el más viejo (su cursor →
/// `CursorExpired`).
#[tokio::test]
async fn fs_list_lru_expulsa_el_mas_viejo() {
    let d = spawn_daemon(None).await;
    seed(&d.mem, 3).await; // ≥2 para que cada limit=1 retenga
    let c = connected_client(&d).await;
    // Abre 9 listados (MAX_OPEN_LISTINGS = 8): el 9º expulsa el 1º.
    let mut cursores = Vec::new();
    for _ in 0..9 {
        let page = list_page(&c, "mem:///", Some(1), None).await;
        cursores.push(page.next_cursor.expect("retiene"));
    }
    // El primer cursor fue expulsado.
    let err = c
        .call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: Some(1),
                cursor: Some(cursores[0].clone()),
            },
        )
        .await
        .expect_err("el 1º fue expulsado");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.data, Some(norte_proto::Error::CursorExpired));
        }
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
    // El último sigue vivo.
    let ok = list_page(&c, "mem:///", Some(1), Some(cursores[8].clone())).await;
    assert!(!ok.entries.is_empty(), "el más reciente sobrevive");
}

/// TTL: un listado retenido sin continuar caduca (su cursor → `CursorExpired`).
#[tokio::test]
async fn fs_list_ttl_expira_el_listado() {
    let d = spawn_daemon_ttl(None, Duration::from_millis(150)).await;
    seed(&d.mem, 3).await;
    let c = connected_client(&d).await;
    let page = list_page(&c, "mem:///", Some(1), None).await;
    let cur = page.next_cursor.expect("retiene");
    tokio::time::sleep(Duration::from_millis(400)).await;
    let err = c
        .call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: Some(1),
                cursor: Some(cur),
            },
        )
        .await
        .expect_err("caducó por TTL");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.data, Some(norte_proto::Error::CursorExpired));
        }
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

#[tokio::test]
async fn fs_stat_de_inexistente_viaja_como_taxonomia_en_data() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, FsStatResult>(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("mem:///nada"),
            },
        )
        .await
        .expect_err("no existe");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert_eq!(rpc.data, Some(norte_proto::Error::NotFound));
        }
        other => panic!("esperaba Rpc con data, fue {other:?}"),
    }
}

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

/// M3-3b Task 3: round-trip de scope. Un agente pide (`request_scope`) — sin
/// concesión su copia dentro sigue denegada —; un humano concede
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
    // La limpieza es asíncrona del lado del daemon: margen generoso sobre UDS
    // local antes de comprobar (patrón de los otros tests con timing).
    tokio::time::sleep(Duration::from_millis(300)).await;
    let human = connected_client(&d).await;
    let err = human
        .call::<_, GrantScopeResult>(
            methods::POLICY_GRANT_SCOPE,
            &GrantScopeParams { request_id },
        )
        .await
        .expect_err("la pendiente no debía sobrevivir a su conexión");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
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

fn copy_params(from: &str, to: &str) -> FsCopyParams {
    FsCopyParams {
        from: vp(from),
        to: vp(to),
        on_collision: norte_proto::CollisionPolicy::default(),
        symlinks: norte_proto::SymlinkPolicy::default(),
        resume: norte_proto::ResumePolicy::default(),
        verify: norte_proto::VerifyPolicy::default(),
    }
}

/// Concede a la sesión del `agent` un scope de `copy` sobre `mem:///proj` con
/// el round-trip del wire (request + grant): el camino real, no un atajo.
async fn grant_copy_scope(agent: &Client, human: &Client, session: &str) {
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
async fn next_approval(human: &mut Client) -> PolicyApprovalRequired {
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

fn assert_not_approved(err: ClientError) {
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

    // Re-decidir el mismo id: la decisión lo consumió → INVALID_PARAMS.
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
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
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

    // Decidir un id desconocido → INVALID_PARAMS.
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
    assert!(matches!(err3, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));

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

/// `agent_session` se valida fail-closed en el handshake (encoding-auditor H1
/// de T5): controles, bidi, vacío o kilométrico → `INVALID_PARAMS`. El id
/// viaja a journal, logs y modales de aprobación — jamás lo elige libre el
/// agente.
#[tokio::test]
async fn agent_session_hostil_se_rechaza_en_initialize() {
    let d = spawn_daemon(None).await;
    let hostiles = [
        "s1\nmem:///fake",       // inyección de líneas
        "s1\u{202e}ypoc",        // override RTL
        "s1\u{1b}]0;pwned\u{7}", // OSC/ANSI
        "",                      // vacío
        &"a".repeat(65),         // demasiado largo
        "con espacios",          // fuera de charset
    ];
    for session in hostiles {
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
            .expect_err("sesión hostil rechazada");
        assert!(
            matches!(err, ClientError::Rpc(ref rpc) if rpc.code == codes::INVALID_PARAMS),
            "esperaba INVALID_PARAMS para {session:?}, fue {err:?}"
        );
    }
    // El charset legal completo pasa.
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
        .expect("sesión válida");
    assert_eq!(ok.protocol_version, methods::PROTOCOL_VERSION);
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

/// `connection.trust_host_key` (0.7.0, fase 6e) existe en el dispatch y
/// llega al engine: sin conector configurado responde la taxonomía
/// `Unsupported` por el wire — no `METHOD_NOT_FOUND` (eso significaría que
/// el handler falta).
#[tokio::test]
async fn trust_host_key_llega_al_engine() {
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
        .expect_err("sin conector: Unsupported");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert!(
                matches!(rpc.data, Some(norte_proto::Error::Unsupported)),
                "taxonomía Unsupported, fue {:?}",
                rpc.data
            );
        }
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

#[tokio::test]
async fn metodo_desconocido_es_method_not_found() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, serde_json::Value>("fs.inventado", &serde_json::json!({}))
        .await
        .expect_err("no existe el método");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::METHOD_NOT_FOUND));
}

// ---------- tasks: progreso, terminal, cancel, broadcast ----------

/// Espera notificaciones task.progress de una task hasta su estado
/// terminal; devuelve los snapshots vistos.
async fn drain_task(c: &mut Client, task_id: u64) -> Vec<TaskProgress> {
    let mut seen = Vec::new();
    loop {
        let n = tokio::time::timeout(Duration::from_secs(5), c.notification())
            .await
            .expect("notificación antes del timeout")
            .expect("conexión viva");
        assert_eq!(n.method, methods::TASK_PROGRESS);
        let p: TaskProgress =
            serde_json::from_value(n.params.expect("params")).expect("TaskProgress");
        if p.task_id.get() != task_id {
            continue;
        }
        let terminal = p.state.is_terminal();
        seen.push(p);
        if terminal {
            return seen;
        }
    }
}

#[tokio::test]
async fn fs_copy_progresa_hasta_completed() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///src.bin", &vec![0xAB; 5000]).await;
    let mut c = connected_client(&d).await;

    let task: FsTaskResult = c
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///src.bin"),
                to: vp("mem:///dst.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
            },
        )
        .await
        .expect("fs.copy");
    let seen = drain_task(&mut c, task.task_id.get()).await;
    let last = seen.last().expect("al menos el terminal");
    assert_eq!(last.state, TaskState::Completed);
    assert_eq!(last.bytes_done, 5000);
    let stat: FsStatResult = c
        .call(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("mem:///dst.bin"),
            },
        )
        .await
        .expect("el destino existe");
    assert_eq!(stat.entry.size, Some(5000));
}

#[tokio::test]
async fn task_cancel_por_el_socket_cancela_limpio() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///grande.bin", &vec![0xCD; 100_000]).await;
    // Latencia por op: da tiempo a cancelar en mitad.
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(30)));
    let mut c = connected_client(&d).await;

    let task: FsTaskResult = c
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///grande.bin"),
                to: vp("mem:///copia.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
            },
        )
        .await
        .expect("fs.copy");
    let _: TaskCancelResult = c
        .call(
            methods::TASK_CANCEL,
            &TaskCancelParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("task.cancel");
    let seen = drain_task(&mut c, task.task_id.get()).await;
    assert_eq!(
        seen.last().expect("terminal").state,
        TaskState::Cancelled,
        "cancelación cooperativa confirmada por notificación"
    );
}

/// La base de la fase 3: un SEGUNDO cliente ve el progreso de las tasks
/// que encoló el primero.
#[tokio::test]
async fn el_progreso_se_difunde_a_todos_los_clientes() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///src.bin", &vec![0xEE; 2000]).await;
    let c1 = connected_client(&d).await;
    let mut c2 = connected_client(&d).await;

    let task: FsTaskResult = c1
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///src.bin"),
                to: vp("mem:///dst.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
            },
        )
        .await
        .expect("fs.copy de c1");
    // c2 no pidió nada — y aun así ve la task de c1 hasta el terminal.
    let seen = drain_task(&mut c2, task.task_id.get()).await;
    assert_eq!(seen.last().expect("terminal").state, TaskState::Completed);
}

// ---------- lifecycle ----------

#[tokio::test]
async fn daemon_shutdown_graceful_espera_y_apaga() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let _: DaemonShutdownResult = c
        .call(
            methods::DAEMON_SHUTDOWN,
            &DaemonShutdownParams { graceful: true },
        )
        .await
        .expect("shutdown aceptado");
    let joined = tokio::time::timeout(Duration::from_secs(5), d.run)
        .await
        .expect("run() termina")
        .expect("join limpio");
    joined.expect("apagado sin error");
    assert!(!d.socket.exists(), "el socket se retira del FS");
}

#[tokio::test]
async fn daemon_se_apaga_solo_por_inactividad() {
    // Idle holgado (1,2 s): una CI congelada no puede apagar el daemon
    // antes de que el cliente llegue a conectar (m3 del rust-reviewer).
    let d = spawn_daemon(Some(Duration::from_millis(1200))).await;
    {
        // Una conexión breve: mientras vive, no hay apagado.
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
            plugins_dir: None,
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
            plugins_dir: None,
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

async fn spawn_daemon_at(socket: PathBuf) -> TestDaemon {
    // El tempdir padre lo posee el caller; aquí un guard vacío.
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
            plugins_dir: None,
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

// ---------- protocolo crudo (frames hostiles) ----------

/// Lee UNA línea completa del stream (UDS es stream: un read puede ser
/// parcial — m4 del rust-reviewer).
async fn read_frame(s: &mut tokio::net::UnixStream) -> serde_json::Value {
    use tokio::io::AsyncReadExt;
    let mut decoder = norte_proto::wire::FrameDecoder::new();
    let mut buf = vec![0u8; 4096];
    loop {
        if let Some(frame) = decoder.next_frame() {
            return serde_json::from_slice(&frame).expect("respuesta JSON");
        }
        let n = tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf))
            .await
            .expect("respuesta antes del timeout")
            .expect("read");
        assert_ne!(n, 0, "conexión cerrada esperando respuesta");
        decoder.push(&buf[..n]).expect("frame razonable");
    }
}

/// Frames hostiles directamente sobre el socket, sin el Client: JSON roto
/// = -32700; JSON válido que no es envelope = -32600; request con id de
/// tipo ilegal = -32600 (JAMÁS silencio); params null en daemon.shutdown
/// (la forma canónica del golden) funciona.
#[tokio::test]
async fn frames_hostiles_y_formas_canonicas_crudas() {
    use tokio::io::AsyncWriteExt;
    let d = spawn_daemon(None).await;

    let mut s = tokio::net::UnixStream::connect(&d.socket)
        .await
        .expect("connect crudo");

    s.write_all(b"esto no es json\n").await.expect("write");
    let resp = read_frame(&mut s).await;
    assert_eq!(resp["error"]["code"], serde_json::json!(-32700));
    assert_eq!(resp["id"], serde_json::Value::Null);

    // JSON válido, envelope inválido: -32600, no -32700 (M2 del guardian).
    s.write_all(b"{\"foo\":1}\n").await.expect("write");
    let resp = read_frame(&mut s).await;
    assert_eq!(resp["error"]["code"], serde_json::json!(-32600));

    // id ilegal (negativo): -32600, jamás tragado como notification (M3).
    s.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":-1,\"method\":\"fs.stat\",\"params\":null}\n")
        .await
        .expect("write");
    let resp = read_frame(&mut s).await;
    assert_eq!(resp["error"]["code"], serde_json::json!(-32600));

    // initialize + daemon.shutdown con params null (golden canónico, M1).
    s.write_all(
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"client_info\":{\"name\":\"raw\",\"version\":\"0\"},\"protocol_version\":\"0.12.0\",\"encodings\":[\"json\"]}}\n",
    )
    .await
    .expect("write");
    let resp = read_frame(&mut s).await;
    assert!(resp["result"]["protocol_version"].is_string(), "{resp}");
    s.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"daemon.shutdown\",\"params\":null}\n")
        .await
        .expect("write");
    let resp = read_frame(&mut s).await;
    assert!(resp["error"].is_null(), "params null aceptado: {resp}");
}

/// initialize repetido = error de protocolo (decisión pinneada, m4 del
/// guardian) — y la conexión sigue viva y usable.
#[tokio::test]
async fn initialize_repetido_es_invalid_request() {
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
        .expect_err("re-initialize rechazado");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));
    let _: FsListResult = c
        .call(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: None,
                cursor: None,
            },
        )
        .await
        .expect("la conexión sigue viva");
}

/// B1 del rust-reviewer: una `call()` DESPUÉS de morir la conexión falla
/// con `ConnectionClosed` en vez de colgarse para siempre.
#[tokio::test]
async fn call_tras_el_cierre_no_se_cuelga() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    // Apagar el daemon deja la conexión muerta.
    let _: DaemonShutdownResult = c
        .call(
            methods::DAEMON_SHUTDOWN,
            &DaemonShutdownParams { graceful: true },
        )
        .await
        .expect("shutdown");
    tokio::time::timeout(Duration::from_secs(5), d.run)
        .await
        .expect("apagado")
        .expect("join")
        .expect("run ok");
    // Dar tiempo a que el reader del cliente vea el EOF.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let err = tokio::time::timeout(
        Duration::from_secs(5),
        c.call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: None,
                cursor: None,
            },
        ),
    )
    .await
    .expect("responde, JAMÁS se cuelga")
    .expect_err("la conexión está muerta");
    assert!(
        matches!(err, ClientError::ConnectionClosed | ClientError::Io(_)),
        "{err:?}"
    );
}

// ---------- métodos 0.5.0 (fase 3) ----------

/// `task.list` devuelve el snapshot de las tasks VIVAS: el resync de un
/// frontend que se conecta tarde.
#[tokio::test]
async fn task_list_da_el_snapshot_de_tasks_vivas() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///grande.bin", &vec![0xAB; 50_000]).await;
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(20)));
    let c1 = connected_client(&d).await;
    let task: FsTaskResult = c1
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///grande.bin"),
                to: vp("mem:///copia.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
            },
        )
        .await
        .expect("fs.copy");
    // Cliente TARDÍO: ve la task del primero por task.list.
    let mut c2 = connected_client(&d).await;
    let list: methods::TaskListResult = c2
        .call(methods::TASK_LIST, &methods::TaskListParams {})
        .await
        .expect("task.list");
    assert!(
        list.tasks.iter().any(|t| t.task_id == task.task_id),
        "la task viva del otro cliente aparece: {list:?}"
    );
    // Y sigue viéndola progresar hasta el terminal por broadcast.
    let seen = drain_task(&mut c2, task.task_id.get()).await;
    assert_eq!(seen.last().expect("terminal").state, TaskState::Completed);
}

/// `fs.read` con rango: bytes exactos en base64 y eof honesto.
#[tokio::test]
async fn fs_read_devuelve_tramos_con_eof_honesto() {
    use base64::Engine as _;
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///f.bin", b"0123456789").await;
    let c = connected_client(&d).await;

    let r: methods::FsReadResult = c
        .call(
            methods::FS_READ,
            &methods::FsReadParams {
                path: vp("mem:///f.bin"),
                range: Some(norte_proto::ByteRange {
                    offset: 2,
                    len: Some(3),
                }),
            },
        )
        .await
        .expect("fs.read");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&r.content_b64)
        .expect("base64 válido");
    assert_eq!(bytes, b"234");
    assert!(!r.eof, "quedan bytes detrás del tramo");

    // Tramo hasta el final: eof true.
    let r: methods::FsReadResult = c
        .call(
            methods::FS_READ,
            &methods::FsReadParams {
                path: vp("mem:///f.bin"),
                range: Some(norte_proto::ByteRange {
                    offset: 5,
                    len: Some(100),
                }),
            },
        )
        .await
        .expect("fs.read");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&r.content_b64)
        .expect("base64 válido");
    assert_eq!(bytes, b"56789");
    assert!(r.eof);

    // Sin rango: el archivo entero (cabe de sobra en el tope).
    let r: methods::FsReadResult = c
        .call(
            methods::FS_READ,
            &methods::FsReadParams {
                path: vp("mem:///f.bin"),
                range: None,
            },
        )
        .await
        .expect("fs.read");
    assert!(r.eof);
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(&r.content_b64)
            .expect("base64"),
        b"0123456789"
    );
}

/// `fs.capabilities`: el frontend decide (F8 papelera, ADR 0009) sin
/// lógica propia.
#[tokio::test]
async fn fs_capabilities_viaja_por_el_socket() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let r: methods::FsCapabilitiesResult = c
        .call(
            methods::FS_CAPABILITIES,
            &methods::FsCapabilitiesParams {
                path: vp("mem:///"),
            },
        )
        .await
        .expect("fs.capabilities");
    assert!(
        r.capabilities
            .flags
            .contains(norte_proto::CapabilityFlags::TRASH),
        "MemProvider declara TRASH: {:?}",
        r.capabilities.flags
    );
}

// ---------- M3-4 T4: journal en el daemon + policy.undo_session ----------

/// Daemon con JOURNAL (in-memory) + `ScopedPolicy` con regla `allow` + registro
/// de scopes compartido — el escenario del daemon real de M3-4 (dueño único
/// del journal, ADR 0024).
async fn spawn_daemon_journal() -> TestDaemon {
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
    let daemon = Daemon::bind_with_scopes(
        engine,
        scopes,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: None,
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
async fn wait_terminal(c: &Client, task_id: norte_proto::TaskId) -> norte_proto::TaskState {
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

// ---------- plugin.* (M4-P3) ----------

/// Manifiesto válido mínimo (mismo del test de `norte_core::plugins`).
const DEMO_MANIFEST: &str = r#"
[plugin]
id = "org.norte.demo"
name = "Demo"
publisher = "norte"
version = "0.1.0"
category = "command"
[capabilities]
fs-read = "scoped"
"#;

/// Daemon con `plugins_dir` apuntando a un tempdir SEMBRADO con un plugin
/// descubrible (`plugins/org.norte.demo/plugin.toml`). JAMÁS toca el
/// `~/.config` real: el `plugins_dir` explícito aísla el estado del test.
async fn spawn_daemon_plugins() -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    // Raíz de plugins DENTRO del mismo tempdir (se limpia con `_dir`).
    let plugins_root = dir.path().join("cfg");
    let plugin_dir = plugins_root.join("plugins").join("org.norte.demo");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin");
    std::fs::write(plugin_dir.join("plugin.toml"), DEMO_MANIFEST).expect("write manifest");

    let engine = Arc::new(Engine::new());
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let daemon = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: Some(plugins_root),
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

/// `plugin.list` por el socket ve el plugin sembrado, nace sin aprobar/activar.
#[tokio::test]
async fn plugin_list_ve_el_catalogo_sembrado() {
    let d = spawn_daemon_plugins().await;
    let c = connected_client(&d).await;
    let list: methods::PluginListResult = c
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list");
    assert_eq!(list.plugins.len(), 1, "el plugin sembrado se descubre");
    let p = &list.plugins[0];
    assert_eq!(p.id, "org.norte.demo");
    assert!(!p.approved, "nace sin aprobar");
    assert!(!p.enabled, "nace sin activar");
    assert!(list.errors.is_empty());
}

/// Un HUMANO aprueba por el socket; `plugin.list` lo refleja (y persistió, así
/// que una NUEVA conexión también lo ve aprobado).
#[tokio::test]
async fn plugin_set_approval_humano_se_refleja_y_persiste() {
    let d = spawn_daemon_plugins().await;
    let human = connected_client(&d).await;
    let _: methods::PluginSetApprovalResult = human
        .call(
            methods::PLUGIN_SET_APPROVAL,
            &methods::PluginSetApprovalParams {
                id: "org.norte.demo".into(),
                approved: true,
            },
        )
        .await
        .expect("aprobación aceptada");

    let list: methods::PluginListResult = human
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list tras aprobar");
    assert!(list.plugins[0].approved, "la aprobación se refleja");

    // Una conexión NUEVA lee el estado persistido (mismo daemon, mismo dir).
    let otra = connected_client(&d).await;
    let list2: methods::PluginListResult = otra
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list en otra conexión");
    assert!(list2.plugins[0].approved, "la aprobación persistió");
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
            },
        )
        .await
        .expect_err("id desconocido");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
}
