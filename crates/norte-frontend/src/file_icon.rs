//! Which of the window's own colored icons a listing row gets (ADR 0184):
//! a kind decided from the entry's kind and its name's RAW bytes, never
//! from the masked display text.
//!
//! The drawings are `assets/file-icons/<id>.svg`, ours, multicolor. A
//! `file-icons` plugin that answers for a row still wins over this.

use norte_proto::EntryKind;

/// Every icon id there is a drawing for, in no particular order. A test
/// pins that each one has its `assets/file-icons/<id>.svg`.
pub const IDS: &[&str] = &[
    "folder", "file", "text", "pdf", "doc", "sheet", "slides", "image", "audio", "video",
    "archive", "code", "script", "config",
];

/// The icon id for an entry: `folder` for a directory, a type from the
/// extension for a file, `file` when nothing matches.
///
/// ```
/// use norte_frontend::file_icon::icon_kind;
/// use norte_proto::EntryKind;
/// assert_eq!(icon_kind(b"informe.PDF", EntryKind::File), "pdf");
/// assert_eq!(icon_kind(b"fotos", EntryKind::Dir), "folder");
/// assert_eq!(icon_kind(b"Makefile", EntryKind::File), "script");
/// assert_eq!(icon_kind(b"sin-extension", EntryKind::File), "file");
/// ```
#[must_use]
pub fn icon_kind(name: &[u8], kind: EntryKind) -> &'static str {
    if kind == EntryKind::Dir {
        return "folder";
    }
    let lower = name.to_ascii_lowercase();
    if let Some((_, id)) = SPECIAL.iter().find(|(n, _)| *n == lower.as_slice()) {
        return id;
    }
    // The LAST dot, and not one that starts the name: `.bashrc` has no
    // extension, `foto.tar.gz` is a `gz`.
    let ext = match lower.iter().rposition(|&b| b == b'.') {
        Some(0) | None => return "file",
        Some(i) => &lower[i + 1..],
    };
    BY_EXTENSION
        .iter()
        .find(|(e, _)| *e == ext)
        .map_or("file", |(_, id)| id)
}

/// Whole names, lowercase, that say what they are without an extension.
const SPECIAL: &[(&[u8], &str)] = &[
    (b"makefile", "script"),
    (b"gnumakefile", "script"),
    (b"justfile", "script"),
    (b"dockerfile", "config"),
    (b"readme", "text"),
    (b"license", "text"),
    (b"licence", "text"),
    (b"copying", "text"),
    (b"changelog", "text"),
];

/// Extensions, lowercase, without the dot. Close to the `file-icons`
/// plugin's table; this one splits `pdf` and plain text out of "doc"
/// because they have a drawing of their own.
const BY_EXTENSION: &[(&[u8], &str)] = &[
    (b"pdf", "pdf"),
    (b"txt", "text"),
    (b"md", "text"),
    (b"markdown", "text"),
    (b"rst", "text"),
    (b"log", "text"),
    (b"tex", "text"),
    (b"doc", "doc"),
    (b"docx", "doc"),
    (b"odt", "doc"),
    (b"rtf", "doc"),
    (b"epub", "doc"),
    (b"pages", "doc"),
    (b"xls", "sheet"),
    (b"xlsx", "sheet"),
    (b"ods", "sheet"),
    (b"csv", "sheet"),
    (b"tsv", "sheet"),
    (b"numbers", "sheet"),
    (b"ppt", "slides"),
    (b"pptx", "slides"),
    (b"odp", "slides"),
    (b"key", "slides"),
    (b"png", "image"),
    (b"jpg", "image"),
    (b"jpeg", "image"),
    (b"gif", "image"),
    (b"webp", "image"),
    (b"svg", "image"),
    (b"bmp", "image"),
    (b"tif", "image"),
    (b"tiff", "image"),
    (b"ico", "image"),
    (b"heic", "image"),
    (b"avif", "image"),
    (b"raw", "image"),
    (b"psd", "image"),
    (b"xcf", "image"),
    (b"mp3", "audio"),
    (b"wav", "audio"),
    (b"flac", "audio"),
    (b"ogg", "audio"),
    (b"oga", "audio"),
    (b"m4a", "audio"),
    (b"opus", "audio"),
    (b"aac", "audio"),
    (b"mp4", "video"),
    (b"mkv", "video"),
    (b"webm", "video"),
    (b"mov", "video"),
    (b"avi", "video"),
    (b"m4v", "video"),
    (b"wmv", "video"),
    (b"zip", "archive"),
    (b"tar", "archive"),
    (b"gz", "archive"),
    (b"tgz", "archive"),
    (b"xz", "archive"),
    (b"txz", "archive"),
    (b"bz2", "archive"),
    (b"zst", "archive"),
    (b"7z", "archive"),
    (b"rar", "archive"),
    (b"deb", "archive"),
    (b"rpm", "archive"),
    (b"jar", "archive"),
    (b"iso", "archive"),
    (b"dmg", "archive"),
    (b"rs", "code"),
    (b"py", "code"),
    (b"js", "code"),
    (b"mjs", "code"),
    (b"ts", "code"),
    (b"jsx", "code"),
    (b"tsx", "code"),
    (b"go", "code"),
    (b"c", "code"),
    (b"h", "code"),
    (b"cpp", "code"),
    (b"hpp", "code"),
    (b"cc", "code"),
    (b"java", "code"),
    (b"kt", "code"),
    (b"rb", "code"),
    (b"lua", "code"),
    (b"php", "code"),
    (b"cs", "code"),
    (b"swift", "code"),
    (b"zig", "code"),
    (b"html", "code"),
    (b"htm", "code"),
    (b"css", "code"),
    (b"scss", "code"),
    (b"vue", "code"),
    (b"svelte", "code"),
    (b"sql", "code"),
    (b"hs", "code"),
    (b"ex", "code"),
    (b"exs", "code"),
    (b"dart", "code"),
    (b"scala", "code"),
    (b"wit", "code"),
    (b"sh", "script"),
    (b"bash", "script"),
    (b"zsh", "script"),
    (b"fish", "script"),
    (b"ps1", "script"),
    (b"bat", "script"),
    (b"cmd", "script"),
    (b"exe", "script"),
    (b"msi", "script"),
    (b"appimage", "script"),
    (b"toml", "config"),
    (b"yaml", "config"),
    (b"yml", "config"),
    (b"json", "config"),
    (b"jsonc", "config"),
    (b"ini", "config"),
    (b"conf", "config"),
    (b"cfg", "config"),
    (b"env", "config"),
    (b"xml", "config"),
    (b"lock", "config"),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Every id the table can answer has a drawing, and every drawing is
    /// reachable: an id without its file would be a blank icon in the
    /// window, a file nobody answers is dead weight.
    #[test]
    fn every_id_has_a_drawing_and_every_drawing_an_id() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/file-icons");
        let answered: std::collections::BTreeSet<&str> = BY_EXTENSION
            .iter()
            .chain(SPECIAL)
            .map(|(_, id)| *id)
            .chain(["folder", "file"])
            .collect();
        for id in IDS {
            assert!(dir.join(format!("{id}.svg")).is_file(), "{id}.svg");
            assert!(answered.contains(id), "{id} is never answered");
        }
        for id in &answered {
            assert!(IDS.contains(id), "{id} is answered but not in IDS");
        }
    }

    #[test]
    fn names_that_are_not_extensions() {
        assert_eq!(icon_kind(b".bashrc", EntryKind::File), "file");
        assert_eq!(icon_kind(b"foto.tar.gz", EntryKind::File), "archive");
        assert_eq!(icon_kind(b"trailing.", EntryKind::File), "file");
        assert_eq!(icon_kind(b"lat\xe9n.pdf", EntryKind::File), "pdf");
        assert_eq!(icon_kind(b"enlace.png", EntryKind::Symlink), "image");
    }
}
