//! E2E of the WIT `decorator` interface (ADR 0037 decision 2, world
//! `norte-decorator`): compiles the REAL guest
//! `examples-wasm/decorator-demo` to `wasm32-wasip2` and runs it through
//! [`PluginRuntime::instantiate_decorator`], verifying the POSITIONAL 1:1
//! contract (ADR 0037 decision table 1: `result[i]` decorates
//! `entries[i]`, never reordered nor sparse) against a guest that badges
//! `"M"` on entries whose name contains `"mod"`.
//!
//! SKIP if the `wasm32-wasip2` target is not installed.

use norte_plugin_host::decorator_iface::{Entry, EntryKind};
use norte_plugin_host::{Capabilities, PluginRuntime};

mod support;

#[test]
fn decorator_wit_e2e_positional_roundtrip_wasm_real() {
    let Some(wasm) = support::build_guest("decorator-demo") else {
        return;
    };

    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut inst = rt
        .instantiate_decorator(&wasm, Capabilities::default())
        .expect("instantiate the decorator guest");

    let entries: Vec<Entry> = [&b"module.rs"[..], b"README.md", b"my_mod_2.rs"]
        .into_iter()
        .map(|n| Entry {
            name: n.to_vec(),
            kind: EntryKind::File,
        })
        .collect();
    let out = inst.decorate(&entries).expect("decorate without a trap");

    assert_eq!(out.len(), entries.len(), "positional 1:1, never sparse");
    assert_eq!(
        out[0].badge.as_deref(),
        Some("M"),
        "\"module.rs\" contains \"mod\" → badge"
    );
    assert_eq!(out[0].role.as_deref(), Some("warning"));
    assert_eq!(
        out[1].badge, None,
        "\"README.md\" does not contain \"mod\" → no badge"
    );
    assert_eq!(out[1].role, None);
    assert_eq!(
        out[2].badge.as_deref(),
        Some("M"),
        "\"my_mod_2.rs\" contains \"mod\" → badge"
    );

    // An EMPTY batch is a legitimate edge case (a page with no visible
    // entries): the result is an empty vector, not an error.
    let empty = inst.decorate(&[]).expect("empty batch without a trap");
    assert!(empty.is_empty());
}
