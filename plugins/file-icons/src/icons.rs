//! The table: from a name, in bytes, to a badge.
//!
//! Special names first (`Cargo.toml` is Rust before it is TOML), then the
//! extension. The extension is the tail after the LAST dot, and a name that
//! starts with a dot and has no other dot — `.bashrc` — has no extension:
//! the same rule `mark.extension` and the rename template use in norte, on
//! purpose, so the three agree on what "the extension" of a name is.
//!
//! Everything is done on bytes: a name is not text (a listing can carry
//! `caf\xff.md`, and it is still a document). Comparison is ASCII
//! case-insensitive, so `PHOTO.JPG` is an image.

/// Which glyph set to answer with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// One emoji per class. Two cells wide in most terminals.
    Emoji,
    /// One to two ASCII characters per class, for fonts without emoji.
    Ascii,
}

/// A kind of file, as far as a name can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Rust,
    Code,
    Script,
    Doc,
    Readme,
    Image,
    Audio,
    Video,
    Archive,
    Config,
    Build,
    Git,
    Container,
    Licence,
}

impl Kind {
    /// The badge for this kind, in the given style. Every glyph is at most
    /// two characters: the host caps a badge at eight after masking, and a
    /// badge is painted next to a file name, where short is the point.
    pub const fn glyph(self, style: Style) -> &'static str {
        match (self, style) {
            (Kind::Rust, Style::Emoji) => "🦀",
            (Kind::Code, Style::Emoji) => "💻",
            (Kind::Rust | Kind::Code, Style::Ascii) => "{}",
            (Kind::Script, Style::Emoji) => "⚡",
            (Kind::Script, Style::Ascii) => "$",
            (Kind::Doc, Style::Emoji) => "📄",
            (Kind::Readme, Style::Emoji) => "📖",
            (Kind::Licence, Style::Emoji) => "📜",
            (Kind::Doc | Kind::Readme | Kind::Licence, Style::Ascii) => "''",
            (Kind::Image, Style::Emoji) => "🖼",
            (Kind::Image, Style::Ascii) => "%",
            (Kind::Audio, Style::Emoji) => "🎵",
            (Kind::Audio, Style::Ascii) => "~",
            (Kind::Video, Style::Emoji) => "🎬",
            (Kind::Video, Style::Ascii) => ">",
            (Kind::Archive, Style::Emoji) => "📦",
            (Kind::Archive, Style::Ascii) => "[]",
            (Kind::Config, Style::Emoji) => "⚙",
            (Kind::Build, Style::Emoji) => "🔧",
            (Kind::Git, Style::Emoji) => "🐙",
            (Kind::Container, Style::Emoji) => "🐳",
            (Kind::Config | Kind::Build | Kind::Git | Kind::Container, Style::Ascii) => "#",
        }
    }

    /// Every kind, for tests that sweep the table.
    pub const ALL: &'static [Kind] = &[
        Kind::Rust,
        Kind::Code,
        Kind::Script,
        Kind::Doc,
        Kind::Readme,
        Kind::Image,
        Kind::Audio,
        Kind::Video,
        Kind::Archive,
        Kind::Config,
        Kind::Build,
        Kind::Git,
        Kind::Container,
        Kind::Licence,
    ];
}

/// Names that mean something on their own, matched whole and
/// case-insensitively, before any extension.
const SPECIAL: &[(&[u8], Kind)] = &[
    (b"cargo.toml", Kind::Rust),
    (b"cargo.lock", Kind::Rust),
    (b"makefile", Kind::Build),
    (b"gnumakefile", Kind::Build),
    (b"justfile", Kind::Build),
    (b"cmakelists.txt", Kind::Build),
    (b"build.gradle", Kind::Build),
    (b".gitignore", Kind::Git),
    (b".gitattributes", Kind::Git),
    (b".gitmodules", Kind::Git),
    (b".git", Kind::Git),
    (b"dockerfile", Kind::Container),
    (b"docker-compose.yml", Kind::Container),
    (b"docker-compose.yaml", Kind::Container),
    (b"license", Kind::Licence),
    (b"licence", Kind::Licence),
    (b"copying", Kind::Licence),
    (b"readme", Kind::Readme),
];

/// Extensions, lowercase, without the dot.
const BY_EXTENSION: &[(&[u8], Kind)] = &[
    (b"rs", Kind::Rust),
    (b"py", Kind::Code),
    (b"js", Kind::Code),
    (b"ts", Kind::Code),
    (b"jsx", Kind::Code),
    (b"tsx", Kind::Code),
    (b"go", Kind::Code),
    (b"c", Kind::Code),
    (b"h", Kind::Code),
    (b"cpp", Kind::Code),
    (b"hpp", Kind::Code),
    (b"cc", Kind::Code),
    (b"java", Kind::Code),
    (b"kt", Kind::Code),
    (b"rb", Kind::Code),
    (b"lua", Kind::Code),
    (b"php", Kind::Code),
    (b"cs", Kind::Code),
    (b"swift", Kind::Code),
    (b"zig", Kind::Code),
    (b"sh", Kind::Script),
    (b"bash", Kind::Script),
    (b"zsh", Kind::Script),
    (b"fish", Kind::Script),
    (b"ps1", Kind::Script),
    (b"bat", Kind::Script),
    (b"md", Kind::Doc),
    (b"markdown", Kind::Doc),
    (b"txt", Kind::Doc),
    (b"rst", Kind::Doc),
    (b"pdf", Kind::Doc),
    (b"doc", Kind::Doc),
    (b"docx", Kind::Doc),
    (b"odt", Kind::Doc),
    (b"epub", Kind::Doc),
    (b"png", Kind::Image),
    (b"jpg", Kind::Image),
    (b"jpeg", Kind::Image),
    (b"gif", Kind::Image),
    (b"webp", Kind::Image),
    (b"svg", Kind::Image),
    (b"bmp", Kind::Image),
    (b"tiff", Kind::Image),
    (b"ico", Kind::Image),
    (b"mp3", Kind::Audio),
    (b"wav", Kind::Audio),
    (b"flac", Kind::Audio),
    (b"ogg", Kind::Audio),
    (b"m4a", Kind::Audio),
    (b"opus", Kind::Audio),
    (b"mp4", Kind::Video),
    (b"mkv", Kind::Video),
    (b"webm", Kind::Video),
    (b"mov", Kind::Video),
    (b"avi", Kind::Video),
    (b"zip", Kind::Archive),
    (b"tar", Kind::Archive),
    (b"gz", Kind::Archive),
    (b"tgz", Kind::Archive),
    (b"xz", Kind::Archive),
    (b"bz2", Kind::Archive),
    (b"zst", Kind::Archive),
    (b"7z", Kind::Archive),
    (b"rar", Kind::Archive),
    (b"toml", Kind::Config),
    (b"yaml", Kind::Config),
    (b"yml", Kind::Config),
    (b"json", Kind::Config),
    (b"ini", Kind::Config),
    (b"conf", Kind::Config),
    (b"cfg", Kind::Config),
    (b"env", Kind::Config),
];

/// The extension of a base name, in bytes and without the dot; `None` if it
/// has none. Last dot; a leading dot alone does not count.
pub fn extension_of(name: &[u8]) -> Option<&[u8]> {
    match name.iter().rposition(|b| *b == b'.') {
        Some(0) | None => None,
        Some(i) => Some(&name[i + 1..]),
    }
}

fn eq_ignore_ascii_case(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
}

/// What kind of file a name says it is, or `None` if it says nothing.
pub fn kind_of(name: &[u8]) -> Option<Kind> {
    if let Some((_, k)) = SPECIAL.iter().find(|(n, _)| eq_ignore_ascii_case(n, name)) {
        return Some(*k);
    }
    // `README.md`, `LICENSE.txt`: the stem says more than the extension.
    if let Some(i) = name.iter().rposition(|b| *b == b'.') {
        if i > 0 {
            let stem = &name[..i];
            if eq_ignore_ascii_case(stem, b"readme") {
                return Some(Kind::Readme);
            }
            if eq_ignore_ascii_case(stem, b"license") || eq_ignore_ascii_case(stem, b"licence") {
                return Some(Kind::Licence);
            }
        }
    }
    let ext = extension_of(name)?;
    BY_EXTENSION
        .iter()
        .find(|(e, _)| eq_ignore_ascii_case(e, ext))
        .map(|(_, k)| *k)
}

/// The badge for a name in a style, or `None`: the whole plugin, as a pure
/// function.
pub fn badge_for(name: &[u8], style: Style) -> Option<&'static str> {
    kind_of(name).map(|k| k.glyph(style))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_extension_is_the_last_dot_and_a_dotfile_has_none() {
        assert_eq!(extension_of(b"archive.tar.gz"), Some(&b"gz"[..]));
        assert_eq!(extension_of(b".bashrc"), None);
        assert_eq!(extension_of(b"x"), None);
        assert_eq!(extension_of(b"a."), Some(&b""[..]));
    }

    #[test]
    fn names_map_to_kinds() {
        assert_eq!(badge_for(b"main.rs", Style::Emoji), Some("🦀"));
        assert_eq!(
            badge_for(b"Cargo.toml", Style::Emoji),
            Some("🦀"),
            "special before .toml"
        );
        assert_eq!(badge_for(b"config.toml", Style::Emoji), Some("⚙"));
        assert_eq!(
            badge_for(b".bashrc", Style::Emoji),
            None,
            "a dotfile has no extension"
        );
        assert_eq!(
            badge_for(b"archive.tar.gz", Style::Emoji),
            Some("📦"),
            "last dot"
        );
        assert_eq!(
            badge_for(b"PHOTO.JPG", Style::Emoji),
            Some("🖼"),
            "case-insensitive"
        );
        assert_eq!(
            badge_for(b"caf\xff.md", Style::Emoji),
            Some("📄"),
            "bytes, not text"
        );
        assert_eq!(badge_for(b"README", Style::Emoji), Some("📖"));
        assert_eq!(
            badge_for(b"README.md", Style::Emoji),
            Some("📖"),
            "the stem wins"
        );
        assert_eq!(badge_for(b"LICENSE.txt", Style::Emoji), Some("📜"));
        assert_eq!(badge_for(b"song.mp3", Style::Emoji), Some("🎵"));
        assert_eq!(badge_for(b"x", Style::Emoji), None);
        assert_eq!(badge_for(b"", Style::Emoji), None);
        assert_eq!(badge_for(b"main.rs", Style::Ascii), Some("{}"));
        assert_eq!(badge_for(b"deploy.sh", Style::Ascii), Some("$"));
    }

    #[test]
    fn every_glyph_is_short_and_printable() {
        for k in Kind::ALL {
            for style in [Style::Emoji, Style::Ascii] {
                let g = k.glyph(style);
                assert!(!g.is_empty());
                assert!(g.chars().count() <= 2, "{g:?}");
                assert!(!g.chars().any(char::is_control), "{g:?}");
            }
        }
    }
}
