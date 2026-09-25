//! E2E of `previewer::render-styled` (ADR 0037 decision 2, WIT 0.6.0):
//! compiles the REAL guest `examples-wasm/previewer-demo` to
//! `wasm32-wasip2` and runs [`PluginRuntime::instantiate`] +
//! `PluginInstance::render_styled_preview`, verifying:
//!
//! - Round-trip of REAL roles+fg from a guest that classifies tokens
//!   (digits → `role: "number"`; a fixed keyword → `role: "keyword"` +
//!   `fg`), not a mock.
//! - The FOUR anti-DoS caps of the ADR 0037 decision table 1 are applied
//!   POST-return from the guest and reject the WHOLE thing (fail-closed,
//!   they do not truncate): a real guest returning a line with more than
//!   256 spans triggers [`RuntimeError::StyledPreviewTooLarge`] — the
//!   caller (`norte-core`, out of this crate's scope) is the one who
//!   decides to fall back to the plain preview; here the runtime's
//!   contract is fixed.
//!
//! SKIP if the `wasm32-wasip2` target is not installed (see `support`).

use norte_plugin_host::{Capabilities, PluginRuntime, RuntimeError};

mod support;

#[test]
fn styled_preview_roundtrip_roles_and_fg_wasm_real() {
    let Some(wasm) = support::build_guest("previewer-demo") else {
        return;
    };
    let rt = PluginRuntime::new().expect("engine");
    let mut inst = rt
        .instantiate(&wasm, Capabilities::default())
        .expect("instance");

    let content = b"hello 42 TODO world";
    let lines = inst
        .render_styled_preview("text/plain", content, None)
        .expect("render-styled");

    // Line 0 = plain header (no role/fg), a single span.
    assert_eq!(lines[0].len(), 1, "header = one span");
    assert!(lines[0][0].role.is_none() && lines[0][0].fg.is_none());
    assert!(lines[0][0].text.contains("text/plain"));

    // Line 1 = the tokenized content: "hello" plain, "42" → number,
    // "TODO" → keyword with fixed fg, "world" plain.
    let spans = &lines[1];
    let numbers: Vec<_> = spans
        .iter()
        .filter(|s| s.role.as_deref() == Some("number"))
        .collect();
    assert_eq!(numbers.len(), 1, "one number span: {spans:?}");
    assert_eq!(numbers[0].text, "42");
    assert!(numbers[0].fg.is_none(), "number carries no fixed fg");

    let keywords: Vec<_> = spans
        .iter()
        .filter(|s| s.role.as_deref() == Some("keyword"))
        .collect();
    assert_eq!(keywords.len(), 1, "one keyword span: {spans:?}");
    assert_eq!(keywords[0].text, "TODO");
    assert_eq!(
        keywords[0].fg,
        Some((255, 200, 0)),
        "keyword carries a fixed fg IN ADDITION to the role (the host decides which paints)"
    );

    let plain: Vec<_> = spans.iter().filter(|s| s.role.is_none()).collect();
    assert!(
        plain.iter().any(|s| s.text == "hello") && plain.iter().any(|s| s.text == "world"),
        "unclassified tokens stay plain: {spans:?}"
    );
}

#[test]
fn styled_preview_exceeding_the_spans_per_line_cap_is_rejected_whole() {
    let Some(wasm) = support::build_guest("previewer-demo") else {
        return;
    };
    let rt = PluginRuntime::new().expect("engine");
    let mut inst = rt
        .instantiate(&wasm, Capabilities::default())
        .expect("instance");

    // The guest tokenizes on spaces and separates each token with an
    // extra single-character span: 130 words in one line → 259 spans
    // (130 tokens + 129 separators), above the 256/line cap (ADR
    // 0037 decision table 1, D4 amendment).
    let words: Vec<String> = (0..130).map(|i| format!("w{i}")).collect();
    let line = words.join(" ");
    let content = line.as_bytes();

    let err = inst
        .render_styled_preview("text/plain", content, None)
        .expect_err("a 259-span line exceeds the 256 cap");
    assert!(
        matches!(err, RuntimeError::StyledPreviewTooLarge(ref m) if m.contains("spans")),
        "was {err:?}"
    );
}
