//! Modelo de theming compartido de norte: colores, roles semánticos y temas,
//! independientes del framework de render (ADR 0020).
//!
//! El TUI (`norte-tui`) lo consume ya; la GUI de M5 REUSA el mismo modelo. Por
//! eso este crate NO depende de `ratatui` ni de ningún backend: expone un
//! [`Color`] propio (RGB de 24 bits con degradación a 256/16), [`Style`]s por
//! [`Role`] semántico, y una capa de efectos OPACA reservada a la GPU de la
//! GUI que un frontend de terminal ignora sin coste.
//!
//! ```
//! use norte_theme::{Theme, Role, ColorDepth, ResolvedColor};
//! let theme = Theme::from_toml(r##"
//!     name = "demo"
//!     [roles]
//!     selection = { bg = "#45475a", bold = true }
//! "##).unwrap();
//! let sel = theme.style(Role::Selection);
//! assert!(sel.bold);
//! // El color degrada a la profundidad del terminal:
//! let bg = sel.bg.unwrap().resolve(ColorDepth::Truecolor);
//! assert_eq!(bg, ResolvedColor::Rgb(0x45, 0x47, 0x5a));
//! ```
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod color;
mod files;
mod presets;
mod role;
mod style;
mod theme;
pub mod vscode;

pub use color::{Color, ColorDepth, ColorParseError, ResolvedColor};
pub use files::{FileColors, FileKind, extension_of};
pub use presets::{DEFAULT_PRESET, preset_names, preset_source};
pub use role::Role;
pub use style::Style;
pub use theme::{Theme, ThemeError};
