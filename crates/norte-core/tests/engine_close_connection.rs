//! `Engine::close_connection` (#140): desconectar SUELTA la sesión.
//!
//! Sin esto, «desconectar» solo movía el panel a otro sitio y el socket seguía
//! abierto hasta que la sesión venciera sola.

use std::sync::Arc;

use norte_core::Engine;
use norte_proto::VPath;
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

/// Un provider de PROCESO —registrado por su scheme entero, como el local— no
/// es una sesión: no hay nada que soltar, y contestar que sí sería mentir sobre
/// algo que sigue exactamente igual.
#[tokio::test]
async fn un_provider_de_proceso_no_se_cierra() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    assert!(
        !engine.close_connection(&vp("mem:///casa")),
        "no había sesión que cerrar"
    );
    // Y sigue sirviendo: cerrar lo que no era una sesión no puede dejar el
    // scheme inservible.
    mem.mkdir(&vp("mem:///casa")).await.expect("mkdir");
    assert!(engine.stat(&vp("mem:///casa")).await.is_ok());
}

/// Cerrar lo que no está es `false` y no un error: quien desconecta quiere
/// quedarse sin conexión, y ya lo está.
#[tokio::test]
async fn cerrar_lo_que_no_hay_no_es_un_error() {
    let engine = Engine::new();
    assert!(!engine.close_connection(&vp("sftp://host/casa")));
}
