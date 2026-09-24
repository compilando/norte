//! What gets requested IN THE BACKGROUND to fill in what the listing left
//! half-done.
//!
//! Four probes with the same shape — launch, a `Receiver`, and supersession
//! as cancellation: dropping the receiver discards the response — and all of
//! them were born in the `ntc` binary's root, which is a crate DISTINCT from
//! this lib. They are the foundation the pane's long tasks hang off
//! (`crate::jobs`, when it lands) and the `cd` landing ritual, so they leave
//! before those do.
//!
//! [`Probed`] is the dedup: a stat that failed is not retried until the
//! listing is renewed, so as not to hammer a broken provider.

use norte_core::backend::Backend;
use norte_frontend::layout::SlotId;
use norte_proto::{Entry, Error, VPath};

use crate::viewer::Viewer;
use crate::viewer_open::{Modo, viewer_for_width};

/// VIEWPORT stat probe (#52): hydrates size/mtime for the VISIBLE entries the
/// lazy listing left as None — not just the focused one, or the Size/Date
/// columns stay blank in every other row. At most ONE batch in flight,
/// bounded to [`STAT_BATCH_MAX`] paths and resolved with
/// [`STAT_BATCH_CONCURRENCY`] concurrency (a remote session cannot afford N
/// RTTs in series). Dedup by `(pane, path)` in the run loop's `probed` set: a
/// failed stat is not retried until the listing is renewed (so as not to
/// hammer a broken provider). Each stat is bounded with a timeout
/// (`STAT_PROBE_TIMEOUT`): a hung provider does not block the batch forever.
pub struct StatProbe {
    /// The resolved stats: `(pane, path, entry)`. Dropping the receiver
    /// discards the batch, which is this probe's cancellation.
    pub rx: tokio::sync::oneshot::Receiver<Vec<(usize, VPath, Entry)>>,
}

/// Dedup for probe #52: `(pane, path)` already requested. Cleared on every
/// new listing (cd/refresh) — entries are born lazy again.
pub type Probed = std::collections::HashSet<(usize, VPath)>;

/// Row radius of the window that probe #52 hydrates around each pane's
/// cursor (an approximation of the viewport: the real height is decided by
/// the widget when painting). Covers a tall terminal with margin.
pub const STAT_WINDOW_RADIUS: usize = 64;

/// Cap on paths per batch for probe #52: whatever does not fit is requested
/// on the next round, already without the ones the previous batch hydrated.
pub const STAT_BATCH_MAX: usize = 64;

/// Simultaneous stats within a batch (#52): bounds requests in flight
/// against the daemon without serializing the whole screen's latency.
pub const STAT_BATCH_CONCURRENCY: usize = 8;

/// Timeout for the on-focus probe's stat (#52, MINOR-1): a hung remote
/// provider must not leave the probe in flight indefinitely — once the
/// deadline is past it is treated as a failure (the entry stays `None`, not
/// retried until the selection changes).
pub const STAT_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Launches the `StatProbe` batch: clones the `Backend` (cheap, internal
/// Arc) and the paths so the task does not hold the run loop's borrow.
/// Failures (a provider error or a timeout) simply do not come back — the
/// entry stays lazy and the run loop's dedup avoids retrying in a loop.
pub fn spawn_stat_probe(backend: &Backend, paths: Vec<(usize, VPath)>) -> StatProbe {
    use futures::StreamExt as _;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let b = backend.clone();
    tokio::spawn(async move {
        let hydrated: Vec<(usize, VPath, Entry)> = futures::stream::iter(paths)
            .map(|(pane, path)| {
                let b = b.clone();
                async move {
                    let entry = tokio::time::timeout(STAT_PROBE_TIMEOUT, b.stat(&path))
                        .await
                        .ok()
                        .and_then(Result::ok)?;
                    Some((pane, path, entry))
                }
            })
            .buffer_unordered(STAT_BATCH_CONCURRENCY)
            .filter_map(|r| async move { r })
            .collect()
            .await;
        let _ = tx.send(hydrated);
    });
    StatProbe { rx }
}

/// Stat probe for the diffs panel's selected row (#157).
/// A mold of [`StatProbe`], reduced to what that case needs: at most two
/// paths (the row's two sides), so neither `STAT_BATCH_CONCURRENCY` nor a
/// batch cap is needed — the selection itself already bounds how many must
/// be requested.
pub struct CompareStatProbe {
    /// The resolved stats: `None` = that side does not have the file.
    pub rx: tokio::sync::oneshot::Receiver<Vec<(VPath, Option<Entry>)>>,
    /// The comparison it was requested under (#198): the result is only
    /// valid for that one.
    pub generation: u64,
}

/// Launches probe #157: one `stat` per path, with the same timeout as the
/// normal pane's so it is not left in flight forever against a hung
/// provider. A failure (error or timeout) travels as `(path, None)` instead
/// of being dropped — unlike [`spawn_stat_probe`], here it DOES matter to
/// know what was requested and did not arrive: that is what
/// `App::hydrate_compare_size` uses to mark it probed and not retry it every
/// frame.
pub fn spawn_compare_stat_probe(
    backend: &Backend,
    paths: Vec<VPath>,
    generation: u64,
) -> CompareStatProbe {
    use futures::StreamExt as _;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let b = backend.clone();
    tokio::spawn(async move {
        let result: Vec<(VPath, Option<Entry>)> = futures::stream::iter(paths)
            .map(|path| {
                let b = b.clone();
                async move {
                    let entry = tokio::time::timeout(STAT_PROBE_TIMEOUT, b.stat(&path))
                        .await
                        .ok()
                        .and_then(Result::ok);
                    (path, entry)
                }
            })
            .buffer_unordered(STAT_BATCH_CONCURRENCY)
            .collect()
            .await;
        let _ = tx.send(result);
    });
    CompareStatProbe { rx, generation }
}

/// Plugin decorations fetch IN FLIGHT (G3b, ADR 0037): the target pane/dir
/// and the one-shot channel. A mold of [`StatProbe`] — ONE PER PANE
/// (#117-follow-up review MINOR-2: with a global slot, a cd in pane B
/// stomped on A's fetch in flight and its configured `plugin:` columns
/// stayed blank until A's next cd — with columns now config-driven that
/// contradicted "never a column permanently blank"). `dir` is kept to
/// discard a LATE response that no longer matches the pane's current
/// listing. Limit inherited from the decorations design (review MINOR-3):
/// `paths` is the page ALREADY listed when the cd settled — entries drained
/// LATER by incremental fill (#52/#54) do not travel in the request and
/// paint blank until the next re-list (documented, same scope as the
/// decorations).
/// `plugin:` column values by Display id → `VPath` → sanitized cell
/// (#117-follow-up) — the shape `PaneState::set_plugin_columns` consumes.
pub type PluginColumnValues =
    std::collections::HashMap<String, std::collections::HashMap<VPath, String>>;

/// The label its MANIFEST gives each plugin column, by Display id, already
/// sanitized: what `ColumnsSettings::apply_plugin_headers` installs so the
/// header does not show the id.
pub type PluginColumnHeaders = std::collections::BTreeMap<String, String>;

/// A plugin decorations and columns fetch in flight, per SLOT.
pub struct DecorateFetch {
    /// The SLOT it goes to, not the position: a late response has to land in
    /// the listing that requested it, not in whoever now occupies its spot.
    pub slot: SlotId,
    /// The dir it was requested under: if the slot is already on another
    /// one, it is dropped.
    pub dir: VPath,
    /// Decorations by path, and column values by plugin id.
    pub rx: tokio::sync::oneshot::Receiver<(
        std::collections::HashMap<VPath, norte_frontend::Decoration>,
        PluginColumnValues,
        PluginColumnHeaders,
    )>,
}

/// How often the log gets pulled from the daemon (#328).
///
/// The terminal repaints per frame and its loop wakes ten times a second, so
/// without this brake the open panel would be ten RPCs a second to show the
/// same thing. Half a second: a log is read, not timed to the second — and
/// it is the same cadence as the window, which does need its own timer
/// because it only repaints when someone does something.
pub const LOG_TAIL_PERIODO: std::time::Duration = std::time::Duration::from_millis(500);

/// One round of `log.tail` in flight (#328).
///
/// The EPOCH travels with the request: between asking and answering there is
/// room for the panel to close and reopen, and the previous session's
/// response has to die instead of landing — with its cursor — in the new
/// panel.
pub struct LogTailProbe {
    /// The panel opening it was requested under.
    pub epoca: u64,
    /// Whatever the daemon answered, whole: the error too, because
    /// [`Error::Unsupported`] is the fact that this daemon has no log to
    /// serve and asking should stop.
    pub rx: tokio::sync::oneshot::Receiver<Result<norte_proto::methods::LogTailResult, Error>>,
}

/// Launches a round of `log.tail` from where the cursor was left.
///
/// `cursor: None` the first time — "give me whatever there is" — and the
/// `next` that arrived afterward; never a zero, which against a ring that
/// already wrapped around would report a false `lost`.
pub fn spawn_log_tail(backend: &Backend, cursor: Option<u64>, epoca: u64) -> LogTailProbe {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let b = backend.clone();
    tokio::spawn(async move {
        let _ = tx.send(b.log_tail(cursor, crate::logview::MAX_REMOTO).await);
    });
    LogTailProbe { epoca, rx }
}

/// What is waited for the plugin catalogue, same as the window.
pub const PLAZO_PANELES: std::time::Duration = std::time::Duration::from_secs(5);

/// A plugin panel repaint in flight (phase 3).
///
/// Carries the SLOT and the SIGNATURE of what it requested: when it arrives,
/// if that slot already wants something else — the cursor moved, the panel
/// was resized — the response is dropped. Same rule as the preview, for the
/// same reason.
pub struct PanelRenderProbe {
    /// The slot the frame goes to.
    pub slot: SlotId,
    /// What was requested: if the slot already wants something else, it is
    /// not applied.
    pub firma: crate::panelplugin::Firma,
    /// The frame, or `None` if no consented plugin paints that panel.
    pub rx: tokio::sync::oneshot::Receiver<Result<Option<norte_proto::methods::PanelFrame>, Error>>,
}

/// Asks the core for a plugin panel's frame.
///
/// Fail-soft like everything cosmetic: if the call fails, the slot keeps its
/// previous frame — or empty, if there was none — and the reader sees no
/// error over something that only decorates.
///
/// No deadline, unlike the catalogue: the guest runs on the runtime's epoch,
/// which is what cuts it off if it overruns. A deadline here would be a
/// second clock over the same guest.
#[must_use]
pub fn spawn_panel_render(
    backend: &Backend,
    slot: SlotId,
    firma: crate::panelplugin::Firma,
    params: norte_proto::methods::PluginPanelRenderParams,
) -> PanelRenderProbe {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let b = backend.clone();
    tokio::spawn(async move {
        let _ = tx.send(b.plugin_panel_render(params).await);
    });
    PanelRenderProbe { slot, firma, rx }
}

/// The plugin catalogue in flight, to know which PANELS they contribute
/// (phase 3).
///
/// One per session and no epoch: there is no open panel it belongs to — what
/// it brings is the declaration of which slots exist, which comes before
/// opening any of them — and it is requested once at startup. If it ever
/// needs repeating (approving a plugin without restarting), this is the
/// place.
pub struct PanelsProbe {
    /// The whole catalogue: panels come out of `PluginInfo.panels`, and the
    /// approved/enabled filter is applied by `KindRegistry::insert_panels`,
    /// which is where that rule lives for both frontends.
    pub rx: tokio::sync::oneshot::Receiver<Result<norte_proto::methods::PluginListResult, Error>>,
}

/// Requests the catalogue to declare the panels plugins contribute.
///
/// Fail-soft like the rest of the cosmetic stuff: if the RPC fails, there
/// are no plugin panels and the screen is the usual one.
#[must_use]
pub fn spawn_panels(backend: &Backend) -> PanelsProbe {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let b = backend.clone();
    tokio::spawn(async move {
        // With the SAME deadline as the window (`PLAZO_PLUGINS`): a daemon
        // that does not answer leaves the session without plugin panels, not
        // a probe hung forever. The same decision had two answers.
        let res = match tokio::time::timeout(PLAZO_PANELES, b.plugins_list()).await {
            Ok(r) => r,
            Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
        };
        let _ = tx.send(res);
    });
    PanelsProbe { rx }
}

/// A `log.level` request to the daemon in flight (#328). Same mold and same
/// epoch as [`LogTailProbe`].
pub struct LogLevelProbe {
    /// The panel opening it was requested under.
    pub epoca: u64,
    /// The level that actually ended up set, which may not be the one
    /// requested: the daemon's ring is global to its clients and only goes
    /// up.
    pub rx: tokio::sync::oneshot::Receiver<Result<String, Error>>,
}

/// Asks the daemon to capture at least `level`.
pub fn spawn_log_level(
    backend: &Backend,
    level: norte_config::logline::LogLevel,
    epoca: u64,
) -> LogLevelProbe {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let b = backend.clone();
    let requested = level.wire().to_owned();
    tokio::spawn(async move {
        let _ = tx.send(b.log_level(&requested).await);
    });
    LogLevelProbe { epoca, rx }
}

/// A preview read in flight, per SLOT.
///
/// Keeps the path it requested: when it arrives, if the slot already wants
/// something else — the cursor moved while it was in flight — the response
/// is DROPPED. This is spec rule 3 and the lesson from P6's phase C, which
/// is the same thing.
pub struct PreviewFetch {
    /// The path that was requested: if the slot already wants another, the
    /// response is dropped.
    pub path: VPath,
    /// The built viewer, or the untranslated read error.
    pub rx: tokio::sync::oneshot::Receiver<Result<Viewer, Error>>,
}

/// Reads `path` in the background for slot `slot`.
///
/// No `select!` over the keyboard, unlike
/// [`crate::viewer_open::open_viewer`]: nobody is waiting in front of the
/// preview, so there is nothing to cancel with `Esc`. What there is is
/// supersession: moving the cursor drops this `Receiver` and the response is
/// lost without being applied.
///
/// `columns` is the slot's width in cells, for the previewer (0.66.0): `None`
/// when unknown, and the guest chooses.
///
/// Requests `Modo::Nada` from [`viewer_for_width`] without looking at
/// `[ui] images`: a slot's DOCKED viewer does not yet know how to place
/// pixels — that belongs to the full-screen viewer, T3/T4 of phase 5 WOW —
/// so requesting the thumbnail here would be a plugin-host call nobody uses.
pub fn spawn_preview_fetch(backend: &Backend, path: VPath, columns: Option<u32>) -> PreviewFetch {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let b = backend.clone();
    let p = path.clone();
    tokio::spawn(async move {
        let res = viewer_for_width(&b, &p, columns, Modo::Nada).await;
        let _ = tx.send(res.map(|(viewer, _imagen)| viewer));
    });
    PreviewFetch { path, rx }
}

/// Launches the decorations fetch (G3b) for ALL of `pane`'s currently listed
/// entries (the "visible page" — the listing ALREADY loaded, whether that is
/// a large dir's first page while it paginates or the whole dir; the rest of
/// a dir still filling in stays undecorated until the next visit, same
/// documented MVP scope as in the ADR/plan). No special "some decorator
/// enabled" guard: `Backend::plugin_decorate` resolves the catalogue on
/// every call (cheap when embedded, one remote RPC) — trying it on every
/// listing and silently dropping it when there are no consented decorators
/// is simpler and more honest than caching a flag that could go stale after
/// an F12.
/// `None` if the pane has no entries (nothing to decorate).
/// #117-follow-up: the SAME trip also brings the scheme's CONFIGURED
/// `plugin:` column values (`plugin_cols` = (plugin, column) pairs from
/// `ColumnsSettings::plugin_ids_for`) — a single slot in flight, a single
/// anti-stale guard.
pub fn spawn_decorate_fetch(
    backend: &Backend,
    slot: SlotId,
    dir: VPath,
    paths: Vec<VPath>,
    // Each path's class, positional (ADR 0105): an icon decorator needs it
    // for the folder.
    kinds: Vec<norte_proto::EntryKind>,
    plugin_cols: Vec<(String, String)>,
) -> Option<DecorateFetch> {
    if paths.is_empty() {
        return None;
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    let b = backend.clone();
    tokio::spawn(async move {
        let plugins = b.plugin_decorate(&paths, &kinds).await.unwrap_or_default();
        let merged = norte_frontend::merge_decorations(&paths, &plugins);
        // Review MINOR-1 (rule 3 in spirit): a SUPERSEDED fetch (the run loop
        // stomped the slot → rx dropped) cuts short before each remaining
        // RPC instead of spending up to 12 calls (8 painted columns and 4
        // for the status bar, ADR 0137) whose send would fail anyway.
        let (cols, headers) =
            fetch_plugin_columns(&b, &plugin_cols, &paths, || tx.is_closed()).await;
        let _ = tx.send((merged, cols, headers));
    });
    Some(DecorateFetch { slot, dir, rx })
}

/// Values of the configured `plugin:` columns (#117-follow-up): membership
/// validation + collision dedupe lives in the SHARED model
/// (`norte_frontend::columns::validated_plugin_requests` — review MAJOR-1: a
/// single definition for both frontends; a bare-id collision = blank before
/// false attribution, real disambiguation = issue #120). Fail-soft per
/// column: a down catalogue or a failed RPC = blank cells, never a listing
/// error. `superseded` cuts short between RPCs when the fetch has already
/// been stomped (review MINOR-1).
async fn fetch_plugin_columns(
    backend: &Backend,
    requested: &[(String, String)],
    paths: &[VPath],
    superseded: impl Fn() -> bool,
) -> (PluginColumnValues, PluginColumnHeaders) {
    let mut out = std::collections::HashMap::new();
    let mut headers = std::collections::BTreeMap::new();
    if requested.is_empty() {
        return (out, headers);
    }
    let Ok(list) = backend.plugins_list().await else {
        return (out, headers);
    };
    for (plugin, column) in
        norte_frontend::columns::validated_plugin_requests(requested, &list.plugins)
    {
        // The label its manifest gave it, so the header does not show the
        // id. Text from a plugin: masked and bounded like any header.
        if let Some(h) = list
            .plugins
            .iter()
            .find(|p| p.id == plugin)
            .and_then(|p| p.columns.iter().find(|c| c.id == column))
        {
            let sane: String = norte_frontend::columns::sanitize_header(&h.header)
                .chars()
                .take(norte_frontend::columns::HEADER_MAX_CHARS)
                .collect();
            if !sane.is_empty() {
                headers.insert(
                    norte_frontend::columns::plugin_display_id(&plugin, &column),
                    sane,
                );
            }
        }
        if superseded() {
            return (out, headers);
        }
        let raw = backend
            .plugin_column_values(&plugin, &column, paths)
            .await
            .unwrap_or_default();
        let sanitized = norte_frontend::columns::sanitize_column_values(paths, &raw);
        out.insert(
            norte_frontend::columns::plugin_display_id(&plugin, &column),
            sanitized,
        );
    }
    (out, headers)
}
