//! [`Style`]: el aspecto de un [`Role`](crate::Role) — color de frente/fondo y
//! atributos. Independiente del backend: el frontend traduce a su tipo `Style`.

use serde::{Deserialize, Serialize};

use crate::color::Color;

/// Estilo visual: colores opcionales + atributos. Un campo ausente = «hereda»
/// (el frontend deja el del terminal / el heredado del rol base).
// Los cinco atributos son banderas independientes de terminal (bold/dim/
// italic/underline/reverse): un struct de bools ES la representación natural,
// no un enum ni flags empaquetadas.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Style {
    /// Color de primer plano (texto).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fg: Option<Color>,
    /// Color de fondo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg: Option<Color>,
    /// Negrita.
    pub bold: bool,
    /// Atenuado.
    pub dim: bool,
    /// Cursiva.
    pub italic: bool,
    /// Subrayado.
    pub underline: bool,
    /// Invierte frente y fondo (el `REVERSED` de hoy).
    pub reverse: bool,
}

impl Style {
    /// Estilo vacío (todo heredado).
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

    /// Con color de frente.
    #[must_use]
    pub const fn fg(mut self, c: Color) -> Self {
        self.fg = Some(c);
        self
    }

    /// Con color de fondo.
    #[must_use]
    pub const fn bg(mut self, c: Color) -> Self {
        self.bg = Some(c);
        self
    }

    /// Marca negrita.
    #[must_use]
    pub const fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    /// Marca atenuado.
    #[must_use]
    pub const fn dim(mut self) -> Self {
        self.dim = true;
        self
    }

    /// Marca invertido.
    #[must_use]
    pub const fn reverse(mut self) -> Self {
        self.reverse = true;
        self
    }

    /// Superpone `over` SOBRE `self`: los colores presentes en `over` pisan;
    /// los atributos se acumulan con OR (un rol base + override del tema).
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
