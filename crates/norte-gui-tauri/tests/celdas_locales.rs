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

/// `[ui.columns]` con `size` en formato EXACTO.
///
/// La otra mitad de #108 que esta ventana ignoraba: pedía el estilo de
/// fábrica, así que un `format` configurado no hacía nada aquí mientras el
/// terminal sí lo honraba. Se elige `exact` porque su respuesta es un número
/// comprobable —`12`— y la de fábrica para `size` es `iec`, o sea «12 B»: si
/// el estilo no se aplicara, el test lo diría en vez de agotar su bucle.
fn columnas_con_tamano_exacto() -> norte_frontend::columns::ColumnsSettings {
    let cfg = norte_config::ColumnsConfig {
        specs: [(
            "size".to_owned(),
            norte_config::ColumnSpec {
                width: None,
                align: None,
                format: Some("exact".to_owned()),
                header: None,
            },
        )]
        .into_iter()
        .collect(),
        ..norte_config::ColumnsConfig::default()
    };
    norte_frontend::columns::ColumnsSettings::resolve(&cfg)
}

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
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: norte_ui_host::ajustes_por_defecto(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        // Sin perfil: este test mira las celdas de un listado local, y un
        // perfil activo no cambia lo que un `stat` devuelve.
        profile: None,
        columns: columnas_con_tamano_exacto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
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
        // `12` y no «12 B»: con `format = "exact"` puesto, la celda tiene que
        // salir en exacto. Si el estilo configurado no se aplicara, aquí
        // llegaría el de fábrica (`iec`) y este bucle se agotaría.
        if size.text.as_deref() == Some("12") {
            return;
        }
        assert_ne!(
            size.text.as_deref(),
            Some("12 B"),
            "la celda salió con el formato de FÁBRICA: `[ui.columns]` no se \
             está aplicando en la ventana"
        );
    }
    panic!("el tamaño de un fichero real nunca llegó a la celda");
}
