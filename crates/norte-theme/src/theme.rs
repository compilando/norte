//! [`Theme`]: un conjunto de [`Style`]s por [`Role`], más una capa de efectos
//! OPACA reservada a la GPU de la GUI (ADR 0020 D4). Se parsea desde TOML.

use std::collections::HashMap;

use serde::Deserialize;

use crate::role::Role;
use crate::style::Style;

/// Un tema completo. Los roles ausentes heredan su
/// [`fallback`](Role::fallback), así que un tema parcial SIEMPRE resuelve.
///
/// Deliberadamente TOLERANTE a claves desconocidas de nivel superior (no
/// `deny_unknown_fields`): así un tema con secciones de una versión más nueva
/// (p. ej. `[effects]` de la GUI, o `[files]`) no rompe un parser viejo.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Theme {
    /// Nombre legible del tema (informativo).
    #[serde(default)]
    pub name: Option<String>,
    /// Estilos explícitos por rol; lo que falte cae al fallback.
    #[serde(default)]
    pub roles: HashMap<Role, Style>,
    /// Efectos de GPU (gradientes, glow, animación…): OPACOS. Un frontend de
    /// terminal los IGNORA; la GUI de M5 los interpretará (ADR 0020 D4). Se
    /// guardan sin tipar para no romper temas cuando M5 defina el esquema.
    #[serde(default)]
    pub effects: Option<toml::Value>,
}

/// Error al cargar un tema.
#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    /// El TOML no parsea.
    #[error("tema TOML inválido: {0}")]
    Toml(#[from] toml::de::Error),
}

impl Theme {
    /// Parsea un tema desde su fuente TOML.
    ///
    /// # Errors
    /// [`ThemeError::Toml`] si el TOML no es válido.
    pub fn from_toml(src: &str) -> Result<Self, ThemeError> {
        Ok(toml::from_str(src)?)
    }

    /// El [`Style`] efectivo de un rol: el del tema si lo define, o su
    /// [`fallback`](Role::fallback) monocromo. Un rol EXPLÍCITO del tema
    /// REEMPLAZA al fallback entero (el autor toma control total del rol), no
    /// se mezcla — así `selection = { bg = "…" }` da fondo sin heredar el
    /// `reverse` del fallback.
    #[must_use]
    pub fn style(&self, role: Role) -> Style {
        self.roles.get(&role).copied().unwrap_or(role.fallback())
    }

    /// `true` si el tema tiene efectos declarados (los ignora un frontend de
    /// terminal; útil para que la GUI decida si activar el render de GPU).
    #[must_use]
    pub fn has_effects(&self) -> bool {
        self.effects.is_some()
    }
}
