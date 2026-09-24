//! E2E of the WIT `columns` interface (ADR 0037 decision 2, world
//! `norte-columns`): compiles the REAL guest `examples-wasm/columns-demo`
//! to `wasm32-wasip2` and runs it through
//! [`PluginRuntime::instantiate_columns`], verifying the POSITIONAL 1:1
//! contract (ADR 0037 decision table 1: `result[i]` values `entries[i]`,
//! never reordered nor sparse) against a guest that values the
//! `"name-len"` column with the name's length in bytes, and answers `none`
//! for the whole page when asked for a column id it does not declare.
//!
//! SKIP if the `wasm32-wasip2` target is not installed.

use norte_plugin_host::{Capabilities, PluginRuntime};

mod support;

/// The location as it crosses to the guest: token and empty prefix (the
/// root IS the visible directory in these tests).
fn loc(token: &str) -> norte_plugin_host::columns_iface::LocationRef {
    norte_plugin_host::columns_iface::LocationRef {
        token: token.to_owned(),
        prefix: Vec::new(),
    }
}

#[test]
fn columns_wit_e2e_positional_roundtrip_wasm_real() {
    let Some(wasm) = support::build_guest("columns-demo") else {
        return;
    };

    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut inst = rt
        .instantiate_columns(&wasm, Capabilities::default())
        .expect("instantiate the columns guest");

    let entries: Vec<Vec<u8>> = vec![b"a.rs".to_vec(), b"README.md".to_vec(), b"x".to_vec()];
    let out = inst
        .column_values("name-len", None, &entries)
        .expect("column_values without a trap");

    assert_eq!(out.len(), entries.len(), "positional 1:1, never sparse");
    assert_eq!(out[0].as_deref(), Some("4"), "\"a.rs\" = 4 bytes");
    assert_eq!(out[1].as_deref(), Some("9"), "\"README.md\" = 9 bytes");
    assert_eq!(out[2].as_deref(), Some("1"), "\"x\" = 1 byte");

    // A column id the guest does NOT declare: `none` for the WHOLE page,
    // never guessed nor omitted from the positional vector.
    let unknown = inst
        .column_values("not-declared", None, &entries)
        .expect("column_values without a trap");
    assert_eq!(unknown, vec![None, None, None]);

    // An EMPTY batch is a legitimate edge case (a page with no visible
    // entries).
    let empty = inst
        .column_values("name-len", None, &[])
        .expect("empty batch without a trap");
    assert!(empty.is_empty());
}

/// ADR 0057: a guest WITHOUT the `location` capability approved resolves
/// not a single byte, and the host does not even look at the token — the
/// same criterion as scoped `fs-read`. The guest keeps answering: empty
/// cells, not a jam.
#[test]
fn without_the_capability_the_host_denies_before_touching_anything_wasm_real() {
    let Some(wasm) = support::build_guest("columns-demo") else {
        return;
    };
    let spy = std::sync::Arc::new(support::SpyLocation::default());
    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut inst = rt
        .instantiate_columns_with_location(
            &wasm,
            Capabilities::default(), // without `location`
            Some(spy.clone()),
        )
        .expect("instantiate the columns guest");

    let out = inst
        .column_values("stat-size", Some(&loc("tok")), &[b"a.txt".to_vec()])
        .expect("column_values without a trap");
    assert_eq!(out, vec![None], "without the capability, an empty cell");
    assert_eq!(spy.calls(), 0, "the host did not resolve a single byte");
}

/// With the capability approved AND an injected resolver, the guest
/// really reads.
#[test]
fn with_the_capability_the_guest_reads_under_the_token_wasm_real() {
    let Some(wasm) = support::build_guest("columns-demo") else {
        return;
    };
    let spy = std::sync::Arc::new(support::SpyLocation::default());
    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut inst = rt
        .instantiate_columns_with_location(&wasm, support::caps_with_location(), Some(spy.clone()))
        .expect("instantiate the columns guest");

    let out = inst
        .column_values(
            "stat-size",
            Some(&loc("tok")),
            &[b"a.txt".to_vec(), b"no".to_vec()],
        )
        .expect("column_values without a trap");
    assert_eq!(
        out,
        vec![Some("42".to_owned()), None],
        "what the host resolves arrives as-is; what it doesn't, an empty cell"
    );
    assert_eq!(spy.calls(), 2, "one call per entry");
}

/// Without a token — the host could not open the directory — the guest
/// does not fail either.
#[test]
fn without_a_token_the_guest_keeps_answering_wasm_real() {
    let Some(wasm) = support::build_guest("columns-demo") else {
        return;
    };
    let spy = std::sync::Arc::new(support::SpyLocation::default());
    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut inst = rt
        .instantiate_columns_with_location(&wasm, support::caps_with_location(), Some(spy.clone()))
        .expect("instantiate the columns guest");
    let out = inst
        .column_values("stat-size", None, &[b"a.txt".to_vec()])
        .expect("column_values without a trap");
    assert_eq!(out, vec![None]);
    assert_eq!(spy.calls(), 0);
}
