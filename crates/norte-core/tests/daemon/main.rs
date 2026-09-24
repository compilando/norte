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
    /// El tempdir del daemon, que es a la vez su raíz de config: de aquí sale
    /// el `connections.toml` que sirve `connection.list` (#365). Se guarda
    /// para que viva tanto como el daemon Y para poder escribir dentro.
    dir: tempfile::TempDir,
    mem: Arc<MemProvider>,
}

impl TestDaemon {
    /// La raíz de config de ESTE daemon: un tempdir, jamás el `~/.config`
    /// de quien corra la suite (#365).
    ///
    /// Es donde un test puede poner un `connections.toml` y contar con que el
    /// daemon lea ése. Antes no existía, `connection.list` leía la config real
    /// y el color de la suite dependía de la máquina.
    fn config_dir(&self) -> &std::path::Path {
        self.dir.path()
    }
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
        dir,
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
        dir,
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
        dir,
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

// Un solo binario de test, muchos ficheros (ola W10): el antiguo `daemon.rs`
// de 9.000 líneas, agrupado por la familia de métodos que cada test ejercita.
// Los helpers son `pub(super)` y viajan entre ficheros por los `use x::*`.

mod busqueda;
mod compare_sync;
mod fs_ops;
mod index_ai;
mod listado;
mod plugins;
mod policy;
mod rename;
mod sesion;
mod tasks;

use compare_sync::*;
use fs_ops::*;
use listado::*;
use plugins::*;
use policy::*;
use rename::*;
use sesion::*;
use tasks::*;
