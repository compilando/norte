//! Guest WASM de ejemplo (M4-P2, P2 Task 4a; ADR 0037 G3): un previewer
//! mínimo.
//!
//! Exporta AMBAS interfaces del world `norte-plugin` (el world las exige las
//! dos): `previewer::render` produce una cabecera + las primeras 3 líneas del
//! contenido y registra una línea vía `host-log::log`; `command::run` responde
//! "no soportado" porque este guest es solo de categoría previewer.
//!
//! Si el plugin declara `[config.banner]`, `render` antepone el valor
//! resuelto (leído vía `host-config::get`, P2) al render — demuestra que el
//! previewer TAMBIÉN recibe `[config]` (no solo `command`, Task 3). Sin
//! `[config]`/settings instaladas, `host-config::get` devuelve `none` y el
//! render es idéntico al de antes de P2 (retrocompatible).
//!
//! `previewer::render-styled` (ADR 0037 decisión 2, WIT 0.6.0) es un
//! mini-highlighter REAL, no el envoltorio trivial de un solo span que el
//! ADR permite para un guest de solo-texto: la cabecera es un span plano
//! (sin rol); cada línea de contenido se tokeniza por espacios y cada token
//! se clasifica — dígitos puros → `role: "number"`; una de las palabras
//! clave fijas de [`KEYWORDS`] → `role: "keyword"` CON un `fg` fijo (para
//! demostrar que un span puede llevar ambos campos a la vez, aunque el host
//! prefiera `role` al pintar); cualquier otra cosa → span plano. Suficiente
//! para que los e2e afirmen roles+fg reales y, con una línea de bastantes
//! palabras, disparen el tope de spans/línea del host (ADR 0037 tabla de
//! decisión 1).
#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

wit_bindgen::generate!({
    world: "norte-plugin",
    path: "wit",
    // `host-log`/`host-config` viven en OTRO paquete desde la partición
    // (ADR 0041 decisión 4); wit-bindgen exige decidir explícitamente qué
    // hacer con los imports de fuera del paquete del world.
    generate_all,
});

use exports::norte::plugin::command::Guest as CommandGuest;
use exports::norte::plugin::previewer::{Guest as PreviewerGuest, PreviewInput, Span};
use norte::host::{host_config, host_log};

/// Palabras clave fijas del mini-highlighter (deterministas para los e2e):
/// cualquier token EXACTO en esta lista se pinta con `role: "keyword"`.
const KEYWORDS: &[&str] = &["TODO", "FIXME", "norte"];

struct Demo;

impl PreviewerGuest for Demo {
    fn render(input: PreviewInput) -> Result<String, String> {
        let n = input.content.len();
        host_log::log(&format!("previewer-demo: {n} bytes de {}", input.mimetype));
        let text = String::from_utf8_lossy(&input.content);
        let head: alloc::vec::Vec<&str> = text.lines().take(3).collect();
        // P2 Task 4a: `banner` es OPCIONAL — `none` si el manifiesto no
        // declara `[config.banner]` (o no se instalaron settings), en cuyo
        // caso el prefijo queda vacío y el render es igual que sin P2.
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

        // Cabecera: un único span plano (sin rol/fg), igual contenido que el
        // prefijo de `render`.
        lines.push(alloc::vec![Span {
            text: format!("[{}] {n} bytes", input.mimetype),
            role: None,
            fg: None,
        }]);

        for line in text.lines().take(3) {
            lines.push(highlight_line(line));
        }
        Ok(lines)
    }
}

/// Tokeniza `line` por espacios (conservando un span-separador de un
/// carácter entre tokens, para que el join visual sea legible) y clasifica
/// cada token: dígitos puros → `number`; keyword fija → `keyword` (+ `fg`
/// fijo); cualquier otra cosa → plano.
fn highlight_line(line: &str) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    let mut first = true;
    for word in line.split(' ') {
        if !first {
            spans.push(Span {
                text: " ".to_string(),
                role: None,
                fg: None,
            });
        }
        first = false;
        let is_number = !word.is_empty() && word.bytes().all(|b| b.is_ascii_digit());
        if is_number {
            spans.push(Span {
                text: word.to_string(),
                role: Some("number".to_string()),
                fg: None,
            });
        } else if KEYWORDS.contains(&word) {
            spans.push(Span {
                text: word.to_string(),
                role: Some("keyword".to_string()),
                fg: Some((255, 200, 0)),
            });
        } else {
            spans.push(Span {
                text: word.to_string(),
                role: None,
                fg: None,
            });
        }
    }
    spans
}

impl CommandGuest for Demo {
    fn run(_id: String, _arg: String) -> Result<String, String> {
        Err("previewer-demo no aporta comandos".to_string())
    }
}

export!(Demo);
