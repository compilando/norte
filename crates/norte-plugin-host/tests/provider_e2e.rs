//! E2E of the WIT `provider` interface (#30 stage 2, ADR 0032): compiles
//! the REAL guest `examples-wasm/provider-mem` to `wasm32-wasip2` and runs
//! it through [`PluginRuntime::instantiate_provider`], verifying the
//! async→sync projection of the `Provider` trait:
//!
//! - `capabilities` → `read-only`.
//! - `stat` of file/dir/nonexistent.
//! - PAGINATED `list-dir` (pages of 2 → two calls with a cursor).
//! - RANGED `read` (offset+len, short chunk = EOF).
//! - a HOSTILE name (`a\xff\xfeb`) BYTE-EXACT round trip (rule 1).
//!
//! SKIP if the `wasm32-wasip2` target is not installed.

use std::path::PathBuf;
use std::process::Command;

use norte_plugin_host::{Capabilities, PluginRuntime, provider_iface::EntryKind};

/// Path segments from `&[&[u8]]`.
fn segs(parts: &[&[u8]]) -> Vec<Vec<u8>> {
    parts.iter().map(|p| p.to_vec()).collect()
}

// A single test (a single guest compilation, ~1.7 s) that walks the whole
// read path: hence its length.
#[expect(
    clippy::too_many_lines,
    reason = "walks the real guest's whole read path"
)]
#[test]
fn provider_wit_e2e_wasm_real() {
    let Some(wasm) = build_guest("provider-mem") else {
        eprintln!("SKIP: target wasm32-wasip2 not installed");
        return;
    };

    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut inst = rt
        .instantiate_provider(&wasm, Capabilities::default())
        .expect("instantiate the provider guest");

    // configure (#30 stage 3c): the mem-provider implements it as a no-op;
    // here only the host↔guest binding is checked to round-trip without
    // a trap.
    let cfg = norte_plugin_host::provider_iface::ProviderConfig {
        endpoint: String::new(),
        user: String::new(),
        password: String::new(),
        base: String::new(),
    };
    inst.configure(&cfg)
        .expect("configure without a trap")
        .expect("mem configure is a no-op");

    // capabilities: read-only.
    assert!(
        inst.capabilities().expect("capabilities").read_only,
        "provider-mem declares itself read-only"
    );

    // stat of a file: kind File + size of the "hola norte\n" content (11).
    let st = inst
        .stat(&segs(&[b"docs", b"hello.txt"]))
        .expect("stat without a trap")
        .expect("hello.txt exists");
    assert_eq!(st.kind, EntryKind::File);
    assert_eq!(st.name, b"hello.txt");
    assert_eq!(st.size, Some(11));

    // stat of a dir: kind Dir.
    let d = inst
        .stat(&segs(&[b"docs"]))
        .expect("stat without a trap")
        .expect("docs exists");
    assert_eq!(d.kind, EntryKind::Dir);

    // stat of a nonexistent one: VfsError::NotFound (logical, not a trap).
    assert!(
        inst.stat(&segs(&[b"does_not_exist"]))
            .expect("stat without a trap")
            .is_err(),
        "a nonexistent path gives a logical error"
    );

    // PAGINATED list-dir of the root: 3 children (docs, vacio.txt, hostile)
    // in pages of 2 → first page 2 + cursor, second 1 + None.
    let p1 = inst
        .list_dir(&[], None)
        .expect("list without a trap")
        .expect("root lists");
    assert_eq!(p1.entries.len(), 2, "first page = 2 entries");
    let cursor = p1.next_cursor.clone().expect("there is a second page");
    let p2 = inst
        .list_dir(&[], Some(&cursor))
        .expect("list without a trap")
        .expect("root page 2");
    assert_eq!(p2.entries.len(), 1, "second page = 1 entry");
    assert!(p2.next_cursor.is_none(), "no third page");
    // Reassembled: the 3 names.
    let mut names: Vec<Vec<u8>> = p1
        .entries
        .iter()
        .chain(&p2.entries)
        .map(|e| e.name.clone())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![b"docs".to_vec(), b"hostile".to_vec(), b"vacio.txt".to_vec()]
    );

    // RANGED read: "hola norte\n" → offset 5, len 5 = "norte".
    let chunk = inst
        .read(&segs(&[b"docs", b"hello.txt"]), 5, 5)
        .expect("read without a trap")
        .expect("hello.txt readable");
    assert_eq!(chunk, b"norte");
    // len that overshoots the end → short chunk (EOF): from 5, asks for
    // 100, gives 6.
    let tail = inst
        .read(&segs(&[b"docs", b"hello.txt"]), 5, 100)
        .expect("read without a trap")
        .expect("readable");
    assert_eq!(tail, b"norte\n");
    // offset at the end → empty chunk (EOF).
    let eof = inst
        .read(&segs(&[b"docs", b"hello.txt"]), 11, 10)
        .expect("read without a trap")
        .expect("readable");
    assert!(eof.is_empty(), "offset at EOF = empty chunk");
    // HOSTILE offset/len (u64::MAX): the guest's clamping (usize::try_from
    // + saturating + min) must not panic on a 32-bit guest.
    let huge_off = inst
        .read(&segs(&[b"docs", b"hello.txt"]), u64::MAX, u64::MAX)
        .expect("read without a trap")
        .expect("readable");
    assert!(huge_off.is_empty(), "offset u64::MAX = empty, no panic");
    let huge_len = inst
        .read(&segs(&[b"docs", b"hello.txt"]), 0, u64::MAX)
        .expect("read without a trap")
        .expect("readable");
    assert_eq!(
        huge_len, b"hola norte\n",
        "len u64::MAX is capped to the file"
    );

    // empty file: read gives empty; stat gives size 0.
    assert_eq!(
        inst.stat(&segs(&[b"vacio.txt"])).unwrap().unwrap().size,
        Some(0)
    );

    // BYTE-EXACT HOSTILE NAME (rule 1): /hostile/<a\xff\xfeb> exists, is
    // listed with its raw bytes, and its content is those same bytes.
    let hostile: &[u8] = b"a\xff\xfeb";
    let slash: &[u8] = b"a/b"; // INTERIOR `/`: a single segment, not a separator
    let hp = inst
        .list_dir(&segs(&[b"hostile"]), None)
        .expect("list without a trap")
        .expect("hostile lists");
    assert_eq!(hp.entries.len(), 2);
    let names: Vec<&[u8]> = hp.entries.iter().map(|e| e.name.as_slice()).collect();
    assert!(
        names.contains(&hostile),
        "the non-UTF-8 name crosses the WIT byte for byte"
    );
    assert!(
        names.contains(&slash),
        "a name with an interior `/` is ONE segment, not two"
    );
    let hbytes = inst
        .read(&segs(&[b"hostile", hostile]), 0, 100)
        .expect("read without a trap")
        .expect("readable");
    assert_eq!(
        hbytes, hostile,
        "the hostile content round-trips byte-exact"
    );
    // The interior `/` is read as ONE segment: if the WIT treated it as a
    // separator, this path would have 3 segments and would not resolve.
    let sbytes = inst
        .read(&segs(&[b"hostile", slash]), 0, 100)
        .expect("read without a trap")
        .expect("readable");
    assert_eq!(sbytes, slash, "the interior `/` does not split the segment");
}

/// Compiles `examples-wasm/<name>/` to `wasm32-wasip2` (release). `None`
/// (SKIP) if the target is not there; if it is but it does not compile, it
/// is a real failure.
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
        .expect("could not launch cargo to compile the guest");
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
