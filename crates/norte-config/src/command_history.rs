//! The command bar's history on disk (ADR 0181): `<state dir>/command-history`,
//! one order per line, private to the user, shared by both frontends.

use std::path::{Path, PathBuf};

/// Where the history lives, if there is a state directory.
#[must_use]
pub fn path() -> Option<PathBuf> {
    crate::dirs::state_dir().map(|d| d.join("command-history"))
}

/// The orders in `path`, oldest first; a missing or unreadable file is an
/// empty history (it is a convenience, never a reason to refuse to start).
#[must_use]
pub fn load(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .filter(|l| !l.trim().is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Writes `history` to `path` through a temp file and a rename in the same
/// directory, created mode 0600 on unix: orders can name private paths.
///
/// # Errors
/// Creating the directory, writing or renaming failed.
pub fn save(path: &Path, history: &[String]) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    for order in history {
        // One order per line: the bar never takes a newline, but a file
        // edited by hand might, and it must not become two orders.
        writeln!(f, "{}", order.replace(['\n', '\r'], " "))?;
    }
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_history_survives_a_restart_privately() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub").join("command-history");
        assert!(load(&p).is_empty(), "missing file = empty");
        save(&p, &["ls -la".into(), "make test".into()]).unwrap();
        assert_eq!(load(&p), ["ls -la", "make test"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
