//! Puente entre el modelo de tema (`norte-theme`, ADR 0020) y ratatui: resuelve
//! el tema efectivo, detecta la profundidad de color del terminal y traduce
//! [`norte_theme::Style`] a [`ratatui::style::Style`] a esa profundidad.
//!
//! Vive en el frontend (regla 7: presentación, no lógica de negocio). La GUI de
//! M5 tendrá su propio puente contra el MISMO `norte-theme`.

use std::path::Path;

use norte_proto::EntryKind;
use norte_theme::{Color, ColorDepth, FileKind, ResolvedColor, Role, Theme};
use ratatui::style::{Color as RColor, Modifier, Style as RStyle};

/// El tema resuelto + la profundidad de color a la que se pinta.
#[derive(Debug, Clone)]
pub struct TuiTheme {
    theme: Theme,
    depth: ColorDepth,
}

impl Default for TuiTheme {
    fn default() -> Self {
        Self::new(Theme::preset_default(), detect_depth())
    }
}

impl TuiTheme {
    /// Tema + profundidad explícitos.
    #[must_use]
    pub fn new(theme: Theme, depth: ColorDepth) -> Self {
        Self { theme, depth }
    }

    /// `true` si el tema declara efectos de GPU (la TUI los ignora; solo lo
    /// expone para diagnósticos).
    #[must_use]
    pub fn has_effects(&self) -> bool {
        self.theme.has_effects()
    }

    /// El estilo ratatui de un rol semántico.
    #[must_use]
    pub fn role(&self, role: Role) -> RStyle {
        self.convert(self.theme.style(role))
    }

    /// El estilo ratatui de una ENTRADA de fichero (color por extensión/kind,
    /// ADR 0020 D2).
    #[must_use]
    pub fn entry(&self, name: &[u8], kind: EntryKind) -> RStyle {
        self.convert(self.theme.file_style(name, map_kind(kind)))
    }

    fn convert(&self, s: norte_theme::Style) -> RStyle {
        let mut out = RStyle::default();
        if let Some(c) = s.fg {
            out = out.fg(self.color(c));
        }
        if let Some(c) = s.bg {
            out = out.bg(self.color(c));
        }
        let mut m = Modifier::empty();
        m.set(Modifier::BOLD, s.bold);
        m.set(Modifier::DIM, s.dim);
        m.set(Modifier::ITALIC, s.italic);
        m.set(Modifier::UNDERLINED, s.underline);
        m.set(Modifier::REVERSED, s.reverse);
        out.add_modifier(m)
    }

    fn color(&self, c: Color) -> RColor {
        match c.resolve(self.depth) {
            ResolvedColor::Rgb(r, g, b) => RColor::Rgb(r, g, b),
            ResolvedColor::Indexed(i) => RColor::Indexed(i),
        }
    }
}

/// `EntryKind` del protocolo → `FileKind` del tema. El protocolo no distingue
/// aún ejecutable/fifo/… (el `Entry` no lleva modo): todo lo que no es dir ni
/// symlink cae a `Regular`; el color por EXTENSIÓN sigue aplicando.
fn map_kind(kind: EntryKind) -> FileKind {
    match kind {
        EntryKind::Dir => FileKind::Dir,
        EntryKind::Symlink => FileKind::Symlink,
        EntryKind::File | EntryKind::Other => FileKind::Regular,
    }
}

/// Detecta la profundidad de color del terminal (heurística — no hay API
/// portable fiable, ADR 0020 D4): `COLORTERM=truecolor/24bit` → truecolor;
/// `TERM` con `256` → 256; en otro caso, 16 colores.
#[must_use]
pub fn detect_depth() -> ColorDepth {
    if let Ok(ct) = std::env::var("COLORTERM")
        && (ct.contains("truecolor") || ct.contains("24bit"))
    {
        return ColorDepth::Truecolor;
    }
    if std::env::var("TERM").is_ok_and(|t| t.contains("256")) {
        return ColorDepth::Ansi256;
    }
    ColorDepth::Ansi16
}

/// Resuelve la especificación `[ui].theme`: un NOMBRE de preset embebido o una
/// RUTA a un `.toml` propio. `None` = el preset por defecto.
///
/// # Errors
/// Un mensaje (ya legible) si la ruta no se lee o el TOML no parsea; el caller
/// decide degradar al default y avisar.
pub fn resolve(spec: Option<&str>, depth: ColorDepth) -> Result<TuiTheme, String> {
    let Some(spec) = spec else {
        return Ok(TuiTheme::new(Theme::preset_default(), depth));
    };
    // Preset embebido por nombre.
    if let Some(theme) = Theme::preset(spec).map_err(|e| e.to_string())? {
        return Ok(TuiTheme::new(theme, depth));
    }
    // Si no es preset, es una ruta a un fichero de tema.
    let path = Path::new(spec);
    let raw = std::fs::read_to_string(path).map_err(|e| format!("tema {spec}: {e}"))?;
    let theme = Theme::from_toml(&raw).map_err(|e| format!("tema {spec}: {e}"))?;
    Ok(TuiTheme::new(theme, depth))
}
