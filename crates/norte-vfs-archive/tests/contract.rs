//! `readonly_provider_contract!` sobre tar (`TarSmith` + `MemProvider`): la
//! suite RO completa contra la lógica real del provider (split de vpath,
//! índice, passthrough de lectura) sin FS del host ni Docker.

mod common;

use norte_proto::VPath;
use norte_testkit::TarSmith;
use norte_vfs_archive::ArchiveProvider;

/// Nombres del corpus que caben en un header ustar (`TarSmith` no forja GNU
/// longname): ≤ 100 bytes. Los largos los cubre la suite zip (fase 8e).
fn hostile_names() -> Vec<Vec<u8>> {
    norte_testkit::corpus::hostile_names()
        .into_iter()
        .map(|n| n.bytes)
        .filter(|b| {
            // `hostile/` + nombre debe caber en los 100 bytes del header.
            b.len() <= 100 - "hostile/".len()
        })
        .collect()
}

/// El árbol canónico que exige la macro RO, forjado como tar.
fn canonical_tar() -> Vec<u8> {
    let mut smith = TarSmith::new()
        .dir(b"docs")
        .file(b"docs/hello.txt", b"hola norte\n")
        .file(b"docs/sub/nested.bin", b"\x00\x01\x02\xff")
        .file(b"vacio.txt", b"")
        .dir(b"hostile");
    for name in hostile_names() {
        let mut full = b"hostile/".to_vec();
        full.extend_from_slice(&name);
        smith = smith.file(&full, &name);
    }
    smith.build()
}

fn fresh() -> ArchiveProvider {
    // La macro evalúa `factory` dentro del test async de tokio; la siembra
    // del Mem es async pura (sin IO/timers) — el executor de futures basta
    // y no pisa el runtime de tokio.
    futures::executor::block_on(async {
        let (provider, _) = common::tar_provider(&canonical_tar()).await;
        provider
    })
}

fn root() -> VPath {
    let path = norte_testkit::MemProvider::root()
        .join(norte_proto::Segment::new(b"fixture.tar".to_vec()).expect("seg"));
    VPath::archive_compose("tar", &path, &[]).expect("compose")
}

norte_vfs::readonly_provider_contract! {
    mod tar_ro,
    factory: fresh(),
    root: root(),
    hostile_names: hostile_names(),
}
