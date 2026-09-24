//! Declarative openers (#28, spec §7.3, ADR 0010): map mimetype+OS to an
//! external binary chosen by the user (`bat`, `delta`, `unrar`, `xdg-open`…)
//! with field codes `%f`/`%F`/`%d`. They are CONFIGURATION, not plugins:
//! WASM plugins have no `exec` (spec §7.1), so this is the only path to
//! delegate to external tools — runtime detection and clean degradation
//! ("install X"), never linked.
//!
//! This module is PURE RESOLUTION (parsing, selection by mimetype+OS,
//! byte-safe argv construction, PATH probing); the actual `spawn` is done by
//! the frontend. No network or disk I/O other than probing the binary.

use std::ffi::OsStr;
use std::path::Path;

use serde::Deserialize;

/// Error loading `openers.toml`.
#[derive(Debug, thiserror::Error)]
pub enum OpenerError {
    /// The TOML fails to parse or has unknown keys.
    #[error("openers.toml: {0}")]
    Toml(String),
    /// An entry with an empty `command` (no binary to launch).
    #[error("opener for {mime:?}: `command` cannot be empty")]
    EmptyCommand {
        /// The invalid entry's mimetype.
        mime: String,
    },
}

/// An entry in `openers.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Opener {
    /// Mimetype glob: `text/*` or exact `application/pdf`.
    mime: String,
    /// The OS it applies to (`linux`/`macos`/`windows`, values from
    /// [`std::env::consts::OS`]); absent = any.
    #[serde(default)]
    os: Option<String>,
    /// Does it open its own WINDOW? Absent = `false`.
    ///
    /// A terminal program (`bat`, `vim`) needs the frontend to step aside
    /// and wait for it; a graphical one (`zed`, `loupe`) returns control
    /// instantly, and suspending the TUI for it leaves the reader staring
    /// at a blank terminal until they close a window that is somewhere
    /// else. Norte cannot guess which is which: whoever writes the rule
    /// states it.
    #[serde(default)]
    detached: Option<bool>,
    /// argv template: `["bat", "--paging=always", "%f"]`. The first token is
    /// the binary; field codes `%f`/`%F`/`%d` are substituted ONLY as whole
    /// tokens (never inside a literal — so a non-UTF-8 path is never
    /// concatenated with text and is preserved byte for byte, hard rule 1).
    /// A field code EMBEDDED in a literal (`--file=%f`) is NOT expanded: it
    /// is passed as-is as a literal argument (use its own token instead).
    command: Vec<String>,
}

/// A parsed `openers.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenersConfig {
    /// The entries, in declaration order.
    #[serde(default, rename = "opener")]
    openers: Vec<Opener>,
}

impl OpenersConfig {
    /// Empty config (no openers): the default when the file does not exist.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Prepends `higher`'s openers (the HIGHER-precedence layer): on a
    /// mimetype+OS tie, the upper layer wins ([`Self::resolve`] matches the
    /// first entry). Used by the frontend when merging layers (user over
    /// system); the PROJECT layer is excluded before calling here — a
    /// hostile repo must not inject external commands that get executed.
    pub fn extend_front(&mut self, higher: OpenersConfig) {
        let mut merged = higher.openers;
        merged.append(&mut self.openers);
        self.openers = merged;
    }

    /// Parses an `openers.toml`.
    ///
    /// # Errors
    /// [`OpenerError::Toml`] if it fails to parse or has unknown keys;
    /// [`OpenerError::EmptyCommand`] if an entry has an empty `command`.
    ///
    /// ```
    /// let cfg = norte_frontend::openers::OpenersConfig::parse(
    ///     "[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"%f\"]\n",
    /// )
    /// .unwrap();
    /// assert!(cfg.resolve("text/plain").is_some());
    /// ```
    pub fn parse(s: &str) -> Result<Self, OpenerError> {
        let cfg: Self = toml::from_str(s).map_err(|e| OpenerError::Toml(e.message().to_owned()))?;
        for o in &cfg.openers {
            if o.command.is_empty() {
                return Err(OpenerError::EmptyCommand {
                    mime: o.mime.clone(),
                });
            }
        }
        Ok(cfg)
    }

    /// The opener for `mime` on the current OS: among the entries whose
    /// mimetype glob matches, the one specific to THIS OS wins over the
    /// agnostic one; on a tie, the first declared. `None` if none applies.
    #[must_use]
    pub fn resolve(&self, mime: &str) -> Option<&Opener> {
        self.resolve_for(mime, std::env::consts::OS)
    }

    /// Like [`Self::resolve`] but with the OS explicit (testable without
    /// depending on the runner's OS).
    ///
    /// ```
    /// use norte_frontend::openers::OpenersConfig;
    /// let cfg = OpenersConfig::parse(
    ///     "[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"%f\"]\n",
    /// )
    /// .unwrap();
    /// assert_eq!(cfg.resolve_for("text/html", "linux").unwrap().program(), "bat");
    /// assert!(cfg.resolve_for("application/pdf", "linux").is_none());
    /// ```
    #[must_use]
    pub fn resolve_for(&self, mime: &str, os: &str) -> Option<&Opener> {
        let matches = |o: &&Opener| mimetype_matches(&o.mime, mime);
        // Specific to this OS first; then the agnostic one (os = None).
        self.openers
            .iter()
            .find(|o| o.os.as_deref() == Some(os) && matches(o))
            .or_else(|| self.openers.iter().find(|o| o.os.is_none() && matches(o)))
    }
}

impl Opener {
    /// The binary to launch (the `command`'s first token). Never empty:
    /// construction guarantees it ([`OpenersConfig::parse`]).
    #[must_use]
    pub fn program(&self) -> &str {
        &self.command[0]
    }

    /// The complete argv with field codes substituted. `%f` → the FIRST
    /// path; `%F` → ALL of them (one arg per path); `%d` → the directory.
    /// Substitution ONLY of whole, byte-safe tokens: a path travels as an
    /// `OsStr` without going through `String` (hard rule 1). A field code
    /// whose input is empty is OMITTED (the binary is not launched with a
    /// literal `%f` token).
    ///
    /// Security: the caller passes ABSOLUTE paths (`vpath_to_native`), which
    /// start with `/` (or `\\?\…` on Windows). That keeps a hostile name
    /// like `-rf` or `--config=…` from sneaking in as a FLAG of the target
    /// program: every field code is a single argv element AND never starts
    /// with `-`.
    #[must_use]
    pub fn argv(&self, files: &[&Path], dir: &Path) -> Vec<std::ffi::OsString> {
        expand_argv(&self.command, files, dir)
    }

    /// Does it open its own window and therefore NOT get waited for?
    /// (`detached`).
    #[must_use]
    pub fn detached(&self) -> bool {
        self.detached.unwrap_or(false)
    }
}

/// Substitutes an argv template's field codes. `%f` → the FIRST path; `%F` →
/// ALL of them (one arg per path); `%d` → the directory.
///
/// Free-standing and public because `openers.toml` is not the only place the
/// user writes a template: `[ui] editor` uses the same grammar, and having
/// two expanders would mean two quoting rules over paths that are BYTES
/// (hard rule 1). See [`Opener::argv`] for the full contract, including why
/// a field code is always a whole argv element and never gets concatenated
/// with text.
#[must_use]
pub fn expand_argv(command: &[String], files: &[&Path], dir: &Path) -> Vec<std::ffi::OsString> {
    let mut out = Vec::with_capacity(command.len());
    let Some((program, rest)) = command.split_first() else {
        return out;
    };
    // The first token (the binary) is NEVER interpolated.
    out.push(program.as_str().into());
    for tok in rest {
        match tok.as_str() {
            "%f" => {
                if let Some(first) = files.first() {
                    out.push(first.as_os_str().to_os_string());
                }
            }
            "%F" => out.extend(files.iter().map(|p| p.as_os_str().to_os_string())),
            "%d" => out.push(dir.as_os_str().to_os_string()),
            lit => out.push(OsStr::new(lit).to_os_string()),
        }
    }
    out
}

/// `true` if `program` is a locatable executable binary: an existing
/// absolute path, or a name present in some `PATH` entry. Pure probing
/// (stat), running nothing — the basis of clean degradation ("install X").
/// Takes an `OsStr` for the same reason as [`resolve_program`], which it
/// implements: a program is BYTES (hard rule 1), and a probe that asked for
/// text would invite the next caller to write a `to_str()`.
#[must_use]
pub fn program_available(program: &std::ffi::OsStr) -> bool {
    resolve_program(program).is_some()
}

/// The ABSOLUTE path of the binary `program` names, or `None` if not found.
/// Same probing as [`program_available`] —which it now implements— but
/// returning WHAT was found.
///
/// The difference matters when the child is launched with
/// `Command::current_dir` set (S4, `app.terminal`): on unix, `current_dir`
/// is applied BEFORE resolving the program, so `execvp` resolves a relative
/// name against the directory the user is NAVIGATING, not norte's. With a
/// `.` (or an empty component) in the `PATH`, a file named `kitty` inside a
/// freshly extracted archive would run as the user — and the probe would not
/// see it, because it runs with norte's cwd: probe and launch would be
/// looking at different directories by construction. Launching the absolute
/// path the probe returned is what makes the two agree.
///
/// A `program` that is ALREADY absolute is returned as-is if it exists. A
/// relative one with a separator (`./tool`) is resolved against the current
/// cwd and canonicalized to absolute, for the same reason.
///
/// Takes `OsStr` and not `&str` because since #302 it also resolves the
/// editor, which comes from `$VISUAL`/`$EDITOR`: an environment variable is
/// BYTES and an editor can live under a path that is not UTF-8 like
/// anything else (hard rule 1).
#[must_use]
pub fn resolve_program(program: &std::ffi::OsStr) -> Option<std::path::PathBuf> {
    resolve_program_in(program, std::env::var_os("PATH").as_deref())
}

/// The testable core of [`resolve_program`]: the `PATH` comes in as an
/// ARGUMENT.
///
/// Same discipline as [`crate::shell`]'s `*_from`: a test that touched the
/// process's variable would race every other test in the same binary, and
/// `std::env::set_var` is `unsafe` since Rust 2024 (hard rule 5). What this
/// core does NOT abstract away is the disk: the probe is a real `stat`, so
/// its tests set up a temp directory instead of faking one.
///
/// ```
/// use std::ffi::OsStr;
/// use norte_frontend::openers::resolve_program_in;
///
/// // A RELATIVE PATH entry resolves nothing, nor does the empty one —which
/// // means "the current directory"—, even if the file is there: the child
/// // is launched with the NAVIGATED directory as its cwd (#302).
/// assert_eq!(resolve_program_in(OsStr::new("sh"), Some(OsStr::new("."))), None);
/// assert_eq!(resolve_program_in(OsStr::new("sh"), Some(OsStr::new(""))), None);
/// // And with no PATH there is nowhere to look.
/// assert_eq!(resolve_program_in(OsStr::new("sh"), None), None);
/// ```
#[must_use]
pub fn resolve_program_in(
    program: &std::ffi::OsStr,
    path_var: Option<&std::ffi::OsStr>,
) -> Option<std::path::PathBuf> {
    let p = Path::new(program);
    if p.is_absolute() {
        return with_extensions(p).find(|c| is_executable(c));
    }
    // A name with a separator but relative (`./tool`) resolves against the
    // cwd; a plain one (`bat`) is looked up in the PATH. "With a separator"
    // is asked via the PARENT and not via the separator byte: that way it
    // works the same on Windows, where there are two separators.
    if p.parent().is_some_and(|d| !d.as_os_str().is_empty()) {
        // Absolute BEFORE anyone changes the child's cwd. The `join` comes
        // FIRST and the probe after, because on Windows `C:tool` is
        // relative to THAT DRIVE's current directory and `join` replaces it
        // instead of composing it: probing the relative one and returning
        // the composed one would say yes about one file and launch another.
        let absolute = std::env::current_dir().ok()?.join(p);
        if !absolute.is_absolute() {
            return None;
        }
        return with_extensions(&absolute).find(|c| is_executable(c));
    }
    let path = path_var?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        // An EMPTY `PATH` entry means "the current directory", which for a
        // child with `current_dir` set is the navigated directory: it is
        // never resolved against it, not even if it exists.
        .filter(|c| c.is_absolute())
        .flat_map(|c| with_extensions(&c).collect::<Vec<_>>())
        .find(|c| is_executable(c))
}

/// The candidate as-is. On unix an executable carries no extension and the
/// question does not exist.
#[cfg(unix)]
fn with_extensions(p: &Path) -> impl Iterator<Item = std::path::PathBuf> {
    std::iter::once(p.to_path_buf())
}

/// The candidate as-is and with each `PATHEXT` extension appended.
///
/// On Windows executability is decided by the extension, and the OS itself
/// tries `PATHEXT` when given a bare name. Since here "does not resolve"
/// came to mean "does not launch" (#302), without this a hand-written
/// `[ui] editor = vim` would stop opening anything — the OS used to resolve
/// it before.
#[cfg(not(unix))]
fn with_extensions(p: &Path) -> impl Iterator<Item = std::path::PathBuf> {
    let base = p.to_path_buf();
    let exts = std::env::var_os("PATHEXT")
        .unwrap_or_else(|| std::ffi::OsString::from(".COM;.EXE;.BAT;.CMD"));
    let mut out = vec![base.clone()];
    for ext in exts.to_string_lossy().split(';').filter(|e| !e.is_empty()) {
        let mut with_ext = base.clone().into_os_string();
        with_ext.push(ext);
        out.push(std::path::PathBuf::from(with_ext));
    }
    out.into_iter()
}

/// `true` if `p` exists and (on unix) has some execute bit.
#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// `true` if `p` exists as a file (Windows has no execute bit; executability
/// is decided by the extension, outside this probe's scope).
#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// Guesses the mimetype by EXTENSION (a light heuristic, no sniffing or
/// content reading). A LOCAL copy of `norte-core`'s criterion (the frontend
/// crate does not depend on the core): no recognizable extension →
/// `application/octet-stream`.
///
/// Operates on raw bytes (hard rule 1): it splits on the LAST `.` at the
/// byte level and only the EXTENSION is validated as UTF-8 — a name with a
/// non-UTF-8 stem but an ASCII extension (`caf\xe9\xff.txt`) does detect
/// `text/plain`. A non-UTF-8 extension matches nothing.
///
/// ```
/// use norte_frontend::openers::guess_mime;
/// assert_eq!(guess_mime(b"notes.md"), "text/plain");
/// assert_eq!(guess_mime(b"sin_extension"), "application/octet-stream");
/// // non-UTF-8 stem + ASCII extension: the extension governs.
/// assert_eq!(guess_mime(b"caf\xe9\xff.pdf"), "application/pdf");
/// ```
#[must_use]
pub fn guess_mime(name: &[u8]) -> &'static str {
    let ext = name
        .iter()
        .rposition(|&b| b == b'.')
        .and_then(|dot| std::str::from_utf8(&name[dot + 1..]).ok())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("txt" | "md" | "rs" | "toml" | "log" | "csv" | "ini" | "conf") => "text/plain",
        Some("json") => "application/json",
        Some("html" | "htm") => "text/html",
        Some("xml") => "text/xml",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}

/// The DESKTOP's OWN launcher: the program the system has associated with
/// the file. It is `pane.open`'s last resort when `ns.toml` declares no
/// opener for that mimetype — without this, a user who has written no
/// configuration cannot open anything.
///
/// Returns `(program, argv)` ready for `spawn`; the binary is probed
/// separately with [`program_available`] (a Linux with no `xdg-utils`
/// installed is a real case, not a theoretical one).
///
/// The argv is built byte by byte from the NATIVE path, never from a
/// conversion to text (hard rule 1): a non-UTF-8 name arrives intact at the
/// associated program.
///
/// Per platform:
/// - **Linux and other unix**: `xdg-open`, the freedesktop standard.
/// - **macOS**: `open`, which ships with the base system.
/// - **Windows**: `explorer.exe`, NOT `cmd /C start`. The difference
///   matters: `cmd` re-interprets its command line, so a file name with `&`
///   or `^` can execute what it should not; `explorer.exe` receives the
///   argument as-is. (`explorer` returns exit code 1 even when it opens
///   fine — that is why this path does not interpret the status.)
///
/// ```
/// # use std::path::Path;
/// let (program, argv) = norte_frontend::openers::system_opener(Path::new("/tmp/a.pdf"));
/// assert!(!program.is_empty());
/// assert_eq!(argv.last().map(std::ffi::OsString::as_os_str), Some(Path::new("/tmp/a.pdf").as_os_str()));
/// ```
#[must_use]
pub fn system_opener(file: &Path) -> (String, Vec<std::ffi::OsString>) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer.exe"
    } else {
        "xdg-open"
    };
    (
        program.to_owned(),
        vec![
            std::ffi::OsString::from(program),
            file.as_os_str().to_os_string(),
        ],
    )
}

/// Does the glob `pat` (`text/*` or exact `application/pdf`) match `mime`?
fn mimetype_matches(pat: &str, mime: &str) -> bool {
    match pat.strip_suffix("/*") {
        Some(prefix) => mime.split('/').next() == Some(prefix),
        None => pat == mime,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[cfg(unix)]
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    const SAMPLE: &str = r#"
[[opener]]
mime = "application/pdf"
os = "linux"
command = ["xdg-open", "%f"]

[[opener]]
mime = "text/*"
command = ["bat", "--paging=always", "%f"]

[[opener]]
mime = "text/plain"
os = "macos"
command = ["open", "-t", "%f"]
"#;

    #[test]
    fn parse_empty_and_absent() {
        assert!(OpenersConfig::empty().resolve("text/plain").is_none());
        assert!(OpenersConfig::parse("").unwrap().openers.is_empty());
    }

    #[test]
    fn parse_rejects_empty_command_and_unknown_keys() {
        let empty_cmd = "[[opener]]\nmime = \"text/*\"\ncommand = []\n";
        assert!(matches!(
            OpenersConfig::parse(empty_cmd),
            Err(OpenerError::EmptyCommand { .. })
        ));
        let unknown = "[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\"]\nfoo = 1\n";
        assert!(matches!(
            OpenersConfig::parse(unknown),
            Err(OpenerError::Toml(_))
        ));
    }

    #[test]
    fn resolve_prefers_specific_os_then_agnostic() {
        let cfg = OpenersConfig::parse(SAMPLE).unwrap();
        // On macOS the os=macos entry wins over the agnostic text/*.
        assert_eq!(
            cfg.resolve_for("text/plain", "macos").unwrap().program(),
            "open"
        );
        // On linux there is no specific text/plain: it falls back to the
        // agnostic text/*.
        assert_eq!(
            cfg.resolve_for("text/plain", "linux").unwrap().program(),
            "bat"
        );
        // pdf only exists for linux: on windows it does not resolve.
        assert_eq!(
            cfg.resolve_for("application/pdf", "linux")
                .unwrap()
                .program(),
            "xdg-open"
        );
        assert!(cfg.resolve_for("application/pdf", "windows").is_none());
    }

    #[test]
    fn mimetype_glob() {
        assert!(mimetype_matches("text/*", "text/html"));
        assert!(mimetype_matches("application/pdf", "application/pdf"));
        assert!(!mimetype_matches("text/*", "application/pdf"));
        assert!(!mimetype_matches("text/plain", "text/html"));
    }

    #[test]
    fn argv_substitutes_field_codes() {
        let cfg = OpenersConfig::parse(
            "[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"-n\", \"%f\"]\n",
        )
        .unwrap();
        let o = cfg.resolve_for("text/plain", "linux").unwrap();
        let f = PathBuf::from("/home/u/a.txt");
        let argv = o.argv(&[&f], Path::new("/home/u"));
        assert_eq!(
            argv,
            vec![
                OsString::from("bat"),
                OsString::from("-n"),
                OsString::from("/home/u/a.txt")
            ]
        );
    }

    #[test]
    fn argv_multiple_and_dir() {
        let cfg = OpenersConfig::parse(
            "[[opener]]\nmime = \"text/*\"\ncommand = [\"ls\", \"%F\", \"%d\"]\n",
        )
        .unwrap();
        let o = cfg.resolve_for("text/x", "linux").unwrap();
        let (a, b) = (PathBuf::from("/x/a"), PathBuf::from("/x/b"));
        let argv = o.argv(&[&a, &b], Path::new("/x"));
        assert_eq!(
            argv,
            vec![
                OsString::from("ls"),
                OsString::from("/x/a"),
                OsString::from("/x/b"),
                OsString::from("/x"),
            ]
        );
    }

    /// Hard rule 1: a path with non-UTF-8 bytes survives byte for byte in
    /// the argv — `%f` is a whole token, never concatenated with a literal
    /// nor passed through `String`.
    #[cfg(unix)]
    #[test]
    fn argv_preserves_non_utf8_bytes() {
        let cfg =
            OpenersConfig::parse("[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"%f\"]\n")
                .unwrap();
        let o = cfg.resolve_for("text/x", "linux").unwrap();
        let hostile = PathBuf::from(OsString::from_vec(b"/x/\xff\xfe.txt".to_vec()));
        let argv = o.argv(&[&hostile], Path::new("/x"));
        assert_eq!(argv[1].as_bytes(), b"/x/\xff\xfe.txt");
    }

    #[test]
    fn field_code_with_empty_input_is_omitted() {
        let cfg =
            OpenersConfig::parse("[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"%f\"]\n")
                .unwrap();
        let o = cfg.resolve_for("text/x", "linux").unwrap();
        // With no files: %f is omitted, no literal "%f" remains.
        assert_eq!(o.argv(&[], Path::new("/x")), vec![OsString::from("bat")]);
    }

    #[test]
    fn guess_mime_by_extension() {
        assert_eq!(guess_mime(b"a.txt"), "text/plain");
        assert_eq!(guess_mime(b"a.PDF"), "application/pdf"); // case-insensitive
        assert_eq!(guess_mime(b"a.png"), "image/png");
        assert_eq!(guess_mime(b"sin_ext"), "application/octet-stream");
        // Non-UTF-8 extension: matches nothing.
        assert_eq!(guess_mime(b"a.\xff\xfe"), "application/octet-stream");
        // Non-UTF-8 stem but ASCII extension: the extension governs (byte-split).
        assert_eq!(guess_mime(b"caf\xe9\xff.txt"), "text/plain");
        assert_eq!(guess_mime(b"\xff\xff.png"), "image/png");
    }

    /// The system launcher passes the path as ONE argument of its own,
    /// byte-exact: a non-UTF-8 name or one with shell metacharacters
    /// arrives intact and is never re-interpreted (that is why Windows uses
    /// `explorer.exe` and not `cmd /C start`).
    #[test]
    fn system_opener_passes_the_path_as_a_byte_exact_argument() {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let hostile =
                std::path::PathBuf::from(OsStr::from_bytes(b"/tmp/a & b; rm -rf \xff\xfe.pdf"));
            let (program, argv) = system_opener(&hostile);
            assert_eq!(argv.len(), 2, "binary + file, no shell in between");
            assert_eq!(argv[0], OsString::from(&program));
            assert_eq!(
                argv[1].as_os_str().as_bytes(),
                b"/tmp/a & b; rm -rf \xff\xfe.pdf",
                "the path travels byte for byte"
            );
        }
        let (program, argv) = system_opener(Path::new("/tmp/x.pdf"));
        assert!(!program.is_empty());
        assert_eq!(argv[1], OsString::from("/tmp/x.pdf"));
        // Each platform's own binary, not a made-up one.
        let expected = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(target_os = "windows") {
            "explorer.exe"
        } else {
            "xdg-open"
        };
        assert_eq!(program, expected);
    }

    #[test]
    fn program_available_finds_path_binaries() {
        // `sh` exists on any CI unix; a made-up name does not.
        #[cfg(unix)]
        assert!(program_available(OsStr::new("sh")));
        assert!(!program_available(OsStr::new(
            "norte-binary-that-does-not-exist-xyz"
        )));
    }

    /// A RELATIVE `PATH` entry —`.`, or the empty one that means the same—
    /// resolves NOTHING (#302).
    ///
    /// This is the whole condition behind the hole: the child is launched
    /// with `Command::current_dir` set to the directory the reader is
    /// navigating and, on unix, `current_dir` is applied BEFORE resolving
    /// the program. With a `.` in the `PATH`, a file named `vim` inside a
    /// freshly extracted archive would run when F4 is pressed. Here the
    /// probe looks from the test's cwd, where the executable DOES exist,
    /// and it still says no.
    #[cfg(unix)]
    #[test]
    fn a_relative_path_entry_resolves_nothing() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let evil = dir.path().join("editor-hostil");
        std::fs::write(&evil, b"#!/bin/sh\n").expect("write");
        std::fs::set_permissions(&evil, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let name = OsStr::new("editor-hostil");
        // Absolute: it is found, which is what makes the case below honest.
        assert_eq!(
            resolve_program_in(name, Some(dir.path().as_os_str())).as_deref(),
            Some(evil.as_path())
        );
        // Relative and empty: neither one resolves, even though the file is there.
        let relative: std::path::PathBuf = dir
            .path()
            .file_name()
            .map(|n| std::path::Path::new("..").join(n))
            .expect("has a name");
        for path_var in [OsStr::new("."), OsStr::new(""), relative.as_os_str()] {
            assert_eq!(
                resolve_program_in(name, Some(path_var)),
                None,
                "a relative PATH entry cannot resolve a program"
            );
        }
        // And with no `PATH` there is nowhere to look.
        assert_eq!(resolve_program_in(name, None), None);
    }

    /// The program is taken as BYTES: an editor under a path that is not
    /// UTF-8 resolves the same (hard rule 1).
    #[cfg(unix)]
    #[test]
    fn a_program_with_a_non_utf8_name_resolves() {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let name = OsStr::from_bytes(b"ed\xffitor");
        let path = dir.path().join(name);
        std::fs::write(&path, b"#!/bin/sh\n").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        assert_eq!(
            resolve_program_in(name, Some(dir.path().as_os_str())).as_deref(),
            Some(path.as_path())
        );
    }
}
