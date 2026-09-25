//! Command-line parsing SHARED by the interactive frontends (TUI and GUI).
//! Neither uses clap: they are binaries whose startup time is noticeable, and
//! their argument surface is a handful of flags. Sharing it keeps them from
//! diverging on what actually is a contract with the person typing: what the
//! positional is, which flags exist, and what happens with one that is not.
//!
//! Each frontend DECLARES its surface ([`parse`] receives which flags it
//! accepts), so a flag it does not support comes out through [`Cli::unknown`]
//! and the binary can reject it with a message — never swallow it silently,
//! which is how `--help` used to end up dying inside the terminal's
//! initializer.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

/// What the command line asked for.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Cli {
    /// First positional argument: the starting directory. Kept as a raw path
    /// (rule 1: a directory name has no reason to be UTF-8). Further
    /// positionals are ignored.
    pub dir: Option<PathBuf>,
    /// Boolean flags present, by EXACT name (`--daemon`).
    pub flags: Vec<String>,
    /// Flags with a value, by exact name (`--socket` → its raw value).
    pub values: BTreeMap<String, OsString>,
    /// Help was requested (`-h`/`--help`).
    pub help: bool,
    /// The version was requested (`-V`/`--version`).
    pub version: bool,
    /// FIRST unrecognized flag, exactly as typed.
    pub unknown: Option<String>,
}

impl Cli {
    /// Was this boolean flag present?
    #[must_use]
    pub fn has(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f == flag)
    }

    /// A value-flag's value, as a `String` with a LOSSY conversion — only for
    /// values that are text by contract (a preset name). For paths use
    /// [`Cli::path`], which does not touch the bytes.
    #[must_use]
    pub fn text(&self, flag: &str) -> Option<String> {
        self.values
            .get(flag)
            .map(|v| v.to_string_lossy().into_owned())
    }

    /// A value-flag's value, with the BYTES intact.
    ///
    /// For what is not text by contract even though it looks like it: a
    /// layout's name ends up as `layouts/<name>.toml`, so passing it through
    /// [`Cli::text`] changed which file gets opened — two different invalid
    /// byte sequences landed on the same `\u{FFFD}.toml` — without saying
    /// anything (#246).
    #[must_use]
    pub fn os_text(&self, flag: &str) -> Option<&std::ffi::OsStr> {
        self.values.get(flag).map(OsString::as_os_str)
    }

    /// A value-flag's value, as a path (bytes intact).
    #[must_use]
    pub fn path(&self, flag: &str) -> Option<PathBuf> {
        self.values.get(flag).map(PathBuf::from)
    }
}

/// Parses `argv` WITHOUT `argv[0]` (the caller skips it).
///
/// `bool_flags` and `value_flags` are the surface the frontend supports;
/// `-h`/`--help` and `-V`/`--version` are always recognized. A flag outside
/// those lists goes to [`Cli::unknown`] instead of being ignored. A
/// value-flag with no value after it (`--socket` at the end) ends up with no
/// entry, as if it had not been passed.
#[must_use]
pub fn parse<I, S>(argv: I, bool_flags: &[&str], value_flags: &[&str]) -> Cli
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut out = Cli::default();
    let mut it = argv.into_iter().map(Into::into);
    while let Some(arg) = it.next() {
        let a = arg.to_string_lossy().into_owned();
        match a.as_str() {
            "-h" | "--help" => out.help = true,
            "-V" | "--version" => out.version = true,
            s if bool_flags.contains(&s) => out.flags.push(s.to_owned()),
            s if value_flags.contains(&s) => {
                if let Some(v) = it.next() {
                    out.values.insert(s.to_owned(), v);
                }
            }
            // A bare `-` is a positional by convention (stdin), not a flag.
            s if s.starts_with('-') && s != "-" => {
                if out.unknown.is_none() {
                    out.unknown = Some(s.to_owned());
                }
            }
            _ if out.dir.is_none() => out.dir = Some(PathBuf::from(arg)),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Cli, parse};

    const BOOL: &[&str] = &["--daemon"];
    const VALUE: &[&str] = &["--preset", "--socket"];

    /// The positional is the DIR; value-flags take the following argument;
    /// booleans are recorded by name.
    #[test]
    fn positional_flags_and_values() {
        let c = parse(
            [
                "/tmp/x", "--preset", "vim", "--daemon", "--socket", "/run/s",
            ],
            BOOL,
            VALUE,
        );
        assert_eq!(c.dir.as_deref(), Some(std::path::Path::new("/tmp/x")));
        assert_eq!(c.text("--preset").as_deref(), Some("vim"));
        assert!(c.has("--daemon"));
        assert_eq!(
            c.path("--socket").as_deref(),
            Some(std::path::Path::new("/run/s"))
        );
        assert!(c.unknown.is_none());
    }

    /// `--help`/`--version` are ALWAYS recognized, no matter who declares
    /// them: they used to fall into "unknown flag, ignore" and the binary
    /// kept going until it died holding the terminal.
    #[test]
    fn help_and_version_always() {
        for a in ["-h", "--help"] {
            assert!(parse([a], &[], &[]).help, "{a}");
        }
        for a in ["-V", "--version"] {
            assert!(parse([a], &[], &[]).version, "{a}");
        }
    }

    /// A flag outside the DECLARED surface is named (the frontend rejects
    /// it), even when another frontend does support it — each one answers
    /// for its own.
    #[test]
    fn flag_outside_the_declared_surface() {
        let c = parse(["--daemon", "/tmp"], &[], VALUE);
        assert_eq!(c.unknown.as_deref(), Some("--daemon"));
        assert_eq!(c.dir.as_deref(), Some(std::path::Path::new("/tmp")));
        assert_eq!(
            parse(["--nope"], BOOL, VALUE).unknown.as_deref(),
            Some("--nope")
        );
    }

    /// A value-flag with NO value after it invents nothing; with no
    /// arguments, nothing is asked for.
    #[test]
    fn missing_and_empty_value() {
        let c = parse(["--socket"], BOOL, VALUE);
        assert!(c.path("--socket").is_none());
        assert_eq!(
            parse(std::iter::empty::<String>(), BOOL, VALUE),
            Cli::default()
        );
    }

    /// Rule 1: a dir with non-UTF-8 bytes arrives WHOLE (no lossy anywhere
    /// on the data's path).
    #[test]
    #[cfg(unix)]
    fn non_utf8_dir_survives() {
        use std::os::unix::ffi::OsStringExt as _;
        let raw = std::ffi::OsString::from_vec(b"/tmp/due\xffo".to_vec());
        let c = parse([raw.clone()], BOOL, VALUE);
        assert_eq!(c.dir.as_deref(), Some(std::path::Path::new(&raw)));
    }
}
