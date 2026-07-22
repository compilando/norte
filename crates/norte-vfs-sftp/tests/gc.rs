//! `gc_partials` de sftp (#11, ADR 0012) contra el servidor in-process:
//! barre los `.norte-partial.*` huérfanos por su FORMA exacta (estable
//! 32-hex / efímero `eph.<seq>`), jamás archivos reales del usuario.
#![cfg(target_os = "linux")]

mod common;

use norte_proto::{Authority, Segment, VPath};
use norte_vfs::Provider;
use norte_vfs_sftp::SftpProvider;

fn root() -> VPath {
    SftpProvider::root(Authority::new("test:22").expect("authority"))
}

fn seg(b: &[u8]) -> Segment {
    Segment::new(b.to_vec()).expect("segmento")
}

#[tokio::test]
async fn gc_partials_barre_por_forma_exacta() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().to_path_buf();
    let session = common::connect(&base, common::Mode::Honest).await;
    let p = SftpProvider::new(session, "/");

    // Huérfanos con las DOS formas del staging sftp…
    std::fs::write(
        base.join(".norte-partial.0123456789abcdef0123456789abcdef"),
        b"x",
    )
    .expect("estable");
    std::fs::write(base.join(".norte-partial.eph.7"), b"x").expect("efimero");
    // …y archivos del USUARIO con el prefijo pero no la forma (H2).
    std::fs::write(base.join(".norte-partial.backup"), b"mio").expect("user 1");
    std::fs::write(base.join(".norte-partial.eph.no-num"), b"mio").expect("user 2");
    // Forma de OTRO provider (local: eph con pid-seq) tampoco se toca aquí.
    std::fs::write(base.join(".norte-partial.0123456789abcdef.42-1"), b"?").expect("otra forma");

    let removed = p
        .gc_partials(&root(), std::time::Duration::ZERO)
        .await
        .expect("gc");
    assert_eq!(removed, 2, "solo las dos formas sftp se barren");
    assert!(base.join(".norte-partial.backup").exists());
    assert!(base.join(".norte-partial.eph.no-num").exists());
    assert!(base.join(".norte-partial.0123456789abcdef.42-1").exists());
    assert!(
        !base
            .join(".norte-partial.0123456789abcdef0123456789abcdef")
            .exists()
    );
    assert!(!base.join(".norte-partial.eph.7").exists());
}

/// `older_than` respeta el mtime: un staging RECIENTE no se barre (una
/// reanudación en curso sobrevive a un gc con umbral holgado).
#[tokio::test]
async fn gc_partials_respeta_older_than() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().to_path_buf();
    let session = common::connect(&base, common::Mode::Honest).await;
    let p = SftpProvider::new(session, "/");

    std::fs::write(base.join(".norte-partial.eph.3"), b"x").expect("staging");
    let removed = p
        .gc_partials(&root(), std::time::Duration::from_hours(1))
        .await
        .expect("gc");
    assert_eq!(removed, 0, "recién tocado: se queda");
    assert!(base.join(".norte-partial.eph.3").exists());
    let _ = seg(b"ancla"); // (usa el helper; el drop del tempdir limpia)
}
