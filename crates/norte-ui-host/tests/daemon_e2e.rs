//! El host contra un daemon DE VERDAD.
//!
//! Los demás tests usan un backend de tabla, que es lo que los hace
//! deterministas. Este hace lo contrario a propósito: levanta un daemon real
//! sobre un socket temporal, conecta el SDK y comprueba que lo que el host
//! proyecta sale de un JSON-RPC que ha ido y ha vuelto — listado inicial,
//! navegación, y la sesión escrita al cerrar.
//!
//! Sin pantalla y sin Node: el host es útil a un test headless antes de que
//! exista renderer alguno, que era la condición de la fase 2.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use norte_client::RemoteBackend;
use norte_core::Engine;
use norte_core::daemon::{Daemon, DaemonConfig};
use norte_proto::VPath;
use norte_proto::methods::ClientInfo;
use norte_testkit::MemProvider;
use norte_ui_host::action::UiAction;
use norte_ui_host::dto::{SlotView, UiUpdate};
use norte_ui_host::{UiHost, UiHostOptions, UiSubscription, Update, ViewSnapshot};
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire de test")
}

struct DaemonDePrueba {
    socket: std::path::PathBuf,
    /// El provider de memoria que hay DETRÁS del daemon, para que un test
    /// pueda sembrar algo más que el arbolito de arranque.
    mem: Arc<MemProvider>,
    _run: tokio::task::JoinHandle<Result<(), norte_core::daemon::DaemonError>>,
    _dir: tempfile::TempDir,
}

async fn escribe(mem: &MemProvider, wire: &str, contenido: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write abre");
    sink.write(Bytes::copy_from_slice(contenido))
        .await
        .expect("chunk entra");
    sink.commit().await.expect("commit publica");
}

/// Un daemon sobre un provider en memoria con un arbolito dentro.
async fn daemon() -> DaemonDePrueba {
    let dir = tempfile::tempdir().expect("tempdir");
    let mem = Arc::new(MemProvider::new());
    mem.mkdir(&vp("mem:///casa")).await.expect("mkdir casa");
    mem.mkdir(&vp("mem:///casa/docs"))
        .await
        .expect("mkdir docs");
    escribe(&mem, "mem:///casa/notas.txt", b"hola").await;
    escribe(&mem, "mem:///casa/docs/a.md", b"# a").await;

    let socket = dir.path().join("d.sock");
    let engine = Arc::new(Engine::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let d = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: Some(dir.path().to_path_buf()),
            state_dir: Some(dir.path().to_path_buf()),
        },
    )
    .await
    .expect("bind");
    DaemonDePrueba {
        socket,
        mem,
        _run: tokio::spawn(d.run()),
        _dir: dir,
    }
}

async fn host_contra(d: &DaemonDePrueba) -> (UiHost, ViewSnapshot) {
    let backend = RemoteBackend::connect(
        d.socket.clone(),
        None,
        ClientInfo {
            name: "ui-host-e2e".into(),
            version: "0.0.0".into(),
        },
    )
    .await
    .expect("conecta");
    UiHost::start(UiHostOptions {
        backend: Arc::new(backend),
        initial_dir: vp("mem:///casa"),
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        // La fila `..` apagada: estos tests razonan sobre índices de
        // listado, y una fila más al principio los desplazaría todos sin
        // decir nada de lo que prueban.
        settings: {
            let mut cfg = norte_ui_host::ajustes_por_defecto();
            cfg.common.ui_parent_entry = Some(false);
            cfg
        },
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("arranca")
}

fn listado(snap: &ViewSnapshot) -> &norte_ui_host::dto::BrowserSlotView {
    let SlotView::Browser(b) = snap
        .slots
        .iter()
        .find(|s| matches!(s, SlotView::Browser(_)))
        .expect("hay listado")
    else {
        unreachable!("filtrado arriba")
    };
    b
}

async fn siguiente_foto(sub: &mut UiSubscription) -> ViewSnapshot {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(Duration::from_secs(5), sub.recv())
            .await
            .expect("una foto antes del plazo")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente
            && let UiUpdate::Snapshot(s) = m.payload
        {
            return *s;
        }
    }
    panic!("no llegó ninguna foto");
}

/// El listado inicial viene del daemon, ordenado por la capa compartida.
#[tokio::test]
async fn el_listado_inicial_llega_del_daemon() {
    let d = daemon().await;
    let (_h, snap) = host_contra(&d).await;
    let b = listado(&snap);
    let nombres: Vec<&str> = b.rows.iter().map(|r| r.display_name.as_str()).collect();
    assert_eq!(nombres, vec!["docs", "notas.txt"], "directorios primero");
    assert!(b.path_display.ends_with("/casa"));
}

/// Navegar de verdad: entrar en un directorio y volver, contra el daemon.
#[tokio::test]
async fn navegar_contra_el_daemon() {
    let d = daemon().await;
    let (h, snap) = host_contra(&d).await;
    let docs = listado(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("docs está")
        .key;
    let epoca = listado(&snap).generation;
    let mut sub = h.subscribe();

    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs,
        generation: epoca,
    })
    .await
    .expect("host vivo");
    let dentro = siguiente_foto(&mut sub).await;
    assert!(listado(&dentro).path_display.ends_with("/casa/docs"));
    assert_eq!(listado(&dentro).rows.len(), 1);

    h.dispatch(UiAction::History {
        slot_id: 1,
        back: true,
    })
    .await
    .expect("host vivo");
    let fuera = siguiente_foto(&mut sub).await;
    assert!(listado(&fuera).path_display.ends_with("/casa"));
}

/// La sesión se escribe al cerrar, y el daemon la devuelve en la siguiente
/// vida del host: es la prueba de que el documento cruza el wire entero.
#[tokio::test]
async fn la_sesion_sobrevive_al_cierre() {
    let d = daemon().await;
    let (h, snap) = host_contra(&d).await;
    let docs = listado(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("docs está")
        .key;
    let epoca = listado(&snap).generation;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs,
        generation: epoca,
    })
    .await
    .expect("host vivo");
    siguiente_foto(&mut sub).await;
    let informe = h.shutdown().await.expect("apaga");
    assert!(!informe.incomplete, "la dueña escribió");

    // Otra vida del host, contra el MISMO daemon.
    let (_h2, otra) = host_contra(&d).await;
    assert!(
        listado(&otra).path_display.ends_with("/casa/docs"),
        "arranca donde lo dejó la vida anterior: {}",
        listado(&otra).path_display
    );
}

/// Las columnas configuradas llegan CON su valor.
///
/// El spike de Tauri las enseñó vacías contra un daemon de verdad, y el
/// backend de tabla no lo veía: sus entradas se construyen a mano y siempre
/// traen tamaño. Lo que cruza el wire es otra cosa.
#[tokio::test]
async fn las_celdas_traen_valor_contra_el_daemon() {
    let d = daemon().await;
    let (_h, snap) = host_contra(&d).await;
    let b = listado(&snap);
    let fichero = b
        .rows
        .iter()
        .find(|r| r.display_name == "notas.txt")
        .expect("el fichero está");
    let size = fichero
        .cells
        .iter()
        .find(|c| c.column == "size")
        .expect("la columna size está configurada");
    assert!(
        size.text.is_some(),
        "un fichero con tamaño trae su celda: {:?}",
        fichero.cells
    );
}

/// Lo que lanza OTRO cliente del mismo daemon aparece en este tablero, dicho
/// como ajeno, y esta ventana puede pararlo.
///
/// Es la prueba que el backend de tabla no puede dar: ahí las tasks ajenas
/// las empuja el test por un canal que él mismo abre. Aquí la task nace en
/// otra conexión, el daemon la difunde, y el SDK decide que es ajena — que es
/// el camino que de verdad recorre una copia lanzada desde el TUI mientras la
/// ventana está abierta.
#[tokio::test]
async fn una_task_de_otro_cliente_se_ve_y_se_puede_parar() {
    let d = daemon().await;
    let (h, _snap) = host_contra(&d).await;
    let mut sub = h.subscribe();

    // El «otro frontend»: otra conexión humana al mismo daemon.
    let otro = RemoteBackend::connect(
        d.socket.clone(),
        None,
        ClientInfo {
            name: "otro-frontend-e2e".into(),
            version: "0.0.0".into(),
        },
    )
    .await
    .expect("conecta");
    // La copia tiene que seguir VIVA cuando su primer progreso se difunde:
    // el SDK no anuncia como ajena una task que ya llegó terminal —no habría
    // a qué suscribirse—, así que una copia instantánea contra un provider
    // en memoria no probaría nada. El provider se frena a propósito.
    escribe(
        &d.mem,
        "mem:///casa/grande.bin",
        &vec![7u8; 4 * 1024 * 1024],
    )
    .await;
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(30)));
    let task = otro
        .transfer(
            norte_client::Transfer::Copy,
            &vp("mem:///casa/grande.bin"),
            &vp("mem:///casa/copia.bin"),
            norte_client::TransferOptions::default(),
        )
        .await
        .expect("encola la copia");

    // El tablero de ESTA ventana la enseña, y dice que no es suya.
    let mut vista = None;
    for _ in 0..40 {
        let siguiente = tokio::time::timeout(Duration::from_secs(5), sub.recv())
            .await
            .expect("una actualización antes del plazo")
            .expect("el host sigue vivo");
        let tasks = match siguiente {
            Update::Message(m) => match m.payload {
                UiUpdate::Snapshot(s) => s.tasks.clone(),
                UiUpdate::Patch(p) => p
                    .changes
                    .iter()
                    .find_map(|c| match c {
                        norte_ui_host::dto::ViewChange::Tasks { tasks } => Some(tasks.clone()),
                        _ => None,
                    })
                    .unwrap_or_default(),
                UiUpdate::Notice(_) => Vec::new(),
            },
            Update::Lagged => Vec::new(),
        };
        if let Some(t) = tasks.iter().find(|t| t.task_id == task.id().get()) {
            vista = Some(t.clone());
            break;
        }
    }
    let vista = vista.expect("la task del otro cliente llegó al tablero");
    assert!(vista.foreign, "y el tablero dice que es ajena: {vista:?}");

    // Y se puede cancelar desde aquí: es la misma sesión, así que pararla es
    // legítimo — y el tablero que la enseña sin poder tocarla sería una
    // ventana mirando arder.
    let ack = h
        .dispatch(UiAction::CancelTask {
            task_id: vista.task_id,
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, norte_ui_host::bridge::ActionAck::Applied { .. }),
        "{ack:?}"
    );
}

/// #270 — `norte_ui_host::backend::transferir` manda
/// `TransferOptions { on_collision, ..Default::default() }`, o sea
/// `SymlinkPolicy::Preserve` y `ResumePolicy::Off`. Que esos dos lleguen así
/// lo AFIRMABA un comentario y nada más: el doble de test del host implementa
/// `HostBackend` directamente y no pasa por este impl.
///
/// `Preserve` es lo que impide que una copia DEREFERENCIE un symlink hostil
/// del origen, así que se comprueba por su efecto y no por serialización: el
/// enlace sigue siendo un enlace del otro lado, con su target intacto. Si el
/// default se cayera del camino, el destino sería un fichero con el contenido
/// del target — que es exactamente la fuga que `Preserve` existe para evitar.
#[tokio::test]
async fn una_copia_del_host_preserva_los_symlinks_del_origen() {
    use norte_proto::CollisionPolicy;
    use norte_ui_host::backend::HostBackend;

    let d = daemon().await;
    d.mem
        .mkdir(&vp("mem:///casa/src"))
        .await
        .expect("mkdir src");
    escribe(&d.mem, "mem:///casa/src/real.txt", b"secreto").await;
    d.mem
        .symlink(
            &vp("mem:///casa/src/enlace"),
            b"real.txt",
            norte_vfs::SymlinkKind::File,
        )
        .await
        .expect("symlink");

    let backend = RemoteBackend::connect(
        d.socket.clone(),
        None,
        ClientInfo {
            name: "ui-host-e2e-symlink".into(),
            version: "0.0.0".into(),
        },
    )
    .await
    .expect("conecta");

    let task = backend
        .copy(
            vp("mem:///casa/src"),
            vp("mem:///casa/dst"),
            CollisionPolicy::Fail,
        )
        .await
        .expect("encola la copia");
    let mut progreso = task.progress;
    loop {
        let estado = progreso.borrow_and_update().state.clone();
        if estado.is_terminal() {
            assert_eq!(estado, norte_proto::TaskState::Completed, "{estado:?}");
            break;
        }
        tokio::time::timeout(Duration::from_secs(10), progreso.changed())
            .await
            .expect("la copia termina antes del plazo")
            .expect("el canal de progreso sigue vivo");
    }

    let copiado = d
        .mem
        .stat(&vp("mem:///casa/dst/enlace"))
        .await
        .expect("el enlace llegó al destino");
    assert_eq!(
        copiado.kind,
        norte_proto::EntryKind::Symlink,
        "la copia DEREFERENCIÓ el enlace: `SymlinkPolicy::Preserve` no llegó al wire"
    );
}
