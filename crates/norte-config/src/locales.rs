//! Extra interface languages (ADR 0188): one `<tag>.ftl` per language under
//! `<config>/locales/`. This module only READS them; `norte-i18n` decides
//! what a file may install.

use std::io::Read;
use std::path::Path;

/// The directory, under the user config dir.
pub const DIR: &str = "locales";

/// What [`read_locales`] found: `(tag, source)` pairs, and one line per file
/// it skipped, for the log.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Found {
    /// File stem and contents, sorted by stem.
    pub sources: Vec<(String, String)>,
    /// Why each skipped file was skipped.
    pub skipped: Vec<String>,
}

/// Reads `dir`'s `.ftl` files: at most `max_files`, each at most `max_bytes`.
/// A missing directory is no languages, not an error.
///
/// **This blocks**: callers on a runtime go through `spawn_blocking`
/// (rule 2). A stem that is not UTF-8 is skipped rather than decoded lossily
/// — it would become a tag nobody typed.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// std::fs::write(dir.path().join("fr.ftl"), "help-title = Aide\n").unwrap();
/// std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
/// let found = norte_config::locales::read_locales(dir.path(), 1024, 8);
/// assert_eq!(found.sources, vec![("fr".to_owned(), "help-title = Aide\n".to_owned())]);
/// ```
#[must_use]
pub fn read_locales(dir: &Path, max_bytes: usize, max_files: usize) -> Found {
    let mut found = Found::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    let mut paths: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "ftl"))
        .collect();
    paths.sort();
    for path in paths {
        // Raw: whoever paints these lines masks them (`norte-frontend`).
        let shown = path.display().to_string();
        // A regular file only (following symlinks): opening a FIFO, or a
        // link to `/dev/tty`, blocks until a writer appears, and the window
        // reads this before it opens.
        if !std::fs::metadata(&path).is_ok_and(|m| m.is_file()) {
            found.skipped.push(format!("{shown}: not a regular file"));
            continue;
        }
        if found.sources.len() >= max_files {
            found
                .skipped
                .push(format!("{shown}: more than {max_files} languages"));
            continue;
        }
        let Some(tag) = path.file_stem().and_then(|s| s.to_str()) else {
            found
                .skipped
                .push(format!("{shown}: the file name is not UTF-8"));
            continue;
        };
        match read_bounded(&path, max_bytes) {
            Ok(Some(source)) => found.sources.push((tag.to_owned(), source)),
            Ok(None) => found
                .skipped
                .push(format!("{shown}: larger than {max_bytes} bytes")),
            Err(e) => found.skipped.push(format!("{shown}: {e}")),
        }
    }
    found
}

/// The file as UTF-8 text, or `None` past `max_bytes`.
fn read_bounded(path: &Path, max_bytes: usize) -> std::io::Result<Option<String>> {
    let limit = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Ok(None);
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "not UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Too big, too many, not text: each one skipped with its reason, and the
    /// rest still read.
    #[test]
    fn what_cannot_be_read_is_skipped_and_said() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("big.ftl"), "x".repeat(20)).expect("write");
        std::fs::write(dir.path().join("de.ftl"), [0xff, 0xfe]).expect("write");
        std::fs::write(dir.path().join("fr.ftl"), "a = b\n").expect("write");
        std::fs::write(dir.path().join("it.ftl"), "a = c\n").expect("write");
        // A directory named like a language: not a regular file, skipped
        // without opening — what keeps a FIFO from hanging the start.
        std::fs::create_dir(dir.path().join("pt.ftl")).expect("mkdir");
        let found = read_locales(dir.path(), 10, 1);
        assert_eq!(found.sources, vec![("fr".to_owned(), "a = b\n".to_owned())]);
        assert_eq!(found.skipped.len(), 4, "{:?}", found.skipped);
        assert!(
            found
                .skipped
                .iter()
                .any(|s| s.contains("not a regular file"))
        );
        assert_eq!(
            read_locales(&dir.path().join("missing"), 10, 1),
            Found::default()
        );
    }
}
