//! A Visual Studio Code theme, read and projected onto a [`Theme`]
//! (spec 2026-09-11, F5).
//!
//! Two facts shape this module:
//!
//! 1. **A VS Code theme is NOT a complete palette.** `dark_modern.json`
//!    includes `dark_plus.json`, which includes `dark_vs.json`, and none of the
//!    three defines `list.*` or `scrollbarSlider.*`: those live in the editor's
//!    color registry. That is why [`to_theme`] paints ON TOP of a base
//!    (`vscode-dark` or `vscode-light`) and whatever the theme leaves unsaid the base supplies,
//!    instead of falling back to monochrome.
//! 2. **This module does not touch the disk.** `include` is returned raw and whoever
//!    has the file —the binary— walks the chain and calls
//!    [`VsCodeTheme::merge_under`]. `norte-theme` is a pure model.
//!
//! `tokenColors` and `semanticTokenColors` are IGNORED: norte does not color
//! syntax. Only the `colors` block counts.
//!
//! ```
//! use norte_theme::{Role, Theme, vscode};
//!
//! let src = r##"{
//!     // A marketplace theme carries comments and trailing commas.
//!     "type": "dark",
//!     "colors": { "editor.background": "#101010", },
//! }"##;
//! let theme = vscode::parse(src).unwrap();
//! let base = Theme::preset(theme.base_or_default().preset()).unwrap().unwrap();
//! let t = vscode::to_theme(&theme.colors, &base);
//! assert_eq!(t.style(Role::Background).bg.unwrap().to_hex(), "#101010");
//! ```

use std::collections::HashMap;

use serde::Deserialize;

use crate::color::Color;
use crate::role::Role;
use crate::style::Style;
use crate::theme::Theme;

/// Whether the theme is light or dark: decides which preset it is painted over.
///
/// ```
/// use norte_theme::vscode::VsBase;
/// assert_eq!(VsBase::Light.preset(), "vscode-light");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VsBase {
    /// `"dark"`, `"vs-dark"`, `"hc"`, `"hc-black"`.
    Dark,
    /// `"light"`, `"vs"`, `"hc-light"`.
    Light,
}

impl VsBase {
    /// The embedded preset that serves as the base for this type.
    ///
    /// ```
    /// use norte_theme::{Theme, vscode::VsBase};
    /// assert!(Theme::preset(VsBase::Dark.preset()).unwrap().is_some());
    /// ```
    #[must_use]
    pub const fn preset(self) -> &'static str {
        match self {
            Self::Dark => "vscode-dark",
            Self::Light => "vscode-light",
        }
    }

    /// Reads the `"type"` field. An unrecognized value is `None`, just like
    /// its absence: neither says anything.
    fn from_type(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "dark" | "vs-dark" | "hc" | "hc-black" | "hcdark" | "hc-dark" => Some(Self::Dark),
            "light" | "vs" | "hc-light" | "hclight" => Some(Self::Light),
            _ => None,
        }
    }
}

/// A VS Code color: RGB plus an alpha channel norte does not have.
///
/// VS Code accepts `#rgb`, `#rgba`, `#rrggbb` and `#rrggbbaa`. norte's theme
/// model is 24-bit RGB (ADR 0020), so the alpha is kept here and
/// [`to_theme`] COMPOSITES it over the theme's background — the same thing that was done by
/// hand with `scrollbar-slider` in the two presets. Simply dropping it
/// would turn a 40 % slider into an opaque, garish one.
///
/// ```
/// use norte_theme::vscode::VsColor;
/// let c = VsColor::parse("#ffffff80").unwrap();
/// assert_eq!(c.alpha, 0x80);
/// assert_eq!(VsColor::parse("#abc").unwrap().rgb.to_hex(), "#aabbcc");
/// assert!(VsColor::parse("red").is_none());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VsColor {
    /// The three color channels.
    pub rgb: Color,
    /// Opacity, 255 = opaque.
    pub alpha: u8,
}

impl VsColor {
    /// Parses the four forms VS Code accepts. `None` if it is none of them.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let hex = s.strip_prefix('#')?;
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        // The short forms double each digit: `#abc8` = `#aabbcc88`.
        let long: String = match hex.len() {
            3 | 4 => hex.chars().flat_map(|c| [c, c]).collect(),
            6 | 8 => hex.to_owned(),
            _ => return None,
        };
        let rgb = Color::parse(&format!("#{}", &long[..6])).ok()?;
        let alpha = match long.get(6..8) {
            Some(a) => u8::from_str_radix(a, 16).ok()?,
            None => 255,
        };
        Some(Self { rgb, alpha })
    }

    /// The color resulting from painting this one over an opaque `background`.
    ///
    /// ```
    /// use norte_theme::{Color, vscode::VsColor};
    /// // #797979 at 40 % over #1f1f1f: vscode-dark's `scrollbar-slider`.
    /// let c = VsColor { rgb: Color::rgb(0x79, 0x79, 0x79), alpha: 102 };
    /// assert_eq!(c.over(Color::rgb(0x1f, 0x1f, 0x1f)).to_hex(), "#434343");
    /// ```
    #[must_use]
    pub fn over(self, background: Color) -> Color {
        let a = u16::from(self.alpha);
        let blend = |c: u8, f: u8| -> u8 {
            let v = (u16::from(c) * a + u16::from(f) * (255 - a) + 127) / 255;
            // c,f ≤ 255 and a ≤ 255 ⇒ v ≤ 255: the try_from never fails.
            u8::try_from(v).unwrap_or(u8::MAX)
        };
        Color::rgb(
            blend(self.rgb.r, background.r),
            blend(self.rgb.g, background.g),
            blend(self.rgb.b, background.b),
        )
    }
}

/// A VS Code theme already read, with `include` still unresolved.
#[derive(Debug, Clone, Default)]
pub struct VsCodeTheme {
    /// The JSON's `"name"`, if it has one.
    pub name: Option<String>,
    /// The `"type"`. `None` if missing or unrecognized; see
    /// [`Self::base_or_default`].
    pub base: Option<VsBase>,
    /// The `"include"` as is, relative to the file that names it.
    pub include: Option<String>,
    /// The colors that parse, by VS Code id.
    pub colors: HashMap<String, VsColor>,
    /// The ids whose value is not a color, sorted. VS Code ignores them and so
    /// does this, but they are reported: a theme that silently loses colors
    /// looks too much like a broken importer.
    pub ignored: Vec<String>,
}

impl VsCodeTheme {
    /// The effective type: the declared one, or dark, which is what VS Code assumes.
    ///
    /// ```
    /// use norte_theme::vscode::{VsBase, VsCodeTheme};
    /// assert_eq!(VsCodeTheme::default().base_or_default(), VsBase::Dark);
    /// ```
    #[must_use]
    pub fn base_or_default(&self) -> VsBase {
        self.base.unwrap_or(VsBase::Dark)
    }

    /// Puts `parent` UNDER this theme: the child wins every color it already
    /// has, the parent fills the rest, and the `include` becomes the
    /// parent's to follow the chain. The name is always the child's.
    ///
    /// ```
    /// use norte_theme::vscode::parse;
    /// let mut child = parse(r##"{"include":"p.json","colors":{"foreground":"#111111"}}"##).unwrap();
    /// let parent = parse(r##"{"type":"light","colors":{"foreground":"#999999","focusBorder":"#0000ff"}}"##).unwrap();
    /// child.merge_under(parent);
    /// assert_eq!(child.colors["foreground"].rgb.to_hex(), "#111111");
    /// assert!(child.colors.contains_key("focusBorder"));
    /// assert!(child.include.is_none());
    /// ```
    pub fn merge_under(&mut self, parent: VsCodeTheme) {
        for (id, color) in parent.colors {
            self.colors.entry(id).or_insert(color);
        }
        self.base = self.base.or(parent.base);
        self.include = parent.include;
        // An invalid one in the parent that the child does define has not been lost.
        let colors = &self.colors;
        self.ignored.extend(
            parent
                .ignored
                .into_iter()
                .filter(|id| !colors.contains_key(id)),
        );
        self.ignored.sort();
        self.ignored.dedup();
    }
}

/// Error reading a VS Code theme.
#[derive(Debug, thiserror::Error)]
pub enum VsCodeError {
    /// It is not valid JSON (nor JSONC), or its shape is not that of a theme.
    #[error("invalid VSCode theme: {0}")]
    Json(#[from] serde_json::Error),
}

/// The JSON's shape, lenient: a color that is not a string does not bring down the theme.
#[derive(Deserialize)]
struct Raw {
    #[serde(default)]
    name: Option<serde_json::Value>,
    #[serde(default, rename = "type")]
    kind: Option<serde_json::Value>,
    #[serde(default)]
    include: Option<serde_json::Value>,
    #[serde(default)]
    colors: Option<HashMap<String, serde_json::Value>>,
}

/// A JSON string, or nothing: a `"name": 3` is treated as absent.
fn string_of(v: Option<serde_json::Value>) -> Option<String> {
    match v {
        Some(serde_json::Value::String(s)) => Some(s),
        _ => None,
    }
}

/// Parses a VS Code theme (JSON with comments and trailing commas).
///
/// A `null` in `colors` counts as absent; any other value that is not
/// a color goes to [`VsCodeTheme::ignored`].
///
/// # Errors
/// [`VsCodeError::Json`] if the text is not JSONC or does not have the shape of a theme.
///
/// ```
/// let t = norte_theme::vscode::parse(r##"{"name":"X","colors":{"foreground":7}}"##).unwrap();
/// assert_eq!(t.ignored, ["foreground"]);
/// ```
pub fn parse(src: &str) -> Result<VsCodeTheme, VsCodeError> {
    let raw: Raw = serde_json::from_str(&strip_jsonc(src))?;
    let mut colors = HashMap::new();
    let mut ignored = Vec::new();
    for (id, value) in raw.colors.unwrap_or_default() {
        match &value {
            serde_json::Value::Null => {}
            serde_json::Value::String(s) => match VsColor::parse(s) {
                Some(c) => {
                    colors.insert(id, c);
                }
                None => ignored.push(id),
            },
            _ => ignored.push(id),
        }
    }
    ignored.sort();
    Ok(VsCodeTheme {
        name: string_of(raw.name),
        base: string_of(raw.kind).as_deref().and_then(VsBase::from_type),
        include: string_of(raw.include),
        colors,
        ignored,
    })
}

/// JSONC → JSON: strips `//` and `/* */` and trailing commas.
///
/// By hand and not with a dependency: it is thirty lines (rule 8). A `//`
/// INSIDE a string is not a comment —`"https://…"` appears in real
/// themes—, so the machine tracks whether it is inside one. Comments
/// are replaced with spaces and line breaks are kept, so that a
/// `serde_json` error still points at the right line.
fn strip_jsonc(src: &str) -> String {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => {
                    if let Some(escaped) = chars.next() {
                        out.push(escaped);
                    }
                }
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut prev = '\0';
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                    }
                    if prev == '*' && c == '/' {
                        break;
                    }
                    prev = c;
                }
                out.push(' ');
            }
            ('}' | ']', _) => {
                // Trailing comma: the last non-blank thing before the closer.
                let end = out.trim_end().len();
                if out[..end].ends_with(',') {
                    out.remove(end - 1);
                }
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// VS Code id → norte role. The `bool` is `true` if the id fills the role's `bg`
/// and `false` if it fills the `fg`.
///
/// **Order matters: if two ids feed the same side of a role, the
/// one that comes LATER wins.** That is how a fallback is written: `editor.foreground` before
/// `foreground`, because One Dark Pro —one of the most installed themes— does not
/// define `foreground` and without the fallback its text would come out in the base's gray.
///
/// It is the same table `vscode-dark` and `vscode-light` were transcribed
/// through (each header repeats it), minus `widget.shadow`: a
/// shadow is alpha, and without alpha the window's stylesheet does a better
/// job with its translucent fallback.
///
/// ```
/// use norte_theme::{Role, vscode::MAPPING};
/// assert!(MAPPING.contains(&("list.hoverBackground", Role::Hover, true)));
/// ```
pub const MAPPING: &[(&str, Role, bool)] = &[
    ("editor.background", Role::Background, true),
    ("editor.foreground", Role::Regular, false),
    ("foreground", Role::Regular, false),
    ("sideBar.background", Role::PaneBackground, true),
    ("editor.background", Role::PaneFocusBackground, true),
    ("list.activeSelectionBackground", Role::Selection, true),
    ("list.activeSelectionForeground", Role::Selection, false),
    (
        "list.inactiveSelectionBackground",
        Role::SelectionUnfocused,
        true,
    ),
    (
        "list.inactiveSelectionForeground",
        Role::SelectionUnfocused,
        false,
    ),
    ("list.hoverBackground", Role::Hover, true),
    ("focusBorder", Role::BorderFocus, false),
    ("focusBorder", Role::FocusBorder, false),
    ("panel.border", Role::BorderUnfocused, false),
    ("panel.border", Role::Separator, false),
    ("widget.border", Role::ModalBorder, false),
    ("statusBar.background", Role::StatusBar, true),
    ("statusBar.foreground", Role::StatusBar, false),
    ("sideBarSectionHeader.foreground", Role::Title, false),
    ("sideBarTitle.foreground", Role::Title, false),
    ("button.background", Role::Button, true),
    ("button.foreground", Role::Button, false),
    ("editor.findMatchBackground", Role::Match, true),
    ("editorError.foreground", Role::Error, false),
    ("errorForeground", Role::Error, false),
    ("editorWarning.foreground", Role::Warning, false),
    ("editorInfo.foreground", Role::Info, false),
    ("descriptionForeground", Role::Muted, false),
    ("badge.background", Role::Badge, true),
    ("badge.foreground", Role::Badge, false),
    ("input.background", Role::InputBackground, true),
    ("input.border", Role::InputBorder, false),
    ("editorWidget.background", Role::WidgetBackground, true),
    ("scrollbarSlider.background", Role::ScrollbarSlider, true),
];

/// Paints `colors` over `base` and returns a COMPLETE theme.
///
/// It starts from a clone of `base`; for each [`MAPPING`] row whose id is in
/// `colors`, it replaces THAT side of the role and leaves the other side and the attributes as
/// the base had them. What the theme does not say —roles with no id, `[files]`,
/// `hostile-badge`, `mark`— still comes from the base. The `name` is left empty:
/// the base's would lie, and whoever imports sets it.
///
/// A color with alpha is composited over the background: the theme's
/// `editor.background` (itself composited over the base's), or the base's if the theme
/// does not have one. It is an approximation —a side bar color is really seen
/// over the side bar— and it is the same one used when transcribing
/// the presets.
///
/// ```
/// use std::collections::HashMap;
/// use norte_theme::{Role, Theme, vscode::{to_theme, VsColor}};
///
/// let base = Theme::preset("vscode-dark").unwrap().unwrap();
/// let mut colors = HashMap::new();
/// colors.insert("focusBorder".to_owned(), VsColor::parse("#ff0000").unwrap());
/// let t = to_theme(&colors, &base);
/// assert_eq!(t.style(Role::FocusBorder).fg.unwrap().to_hex(), "#ff0000");
/// assert_eq!(t.style(Role::Hover), base.style(Role::Hover));
/// ```
#[must_use]
pub fn to_theme<S: std::hash::BuildHasher>(
    colors: &HashMap<String, VsColor, S>,
    base: &Theme,
) -> Theme {
    let base_background = base
        .style(Role::Background)
        .bg
        .unwrap_or(Color::rgb(0, 0, 0));
    let background = colors
        .get("editor.background")
        .map_or(base_background, |c| c.over(base_background));

    let mut theme = base.clone();
    theme.name = None;
    for &(id, role, is_background) in MAPPING {
        let Some(color) = colors.get(id) else {
            continue;
        };
        // `roles.get`, not `style()`: the monochrome fallback of a role the
        // base does not define (a `reverse`) must not sneak in under a new color.
        let prev = theme.roles.get(&role).copied().unwrap_or_default();
        let side = if id == "editor.background" {
            // Already composited above: compositing it again over itself would
            // lighten it a second time.
            Style::new().bg(background)
        } else if is_background {
            Style::new().bg(color.over(background))
        } else {
            // A translucent foreground is seen over ITS role's background
            // (`statusBar.foreground` over `statusBar.background`), and the
            // table always puts a role's background before its foreground.
            Style::new().fg(color.over(prev.bg.unwrap_or(background)))
        };
        theme.roles.insert(role, prev.overlay(side));
    }
    theme
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dark() -> Theme {
        Theme::preset("vscode-dark")
            .expect("parses")
            .expect("preset")
    }

    /// JSONC: a marketplace theme carries comments and trailing commas.
    /// `serde_json` does not accept them, so they have to be cleaned first.
    #[test]
    fn parses_jsonc_with_comments_and_trailing_comma() {
        let src = r##"{
            // the name
            "name": "Mío",
            "type": "dark",
            "colors": {
                "editor.background": "#1F1F1F", /* block */
                "foreground": "#CCCCCC",
            },
        }"##;
        let t = parse(src).expect("parses");
        assert_eq!(t.name.as_deref(), Some("Mío"));
        assert_eq!(t.base, Some(VsBase::Dark));
        assert_eq!(t.colors.len(), 2);
    }

    /// A `//` inside a string is NOT a comment: URLs appear in
    /// real themes, and eating them would leave the string unterminated.
    #[test]
    fn a_double_slash_inside_a_string_is_not_a_comment() {
        let src = r##"{"name": "https://x.y/*z*/", "colors": {"a": "#fff"}}"##;
        let t = parse(src).expect("parses");
        assert_eq!(t.name.as_deref(), Some("https://x.y/*z*/"));
        assert_eq!(t.colors.len(), 1);
    }

    /// An escaped quote does not close the string, and a comment in an array
    /// —like those in `dark_plus.json`— does not break the trailing comma either.
    #[test]
    fn escapes_and_comments_in_arrays() {
        let src = "{\"name\": \"a\\\"//b\", \"x\": [1, // c\n 2, ],\n}";
        let t = parse(src).expect("parses");
        assert_eq!(t.name.as_deref(), Some("a\"//b"));
        // A comment BETWEEN the trailing comma and the closer.
        assert!(parse("{\"x\": [1, /* c */ ], \"k\": \",\" }").is_ok());
    }

    /// A field that is not a string, or `colors: null`, does not bring down the theme.
    #[test]
    fn oddly_shaped_fields_are_tolerated() {
        let t = parse(r#"{"name": 3, "type": ["dark"], "include": {}, "colors": null}"#)
            .expect("parses");
        assert!(t.name.is_none() && t.base.is_none() && t.include.is_none());
        assert!(t.colors.is_empty());
    }

    /// A translucent `editor.background` is composited ONCE over the base.
    #[test]
    fn a_translucent_background_is_composited_only_once() {
        let base = dark();
        let src = r##"{"colors":{"editor.background":"#ffffff80"}}"##;
        let t = to_theme(&parse(src).unwrap().colors, &base);
        let expected = VsColor::parse("#ffffff80")
            .unwrap()
            .over(base.style(Role::Background).bg.unwrap());
        assert_eq!(t.style(Role::Background).bg, Some(expected));
        assert_eq!(t.style(Role::PaneFocusBackground).bg, Some(expected));
    }

    /// A translucent foreground is composited over its own role's background.
    #[test]
    fn a_translucent_foreground_is_composited_over_its_role_background() {
        let src = r##"{"colors":{
            "editor.background":"#000000",
            "statusBar.background":"#ffffff",
            "statusBar.foreground":"#00000080"
        }}"##;
        let t = to_theme(&parse(src).unwrap().colors, &dark());
        let expected = VsColor::parse("#00000080")
            .unwrap()
            .over(Color::rgb(255, 255, 255));
        assert_eq!(t.style(Role::StatusBar).fg, Some(expected));
    }

    /// An eight-digit `#RRGGBBAA` is legal in VS Code, and so is `#rgba`.
    #[test]
    fn the_four_color_forms_parse() {
        let src = r##"{"colors":{"a":"#000000","b":"#00000066","c":"#abc","d":"#abc8"}}"##;
        let t = parse(src).expect("parses");
        assert_eq!(t.colors["b"].alpha, 0x66);
        assert_eq!(t.colors["d"].rgb.to_hex(), "#aabbcc");
        assert_eq!(t.colors["d"].alpha, 0x88);
        assert!(t.ignored.is_empty());
    }

    /// Alpha is COMPOSITED over the theme's own background: One Dark Pro's
    /// slider `#4e566660` over its `#282c34`, not an opaque `#4e5666`.
    #[test]
    fn a_color_with_alpha_is_composited_over_the_theme_background() {
        let src = r##"{"colors":{
            "editor.background":"#282c34",
            "scrollbarSlider.background":"#4e566660"
        }}"##;
        let t = to_theme(&parse(src).unwrap().colors, &dark());
        let slider = t.style(Role::ScrollbarSlider).bg.unwrap();
        assert_eq!(
            slider,
            VsColor::parse("#4e566660")
                .unwrap()
                .over(Color::rgb(0x28, 0x2c, 0x34))
        );
        assert_ne!(
            slider.to_hex(),
            "#4e5666",
            "the alpha is not simply dropped"
        );
    }

    /// A value that is not a color does not bring down the theme: it is ignored and reported.
    #[test]
    fn an_invalid_color_is_ignored_and_listed() {
        let src = r##"{"colors":{"a":"transparent","b":"#12","c":null,"d":"#123456"}}"##;
        let t = parse(src).expect("parses");
        assert_eq!(t.ignored, ["a", "b"], "null is absent, not invalid");
        assert_eq!(t.colors.len(), 1);
    }

    /// `include` is RETURNED unresolved: this module does not touch the disk.
    #[test]
    fn include_is_returned_raw() {
        let src = r#"{"include":"./dark_plus.json","type":"dark","colors":{}}"#;
        assert_eq!(
            parse(src).unwrap().include.as_deref(),
            Some("./dark_plus.json")
        );
    }

    /// The high-contrast and light types are recognized; an unknown one is
    /// silence, and silence is dark, as in VS Code.
    #[test]
    fn vscode_types() {
        let kind = |s: &str| parse(&format!(r#"{{"type":"{s}"}}"#)).unwrap().base;
        assert_eq!(kind("hc-black"), Some(VsBase::Dark));
        assert_eq!(kind("hc-light"), Some(VsBase::Light));
        assert_eq!(kind("vs"), Some(VsBase::Light));
        assert_eq!(kind("sepia"), None);
        assert_eq!(parse("{}").unwrap().base_or_default(), VsBase::Dark);
    }

    /// The parent goes UNDER: the child wins, the type is inherited if the child is silent,
    /// and the include advances to the parent's.
    #[test]
    fn merge_under_child_wins_and_the_chain_advances() {
        let mut child = parse(r##"{"name":"h","include":"p","colors":{"a":"#111111"}}"##).unwrap();
        let parent = parse(
            r##"{"name":"p","type":"light","include":"grandparent","colors":{"a":"#999999","b":"#222222"}}"##,
        )
        .unwrap();
        child.merge_under(parent);
        assert_eq!(child.name.as_deref(), Some("h"));
        assert_eq!(child.base, Some(VsBase::Light));
        assert_eq!(child.include.as_deref(), Some("grandparent"));
        assert_eq!(child.colors["a"].rgb.to_hex(), "#111111");
        assert_eq!(child.colors["b"].rgb.to_hex(), "#222222");
    }

    /// A theme that sets TWENTY keys produces a COMPLETE theme: what it does not
    /// say the base supplies (spec 2026-09-11, F5). Without this, importing from the
    /// marketplace would give twenty colors and the rest in monochrome, which reads
    /// as a broken importer.
    #[test]
    fn what_the_theme_does_not_say_the_base_supplies() {
        let base = dark();
        let mut colors = HashMap::new();
        colors.insert(
            "editor.background".to_owned(),
            VsColor::parse("#101010").unwrap(),
        );
        let t = to_theme(&colors, &base);
        assert_eq!(t.style(Role::Background).bg.unwrap().to_hex(), "#101010");
        assert_eq!(
            t.style(Role::Hover).bg,
            base.style(Role::Hover).bg,
            "a role the theme is silent about is inherited from the base"
        );
        assert!(t.style(Role::Regular).fg.is_some());
        assert!(t.name.is_none(), "the base's name would lie");
        for &role in Role::CORE {
            let s = t.style(role);
            assert!(s.fg.is_some() || s.bg.is_some(), "{role:?} without color");
        }
    }

    /// An id that fills one side does not erase the other nor the base's attributes:
    /// `title` is bold in `vscode-dark` and stays so.
    #[test]
    fn one_side_does_not_erase_the_other() {
        let base = dark();
        let mut colors = HashMap::new();
        colors.insert(
            "button.background".to_owned(),
            VsColor::parse("#00ff00").unwrap(),
        );
        colors.insert(
            "sideBarTitle.foreground".to_owned(),
            VsColor::parse("#ff00ff").unwrap(),
        );
        let t = to_theme(&colors, &base);
        assert_eq!(t.style(Role::Button).fg, base.style(Role::Button).fg);
        assert!(t.style(Role::Title).bold);
    }

    /// The order of [`MAPPING`] is the fallback: `foreground` beats
    /// `editor.foreground` if both are present, and the latter stands in if the former is missing.
    #[test]
    fn the_last_id_in_the_table_wins() {
        let editor_only = parse(r##"{"colors":{"editor.foreground":"#abb2bf"}}"##).unwrap();
        let t = to_theme(&editor_only.colors, &dark());
        assert_eq!(t.style(Role::Regular).fg.unwrap().to_hex(), "#abb2bf");

        let both = parse(r##"{"colors":{"editor.foreground":"#abb2bf","foreground":"#cccccc"}}"##)
            .unwrap();
        let t = to_theme(&both.colors, &dark());
        assert_eq!(t.style(Role::Regular).fg.unwrap().to_hex(), "#cccccc");
    }

    /// `widget.shadow` is left out on purpose: see [`MAPPING`]'s rustdoc.
    #[test]
    fn the_shadow_is_not_imported() {
        assert!(!MAPPING.iter().any(|(id, ..)| *id == "widget.shadow"));
    }

    /// `tokenColors` is IGNORED: norte does not color syntax. An importer that
    /// silently swallowed half its input would be a green test that
    /// proves nothing — that is why the module's rustdoc says so.
    #[test]
    fn token_colors_is_ignored() {
        let src = r#"{"type":"dark","colors":{},"tokenColors":[{"scope":"comment"}]}"#;
        assert!(parse(src).is_ok());
    }

    /// What is not JSONC is an error, not an empty theme.
    #[test]
    fn garbage_is_an_error() {
        assert!(parse("this is not json").is_err());
        assert!(parse(r##"{"colors": ["#fff"]}"##).is_err());
    }
}
