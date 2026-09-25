//! Example WASM guest (M4-P2, P2 Task 4a; ADR 0037 G3): a minimal
//! previewer.
//!
//! Exports BOTH interfaces of the `norte-plugin` world (the world requires
//! both): `previewer::render` produces a header + the first 3 lines of
//! the content and logs a line via `host-log::log`; `command::run`
//! responds "not supported" because this guest is previewer-category only.
//!
//! If the plugin declares `[config.banner]`, `render` prepends the
//! resolved value (read via `host-config::get`, P2) to the render —
//! demonstrates that the previewer ALSO receives `[config]` (not just
//! `command`, Task 3). Without `[config]`/installed settings,
//! `host-config::get` returns `none` and the render is identical to
//! before P2 (backward compatible).
//!
//! `previewer::render-styled` (ADR 0037 decision 2, WIT 0.6.0) is a REAL
//! mini-highlighter, not the trivial single-span wrapper the ADR allows
//! for a text-only guest: the header is a plain span (no role); each
//! content line is tokenized on spaces and each token is classified —
//! pure digits → `role: "number"`; one of the fixed [`KEYWORDS`] words →
//! `role: "keyword"` WITH a fixed `fg` (to demonstrate that a span can
//! carry both fields at once, even though the host prefers `role` when
//! painting); anything else → plain span. Enough for the e2e to assert
//! real roles+fg and, with a line of enough words, trigger the host's
//! spans/line cap (ADR 0037 decision table 1).
#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

wit_bindgen::generate!({
    world: "norte-plugin",
    path: "wit",
    // `host-log`/`host-config` live in ANOTHER package since the split
    // (ADR 0041 decision 4); wit-bindgen requires explicitly deciding what
    // to do with imports from outside the world's package.
    generate_all,
});

use exports::norte::plugin::command::Guest as CommandGuest;
use exports::norte::plugin::previewer::{Guest as PreviewerGuest, PreviewInput, Span};
use norte::host::{host_config, host_log};

/// Fixed mini-highlighter keywords (deterministic for the e2e tests): any
/// EXACT token in this list is painted with `role: "keyword"`.
const KEYWORDS: &[&str] = &["TODO", "FIXME", "norte"];

struct Demo;

impl PreviewerGuest for Demo {
    fn render(input: PreviewInput) -> Result<String, String> {
        let n = input.content.len();
        host_log::log(&format!("previewer-demo: {n} bytes of {}", input.mimetype));
        let text = String::from_utf8_lossy(&input.content);
        let head: alloc::vec::Vec<&str> = text.lines().take(3).collect();
        // P2 Task 4a: `banner` is OPTIONAL — `none` if the manifest does
        // not declare `[config.banner]` (or no settings were installed),
        // in which case the prefix stays empty and the render is the same
        // as without P2.
        let banner = host_config::get("banner")
            .map(|b| format!("{b}\n"))
            .unwrap_or_default();
        Ok(format!(
            "{banner}[{}] {n} bytes\n{}",
            input.mimetype,
            head.join("\n")
        ))
    }

    fn render_styled(input: PreviewInput) -> Result<Vec<Vec<Span>>, String> {
        let n = input.content.len();
        let text = String::from_utf8_lossy(&input.content);
        let mut lines: Vec<Vec<Span>> = Vec::new();

        // Header: a single plain span (no role/fg), same content as
        // `render`'s prefix.
        lines.push(alloc::vec![Span {
            text: format!("[{}] {n} bytes", input.mimetype),
            role: None,
            fg: None,
            bg: None,
        }]);

        for line in text.lines().take(3) {
            lines.push(highlight_line(line));
        }
        Ok(lines)
    }
}

/// Tokenizes `line` on spaces (keeping a one-character separator span
/// between tokens, so the visual join is readable) and classifies each
/// token: pure digits → `number`; a fixed keyword → `keyword` (+ a fixed
/// `fg`); anything else → plain.
fn highlight_line(line: &str) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    let mut first = true;
    for word in line.split(' ') {
        if !first {
            spans.push(Span {
                text: " ".to_string(),
                role: None,
                fg: None,
                bg: None,
            });
        }
        first = false;
        let is_number = !word.is_empty() && word.bytes().all(|b| b.is_ascii_digit());
        if is_number {
            spans.push(Span {
                text: word.to_string(),
                role: Some("number".to_string()),
                fg: None,
                bg: None,
            });
        } else if KEYWORDS.contains(&word) {
            spans.push(Span {
                text: word.to_string(),
                role: Some("keyword".to_string()),
                fg: Some((255, 200, 0)),
                bg: None,
            });
        } else {
            spans.push(Span {
                text: word.to_string(),
                role: None,
                fg: None,
                bg: None,
            });
        }
    }
    spans
}

impl CommandGuest for Demo {
    fn run(_id: String, _arg: String) -> Result<String, String> {
        Err("previewer-demo does not provide commands".to_string())
    }
}

export!(Demo);
