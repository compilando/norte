//! The WIT is THREE packages (ADR 0041 decision 4), and this watches that
//! they stay that way.
//!
//! What the split buys is not visible in any behavior test: the
//! `examples-wasm/` guests are always recompiled against the current WIT,
//! so the whole suite passes just as green with one package as with
//! three. What breaks by putting them back together happens to a `.wasm`
//! artifact that is ALREADY compiled, outside this repo, on someone
//! else's machine: a package's version travels inside the name of each
//! interface, so a `provider` bump would rename `norte:plugin/previewer`
//! and that previewer would stop instantiating.
//!
//! And `provider` is going to move: ADR 0041 decision 3 says its gaps —
//! server-side copy, trash, attributes, resume, cancellation — get filled
//! in when a real plugin asks for them. Each one of those is a bump.
//!
//! That is why this looks at the STRUCTURE and not the behavior. It is the
//! only place where the damage can be detected before it is caused.

use std::path::{Path, PathBuf};

use norte_plugin_host::{SERVED_WIT, wit_mismatch, wit_packages};

mod support;

fn wit_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("wit")
}

fn read_wit(rel: &str) -> String {
    let p = wit_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("reading {}: {e}", p.display()))
}

/// Every package declares its name, and they are three different ones.
#[test]
fn they_are_three_packages_with_different_names() {
    for (file, package) in [
        ("norte-plugin.wit", "package norte:plugin@"),
        ("deps/host/host.wit", "package norte:host@"),
        ("deps/provider/provider.wit", "package norte:provider@"),
    ] {
        let src = read_wit(file);
        assert!(
            src.contains(package),
            "{file} must declare `{package}…`; if it merged with another package, \
             bumping one invalidates the other's .wasm files again (ADR 0041 d4)"
        );
    }
}

/// `provider` must NOT go back to the shared package. This is the concrete
/// regression.
#[test]
fn provider_does_not_go_back_to_the_shared_package() {
    let shared = read_wit("norte-plugin.wit");
    assert!(
        !shared.contains("interface provider"),
        "`interface provider` reappeared in norte:plugin. Any change to it \
         would rename norte:plugin/previewer and bring down already-compiled \
         third-party previewers (ADR 0041 d4)"
    );
    assert!(
        !shared.contains("world norte-provider"),
        "the `norte-provider` world reappeared in norte:plugin"
    );
}

/// The host's two gates live in their own package and NOT in the shared
/// one: all four worlds import them, so sharing a package with any
/// category ties its version to that category's.
#[test]
fn the_host_gates_are_in_their_own_package() {
    let host = read_wit("deps/host/host.wit");
    for iface in ["interface host-log", "interface host-config"] {
        assert!(host.contains(iface), "`{iface}` must live in norte:host");
    }
    let shared = read_wit("norte-plugin.wit");
    for iface in ["interface host-log", "interface host-config"] {
        assert!(
            !shared.contains(iface),
            "`{iface}` reappeared in norte:plugin"
        );
    }
}

/// Every cross-package reference is VERSIONED. Without the version, the
/// resolver does not find the package — the real error the split
/// produced — and the failure shows up as a broken `bindgen!`, far from
/// the WIT that caused it.
#[test]
fn cross_references_carry_a_version() {
    for file in [
        "norte-plugin.wit",
        "deps/provider/provider.wit",
        "deps/renamer/renamer.wit",
        "deps/hook/hook.wit",
        "deps/thumbnail/thumbnail.wit",
        "deps/panel/panel.wit",
    ] {
        let src = read_wit(file);
        for (i, line) in src.lines().enumerate() {
            let l = line.trim();
            if !l.starts_with("import norte:") && !l.starts_with("use norte:") {
                continue;
            }
            assert!(
                l.contains('@'),
                "{file}:{} references another package with no version: `{l}`",
                i + 1
            );
        }
    }
}

/// What the host SAYS it serves is what the `.wit` files declare. If
/// someone bumps `norte:plugin` to 0.9.0 and does not touch the table,
/// the catalog would list freshly compiled guests as broken — or worse,
/// load the old ones without warning.
#[test]
fn served_wit_matches_the_package_files() {
    let mut declared: Vec<(String, String)> = Vec::new();
    for file in [
        "norte-plugin.wit",
        "deps/host/host.wit",
        "deps/provider/provider.wit",
        "deps/location/location.wit",
        "deps/renamer/renamer.wit",
        "deps/hook/hook.wit",
        "deps/thumbnail/thumbnail.wit",
        "deps/panel/panel.wit",
    ] {
        let src = read_wit(file);
        let line = src
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with("package norte:"))
            .unwrap_or_else(|| panic!("{file} does not declare `package norte:…`"));
        let body = line.trim_start_matches("package ").trim_end_matches(';');
        let (package, version) = body.split_once('@').expect("version");
        declared.push((package.to_owned(), version.to_owned()));
    }
    declared.sort();
    let mut served: Vec<(String, String)> = SERVED_WIT
        .iter()
        .map(|(p, v)| ((*p).to_owned(), (*v).to_owned()))
        .collect();
    served.sort();
    assert_eq!(
        served, declared,
        "SERVED_WIT is not what the .wit files declare"
    );
}

/// A REAL guest's imports name the served packages, and a freshly
/// compiled guest is not a mismatch.
#[test]
fn wit_imports_of_a_real_guest_name_the_served_versions() {
    let Some(wasm) = support::build_guest("previewer-demo") else {
        return;
    };
    let bytes = std::fs::read(wasm).expect("reads the guest");
    let imports = wit_packages(&bytes);
    assert!(
        imports.contains(&("norte:plugin".to_owned(), "0.10.0".to_owned())),
        "{imports:?}"
    );
    assert!(
        imports.contains(&("norte:host".to_owned(), "0.1.0".to_owned())),
        "{imports:?}"
    );
    assert!(wit_mismatch(&imports).is_none());
}

/// A guest compiled against another version of the package is a mismatch
/// with BOTH versions in hand: its own and the served one. Built by
/// rewriting `@0.10.0` to `@0.70.0` in the real guest's bytes — same
/// length, so the sections stay valid and the reader walks them.
#[test]
fn a_guest_built_against_another_version_is_a_mismatch() {
    let Some(wasm) = support::build_guest("previewer-demo") else {
        return;
    };
    let bytes = std::fs::read(wasm).expect("reads the guest");
    // The EXPORTS side (`norte:plugin`). `0.70.0`: a version the host does
    // not serve for any package, so confusing package and version does
    // not happen by chance.
    let old = support::rewrite_bytes(&bytes, b"@0.10.0", b"@0.70.0");
    let imports = wit_packages(&old);
    let m = wit_mismatch(&imports).expect("mismatch");
    assert_eq!(m.package, "norte:plugin");
    assert_eq!(m.built_against, "0.70.0");
    assert_eq!(m.served, "0.10.0");

    // And the IMPORTS side (`norte:host`): either one can be mismatched.
    let old = support::rewrite_bytes(&bytes, b"@0.1.0", b"@0.0.9");
    let imports = wit_packages(&old);
    let m = wit_mismatch(&imports).expect("mismatch in imports");
    assert_eq!(m.package, "norte:host");
    assert_eq!(m.built_against, "0.0.9");
    assert_eq!(m.served, "0.1.0");
}

/// Bytes that are not a component have no imports: neither an error nor a
/// panic, which is what the catalog needs so that a garbage `plugin.wasm`
/// stays "without a binary" and not "catalog down".
#[test]
fn bytes_that_are_not_a_component_have_no_imports() {
    assert!(wit_packages(b"\0asm\x01\0\0\0").is_empty());
    assert!(wit_packages(b"garbage").is_empty());
    assert!(wit_packages(b"").is_empty());
    assert!(wit_mismatch(&[]).is_none());
}

/// The shared package imports from the host one, never the other way
/// around: `norte:host` is the graph's leaf. A cycle here goes undetected
/// by anyone until the resolver complains, and its message does not say
/// which of the two sides is the extra one.
#[test]
fn host_depends_on_nobody() {
    let host = read_wit("deps/host/host.wit");
    for line in host.lines() {
        let l = line.trim();
        assert!(
            !(l.starts_with("import norte:") || l.starts_with("use norte:")),
            "norte:host must be the graph's leaf, and it depends on something: `{l}`"
        );
    }
}
