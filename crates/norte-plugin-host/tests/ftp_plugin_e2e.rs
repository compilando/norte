//! E2E of the complete network stack (#30 stage 3b, PROOF): a REAL FTP
//! guest (`ftp-probe`, SYNC suppaftp over wasi:sockets) connects to an
//! IN-PROCESS FTP server (`libunftp`, no Docker) THROUGH the `net`
//! capability's gating, and lists a directory. Proves: gated `net`
//! capability → wasm-compiled FTP client → real FTP server, end-to-end.
//!
//! The passive DATA port is negotiated by the server (dynamic): it works
//! because `net`'s allow-list is by IP (bare-ip = every port on the host),
//! which passive FTP REQUIRES (justifies that stage 3a semantic).
//!
//! Linux-only (the libunftp harness maps over the host FS) + SKIP without
//! the `wasm32-wasip2` target.
#![cfg(target_os = "linux")]

use std::io::Write;
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use norte_plugin_host::{Capabilities, PluginRuntime};

/// Starts libunftp over `home` on an ephemeral port (a thread with its own
/// tokio runtime) and returns the port. Waits until it listens.
fn spawn_ftp_server(home: PathBuf) -> u16 {
    // Ephemeral port via bind-then-drop (same technique as
    // norte-vfs-ftp's harness).
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
            .greeting("norte plugin ftp e2e")
            .build()
            .expect("build server");
            let _ = server.listen(format!("127.0.0.1:{port}")).await;
        });
    });

    // Waits for the listen to bind.
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return port;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the ftp server did not start on :{port}");
}

#[test]
fn ftp_plugin_stack_e2e() {
    let Some(wasm) = build_guest("ftp-probe") else {
        eprintln!("SKIP: target wasm32-wasip2 not installed");
        return;
    };

    // Tempdir seeded with TWO files: the guest's LIST must see 2 entries.
    let dir = tempfile::tempdir().expect("tempdir");
    for name in ["one.txt", "two.txt"] {
        let mut f = std::fs::File::create(dir.path().join(name)).expect("create");
        f.write_all(b"x").expect("write");
    }
    let port = spawn_ftp_server(dir.path().to_path_buf());
    let target = format!("127.0.0.1:{port}");

    let rt = PluginRuntime::new().expect("runtime");
    // WITH `net` (allow-list = 127.0.0.1, all ports → control + passive
    // data): the guest connects to the FTP and lists.
    let mut inst = rt
        .instantiate(&wasm, Capabilities::with_net(vec!["127.0.0.1".to_owned()]))
        .expect("instantiate with net");
    let out = inst
        .run_command("list", &target)
        .expect("the FTP guest must connect and list over gated net");
    assert_eq!(out, "2", "LIST of / sees the 2 seeded files: {out:?}");

    // WITHOUT `net`: not even the control connect gets out
    // (socket_addr_check rejects).
    let mut inst = rt
        .instantiate(&wasm, Capabilities::default())
        .expect("instantiate without net");
    assert!(
        inst.run_command("list", &target).is_err(),
        "without the net capability the FTP guest cannot even connect"
    );
}

fn build_guest(name: &str) -> Option<norte_plugin_host::WasmArtifact> {
    if !target_installed("wasm32-wasip2") {
        eprintln!("SKIP: target wasm32-wasip2 not installed");
        return None;
    }
    let guest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples-wasm")
        .join(name);
    let target_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("wasm-guests");
    let status = Command::new(env!("CARGO"))
        .current_dir(&guest_dir)
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-wasip2",
            "--target-dir",
        ])
        .arg(&target_dir)
        .status()
        .expect("cargo build of the guest");
    assert!(status.success(), "guest {name} did not compile");
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join(format!("{}.wasm", name.replace('-', "_")));
    assert!(wasm.exists(), "{} was not found", wasm.display());
    // With the fingerprint of what was just compiled (ADR 0142).
    Some(norte_plugin_host::WasmArtifact::trusting_current(wasm).expect("guest"))
}

fn target_installed(target: &str) -> bool {
    Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l == target)
        })
}

// ---- #30 M1: guest-side RETR cache guards (O(n) reading) ----

/// Sets up a `ProviderInstance` of the ftp-provider guest against a
/// libunftp over `home`, ready to read. `None` (SKIP) without the wasm
/// target. Returns the runtime alongside the instance to keep the epoch
/// ticker alive.
fn configured_ftp_provider(
    home: PathBuf,
) -> Option<(PluginRuntime, norte_plugin_host::ProviderInstance)> {
    let wasm = build_guest("ftp-provider")?;
    let port = spawn_ftp_server(home);
    let rt = PluginRuntime::new().expect("runtime");
    let mut inst = rt
        .instantiate_provider(&wasm, Capabilities::with_net(vec!["127.0.0.1".to_owned()]))
        .expect("instantiate provider");
    let cfg = norte_plugin_host::provider_iface::ProviderConfig {
        endpoint: format!("127.0.0.1:{port}"),
        user: "anonymous".to_owned(),
        password: "anonymous".to_owned(),
        base: "/".to_owned(),
    };
    inst.configure(&cfg)
        .expect("configure without a trap")
        .expect("configure ok");
    Some((rt, inst))
}

/// Reads `segments` in chunks of `chunk` bytes (like the adapter),
/// reassembling; stops at the first short chunk (EOF).
fn read_all_chunked(
    inst: &mut norte_plugin_host::ProviderInstance,
    segments: &[Vec<u8>],
    chunk: u64,
) -> Vec<u8> {
    // The helper treats a short chunk as EOF: only valid if `chunk` does
    // not exceed the guest's ceiling (1 MiB), where a short-read is a real
    // EOF.
    assert!(
        chunk <= 1 << 20,
        "the helper assumes chunk <= the guest's ceiling"
    );
    let mut out = Vec::new();
    let mut off = 0u64;
    loop {
        let c = inst
            .read(segments, off, chunk)
            .expect("read without a trap")
            .expect("read ok");
        if c.is_empty() {
            break;
        }
        off += c.len() as u64;
        let short = (c.len() as u64) < chunk;
        out.extend_from_slice(&c);
        if short {
            break;
        }
    }
    out
}

#[test]
fn ftp_large_sequential_byte_exact() {
    let dir = tempfile::tempdir().expect("tempdir");
    // 512 KiB > several 64 KiB chunks: exercises RETR reuse.
    let content: Vec<u8> = (0u32..512 * 1024).map(|i| (i % 251) as u8).collect();
    std::fs::write(dir.path().join("big.bin"), &content).expect("seed");
    let Some((_rt, mut inst)) = configured_ftp_provider(dir.path().to_path_buf()) else {
        return;
    };
    let got = read_all_chunked(&mut inst, &[b"big.bin".to_vec()], 64 * 1024);
    assert_eq!(got, content, "byte-exact sequential read");
}

#[test]
fn ftp_interleaving_stat_does_not_desync() {
    let dir = tempfile::tempdir().expect("tempdir");
    let content: Vec<u8> = (0u32..200 * 1024).map(|i| (i % 251) as u8).collect();
    std::fs::write(dir.path().join("f.bin"), &content).expect("seed f");
    std::fs::write(dir.path().join("other.txt"), b"data").expect("seed other");
    let Some((_rt, mut inst)) = configured_ftp_provider(dir.path().to_path_buf()) else {
        return;
    };
    // Reads one chunk (half the file), ABANDONS it without reaching EOF,
    // then stats another path: if the cache did not drain, the pending
    // 226 would desync the stat.
    let half = inst
        .read(&[b"f.bin".to_vec()], 0, 64 * 1024)
        .expect("read without a trap")
        .expect("read ok");
    assert_eq!(half.len(), 64 * 1024);
    let st = inst
        .stat(&[b"other.txt".to_vec()])
        .expect("stat without a trap")
        .expect("other.txt exists");
    assert_eq!(
        st.size,
        Some(4),
        "stat after an abandoned read does NOT desync"
    );
    // A whole re-read of the first one is still byte-exact.
    let got = read_all_chunked(&mut inst, &[b"f.bin".to_vec()], 64 * 1024);
    assert_eq!(got, content, "byte-exact whole re-read");
}

#[test]
fn ftp_range_then_list_ok() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("r.bin"), b"0123456789").expect("seed");
    let Some((_rt, mut inst)) = configured_ftp_provider(dir.path().to_path_buf()) else {
        return;
    };
    // Bounded range (offset 2, 3 bytes) = "234"; leaves the cache alive.
    let slice = inst
        .read(&[b"r.bin".to_vec()], 2, 3)
        .expect("read without a trap")
        .expect("read ok");
    assert_eq!(slice, b"234");
    // list_dir of the root must work (cache flush before the command).
    let page = inst
        .list_dir(&[], None)
        .expect("list without a trap")
        .expect("root lists");
    assert!(
        page.entries.iter().any(|e| e.name == b"r.bin"),
        "list after a range sees the file: the cache drained cleanly"
    );
}

#[test]
fn ftp_non_sequential_reread_over_a_live_cache() {
    let dir = tempfile::tempdir().expect("tempdir");
    // 130 KiB NOT aligned to 64 KiB: crosses chunk boundaries with a
    // partial tail.
    let content: Vec<u8> = (0u32..130 * 1024).map(|i| (i % 251) as u8).collect();
    std::fs::write(dir.path().join("g.bin"), &content).expect("seed");
    let Some((_rt, mut inst)) = configured_ftp_provider(dir.path().to_path_buf()) else {
        return;
    };
    let seg = [b"g.bin".to_vec()];
    // One chunk from 0 (leaves the cache ALIVE at next_offset=64Ki).
    let a = inst.read(&seg, 0, 64 * 1024).expect("read").expect("ok");
    assert_eq!(a.as_slice(), &content[..64 * 1024]);
    // Re-reads from 0 with NO intervening flush op: offset does not match
    // (next_offset=64Ki) → miss → flush+re-RETR. Must give the SAME first
    // bytes, not garbage.
    let b = inst.read(&seg, 0, 64 * 1024).expect("read").expect("ok");
    assert_eq!(
        b.as_slice(),
        &content[..64 * 1024],
        "byte-exact re-read from 0"
    );
    // Jump forward to 128 KiB (miss again) → 2 KiB tail.
    let c = inst
        .read(&seg, 128 * 1024, 64 * 1024)
        .expect("read")
        .expect("ok");
    assert_eq!(
        c.as_slice(),
        &content[128 * 1024..],
        "byte-exact forward jump (tail)"
    );
    // And a whole sequential read from zero is still correct.
    let whole = read_all_chunked(&mut inst, &seg, 64 * 1024);
    assert_eq!(whole, content, "byte-exact whole read after the jumps");
}

#[test]
fn ftp_mlsd_size_over_4gib() {
    let dir = tempfile::tempdir().expect("tempdir");
    // SPARSE 5 GiB file (set_len writes no blocks): > u32::MAX.
    let f = std::fs::File::create(dir.path().join("huge.bin")).expect("create");
    f.set_len(5 * 1024 * 1024 * 1024).expect("set_len 5 GiB");
    drop(f);
    let Some((_rt, mut inst)) = configured_ftp_provider(dir.path().to_path_buf()) else {
        return;
    };
    // stat: size == exactly 5 GiB (not truncated to usize/u32, no
    // NotFound/Io).
    let st = inst
        .stat(&[b"huge.bin".to_vec()])
        .expect("stat without a trap")
        .expect("huge.bin exists");
    assert_eq!(
        st.size,
        Some(5 * 1024 * 1024 * 1024),
        "size u64 without truncation"
    );
    // list: the entry appears with its size, the page does NOT fail.
    let page = inst
        .list_dir(&[], None)
        .expect("list without a trap")
        .expect("root lists");
    let e = page
        .entries
        .iter()
        .find(|e| e.name == b"huge.bin")
        .expect("huge.bin listed");
    assert_eq!(e.size, Some(5 * 1024 * 1024 * 1024));
}
