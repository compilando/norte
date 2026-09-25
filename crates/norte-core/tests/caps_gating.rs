//! Capability gating in the engine (phase 10a): the engine queries caps and
//! degrades/gates correctly on the cross-provider path. "No surprises" =
//! no lied-about cap and no degradation on its own initiative.
//!
//! A READ-ONLY archive (`zip+mem`, ADR 0018) is used as the provider that does
//! NOT declare TRASH or writing: it is the honest case to test that
//! - a `DeleteMode::Trash` without the `TRASH` cap does NOT degrade to permanent (ADR 0009 B2),
//! - a copy INTO a `READ_ONLY` provider is rejected cleanly (`Unsupported`).

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use norte_core::Engine;
use norte_proto::{DeleteMode, Error, TaskState, VPath};
use norte_testkit::{MemProvider, ZipSmith};
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid test wire")
}

/// Engine with a `MemProvider` that holds `a.zip` (seeded with `ZipSmith`)
/// and a plain `src.txt` as a possible copy source.
async fn engine_with_zip(entries: &[(&[u8], &[u8])]) -> Engine {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());

    let mut smith = ZipSmith::new();
    for (name, content) in entries {
        smith = smith.file(name, content);
    }
    let zip = smith.build();

    for (path, bytes) in [("a.zip", zip.as_slice()), ("src.txt", b"source".as_slice())] {
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

/// The result of a rejected mutating op: either `Err(Unsupported)` upfront, or a
/// Task that ends `Failed`. In both cases it is NOT `Completed`.
async fn assert_rejected(result: Result<norte_core::TaskHandle, Error>) {
    match result {
        Err(e) => assert_eq!(
            e,
            Error::Unsupported,
            "an upfront rejection must be Unsupported"
        ),
        Ok(handle) => assert!(
            matches!(handle.join().await, TaskState::Failed { .. }),
            "the task of a vetoed mutation must end Failed, not Completed"
        ),
    }
}

#[tokio::test]
async fn trash_on_readonly_archive_does_not_degrade() {
    // The archive is READ_ONLY → it does not declare TRASH. A Trash delete must
    // NOT degrade to permanent on its own (ADR 0009 B2): it fails cleanly.
    let engine = engine_with_zip(&[(b"hello.txt", b"contents")]).await;
    let victim = vp("zip+mem:///a.zip/!/hello.txt");

    assert_rejected(engine.delete_with(&victim, DeleteMode::Trash).await).await;

    // And the file is still INSIDE the zip (neither deleted nor made permanent).
    let mut stream = engine.read(&victim, None).await.expect("still readable");
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk"));
    }
    assert_eq!(out, b"contents");
}

#[tokio::test]
async fn copy_into_readonly_archive_is_rejected() {
    // Copying INTO an archive (READ_ONLY) = a clean rejection, never a
    // half-finished write into the container.
    let engine = engine_with_zip(&[(b"hello.txt", b"x")]).await;
    let dst = vp("zip+mem:///a.zip/!/new.txt");

    assert_rejected(engine.copy(&vp("mem:///src.txt"), &dst).await).await;
}
