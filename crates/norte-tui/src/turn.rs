//! The event loop's turn, in pieces.
//!
//! Every turn always does the same things and in this order: drain what
//! dispatch left REQUESTED ([`drain_pending`]), plant the modals that were
//! waiting their turn ([`open_retained_modals`]), prepare the frame
//! ([`prepare_frame`]), paint — that stays in the loop, which is the one
//! holding the terminal —, hand back to the model what only the painted
//! frame knows ([`after_frame`]), and request whatever is left to hydrate
//! ([`spawn_probes`]).
//!
//! It is here and not in `App` for a reason that keeps repeating: these are
//! things the model cannot do alone — they launch Tasks, suspend the
//! terminal, talk to the backend — and the loop can. Pulling them out of the
//! loop is what keeps `run` readable: what is left there is the ORDER, which
//! is its only responsibility.

use norte_core::backend::Backend;
use norte_i18n::{t, ta};
use norte_proto::VPath;

use crate::app::{App, Modal};
use crate::event_loop::RunError;
use crate::jobs::{InFlight, launch_compare, launch_sync_apply, launch_sync_plan};
use crate::lua::refresh_lua_status;
use crate::mouse;
use crate::navigate::{apply_cd, cd};
use crate::overlays::{fetch_plugin_page, settle_help_over_modal, watch_refresh_allowed};
use crate::probes::{
    LOG_TAIL_PERIODO, STAT_BATCH_MAX, STAT_WINDOW_RADIUS, spawn_compare_stat_probe,
    spawn_log_level, spawn_log_tail, spawn_preview_fetch, spawn_stat_probe,
};
use crate::refresh::{after_panes_refresh, refresh_panes};
use crate::suspend::run_suspended;
use crate::tty;
use crate::ui;

/// Launches what dispatch left REQUESTED: comparing, syncing, approving a
/// plan, coming home after disconnecting, asking for a transfer's
/// destination, and suspending the TUI for a user program.
///
/// All of that is drained in the turn's HEADER and nowhere else. The reason
/// is always the same: the arms that answer a key each have their own early
/// exit, so the one point that covers all of them — present and future — is
/// this one.
pub async fn drain_pending(
    app: &mut App,
    backend: &Backend,
    capture: &mut mouse::Capture,
    // BOUND console, and this had to be fixed: down here there is a `cd`
    // — `pending_disconnect_dest`'s — and disconnecting from a remote often
    // goes back through the trail to ANOTHER remote, i.e. another connection
    // that takes a while. With a detached console that was exactly #323's
    // freeze, and on top of it with `app.busy` set: the state said "I'm
    // showing a spinner" and there was nothing on screen.
    events: &mut crate::console::Console<'_>,
    work: &mut InFlight,
) {
    // No wait survives a loop turn: whatever there was got resolved,
    // cancelled or failed within the previous turn. Clearing it here makes a
    // hung `Busy` structurally impossible, whatever happens with whoever is
    // waiting's exit paths — present and future.
    app.busy = None;
    // #135: the suspension is drained HERE and NOWHERE else. #28's opener
    // is launched at three points (key dispatch, the palette's and help's)
    // because each has its own `continue`; a suspension is also left
    // pending by `Modal::CommandLine`'s Enter, which lives in a fourth arm
    // with its own `continue` — so the spot that covers all of them,
    // present and future, is the turn's header. Before the draw: the panels
    // that get repainted are already the refreshed listing's.
    // `Shift+F2`: dispatch resolved WHAT to compare; the run loop owns the
    // channel and the Task, so it launches. Same split as
    // `pending_shell`/`pending_open`, and in the same turn header, for the
    // same reason: the arms that answer keys have their own `continue`s.
    if let Some(params) = app.pending_compare.take() {
        launch_compare(app, backend, &mut work.compare, params).await;
    }
    // #311: dispatch resolved WHAT to sum; reading the sums file and
    // awaiting the report is I/O, and that belongs to the run loop. Same
    // split.
    if let Some(req) = app.pending_checksum.take() {
        crate::mutations::checksum_start(app, backend, work, req).await;
    }
    // Phase 8: dispatch requested an organize plan; the request lives in
    // `work`, which belongs to the run loop. Same split.
    if std::mem::take(&mut app.pending_organize) {
        crate::jobs::spawn_organize_plan(app, backend, work, None);
    }
    // #149 and #164: does it fit at the destination, and can the
    // destination hold what gets written to it? Both are I/O, so the modal
    // opens WITHOUT the notices and this turn fills them in. The split is
    // `pending_compare`'s: dispatch decides WHAT, the run loop asks it.
    //
    // The two questions fail DIFFERENTLY, and that is deliberate.
    //
    // Space swallows the failure: not being able to enumerate volumes must
    // not block a copy nor paint an alarm, and "I don't know" is said by
    // staying silent — that is `space::warning`'s contract.
    //
    // Confinement does not. There, silence MEANS "this destination holds
    // its own writes", so swallowing the failure would be asserting it
    // without knowing: fail-open on a security line. If it is not known, it
    // warns (W5 B's security review).
    if let Some(check) = app.pending_dest_check.take() {
        let free = match check.total {
            // With no total there is no space question to ask, and
            // enumerating volumes to throw away the answer is I/O for
            // nothing.
            None => None,
            Some(_) => backend
                .volumes(false)
                .await
                .ok()
                .and_then(|vols| norte_frontend::space::free_for(&check.to, &vols)),
        };
        let space_notice = norte_frontend::space::warning(check.total, free, norte_i18n::active());
        let confinement_notice = match backend.capabilities(&check.to).await {
            Ok(caps) => norte_frontend::confine::warning(caps, norte_i18n::active()),
            Err(_) => norte_frontend::confine::warning(
                norte_proto::Capabilities {
                    flags: norte_proto::CapabilityFlags::empty(),
                    max_path: None,
                },
                norte_i18n::active(),
            ),
        };
        // The TWO dialogs, which are a transfer's two paths: with several
        // items it is confirmed, and with one the name is typed. Splitting
        // the notice by which of the two came out is what left a lone file
        // copying with nothing said (#343).
        if let Some(
            Modal::ConfirmTransfer { space, confine, .. }
            | Modal::TransferName { space, confine, .. },
        ) = app.modal.as_mut()
        {
            *space = space_notice;
            *confine = confinement_notice;
        }
    }
    // `Ctrl+Y` / `s` / `m`: dispatch resolved WHAT to sync, and it launches
    // here — same split as the comparison, in the same turn header and for
    // the same reason.
    if let Some(params) = app.pending_sync.take() {
        launch_sync_plan(app, backend, &mut work.sync, params).await;
    }
    // And the approval, which is the SAME dialog's SECOND Task. All that
    // travels is the hash (ADR 0049).
    if let Some(hash) = app.pending_sync_apply.take() {
        launch_sync_apply(app, backend, &mut work.sync, &hash).await;
    }
    // Phase 4: the disk map asks to measure. Drained here and not in
    // dispatch because measuring is I/O and this loop owns the backend —
    // same split as the checksums and the comparison.
    if std::mem::take(&mut app.disk_map_stale) {
        crate::jobs::lanzar_disk_map(app, backend, work).await;
    }
    // Phase 7: the timeline asks for its first page, for the same reason.
    // It is flagged by whoever opens it and by whoever INHERITS it from a
    // saved layout, which is the case that goes through no key at all.
    if std::mem::take(&mut app.timeline_stale) {
        crate::dispatch::cargar_timeline(app, backend, None).await;
    }
    // And entering one of the map's children is a NORMAL `cd`, with its
    // usual return ritual: the map points, and navigating is the same path
    // navigation always takes. A second path is what ADR 0077 exists to
    // prevent.
    if let Some(name) = app.pending_disk_map_enter.take() {
        let dest = app
            .disk_map_slot()
            .and_then(|s| app.panes.disk_map(s))
            .and_then(|m| m.dir().map(|d| d.join(name)));
        if let Some(dir) = dest {
            let outcome = cd(app, backend, events, dir).await;
            apply_cd(
                &app.panes,
                &mut work.fill,
                &mut work.decorate,
                &mut work.probed,
                &mut work.search,
                outcome,
            );
        }
    }
    // #140: the panel that just disconnected leaves through the same `cd`
    // as any other navigation, with its return ritual.
    if let Some(dest) = app.pending_disconnect_dest.take() {
        let outcome = cd(app, backend, events, dest).await;
        apply_cd(
            &app.panes,
            &mut work.fill,
            &mut work.decorate,
            &mut work.probed,
            &mut work.search,
            outcome,
        );
    }
    // The sequence goes to the EMULATOR and not the program: it is written
    // raw to the terminal's output, which belongs to this loop and not to
    // `dispatch` (#286).
    if let Some(bytes) = app.pending_osc52.take() {
        use std::io::Write as _;
        let mut out = std::io::stdout();
        if out.write_all(&bytes).and_then(|()| out.flush()).is_err() {
            app.message = Some(t("msg-clipboard-failed"));
        }
    }
    if let Some(pending) = app.take_pending_shell() {
        handle_suspension(app, backend, capture, events, work, pending).await;
    }
    if std::mem::take(&mut app.pending_subshell) {
        handle_subshell(app, backend, capture, events, work).await;
    }
}

/// Suspends the TUI for the program dispatch left requested (#135).
///
/// Split out of [`drain_pending`] for size, not for concept: it is still a
/// turn-header drain and must not be called from anywhere else.
async fn handle_suspension(
    app: &mut App,
    backend: &Backend,
    capture: &mut mouse::Capture,
    events: &mut crate::console::Console<'_>,
    work: &mut InFlight,
    pending: crate::app::PendingShell,
) {
    {
        let crate::app::PendingShell {
            argv,
            cwd,
            wait_for_key,
            check_regular,
        } = pending;
        // #303: the check goes RIGHT NEXT to the launch, which is why it
        // lives here and not where the gesture was resolved. Doing it in
        // `on_tick` — where opening the editor is decided — bought nothing:
        // between that and this, `refresh_panes` runs, i.e. the WHOLE
        // re-listing of both panels, which on a remote pane is seconds.
        // That is exactly the window this check exists to narrow.
        //
        // Still does not close it: between this `stat` and the `exec` there
        // is a gap.
        if let Some(reason) = crate::gestures::motivo_para_no_lanzar(backend, check_regular).await {
            app.message = Some(reason);
            return;
        }
        // Audit (S4 review): the journal sees NONE of this on purpose
        // (design §D), so the trace that a shell happened here lives in the
        // log. Without the command line — it belongs to the user and has no
        // reason to end up in a file — and with just the program.
        let launched = argv.first().map(|a| a.to_string_lossy().into_owned());
        tracing::info!(
            program = launched.as_deref().unwrap_or("(none)"),
            wait_for_key,
            "TUI suspended for a user-started program (not journalled: no actor, no reversal)"
        );
        // Nothing to run = `app.toggle-panels`: it only shows the host
        // terminal. Refreshing after it would cost a full re-listing
        // (remote included) for a key that does not touch the disk.
        let launched_something = !argv.is_empty();
        // The terminal comes out of the console, which has owned it since a
        // long wait needed to repaint. With no terminal there is nothing to
        // hand over: the case only exists in tests, and there suspending
        // means nothing.
        let Some(terminal) = events.terminal() else {
            return;
        };
        if let Err(e) = run_suspended(terminal, capture, argv, cwd, wait_for_key).await {
            // `detail_for_bar`, never the OS's raw `Display` (S4 review,
            // L1/m4): the system localizes it on its own, has no cap, and —
            // if the error comes from a broken join — drags along a panic's
            // payload. And the program IS NAMED, like `msg-open-failed`
            // does: otherwise a deleted `$SHELL` and an alternate screen
            // that did not close give the same text.
            app.message = Some(ta(
                "msg-shell-failed",
                &[
                    ("program", launched.as_deref().unwrap_or("-")),
                    ("error", &crate::app::detail_for_bar(&e.to_string())),
                ],
            ));
        }
        // Whatever the shell did on disk is seen upon returning, through
        // the SAME path as `pane.refresh` (#118): cancelable refresh + the
        // full ritual, never a hand-rolled `set_listing`.
        //
        // GATED just like the watcher's refresh (S4 review, M2):
        // `refresh_panes` polls `events` and eats every key that is not
        // Esc/Ctrl+C, reinterpreting Esc as "abandon the refresh". With a
        // modal in front — an agent approval may have landed as the prompt
        // closed — that swallows the user's answer until the approval
        // expires. If it cannot refresh now, the watcher (capacity-1
        // channel) or the tick do it once the overlay closes.
        if launched_something && watch_refresh_allowed(app) {
            let refreshed = refresh_panes(app, backend, events).await;
            after_panes_refresh(
                app,
                refreshed,
                &mut work.fill,
                &mut work.probed,
                &mut work.search,
            );
        }
    }
}

/// Hands the terminal over to the persistent subshell (#142), starting it if
/// this is the first time.
///
/// The shell is created LAZILY and dies with the session: whoever never
/// presses the key pays no `fork`, and whoever presses it twice goes back to
/// the same shell — with its history and its variables — which is the whole
/// difference between this and the old scrollback.
///
/// POSIX (ADR 0084): on Windows there is no pty to hand over, so the key
/// DECLINES with the same message as `app.terminal` over a remote pane —
/// which is the truth, and was what the ADR promised with nothing fulfilling
/// it.
#[cfg(not(unix))]
#[allow(clippy::unused_async)] // same signature as the Unix one: the caller does not branch.
async fn handle_subshell(
    app: &mut App,
    _backend: &Backend,
    _terminal: &mut tty::Tui,
    _capture: &mut mouse::Capture,
    _events: &mut crate::console::Console<'_>,
    _work: &mut InFlight,
) {
    app.message = Some(t("msg-subshell-not-here"));
}

#[cfg(unix)]
async fn handle_subshell(
    app: &mut App,
    backend: &Backend,
    capture: &mut mouse::Capture,
    events: &mut crate::console::Console<'_>,
    work: &mut InFlight,
) {
    // With no detach chord the terminal is not handed over: the reader
    // would have no way back. See `detach_chord`.
    let Some(chord) = app.subshell_chord else {
        app.message = Some(t("msg-subshell-no-key"));
        return;
    };
    // A REMOTE pane has no local directory, and a local shell there would
    // be a shell somewhere other than what the panel shows. Same verdict and
    // same message as `app.terminal`.
    let dir = match crate::gestures::shell_cwd(app) {
        Ok(dir) => dir,
        Err(msg) => {
            app.message = Some(msg);
            return;
        }
    };
    // A shell that DIED (the reader typed `exit`) is replaced, not
    // resurrected: a dead child's pty does not accept writes and the key
    // would have stopped working for the rest of the session.
    if work
        .subshell
        .as_mut()
        .is_some_and(crate::subshell::Subshell::muerto)
    {
        work.subshell = None;
    }
    if work.subshell.is_none() {
        let size = events
            .terminal()
            .and_then(|t| t.size().ok())
            .map_or((80, 24), |s| (s.width, s.height));
        match crate::subshell::Subshell::arrancar(&dir, size) {
            Ok(sub) => work.subshell = Some(sub),
            Err(e) => {
                app.message = Some(ta(
                    "msg-shell-failed",
                    &[
                        ("program", "$SHELL"),
                        ("error", &crate::app::detail_for_bar(&e.to_string())),
                    ],
                ));
                return;
            }
        }
    }
    let Some(sub) = work.subshell.as_mut() else {
        return;
    };
    // Audit: same criterion as the suspension above (#135's design §D).
    // The journal sees none of this on purpose, and the line the reader
    // types does not end up in any file.
    tracing::info!("TUI handed the terminal to its persistent subshell (not journalled)");
    // `block_in_place` and not an `await`: handing over the terminal is
    // blocking I/O that lasts as long as the shell session does. See
    // `attach_subshell`.
    // The terminal comes out of the console (its owner since #323). With no
    // terminal there is nothing to hand over: it only happens in tests, and
    // there the subshell means nothing.
    let Some(terminal) = events.terminal() else {
        return;
    };
    let handed = tokio::task::block_in_place(|| {
        crate::suspend::attach_subshell(terminal, capture, sub, &dir, chord)
    });
    let dest = match handed {
        Ok(dest) => dest,
        Err(e) => {
            app.message = Some(ta(
                "msg-shell-failed",
                &[
                    ("program", "$SHELL"),
                    ("error", &crate::app::detail_for_bar(&e.to_string())),
                ],
            ));
            None
        }
    };
    // The panel FOLLOWS the shell: if the reader did a `cd` in there,
    // coming back leaves the panel where they ended up. It is the other
    // half of the following, and it goes through the usual cd (cancelable,
    // with a trail), never a hand-rolled `set_listing`.
    if let Some(dest) = dest
        && watch_refresh_allowed(app)
    {
        // A failure here is NOT swallowed: the shell said where it is and
        // norte has not been able to go, and a following that sometimes does
        // not happen with nothing said is indistinguishable from a broken
        // one.
        let Ok(vpath) = norte_vfs_local::vpath_from_native(&dest) else {
            app.message = Some(t("msg-subshell-bad-cwd"));
            return;
        };
        let outcome = cd(app, backend, events, vpath).await;
        apply_cd(
            &app.panes,
            &mut work.fill,
            &mut work.decorate,
            &mut work.probed,
            &mut work.search,
            outcome,
        );
        return;
    }
    // And if it did not move, whatever the shell touched on disk is seen
    // just the same: through the same cancelable refresh as the suspension,
    // and with the same gate (a modal in front would swallow the reader's
    // answer).
    if watch_refresh_allowed(app) {
        let refreshed = refresh_panes(app, backend, events).await;
        after_panes_refresh(
            app,
            refreshed,
            &mut work.fill,
            &mut work.probed,
            &mut work.search,
        );
    }
}

/// Plants the modals that arrived with another one open and were waiting
/// their turn: the AI rename plan and the semantic hits.
///
/// One per turn and in this order: if the plan just opened, the hits keep
/// waiting. An open modal is never stepped on — that is
/// `open_next_pending`'s discipline — and that is why what is retained is
/// stored in [`InFlight`] instead of being planted the moment it is
/// harvested.
pub fn open_retained_modals(app: &mut App, work: &mut InFlight) {
    // Review MINOR-1: `over_modal` describes the modal there is NOW, not one
    // already answered. Before planting the retained modals below, which
    // have to find the flag already cleared.
    settle_help_over_modal(app);
    // Retained AI plan (M4-IA): opens as soon as the active modal closes.
    // Approvals do not compete here: with the queue non-empty and no modal,
    // `open_next_pending` would already have opened one when the previous
    // one closed.
    if app.modal.is_none()
        && let Some(pending) = work.pending_ai_plan.take()
    {
        app.modal = Some(Modal::AiRenamePlan {
            dir: pending.dir,
            entries: pending.entries,
            offset: 0,
            seen: norte_frontend::AI_RENAME_PAIR_LIMIT,
            plan: pending.plan,
        });
    }
    // Retained checksums (#311): same discipline. The bar already promised
    // they would show once the dialog in front closed, and this is what
    // fulfills it.
    if app.modal.is_none()
        && let Some((title_key, rows)) = work.pending_checksums.take()
    {
        app.modal = Some(Modal::Checksums {
            title_key,
            rows,
            offset: 0,
        });
    }
    // Retained semantic hits (M4-IA-2): same discipline. If the AI plan
    // above just opened, `is_none` leaves them waiting.
    if app.modal.is_none()
        && let Some(hits) = work.pending_semantic.take()
    {
        app.modal = Some(Modal::SemanticHits {
            hits,
            offset: 0,
            cursor: 0,
        });
    }
}

/// What has to be made ready BEFORE painting: the Lua bar, the help laid
/// out for the terminal it is about to be painted on, and each pane's
/// window reconciled with its cursor.
///
/// The window before the draw and not after: doing it the other way cost a
/// frame of delay, and the cursor could land outside the painted window
/// right when it reached the edge — that is, disappear from the screen.
///
/// # Errors
///
/// [`RunError::Terminal`] if the terminal cannot be measured.
pub async fn prepare_frame(
    app: &mut App,
    backend: &Backend,
    terminal: &mut tty::Tui,
    lua_host: Option<&crate::lua::LuaHost>,
) -> Result<(), RunError> {
    // Lua bar every turn, BEFORE the draw (cached in the host).
    refresh_lua_status(app, lua_host);
    // H3b: the help is LAID OUT for the terminal it is about to be
    // painted on, right before the draw — the model bounds its scroll
    // against the number of lines that came out, and only the render knows
    // that (see `HelpView::refresh`). Every turn, not just on a theme
    // change: a resize goes through no key at all.
    if let Some(lang) = app.help.as_ref().map(|h| h.state.lang()) {
        // H3e: a plugin node's page is requested HERE, on demand and only
        // once per overlay (`fetch_plugin_page`). Before laying out, so the
        // page that just arrived is painted in THIS frame and not the next
        // one.
        fetch_plugin_page(backend, app).await;
        let size = terminal.size().map_err(RunError::Terminal)?;
        let (width, height) = ui::help_body_size(
            ratatui::layout::Rect::new(0, 0, size.width, size.height),
            lang,
        );
        app.refresh_help(width, height);
    }
    // Each pane's window is reconciled BEFORE painting (#124 + sticky
    // scroll): the cursor is already where the key left it, so this decides
    // which rows are seen and the draw paints them. Doing it AFTER cost a
    // frame of delay — the cursor could land outside the painted window,
    // i.e. disappear from the screen right when it reached the edge.
    {
        let s = terminal.size().map_err(RunError::Terminal)?;
        ui::before_frame(app, ratatui::layout::Rect::new(0, 0, s.width, s.height));
    }
    Ok(())
}

/// What only the ALREADY painted frame knows, handed back to the model: its
/// real height (pagination and the probe's radius come from there), the
/// geometry the mouse resolves its clicks against, and what the docked
/// slots — viewer, tree, attributes sheet — want to show next.
///
/// A click resolved against a layout that is not the painted one does not
/// fail loudly: it marks the file next door.
pub async fn after_frame(
    app: &mut App,
    backend: &Backend,
    work: &mut InFlight,
    painted: ratatui::layout::Rect,
) {
    // #124: the viewport's REAL height goes back to the model after every
    // frame — pagination (`page_step`) and the stat probe's radius come
    // from there instead of constants that lie on any terminal that does
    // not measure exactly that. With the viewer open it is 0 rows (no pane
    // painted) and the model falls back to its defaults.
    // The just-painted frame's REAL height: if the terminal resized between
    // `before_frame` and the draw, this is the correct one, and pagination
    // and the stat probe's radius come from it.
    ui::before_frame(app, painted);
    // SAME treatment for the mouse's geometry: the draw is the one that
    // knows where each pane landed and with what scroll, so it hands it
    // back to the model and the hit test resolves against the screen the
    // user is looking at. Without this the layout would have to be
    // recomputed on every click, and a click resolved against a layout that
    // is not the painted one does not fail loudly: it marks the file next
    // door.
    mouse::after_frame(
        app,
        ui::pane_geometry(app, painted),
        mouse::FrameZones {
            tabs: ui::tab_zones(app, painted),
            menus: ui::menu_zones(app, painted),
            panels: ui::panel_zones(app, painted),
            keys: ui::key_zones(app, painted),
            modal: ui::modal_zones(app, painted),
            places: ui::places_zones(app, painted),
            tree: ui::tree_zones(app, painted),
            extensions: ui::extension_zones(app, painted),
            help: ui::help_zones(app, painted),
            session: ui::session_zone(app, painted),
            status_items: ui::status_item_zones(app, painted),
            borders: ui::resize_borders(app, painted),
            slots: ui::panel_slots(app, painted),
        },
    );
    // A PLUGIN's panel (phase 3) repaints when something its guest would
    // see changes: the directory, the slot's size, or the pointed-at row.
    // One live request per slot, and the next one REPLACES the previous —
    // dropping the receiver is the cancellation — which is the same rule as
    // the preview below.
    crate::panelplugin::pedir_marco(app, backend, work, painted);
    // L3: the docked viewer follows the active listing's cursor. What is
    // requested comes out of `preview::want`, which returns `None` when the
    // slot was not placed — closed, behind a tab, or collapsed for lack of
    // room. That is why a hidden slot's suspension is not a check somebody
    // could forget to write: with no target there is nothing to request.
    {
        let res = ui::resolved_for(app, painted);
        match crate::preview::want(app, &res) {
            Some((slot, crate::preview::Want::File(path))) => {
                let already = app
                    .panes
                    .preview(slot)
                    .and_then(|p| p.shown().cloned())
                    .is_some_and(|s| s == path);
                let in_flight = work.preview.get(slot).is_some_and(|f| f.path == path);
                if !already && !in_flight {
                    // The SLOT's width, not the screen's (0.66.0): an image
                    // previewer shrinks to whatever it is told, and the
                    // docked one is half the terminal. Without the frame's
                    // two borders.
                    let columns = ui::slot_rect(&res, slot)
                        .map(|r| u32::from(r.width.saturating_sub(2).max(1)));
                    // Starting another one REPLACES whichever there was: the
                    // old `Receiver` is dropped here and its response is
                    // never applied.
                    work.preview
                        .set(slot, Some(spawn_preview_fetch(backend, path, columns)));
                }
            }
            Some((slot, crate::preview::Want::Note(key))) => {
                // A directory is not read: what it is is said. And whatever
                // was in flight stops mattering.
                work.preview.remove(slot);
                let text = t(key);
                if let Some(p) = app.panes.preview_mut(slot)
                    && (p.note().is_none_or(|n| n != text) || p.shown().is_some())
                {
                    p.say(None, text);
                }
            }
            None => {}
        }
        // #136: the tree DOES request, and that is why it requests ONE
        // branch per turn: a ten-thousand-entry directory or a slow remote
        // must not jam the loop, and the next turn requests the next one.
        if let Some(dir) = app.tree().and_then(crate::tree::Tree::wants) {
            let child_dirs: Option<Vec<_>> = match backend.list(&dir).await {
                Ok(mut entries) => Some({
                    // The SAME order as the listing next door, with the
                    // same comparator: two columns showing the same thing
                    // in a different order read as if they said different
                    // things.
                    norte_frontend::sort_entries(&mut entries);
                    entries
                        .into_iter()
                        .filter(|e| e.kind == norte_proto::EntryKind::Dir)
                        .map(|e| e.path)
                        .collect()
                }),
                // A branch that will not be read: decided by the shared
                // model — empty, or re-anchor if it was the root.
                Err(_) => None,
            };
            if let Some(t) = app.tree_mut() {
                match child_dirs {
                    Some(children) => t.insert_children(dir, children),
                    None => t.branch_unreadable(dir),
                }
            }
        }
        // The attributes sheet requests NOTHING: what it shows already came
        // in the listing, so this is a copy, not a request. A slot the
        // layout pass did not place produces no target and is not touched.
        match crate::metadata::want(app, &res) {
            Some((slot, crate::metadata::Want::Entry(e, up))) => {
                if let Some(sheet) = app.panes.metadata_mut(slot) {
                    *sheet = Some((*e, up));
                }
            }
            Some((slot, crate::metadata::Want::Note(_))) => {
                if let Some(sheet) = app.panes.metadata_mut(slot) {
                    *sheet = None;
                }
            }
            None => {}
        }
    }
}

/// Requests what is left to hydrate: the sizes of a lazy listing's VISIBLE
/// entries (#52) and the differences panel's selected row's (#157). At most
/// one probe of each in flight.
pub fn spawn_probes(app: &mut App, backend: &Backend, work: &mut InFlight) {
    // #52: lazy listing — VISIBLE entries with no size are hydrated in
    // batches (max one in flight; dedup by (pane, path) in `work.probed`).
    if work.stat.is_none() {
        let batch: Vec<(usize, VPath)> = app
            .needs_stat_window(STAT_WINDOW_RADIUS)
            .into_iter()
            .filter(|c| !work.probed.contains(c))
            .take(STAT_BATCH_MAX)
            .collect();
        if !batch.is_empty() {
            work.probed.extend(batch.iter().cloned());
            work.stat = Some(spawn_stat_probe(backend, batch));
        }
    }
    // #157: the differences panel's selected row, same treatment.
    if work.compare_stat.is_none() {
        let targets = app.compare_size_probe_targets();
        if !targets.is_empty() {
            work.compare_stat = Some(spawn_compare_stat_probe(
                backend,
                targets,
                app.compare_generation(),
            ));
        }
    }
    spawn_log_probes(app, backend, work);
}

/// The DAEMON's log (#328): pulling its lines and raising its level.
///
/// Two conditions before spending a single RPC, and each one covers a
/// different bug that had already slipped through:
///
/// - **There is a daemon.** With the core embedded — which is `ntc`'s
///   default startup — the core's ring is this process's own, i.e. the one
///   the panel is already reading. Asking `Backend::log_tail` returns
///   `Unsupported` with every reason to, but the panel read it as a fact
///   about a daemon and ended up saying "this daemon does not serve its
///   log" where there is none. The answer is not a different sentence: it
///   is not asking.
/// - **The panel is SEEN.** Not that it exists: one hidden behind a tab that
///   is not the active one still exists, and probing it is two RPCs a
///   second, the whole session, for something nobody has in front of them.
///
/// With both met it asks even with the source set to "this terminal",
/// because that is the only way to know whether there is a second source to
/// offer — and therefore to decide whether the `s` key means anything.
///
/// With no timer of its own: this terminal already repaints per frame and
/// its loop wakes ten times a second, so all that is needed is
/// [`crate::probes::LOG_TAIL_PERIODO`]'s brake. The window does need a
/// clock because it only repaints when someone does something.
fn spawn_log_probes(app: &mut App, backend: &Backend, work: &mut InFlight) {
    // The transport's is asked to the `Backend` and not to the state copied
    // in `App`: here it is about to talk over the wire, and whoever decides
    // if there is a wire is whoever holds it.
    if !backend.is_remote() || app.log_slot_visible().is_none() {
        return;
    }
    // What the key left requested: raising the daemon's ring level. It is
    // ALWAYS taken even if another is in flight — the last keypress rules —
    // and the answer says along the way whether that daemon knows about
    // logging.
    //
    // And it is dropped with no request at all to a daemon that already
    // said it has no log: raising the level of a ring that does not exist
    // is one RPC per keypress whose answer is already known.
    if let Some(level) = app.log_remote.pide_nivel.take()
        && app.log_remote.debe_pedir()
    {
        work.log_level = Some(spawn_log_level(backend, level, app.log_remote.epoca));
    }
    if work.log_tail.is_some()
        || !app.log_remote.debe_pedir()
        || work
            .log_next_at
            .is_some_and(|t| t > tokio::time::Instant::now())
    {
        return;
    }
    work.log_next_at = Some(tokio::time::Instant::now() + LOG_TAIL_PERIODO);
    work.log_tail = Some(spawn_log_tail(
        backend,
        app.log_remote.cursor,
        app.log_remote.epoca,
    ));
}
