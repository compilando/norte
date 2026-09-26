//! `VPath` ↔ native path conversion and the `\\?\` prefix (paths >260,
//! reserved names, trailing dots/spaces).
//!
//! Lives HERE and not in the local provider because these are rules about
//! a path's SHAPE, not disk access, and two frontends need them without
//! wanting a provider: the terminal, to hand a path to a shell tool, and
//! the graphical window, to know where it starts. Having them in
//! `norte-vfs-local` would force both to drag along the project's only
//! crate allowed to use `unsafe`, with `openat2` and `ConfinedRoot`
//! inside, into a process whose only transport is a socket — exactly the
//! opposite of what ADR 0066 promises (#254). They don't belong in
//! `norte-proto` either: that's the wire, and a system `Path` never
//! crosses any cable.
//!
//! Security boundary (ADR 0001): on Windows a segment's bytes are
//! validated as WTF-8 and DECODED to UTF-16 (`OsStringExt::from_wide`) —
//! zero `unsafe`: unchecked reconstruction of `OsStr` stays forbidden
//! because its contract ("bytes from `as_encoded_bytes` of the same Rust
//! version") doesn't cover bytes that arrived over the wire.

#[cfg(unix)]
use std::ffi::OsStr;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use norte_proto::{Error, VPath};

/// Raw bytes of an `OsStr` (the shape `Segment` stores).
///
/// Unix: the OS's bytes as they are. Windows: WTF-8 (`as_encoded_bytes`).
///
/// A thin re-export of [`crate::wtf8::os_to_bytes`], which is where the
/// conversion has lived since `norte-core::volumes::windows` needed it for
/// `Volume::label`. Now this module is its neighbor and the re-export just
/// saves spelling out the path in the two spots below.
pub(crate) use crate::wtf8::os_to_bytes;

/// Rebuilds an `OsString` from a segment's bytes.
///
/// # Errors
/// [`Error::InvalidPath`] on Windows if the bytes aren't valid WTF-8
/// (impossible as a Windows file name; also, unchecked reconstruction
/// would be unsound).
#[cfg(unix)]
pub fn bytes_to_os(bytes: &[u8]) -> Result<OsString, Error> {
    use std::os::unix::ffi::OsStrExt;
    // Unix: any byte is valid in a name; safe 1:1 conversion.
    Ok(OsStr::from_bytes(bytes).to_os_string())
}

/// Rebuilds an `OsString` from a segment's bytes (Windows: WTF-8 validated
/// → UTF-16 → `from_wide`, no `unsafe`).
///
/// # Errors
/// [`Error::InvalidPath`] if the bytes aren't valid WTF-8, or if they
/// contain `\` (a separator even under `\\?\`: one segment would produce
/// TWO components) or `:` (NTFS Alternate Data Stream: the data would end
/// up hidden in a stream `list` never returns).
#[cfg(windows)]
pub fn bytes_to_os(bytes: &[u8]) -> Result<OsString, Error> {
    use std::os::windows::ffi::OsStringExt;
    if bytes.contains(&b'\\') || bytes.contains(&b':') {
        return Err(Error::InvalidPath);
    }
    let wide = crate::wtf8::decode_to_wide(bytes).ok_or(Error::InvalidPath)?;
    Ok(OsString::from_wide(&wide))
}

/// A symlink's target → `OsString`. Unix: bytes as they are. The target is
/// NOT a segment: `bytes_to_os`'s restrictions don't apply to it.
///
/// # Errors
/// [`Error::InvalidPath`] if the bytes aren't representable as a system
/// path (on Windows, if they aren't valid WTF-8).
#[cfg(unix)]
pub fn link_target_to_os(bytes: &[u8]) -> Result<OsString, Error> {
    use std::os::unix::ffi::OsStrExt;
    Ok(OsStr::from_bytes(bytes).to_os_string())
}

/// A symlink's target → `OsString` (Windows): WTF-8 validated, WITHOUT the
/// segment restrictions — a legitimate target contains `\` and `:`.
///
/// # Errors
/// [`Error::InvalidPath`] if the bytes aren't valid WTF-8.
#[cfg(windows)]
pub fn link_target_to_os(bytes: &[u8]) -> Result<OsString, Error> {
    use std::os::windows::ffi::OsStringExt;
    let wide = crate::wtf8::decode_to_wide(bytes).ok_or(Error::InvalidPath)?;
    Ok(OsString::from_wide(&wide))
}

/// `p`'s native path under `base`: `base/<seg1>/<seg2>/…`.
///
/// On Windows the result ALWAYS carries the verbatim `\\?\` prefix (paths
/// longer than 260, `CON`/`NUL`, trailing dots/spaces kept intact).
/// Windows special case: an empty `base` = "OS root" — the FIRST segment
/// is the drive prefix (`C:`) and its separator is restored to it (avoids
/// the drive-relative path `C:Users` a naive `push` would produce).
///
/// # Errors
/// [`Error::InvalidPath`] if some segment isn't natively representable.
pub fn to_native(base: &Path, p: &VPath) -> Result<PathBuf, Error> {
    let mut segs = p.segments();
    let mut out = if cfg!(windows) && base.as_os_str().is_empty() {
        let Some(first) = segs.next() else {
            return Err(Error::InvalidPath);
        };
        // The first segment is the OS root's prefix: a drive (`C:`) or
        // UNC/verbatim (`\\server\share`, `\\?\…`). It doesn't go through
        // bytes_to_os (which rejects `\`/`:` as a separator/ADS in names):
        // the prefix is the only place where they're legal.
        os_root_base(first)?
    } else {
        base.to_path_buf()
    };
    for seg in segs {
        out.push(bytes_to_os(seg)?);
    }
    Ok(verbatim(out))
}

/// Base `PathBuf` of the Windows OS root from the `VPath`'s first segment:
/// a drive `X:` (with its separator restored, avoiding the drive-relative
/// path `C:Users`) or a UNC/verbatim prefix `\\…` (#22 — a `\\server\share`
/// cwd no longer aborts startup; [`verbatim`] later normalizes it to
/// `\\?\UNC\…`). The prefix is rebuilt WITHOUT the segment restrictions
/// because it legitimately contains `\` and `:`.
fn os_root_base(bytes: &[u8]) -> Result<PathBuf, Error> {
    if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let s = std::str::from_utf8(bytes).map_err(|_| Error::InvalidPath)?;
        let mut drive = OsString::from(s);
        drive.push(std::path::MAIN_SEPARATOR_STR);
        return Ok(PathBuf::from(drive));
    }
    if is_bare_windows_prefix(bytes) {
        // WTF-8 → OsString WITHOUT the segment restrictions (the prefix
        // legitimately carries `\` and `:`); reuses
        // `link_target_to_os`'s unrestricted decoder. The SHAPE was
        // already validated by `is_bare_windows_prefix` (a single Prefix
        // component, not a fat one).
        return Ok(PathBuf::from(link_target_to_os(bytes)?));
    }
    Err(Error::InvalidPath)
}

/// `true` if the bytes are EXACTLY a "bare" Windows root prefix (a single
/// `Prefix` component, with NO path tail): UNC `\\server\share`,
/// verbatim-disk `\\?\C:` or verbatim-UNC `\\?\UNC\server\share`. A
/// byte-level recognizer — compiled and tested on every OS; on Windows
/// these bytes come from `Component::Prefix::as_os_str`.
///
/// Rejects on purpose (stricter than `std`, review #22):
/// - the DEVICE namespace `\\.\…` (raw disk/pipe I/O, not navigation — a
///   least-privilege regression),
/// - a FAT first segment with a tail (`\\?\C:\Windows\…`): would expand
///   into several native components, skipping `bytes_to_os`'s
///   per-segment guard (which rejects `\`/`:`),
/// - odd verbatim forms (Volume GUID): fail-closed to `InvalidPath`,
///   never a malformed native path.
fn is_bare_windows_prefix(bytes: &[u8]) -> bool {
    if let Some(rest) = bytes.strip_prefix(br"\\?\") {
        // Verbatim-UNC `\\?\UNC\server\share`.
        if let Some(unc) = rest.strip_prefix(br"UNC\") {
            return is_bare_unc_body(unc);
        }
        // Verbatim-disk `\\?\C:` — drive letter + `:`, nothing else.
        return rest.len() == 2 && rest[0].is_ascii_alphabetic() && rest[1] == b':';
    }
    if let Some(unc) = bytes.strip_prefix(br"\\") {
        return is_bare_unc_body(unc);
    }
    false
}

/// `server\share` with both non-empty and NO more `\` (exactly two
/// components). `server` can't be `.` (device) nor `?` (verbatim marker):
/// those go through other branches or are rejected.
fn is_bare_unc_body(body: &[u8]) -> bool {
    let mut parts = body.split(|&b| b == b'\\');
    let (Some(server), Some(share), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !server.is_empty() && !share.is_empty() && server != b"." && server != b"?"
}

/// Converts a `file://` `VPath` (no authority) to its NATIVE path — the
/// inverse of [`vpath_from_native`]. The base is the OS root (same as
/// `LocalProvider::os_root`): `/` on unix; on Windows the drive/UNC
/// carried in the first segment. Byte for byte (rule 1).
///
/// Use: a frontend that needs the real path to launch an external program
/// (opener, #28) over a local file.
///
/// # Errors
/// [`Error::InvalidPath`] if the scheme isn't `file`, if it carries an
/// authority (it's from ANOTHER provider), or if some segment isn't
/// natively representable.
///
/// ```
/// use norte_proto::{Scheme, Segment, VPath};
/// let vp = VPath::root(Scheme::new("file").unwrap(), None)
///     .join(Segment::new(b"etc".to_vec()).unwrap())
///     .join(Segment::new(b"hosts".to_vec()).unwrap());
/// # #[cfg(unix)]
/// assert_eq!(
///     norte_vfs::native::vpath_to_native(&vp).unwrap(),
///     std::path::Path::new("/etc/hosts")
/// );
/// ```
pub fn vpath_to_native(p: &VPath) -> Result<PathBuf, Error> {
    if p.scheme() != "file" || p.authority().is_some() {
        return Err(Error::InvalidPath);
    }
    // Same base as `LocalProvider::os_root`: empty on Windows (the first
    // segment is the drive/UNC), `/` on unix.
    let base = if cfg!(windows) {
        PathBuf::new()
    } else {
        PathBuf::from("/")
    };
    to_native(&base, p)
}

/// Converts an absolute NATIVE path to `VPath` (`file:///…`), byte for byte.
/// The inverse of `LocalProvider::os_root`'s resolution.
///
/// # Errors
/// [`Error::InvalidPath`] if the path can't be normalized or contains
/// components not representable as segments.
///
/// # Panics
/// Never: the `file` scheme is constant and valid.
pub fn vpath_from_native(path: &Path) -> Result<VPath, Error> {
    use norte_proto::{Scheme, Segment};
    let abs = std::path::absolute(path).map_err(|_| Error::InvalidPath)?;
    let mut out = VPath::root(Scheme::new("file").expect("constant, valid scheme"), None);
    for comp in abs.components() {
        use std::path::Component;
        match comp {
            Component::RootDir => {}
            Component::Prefix(pr) => {
                // Windows: the drive (`C:`) or the UNC travels as the first segment.
                let seg =
                    Segment::new(os_to_bytes(pr.as_os_str())).map_err(|_| Error::InvalidPath)?;
                out = out.join(seg);
            }
            Component::Normal(os) => {
                let seg = Segment::new(os_to_bytes(os)).map_err(|_| Error::InvalidPath)?;
                out = out.join(seg);
            }
            // `absolute` doesn't resolve `..` against the FS but does
            // fold it lexically on Windows; on unix it can survive: rejected.
            Component::CurDir | Component::ParentDir => return Err(Error::InvalidPath),
        }
    }
    Ok(out)
}

/// Applies the verbatim prefix on Windows; identity elsewhere.
#[cfg(not(windows))]
#[must_use]
pub fn verbatim(p: PathBuf) -> PathBuf {
    p
}

/// Applies the verbatim prefix on Windows; identity elsewhere.
#[cfg(windows)]
#[must_use]
pub fn verbatim(p: PathBuf) -> PathBuf {
    use std::path::{Component, Prefix};
    // Already verbatim: don't touch.
    if let Some(Component::Prefix(pr)) = p.components().next() {
        match pr.kind() {
            Prefix::Verbatim(_) | Prefix::VerbatimUNC(..) | Prefix::VerbatimDisk(_) => return p,
            Prefix::UNC(server, share) => {
                // \\server\share\… → \\?\UNC\server\share\…
                let mut out = PathBuf::from(r"\\?\UNC");
                out.push(server);
                out.push(share);
                for c in p.components() {
                    match c {
                        Component::Prefix(_) | Component::RootDir => {}
                        other => out.push(other.as_os_str()),
                    }
                }
                return out;
            }
            _ => {}
        }
    }
    let mut s = OsString::from(r"\\?\");
    s.push(p.as_os_str());
    PathBuf::from(s)
}

#[cfg(test)]
mod root_base_tests {
    use super::{is_bare_windows_prefix, os_root_base};
    use norte_proto::Error;

    #[test]
    fn accepts_only_bare_unc_and_verbatim_prefixes() {
        // "Bare" UNC and verbatim (a single Prefix component): accepted.
        assert!(is_bare_windows_prefix(br"\\server\share"));
        assert!(is_bare_windows_prefix(br"\\wsl$\Ubuntu")); // \\wsl$ from the issue
        assert!(is_bare_windows_prefix(br"\\?\C:"));
        assert!(is_bare_windows_prefix(br"\\?\UNC\server\share"));
    }

    #[test]
    fn rejects_device_fat_and_malformed() {
        // Device namespace: raw I/O, NOT navigation (review #22).
        assert!(!is_bare_windows_prefix(br"\\.\PhysicalDrive0"));
        assert!(!is_bare_windows_prefix(br"\\.\C:"));
        // A FAT first segment with a tail: would skip the per-segment guard.
        assert!(!is_bare_windows_prefix(br"\\?\C:\Windows"));
        assert!(!is_bare_windows_prefix(br"\\server\share\dir"));
        // Incomplete / malformed UNC.
        assert!(!is_bare_windows_prefix(br"\\server"));
        assert!(!is_bare_windows_prefix(br"\\"));
        assert!(!is_bare_windows_prefix(br"\single"));
        assert!(!is_bare_windows_prefix(b"C:"));
        assert!(!is_bare_windows_prefix(b"normal"));
    }

    #[test]
    fn os_root_base_accepts_drive_and_bare_unc() {
        // Drive: accepted, with its separator restored.
        let drive = os_root_base(b"C:").expect("valid drive");
        assert!(drive.to_string_lossy().starts_with("C:"));
        // #22: a bare UNC prefix is no longer rejected (before = no startup).
        assert!(os_root_base(br"\\server\share").is_ok());
        assert!(os_root_base(br"\\?\C:").is_ok());
        // But a fat one/device IS rejected (least privilege).
        assert_eq!(
            os_root_base(br"\\.\PhysicalDrive0"),
            Err(Error::InvalidPath)
        );
        assert_eq!(os_root_base(br"\\?\C:\Windows"), Err(Error::InvalidPath));
    }

    /// #28 encoding: a name with NON-UTF8 bytes survives byte for byte
    /// through the native → `vpath_to_native` → native (unix) round trip.
    /// Fixture-level guard of `vpath_from_native`'s inverse.
    #[cfg(unix)]
    #[test]
    fn vpath_to_native_round_trip_bytes_no_utf8() {
        use super::vpath_to_native;
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        use std::path::Path;
        let native = Path::new(OsStr::from_bytes(b"/x/\xff\xfe.txt"));
        let vpath = super::vpath_from_native(native).expect("vpath");
        let back = vpath_to_native(&vpath).expect("native");
        assert_eq!(back.as_os_str().as_bytes(), b"/x/\xff\xfe.txt");
    }

    #[test]
    fn os_root_base_rejects_a_first_segment_that_is_not_a_prefix() {
        // Neither a drive nor UNC: an ordinary name as the OS root is InvalidPath.
        assert_eq!(os_root_base(b"Users"), Err(Error::InvalidPath));
        assert_eq!(os_root_base(b"C"), Err(Error::InvalidPath));
        assert_eq!(os_root_base(b""), Err(Error::InvalidPath));
    }

    /// #22: a real native round trip of a UNC cwd (Windows ONLY: `to_native`
    /// under an empty root is a `cfg!(windows)` path, and the
    /// `Component::Prefix` classification is Windows semantics). Blocks the
    /// fix when CI runs on Windows; on Linux this test's body doesn't compile.
    #[cfg(windows)]
    #[test]
    fn unc_cwd_round_trips_byte_exact() {
        use super::{to_native, vpath_from_native};
        use std::path::Path;
        let native = Path::new(r"\\server\share\dir");
        let vpath = vpath_from_native(native).expect("vpath from UNC");
        // The first segment is the bare prefix, `dir` is separate.
        let back = to_native(Path::new(""), &vpath).expect("to_native UNC");
        // verbatim() canonicalizes UNC → \\?\UNC\server\share\dir (same file).
        assert_eq!(back, Path::new(r"\\?\UNC\server\share\dir"));
    }
}
