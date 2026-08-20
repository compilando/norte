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
            plugins_dir: None,
            state_dir: Some(dir.path().to_path_buf()),
        },
    )
    .await
    .expect("bind");
    DaemonDePrueba {
        socket,
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
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        columns: norte_ui_host::columnas_por_defecto(),
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
            return s;
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
    let mut sub = h.subscribe();

    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs,
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
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs,
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
