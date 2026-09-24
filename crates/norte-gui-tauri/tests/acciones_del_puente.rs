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
const ACCIONES_DEL_HOST: &str = include_str!("../../norte-ui-host/tests/golden/actions.json");

/// The `action: "…"` literals of the `UiAction` union type in `types.ts`.
fn acciones_del_renderer() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let aguja = "action: \"";
    let mut desde = 0usize;
    while let Some(i) = TYPES_TS[desde..].find(aguja) {
        let ini = desde + i + aguja.len();
        let fin = ini + TYPES_TS[ini..].find('"').expect("closed literal");
        out.insert(TYPES_TS[ini..fin].to_owned());
        desde = fin;
    }
    out
}

/// The wire names the host pins in its golden: the VALUE of `"action"` in
/// each fixture, not the fixture's key (which is a test name).
fn acciones_del_host() -> BTreeSet<String> {
    let v: serde_json::Value = serde_json::from_str(ACCIONES_DEL_HOST).expect("golden JSON");
    v.as_object()
        .expect("fixture map")
        .values()
        .filter_map(|f| f.get("action").and_then(|a| a.as_str()).map(str::to_owned))
        .collect()
}

#[test]
fn toda_accion_que_el_renderer_envia_la_entiende_el_host() {
    let renderer = acciones_del_renderer();
    let host = acciones_del_host();
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
