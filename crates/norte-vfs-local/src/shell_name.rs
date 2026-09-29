//! The Win32 name the shell is handed for a verbatim path, or a refusal
//! (#25).
//!
//! The shell parses only Win32 names, and stripping `\\?\` hands the name to
//! Win32's normalisation: `foo.` becomes `foo`, a segment `...` becomes its
//! PARENT, `NUL.txt` becomes a device. Each of those would recycle another
//! object than the one looked at. Nothing is rewritten: a name whose Win32
//! reading could differ from its verbatim one is refused with `Unsupported`,
//! which leaves the permanent delete (verbatim, through std) to the user.
//!
//! Platform-neutral on purpose, so its tests run on every gate.

use norte_proto::Error;

/// Longest name the shell parses: `MAX_PATH` minus the NUL.
const MAX_NAME: usize = 259;

/// NUL-terminated `X:\…` for the shell, from a `\\?\X:\…` (or `X:\…`) path.
pub(crate) fn shell_name(wide: &[u16]) -> Result<Vec<u16>, Error> {
    let verbatim: Vec<u16> = r"\\?\".encode_utf16().collect();
    let name = wide.strip_prefix(verbatim.as_slice()).unwrap_or(wide);
    let refuse = Err(Error::Unsupported);

    let [drive, colon, sep, rest @ ..] = name else {
        return refuse;
    };
    let is_letter = u8::try_from(*drive).is_ok_and(|b| b.is_ascii_alphabetic());
    if !is_letter || *colon != u16::from(b':') || *sep != u16::from(b'\\') || rest.is_empty() {
        return refuse;
    }
    if name.len() > MAX_NAME || char::decode_utf16(name.iter().copied()).any(|c| c.is_err()) {
        return refuse;
    }
    if rest
        .split(|&u| u == u16::from(b'\\'))
        .any(|c| !plain_component(c))
    {
        return refuse;
    }
    Ok(name.iter().copied().chain(std::iter::once(0)).collect())
}

/// A component Win32 reads exactly as NTFS stores it.
fn plain_component(c: &[u16]) -> bool {
    let Some(&last) = c.last() else {
        return false;
    };
    if last == u16::from(b'.') || last == u16::from(b' ') {
        return false;
    }
    if c.iter()
        .any(|&u| u < 0x20 || br#"<>"|?*/:"#.iter().any(|&b| u == u16::from(b)))
    {
        return false;
    }
    !is_device(c)
}

/// `CON`, `NUL.txt`, `com¹`, `LPT3 .log`…: the stem, before the first dot
/// and without trailing spaces, is a DOS device.
fn is_device(c: &[u16]) -> bool {
    let stem = c.split(|&u| u == u16::from(b'.')).next().unwrap_or(c);
    let stem = String::from_utf16_lossy(stem);
    let stem = stem.trim_end_matches(' ').to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    let (Some(head), Some(tail)) = (stem.get(..3), stem.get(3..)) else {
        return false;
    };
    (head == "COM" || head == "LPT")
        && tail.chars().count() == 1
        && tail
            .chars()
            .all(|d| d.is_ascii_digit() || matches!(d, '¹' | '²' | '³'))
}

#[cfg(test)]
mod tests {
    use super::shell_name;
    use norte_proto::Error;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn refused(wide: &[u16]) -> bool {
        matches!(shell_name(wide), Err(Error::Unsupported))
    }

    #[test]
    fn a_plain_drive_path_loses_only_the_prefix() {
        let got = shell_name(&w(r"\\?\C:\Users\me\notes.txt")).expect("plain");
        assert_eq!(got, w("C:\\Users\\me\\notes.txt\0"));
        assert!(shell_name(&w(r"D:\a b\c")).is_ok());
    }

    /// Each of these names a DIFFERENT object once Win32 reads it.
    #[test]
    fn names_win32_would_normalise_are_refused() {
        for name in [
            r"\\?\C:\d\foo.",
            r"\\?\C:\d\foo ",
            r"\\?\C:\d\...",
            r"\\?\C:\d\. .",
            r"\\?\C:\d\ ",
            r"\\?\C:\d\CON",
            r"\\?\C:\d\nul.txt",
            r"\\?\C:\d\Com1.log",
            r"\\?\C:\d\LPT9",
            r"\\?\C:\d\COM¹",
            r"\\?\C:\d\aux .x",
            r"\\?\C:\d\conin$",
            r"\\?\C:\d\a*b",
            r"\\?\C:\d\a?b",
            r"\\?\C:\d\a<b",
            r"\\?\C:\d\a:stream",
            r"\\?\C:\d\\x",
            r"\\?\C:\d\",
        ] {
            assert!(refused(&w(name)), "{name}");
        }
    }

    /// The device check is on the stem, not a prefix.
    #[test]
    fn names_that_only_look_like_devices_pass() {
        for name in [
            r"\\?\C:\d\CONSOLE",
            r"\\?\C:\d\nul_x.txt",
            r"\\?\C:\d\COM10",
            r"\\?\C:\d\x.CON",
        ] {
            assert!(shell_name(&w(name)).is_ok(), "{name}");
        }
    }

    /// Anything that is not `X:\…` would resolve against the cwd or a
    /// namespace the shell reads differently.
    #[test]
    fn shapes_other_than_a_drive_path_are_refused() {
        for name in [
            r"\\?\UNC\srv\share\x",
            r"\\?\unc\srv\share\x",
            r"\\?\Volume{0b1c}\x",
            r"\\?\GLOBALROOT\Device\x",
            r"\\?\rel\x",
            r"\\.\C:\x",
            r"\\srv\share\x",
            r"rel\x",
            r"C:x",
            r"C:\",
            r"1:\x",
        ] {
            assert!(refused(&w(name)), "{name}");
        }
    }

    #[test]
    fn too_long_and_unpaired_surrogates_are_refused() {
        let long = format!(r"\\?\C:\{}", "a".repeat(260));
        assert!(refused(&w(&long)));
        let fits = format!(r"C:\{}", "a".repeat(256));
        assert!(shell_name(&w(&fits)).is_ok());

        let mut surrogate = w(r"\\?\C:\d\x");
        surrogate.push(0xD800);
        assert!(refused(&surrogate));
        let mut ctrl = w(r"\\?\C:\d\x");
        ctrl.push(0x1F);
        assert!(refused(&ctrl));
    }
}
