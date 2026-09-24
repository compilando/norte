//! [`Style`]: the look of a [`Role`](crate::Role) — foreground/background color and
//! attributes. Backend-independent: the frontend translates it to its `Style` type.

use serde::{Deserialize, Serialize};

use crate::color::Color;

/// Visual style: optional colors + attributes. An absent field = "inherit"
/// (the frontend keeps the terminal's / the one inherited from the base role).
// The five attributes are independent terminal flags (bold/dim/
// italic/underline/reverse): a struct of bools IS the natural representation,
// not an enum or packed flags.
// TODO(translation): review — the reason says four attributes; there are five.
#[expect(
    clippy::struct_excessive_bools,
    reason = "four independent style attributes, not an enum or packed flags"
)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Style {
    /// Foreground (text) color.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fg: Option<Color>,
    /// Background color.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg: Option<Color>,
    /// Bold.
    pub bold: bool,
    /// Dimmed.
    pub dim: bool,
    /// Italic.
    pub italic: bool,
    /// Underlined.
    pub underline: bool,
    /// Swaps foreground and background (today's `REVERSED`).
    pub reverse: bool,
}

impl Style {
    /// Empty style (everything inherited).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            fg: None,
            bg: None,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            reverse: false,
        }
    }

    /// With a foreground color.
    #[must_use]
    pub const fn fg(mut self, c: Color) -> Self {
        self.fg = Some(c);
        self
    }

    /// With a background color.
    #[must_use]
    pub const fn bg(mut self, c: Color) -> Self {
        self.bg = Some(c);
        self
    }

    /// Sets bold.
    #[must_use]
    pub const fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    /// Sets dimmed.
    #[must_use]
    pub const fn dim(mut self) -> Self {
        self.dim = true;
        self
    }

    /// Sets reversed.
    #[must_use]
    pub const fn reverse(mut self) -> Self {
        self.reverse = true;
        self
    }

    /// Overlays `over` ON TOP of `self`: colors present in `over` win;
    /// attributes accumulate with OR (a base role + the theme's override).
    #[must_use]
    pub fn overlay(self, over: Style) -> Style {
        Style {
            fg: over.fg.or(self.fg),
            bg: over.bg.or(self.bg),
            bold: self.bold || over.bold,
            dim: self.dim || over.dim,
            italic: self.italic || over.italic,
            underline: self.underline || over.underline,
            reverse: self.reverse || over.reverse,
        }
    }
}
