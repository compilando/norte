//! Guest WASM (#29): previewer con syntax-highlight REAL vía syntect.
//!
//! `previewer::render` recibe el texto YA decodificado por el core (§6.2 — no
//! asume UTF-8 sobre bytes crudos; aun así hace `from_utf8_lossy` defensivo),
//! elige un syntax por el mimetype (o por la primera línea) y devuelve el
//! contenido resaltado como ANSI de 24 bits. El host lo SANEA (frontend `ansi`:
//! solo color de primer plano llega al pane). `command::run` responde "no
//! soportado" (guest solo de categoría previewer).

use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::as_24_bit_terminal_escaped;

wit_bindgen::generate!({
    world: "norte-plugin",
    path: "wit",
});

use exports::norte::plugin::command::Guest as CommandGuest;
use exports::norte::plugin::previewer::{Guest as PreviewerGuest, PreviewInput, Span};
use norte::plugin::host_log;

struct Syntect;

/// Mapea el mimetype (grano grueso del core) a un token de extensión que
/// syntect reconoce. `None` = intentar detección por la primera línea.
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

/// Elige el `SyntaxReference`: por extensión del mimetype, si no por la primera
/// línea del contenido, y si no, texto plano (sin resaltar).
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
        // El host ya decodificó (§6.2); el lossy es defensivo, jamás asume
        // UTF-8 sobre bytes crudos.
        let text = String::from_utf8_lossy(&input.content);
        host_log::log(&format!(
            "previewer-syntect: {} bytes de {}",
            input.content.len(),
            input.mimetype
        ));

        let ps = SyntaxSet::load_defaults_newlines();
        let ts = ThemeSet::load_defaults();
        let theme = ts
            .themes
            .get("base16-ocean.dark")
            .ok_or("tema base16-ocean.dark ausente")?;
        let syntax = pick_syntax(&ps, &input.mimetype, &text);
        let mut hl = HighlightLines::new(syntax, theme);

        let mut out = String::new();
        for line in text.lines() {
            let ranges = hl
                .highlight_line(line, &ps)
                .map_err(|e| format!("syntect: {e}"))?;
            out.push_str(&as_24_bit_terminal_escaped(&ranges[..], false));
            // Reset explícito al fin de línea (el saneador lo entiende) + \n.
            out.push_str("\x1b[0m\n");
        }
        Ok(out)
    }

    // ADR 0037 (WIT 0.6.0): `render-styled` es un export REQUERIDO de
    // `previewer`. Este guest no traduce su resaltado ANSI a spans
    // estructurados (deuda: haría falta parsear SGR igual que
    // `norte-frontend::ansi`, fuera de alcance de G3 Task 2) — implementa el
    // envoltorio TRIVIAL que el ADR reserva para un guest de solo-texto: un
    // span plano por línea, sin `role` ni `fg`. El texto por span lleva los
    // códigos ANSI crudos de `render` (el host los vería como texto literal
    // si alguien llamase a `preview_styled` sobre este guest hoy); un futuro
    // parseo real de SGR es la mejora natural, no requerida por este guest.
    fn render_styled(input: PreviewInput) -> Result<Vec<Vec<Span>>, String> {
        let plain = Self::render(input)?;
        Ok(plain
            .lines()
            .map(|l| {
                vec![Span {
                    text: l.to_string(),
                    role: None,
                    fg: None,
                }]
            })
            .collect())
    }
}

impl CommandGuest for Syntect {
    fn run(_id: String, _arg: String) -> Result<String, String> {
        Err("previewer-syntect no aporta comandos".to_string())
    }
}

export!(Syntect);
