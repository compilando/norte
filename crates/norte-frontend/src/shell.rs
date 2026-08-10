//! Shell integration — the PURE half (design
//! `docs/superpowers/specs/2026-08-10-shell-integration-design.md`).
//!
//! Nothing here touches a terminal or spawns anything, so all of it is
//! unit-testable without a tty. This module carries `--pick` (§B): the
//! picker's byte-exact output; and `cd_bytes`/`Shell` (§C): what goes in the
//! `--cd-file` and the wrapper text `norte shell-init` prints.
//! `login_shell`/`terminal_argv` (§D/§E) land in a later task of the same
//! plan.

/// The picker's output: every path's bytes, each followed by a NUL.
///
/// A local (`file://`) path comes out in NATIVE form (`/tmp/a`), because
/// that is what the tool on the other side of the pipe will open — a
/// `norte-vfs-local::vpath_to_native` failure (not `file://`, or an
/// authority that makes it someone else's provider) falls back to the wire
/// form, which is the only lossless thing to say about a location that has
/// no native path at all.
///
/// NUL-terminated, not NUL-separated: a single result is unambiguous on its
/// own and `xargs -0` is happy either way. The bytes are the path's,
/// untouched — a name is bytes (rule 1) and a picker that lossily decodes is
/// a picker that opens the wrong file.
#[must_use]
pub fn pick_bytes(paths: &[norte_proto::VPath]) -> Vec<u8> {
    let mut out = Vec::new();
    for p in paths {
        match norte_vfs_local::vpath_to_native(p) {
            #[cfg(unix)]
            Ok(native) => {
                use std::os::unix::ffi::OsStrExt;
                out.extend_from_slice(native.as_os_str().as_bytes());
            }
            // Windows has no byte-exact `OsStr` accessor: a picker that
            // round-trips arbitrary bytes on that platform is not part of
            // this item (design §B says so explicitly).
            #[cfg(not(unix))]
            Ok(native) => {
                out.extend_from_slice(native.to_string_lossy().into_owned().as_bytes());
            }
            Err(_) => out.extend_from_slice(p.to_wire().as_bytes()),
        }
        out.push(0);
    }
    out
}

/// What to write into the `--cd-file`, or `None` when the pane is not local.
///
/// `Some` is the directory's native bytes with a trailing NUL. `None` means
/// write nothing at all — an empty file tells the wrapper to leave the shell
/// where it is, and a norte that died mid-write can therefore never move a
/// shell to half a path. Whether a pane is local IS whether
/// `norte_vfs_local::vpath_to_native` accepts it: `file://` with no
/// authority, exactly the same test `pick_bytes` uses for its native/wire
/// split — so a `sftp://`/`s3://` pane, or a `file://` one with an
/// authority, is `None` here too.
///
/// Unlike [`pick_bytes`] there is no wire-form fallback: a shell can only
/// `cd` into a real directory on disk, and a wire form is not one.
#[must_use]
pub fn cd_bytes(dir: &norte_proto::VPath) -> Option<Vec<u8>> {
    let native = norte_vfs_local::vpath_to_native(dir).ok()?;
    #[cfg(unix)]
    let mut bytes = {
        use std::os::unix::ffi::OsStrExt;
        native.as_os_str().as_bytes().to_vec()
    };
    // Same Windows caveat as `pick_bytes`: no byte-exact `OsStr` accessor, so
    // this is lossy there. A Windows picker/cd is not part of this item.
    #[cfg(not(unix))]
    let mut bytes = native.to_string_lossy().into_owned().into_bytes();
    bytes.push(0);
    Some(bytes)
}

/// A shell we can emit a cd-on-quit wrapper for (`norte shell-init`, §C).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    /// bash.
    Bash,
    /// zsh.
    Zsh,
    /// fish.
    Fish,
}

/// bash/zsh's wrapper, verbatim (design §C, ONE deviation from its literal
/// text — see below). Reads the cd-file with `IFS= read -r -d ''`, never
/// `$(cat …)` — command substitution cannot carry a NUL and strips trailing
/// newlines, corrupting any directory name that ends in one. Invokes
/// `command ntc`, never bare `ntc`: this function is itself bound to the name
/// `ntc`, so a bare call would recurse forever.
///
/// The design's draft named the exit-code variable `status`, which zsh
/// reserves (it aliases `$status` to `$?` itself): `local status=$?` fails
/// there with "read-only variable: status", caught by actually running zsh
/// against this wrapper rather than only reading the shell code. Renamed to
/// `rc` for both shells, so bash and zsh keep sharing one wrapper body.
const BASH_ZSH_WRAPPER: &str = "\
ntc() {
    local f
    f=\"$(mktemp \"${TMPDIR:-/tmp}/ntc-cd.XXXXXX\")\" || return 1
    command ntc --cd-file \"$f\" \"$@\"
    local rc=$?
    local dir
    IFS= read -r -d '' dir < \"$f\"
    rm -f -- \"$f\"
    if [ -n \"$dir\" ]; then
        cd -- \"$dir\" || return $?
    fi
    return $rc
}
";

/// fish's wrapper, verbatim (design §C). `string split0` is fish's NUL-safe
/// read, the equivalent of bash/zsh's `read -d ''` above; same
/// `command ntc` rule against recursion.
const FISH_WRAPPER: &str = "\
function ntc
    set -l f (mktemp (test -n \"$TMPDIR\"; and echo $TMPDIR; or echo /tmp)/ntc-cd.XXXXXX)
    or return 1
    command ntc --cd-file $f $argv
    set -l status_code $status
    set -l dir (string split0 < $f)
    rm -f -- $f
    if test -n \"$dir[1]\"
        cd -- $dir[1]
    end
    return $status_code
end
";

impl Shell {
    /// `"bash"`/`"zsh"`/`"fish"`, else `None`. What `norte shell-init` and
    /// `norte doctor` parse; case-sensitive on purpose — a shell name is not
    /// user prose, it is one of exactly three tokens a rc file will pass
    /// verbatim.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            "fish" => Some(Self::Fish),
            _ => None,
        }
    }

    /// The wrapper's source, for `eval` (bash/zsh) or `source` (fish).
    #[must_use]
    pub fn wrapper(self) -> &'static str {
        match self {
            Self::Bash | Self::Zsh => BASH_ZSH_WRAPPER,
            Self::Fish => FISH_WRAPPER,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::VPath;

    /// NUL-TERMINATED, not NUL-separated: one result is unambiguous and
    /// `xargs -0` is happy either way. The bytes are the path's, untouched —
    /// a name is bytes (rule 1) and a picker that lossily decodes is a picker
    /// that opens the wrong file.
    #[test]
    fn pick_bytes_terminates_every_path_with_nul() {
        let a = VPath::parse("file:///tmp/a").unwrap();
        let b = VPath::parse("file:///tmp/b").unwrap();
        assert_eq!(pick_bytes(&[a, b]), b"/tmp/a\0/tmp/b\0".to_vec());
    }

    /// A remote path has no native form, so what comes out is the wire form —
    /// which is what the tool asked norte to pick.
    #[test]
    fn pick_bytes_of_a_remote_path_is_its_wire_form() {
        let p = VPath::parse("sftp://host/x").unwrap();
        assert_eq!(pick_bytes(&[p]), b"sftp://host/x\0".to_vec());
    }

    /// Nothing selected is an empty output, never a stray NUL: a consumer
    /// that reads one empty record would open the current directory.
    #[test]
    fn pick_bytes_of_nothing_is_nothing() {
        assert!(pick_bytes(&[]).is_empty());
    }

    /// A directory is bytes and the file is NUL-delimited, so a name ending
    /// in a newline survives — `$(...)` would eat it, which is why the
    /// wrapper reads a file instead.
    #[test]
    fn cd_bytes_are_the_raw_directory_plus_a_nul() {
        let p = VPath::parse("file:///tmp/we%0Aird").unwrap();
        assert_eq!(cd_bytes(&p), Some(b"/tmp/we\nird\0".to_vec()));
    }

    /// Not `file://` writes NOTHING: there is no local cwd that corresponds
    /// to `sftp://host/x`, and an empty file is the wrapper's signal to leave
    /// the shell where it is.
    #[test]
    fn cd_bytes_of_a_remote_pane_are_nothing() {
        assert_eq!(cd_bytes(&VPath::parse("sftp://host/x").unwrap()), None);
        assert_eq!(cd_bytes(&VPath::parse("s3://b/k").unwrap()), None);
    }

    /// The wrapper must call the BINARY, not itself. A function named `ntc`
    /// that runs `ntc` is infinite recursion, and it is the one way this can
    /// fail catastrophically — so it is pinned for all three shells.
    #[test]
    fn every_wrapper_calls_the_binary_not_the_function() {
        for sh in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let w = sh.wrapper();
            assert!(w.contains("command ntc"), "{sh:?} recurses: {w}");
        }
    }

    /// No wrapper may read the cd-file with command substitution: it cannot
    /// carry a NUL and it strips trailing newlines.
    #[test]
    fn no_wrapper_uses_command_substitution_on_the_cd_file() {
        for sh in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let w = sh.wrapper();
            assert!(!w.contains("$(cat"), "{sh:?} uses $(cat …)");
            assert!(!w.contains("(cat "), "{sh:?} uses (cat …)");
        }
    }

    /// `Shell::parse` accepts exactly the three names `shell-init`/`doctor`
    /// document, and nothing else — not a capitalised variant, not a path.
    #[test]
    fn shell_parse_is_exact_and_closed() {
        assert_eq!(Shell::parse("bash"), Some(Shell::Bash));
        assert_eq!(Shell::parse("zsh"), Some(Shell::Zsh));
        assert_eq!(Shell::parse("fish"), Some(Shell::Fish));
        assert_eq!(Shell::parse("Bash"), None);
        assert_eq!(Shell::parse("sh"), None);
        assert_eq!(Shell::parse(""), None);
    }
}
