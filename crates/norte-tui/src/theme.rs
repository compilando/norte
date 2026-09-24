//! Bridge between the theme model (`norte-theme`, ADR 0020) and ratatui:
//! resolves the effective theme, detects the terminal's color depth, and
//! translates [`norte_theme::Style`] to [`ratatui::style::Style`] at that
//! depth.
//!
//! Lives in the frontend (rule 7: presentation, not business logic). M5's GUI
//! will have its own bridge against the SAME `norte-theme`.

use norte_proto::EntryKind;
use norte_theme::{Color, ColorDepth, FileKind, ResolvedColor, Role, Theme};
use ratatui::style::{Color as RColor, Modifier, Style as RStyle};

pub use norte_frontend::theme::{ResolveError, resolve_theme};

/// The resolved theme + the color depth it is painted at.
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
    /// Explicit theme + depth.
    #[must_use]
    pub fn new(theme: Theme, depth: ColorDepth) -> Self {
        Self { theme, depth }
    }

    /// The color depth things are painted at: for building ANOTHER theme with
    /// the same one (the wizard's preview).
    #[must_use]
    pub fn depth(&self) -> ColorDepth {
        self.depth
    }

    /// `true` if the theme declares GPU effects (the TUI ignores them; it
    /// only exposes this for diagnostics).
    #[must_use]
    pub fn has_effects(&self) -> bool {
        self.theme.has_effects()
    }

    /// The resolved theme's name (to match the selector's cursor against the
    /// current theme).
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.theme.name.as_deref()
    }

    /// The ratatui style of a semantic role.
    #[must_use]
    pub fn role(&self, role: Role) -> RStyle {
        self.convert(self.theme.style(role))
    }

    /// The ratatui style of a file ENTRY (colored by extension/kind,
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

/// Protocol `EntryKind` → theme `FileKind`.
///
/// Delegates to `norte_frontend::theme::file_kind_of`, which belongs to BOTH
/// frontends: the window needs the same mapping since bridge 66, and the same
/// decision written twice drifts apart in silence (ADR 0077). The why — that
/// `Entry` carries no mode — is there.
fn map_kind(kind: EntryKind) -> FileKind {
    norte_frontend::theme::file_kind_of(kind)
}

/// Detects the terminal's color depth (heuristic — there is no reliable
/// portable API, ADR 0020 D4): `COLORTERM=truecolor/24bit` → truecolor; `TERM`
/// containing `256` → 256; otherwise, 16 colors.
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

/// Resolves the `[ui].theme` spec to a [`TuiTheme`] (shared theme plus the
/// terminal's depth). The name/path resolution lives in
/// `norte_frontend::theme` (shared with the GUI).
///
/// # Errors
/// Those of [`resolve_theme`].
pub fn resolve(spec: Option<&str>, depth: ColorDepth) -> Result<TuiTheme, ResolveError> {
    Ok(TuiTheme::new(resolve_theme(spec)?, depth))
}
