//! `provider_contract!` sobre un servidor FTP IN-PROCESS (ADR 0014): la MISMA
//! suite que pasan `MemProvider`, `LocalProvider` y `SftpProvider`, ahora
//! contra un provider FTP de verdad, con el corpus de nombres hostiles. Corre
//! en CI normal, sin Docker (el servidor real es un job nightly aparte).
//!
//! Solo-Linux (como sftp): el servidor libunftp mapea las ops sobre el FS del
//! HOST, así que su fidelidad exige un FS POSIX (case-sensitive,
//! byte-preserving). macOS/Windows no pueden respaldar el harness fiel. El
//! provider es OS-agnóstico; el nightly (servidor FTP real) valida producción.
#![cfg(target_os = "linux")]

mod common;

use norte_proto::Authority;
use norte_vfs_ftp::FtpProvider;

/// Provider FTP fresco sobre un tempdir + servidor libunftp in-process.
async fn fresh() -> FtpProvider {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().to_path_buf();
    let ftp = common::connect(&base).await;
    // El tempdir vive tanto como el provider (tests efímeros; el SO limpia /tmp).
    std::mem::forget(dir);
    // El servidor mapea `/` al tempdir, así que la base del cliente es `/`.
    FtpProvider::new(ftp, "/").await.expect("provider")
}

fn hostile_names() -> Vec<Vec<u8>> {
    norte_testkit::corpus::hostile_names()
        .into_iter()
        .map(|n| n.bytes)
        .collect()
}

norte_vfs::provider_contract! {
    mod ftp_inproc,
    factory: fresh().await,
    root: FtpProvider::root(Authority::new("test:21").expect("authority válida")),
    hostile_names: hostile_names(),
}
