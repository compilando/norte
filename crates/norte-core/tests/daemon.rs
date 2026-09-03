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

/// Cuánto se le da a una condición del daemon antes de darla por rota.
///
/// **Aquí sondear está BIEN, y es la diferencia con `norte-ui-host`.** Allí el
/// actor vive en el proceso del test y se puede esperar a que el ejecutor se
/// quede ocioso; aquí hay un daemon de verdad al otro lado de un socket de
/// verdad, y no hay forma de saber que ha terminado de pensar salvo
/// preguntándole. Lo que NO vale es dormir un plazo fijo y afirmar: eso es una
/// apuesta sobre cuánto tarda una máquina cargada.
///
/// El plazo es presupuesto de FALLO, no de espera: en verde no se consume.
const PLAZO: Duration = Duration::from_secs(10);

/// Sondea `cond` hasta que sea cierta, y falla NOMBRANDO lo que esperaba.
///
/// El respiro entre sondeos existe para no quemar CPU contra un socket; no es
/// lo que sostiene la prueba —eso lo hace la condición— y por eso el test no
/// se vuelve más frágil si la máquina va lenta: solo da más vueltas.
macro_rules! hasta {
    ($que_esperaba:expr, $cond:expr) => {{
        let limite = tokio::time::Instant::now() + PLAZO;
        loop {
            if $cond {
                break;
            }
            assert!(
                tokio::time::Instant::now() < limite,
                "nunca ocurrió: {}",
                $que_esperaba
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }};
}

/// El id de petición que la task de fondo deja en su hueco, cuando llega.
///
/// Seis tests lo esperaban con un `loop` SIN plazo: si el id no llegaba, el
/// test se colgaba en vez de fallar — y un test colgado no dice qué esperaba.
/// Es el mismo defecto que un `sleep` a ciegas, con otra cara.
async fn esperar_id(slot: &Arc<std::sync::Mutex<Option<u64>>>) -> u64 {
    let limite = tokio::time::Instant::now() + PLAZO;
    loop {
        if let Some(id) = *slot.lock().expect("id lock") {
            return id;
        }
        assert!(
            tokio::time::Instant::now() < limite,
            "la petición de fondo nunca publicó su id"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
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
    spawn_daemon_mem(idle, listing_ttl, MemProvider::new()).await
}

async fn spawn_daemon_mem(
    idle: Option<Duration>,
    listing_ttl: Duration,
    mem: MemProvider,
) -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let engine = Arc::new(Engine::new());
    let mem = Arc::new(mem);
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    // EL spool de `sync.plan`, uno por daemon y bajo su propio tempdir (ADR
    // 0049). Sin él `sync.plan` responde `Unsupported`.
    engine.set_spool(norte_core::sync::Spool::new(dir.path()));
    let daemon = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: idle,
            listing_ttl,
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
                attrs: Vec::new(),
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

/// La versión N-1 del daemon, DERIVADA de `PROTOCOL_VERSION`.
///
/// Escrita a mano (era `"0.38.2"`), la ventana que este test dice comprobar
/// pasaba a ser «una versión vieja cualquiera» al primer bump y un rojo al
/// segundo, culpando al cambio que pasara por delante. En 0.x el minor es el
/// major efectivo, así que N-1 es minor menos uno.
fn n_minus_one() -> String {
    let (major, rest) = methods::PROTOCOL_VERSION.split_once('.').expect("semver");
    let (minor, _) = rest.split_once('.').expect("semver");
    let minor: u64 = minor.parse().expect("minor numérico");
    assert_eq!(major, "0", "fuera de 0.x la ventana N-1 la define el major");
    assert!(minor > 0, "0.0.x no tiene N-1 que pedir");
    format!("{major}.{}.2", minor - 1)
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
        .expect_err("0.1.0 no es ni N ni N-1");
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
    // N-1 SÍ entra.
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
                attrs: Vec::new(),
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
                attrs: Vec::new(),
            },
        )
        .await
        .expect("fs.stat");
    assert_eq!(stat.entry.size, Some(4));
}

// ---------- attrs por el wire (#108 bloque 2, ADR 0039) ----------
// (Sustituye al pin del bloque 1 «el daemon ignora los ids pedidos»: desde
// este bloque el daemon valida, cruza con lo anunciado y materializa.)

fn assert_rpc_code(err: &ClientError, code: i64) {
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, code, "código RPC: {rpc:?}"),
        other => panic!("esperaba error RPC {code}, fue {other:?}"),
    }
}

/// Daemon con `MemProvider` de attrs sintéticos: el catálogo llega por
/// `fs.capabilities` (saneado por `AttrCatalog::new`) y `fs.list`/`fs.stat`
/// materializan lo pedido∩anunciado.
async fn spawn_daemon_attrs() -> TestDaemon {
    spawn_daemon_mem(
        None,
        Duration::from_mins(2),
        MemProvider::new().with_synthetic_attrs(),
    )
    .await
}

#[tokio::test]
async fn fs_capabilities_publica_el_catalogo_del_provider() {
    let d = spawn_daemon_attrs().await;
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
        r.attrs.iter().any(|a| a.id == "mem.owner"),
        "catálogo publicado: {:?}",
        r.attrs
    );
}

#[tokio::test]
async fn fs_list_attrs_malformado_o_sobre_tope_es_invalid_params() {
    let d = spawn_daemon_attrs().await;
    let c = connected_client(&d).await;
    // Id malformado (mayúsculas) → -32602.
    let err = c
        .call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: None,
                cursor: None,
                attrs: vec!["MAYUS.no".into()],
            },
        )
        .await
        .expect_err("id malformado debe ser error");
    assert_rpc_code(&err, codes::INVALID_PARAMS);
    // 17 ids válidos (el deserializador materializa 16+1 como testigo).
    let err = c
        .call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: None,
                cursor: None,
                attrs: (0..17).map(|i| format!("a.b{i}")).collect(),
            },
        )
        .await
        .expect_err("sobre-tope debe ser error");
    assert_rpc_code(&err, codes::INVALID_PARAMS);
}

#[tokio::test]
async fn fs_stat_devuelve_solo_lo_pedido_y_anunciado() {
    let d = spawn_daemon_attrs().await;
    write_file(&d.mem, "mem:///f.txt", b"hola").await;
    let c = connected_client(&d).await;
    let r: FsStatResult = c
        .call(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("mem:///f.txt"),
                attrs: vec!["mem.mode".into(), "zz.desconocido".into()],
            },
        )
        .await
        .expect("fs.stat");
    assert!(
        matches!(
            r.entry.attrs.get("mem.mode"),
            Some(norte_proto::AttrValue::Uint(_))
        ),
        "mem.mode materializado: {:?}",
        r.entry.attrs
    );
    // Id válido pero no anunciado: AUSENTE, jamás error.
    assert!(!r.entry.attrs.contains_key("zz.desconocido"));
    assert_eq!(r.entry.attrs.len(), 1);
}

#[tokio::test]
async fn fs_list_paginado_conserva_los_attrs_del_arranque() {
    let d = spawn_daemon_attrs().await;
    seed(&d.mem, 3).await;
    let c = connected_client(&d).await;
    // Primera página CON attrs; continuaciones SIN re-mandarlos.
    let mut r: FsListResult = c
        .call(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: Some(1),
                cursor: None,
                attrs: vec!["mem.mode".into()],
            },
        )
        .await
        .expect("fs.list");
    let mut total = 0;
    loop {
        // TODA entrada de TODA página lleva el attr del arranque (la última
        // página puede venir vacía: el stream no sabe que acabó hasta
        // drenarla).
        for e in &r.entries {
            assert!(
                e.attrs.contains_key("mem.mode"),
                "entrada sin mem.mode: {e:?}"
            );
        }
        total += r.entries.len();
        let Some(cursor) = r.next_cursor.clone() else {
            break;
        };
        // La continuación no re-manda attrs: el stream retenido ya los lleva.
        r = list_page(&c, "mem:///", Some(1), Some(cursor)).await;
    }
    assert_eq!(total, 3);
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
                attrs: Vec::new(),
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
            attrs: Vec::new(),
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
            plugins_dir: Some(dir.path().to_path_buf()),
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
                attrs: Vec::new(),
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
                    attrs: Vec::new(),
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
                attrs: Vec::new(),
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
                attrs: Vec::new(),
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
    // Un temporizador DEL SISTEMA BAJO PRUEBA, no una espera nuestra: lo que
    // este test comprueba es que el TTL de 150 ms caduca el listado, así que
    // hay que dejar pasar ese tiempo. No se puede sondear —caducar es dejar de
    // estar— ni saltar con reloj virtual: el daemon corre en su propio runtime
    // y sus temporizadores no los controla el test.
    //
    // Hacerlo determinista pide inyectar el reloj en el daemon, que es cambio
    // de producción y no lo vale por un test. Los 400 ms son 2,6× el plazo.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let err = c
        .call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: Some(1),
                cursor: Some(cur),
                attrs: Vec::new(),
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
                attrs: Vec::new(),
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
        dest_anchor: None,
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

/// El informe de `archive.test` es de quien lanzó la Task: un id ajeno se
/// contesta igual que uno que no existió nunca, que es lo que hacen sus dos
/// gemelos (`fs.rename_batch_report`, `sync.report`).
#[tokio::test]
async fn el_informe_de_un_test_de_archivo_no_es_de_cualquiera() {
    let d = spawn_daemon(None).await;
    let humano = connected_client(&d).await;
    let err = humano
        .call::<_, methods::ArchiveTestResult>(
            methods::ARCHIVE_TEST_REPORT,
            &methods::ArchiveTestReportParams {
                task_id: norte_proto::TaskId::new(4242),
            },
        )
        .await
        .expect_err("ese id nunca fue un test");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(
                rpc.data,
                Some(norte_proto::Error::NotFound),
                "{:?}",
                rpc.data
            );
        }
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// #250 — el informe de un empaquetado tiene la MISMA disciplina que el de un
/// test de archivo: un id que jamás fue un `archive.pack` se contesta
/// `NotFound`, que es también lo que se contesta a uno ajeno.
///
/// Existe porque el handler es un gemelo del de al lado y «es un gemelo» no es
/// evidencia: lo que aquí se fija es que el método está CABLEADO en el dispatch
/// —renombrarlo o no enrutarlo pasaba la suite entera— y que su respuesta a lo
/// desconocido no filtra existencia.
#[tokio::test]
async fn el_informe_de_un_empaquetado_no_es_de_cualquiera() {
    let d = spawn_daemon(None).await;
    let humano = connected_client(&d).await;
    let err = humano
        .call::<_, methods::ArchivePackReportResult>(
            methods::ARCHIVE_PACK_REPORT,
            &methods::ArchivePackReportParams {
                task_id: norte_proto::TaskId::new(4242),
            },
        )
        .await
        .expect_err("ese id nunca fue un empaquetado");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(
                rpc.data,
                Some(norte_proto::Error::NotFound),
                "{:?}",
                rpc.data
            );
        }
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

fn copy_params(from: &str, to: &str) -> FsCopyParams {
    FsCopyParams {
        from: vp(from),
        to: vp(to),
        on_collision: norte_proto::CollisionPolicy::default(),
        symlinks: norte_proto::SymlinkPolicy::default(),
        resume: norte_proto::ResumePolicy::default(),
        verify: norte_proto::VerifyPolicy::default(),
        dest_anchor: None,
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

/// #325, el gemelo del de arriba: `connection.provide_secret` existe en el
/// dispatch y llega al engine. Sin conector responde `Unsupported` por el
/// wire; un `METHOD_NOT_FOUND` querría decir que el handler falta.
#[tokio::test]
async fn provide_secret_llega_al_engine() {
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
                dest_anchor: None,
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
                attrs: Vec::new(),
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
                dest_anchor: None,
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
                dest_anchor: None,
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
            &DaemonShutdownParams {
                graceful: true,
                ..Default::default()
            },
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

/// Espera una `daemon.going_away` y devuelve si dice que vuelvas.
async fn going_away(c: &mut Client) -> bool {
    loop {
        let n = tokio::time::timeout(Duration::from_secs(5), c.notification())
            .await
            .expect("notificación antes del timeout")
            .expect("conexión viva");
        if n.method == methods::DAEMON_GOING_AWAY {
            let p: methods::DaemonGoingAway =
                serde_json::from_value(n.params.expect("params")).expect("DaemonGoingAway");
            return p.reconnect;
        }
    }
}

/// Un RELEVO avisa de que vuelvas, y avisa ANTES de dejar de aceptar.
#[tokio::test]
async fn un_relevo_avisa_de_que_vuelvas() {
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
        .expect("relevo aceptado");

    assert!(going_away(&mut c).await, "un relevo dice que vuelvas");
}

/// Y una parada corriente avisa de lo CONTRARIO. Es lo que separa «el daemon se
/// paró» de «se cayó la conexión», que para quien lo lee no son lo mismo — y es
/// lo que impide que un cliente resucite lo que el usuario acaba de parar.
#[tokio::test]
async fn una_parada_avisa_de_que_no_vuelvas() {
    let d = spawn_daemon(None).await;
    let mut c = connected_client(&d).await;
    let _: DaemonShutdownResult = c
        .call(methods::DAEMON_SHUTDOWN, &DaemonShutdownParams::default())
        .await
        .expect("parada aceptada");

    assert!(!going_away(&mut c).await, "una parada dice que no vuelvas");
}

/// Un relevo con una task VIVA se rehúsa, en la respuesta, mientras todavía hay
/// alguien a quien contestar — y no toca nada: el daemon sigue aceptando.
///
/// La negativa va DELANTE y no después de esperar a las tasks porque la
/// respuesta de `daemon.shutdown` sale en el acto: una negativa decidida
/// minutos más tarde no tendría a quién decírsela, y para entonces el listener
/// ya habría dejado de aceptar — «rehusar» significaría volver a aceptar, que
/// es una máquina de estados que nadie pidió.
#[tokio::test]
async fn un_relevo_con_una_task_viva_se_rehusa_y_no_toca_nada() {
    let mem = MemProvider::new();
    write_file(&mem, "mem:///src.bin", &vec![7u8; 256 * 1024]).await;
    // Latencia por operación: la copia sigue viva mientras se pide el relevo,
    // de forma determinista y sin dormir a ciegas.
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
            },
        )
        .await
        .expect("copia lanzada");

    let err = c
        .call::<_, DaemonShutdownResult>(
            methods::DAEMON_SHUTDOWN,
            &DaemonShutdownParams {
                graceful: true,
                mode: methods::ShutdownMode::Handover,
            },
        )
        .await
        .expect_err("con una copia viva, no");
    assert!(
        matches!(&err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST),
        "{err:?}"
    );

    // Y no tocó nada: el daemon sigue en pie y sirviendo.
    let _: FsStatResult = c
        .call(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("mem:///"),
                attrs: Vec::new(),
            },
        )
        .await
        .expect("el daemon sigue aceptando tras rehusar el relevo");
}

/// **El socket se retira ANTES de drenar, no después**, y eso solo importa
/// desde que existe el relevo.
///
/// Al apagar, el daemon suelta el listener y espera a sus tasks. Si el fichero
/// del socket sigue ahí durante esa espera, un cliente que reconecte recibe
/// `ECONNREFUSED`, arranca el reemplazo —cosa que ANTES de esta fase no hacía
/// nunca—, el reemplazo borra la ruta rancia y enlaza la suya… y el daemon
/// viejo, al terminar de drenar, borra el socket DEL REEMPLAZO. Éste se queda
/// escuchando en un inodo sin nombre, y como el permiso de arranque es de un
/// solo uso, nadie lo vuelve a levantar.
///
/// El test fija el orden: con una task viva —o sea, en pleno drenaje— la ruta
/// ya no existe.
#[tokio::test]
async fn el_socket_se_retira_antes_de_drenar() {
    let mem = MemProvider::new();
    write_file(&mem, "mem:///src.bin", &vec![7u8; 4 * 1024 * 1024]).await;
    // Latencia ALTA por operación: el drenaje dura segundos, así que «el
    // socket se fue» y «el daemon terminó» no pueden confundirse.
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
            },
        )
        .await
        .expect("copia lanzada");

    // Parada graceful: entra en el drenaje con la copia viva.
    let _: DaemonShutdownResult = c
        .call(methods::DAEMON_SHUTDOWN, &DaemonShutdownParams::default())
        .await
        .expect("parada aceptada");

    // La ruta tiene que desaparecer MIENTRAS todavía se drena. Las dos mitades
    // son la aserción: sin la segunda, un drenaje que acabara rápido haría pasar
    // el test con el borrado al final, que es justo lo que rompe el relevo.
    let mut retirado = false;
    for _ in 0..50 {
        if !d.socket.exists() {
            retirado = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(retirado, "el socket sigue ahí durante el drenaje");
    assert!(
        futures::FutureExt::now_or_never(&mut d.run).is_none(),
        "y el daemon TODAVÍA no ha terminado: si ya terminó, este test no          distingue el borrado temprano del tardío"
    );
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
    //
    // La versión se INTERPOLA desde `PROTOCOL_VERSION` y no se escribe a mano:
    // clavada aquí (era `"0.38.0"`), el frame envejecía sin que nadie lo
    // tocara y este test se ponía rojo dos bumps después, culpando al cambio
    // que pasara por delante. Lo que prueba es el marco crudo, no la ventana
    // N/N-1 —de eso se ocupa `version_compatible` en `norte-proto`—.
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
                attrs: Vec::new(),
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
            &DaemonShutdownParams {
                graceful: true,
                ..Default::default()
            },
        )
        .await
        .expect("shutdown");
    tokio::time::timeout(Duration::from_secs(5), d.run)
        .await
        .expect("apagado")
        .expect("join")
        .expect("run ok");
    // Sin margen fijo: lo que este test afirma es que la llamada CONTESTA y
    // jamás se cuelga, y eso ya lo sostiene el `timeout` de abajo. Dormir
    // antes solo hacía que el caso interesante —llamar ANTES de que el reader
    // vea el EOF— nunca se probara.
    let err = tokio::time::timeout(
        Duration::from_secs(5),
        c.call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: None,
                cursor: None,
                attrs: Vec::new(),
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
                dest_anchor: None,
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
                dest_anchor: None,
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
                expected_digest: None,
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

/// Y con el ancla BUENA —la que el propio `plugin.list` acaba de dar— sí
/// concede: el campo cierra una ventana, no la puerta.
#[tokio::test]
async fn plugin_set_approval_con_el_ancla_que_se_leyo_concede() {
    let d = spawn_daemon_plugins().await;
    let human = connected_client(&d).await;
    let list: methods::PluginListResult = human
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list");
    let ancla = list.plugins[0]
        .manifest_digest
        .clone()
        .expect("el catálogo trae el ancla que un humano lee");

    let _: methods::PluginSetApprovalResult = human
        .call(
            methods::PLUGIN_SET_APPROVAL,
            &methods::PluginSetApprovalParams {
                id: "org.norte.demo".into(),
                approved: true,
                expected_digest: Some(ancla),
            },
        )
        .await
        .expect("el ancla que se leyó concede");

    let despues: methods::PluginListResult = human
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list tras aprobar");
    assert!(despues.plugins[0].approved);
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

// ---------- plugin.help (H3e) ----------

/// Daemon sembrado con el plugin demo, dando al test la oportunidad de escribir
/// su propio `help.md` (H3e). `seed` recibe `(raiz_del_tempdir, dir_del_plugin)`
/// — la raíz para poder dejar ficheros FUERA del directorio del plugin, que es
/// justo lo que el caso del enlace escapado necesita.
async fn spawn_daemon_help_plugin(
    seed: impl FnOnce(&std::path::Path, &std::path::Path),
) -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let plugins_root = dir.path().join("cfg");
    let plugin_dir = plugins_root.join("plugins").join("org.norte.demo");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin");
    std::fs::write(plugin_dir.join("plugin.toml"), DEMO_MANIFEST).expect("write manifest");
    seed(dir.path(), &plugin_dir);

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

/// `plugin.help` por el socket devuelve el `help.md` del plugin, ya acotado por
/// el host: cuerpo íntegro, sin recorte ni pérdida.
#[tokio::test]
async fn plugin_help_devuelve_la_pagina_acotada_del_plugin() {
    let d = spawn_daemon_help_plugin(|_root, plugin_dir| {
        std::fs::write(
            plugin_dir.join("help.md"),
            "# Demo\n\nLa página del plugin demo.\n",
        )
        .expect("write help.md");
    })
    .await;
    let c = connected_client(&d).await;
    let help: methods::PluginHelpResult = c
        .call(
            methods::PLUGIN_HELP,
            &methods::PluginHelpParams {
                id: "org.norte.demo".into(),
            },
        )
        .await
        .expect("plugin.help responde");
    assert!(help.markdown.contains("demo"), "llega el cuerpo: {help:?}");
    assert!(
        !help.truncated && !help.lossy,
        "nada que recortar: {help:?}"
    );

    // Y `plugin.list` lo anuncia, para que el frontend no pida en vano.
    let list: methods::PluginListResult = c
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list");
    assert!(list.plugins[0].has_help, "has_help lo anuncia");
}

/// Un id que NO está en el catálogo es `INVALID_PARAMS` — mismo trato que
/// `plugin.set_approval` da a un plugin fantasma. El id nunca se compone en una
/// ruta, así que un `../` solo falla el lookup.
#[tokio::test]
async fn plugin_help_de_un_id_desconocido_es_invalid_params() {
    let d = spawn_daemon_help_plugin(|_root, plugin_dir| {
        std::fs::write(plugin_dir.join("help.md"), "# Demo\n").expect("write help.md");
    })
    .await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::PluginHelpResult>(
            methods::PLUGIN_HELP,
            &methods::PluginHelpParams {
                id: "no.existe".into(),
            },
        )
        .await
        .expect_err("un plugin fantasma no tiene página");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));

    let err2 = c
        .call::<_, methods::PluginHelpResult>(
            methods::PLUGIN_HELP,
            &methods::PluginHelpParams {
                id: "../../etc/passwd".into(),
            },
        )
        .await
        .expect_err("un id con travesía es solo un id desconocido");
    assert!(matches!(err2, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
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

/// El agujero que cierra la guarda del host, comprobado en el punto donde un
/// AGENTE llega: `plugin.help` no puede convertirse en una lectura de fichero
/// arbitrario que rodee el motor de policy. Un `help.md` que es un enlace a algo
/// de FUERA del directorio del plugin se sirve como página en blanco.
#[cfg(unix)]
#[tokio::test]
async fn plugin_help_no_sirve_un_help_md_que_escapa_del_directorio() {
    let d = spawn_daemon_help_plugin(|root, plugin_dir| {
        let secreto = root.join("secreto.md");
        std::fs::write(&secreto, "CLAVE-PRIVADA-QUE-NO-DEBE-CRUZAR-EL-WIRE")
            .expect("write secreto");
        std::os::unix::fs::symlink(&secreto, plugin_dir.join("help.md")).expect("symlink");
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
        .expect("un plugin conocido siempre responde");
    assert_eq!(
        help.markdown, "",
        "un enlace que sale del directorio no se sirve"
    );
}

/// Manifiesto con `[config]` (G3c): tres claves de tipos distintos, para
/// ejercitar `plugin.get_config`/`plugin.set_config` de punta a punta por
/// el socket.
const CONFIG_MANIFEST: &str = r#"
[plugin]
id = "org.norte.cfg"
name = "Cfg Demo"
publisher = "norte"
version = "0.1.0"
category = "command"

[config.greeting]
type = "string"
default = "hola"

[config.retries]
type = "int"
default = 3
min = 0
max = 10

[config.mode]
type = "enum"
default = "fast"
values = ["fast", "thorough"]
"#;

/// Daemon sembrado con [`CONFIG_MANIFEST`] (G3c) — espejo de
/// `spawn_daemon_plugins`, distinto manifiesto.
async fn spawn_daemon_config_plugin() -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let plugins_root = dir.path().join("cfg");
    let plugin_dir = plugins_root.join("plugins").join("org.norte.cfg");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin");
    std::fs::write(plugin_dir.join("plugin.toml"), CONFIG_MANIFEST).expect("write manifest");

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

/// `plugin.get_config` por el socket: esquema + valor efectivo de las TRES
/// claves, ABIERTO a cualquier conexión (leer no consiente nada) — incluso
/// SIN aprobar/activar el plugin (mismo criterio que `plugin.list`).
#[tokio::test]
async fn plugin_get_config_ve_el_esquema_y_los_defaults() {
    let d = spawn_daemon_config_plugin().await;
    let c = connected_client(&d).await;
    let res: methods::PluginGetConfigResult = c
        .call(
            methods::PLUGIN_GET_CONFIG,
            &methods::PluginGetConfigParams {
                id: "org.norte.cfg".into(),
            },
        )
        .await
        .expect("plugin.get_config");
    assert_eq!(res.keys.len(), 3);
    let greeting = res.keys.iter().find(|k| k.key == "greeting").unwrap();
    assert_eq!(greeting.kind, "string");
    assert_eq!(greeting.value, "hola");
    let retries = res.keys.iter().find(|k| k.key == "retries").unwrap();
    assert_eq!(retries.kind, "int");
    assert_eq!(retries.min, Some(0));
    assert_eq!(retries.max, Some(10));
    let mode = res.keys.iter().find(|k| k.key == "mode").unwrap();
    assert_eq!(mode.kind, "enum");
    assert_eq!(
        mode.values,
        vec!["fast".to_string(), "thorough".to_string()]
    );
}

/// `plugin.get_config` de un id DESCONOCIDO responde `keys: []` — nunca un
/// error (mismo criterio indulgente que `plugin.list` con un catálogo
/// vacío).
#[tokio::test]
async fn plugin_get_config_id_desconocido_es_keys_vacio() {
    let d = spawn_daemon_config_plugin().await;
    let c = connected_client(&d).await;
    let res: methods::PluginGetConfigResult = c
        .call(
            methods::PLUGIN_GET_CONFIG,
            &methods::PluginGetConfigParams {
                id: "org.norte.fantasma".into(),
            },
        )
        .await
        .expect("plugin.get_config no es error con id desconocido");
    assert!(res.keys.is_empty());
}

/// Un HUMANO fija un valor válido; `plugin.get_config` lo refleja Y
/// persistió (una NUEVA conexión también lo ve).
#[tokio::test]
async fn plugin_set_config_humano_se_refleja_y_persiste() {
    let d = spawn_daemon_config_plugin().await;
    let human = connected_client(&d).await;
    let _: methods::PluginSetConfigResult = human
        .call(
            methods::PLUGIN_SET_CONFIG,
            &methods::PluginSetConfigParams {
                id: "org.norte.cfg".into(),
                key: "greeting".into(),
                value: "hola mundo".into(),
            },
        )
        .await
        .expect("set_config con un valor válido");

    let res: methods::PluginGetConfigResult = human
        .call(
            methods::PLUGIN_GET_CONFIG,
            &methods::PluginGetConfigParams {
                id: "org.norte.cfg".into(),
            },
        )
        .await
        .expect("get_config tras set_config");
    assert_eq!(
        res.keys.iter().find(|k| k.key == "greeting").unwrap().value,
        "hola mundo"
    );

    let otra = connected_client(&d).await;
    let res2: methods::PluginGetConfigResult = otra
        .call(
            methods::PLUGIN_GET_CONFIG,
            &methods::PluginGetConfigParams {
                id: "org.norte.cfg".into(),
            },
        )
        .await
        .expect("get_config en otra conexión");
    assert_eq!(
        res2.keys
            .iter()
            .find(|k| k.key == "greeting")
            .unwrap()
            .value,
        "hola mundo",
        "el valor persistió"
    );
}

/// Un valor INVÁLIDO (fuera de `[min,max]`) es `INVALID_PARAMS` y NO se
/// persiste — `plugin.get_config` sigue viendo el default.
#[tokio::test]
async fn plugin_set_config_valor_invalido_no_persiste() {
    let d = spawn_daemon_config_plugin().await;
    let human = connected_client(&d).await;
    let err = human
        .call::<_, methods::PluginSetConfigResult>(
            methods::PLUGIN_SET_CONFIG,
            &methods::PluginSetConfigParams {
                id: "org.norte.cfg".into(),
                key: "retries".into(),
                value: "999".into(),
            },
        )
        .await
        .expect_err("999 fuera de [0,10]");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));

    let res: methods::PluginGetConfigResult = human
        .call(
            methods::PLUGIN_GET_CONFIG,
            &methods::PluginGetConfigParams {
                id: "org.norte.cfg".into(),
            },
        )
        .await
        .expect("get_config tras el rechazo");
    assert_eq!(
        res.keys.iter().find(|k| k.key == "retries").unwrap().value,
        "3",
        "el rechazo no debe haber tocado el default"
    );
}

/// Una clave DESCONOCIDA es `INVALID_PARAMS` (no se ensucia `config.toml`
/// con claves que el esquema no declara).
#[tokio::test]
async fn plugin_set_config_clave_desconocida_es_invalid_params() {
    let d = spawn_daemon_config_plugin().await;
    let human = connected_client(&d).await;
    let err = human
        .call::<_, methods::PluginSetConfigResult>(
            methods::PLUGIN_SET_CONFIG,
            &methods::PluginSetConfigParams {
                id: "org.norte.cfg".into(),
                key: "no-such-key".into(),
                value: "x".into(),
            },
        )
        .await
        .expect_err("clave desconocida");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
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
                columns: None,
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
                dest_anchor: None,
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
                dest_anchor: None,
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
                dest_anchor: None,
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
        attrs: Vec::new(),
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
                attrs: Vec::new(),
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

// ---------- fs.compare (C6 del plan de comparación de directorios) ----------

/// Params de `fs.compare` con los criterios por defecto (sin hash).
fn compare_params(left: &str, right: &str) -> methods::FsCompareParams {
    methods::FsCompareParams {
        left: vp(left),
        right: vp(right),
        criteria: methods::CompareCriteria::default(),
        max_depth: None,
        mtime_tolerance_ms: 2000,
        follow_symlinks: false,
        descend_orphans: None,
    }
}

/// Drena `compare.rows` + `task.progress` de una comparación hasta su
/// terminal. Mismo criterio que [`drain_search`]: tras el terminal aún se
/// vacía brevemente lo ya encolado (las dos bombas son tasks distintas).
/// Devuelve los LOTES y el estado terminal.
async fn drain_compare(
    c: &mut Client,
    task_id: u64,
) -> (Vec<methods::CompareRowsBatch>, TaskState) {
    let mut batches: Vec<methods::CompareRowsBatch> = Vec::new();
    let mut terminal: Option<TaskState> = None;
    loop {
        let to = if terminal.is_some() {
            Duration::from_millis(400)
        } else {
            Duration::from_secs(5)
        };
        let n = match tokio::time::timeout(to, c.notification()).await {
            Ok(Some(n)) => n,
            Ok(None) => break,
            Err(_) => {
                assert!(terminal.is_some(), "timeout esperando la comparación");
                break;
            }
        };
        if n.method == methods::COMPARE_ROWS {
            let b: methods::CompareRowsBatch =
                serde_json::from_value(n.params.expect("params")).expect("CompareRowsBatch");
            if b.task_id.get() == task_id {
                batches.push(b);
            }
        } else if n.method == methods::TASK_PROGRESS {
            let p: TaskProgress =
                serde_json::from_value(n.params.expect("params")).expect("TaskProgress");
            if p.task_id.get() == task_id && p.state.is_terminal() {
                terminal = Some(p.state);
            }
        }
    }
    (
        batches,
        terminal.expect("estado terminal de la comparación"),
    )
}

/// Round-trip por el socket: las filas llegan en lotes ACOTADOS por
/// `COMPARE_ROWS_MAX_BATCH`, coalescidos, y la Task acaba `Completed`.
#[tokio::test]
async fn fs_compare_round_trip_en_lotes_acotados() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///l")).await.expect("mkdir l");
    d.mem.mkdir(&vp("mem:///r")).await.expect("mkdir r");
    for i in 0..600 {
        write_file(&d.mem, &format!("mem:///l/f{i}.txt"), b"x").await;
        write_file(&d.mem, &format!("mem:///r/f{i}.txt"), b"x").await;
    }
    let mut c = connected_client(&d).await;

    let task: FsTaskResult = c
        .call(methods::FS_COMPARE, &compare_params("mem:///l", "mem:///r"))
        .await
        .expect("fs.compare");
    let (batches, state) = drain_compare(&mut c, task.task_id.get()).await;
    assert_eq!(state, TaskState::Completed);
    assert!(
        batches
            .iter()
            .all(|b| b.rows.len() <= methods::COMPARE_ROWS_MAX_BATCH),
        "lote por encima del tope"
    );
    let rows: usize = batches.iter().map(|b| b.rows.len()).sum();
    assert_eq!(rows, 600, "una fila por pareja");
    assert!(batches.len() < 600, "una frame por fila no es coalescer");
    assert!(
        batches
            .iter()
            .flat_map(|b| &b.rows)
            .all(|r| r.sides_are_consistent() && r.reason_is_consistent()),
        "el daemon no puede emitir filas incoherentes"
    );
}

/// Las filas son de quien lanzó la comparación: otra conexión NUNCA ve un
/// `compare.rows` ajeno (mismo criterio direccional que `search.hits`).
#[tokio::test]
async fn fs_compare_filas_solo_al_dueno() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///l")).await.expect("mkdir l");
    d.mem.mkdir(&vp("mem:///r")).await.expect("mkdir r");
    write_file(&d.mem, "mem:///l/a.txt", b"x").await;
    write_file(&d.mem, "mem:///r/a.txt", b"x").await;
    let mut duena = connected_client(&d).await;
    let mut ajena = connected_client(&d).await;

    let task: FsTaskResult = duena
        .call(methods::FS_COMPARE, &compare_params("mem:///l", "mem:///r"))
        .await
        .expect("fs.compare de la dueña");
    let task_id = task.task_id.get();

    // La otra conexión observa hasta el terminal y jamás ve un compare.rows.
    loop {
        let notif = tokio::time::timeout(Duration::from_secs(5), ajena.notification())
            .await
            .expect("timeout esperando el terminal en la conexión ajena")
            .expect("canal vivo");
        assert_ne!(
            notif.method,
            methods::COMPARE_ROWS,
            "una conexión ajena recibió filas que no son suyas"
        );
        if notif.method == methods::TASK_PROGRESS {
            let p: TaskProgress =
                serde_json::from_value(notif.params.expect("params")).expect("TaskProgress");
            if p.task_id.get() == task_id && p.state.is_terminal() {
                break;
            }
        }
    }
    let (batches, state) = drain_compare(&mut duena, task_id).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(batches.iter().map(|b| b.rows.len()).sum::<usize>(), 1);
}

/// Cancelación por el wire (regla dura 3): `task.cancel` termina la Task como
/// `Cancelled` —el único `Err` del motor ES la cancelación, no un fallo— y los
/// lotes paran.
#[tokio::test]
async fn fs_compare_cancel_por_wire() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///l")).await.expect("mkdir l");
    d.mem.mkdir(&vp("mem:///r")).await.expect("mkdir r");
    for i in 0..200 {
        write_file(&d.mem, &format!("mem:///l/f{i}.txt"), b"x").await;
        write_file(&d.mem, &format!("mem:///r/f{i}.txt"), b"x").await;
    }
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(20)));
    let mut c = connected_client(&d).await;

    let task: FsTaskResult = c
        .call(methods::FS_COMPARE, &compare_params("mem:///l", "mem:///r"))
        .await
        .expect("fs.compare");
    let _: TaskCancelResult = c
        .call(
            methods::TASK_CANCEL,
            &TaskCancelParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("task.cancel");
    let (batches, state) = drain_compare(&mut c, task.task_id.get()).await;
    assert_eq!(state, TaskState::Cancelled);
    assert!(
        batches.iter().map(|b| b.rows.len()).sum::<usize>() < 200,
        "siguieron llegando filas tras el cancel"
    );

    // No queda como task VIVA (no fuga).
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

/// Dos raíces que resuelven al mismo sitio son `-32602` y NO crean Task:
/// comparar algo contra sí mismo durante una hora no es una petición, es una
/// errata de quien llama.
#[tokio::test]
async fn fs_compare_contra_si_misma_es_invalid_params_sin_task() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///data")).await.expect("mkdir");
    let c = connected_client(&d).await;
    let before: methods::TaskListResult = c
        .call(methods::TASK_LIST, &methods::TaskListParams::default())
        .await
        .expect("task.list");

    let err = c
        .call::<_, FsTaskResult>(
            methods::FS_COMPARE,
            &compare_params("mem:///data", "mem:///data"),
        )
        .await
        .expect_err("rechazada");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_PARAMS),
        other => panic!("esperaba Rpc INVALID_PARAMS, fue {other:?}"),
    }
    let after: methods::TaskListResult = c
        .call(methods::TASK_LIST, &methods::TaskListParams::default())
        .await
        .expect("task.list");
    assert_eq!(
        after.tasks.len(),
        before.tasks.len(),
        "no puede crearse Task alguna"
    );
}

/// `follow_symlinks: true` es `-32602`: el motor acepta el campo y lo IGNORA,
/// y servir en silencio un recorrido distinto del pedido es peor que no
/// ofrecerlo.
#[tokio::test]
async fn fs_compare_follow_symlinks_es_invalid_params() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///l")).await.expect("mkdir l");
    d.mem.mkdir(&vp("mem:///r")).await.expect("mkdir r");
    let c = connected_client(&d).await;

    let mut p = compare_params("mem:///l", "mem:///r");
    p.follow_symlinks = true;
    let err = c
        .call::<_, FsTaskResult>(methods::FS_COMPARE, &p)
        .await
        .expect_err("rechazada");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_PARAMS),
        other => panic!("esperaba Rpc INVALID_PARAMS, fue {other:?}"),
    }
}

/// Un lado MAL ESCRITO es `-32602` por el socket, no «ningún lado».
///
/// Quien lo rechaza es el TIPO (`DescendSide` no tiene `serde(other)`), no un
/// `if` del handler: `parse_params` no llega a construir la petición. El test
/// vive aquí igualmente porque lo que hay que garantizar es la respuesta que ve
/// el cliente, y porque si alguien ablandara el tipo a `Side` —que sí degrada—
/// este test es el que se pone rojo. `"unknown"` va en la lista a propósito: es
/// el valor que `Side` aceptaría y que significa «ningún lado».
#[tokio::test]
async fn fs_compare_un_lado_mal_escrito_es_invalid_params() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///l")).await.expect("mkdir l");
    d.mem.mkdir(&vp("mem:///r")).await.expect("mkdir r");
    let c = connected_client(&d).await;

    for malo in ["lft", "unknown", "both"] {
        let err = c
            .call::<_, FsTaskResult>(
                methods::FS_COMPARE,
                &serde_json::json!({
                    "left": "mem:///l",
                    "right": "mem:///r",
                    "descend_orphans": malo,
                }),
            )
            .await
            .expect_err("rechazada");
        match err {
            ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_PARAMS, "{malo}"),
            other => panic!("esperaba Rpc INVALID_PARAMS para {malo}, fue {other:?}"),
        }
    }
}

/// Y el lado BIEN escrito llega hasta el motor: el huérfano de la izquierda se
/// enumera, y el de la derecha sigue siendo una fila. Es el cable entero —wire,
/// engine, walk— y no solo la struct.
#[tokio::test]
async fn fs_compare_descend_orphans_llega_al_motor() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///l")).await.expect("mkdir l");
    d.mem.mkdir(&vp("mem:///r")).await.expect("mkdir r");
    d.mem.mkdir(&vp("mem:///l/solo")).await.expect("mkdir solo");
    write_file(&d.mem, "mem:///l/solo/dentro.txt", b"x").await;
    d.mem.mkdir(&vp("mem:///r/otro")).await.expect("mkdir otro");
    // Con hijo: sin él, «el otro lado no se descendió» se cumpliría solo.
    write_file(&d.mem, "mem:///r/otro/dentro-derecha.txt", b"y").await;
    let mut c = connected_client(&d).await;

    let mut p = compare_params("mem:///l", "mem:///r");
    p.descend_orphans = Some(methods::DescendSide::Left);
    let task: FsTaskResult = c
        .call(methods::FS_COMPARE, &p)
        .await
        .expect("fs.compare aceptada");
    let (batches, terminal) = drain_compare(&mut c, task.task_id.get()).await;
    assert_eq!(terminal, TaskState::Completed);

    let nombres: Vec<Vec<u8>> = batches
        .iter()
        .flat_map(|b| &b.rows)
        .filter_map(|row| {
            [row.left.as_ref(), row.right.as_ref()]
                .into_iter()
                .flatten()
                .next()
                .and_then(|e| e.path.file_name())
                .map(|s| s.as_bytes().to_vec())
        })
        .collect();
    assert!(nombres.contains(&b"solo".to_vec()), "{nombres:?}");
    assert!(
        nombres.contains(&b"dentro.txt".to_vec()),
        "el huérfano del origen no se enumeró: {nombres:?}"
    );
    assert!(nombres.contains(&b"otro".to_vec()), "{nombres:?}");
    assert!(
        !nombres.contains(&b"dentro-derecha.txt".to_vec()),
        "el huérfano del DESTINO no se descendió: {nombres:?}"
    );
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

// ---------------------------------------------------------------- sync.plan

fn sync_params(source: &str, dest: &str) -> methods::SyncPlanParams {
    methods::SyncPlanParams {
        source: vp(source),
        dest: vp(dest),
        mode: methods::SyncMode::Update,
        compare: methods::SyncCompareOptions::default(),
        on_unknown: methods::OnUnknown::Copy,
        include: None,
    }
}

/// Drena `sync.steps` + `sync.plan_done` + `task.progress` de un plan hasta su
/// terminal. Devuelve los lotes EN ORDEN, el cierre (si lo hubo) y el estado.
///
/// El orden importa y por eso no se descarta: `sync.plan_done` CIERRA el plan, y
/// un lote después de él sería un cliente aprobando un hash de un plan que
/// todavía estaba llegando.
async fn drain_sync(
    c: &mut Client,
    task_id: u64,
) -> (
    Vec<methods::SyncStepsBatch>,
    Option<methods::SyncPlanDone>,
    TaskState,
) {
    let mut batches = Vec::new();
    let mut done: Option<methods::SyncPlanDone> = None;
    let mut terminal = None;
    let mut progreso = None;
    loop {
        let next = tokio::time::timeout(Duration::from_secs(10), c.notification()).await;
        let n = match next {
            Ok(Some(n)) => n,
            Ok(None) => break,
            Err(_) => {
                assert!(terminal.is_some(), "timeout esperando el plan");
                break;
            }
        };
        if n.method == methods::SYNC_STEPS {
            let b: methods::SyncStepsBatch =
                serde_json::from_value(n.params.expect("params")).expect("SyncStepsBatch");
            if b.task_id.get() == task_id {
                assert!(
                    done.is_none(),
                    "un sync.steps DESPUÉS del sync.plan_done: el cierre tiene que ser el último"
                );
                batches.push(b);
            }
        } else if n.method == methods::SYNC_PLAN_DONE {
            let d: methods::SyncPlanDone =
                serde_json::from_value(n.params.expect("params")).expect("SyncPlanDone");
            if d.task_id.get() == task_id {
                assert!(done.is_none(), "dos cierres para un plan");
                done = Some(d);
            }
        } else if n.method == methods::TASK_PROGRESS {
            let p: TaskProgress =
                serde_json::from_value(n.params.expect("params")).expect("TaskProgress");
            if p.task_id.get() == task_id && p.state.is_terminal() {
                // El contrato de progreso de una Task `SyncPlan`, tal y como lo
                // publica su rustdoc: cuenta PASOS y no bytes, y `entries_done`
                // es la ÚNICA señal con la que un cliente detecta un `sync.steps`
                // perdido. Sin esto, las dos frases son solo prosa.
                assert_eq!(p.kind, norte_proto::TaskKind::SyncPlan);
                assert_eq!(p.bytes_done, 0, "planificar no escribe un byte");
                assert!(p.current.is_none(), "ninguna ruta al broadcast");
                progreso = Some(p.entries_done);
                terminal = Some(p.state);
                // El cierre puede ir DETRÁS del terminal: se sigue drenando
                // hasta que el timeout corto de arriba dice que no queda nada.
            }
        }
    }
    if let (Some(entries), Some(TaskState::Completed)) = (progreso, terminal.as_ref()) {
        let vistos: u64 = batches
            .iter()
            .map(|b| u64::try_from(b.steps.len()).expect("cabe"))
            .sum();
        assert_eq!(
            entries, vistos,
            "entries_done tiene que cuadrar con los pasos entregados"
        );
    }
    (batches, done, terminal.expect("estado terminal del plan"))
}

/// Round-trip por el socket: los pasos llegan en lotes acotados y el
/// `sync.plan_done` los CIERRA — nunca al revés.
#[tokio::test]
async fn sync_plan_round_trip_pasos_y_despues_el_cierre() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///s")).await.expect("mkdir s");
    d.mem.mkdir(&vp("mem:///d")).await.expect("mkdir d");
    for i in 0..600 {
        write_file(&d.mem, &format!("mem:///s/f{i}.txt"), b"x").await;
    }
    let mut c = connected_client(&d).await;

    let task: FsTaskResult = c
        .call(methods::SYNC_PLAN, &sync_params("mem:///s", "mem:///d"))
        .await
        .expect("sync.plan");
    let (batches, done, state) = drain_sync(&mut c, task.task_id.get()).await;
    assert_eq!(state, TaskState::Completed);
    assert!(
        batches
            .iter()
            .all(|b| b.steps.len() <= methods::SYNC_STEPS_MAX_BATCH),
        "lote por encima del tope"
    );
    let steps: usize = batches.iter().map(|b| b.steps.len()).sum();
    assert_eq!(steps, 600);
    assert!(batches.len() < 600, "una frame por paso no es coalescer");
    let done = done.expect("el plan cerró");
    assert_eq!(done.counts.copy, 600);
    assert!(done.executable);
    assert_eq!(done.plan_hash.as_str().len(), methods::PLAN_HASH_LEN);
}

/// Los pasos son de quien lanzó el plan: otra conexión NUNCA ve un `sync.steps`
/// ni un `sync.plan_done` ajeno (mismo criterio direccional que
/// `compare.rows`). Y es más grave aquí: el `plan_hash` ES la autorización para
/// escribir.
#[tokio::test]
async fn sync_plan_pasos_y_hash_solo_al_dueno() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///s")).await.expect("mkdir s");
    d.mem.mkdir(&vp("mem:///d")).await.expect("mkdir d");
    write_file(&d.mem, "mem:///s/a.txt", b"x").await;
    let mut duena = connected_client(&d).await;
    let mut ajena = connected_client(&d).await;

    let task: FsTaskResult = duena
        .call(methods::SYNC_PLAN, &sync_params("mem:///s", "mem:///d"))
        .await
        .expect("sync.plan de la dueña");
    let task_id = task.task_id.get();

    loop {
        let notif = tokio::time::timeout(Duration::from_secs(5), ajena.notification())
            .await
            .expect("timeout esperando el terminal en la conexión ajena")
            .expect("canal vivo");
        assert_ne!(notif.method, methods::SYNC_STEPS, "pasos que no son suyos");
        assert_ne!(
            notif.method,
            methods::SYNC_PLAN_DONE,
            "un plan_hash ajeno es una autorización de escritura ajena"
        );
        if notif.method == methods::TASK_PROGRESS {
            let p: TaskProgress =
                serde_json::from_value(notif.params.expect("params")).expect("TaskProgress");
            if p.task_id.get() == task_id && p.state.is_terminal() {
                break;
            }
        }
    }
    let (batches, done, state) = drain_sync(&mut duena, task_id).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(batches.iter().map(|b| b.steps.len()).sum::<usize>(), 1);
    assert!(done.is_some());
}

/// Los dos campos de `compare` que no son del llamante, y el tope de `include`:
/// `-32602` SIN crear Task.
#[tokio::test]
async fn sync_plan_params_que_no_son_del_llamante_son_invalid_params() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///s")).await.expect("mkdir s");
    d.mem.mkdir(&vp("mem:///d")).await.expect("mkdir d");
    let c = connected_client(&d).await;

    let mut p = sync_params("mem:///s", "mem:///d");
    p.compare.follow_symlinks = true;
    assert_invalid_params(c.call::<_, FsTaskResult>(methods::SYNC_PLAN, &p).await);

    let mut p = sync_params("mem:///s", "mem:///d");
    p.compare.descend_orphans = Some(methods::DescendSide::Right);
    assert_invalid_params(c.call::<_, FsTaskResult>(methods::SYNC_PLAN, &p).await);

    let mut p = sync_params("mem:///s", "mem:///d");
    p.include = Some(vec![
        methods::RelPath::parse_wire("x").expect("rel");
        methods::SYNC_MAX_INCLUDE + 1
    ]);
    assert_invalid_params(c.call::<_, FsTaskResult>(methods::SYNC_PLAN, &p).await);
}

/// Raíces solapadas: categoría del wire (`OverlappingRoots`), no `-32602`, y con
/// la relación dentro — un frontend pinta las tres distinto.
#[tokio::test]
async fn sync_plan_raices_solapadas_viajan_con_su_relacion() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///a")).await.expect("mkdir a");
    d.mem.mkdir(&vp("mem:///a/sub")).await.expect("mkdir sub");
    let c = connected_client(&d).await;

    let err = c
        .call::<_, FsTaskResult>(methods::SYNC_PLAN, &sync_params("mem:///a", "mem:///a/sub"))
        .await
        .expect_err("solapadas");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(
                rpc.data,
                Some(Error::OverlappingRoots {
                    relation: norte_proto::RootOverlap::DestInsideSource
                })
            ),
            "fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
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

/// Tercera de las cuatro muertes del spool (ADR 0049): al cerrarse la conexión
/// se van sus planes. Un plan sin dueño no lo puede aplicar nadie, y lo que
/// quedaría en disco es un listado relativo de dos árboles.
#[tokio::test]
async fn sync_plan_al_cerrar_la_conexion_se_van_sus_planes() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///s")).await.expect("mkdir s");
    d.mem.mkdir(&vp("mem:///d")).await.expect("mkdir d");
    write_file(&d.mem, "mem:///s/a.txt", b"x").await;
    // El socket vive en el mismo tempdir que el estado del daemon.
    let spool_dir = d
        .socket
        .parent()
        .expect("el socket cuelga del tempdir")
        .join(norte_core::sync::SPOOL_DIR_NAME);
    let mut c = connected_client(&d).await;

    let task: FsTaskResult = c
        .call(methods::SYNC_PLAN, &sync_params("mem:///s", "mem:///d"))
        .await
        .expect("sync.plan");
    let (_, done, state) = drain_sync(&mut c, task.task_id.get()).await;
    assert_eq!(state, TaskState::Completed);
    assert!(done.is_some());
    assert_eq!(
        std::fs::read_dir(&spool_dir)
            .expect("dir de spools")
            .count(),
        1,
        "el plan aprobado se retiene mientras vive su conexión"
    );

    drop(c);
    // El desmontaje de la conexión es asíncrono. Se espera A LA CONDICIÓN con
    // un presupuesto, jamás un plazo fijo: un `sleep` calibrado a ojo es lo que
    // convierte un test en intermitente bajo carga.
    let hasta = tokio::time::Instant::now() + Duration::from_secs(10);
    let restantes = loop {
        let n = std::fs::read_dir(&spool_dir)
            .expect("dir de spools")
            .count();
        if n == 0 || tokio::time::Instant::now() >= hasta {
            break n;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert_eq!(restantes, 0, "un plan sin dueño no lo aplica nadie");
}

/// Asevera un `-32602` (params que el server rehúsa sin crear Task).
fn assert_invalid_params<T: std::fmt::Debug>(r: Result<T, ClientError>) {
    match r {
        Err(ClientError::Rpc(rpc)) => assert_eq!(rpc.code, codes::INVALID_PARAMS, "{rpc:?}"),
        other => panic!("esperaba INVALID_PARAMS, fue {other:?}"),
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
struct DegradingConnector {
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
struct FailingConnector;

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
async fn spawn_daemon_failing() -> TestDaemon {
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

/// H1 (encoding review #108-b2): los valores HOSTILES cruzan el socket de
/// verdad — `Bytes` no-UTF-8 byte-exacto tras `encode(bytes_b64)+decode`, y
/// `Text` con RTL override/ZWJ char-exacto tras el cinturón de emisión.
#[tokio::test]
async fn attrs_hostiles_cruzan_el_socket_byte_exactos() {
    let d = spawn_daemon_attrs().await;
    write_file(&d.mem, "mem:///f.txt", b"x").await;
    let c = connected_client(&d).await;
    let r: FsStatResult = c
        .call(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("mem:///f.txt"),
                attrs: vec!["mem.owner".into(), "mem.note".into()],
            },
        )
        .await
        .expect("fs.stat");
    assert_eq!(
        r.entry.attrs.get("mem.owner"),
        Some(&norte_proto::AttrValue::Bytes(
            b"due\xf1o-\xff\xfe".to_vec()
        )),
        "bytes crudos byte-exactos tras el wire"
    );
    assert_eq!(
        r.entry.attrs.get("mem.note"),
        Some(&norte_proto::AttrValue::Text(
            "\u{202e}atón\u{202c} a\u{200d}b".to_owned()
        )),
        "texto hostil char-exacto tras el wire"
    );
}

/// m2 (protocol-guardian #108-b2): garantía del bloque 1 que sigue viva —
/// pedir un id BIEN FORMADO a un provider con catálogo VACÍO sale bien en
/// `fs.list` (no -32602) y las entradas vienen peladas.
#[tokio::test]
async fn fs_list_id_valido_sobre_catalogo_vacio_no_es_error() {
    let d = spawn_daemon(None).await; // MemProvider SIN attrs sintéticos
    write_file(&d.mem, "mem:///f.txt", b"hola").await;
    let c = connected_client(&d).await;
    let list: FsListResult = c
        .call(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: None,
                cursor: None,
                attrs: vec!["posix.mode".into()],
            },
        )
        .await
        .expect("id válido sobre catálogo vacío jamás es error");
    assert_eq!(list.entries.len(), 1);
    for e in &list.entries {
        assert!(e.attrs.is_empty(), "catálogo vacío = entradas peladas");
    }
}

// ---------- ai.rename_plan (M4-IA, ADR 0031) ----------

/// Proveedor de IA falso (copiado de `ai_rename.rs` — los binarios de test no
/// comparten código): devuelve un JSON canned en dos deltas. `delay` retrasa
/// la entrega para dejar la request EN VUELO (test de rpc.cancel, #72).
struct FakeAi {
    reply: String,
    delay: Option<Duration>,
}

#[async_trait]
impl norte_ai::AiProvider for FakeAi {
    #[allow(clippy::unnecessary_literal_bound)] // firma del trait (&self→&str)
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
async fn spawn_daemon_ai(reply: &str) -> TestDaemon {
    spawn_daemon_ai_delay(reply, None).await
}

/// Como [`spawn_daemon_ai`] pero el proveedor RETRASA su respuesta: la
/// request queda en vuelo hasta que un `rpc.cancel` la retire.
async fn spawn_daemon_ai_slow(reply: &str, delay: Duration) -> TestDaemon {
    spawn_daemon_ai_delay(reply, Some(delay)).await
}

async fn spawn_daemon_ai_delay(reply: &str, delay: Option<Duration>) -> TestDaemon {
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
        _dir: dir,
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

// ---------- index.embed / index.search_semantic (M4-IA-2) ----------

/// Daemon con índice en memoria + proveedor de embeddings fake (M4-IA-2):
/// `[ai]` habilitado con un proveedor `fake` declarado como `embed_provider`.
/// `delay` retrasa cada `embed` para dejar la request EN VUELO (rpc.cancel).
async fn spawn_daemon_embed(delay: Option<Duration>) -> TestDaemon {
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

/// Camino feliz por el socket: build → embed → `search_semantic` devuelve el
/// fichero sembrado como primer hit, con un score que es un número JSON
/// FINITO (el cinturón anti-NaN del engine es contractual: un `NaN`
/// serializaría como `null` y rompería la respuesta en el cliente).
#[tokio::test]
async fn semantic_por_el_socket_devuelve_hits() {
    let d = spawn_daemon_embed(None).await;
    d.mem.mkdir(&vp("mem:///r")).await.expect("mkdir r");
    write_file(&d.mem, "mem:///r/a.txt", b"contenido alfa").await;
    let mut c = connected_client(&d).await;

    let t: FsTaskResult = c
        .call(
            methods::INDEX_BUILD,
            &methods::IndexBuildParams {
                root: vp("mem:///r"),
            },
        )
        .await
        .expect("index.build");
    let seen = drain_task(&mut c, t.task_id.get()).await;
    assert_eq!(seen.last().expect("terminal").state, TaskState::Completed);

    let t: FsTaskResult = c
        .call(
            methods::INDEX_EMBED,
            &methods::IndexEmbedParams {
                root: vp("mem:///r"),
            },
        )
        .await
        .expect("index.embed");
    let seen = drain_task(&mut c, t.task_id.get()).await;
    assert_eq!(seen.last().expect("terminal").state, TaskState::Completed);

    let r: methods::IndexSearchSemanticResult = c
        .call(
            methods::INDEX_SEARCH_SEMANTIC,
            &methods::IndexSearchSemanticParams {
                root: Some(vp("mem:///r")),
                query: "contenido alfa".into(),
                k: 5,
            },
        )
        .await
        .expect("index.search_semantic");
    let top = r.hits.first().expect("al menos un hit");
    assert_eq!(top.path, vp("mem:///r/a.txt"));
    assert!(
        top.score.is_finite(),
        "score finito por contrato del wire, fue {}",
        top.score
    );
}

/// Y con un nombre HOSTIL, byte a byte por el socket (#122).
///
/// El test de arriba redondea `a.txt`, así que la ruta que vuelve cabe en
/// ASCII y no dice nada del camino NDJSON. Aquí el fichero lleva bytes que no
/// son UTF-8 (`%FF%FE` en el wire), que es lo que un `to_string_lossy` de más
/// convertiría en `\u{FFFD}` — dando un hit que apunta a un fichero que no
/// existe, y sobre el que un frontend haría `cd` sin encontrar nada.
#[tokio::test]
async fn semantic_por_el_socket_sobrevive_a_un_nombre_hostil() {
    let d = spawn_daemon_embed(None).await;
    d.mem.mkdir(&vp("mem:///r")).await.expect("mkdir r");
    let hostil = "mem:///r/%FF%FE.txt";
    write_file(&d.mem, hostil, b"contenido alfa").await;
    let mut c = connected_client(&d).await;

    let t: FsTaskResult = c
        .call(
            methods::INDEX_BUILD,
            &methods::IndexBuildParams {
                root: vp("mem:///r"),
            },
        )
        .await
        .expect("index.build");
    let seen = drain_task(&mut c, t.task_id.get()).await;
    assert_eq!(seen.last().expect("terminal").state, TaskState::Completed);

    let t: FsTaskResult = c
        .call(
            methods::INDEX_EMBED,
            &methods::IndexEmbedParams {
                root: vp("mem:///r"),
            },
        )
        .await
        .expect("index.embed");
    let seen = drain_task(&mut c, t.task_id.get()).await;
    assert_eq!(seen.last().expect("terminal").state, TaskState::Completed);

    let r: methods::IndexSearchSemanticResult = c
        .call(
            methods::INDEX_SEARCH_SEMANTIC,
            &methods::IndexSearchSemanticParams {
                root: Some(vp("mem:///r")),
                query: "contenido alfa".into(),
                k: 5,
            },
        )
        .await
        .expect("index.search_semantic");
    let top = r.hits.first().expect("al menos un hit");
    assert_eq!(
        top.path,
        vp(hostil),
        "la ruta volvió del socket con otros bytes"
    );
    // Y los bytes son los de verdad, no un reemplazo: `\u{FFFD}` en UTF-8 es
    // `efbfbd`, y comparar la ruta reconstruida no lo distinguiría si el
    // parser hubiera aceptado el escape de otra cosa.
    assert_eq!(
        top.path.file_name().expect("nombre").as_bytes(),
        b"\xff\xfe.txt"
    );
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

// ---------- fs.rename_batch{,_plan,_report} (0.36.0, ADR 0042) ----------

/// Un nombre base desde sus BYTES: lo que un `String` no habría podido llevar.
fn sg(bytes: &[u8]) -> norte_proto::Segment {
    norte_proto::Segment::new(bytes.to_vec()).expect("segment de test")
}

/// Contenido completo de un fichero del `MemProvider` (para comprobar QUÉ
/// fichero acabó bajo cada nombre tras una permutación).
async fn read_all(mem: &MemProvider, wire: &str) -> Vec<u8> {
    use futures::StreamExt;
    let mut stream = mem.read(&vp(wire), None).await.expect("read abre");
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk"));
    }
    out
}

fn pair(from: &[u8], to: &[u8]) -> methods::RenamePair {
    methods::RenamePair {
        from: sg(from),
        to: sg(to),
    }
}

/// El plan cruza el socket con un nombre NO-UTF8 intacto, y NO muta nada.
///
/// El nombre viaja percent-encoded (`caf%FF.txt`) y vuelve como los mismos
/// bytes: es el caso que motiva que `RenamePair` lleve `Segment` y no `String`
/// (regla dura 1).
#[tokio::test]
async fn rename_batch_plan_responde_por_el_socket() {
    let d = spawn_daemon(None).await;
    let hostile = b"caf\xff.txt";
    write_file(&d.mem, "mem:///caf%FF.txt", b"x").await;
    write_file(&d.mem, "mem:///b.txt", b"y").await;
    let c = connected_client(&d).await;

    let plan: methods::FsRenameBatchPlanResult = c
        .call(
            methods::FS_RENAME_BATCH_PLAN,
            &methods::FsRenameBatchPlanParams {
                dir: vp("mem:///"),
                pairs: vec![pair(hostile, b"cafe.txt")],
            },
        )
        .await
        .expect("fs.rename_batch_plan");

    assert!(plan.executable, "{:?}", plan.collisions);
    assert_eq!(plan.steps.len(), 1);
    assert_eq!(
        plan.steps[0].from.as_bytes(),
        hostile,
        "los bytes hostiles sobreviven al viaje de ida y vuelta",
    );
    assert_eq!(plan.steps[0].to.as_bytes(), b"cafe.txt");
    assert_eq!(plan.plan_hash.to_string().len(), 64);
    // Planificar NO muta: el fichero sigue con su nombre.
    assert!(d.mem.stat(&vp("mem:///caf%FF.txt")).await.is_ok());
    assert!(matches!(
        d.mem.stat(&vp("mem:///cafe.txt")).await,
        Err(Error::NotFound)
    ));
}

/// El caso que el bucle de un `fs.move` por pareja NUNCA pudo hacer: una
/// permutación `a→b, b→a` como UNA Task por el socket.
#[tokio::test]
async fn rename_batch_ejecuta_una_permutacion_por_el_socket() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///a", b"soy-a").await;
    write_file(&d.mem, "mem:///b", b"soy-b").await;
    let c = connected_client(&d).await;
    let pairs = vec![pair(b"a", b"b"), pair(b"b", b"a")];

    let plan: methods::FsRenameBatchPlanResult = c
        .call(
            methods::FS_RENAME_BATCH_PLAN,
            &methods::FsRenameBatchPlanParams {
                dir: vp("mem:///"),
                pairs: pairs.clone(),
            },
        )
        .await
        .expect("plan");
    assert!(plan.executable, "{:?}", plan.collisions);
    assert_eq!(plan.steps.len(), 3, "dos renames y un temporal");

    let task: FsTaskResult = c
        .call(
            methods::FS_RENAME_BATCH,
            &methods::FsRenameBatchParams {
                dir: vp("mem:///"),
                pairs,
                plan_hash: plan.plan_hash.clone(),
            },
        )
        .await
        .expect("fs.rename_batch");
    assert_eq!(wait_terminal(&c, task.task_id).await, TaskState::Completed);
    assert_eq!(read_all(&d.mem, "mem:///a").await, b"soy-b");
    assert_eq!(read_all(&d.mem, "mem:///b").await, b"soy-a");

    // El informe del lote por el wire: la corrida fue limpia.
    let report: methods::FsRenameBatchReportResult = c
        .call(
            methods::FS_RENAME_BATCH_REPORT,
            &methods::FsRenameBatchReportParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("fs.rename_batch_report");
    assert_eq!(report.applied, 3);
    assert_eq!(report.rolled_back, 0);
    assert!(report.stuck.is_none());
    assert!(report.uncertain.is_none());
    assert_eq!(report.compensations_lost, 0);
}

/// Un lote que falla a mitad se desanda entero, y el INFORME lo cuenta por el
/// wire: el `Failed` de la Task solo dice la causa.
#[tokio::test]
async fn rename_batch_fallido_cuenta_su_rollback_por_el_socket() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///a", b"1").await;
    write_file(&d.mem, "mem:///b", b"2").await;
    let c = connected_client(&d).await;
    let pairs = vec![pair(b"a", b"x"), pair(b"b", b"y")];
    let plan: methods::FsRenameBatchPlanResult = c
        .call(
            methods::FS_RENAME_BATCH_PLAN,
            &methods::FsRenameBatchPlanParams {
                dir: vp("mem:///"),
                pairs: pairs.clone(),
            },
        )
        .await
        .expect("plan");
    // El SEGUNDO paso muere; el primero ya se aplicó y hay que desandarlo.
    d.mem.faults().fail_rename_at(&vp("mem:///b"));

    let task: FsTaskResult = c
        .call(
            methods::FS_RENAME_BATCH,
            &methods::FsRenameBatchParams {
                dir: vp("mem:///"),
                pairs,
                plan_hash: plan.plan_hash,
            },
        )
        .await
        .expect("fs.rename_batch");
    assert!(
        matches!(
            wait_terminal(&c, task.task_id).await,
            TaskState::Failed { .. }
        ),
        "el lote falla entero"
    );
    assert!(d.mem.stat(&vp("mem:///a")).await.is_ok(), "a volvió");
    assert!(matches!(
        d.mem.stat(&vp("mem:///x")).await,
        Err(Error::NotFound)
    ));

    let report: methods::FsRenameBatchReportResult = c
        .call(
            methods::FS_RENAME_BATCH_REPORT,
            &methods::FsRenameBatchReportParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("fs.rename_batch_report");
    assert_eq!(report.applied, 1);
    assert_eq!(report.rolled_back, 1);
    assert_eq!(report.failed_pair, Some(1), "la fila `b → y`");
    assert!(report.stuck.is_none(), "el rollback SÍ pudo terminar");
}

/// Un `task_id` que jamás fue un lote es `INVALID_PARAMS`, no un informe en
/// blanco que se pudiera leer como «fue todo bien».
#[tokio::test]
async fn rename_batch_report_de_una_task_desconocida_es_invalid_params() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::FsRenameBatchReportResult>(
            methods::FS_RENAME_BATCH_REPORT,
            &methods::FsRenameBatchReportParams {
                task_id: norte_proto::TaskId::new(4242),
            },
        )
        .await
        .expect_err("id desconocido");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::NotFound)),
            "NotFound de la taxonomía — la MISMA que da el brazo embebido, \
             fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// El informe de un lote AJENO contesta lo mismo que un id que nunca existió:
/// lleva rutas del directorio de otro actor, y distinguir «no es tuya» de «no
/// existe» ya sería confirmar que existió (mismo criterio que `task.cancel`).
/// El humano, en cambio, ve el informe del lote del agente — es quien tiene que
/// limpiar si se atascó.
#[tokio::test]
async fn un_agente_no_lee_el_informe_de_un_lote_ajeno() {
    let d = spawn_daemon_policy().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/a", b"1").await;
    let human = connected_client(&d).await;
    let agent = connected_agent(&d, "s1").await;

    // Lote del HUMANO.
    let pairs = vec![pair(b"a", b"b")];
    let plan: methods::FsRenameBatchPlanResult = human
        .call(
            methods::FS_RENAME_BATCH_PLAN,
            &methods::FsRenameBatchPlanParams {
                dir: vp("mem:///proj"),
                pairs: pairs.clone(),
            },
        )
        .await
        .expect("plan");
    let task: FsTaskResult = human
        .call(
            methods::FS_RENAME_BATCH,
            &methods::FsRenameBatchParams {
                dir: vp("mem:///proj"),
                pairs,
                plan_hash: plan.plan_hash,
            },
        )
        .await
        .expect("lote del humano");
    assert_eq!(
        wait_terminal(&human, task.task_id).await,
        TaskState::Completed
    );

    // El agente pide ESE informe: respuesta indistinguible de un id inventado.
    let ajeno = agent
        .call::<_, methods::FsRenameBatchReportResult>(
            methods::FS_RENAME_BATCH_REPORT,
            &methods::FsRenameBatchReportParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect_err("informe ajeno");
    let inventado = agent
        .call::<_, methods::FsRenameBatchReportResult>(
            methods::FS_RENAME_BATCH_REPORT,
            &methods::FsRenameBatchReportParams {
                task_id: norte_proto::TaskId::new(9999),
            },
        )
        .await
        .expect_err("id inventado");
    match (ajeno, inventado) {
        (ClientError::Rpc(a), ClientError::Rpc(b)) => {
            assert!(matches!(a.data, Some(Error::NotFound)), "{:?}", a.data);
            assert_eq!(
                (a.code, a.message),
                (b.code, b.message),
                "las dos respuestas tienen que ser LA MISMA",
            );
        }
        other => panic!("esperaba dos Rpc, fue {other:?}"),
    }

    // Y el humano sí lo lee.
    let report: methods::FsRenameBatchReportResult = human
        .call(
            methods::FS_RENAME_BATCH_REPORT,
            &methods::FsRenameBatchReportParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("el dueño lee su informe");
    assert_eq!(report.applied, 1);
}

/// La DERIVA se rehúsa con la categoría accionable (`plan_stale`), no con un
/// error interno genérico: el humano sabe que tiene que volver a planificar.
#[tokio::test]
async fn rename_batch_con_hash_rancio_es_plan_stale() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///a", b"1").await;
    let c = connected_client(&d).await;
    let pairs = vec![pair(b"a", b"z")];
    let plan: methods::FsRenameBatchPlanResult = c
        .call(
            methods::FS_RENAME_BATCH_PLAN,
            &methods::FsRenameBatchPlanParams {
                dir: vp("mem:///"),
                pairs: pairs.clone(),
            },
        )
        .await
        .expect("plan");
    assert!(plan.executable);

    // El destino aparece A ESPALDAS del daemon: el re-plan lo ve ocupado y
    // concluye otra cosa.
    write_file(&d.mem, "mem:///z", b"intruso").await;

    let err = c
        .call::<_, FsTaskResult>(
            methods::FS_RENAME_BATCH,
            &methods::FsRenameBatchParams {
                dir: vp("mem:///"),
                pairs,
                plan_hash: plan.plan_hash,
            },
        )
        .await
        .expect_err("el plan aprobado ya no vale");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PlanStale)),
            "PlanStale, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
    // Y no tocó nada.
    assert!(d.mem.stat(&vp("mem:///a")).await.is_ok());
    assert_eq!(read_all(&d.mem, "mem:///z").await, b"intruso");
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

/// El tope de parejas se impone EN LA FRONTERA y RECHAZA (no recorta): un lote
/// recortado ejecutaría un plan distinto del pedido. Los DOS métodos.
#[tokio::test]
async fn rename_batch_por_encima_del_tope_de_parejas_es_invalid_params() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let too_many: Vec<methods::RenamePair> = (0..=methods::FS_RENAME_BATCH_MAX_PAIRS)
        .map(|i| pair(format!("f{i}").as_bytes(), format!("g{i}").as_bytes()))
        .collect();
    assert_eq!(too_many.len(), methods::FS_RENAME_BATCH_MAX_PAIRS + 1);

    let err = c
        .call::<_, methods::FsRenameBatchPlanResult>(
            methods::FS_RENAME_BATCH_PLAN,
            &methods::FsRenameBatchPlanParams {
                dir: vp("mem:///"),
                pairs: too_many.clone(),
            },
        )
        .await
        .expect_err("por encima del tope");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::InvalidPath)),
            "InvalidPath, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }

    let err = c
        .call::<_, FsTaskResult>(
            methods::FS_RENAME_BATCH,
            &methods::FsRenameBatchParams {
                dir: vp("mem:///"),
                pairs: too_many,
                plan_hash: methods::PlanHash::parse(&"0".repeat(64)).expect("hash"),
            },
        )
        .await
        .expect_err("por encima del tope");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::InvalidPath)),
            "InvalidPath, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }

    // Justo en el tope NO es error de params (muere por otra cosa o pasa): el
    // rechazo es del EXCESO, no del tamaño legal.
    let at_cap: Vec<methods::RenamePair> = (0..methods::FS_RENAME_BATCH_MAX_PAIRS)
        .map(|i| pair(format!("f{i}").as_bytes(), format!("g{i}").as_bytes()))
        .collect();
    let plan: methods::FsRenameBatchPlanResult = c
        .call(
            methods::FS_RENAME_BATCH_PLAN,
            &methods::FsRenameBatchPlanParams {
                dir: vp("mem:///"),
                pairs: at_cap,
            },
        )
        .await
        .expect("el tope exacto se planifica");
    assert!(!plan.executable, "ninguno de esos ficheros existe");
}

// -------------------------------------------------- sync.apply / sync.report

/// El plan de `sync.plan` sobre un árbol de tres ficheros, ya cerrado, por la
/// conexión que lo pidió. Devuelve el cierre —el `plan_hash` está ahí y en
/// ningún otro sitio— para poder aplicarlo.
async fn plan_sobre(d: &TestDaemon, c: &mut Client) -> methods::SyncPlanDone {
    d.mem.mkdir(&vp("mem:///s")).await.expect("mkdir s");
    d.mem.mkdir(&vp("mem:///d")).await.expect("mkdir d");
    write_file(&d.mem, "mem:///s/a.txt", b"aaa").await;
    write_file(&d.mem, "mem:///s/b.txt", b"bbbb").await;
    // Y uno que YA existe en el destino con otro TAMAÑO: una sobrescritura por
    // el rung de tamaño (`Certain`), para que el informe cuente algo más que
    // copias. Con los dos del mismo tamaño la cascada bajaría a la fecha, y el
    // reloj lógico del `MemProvider` avanza de uno en uno: dos ficheros escritos
    // seguidos caen dentro de la tolerancia y salen `Same`, o sea sin paso.
    write_file(&d.mem, "mem:///s/c.txt", b"nuevo, y mas largo").await;
    write_file(&d.mem, "mem:///d/c.txt", b"viejo").await;

    let task: FsTaskResult = c
        .call(methods::SYNC_PLAN, &sync_params("mem:///s", "mem:///d"))
        .await
        .expect("sync.plan");
    let (_batches, done, state) = drain_sync(c, task.task_id.get()).await;
    assert_eq!(state, TaskState::Completed);
    done.expect("el plan cerró con su hash")
}

/// Aplicar el plan aprobado lo EJECUTA, y el informe cuenta lo que pasó: tantos
/// pasos hechos como copias y sobrescrituras traía el plan, y un lote del
/// journal bajo el que buscarlos — sin él no hay undo que pedir.
#[tokio::test]
async fn sync_apply_ejecuta_el_plan_que_se_aprobo() {
    let d = spawn_daemon_journal().await;
    let mut c = connected_client(&d).await;
    let done = plan_sobre(&d, &mut c).await;
    assert!(done.executable);

    let task: FsTaskResult = c
        .call(
            methods::SYNC_APPLY,
            &methods::SyncApplyParams {
                plan_hash: done.plan_hash.clone(),
            },
        )
        .await
        .expect("sync.apply aceptado");
    assert_eq!(
        wait_terminal(&c, task.task_id).await,
        TaskState::Completed,
        "el plan se aplicó entero"
    );

    let report: methods::SyncReportResult = c
        .call(
            methods::SYNC_REPORT,
            &methods::SyncReportParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("sync.report");
    assert_eq!(report.done, done.counts.copy + done.counts.overwrite);
    assert_eq!(report.failed, 0, "{:?}", report.failures);
    assert!(report.batch_id.is_some(), "el undo lo necesita");
    // Y el árbol de verdad: el destino tiene los bytes del origen.
    let mut stream = d
        .mem
        .read(&vp("mem:///d/c.txt"), None)
        .await
        .expect("read del destino");
    let mut bytes = Vec::new();
    while let Some(chunk) = futures::StreamExt::next(&mut stream).await {
        bytes.extend_from_slice(&chunk.expect("chunk"));
    }
    assert_eq!(bytes, b"nuevo, y mas largo", "la sobrescritura ocurrió");
    assert_eq!(done.counts.overwrite, 1, "el plan traía una sobrescritura");
}

/// Un hash que este daemon no emitió jamás es `PlanStale`: no existe plan vivo
/// con ese nombre, y esa es la única cosa que la respuesta dice.
#[tokio::test]
async fn sync_apply_de_un_hash_que_nadie_emitio_es_plan_stale() {
    let d = spawn_daemon_journal().await;
    let c = connected_client(&d).await;

    let inventado =
        methods::PlanHash::parse(&"0".repeat(methods::PLAN_HASH_LEN)).expect("hex válido");
    let err = c
        .call::<_, FsTaskResult>(
            methods::SYNC_APPLY,
            &methods::SyncApplyParams {
                plan_hash: inventado,
            },
        )
        .await
        .expect_err("nadie emitió ese plan");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PlanStale)),
            "PlanStale, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// Un hash MALFORMADO muere en el deserializador (`-32602`) y no se disfraza de
/// `PlanStale`: «esto no es un hash» y «el mundo se movió» son hechos distintos,
/// y contestar el segundo a quien mandó el primero le miente sobre el estado del
/// mundo.
#[tokio::test]
async fn sync_apply_con_hash_malformado_es_invalid_params_y_no_plan_stale() {
    let d = spawn_daemon_journal().await;
    let c = connected_client(&d).await;

    // Ni hexadecimal, ni de la longitud correcta, ni en minúsculas: las tres
    // formas de no ser un `PlanHash`.
    for basura in [
        serde_json::json!({"plan_hash": "nope"}),
        serde_json::json!({"plan_hash": "0".repeat(methods::PLAN_HASH_LEN - 1)}),
        serde_json::json!({"plan_hash": "A".repeat(methods::PLAN_HASH_LEN)}),
        serde_json::json!({}),
    ] {
        assert_invalid_params(
            c.call::<_, FsTaskResult>(methods::SYNC_APPLY, &basura)
                .await,
        );
    }
}

/// El plan de OTRA conexión es `PlanStale`, no una categoría propia: el plan
/// está atado a la conexión que lo produjo, y contestar algo distinto de «no hay
/// plan vivo con ese hash» construiría un oráculo de existencia sobre los planes
/// ajenos — que son autorizaciones de escritura.
#[tokio::test]
async fn sync_apply_de_un_plan_de_otra_conexion_es_plan_stale() {
    let d = spawn_daemon_journal().await;
    let mut duena = connected_client(&d).await;
    let done = plan_sobre(&d, &mut duena).await;
    let ajena = connected_client(&d).await;

    let err = ajena
        .call::<_, FsTaskResult>(
            methods::SYNC_APPLY,
            &methods::SyncApplyParams {
                plan_hash: done.plan_hash.clone(),
            },
        )
        .await
        .expect_err("el plan no es suyo");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PlanStale)),
            "PlanStale, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
    // Y la dueña sí puede: el rechazo era de la conexión, no del plan.
    let _: FsTaskResult = duena
        .call(
            methods::SYNC_APPLY,
            &methods::SyncApplyParams {
                plan_hash: done.plan_hash,
            },
        )
        .await
        .expect("su propio plan sí");
}

/// Un plan se aprueba UNA vez: al terminar la aplicación —en cualquier estado—
/// el plan retenido se ha ido, así que el mismo hash ya no ejecuta nada. Sin
/// esto, un hash filtrado sería una autorización de escritura reutilizable.
#[tokio::test]
async fn el_spool_se_gasta_en_cuanto_la_aplicacion_termina() {
    let d = spawn_daemon_journal().await;
    let mut c = connected_client(&d).await;
    let done = plan_sobre(&d, &mut c).await;

    let task: FsTaskResult = c
        .call(
            methods::SYNC_APPLY,
            &methods::SyncApplyParams {
                plan_hash: done.plan_hash.clone(),
            },
        )
        .await
        .expect("sync.apply aceptado");
    assert_eq!(wait_terminal(&c, task.task_id).await, TaskState::Completed);

    let err = c
        .call::<_, FsTaskResult>(
            methods::SYNC_APPLY,
            &methods::SyncApplyParams {
                plan_hash: done.plan_hash,
            },
        )
        .await
        .expect_err("un plan se aprueba una vez");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PlanStale)),
            "PlanStale, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
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

/// El informe lo ve quien podría ver la Task: su dueño o cualquier conexión
/// HUMANA. Un agente que pregunta por el informe de otro recibe la MISMA
/// respuesta que ante un id inventado — el informe lleva rutas relativas de dos
/// árboles ajenos, y distinguir «no es tuya» de «no existe» ya sería filtrar que
/// existió.
#[tokio::test]
async fn sync_report_ajeno_contesta_lo_mismo_que_un_id_inventado() {
    let d = spawn_daemon_journal().await;
    let mut duena = connected_client(&d).await;
    let done = plan_sobre(&d, &mut duena).await;
    let task: FsTaskResult = duena
        .call(
            methods::SYNC_APPLY,
            &methods::SyncApplyParams {
                plan_hash: done.plan_hash,
            },
        )
        .await
        .expect("sync.apply");
    assert_eq!(
        wait_terminal(&duena, task.task_id).await,
        TaskState::Completed
    );

    let fisgon = connected_agent(&d, "s-fisgona").await;
    let ajeno = fisgon
        .call::<_, methods::SyncReportResult>(
            methods::SYNC_REPORT,
            &methods::SyncReportParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect_err("no es suya");
    let inventado = fisgon
        .call::<_, methods::SyncReportResult>(
            methods::SYNC_REPORT,
            &methods::SyncReportParams {
                task_id: norte_proto::TaskId::new(99_999),
            },
        )
        .await
        .expect_err("nunca existió");
    for err in [ajeno, inventado] {
        match err {
            ClientError::Rpc(rpc) => assert!(
                matches!(rpc.data, Some(Error::NotFound)),
                "NotFound, fue {:?}",
                rpc.data
            ),
            other => panic!("esperaba Rpc, fue {other:?}"),
        }
    }
    // Otra conexión HUMANA sí lo ve: la simetría de `may_observe`.
    let otro_humano = connected_client(&d).await;
    let _: methods::SyncReportResult = otro_humano
        .call(
            methods::SYNC_REPORT,
            &methods::SyncReportParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("un humano ve los informes del daemon que gobierna");
}

// ---------- L2: la sesión de UI por el socket ----------

/// La sesión va y vuelve, y la revisión sube. La primera conexión humana que
/// pregunta se la queda.
#[tokio::test]
async fn session_get_y_put_por_el_socket() {
    // Con `state_dir`, porque `owner` significa «esto se guarda»: un daemon
    // sin dónde escribir contesta que no, y con razón.
    let estado = tempfile::tempdir().expect("tempdir");
    let d = spawn_daemon_estado(estado.path()).await;
    let c = connected_client(&d).await;
    let g: methods::SessionGetResult = c
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert_eq!(g.session.revision, 0);
    assert_eq!(
        g.session.version, 0,
        "sin esquema hasta que alguien escriba"
    );
    assert!(g.owner, "la primera conexión humana se la queda");

    let cuerpo = serde_json::json!({ "version": 1, "slots": {} });
    let p: methods::SessionPutResult = c
        .call(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: 0,
                body: cuerpo.clone(),
            },
        )
        .await
        .expect("session.put");
    assert_eq!(p.revision, 1);

    let g2: methods::SessionGetResult = c
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert_eq!(g2.session.body, cuerpo, "vuelve el mismo documento");
    assert_eq!(g2.session.version, 1);
    assert_eq!(g2.session.revision, 1);
}

/// Una revisión rancia por el wire es la taxonomía `Conflict` en `data`, no un
/// error de transporte: el cliente distingue «vuelve a leer» de «el daemon se
/// rompió».
#[tokio::test]
async fn session_put_rancio_es_conflict() {
    // CON `state_dir`: desde la revisión de #237 un core que no persiste
    // rehúsa el `put` entero, así que un conflicto de revisión solo se puede
    // provocar donde de verdad se escribe.
    let estado = tempfile::tempdir().expect("tmp");
    let d = spawn_daemon_estado(estado.path()).await;
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
        .expect("el primero entra");
    let err = c
        .call::<_, methods::SessionPutResult>(methods::SESSION_PUT, &params)
        .await
        .expect_err("la revisión ya no es la vigente");
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
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// Por encima del tope: `LimitExceeded` con SU token, y la sesión almacenada
/// se queda como estaba.
#[tokio::test]
async fn session_put_sobre_el_tope_es_limit_exceeded() {
    // CON `state_dir`, por lo mismo que el test de arriba: el tope se
    // comprueba después de la propiedad, y sin escritor no se llega.
    let estado = tempfile::tempdir().expect("tmp");
    let d = spawn_daemon_estado(estado.path()).await;
    let c = connected_client(&d).await;
    let _: methods::SessionGetResult = c
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    let gordo = serde_json::json!({ "x": "y".repeat(methods::SESSION_BODY_MAX + 1) });
    let err = c
        .call::<_, methods::SessionPutResult>(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: 0,
                body: gordo,
            },
        )
        .await
        .expect_err("no cabe");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert!(
                matches!(
                    rpc.data,
                    Some(Error::LimitExceeded { ref limit }) if limit == Error::LIMIT_SESSION_BODY
                ),
                "LimitExceeded session-body, fue {:?}",
                rpc.data
            );
        }
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
    let g: methods::SessionGetResult = c
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert_eq!(g.session.revision, 0, "no se escribió nada");
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

/// El segundo cliente del MISMO daemon recibe una copia y corre suelto: su
/// `put` se rehúsa y la sesión de la dueña se queda intacta.
#[tokio::test]
async fn el_segundo_cliente_recibe_copia_y_no_escribe() {
    let estado = tempfile::tempdir().expect("tempdir");
    let d = spawn_daemon_estado(estado.path()).await;
    let uno = connected_client(&d).await;
    let g1: methods::SessionGetResult = uno
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(g1.owner);
    let _: methods::SessionPutResult = uno
        .call(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: 0,
                body: serde_json::json!({ "quien": "uno" }),
            },
        )
        .await
        .expect("la dueña escribe");

    let dos = connected_client(&d).await;
    let g2: methods::SessionGetResult = dos
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(!g2.owner, "la segunda corre suelta");
    assert_eq!(
        g2.session.body["quien"],
        serde_json::json!("uno"),
        "recibe COPIA"
    );
    let err = dos
        .call::<_, methods::SessionPutResult>(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: 1,
                body: serde_json::json!({ "quien": "dos" }),
            },
        )
        .await
        .expect_err("quien no es dueña no escribe");
    // Con taxonomía y no con prosa: el cliente distingue «no mandas» de «tus
    // params están mal» sin leer inglés — y es la misma negativa que da el
    // brazo embebido.
    match err {
        ClientError::Rpc(ref rpc) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert_eq!(rpc.data, Some(Error::PermissionDenied), "{:?}", rpc.data);
        }
        ref other => panic!("esperaba Rpc, fue {other:?}"),
    }
    let g3: methods::SessionGetResult = uno
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert_eq!(g3.session.body["quien"], serde_json::json!("uno"));
}

/// La dueña que se va SUELTA la sesión: la siguiente conexión humana la toma.
/// Sin esto, un cliente que muere deja la pantalla de rehén hasta el relevo.
#[tokio::test]
async fn al_morir_la_duena_la_sesion_queda_libre() {
    let estado = tempfile::tempdir().expect("tempdir");
    let d = spawn_daemon_estado(estado.path()).await;
    let uno = connected_client(&d).await;
    let g1: methods::SessionGetResult = uno
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(g1.owner);
    drop(uno);

    // La desconexión se procesa en el servidor; se reintenta hasta verla. El
    // límite es de TIEMPO y no un número de vueltas: bajo carga, «50 yields»
    // es una carrera que se pierde y un rojo intermitente.
    let libre = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let dos = connected_client(&d).await;
            let g: methods::SessionGetResult = dos
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
        libre.is_ok(),
        "la sesión quedó de rehén de una conexión muerta"
    );
}

/// Un daemon SIN dónde escribir no dice que manda: `owner: false`, y el
/// cliente se ve suelto en vez de escribir cada segundo una pantalla que no
/// va a llegar a ningún disco.
#[tokio::test]
async fn sin_state_dir_nadie_es_duena() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let g: methods::SessionGetResult = c
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(!g.owner, "sin escritor no hay dueña que prometer");
}

/// Daemon con `state_dir` propio: el que persiste la sesión de UI (L2). El
/// directorio lo pone el test, y por eso ningún test toca el estado real.
async fn spawn_daemon_estado(state: &std::path::Path) -> TestDaemon {
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
        _dir: dir,
        mem,
    }
}

/// El relevo es el evento por el que esto existe: un daemon se va, y lo que el
/// cliente había puesto está en disco cuando arranca el siguiente.
#[tokio::test]
async fn la_sesion_sobrevive_a_un_relevo() {
    let estado = tempfile::tempdir().expect("tmp");
    let d = spawn_daemon_estado(estado.path()).await;
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
    // El volcado y la suelta del lock ocurren DENTRO de `run`: esperarlo es
    // esperar exactamente a lo que el sucesor necesita encontrar hecho.
    d.run.await.expect("join").expect("apagado limpio");

    let d2 = spawn_daemon_estado(estado.path()).await;
    let c2 = connected_client(&d2).await;
    let g: methods::SessionGetResult = c2
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert_eq!(g.session.body["dir"], serde_json::json!("file:///casa"));
    assert_eq!(g.session.revision, 1, "la revisión también sobrevive");
    assert_eq!(g.session.version, 1);
}

/// #237: un daemon que arranca mientras OTRO core tiene el lock de la sesión
/// no lo volvía a intentar jamás.
///
/// `session_persists` se calculaba una vez en el bind, así que contestaba
/// `owner: false` durante toda su vida — también horas después de que el otro
/// proceso se hubiera ido y el fichero llevara libre desde entonces. El brazo
/// embebido ya reintentaba (#234); éste es el del daemon.
///
/// Y al tomarlo tarde ADOPTA el documento de disco: lo que el otro core
/// escribió después de que éste arrancara es lo vigente, y servir la copia
/// vieja con el número nuevo sería perderlo sin que nada lo notara.
#[tokio::test]
async fn un_daemon_suelto_toma_la_sesion_cuando_queda_libre() {
    let estado = tempfile::tempdir().expect("tmp");
    let uno = spawn_daemon_estado(estado.path()).await;
    let c1 = connected_client(&uno).await;
    let g1: methods::SessionGetResult = c1
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(g1.owner, "el primero tiene el lock");

    // El segundo arranca CON el lock tomado: corre suelto.
    let dos = spawn_daemon_estado(estado.path()).await;
    let c2 = connected_client(&dos).await;
    let g2: methods::SessionGetResult = c2
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(!g2.owner, "con el lock de otro, suelto");

    // El primero escribe DESPUÉS de que el segundo haya arrancado: esto es lo
    // que el segundo tiene que adoptar, y no puede haberlo leído al nacer.
    let _: methods::SessionPutResult = c1
        .call(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: 0,
                body: serde_json::json!({ "quien": "el primero" }),
            },
        )
        .await
        .expect("la dueña escribe");
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
    uno.run.await.expect("join").expect("apagado limpio");

    // El límite es de TIEMPO y no un número de vueltas: el escritor reintenta
    // en su tick, y bajo carga contar vueltas es un rojo intermitente.
    let tomada = tokio::time::timeout(Duration::from_secs(20), async {
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
    .expect("el segundo nunca tomó una sesión que llevaba libre");

    assert_eq!(
        tomada.session.body["quien"],
        serde_json::json!("el primero"),
        "y adopta el documento que dejó el otro, no el que tenía al nacer"
    );
    assert_eq!(tomada.session.revision, 1, "con su revisión");
}

/// **La promesa de ADR 0059, de punta a punta**: una sesión que escribió un
/// binario MÁS NUEVO no se lee y —lo que importa— no se pisa.
///
/// Sin el gate en el escritor, esto se rompía en un segundo y en silencio: el
/// fichero del futuro no se cargaba, el core arrancaba en la revisión 0, el
/// primer `put` del cliente la aceptaba, y el volcado siguiente publicaba
/// encima. Perder la sesión de un binario nuevo contra uno viejo no se
/// recupera, así que la afirmación es sobre los BYTES del fichero.
#[tokio::test]
async fn una_sesion_del_futuro_no_se_pisa_por_el_socket() {
    let estado = tempfile::tempdir().expect("tmp");
    let futura = methods::Session {
        version: norte_core::ui_session::disk::SCHEMA_VERSION + 1,
        revision: 7,
        body: serde_json::json!({ "de": "un binario más nuevo" }),
    };
    norte_core::ui_session::disk::write(estado.path(), &futura).expect("escribe la del futuro");
    let fichero = norte_core::ui_session::disk::path(estado.path());
    let antes = std::fs::read(&fichero).expect("lee");

    let d = spawn_daemon_estado(estado.path()).await;
    let c = connected_client(&d).await;
    let g: methods::SessionGetResult = c
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(!g.owner, "no se lee, así que tampoco se escribe");
    assert_eq!(g.session.revision, 0, "arranca desde la configuración");
    // Y un cliente que IGNORE `owner` tampoco la pisa. Antes se le aceptaba en
    // memoria y el cuerpo moría con el proceso; desde la revisión de #237 se
    // rehúsa de plano, que es lo que ya hacía el brazo embebido — y lo que hay
    // que hacer en cuanto el escritor puede tomar el lock tarde.
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
        .expect_err("sobre una sesión del futuro no se escribe ni en memoria");
    match err {
        ClientError::Rpc(ref rpc) => {
            assert_eq!(rpc.data, Some(Error::PermissionDenied), "{:?}", rpc.data);
        }
        ref other => panic!("esperaba Rpc, fue {other:?}"),
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
    d.run.await.expect("join").expect("apagado limpio");

    assert_eq!(
        std::fs::read(&fichero).expect("lee"),
        antes,
        "el fichero del futuro tiene que seguir byte a byte como estaba"
    );
}

/// El core que no tiene el lock sirve la pantalla y NO la escribe: dos cores
/// sobre un mismo estado no se pisan.
#[tokio::test]
async fn un_core_suelto_no_escribe_el_estado_ajeno() {
    let estado = tempfile::tempdir().expect("tmp");
    let duena = spawn_daemon_estado(estado.path()).await;
    let c = connected_client(&duena).await;
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
                body: serde_json::json!({ "quien": "la dueña" }),
            },
        )
        .await
        .expect("session.put");

    // El segundo core arranca CON la pantalla —el lock decide quién escribe,
    // no quién lee— aunque todavía no esté en disco.
    let suelto = spawn_daemon_estado(estado.path()).await;
    let c2 = connected_client(&suelto).await;
    let g2: methods::SessionGetResult = c2
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(!g2.owner, "el segundo core corre suelto");
    // La revisión sale de SU `get` y no se da por cero: el core suelto carga lo
    // que haya en disco, y para cuando arranca, la dueña puede haber volcado ya
    // —el primer tick de su escritor es inmediato—. Fijar el cero aquí era
    // afirmar quién ganaba esa carrera, y bajo carga la perdía: rojo
    // intermitente en un test que no habla de revisiones.
    // Y desde la revisión de #237 el `put` de un core suelto se REHÚSA, igual
    // que en el brazo embebido: aceptarlo en memoria dejó de ser inocuo cuando
    // el escritor pudo tomar el lock tarde —el cuerpo aceptado suelto
    // sobrevivía a la adopción y se publicaba encima de la pantalla ajena—.
    let err = c2
        .call::<_, methods::SessionPutResult>(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: g2.session.revision,
                body: serde_json::json!({ "quien": "el suelto" }),
            },
        )
        .await
        .expect_err("un core suelto no escribe NI en memoria");
    match err {
        ClientError::Rpc(ref rpc) => {
            assert_eq!(rpc.data, Some(Error::PermissionDenied), "{:?}", rpc.data);
        }
        ref other => panic!("esperaba Rpc, fue {other:?}"),
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
    suelto.run.await.expect("join").expect("apagado limpio");

    // En disco no ha dejado NADA suyo. El fichero puede existir ya —la dueña
    // vuelca cada segundo, y este test no compite con ese reloj— pero lo que
    // diga es de ELLA. La afirmación no es «no hay fichero», que dependería
    // del tick, sino «el fichero no es del suelto», que no depende de nada.
    let fichero = norte_core::ui_session::disk::path(estado.path());
    let quien = |ruta: &std::path::Path| -> Option<String> {
        let raw = std::fs::read(ruta).ok()?;
        let s: methods::Session = serde_json::from_slice(&raw).ok()?;
        Some(s.body["quien"].to_string())
    };
    if let Some(q) = quien(&fichero) {
        assert_eq!(
            q, "\"la dueña\"",
            "un core suelto escribió el estado de otro"
        );
    }

    // Y al apagarse la dueña, el fichero es suyo sin ambigüedad: su volcado
    // final es el que manda.
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
    duena.run.await.expect("join").expect("apagado limpio");
    assert_eq!(
        quien(&fichero).as_deref(),
        Some("\"la dueña\""),
        "el volcado final es el de la dueña"
    );
}

// ---------------------------------------------------------------------------
// El registro del daemon por el cable (#328, ADR 0092).
// ---------------------------------------------------------------------------

/// Un daemon con anillo de registro montado, y el anillo.
///
/// El anillo es EL MISMO objeto de los dos lados —el daemon lo sirve, el
/// subscriber del test escribe en él— porque en el proceso de verdad también
/// lo es: quien monta el registro es el binario, y el daemon solo lo sirve.
async fn spawn_daemon_con_anillo() -> (TestDaemon, norte_config::logring::LogRing) {
    spawn_daemon_con_anillo_de(norte_config::logring::RING_DEFAULT).await
}

/// El mismo, con el anillo del tamaño que pida el test.
///
/// Un anillo PEQUEÑO es la única forma de llegar al desbordamiento sin emitir
/// dos mil líneas, y el desbordamiento es lo que hace comprobable el `lost`.
async fn spawn_daemon_con_anillo_de(cap: usize) -> (TestDaemon, norte_config::logring::LogRing) {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let engine = Arc::new(Engine::new());
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let anillo = norte_config::logring::LogRing::new(cap);
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
    .with_log_ring(anillo.clone());
    let run = tokio::spawn(daemon.run());
    (
        TestDaemon {
            socket,
            run,
            _dir: dir,
            mem,
        },
        anillo,
    )
}

/// Encamina las líneas de ESTE hilo al anillo mientras viva el guard.
///
/// Un subscriber GLOBAL solo se puede instalar una vez por proceso, y estos
/// tests necesitan el suyo; el de ámbito lo resuelve, igual que `con_lineas`
/// en `norte-ui-host`. El daemon corre en el mismo hilo (el runtime de
/// `#[tokio::test]` es de un hilo), así que sus líneas entran también — que es
/// exactamente lo que pasa en el proceso de verdad.
///
/// Por la capa de `tracing` y no metiendo líneas a mano: el filtro por el que
/// pasa esa capa es donde vive la cota de `suppaftp`, y un atajo que se la
/// saltara probaría un camino que no existe.
fn hacia_el_anillo(anillo: &norte_config::logring::LogRing) -> tracing::subscriber::DefaultGuard {
    use tracing_subscriber::layer::SubscriberExt as _;
    let s = tracing_subscriber::registry().with(norte_config::logring::ring_layer(anillo));
    tracing::subscriber::set_default(s)
}

/// LA prueba de este trabajo: la cota sigue viva al otro lado del socket.
///
/// `suppaftp` escribe `PASS <contraseña>` en TRACE (#43, regla 10), y el nivel
/// del anillo se sube DESDE la interfaz. Si subir el nivel por `log.level`
/// dejara pasar un target de terceros, una pulsación en un panel pondría una
/// contraseña en pantalla.
///
/// **Cubre el camino de `tracing`, no el del puente `log`.** `suppaftp` no
/// emite eventos de `tracing`: emite `log::trace!`, y `tracing-log` los
/// despacha con el `target` estático `"log"`. Ese otro camino ya está fijado
/// en `norte-config`
/// (`logring::tests::la_contrasena_no_entra_ni_por_el_puente_de_log`), y la
/// cota es la MISMA función para los dos, así que repetirlo por el socket
/// probaría dos veces lo mismo. Lo que este test añade es que subir el nivel
/// POR EL CABLE no la levanta; el nombre `suppaftp` está aquí porque es el
/// target que la lista blanca nombra, no porque éste sea su camino real.
#[tokio::test]
async fn subir_el_nivel_por_el_cable_no_levanta_la_cota() {
    let (d, anillo) = spawn_daemon_con_anillo().await;
    let _guard = hacia_el_anillo(&anillo);
    let c = connected_client(&d).await;

    let nivel: methods::LogLevelResult = c
        .call(methods::LOG_LEVEL, &serde_json::json!({ "level": "trace" }))
        .await
        .expect("el humano sube el nivel");
    assert_eq!(nivel.level, "trace", "el daemon contesta el que QUEDÓ");

    tracing::trace!(target: "suppaftp", "PASS secreto-de-verdad");
    tracing::trace!(target: "hyper::proto", "cabecera cruda");
    tracing::trace!(target: "norte_core::connect", "esto sí");

    let r: methods::LogTailResult = c
        .call(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": null, "max": 500 }),
        )
        .await
        .expect("el humano lee el registro");
    let mensajes: Vec<_> = r.lines.iter().map(|l| l.message.as_str()).collect();
    assert!(mensajes.iter().any(|m| m.contains("esto sí")));
    assert!(!mensajes.iter().any(|m| m.contains("secreto-de-verdad")));
    assert!(!mensajes.iter().any(|m| m.contains("cabecera cruda")));
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

/// El cursor sobrevive dos llamadas y no repite ni se salta líneas.
#[tokio::test]
async fn el_cursor_encadena_dos_llamadas() {
    let (d, anillo) = spawn_daemon_con_anillo().await;
    let _guard = hacia_el_anillo(&anillo);
    let c = connected_client(&d).await;

    tracing::info!(target: "norte_core::prueba", "primera");
    let a: methods::LogTailResult = c
        .call(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": null, "max": 500 }),
        )
        .await
        .expect("primera vuelta");
    assert!(a.lines.iter().any(|l| l.message.contains("primera")));
    tracing::info!(target: "norte_core::prueba", "segunda");
    let b: methods::LogTailResult = c
        .call(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": a.next, "max": 500 }),
        )
        .await
        .expect("segunda vuelta");
    assert!(b.lines.iter().any(|l| l.message.contains("segunda")));
    assert!(!b.lines.iter().any(|l| l.message.contains("primera")));
    // Sin cursor no se afirma ningún hueco: nadie perdió lo que nunca esperó.
    assert_eq!(a.lost, 0);
    assert_eq!(b.lost, 0, "un cursor al día no se perdió nada");
    // El nivel y el fondo de la historia viajan con las líneas, para que el
    // panel pueda decir «esto es todo lo que hay» y a qué nivel se capturó.
    assert_eq!(a.level, "info", "el anillo arranca en INFO");
    assert_eq!(
        a.capacity,
        u32::try_from(norte_config::logring::RING_DEFAULT).expect("cabe"),
    );
}

/// Un cursor que se quedó atrás recibe el hueco CONTADO, y no un cero.
///
/// Es lo único que hace honesto el sondeo, y es el caso que ninguno de los
/// otros tests toca: todos preguntan al día o sin cursor, así que el `lost` que
/// viaja por el cable siempre valía cero — sustituir esa cuenta por un `0`
/// literal en el daemon los habría dejado a todos verdes. El fallo que esto
/// impide es concreto: un panel sondea, la máquina se atasca treinta segundos
/// con el daemon a tope, el panel vuelve a preguntar y se le contesta un
/// registro con un salto y ninguna explicación — una línea que falta es
/// indistinguible de un suceso que no ocurrió.
///
/// Se afirma el número EXACTO, no `> 0`: un `lost` que solo tiene que ser
/// positivo lo cumple cualquier cuenta mal hecha, y este número alimenta una
/// marca de hueco que dice cuántas.
#[tokio::test]
async fn un_cursor_que_se_quedo_atras_recibe_el_hueco_contado() {
    const CAP: usize = 64;
    const EMITIDAS: u64 = 100;

    let (d, anillo) = spawn_daemon_con_anillo_de(CAP).await;
    let _guard = hacia_el_anillo(&anillo);
    let c = connected_client(&d).await;

    tracing::info!(target: "norte_core::prueba", "la última que este cliente vio");
    let a: methods::LogTailResult = c
        .call(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": null, "max": 500 }),
        )
        .await
        .expect("primera vuelta");
    assert_eq!(a.lost, 0, "sin cursor no se afirma hueco");

    // El cliente se queda parado mientras el daemon sigue trabajando, y el
    // anillo da la vuelta por debajo de su cursor.
    for i in 0..EMITIDAS {
        tracing::info!(target: "norte_core::prueba", "mientras no mirabas: {i}");
    }

    let b: methods::LogTailResult = c
        .call(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": a.next, "max": 500 }),
        )
        .await
        .expect("segunda vuelta");

    // Que entraron exactamente las mías y nada más es lo que hace legible el
    // número de abajo: sin esto, un `lost` distinto no diría si falla la
    // cuenta o si el daemon logueó por su cuenta.
    assert_eq!(
        b.next - a.next,
        EMITIDAS,
        "entre las dos vueltas entraron solo las líneas del test"
    );
    // De las 100 que entraron, el anillo solo conserva 64: las 36 primeras
    // —justo las que este cursor esperaba— se cayeron por detrás.
    assert_eq!(
        b.lost,
        EMITIDAS - u64::try_from(CAP).expect("cabe"),
        "el hueco se cuenta, no se calla"
    );
    assert_eq!(b.lines.len(), CAP, "y llega el anillo entero");
    assert!(
        !b.lines
            .iter()
            .any(|l| l.message.contains("la última que este cliente vio")),
        "esa ya se había caído: es de lo que el hueco cuenta"
    );
    // Y la vuelta siguiente, con el cursor al día, no arrastra el hueco de la
    // anterior: `lost` es de ESTE cursor, no de todo lo que el anillo tiró.
    let c2: methods::LogTailResult = c
        .call(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": b.next, "max": 500 }),
        )
        .await
        .expect("tercera vuelta");
    assert_eq!(c2.lost, 0, "un cursor al día no perdió nada");
}

/// `max` se acota en el servidor: pedir un millón no manda un millón.
#[tokio::test]
async fn el_servidor_acota_max() {
    let (d, anillo) = spawn_daemon_con_anillo().await;
    let _guard = hacia_el_anillo(&anillo);
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
        .expect("pedir de más no es un error");
    // EXACTAMENTE mil, que aquí es determinista: 1200 líneas emitidas en un
    // anillo de 2000, así que ninguna se cayó y el recorte es lo único que
    // limita. Un `<=` habría pasado igual con un servidor que contestara una
    // sola línea, o ninguna.
    assert_eq!(
        r.lines.len(),
        1000,
        "el servidor recorta a su tope, ni más ni menos"
    );
    // Y lo que no cupo NO se pierde: sigue después de `next`.
    let siguiente: methods::LogTailResult = c
        .call(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": r.next, "max": 500 }),
        )
        .await
        .expect("la vuelta siguiente recoge el resto");
    assert!(
        !siguiente.lines.is_empty(),
        "quedaban líneas después del recorte"
    );
}

/// `max: 0` es un error de params, no una página vacía.
///
/// Un panel que sondeara con cero recibiría una lista vacía cada vuelta con el
/// cursor parado, y en pantalla eso se lee como «no está pasando nada» en vez
/// de como el error de programación que es. Mismo criterio que
/// `FsListParams::limit`.
#[tokio::test]
async fn max_cero_no_es_una_pagina_vacia() {
    let (d, anillo) = spawn_daemon_con_anillo().await;
    let _guard = hacia_el_anillo(&anillo);
    let c = connected_client(&d).await;

    let err = c
        .call::<_, methods::LogTailResult>(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": null, "max": 0 }),
        )
        .await
        .expect_err("cero no es una página");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_PARAMS),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// Un daemon SIN anillo dice que no lo tiene, y no contesta una lista vacía.
///
/// Es la diferencia que hace posible que el panel degrade a su registro local
/// DICIENDO por qué: un registro vacío y un registro ausente no pueden leerse
/// igual (#326). Es también el daemon compilado sin la feature `logging`, y el
/// que arrancó cuando ya había otro subscriber instalado.
#[tokio::test]
async fn sin_anillo_el_registro_no_existe_en_vez_de_estar_vacio() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;

    for (metodo, params) in [
        (
            methods::LOG_TAIL,
            serde_json::json!({ "cursor": null, "max": 10 }),
        ),
        (methods::LOG_LEVEL, serde_json::json!({ "level": "debug" })),
    ] {
        let err = c
            .call::<_, serde_json::Value>(metodo, &params)
            .await
            .expect_err("este daemon no tiene registro que servir");
        match err {
            ClientError::Rpc(rpc) => assert!(
                matches!(rpc.data, Some(norte_proto::Error::Unsupported)),
                "{metodo}: se esperaba Unsupported, fue {:?}",
                rpc.data
            ),
            other => panic!("esperaba Rpc, fue {other:?}"),
        }
    }
}

/// Un nivel que no está en el vocabulario NO se degrada al de por defecto.
///
/// Aceptar lo que no se entiende y poner otra cosa dejaría al lector creyendo
/// que pidió algo que nadie hizo.
///
/// Y se rechaza con `INVALID_PARAMS`, que es OTRO error que el del daemon sin
/// anillo (`Unsupported`, ver
/// `sin_anillo_el_registro_no_existe_en_vez_de_estar_vacio`). Con un solo
/// código, un cliente no podría distinguir «este daemon no tiene registro» de
/// «mandé una errata», y las dos cosas piden respuestas distintas: la primera
/// degrada al anillo local para siempre, la segunda se corrige y se reintenta.
#[tokio::test]
async fn un_nivel_desconocido_se_rechaza_y_el_anillo_no_se_mueve() {
    let (d, anillo) = spawn_daemon_con_anillo().await;
    let _guard = hacia_el_anillo(&anillo);
    let c = connected_client(&d).await;

    let err = c
        .call::<_, methods::LogLevelResult>(
            methods::LOG_LEVEL,
            &serde_json::json!({ "level": "verboso-del-todo" }),
        )
        .await
        .expect_err("ese nivel no existe");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(
            rpc.code,
            codes::INVALID_PARAMS,
            "una errata no es una capacidad que falte"
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
    assert_eq!(
        anillo.level(),
        norte_config::logline::LogLevel::Info,
        "el anillo se queda donde estaba"
    );

    // Y bajar no baja: pedir menos verbosidad que la vigente contesta la
    // vigente, que es la respuesta honesta y no un fallo.
    let _: methods::LogLevelResult = c
        .call(methods::LOG_LEVEL, &serde_json::json!({ "level": "debug" }))
        .await
        .expect("sube a debug");
    let r: methods::LogLevelResult = c
        .call(methods::LOG_LEVEL, &serde_json::json!({ "level": "error" }))
        .await
        .expect("pedir menos no es un error");
    assert_eq!(r.level, "debug", "el anillo NUNCA baja");
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
