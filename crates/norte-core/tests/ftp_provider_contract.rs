//! `provider_contract!` over the FTP-via-plugin guest (#30 stage 3c, ADR 0033)
//! against an IN-PROCESS `libunftp` server — the SAME suite that `MemProvider`,
//! `SftpProvider` and the defunct `norte-vfs-ftp` used to pass, now over the
//! provider running in WASM. It uses the `.wasm` artifact EMBEDDED in
//! `norte-core`, so it validates exactly what gets shipped.
//!
//! Linux-only (like the original sftp/ftp contract): `libunftp` maps its ops
//! onto the host's FS, whose fidelity requires a POSIX FS (case-sensitive,
//! byte-preserving). The provider itself is OS-agnostic.
//!
//! Unlike the wasm E2E tests that SKIP, `provider_contract!` does NOT know how
//! to skip: this test REQUIRES the wasmtime runtime (always present) + the
//! embedded artifact (always present). It compiles and runs no guest at test
//! time — the `.wasm` is already inside the binary.
#![cfg(target_os = "linux")]

use std::net::TcpStream;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::Duration;

use norte_core::ftp_plugin::connect_ftp_plugin;
use norte_core::plugin_provider::PluginProvider;
use norte_proto::{Scheme, VPath};
use norte_vfs::Provider;

/// Starts `libunftp` over `home` on an ephemeral port (a thread with its own
/// tokio runtime) and returns the port. Waits until it listens. Copied from
/// the `spawn_ftp_server` helper in `norte-plugin-host/tests/ftp_plugin_e2e.rs`.
fn spawn_libunftp(home: PathBuf) -> u16 {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("ephemeral bind");
    let port = probe.local_addr().expect("addr").port();
    drop(probe);

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async move {
            let server = libunftp::ServerBuilder::new(Box::new(move || {
                unftp_sbe_fs::Filesystem::new(home.clone()).expect("fs backend")
            }))
            .greeting("norte ftp-via-plugin contract")
            .build()
            .expect("build server");
            let _ = server.listen(format!("127.0.0.1:{port}")).await;
        });
    });

    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return port;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the ftp server did not start on :{port}");
}

/// A FRESH FTP-via-plugin provider over a tempdir + in-process libunftp
/// server, connected through the real wiring (resolve → net → configure).
async fn fresh() -> PluginProvider {
    let dir = tempfile::tempdir().expect("tempdir");
    let port = spawn_libunftp(dir.path().to_path_buf());
    // The tempdir lives as long as the provider does (ephemeral tests; the OS cleans /tmp).
    std::mem::forget(dir);
    connect_ftp_plugin("127.0.0.1", port, "anonymous", "anonymous", "/")
        .await
        .expect("connected ftp-via-plugin provider")
}

/// Provider + the LIVE `TempDir` (for tests that seed files on the server's FS
/// underneath — non-UTF-8 names the provider would never create).
async fn fresh_keep() -> (PluginProvider, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let port = spawn_libunftp(dir.path().to_path_buf());
    let provider = connect_ftp_plugin("127.0.0.1", port, "anonymous", "anonymous", "/")
        .await
        .expect("connected ftp-via-plugin provider");
    (provider, dir)
}

/// The `PluginProvider`'s root is SCHEME-ONLY (no authority) when used
/// directly; the adapter also tolerates an authority (the paths the engine
/// routes carry one, encoding H1).
fn ftp_root() -> VPath {
    VPath::root(Scheme::new("ftp").expect("ftp scheme"), None)
}

/// The shared hostile corpus PLUS two names specific to FTP extraction that
/// the defunct `norte-vfs-ftp/tests/hostile.rs` covered and the corpus does
/// not have (encoding M3a): a `;` (which the raw MLSD extractor's
/// `split_once(' ')` must NOT truncate — suppaftp would truncate it with its
/// `split(';')`) and a leading space (MLSD preserves it; LIST would lose it).
fn hostile_names() -> Vec<Vec<u8>> {
    let mut names: Vec<Vec<u8>> = norte_testkit::corpus::hostile_names()
        .into_iter()
        .map(|n| n.bytes)
        .collect();
    names.push(b"a;b.txt".to_vec());
    names.push(b" sp.txt".to_vec());
    names
}

norte_vfs::provider_contract! {
    mod ftp_plugin_inproc,
    factory: fresh().await,
    root: ftp_root(),
    hostile_names: hostile_names(),
}

/// CR/LF in a segment = an attempted FTP command injection
/// (`x\r\nDELE victim`): the guest rejects it CLEANLY as `InvalidPath`, and
/// never executes the smuggled command (encoding M3c; `Segment` allows
/// `\r`/`\n`, so the path is built and reaches the guest).
#[tokio::test]
async fn crlf_in_a_segment_is_invalid_path() {
    let p = fresh().await;
    let root = ftp_root();
    for probe in [b"x\r\nDELE victim".as_slice(), b"y\nNOOP".as_slice()] {
        let seg = norte_proto::Segment::new(probe.to_vec()).expect("valid segment with CR/LF");
        let path = root.join(seg);
        assert_eq!(
            p.stat(&path).await.unwrap_err(),
            norte_proto::Error::InvalidPath,
            "stat with CR/LF must be InvalidPath: {:?}",
            String::from_utf8_lossy(probe)
        );
        match p.write(&path).await {
            Err(norte_proto::Error::InvalidPath) => {}
            Err(e) => panic!("write with CR/LF should have been InvalidPath, was {e:?}"),
            Ok(_) => panic!("write with CR/LF should have been InvalidPath, it opened the sink"),
        }
    }
}

/// A NON-UTF-8 name (`caf\xE9.txt`, raw 0xE9) seeded DIRECTLY on the server's
/// FS: suppaftp decodes it lossily (U+FFFD); the guest skips it and the
/// listing NEVER emits a corrupt `Entry` nor the bytes 0xEF 0xBF 0xBD
/// (encoding M3b — the provider would never create that name, so only a file
/// seeded from outside exercises this).
#[tokio::test]
async fn a_non_utf8_name_on_the_server_is_not_corrupted() {
    use futures::StreamExt;
    let (p, dir) = fresh_keep().await;
    // 0xE9 = é in latin-1; NOT valid UTF-8.
    let raw_name = b"caf\xE9.txt";
    let mut path = dir.path().to_path_buf();
    path.push(std::ffi::OsStr::from_bytes(raw_name));
    std::fs::write(&path, b"x").expect("seed the non-UTF-8 file on the server's FS");

    // list of the root: the guest fails the PAGE CLEANLY (InvalidPath) when it
    // hits the lossy name — the correct fail-loud behavior (the defunct
    // provider did the same). Either `list()` returns that error directly, or
    // it opens and some item is InvalidPath; in NO case does it emit a name
    // with U+FFFD (0xEF 0xBF 0xBD).
    let root = ftp_root();
    let mut saw_replacement = false;
    match p.list(&root).await {
        // Clean rejection BEFORE opening the stream: acceptable (fail-loud).
        Err(norte_proto::Error::InvalidPath) => {}
        Err(e) => panic!("unexpected error opening list: {e:?}"),
        Ok(mut stream) => {
            while let Some(entry) = stream.next().await {
                match entry {
                    Ok(e) => {
                        let bytes = e.path.file_name().expect("has a name").as_bytes().to_vec();
                        if bytes.windows(3).any(|w| w == [0xEF, 0xBF, 0xBD]) {
                            saw_replacement = true;
                        }
                    }
                    Err(norte_proto::Error::InvalidPath) => {}
                    Err(e) => panic!("unexpected error while listing: {e:?}"),
                }
            }
        }
    }
    assert!(
        !saw_replacement,
        "the listing must never emit a name with U+FFFD (0xEF 0xBF 0xBD)"
    );
    drop(dir);
}
