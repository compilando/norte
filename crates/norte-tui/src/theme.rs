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

use norte_frontend::treemap::{ChildClass, Ground};

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

    /// A role's foreground as the theme states it, before degrading it to
    /// the terminal's depth: what pixels are painted in (`rail_icons`).
    #[must_use]
    pub fn role_rgb(&self, role: Role) -> Option<[u8; 3]> {
        self.theme.style(role).fg.map(|c| [c.r, c.g, c.b])
    }

    /// A role's background as the terminal SHOWS it: degraded to this
    /// depth, and an index taken as xterm's default palette. What sixel
    /// paints the rail's empty pixels in, so they match the cells around.
    #[must_use]
    pub fn role_bg_shown(&self, role: Role) -> Option<[u8; 3]> {
        Some(match self.color(self.theme.style(role).bg?) {
            RColor::Rgb(r, g, b) => [r, g, b],
            RColor::Indexed(i) => xterm_rgb(i),
            _ => return None,
        })
    }

    /// [`Self::role_rgb`] for the background.
    #[must_use]
    pub fn role_bg_rgb(&self, role: Role) -> Option<[u8; 3]> {
        self.theme.style(role).bg.map(|c| [c.r, c.g, c.b])
    }

    /// The ratatui style of a file ENTRY (colored by extension/kind,
    /// ADR 0020 D2).
    #[must_use]
    pub fn entry(&self, name: &[u8], kind: EntryKind) -> RStyle {
        self.convert(self.theme.file_style(name, map_kind(kind)))
    }

    /// The fill of a disk-map rectangle of `class` in tone `shade`, and the
    /// colour its label reads in: `(background, foreground)`.
    ///
    /// The class colour is the first of
    /// [`ChildClass::colour_candidates`] the theme defines, toned
    /// (ADR 0175) and degraded to this terminal's depth. It is mixed BEFORE
    /// degrading, so a 256-colour terminal gets three tones too, not one.
    /// `None` when the theme defines none (a monochrome one): the caller
    /// falls back to the role's own style, reversed.
    #[must_use]
    pub fn class_fill(&self, class: ChildClass, shade: u8) -> Option<(RColor, RColor)> {
        let colour = class
            .colour_candidates()
            .iter()
            .find_map(|(role, ground)| {
                let style = self.theme.style(*role);
                match ground {
                    Ground::Fg => style.fg,
                    Ground::Bg => style.bg,
                }
            })?;
        // The tone asked for, as the terminal will show it. In 16 colours
        // (and in 256 for a dark panel) a dark tone quantises to the very
        // index of the panel and the rectangle vanishes: then the next tone
        // that does not, the class colour untoned first.
        let panels: Vec<RColor> = [Role::Background, Role::PaneBackground]
            .into_iter()
            .filter_map(|r| self.role(r).bg)
            .collect();
        let shown = [shade, 0, 2, 1]
            .into_iter()
            .map(|s| self.color(tone(colour, s)))
            .find(|c| !panels.contains(c))
            .unwrap_or_else(|| self.color(tone(colour, shade)));
        // Dark text on a light fill and the other way round: the class
        // colour is what the theme chose for ITS text or fill, so what
        // reads on it is decided from the colour as SHOWN (an index is
        // xterm's palette), not from the theme nor the unquantised tone.
        let [r, g, b] = match shown {
            RColor::Rgb(r, g, b) => [r, g, b],
            RColor::Indexed(i) => xterm_rgb(i),
            _ => [0, 0, 0],
        };
        let luma = (299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b)) / 1000;
        let label = if luma > 140 {
            Color {
                r: 16,
                g: 16,
                b: 16,
            }
        } else {
            Color {
                r: 245,
                g: 245,
                b: 245,
            }
        };
        Some((shown, self.color(label)))
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

/// A class colour in one of the disk map's tones (ADR 0175): `0` as the
/// theme gives it, `1` darker by 30 %, `2` lighter by 25 %.
fn tone(colour: Color, shade: u8) -> Color {
    let mix = |v: u8, to: u8, pct: u16| {
        let (v, to) = (u16::from(v), u16::from(to));
        u8::try_from((v * (100 - pct) + to * pct) / 100).unwrap_or(u8::MAX)
    };
    let by = |to: u8, pct: u16| Color {
        r: mix(colour.r, to, pct),
        g: mix(colour.g, to, pct),
        b: mix(colour.b, to, pct),
    };
    match shade {
        1 => by(0, 30),
        2 => by(255, 25),
        _ => colour,
    }
}

/// Colour `i` of xterm's default 256-colour palette.
fn xterm_rgb(i: u8) -> [u8; 3] {
    const BASE: [[u8; 3]; 16] = [
        [0, 0, 0],
        [205, 0, 0],
        [0, 205, 0],
        [205, 205, 0],
        [0, 0, 238],
        [205, 0, 205],
        [0, 205, 205],
        [229, 229, 229],
        [127, 127, 127],
        [255, 0, 0],
        [0, 255, 0],
        [255, 255, 0],
        [92, 92, 255],
        [255, 0, 255],
        [0, 255, 255],
        [255, 255, 255],
    ];
    const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match i {
        0..=15 => BASE[usize::from(i)],
        16..=231 => {
            let n = usize::from(i - 16);
            [CUBE[n / 36], CUBE[n / 6 % 6], CUBE[n % 6]]
        }
        _ => [8 + 10 * (i - 232); 3],
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
/// portable API, ADR 0020 D4). See [`depth_from`].
#[must_use]
pub fn detect_depth() -> ColorDepth {
    depth_from(
        std::env::var("COLORTERM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
        cfg!(windows),
    )
}

/// `COLORTERM=truecolor/24bit` → truecolor; `TERM` containing `256` → 256;
/// otherwise 16 colors — EXCEPT on Windows, whose console has drawn 24-bit
/// color since Windows 10 (1703) and whose Windows Terminal sets neither
/// variable. There, the 16-color fallback turned every theme into the
/// console's Campbell palette (seen at the VM's desktop: catppuccin-latte in
/// Campbell's blue, yellow and purple).
#[must_use]
pub fn depth_from(colorterm: Option<&str>, term: Option<&str>, windows: bool) -> ColorDepth {
    if colorterm.is_some_and(|ct| ct.contains("truecolor") || ct.contains("24bit")) {
        return ColorDepth::Truecolor;
    }
    if term.is_some_and(|t| t.contains("256")) {
        return ColorDepth::Ansi256;
    }
    if windows && term.is_none() {
        return ColorDepth::Truecolor;
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

#[cfg(test)]
mod rgb_tests {
    use super::TuiTheme;
    use norte_theme::{ColorDepth, Role, Theme};

    /// The pixels are drawn in the THEME's colour, not the one degraded
    /// for the terminal's depth: a raster has no palette to fit.
    #[test]
    fn role_rgb_is_the_themes_own_colour_at_any_depth() {
        let theme = Theme::preset("vscode-dark").unwrap().unwrap();
        let t = TuiTheme::new(theme, ColorDepth::Ansi16);
        assert_eq!(t.role_rgb(Role::Title), Some([0xcc, 0xcc, 0xcc]));
        assert_eq!(t.role_rgb(Role::Muted), Some([0x9d, 0x9d, 0x9d]));
        assert_eq!(t.role_rgb(Role::Background), None, "a bg-only role");
    }

    /// Sixel paints the rail's empty pixels in its background, and they
    /// must match the cells around them — the colour the terminal SHOWS,
    /// after degrading to its depth, not the theme's.
    #[test]
    fn role_bg_shown_is_the_colour_after_degrading() {
        let theme = || Theme::preset("vscode-dark").unwrap().unwrap();
        let truecolor = TuiTheme::new(theme(), ColorDepth::Truecolor);
        assert_eq!(
            truecolor.role_bg_shown(Role::Background),
            Some([0x1f, 0x1f, 0x1f])
        );
        // #1f1f1f in 256 colours is the grey ramp's 234 (8 + 10·2 = 28).
        let ansi256 = TuiTheme::new(theme(), ColorDepth::Ansi256);
        let [r, g, b] = ansi256.role_bg_shown(Role::Background).expect("bg");
        assert!(r == g && g == b && r.abs_diff(0x1f) <= 10, "{r}");
        // In 16 colours it is ANSI black.
        let ansi16 = TuiTheme::new(theme(), ColorDepth::Ansi16);
        assert_eq!(ansi16.role_bg_shown(Role::Background), Some([0, 0, 0]));
    }
}

#[cfg(test)]
mod class_fill_tests {
    use super::{Color, TuiTheme, xterm_rgb};
    use norte_frontend::treemap::ChildClass;
    use norte_theme::{ColorDepth, Role};
    use ratatui::style::Color as RColor;

    const CLASSES: [ChildClass; 7] = [
        ChildClass::Directory,
        ChildClass::Code,
        ChildClass::Archive,
        ChildClass::Image,
        ChildClass::Media,
        ChildClass::Document,
        ChildClass::Other,
    ];

    const DEPTHS: [ColorDepth; 3] = [
        ColorDepth::Truecolor,
        ColorDepth::Ansi256,
        ColorDepth::Ansi16,
    ];

    fn shown(c: RColor) -> [u8; 3] {
        match c {
            RColor::Rgb(r, g, b) => [r, g, b],
            RColor::Indexed(i) => xterm_rgb(i),
            other => panic!("{other:?}"),
        }
    }

    /// The fill is never the panel's own colour, at any depth: tone 1
    /// (30 % toward black) quantised to 16 colours landed on the same index
    /// as a dark panel and the rectangle vanished (#423).
    #[test]
    fn a_fill_is_never_the_panel_at_any_depth() {
        for name in norte_theme::preset_names() {
            for depth in DEPTHS {
                let t = TuiTheme::new(norte_theme::Theme::preset(name).unwrap().unwrap(), depth);
                let panel: Vec<RColor> = [Role::Background, Role::PaneBackground]
                    .into_iter()
                    .filter_map(|r| t.role(r).bg)
                    .collect();
                for class in CLASSES {
                    for shade in 0..3 {
                        let Some((bg, _)) = t.class_fill(class, shade) else {
                            continue;
                        };
                        assert!(
                            !panel.contains(&bg),
                            "{name} {depth:?} {class:?} shade {shade}: {bg:?} is the panel"
                        );
                    }
                }
            }
        }
    }

    /// The label reads on the colour the terminal SHOWS, not on the
    /// unquantised one: luma is taken from the degraded fill.
    #[test]
    fn the_label_reads_on_the_degraded_fill() {
        for name in norte_theme::preset_names() {
            for depth in DEPTHS {
                let t = TuiTheme::new(norte_theme::Theme::preset(name).unwrap().unwrap(), depth);
                for class in CLASSES {
                    for shade in 0..3 {
                        let Some((bg, fg)) = t.class_fill(class, shade) else {
                            continue;
                        };
                        let [r, g, b] = shown(bg);
                        let luma =
                            (299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b)) / 1000;
                        let want = if luma > 140 {
                            t.color(Color {
                                r: 16,
                                g: 16,
                                b: 16,
                            })
                        } else {
                            t.color(Color {
                                r: 245,
                                g: 245,
                                b: 245,
                            })
                        };
                        assert_eq!(fg, want, "{name} {depth:?} {class:?} {shade}: {bg:?}");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod depth_tests {
    use super::depth_from;
    use norte_theme::ColorDepth;

    /// Windows Terminal sets neither variable and draws 24-bit color: the
    /// 16-color fallback is for a unix terminal that says nothing.
    #[test]
    fn windows_without_variables_is_truecolor() {
        assert_eq!(depth_from(None, None, true), ColorDepth::Truecolor);
        assert_eq!(depth_from(None, None, false), ColorDepth::Ansi16);
    }

    /// What the variables say still wins, on both.
    #[test]
    fn the_variables_still_decide() {
        assert_eq!(
            depth_from(Some("truecolor"), None, false),
            ColorDepth::Truecolor
        );
        assert_eq!(
            depth_from(None, Some("xterm-256color"), true),
            ColorDepth::Ansi256
        );
        assert_eq!(depth_from(None, Some("xterm"), true), ColorDepth::Ansi16);
    }
}
