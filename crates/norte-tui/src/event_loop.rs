//! The TUI's event loop, and what can bring it down.
//!
//! For now only the error. The loop (`run`, ~2,500 lines) is still in the
//! `ntc` binary's root and comes in the next round; the error goes first
//! because it is what makes that possible: rule 6 forbids `anyhow` in a
//! library, and `run` returned `anyhow::Result<()>`.
//!
//! The wall turned out to be FOUR lines —two `terminal.size()`, one
//! `terminal.draw()` and a `.context("terminal event")`— so two variants
//! cover it. Both carry a `std::io::Error` inside; they are split because
//! what that `.context` contributed was WHICH of the two surfaces failed,
//! and a single `Io(..)` would lose it: a terminal that will not be measured
//! is not diagnosed the same as an event stream that cuts off.
//!
//! These texts do NOT go through Fluent, and that is deliberate: they are
//! not interface strings but the diagnostic `anyhow` prints when the process
//! dies, which is exactly the use the previous `.context` gave them.

use crate::app::{App, CompareState, SearchState, detail_for_bar, error_category};
use crate::config::{self, Layers};
use crate::config_reload::reload_config;
use crate::console::Console;
use crate::dispatch::dispatch;
use crate::fill::{Fill, apply_fill_msg};
use crate::gestures::launch_opener;
use crate::jobs::{
    InFlight, SearchRun, SyncTick, drain_compare, drain_search, drain_sync_plan, harvest_ai_rename,
    harvest_checksum, harvest_rename_batch, harvest_semantic, harvest_sync_apply,
};
use crate::keymap::{Command, Resolver};
use crate::keys::on_key;
use crate::lua::{RunOutcome, load_lua, start_lua_run};
use crate::mouse;
use crate::nav;
use crate::navigate::{cd, request_decorations, settle_cd};
use crate::overlays::watch_refresh_allowed;
use crate::paste::route_paste;
use crate::probes::{DecorateFetch, Probed};
use crate::refresh::{after_panes_refresh, on_tick, reap_search_run, refresh_panes};
use crate::screens::pane_attr_ids;
use crate::session_push::{
    JOURNAL_IDLE, SessionPush, capture_session, drain_notices, push_session,
};
use crate::tty;
use crate::turn;
use crate::ui;
use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use norte_core::backend::{Backend, ConnEvent};
use norte_frontend::layout::BySlot;
use norte_i18n::{t, ta};

/// What aborts the event loop.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// The terminal would not be measured (`size`) or painted (`draw`).
    #[error("the terminal did not respond: {0}")]
    Terminal(#[source] std::io::Error),
    /// The terminal's event stream broke. A stream that simply RUNS OUT is
    /// not this: closing the window exits through the same place as an
    /// `app.quit`, so as not to lose the session's last snapshot.
    #[error("terminal event: {0}")]
    Event(#[source] std::io::Error),
}

/// The TRANSLATED sentence for "this session is not being recorded"
/// (#167/#177).
///
/// `NoJournal::text()`'s text is for the operator's log and goes raw; this
/// is interface, and this binary's interface goes through Fluent.
///
/// The two arms say DIFFERENT things since #178: `Busy` is "this happened
/// and was not recorded" and `Failed` is "this has not happened". Sharing a
/// sentence was the bug.
fn journal_warning_i18n(why: &norte_core::embedded::NoJournal) -> String {
    use norte_core::embedded::NoJournal as N;
    match why {
        N::Busy => t("msg-journal-busy"),
        // `detail_for_bar` and not the raw `Display`: the reason is
        // `sqlx`/`JournalError`'s error, which carries whole paragraphs (the
        // `Corrupt` ones) and text derived from environment paths. The
        // status bar has a sanitizer for exactly this, and everything else
        // goes through it.
        N::Failed(reason) => ta(
            "msg-journal-refused",
            &[("motivo", &crate::app::detail_for_bar(reason))],
        ),
        // `#[non_exhaustive]`: a new reason cannot stay silent — if one ever
        // shows up, at least the core's text comes through.
        other => other.text(),
    }
}

/// Runs a command and settles its outcome: the `cd` it may carry
/// ([`settle_cd`]) and the live search's harvest —rule 3: a cd turns off the
/// virtual pane and its Task has to die with it, not on the next key.
///
/// What it does NOT do, on purpose, is launch the external command
/// `pane.open` might have left resolved: only the sites that are the user's
/// ENTRY over a listing do that, and it is read at each one
/// ([`launch_pending_open`]). The key resolver does not go through here
/// either: it needs to read `nav_stalled` BEFORE the outcome is consumed, and
/// resets the counter with it.
#[expect(clippy::too_many_arguments, reason = "loop wiring, not an API")]
pub(crate) async fn run_command(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
    help_lines: &mut Vec<ratatui::text::Line<'static>>,
    lang: norte_i18n::Lang,
    quick_mode: nav::Mode,
    confirm_quit: config::ConfirmQuit,
    cfg: &config::LoadedConfig,
    fill: &mut BySlot<Fill>,
    decorate_fetch: &mut BySlot<DecorateFetch>,
    last_probed: &mut Probed,
    search_run: &mut Option<SearchRun>,
    cmd: Command,
) {
    let outcome = dispatch(
        app,
        backend,
        events,
        help_lines,
        lang,
        quick_mode,
        confirm_quit,
        cfg,
        cmd,
    )
    .await;
    settle_cd(
        app,
        backend,
        fill,
        decorate_fetch,
        last_probed,
        search_run,
        outcome,
    );
    reap_search_run(app, search_run);
}

/// Launches the external command `pane.open` (#28) may have left resolved.
/// Lives in the run loop because it is the one with the terminal: opening an
/// opener suspends it. Launches whatever dispatch left requested, with the
/// terminal the console holds —which has held it since a long wait needed
/// repainting.
///
/// With no terminal (a detached console) nothing gets launched AND WHATEVER
/// IS PENDING IS DISCARDED: leaving it set would fire it at the next site
/// that does have a terminal, long after the key that requested it.
pub(crate) async fn launch_pending(
    app: &mut App,
    console: &mut crate::console::Console<'_>,
    capture: &mut mouse::Capture,
) {
    let Some(pending) = app.pending_open.take() else {
        return;
    };
    if let Some(terminal) = console.terminal() {
        app.message = Some(launch_opener(terminal, capture, pending).await);
    }
}

/// The event loop: draws, waits, routes the key and drains whatever the
/// background tasks have brought, until `app.quit`.
///
/// Never had rustdoc, and until this commit did not need it: it was a
/// private function in a binary's root. On becoming `pub` in a library,
/// `missing_docs` requires it, and it is worth stating here the three things
/// its body repeats that no caller can guess.
///
/// **It does not own the terminal, it borrows it.** `main` creates it and
/// restores it; here mouse capture is SWITCHED ON and OFF hot (`[ui] mouse`)
/// and released around every suspension. If this function returns `Err`, the
/// terminal is still `main`'s responsibility, which restores it BEFORE
/// propagating — a broken loop is not a cancelled `--pick`.
///
/// **Clean exit goes through a single place.** `app.quit` is the only path,
/// and that is why the terminal's EOF (the window gets closed on you) turns
/// it on instead of doing a `return`: exiting earlier used to lose the
/// session's last snapshot.
///
/// **Its parameters are state a reload can replace.** The three resolvers,
/// `help_lines`, the quick search's mode and `confirm_quit` are taken by
/// `&mut` because [`crate::config_reload::reload_config`] replaces them hot,
/// and replaces ALL of them or NONE.
///
/// # Errors
///
/// [`RunError`], which are the two terminal surfaces that can really fail:
/// measuring it or painting it, and its event stream. Everything else that
/// goes wrong —a listing that will not be read, a rejected mutation, a
/// plugin that does not respond— is a message in the bar, not an error from
/// the loop: the rule is that the file manager does not crash because the
/// filesystem says no.
#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "loop wiring, not an API"
)]
pub async fn run(
    terminal: &mut tty::Tui,
    // Mouse capture: created by `main` (the terminal's owner) and withdrawn
    // on exit; here it is SWITCHED ON and OFF hot (`[ui] mouse`) and
    // released around every suspension for an external opener.
    capture: &mut mouse::Capture,
    app: &mut App,
    backend: &Backend,
    resolver: &mut Resolver,
    viewer_resolver: &mut Resolver,
    dialog_resolver: &mut Resolver,
    help_lines: &mut Vec<ratatui::text::Line<'static>>,
    // H3b: the NEGOTIATED language (`NORTE_LANG` > `[ui] lang` > environment,
    // the same value handed to `norte_i18n::force`). The help corpus is
    // per-locale, so the overlay must open on the locale the rest of the UI
    // already speaks — `Lang::from_env()` here would hand a reader whose
    // `[ui] lang` says `es` an English corpus inside a Spanish UI. Fixed for
    // the session: `force` is called once, so the hot reload keeps this value.
    lang: norte_i18n::Lang,
    // `mut` since the profiles feature (ADR 0079): switching profile rebuilds
    // the layers hot, so this is no longer constant for the session.
    mut layers: Layers,
    cli_preset: Option<String>,
    // Quick search's mode (`[ui] quick_search`): lives in the run loop like
    // the CLI preset and gets updated on config's hot reload.
    mut quick_mode: nav::Mode,
    // `[ui] confirm_quit` (S2): same pattern as `quick_mode` — lives in the
    // run loop, `applies_live` (only affects NEW `app.quit`s, one already in
    // progress has already decided) and gets updated on hot reload.
    mut confirm_quit: config::ConfirmQuit,
    // S3 (`app.settings`): the WHOLE config lives here, not just the loose
    // fields above — the settings overlay needs to read ANY entry of the
    // catalogue (`crate::settings::build_rows`), not a fixed list. Updated
    // WHOLE on every OK hot reload (`reload_config`, at the end, after
    // everything else applies — same criterion as `quick_mode`/
    // `confirm_quit`: only if EVERYTHING applied).
    mut cfg: config::LoadedConfig,
    mut cfg_rx: tokio::sync::mpsc::Receiver<()>,
    mut foreign_tasks: Option<tokio::sync::mpsc::UnboundedReceiver<norte_core::backend::TaskRef>>,
    mut conn_events: Option<tokio::sync::mpsc::UnboundedReceiver<ConnEvent>>,
    mut approvals: Option<
        tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::PolicyApprovalRequired>,
    >,
    mut degraded: Option<
        tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::ConnectionDegraded>,
    >,
    mut failed: Option<
        tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::ConnectionFailed>,
    >,
    mut plugin_notices: Option<
        tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::PluginNotice>,
    >,
    mut journal_warnings: Option<
        tokio::sync::mpsc::UnboundedReceiver<norte_core::embedded::JournalStatus>,
    >,
) -> Result<(), RunError> {
    let mut events = EventStream::new();
    // Alt alone opens the menu (`[ui] alt_menu`). It only gets to see a bare
    // Alt if the kitty protocol was requested; without it, nothing of the
    // sort comes through.
    let mut alt_solo = crate::alt_menu::AltSolo::default();
    // Tasks panel tick: copies snapshots from the watch (never blocks).
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // L2: the session is written ONCE per second, not per key. Its own tick
    // and not the 100 ms one because they are two different rhythms: the
    // tasks panel watches an in-memory `watch` and this ends up in a file.
    let mut session_tick = tokio::time::interval(std::time::Duration::from_secs(1));
    session_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut session_push = SessionPush::start(backend, app.session.revision);
    // Hot-reload debounce WITHOUT blocking the loop (phase 6 review): every
    // config event pushes the deadline; the reload runs when it expires.
    let mut reload_at: Option<tokio::time::Instant> = None;
    // Paginated listings filling in the background (ADR 0017): one slot PER
    // PANE — both panes can be paginating at once, and with a global slot one
    // pane's cd killed the other's drainer (see [`Fill`]).
    // Where `work.fill`'s sweep continues (see the `select!`'s arm).
    // Live search in progress (liveSearch T6): at most one (there is one
    // virtual pane). `Fill` shape: drained in the select and released on
    // exit.
    // Directory comparison in progress (`Shift+F2`): at most one — there is
    // one compare panel. Same shape as `work.search`.
    // Sync in progress (`Ctrl+Y`): at most one — there is one panel, and
    // approving a plan while another applies would be approving blind.
    // In-flight `ai.rename_plan` request (M4-IA): at most one — relaunching
    // aborts the previous one. Harvested in the select and Esc (BROWSE)
    // cancels it.
    // AI plan ready that arrived with ANOTHER modal open: RETAINED here
    // (`App`'s queue is specific to approvals) and opened as soon as there
    // is no modal — never overwrite (`open_next_pending`'s discipline).
    // In-flight `fs.rename_batch_plan` request (§17): at most one, harvested
    // in the select like `work.ai_rename`.
    // In-flight semantic search (M4-IA-2): same shape as `work.ai_rename` —
    // at most one, relaunching aborts the previous one, Esc (BROWSE) cancels.
    // Hits ready that arrived with ANOTHER modal open: RETAINED here and
    // opened as soon as there is no modal (`work.pending_ai_plan`'s
    // discipline).
    // Lua scripting (M4, ADR 0026): host layered with TOFU trust. Like
    // `work.fill`, the state lives in the run loop. The in-flight run (at
    // most ONE: there is one Lua state) is polled inline in the select —
    // `CommandRun` is `!Send` and this future runs in `block_on`, never in
    // `spawn`.
    let mut lua_host = load_lua(app, &layers).await;
    // On-focus stat probe (#52, lazy listing): at most one in flight, dedup
    // by (pane, path) — two panes over the SAME dir must each hydrate their
    // own (does not retry a failed stat until the selection changes).
    // Stat probe of the compare panel's SELECTED row (#157): same shape as
    // `work.stat`, at most one in flight. The dedup lives in
    // `App::compare_size_probed` and not in a run-loop local variable
    // (unlike `work.probed`) because `App::compare_size_probe_targets`
    // already queries it to decide what is still missing.
    // In-flight plugin decoration fetch (G3b, ADR 0037): at most one, shape
    // of `work.stat`/`work.fill`.
    // L3: one in-flight preview read per slot, superseded on move.
    // #106 (watching): watching the visible dirs — notify with a fallback
    // to polling (the inotify pitfall). The watched set re-syncs on EVERY
    // turn (cheap diff, a no-op with no changes).
    // Rule 2, a one-off exemption (review MINOR-6): creating the watcher and
    // rewatch's watch()/unwatch() are short inline syscalls (same documented
    // criterion as ratatui's synchronous draw further below); they only run
    // on startup or on CHANGING dir.
    // Everything this loop has left pending and not yet harvested (see
    // [`InFlight`]): fills, probes and the background jobs.
    let mut work = InFlight::default();
    // The INITIAL listings also get decorated (ADR 0105): `main` builds them
    // without going through `settle_cd`, and without this a freshly opened
    // `ntc` had neither an icon nor a badge until the first `cd`.
    for pane in 0..app.panes.len() {
        request_decorations(app, backend, &mut work.decorate, pane);
    }
    // Which PANELS the plugins contribute (phase 3): once, at startup. What
    // it brings is the declaration of which slots exist —not any of their
    // content— and without it a saved layout that includes a plugin panel
    // would open with the slot undeclared: it neither gets placed, nor
    // focused, nor shows up in the bar.
    work.panels = Some(crate::probes::spawn_panels(backend));
    let mut dir_watch = norte_frontend::watch::DirWatch::new();
    let mut dir_watch_alive = true;
    loop {
        dir_watch.rewatch(&watch_targets(app));
        if dir_watch.take_degraded_notice() {
            app.message = Some(t("status-watch-degraded"));
        }
        // The manager changed governance or a plugin's settings: whatever
        // the plugins said about each listing is forgotten and requested
        // again. `set` on the slot drops the in-flight batch, so a response
        // from before the change does not land.
        if std::mem::take(&mut app.redecorate) {
            for pane in &mut app.panes {
                pane.set_decorations(std::collections::HashMap::new());
                pane.set_plugin_columns(std::collections::HashMap::new());
            }
            for pane in 0..app.panes.len() {
                request_decorations(app, backend, &mut work.decorate, pane);
            }
        }
        // Each panel's footer free space (spec 2026-09-10): requested when a
        // listing lands or refreshes, and only if the footer is on — with it
        // off nobody needs the mount table. A failure leaves the cache as it
        // was: the footer stays silent about the space rather than inventing
        // it.
        //
        // INLINE but BOUNDED (M5 review): `Backend` is not `Clone`, so it
        // cannot be launched into a task the way the window does, and the
        // enumeration does one `statvfs` per mount with a 200 ms deadline
        // each — a hung network mount used to stop the whole loop. The
        // 250 ms cap is the maximum price per listing; past it, the footer
        // keeps the previous table, which still has the right mount.
        if app.chrome.pane_footer()
            && std::mem::take(&mut app.volumes_stale)
            && let Ok(Ok(vols)) = tokio::time::timeout(
                std::time::Duration::from_millis(250),
                backend.volumes(false),
            )
            .await
        {
            app.volumes = vols;
        }
        turn::drain_pending(
            app,
            backend,
            capture,
            &mut Console::new(&mut events, terminal),
            &mut work,
        )
        .await;
        turn::open_retained_modals(app, &mut work);
        // Phase 9, the HANDOFF's two halves, and here and not on the
        // one-second tick: the reader just pressed it, and waiting a tick to
        // send the order is noticeable. The session writer belongs to the
        // loop, so the channel's plumbing lives here — same split as the
        // rest of the `pending_*` fields.
        if std::mem::take(&mut app.pending_handoff) {
            let _ = crate::session_push::request_handoff(app, &mut session_push);
        }
        crate::session_push::drain_notices(app, &mut session_push);
        if std::mem::take(&mut app.handoff_ready) {
            // The screen is written and the session, released: the window
            // launches with `--attach` and this process leaves. `--attach`
            // is what brings the marks back; without it, the window would
            // open where you were but without what you had pointed at.
            //
            // And it only leaves if the window is STILL ALIVE after a
            // moment. A successful `spawn` only says the process started:
            // the window that did not know `--attach` used to exit with code
            // 2 while this terminal was already gone, leaving the reader
            // with neither. If it dies, staying is correct —the screen is
            // still in the core and this process is still showing it— and
            // the session is RECLAIMED, having been released for the window
            // that never arrived.
            let outcome = match crate::handoff::spawn_window(&crate::handoff::window_argv()) {
                Ok(child) => crate::handoff::wait_startup(child, crate::handoff::GRACE)
                    .await
                    .map_err(|code| code.map_or_else(|| "?".to_owned(), |c| c.to_string())),
                Err(e) => {
                    tracing::warn!(error = %e, "the handoff could not launch the window");
                    Err(e.to_string())
                }
            };
            match outcome {
                Ok(()) => app.quit = true,
                Err(reason) => {
                    app.message = Some(ta("msg-handoff-window-died", &[("reason", &reason)]));
                    crate::session_push::reclaim_soon(&mut session_push);
                }
            }
        }
        turn::prepare_frame(app, backend, terminal, lua_host.as_ref()).await?;
        // A one-off exemption from rule 2: the draw writes the control
        // terminal synchronously (ratatui's official async pattern; bounded,
        // multi-thread runtime).
        let painted_area = terminal
            .draw(|f| ui::draw(f, app))
            .map_err(RunError::Terminal)?
            .area;
        // T4 (phase 5 WOW): the pixels go AFTER the frame and outside
        // ratatui — an APC does not fit in a cell, and ratatui paints cells
        // (`draw_viewer` already left the slot empty when there is an
        // image). The rect comes from `ui::rect_del_visor(app, painted_area)`
        // — the SAME function `draw_viewer` uses, not a copy (two counts of
        // the same slot diverge silently) — and is already the INTERIOR with
        // no borders (review, CRITICAL 2).
        //
        // Covers two of the four erase moments on its own (closing the
        // viewer, moving it to another file): both change `app.viewer_imagen`
        // BEFORE this point in the same loop turn, so the comparison below
        // against what is really on the terminal
        // (`kitty_graphics::delete_placed`/`ya_placed`, PROCESS state)
        // already resolves it with no extra code. The other two —handing
        // over the terminal and quitting— have no guaranteed next frame to
        // do this count, and that is why they call `delete_placed`
        // directly (`suspend::suspend_terminal`, `tty::restore`).
        //
        // A painting failure NEVER brings the TUI down (the painting rule):
        // it is swallowed with a `tracing::debug!`.
        {
            use std::io::Write as _;
            // `ui::image_to_place` looks at EVERYTHING needed to decide
            // whether pixels get placed this frame — the SAME function
            // `panels::draw_viewer` uses to decide whether to blank the slot
            // (branch review, finding 2: these used to be two separate
            // counts that could diverge, see its rustdoc):
            // - there is an image requested, for the file the viewer shows
            //   RIGHT NOW (not one from a previous file, left hanging in the
            //   window between changing `app.viewer` and `viewer_open`
            //   updating `app.viewer_imagen`);
            // - there is NOTHING painted over the viewer this frame: kitty's
            //   pixels go in front of the text and survive any repaint of
            //   cells, so without this guard an F1 or a palette open over
            //   the viewer used to be covered by the thumbnail;
            // - the rect is not empty (review, round 2, breakage of CRITICAL
            //   2): a `rect` with zero width or height (a very short
            //   terminal, the body left with no room after the bars) is not
            //   filtered out — and in kitty `c=0,r=0` means the image's
            //   NATURAL size, so without this guard a thumbnail would be
            //   placed at pixel size over the whole screen.
            //
            // Computed BEFORE borrowing `app.viewer_imagen` mutably: it looks
            // at the whole `App`, and an already-live mutable borrow of one
            // of its fields would prevent that.
            let placement = ui::image_to_place(app, painted_area);
            match (&mut app.viewer_imagen, placement) {
                (Some(image), Some(placement)) => {
                    let rect = placement.rect;
                    // IMPORTANT 4: without this shortcut, a STILL viewer used
                    // to retransmit the whole PNG (up to 1920 px per side, in
                    // base64) on every frame — and the loop turns even when
                    // nobody types, `session_tick` wakes it once a second.
                    // `placed_in` exists for exactly this: if the id is
                    // already the placed one AND the rect did not change,
                    // there is nothing to redo.
                    let already_placed = crate::kitty_graphics::ya_placed(image.id)
                        && image.placed_in == Some(placement);
                    if !already_placed {
                        let out = terminal.backend_mut();
                        crate::kitty_graphics::delete_placed(out);
                        let esc = crate::kitty_graphics::escape_place(
                            image.id,
                            &image.bytes,
                            rect,
                            placement.crop,
                        );
                        // CRITICAL 1: `a=T` places at the CURSOR's position,
                        // and after `terminal.draw` the cursor is left
                        // wherever the last run of repainted cells ended —
                        // arbitrary, and changes frame to frame. The cursor
                        // is moved to the rect BEFORE the APC; `C=1` (in
                        // `escape_place`) keeps placing from moving it in
                        // turn (and potentially scrolling the screen if it
                        // falls on the last row).
                        let written =
                            crossterm::execute!(out, crossterm::cursor::MoveTo(rect.x, rect.y))
                                .and_then(|()| out.write_all(esc.as_bytes()))
                                .and_then(|()| out.flush());
                        match written {
                            Ok(()) => {
                                crate::kitty_graphics::mark_placed(image.id);
                                image.placed_in = Some(placement);
                            }
                            Err(e) => {
                                // MINOR 7: a failure midway through writing
                                // the APC leaves the terminal waiting for its
                                // close — everything painted afterward would
                                // read as its payload. The terminator is
                                // ALWAYS written.
                                let _ = out.write_all(b"\x1b\\");
                                tracing::debug!(
                                    error = %e,
                                    id = image.id,
                                    "could not place the viewer's image"
                                );
                            }
                        }
                    }
                }
                _ => crate::kitty_graphics::delete_placed(terminal.backend_mut()),
            }
        }
        if app.quit {
            // The last snapshot, and waiting for it. The one-second tick
            // loses whatever happened within that second, and quitting is
            // when it hurts most: until now, closing norte right after a
            // `cd` used to save the previous directory.
            //
            // Without the modal's gate, on purpose: an open modal means
            // "do not save what I am deciding", and here nothing is being
            // decided any more — it is quitting, and what has to be saved is
            // where things stood.
            // The subshell dies WITH norte (#142), and it does so through
            // `Subshell`'s `Drop` and not here: the loop also exits through
            // `RunError`, and a close that only covered this branch would
            // leave an orphaned shell right when the terminal broke.
            drain_notices(app, &mut session_push);
            let last = (!app.session.detached)
                .then(|| capture_session(app, &mut session_push))
                .flatten();
            session_push.close(last).await;
            return Ok(());
        }
        turn::after_frame(app, backend, &mut work, painted_area).await;
        // The `brief` splash screen expires by the PAINT clock, the same one
        // that expires notices: measuring it with another would be a
        // deadline tests cannot fix. Comes after the frame because what it
        // promises is "it is seen, and it removes itself", not "it is
        // removed before being seen".
        crate::splash::tick(app);
        // The row the reader chose by its number: navigated here, where the
        // backend is. Today every splash row carries a directory, so this is
        // an ordinary `cd` —with its return ritual— and not a second door to
        // the dispatcher.
        if let Some((_, Some(arg))) = app.pending_splash_row.take() {
            match norte_proto::VPath::parse(&arg) {
                Ok(destination) => {
                    let outcome = cd(
                        app,
                        backend,
                        &mut Console::new(&mut events, terminal),
                        destination,
                    )
                    .await;
                    settle_cd(
                        app,
                        backend,
                        &mut work.fill,
                        &mut work.decorate,
                        &mut work.probed,
                        &mut work.search,
                        outcome,
                    );
                }
                // A row with a path that does not parse does not navigate
                // and says so: this session wrote it, so if it happens it is
                // our bug.
                Err(_) => app.message = Some(t("err-invalid-path")),
            }
        }
        // The processes panel that opens and closes on its own (`[ui]
        // processes_panel = "auto"`, spec 2026-09-15): a panel taking up a
        // third of the screen to say "nothing running" does not earn its
        // place, and looking for its key right as a copy starts does not
        // either.
        //
        // Opens WITHOUT taking the keyboard —the reader is on their
        // listing— and only closes what it opened itself: a panel a person
        // opened stays.
        if app.chrome.processes_panel() == norte_config::load::ProcessesPanel::Auto {
            // Only work that LASTS opens the panel (ADR 0146): a search has
            // its own list, and a copy that finishes in a second is counted
            // by the status bar without taking a third of the listing away.
            //
            // CLOSES as always, when no work row is left: that is how the
            // one that took a while is seen as finished.
            //
            // The strip is re-noted HERE and not only on the tick: this turn
            // may come from a key, and with the previous tick's strip the
            // panel would open for a burst that already ended and close on
            // the next one — the flicker all of this exists to avoid.
            app.note_strip();
            let opens = app.strip.wants_panel(app.now_ms());
            let has_tasks = app
                .board
                .rows()
                .iter()
                .any(|r| norte_frontend::tasks::counts_as_work(r.last.kind));
            if opens && app.processes_slot().is_none() {
                app.open_processes(false);
                app.processes_auto = true;
            } else if !has_tasks && app.processes_auto {
                app.close_processes();
                app.processes_auto = false;
            }
        }
        turn::spawn_probes(app, backend, &mut work);
        tokio::select! {
            _ = session_tick.tick() => {
                // One more second for the bar's notice (spec 2026-09-10).
                app.tick_notices();
                push_session(app, &mut session_push);
                // #179: release the journal once it has gone a while unused.
                // This process used to take it on the first mutation and
                // never return it until quitting, so a copy at 9am left
                // `norte daemon run` and `norte audit` unable to open it for
                // the whole day. The window reopens itself on the next
                // mutation, and reopening RE-READS the chain, which is what
                // makes it safe.
                //
                // In the arm's BODY and not its condition: the close is NOT
                // cancel-safe, and dropping it halfway leaves the pool dying
                // in sqlx's worker and the next `resolve` colliding with our
                // own lock — a "session not recording" warning we would have
                // invented ourselves.
                backend.release_journal_if_idle(JOURNAL_IDLE).await;
            }
            _ = tick.tick() => {
                // Mutation finished → panes refresh; the complete ritual
                // (drainer/probe #52/search) lives in `after_panes_refresh`
                // — the SINGLE one for the refresh's three triggers (#117).
                let refreshed = on_tick(app, backend, &mut Console::new(&mut events, terminal)).await;
                after_panes_refresh(app, refreshed, &mut work.fill, &mut work.probed, &mut work.search);
            }
            ev = dir_watch.rx.recv(), if dir_watch_alive && watch_refresh_allowed(app) => {
                // #106: an EXTERNAL change in a watched dir (debounced) —
                // same path as pane.refresh (Ctrl+R): cancelable refresh +
                // ritual #118. GATED (review MAJOR-2): with an overlay/quick
                // search open, `refresh_panes` would swallow the user's keys
                // and Esc would change meaning — the precondition leaves the
                // event QUEUED (capacity-1 channel) and fires once the
                // overlay closes.
                if let Some(()) = ev {
                    // The disk map is a snapshot from a while ago, and this
                    // says something changed — but does NOT say what, so the
                    // only honest thing is to measure again. The flag is
                    // switched on and `drain_pending` drains it with the
                    // backend in hand; measuring here would leave the loop
                    // waiting on a whole tree.
                    if app.disk_map_slot().is_some() {
                        app.disk_map_stale = true;
                    }
                    let refreshed =
                        refresh_panes(app, backend, &mut Console::new(&mut events, terminal)).await;
                    after_panes_refresh(
                        app,
                        refreshed,
                        &mut work.fill,
                        &mut work.probed,
                        &mut work.search,
                    );
                } else {
                    // Unreachable with `dir_watch` alive (it holds the raw
                    // sender): if it happened, DISARM the arm — a closed
                    // channel would return None in a loop (100% spin,
                    // review MINOR-1).
                    tracing::warn!("dir watch pipeline died; arm disarmed");
                    dir_watch_alive = false;
                }
            }
            Some(task) = async {
                match &mut foreign_tasks {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                // Task from ANOTHER frontend of the same session (phase 3):
                // onto the panel.
                app.board.push_foreign(&task);
            }
            Some(ev) = async {
                match &mut conn_events {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                app.message = Some(match ev {
                    ConnEvent::Restored => t("msg-daemon-restored"),
                    // A handoff and a stop look the same the moment the
                    // connection drops: this notice arrives first and is the
                    // only thing that tells them apart.
                    ConnEvent::GoingAway { reconnect: true } => t("msg-daemon-handover"),
                    ConnEvent::GoingAway { reconnect: false } => t("msg-daemon-stopping"),
                    // `Lost` and the wildcard together: `ConnEvent` is
                    // non-exhaustive, and an event from a newer SDK reads as
                    // a loss, which is the conservative choice.
                    ConnEvent::Lost | _ => t("msg-daemon-lost"),
                });
            }
            Some(req) = async {
                match &mut approvals {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                // Pending policy approval (M3-3b T5): onto the dialog queue
                // (never overwrites an open modal) and opens if it should.
                app.pending_approvals.push_back(req);
                app.open_next_pending();
            }
            Some(d) = async {
                match &mut degraded {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                // #44: a remote session degraded to plaintext — a PERSISTENT
                // indicator in the status bar (does not overwrite a
                // transient `message`). H3d: the STRUCTURED value is kept,
                // not the sentence — the bar composes it
                // (`App::connection_banner`) and help can ask which scheme
                // degraded.
                app.note_degraded(d);
            }
            Some(f) = async {
                match &mut failed {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                // #322: a connection did NOT open, with the reason. Arrives
                // AFTER the error from the listing that triggered it —the
                // handler waiting for it blocks this select meanwhile— so it
                // overwrites the generic category with the specific
                // sentence, which is the order that is wanted.
                app.note_connection_failed(&f);
            }
            Some(n) = async {
                match &mut plugin_notices {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                // ADR 0100: a hook's sentence, attributed to the plugin, as a
                // transient bar message — same as a connection failure. Not
                // a persistent indicator: it speaks of a mutation that
                // already happened.
                app.note_plugin_notice(&n);
            }
            Some(state) = async {
                match &mut journal_warnings {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                // #167/#177: this session just mutated without getting
                // recorded. One per EPISODE (the core does not repeat while
                // the reason does not change), so overwriting `message` here
                // cannot turn into a drip. And since `message` gets cleared
                // by the next key, the fact is also noted in the bar's
                // PERSISTENT indicator: this is not a notice that can be
                // lost by pressing an arrow.
                //
                // #179: and recovery TURNS OFF that indicator. Without this,
                // a passing occupant —another `norte cp`, a daemon
                // restarting— would leave a three-hour session showing "not
                // recording" over mutations that ARE being recorded.
                use norte_core::embedded::JournalStatus;
                match state {
                    JournalStatus::Lost(why) => {
                        app.message = Some(journal_warning_i18n(&why));
                        app.note_no_journal(why);
                    }
                    JournalStatus::Recovered => {
                        app.message = Some(t("msg-journal-recovered"));
                        app.note_journal_recovered();
                    }
                    // #203: the same fact, a different explanation — and the
                    // bar says it with a different sentence, because the
                    // usual one also shows up when there is a live daemon
                    // and so is no longer looked at.
                    JournalStatus::Squatted => {
                        app.message = Some(t("msg-journal-squatted"));
                        app.note_journal_squatted();
                    }
                    // `#[non_exhaustive]`: a new transition cannot change the
                    // indicator blindly — it is ignored until someone
                    // deliberately teaches it.
                    _ => {}
                }
            }
            res = async {
                match &mut work.stat {
                    Some(pr) => (&mut pr.rx).await.ok(),
                    None => std::future::pending().await,
                }
            } => {
                // Viewport stat probe (#52): the slot is ALWAYS cleared
                // (whether it hydrated something, the stat failed or the
                // channel closed) — the dedup through `work.probed` avoids
                // retrying until a new listing empties it.
                work.stat = None;
                for (pane, path, entry) in res.unwrap_or_default() {
                    app.panes[pane].hydrate(&path, entry.size, entry.mtime_ms);
                }
            }
            res = async {
                match &mut work.panels {
                    Some(pr) => (&mut pr.rx).await.ok(),
                    None => std::future::pending().await,
                }
            } => {
                // The catalogue arrived: the panels the CONSENTED plugins
                // declare become real kinds (phase 3). The slot is cleared
                // no matter what — if it failed, this session is left with
                // no plugin panels, which is the usual screen.
                work.panels = None;
                if let Some(Ok(list)) = res {
                    app.kinds.insert_panels(&list.plugins);
                }
            }
            res = async {
                match &mut work.panel_render {
                    Some(pr) => (&mut pr.rx).await.ok(),
                    None => std::future::pending().await,
                }
            } => {
                // A plugin panel's frame (phase 3). The slot is ALWAYS
                // cleared, and `land` is the one that decides whether
                // the response still counts, by comparing the signature:
                // while it was in flight, the cursor may have moved and that
                // frame describes a different screen.
                // The probe is TAKEN, not cloned: the arm is rebuilt on every
                // `select!` turn, so cloning the signature there was cloning
                // it per poll rather than per response.
                if let Some(pr) = work.panel_render.take() {
                    crate::panelplugin::land(app, pr.slot, &pr.signature, res);
                }
            }
            (epoch, res) = async {
                match &mut work.log_tail {
                    Some(pr) => (pr.epoch, (&mut pr.rx).await.ok()),
                    None => std::future::pending().await,
                }
            } => {
                // The daemon's log (#328): the slot is ALWAYS cleared,
                // whether the task answered, failed or died — `log_next_at`'s
                // brake is what prevents a retry loop, and a closed channel
                // cannot leave the probe switched off forever.
                work.log_tail = None;
                if let Some(res) = res {
                    crate::logview::land_tail(app, epoch, res);
                }
            }
            (epoch, res) = async {
                match &mut work.log_level {
                    Some(pr) => (pr.epoch, (&mut pr.rx).await.ok()),
                    None => std::future::pending().await,
                }
            } => {
                // The level the daemon REALLY left in place, which may not
                // be the one requested: its ring is global to its clients
                // and only ever goes up.
                work.log_level = None;
                if let Some(res) = res {
                    crate::logview::land_level(app, epoch, res);
                }
            }
            (generation, res) = async {
                match &mut work.compare_stat {
                    Some(pr) => (pr.generation, (&mut pr.rx).await.ok()),
                    None => std::future::pending().await,
                }
            } => {
                // Stat probe of the compare panel's selected row (#157): the
                // slot is ALWAYS cleared, same as the one above. A closed
                // channel (`res` is `None`) does not mark anything as
                // probed: the next time the selection asks for it again it
                // is retried, instead of leaving the row orphaned forever
                // because the task that requested it died halfway.
                work.compare_stat = None;
                for (path, entry) in res.unwrap_or_default() {
                    // The generation is the REQUEST's, not now's: if another
                    // comparison started while it was in flight, `hydrate`
                    // discards it.
                    app.hydrate_compare_size(generation, path, entry.and_then(|e| e.size));
                }
            }
            (slot, res) = std::future::poll_fn(|cx| {
                // One per SLOT, and as many as there are slots.
                // `oneshot::Receiver` is `Unpin`, so it is polled by hand;
                // `select!` has fixed arity and here the layout sets the
                // arity.
                for (id, f) in work.decorate.iter_mut() {
                    if let std::task::Poll::Ready(r) =
                        std::pin::Pin::new(&mut f.rx).poll(cx)
                    {
                        return std::task::Poll::Ready((id, r.ok()));
                    }
                }
                std::task::Poll::Pending
            }) => {
                // Decoration fetch (G3b): ALWAYS cleared. A late response
                // whose `dir` no longer matches the pane's (the user cd'd
                // again while it was in flight) is DISCARDED — it never
                // paints badges for a listing no longer on screen (same
                // anti-stale criterion as `apply_fill_msg`'s drain guard for
                // virtual search).
                if let Some(f) = work.decorate.remove(slot)
                    && let Some((map, cols, headers)) = res
                {
                    // LABELS are not part of the listing: they say what a
                    // column is called, and that holds even if this response
                    // arrives late or for another directory. They go to the
                    // shared model, outside the cells' anti-stale guard.
                    app.columns.apply_plugin_headers(headers);
                    if let Some(p) = app.panes.browser_mut(f.slot)
                        && p.dir() == &f.dir
                    {
                        p.set_decorations(map);
                        // #117-follow-up: plugin column values travel in the
                        // same fetch and share the anti-stale guard.
                        p.set_plugin_columns(cols);
                    }
                }
            }
            (slot, res) = std::future::poll_fn(|cx| {
                // Preview reads, one per slot. Same hand-polling as the
                // decorations and for the same reason: the layout sets the
                // arity, not `select!`.
                for (id, f) in work.preview.iter_mut() {
                    if let std::task::Poll::Ready(r) =
                        std::pin::Pin::new(&mut f.rx).poll(cx)
                    {
                        return std::task::Poll::Ready((id, r.ok()));
                    }
                }
                std::task::Poll::Pending
            }) => {
                // The response is applied ONLY if the slot still wants that
                // same path: while it was in flight, the cursor may have
                // moved. And an error is PAINTED, never asked about — the
                // preview follows the cursor, so a dialog per keystroke
                // would turn walking down a directory into a burst of
                // modals.
                if let Some(f) = work.preview.remove(slot) {
                    match res {
                        Some(Ok(viewer)) => {
                            if let Some(p) = app.panes.preview_mut(slot) {
                                p.show(f.path, viewer);
                            }
                        }
                        Some(Err(e)) => {
                            let key = error_category(&e);
                            app.preview_failed(slot, &key);
                        }
                        // The task died without answering: no invented error
                        // is painted, whatever was there is left and the
                        // next cursor movement tries again.
                        None => {}
                    }
                }
            }
            (pane, msg) = std::future::poll_fn(|cx| {
                // One channel PER SLOT, not per position, and as many as
                // there are slots. `tokio::select!` has fixed arity, so they
                // are polled by hand: `poll_recv` registers the waker, so
                // this is as cancel-safe as `recv` and losing the race does
                // not lose the other one's batch.
                //
                // The sweep STARTS where the previous one ended. Always
                // polling from the beginning would let a fast drainer on the
                // first slot never let the others speak — with two panels
                // `select!` avoided this on its own, because it picks at
                // random.
                let n = work.fill.len();
                for k in 0..n {
                    let Some((id, f)) = work.fill.iter_mut().nth((work.fill_cursor + k) % n) else {
                        break;
                    };
                    if let std::task::Poll::Ready(m) = f.rx.poll_recv(cx) {
                        work.fill_cursor = (work.fill_cursor + k + 1) % n;
                        return std::task::Poll::Ready((id, m));
                    }
                }
                std::task::Poll::Pending
            }) => {
                // Batch from the paginated listing's drainer (ADR 0017): to
                // ITS slot's pane. `None` = closed channel (end of
                // draining).
                apply_fill_msg(app, &mut work.fill, pane, msg);
            }
            hits = async {
                // Only drained while the run is still alive (`Running`): a
                // closed channel would return `None` in a loop (spin) — on
                // reading the `None` it moves to terminal and this arm is
                // left pending.
                match &mut work.search {
                    Some(s) if s.state == SearchState::Running => s.rx.recv().await,
                    _ => std::future::pending().await,
                }
            } => {
                drain_search(app, &mut work.search, hits);
            }
            batch = async {
                // Same as the hits arm: only drained with the run ALIVE,
                // because a closed channel would return `None` in a loop
                // (spin).
                match &mut work.compare {
                    Some(c) if c.state == CompareState::Running => c.rx.recv().await,
                    _ => std::future::pending().await,
                }
            } => {
                drain_compare(app, &mut work.compare, batch);
            }
            // A SINGLE arm for the dialog's two Tasks: `select!` does not
            // allow borrowing `work.sync` twice, and they are successive
            // phases of the same run — there is never a plan and an
            // application at the same time.
            tick = async {
                let Some(s) = &mut work.sync else {
                    return std::future::pending().await;
                };
                if s.applying {
                    // The application has no channel: it waits for its Task
                    // to change state. `changed()` with the sender gone
                    // returns `Err`, and that is also an ending — it exits
                    // and the harvest reads whatever snapshot there is.
                    let alive = s.progress.changed().await.is_ok();
                    return SyncTick::Applied { alive };
                }
                match &mut s.rx {
                    // An already-closed channel would return `None` in a
                    // loop (spin): `drain_sync_plan` sets `rx = None` on
                    // close.
                    Some(rx) => SyncTick::Plan(rx.recv().await),
                    None => std::future::pending().await,
                }
            } => {
                match tick {
                    SyncTick::Plan(event) => drain_sync_plan(app, &mut work.sync, event),
                    SyncTick::Applied { alive } => {
                        harvest_sync_apply(app, backend, &mut work.sync, alive).await;
                    }
                }
            }
            res = async {
                // In-flight ai.rename_plan (M4-IA): harvested without
                // blocking — the arm only arms with a live run
                // (work.stat's shape).
                match &mut work.ai_rename {
                    Some(r) => (&mut r.handle).await,
                    None => std::future::pending().await,
                }
            } => {
                harvest_ai_rename(app, backend, &mut work, res);
            }
            res = async {
                // In-flight ai.organize_plan / plugin.organize_plan (phase
                // 8): the same shape, and a single slot because both produce
                // the same plan and the same modal.
                match &mut work.organize {
                    Some(r) => (&mut r.handle).await,
                    None => std::future::pending().await,
                }
            } => {
                crate::jobs::harvest_organize(app, &mut work, res);
            }
            res = async {
                // In-flight fs.rename_batch_plan (§17): harvested without
                // blocking, `work.ai_rename`'s arm's shape.
                match &mut work.rename_batch {
                    Some(r) => (&mut r.handle).await,
                    None => std::future::pending().await,
                }
            } => {
                harvest_rename_batch(app, &mut work, res);
            }
            res = async {
                // In-flight index.search_semantic (M4-IA-2): harvested
                // without blocking — `work.ai_rename`'s arm's shape.
                match &mut work.semantic {
                    Some(r) => (&mut r.handle).await,
                    None => std::future::pending().await,
                }
            } => {
                harvest_semantic(app, &mut work, res);
            }
            res = async {
                // The same question, for the "go to" SECTION (phase 6): its
                // own slot because what is done with the response is a
                // different thing, and because both can be alive at once.
                match &mut work.goto_index {
                    Some(r) => (&mut r.handle).await,
                    None => std::future::pending().await,
                }
            } => {
                crate::jobs::harvest_goto_index(app, &mut work, res);
            }
            res = async {
                // A checksum batch's report (#311): same shape. Waiting for
                // the terminal state lives INSIDE the spawn, so there is
                // nothing more to harvest here.
                match &mut work.checksum {
                    Some(r) => (&mut r.handle).await,
                    None => std::future::pending().await,
                }
            } => {
                harvest_checksum(app, &mut work, res);
            }
            res = async {
                // A disk map's report (phase 4): same shape as the checksum
                // one. Waiting for the terminal state lives INSIDE the
                // spawn, so there is nothing more to harvest here.
                match &mut work.disk_map {
                    Some(r) => (&mut r.handle).await,
                    None => std::future::pending().await,
                }
            } => {
                crate::jobs::harvest_disk_map(app, &mut work, res);
            }
            outcome = async {
                match &mut work.lua {
                    Some((run, _)) => run.await,
                    None => std::future::pending().await,
                }
            } => {
                // IMMEDIATE DROP of the resolved CommandRun (the driver's
                // contract): keeping it would leave `run_active` on and the
                // Lua status bar frozen.
                work.lua = None;
                match outcome {
                    RunOutcome::Ok { messages } => {
                        if !messages.is_empty() {
                            // Joined with " · " and through detail_for_bar
                            // (cap + masked): the bar is one line.
                            app.message = Some(detail_for_bar(&messages.join(" · ")));
                        }
                    }
                    RunOutcome::Err { detail, .. } => {
                        app.message = Some(ta(
                            "err-lua-command",
                            &[("detail", &detail_for_bar(&detail))],
                        ));
                    }
                    RunOutcome::Cancelled => app.message = Some(t("err-lua-cancelled")),
                    RunOutcome::TimedOut => app.message = Some(t("err-lua-timeout")),
                }
                // FIFO: starts the next queued one. In a loop: if one no
                // longer exists (hot reload removed it → `err-lua-unknown`),
                // the rest of the queue does not stay stuck.
                if let Some(host) = lua_host.as_ref() {
                    while work.lua.is_none() {
                        let Some(next) = work.lua_queue.pop_front() else {
                            break;
                        };
                        work.lua = start_lua_run(app, host, backend, &next);
                    }
                }
            }
            Some(()) = cfg_rx.recv() => {
                // A burst of saves: pushes the deadline (ADR 0007).
                reload_at =
                    Some(tokio::time::Instant::now() + std::time::Duration::from_millis(300));
            }
            () = async {
                match reload_at {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending().await,
                }
            } => {
                reload_at = None;
                while cfg_rx.try_recv().is_ok() {}
                // #117: if the reload changes a visible pane's configured
                // attrs, it has to re-list (the values only arrive by
                // requesting them) — same path as the picker's confirm.
                let attrs_before = pane_attr_ids(app);
                reload_config(
                    app,
                    backend,
                    resolver,
                    viewer_resolver,
                    dialog_resolver,
                    help_lines,
                    lang,
                    &layers,
                    cli_preset.as_deref(),
                    &mut quick_mode,
                    &mut confirm_quit,
                    &mut cfg,
                )
                .await;
                // `[ui] mouse` hot: switching it on or off without
                // restarting. `set` is idempotent, so a reload that did not
                // touch the key (or failed entirely, leaving the current
                // config) sends nothing to the terminal.
                //
                // A one-off exemption from rule 2, the SAME as the draw
                // above: a few synchronous escape bytes to the control
                // terminal, bounded, and only when the key CHANGES.
                if let Err(e) =
                    capture.set(cfg.common.ui_mouse.unwrap_or(true), terminal.backend_mut())
                {
                    // And it is reported, as at startup: whoever just turned
                    // the mouse on from the settings overlay and finds that
                    // clicking does nothing deserves to know why (before,
                    // this only went to the log).
                    tracing::warn!(error = %e, "could not change mouse capture");
                    app.message = Some(t("msg-mouse-capture-failed"));
                }
                // `[ui] alt_menu` hot, with the same exemption. The terminal
                // is NOT asked: what it answered at startup is read
                // (`alt_menu::query_support`), because with the event
                // reader alive the question blocks for two seconds and says
                // "no".
                if let Err(e) = crate::alt_menu::set(
                    cfg.common.ui_alt_menu.unwrap_or(false),
                    crate::alt_menu::supported,
                    terminal.backend_mut(),
                ) {
                    tracing::warn!(error = %e, "could not change the keyboard protocol");
                }
                if pane_attr_ids(app) != attrs_before {
                    let refreshed =
                        refresh_panes(app, backend, &mut Console::new(&mut events, terminal)).await;
                    after_panes_refresh(
                        app,
                        refreshed,
                        &mut work.fill,
                        &mut work.probed,
                        &mut work.search,
                    );
                }
                // Lua scripting's hot reload (ADR 0026): a WHOLE NEW host
                // (never a half-updated state). An in-flight `CommandRun`
                // keeps the OLD state via its cloned handles (documented in
                // `lua::api`) and is not touched; statusbar/state are reborn.
                // The queue too: its names pointed at the old registry (and
                // if `load_lua` gave None, there would be nobody left to
                // drain it).
                lua_host = load_lua(app, &layers).await;
                work.lua_queue.clear();
            }
            maybe = events.next() => {
                // The terminal's EOF —the window gets closed on you—: exits
                // through the SAME place as an `app.quit`, which is where the
                // session's last snapshot is saved. Exiting here with a
                // `return` used to lose it.
                let Some(event) = maybe else { app.quit = true; continue; };
                let event = event.map_err(RunError::Event)?;
                // Mouse (`[ui] mouse`): only arrives if capture was
                // requested — without it the emulator reports nothing and
                // this arm never runs. The gesture's semantics (marking,
                // sweeping, transferring) live in `norte-frontend` (rule 7);
                // here only the cell is resolved and applied.
                if let Event::Mouse(me) = event {
                    alt_solo.release();
                    mouse::on_mouse(
                        app,
                        backend,
                        capture,
                        &mut Console::new(&mut events, terminal),
                        resolver,
                        help_lines,
                        lang,
                        quick_mode,
                        confirm_quit,
                        &cfg,
                        &mut work,
                        me,
                    )
                    .await;
                    // A click on the key bar left a key to synthesize (spec
                    // 2026-09-10): goes through `on_key`, the SAME path as a
                    // real key, with the three resolvers. There is no second
                    // dispatch that could diverge.
                    if let Some(key) = app.pending_key.take() {
                        on_key(
                            app,
                            backend,
                            capture,
                            &mut Console::new(&mut events, terminal),
                            resolver,
                            viewer_resolver,
                            dialog_resolver,
                            help_lines,
                            lang,
                            quick_mode,
                            confirm_quit,
                            &cfg,
                            cli_preset.as_deref(),
                            lua_host.as_ref(),
                            &mut work,
                            key,
                        )
                        .await;
                    }
                } else if let Event::Key(key) = event
                    && crate::alt_menu::es_modifier(&key)
                {
                    // A modifier alone is not a key for the keymap: it only
                    // feeds the gesture. With the menu open it folds it; with
                    // another overlay in front it does nothing, like F9
                    // there.
                    if alt_solo.key(&key) && (app.menu.is_some() || !mouse::overlay_open(app)) {
                        // Like F9 through `on_key`: a half-typed sequence
                        // (`g`…) is abandoned, or the next key after closing
                        // the menu would complete it.
                        resolver.reset();
                        app.toggle_menu();
                    }
                } else if let Event::Key(key) = event
                    // `Repeat` counts as a press: with the kitty protocol
                    // requested, a held arrow arrives this way, and filtering
                    // only `Press` would advance one row. Without it,
                    // crossterm sends everything as `Press`.
                    && key.kind != crossterm::event::KeyEventKind::Release
                {
                    alt_solo.key(&key);
                    on_key(
                        app,
                        backend,
                        capture,
                        // The console WITH terminal: this is the path that
                        // reaches a long navigation, and it is the only one
                        // that needs to be able to repaint while it waits.
                        &mut Console::new(&mut events, terminal),
                        resolver,
                        viewer_resolver,
                        dialog_resolver,
                        help_lines,
                        lang,
                        quick_mode,
                        confirm_quit,
                        &cfg,
                        cli_preset.as_deref(),
                        lua_host.as_ref(),
                        &mut work,
                        key,
                    )
                    .await;
                } else if let Event::Paste(text) = event {
                    // Bracketed paste (#143): ONE router beside the key
                    // dispatch above, not a second one — see `route_paste`.
                    app.message = None;
                    route_paste(app, &text);
                }
            }
        }
        // A profile switch requested by `profile.pick`/`next`/`prev`, once
        // per turn and HERE.
        //
        // Not beside each `run_command` like `launch_pending_open`: that one
        // needs the terminal, which `keys.rs` has; this one needs the
        // layers, the three resolvers and the whole config, which are only
        // here. Five sites passing around twelve parameters to serve three
        // commands would be the worse wiring.
        if let Some(name) = app.pending_profile.take() {
            switches_profile(
                &name,
                app,
                backend,
                &mut Console::new(&mut events, terminal),
                resolver,
                viewer_resolver,
                dialog_resolver,
                help_lines,
                lang,
                &mut layers,
                cli_preset.as_deref(),
                &mut quick_mode,
                &mut confirm_quit,
                &mut cfg,
            )
            .await;
        }
        // The sidebar's drives, once per turn and AFTER the profile switch,
        // which mounts a new screen and therefore also requests them. Three
        // paths —opening the panel, unfolding its section, mounting a layout
        // that already carries it— and a single site that serves them, which
        // is what keeps them from being missing exactly where nobody
        // remembered (see `App::places_wants_drives`).
        crate::screens::drain_places_drives(app, backend).await;
        // Resize/Focus/etc: the loop's opening draw repaints on its own.
    }
}

/// Switches profile hot (ADR 0079, D8).
///
/// The order IS the design:
///
/// 1. Dump the state of the profile being left, BEFORE touching anything. If
///    this happened after reloading, what got saved under the old name would
///    be the new profile's state.
/// 2. Rebuild the layers with the profile's directory and reload. That
///    reload is `reload_config`, which was already "all or nothing": if it
///    fails, it leaves the current config and warns, which is exactly what
///    D7 asks for a switch (`Switch`).
/// 3. Only if it applied: mount the new profile's saved layout, make it the
///    active one, and say what could not be applied without restarting.
///
/// What does NOT need doing by hand: step 2 applies theme, keymap, columns,
/// favorites and openers, and each slot's seeding comes from
/// `apply_session`, which already knows how to read the layout under the
/// profile's key.
#[expect(clippy::too_many_arguments, reason = "loop wiring, not an API")]
async fn switches_profile(
    name: &std::ffi::OsStr,
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
    resolver: &mut Resolver,
    viewer_resolver: &mut Resolver,
    dialog_resolver: &mut Resolver,
    help_lines: &mut Vec<ratatui::text::Line<'static>>,
    lang: norte_i18n::Lang,
    layers: &mut Layers,
    cli_preset: Option<&str>,
    quick_mode: &mut nav::Mode,
    confirm_quit: &mut config::ConfirmQuit,
    cfg: &mut config::LoadedConfig,
) {
    // 1 — the state of the profile being LEFT, captured before anything else.
    let outgoing = app.session_body();
    let before = cfg.common.clone();

    // 2 — the new layers. A name the resolver cannot hang leaves the layers
    // as they were, and then the reload would change nothing: the switch is
    // refused instead of pretending something happened.
    let new_layers = norte_config::standard_layers_with_profile(Some(name));
    if !new_layers
        .dirs
        .iter()
        .any(|(_, k)| *k == config::Layer::Profile)
    {
        app.message = Some(ta(
            "msg-profile-not-applied",
            &[("profile", &name.to_string_lossy())],
        ));
        return;
    }
    let previous = std::mem::replace(layers, new_layers);
    let applied = reload_config(
        app,
        backend,
        resolver,
        viewer_resolver,
        dialog_resolver,
        help_lines,
        lang,
        layers,
        cli_preset,
        quick_mode,
        confirm_quit,
        cfg,
    )
    .await;
    if !applied {
        // `reload_config` already left the current config in place and said
        // why; here only the layers are returned, which is the only thing
        // this level had touched. The reader stays on the profile they had,
        // with the state they had.
        *layers = previous;
        return;
    }

    // 3 — the new profile is the active one, and its screen is mounted from
    // the same body: `apply_session` reads the layout under the profile's
    // key and stores the others'.
    app.active_profile = Some(name.to_os_string());
    // The vector it returns is discarded on purpose, same as
    // `apply_session_value` does at startup: whoever needs a listing is
    // resolved by `refresh_panes` right after, which is the same path the
    // reload already uses when a pane's attrs change.
    let _ = app.apply_session(&outgoing);
    // And `[profile.start]`, which is what makes a freshly created profile
    // or one arriving from another machine useful: where each slot opens the
    // first time. Comes AFTER the session because the session wins — a
    // profile is a workspace, not a bookmark that returns you to the start
    // every time.
    //
    // The veto is NOT `outgoing`: that is `session_body()`, the screen RIGHT
    // NOW, which names every live slot and therefore would never let
    // anything be seeded. It is set by the method itself, with what was read
    // from disk and what has already been seeded.
    let _ = app.seed_profile_start(&cfg.common.profile_start);
    let _ = crate::refresh::refresh_panes(app, backend, events).await;
    let outside = crate::app::profile::not_hot_reloadable(&before, &cfg.common);
    // What the profile FILE carries and is not understood outranks the other
    // two messages: "could not apply hot" describes a limit of this process,
    // and this describes lines that are never going to do anything. Staying
    // silent about them is what turned `[profile.start]` into a trap.
    for warning in &cfg.common.profile_warnings {
        tracing::warn!(reason = %warning, "profile line ignored");
    }
    app.message = Some(if !cfg.common.profile_warnings.is_empty() {
        ta(
            "msg-profile-config-ignored",
            &[
                ("profile", &name.to_string_lossy()),
                ("n", &cfg.common.profile_warnings.len().to_string()),
            ],
        )
    } else if outside.is_empty() {
        ta(
            "msg-profile-switched",
            &[("profile", &name.to_string_lossy())],
        )
    } else {
        ta(
            "msg-profile-switched-partial",
            &[
                ("profile", &name.to_string_lossy()),
                ("keys", &outside.join(", ")),
            ],
        )
    });
}

/// The panes' watchable NATIVE dirs (#106): only `file://` (an sftp/S3/archive
/// dir has no inotify — its refresh is still Ctrl+R) and only real panes (the
/// virtual search one shows no dir). Pure: `vpath_to_native` does not touch
/// the FS.
#[must_use]
pub fn watch_targets(app: &App) -> [Option<std::path::PathBuf>; 2] {
    std::array::from_fn(|i| {
        let p = &app.panes[i];
        if p.virtual_search {
            return None;
        }
        norte_vfs_local::vpath_to_native(p.dir()).ok()
    })
}
