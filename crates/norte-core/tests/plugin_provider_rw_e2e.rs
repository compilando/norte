//! E2E of [`PluginProvider`]'s WRITE path (#30 stage 2b-write, ADR 0032):
//! compiles the WRITABLE `provider-mem-rw` guest, wraps it in a
//! `norte_vfs::Provider` and verifies the transactional `ByteSink` projection
//! (writer resource) + mkdir/remove/rename.
//!
//! SKIP without the `wasm32-wasip2` target.

use std::path::PathBuf;
use std::process::Command;

use bytes::Bytes;
use futures::StreamExt;
use norte_core::plugin_provider::PluginProvider;
use norte_plugin_host::{Capabilities as HostCaps, PluginRuntime};
use norte_vfs::Provider;
use norte_vfs::proto::{CapabilityFlags, EntryKind, Error, Scheme, Segment, VPath};

fn root() -> VPath {
    VPath::root(Scheme::new("mem").expect("scheme"), None)
}

fn child(base: &VPath, name: &[u8]) -> VPath {
    base.join(Segment::new(name.to_vec()).expect("segment"))
}

async fn read_all(p: &PluginProvider, path: &VPath) -> Result<Vec<u8>, Error> {
    let mut s = p.read(path, None).await?;
    let mut out = Vec::new();
    while let Some(c) = s.next().await {
        out.extend_from_slice(&c?);
    }
    Ok(out)
}

#[tokio::test]
async fn plugin_provider_write_path() {
    let Some(wasm) = build_guest("provider-mem-rw") else {
        eprintln!("SKIP: target wasm32-wasip2 not installed");
        return;
    };
    let rt = PluginRuntime::new().expect("runtime");
    let wasm = norte_plugin_host::WasmArtifact::trusting_current(&wasm).expect("guest");
    let p = PluginProvider::new(rt, &wasm, HostCaps::default(), "mem").expect("adapter");
    let root = root();

    // The guest is WRITABLE: no READ_ONLY flag.
    assert!(!p.capabilities().flags.contains(CapabilityFlags::READ_ONLY));

    // ---- write + commit: creates the file with byte-exact (hostile) content ----
    let new = child(&root, b"new.bin");
    let content: &[u8] = b"line\x00\xff\xfe end";
    // Staging is transactional: before the commit the path does NOT exist.
    let mut sink = p.write(&new).await.expect("opens writer");
    assert_eq!(
        p.stat(&new).await.unwrap_err(),
        Error::NotFound,
        "the final path does not exist until commit"
    );
    sink.write(Bytes::from_static(b"line\x00"))
        .await
        .expect("chunk 1");
    sink.write(Bytes::from_static(b"\xff\xfe end"))
        .await
        .expect("chunk 2");
    sink.commit().await.expect("commit");
    // Now it exists and its content is byte-exact.
    let st = p.stat(&new).await.expect("exists after commit");
    assert_eq!(st.kind, EntryKind::File);
    assert_eq!(st.size, Some(content.len() as u64));
    assert_eq!(read_all(&p, &new).await.unwrap(), content);

    // ---- abort: publishes nothing ----
    let aborted = child(&root, b"aborted.txt");
    let mut sink = p.write(&aborted).await.expect("opens writer");
    sink.write(Bytes::from_static(b"garbage"))
        .await
        .expect("chunk");
    sink.abort().await.expect("abort");
    assert_eq!(
        p.stat(&aborted).await.unwrap_err(),
        Error::NotFound,
        "abort publishes nothing"
    );

    // ---- mkdir ----
    let dir = child(&root, b"newdir");
    p.mkdir(&dir).await.expect("mkdir");
    assert_eq!(p.stat(&dir).await.expect("dir exists").kind, EntryKind::Dir);
    // mkdir over something existing = Conflict.
    assert!(matches!(
        p.mkdir(&dir).await.unwrap_err(),
        Error::Conflict { .. }
    ));

    // ---- remove ----
    // NOTE: `vacio.txt` is fixture data baked into the `provider-mem-rw` guest
    // (crates/norte-plugin-host/examples-wasm/provider-mem-rw/src/lib.rs,
    // owned by another task) and kept verbatim — see the T05 report's
    // cross-file literals.
    let empty = child(&root, b"vacio.txt");
    assert!(p.stat(&empty).await.is_ok());
    p.remove(&empty).await.expect("remove");
    assert_eq!(p.stat(&empty).await.unwrap_err(), Error::NotFound);

    // ---- rename ----
    // NOTE: `docs/hello.txt` and its content "hola norte\n" are the same kind
    // of guest fixture data; kept verbatim.
    let hello = child(&child(&root, b"docs"), b"hello.txt");
    let renamed = child(&child(&root, b"docs"), b"renamed.txt");
    p.rename(&hello, &renamed).await.expect("rename");
    assert_eq!(p.stat(&hello).await.unwrap_err(), Error::NotFound);
    assert_eq!(read_all(&p, &renamed).await.unwrap(), b"hola norte\n");

    // ---- rule 1: a non-UTF-8 NAME (valid Segment) round-trips through write
    // and rename byte-exact ----
    let hostile = child(&root, b"h\xff\xfe.bin");
    let mut sink = p
        .write(&hostile)
        .await
        .expect("opens writer for hostile name");
    sink.write(Bytes::from_static(b"x")).await.expect("chunk");
    sink.commit().await.expect("commit");
    assert!(
        p.stat(&hostile).await.is_ok(),
        "the non-UTF-8 name is created"
    );
    let hostile2 = child(&root, b"h\xfe\xff.mov");
    p.rename(&hostile, &hostile2)
        .await
        .expect("rename hostile name");
    assert_eq!(p.stat(&hostile).await.unwrap_err(), Error::NotFound);
    assert_eq!(read_all(&p, &hostile2).await.unwrap(), b"x");
}

fn build_guest(name: &str) -> Option<PathBuf> {
    if !target_installed("wasm32-wasip2") {
        eprintln!("SKIP: target wasm32-wasip2 not installed");
        return None;
    }
    let guest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("norte-plugin-host")
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
    assert!(status.success(), "the {name} guest did not build");
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join(format!("{}.wasm", name.replace('-', "_")));
    assert!(wasm.exists(), "{} not found", wasm.display());
    Some(wasm)
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
