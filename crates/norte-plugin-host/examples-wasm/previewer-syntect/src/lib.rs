//! WASM guest (#29): previewer with REAL syntax highlighting via syntect.
//!
//! `previewer::render` receives text ALREADY decoded by the core (§6.2 — it
//! does not assume UTF-8 over raw bytes; it still does a defensive
//! `from_utf8_lossy`), picks a syntax by mimetype (or by the first line)
//! and returns the highlighted content as 24-bit ANSI. The host SANITIZES
//! it (`ansi` frontend: only foreground color reaches the pane).
//! `command::run` responds "not supported" (previewer-category only
//! guest).

use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::as_24_bit_terminal_escaped;

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
use norte::host::host_log;

struct Syntect;

/// Maps the mimetype (the core's coarse grain) to an extension token
/// syntect recognizes. `None` = try detection by the first line.
fn ext_for_mime(mime: &str) -> Option<&'static str> {
    match mime {
        "application/json" => Some("json"),
        "text/html" => Some("html"),
        "text/xml" | "application/xml" => Some("xml"),
        "text/javascript" | "application/javascript" => Some("js"),
        "text/css" => Some("css"),
        "text/markdown" => Some("md"),
        _ => None,
    }
}

/// Picks the `SyntaxReference`: by the mimetype's extension, failing that
/// by the content's first line, and failing that, plain text (no
/// highlighting).
fn pick_syntax<'a>(ps: &'a SyntaxSet, mime: &str, text: &str) -> &'a SyntaxReference {
    if let Some(ext) = ext_for_mime(mime) {
        if let Some(s) = ps.find_syntax_by_extension(ext) {
            return s;
        }
    }
    text.lines()
        .next()
        .and_then(|l| ps.find_syntax_by_first_line(l))
        .unwrap_or_else(|| ps.find_syntax_plain_text())
}

impl PreviewerGuest for Syntect {
    fn render(input: PreviewInput) -> Result<String, String> {
        // The host already decoded (§6.2); the lossy is defensive, it
        // never assumes UTF-8 over raw bytes.
        let text = String::from_utf8_lossy(&input.content);
        host_log::log(&format!(
            "previewer-syntect: {} bytes of {}",
            input.content.len(),
            input.mimetype
        ));

        let (ps, theme) = highlighter()?;
        let syntax = pick_syntax(&ps, &input.mimetype, &text);
        let mut hl = HighlightLines::new(syntax, &theme);

        let mut out = String::new();
        for line in text.lines() {
            let ranges = hl
                .highlight_line(line, &ps)
                .map_err(|e| format!("syntect: {e}"))?;
            out.push_str(&as_24_bit_terminal_escaped(&ranges[..], false));
            // Explicit reset at end of line (the sanitizer understands it) + \n.
            out.push_str("\x1b[0m\n");
        }
        Ok(out)
    }

    // ADR 0037 (WIT 0.6.0): the styled twin, and the one the viewer calls.
    // The colour travels in `fg`, straight from syntect's ranges: never as
    // escapes inside `text`, which the viewer paints as text (#373).
    fn render_styled(input: PreviewInput) -> Result<Vec<Vec<Span>>, String> {
        let text = String::from_utf8_lossy(&input.content);
        let (ps, theme) = highlighter()?;
        let syntax = pick_syntax(&ps, &input.mimetype, &text);
        let mut hl = HighlightLines::new(syntax, &theme);
        text.lines()
            .map(|line| {
                let ranges = hl
                    .highlight_line(line, &ps)
                    .map_err(|e| format!("syntect: {e}"))?;
                Ok(ranges
                    .into_iter()
                    .map(|(style, piece)| Span {
                        text: piece.to_string(),
                        role: None,
                        fg: Some((style.foreground.r, style.foreground.g, style.foreground.b)),
                        bg: None,
                    })
                    .collect())
            })
            .collect()
    }
}

/// The syntaxes and the theme both renders use.
fn highlighter() -> Result<(SyntaxSet, syntect::highlighting::Theme), String> {
    let ps = SyntaxSet::load_defaults_newlines();
    let mut ts = ThemeSet::load_defaults();
    let theme = ts
        .themes
        .remove("base16-ocean.dark")
        .ok_or("base16-ocean.dark theme missing")?;
    Ok((ps, theme))
}

impl CommandGuest for Syntect {
    fn run(_id: String, _arg: String) -> Result<String, String> {
        Err("previewer-syntect does not provide commands".to_string())
    }
}

export!(Syntect);
