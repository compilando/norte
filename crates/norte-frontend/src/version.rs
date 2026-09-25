//! Which binary this is: the version the workspace declares and the revision
//! of the tree it came from, fixed by `build.rs` at compile time.
//!
//! The version alone is misleading on a development machine: `just link`
//! points `ntc` at `target/debug`, and "0.3.0-alpha.3" is the same string ten
//! commits after the tag. The revision is what tells one binary from another.

/// The workspace version, the same one every crate carries.
///
/// ```
/// assert!(norte_frontend::version::VERSION.starts_with("0."));
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The tree's revision: `git describe --tags --always --dirty --long`
/// (for example `v0.3.0-alpha.3-10-g674b0eb9`, with `-dirty` appended if there
/// were uncommitted changes), or whatever the packager put in
/// `NORTE_REVISION`, or `unknown` if there is neither `.git` nor the variable.
///
/// ```
/// assert!(!norte_frontend::version::REVISION.is_empty());
/// ```
pub const REVISION: &str = env!("NORTE_REVISION");

/// The line shown by `--version`, the TUI's help, and the window's title:
/// `0.3.0-alpha.3 (v0.3.0-alpha.3-10-g674b0eb9)`.
///
/// ```
/// let l = norte_frontend::version::VERSION_LINE;
/// assert!(l.starts_with(norte_frontend::version::VERSION));
/// assert!(l.ends_with(')'));
/// ```
pub const VERSION_LINE: &str =
    concat!(env!("CARGO_PKG_VERSION"), " (", env!("NORTE_REVISION"), ")");
