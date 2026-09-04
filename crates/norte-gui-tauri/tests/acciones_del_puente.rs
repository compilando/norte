//! Toda acción que el renderer ENVÍA existe con ese nombre en el wire del
//! host.
//!
//! `UiAction` se deserializa en la frontera Tauri con `rename_all =
//! "snake_case"`, así que el nombre que viaja lo decide el Rust
//! (`PanelBarActivate` → `panel_bar_activate`) y el TypeScript lo escribe a
//! mano. Un nombre que no case no se cae: `dispatch` devuelve un error que
//! el renderer no pinta, y el botón no hace NADA. Así estuvo la barra de
//! paneles entera (`panelbar_activate`) — con `gui-ci` verde, porque el
//! test del renderer comprobaba lo que el renderer decía enviar, no lo que
//! el host entiende.
//!
//! El golden de ui-host (`tests/golden/actions.json`) es la lista de nombres
//! que el host acepta, fijada por su propio test. Este lee `types.ts`, que
//! es la única fuente de verdad de lo que el renderer envía.

use std::collections::BTreeSet;

const TYPES_TS: &str = include_str!("../ui/src/types.ts");
const ACCIONES_DEL_HOST: &str = include_str!("../../norte-ui-host/tests/golden/actions.json");

/// Los literales `action: "…"` del tipo de unión `UiAction` en `types.ts`.
fn acciones_del_renderer() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let aguja = "action: \"";
    let mut desde = 0usize;
    while let Some(i) = TYPES_TS[desde..].find(aguja) {
        let ini = desde + i + aguja.len();
        let fin = ini + TYPES_TS[ini..].find('"').expect("literal cerrado");
        out.insert(TYPES_TS[ini..fin].to_owned());
        desde = fin;
    }
    out
}

/// Los nombres de wire que el host fija en su golden: el VALOR de `"action"`
/// en cada fixture, no la clave del fixture (que es un nombre de test).
fn acciones_del_host() -> BTreeSet<String> {
    let v: serde_json::Value = serde_json::from_str(ACCIONES_DEL_HOST).expect("golden JSON");
    v.as_object()
        .expect("mapa de fixtures")
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
        "el barrido no leyó `types.ts`: {renderer:?}"
    );
    assert!(host.len() >= 40, "el golden no se leyó: {host:?}");
    let huerfanas: Vec<&String> = renderer.difference(&host).collect();
    assert!(
        huerfanas.is_empty(),
        "el renderer envía acciones que el host no deserializa (el clic muere en silencio): {huerfanas:?}"
    );
}
