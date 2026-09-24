//! Every action the renderer SENDS exists under that name on the host's
//! wire.
//!
//! `UiAction` is deserialized at the Tauri boundary with `rename_all =
//! "snake_case"`, so the name that travels is decided by Rust
//! (`PanelBarActivate` → `panel_bar_activate`) and TypeScript writes it by
//! hand. A name that does not match does not crash: `dispatch` returns an
//! error the renderer does not paint, and the button does NOTHING. That is
//! how the whole panel bar stood (`panelbar_activate`) — with `gui-ci` green,
//! because the renderer's test checked what the renderer said it sent, not
//! what the host understands.
//!
//! ui-host's golden (`tests/golden/actions.json`) is the list of names the
//! host accepts, pinned by its own test. This one reads `types.ts`, which is
//! the only source of truth for what the renderer sends.

use std::collections::BTreeSet;

const TYPES_TS: &str = include_str!("../ui/src/types.ts");
const HOST_ACTIONS: &str = include_str!("../../norte-ui-host/tests/golden/actions.json");

/// The `action: "…"` literals of the `UiAction` union type in `types.ts`.
fn renderer_actions() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let needle = "action: \"";
    let mut from = 0usize;
    while let Some(i) = TYPES_TS[from..].find(needle) {
        let ini = from + i + needle.len();
        let fin = ini + TYPES_TS[ini..].find('"').expect("closed literal");
        out.insert(TYPES_TS[ini..fin].to_owned());
        from = fin;
    }
    out
}

/// The wire names the host pins in its golden: the VALUE of `"action"` in
/// each fixture, not the fixture's key (which is a test name).
fn host_actions() -> BTreeSet<String> {
    let v: serde_json::Value = serde_json::from_str(HOST_ACTIONS).expect("golden JSON");
    v.as_object()
        .expect("fixture map")
        .values()
        .filter_map(|f| f.get("action").and_then(|a| a.as_str()).map(str::to_owned))
        .collect()
}

#[test]
fn every_action_the_renderer_sends_the_host_understands() {
    let renderer = renderer_actions();
    let host = host_actions();
    assert!(
        renderer.len() >= 40,
        "the sweep did not read `types.ts`: {renderer:?}"
    );
    assert!(host.len() >= 40, "the golden was not read: {host:?}");
    let huerfanas: Vec<&String> = renderer.difference(&host).collect();
    assert!(
        huerfanas.is_empty(),
        "the renderer sends actions the host does not deserialize (the click dies silently): {huerfanas:?}"
    );
}
