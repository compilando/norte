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

/// Adds `order` to the history in `path`: re-reads the file (another
/// frontend may have added since), skips a repeat of the last one, keeps
/// the newest `max`, and writes it back. One append at a time per process;
/// across processes the temp names never collide, and the rename is atomic.
///
/// # Errors
/// Creating the directory, writing or renaming failed.
pub fn append(path: &Path, order: &str, max: usize) -> std::io::Result<()> {
    static ONE_WRITER: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = ONE_WRITER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut history = load(path);
    if history.last().map(String::as_str) != Some(order) {
        history.push(order.to_owned());
    }
    let excess = history.len().saturating_sub(max);
    history.drain(..excess);
    save(path, &history)
}

/// Writes `history` to `path` through a temp file of its own and a rename
/// in the same directory, created mode 0600 on unix: orders can name
/// private paths.
///
/// # Errors
/// Creating the directory, writing or renaming failed.
pub fn save(path: &Path, history: &[String]) -> std::io::Result<()> {
    use std::io::Write as _;
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp-{}-{n}", std::process::id()));
    let mut opts = std::fs::OpenOptions::new();
    // `create_new`: never follows nor reuses a file someone left there.
    opts.write(true).create_new(true);
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

    /// Orders appended from several threads (and, the same way, from the
    /// terminal and the window at once) are all kept: each append re-reads
    /// the file, and no two writers share a temp file.
    #[test]
    fn concurrent_appends_lose_nothing() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("command-history");
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let p = p.clone();
                std::thread::spawn(move || append(&p, &format!("echo {i}"), 200))
            })
            .collect();
        for t in threads {
            t.join().unwrap().unwrap();
        }
        let mut got = load(&p);
        got.sort();
        assert_eq!(got.len(), 8, "{got:?}");
        assert!(
            std::fs::read_dir(d.path()).unwrap().count() == 1,
            "no temp left"
        );
    }

    #[test]
    fn append_skips_a_repeat_and_caps() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("h");
        for o in ["a", "a", "b", "c"] {
            append(&p, o, 2).unwrap();
        }
        assert_eq!(load(&p), ["b", "c"]);
    }

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
