//! Live config reload plumbing (feature `watch`): a native filesystem
//! watcher (inotify/FSEvents/ReadDirectoryChangesW) with a polling
//! fallback, so frontends can hot-reload `norte.toml`/`keymap.toml`/
//! `openers.toml` without blocking their async runtime.

use std::path::{Path, PathBuf};

use crate::dirs::Layers;

/// Watch mode achieved (for the user-facing warning, ADR 0007).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchMode {
    /// Native watcher (inotify/FSEvents/ReadDirectoryChangesW).
    Notify,
    /// Degraded to polling mtimes every 2s (with a warning, never failing).
    Polling,
}

/// Live watch of the config dirs: dropping this value STOPS it in both modes
/// (the native watcher closes; the polling task is cancelled via a token —
/// rule 3).
pub struct Watch {
    /// How it is currently watching.
    pub mode: WatchMode,
    _watcher: Option<notify::RecommendedWatcher>,
    cancel: tokio_util::sync::CancellationToken,
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Watches the layer directories and sends `()` over `tx` on every change (no
/// debounce: that is the consumer's job). Native watcher if it starts — and
/// ALWAYS a slow fallback poll (layers whose dir does not exist yet,
/// overflowed inotify queues); if the native one does not start at all, the
/// poll switches to fast and `mode` gives it away for the warning
/// (documented fallback: degrade, never fail). The setup (stats + inotify)
/// runs in `spawn_blocking` — callable from async (rule 2).
pub async fn watch(layers: &Layers, tx: tokio::sync::mpsc::Sender<()>) -> Watch {
    use notify::Watcher;
    let layers2 = layers.clone();
    let tx2 = tx.clone();
    let watcher = tokio::task::spawn_blocking(move || {
        let mut watcher = notify::recommended_watcher({
            move |res: Result<notify::Event, notify::Error>| {
                // The native watcher watches the WHOLE DIRECTORY (there is no
                // portable per-file watch), and the user's config dir has
                // LIVE neighbors: `index.db` (semantic index),
                // `journal.db`/`-shm`, the persist lockfile. `SQLite` writes
                // there while the app runs, so without this filter every
                // write triggered a full hot-reload — which closes the open
                // help (F1) and palette and paints "config reloaded". Only
                // the three TOML layers count, the SAME ones the polling
                // snapshot looks at ([`CONFIG_FILES`]).
                //
                // Also on Err (review M3): a notify error means "you may
                // have missed events" — rereading EVERYTHING is exactly the
                // right response, and an event with NO paths (backends that
                // do not carry them) is treated with the same conservatism.
                // try_send: changes are coalesced; losing one with the
                // channel full is harmless.
                let interesting = match &res {
                    Err(_) => true,
                    // `Access(_)` is a READ (including the open the reload
                    // itself does when rereading the layers): it changes
                    // nothing and, without discarding it, the reload fed
                    // back into itself — reloading opens norte.toml, the
                    // open triggers another reload. Every write still
                    // arrives as `Create`/`Modify`/`Remove`.
                    Ok(ev) => {
                        !matches!(ev.kind, notify::EventKind::Access(_))
                            && (ev.paths.is_empty() || ev.paths.iter().any(|p| is_config_file(p)))
                    }
                };
                if interesting {
                    let _ = tx2.try_send(());
                }
            }
        })
        .ok()?;
        let mut watching = false;
        for (dir, _kind) in &layers2.dirs {
            if dir.is_dir()
                && watcher
                    .watch(dir, notify::RecursiveMode::NonRecursive)
                    .is_ok()
            {
                watching = true;
            }
        }
        watching.then_some(watcher)
    })
    .await
    .ok()
    .flatten();

    let mode = if watcher.is_some() {
        WatchMode::Notify
    } else {
        WatchMode::Polling
    };
    // Fallback poll: fast if it is the ONLY mechanism; slow as the native
    // one's safety net (dirs created live, missed events).
    let period = match mode {
        WatchMode::Polling => std::time::Duration::from_secs(2),
        WatchMode::Notify => std::time::Duration::from_secs(10),
    };
    let cancel = tokio_util::sync::CancellationToken::new();
    spawn_poll(layers.clone(), tx, period, cancel.clone());
    Watch {
        mode,
        _watcher: watcher,
        cancel,
    }
}

/// Pure POLLING watch with its own period (the mechanism behind [`watch`]'s
/// fallback, exposed so its cancellation can be tested).
///
/// # Panics
/// If called outside a tokio runtime (it does `tokio::spawn`).
#[doc(hidden)]
#[must_use]
pub fn watch_polling(
    layers: &Layers,
    tx: tokio::sync::mpsc::Sender<()>,
    period: std::time::Duration,
) -> Watch {
    let cancel = tokio_util::sync::CancellationToken::new();
    spawn_poll(layers.clone(), tx, period, cancel.clone());
    Watch {
        mode: WatchMode::Polling,
        _watcher: None,
        cancel,
    }
}

/// Mtime+size polling task, cancellable (rule 3).
fn spawn_poll(
    layers: Layers,
    tx: tokio::sync::mpsc::Sender<()>,
    period: std::time::Duration,
    cancel: tokio_util::sync::CancellationToken,
) {
    tokio::spawn(async move {
        let mut last: Option<Vec<(PathBuf, std::time::SystemTime, u64)>> = None;
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(period) => {}
            }
            let layers2 = layers.clone();
            let Ok(snapshot) = tokio::task::spawn_blocking(move || snapshot(&layers2)).await else {
                return;
            };
            if let Some(prev) = &last
                && *prev != snapshot
                && tx.send(()).await.is_err()
            {
                return;
            }
            last = Some(snapshot);
        }
    });
}

/// The TOML layers an edit can change: the ONLY thing that triggers a
/// hot-reload, both from the native watcher (which can only watch the whole
/// dir) and from the polling snapshot. Any other file in the config dir —
/// `index.db`, `journal.db`, lockfiles — is noise from the app itself.
const CONFIG_FILES: [&str; 3] = ["norte.toml", "keymap.toml", "openers.toml"];

/// Is `p` one of the [`CONFIG_FILES`] layers? By file NAME: the watcher hands
/// back absolute paths from the watched dir, and a rename of
/// `norte.toml.tmp` to `norte.toml` (how editors write, and how the atomic
/// persist itself writes) arrives with the destination among its paths.
fn is_config_file(p: &Path) -> bool {
    p.file_name()
        .is_some_and(|n| CONFIG_FILES.iter().any(|c| n == *c))
}

/// Snapshot of (mtime, size) for the config files present — the size catches
/// writes within the FS mtime's granularity.
fn snapshot(layers: &Layers) -> Vec<(PathBuf, std::time::SystemTime, u64)> {
    let mut out = Vec::new();
    for (dir, _kind) in &layers.dirs {
        // "openers.toml" is added here (pre-existing debt closed when this
        // module was copied to norte-config): the native watcher watches the
        // WHOLE dir and already caught its edits, but the polling fallback
        // only looked at norte.toml/keymap.toml — an openers.toml edited
        // under pure polling (native watcher down) was missed.
        for name in CONFIG_FILES {
            let p = dir.join(name);
            if let Ok(md) = std::fs::metadata(&p)
                && let Ok(m) = md.modified()
            {
                let len = md.len();
                out.push((p, m, len));
            }
        }
    }
    out
}
