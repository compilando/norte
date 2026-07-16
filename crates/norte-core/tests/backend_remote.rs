//! Matriz de la fase 3 M2: `Backend::Remote` contra un daemon real (socket
//! en tempdir) — la MISMA superficie que el embebido, tasks foráneas,
//! resync por `task.list` y reconexión con aviso.
#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use norte_core::Engine;
use norte_core::backend::remote::RemoteBackend;
use norte_core::backend::{Backend, ConnEvent, TaskRef};
use norte_core::daemon::{Daemon, DaemonConfig};
use norte_proto::methods::ClientInfo;
use norte_proto::{ByteRange, CapabilityFlags, TaskState, VPath};
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

struct TestDaemon {
    socket: PathBuf,
    /// Mantiene vivo el `run()` del daemon durante el test.
    _run: tokio::task::JoinHandle<Result<(), norte_core::daemon::DaemonError>>,
    _dir: tempfile::TempDir,
    mem: Arc<MemProvider>,
}

async fn spawn_daemon_with(dir: tempfile::TempDir, mem: Arc<MemProvider>) -> TestDaemon {
    let socket = dir.path().join("d.sock");
    let engine = Arc::new(Engine::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let daemon = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: std::time::Duration::from_mins(2),
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
    }
}

async fn spawn_daemon() -> TestDaemon {
    spawn_daemon_with(
        tempfile::tempdir().expect("tempdir"),
        Arc::new(MemProvider::new()),
    )
    .await
}

async fn remote(d: &TestDaemon) -> RemoteBackend {
    RemoteBackend::connect(
        d.socket.clone(),
        None,
        ClientInfo {
            name: "backend-test".into(),
            version: "0.0.0".into(),
        },
    )
    .await
    .expect("connect")
}

/// Espera el terminal de una task vía su watch.
async fn join_ref(task: TaskRef) -> TaskState {
    tokio::time::timeout(Duration::from_secs(5), task.join())
        .await
        .expect("terminal antes del timeout")
}

// ---------- superficie unificada ----------

#[tokio::test]
async fn remote_copy_list_read_capabilities_como_el_embebido() {
    let d = spawn_daemon().await;
    write_file(&d.mem, "mem:///src.bin", b"contenido-remoto").await;
    let backend = Backend::Remote(remote(&d).await);

    // list
    let entries = backend.list(&vp("mem:///")).await.expect("list");
    assert_eq!(entries.len(), 1);

    // copy como TaskRef con progreso hasta terminal
    let task = backend
        .copy(
            &vp("mem:///src.bin"),
            &vp("mem:///dst.bin"),
            norte_core::TransferOptions::default(),
        )
        .await
        .expect("copy");
    assert_eq!(join_ref(task).await, TaskState::Completed);

    // read con rango (viewer)
    let bytes = backend
        .read(
            &vp("mem:///dst.bin"),
            Some(ByteRange {
                offset: 10,
                len: Some(6),
            }),
        )
        .await
        .expect("read");
    assert_eq!(bytes, b"remoto");

    // capabilities (gating de F8)
    let caps = backend
        .capabilities(&vp("mem:///"))
        .await
        .expect("capabilities");
    assert!(caps.flags.contains(CapabilityFlags::TRASH));
}

/// Dos frontends, la misma sesión: el backend B ve como FORÁNEA la task
/// encolada por el backend A — el criterio de la fase 3.
#[tokio::test]
async fn dos_backends_ven_las_mismas_tasks() {
    let d = spawn_daemon().await;
    write_file(&d.mem, "mem:///grande.bin", &vec![0xAB; 100_000]).await;
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(10)));

    let a = Backend::Remote(remote(&d).await);
    let mut b = Backend::Remote(remote(&d).await);
    let mut foreign = b.take_foreign_tasks().expect("canal foráneo");

    let own = a
        .copy(
            &vp("mem:///grande.bin"),
            &vp("mem:///copia.bin"),
            norte_core::TransferOptions::default(),
        )
        .await
        .expect("copy de A");
    let own_id = own.id();

    let seen = tokio::time::timeout(Duration::from_secs(5), foreign.recv())
        .await
        .expect("task foránea antes del timeout")
        .expect("canal vivo");
    assert_eq!(seen.id(), own_id, "B ve la task de A");
    assert_eq!(join_ref(seen).await, TaskState::Completed);
}

/// La cancelación remota funciona desde el `TaskRef` (mismo gesto que el
/// embebido).
#[tokio::test]
async fn cancel_remoto_desde_el_task_ref() {
    let d = spawn_daemon().await;
    write_file(&d.mem, "mem:///grande.bin", &vec![0xCD; 200_000]).await;
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(20)));
    let backend = Backend::Remote(remote(&d).await);

    let task = backend
        .copy(
            &vp("mem:///grande.bin"),
            &vp("mem:///copia.bin"),
            norte_core::TransferOptions::default(),
        )
        .await
        .expect("copy");
    task.cancel();
    assert_eq!(join_ref(task).await, TaskState::Cancelled);
}

/// Un backend que se conecta TARDE ve las tasks vivas por el resync de
/// task.list.
#[tokio::test]
async fn resync_al_conectar_ve_tasks_en_marcha() {
    let d = spawn_daemon().await;
    write_file(&d.mem, "mem:///grande.bin", &vec![0xEE; 200_000]).await;
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(15)));
    let a = Backend::Remote(remote(&d).await);
    let own = a
        .copy(
            &vp("mem:///grande.bin"),
            &vp("mem:///copia.bin"),
            norte_core::TransferOptions::default(),
        )
        .await
        .expect("copy de A");

    // B llega tarde y aun así la ve (por task.list, no por broadcast).
    let mut b = Backend::Remote(remote(&d).await);
    let mut foreign = b.take_foreign_tasks().expect("canal foráneo");
    let seen = tokio::time::timeout(Duration::from_secs(5), foreign.recv())
        .await
        .expect("resync antes del timeout")
        .expect("canal vivo");
    assert_eq!(seen.id(), own.id());
}

/// Enlaza un daemon en un socket CONCRETO (para poder resucitarlo en el
/// mismo path tras apagarlo).
async fn bind_at(
    mem: Arc<MemProvider>,
    socket: PathBuf,
) -> (
    tokio_util::sync::CancellationToken,
    tokio::task::JoinHandle<Result<(), norte_core::daemon::DaemonError>>,
) {
    let engine = Arc::new(Engine::new());
    engine.register_provider(mem as Arc<dyn Provider>);
    let daemon = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket),
            idle_timeout: None,
            listing_ttl: std::time::Duration::from_mins(2),
        },
    )
    .await
    .expect("bind");
    let shutdown = daemon.shutdown_token();
    (shutdown, tokio::spawn(daemon.run()))
}

/// Reconexión con aviso: al morir el daemon llega `Lost`; al volver otro
/// en el MISMO socket, `Restored` — y las operaciones vuelven a funcionar.
#[tokio::test]
async fn reconexion_avisa_y_recupera() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let mem = Arc::new(MemProvider::new());
    write_file(&mem, "mem:///f", b"x").await;
    let (shutdown1, run1) = bind_at(Arc::clone(&mem), socket.clone()).await;

    let mut backend = Backend::Remote(
        RemoteBackend::connect(
            socket.clone(),
            None,
            ClientInfo {
                name: "backend-test".into(),
                version: "0.0.0".into(),
            },
        )
        .await
        .expect("connect"),
    );
    let mut events = backend.take_conn_events().expect("canal de eventos");
    assert!(backend.list(&vp("mem:///")).await.is_ok());

    // Apagar el daemon (graceful cierra las conexiones): aviso Lost.
    shutdown1.cancel();
    tokio::time::timeout(Duration::from_secs(5), run1)
        .await
        .expect("apagado")
        .expect("join")
        .expect("run ok");
    let ev = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("Lost antes del timeout")
        .expect("canal vivo");
    assert_eq!(ev, ConnEvent::Lost);
    // Mientras está caído, la taxonomía es honesta.
    assert!(matches!(
        backend.list(&vp("mem:///")).await,
        Err(norte_proto::Error::ProviderUnavailable { retryable: true })
    ));

    // Daemon nuevo en el MISMO socket: Restored y operativo.
    let (_shutdown2, _run2) = bind_at(Arc::clone(&mem), socket.clone()).await;
    let ev = tokio::time::timeout(Duration::from_secs(15), events.recv())
        .await
        .expect("Restored antes del timeout (backoff ≤5 s)")
        .expect("canal vivo");
    assert_eq!(ev, ConnEvent::Restored);
    let entries = backend
        .list(&vp("mem:///"))
        .await
        .expect("operativo tras reconectar");
    assert_eq!(entries.len(), 1);
}

/// Los errores del daemon llegan como TAXONOMÍA (el contrato de los
/// frontends), no como error de transporte.
#[tokio::test]
async fn errores_remotos_son_taxonomia() {
    let d = spawn_daemon().await;
    let backend = Backend::Remote(remote(&d).await);
    assert_eq!(
        backend.list(&vp("mem:///no-existe")).await.unwrap_err(),
        norte_proto::Error::NotFound
    );
    assert_eq!(
        backend
            .read(&vp("mem:///no-existe"), None)
            .await
            .unwrap_err(),
        norte_proto::Error::NotFound
    );
    // move de algo que no existe: NotFound del stat del origen.
    let task = backend
        .move_(
            &vp("mem:///no-existe"),
            &vp("mem:///x"),
            norte_core::TransferOptions::default(),
        )
        .await
        .expect("la task se encola");
    assert!(matches!(join_ref(task).await, TaskState::Failed { .. }));
}

/// `fs.read` remoto en varias llamadas: un rango mayor que el tope por
/// llamada se re-pide con el offset avanzado hasta juntar todo.
#[tokio::test]
async fn read_remoto_multichunk() {
    let d = spawn_daemon().await;
    // > 8 MiB (FS_READ_MAX_CHUNK): fuerza al menos dos llamadas.
    let size = usize::try_from(norte_proto::methods::FS_READ_MAX_CHUNK).unwrap() + 4096;
    let content: Vec<u8> = (0..size).map(|i| u8::try_from(i % 251).unwrap()).collect();
    write_file(&d.mem, "mem:///grande.bin", &content).await;
    let backend = Backend::Remote(remote(&d).await);

    let bytes = backend
        .read(&vp("mem:///grande.bin"), None)
        .await
        .expect("read completo multichunk");
    assert_eq!(bytes.len(), size);
    assert_eq!(bytes, content, "los tramos se juntan byte-exactos");
}

/// Un delete remoto a papelera es una task como las demás.
#[tokio::test]
async fn delete_remoto_a_papelera() {
    let d = spawn_daemon().await;
    write_file(&d.mem, "mem:///victima", b"x").await;
    let backend = Backend::Remote(remote(&d).await);
    let task = backend
        .delete(&vp("mem:///victima"), norte_proto::DeleteMode::Trash)
        .await
        .expect("delete");
    assert_eq!(join_ref(task).await, TaskState::Completed);
    assert_eq!(
        d.mem.stat(&vp("mem:///victima")).await.unwrap_err(),
        norte_proto::Error::NotFound
    );
}

/// B1 del rust-reviewer: si el daemon MUERE a mitad de una task nuestra,
/// su `join()` NO cuelga — la reconexión reconcilia la huérfana como
/// `Failed{ProviderUnavailable}`.
#[tokio::test]
async fn task_en_vuelo_no_cuelga_si_el_daemon_muere() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let mem = Arc::new(MemProvider::new());
    write_file(&mem, "mem:///big.bin", &vec![0xAB; 500_000]).await;
    // Latencia alta: la copia sigue viva cuando matamos el daemon.
    mem.faults()
        .set_latency_per_op(Some(Duration::from_millis(50)));
    let (shutdown1, run1) = bind_at(Arc::clone(&mem), socket.clone()).await;

    let backend = Backend::Remote(
        RemoteBackend::connect(
            socket.clone(),
            None,
            ClientInfo {
                name: "backend-test".into(),
                version: "0.0.0".into(),
            },
        )
        .await
        .expect("connect"),
    );
    let task = backend
        .copy(
            &vp("mem:///big.bin"),
            &vp("mem:///copia.bin"),
            norte_core::TransferOptions::default(),
        )
        .await
        .expect("copy encolada");

    // Matar el daemon con la task viva (hard: cancela y cierra ya).
    shutdown1.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), run1).await;

    // Un daemon NUEVO y vacío en el mismo socket: la reconexión no la
    // encuentra → reconciliada como Failed, join JAMÁS cuelga.
    let (_shutdown2, _run2) = bind_at(Arc::clone(&mem), socket.clone()).await;
    let state = tokio::time::timeout(Duration::from_secs(20), task.join())
        .await
        .expect("join responde, jamás se cuelga");
    assert!(
        matches!(state, TaskState::Failed { .. } | TaskState::Cancelled),
        "la huérfana se resuelve, no cuelga: {state:?}"
    );
}

// ---------- approval router por el backend (M3-3b T5) ----------

/// Daemon con regla `ask` + router de aprobaciones (patrón de tests/daemon.rs).
async fn spawn_daemon_ask() -> TestDaemon {
    use norte_core::daemon::DaemonApprovalResolver;
    use norte_core::{PolicyConfig, ScopeRegistry, ScopedPolicy};
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let scopes = ScopeRegistry::new();
    let cfg = PolicyConfig::parse("[[rule]]\naction=\"ask\"").expect("policy cfg");
    let policy = ScopedPolicy::new(scopes.clone(), cfg);
    let approvals = Arc::new(DaemonApprovalResolver::new(Duration::from_secs(30)));
    let engine = Arc::new(Engine::new().with_policy(Arc::new(policy), Arc::clone(&approvals) as _));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let daemon = norte_core::daemon::Daemon::bind_with_policy(
        engine,
        scopes,
        approvals,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
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
    }
}

/// Conexión cruda de AGENTE con scope de copy sobre `mem:///proj` ya concedido
/// (request por el agente + grant por un humano efímero).
async fn agent_with_scope(d: &TestDaemon, session: &str) -> norte_core::daemon::Client {
    use norte_proto::methods::{
        self, GrantScopeParams, GrantScopeResult, InitializeParams, RequestScopeParams,
        RequestScopeResult,
    };
    let agent = norte_core::daemon::Client::connect(&d.socket)
        .await
        .expect("connect agente");
    let _: methods::InitializeResult = agent
        .call(
            methods::INITIALIZE,
            &InitializeParams {
                client_info: ClientInfo {
                    name: "agent".into(),
                    version: "0.0.0".into(),
                },
                protocol_version: methods::PROTOCOL_VERSION.into(),
                encodings: vec!["json".into()],
                agent_session: Some(session.into()),
            },
        )
        .await
        .expect("initialize agente");
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
    let mut human = norte_core::daemon::Client::connect(&d.socket)
        .await
        .expect("connect humano");
    human
        .initialize(ClientInfo {
            name: "granter".into(),
            version: "0.0.0".into(),
        })
        .await
        .expect("initialize humano");
    let _: GrantScopeResult = human
        .call(
            methods::POLICY_GRANT_SCOPE,
            &GrantScopeParams {
                request_id: req.request_id,
            },
        )
        .await
        .expect("grant_scope");
    agent
}

/// Lanza el fs.copy del agente en una task propia (queda suspendido en el Ask).
fn spawn_agent_copy(
    agent: norte_core::daemon::Client,
) -> tokio::task::JoinHandle<
    Result<norte_proto::methods::FsTaskResult, norte_core::daemon::ClientError>,
> {
    use norte_proto::methods::{self, FsCopyParams};
    tokio::spawn(async move {
        agent
            .call::<_, methods::FsTaskResult>(
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
    })
}

/// El camino VIVO del T5: la bomba del `RemoteBackend` enruta
/// `policy.approval_required` al canal de `take_approvals`, y
/// `Backend::policy_decide(approve)` desbloquea la copia del agente.
#[tokio::test]
async fn backend_recibe_approval_y_decide_aprueba() {
    let d = spawn_daemon_ask().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/src.txt", b"hola").await;
    let mut backend = Backend::Remote(remote(&d).await);
    let mut approvals = backend.take_approvals().expect("canal de approvals");

    let agent = agent_with_scope(&d, "s1").await;
    let copy = spawn_agent_copy(agent);

    let req = tokio::time::timeout(Duration::from_secs(5), approvals.recv())
        .await
        .expect("approval llega")
        .expect("canal vivo");
    assert_eq!(req.op, "copy");
    assert_eq!(req.session.as_deref(), Some("s1"));

    backend
        .policy_decide(req.approval_id, true)
        .await
        .expect("decide approve");
    let res = tokio::time::timeout(Duration::from_secs(5), copy)
        .await
        .expect("no cuelga")
        .expect("join");
    assert!(res.expect("aprobada procede").task_id.get() > 0);
}

/// El camino de RESYNC del T5: un `RemoteBackend` que conecta DESPUÉS del
/// broadcast recibe la pendiente vía `policy.pending` (`ttl_ms` 0 =
/// desconocido) y puede denegarla.
#[tokio::test]
async fn backend_tardio_resincroniza_pendientes_y_deniega() {
    let d = spawn_daemon_ask().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/src.txt", b"hola").await;

    let agent = agent_with_scope(&d, "s1").await;
    let copy = spawn_agent_copy(agent);
    // Sincroniza: espera a que el Ask esté REGISTRADO en el daemon antes de
    // conectar el backend (sin esto, la pendiente podría llegarle por
    // broadcast con ttl real y el assert de resync sería flaky — MAJOR-2 del
    // rust-reviewer). Poll con un humano crudo a `policy.pending`.
    {
        let mut probe = norte_core::daemon::Client::connect(&d.socket)
            .await
            .expect("connect probe");
        probe
            .initialize(ClientInfo {
                name: "probe".into(),
                version: "0.0.0".into(),
            })
            .await
            .expect("initialize probe");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let listed: norte_proto::methods::PolicyPendingResult = probe
                .call(norte_proto::methods::POLICY_PENDING, &serde_json::json!({}))
                .await
                .expect("policy.pending");
            if !listed.pending.is_empty() {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "el Ask nunca llegó a pendiente"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // El probe muere aquí: al conectar el backend, el resync es el único
        // camino posible para la pendiente ya registrada.
    }

    // El frontend conecta TARDE: la pendiente le llega por el resync.
    let mut backend = Backend::Remote(remote(&d).await);
    let mut approvals = backend.take_approvals().expect("canal de approvals");
    let req = tokio::time::timeout(Duration::from_secs(5), approvals.recv())
        .await
        .expect("resync entrega la pendiente")
        .expect("canal vivo");
    assert_eq!(req.op, "copy");
    assert_eq!(req.ttl_ms, 0, "TTL desconocido en el resync");

    backend
        .policy_decide(req.approval_id, false)
        .await
        .expect("decide deny");
    let res = tokio::time::timeout(Duration::from_secs(5), copy)
        .await
        .expect("no cuelga")
        .expect("join");
    let err = res.expect_err("denegada");
    assert!(
        matches!(
            err,
            norte_core::daemon::ClientError::Rpc(ref rpc)
                if matches!(rpc.data, Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "not-approved")
        ),
        "PolicyDenied not-approved, fue {err:?}"
    );
}
