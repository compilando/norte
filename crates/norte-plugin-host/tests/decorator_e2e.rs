//! E2E de la interfaz WIT `decorator` (ADR 0037 decisión 2, world
//! `norte-decorator`): compila el guest REAL `examples-wasm/decorator-demo` a
//! `wasm32-wasip2` y lo ejecuta a través de
//! [`PluginRuntime::instantiate_decorator`], verificando el contrato
//! POSICIONAL 1:1 (ADR 0037 tabla de decisión 1: `result[i]` decora
//! `entries[i]`, nunca reordenado ni disperso) contra un guest que badgea
//! `"M"` las entradas cuyo nombre contiene `"mod"`.
//!
//! SKIP si el target `wasm32-wasip2` no está instalado.

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
        .expect("instanciar el decorator");

    let entries: Vec<Entry> = [&b"module.rs"[..], b"README.md", b"my_mod_2.rs"]
        .into_iter()
        .map(|n| Entry {
            name: n.to_vec(),
            kind: EntryKind::File,
        })
        .collect();
    let out = inst.decorate(&entries).expect("decorate sin trap");

    assert_eq!(out.len(), entries.len(), "positional 1:1, nunca disperso");
    assert_eq!(
        out[0].badge.as_deref(),
        Some("M"),
        "\"module.rs\" contiene \"mod\" → badge"
    );
    assert_eq!(out[0].role.as_deref(), Some("warning"));
    assert_eq!(
        out[1].badge, None,
        "\"README.md\" no contiene \"mod\" → sin badge"
    );
    assert_eq!(out[1].role, None);
    assert_eq!(
        out[2].badge.as_deref(),
        Some("M"),
        "\"my_mod_2.rs\" contiene \"mod\" → badge"
    );

    // Un lote VACÍO es un caso límite legítimo (página sin entradas visibles):
    // el resultado es un vector vacío, no un error.
    let empty = inst.decorate(&[]).expect("lote vacío sin trap");
    assert!(empty.is_empty());
}
