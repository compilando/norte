//! The dispatch table: a command name (ADR 0006, the same ones the palette
//! and the wire see) and its effect.
//!
//! It lived in the `ntc` binary's root —a crate DIFFERENT from this lib—
//! and comes out WHOLE. It is a flat 109-arm `match` and is not split up:
//! this repository already argued in writing against splitting flat tables
//! (`norte-core/src/daemon/server.rs`), and the argument is the same here —
//! splitting an exhaustive match by topic trades a table the compiler checks
//! all at once for several that have to be kept in sync by hand.
//!
//! What makes it big is the vocabulary, not the depth: almost every arm is
//! one to three lines, and what each one calls already lives in its own
//! module — that is what this branch's nine previous rounds were pulling out
//! from underneath here.

use crate::app::{
    App, ExtensionManager, Modal, NavPopupKind, Palette, Settings, Trail, TrailStep, TransferKind,
    error_message,
};
use crate::config;
use crate::gestures::{
    EditLaunch, EnterAction, disconnect, edit_under_cursor, enter_action, mirror_plan,
    mirror_target_plan, pull_plan, resolve_opener, run_pane_gesture, shell_cwd,
};
use crate::keymap::Command;
use crate::mutations::{combine_pieces, launch_size_count, test_archive, unpack};
use crate::nav;
use crate::navigate::{Cd, cd, cd_in};
use crate::overlays::open_contextual_help;
use crate::refresh::refresh_panes;
use crate::screens::{open_drive_popup, plugin_config_summaries};
use crate::trail::walk_trail;
use crate::viewer_open::{open_viewer, viewer_do, viewer_sibling};
use norte_core::backend::Backend;
use norte_i18n::{t, ta};
use norte_proto::EntryKind;

/// Runs a named command (ADR 0006: the same names the palette and the wire
/// will see). A listing error on a cd does NOT bring the TUI down: the pane
/// stays where it was (visible warning: message bar, issue #20).
#[expect(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    reason = "command→effect dispatch table, not an API"
)]
pub async fn dispatch(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
    help_lines: &[ratatui::text::Line<'static>],
    // H3b: the negotiated language `Command::AppHelp` opens the corpus in.
    // See the same parameter on `run`.
    lang: norte_i18n::Lang,
    quick_mode: nav::Mode,
    confirm_quit: config::ConfirmQuit,
    // S3 (`app.settings`): the CURRENT config — read-only, to build the
    // overlay's rows when opening it (`crate::settings::build_rows`).
    cfg: &config::LoadedConfig,
    cmd: Command,
) -> Cd {
    // Only the cds (nav.enter/nav.parent) touch the background fill; the
    // rest of the commands leave it as it is (`Cancelled`).
    let mut cd_outcome = Cd::Cancelled;
    match cmd {
        // S2 (`[ui] confirm_quit`): ONLY this arm (the named dispatch of
        // `app.quit`, reachable by keymap AND by the palette) honors the
        // config and can open `Modal::ConfirmQuit`. The hardcoded
        // `app.quit = true`s from Ctrl+C scattered through the rest of this
        // file (each overlay has its own, documented in place) are the
        // emergency exit — they stay IMMEDIATE on purpose, never asking.
        Command::AppQuit => {
            if crate::app::quit_needs_confirm(confirm_quit, app.board.has_active()) {
                app.modal = Some(Modal::ConfirmQuit);
            } else {
                app.quit = true;
            }
        }
        // `--pick` (S2): the run loop is the only caller (the Enter/
        // Ctrl+Enter override below), and only ever under `--pick` — but the
        // guard stays here too, not just there, because `app.pick-accept` is
        // also reachable through the palette (H1 T4 put every catalogue name
        // there) and a preset a user hand-writes could bind it directly.
        // Outside `--pick` this is a no-op: there is nothing to accept into.
        Command::AppPickAccept => {
            if app.pick {
                app.picked = Some(app.focused().marked_paths());
                app.quit = true;
            }
        }
        // `pane.copy-path` (#286): the catalogue declared it `Live` from the
        // GPUI GUI, which implemented it; that one was retired and nobody
        // kept the command, so the catalogue was promising something nobody
        // did.
        //
        // Two paths, and the order matters: first the desktop's helper
        // (`wl-copy`, `xclip`), because it ANSWERS whether it worked; and if
        // there is none —the normal case in an SSH session— the OSC 52
        // sequence, which is an option a terminal has and a window does not.
        // A terminal that does not support it ignores it silently and there
        // is no way to ask it, so the message SAYS which path it took: that
        // is what turns an uncertainty into something the reader can verify
        // by pasting.
        Command::PaneCopyPath => {
            let paths = app.focused().marked_paths();
            if paths.is_empty() {
                app.message = Some(t("msg-nothing-selected"));
            } else {
                let bytes = norte_frontend::shell::clipboard_bytes(&paths);
                let n = paths.len().to_string();
                app.message = Some(match norte_frontend::shell::copy_to_clipboard(&bytes) {
                    norte_frontend::shell::ClipboardOutcome::Done(_) => {
                        ta("msg-paths-copied", &[("n", &n)])
                    }
                    norte_frontend::shell::ClipboardOutcome::NoHelper => {
                        app.pending_osc52 = Some(norte_frontend::shell::osc52(&bytes));
                        ta("msg-paths-copied-osc52", &[("n", &n)])
                    }
                    norte_frontend::shell::ClipboardOutcome::Failed => t("msg-clipboard-failed"),
                });
            }
        }
        Command::PaneSwitch => app.switch_focus(),
        Command::TabNew => app.tab_new(),
        Command::TabClose => app.tab_close(),
        Command::TabNext => app.tab_cycle(1),
        Command::TabPrev => app.tab_cycle(-1),
        Command::TabMoveLeft => app.tab_move(-1),
        Command::TabMoveRight => app.tab_move(1),
        Command::TabGoto1 => app.tab_goto(1),
        Command::TabGoto2 => app.tab_goto(2),
        Command::TabGoto3 => app.tab_goto(3),
        Command::TabGoto4 => app.tab_goto(4),
        Command::TabGoto5 => app.tab_goto(5),
        Command::TabGoto6 => app.tab_goto(6),
        Command::TabGoto7 => app.tab_goto(7),
        Command::TabGoto8 => app.tab_goto(8),
        Command::TabGoto9 => app.tab_goto(9),
        // Toggle, and through the SAME place as the side panels: the bar is
        // the application's chrome, so opening it cannot mean two things
        // depending on where it is asked from.
        Command::AppMenu => app.toggle_menu(),
        Command::LayoutSplitH => app.layout_split(norte_frontend::layout::Dir::Horizontal),
        Command::LayoutSplitV => app.layout_split(norte_frontend::layout::Dir::Vertical),
        Command::LayoutFocusNext => app.layout_focus(1),
        Command::LayoutFocusPrev => app.layout_focus(-1),
        Command::LayoutCloseSlot => {
            if app.layout_close_slot() {
                // Said out loud, with the shortcut the current preset really
                // binds: closing a panel is easy to do by accident and hard
                // to undo if you do not know how. The same sentence as the
                // window.
                app.message = Some(norte_frontend::notes::slot_closed(
                    app.chord_split_h.clone().as_deref(),
                    lang,
                ));
            } else {
                app.message = Some(norte_i18n::t("msg-layout-last-panel"));
            }
        }
        Command::LayoutGrow => app.layout_resize(1),
        Command::LayoutShrink => app.layout_resize(-1),
        Command::LayoutEqualize => app.layout_equalize(),
        Command::LayoutFlip => app.layout_flip(),
        Command::LayoutSetTarget => app.layout_set_target(),
        // L3: opening the sidebar is the moment to request the volumes, and
        // the ONLY one along with unfolding its section. If it was already
        // open they are not requested again: that press only takes the
        // keyboard.
        //
        // The request is left flagged (`App::places_wants_drives`) and the
        // loop serves it: no I/O happens here, and that way the same path
        // works for the other places the panel appears without going
        // through this key.
        Command::LayoutPlaces => app.toggle_places(),
        // The docked viewer asks for nothing here: what it reads comes from
        // `preview::want` in the loop, against each frame's cursor.
        Command::LayoutPreview => app.toggle_preview(),
        Command::LayoutProcesses => app.toggle_processes(),
        Command::LayoutLog => app.toggle_log(),
        Command::LayoutDiskMap => app.toggle_disk_map(),
        // The timeline (phase 7): opening is placing the slot, and reading
        // the journal is done HERE, which is where the backend is — like the
        // connections selector reads its file in its own arm.
        //
        // A daemon with no journal answers `Unsupported` and it is SAID: an
        // empty panel and a panel that cannot exist do not read the same. It
        // still opens either way, because the slot is what gives it
        // somewhere to say it.
        Command::LayoutTimeline => {
            app.toggle_timeline();
            // Re-read EVERY time the key leaves the panel open, not just on
            // creating it: between closing it and opening it again anything
            // could have happened —in fact that is the normal case, because
            // what gets done gets done with the panel closed— and a history
            // showing an earlier snapshot is worse than an empty one: the
            // empty one is noticeable.
            if app.timeline_slot().is_some() {
                load_timeline(app, backend, None).await;
            }
        }
        // #362: the terminal panel. The key opens, gives the keyboard and
        // returns it; it does NOT close, because closing kills the reader's
        // shell.
        //
        // The shell is started here and not in `toggle_terminal` because
        // starting it is I/O —a pty and a process— and the layout's state
        // does not do that. If it fails, it is said and the slot stays: an
        // empty panel that explains why is better than a key that does not
        // respond.
        Command::LayoutTerminal => {
            if app.terminal.is_some() {
                // There is already a shell: this is just the keyboard coming
                // and going, and asks for no directory at all. Asking here
                // would leave the reader unable to get back to THEIR
                // terminal because they are looking at a remote panel.
                app.toggle_terminal();
            } else {
                // Starting it does ask for a local directory, and
                // `shell_cwd` already knows how to say why there is none —
                // it is the same gate as `app.terminal`, and answers no over
                // a remote panel.
                match shell_cwd(app) {
                    Ok(dir) => {
                        app.toggle_terminal();
                        // The real size is set by the paint as soon as it
                        // knows which rectangle it got; this one is the
                        // startup size and lasts as long as the first turn
                        // takes.
                        match crate::termpanel::open(&dir, (80, 24)) {
                            Ok(t) => {
                                // Recorded, like its two siblings and with
                                // the same "not journalled" written: a shell
                                // the reader opens is the reader acting with
                                // their own permissions, not a norte mutation
                                // —there is no actor to attribute and no
                                // reversal to record—. But starting a shell
                                // is the most privileged thing a frontend
                                // does, and the log panel is now a surface
                                // that gets looked at.
                                tracing::info!(
                                    "TUI opened a shell in a terminal panel \
                                     (not journalled: no actor, no reversal)"
                                );
                                app.terminal = Some(t);
                            }
                            // The slot stays open even if the shell does not
                            // start: an empty panel with the reason written
                            // reads better than a key that does nothing.
                            Err(e) => app.message = Some(e.to_string()),
                        }
                    }
                    Err(msg) => app.message = Some(msg),
                }
            }
        }
        // #136: the tree opens, focuses and closes like the sidebar. Its
        // content is requested by the run loop, one branch per turn.
        Command::PaneTree => app.toggle_tree(),
        Command::LayoutMetadata => app.toggle_metadata(),
        // The layouts directory's listing is done HERE, off the runtime, and
        // arrives already built at `App` (rule 2). A directory that cannot
        // be read gives an empty list: the five factory ones remain, which
        // is more than nothing.
        Command::LayoutPick => {
            // Listing AND READING, both off the runtime: each row of the
            // selector paints its screen, and reading it as the cursor
            // passes over would be I/O in the loop (#244 M2, rule 2). With
            // no config directory there are no user files: the five factory
            // ones remain. It used to fall back to `PathBuf::default()`,
            // which reads `./layouts/` of the current directory — meaning,
            // cloning a repo and pressing F9 (#244 m3).
            let mine = match config::user_config_dir() {
                Some(dir) => tokio::task::spawn_blocking(move || {
                    use norte_frontend::layout::config;
                    config::list(&dir)
                        .into_iter()
                        .map(|name| norte_frontend::layout_picker::UserLayout {
                            tree: config::load(&dir, &name).map_err(|e| e.to_string()),
                            name,
                        })
                        .collect::<Vec<_>>()
                })
                .await
                .unwrap_or_default(),
                None => Vec::new(),
            };
            app.open_layout_picker(mine);
        }
        // The profiles. Listing the directory AND reading each one's
        // `norte.toml` —that is where the row's title and a broken one's
        // reason come from— both off the runtime, for the same reason as the
        // layouts (rule 2, #244). With no config directory there are no
        // profiles: empty list, and never `PathBuf::default()`, which reads
        // `./profiles/` of the current directory (#244 m3).
        Command::ProfilePick => {
            let profiles = match config::user_config_dir() {
                Some(dir) => {
                    tokio::task::spawn_blocking(move || norte_frontend::config::read_profiles(&dir))
                        .await
                        .unwrap_or_default()
                }
                None => Vec::new(),
            };
            app.open_profile_picker(profiles);
        }
        // #306: saving what is on screen as a profile. Only the prompt opens
        // here; disk is touched by Enter, in the run loop.
        Command::ProfileSaveAs => app.open_profile_save_as(),
        // `profile.next`/`profile.prev` cycle through the list WITHOUT
        // opening the selector, which is what whoever has two profiles and
        // alternates wants. The switch itself is done by the run loop
        // (task 4): here only which one is next gets decided.
        Command::ProfileNext | Command::ProfilePrev => {
            let profiles = match config::user_config_dir() {
                Some(dir) => {
                    tokio::task::spawn_blocking(move || norte_frontend::config::read_profiles(&dir))
                        .await
                        .unwrap_or_default()
                }
                None => Vec::new(),
            };
            app.pending_profile = norte_frontend::profile_picker::next_profile(
                &profiles,
                app.active_profile.as_deref(),
                matches!(cmd, Command::ProfileNext),
            );
        }
        // `pane.sync-nav`: the PERMANENT mirror. It navigates nothing by
        // itself —it switches the mode on or off and says so—; whoever
        // mirrors is the single point every navigation goes through.
        Command::PaneSyncNav => {
            app.sync_nav = !app.sync_nav;
            app.message = Some(t(if app.sync_nav {
                "msg-sync-nav-on"
            } else {
                "msg-sync-nav-off"
            }));
        }
        // `pane.mirror`: the location comes from the FOCUSED pane and the
        // other one travels.
        Command::PaneMirror => {
            let plan = mirror_plan(app);
            let origin = app.focus();
            cd_outcome = run_pane_gesture(app, backend, events, plan, origin).await;
        }
        // `pane.mirror-target`: the same gesture, but what travels is the
        // CURSOR's target (the folder under it, if it is one).
        Command::PaneMirrorTarget => {
            let plan = mirror_target_plan(app);
            let origin = app.focus();
            cd_outcome = run_pane_gesture(app, backend, events, plan, origin).await;
        }
        // `pane.pull`: the same gesture backwards — the location comes from
        // the OTHER pane and the focused one travels.
        Command::PanePull => {
            let plan = pull_plan(app);
            // The origin is the SAME "other panel" the plan resolved.
            let origin = app.target_index().unwrap_or_else(|| app.focus());
            cd_outcome = run_pane_gesture(app, backend, events, plan, origin).await;
        }
        // `pane.swap`: does NOT touch disk — both listings already existed
        // and only switch sides. The half `dispatch` does not see (fill in
        // flight, decoration fetches, probe dedup) travels to the run loop.
        Command::PaneSwap => {
            app.swap_panes();
            cd_outcome = Cd::Swapped;
        }
        Command::NavBack => {
            cd_outcome = walk_trail(app, backend, events, TrailStep::Back).await;
        }
        Command::NavForward => {
            cd_outcome = walk_trail(app, backend, events, TrailStep::Forward).await;
        }
        // `/` (spec 2026-07-18): starts the quick search in the config's
        // mode. With one already active the keys get swallowed before the
        // resolver, so this arm only runs to OPEN it — no recursion.
        Command::PaneQuickSearch => app.focused_mut().quick_start(quick_mode),
        // `Alt+↓` / `Ctrl+D` (spec 2026-07-18): with the popup open its keys
        // get swallowed before the resolver (overlay pattern) — these arms
        // only run to OPEN it.
        Command::PaneHistory => app.open_nav_popup(NavPopupKind::History),
        Command::PaneHotlist => app.open_nav_popup(NavPopupKind::Hotlist),
        // Spec 2026-09-15 D6/D7: popular ones and a SIDE's history. The side
        // is `panes[0]`/`panes[1]`, like in `pane.select-drive-left/-right`.
        Command::PanePopular => app.open_nav_popup(NavPopupKind::Popular),
        Command::PaneHistoryLeft => app.open_side_history(0),
        Command::PaneHistoryRight => app.open_side_history(1),
        // D5: jumping to the point is a NORMAL navigation —it enters the
        // trail— so `nav.back` undoes the jump.
        Command::NavJumpBack => {
            let pane = app.focus();
            match norte_frontend::history::jump_target(&app.history[pane]) {
                Ok(dir) => cd_outcome = cd_in(app, backend, events, pane, dir, Trail::Record).await,
                Err(key) => app.message = Some(t(key)),
            }
        }
        Command::NavSetJumpPoint => {
            let pane = app.focus();
            let dir = app.panes[pane].dir().clone();
            app.history[pane].set_jump(dir);
            app.message = Some(t("msg-nav-jump-point-set"));
        }
        // `pane.select-drive*` (design §D): `-left`/`-right` name a SIDE —
        // `panes[0]`/`panes[1]` — not the focus, which is what Total
        // Commander's `Alt+F1`/`Alt+F2` do; only the unsided variant reads
        // `app.focus()`.
        Command::PaneSelectDrive => {
            open_drive_popup(app, backend, app.focus(), false).await;
        }
        Command::PaneSelectDriveLeft => {
            open_drive_popup(app, backend, 0, false).await;
        }
        Command::PaneSelectDriveRight => {
            open_drive_popup(app, backend, 1, false).await;
        }
        // `Alt+F7` (liveSearch T6): opens the live search dialog. While it
        // is open its keys are eaten before the resolver (overlay pattern) —
        // this arm only runs to OPEN it.
        Command::PaneSearch => app.open_search_dialog(),
        // `Shift+F2`: compares the two panes. It only RESOLVES the params and
        // leaves them in `pending_compare` — launching belongs to the run
        // loop, which holds the channel and the Task.
        Command::PaneCompareDirs => app.request_compare(),
        // `Ctrl+Y`: plans a sync from this pane to the other. It only
        // RESOLVES the params (and the negatives, the journal one first);
        // launching belongs to the run loop. `Mirror` has no global key on
        // purpose: deleting at the destination is something asked for from
        // the diff panel, with what is about to be deleted in front of you.
        Command::PaneSyncDirs => {
            app.request_sync(norte_proto::methods::SyncMode::Update);
        }
        Command::CursorUp => app.focused_mut().move_up(1),
        Command::CursorDown => app.focused_mut().move_down(1),
        // #124: a PAGE is one screen of the pane (minus one row of
        // context), not a constant — the real height comes from the last
        // frame.
        Command::CursorPageUp => {
            let step = app.focused().page_step();
            app.focused_mut().move_up(step);
        }
        Command::CursorPageDown => {
            let step = app.focused().page_step();
            app.focused_mut().move_down(step);
        }
        Command::CursorTop => app.focused_mut().move_to_start(),
        Command::CursorBottom => app.focused_mut().move_to_end(),
        // A directory is navigated; a FILE is opened —with its associated
        // program if it is on this disk, and with the internal viewer if
        // not—, which is what an orthodox file manager does. Before, over a
        // file, this key did nothing and did not say so either.
        Command::NavEnter => match enter_action(app) {
            EnterAction::Cd(dir) => cd_outcome = cd(app, backend, events, dir).await,
            // Going up through the `..` row leaves the cursor over the
            // directory being left, same as the dedicated key
            // (`NavParent`): that is what makes going up and down
            // reversible, and it cannot depend on which of the two you used
            // to go up.
            EnterAction::Up(parent) => {
                let child = app.focused().dir().clone();
                app.focused_mut().set_pending_focus(child);
                cd_outcome = cd(app, backend, events, parent).await;
                if matches!(cd_outcome, Cd::Failed(_)) {
                    app.focused_mut().clear_pending_focus();
                }
            }
            EnterAction::OpenExternal => resolve_opener(app),
            EnterAction::View(path) => open_viewer(app, backend, events, path).await,
            EnterAction::Nothing => {}
        },
        Command::NavParent => {
            // Leaving a file's inner root = the dir that CONTAINS the
            // container (the syntactic parent would be a compound with no
            // marker: malformed, ADR 0018).
            let dir = app.focused().dir().clone();
            // Pending focus (spec 2026-07-24 §S1): the child we come from,
            // to select it in the parent's listing. On leaving a file's
            // inner root the child is NOT `dir` (that is the virtual
            // compound path, not a real entry in the parent's listing) but
            // the container file itself (`aref.outer`).
            let (parent, child) = match dir.archive_split() {
                Ok(Some(aref)) if aref.inner.is_empty() => {
                    let outer = aref.outer.clone();
                    (outer.parent(), outer)
                }
                _ => (dir.parent(), dir.clone()),
            };
            if let Some(parent) = parent {
                app.focused_mut().set_pending_focus(child);
                cd_outcome = cd(app, backend, events, parent).await;
                // Review S, M2: a FAILED `cd` (permission denied, daemon
                // error…) never calls `set_listing` (`cd`'s doc, `Err` arm),
                // so the hint just set above is never consumed — clearing it
                // here keeps it from surviving into an unrelated future
                // `cd`. `Cd::Suspended` (the TOFU modal, which RETRIES this
                // same navigation) KEEPS it on purpose: the retry must still
                // land on `child`. A `Cd::Cancelled` (Esc) also keeps it —
                // the reader stays on the same listing, and the hint dies
                // with the next cd that does land.
                if matches!(cd_outcome, Cd::Failed(_)) {
                    app.focused_mut().clear_pending_focus();
                }
            } else {
                // Root `/` or a Windows drive root (`parent()` = None):
                // before, this was a SILENT no-op (#20). Now it warns via
                // the bar.
                app.message = Some(t("msg-nav-at-top"));
            }
        }
        Command::PaneCopy | Command::PaneMove => {
            let kind = if cmd == Command::PaneCopy {
                TransferKind::Copy
            } else {
                TransferKind::Move
            };
            // Orthodox destination: the OTHER pane's DIRECTORY. The sources
            // are the marks, or the cursor if there are none (#103). The
            // rest — an editable name with a single item (#105), a list
            // confirm with several — is decided by `open_transfer`, which is
            // the SAME door a mouse drop comes in through: a second path for
            // submitting a transfer is a path left without confirmation,
            // without collision handling, or without undo the moment either
            // one changes.
            // With no destination designated and more than two panels, it is
            // not guessed: a copy toward a panel the reader did not have in
            // mind is silent data loss (ADR 0058 D7).
            // With no candidate for the `target` role the operation ASKS
            // (spec L1): with a single listing there is no "the other
            // panel", and with three or more it is not guessed which one —
            // in both cases the direction is typed instead of failing.
            // Guessing it would be silent data loss (ADR 0058 D7); staying
            // silent, a dead key.
            if let Some(dest) = app.target_index() {
                app.open_transfer(kind, app.focus(), dest, None);
            } else {
                app.open_transfer_dest(kind);
            }
        }
        // #105: shift+F6 — rename in situ (Move to `from`'s PARENT, editable
        // name). Also correct on the virtual pane: the destination comes
        // from the hit's own path, not the pane's dir.
        Command::PaneRename => app.open_rename(),
        // #310: batch rename without AI. Opens the TEMPLATE; the run loop
        // requests the plan on confirm, and it is reviewed by the same modal
        // that already reviews the AI one — what makes the operation safe is
        // not where the names came from.
        Command::PaneRenameBatch => {
            if app.rename_batch_names().is_empty() {
                app.message = Some(t("msg-rename-batch-nothing"));
            } else {
                app.open_rename_batch();
            }
        }
        // #106: Ctrl+R — manual reload. Reuses the post-mutation refresh
        // (cancelable rule 3; marks survive via refill with VISIBLE pruning,
        // cursor by index; the virtual search pane is skipped — its hits do
        // not live in a dir). Both panes, as after one of our own tasks: an
        // external change rarely respects focus.
        // #118: the outcome TRAVELS to the run loop (`Cd::Refreshed`) —
        // dispatch does not see `fill`/`last_probed`, and without the ritual
        // a live paginated drainer would duplicate rows over the freshly
        // completed listing.
        Command::PaneRefresh => {
            cd_outcome = Cd::Refreshed(refresh_panes(app, backend, events).await);
        }
        // Insert/Ctrl+A/Ctrl+Shift+A/`*` (#103): mc/Total Commander —
        // toggle this entry's mark and advance (holding Insert sweeps a
        // range). Review MAJOR: under a quick search in Filter,
        // `toggle_mark` acts on the FILTERED selection while the real cursor
        // is something else — advancing the real cursor desyncs the range
        // the filter sweeps. The full composition (marking + what it
        // advances to depending on whether there is a filter, clamped
        // without wrapping) lives in the shared model.
        Command::MarkToggle => app.focused_mut().toggle_mark_and_advance(),
        // The rest of the "mark while moving" family. The page SIZE comes
        // from the last PAINTED frame, like the `pane.page-down` next to it:
        // a constant here would mark a different stretch than the one the
        // cursor covers as soon as the window did not measure that.
        Command::MarkToggleUp => app.focused_mut().toggle_mark_and_retreat(),
        Command::MarkTogglePageDown | Command::MarkTogglePageUp => {
            let n = app.focused().page_step();
            let down = cmd == Command::MarkTogglePageDown;
            app.focused_mut().toggle_mark_page(n, down);
        }
        Command::MarkToTop => app.focused_mut().mark_to_top(),
        Command::MarkToBottom => app.focused_mut().mark_to_bottom(),
        Command::MarkAll => app.focused_mut().mark_all(),
        Command::MarkInvert => app.focused_mut().invert_marks(),
        Command::MarkClear => app.focused_mut().clear_marks(),
        // `+`/`-` (#103 T9): open the pattern modal (free text, see the
        // `app.modal.is_some()` arm above) — marking/unmarking runs on
        // confirm (`mark_pattern_confirm`), not here.
        Command::MarkPatternAdd => app.open_mark_pattern(true),
        Command::MarkPatternRemove => app.open_mark_pattern(false),
        // #313: the extension of the entry UNDER THE CURSOR. With nothing
        // under the cursor, or over something with no extension, it marks
        // nothing and says so: marking "everything that also has no
        // extension" is another rule nobody asked for.
        Command::MarkExtensionAdd | Command::MarkExtensionRemove => {
            let add = cmd == Command::MarkExtensionAdd;
            let n = app.focused_mut().mark_same_extension(add);
            if n == 0 {
                app.message = Some(t("msg-mark-no-extension"));
            }
        }
        Command::MarkFiles => {
            app.focused_mut().mark_kind(false);
        }
        Command::MarkDirs => {
            app.focused_mut().mark_kind(true);
        }
        // The safety net for whoever hit "unmark all" by accident. With no
        // snapshot —no bulk gesture yet, or a `cd` that took it away— it
        // says so, instead of leaving the panel with no marks pretending
        // that was how it was before.
        Command::MarkRestore => match app.focused_mut().restore_previous_marks() {
            Some(n) => app.message = Some(ta("msg-marks-restored", &[("n", &n.to_string())])),
            None => app.message = Some(t("msg-marks-nothing-to-restore")),
        },
        // #104: F7 — create a directory in the focused pane. In the search
        // VIRTUAL pane there is no visible destination directory (review
        // MINOR-2: `dir()` is the walk's root, not what is painted).
        Command::PaneMkdir => {
            if app.focused().virtual_search {
                app.message = Some(t("msg-mkdir-in-search"));
            } else {
                app.open_mkdir();
            }
        }
        // M4-IA: AI-assisted rename of the focused dir. In the search
        // VIRTUAL pane there is no single directory to rename (same criterion
        // as `PaneMkdir`). The prompt's keys and the request live in the
        // run loop (Tier-A interception + `AiRenameRun`).
        Command::PaneAiRename => {
            if app.focused().virtual_search {
                app.message = Some(t("msg-ai-rename-in-search"));
            } else {
                app.open_ai_rename();
            }
        }
        // Phase 8: organize the focused directory. WITHOUT an instruction
        // prompt, unlike renaming — what is asked is "look at this
        // directory and propose a shape", and an empty text box in front
        // would suggest there is something to type. The plan lands in the
        // same place as an `organizer` plugin's, and is reviewed the same
        // way.
        Command::PaneOrganize => {
            if app.focused().virtual_search {
                app.message = Some(t("msg-ai-rename-in-search"));
            } else {
                app.pending_organize = true;
            }
        }
        // M4-IA-2: semantic search over the index (all roots). In the
        // search VIRTUAL pane the prompt would collide with the mode's own
        // Esc/Enter semantics (same criterion as `PaneAiRename`). The
        // prompt's keys and the request live in the run loop (Tier-A
        // interception + `SemanticRun`).
        Command::PaneSemanticSearch => {
            if app.focused().virtual_search {
                app.message = Some(t("msg-semantic-in-search"));
            } else {
                app.open_semantic_search();
            }
        }
        Command::PaneDelete | Command::PaneDeletePermanent => {
            // F8 = trash if the provider declares it; without it, the SAME
            // dialog warns it is PERMANENT (degradation with an informed
            // user, ADR 0009). shift+f8 = permanent. The capability is
            // probed ONCE PER BATCH using the first item (#103 T10): all the
            // marks live in the same directory of the same provider, so N
            // probes would be N network round-trips for the same answer.
            if let Some(first) = app.focused().marked_paths().first() {
                let has_trash = backend
                    .capabilities(first)
                    .await
                    .is_ok_and(|c| c.flags.contains(norte_proto::CapabilityFlags::TRASH));
                let permanent = cmd == Command::PaneDeletePermanent || !has_trash;
                app.open_delete_modal(permanent);
            }
        }
        Command::PaneView => {
            // Symlinks too (same criterion as nav.enter): if it points to a
            // dir, the read will fail with a visible message.
            let target = app
                .focused()
                .selected()
                .filter(|e| matches!(e.kind, EntryKind::File | EntryKind::Symlink))
                .map(|e| e.path.clone());
            if let Some(path) = target {
                open_viewer(app, backend, events, path).await;
            }
        }
        // #140: pick from `connections.toml`. Reading the file belongs to
        // the frontend —the picker does not touch disk— and navigating, to
        // the run loop.
        Command::PaneConnect => {
            let dir = norte_core::connect::config_dir();
            match norte_core::connect::named_connections(&dir).await {
                // The unusable ones go AFTER the good ones and are not mixed
                // in (#365): the first thing seen is what does lead
                // somewhere, and what is not usable stays below, visible and
                // unselectable. Making them disappear would leave the reader
                // hunting for why a connection they wrote is missing.
                Ok((rows, unusable)) => app.open_connections_picker(
                    rows.into_iter()
                        .map(|(name, url)| {
                            norte_frontend::connections_picker::Row::buena(name, url)
                        })
                        .chain(unusable.into_iter().map(|(name, reason)| {
                            norte_frontend::connections_picker::Row::unusable(name, reason)
                        }))
                        .collect(),
                ),
                Err(e) => app.message = Some(error_message(&e)),
            }
        }
        // And disconnecting RELEASES the session, not just leaves the
        // panel: otherwise the socket would stay open until the session
        // expired on its own and "disconnect" would just be a name for
        // going elsewhere.
        Command::PaneDisconnect => disconnect(app, backend).await,
        Command::PaneOpen => resolve_opener(app),
        // #133: F4 EDITS. The run loop runs it, like the shell and like
        // `pane.open`: it is the one holding the terminal, and suspending
        // the TUI to hand it back to a full-screen program is exactly what
        // `app.terminal` already does.
        //
        // The path travels as an ARGUMENT and not inside a command line: a
        // name with a quote, a `$` or a line break either breaks the line or
        // runs part of itself, and here names are bytes (rule 1).
        Command::PaneEdit => match edit_under_cursor(app) {
            Ok(EditLaunch::Shell(pending)) => app.pending_shell = Some(pending),
            Ok(EditLaunch::Open(pending)) => app.pending_open = Some(pending),
            Err(msg) => app.message = Some(msg),
        },
        // #312: compare TWO files. Same split as editing —the run loop is
        // the one holding the terminal— and the same kind of result, because
        // the deal is the same: `[ui] diff` can be a window, and the default
        // `diff -u` is a terminal program whose output has to be held onto.
        Command::PaneCompareFiles => match crate::gestures::compare_files(app) {
            Ok(EditLaunch::Shell(pending)) => app.pending_shell = Some(pending),
            Ok(EditLaunch::Open(pending)) => app.pending_open = Some(pending),
            Err(msg) => app.message = Some(msg),
        },
        // Shift+F4: an EMPTY file in this directory and the editor on top.
        //
        // The name is asked HERE and the daemon creates the file
        // (`fs.create`, #290), not the editor on save. Leaving it to the
        // editor —what this key used to do— created the file outside norte:
        // without going through policy, without a journal entry and without
        // undo (hard rule 4). It is also what the window does with this same
        // command.
        //
        // The guard is that the pane has a NATIVE shape, not the shell's
        // `shell_cwd`: that one fails for two reasons —a remote pane, or a
        // local one with no valid cwd for a child (Windows, a path that only
        // exists with `\\?\`)— and the second does not apply here. Creating
        // needs no cwd, and the editor carries it as best-effort, so gating
        // on it would have rejected `edit-new` outright on a long Windows
        // path.
        Command::PaneEditNew => {
            if norte_vfs_local::vpath_to_native(app.focused().dir()).is_ok() {
                app.open_edit_new();
            } else {
                app.message = Some(crate::gestures::shell_remote_message(app));
            }
        }
        // #135 (S4, design §D): all three are RESOLVED here and the run
        // loop executes them, since it owns the terminal — same split as
        // `pane.open`. None of this goes to the journal: a shell the user
        // opens is the user acting with their own permissions, not a norte
        // mutation (there is no actor to attribute nor a reversal to
        // record), and whatever changes on disk is picked up by the watcher
        // and the refresh on return.
        Command::AppTerminal => match shell_cwd(app) {
            Ok(dir) => {
                app.pending_shell = Some(crate::app::PendingShell {
                    argv: vec![norte_frontend::shell::login_shell().into_os_string()],
                    cwd: Some(dir),
                    // The shell is already interactive: on leaving it,
                    // returning to the panels is exactly what is wanted.
                    wait_for_key: false,
                    check_regular: None,
                });
            }
            Err(msg) => app.message = Some(msg),
        },
        // Phase 9: the HANDOFF to the window. Here it only CHECKS and
        // requests; dumping the screen, releasing the session and launching
        // the window are three trips that do not fit in dispatching a key,
        // so the session writer and the run loop do them (`request_handoff`).
        //
        // The two blockers are stated BEFORE, with their reason, and are the
        // same ones that dim the row in the palette and in the reference
        // sheet: a key that does nothing is tolerable; one that releases the
        // screen and launches a window nobody will see, is not.
        Command::AppHandoff => {
            if !app.backend_daemon {
                app.message = Some(t("msg-handoff-needs-daemon"));
            } else if !app.has_desktop {
                app.message = Some(t("msg-handoff-needs-desktop"));
            } else {
                app.pending_handoff = true;
            }
        }
        // #142: mc's SUBSHELL, not the scrollback. Here it only ASKS; the
        // run loop starts it —lazily, the first time— and hands it the
        // screen, since it owns the terminal. Same split as the three above
        // and for the same reason.
        //
        // Unlike `app.terminal`, the directory is NOT resolved here: the
        // shell already exists between one keypress and the next, and where
        // it goes is decided when the terminal is handed to it.
        Command::AppTogglePanels => app.pending_subshell = true,
        // Only opens the prompt; its Enter leaves the `$SHELL -c` pending,
        // in the run loop (which is the one that reads a free-text modal's
        // raw keys). The locality guard repeats there — the directory may
        // have changed between opening the prompt and confirming it.
        Command::PaneCommandLine => match shell_cwd(app) {
            Ok(_) => app.open_command_line(),
            Err(msg) => app.message = Some(msg),
        },
        Command::ViewerClose => {
            // With the preview docked, `viewer.close` RELEASES the keyboard
            // and leaves the panel where it is: closing it is
            // `layout.preview`. Closing a panel the reader only wanted to
            // stop operating is the wrong response, and it is the same rule
            // as the sidebar's.
            if app.key_owner() == crate::app::KeyOwner::Preview {
                app.return_keys_to_panes();
            } else {
                // `close_viewer` clears the viewer AND its thumbnail at the
                // same time (review finding, T3 phase 5): an `app.viewer =
                // None` left loose here left `app.viewer_imagen` pointing at
                // the previous image while the viewer was already gone.
                app.close_viewer();
            }
        }
        Command::ViewerUp => viewer_do(app, |v| v.scroll_up(1)),
        Command::ViewerDown => viewer_do(app, |v| v.scroll_down(1)),
        Command::ViewerPageUp => viewer_do(app, |v| v.scroll_up(crate::viewer::PAGE)),
        Command::ViewerPageDown => viewer_do(app, |v| v.scroll_down(crate::viewer::PAGE)),
        Command::ViewerTop => viewer_do(app, crate::viewer::Viewer::scroll_top),
        Command::ViewerBottom => viewer_do(app, crate::viewer::Viewer::scroll_bottom),
        Command::ViewerLeft => viewer_do(app, |v| v.scroll_left(1)),
        Command::ViewerRight => viewer_do(app, |v| v.scroll_right(1)),
        Command::ViewerEncoding => viewer_do(app, crate::viewer::Viewer::cycle_encoding),
        Command::ViewerEncodingAuto => viewer_do(app, crate::viewer::Viewer::reset_encoding),
        Command::ViewerHex => viewer_do(app, crate::viewer::Viewer::toggle_hex),
        Command::ViewerZoomIn => viewer_do(app, crate::viewer::Viewer::zoom_in),
        Command::ViewerZoomOut => viewer_do(app, crate::viewer::Viewer::zoom_out),
        Command::ViewerZoomFit => viewer_do(app, crate::viewer::Viewer::zoom_fit),
        Command::ViewerNext => viewer_sibling(app, backend, events, true).await,
        Command::ViewerPrev => viewer_sibling(app, backend, events, false).await,
        // H3c: the page for WHERE the reader IS, not the index. The whole
        // body lives in `open_contextual_help` (documented there) so tests
        // open help through the SAME path as F1.
        // H3e: the catalogue snapshot is taken HERE, on the opening path —
        // a single call, never while painting. A backend failure leaves help
        // without extension rows (and with every `plugin:` command dimmed),
        // which is exactly what "could not find out" means; it never brings
        // down all of help.
        Command::AppHelp => {
            let plugins = backend.plugins_list().await.ok();
            open_contextual_help(app, lang, help_lines, plugins.as_ref());
        }
        Command::PaneNamesEncoding => {
            // #57: cycles the focused pane's non-UTF8 name reinterpretation
            // (display-only, rule 1). The announcement goes through the bar.
            let label = app.focused_mut().cycle_name_encoding();
            app.message = Some(match label {
                Some(enc) => ta("msg-names-encoding", &[("enc", enc)]),
                None => t("msg-names-encoding-off"),
            });
        }
        Command::PaneToggleHidden => {
            // #107: presentation-only — the pane sets dotfiles aside/back,
            // the provider does not re-list. The announcement goes through
            // the bar.
            let showing = app.focused_mut().toggle_hidden();
            app.message = Some(if showing {
                t("msg-hidden-shown")
            } else {
                t("msg-hidden-hidden")
            });
        }
        Command::AppTheme => app.open_theme_picker(),
        // ASYNC for the same reason as `app.palette`: the plugin column rows
        // come from `plugin.list` (approved + activated). A failed fetch
        // does NOT prevent the picker from opening — it degrades to
        // builtins + attrs, same as the palette degrades to built-ins.
        // #138: the same semantics as a click on the header
        // (`SortSpec::after_click`) — the active column reverses, a new one
        // sorts ascending— and on the FOCUSED pane, not on both: order
        // belongs to a listing, like the cursor.
        Command::PaneSortName => app.sort_focused_by(norte_frontend::SortColumn::Name),
        Command::PaneSortExt => app.sort_focused_by(norte_frontend::SortColumn::Extension),
        Command::PaneSortSize => app.sort_focused_by(norte_frontend::SortColumn::Size),
        Command::PaneSortTime => app.sort_focused_by(norte_frontend::SortColumn::Mtime),
        // The "sort menu" is the columns dialog: that is where the column,
        // the direction and `dirs_first` are, and `dialog.sort` sorts by the
        // row under the cursor. A second screen for the same thing would be
        // another one to maintain and another one to learn.
        Command::PaneSortMenu => {
            let plugins = backend
                .plugins_list()
                .await
                .map(|l| l.plugins)
                .unwrap_or_default();
            app.open_columns_picker(&plugins);
        }
        // #139: properties come from the listing. The only thing that needs
        // asking for is what a listing does not know —how much space a
        // folder takes—, and it is asked only if the entry is one.
        Command::PaneProperties => {
            if let Some(dir) = app.open_properties() {
                // A folder's date does not come in a lazy listing (#52) and
                // a `stat` knows it: it is requested once, on open.
                if let Ok(fresh) = backend.stat(&dir).await {
                    app.properties_hydrate(fresh);
                }
                launch_size_count(app, backend, vec![dir], true).await;
            }
        }
        // #314: change permissions. The operand is the usual one —what is
        // marked, or the cursor—, and the field is pre-filled with the mode
        // of the entry under the cursor: typing over an empty field is how
        // the execute bit gets taken away from something that had it.
        //
        // The `stat` with `posix.mode` is requested HERE and not pulled from
        // the listing: norte's listings are lazy (#52) and the mode does not
        // travel in them unless someone asks for it.
        Command::PaneChmod => {
            let targets = app.focused().marked_paths();
            if targets.is_empty() {
                app.message = Some(t("msg-nothing-selected"));
            } else {
                let cursor = app.focused().selected().map(|e| e.path.clone());
                let mode = match cursor {
                    Some(p) => backend
                        .stat_attrs(&p, &["posix.mode".to_owned()])
                        .await
                        .ok()
                        .and_then(|e| norte_frontend::chmod::mode_of(&e)),
                    None => None,
                };
                app.open_chmod(targets, mode);
            }
        }
        // And counting by hand, over what is MARKED (or the cursor if there
        // are no marks): "how much space does all this take?" is a question
        // about the selection.
        Command::PaneDirSize => {
            let targets = app.focused().marked_paths();
            launch_size_count(app, backend, targets, false).await;
        }
        // #311: checksums. Computing is over what is marked (the usual
        // operand); verifying is over the checksum file under the cursor,
        // and resolves the names against ITS directory.
        Command::PaneChecksum => {
            let paths = app.focused().marked_paths();
            if paths.is_empty() {
                app.message = Some(t("msg-nothing-selected"));
            } else {
                app.pending_checksum = Some(crate::app::ChecksumRequest::Compute { paths });
            }
        }
        Command::PaneChecksumVerify => match app.focused().selected().map(|e| e.path.clone()) {
            Some(sums) => {
                app.pending_checksum = Some(crate::app::ChecksumRequest::Verify { sums });
            }
            None => app.message = Some(t("msg-nothing-selected")),
        },
        // #132: writing archives. The five commands the four imported
        // presets bind and norte did not have.
        Command::PanePack => app.open_pack(),
        Command::PaneSplitFile => app.open_split(),
        Command::PaneUnpack => unpack(app, backend).await,
        Command::PaneTestArchive => test_archive(app, backend).await,
        Command::PaneCombineFiles => combine_pieces(app, backend).await,
        Command::PaneColumns => {
            let plugins = backend
                .plugins_list()
                .await
                .map(|l| l.plugins)
                .unwrap_or_default();
            app.open_columns_picker(&plugins);
        }
        Command::AppExtensions => match backend.plugins_list().await {
            // The catalogue arrives ALREADY sorted by category and id from
            // the core.
            Ok(list) => {
                // (P1 encoding audit F1) INGEST: clamps+masks `description`
                // ONCE here, not on every frame of `plugin_description_line`
                // — a defense against a hostile/compromised daemon that
                // ignores the manifest's cap.
                let mut plugins = list.plugins;
                crate::app::clamp_plugin_descriptions(&mut plugins);
                // And the catalogue the manager just brought declares the
                // panels again (phase 3): approving a plugin here has to
                // place its panel in this session, and deactivating it has
                // to remove it. Asking only at startup left the latter never
                // happening.
                app.kinds.insert_panels(&plugins);
                app.extensions = Some(ExtensionManager {
                    plugins,
                    errors: list.errors,
                    cursor: 0,
                    focus: crate::app::ExtFocus::List,
                    config: None,
                });
            }
            Err(e) => app.message = Some(error_message(&e)),
        },
        // Ctrl+P / vim `:` (H1 T4, spec-promised): opens the palette over the
        // PRECOMPUTED snapshot (`App::palette_rows`, `main::build_keymaps`
        // + hot-reload) — it never recomputes the effective keymap here.
        // Choosing `app.palette` FROM the palette (the run loop closes it
        // BEFORE dispatching, `enter`) is an observable no-op: it closes and
        // reopens empty — harmless, no state recursion.
        //
        // (P1) it is now ASYNC, like `app.extensions` above: the plugin rows
        // need `backend.plugins_list().await` (approved + activated,
        // `palette::plugin_rows`). Unlike `app.extensions` (which does NOT
        // open the manager if the fetch fails), the built-ins must ALWAYS be
        // dispatchable — a downed daemon must not bring down the whole
        // palette, only degrade it (no plugin rows + a notice), same
        // principle as "a listing error does not bring down the TUI" as the
        // rest of `dispatch`.
        Command::AppPalette => {
            // MINOR-6 (H1 close): Ctrl+P/`:` live in `[global]`, merged into
            // BOTH effective keymaps — the palette can also be opened from
            // the viewer, not only from browse (`rows_for_context` doc).
            let mut rows =
                crate::palette::rows_for_context(&app.palette_rows, app.viewer.is_some());
            match backend.plugins_list().await {
                Ok(list) => {
                    // (P1 encoding audit F1) INGEST: same clamp as the
                    // `app.extensions` arm — a single entry point, same cap.
                    let mut plugins = list.plugins;
                    crate::app::clamp_plugin_descriptions(&mut plugins);
                    // Same catalogue, same panel declaration as in the
                    // manager's arm (phase 3).
                    app.kinds.insert_panels(&plugins);
                    rows.extend(crate::palette::plugin_rows(&plugins));
                }
                Err(e) => app.message = Some(error_message(&e)),
            }
            app.palette = Some(Palette::with_recent(rows, &app.palette_recent));
        }
        // "Go to anywhere" (phase 6). The connections are read HERE, the
        // same way `pane.connect` reads them, and for the same reason:
        // touching disk belongs to the run loop, not the model. If they
        // cannot be read, it opens all the same with one fewer section — the
        // screen that joins six lists does not fall over because one is
        // missing, and saying so in the bar would cover up what the reader
        // came to do.
        Command::AppGoto => {
            let dir = norte_core::connect::config_dir();
            // Only the ones that lead somewhere, HERE: "go to anywhere" is a
            // list of destinations, and an unusable entry is not one. The
            // place that says what is wrong with it is the connections
            // picker (#365), which is where the reader goes to fix it.
            let (connections, _unusable) = norte_core::connect::named_connections(&dir)
                .await
                .unwrap_or_default();
            crate::goto::open(app, &connections);
        }
        // `F11` (S3): settings overlay — the rows are born from the CURRENT
        // `cfg` (same criterion as `help_lines`/`app.palette_rows`: rebuilt
        // on open, never a carried-over copy). Plugins section (G3c): a
        // summary PER plugin with `[config]` (real now, no longer P2's
        // informational note — `plugin_config_summaries`).
        Command::AppSettings => {
            let summaries = plugin_config_summaries(backend).await;
            app.settings = Some(Settings::new(crate::settings::build_rows(cfg, &summaries)));
        }
        Command::TaskCancel => {
            app.message = Some(if app.board.cancel_last_running() {
                t("msg-cancelling")
            } else {
                t("msg-no-tasks")
            });
        }
        // The same task that would be cancelled, and the meaning from its
        // LIVE state (ADR 0147). Against a daemon that cannot pause, it
        // SAYS so: a pause that does not happen and is not reported is worse
        // than not offering one.
        //
        // The serial queue (ADR 0149): the session's switch, and moving the
        // process panel's flagged task within the queue.
        Command::TaskQueue => {
            app.enqueue = !app.enqueue;
            app.message = Some(t(if app.enqueue {
                "msg-queue-on"
            } else {
                "msg-queue-off"
            }));
        }
        Command::TaskUp | Command::TaskDown => {
            let up = cmd == Command::TaskUp;
            app.message = Some(match app.processes_selected() {
                None => t("msg-no-tasks"),
                Some(task) => match task.mover_en_cola(up).await {
                    Ok(()) => t("msg-queued-moved"),
                    Err(norte_proto::Error::Unsupported) => t("msg-queued-not-moved"),
                    Err(e) => error_message(&e),
                },
            });
        }
        // Retry the transfer that failed, with its SAME options (ADR 0148):
        // the context was already saved for the collision dialog, and
        // without this a network failure forced redoing the operation by
        // hand.
        Command::TaskRetry => {
            if let Some(r) = app.board.last_failed_retry() {
                app.message = Some(t("msg-retrying"));
                crate::mutations::submit_transfer(app, backend, r.kind, r.from, r.to, r.opts).await;
            } else {
                app.message = Some(t("msg-no-retry"));
            }
        }
        // The call runs in its own task and is awaited only BRIEFLY: against
        // a hung remote daemon, awaiting it here would freeze the UI loop
        // until its thirty-second deadline. If it does not answer in time,
        // "pausing…" stays put, and the real state arrives via progress.
        Command::TaskPause => {
            app.message = Some(match app.board.last_running() {
                None => t("msg-no-tasks"),
                Some((task, _))
                    if !norte_frontend::tasks::pausable(task.progress().borrow().kind) =>
                {
                    t("msg-pause-not-this")
                }
                Some((task, paused)) => {
                    let requested = tokio::spawn(async move { task.set_paused(!paused).await });
                    let wait = std::time::Duration::from_millis(300);
                    match tokio::time::timeout(wait, requested).await {
                        Ok(Ok(Err(norte_proto::Error::Unsupported))) => t("msg-pause-unsupported"),
                        Ok(Ok(Err(e))) => error_message(&e),
                        _ if paused => t("msg-resuming"),
                        _ => t("msg-pausing"),
                    }
                }
            });
        } // No wildcard (#112): `Command` is exhaustive — a new command with
          // no arm is a COMPILE error, not a runtime panic.
    }
    cd_outcome
}

/// Fetches a page of the timeline and puts it in its slot (phase 7).
///
/// `from` is the cursor: `None` for the first —the newest— and the previous
/// one's `next_before_seq` to keep going backward.
///
/// A failure is REPORTED in the bar and leaves the panel as it was. The two
/// that are truly expected are a daemon with no journal (`Unsupported`) and
/// one that does not know the method, and both mean the same thing to the
/// reader: there is no history to show here. An empty panel with no
/// explanation reads as "you have done nothing", which is a different thing.
pub async fn load_timeline(app: &mut App, backend: &Backend, from: Option<i64>) {
    let Some(id) = app.timeline_slot() else {
        return;
    };
    match backend
        .journal_list(from, crate::timeline::PER_PAGE, None)
        .await
    {
        Ok(page) => {
            if let Some(t) = app.panes.timeline_mut(id) {
                if from.is_none() {
                    // A REREAD returns to the row the cursor had, if it
                    // still exists: rereading with the panel open must not
                    // move the reader from where they were.
                    let return_to = t.selected().map(|r| r.seq);
                    *t = norte_frontend::timeline::Timeline::new(&page.rows, page.next_before_seq);
                    if let Some(seq) = return_to
                        && let Some(i) = t.rows().iter().position(|r| r.seq == seq)
                    {
                        t.set_cursor(i);
                    }
                } else {
                    t.extend(&page.rows, page.next_before_seq);
                }
            }
        }
        Err(norte_proto::Error::Unsupported) => {
            app.message = Some(t("timeline-unavailable"));
        }
        Err(e) => app.message = Some(error_message(&e)),
    }
}
