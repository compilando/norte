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
                let mut spans: Vec<Span> = Vec::new();
                for (style, piece) in ranges {
                    let fg = (style.foreground.r, style.foreground.g, style.foreground.b);
                    match spans.last_mut() {
                        // Neighbours of one colour are one span: a minified
                        // line has thousands of tokens and a handful of colours.
                        Some(last) if last.fg == Some(fg) => last.text.push_str(piece),
                        _ => spans.push(Span {
                            text: piece.to_string(),
                            role: None,
                            fg: Some(fg),
                            bg: None,
                        }),
                    }
                }
                Ok(fit_line(spans, line))
            })
            .collect()
    }
}

/// The host's caps per line (`norte-plugin-host`, ADR 0037): a line over
/// either is REJECTED with the whole preview, and the viewer falls back to
/// no highlighting at all.
const MAX_SPANS_PER_LINE: usize = 256;
const MAX_SPAN_BYTES: usize = 4 * 1024;

/// A line the host will accept: the highlighted spans if they fit, otherwise
/// the line uncoloured in chunks that do — one dense line loses its colour,
/// not the whole file's. Past 256 chunks (1 MiB in one line) the rest of that
/// line is not shown; the host's total cap is 4 MiB anyway.
fn fit_line(spans: Vec<Span>, line: &str) -> Vec<Span> {
    if spans.len() <= MAX_SPANS_PER_LINE && spans.iter().all(|s| s.text.len() <= MAX_SPAN_BYTES) {
        return spans;
    }
    let mut chunks = Vec::new();
    let mut rest = line;
    while !rest.is_empty() && chunks.len() < MAX_SPANS_PER_LINE {
        let mut end = rest.len().min(MAX_SPAN_BYTES);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        chunks.push(Span {
            text: rest[..end].to_string(),
            role: None,
            fg: None,
            bg: None,
        });
        rest = &rest[end..];
    }
    chunks
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
