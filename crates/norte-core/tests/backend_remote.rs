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
use norte_proto::methods::{ClientInfo, FsSearchParams, SearchHits};
use norte_proto::{ByteRange, CapabilityFlags, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;
use tokio::sync::mpsc;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido de test")
}

/// Drena el canal de hits hasta su CIERRE (con tope de tiempo: si el route no
/// se retirase, esto colgaría). Devuelve los paths de display, ordenados.
async fn drain_search(mut rx: mpsc::Receiver<SearchHits>) -> Vec<String> {
    let mut got = Vec::new();
    while let Some(hits) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("un lote o el cierre del canal antes del timeout")
    {
        for e in hits.entries {
            got.push(e.path.display_lossy());
        }
    }
    got.sort();
    got
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
            plugins_dir: None,
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

/// #93: `skipped` del contenedor llega por AMBOS modos del Backend — el
/// remoto lo trae la primera página de `fs.list`; el embebido consulta el
/// provider. Sin omitidas: `None` (el badge no se pinta).
#[tokio::test]
async fn list_skipped_llega_por_ambos_modos() {
    let mem = Arc::new(MemProvider::new().with_list_skipped(3));
    let d = spawn_daemon_with(tempfile::tempdir().expect("tempdir"), Arc::clone(&mem)).await;
    write_file(&d.mem, "mem:///a.txt", b"x").await;

    let remoto = Backend::Remote(remote(&d).await);
    let (entries, skipped) = remoto
        .list_with_skipped(&vp("mem:///"))
        .await
        .expect("list remoto");
    assert_eq!(entries.len(), 1);
    assert_eq!(skipped, Some(3), "remoto: viaja en la página de fs.list");

    let engine = Arc::new(Engine::new());
    engine.register_provider(mem as Arc<dyn Provider>);
    let embebido = Backend::Embedded(engine);
    let (_, skipped) = embebido
        .list_with_skipped(&vp("mem:///"))
        .await
        .expect("list embebido");
    assert_eq!(skipped, Some(3), "embebido: consulta el provider");

    // Provider sin omitidas: None en ambos modos.
    let d2 = spawn_daemon().await;
    write_file(&d2.mem, "mem:///b.txt", b"y").await;
    let remoto2 = Backend::Remote(remote(&d2).await);
    let (_, skipped) = remoto2
        .list_with_skipped(&vp("mem:///"))
        .await
        .expect("list remoto sin omitidas");
    assert_eq!(skipped, None);
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

    // stat (fs.stat, M4 Lua T3)
    let entry = backend.stat(&vp("mem:///dst.bin")).await.expect("stat");
    assert_eq!(entry.size, Some(16));
}

/// Un clon de `Backend::Remote` comparte conexión/watches pero NO puede
/// robarle al dueño original los canales one-shot (`take_foreign_tasks`,
/// `take_conn_events`, `take_approvals`): si el clon los tomara, la TUI
/// dueña se quedaría sin canal y los `ask` de policy caducarían a `deny`
/// en silencio (MAJOR del rust-reviewer sobre e408373). El orden importa:
/// el clon intenta robar ANTES que el dueño reclame los suyos.
/// #104: fs.mkdir por el wire — Task remota hasta terminal, y el dir existe.
#[tokio::test]
async fn remote_mkdir_como_task() {
    let d = spawn_daemon().await;
    let backend = Backend::Remote(remote(&d).await);
    let task = backend.mkdir(&vp("mem:///wire-dir")).await.expect("mkdir");
    assert_eq!(join_ref(task).await, TaskState::Completed);
    assert!(d.mem.stat(&vp("mem:///wire-dir")).await.is_ok());
}

#[tokio::test]
async fn un_clon_no_roba_los_canales_del_dueno() {
    let d = spawn_daemon().await;
    let mut backend = Backend::Remote(remote(&d).await);
    let mut clone = backend.clone();

    assert!(clone.take_foreign_tasks().is_none());
    assert!(clone.take_conn_events().is_none());
    assert!(clone.take_approvals().is_none());

    assert!(backend.take_foreign_tasks().is_some());
    assert!(backend.take_conn_events().is_some());
    assert!(backend.take_approvals().is_some());
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
            plugins_dir: None,
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

/// El brazo de AGENTE sigue siendo agente después de reconectar.
///
/// `establish` corre también en cada reconexión, y el daemon fija el actor en
/// el handshake sin recordar el de la conexión anterior: si la sesión no
/// viajase ahí, el backend volvería como `Actor::User` —allow-all— y nada se
/// pondría rojo, porque el síntoma del fallo es que las lecturas EMPIEZAN a
/// funcionar. De ahí que la aserción de después de `Restored` sea la que
/// importa: un `Ok` ahí es el actor blanqueándose solo.
#[tokio::test]
async fn el_brazo_de_agente_sigue_siendo_agente_tras_reconectar() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let mem = Arc::new(MemProvider::new());
    write_file(&mem, "mem:///f", b"x").await;
    let (shutdown1, run1) = bind_at(Arc::clone(&mem), socket.clone()).await;

    let mut agente = Backend::Remote(
        RemoteBackend::connect_as_agent(
            socket.clone(),
            ClientInfo {
                name: "backend-test".into(),
                version: "0.0.0".into(),
            },
            "sesion.agente".into(),
        )
        .await
        .expect("connect_as_agent"),
    );
    let mut events = agente.take_conn_events().expect("canal de eventos");
    // Sin scope concedido, el gate de lectura de agente lo veda. Un `User`
    // sobre este mismo daemon lista sin problema — lo comprueba
    // `reconexion_avisa_y_recupera`, que es el control de este test.
    assert!(
        matches!(
            agente.list(&vp("mem:///")).await,
            Err(norte_proto::Error::PolicyDenied { .. })
        ),
        "de entrada la conexión ya tiene que ser de agente"
    );

    // Muere el daemon…
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

    // …y vuelve otro en el MISMO socket: el brazo reconecta solo.
    let (_shutdown2, _run2) = bind_at(Arc::clone(&mem), socket.clone()).await;
    let ev = tokio::time::timeout(Duration::from_secs(15), events.recv())
        .await
        .expect("Restored antes del timeout (backoff ≤5 s)")
        .expect("canal vivo");
    assert_eq!(ev, ConnEvent::Restored);
    let tras_reconectar = agente.list(&vp("mem:///")).await;
    assert!(
        matches!(
            tras_reconectar,
            Err(norte_proto::Error::PolicyDenied { .. })
        ),
        "tras reconectar SIGUE siendo agente; un Ok aquí es el actor \
         blanqueado a User, y fue {tras_reconectar:?}"
    );
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

// ---------- fs.search remoto (live search T5) ----------

/// `Backend::search` remoto = misma superficie que el embebido: los hits
/// llegan por la notificación `search.hits`, la bomba del `RemoteBackend` los
/// enruta por `task_id` al `rx` que devuelve `search`, y el `rx` se cierra al
/// terminal (retirada del route con gracia). Mismo árbol y mismo resultado que
/// `embedded_search_stream_de_hits`.
#[tokio::test]
async fn remote_search_como_el_embebido() {
    let d = spawn_daemon().await;
    write_file(&d.mem, "mem:///a.rs", b"").await;
    write_file(&d.mem, "mem:///b.txt", b"").await;
    d.mem.mkdir(&vp("mem:///sub")).await.expect("mkdir");
    write_file(&d.mem, "mem:///sub/c.rs", b"").await;
    let backend = Backend::Remote(remote(&d).await);

    let (task, rx) = backend
        .search(FsSearchParams {
            root: vp("mem:///"),
            name_glob: Some("*.rs".into()),
            name_regex: None,
            content: None,
            content_regex: None,
            case_sensitive: false,
            max_hits: None,
        })
        .await
        .expect("search");

    let got = drain_search(rx).await;
    assert_eq!(
        got,
        vec![
            vp("mem:///a.rs").display_lossy(),
            vp("mem:///sub/c.rs").display_lossy()
        ]
    );
    assert_eq!(join_ref(task).await, TaskState::Completed);
}

/// M1 (encoding, MEDIA): el nombre hostil cruza el WIRE byte-EXACTO. Un
/// fichero de nombre `[0xFF, 0xFE]` (no-UTF8, jamás decodificable) sembrado en
/// el árbol vuelve por el daemon real con sus bytes intactos —el mismo assert
/// que el embebido, ahora cruzando la serialización JSON-RPC (regla dura §1:
/// nombres = bytes, jamás se asume UTF-8).
#[tokio::test]
async fn remote_search_preserva_nombre_no_utf8_byte_exacto() {
    let d = spawn_daemon().await;
    let seg = norte_proto::Segment::new(vec![0xFF, 0xFE]).expect("segmento válido");
    let hostil = MemProvider::root().join(seg);
    let mut sink = d.mem.write(&hostil).await.expect("write abre");
    sink.write(Bytes::new()).await.expect("chunk vacío");
    sink.commit().await.expect("commit publica");
    let backend = Backend::Remote(remote(&d).await);

    let (task, mut rx) = backend
        .search(FsSearchParams {
            root: vp("mem:///"),
            name_glob: Some("*".into()),
            name_regex: None,
            content: None,
            content_regex: None,
            case_sensitive: false,
            max_hits: None,
        })
        .await
        .expect("search");

    let mut names: Vec<Vec<u8>> = Vec::new();
    while let Some(hits) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("un lote o el cierre antes del timeout")
    {
        for e in hits.entries {
            names.push(e.path.file_name().expect("con nombre").as_bytes().to_vec());
        }
    }
    assert_eq!(join_ref(task).await, TaskState::Completed);
    assert!(
        names.iter().any(|n| n.as_slice() == [0xFF, 0xFE]),
        "el nombre no-UTF8 sobrevivió el wire byte-exacto: {names:?}"
    );
}

/// Dos búsquedas CONCURRENTES en la MISMA conexión no mezclan sus lotes: el
/// enrutado por `task_id` entrega a cada `rx` solo SUS hits (subtrees y globs
/// disjuntos → cero solape observable si el enrutado es correcto).
#[tokio::test]
async fn remote_dos_busquedas_no_se_cruzan() {
    let d = spawn_daemon().await;
    d.mem.mkdir(&vp("mem:///da")).await.expect("mkdir da");
    d.mem.mkdir(&vp("mem:///db")).await.expect("mkdir db");
    write_file(&d.mem, "mem:///da/a1.rs", b"").await;
    write_file(&d.mem, "mem:///da/a2.rs", b"").await;
    write_file(&d.mem, "mem:///db/b1.txt", b"").await;
    let backend = Backend::Remote(remote(&d).await);

    let (t1, rx1) = backend
        .search(FsSearchParams {
            root: vp("mem:///da"),
            name_glob: Some("*.rs".into()),
            name_regex: None,
            content: None,
            content_regex: None,
            case_sensitive: false,
            max_hits: None,
        })
        .await
        .expect("search da");
    let (t2, rx2) = backend
        .search(FsSearchParams {
            root: vp("mem:///db"),
            name_glob: Some("*.txt".into()),
            name_regex: None,
            content: None,
            content_regex: None,
            case_sensitive: false,
            max_hits: None,
        })
        .await
        .expect("search db");

    let g1 = drain_search(rx1).await;
    let g2 = drain_search(rx2).await;
    assert_eq!(
        g1,
        vec![
            vp("mem:///da/a1.rs").display_lossy(),
            vp("mem:///da/a2.rs").display_lossy()
        ],
        "rx1 solo ve los .rs de da"
    );
    assert_eq!(
        g2,
        vec![vp("mem:///db/b1.txt").display_lossy()],
        "rx2 solo ve el .txt de db"
    );
    assert_eq!(join_ref(t1).await, TaskState::Completed);
    assert_eq!(join_ref(t2).await, TaskState::Completed);
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
            plugins_dir: None,
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

// ---------- #74: submit abandonado → rpc.cancel del dispatch en vuelo ------

/// Conector colgado con sonda de drop: `cancelled` se enciende cuando el
/// future del dial se DROPEA a mitad (la cancelación #47 del pool).
struct ProbedHangingConnector {
    started: std::sync::atomic::AtomicUsize,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}

struct DropProbe(Arc<std::sync::atomic::AtomicBool>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl norte_core::connect::RemoteConnector for ProbedHangingConnector {
    async fn connect(
        &self,
        _s: &str,
        _a: &str,
    ) -> Result<norte_core::connect::Connected, norte_proto::Error> {
        self.started
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _probe = DropProbe(self.cancelled.clone());
        std::future::pending().await
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

/// #74: dropear el future de un submit remoto EN VUELO envía `rpc.cancel`
/// (guard drop-based del backend, patrón #72) — el dispatch del daemon muere
/// PRE-efecto (aquí, cancelando el dial #47) y la Task jamás nace huérfana
/// sin canceller. Es la ventana del driver Lua que ABANDONA el run con el
/// submit en vuelo.
#[tokio::test]
async fn submit_abandonado_envia_rpc_cancel_y_mata_el_dispatch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let engine = Arc::new(Engine::new());
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let conn = Arc::new(ProbedHangingConnector {
        started: std::sync::atomic::AtomicUsize::new(0),
        cancelled: cancelled.clone(),
    });
    engine.set_connector(conn.clone());
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
    let _run = tokio::spawn(daemon.run());
    let backend = Backend::Remote(
        RemoteBackend::connect(
            socket,
            None,
            ClientInfo {
                name: "backend-test".into(),
                version: "0.0.0".into(),
            },
        )
        .await
        .expect("connect"),
    );

    let b2 = backend.clone();
    let submit = tokio::spawn(async move {
        b2.copy(
            &vp("sftp://h/a"),
            &vp("sftp://h/b"),
            norte_core::TransferOptions::default(),
        )
        .await
    });
    // El dispatch está EN el dial (fs.copy en vuelo, sin respuesta).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while conn.started.load(std::sync::atomic::Ordering::SeqCst) == 0 {
        assert!(tokio::time::Instant::now() < deadline, "el dial no arrancó");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // El caller ABANDONA el future del submit (p. ej. gracia del driver Lua
    // agotada): el guard debe enviar rpc.cancel con el id en vuelo.
    submit.abort();
    let _ = submit.await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !cancelled.load(std::sync::atomic::Ordering::SeqCst) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "el dispatch del daemon sigue vivo: el abandono no envió rpc.cancel (#74)"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

// ---------- sync.plan / sync.apply remotos (0.40.0, ADR 0049) ----------

/// Daemon con journal y spool: lo que hace falta para planificar y aplicar.
async fn spawn_daemon_sync() -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let journal = Arc::new(norte_core::SqliteJournal::new(
        norte_core::Journal::open_in_memory()
            .await
            .expect("journal"),
    ));
    let engine = Arc::new(Engine::with_journal(journal));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    engine.set_spool(norte_core::sync::Spool::new(dir.path()));
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
        _run: run,
        _dir: dir,
        mem,
    }
}

/// El ciclo entero por el brazo REMOTO, que es donde vive el enrutado: los dos
/// eventos del plan —`sync.steps` y `sync.plan_done`— viajan por el MISMO `rx`,
/// el cierre va el último, y el hash que trae ejecuta.
///
/// Que compartan canal no es un detalle de implementación: con dos mapas, el
/// orden entre un lote y el cierre dependería de cómo el runtime despierta dos
/// receptores, y un cliente podría aprobar el hash de un plan que todavía
/// estaba llegando.
#[tokio::test]
async fn remote_sync_plan_y_apply_como_el_embebido() {
    let d = spawn_daemon_sync().await;
    d.mem.mkdir(&vp("mem:///s")).await.expect("mkdir s");
    d.mem.mkdir(&vp("mem:///d")).await.expect("mkdir d");
    for i in 0..300 {
        write_file(&d.mem, &format!("mem:///s/f{i}.txt"), b"x").await;
    }
    let backend = Backend::Remote(remote(&d).await);

    let (task, mut rx) = backend
        .sync_plan(norte_proto::methods::SyncPlanParams {
            source: vp("mem:///s"),
            dest: vp("mem:///d"),
            mode: norte_proto::methods::SyncMode::Update,
            compare: norte_proto::methods::SyncCompareOptions::default(),
            on_unknown: norte_proto::methods::OnUnknown::Copy,
            include: None,
        })
        .await
        .expect("sync.plan remoto");

    let mut pasos = 0usize;
    let mut done = None;
    while let Some(event) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("un evento o el cierre del canal antes del timeout")
    {
        match event {
            norte_core::sync::SyncPlanEvent::Steps(b) => {
                assert!(
                    done.is_none(),
                    "un lote DESPUÉS del cierre: el orden es la cola de UN canal"
                );
                pasos += b.steps.len();
            }
            norte_core::sync::SyncPlanEvent::Done(d) => {
                assert!(done.is_none(), "dos cierres para un plan");
                done = Some(d);
            }
        }
    }
    assert_eq!(join_ref(task).await, TaskState::Completed);
    let done = done.expect("el plan cerró con su hash");
    assert_eq!(pasos, 300);
    assert_eq!(done.counts.copy, 300);

    let applying = backend
        .sync_apply(&done.plan_hash)
        .await
        .expect("sync.apply");
    let id = applying.id();
    assert_eq!(join_ref(applying).await, TaskState::Completed);
    let report = backend.sync_report(id).await.expect("sync.report");
    assert_eq!(report.done, 300);
    assert_eq!(report.failed, 0, "{:?}", report.failures);
    assert!(report.batch_id.is_some());

    // Y el plan se gastó: el mismo hash no vuelve a ejecutar.
    assert!(matches!(
        backend.sync_apply(&done.plan_hash).await,
        Err(norte_proto::Error::PlanStale)
    ));
}

/// Un daemon SIN spool contesta `Unsupported` por el wire y el brazo remoto lo
/// entrega tal cual: fail-closed también a través del socket, y distinguible de
/// un fallo real.
#[tokio::test]
async fn remote_sync_plan_sin_spool_es_unsupported() {
    let d = spawn_daemon().await;
    d.mem.mkdir(&vp("mem:///s")).await.expect("mkdir s");
    d.mem.mkdir(&vp("mem:///d")).await.expect("mkdir d");
    let backend = Backend::Remote(remote(&d).await);

    assert!(matches!(
        backend
            .sync_plan(norte_proto::methods::SyncPlanParams {
                source: vp("mem:///s"),
                dest: vp("mem:///d"),
                mode: norte_proto::methods::SyncMode::Update,
                compare: norte_proto::methods::SyncCompareOptions::default(),
                on_unknown: norte_proto::methods::OnUnknown::Copy,
                include: None,
            })
            .await,
        Err(norte_proto::Error::Unsupported)
    ));
}
