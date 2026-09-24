//! norte's shared theming model: colors, semantic roles and themes,
//! independent of the rendering framework (ADR 0020).
//!
//! The TUI (`norte-tui`) already consumes it; the M5 GUI REUSES the same model. That
//! is why this crate does NOT depend on `ratatui` or any backend: it exposes its own
//! [`Color`] (24-bit RGB with degradation to 256/16), [`Style`]s per semantic
//! [`Role`], and an OPAQUE effects layer reserved for the GUI's GPU that a
//! terminal frontend ignores at no cost.
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
//! // The color degrades to the terminal's depth:
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
