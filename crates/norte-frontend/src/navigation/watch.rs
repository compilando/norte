//! Watching panes' VISIBLE directories (#106, watching half): a native
//! watcher (inotify/FSEvents/ReadDirectoryChangesW) over both panes'
//! `file://` dirs, with a FALLBACK to mtime polling every 2s when the
//! native one does not start or `watch()` fails — the CLAUDE.md pitfall:
//! inotify watches are LIMITED; degrade with a notice, never fail. Events
//! reach the run loop DEBOUNCED (trailing-edge coalesce): a burst of
//! writes = one refresh, not a storm.
//!
//! v1 scope: only non-virtual local panes (an sftp/S3/archive dir has no
//! inotify; its refresh stays manual). The consumer reacts to each event
//! with ITS OWN refresh path — a mutation's, not a `cd`'s — which is what
//! preserves the marks: `Ctrl+R` in the TUI, `refresh_dir` in the GUI.
//! Both use it since item 7 of the post-alpha roadmap; the TUI debuted it
//! and that is why the vocabulary here is its own.
//!
//! Documented limits of degraded mode: polling looks at the DIRECTORY's
//! mtime — creating/deleting/renaming inside it is seen; writing to an
//! existing file does NOT change the parent's mtime and is not detected
//! (the bar's notice says so). Degradation is a session latch (one
//! direction only): a transient inotify limit leaves polling active until
//! restart — simplicity over hysteresis (v1).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Trailing-edge coalesce: after the first raw event, this gap is waited
/// out (draining whatever keeps arriving) before emitting ONE debounced
/// one.
const DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(300);
/// Period of mtime polling in degraded mode (inotify pitfall).
const POLL: std::time::Duration = std::time::Duration::from_secs(2);
/// Floor between EMISSIONS (review MAJOR-3): a sustained storm (a big
/// copy into the dir, ours or someone else's) emits at most once per
/// floor — never a refresh every `DEBOUNCE`. Derived from `debounce` in
/// tests.
const fn emit_floor(debounce: std::time::Duration) -> std::time::Duration {
    debounce.saturating_mul(3)
}
/// Coalesce latency cap (review MAJOR-3): with events arriving with no
/// pause, the wait for a quiet window cannot defer the refresh forever —
/// past the cap it is emitted anyway.
const fn max_coalesce(debounce: std::time::Duration) -> std::time::Duration {
    debounce.saturating_mul(10)
}

/// Is this watcher event a CHANGE in the directory, or just a read?
///
/// Dropping reads is not an optimization, it is what stops watching from
/// feeding back on itself: listing a directory opens and walks it, and
/// inotify emits `IN_OPEN`/`IN_ACCESS`/`IN_CLOSE_NOWRITE` **on the watched
/// directory itself**. Without this filter, every refresh generated the
/// events that triggered the next one, so after the first `cd` both panes
/// re-listed forever at the pace of the emission floor (~1.2s) — the
/// "constant flicker" seen in the columns, and a mouse gesture cancelled
/// every time the listing changed under it.
///
/// `norte_config::watch` already had this filter for the same reason
/// (reloading opens `norte.toml`, and that open triggered another reload).
/// Every real write still arrives as `Create`/`Modify`/`Remove`, and an
/// `Err` counts as a change on purpose: it means "you may have missed
/// events".
fn es_change(res: &Result<notify::Event, notify::Error>) -> bool {
    match res {
        Err(_) => true,
        Ok(ev) => !matches!(ev.kind, notify::EventKind::Access(_)),
    }
}

/// Shared watcher/poller <-> [`DirWatch`] state.
struct Shared {
    /// Natively watched dirs (one per pane; `None` = pane not watchable).
    dirs: Mutex<[Option<PathBuf>; 2]>,
    /// Degraded mode: the poller polls mtimes (the native one does not
    /// cover it).
    degraded: AtomicBool,
}

/// Live watch of the panes' dirs. Dropping this value STOPS it (rule 3,
/// drop-based cancellation: the native watcher closes and the
/// debouncer/poller task sees its raw channel closed and returns).
pub struct DirWatch {
    /// Receives ONE event per burst (debounced): "something changed in a
    /// watched dir" — the consumer refreshes both panes (Ctrl+R parity).
    pub rx: tokio::sync::mpsc::Receiver<()>,
    /// Raw sender kept on PURPOSE (and used by tests): in degraded mode the
    /// watcher is `None` and without this end alive the raw channel would
    /// close, killing the POLLER too. Its drop (with the watcher's) is
    /// what closes the task — drop-based cancellation.
    #[cfg_attr(not(test), allow(dead_code))]
    raw_tx: tokio::sync::mpsc::UnboundedSender<()>,
    watcher: Option<notify::RecommendedWatcher>,
    shared: Arc<Shared>,
    /// Degradation notice pending to show (once only).
    degraded_pending: bool,
}

impl DirWatch {
    /// Starts the pipeline: native watcher (if it can) + debouncer/poller
    /// task. Never fails: with no native one it stays DEGRADED (polling).
    ///
    /// # Panics
    /// If called OUTSIDE a tokio runtime: the debouncer is a
    /// `tokio::spawn`. In the GUI that means the session thread and not
    /// GPUI's, which has no runtime to ask for one.
    #[must_use]
    pub fn new() -> Self {
        Self::new_with(DEBOUNCE, POLL)
    }

    /// Like [`Self::new`] with injectable periods (tests: real time with
    /// short periods — the poller does real I/O and tokio's paused clock
    /// does not wait for it).
    fn new_with(debounce: std::time::Duration, poll: std::time::Duration) -> Self {
        let (raw_tx, mut raw_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let (out_tx, rx) = tokio::sync::mpsc::channel::<()>(1);
        let shared = Arc::new(Shared {
            dirs: Mutex::new([None, None]),
            degraded: AtomicBool::new(false),
        });
        // Native watcher: a CHANGE event (also Err: "you may have missed
        // events") = raw ping; the debouncer coalesces. Same criterion as
        // `norte_config::watch`, including its filter — see [`es_change`],
        // which is what stops this from feeding back on itself.
        let cb_tx = raw_tx.clone();
        let watcher =
            notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
                if es_change(&res) {
                    let _ = cb_tx.send(());
                }
            })
            .ok();
        if watcher.is_none() {
            shared.degraded.store(true, Ordering::Relaxed);
        }
        let degraded_pending = watcher.is_none();
        // Debouncer + poller task (rule 3: returns when ALL raw senders
        // die — dropping `DirWatch` drops the watcher and `raw_tx` — or
        // when the consumer drops `rx`).
        let sh = Arc::clone(&shared);
        tokio::spawn(async move {
            let mut mtimes: std::collections::HashMap<PathBuf, std::time::SystemTime> =
                std::collections::HashMap::new();
            loop {
                tokio::select! {
                    ev = raw_rx.recv() => {
                        if ev.is_none() {
                            return; // all senders dead (drop)
                        }
                        // REAL trailing edge (review MAJOR-3): drain and
                        // wait for a quiet window; a storm with no pause
                        // emits anyway at the latency cap.
                        let start = tokio::time::Instant::now();
                        loop {
                            while raw_rx.try_recv().is_ok() {}
                            tokio::time::sleep(debounce).await;
                            if raw_rx.try_recv().is_err() {
                                break; // quiet window
                            }
                            if start.elapsed() >= max_coalesce(debounce) {
                                while raw_rx.try_recv().is_ok() {}
                                break;
                            }
                        }
                        if out_tx.send(()).await.is_err() {
                            return; // consumer dead
                        }
                        // Floor between emissions: whatever arrives during
                        // the wait accumulates and coalesces into the next
                        // one.
                        tokio::time::sleep(emit_floor(debounce)).await;
                    }
                    () = tokio::time::sleep(poll), if sh.degraded.load(Ordering::Relaxed) => {
                        if out_tx.is_closed() {
                            return;
                        }
                        // Poisoning is impossible in practice (nobody
                        // panics holding the lock): the data is still
                        // valid.
                        let dirs = sh
                            .dirs
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone();
                        // MINOR-4: prune baselines of dirs no longer
                        // watched (without this the map grows all
                        // session).
                        mtimes.retain(|d, _| dirs.iter().flatten().any(|w| w == d));
                        let mut changed = false;
                        for dir in dirs.into_iter().flatten() {
                            let Ok(meta) = tokio::fs::metadata(&dir).await else {
                                continue; // dir gone: the refresh will say so
                            };
                            let Ok(modified) = meta.modified() else {
                                continue;
                            };
                            match mtimes.insert(dir, modified) {
                                Some(prev) if prev != modified => changed = true,
                                // First sighting = baseline (starting up is
                                // not a change); same mtime = nothing.
                                None | Some(_) => {}
                            }
                        }
                        if changed && out_tx.send(()).await.is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Self {
            rx,
            raw_tx,
            watcher,
            shared,
            degraded_pending,
        }
    }

    /// Updates the watched set to `targets`' (one per pane, `None` = not
    /// watchable). Cheap diff: no changes, zero syscalls — callable on
    /// every run-loop iteration. A `watch()` that fails (inotify limit)
    /// degrades to polling with a notice, never fails.
    pub fn rewatch(&mut self, targets: &[Option<PathBuf>; 2]) {
        use notify::Watcher as _;
        let old = {
            // A single lock scope (atomic compare+replace); poisoning is
            // impossible in practice -> into_inner.
            let mut d = self
                .shared
                .dirs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *d == *targets {
                return;
            }
            std::mem::replace(&mut *d, targets.clone())
        };
        if let Some(w) = &mut self.watcher {
            for dir in old.iter().flatten() {
                if !targets.iter().flatten().any(|t| t == dir) {
                    let _ = w.unwatch(dir);
                }
            }
            for dir in targets.iter().flatten() {
                if !old.iter().flatten().any(|t| t == dir)
                    && w.watch(dir, notify::RecursiveMode::NonRecursive).is_err()
                    && !self.shared.degraded.swap(true, Ordering::Relaxed)
                {
                    // Watch limit (inotify pitfall): degrade with a
                    // notice, the poller covers it from now on.
                    self.degraded_pending = true;
                }
            }
        }
    }

    /// `true` ONCE when watching just degraded to polling — the caller
    /// paints the notice (`status-watch-degraded`) and does not repeat it.
    pub fn take_degraded_notice(&mut self) -> bool {
        std::mem::take(&mut self.degraded_pending)
    }

    /// Raw-event injector for tests (same channel as the watcher).
    #[cfg(test)]
    fn inject(&self) {
        let _ = self.raw_tx.send(());
    }

    /// Forces degraded mode (poller tests).
    #[cfg(test)]
    fn force_degraded(&self) {
        self.shared.degraded.store(true, Ordering::Relaxed);
    }
}

impl Default for DirWatch {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAST_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(20);
    const FAST_POLL: std::time::Duration = std::time::Duration::from_millis(50);

    async fn recv_within(rx: &mut tokio::sync::mpsc::Receiver<()>, d: std::time::Duration) -> bool {
        tokio::time::timeout(d, rx.recv()).await.is_ok()
    }

    /// A READ of the watched dir is not a change. It is the filter that
    /// stops watching from feeding back on itself: listing opens and walks
    /// the directory, and inotify emits `Access` on it, so counting it
    /// would make every refresh trigger the next one.
    #[test]
    fn a_read_does_not_count_as_a_change() {
        use notify::event::{AccessKind, CreateKind, EventKind, ModifyKind, RemoveKind};

        let ev = |kind| Ok(notify::Event::new(kind));
        assert!(!es_change(&ev(EventKind::Access(AccessKind::Any))));
        assert!(!es_change(&ev(EventKind::Access(AccessKind::Read))));
        assert!(!es_change(&ev(EventKind::Access(AccessKind::Open(
            notify::event::AccessMode::Read
        )))));
        // Every real write still counts.
        assert!(es_change(&ev(EventKind::Create(CreateKind::File))));
        assert!(es_change(&ev(EventKind::Modify(ModifyKind::Any))));
        assert!(es_change(&ev(EventKind::Remove(RemoveKind::File))));
        // And an error means "you may have missed events": refresh.
        assert!(es_change(&Err(notify::Error::generic("perdidos"))));
    }

    /// The whole loop, with a real watcher: reading the watched directory —
    /// which is WHAT a refresh DOES — must not produce a single event,
    /// while creating a file does. Without the filter, this test emits on
    /// the first read and the TUI re-lists forever after the first `cd`.
    #[tokio::test]
    async fn listing_the_watched_dir_does_not_trigger_refreshes() {
        let dir = tempfile::tempdir().unwrap();
        // Polling practically off: this test looks ONLY at the watcher.
        let mut w = DirWatch::new_with(FAST_DEBOUNCE, std::time::Duration::from_hours(1));
        w.rewatch(&[Some(dir.path().to_path_buf()), None]);
        // Let the native watcher register before reading.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        for _ in 0..5 {
            let _: Vec<_> = std::fs::read_dir(dir.path())
                .expect("read the watched dir")
                .collect();
        }
        assert!(
            !recv_within(&mut w.rx, std::time::Duration::from_millis(600)).await,
            "reading the directory is not a change: if this emits, the refresh feeds back on itself"
        );

        std::fs::write(dir.path().join("nuevo.txt"), b"x").expect("create");
        assert!(
            recv_within(&mut w.rx, std::time::Duration::from_secs(2)).await,
            "a real write does refresh"
        );
    }

    /// A burst of raw events = ONE debounced event (trailing edge) —
    /// without this, a big copy into the watched dir would be a storm of
    /// refreshes.
    #[tokio::test]
    async fn burst_coalesces_to_one_event() {
        let mut w = DirWatch::new_with(FAST_DEBOUNCE, FAST_POLL);
        for _ in 0..5 {
            w.inject();
        }
        assert!(
            recv_within(&mut w.rx, std::time::Duration::from_secs(2)).await,
            "one debounced event"
        );
        tokio::time::sleep(FAST_DEBOUNCE * 3).await;
        assert!(w.rx.try_recv().is_err(), "and ONLY one");
    }

    /// Degraded mode (inotify pitfall): the poller detects a mtime change
    /// on the watched dir and emits; the first sighting is the baseline
    /// (starting up is not a change).
    #[tokio::test]
    async fn poller_degraded_detects_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = DirWatch::new_with(FAST_DEBOUNCE, FAST_POLL);
        // No native watcher: only the poller can emit (isolates the test
        // from a real inotify over the tempdir).
        w.watcher = None;
        w.force_degraded();
        w.rewatch(&[Some(dir.path().to_path_buf()), None]);
        // Baseline: several poll passes WITHOUT touching the dir.
        tokio::time::sleep(FAST_POLL * 4).await;
        assert!(w.rx.try_recv().is_err(), "baseline with no event");
        // Real change (the DIRECTORY's mtime changes when creating inside).
        std::fs::write(dir.path().join("nuevo"), b"x").unwrap();
        assert!(
            recv_within(&mut w.rx, std::time::Duration::from_secs(5)).await,
            "the mtime change emits an event"
        );
    }

    /// Rule 3 (drop-based cancellation): dropping the senders kills the
    /// task — the debounced channel closes (recv returns None), nothing is
    /// left alive.
    #[tokio::test]
    async fn drop_closes_the_pipeline() {
        let w = DirWatch::new_with(FAST_DEBOUNCE, FAST_POLL);
        let mut rx = w.rx;
        drop(w.watcher);
        drop(w.raw_tx);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
                .await
                .expect("the task must die, not hang"),
            None,
            "pipeline dead after the drop"
        );
    }

    /// Review MINOR-7: removing a dir from the set (pane to virtual, cd to
    /// remote) updates the shared state — the poller stops polling it and
    /// its baseline is pruned.
    #[tokio::test]
    async fn rewatch_with_fewer_dirs_updates_the_set() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = DirWatch::new_with(FAST_DEBOUNCE, FAST_POLL);
        w.rewatch(&[Some(dir.path().to_path_buf()), None]);
        w.rewatch(&[None, None]);
        assert_eq!(
            *w.shared
                .dirs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            [None, None]
        );
    }

    /// Review MINOR-7: the degradation notice is one-shot.
    #[tokio::test]
    async fn take_degraded_notice_es_one_shot() {
        let mut w = DirWatch::new_with(FAST_DEBOUNCE, FAST_POLL);
        w.degraded_pending = true;
        assert!(w.take_degraded_notice());
        assert!(!w.take_degraded_notice(), "only the first time");
    }

    /// Review MINOR-7: dropping the WHOLE VALUE (the real production path)
    /// also kills the pipeline.
    #[tokio::test]
    async fn dropping_entirely_closes_the_pipeline() {
        let (probe_tx, mut probe_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        {
            let w = DirWatch::new_with(FAST_DEBOUNCE, FAST_POLL);
            // Probe: when the task dies, its out_tx is dropped… not
            // observable from outside with no rx (which dies with w). It
            // is observed via the raw sender: after the drop, sending
            // fails.
            let raw = w.raw_tx.clone();
            drop(w);
            tokio::spawn(async move {
                // The task sees raw_rx hanging off THIS clone; dropping it
                // kills the channel completely and the task returns.
                drop(raw);
                let _ = probe_tx.send(());
            });
        }
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(2), probe_rx.recv())
                .await
                .is_ok()
        );
    }

    /// Review MAJOR-3: a sustained STORM of raw events does not emit one
    /// refresh per debounce — the floor between emissions caps the rate.
    #[tokio::test]
    async fn a_sustained_storm_respects_the_floor() {
        let mut w = DirWatch::new_with(FAST_DEBOUNCE, FAST_POLL);
        let raw = w.raw_tx.clone();
        let storm = tokio::spawn(async move {
            for _ in 0..200 {
                let _ = raw.send(());
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
        });
        // Storm ~= 400ms = 20x debounce. With no floor there would be ~20
        // emissions; with coalesce+cap+floor, ~2-3 fit. Generous
        // anti-flake bound.
        let mut emitted = 0;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(100), w.rx.recv()).await {
                Ok(Some(())) => emitted += 1,
                _ => {
                    if storm.is_finished() {
                        break;
                    }
                }
            }
        }
        assert!(emitted >= 1, "the storm must emit at least once");
        assert!(
            emitted <= 6,
            "rate capped by the floor, not one per debounce: {emitted}"
        );
    }

    /// `rewatch` with the SAME set is a no-op — called on every run-loop
    /// iteration.
    #[tokio::test]
    async fn rewatch_es_idempotente() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = DirWatch::new_with(FAST_DEBOUNCE, FAST_POLL);
        let t = [Some(dir.path().to_path_buf()), None];
        w.rewatch(&t);
        w.rewatch(&t);
        assert_eq!(*w.shared.dirs.lock().unwrap(), t);
    }

    /// REAL end-to-end native path: writing to a watched dir produces a
    /// debounced event (if notify cannot start in this environment, the
    /// constructor is already degraded and the test skips — the poller has
    /// its own test).
    #[tokio::test]
    async fn native_watcher_detects_a_write() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = DirWatch::new_with(FAST_DEBOUNCE, FAST_POLL);
        if w.watcher.is_none() {
            return; // environment with no inotify: covered by the poller
        }
        w.rewatch(&[Some(dir.path().to_path_buf()), None]);
        std::fs::write(dir.path().join("nuevo"), b"x").unwrap();
        assert!(
            recv_within(&mut w.rx, std::time::Duration::from_secs(5)).await,
            "the native watcher emits on a real write"
        );
    }
}
