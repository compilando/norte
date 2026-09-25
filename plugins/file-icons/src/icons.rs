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
    /// One Nerd Font glyph per class (private-use codepoints from the
    /// Font Awesome and Devicons ranges, stable since Nerd Fonts v2). One
    /// cell wide. The window bundles the glyphs it needs; a terminal needs a
    /// patched font, or it paints a box.
    Nerd,
    /// The Seti UI set, the file icons VSCode shows by default (spec
    /// 2026-09-11, F4): one glyph per LANGUAGE where `Nerd` has one per
    /// class, so `main.py` and `main.go` look different. The glyphs are the
    /// `nf-seti-*` range of the same Nerd font (MIT), so the window needs no
    /// second font and a patched terminal font already has them. Seti has
    /// no link or slides icon; those two borrow `Nerd`'s.
    Seti,
}

impl Style {
    /// Every style, for tests that sweep the table.
    pub const ALL: &'static [Style] = &[Style::Emoji, Style::Ascii, Style::Nerd, Style::Seti];
}

/// What the host says an entry is. A name cannot tell a folder from a
/// file, so this comes from the listing, not from the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    File,
    Dir,
    Symlink,
}

/// A kind of file, as far as a name can tell — plus the two the host tells
/// us about, `Folder` and `Link`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Folder,
    Link,
    Rust,
    Code,
    Script,
    Doc,
    Sheet,
    Slides,
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
            (Kind::Folder, Style::Emoji) => "📁",
            (Kind::Folder, Style::Ascii) => "/",
            (Kind::Link, Style::Emoji) => "🔗",
            (Kind::Link, Style::Ascii) => "@",
            (Kind::Sheet, Style::Emoji) => "📊",
            // Every emoji here is wide ON ITS OWN. `📽`, `🖼` and `⚙` are
            // text presentation and need VS16 to be wide, and terminals
            // disagree on a VS16 cell: the row painted one cell off (#374).
            (Kind::Slides, Style::Emoji) => "📈",
            (Kind::Sheet | Kind::Slides, Style::Ascii) => "''",
            (Kind::Rust, Style::Emoji) => "🦀",
            (Kind::Code, Style::Emoji) => "💻",
            (Kind::Rust | Kind::Code, Style::Ascii) => "{}",
            (Kind::Script, Style::Emoji) => "⚡",
            (Kind::Script, Style::Ascii) => "$",
            (Kind::Doc, Style::Emoji) => "📄",
            (Kind::Readme, Style::Emoji) => "📖",
            (Kind::Licence, Style::Emoji) => "📜",
            (Kind::Doc | Kind::Readme | Kind::Licence, Style::Ascii) => "''",
            (Kind::Image, Style::Emoji) => "📷",
            (Kind::Image, Style::Ascii) => "%",
            (Kind::Audio, Style::Emoji) => "🎵",
            (Kind::Audio, Style::Ascii) => "~",
            (Kind::Video, Style::Emoji) => "🎬",
            (Kind::Video, Style::Ascii) => ">",
            (Kind::Archive, Style::Emoji) => "📦",
            (Kind::Archive, Style::Ascii) => "[]",
            (Kind::Config, Style::Emoji) => "🔩",
            (Kind::Build, Style::Emoji) => "🔧",
            (Kind::Git, Style::Emoji) => "🐙",
            (Kind::Container, Style::Emoji) => "🐳",
            (Kind::Config | Kind::Build | Kind::Git | Kind::Container, Style::Ascii) => "#",
            // Nerd Fonts: `nf-fa-*` (U+F000–F2E0) and `nf-dev-*` (U+E700–E7C5),
            // the two ranges v3 did not move. The window's subset font
            // (`ui/src/fonts/`) carries exactly these eighteen glyphs: add
            // one here and it has to be added there too.
            (Kind::Folder, Style::Nerd) => "\u{f07b}",
            (Kind::Link, Style::Nerd) => "\u{f0c1}",
            (Kind::Rust, Style::Nerd) => "\u{e7a8}",
            (Kind::Code, Style::Nerd) => "\u{f121}",
            (Kind::Script, Style::Nerd) => "\u{f120}",
            (Kind::Doc, Style::Nerd) => "\u{f0f6}",
            (Kind::Sheet, Style::Nerd) => "\u{f0ce}",
            (Kind::Slides, Style::Nerd) => "\u{f1c4}",
            (Kind::Readme, Style::Nerd) => "\u{f02d}",
            (Kind::Image, Style::Nerd) => "\u{f1c5}",
            (Kind::Audio, Style::Nerd) => "\u{f001}",
            (Kind::Video, Style::Nerd) => "\u{f008}",
            (Kind::Archive, Style::Nerd) => "\u{f1c6}",
            (Kind::Config, Style::Nerd) => "\u{f013}",
            (Kind::Build, Style::Nerd) => "\u{f0ad}",
            (Kind::Git, Style::Nerd) => "\u{e702}",
            (Kind::Container, Style::Nerd) => "\u{e7b0}",
            (Kind::Licence, Style::Nerd) => "\u{f24e}",
            // Seti (`nf-seti-*`, U+E5FA–E6B7). The subset font carries these
            // too: add one here, add it there. `Code` is only the fallback
            // for a language `SETI_BY_EXTENSION` does not name.
            (Kind::Folder, Style::Seti) => "\u{e613}",
            (Kind::Link, Style::Seti) => "\u{f0c1}",
            (Kind::Rust, Style::Seti) => "\u{e68b}",
            (Kind::Code, Style::Seti) => "\u{e64e}",
            (Kind::Script, Style::Seti) => "\u{e691}",
            (Kind::Doc, Style::Seti) => "\u{e64e}",
            (Kind::Sheet, Style::Seti) => "\u{e6a6}",
            (Kind::Slides, Style::Seti) => "\u{f1c4}",
            (Kind::Readme, Style::Seti) => "\u{e66a}",
            (Kind::Image, Style::Seti) => "\u{e60d}",
            (Kind::Audio, Style::Seti) => "\u{e638}",
            (Kind::Video, Style::Seti) => "\u{e69f}",
            (Kind::Archive, Style::Seti) => "\u{e6aa}",
            (Kind::Config, Style::Seti) => "\u{e615}",
            (Kind::Build, Style::Seti) => "\u{e673}",
            (Kind::Git, Style::Seti) => "\u{e65d}",
            (Kind::Container, Style::Seti) => "\u{e650}",
            (Kind::Licence, Style::Seti) => "\u{e60a}",
        }
    }

    /// Every kind, for tests that sweep the table.
    pub const ALL: &'static [Kind] = &[
        Kind::Folder,
        Kind::Link,
        Kind::Rust,
        Kind::Code,
        Kind::Script,
        Kind::Doc,
        Kind::Sheet,
        Kind::Slides,
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
    (b"rtf", Kind::Doc),
    (b"xlsx", Kind::Sheet),
    (b"xls", Kind::Sheet),
    (b"ods", Kind::Sheet),
    (b"csv", Kind::Sheet),
    (b"tsv", Kind::Sheet),
    (b"pptx", Kind::Slides),
    (b"ppt", Kind::Slides),
    (b"odp", Kind::Slides),
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
    // Code that only `Seti` tells apart; the other styles call it code.
    (b"html", Kind::Code),
    (b"htm", Kind::Code),
    (b"css", Kind::Code),
    (b"scss", Kind::Code),
    (b"sass", Kind::Code),
    (b"vue", Kind::Code),
    (b"svelte", Kind::Code),
    (b"hs", Kind::Code),
    (b"ex", Kind::Code),
    (b"exs", Kind::Code),
    (b"dart", Kind::Code),
    (b"scala", Kind::Code),
    (b"clj", Kind::Code),
    (b"ml", Kind::Code),
    (b"jl", Kind::Code),
    (b"nim", Kind::Code),
    (b"cr", Kind::Code),
    (b"tf", Kind::Code),
    (b"xml", Kind::Config),
    (b"tex", Kind::Doc),
];

/// `Seti` only: one glyph per language, consulted before the kind. Every
/// extension here is also in [`BY_EXTENSION`], so no style shows an icon
/// for a name the others leave blank.
const SETI_BY_EXTENSION: &[(&[u8], &str)] = &[
    (b"py", "\u{e606}"),
    (b"js", "\u{e60c}"),
    (b"jsx", "\u{e625}"),
    (b"tsx", "\u{e625}"),
    (b"ts", "\u{e628}"),
    (b"go", "\u{e627}"),
    (b"c", "\u{e649}"),
    (b"h", "\u{e649}"),
    (b"cpp", "\u{e646}"),
    (b"hpp", "\u{e646}"),
    (b"cc", "\u{e646}"),
    (b"java", "\u{e66d}"),
    (b"kt", "\u{e634}"),
    (b"rb", "\u{e605}"),
    (b"lua", "\u{e620}"),
    (b"php", "\u{e608}"),
    (b"cs", "\u{e648}"),
    (b"swift", "\u{e699}"),
    (b"zig", "\u{e6a9}"),
    (b"ps1", "\u{e683}"),
    (b"md", "\u{e609}"),
    (b"markdown", "\u{e609}"),
    (b"json", "\u{e60b}"),
    (b"yml", "\u{e6a8}"),
    (b"yaml", "\u{e6a8}"),
    (b"csv", "\u{e64a}"),
    (b"tsv", "\u{e64a}"),
    (b"pdf", "\u{e67d}"),
    (b"doc", "\u{e6a5}"),
    (b"docx", "\u{e6a5}"),
    (b"odt", "\u{e6a5}"),
    (b"svg", "\u{e698}"),
    (b"html", "\u{e60e}"),
    (b"htm", "\u{e60e}"),
    (b"css", "\u{e614}"),
    (b"scss", "\u{e603}"),
    (b"sass", "\u{e603}"),
    (b"vue", "\u{e6a0}"),
    (b"svelte", "\u{e697}"),
    (b"hs", "\u{e61f}"),
    (b"ex", "\u{e62d}"),
    (b"exs", "\u{e62d}"),
    (b"dart", "\u{e64c}"),
    (b"scala", "\u{e68e}"),
    (b"clj", "\u{e642}"),
    (b"ml", "\u{e67a}"),
    (b"jl", "\u{e624}"),
    (b"nim", "\u{e677}"),
    (b"cr", "\u{e62f}"),
    (b"tf", "\u{e69a}"),
    (b"xml", "\u{e619}"),
    (b"tex", "\u{e69b}"),
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

/// The badge for a name in a style, or `None`: what a name alone says.
pub fn badge_for(name: &[u8], style: Style) -> Option<&'static str> {
    let kind = kind_of(name);
    // A name that means something on its own (`Cargo.toml`, `README.md`)
    // keeps that meaning in Seti too: the language table only refines what
    // the extension alone would have said. `Readme`/`Licence` can only come
    // from the name, never from `BY_EXTENSION` (a test pins that).
    let whole_name = matches!(kind, Some(Kind::Readme | Kind::Licence))
        || SPECIAL.iter().any(|(n, _)| eq_ignore_ascii_case(n, name));
    if style == Style::Seti && !whole_name {
        let seti = extension_of(name).and_then(|ext| {
            SETI_BY_EXTENSION
                .iter()
                .find(|(e, _)| eq_ignore_ascii_case(e, ext))
                .map(|(_, g)| *g)
        });
        if seti.is_some() {
            return seti;
        }
    }
    kind.map(|k| k.glyph(style))
}

/// The icon for an entry: the whole plugin, as a pure function.
///
/// A folder is a folder before its name says anything — except the few
/// names that mean more (`.git` is the git icon, not a plain folder) — and
/// a link is a link whatever it points at: the listing shows the link, not
/// the target, and the icon says what the row IS.
pub fn icon_for(name: &[u8], class: Class, style: Style) -> Option<&'static str> {
    match class {
        Class::Dir => Some(
            SPECIAL
                .iter()
                .find(|(n, _)| eq_ignore_ascii_case(n, name))
                .map_or(Kind::Folder, |(_, k)| *k)
                .glyph(style),
        ),
        Class::Symlink => Some(Kind::Link.glyph(style)),
        Class::File => badge_for(name, style),
    }
}

/// [`icon_for`] with the two user overrides from `[config]`:
///
/// - `dir_icon`, when not empty, replaces the glyph of a PLAIN folder — the
///   few folders whose name means more (`.git`) keep theirs;
/// - `unknown`, when not empty, is the glyph for a file whose name says
///   nothing, so every row gets one and the column reads as a column.
///
/// Both are the user's text: the host masks and caps them like any badge.
pub fn icon_with<'a>(
    name: &[u8],
    class: Class,
    style: Style,
    dir_icon: &'a str,
    unknown: &'a str,
) -> Option<&'a str> {
    match (class, icon_for(name, class, style)) {
        (Class::Dir, Some(g)) if !dir_icon.is_empty() && g == Kind::Folder.glyph(style) => {
            Some(dir_icon)
        }
        (Class::File, None) if !unknown.is_empty() => Some(unknown),
        (_, found) => found,
    }
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
    fn a_folder_is_a_folder_before_its_name_and_a_link_is_a_link() {
        assert_eq!(icon_for(b"src", Class::Dir, Style::Emoji), Some("📁"));
        assert_eq!(
            icon_for(b"main.rs", Class::Dir, Style::Emoji),
            Some("📁"),
            "a folder called main.rs is still a folder"
        );
        assert_eq!(
            icon_for(b".git", Class::Dir, Style::Emoji),
            Some("🐙"),
            "the few names that mean more than «folder»"
        );
        assert_eq!(icon_for(b"link", Class::Symlink, Style::Emoji), Some("🔗"));
        assert_eq!(
            icon_for(b"main.rs", Class::File, Style::Emoji),
            Some("🦀"),
            "a file goes by its name"
        );
        assert_eq!(icon_for(b"x", Class::File, Style::Emoji), None);
        assert_eq!(icon_for(b"src", Class::Dir, Style::Ascii), Some("/"));
        assert_eq!(icon_for(b"l", Class::Symlink, Style::Ascii), Some("@"));
    }

    #[test]
    fn office_files_have_their_own_icons() {
        assert_eq!(badge_for(b"cuentas.xlsx", Style::Emoji), Some("📊"));
        assert_eq!(badge_for(b"datos.csv", Style::Emoji), Some("📊"));
        assert_eq!(badge_for(b"charla.pptx", Style::Emoji), Some("📈"));
        assert_eq!(badge_for(b"informe.docx", Style::Emoji), Some("📄"));
    }

    #[test]
    fn names_map_to_kinds() {
        assert_eq!(badge_for(b"main.rs", Style::Emoji), Some("🦀"));
        assert_eq!(
            badge_for(b"Cargo.toml", Style::Emoji),
            Some("🦀"),
            "special before .toml"
        );
        assert_eq!(badge_for(b"config.toml", Style::Emoji), Some("🔩"));
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
            Some("📷"),
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
            for style in Style::ALL {
                let g = k.glyph(*style);
                assert!(!g.is_empty());
                assert!(g.chars().count() <= 2, "{g:?}");
                assert!(!g.chars().any(char::is_control), "{g:?}");
            }
        }
    }

    /// Every emoji is ONE codepoint that is wide on its own (#374).
    ///
    /// A text-presentation emoji made wide by VS16 (`🖼\u{FE0F}`) is the
    /// case terminals disagree on: some widen the cell they already drew
    /// when the selector arrives, and the row painted one cell off until its
    /// next repaint (`fjord.jpg g`, `Caféde Flore.jpg`).
    #[test]
    fn every_emoji_is_one_natively_wide_codepoint() {
        use unicode_width::UnicodeWidthStr;
        for k in Kind::ALL {
            let g = k.glyph(Style::Emoji);
            assert_eq!(g.chars().count(), 1, "{k:?}: {g:?} is a sequence");
            assert_eq!(g.width(), 2, "{k:?}: {g:?} is not wide on its own");
        }
    }

    /// Every Nerd glyph is ONE private-use codepoint, one cell wide, and
    /// from one of the two ranges Nerd Fonts v3 kept in place — anything
    /// else would paint a box in a terminal with an older patched font.
    #[test]
    fn every_nerd_glyph_is_one_stable_private_use_codepoint() {
        use unicode_width::UnicodeWidthStr;
        for k in Kind::ALL {
            let g = k.glyph(Style::Nerd);
            let mut chars = g.chars();
            let c = chars.next().expect("a glyph") as u32;
            assert!(chars.next().is_none(), "{k:?}: one codepoint");
            assert!(
                (0xE700..=0xE7C5).contains(&c) || (0xF000..=0xF2E0).contains(&c),
                "{k:?}: U+{c:04X} is outside nf-dev / nf-fa"
            );
            assert_eq!(g.width(), 1, "{k:?}");
        }
    }

    /// Every glyph the Seti style can answer — per kind and per language —
    /// is ONE private-use codepoint, one cell wide, from `nf-seti` or (for
    /// the two Seti lacks) `nf-fa`.
    #[test]
    fn every_seti_glyph_is_one_private_use_codepoint() {
        use unicode_width::UnicodeWidthStr;
        let per_kind = Kind::ALL.iter().map(|k| k.glyph(Style::Seti));
        let per_language = SETI_BY_EXTENSION.iter().map(|(_, g)| *g);
        for g in per_kind.chain(per_language) {
            let mut chars = g.chars();
            let c = chars.next().expect("a glyph") as u32;
            assert!(chars.next().is_none(), "{g:?}: one codepoint");
            assert!(
                (0xE5FA..=0xE6B7).contains(&c) || (0xF000..=0xF2E0).contains(&c),
                "U+{c:04X} is outside nf-seti / nf-fa"
            );
            assert_eq!(g.width(), 1, "U+{c:04X}");
        }
    }

    /// The language table never offers an icon the others leave blank, and
    /// lists each extension once, in lowercase.
    #[test]
    fn the_seti_table_refines_known_extensions_only() {
        for (i, (ext, _)) in SETI_BY_EXTENSION.iter().enumerate() {
            assert!(
                BY_EXTENSION.iter().any(|(e, _)| e == ext),
                "{}: not in BY_EXTENSION",
                String::from_utf8_lossy(ext)
            );
            assert!(!ext.iter().any(u8::is_ascii_uppercase));
            assert!(
                !SETI_BY_EXTENSION[i + 1..].iter().any(|(e, _)| e == ext),
                "{}: listed twice",
                String::from_utf8_lossy(ext)
            );
        }
    }

    /// `badge_for` reads `Readme`/`Licence` as "the whole name spoke": true
    /// only while no extension maps to them.
    #[test]
    fn no_extension_means_readme_or_licence() {
        assert!(
            !BY_EXTENSION
                .iter()
                .any(|(_, k)| matches!(k, Kind::Readme | Kind::Licence))
        );
    }

    #[test]
    fn seti_tells_languages_apart_and_keeps_special_names() {
        let py = badge_for(b"main.py", Style::Seti);
        let go = badge_for(b"main.go", Style::Seti);
        assert_ne!(py, go, "one glyph per language");
        assert_eq!(
            badge_for(b"main.py", Style::Nerd),
            badge_for(b"main.go", Style::Nerd),
            "while Nerd has one per class"
        );
        assert_eq!(badge_for(b"README.md", Style::Seti), Some("\u{e66a}"));
        assert_eq!(badge_for(b"Cargo.toml", Style::Seti), Some("\u{e68b}"));
        assert_eq!(badge_for(b"PAGE.HTML", Style::Seti), Some("\u{e60e}"));
        assert_eq!(badge_for(b"notes.txt", Style::Seti), Some("\u{e64e}"));
        assert_eq!(icon_for(b"src", Class::Dir, Style::Seti), Some("\u{e613}"));
    }

    #[test]
    fn the_overrides_replace_only_the_plain_folder_and_the_unknown_file() {
        let plain = icon_with(b"src", Class::Dir, Style::Ascii, "»", "?");
        assert_eq!(plain, Some("»"), "a plain folder takes the user's glyph");
        let git = icon_with(b".git", Class::Dir, Style::Ascii, "»", "?");
        assert_eq!(git, Some("#"), "a folder that means more keeps its own");
        assert_eq!(
            icon_with(b"x", Class::File, Style::Ascii, "»", "?"),
            Some("?"),
            "a file the table does not know gets the unknown glyph"
        );
        assert_eq!(
            icon_with(b"x", Class::File, Style::Ascii, "»", ""),
            None,
            "and none when the override is empty"
        );
        assert_eq!(
            icon_with(b"main.rs", Class::File, Style::Ascii, "»", "?"),
            Some("{}"),
            "a known file is not touched"
        );
        assert_eq!(
            icon_with(b"src", Class::Dir, Style::Ascii, "", "?"),
            Some("/"),
            "an empty dir_icon means the style's own"
        );
    }

    /// Every emoji glyph measures TWO cells with the measure the terminal
    /// frontend uses (`unicode-width`): a glyph that measures one and paints
    /// two — a text-presentation emoji without VS16 — breaks the icon column
    /// on exactly its rows. The ASCII ones may be one or two.
    #[test]
    fn every_emoji_glyph_measures_two_cells() {
        use unicode_width::UnicodeWidthStr;
        for k in Kind::ALL {
            assert_eq!(k.glyph(Style::Emoji).width(), 2, "{k:?}");
            let w = k.glyph(Style::Ascii).width();
            assert!(w == 1 || w == 2, "{k:?}: {w}");
        }
    }

    #[test]
    fn the_tables_have_no_repeated_key() {
        let mut ext: Vec<&[u8]> = BY_EXTENSION.iter().map(|(e, _)| *e).collect();
        let n = ext.len();
        ext.sort_unstable();
        ext.dedup();
        assert_eq!(ext.len(), n, "an extension listed twice");
        let mut names: Vec<&[u8]> = SPECIAL.iter().map(|(e, _)| *e).collect();
        let n = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), n, "a special name listed twice");
    }
}
