//! `provider_contract!` sobre `services-fs` de opendal (ADR 0016 J): la MISMA
//! suite que pasan Mem/Local/Sftp/Ftp, contra la lógica completa del provider
//! (validación de keys, modelo de dirs, sink, mapeo de errores) sin HTTP. Lo
//! S3-específico que este harness no ejercita (multipart, conditional write,
//! delimiter real) vive en tests/s3.rs contra s3s-fs, y el nightly (`reals3`)
//! valida contra `MinIO`.
//!
//! Solo-Linux (como sftp/ftp): el harness se respalda en el FS del host y
//! solo es fiel en POSIX (case-sensitive, byte-preserving)… con una
//! asimetría CONSCIENTE: las keys S3 son UTF-8-only, así que las fixtures
//! no-UTF8 del corpus se rechazan limpio en el provider (skip del contrato),
//! no llegan al FS.
#![cfg(target_os = "linux")]

mod common;

use norte_proto::Authority;
use norte_vfs_object::ObjectProvider;

/// Provider fresco sobre un tempdir vía `services-fs` (con `atomic_write_dir`
/// FUERA de la raíz listable: un `.tmp` del writer no es una entrada).
fn fresh() -> ObjectProvider {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("root");
    let atomic = dir.path().join("staging");
    std::fs::create_dir_all(&root).expect("root");
    std::fs::create_dir_all(&atomic).expect("staging");
    let op = common::fs_operator(&root, &atomic);
    // El tempdir vive tanto como el provider (tests efímeros; el SO limpia /tmp).
    std::mem::forget(dir);
    ObjectProvider::new(op, "s3")
}

fn hostile_names() -> Vec<Vec<u8>> {
    norte_testkit::corpus::hostile_names()
        .into_iter()
        .map(|n| n.bytes)
        // Los fixtures name_max (255/256 bytes) son keys S3 LEGALES (límite
        // 1024, sin tope por segmento) que este harness de FS no puede
        // almacenar: NAME_MAX POSIX = 255 y el atomic_write_dir de opendal
        // añade ".XXXXXXXX" (9 bytes) al tempfile → tope efectivo 246.
        // Limitación del harness, no del provider — el fs revienta DESPUÉS
        // del open y el contrato exige rechazo limpio o éxito. Los cubre el
        // nightly contra `MinIO` real (tests/reals3.rs).
        .filter(|bytes| bytes.len() <= 246)
        .collect()
}

norte_vfs::provider_contract! {
    mod object_fs,
    factory: fresh(),
    root: ObjectProvider::root("s3", Authority::new("norte-test").expect("authority válida")),
    hostile_names: hostile_names(),
}
