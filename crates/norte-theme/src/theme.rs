//! [`Theme`]: a set of [`Style`]s per [`Role`], plus an OPAQUE effects layer
//! reserved for the GUI's GPU (ADR 0020 D4). Parsed from TOML.

use std::collections::HashMap;

use serde::Deserialize;

use crate::files::{FileColors, FileKind};
use crate::role::Role;
use crate::style::Style;

/// A complete theme. Absent roles inherit their
/// [`fallback`](Role::fallback), so a partial theme ALWAYS resolves.
///
/// Deliberately TOLERANT of unknown top-level keys (no
/// `deny_unknown_fields`): that way a theme with sections from a newer version
/// (e.g. the GUI's `[effects]`, or `[files]`) does not break an old parser.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Theme {
    /// Human-readable theme name (informative).
    #[serde(default)]
    pub name: Option<String>,
    /// Explicit styles per role; whatever is missing falls back.
    #[serde(default)]
    pub roles: HashMap<Role, Style>,
    /// Colors by file type (`[files.kind]` / `[files.ext]`).
    #[serde(default)]
    pub files: FileColors,
    /// GPU effects (gradients, glow, animation…): OPAQUE. A terminal
    /// frontend IGNORES them; the M5 GUI will interpret them (ADR 0020 D4). They are
    /// stored untyped so themes do not break when M5 defines the schema.
    #[serde(default)]
    pub effects: Option<toml::Value>,
}

/// Error loading a theme.
#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    /// The TOML does not parse.
    #[error("invalid TOML theme: {0}")]
    Toml(#[from] toml::de::Error),
}

impl Theme {
    /// Parses a theme from its TOML source.
    ///
    /// # Errors
    /// [`ThemeError::Toml`] if the TOML is not valid.
    pub fn from_toml(src: &str) -> Result<Self, ThemeError> {
        Ok(toml::from_str(src)?)
    }

    /// Writes the theme as TOML that [`Theme::from_toml`] reads back identically.
    ///
    /// The output is DETERMINISTIC —roles in [`Role::ALL`] order, classes
    /// in [`FileKind::ALL`] order, extensions alphabetical— and uses inline
    /// tables, one per role: it is a file someone will open and tweak by
    /// hand, so it is written the way the presets are written, not the way
    /// a generic serializer would leave it with one section per role.
    ///
    /// ```
    /// use norte_theme::{Role, Theme};
    /// let nord = Theme::preset("nord").unwrap().unwrap();
    /// let reread = Theme::from_toml(&nord.to_toml()).unwrap();
    /// assert_eq!(reread.style(Role::Selection), nord.style(Role::Selection));
    /// ```
    #[must_use]
    pub fn to_toml(&self) -> String {
        use std::fmt::Write as _;
        // Writing to a `String` does not fail: the `let _ =` below discard
        // a `Result` that is always `Ok`.
        let mut out = String::new();
        if let Some(name) = &self.name {
            let _ = writeln!(out, "name = {}", toml::Value::from(name.as_str()));
        }
        // `[effects]` goes right after the name: table or bare key,
        // it is valid there, and a bare key AFTER a section would change
        // owner. `toml::to_string` of a table with a single `Value` that
        // came from parsing TOML cannot fail; if it did, the theme would lose
        // its effects and not the rest.
        if let Some(v) = &self.effects {
            let mut t = toml::Table::new();
            t.insert("effects".to_owned(), v.clone());
            if let Ok(e) = toml::to_string(&t) {
                out.push_str(&e);
            }
        }
        if !self.roles.is_empty() {
            out.push_str("\n[roles]\n");
            for role in Role::ALL {
                if let Some(s) = self.roles.get(role) {
                    let _ = writeln!(out, "{} = {}", role.as_kebab(), inline_style(*s));
                }
            }
        }
        if !self.files.kind.is_empty() {
            out.push_str("\n[files.kind]\n");
            // `regular` is not in `ALL` (it is not a class a preset should
            // color), but a theme MAY carry it and `style_for` reads it.
            for kind in FileKind::ALL.iter().chain([&FileKind::Regular]) {
                if let Some(s) = self.files.kind.get(kind) {
                    let _ = writeln!(out, "{} = {}", kind.as_kebab(), inline_style(*s));
                }
            }
        }
        if !self.files.ext.is_empty() {
            out.push_str("\n[files.ext]\n");
            let mut exts: Vec<_> = self.files.ext.iter().collect();
            exts.sort_by(|a, b| a.0.cmp(b.0));
            for (ext, s) in exts {
                let _ = writeln!(out, "{} = {}", toml_key(ext), inline_style(*s));
            }
        }
        out
    }

    /// The effective [`Style`] of a role: the theme's if it defines one, or its
    /// monochrome [`fallback`](Role::fallback). An EXPLICIT theme role
    /// REPLACES the whole fallback (the author takes full control of the role), it is
    /// not merged — so `selection = { bg = "…" }` gives a background without inheriting
    /// the fallback's `reverse`.
    #[must_use]
    pub fn style(&self, role: Role) -> Style {
        self.roles.get(&role).copied().unwrap_or(role.fallback())
    }

    /// The [`Style`] of a file ENTRY `name` (bytes, rule 1) of type
    /// `kind` (ADR 0020 D2). Priority: extension > kind > `regular` role.
    #[must_use]
    pub fn file_style(&self, name: &[u8], kind: FileKind) -> Style {
        self.files
            .style_for(name, kind)
            .unwrap_or_else(|| self.style(Role::Regular))
    }

    /// `true` if the theme has declared effects (a terminal frontend ignores
    /// them; useful for the GUI to decide whether to enable GPU rendering).
    #[must_use]
    pub fn has_effects(&self) -> bool {
        self.effects.is_some()
    }

    /// The names of the declared effects, if the block is a table.
    ///
    /// The `[effects]` block is FREE-FORM on purpose (ADR 0036): each
    /// renderer interprets it, and this crate does not know what any of them means. What it
    /// can say is what they are called, which is what a frontend needs to
    /// list the ones it CANNOT paint — a retro theme that looks identical
    /// reads as broken, so the degradation has to be visible.
    ///
    /// `None` = there is no block, or it is not a table. Both are "nothing
    /// to name" and are not distinguished on purpose: an `[effects]` that is not
    /// a table is a badly written theme, not an empty list of effects.
    ///
    /// ```
    /// use norte_theme::Theme;
    ///
    /// // The factory theme declares one: the window's dialog backdrop
    /// // blur (spec 2026-09-11, V6). The terminal ignores it.
    /// let t = Theme::preset_default();
    /// assert_eq!(t.effect_names().as_deref(), Some(&["backdrop".to_owned()][..]));
    ///
    /// // One that declares none has nothing to name.
    /// let nord = Theme::preset("nord").unwrap().unwrap();
    /// assert!(nord.effect_names().is_none());
    /// ```
    #[must_use]
    pub fn effect_names(&self) -> Option<Vec<String>> {
        match self.effects.as_ref()? {
            toml::Value::Table(t) => Some(t.keys().cloned().collect()),
            _ => None,
        }
    }

    /// The value of ONE effect, if it is a string (`[effects] backdrop =
    /// "blur"`). For the frontend that interprets it without depending on `toml`.
    #[must_use]
    pub fn effect_str(&self, key: &str) -> Option<&str> {
        match self.effects.as_ref()? {
            toml::Value::Table(t) => t.get(key)?.as_str(),
            _ => None,
        }
    }
}

/// `{ fg = "#…", bg = "#…", bold = true }`, with only what the style has.
fn inline_style(s: Style) -> String {
    let mut parts = Vec::new();
    if let Some(fg) = s.fg {
        parts.push(format!("fg = \"{}\"", fg.to_hex()));
    }
    if let Some(bg) = s.bg {
        parts.push(format!("bg = \"{}\"", bg.to_hex()));
    }
    for (on, name) in [
        (s.bold, "bold"),
        (s.dim, "dim"),
        (s.italic, "italic"),
        (s.underline, "underline"),
        (s.reverse, "reverse"),
    ] {
        if on {
            parts.push(format!("{name} = true"));
        }
    }
    if parts.is_empty() {
        "{}".to_owned()
    } else {
        format!("{{ {} }}", parts.join(", "))
    }
}

/// A TOML key: bare if possible, quoted otherwise.
fn toml_key(k: &str) -> String {
    if !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        k.to_owned()
    } else {
        // By hand: the `toml` writer picks a TRIPLE-quoted string
        // when the text has a line break, and that is not valid as a key.
        let mut out = String::from("\"");
        for c in k.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                c if c.is_control() => {
                    use std::fmt::Write as _;
                    let _ = write!(out, "\\u{:04X}", u32::from(c));
                }
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every preset round-trips through `to_toml` without losing anything: roles,
    /// classes, extensions, name and effects.
    #[test]
    fn to_toml_round_trips_every_preset() {
        for name in crate::preset_names() {
            let t = Theme::preset(name).unwrap().unwrap();
            let written = t.to_toml();
            let v = Theme::from_toml(&written)
                .unwrap_or_else(|e| panic!("[{name}] does not reread: {e}\n{written}"));
            assert_eq!(v.name, t.name, "[{name}]");
            assert_eq!(v.roles, t.roles, "[{name}]");
            assert_eq!(v.files.kind, t.files.kind, "[{name}]");
            assert_eq!(v.files.ext, t.files.ext, "[{name}]");
            assert_eq!(v.effects, t.effects, "[{name}]");
        }
    }

    /// A name with quotes or line breaks, an extension that is not a bare key
    /// and an `[effects]` that is not a table: what a hand-written serializer breaks.
    #[test]
    fn to_toml_escapes_the_odd_cases() {
        let mut t = Theme {
            name: Some("dice \"hola\"\ny adiós".to_owned()),
            ..Theme::default()
        };
        t.files.ext.insert("tar.gz".to_owned(), Style::new().bold());
        t.files.ext.insert("a\nb".to_owned(), Style::new().dim());
        t.files.ext.insert("a\"b\\c".to_owned(), Style::new().dim());
        t.files.kind.insert(
            FileKind::Regular,
            Style::new().fg(crate::Color::rgb(1, 2, 3)),
        );
        t.roles.insert(Role::Mark, Style::new());
        t.effects = Some(toml::Value::from("loose"));
        let v = Theme::from_toml(&t.to_toml()).expect("rereads");
        assert_eq!(v.name, t.name);
        assert_eq!(v.files.ext, t.files.ext);
        assert_eq!(v.files.kind, t.files.kind);
        assert_eq!(v.roles, t.roles);
        assert_eq!(v.effects, t.effects);
    }
}
