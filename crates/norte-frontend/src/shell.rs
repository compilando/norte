//! Shell integration — the PURE half (design
//! `docs/superpowers/specs/2026-08-10-shell-integration-design.md`).
//!
//! Nothing here touches a terminal or spawns anything, so all of it is
//! unit-testable without a tty. This module currently carries `--pick`
//! (§B): the picker's byte-exact output. `cd_bytes`/`Shell` (§C) and
//! `login_shell`/`terminal_argv` (§D/§E) land in later tasks of the same
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
}
