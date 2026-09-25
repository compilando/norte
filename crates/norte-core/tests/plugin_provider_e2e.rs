//! E2E of the [`PluginProvider`] adapter (#30 stage 2b, ADR 0032): compiles
//! the `provider-mem` guest to `wasm32-wasip2`, wraps it in a real
//! `norte_vfs::Provider` and verifies the READ CONTRACT — the same checks as
//! `readonly_provider_contract!` (stat/list/read/ranges/hostile names/caps/
//! mutations=Unsupported), showing that the async→sync projection reassembles
//! the streams correctly.
//!
//! It does not invoke the literal macro because that requires the
//! `wasm32-wasip2` target at test time (an environment dependency) and the
//! macro does not know how to SKIP; here it does an explicit SKIP without the
//! target (consistent with the other wasm E2E tests).

use std::path::PathBuf;
use std::process::Command;

use futures::StreamExt;
use norte_core::plugin_provider::PluginProvider;
use norte_plugin_host::{Capabilities as HostCaps, PluginRuntime};
use norte_vfs::Provider;
use norte_vfs::proto::{ByteRange, CapabilityFlags, EntryKind, Error, Scheme, Segment, VPath};

fn root() -> VPath {
    VPath::root(Scheme::new("mem").expect("scheme mem"), None)
}

fn child(base: &VPath, name: &[u8]) -> VPath {
    base.join(Segment::new(name.to_vec()).expect("segment"))
}

async fn read_all(
    p: &PluginProvider,
    path: &VPath,
    range: Option<ByteRange>,
) -> Result<Vec<u8>, Error> {
    let mut s = p.read(path, range).await?;
    let mut out = Vec::new();
    while let Some(c) = s.next().await {
        out.extend_from_slice(&c?);
    }
    Ok(out)
}

async fn list_names(p: &PluginProvider, dir: &VPath) -> Vec<Vec<u8>> {
    let mut names: Vec<Vec<u8>> = p
        .list(dir)
        .await
        .expect("list opens")
        .map(|e| {
            e.expect("entry ok")
                .path
                .file_name()
                .expect("has a name")
                .as_bytes()
                .to_vec()
        })
        .collect()
        .await;
    names.sort();
    names
}

// A single test (one compilation of the guest) that walks the whole read
// contract — hence its length.
#[expect(
    clippy::too_many_lines,
    reason = "a single test walks the entire read contract"
)]
#[tokio::test]
async fn plugin_provider_satisfies_the_read_contract() {
    let Some(wasm) = build_guest("provider-mem") else {
        eprintln!("SKIP: target wasm32-wasip2 not installed");
        return;
    };
    let rt = PluginRuntime::new().expect("runtime");
    let wasm = norte_plugin_host::WasmArtifact::trusting_current(&wasm).expect("guest");
    let p = PluginProvider::new(rt, &wasm, HostCaps::default(), "mem").expect("adapter");
    let root = root();

    // ---- capabilities: READ_ONLY, no write flags ----
    let flags = p.capabilities().flags;
    assert!(flags.contains(CapabilityFlags::READ_ONLY));
    for forbidden in [
        CapabilityFlags::RENAME_ATOMIC,
        CapabilityFlags::SERVER_COPY,
        CapabilityFlags::APPEND,
        CapabilityFlags::RANDOM_WRITE,
        CapabilityFlags::TRASH,
    ] {
        assert!(
            !flags.contains(forbidden),
            "READ_ONLY excludes {forbidden:?}"
        );
    }

    // ---- stat ----
    assert_eq!(
        p.stat(&root).await.expect("root exists").kind,
        EntryKind::Dir
    );
    assert_eq!(
        p.stat(&child(&root, b"does-not-exist")).await.unwrap_err(),
        Error::NotFound
    );
    let f = p
        .stat(&child(&child(&root, b"docs"), b"hello.txt"))
        .await
        .expect("hello.txt");
    assert_eq!(f.kind, EntryKind::File);
    assert_eq!(f.size, Some(11));

    // ---- list: byte-exact tree + kinds coherent with stat ----
    // NOTE: `empty.txt` is fixture data baked into the `provider-mem` guest
    // (crates/norte-plugin-host/examples-wasm/provider-mem/src/lib.rs, owned
    // by another task) and kept verbatim here — see the T05 report's
    // cross-file literals.
    assert_eq!(
        list_names(&p, &root).await,
        vec![b"docs".to_vec(), b"hostile".to_vec(), b"vacio.txt".to_vec()]
    );
    let docs = child(&root, b"docs");
    assert_eq!(
        list_names(&p, &docs).await,
        vec![b"hello.txt".to_vec(), b"sub".to_vec()]
    );
    let entries: Vec<_> = p
        .list(&docs)
        .await
        .expect("list docs")
        .map(|e| e.expect("entry"))
        .collect()
        .await;
    for e in entries {
        assert_eq!(
            e.kind,
            p.stat(&e.path).await.expect("stat").kind,
            "{:?}",
            e.path
        );
    }
    // list of something nonexistent = NotFound; of a file = error.
    assert_eq!(
        p.list(&child(&root, b"does-not-exist")).await.err(),
        Some(Error::NotFound)
    );
    assert!(p.list(&child(&root, b"vacio.txt")).await.is_err());

    // ---- read: full, empty, ranges, past-EOF, missing, dir ----
    // NOTE: the content "hola norte\n" is fixture data baked into the guest
    // (same file as above); kept verbatim, including the byte offsets below
    // that are exact for its 11 bytes.
    let hello = child(&child(&root, b"docs"), b"hello.txt");
    assert_eq!(read_all(&p, &hello, None).await.unwrap(), b"hola norte\n");
    assert_eq!(
        read_all(&p, &child(&child(&docs, b"sub"), b"nested.bin"), None)
            .await
            .unwrap(),
        b"\x00\x01\x02\xff"
    );
    assert_eq!(
        read_all(&p, &child(&root, b"vacio.txt"), None)
            .await
            .unwrap(),
        b""
    );
    // middle range (offset 2, len 3) = "la ".
    assert_eq!(
        read_all(
            &p,
            &hello,
            Some(ByteRange {
                offset: 2,
                len: Some(3)
            })
        )
        .await
        .unwrap(),
        b"la "
    );
    // offset 8, to EOF = "te\n".
    assert_eq!(
        read_all(
            &p,
            &hello,
            Some(ByteRange {
                offset: 8,
                len: None
            })
        )
        .await
        .unwrap(),
        b"te\n"
    );
    // offset past EOF = empty, not an error.
    assert_eq!(
        read_all(
            &p,
            &hello,
            Some(ByteRange {
                offset: 100,
                len: Some(4)
            })
        )
        .await
        .unwrap(),
        b""
    );
    // len that overshoots EOF = clamped.
    assert_eq!(
        read_all(
            &p,
            &hello,
            Some(ByteRange {
                offset: 7,
                len: Some(100)
            })
        )
        .await
        .unwrap(),
        b"rte\n"
    );
    assert_eq!(
        read_all(&p, &child(&root, b"does-not-exist"), None)
            .await
            .unwrap_err(),
        Error::NotFound
    );
    assert!(
        read_all(&p, &docs, None).await.is_err(),
        "reading a dir is an error"
    );

    // ---- byte-exact hostile names ----
    // The guest seeds `a\xff\xfeb` (non-UTF-8, VALID as a Segment) and `a/b`
    // (with an INTERIOR `/` — valid in the WIT but NOT representable as a
    // VPath): the adapter OMITS the second one (there is no path to hang it
    // from), so the listing at the VPath level only carries the first.
    let hostile = b"a\xff\xfeb".to_vec();
    let hdir = child(&root, b"hostile");
    assert_eq!(
        list_names(&p, &hdir).await,
        vec![hostile.clone()],
        "the non-UTF-8 name survives; the one carrying `/` is omitted (not a VPath)"
    );
    assert_eq!(
        read_all(&p, &child(&hdir, &hostile), None).await.unwrap(),
        hostile,
        "content = the name's bytes"
    );

    // ---- mutations: Unsupported (read-only guest) ----
    let new = child(&root, b"new");
    let existing = child(&root, b"empty.txt");
    assert!(matches!(
        p.write(&new).await.err(),
        Some(Error::Unsupported)
    ));
    assert!(matches!(
        p.write(&existing).await.err(),
        Some(Error::Unsupported)
    ));
    assert!(matches!(
        p.mkdir(&new).await.err(),
        Some(Error::Unsupported)
    ));
    assert!(matches!(
        p.remove(&existing).await.err(),
        Some(Error::Unsupported)
    ));
    assert!(matches!(
        p.rename(&existing, &new).await.err(),
        Some(Error::Unsupported)
    ));
    assert!(matches!(
        p.trash(
            &existing,
            &norte_vfs::trash::TrashId::new(0, u64::from(line!()))
        )
        .await
        .err(),
        Some(Error::Unsupported)
    ));
    assert!(
        p.copy_native(&existing, &new).await.is_none(),
        "without SERVER_COPY, copy_native = None"
    );
}

/// P2 Task 4a: a provider NEVER originates from the plugin catalogue (see the
/// `plugin_provider`/`ftp_plugin` doc — a documented deferral), so
/// `set_settings` is not part of any production path. This test proves the
/// plumbing is safe regardless: (a) WITHOUT calling it, the guest behaves
/// exactly as before P2 (empty settings by default — the same default covered
/// by `host_config_sin_settings_es_mapa_vacio` in `norte-plugin-host` for the
/// underlying `Host` trait); (b) CALLING it (with the only honest value
/// available today: an empty map, since `provider-mem` declares no
/// `[config]`) breaks nothing.
#[tokio::test]
async fn plugin_provider_set_settings_is_safe_with_or_without_calling_it() {
    let Some(wasm) = build_guest("provider-mem") else {
        eprintln!("SKIP: target wasm32-wasip2 not installed");
        return;
    };
    let rt = PluginRuntime::new().expect("runtime");
    let wasm = norte_plugin_host::WasmArtifact::trusting_current(&wasm).expect("guest");
    let p = PluginProvider::new(rt, &wasm, HostCaps::default(), "mem").expect("adapter");
    let root = root();

    // Without set_settings: normal behavior (empty default, as always).
    assert_eq!(
        p.stat(&root).await.expect("root exists").kind,
        EntryKind::Dir
    );

    // With set_settings (empty map: the only honest option today, provider-mem
    // declares no [config]) — must break nothing.
    p.set_settings(std::collections::BTreeMap::new()).await;
    assert_eq!(
        p.stat(&root)
            .await
            .expect("still works after set_settings")
            .kind,
        EntryKind::Dir
    );
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
