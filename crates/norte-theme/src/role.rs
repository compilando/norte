//! [`Role`]: los papeles SEMÁNTICOS que un tema estiliza (ADR 0020 D2). El
//! frontend pide un rol, nunca un color suelto. Cada rol trae un
//! [`fallback`](Role::fallback) monocromo que reproduce el aspecto de M1, de
//! modo que SIN tema (o con uno parcial) la UI sigue siendo coherente.

use serde::{Deserialize, Serialize};

use crate::style::Style;

/// Papel semántico de la UI. Añadir una variante es no-breaking: un tema que no
/// la cubre hereda su [`fallback`](Role::fallback).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Role {
    /// Texto normal / entrada de fichero por defecto.
    Regular,
    /// Fila seleccionada en un panel.
    Selection,
    /// Borde del panel con foco.
    BorderFocus,
    /// Borde del panel sin foco.
    BorderUnfocused,
    /// Borde de un modal/diálogo.
    ModalBorder,
    /// Barra de estado.
    StatusBar,
    /// Título de panel/modal.
    Title,
    /// Marca de nombre hostil (bytes no imprimibles, control…).
    HostileBadge,
    /// Mensaje de error.
    Error,
    /// Mensaje de aviso (p. ej. borrado permanente).
    Warning,
    /// Mensaje informativo.
    Info,
    /// Coincidencia de búsqueda resaltada.
    Match,
}

impl Role {
    /// Todos los roles, para iterar (p. ej. validar que un preset los cubre).
    pub const ALL: &'static [Role] = &[
        Role::Regular,
        Role::Selection,
        Role::BorderFocus,
        Role::BorderUnfocused,
        Role::ModalBorder,
        Role::StatusBar,
        Role::Title,
        Role::HostileBadge,
        Role::Error,
        Role::Warning,
        Role::Info,
        Role::Match,
    ];

    /// Estilo por defecto MONOCROMO del rol: reproduce el aspecto de M1
    /// (`BOLD`/`REVERSED`/`DIM` donde hoy los hay) sin color. Es lo que se usa
    /// cuando el tema no define el rol, de modo que un usuario sin tema ve
    /// exactamente la UI de siempre.
    #[must_use]
    pub const fn fallback(self) -> Style {
        match self {
            Role::Selection | Role::StatusBar => Style::new().reverse(),
            Role::BorderFocus | Role::ModalBorder | Role::HostileBadge | Role::Title => {
                Style::new().bold()
            }
            Role::BorderUnfocused => Style::new().dim(),
            // Regular/Error/Warning/Info/Match: sin color por defecto (la UI de
            // M1 no los distinguía). Un tema con color los diferencia.
            Role::Regular | Role::Error | Role::Warning | Role::Info | Role::Match => Style::new(),
        }
    }
}
