//! E2E de `previewer::render-styled` (ADR 0037 decisión 2, WIT 0.6.0):
//! compila el guest REAL `examples-wasm/previewer-demo` a `wasm32-wasip2` y
//! ejecuta [`PluginRuntime::instantiate`] + `PluginInstance::render_styled_preview`,
//! verificando:
//!
//! - Round-trip de roles+fg REALES desde un guest que clasifica tokens
//!   (dígitos → `role: "number"`; palabra clave fija → `role: "keyword"` +
//!   `fg`), no un mock.
//! - Los CUATRO topes anti-DoS de la tabla ADR 0037 decisión 1 se aplican
//!   POST-retorno del guest y rechazan ENTERO (fail-closed, no truncan): un
//!   guest real que devuelve una línea con más de 64 spans dispara
//!   [`RuntimeError::StyledPreviewTooLarge`] — el caller (`norte-core`, fuera
//!   de alcance de este crate) es quien decide caer a la previsualización
//!   plana; aquí se fija el contrato del runtime.
//!
//! SKIP si el target `wasm32-wasip2` no está instalado (ver `support`).

use norte_plugin_host::{Capabilities, PluginRuntime, RuntimeError};

mod support;

#[test]
fn styled_preview_roundtrip_roles_y_fg_wasm_real() {
    let Some(wasm) = support::build_guest("previewer-demo") else {
        return;
    };
    let rt = PluginRuntime::new().expect("engine");
    let mut inst = rt
        .instantiate(&wasm, Capabilities::default())
        .expect("instancia");

    let content = b"hola 42 TODO mundo";
    let lines = inst
        .render_styled_preview("text/plain", content)
        .expect("render-styled");

    // Línea 0 = cabecera plana (sin rol/fg), un único span.
    assert_eq!(lines[0].len(), 1, "cabecera = un span");
    assert!(lines[0][0].role.is_none() && lines[0][0].fg.is_none());
    assert!(lines[0][0].text.contains("text/plain"));

    // Línea 1 = el contenido tokenizado: "hola" plano, "42" → number,
    // "TODO" → keyword con fg fijo, "mundo" plano.
    let spans = &lines[1];
    let numbers: Vec<_> = spans
        .iter()
        .filter(|s| s.role.as_deref() == Some("number"))
        .collect();
    assert_eq!(numbers.len(), 1, "un span número: {spans:?}");
    assert_eq!(numbers[0].text, "42");
    assert!(numbers[0].fg.is_none(), "number no lleva fg fijo");

    let keywords: Vec<_> = spans
        .iter()
        .filter(|s| s.role.as_deref() == Some("keyword"))
        .collect();
    assert_eq!(keywords.len(), 1, "un span keyword: {spans:?}");
    assert_eq!(keywords[0].text, "TODO");
    assert_eq!(
        keywords[0].fg,
        Some((255, 200, 0)),
        "keyword lleva fg fijo ADEMÁS del rol (el host decide cuál pinta)"
    );

    let plain: Vec<_> = spans.iter().filter(|s| s.role.is_none()).collect();
    assert!(
        plain.iter().any(|s| s.text == "hola") && plain.iter().any(|s| s.text == "mundo"),
        "los tokens no clasificados quedan planos: {spans:?}"
    );
}

#[test]
fn styled_preview_supera_tope_de_spans_por_linea_se_rechaza_entero() {
    let Some(wasm) = support::build_guest("previewer-demo") else {
        return;
    };
    let rt = PluginRuntime::new().expect("engine");
    let mut inst = rt
        .instantiate(&wasm, Capabilities::default())
        .expect("instancia");

    // El guest tokeniza por espacios y separa cada token con un span
    // adicional de un solo carácter: 40 palabras en una línea → 79 spans
    // (40 tokens + 39 separadores), por encima del tope de 64/línea (ADR
    // 0037 tabla de decisión 1).
    let words: Vec<String> = (0..40).map(|i| format!("w{i}")).collect();
    let line = words.join(" ");
    let content = line.as_bytes();

    let err = inst
        .render_styled_preview("text/plain", content)
        .expect_err("una línea de 79 spans supera el tope de 64");
    assert!(
        matches!(err, RuntimeError::StyledPreviewTooLarge(ref m) if m.contains("spans")),
        "fue {err:?}"
    );
}
