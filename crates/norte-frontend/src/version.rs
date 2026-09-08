//! Qué binario es este: la versión que declara el workspace y la revisión del
//! árbol del que salió, fijada por `build.rs` en tiempo de compilación.
//!
//! La versión sola engaña en un equipo de desarrollo: `just link` apunta
//! `ntc` a `target/debug`, y «0.3.0-alpha.3» es la misma cadena diez commits
//! después de la etiqueta. La revisión es lo que distingue un binario de otro.

/// La versión del workspace, la misma que llevan todos los crates.
///
/// ```
/// assert!(norte_frontend::version::VERSION.starts_with("0."));
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// La revisión del árbol: `git describe --tags --always --dirty --long`
/// (por ejemplo `v0.3.0-alpha.3-10-g674b0eb9`, o con `-dirty` detrás si había
/// cambios sin commitear), o lo que el empaquetador puso en `NORTE_REVISION`,
/// o `unknown` si no hay ni `.git` ni variable.
///
/// ```
/// assert!(!norte_frontend::version::REVISION.is_empty());
/// ```
pub const REVISION: &str = env!("NORTE_REVISION");

/// La línea que enseñan `--version`, la ayuda del TUI y el título de la
/// ventana: `0.3.0-alpha.3 (v0.3.0-alpha.3-10-g674b0eb9)`.
///
/// ```
/// let l = norte_frontend::version::VERSION_LINE;
/// assert!(l.starts_with(norte_frontend::version::VERSION));
/// assert!(l.ends_with(')'));
/// ```
pub const VERSION_LINE: &str =
    concat!(env!("CARGO_PKG_VERSION"), " (", env!("NORTE_REVISION"), ")");
