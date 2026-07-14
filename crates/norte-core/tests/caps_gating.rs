//! Gating de capabilities en el engine (fase 10a): el engine consulta caps y
//! degrada/gatea correcto en el camino cross-provider. "Sin sorpresas" =
//! ninguna cap mentida y ninguna degradación por cuenta propia.
//!
//! Se usa un archive READ-ONLY (`zip+mem`, ADR 0018) como provider que NO
//! declara TRASH ni escritura: es el caso honesto para probar que
//! - un `DeleteMode::Trash` sin cap `TRASH` NO se degrada a permanente (ADR 0009 B2),
//! - una copia HACIA dentro de un `READ_ONLY` se rechaza limpio (`Unsupported`).

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use norte_core::Engine;
use norte_proto::{DeleteMode, Error, TaskState, VPath};
use norte_testkit::{MemProvider, ZipSmith};
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido de test")
}

/// Engine con un `MemProvider` que contiene `a.zip` (sembrado con `ZipSmith`)
/// y un `src.txt` plano como posible origen de copia.
async fn engine_with_zip(entries: &[(&[u8], &[u8])]) -> Engine {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());

    let mut smith = ZipSmith::new();
    for (name, content) in entries {
        smith = smith.file(name, content);
    }
    let zip = smith.build();

    for (path, bytes) in [("a.zip", zip.as_slice()), ("src.txt", b"origen".as_slice())] {
        let mut sink = mem
            .write(&vp(&format!("mem:///{path}")))
            .await
            .expect("write");
        sink.write(Bytes::copy_from_slice(bytes))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }
    engine.register_provider(mem as Arc<dyn Provider>);
    engine
}

/// El resultado de una op mutante rechazada: o `Err(Unsupported)` upfront, o una
/// Task que termina `Failed`. En ambos casos NO es `Completed`.
async fn assert_rejected(result: Result<norte_core::TaskHandle, Error>) {
    match result {
        Err(e) => assert_eq!(
            e,
            Error::Unsupported,
            "rechazo upfront debe ser Unsupported"
        ),
        Ok(handle) => assert!(
            matches!(handle.join().await, TaskState::Failed { .. }),
            "la task de una mutación vetada debe terminar Failed, no Completed"
        ),
    }
}

#[tokio::test]
async fn trash_on_readonly_archive_does_not_degrade() {
    // Archive es READ_ONLY → no declara TRASH. Un delete Trash NO debe
    // degradar a permanente por su cuenta (ADR 0009 B2): falla limpio.
    let engine = engine_with_zip(&[(b"hola.txt", b"contenido")]).await;
    let victim = vp("zip+mem:///a.zip/!/hola.txt");

    assert_rejected(engine.delete_with(&victim, DeleteMode::Trash).await).await;

    // Y el fichero sigue DENTRO del zip (no se borró ni permanentizó).
    let mut stream = engine.read(&victim, None).await.expect("sigue legible");
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk"));
    }
    assert_eq!(out, b"contenido");
}

#[tokio::test]
async fn copy_into_readonly_archive_is_rejected() {
    // Copiar HACIA dentro de un archive (READ_ONLY) = rechazo limpio, jamás
    // una escritura a medias en el contenedor.
    let engine = engine_with_zip(&[(b"hola.txt", b"x")]).await;
    let dst = vp("zip+mem:///a.zip/!/nuevo.txt");

    assert_rejected(engine.copy(&vp("mem:///src.txt"), &dst).await).await;
}
