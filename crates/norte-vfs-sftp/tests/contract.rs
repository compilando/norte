//! `provider_contract!` sobre un servidor SFTP IN-PROCESS (ADR 0013): la
//! MISMA suite que pasan `MemProvider` y `LocalProvider`, ahora contra un
//! provider REMOTO de verdad, con el corpus de nombres hostiles. Corre en
//! CI normal, sin Docker (el openssh real es un job nightly aparte).

mod common;

use norte_proto::Authority;
use norte_vfs_sftp::SftpProvider;

/// Construye un provider sftp fresco sobre un tempdir + servidor in-process.
/// El bloque es async, así que se envuelve en un runtime propio (la macro
/// `provider_contract!` evalúa `factory` en un test `#[tokio::test]`).
async fn fresh() -> SftpProvider {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().to_path_buf();
    let session = common::connect(&base, common::Mode::Honest).await;
    // El tempdir debe vivir tanto como el provider (tests efímeros; el SO
    // limpia /tmp).
    std::mem::forget(dir);
    // El servidor mapea `/` al tempdir, así que la base REMOTA del cliente
    // es `/` (los paths se componen /segmento, el servidor los rebasa).
    SftpProvider::new(session, "/")
}

fn hostile_names() -> Vec<Vec<u8>> {
    norte_testkit::corpus::hostile_names()
        .into_iter()
        .map(|n| n.bytes)
        .collect()
}

norte_vfs::provider_contract! {
    mod sftp_inproc,
    factory: fresh().await,
    root: SftpProvider::root(Authority::new("test:22").expect("authority válida")),
    hostile_names: hostile_names(),
}
