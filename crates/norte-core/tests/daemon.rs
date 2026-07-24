//! Matriz test-first de la fase 2 de M2: daemon UDS de verdad (socket en
//! tempdir) — handshake, dispatch, broadcast, cancelación, shutdown,
//! seguridad del socket. Todo contra `MemProvider` (spec §12).
#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use norte_core::approval::DenyAll;
use norte_core::daemon::{
    Client, ClientError, Daemon, DaemonApprovalResolver, DaemonConfig, DaemonError,
};
use norte_core::{Engine, PolicyConfig, ScopeRegistry, ScopedPolicy};
use norte_proto::methods::{
    self, ClientInfo, ConnectionDegraded, DaemonShutdownParams, DaemonShutdownResult, FsCopyParams,
    FsListParams, FsListResult, FsSearchParams, FsStatParams, FsStatResult, FsTaskResult,
    GrantScopeParams, GrantScopeResult, InitializeParams, PolicyApprovalRequired,
    PolicyDecideParams, PolicyDecideResult, PolicyPendingResult, RequestScopeParams,
    RequestScopeResult, SearchHits, TaskCancelParams, TaskCancelResult,
};
use norte_proto::wire::codes;
use norte_proto::{Entry, EntryKind, Error, TaskProgress, TaskState, VPath};
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
        .expect_err("0.1.0 no es N ni N-1 de 0.27.0");
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
    // N-1 (0.26.x) SÍ entra.
    let c2 = Client::connect(&d.socket).await.expect("connect");
    let ok: methods::InitializeResult = c2
        .call(
            methods::INITIALIZE,
            &InitializeParams {
                client_info: client_info(),
                protocol_version: "0.26.2".into(),
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

#[tokio::test]
async fn call_tracked_reporta_el_id_asignado() {
    let d = spawn_daemon(None).await;
    let client = connected_client(&d).await;
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
    let s = std::sync::Arc::clone(&seen);
    // fs.stat de un path inexistente: da igual el desenlace (Err), lo que se
    // comprueba es que on_id se invocó exactamente una vez con un id > 0.
    let _res: Result<norte_proto::methods::FsStatResult, _> = client
        .call_tracked(
            norte_proto::methods::FS_STAT,
            &norte_proto::methods::FsStatParams {
                path: vp("mem:///nope"),
            },
            move |id| s.lock().expect("lock").push(id),
        )
        .await;
    let seen = seen.lock().expect("lock");
    assert_eq!(seen.len(), 1, "on_id se invoca exactamente una vez");
    assert!(seen[0] > 0, "id asignado > 0");
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

/// #93: `skipped` (omitidas del contenedor) viaja en el result de `fs.list` y
/// se REPITE en cada página (el cliente puede engancharse en cualquiera). Con
/// un provider normal (sin omitidas) el campo va ausente (`None`).
#[tokio::test]
async fn fs_list_skipped_viaja_en_todas_las_paginas() {
    // Daemon con un MemProvider que simula un contenedor con 7 omitidas.
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let engine = Arc::new(Engine::new());
    let mem = Arc::new(MemProvider::new().with_list_skipped(7));
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let daemon = Daemon::bind(
        engine,
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
    let d = TestDaemon {
        socket,
        run,
        _dir: dir,
        mem,
    };
    seed(&d.mem, 5).await;
    let c = connected_client(&d).await;

    // Listado completo (sin cursor): lo lleva.
    let completo = list_page(&c, "mem:///", None, None).await;
    assert_eq!(completo.skipped, Some(7));

    // Paginado: TODAS las páginas lo repiten (primera, intermedias y última).
    let mut cursor = None;
    let mut paginas = 0;
    loop {
        let page = list_page(&c, "mem:///", Some(2), cursor).await;
        assert_eq!(page.skipped, Some(7), "página {paginas}");
        paginas += 1;
        match page.next_cursor {
            Some(cur) => cursor = Some(cur),
            None => break,
        }
    }
    assert!(paginas >= 3, "hubo continuaciones de verdad");

    // Provider sin omitidas (spawn normal): el campo va ausente.
    let d2 = spawn_daemon(None).await;
    seed(&d2.mem, 1).await;
    let c2 = connected_client(&d2).await;
    assert_eq!(list_page(&c2, "mem:///", None, None).await.skipped, None);
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
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"client_info\":{\"name\":\"raw\",\"version\":\"0\"},\"protocol_version\":\"0.26.0\",\"encodings\":[\"json\"]}}\n",
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

/// `plugin.run_command` de un plugin SIN aprobar es `INVALID_REQUEST` y NO lo
/// ejecuta (fail-closed): el humano no ha consentido, así que el runtime no
/// arranca. El demo sembrado nace sin aprobar/activar (M4-P4). El caso de éxito
/// con un `.wasm` real es E2E de la task siguiente.
#[tokio::test]
async fn plugin_run_command_sin_aprobar_es_invalid_request() {
    let d = spawn_daemon_plugins().await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::PluginRunCommandResult>(
            methods::PLUGIN_RUN_COMMAND,
            &methods::PluginRunCommandParams {
                id: "org.norte.demo".into(),
                command: "echo".into(),
                arg: "hola".into(),
            },
        )
        .await
        .expect_err("un plugin sin aprobar jamás se ejecuta");
    assert!(
        matches!(err, ClientError::Rpc(ref rpc) if rpc.code == codes::INVALID_REQUEST),
        "sin aprobar = INVALID_REQUEST, no se ejecuta: {err:?}"
    );
}

/// `plugin.run_command` de un id DESCONOCIDO es `INVALID_PARAMS` (el cliente
/// pidió un plugin que no existe): no se ejecuta ni se filtra nada.
#[tokio::test]
async fn plugin_run_command_id_desconocido_es_invalid_params() {
    let d = spawn_daemon_plugins().await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::PluginRunCommandResult>(
            methods::PLUGIN_RUN_COMMAND,
            &methods::PluginRunCommandParams {
                id: "org.norte.fantasma".into(),
                command: "echo".into(),
                arg: String::new(),
            },
        )
        .await
        .expect_err("id desconocido");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
}

/// `plugin.preview` de un archivo cuando NO hay ningún previewer instalado
/// (registro vacío, `plugins_dir: None`) devuelve `preview: None` — NO un
/// error: ningún previewer consentido casa el mimetype, así que el frontend cae
/// a la vista cruda. Ni siquiera se leen los bytes del archivo (la resolución
/// falla antes). El caso con un previewer `.wasm` real es E2E de la task
/// siguiente.
#[tokio::test]
async fn plugin_preview_sin_previewer_es_none() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///nota.txt", b"hola mundo").await;
    let c = connected_client(&d).await;
    let res = c
        .call::<_, methods::PluginPreviewResult>(
            methods::PLUGIN_PREVIEW,
            &methods::PluginPreviewParams {
                path: vp("mem:///nota.txt"),
            },
        )
        .await
        .expect("plugin.preview no es error cuando no hay previewer");
    assert!(
        res.preview.is_none(),
        "sin previewer instalado la preview es None (vista cruda), no un error: {res:?}"
    );
}

/// G3a (ADR 0037): `plugin.preview_styled` sin ningún previewer instalado
/// devuelve `preview: None` — MISMO criterio que su gemelo plano, no un
/// error. El client `Backend::plugin_preview_styled` embebido tiene su
/// propio test para el caso `Ok(None)`; este cubre el handler DAEMON contra
/// un socket real.
#[tokio::test]
async fn plugin_preview_styled_sin_previewer_es_none() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///nota.txt", b"hola mundo").await;
    let c = connected_client(&d).await;
    let res = c
        .call::<_, methods::PluginPreviewStyledResult>(
            methods::PLUGIN_PREVIEW_STYLED,
            &methods::PluginPreviewStyledParams {
                path: vp("mem:///nota.txt"),
            },
        )
        .await
        .expect("plugin.preview_styled no es error cuando no hay previewer");
    assert!(
        res.preview.is_none(),
        "sin previewer instalado la preview con estilo es None: {res:?}"
    );
}

/// TOML deliberadamente inválido: el descubridor debe reportarlo en `errors`,
/// no tumbar el catálogo. Un `[[[` sin cerrar no parsea.
const BROKEN_MANIFEST: &str = "no es toml [[[";

/// Daemon apuntado a un `cfg` sembrado con un plugin VÁLIDO (`org.norte.demo`)
/// y uno ROTO (`rota`, TOML inválido). Devuelve también la ruta `cfg` para
/// poder abrir un `PluginRegistry` fresco sobre ella y comprobar persistencia.
async fn spawn_daemon_plugins_ok_y_roto() -> (TestDaemon, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = dir.path().join("cfg");
    let ok_dir = cfg.join("plugins").join("org.norte.demo");
    let rota_dir = cfg.join("plugins").join("rota");
    std::fs::create_dir_all(&ok_dir).expect("mkdir ok");
    std::fs::create_dir_all(&rota_dir).expect("mkdir rota");
    std::fs::write(ok_dir.join("plugin.toml"), DEMO_MANIFEST).expect("write manifest ok");
    std::fs::write(rota_dir.join("plugin.toml"), BROKEN_MANIFEST).expect("write manifest roto");

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
            },
        )
        .await
        .expect_err("un agente no gobierna consentimiento");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));
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

/// `task.cancel` de un AGENTE sobre una task ajena: ack (el contrato no
/// filtra existencia) pero SIN efecto — la copia del humano completa.
#[tokio::test]
async fn task_cancel_de_agente_no_toca_task_del_humano() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///grande.bin", &vec![0xCD; 100_000]).await;
    // Latencia por op GENEROSA (la copia son ~4-6 ops, no proporcional al
    // tamaño): la task sigue viva cuando llega el cancel hostil incluso en
    // un runner cargado — si terminara antes, el test pasaría en vacío.
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(500)));
    let mut human = connected_client(&d).await;
    let agent = connected_agent(&d, "sess-cancel").await;

    let task: FsTaskResult = human
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
        .expect("fs.copy del humano");
    let _: TaskCancelResult = agent
        .call(
            methods::TASK_CANCEL,
            &TaskCancelParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("ack: no se filtra existencia de tasks ajenas");
    let seen = drain_task(&mut human, task.task_id.get()).await;
    assert_eq!(
        seen.last().expect("terminal").state,
        TaskState::Completed,
        "la cancelación de un agente sobre una task ajena NO surte efecto"
    );
}

/// Un agente SÍ cancela su propia task (el gate no bloquea de más).
#[tokio::test]
async fn task_cancel_de_agente_cancela_la_suya() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///grande.bin", &vec![0xEF; 100_000]).await;
    // Latencia generosa: si la copia completara antes del cancel, el
    // terminal sería Completed y el test fallaría por timing, no por gate.
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(500)));
    let mut agent = connected_agent(&d, "sess-own").await;

    let task: FsTaskResult = agent
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
        .expect("fs.copy del agente");
    let _: TaskCancelResult = agent
        .call(
            methods::TASK_CANCEL,
            &TaskCancelParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("task.cancel propio");
    let seen = drain_task(&mut agent, task.task_id.get()).await;
    assert_eq!(
        seen.last().expect("terminal").state,
        TaskState::Cancelled,
        "cancelar lo propio sigue funcionando"
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
            &DaemonShutdownParams { graceful: false },
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

/// Un `task_id` desconocido (o expulsado del anillo) es `INVALID_PARAMS`.
#[tokio::test]
async fn undo_report_task_desconocida_es_invalid_params() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::PolicyUndoReportResult>(
            methods::POLICY_UNDO_REPORT,
            &methods::PolicyUndoReportParams {
                task_id: norte_proto::TaskId::new(424_242),
            },
        )
        .await
        .expect_err("sin undo no hay informe");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
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
            },
        )
        .await
        .expect("la reserva del humano sobrevive al agente glotón");
}

/// #53 (M2, regla 3 + DoD): cerrar una conexión con listings retenidos los
/// SUELTA (RAII: drop de `ConnState` → drop de `OpenListing` → guard
/// decrementa el tope global y muere el productor). Observable extremo-a-
/// extremo por la degradación del tope global: saturado, `fs.list` paginado
/// degrada a listado-completo (`next_cursor=None`); liberado, vuelve a
/// paginar.
#[tokio::test]
async fn cerrar_conexion_libera_sus_listings_retenidos() {
    let d = spawn_daemon(None).await;
    for i in 0..3u32 {
        write_file(&d.mem, &format!("mem:///f{i}.bin"), b"x").await;
    }
    let page = |c: &'static str| FsListParams {
        path: vp(c),
        limit: Some(1),
        cursor: None,
    };

    // Satura el tope GLOBAL (256): 32 conexiones × 8 listings retenidos.
    let mut hoarders = Vec::new();
    for _ in 0..32 {
        let c = connected_client(&d).await;
        for _ in 0..8 {
            let r: FsListResult = c
                .call(methods::FS_LIST, &page("mem:///"))
                .await
                .expect("fs.list");
            assert!(r.next_cursor.is_some(), "retenido (aún bajo el tope)");
        }
        hoarders.push(c);
    }
    // Saturado: una página nueva DEGRADA a listado-completo (no retiene).
    let probe = connected_client(&d).await;
    let r: FsListResult = probe
        .call(methods::FS_LIST, &page("mem:///"))
        .await
        .expect("fs.list degradado");
    assert!(r.next_cursor.is_none(), "saturado degrada a completo");
    assert_eq!(r.entries.len(), 3, "degradado = TODO el listado");

    // Cae UNA conexión acaparadora: sus 8 listings deben soltarse (RAII).
    drop(hoarders.pop());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let r: FsListResult = probe
            .call(methods::FS_LIST, &page("mem:///"))
            .await
            .expect("fs.list tras liberar");
        if r.next_cursor.is_some() {
            break; // volvió a paginar: el tope global bajó — liberado.
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "los listings de la conexión muerta no se liberaron"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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
    let id = loop {
        if let Some(id) = *id_slot.lock().expect("id lock") {
            break id;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
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
    let id = loop {
        if let Some(id) = *id_slot.lock().expect("id lock") {
            break id;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
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
    // pendiente no existe → INVALID_PARAMS, no un ok silencioso.
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
        Err(ClientError::Rpc(rpc)) => assert_eq!(
            rpc.code,
            codes::INVALID_PARAMS,
            "decide sobre pendiente retirada debe ser INVALID_PARAMS"
        ),
        other => panic!("esperaba INVALID_PARAMS, fue {other:?}"),
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
    let id = loop {
        if let Some(id) = *id_slot.lock().expect("id lock") {
            break id;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
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
            },
        )
        .await
        .expect("la conexión sigue viva tras el rpc.cancel de una request resuelta");
    assert_eq!(st.entry.size, Some(4));
}

/// #72 (borde): `rpc.cancel` de un id DESCONOCIDO (nada en vuelo) es un no-op
/// benigno — ni cuelga ni rompe la conexión. Cubre también el caso de un
/// `rpc.cancel` errante mientras NADA está suspendido.
#[tokio::test]
async fn rpc_cancel_de_id_desconocido_es_no_op() {
    let d = spawn_daemon_ask(Duration::from_secs(30)).await;
    let agent = connected_agent(&d, "s1").await;

    agent
        .notify(
            methods::RPC_CANCEL,
            &methods::RpcCancelParams {
                id: norte_proto::wire::RequestId::Num(9999),
            },
        )
        .expect("rpc.cancel notify");

    // La conexión sigue sirviendo: el cancel de un id inexistente se dropea.
    let _: methods::TaskListResult = agent
        .call(methods::TASK_LIST, &methods::TaskListParams {})
        .await
        .expect("la conexión sigue viva tras un rpc.cancel de id desconocido");
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
    let copy_id = loop {
        if let Some(id) = *id_slot.lock().expect("id lock") {
            break id;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };

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
                },
            )
            .await
        }));
    }
    // Deja que los 5 frames lleguen al daemon (se bufferizan tras la copia).
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

// ---------- fs.search (liveSearch T4) ----------

/// Params de `fs.search` con solo un glob de nombre (helper de test).
fn search_by_name(root: &str, name_glob: &str) -> FsSearchParams {
    FsSearchParams {
        root: vp(root),
        name_glob: Some(name_glob.into()),
        name_regex: None,
        content: None,
        content_regex: None,
        case_sensitive: false,
        max_hits: None,
    }
}

/// Drena `search.hits` + `task.progress` de una búsqueda hasta su terminal.
/// Tras ver el terminal sigue vaciando brevemente los `search.hits` ya
/// encolados (la bomba de hits y la de progreso son tasks distintas: el orden
/// entre el último lote y el terminal no está garantizado). Devuelve las
/// entries acumuladas y el estado terminal.
async fn drain_search(c: &mut Client, task_id: u64) -> (Vec<Entry>, TaskState) {
    let mut hits: Vec<Entry> = Vec::new();
    let mut terminal: Option<TaskState> = None;
    loop {
        // Antes del terminal, esperamos generoso; después, solo drenamos lo ya
        // encolado (el walker terminó, no llegará nada nuevo).
        let to = if terminal.is_some() {
            Duration::from_millis(400)
        } else {
            Duration::from_secs(5)
        };
        let n = match tokio::time::timeout(to, c.notification()).await {
            Ok(Some(n)) => n,
            Ok(None) => break,
            Err(_) => {
                assert!(terminal.is_some(), "timeout esperando la búsqueda");
                break;
            }
        };
        if n.method == methods::SEARCH_HITS {
            let sh: SearchHits =
                serde_json::from_value(n.params.expect("params")).expect("SearchHits");
            if sh.task_id.get() == task_id {
                hits.extend(sh.entries);
            }
        } else if n.method == methods::TASK_PROGRESS {
            let p: TaskProgress =
                serde_json::from_value(n.params.expect("params")).expect("TaskProgress");
            if p.task_id.get() == task_id && p.state.is_terminal() {
                terminal = Some(p.state);
            }
        }
    }
    (hits, terminal.expect("estado terminal de la búsqueda"))
}

/// Round-trip: A lanza `fs.search`, recibe `FsTaskResult`, luego `search.hits`
/// con sus entries y un `task.progress` terminal Completed.
#[tokio::test]
async fn fs_search_round_trip() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///sub")).await.expect("mkdir");
    write_file(&d.mem, "mem:///x.rs", b"fn main() {}").await;
    write_file(&d.mem, "mem:///y.txt", b"nope").await;
    write_file(&d.mem, "mem:///sub/z.rs", b"mod z;").await;
    let mut c = connected_client(&d).await;

    let task: FsTaskResult = c
        .call(methods::FS_SEARCH, &search_by_name("mem:///", "*.rs"))
        .await
        .expect("fs.search");
    assert!(task.task_id.get() > 0);

    let (hits, state) = drain_search(&mut c, task.task_id.get()).await;
    assert_eq!(state, TaskState::Completed);
    let mut names: Vec<String> = hits
        .iter()
        .map(|e| {
            String::from_utf8_lossy(e.path.file_name().expect("nombre").as_bytes()).into_owned()
        })
        .collect();
    names.sort();
    assert_eq!(names, vec!["x.rs".to_string(), "z.rs".to_string()]);
}

/// Los hits van SOLO al dueño: A busca, B (otra conexión) jamás recibe un
/// `search.hits` (aunque sí ve el `task.progress`, que se difunde a humanos).
#[tokio::test]
async fn fs_search_hits_solo_al_dueno() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///a.rs", b"x").await;
    write_file(&d.mem, "mem:///b.rs", b"y").await;
    let mut a = connected_client(&d).await;
    let mut b = connected_client(&d).await;

    let task: FsTaskResult = a
        .call(methods::FS_SEARCH, &search_by_name("mem:///", "*.rs"))
        .await
        .expect("fs.search de A");
    let task_id = task.task_id.get();

    // B observa hasta el terminal de la task de A y NUNCA ve un search.hits.
    let mut b_saw_hits = false;
    loop {
        let notif = tokio::time::timeout(Duration::from_secs(5), b.notification())
            .await
            .expect("notif de B antes del timeout")
            .expect("conexión de B viva");
        if notif.method == methods::SEARCH_HITS {
            b_saw_hits = true;
        } else if notif.method == methods::TASK_PROGRESS {
            let prog: TaskProgress =
                serde_json::from_value(notif.params.expect("params")).expect("TaskProgress");
            if prog.task_id.get() == task_id && prog.state.is_terminal() {
                break;
            }
        }
    }
    assert!(!b_saw_hits, "B jamás recibe los hits de la búsqueda de A");

    // A sí los recibió.
    let (hits, state) = drain_search(&mut a, task_id).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(hits.len(), 2);
}

/// Criterios vacíos: `INVALID_PARAMS` con detalle y SIN crear Task.
#[tokio::test]
async fn fs_search_params_invalidos_no_crean_task() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let params = FsSearchParams {
        root: vp("mem:///"),
        name_glob: None,
        name_regex: None,
        content: None,
        content_regex: None,
        case_sensitive: false,
        max_hits: None,
    };
    let err = c
        .call::<_, FsTaskResult>(methods::FS_SEARCH, &params)
        .await
        .expect_err("sin criterios");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_PARAMS),
        other => panic!("esperaba Rpc INVALID_PARAMS, fue {other:?}"),
    }
    // Ninguna task viva ni reciente: la validación falló ANTES del submit.
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
    let _ = list; // (sin entradas sembradas)
    let tasks: methods::TaskListResult = c
        .call(methods::TASK_LIST, &methods::TaskListParams::default())
        .await
        .expect("task.list");
    assert!(tasks.tasks.is_empty(), "no se creó ninguna Task");
}

/// Un glob de nombre malformado también es `INVALID_PARAMS` (diagnóstico del
/// compilador del propio requester).
#[tokio::test]
async fn fs_search_glob_invalido_es_invalid_params() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, FsTaskResult>(methods::FS_SEARCH, &search_by_name("mem:///", "a[b"))
        .await
        .expect_err("glob roto");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_PARAMS),
        other => panic!("esperaba Rpc INVALID_PARAMS, fue {other:?}"),
    }
}

/// Cancelación por el wire: A busca (con latencia), manda `task.cancel` →
/// terminal Cancelled; la task sale de las vivas (no fuga).
#[tokio::test]
async fn fs_search_cancel_por_wire() {
    let d = spawn_daemon(None).await;
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(20)));
    for i in 0..200 {
        write_file(&d.mem, &format!("mem:///f{i}.rs"), b"x").await;
    }
    let mut c = connected_client(&d).await;

    let task: FsTaskResult = c
        .call(methods::FS_SEARCH, &search_by_name("mem:///", "*.rs"))
        .await
        .expect("fs.search");
    let _: TaskCancelResult = c
        .call(
            methods::TASK_CANCEL,
            &TaskCancelParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("task.cancel");
    let (_hits, state) = drain_search(&mut c, task.task_id.get()).await;
    assert_eq!(state, TaskState::Cancelled);

    // No queda como task VIVA (solo puede aparecer su terminal en `recent`).
    let list: methods::TaskListResult = c
        .call(methods::TASK_LIST, &methods::TaskListParams::default())
        .await
        .expect("task.list");
    assert!(
        list.tasks
            .iter()
            .all(|t| t.task_id != task.task_id || t.state.is_terminal()),
        "la task no sigue viva"
    );
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

/// Asevera que una lectura da `PolicyDenied` out-of-scope (helper de #80).
async fn assert_read_denied(agent: &Client, method: &str, params: &impl serde::Serialize) {
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
        },
    )
    .await;
    assert_read_denied(
        &agent,
        methods::FS_STAT,
        &FsStatParams {
            path: vp("mem:///proj/a.txt"),
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
struct EcoProvider;

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
struct DegradingConnector {
    mem: Arc<MemProvider>,
}

#[async_trait]
impl norte_core::connect::RemoteConnector for DegradingConnector {
    async fn connect(
        &self,
        scheme: &str,
        _authority: &str,
    ) -> Result<norte_core::connect::Connected, norte_proto::Error> {
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
}

/// Daemon cuyo engine tiene inyectado un [`DegradingConnector`]: cualquier
/// acceso a `ftp://backup.example/…` establece una sesión degradada.
async fn spawn_daemon_degrading() -> TestDaemon {
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

/// Siguiente `connection.degraded` del stream (ignora otras notifs), con tope.
async fn next_degraded(c: &mut Client) -> ConnectionDegraded {
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
