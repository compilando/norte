//! POSIX permissions as seen from a frontend (#314): reading them from a
//! listing, writing them in octal, and reading them back.
//!
//! The rule lives here and not in the terminal because it is the same one
//! in the window, and a decision duplicated between frontends drifts apart
//! silently (ADR 0077).

/// The twelve bits that can be changed: `rwx` for owner, group and others,
/// plus setuid, setgid and sticky. The ones above say what CLASS the node
/// is, and that does not change.
///
/// It is the PROTOCOL's constant, re-exported: two definitions of the same
/// number in two crates is exactly how they drift apart.
pub use norte_proto::methods::MODE_PERMISSION_BITS as PERMISSION_BITS;

/// Why a typed mode is not valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeError {
    /// Empty, or with something that is not an octal digit.
    NotOctal,
    /// It is octal and goes past the twelve bits.
    TooBig,
    /// A mode was requested for directories without asking for recursive
    /// (#315): without going down the tree there are no directories to
    /// apply it to, so this is a request that will not happen and is said
    /// instead of ignored.
    DirModeWithoutRecursive,
}

impl ModeError {
    /// The Fluent key it is said with.
    #[must_use]
    pub const fn message_key(self) -> &'static str {
        match self {
            Self::NotOctal => "msg-chmod-not-octal",
            Self::TooBig => "msg-chmod-too-big",
            Self::DirModeWithoutRecursive => "msg-chmod-dir-mode-needs-recursive",
        }
    }
}

/// Reads a mode typed in OCTAL (`755`, `0644`, `4755`).
///
/// In octal because it is the form a listing shows and the one someone who
/// knows what they want types. Decimal would be a silent trap: `755` in
/// decimal is `0o1363`, a perfectly valid mode and completely different
/// from the one the human had in mind.
///
/// ```
/// use norte_frontend::chmod::{parse_mode, ModeError};
/// assert_eq!(parse_mode("755"), Ok(0o755));
/// assert_eq!(parse_mode("0644"), Ok(0o644));
/// assert_eq!(parse_mode("4755"), Ok(0o4755), "setuid too");
/// assert_eq!(parse_mode("8"), Err(ModeError::NotOctal));
/// assert_eq!(parse_mode("77777"), Err(ModeError::TooBig));
/// ```
///
/// # Errors
///
/// [`ModeError::NotOctal`] if it is empty or has something that is not an
/// octal digit; [`ModeError::TooBig`] if it goes past the twelve bits.
pub fn parse_mode(text: &str) -> Result<u32, ModeError> {
    let t = text.trim();
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit() && b < b'8') {
        return Err(ModeError::NotOctal);
    }
    let n = u32::from_str_radix(t, 8).map_err(|_| ModeError::TooBig)?;
    if n & !PERMISSION_BITS != 0 {
        return Err(ModeError::TooBig);
    }
    Ok(n)
}

/// What a permissions field can ask for (#315): the mode, whether it goes
/// down the tree, and the DIRECTORIES' mode when it is not the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeRequest {
    /// The twelve bits for what is not a directory.
    pub mode: u32,
    /// Go down through the selection's directories.
    pub recursive: bool,
    /// The directories' mode. `None` = the same as [`Self::mode`].
    pub dir_mode: Option<u32>,
}

/// Reads what was typed in the permissions field: `755`, `-R 755`, or
/// `-R 644,755`.
///
/// The grammar is `chmod`'s and not an invented one: `-R` is the flag
/// someone who already knows what they want writes, and that is why no
/// separate key is needed inside a field where every key is text.
///
/// The SECOND mode is the directories' one, and it exists because
/// `chmod -R 644` over a tree leaves it unusable: with no execute bit, a
/// directory cannot even be entered. Without it, the same mode applies to
/// everything — which is what plain `chmod -R` does and what breaks trees,
/// so the dialog's footer says so.
///
/// A directories' mode WITHOUT `-R` is an error and not a value that gets
/// ignored: whoever types it is asking for something that will not
/// happen.
///
/// ```
/// use norte_frontend::chmod::{parse_request, ModeError};
/// let r = parse_request("755").expect("mode");
/// assert_eq!((r.mode, r.recursive, r.dir_mode), (0o755, false, None));
///
/// let r = parse_request("-R 644,755").expect("mode");
/// assert_eq!((r.mode, r.recursive, r.dir_mode), (0o644, true, Some(0o755)));
///
/// // Two modes without `-R` mean nothing.
/// assert_eq!(parse_request("644,755"), Err(ModeError::DirModeWithoutRecursive));
/// ```
///
/// # Errors
///
/// Those of [`parse_mode`], plus [`ModeError::DirModeWithoutRecursive`].
pub fn parse_request(text: &str) -> Result<ModeRequest, ModeError> {
    let t = text.trim();
    let (recursive, rest) = match t.strip_prefix("-R") {
        Some(r) => (true, r.trim_start()),
        None => (false, t),
    };
    let (mode, dir) = match rest.split_once(',') {
        Some((a, b)) => (a, Some(b)),
        None => (rest, None),
    };
    if dir.is_some() && !recursive {
        return Err(ModeError::DirModeWithoutRecursive);
    }
    Ok(ModeRequest {
        mode: parse_mode(mode)?,
        recursive,
        dir_mode: dir.map(parse_mode).transpose()?,
    })
}

/// The mode in four-digit octal, which is how the field is pre-filled.
///
/// ```
/// use norte_frontend::chmod::format_mode;
/// assert_eq!(format_mode(0o755), "0755");
/// assert_eq!(format_mode(0o4755), "4755");
/// ```
#[must_use]
pub fn format_mode(mode: u32) -> String {
    format!("{:04o}", mode & PERMISSION_BITS)
}

/// An entry's POSIX mode, if its listing carries it (#314).
///
/// Comes from the `posix.mode` attribute published by providers that have
/// permissions. `None` = this listing did not request it, or this location
/// does not have them: both things mean the same thing to whoever paints,
/// which is "I don't know", and neither authorizes making up a default
/// `0644`.
#[must_use]
pub fn mode_of(entry: &norte_proto::Entry) -> Option<u32> {
    match entry.attrs.get("posix.mode")? {
        norte_proto::AttrValue::Uint(m) => u32::try_from(*m).ok().map(|m| m & PERMISSION_BITS),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The field's grammar is `chmod`'s (#315), and these are its four
    /// shapes: the bare mode, the recursive one, the recursive one with a
    /// folders mode, and the invalid one.
    #[test]
    fn el_campo_lee_las_formas_de_chmod() {
        let solo = parse_request("755").expect("mode");
        assert_eq!(
            (solo.mode, solo.recursive, solo.dir_mode),
            (0o755, false, None)
        );

        let rec = parse_request("-R 700").expect("mode");
        assert_eq!((rec.mode, rec.recursive, rec.dir_mode), (0o700, true, None));

        let dos = parse_request("-R 644,755").expect("mode");
        assert_eq!(
            (dos.mode, dos.recursive, dos.dir_mode),
            (0o644, true, Some(0o755))
        );

        // A folders mode WITHOUT `-R` is a request that will not happen,
        // and is said instead of ignored.
        assert_eq!(
            parse_request("644,755"),
            Err(ModeError::DirModeWithoutRecursive)
        );
    }

    /// And the mode's errors are still the same in both positions: an
    /// unreadable folders mode is not swallowed.
    #[test]
    fn un_modo_de_carpetas_invalido_no_se_traga() {
        assert_eq!(parse_request("-R 644,8"), Err(ModeError::NotOctal));
        assert_eq!(parse_request("-R 644,77777"), Err(ModeError::TooBig));
        assert_eq!(parse_request("-R"), Err(ModeError::NotOctal), "no mode");
    }

    /// The space after `-R` is not mandatory nor does it have to be one:
    /// what is typed in a field carries whatever spaces it carries.
    #[test]
    fn el_espacio_tras_la_bandera_da_igual() {
        for text in ["-R755", "-R 755", "-R   755", "  -R 755  "] {
            let r = parse_request(text).unwrap_or_else(|e| panic!("{text}: {e:?}"));
            assert_eq!((r.mode, r.recursive), (0o755, true), "{text}");
        }
    }

    /// The trap that justifies octal: `755` read in decimal is a legal and
    /// different mode, so a mistake here would not give an error — it
    /// would give permissions nobody asked for.
    #[test]
    fn se_lee_en_octal_y_no_en_decimal() {
        assert_eq!(parse_mode("755"), Ok(0o755));
        assert_ne!(parse_mode("755"), Ok(755));
    }

    #[test]
    fn el_espacio_alrededor_no_estorba() {
        assert_eq!(parse_mode("  644 "), Ok(0o644));
    }

    #[test]
    fn los_digitos_que_no_son_octales_se_rechazan() {
        for bad in ["8", "9", "75a", "-1", "", "   ", "0x1ff"] {
            assert_eq!(parse_mode(bad), Err(ModeError::NotOctal), "{bad:?}");
        }
    }

    /// The node-class bits are not a permission: `100644` is "regular file
    /// with 644", and setting it whole would be asking to change what
    /// class it is.
    #[test]
    fn los_bits_de_clase_no_caben() {
        assert_eq!(parse_mode("100644"), Err(ModeError::TooBig));
        assert_eq!(parse_mode("10000"), Err(ModeError::TooBig));
    }

    #[test]
    fn ida_y_vuelta() {
        for m in [0o644, 0o755, 0o600, 0o4755, 0o1777, 0] {
            assert_eq!(parse_mode(&format_mode(m)), Ok(m));
        }
    }

    #[test]
    fn el_modo_sale_del_atributo_del_listado() {
        let mut e = norte_proto::Entry {
            attrs: std::collections::BTreeMap::new(),
            path: norte_proto::VPath::parse("file:///a").expect("wire"),
            kind: norte_proto::EntryKind::File,
            size: None,
            mtime_ms: None,
        };
        assert_eq!(mode_of(&e), None, "without the attribute, it is not known");
        // Whole `st_mode`: the class bits get trimmed off on read, because
        // what can be WRITTEN is the twelve at the bottom.
        e.attrs.insert(
            "posix.mode".to_owned(),
            norte_proto::AttrValue::Uint(0o100_644),
        );
        assert_eq!(mode_of(&e), Some(0o644));
    }
}
