//! Estado del viewer (fase 7, spec §6): texto decodificado con detección
//! (vía `norte-encoding`), «recargar como…» y hexview para binarios. Puro
//! y testeable: la lectura la hace `main` vía el core (regla 7).

use norte_encoding::{Decoded, Detection, Eol};
use norte_i18n::t;
use norte_proto::VPath;

/// Cuántas líneas salta una página (fijo, como en los panes).
pub const PAGE: usize = 10;

/// Bytes por fila del hexview.
const HEX_COLS: usize = 16;

/// Preview producido por un plugin (M4-P5): reemplaza la vista cruda mientras
/// está presente. `lines` ya enmascaradas ([`crate::app::display_name`]).
pub struct PluginPreviewView {
    /// Nombre legible del plugin previewer (ya enmascarado), para el indicador.
    pub plugin_name: String,
    /// Líneas de la salida del plugin, ya enmascaradas (texto de un TERCERO).
    pub lines: Vec<String>,
}

/// El viewer abierto sobre un archivo.
pub struct Viewer {
    /// El archivo mostrado.
    pub path: VPath,
    /// Lo leído (posiblemente truncado al presupuesto del viewer).
    bytes: Vec<u8>,
    /// `true` si el archivo seguía (solo se leyó la cabecera).
    pub truncated: bool,
    /// Hexview activo (automático en binarios; toggle manual).
    pub hex: bool,
    /// Encoding forzado por «recargar como…» (None = detección).
    forced: Option<&'static norte_encoding::Encoding>,
    /// Primera línea visible.
    pub scroll: usize,
    /// Preview de un plugin (M4-P5): si está, REEMPLAZA la vista cruda y la
    /// decodificación (bytes/encoding se ignoran; `lines` ya enmascaradas).
    plugin_preview: Option<PluginPreviewView>,
    // ---- cache de decodificación (se recomputa al cambiar encoding) ----
    text: String,
    encoding_name: &'static str,
    eol: Eol,
    had_errors: bool,
    lines: usize,
}

impl Viewer {
    /// Struct base con todos los campos en su cero (sin decodificar aún).
    fn base(path: VPath, bytes: Vec<u8>, truncated: bool) -> Self {
        Self {
            path,
            bytes,
            truncated,
            hex: false,
            forced: None,
            scroll: 0,
            plugin_preview: None,
            text: String::new(),
            encoding_name: "",
            eol: Eol::None,
            had_errors: false,
            lines: 0,
        }
    }

    /// Viewer sobre `bytes` (ya leídos): detecta encoding y binario.
    #[must_use]
    pub fn new(path: VPath, bytes: Vec<u8>, truncated: bool) -> Self {
        let mut v = Self::base(path, bytes, truncated);
        v.recompute();
        v
    }

    /// Viewer en modo preview de plugin (M4-P5): pinta la salida del plugin en
    /// vez de la vista cruda. El `output` es texto de un TERCERO → cada línea
    /// (partida por `\n`) y el `plugin_name` se enmascaran con
    /// [`crate::app::display_name`] (controles/bidi/invisibles → `�`).
    #[must_use]
    pub fn with_plugin_preview(path: VPath, plugin_name: String, output: &str) -> Self {
        let lines = output
            .split('\n')
            .map(|l| crate::app::display_name(l.as_bytes()).0)
            .collect();
        let plugin_name = crate::app::display_name(&plugin_name.into_bytes()).0;
        let mut v = Self::base(path, Vec::new(), false);
        v.plugin_preview = Some(PluginPreviewView { plugin_name, lines });
        v
    }

    /// El nombre del plugin si el viewer está en modo preview (para el
    /// indicador «via …» de la cabecera), o `None` si es la vista cruda.
    #[must_use]
    pub fn preview_plugin(&self) -> Option<&str> {
        self.plugin_preview.as_ref().map(|p| p.plugin_name.as_str())
    }

    fn recompute(&mut self) {
        let encoding = self.forced.or(match norte_encoding::detect(&self.bytes) {
            Detection::Text { encoding, .. } => Some(encoding),
            Detection::Binary => None,
        });
        if let Some(enc) = encoding {
            // Forzado = sin BOM-sniffing (el usuario MANDA, spec §6.2);
            // truncado = la cola partida queda pendiente, no es pérdida.
            let Decoded {
                text,
                encoding,
                had_errors,
            } = if self.forced.is_some() {
                norte_encoding::decode_forced(&self.bytes, enc, !self.truncated)
            } else {
                norte_encoding::decode(&self.bytes, enc, !self.truncated)
            };
            self.eol = norte_encoding::detect_eol(&text);
            // Para PINTAR: todo EOL (incl. CR de Mac clásico) parte línea.
            let text = text.replace("\r\n", "\n").replace('\r', "\n");
            self.lines = text.lines().count();
            self.text = text;
            self.encoding_name = encoding.name();
            self.had_errors = had_errors;
            self.hex = false;
        } else {
            // Binario: jamás decodificar a ciegas (spec §6) — hexview.
            self.hex = true;
            self.encoding_name = "";
            self.eol = Eol::None;
            self.had_errors = false;
            self.text = String::new();
            self.lines = 0;
        }
        self.scroll = 0;
    }

    /// «Recargar como…»: siguiente encoding del ciclo (spec §6). En un
    /// binario fuerza la PRIMERA decodificación de texto del ciclo.
    pub fn cycle_encoding(&mut self) {
        let cycle = norte_encoding::reload_cycle();
        let next = match self.forced {
            None => 0,
            Some(cur) => cycle
                .iter()
                .position(|e| std::ptr::eq(*e, cur))
                .map_or(0, |i| (i + 1) % cycle.len()),
        };
        self.forced = Some(cycle[next]);
        self.recompute();
    }

    /// Vuelve a la detección automática (y al hexview si era binario).
    pub fn reset_encoding(&mut self) {
        self.forced = None;
        self.recompute();
    }

    /// Alterna el hexview manualmente (el texto decodificado se conserva).
    /// El scroll se reclampa: los totales de fila difieren entre modos.
    pub fn toggle_hex(&mut self) {
        self.hex = !self.hex;
        self.scroll = self.scroll.min(self.total_rows().saturating_sub(1));
    }

    /// Total de filas visibles en el modo actual.
    #[must_use]
    pub fn total_rows(&self) -> usize {
        if let Some(p) = &self.plugin_preview {
            p.lines.len()
        } else if self.hex {
            self.bytes.len().div_ceil(HEX_COLS)
        } else {
            self.lines
        }
    }

    /// Baja `n` filas (con tope).
    pub fn scroll_down(&mut self, n: usize) {
        self.scroll = (self.scroll + n).min(self.total_rows().saturating_sub(1));
    }

    /// Sube `n` filas.
    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    /// Al principio.
    pub fn scroll_top(&mut self) {
        self.scroll = 0;
    }

    /// Al final.
    pub fn scroll_bottom(&mut self) {
        self.scroll = self.total_rows().saturating_sub(1);
    }

    /// Las filas visibles desde `scroll`, ya formateadas para el terminal:
    /// tabs EXPANDIDOS (ratatui los borraría: columnas colapsadas en
    /// silencio) y el resto de controles enmascarados a `�` (un `.ans` con
    /// ESC se ve alterado, jamás sin marca) — misma política que los
    /// nombres (spec §6).
    #[must_use]
    pub fn rows(&self, height: usize) -> Vec<String> {
        if let Some(p) = &self.plugin_preview {
            // Ya enmascaradas al construir; mismo cálculo de ventana.
            p.lines
                .iter()
                .skip(self.scroll)
                .take(height)
                .cloned()
                .collect()
        } else if self.hex {
            hex_rows(&self.bytes, self.scroll, height)
        } else {
            self.text
                .lines()
                .skip(self.scroll)
                .take(height)
                .map(render_line)
                .collect()
        }
    }

    /// Línea de estado: encoding, EOL, pérdidas y truncado — el usuario
    /// SIEMPRE sabe qué está viendo (spec §6).
    #[must_use]
    pub fn status(&self) -> String {
        use std::fmt::Write;
        let mut out = if self.encoding_name.is_empty() {
            t("viewer-binary")
        } else {
            self.encoding_name.to_owned()
        };
        if self.forced.is_some() {
            out.push(' ');
            out.push_str(&t("viewer-forced"));
        }
        if !self.hex {
            // Etiqueta de UI (la lib da identificadores técnicos estables;
            // strings hardcodeados hasta Fluent — fase 9, issue #1).
            let eol = match self.eol {
                Eol::Lf => "LF".to_owned(),
                Eol::CrLf => "CRLF".to_owned(),
                Eol::Cr => "CR".to_owned(),
                Eol::Mixed => t("eol-mixed"),
                Eol::None => t("eol-none"),
            };
            let _ = write!(out, "  {eol}");
        }
        if self.had_errors {
            let _ = write!(out, "  {}", t("viewer-lossy"));
        }
        if self.truncated {
            let _ = write!(out, "  {}", t("viewer-truncated"));
        }
        out
    }
}

/// Ancho de tab del viewer (fijo en M1).
const TAB_WIDTH: usize = 8;

/// Prepara una línea para el terminal: tabs a espacios (tab stops de
/// [`TAB_WIDTH`]) y controles restantes a `�`.
fn render_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut col = 0usize;
    for c in line.chars() {
        if c == '\t' {
            let next = (col / TAB_WIDTH + 1) * TAB_WIDTH;
            for _ in col..next {
                out.push(' ');
            }
            col = next;
        } else if c.is_control() {
            out.push('\u{FFFD}');
            col += 1;
        } else {
            out.push(c);
            col += 1;
        }
    }
    out
}

/// Filas del hexview: `offset  hex×16  ascii`.
fn hex_rows(bytes: &[u8], scroll: usize, height: usize) -> Vec<String> {
    use std::fmt::Write;
    let mut out = Vec::new();
    for row in scroll..(scroll + height) {
        let start = row * HEX_COLS;
        if start >= bytes.len() {
            break;
        }
        let chunk = &bytes[start..(start + HEX_COLS).min(bytes.len())];
        let mut line = format!("{start:08x}  ");
        for (i, b) in chunk.iter().enumerate() {
            let _ = write!(line, "{b:02x} ");
            if i == 7 {
                line.push(' ');
            }
        }
        let hexw = 8 + 2 + HEX_COLS * 3 + 1;
        while line.len() < hexw + 2 {
            line.push(' ');
        }
        for b in chunk {
            line.push(if (0x20..0x7F).contains(b) {
                *b as char
            } else {
                '.'
            });
        }
        out.push(line);
    }
    out
}
