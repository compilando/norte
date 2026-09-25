//! Colors by file TYPE (`LS_COLORS` style, ADR 0020 D2): by `kind`
//! (dir/symlink/exec…) and by EXTENSION. Resolution is extension > kind >
//! `regular` role.

use std::collections::HashMap;

use serde::Deserialize;

use crate::style::Style;

/// Filesystem node type, used to color the entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum FileKind {
    /// Directory.
    Dir,
    /// Symlink.
    Symlink,
    /// Executable regular file.
    Executable,
    /// FIFO / named pipe.
    Fifo,
    /// Socket.
    Socket,
    /// Block device.
    BlockDevice,
    /// Character device.
    CharDevice,
    /// Regular file (no special distinction).
    Regular,
}

impl FileKind {
    /// The classes a theme can color in `[files.kind]`, in the order they
    /// are written. `Regular` is absent: a regular file is the `regular`
    /// role, not a class.
    pub const ALL: &'static [FileKind] = &[
        FileKind::Dir,
        FileKind::Symlink,
        FileKind::Executable,
        FileKind::Fifo,
        FileKind::Socket,
        FileKind::BlockDevice,
        FileKind::CharDevice,
    ];

    /// The kebab key it is written with in `[files.kind]`.
    ///
    /// ```
    /// use norte_theme::FileKind;
    /// assert_eq!(FileKind::BlockDevice.as_kebab(), "block-device");
    /// ```
    #[must_use]
    pub const fn as_kebab(self) -> &'static str {
        match self {
            Self::Dir => "dir",
            Self::Symlink => "symlink",
            Self::Executable => "executable",
            Self::Fifo => "fifo",
            Self::Socket => "socket",
            Self::BlockDevice => "block-device",
            Self::CharDevice => "char-device",
            Self::Regular => "regular",
        }
    }
}

/// Styles by file type. `[files.kind]` colors by class; `[files.ext]`
/// by extension (more specific, it wins).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FileColors {
    /// By node class.
    pub kind: HashMap<FileKind, Style>,
    /// By extension, in ASCII lowercase (the lookup lowercases too).
    pub ext: HashMap<String, Style>,
}

impl FileColors {
    /// The [`Style`] for an entry `name` (bytes, rule 1) of type `kind`, or
    /// `None` if the theme does not color it (the caller falls back to the `regular` role).
    /// Priority: extension > kind.
    #[must_use]
    pub fn style_for(&self, name: &[u8], kind: FileKind) -> Option<Style> {
        if let Some(ext) = extension_of(name) {
            // ASCII lowercase for lenient matching; a non-UTF8 byte in the
            // extension simply matches no key (falls back to kind).
            if let Ok(ext_str) = std::str::from_utf8(ext) {
                let key = ext_str.to_ascii_lowercase();
                if let Some(s) = self.ext.get(&key) {
                    return Some(*s);
                }
            }
        }
        self.kind.get(&kind).copied()
    }
}

/// The extension of `name` = bytes after the LAST `.`, if there is one and it
/// is not a hidden file without extension (`.bashrc` has no `bashrc` extension).
#[must_use]
pub fn extension_of(name: &[u8]) -> Option<&[u8]> {
    let dot = name.iter().rposition(|&b| b == b'.')?;
    // A `.` at position 0 (hidden) or at the end (no ext) does not count.
    if dot == 0 || dot + 1 == name.len() {
        return None;
    }
    Some(&name[dot + 1..])
}
