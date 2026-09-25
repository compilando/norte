//! Tracing setup for the binaries.
//!
//! Lives HERE, not in the core, because there are two families of binaries
//! that need it and only one can depend on the engine: the graphical window
//! talks to the daemon over a socket and cannot drag in the engine, the
//! providers and the plugin host just to write a log line (ADR 0066).
//! Setting it up twice was the earlier decision, and what it cost once was
//! that the copy left out the hardening: a log readable by any local account,
//! carrying the paths the user had browsed, for six commits (#255). This
//! crate already owned `state_dir()` and the `[log]` keys, so here the
//! permission is set in ONE place.
//!
//! SECURITY cap (issue #43, rule 10): `suppaftp` logs every control-channel
//! command at TRACE level of the `log` crate, including `PASS <password>`.
//! The `tracing-log` bridge (a default feature of `tracing-subscriber`) would
//! materialize it with `RUST_LOG=trace`. [`init`](crate::logging::init) adds a
//! static `suppaftp=info` directive AT THE END of the filter, so it beats any
//! `RUST_LOG` — including an explicit `suppaftp=trace` — and the password
//! never reaches the sink.
//!
//! # Which level to use (ADR 0127)
//!
//! | level | when |
//! | --- | --- |
//! | `error!` | something the user asked for failed and does not recover |
//! | `warn!` | something degraded and continued (inotify → polling) |
//! | `info!` | lifecycle: starting up, connecting, starting and finishing a task |
//! | `debug!` | decisions: policy verdicts, keymap resolution |
//! | `trace!` | per entry or per block; never on by default |
//!
//! There is no `fatal`. A fatal failure is an `error!` in a binary's `main`
//! followed by exiting via `anyhow`: a library does not terminate the
//! process.
//!
//! # Where the spans open
//!
//! No logger is injected: `tracing` is already the facade, and the binaries
//! tie it to a subscriber here. The core opens spans at two boundaries and
//! nowhere else, and every event emitted inside inherits them without
//! touching its own line:
//!
//! ```text
//! rpc{conn_id, req_id, method}       the daemon, around each request
//! └─ task{task_id, kind, provider}   `Scheduler::submit`, travels with the job
//! ```
//!
//! The fields of those two are identifiers, types, a scheme and method
//! names. **Never a path or a parameter.** The engine's spans in between
//! (`copy_anchored{from, to}`) do carry paths, but always through
//! `span_path`, which redacts a `user:pass@`. Whatever the peer chooses
//! (method, id) enters bounded and escaped.

use std::path::Path;

pub use crate::schema::LogFormat;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::prelude::*;

/// Builds the `EnvFilter`: default INFO, honors `RUST_LOG` (or `env` if
/// passed, for tests), and ALWAYS caps `suppaftp` to `info` as the last
/// directive (rule 10, not configurable).
fn filter_from(env: Option<&str>) -> EnvFilter {
    let base = match env {
        Some(s) => EnvFilter::builder()
            .with_default_directive(LevelFilter::INFO.into())
            .parse_lossy(s),
        None => EnvFilter::builder()
            .with_default_directive(LevelFilter::INFO.into())
            .from_env_lossy(),
    };
    // Last directive of equal specificity wins → hard cap.
    base.add_directive("suppaftp=info".parse().expect("valid static directive"))
}

/// Default prefix for the rotated files. Rotation is DAILY, so the real name
/// carries the date after it.
const LOG_PREFIX: &str = "norte.log";

/// Creates the log directory CLOSED, and tightens whatever is already inside.
///
/// **0700, not the umask.** What this log holds is the same as what the
/// journal holds — every copy, every delete, every remote host — and in this
/// tree everything that is state is kept closed: `journal.db` 0600 in a 0700
/// dir, the spool the same, `lua-trust.toml` the same, `secrets.age` 0600,
/// the daemon socket 0600. With the default umask this came out 0755/0644,
/// i.e. readable by any local account.
///
/// **And the mode is applied to the parents it creates along the way, which
/// is the important half.** `init_to_file` is the FIRST thing to touch
/// `<state_dir>` in both frontends — before the journal, before anything —
/// so a `create_dir_all` with no mode created `<state_dir>` at 0755; the
/// journal arrives later with its `DirBuilder::mode(0o700)`, which on a
/// directory that ALREADY exists does no chmod at all. The 0755 stuck around
/// forever, exposing the listing of `journal.db`, `lua-trust.toml` and the
/// spool. On a fresh install, and with nothing warning about it.
#[cfg(unix)]
fn create_dir_locked(dir: &Path, prefix: &str) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    // A pre-existing directory is NOT touched by the `create` above (that is
    // the hole this closes), and the appender opens its files with the
    // umask because `tracing-appender` does not let you choose a mode.
    // Whatever is there gets tightened.
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    for entry in std::fs::read_dir(dir)?.flatten() {
        if entry
            .file_name()
            .as_encoded_bytes()
            .starts_with(prefix.as_bytes())
        {
            let _ = std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(0o600));
        }
    }
    Ok(())
}

/// On Windows permissions are ACLs and `<state_dir>` hangs off
/// `%LOCALAPPDATA%`, which already belongs to the user. Nothing equivalent to
/// apply here.
#[cfg(not(unix))]
fn create_dir_locked(dir: &Path, _prefix: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// The file layer, or `None` if the directory could not be prepared.
///
/// **Writes SYNCHRONOUSLY, with no non-blocking writer and therefore no
/// guard**, and that is a decision, not an oversight. A `WorkerGuard` flushes
/// the queue when dropped, which means any exit via `std::process::exit` —
/// and there are several: the CLI's Ctrl+C, the TUI's `--pick`, macOS's
/// `terminate:` — throws away exactly the last lines, which are the ones for
/// the failure someone is investigating. These binaries log a handful of
/// lines per session at INFO level; the cost of writing bare is not
/// measured, and in exchange a whole class of failure disappears.
///
/// `None` instead of `Err`: a log is diagnostic, and a diagnostic that
/// prevents startup is worse than not having one.
///
/// `format` decides how each line is written (ADR 0127): text for a person,
/// or JSON with the fields and the span chain for a program.
fn file_layer<S>(
    dir: &Path,
    retain: usize,
    prefix: &str,
    format: LogFormat,
) -> Option<Box<dyn Layer<S> + Send + Sync>>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    create_dir_locked(dir, prefix).ok()?;
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(prefix)
        // `max_log_files(0)` would have the pruning delete the very file
        // about to be written to: one is the minimum that means anything.
        .max_log_files(retain.max(1))
        .build(dir)
        .ok()?;
    // A file is not a terminal: color codes would clutter it.
    let layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(appender);
    Some(match format {
        LogFormat::Text => layer.boxed(),
        LogFormat::Json => layer
            .json()
            // `span`: the innermost one; `spans`: the whole chain, outside
            // in. With both, one line says which task it happened in and
            // which request asked for it without hunting earlier lines.
            .with_current_span(true)
            .with_span_list(true)
            .boxed(),
    })
}

/// How many rotated files are kept when the config says nothing else. One
/// week: enough that yesterday's failure is still there, little enough that
/// this does not grow unwatched.
const RETAIN_DEFAULT: usize = 7;

/// Where the log goes: `[log] dir`, or `<state_dir>/logs`.
///
/// `None` = there is no state directory (a bare CI, a service with no
/// `HOME`) and therefore no file. The caller degrades.
///
/// A RELATIVE `[log] dir` is refused and falls back to the default: it would
/// resolve against the cwd, which in a file manager is the directory you
/// launched it from — often a repository — and the log would end up inside
/// whoever's working tree. Same rule ADR 0035 C1 applies to the config
/// directory.
#[must_use]
pub fn log_dir(configured: Option<&Path>) -> Option<std::path::PathBuf> {
    match configured {
        Some(d) if d.is_absolute() => Some(d.to_path_buf()),
        Some(d) => {
            tracing::warn!(
                dir = %d.display(),
                "[log] dir is relative and is ignored: the log would end up in the cwd"
            );
            crate::dirs::state_dir().map(|s| s.join("logs"))
        }
        None => crate::dirs::state_dir().map(|s| s.join("logs")),
    }
}

/// What `[log]` says, the way the binaries have it at hand.
///
/// A struct and not two loose parameters because the three places that
/// install logging must pass THE SAME thing, and two `Option`s in a row are
/// two chances to cross them.
#[derive(Debug, Clone, Copy, Default)]
pub struct LogConfig<'a> {
    /// `[log] dir`. `None` = `<state_dir>/logs`.
    pub dir: Option<&'a Path>,
    /// `[log] retain`. `None` = one week of files.
    pub retain: Option<usize>,
    /// Prefix of the rotated file. `None` = `norte.log`.
    ///
    /// Exists because two different processes must NOT rotate the same
    /// file: the graphical window and the daemon can be alive at once, and
    /// one's retention would prune the other's files.
    pub prefix: Option<&'a str>,
    /// `[log] format`: how the FILE is written (ADR 0127). stderr stays in
    /// text regardless, because a person reads it in a terminal.
    pub format: LogFormat,
}

/// Installs the global subscriber: stderr PLUS the rotating file, with the
/// security cap. For `norte-cli` and the daemon.
///
/// Idempotent and non-fatal: if there is already a subscriber, it does
/// nothing.
pub fn init(cfg: LogConfig<'_>) {
    let _ = init_with(true, cfg, None);
}

/// Like [`init_to_file`], plus an in-memory ring the frontend can paint
/// (`panel.log`).
///
/// `None` if a subscriber was already installed, because then NOBODY writes
/// to the ring. Returning it anyway — as the first version did — left the
/// panel showing "nothing to show with this filter" forever, which is
/// exactly the confusion the panel exists to avoid: "nothing has happened"
/// has to be distinguishable from "not connected", and with the `Option` the
/// interface can say the latter.
///
/// The ring's level is raised later, live, with
/// [`LogRing::raise_to`](crate::logring::LogRing::raise_to). It starts at
/// INFO: a verbose level is paid for even when nobody is looking.
#[must_use]
pub fn init_to_file_with_ring(cfg: LogConfig<'_>, cap: usize) -> Option<crate::logring::LogRing> {
    let ring = crate::logring::LogRing::new(cap);
    init_with(false, cfg, Some(&ring)).then_some(ring)
}

/// Like [`init`] — stderr PLUS file — plus the in-memory ring. For the
/// DAEMON (#328, ADR 0092).
///
/// Exists because the daemon needs both things at once and neither of the
/// other two gives them: [`init`] sets up no ring, and
/// [`init_to_file_with_ring`] leaves the process without stderr. Taking
/// stderr away from `norte daemon run` would be a silent regression —
/// whoever starts it in a terminal to see why it will not come up would stop
/// reading anything.
///
/// Only the daemon calls it: a `norte cp` has nobody to show a ring to, and
/// would pay for two thousand lines of memory for no one. The rest of the
/// CLI stays on [`init`].
///
/// `None` under the same rule as [`init_to_file_with_ring`]: if there was
/// already a subscriber, nobody writes to this ring, and returning it would
/// leave the reader believing an empty log means nothing happened.
#[must_use]
pub fn init_with_ring(cfg: LogConfig<'_>, cap: usize) -> Option<crate::logring::LogRing> {
    let ring = crate::logring::LogRing::new(cap);
    init_with(true, cfg, Some(&ring)).then_some(ring)
}

/// Like [`init`] but ONLY to the file. For the frontends.
///
/// The TUI installed no subscriber at all, and said so in a comment: an
/// `fmt` to stderr fights with the alternate screen, so every
/// `tracing::warn!` coming out of there was silently dropped. The GUI
/// installed none either. The two frontends a user actually runs produced
/// not a single diagnostic; this is what fixes it, without writing a byte to
/// a screen they are drawing.
pub fn init_to_file(cfg: LogConfig<'_>) {
    let _ = init_with(false, cfg, None);
}

/// The common setup. `stderr` decides whether the terminal layer goes in
/// too.
///
/// **The filters are PER LAYER and no longer a single global one**, and that
/// change is what makes the log panel possible: with one `EnvFilter` over
/// the whole registry, an INFO level means `DEBUG`s are never emitted, and
/// then no panel can show them afterwards — filtering, in the window, what
/// was never logged is impossible. With per-layer filters, the file and
/// stderr keep exactly their own (same [`filter_from`], same `suppaftp`
/// cap) and the ring carries its own, which can also be changed live.
/// Returns whether THIS setup is the one that got installed: `false` means
/// there was already a subscriber, and then nothing set up here receives
/// anything.
fn init_with(stderr: bool, cfg: LogConfig<'_>, ring: Option<&crate::logring::LogRing>) -> bool {
    let file = match log_dir(cfg.dir) {
        Some(d) => file_layer(
            &d,
            cfg.retain.unwrap_or(RETAIN_DEFAULT),
            cfg.prefix.unwrap_or(LOG_PREFIX),
            cfg.format,
        ),
        None => None,
    }
    .map(|l| l.with_filter(filter_from(None)));
    let terminal = stderr.then(|| {
        tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_filter(filter_from(None))
    });
    let ring = ring.map(crate::logring::ring_layer);
    tracing_subscriber::registry()
        .with(file)
        .with(terminal)
        .with(ring)
        .try_init()
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing::Level;
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::Context;

    /// A layer that records (target, level) for every event that PASSES
    /// THROUGH it (already filtered): what arrives here is exactly what the
    /// sink would log.
    struct Collect(Arc<Mutex<Vec<(String, Level)>>>);
    impl<S: tracing::Subscriber> Layer<S> for Collect {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            let m = event.metadata();
            self.0
                .lock()
                .expect("lock")
                .push((m.target().to_string(), *m.level()));
        }
    }

    #[test]
    fn suppaftp_trace_capped_even_with_trace_env() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry()
            .with(Collect(Arc::clone(&seen)))
            .with(filter_from(Some("trace,suppaftp=trace")));

        tracing::subscriber::with_default(subscriber, || {
            tracing::trace!(target: "suppaftp", "PASS hunter2"); // must be DROPPED
            tracing::info!(target: "suppaftp", "connected"); // passes
            tracing::trace!(target: "other", "visible"); // passes (not capped)
        });

        let seen = seen.lock().expect("lock");
        // The password (suppaftp's TRACE event) NEVER passes the filter.
        assert!(
            !seen.contains(&("suppaftp".to_string(), Level::TRACE)),
            "suppaftp TRACE must be capped: {seen:?}"
        );
        // But suppaftp's info and other targets' trace do.
        assert!(seen.contains(&("suppaftp".to_string(), Level::INFO)));
        assert!(seen.contains(&("other".to_string(), Level::TRACE)));
    }

    /// The files in `dir`, with their content concatenated.
    fn dump(dir: &std::path::Path) -> (usize, String) {
        let files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .expect("list")
            .map(|e| e.expect("entry").path())
            .collect();
        let text = files
            .iter()
            .map(|f| std::fs::read_to_string(f).unwrap_or_default())
            .collect();
        (files.len(), text)
    }

    /// The appender really writes, into the directory it is given.
    #[test]
    fn the_log_lands_in_a_file() {
        let dir = tempfile::tempdir().expect("tmp");
        let layer = file_layer(dir.path(), 3, LOG_PREFIX, LogFormat::Text).expect("appender");
        let sub = tracing_subscriber::registry()
            .with(layer)
            .with(filter_from(None));
        tracing::subscriber::with_default(sub, || {
            tracing::info!(target: "norte_core::test", "one line");
        });

        let (how_many, text) = dump(dir.path());
        assert_eq!(how_many, 1, "one log file");
        assert!(text.contains("one line"), "the event is there: {text}");
    }

    /// `format = "json"`: one JSON line per event, with its fields AND the
    /// span chain it happened in (ADR 0127). This is what lets you follow a
    /// task and the request that asked for it with `jq`, with no regular
    /// expressions over text.
    #[test]
    fn json_format_carries_fields_and_spans() {
        let dir = tempfile::tempdir().expect("tmp");
        let layer = file_layer(dir.path(), 3, LOG_PREFIX, LogFormat::Json).expect("appender");
        let sub = tracing_subscriber::registry()
            .with(layer)
            .with(filter_from(None));
        tracing::subscriber::with_default(sub, || {
            let rpc = tracing::info_span!("rpc", conn_id = 3_u64, method = "fs.copy");
            let _r = rpc.enter();
            let task = tracing::info_span!("task", task_id = 7_u64);
            let _t = task.enter();
            tracing::warn!(entries = 2_u64, "a half-finished copy");
        });

        let (_, text) = dump(dir.path());
        let line = text.lines().next().expect("one line");
        let v: serde_json::Value = serde_json::from_str(line).expect("the line is JSON");
        assert_eq!(v["level"], "WARN");
        assert_eq!(v["fields"]["message"], "a half-finished copy");
        assert_eq!(v["fields"]["entries"], 2);
        assert_eq!(v["span"]["name"], "task", "the current span");
        assert_eq!(v["spans"][0]["name"], "rpc", "outside in");
        assert_eq!(v["spans"][0]["method"], "fs.copy");
        assert_eq!(v["spans"][1]["task_id"], 7);
    }

    /// And text stays the same as always: JSON is optional.
    #[test]
    fn default_format_is_text() {
        assert_eq!(LogFormat::default(), LogFormat::Text);
        let dir = tempfile::tempdir().expect("tmp");
        let layer = file_layer(dir.path(), 3, LOG_PREFIX, LogFormat::Text).expect("appender");
        let sub = tracing_subscriber::registry()
            .with(layer)
            .with(filter_from(None));
        tracing::subscriber::with_default(sub, || tracing::info!("one line"));
        let (_, text) = dump(dir.path());
        assert!(serde_json::from_str::<serde_json::Value>(text.trim()).is_err());
        assert!(text.contains("one line"));
    }

    /// **The `suppaftp` cap covers the FILE just as it covers stderr.**
    ///
    /// It is filtered in the registry, before any layer, so it should follow
    /// from the architecture — and that is exactly why it is checked:
    /// "should follow" is not a test, and what is at stake is a password in a
    /// file that PERSISTS, which is worse than one that passed through a
    /// terminal (hard rule 10).
    ///
    /// In BOTH formats (ADR 0127): the JSON layer is built separately, and a
    /// cap that depended on how the layer is set up would be silently
    /// bypassed.
    #[test]
    fn the_ftp_password_does_not_reach_the_file() {
        for format in [LogFormat::Text, LogFormat::Json] {
            let dir = tempfile::tempdir().expect("tmp");
            let layer = file_layer(dir.path(), 3, LOG_PREFIX, format).expect("appender");
            let sub = tracing_subscriber::registry()
                .with(layer)
                .with(filter_from(Some("trace,suppaftp=trace")));
            tracing::subscriber::with_default(sub, || {
                tracing::trace!(target: "suppaftp", "PASS hunter2");
                tracing::info!(target: "suppaftp", "connected");
            });

            let (_, text) = dump(dir.path());
            assert!(
                !text.contains("hunter2"),
                "the password reached the file ({format:?}): {text}"
            );
            assert!(text.contains("connected"), "and what does pass, passes");
        }
    }

    /// **The log directory is 0700, and the files that land inside it 0600.**
    ///
    /// What the log holds is the same as what the journal holds — every
    /// copy, every delete, every remote host you connect to — and the
    /// journal is 0600. With the default umask this came out 0755/0644,
    /// i.e. readable by any local account on the machine.
    #[cfg(unix)]
    #[test]
    fn the_log_cannot_be_read_by_just_anyone() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().expect("tmp");
        let dir = tmp.path().join("logs");
        let layer = file_layer(&dir, 3, LOG_PREFIX, LogFormat::Text).expect("appender");
        let sub = tracing_subscriber::registry()
            .with(layer)
            .with(filter_from(None));
        tracing::subscriber::with_default(sub, || {
            tracing::info!(target: "norte_core::test", "one line");
        });

        let mode =
            |p: &std::path::Path| std::fs::metadata(p).expect("stat").permissions().mode() & 0o777;
        assert_eq!(
            mode(&dir),
            0o700,
            "the directory, for its owner only: that is what makes what's inside unreachable"
        );
    }

    /// And the sweep tightens whatever was already there from previous days.
    ///
    /// Needed because `tracing-appender` opens its own files, with the umask
    /// and no way to choose a mode: today's comes out 0644 and so does
    /// tomorrow's. That not mattering depends on the directory's 0700, so the
    /// sweep is what fixes the case where the directory was lax at some
    /// point — an install predating this fix, say — and readable files were
    /// left inside.
    #[cfg(unix)]
    #[test]
    fn the_sweep_closes_up_files_that_were_already_there() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().expect("tmp");
        let dir = tmp.path().join("logs");
        std::fs::create_dir_all(&dir).expect("dir");
        let old = dir.join("norte.log.2026-08-01");
        std::fs::write(&old, b"from yesterday").expect("file");
        std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        // A file that is NOT ours is left alone: this directory is ours, but
        // the sweep only touches what carries our prefix.
        let foreign = dir.join("otra-cosa.txt");
        std::fs::write(&foreign, b"foreign").expect("file");
        std::fs::set_permissions(&foreign, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        create_dir_locked(&dir, LOG_PREFIX).expect("sweep");

        let mode =
            |p: &std::path::Path| std::fs::metadata(p).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode(&old), 0o600, "yesterday's log, closed");
        assert_eq!(mode(&foreign), 0o644, "what is not a log, untouched");
    }

    /// And it does NOT loosen the state directory that holds it.
    ///
    /// This is the one that really bites: `init_to_file` is the FIRST thing
    /// to touch `<state_dir>` in both frontends — before the journal, before
    /// anything — so a `create_dir_all` with no mode created `<state_dir>`
    /// at 0755. The journal comes later with its `DirBuilder::mode(0o700)`,
    /// which on a directory that ALREADY exists does no chmod at all: the
    /// 0755 stuck around forever, exposing the listing of `journal.db`,
    /// `lua-trust.toml` and the spool to any local account. On a fresh
    /// install, and silently.
    #[cfg(unix)]
    #[test]
    fn creating_the_log_does_not_loosen_the_state_directory() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().expect("tmp");
        let state = tmp.path().join("state").join("norte");
        let _layer = file_layer::<tracing_subscriber::Registry>(
            &state.join("logs"),
            3,
            LOG_PREFIX,
            LogFormat::Text,
        )
        .expect("appender");

        let mode = std::fs::metadata(&state)
            .expect("stat")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "the parent created along the way, also closed");
    }

    /// A directory that cannot be created does NOT bring the program down:
    /// it degrades. A log is diagnostic, and a diagnostic that prevents
    /// startup is worse than not having one.
    #[test]
    fn an_impossible_directory_degrades_instead_of_failing() {
        let dir = tempfile::tempdir().expect("tmp");
        // A FILE where the directory should go: `create_dir_all` fails.
        let occupied = dir.path().join("occupied");
        std::fs::write(&occupied, b"I am not a directory").expect("file");
        let layer =
            file_layer::<tracing_subscriber::Registry>(&occupied, 3, LOG_PREFIX, LogFormat::Text);
        assert!(layer.is_none(), "cannot create there, so there is no layer");
    }
}
