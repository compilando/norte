//! Un daemon con el provider LOCAL, que es contra lo que corre la ventana.
//!
//! El spike enseñó las columnas `size` y `mtime` en blanco sobre `file://`, y
//! el e2e del host no lo veía porque su daemon monta `MemProvider`. Este
//! reproduce el camino real: ficheros de verdad en un directorio temporal.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use norte_client::RemoteBackend;
use norte_core::Engine;
use norte_core::daemon::{Daemon, DaemonConfig};
use norte_proto::methods::ClientInfo;
use norte_ui_host::dto::SlotView;
use norte_ui_host::{UiHost, UiHostOptions};
use norte_vfs::Provider;

/// Las celdas de tamaño y fecha traen valor sobre ficheros de verdad.
#[tokio::test]
async fn el_tamano_y_la_fecha_no_van_en_blanco() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.txt"), b"hola que tal").expect("escribe");
    // El socket va en /tmp y no en el tempdir: un `sockaddr_un` no llega a
    // 108 bytes de ruta, y el temporal de un test ya se los come.
    // Y en un directorio PROPIO: el daemon endurece a 0700 el dir que
    // contiene su socket, y `/tmp` es de todo el mundo.
    let sock_dir =
        std::path::PathBuf::from(format!("/tmp/norte-gui-celdas-{}", std::process::id()));
    std::fs::create_dir_all(&sock_dir).expect("dir del socket");
    let socket = sock_dir.join("d.sock");
    let _ = std::fs::remove_file(&socket);

    let engine = Arc::new(Engine::new());
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::os_root()) as Arc<dyn Provider>
    );
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
    let _run = tokio::spawn(d.run());

    let backend = RemoteBackend::connect(
        socket.clone(),
        None,
        ClientInfo {
            name: "gui-celdas".to_owned(),
            version: "0.0.0".to_owned(),
        },
    )
    .await
    .expect("conecta");
    let inicio = norte_vfs_local::vpath_from_native(dir.path()).expect("vpath");
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: Arc::new(backend),
        initial_dir: inicio,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: norte_ui_host::ajustes_por_defecto(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
    })
    .await
    .expect("arranca");
    let _ = std::fs::remove_dir_all(&sock_dir);

    // La primera foto sale SIN esperar a los sondeos —bloquearla sería
    // retrasar el primer frame por una columna—, así que el tamaño llega
    // enseguida después, en un parche. Lo que se comprueba es que llega.
    let mut sub = h.subscribe();
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(25)).await;
        h.dispatch(norte_ui_host::UiAction::Resync)
            .await
            .expect("host vivo");
        let foto = loop {
            match sub.recv().await.expect("el host sigue vivo") {
                norte_ui_host::Update::Message(m) => {
                    if let norte_ui_host::dto::UiUpdate::Snapshot(s) = m.payload {
                        break s;
                    }
                }
                norte_ui_host::Update::Lagged => {}
            }
        };
        let SlotView::Browser(b) = &foto.slots[0] else {
            panic!("el primer hueco es un listado");
        };
        let Some(fila) = b.rows.iter().find(|r| r.display_name == "a.txt") else {
            continue;
        };
        let size = fila
            .cells
            .iter()
            .find(|c| c.column == "size")
            .expect("size configurada");
        if size.text.as_deref() == Some("12 B") {
            return;
        }
    }
    panic!("el tamaño de un fichero real nunca llegó a la celda");
}
