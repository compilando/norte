//! A key's routing: who keeps it.
//!
//! It is a precedence CHAIN, and the order is the specification: the menu
//! and the pickers rule over the listing, a modal rules over almost
//! everything (`modal_wins`), and whatever nobody claims falls through to
//! the keymap resolver, which is what turns a chord into a [`Command`].
//!
//! It used to live inside `run`, and that is why `run` had twenty-two
//! hundred lines. It comes out whole because it does not depend on the
//! `select!`: it receives the in-flight work ([`InFlight`]) and whatever
//! hot reload can replace, and it returns once the key has been served —the
//! loop's `continue`s are `return`s here, which is the same thing said
//! without a loop.

use crate::app::{App, Modal, PAGE, Palette, PromptKind, error_message};
use crate::config::{self};
use crate::dispatch::dispatch;
use crate::event_loop::{launch_pending, run_command};
use crate::gestures::{keyboard_owner, submit_command_line};
use crate::jobs::{
    AiRenameRun, InFlight, RenameBatchRun, SemanticRun, launch_search, on_compare_key,
    on_search_dialog_key, on_search_enter, on_search_escape, on_sync_key,
};
use crate::keymap::{
    Command, Count, Resolution, Resolver, chord_from_crossterm, count_ignored_message,
    parse_plugin_key, unavailable_message,
};
use crate::lua::{resolve_lua_trust, run_lua_command};
use crate::mutations::{on_dialog_key, submit_transfer};
use crate::nav;
use crate::navigate::{apply_cd, cd, settle_cd};
use crate::overlays::{close_stale_overlays, help_owns_keys, modal_wins, palette_help};
use crate::refresh::{after_panes_refresh, reap_search_run, refresh_panes};
use crate::screens::{
    HelpDispatch, on_columns_key, on_connections_picker_key, on_disk_map_key, on_extensions_key,
    on_help_key, on_layout_picker_key, on_nav_popup_key, on_panel_key, on_places_key,
    on_processes_key, on_profile_picker_key, on_settings_key, on_theme_picker_key, on_timeline_key,
    on_tree_key, run_plugin_command,
};
use crate::shortcuts_editor::{Maps, on_shortcuts_key};
use crate::trail::{nav_enter_target, nav_stalled};
use crossterm::event::KeyEvent;
use crossterm::event::{KeyCode, KeyModifiers};
use norte_core::TransferOptions;
use norte_core::backend::Backend;
use norte_frontend::SEMANTIC_K;
use norte_i18n::{t, ta};
use norte_proto::VPath;

/// Routes a key through the precedence chain. See the module.
#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "loop wiring, not an API"
)]
pub async fn on_key(
    app: &mut App,
    backend: &Backend,
    capture: &mut crate::mouse::Capture,
    // The terminal travels INSIDE (`events.terminal()`): it had to have a
    // single owner so a long wait could be repainted, and that owner is the
    // console.
    events: &mut crate::console::Console<'_>,
    resolver: &mut Resolver,
    viewer_resolver: &mut Resolver,
    dialog_resolver: &mut Resolver,
    help_lines: &mut Vec<ratatui::text::Line<'static>>,
    lang: norte_i18n::Lang,
    quick_mode: nav::Mode,
    confirm_quit: config::ConfirmQuit,
    cfg: &config::LoadedConfig,
    cli_preset: Option<&str>,
    lua_host: Option<&crate::lua::LuaHost>,
    work: &mut InFlight,
    key: KeyEvent,
) {
    app.message = None;
    if app.menu.is_some() && !modal_wins(app) {
        // The menu bar: FIXED keys, like the palette.
        // There are no `dialog.*` verbs for "next
        // menu", so they cannot come from the keymap
        // either.
        let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
        match key.code {
            KeyCode::Esc if plain => app.close_menu(),
            KeyCode::Left if plain => {
                if let Some(m) = &mut app.menu {
                    m.cycle_menu(-1);
                }
            }
            KeyCode::Right if plain => {
                if let Some(m) = &mut app.menu {
                    m.cycle_menu(1);
                }
            }
            KeyCode::Up if plain => {
                if let Some(m) = &mut app.menu {
                    m.cycle_item(-1);
                }
            }
            KeyCode::Down if plain => {
                if let Some(m) = &mut app.menu {
                    m.cycle_item(1);
                }
            }
            KeyCode::Enter if plain => {
                // The menu CLOSES before dispatching, for
                // the same reason as the palette: the
                // command may open another overlay, and
                // doing it behind this one would leave the
                // menu eating the keys of the one that just
                // opened.
                if let Some(id) = app.take_menu_choice()
                    && let Some(cmd) = Command::parse(&id)
                {
                    // The SAME path as the palette and the
                    // resolver: a command chosen from a
                    // menu runs exactly as if its key had
                    // been pressed.
                    //
                    // The body is duplicated from the arm
                    // of the palette knowingly: extracting
                    // it asks for a twelve-parameter
                    // function —`events`, `terminal`,
                    // `capture`— or refactoring the run
                    // loop, and neither fits in the change
                    // the menu brings.
                    run_command(
                        app,
                        backend,
                        events,
                        help_lines,
                        lang,
                        quick_mode,
                        confirm_quit,
                        cfg,
                        &mut work.fill,
                        &mut work.decorate,
                        &mut work.probed,
                        &mut work.search,
                        cmd,
                    )
                    .await;
                    launch_pending(app, events, capture).await;
                }
            }
            _ => {}
        }
    } else if app.theme_picker.is_some() && !modal_wins(app) {
        on_theme_picker_key(app, dialog_resolver, key.modifiers, key.code).await;
    } else if app.connections_picker.is_some() && !modal_wins(app) {
        // #140: confirming returns the URL and navigating
        // is a `cd` like any other — with its return
        // ritual, so the paginated drainer and the
        // previous pane's probes do not stay alive.
        if let Some(url) = on_connections_picker_key(app, dialog_resolver, key.modifiers, key.code)
        {
            match VPath::parse(&url) {
                Ok(destination) => {
                    let outcome = cd(app, backend, events, destination).await;
                    apply_cd(
                        &app.panes,
                        &mut work.fill,
                        &mut work.decorate,
                        &mut work.probed,
                        &mut work.search,
                        outcome,
                    );
                }
                Err(_) => {
                    app.message = Some(ta(
                        "msg-connect-bad-url",
                        &[("url", &norte_encoding::mask_terminal_hazards(&url))],
                    ));
                }
            }
        }
    } else if app.layout_picker.is_some() && !modal_wins(app) {
        // Same spot in the chain and the SAME allowlist
        // as the theme picker: both are a cursor list
        // that does not mutate data.
        on_layout_picker_key(app, dialog_resolver, key.modifiers, key.code);
    } else if app.profile_picker.is_some() && !modal_wins(app) {
        // The profile one, in the same spot in the chain and with the same
        // allowlist: it is another cursor list that does not mutate data.
        // Confirming leaves the change REQUESTED and the loop does it.
        on_profile_picker_key(app, dialog_resolver, key.modifiers, key.code);
    } else if app.columns_picker.is_some() && !modal_wins(app) {
        // Columns picker (#108 7a): same spot in the
        // chain as the theme picker (overlay before the
        // modal arm, existing precedence).
        if on_columns_key(app, dialog_resolver, key.modifiers, key.code).await {
            // #117: the painted attrs set changed — the
            // values only arrive by asking for them, so it
            // re-lists through the SAME path as after a
            // mutation (ritual in `after_panes_refresh`).
            let refreshed = refresh_panes(app, backend, events).await;
            after_panes_refresh(
                app,
                refreshed,
                &mut work.fill,
                &mut work.probed,
                &mut work.search,
            );
        }
    } else if app.extensions.is_some() && !modal_wins(app) {
        on_extensions_key(
            app,
            backend,
            dialog_resolver,
            lang,
            help_lines,
            key.modifiers,
            key.code,
        )
        .await;
    } else if app.key_owner() == crate::app::KeyOwner::Tree && !modal_wins(app) {
        // #136: the tree sends the listing to the branch
        // chosen through the usual cd flow.
        let outcome = on_tree_key(
            app,
            backend,
            events,
            dialog_resolver,
            key.modifiers,
            key.code,
        )
        .await;
        apply_cd(
            &app.panes,
            &mut work.fill,
            &mut work.decorate,
            &mut work.probed,
            &mut work.search,
            outcome,
        );
    } else if app.key_owner() == crate::app::KeyOwner::Places && !modal_wins(app) {
        // Places sidebar (L3): Enter over a row sends
        // the focused LISTING to that place, through the
        // usual cd flow.
        let outcome = on_places_key(
            app,
            backend,
            events,
            dialog_resolver,
            key.modifiers,
            key.code,
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
    } else if app.key_owner() == crate::app::KeyOwner::Processes && !modal_wins(app) {
        // Process panel (#243): the keys arrive HERE,
        // not at the listing behind it. Without this
        // arm the panel got the focus border, the `▶`
        // never moved and F8 opened the delete dialog
        // over the listing's selection while the reader
        // thought they had the keyboard in the task
        // list.
        on_processes_key(app, dialog_resolver, key.modifiers, key.code);
    } else if app.key_owner() == crate::app::KeyOwner::Log && !modal_wins(app) {
        // Log panel (#323), for the same reason as the process one:
        // its keys are single letters and have to arrive here and not at
        // the listing, where `d` means something else.
        crate::logview::apply(app, dialog_resolver, key.modifiers, key.code);
    } else if app.key_owner() == crate::app::KeyOwner::DiskMap && !modal_wins(app) {
        // The disk map (phase 4), for the same reason as its neighbors: its
        // keys are arrows and one loose letter, and have to arrive here and
        // not at the listing, where `r` means something else.
        on_disk_map_key(app, dialog_resolver, key.modifiers, key.code);
    } else if app.key_owner() == crate::app::KeyOwner::Timeline && !modal_wins(app) {
        // The timeline (phase 7), for the same reason as its neighbors.
        // It is async because reaching the bottom asks for the next page and
        // because Enter opens the undo question, which needs the backend.
        on_timeline_key(app, backend, dialog_resolver, key.modifiers, key.code).await;
    } else if app.key_owner() == crate::app::KeyOwner::Terminal && !modal_wins(app) {
        // The terminal panel (#362), and this arm does NOT look like its
        // neighbors: the others translate keys into commands, and here the
        // BYTES are passed to a shell. Everything typed belongs to it —
        // arrows, `tab`, F5, `ctrl+c`— because inside a shell that is what
        // they mean.
        //
        // With ONE exception, which is the door: the loose chord that
        // opened the panel takes it out. It is compared CANONICALLY
        // (`Chord`) and not as a raw event, for the same reason as the
        // subshell: two different crossterm events —`KeyEventKind`, the
        // shift a `Char` already carries inside— are the same chord, and
        // comparing events made the exit key depend on whether the terminal
        // sends repeats.
        //
        // With no chord (`None`) the panel never gets the keyboard, so this
        // is never entered: `puede_tomar_teclas` prevents it.
        let the_chord = crate::keymap::chord_from_crossterm(key.modifiers, key.code);
        if the_chord.is_some() && the_chord == app.terminal_chord {
            // The SAME `toggle_terminal` that opened it: there is one key,
            // so the way back has to be the same code, or one day one of
            // the two learns something the other does not.
            app.toggle_terminal();
        } else if let Some(t) = app.terminal.as_mut()
            && key.kind == crossterm::event::KeyEventKind::Press
            && let Some(bytes) = crate::subshell::key_to_bytes(&key)
        {
            t.write(&bytes);
        }
    } else if app.key_owner() == crate::app::KeyOwner::Panel && !modal_wins(app) {
        // Plugin panel (phase 3), for the same reason as the two above:
        // without this arm the keys fell to the listing behind it while
        // the screen said the keyboard was here.
        on_panel_key(app, dialog_resolver, key.modifiers, key.code);
    } else if app.nav_popup.is_some() && !modal_wins(app) {
        // History/hotlist popup (spec 2026-07-18): Enter
        // over an item NAVIGATES through the normal cd
        // flow — its outcome touches the fill like any cd.
        let outcome = on_nav_popup_key(
            app,
            backend,
            events,
            dialog_resolver,
            key.modifiers,
            key.code,
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
    } else if app.search_dialog.is_some() && !modal_wins(app) {
        // Alt+F7 dialog (liveSearch T6): captures printables
        // like the other overlays; Enter with a criterion
        // launches the search (opens the virtual pane) — the
        // rest of the keys do not navigate.
        if let Some(params) = on_search_dialog_key(app, key.modifiers, key.code) {
            launch_search(app, backend, &mut work.fill, &mut work.search, params).await;
        }
    } else if app.splash.is_some() && !modal_wins(app) {
        // The splash screen (spec 2026-09-15): any key dismisses it, and
        // with `home` a number opens its row. It goes BEFORE the wizard in
        // the chain because only one of the two can be set —the splash's
        // door yields to it— so the branch reads without thinking about
        // the other.
        if let Some((cmd, arg)) = crate::splash::on_key(app, key.code) {
            app.pending_splash_row = Some((cmd, arg));
        }
    } else if app.wizard.is_some() && !modal_wins(app) {
        // The first-boot wizard (spec 2026-09-10): FIXED keys, like the
        // palette —there is no preset yet, that is exactly what it asks
        // about—. What is chosen is written on finish, through the same
        // path as the settings screen.
        if let Some(outcome) = crate::wizard::apply_key(app, key) {
            crate::wizard::finish(app, backend, outcome).await;
        }
    } else if app.sync.is_some() && !modal_wins(app) {
        // Sync panel: FIXED keys, like the diff panel's. It
        // goes BEFORE it because it is painted on top: the
        // diff panel stays alive behind it with its marks,
        // and the keyboard has to go to what is visible.
        on_sync_key(app, &mut work.sync, key.modifiers, key.code);
    } else if app.compare.is_some() && !modal_wins(app) {
        // Diff panel (`Shift+F2`): FIXED keys, like the
        // search dialog and the palette. It does not resolve
        // through the `dialog` context because there is no
        // `dialog.*` vocabulary for "switch sides" or "hide
        // the equal ones", and it is not a keymap screen of
        // its own because that would mean seven presets
        // touched by a key that does not yet have an
        // established language.
        on_compare_key(
            app,
            backend,
            events,
            &mut work.fill,
            &mut work.decorate,
            &mut work.probed,
            &mut work.search,
            &mut work.compare,
            key.modifiers,
            key.code,
        )
        .await;
    } else if app.goto.is_some() && !modal_wins(app) {
        // "Go to anywhere" (phase 6): FIXED keys, for the same reason as
        // the palette's —there is no `dialog.*` verb for "type a letter"
        // or "go to what I'm pointing at"— and `ctrl+c` keeps its global
        // exit as in every overlay.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            app.quit = true;
            return;
        }
        let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
        match key.code {
            KeyCode::Char(c) if plain => {
                if let Some(g) = &mut app.goto {
                    g.push_char(c);
                }
                crate::jobs::goto::ask_the_index(app, backend, work);
            }
            KeyCode::Backspace if plain => {
                if let Some(g) = &mut app.goto {
                    g.backspace();
                }
                crate::jobs::goto::ask_the_index(app, backend, work);
            }
            KeyCode::Esc if plain => {
                app.goto = None;
                crate::jobs::goto::forget(work);
            }
            KeyCode::Up if plain => {
                if let Some(g) = &mut app.goto {
                    g.up();
                }
            }
            KeyCode::Down if plain => {
                if let Some(g) = &mut app.goto {
                    g.down();
                }
            }
            KeyCode::Enter if plain => {
                let key = app
                    .goto
                    .as_ref()
                    .and_then(|g| g.selected().map(|r| r.key.clone()));
                // The screen closes BEFORE acting, and the request to the
                // index is abandoned: whatever comes next —a cd, a command,
                // a modal— rules the screen, and a late response no longer
                // has anywhere to land.
                app.goto = None;
                crate::jobs::goto::forget(work);
                let Some(key) = key else { return };
                match crate::goto::action(app, &key) {
                    crate::goto::Action::Ir(dir) => {
                        let outcome = crate::navigate::cd(app, backend, events, dir).await;
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
                    // A command chosen here runs EXACTLY as if its key had
                    // been pressed: the SAME `run_command` the resolver
                    // invokes, as the palette already does. Two paths for
                    // the same verb diverge the moment one of them grows a
                    // detail.
                    //
                    // Its rows are the palette's, born from `COMMANDS`, so
                    // the parse cannot fail; the guard is defensive, same as
                    // there.
                    crate::goto::Action::Command(cmd) => {
                        let Some(cmd) = Command::parse(&cmd) else {
                            debug_assert!(false, "goto outside COMMANDS");
                            return;
                        };
                        run_command(
                            app,
                            backend,
                            events,
                            help_lines,
                            lang,
                            quick_mode,
                            confirm_quit,
                            cfg,
                            &mut work.fill,
                            &mut work.decorate,
                            &mut work.probed,
                            &mut work.search,
                            cmd,
                        )
                        .await;
                        launch_pending(app, events, capture).await;
                    }
                    crate::goto::Action::Nothing(msg_id) => {
                        app.message = Some(norte_i18n::t(msg_id));
                    }
                }
            }
            _ => {}
        }
    } else if app.palette.is_some() && !modal_wins(app) {
        // Command palette (H1 T4): free-filter editor, like
        // the search dialog above — its keys are FIXED, they
        // do not resolve through the `dialog` context (H1
        // plan decision 8: there is no `dialog.*` vocabulary
        // for "type a character" or "move the selection").
        // ctrl+c keeps its global meaning (quit), like EVERY
        // overlay.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            app.quit = true;
            return;
        }
        let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
        match key.code {
            KeyCode::Char(c) if plain => {
                if let Some(p) = &mut app.palette {
                    p.push_char(c);
                }
            }
            KeyCode::Backspace if plain => {
                if let Some(p) = &mut app.palette {
                    p.backspace();
                }
            }
            KeyCode::Esc if plain => app.palette = None,
            // H3c: the bridge to the page documenting the
            // highlighted row. It goes HERE, explicit next to
            // `ctrl+c`/`ctrl+p`, because the palette's keys
            // are FIXED (decision 8, above): there is no
            // `dialog.*` verb for "explain this row to me",
            // so it cannot be resolved through the keymap
            // either.
            KeyCode::F(1) if plain => {
                palette_help(app, lang, help_lines);
            }
            KeyCode::Up if plain => {
                if let Some(p) = &mut app.palette {
                    p.up();
                }
            }
            KeyCode::Down if plain => {
                if let Some(p) = &mut app.palette {
                    p.down();
                }
            }
            KeyCode::PageUp if plain => {
                if let Some(p) = &mut app.palette {
                    p.page_up(PAGE);
                }
            }
            KeyCode::PageDown if plain => {
                if let Some(p) = &mut app.palette {
                    p.page_down(PAGE);
                }
            }
            KeyCode::Enter if plain => {
                let cmd = app.palette.as_ref().and_then(Palette::selected);
                app.palette = None;
                if let Some(cmd) = cmd {
                    // What is launched goes to the top next time (spec
                    // 2026-09-10); the session saves it with the next push.
                    norte_frontend::session::note_palette_recent(&mut app.palette_recent, &cmd);
                    // (P1) Enter over a PLUGIN row: the `key`
                    // is `plugin:{id}:{command}`
                    // (`palette::plugin_rows`, never painted)
                    // — it does not live in `COMMANDS`, so it
                    // is routed HERE, before the typed
                    // vocabulary (#112). The plugin's result
                    // is UNTRUSTED text: `detail_for_bar`
                    // (masked + capped, #73 pattern).
                    if let Some((id, command)) = parse_plugin_key(&cmd) {
                        let (id, command) = (id.to_owned(), command.to_owned());
                        run_plugin_command(app, backend, &id, &command).await;
                        return;
                    }
                    // A RENAMER row (C3, ADR 0095): asks the plugin for the
                    // plan and leaves it in the SAME run as the AI one's.
                    if let Some((id, renamer)) = norte_frontend::palette::parse_renamer_key(&cmd) {
                        let (id, renamer) = (id.to_owned(), renamer.to_owned());
                        crate::jobs::spawn_renamer_plan(app, backend, work, &id, &renamer);
                        return;
                    }
                    // An ORGANIZER row (phase 8): the same split, a
                    // different method — and the plan lands in the same
                    // reviewable tree as the model's.
                    if let Some((id, org)) = norte_frontend::palette::parse_organizer_key(&cmd) {
                        let (id, org) = (id.to_owned(), org.to_owned());
                        crate::jobs::spawn_organize_plan(
                            app,
                            backend,
                            work,
                            Some((id.as_str(), org.as_str())),
                        );
                        return;
                    }
                    // The SAME dispatch function the keymap
                    // resolver invokes (#dispatch): a command
                    // chosen in the palette runs EXACTLY as if
                    // its key had been pressed — including
                    // opening another overlay (e.g. `app.help`).
                    // The palette's rows are born from
                    // `COMMANDS`, so the parse cannot fail; the
                    // guard is defensive (#112).
                    let Some(cmd) = Command::parse(&cmd) else {
                        debug_assert!(false, "palette outside COMMANDS");
                        return;
                    };
                    // Parity with the resolver's spot (#118
                    // review): a cd chosen in the palette
                    // (nav.parent…) can also turn off virtual
                    // mode — harvested by the run (rule 3); and
                    // a `pane.open` from the palette leaves its
                    // external command resolved — launch it NOW,
                    // not on the next key.
                    run_command(
                        app,
                        backend,
                        events,
                        help_lines,
                        lang,
                        quick_mode,
                        confirm_quit,
                        cfg,
                        &mut work.fill,
                        &mut work.decorate,
                        &mut work.probed,
                        &mut work.search,
                        cmd,
                    )
                    .await;
                    launch_pending(app, events, capture).await;
                }
            }
            _ => {}
        }
    } else if app.shortcuts.is_some() && !modal_wins(app) {
        // K3c: the shortcuts editor is painted ON TOP OF the
        // settings overlay (which stays open behind it), so it
        // also keeps the keys BEFORE it. In capture mode ALL
        // of them are its — that is what capturing means.
        on_shortcuts_key(
            app,
            cfg,
            cli_preset,
            &Maps {
                browse: resolver.effective(),
                viewer: viewer_resolver.effective(),
                dialog: dialog_resolver.effective(),
            },
            key.modifiers,
            key.code,
        )
        .await;
    } else if app.settings.is_some() && !modal_wins(app) {
        // Settings overlay (S3): same criterion as the
        // palette above (H1 plan decision 8) — its keys
        // are fixed, hardcoded in `on_settings_key`.
        on_settings_key(
            app,
            &Maps {
                browse: resolver.effective(),
                viewer: viewer_resolver.effective(),
                dialog: dialog_resolver.effective(),
            },
            key.modifiers,
            key.code,
        )
        .await;
    } else if help_owns_keys(app) {
        // H3b: help overlay. The key path lives in
        // `on_help_key` (testable, like `on_columns_key`);
        // what is left here is only what the run loop needs,
        // which is DISPATCHING the activated row. The overlay
        // has already closed: the command acts on the panes
        // underneath and help would cover up any confirmation
        // it opens.
        match on_help_key(app, dialog_resolver, key.modifiers, key.code) {
            // (H3e) A PLUGIN row goes out through the SAME
            // dispatch as the palette's Enter, not through a
            // parallel path: `plugin.run_command` is where the
            // authorization comes from and help does not route
            // around it. It does not touch the panes, so it
            // does not drag in the cd bookkeeping from the arm
            // below.
            Some(HelpDispatch::Plugin(id, command)) => {
                run_plugin_command(app, backend, &id, &command).await;
            }
            None => {}
            Some(HelpDispatch::Command(cmd)) => {
                // The SAME dispatch and the SAME bookkeeping
                // afterward as the palette's Enter: a help row
                // is `nav.parent` just as much as a palette
                // one is, so the cd path (paginated fill,
                // decoration, harvesting the live search,
                // pending external opener) has to be the same.
                run_command(
                    app,
                    backend,
                    events,
                    help_lines,
                    lang,
                    quick_mode,
                    confirm_quit,
                    cfg,
                    &mut work.fill,
                    &mut work.decorate,
                    &mut work.probed,
                    &mut work.search,
                    cmd,
                )
                .await;
                launch_pending(app, events, capture).await;
            }
        }
    } else if app.modal.is_some() {
        // MINOR-4 (H1 close): a modal arriving while the
        // palette was open closes it HERE — stale, and this
        // SAME key answers the modal instead of disappearing
        // inside the palette's filter. The settings overlay
        // (S3) is the SAME case: an async modal (e.g. a
        // policy approval) wins. The rest of the overlays
        // (theme picker, columns picker, extensions, nav
        // popup, search dialog) also yield the key
        // (`modal_wins`) but do NOT close: their rows do not
        // go stale like the palette's/settings', and the
        // user gets them back intact on answering. Help is
        // the mixed case (H3c) and `close_stale_overlays`
        // decides it.
        close_stale_overlays(app);
        // Lua's TOFU is resolved HERE (it needs the host,
        // which lives in this loop): it does not navigate or
        // touch `work.fill`.
        if matches!(app.modal, Some(Modal::TrustLuaInit { .. })) {
            resolve_lua_trust(app, lua_host, key.code).await;
            return;
        }
        // ctrl+c keeps its global meaning (quit), like the
        // other overlays (H1 T2) — BEFORE resolving against
        // the hardcoded `dialog` context.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            app.quit = true;
            return;
        }
        // #325: the password field types and deletes HERE, but
        // Enter and Esc still go through the `dialog` context
        // (`ALLOW_ASK_SECRET` allowlist). It deliberately does not
        // enter the `PromptKind` machinery: that lends the field
        // out as `&mut String` —and offers a `text()` to paint
        // it—, which is exactly what a secret cannot give (rule
        // 10).
        if let Some(Modal::AskSecret { input, .. }) = app.modal.as_mut() {
            let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
            match key.code {
                KeyCode::Char(c) if plain => {
                    input.push(c);
                    return;
                }
                KeyCode::Backspace if plain => {
                    input.pop();
                    return;
                }
                _ => {}
            }
        }
        // The ten FREE-TEXT prompts share a keyboard:
        // typing, deleting and Esc are the same operation
        // over the open prompt, and they consume the key
        // BEFORE the `dialog` context —none has a
        // `dialog_action` ALLOWLIST, and `ctrl+c` was
        // already resolved above—. The only thing each one
        // has of its own is Enter: that is where its submit
        // lives, which is async and that is why it is here
        // and not in `App`.
        let prompt = app.modal.as_ref().and_then(Modal::prompt_kind);
        if let Some(kind) = prompt {
            let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
            match key.code {
                KeyCode::Char(c) if plain => app.prompt_push(kind, c),
                KeyCode::Backspace if plain => app.prompt_pop(kind),
                KeyCode::Esc if plain => app.cancel_prompt(kind),
                KeyCode::Enter if plain => match kind {
                    PromptKind::MarkPattern => {
                        if let Ok(n) = app.mark_pattern_confirm() {
                            app.message =
                                Some(ta("msg-marked-by-pattern", &[("n", &n.to_string())]));
                        }
                    }
                    PromptKind::Mkdir => {
                        if let Some(target) = app.mkdir_confirm() {
                            match backend.mkdir(&target).await {
                                Ok(task) => {
                                    app.board.push(&task, None);
                                    app.mkdir_submitted();
                                }
                                // MINOR-1: the name survives
                                // the submit failure.
                                Err(e) => app.mkdir_set_error(error_message(&e)),
                            }
                        }
                    }
                    // #290: creating the file is a daemon task like any
                    // other mutation; the editor opens on the tick that
                    // sees it finish, not here.
                    PromptKind::EditNew => {
                        if let Some(target) = app.edit_new_confirm() {
                            match backend.create_file(&target).await {
                                Ok(task) => {
                                    app.pending_edit_open = Some((task.id(), target));
                                    app.board.push(&task, None);
                                    app.edit_new_submitted();
                                }
                                Err(e) => app.edit_new_set_error(error_message(&e)),
                            }
                        }
                    }
                    // #306: save the workspace as a profile. Writes THREE
                    // files with a lock and tmp+rename, so it goes through
                    // `spawn_blocking` (rule 2) and the modal only closes
                    // once disk has answered yes.
                    PromptKind::ProfileSaveAs => {
                        crate::screens::profile_save_as(app).await;
                    }
                    PromptKind::Pack => {
                        if let Some(params) = app.pack_confirm() {
                            match backend.pack(params).await {
                                Ok(task) => {
                                    app.board.push(&task, None);
                                    app.pack_submitted();
                                }
                                Err(e) => {
                                    app.pack_set_error(error_message(&e));
                                }
                            }
                        }
                    }
                    PromptKind::Split => {
                        if let Some(params) = app.split_confirm() {
                            match backend.split_file(params).await {
                                Ok(task) => {
                                    app.board.push(&task, None);
                                    app.split_submitted();
                                }
                                Err(e) => {
                                    app.split_set_error(error_message(&e));
                                }
                            }
                        }
                    }
                    // #314: permissions. Same mold as splitting: the params
                    // are resolved, sent, and a submit failure KEEPS what
                    // was typed with its diagnosis underneath.
                    PromptKind::Chmod => {
                        if let Some(params) = app.chmod_confirm() {
                            let n = params.paths.len();
                            match backend.set_mode(params).await {
                                Ok(task) => {
                                    app.board.push(&task, None);
                                    app.chmod_submitted();
                                    app.message =
                                        Some(ta("msg-chmod-started", &[("n", &n.to_string())]));
                                }
                                Err(e) => app.chmod_set_error(error_message(&e)),
                            }
                        }
                    }
                    PromptKind::TransferDest => {
                        let _ = app.transfer_dest_confirm();
                    }
                    PromptKind::CommandLine => {
                        if let Some(cmd) = app.command_line_confirm() {
                            submit_command_line(app, &cmd);
                        }
                    }
                    PromptKind::AiRename => {
                        if let Some(instruction) = app.ai_rename_confirm() {
                            let dir = app.focused().dir().clone();
                            let b = backend.clone();
                            let d = dir.clone();
                            // What is MARKED, if there are marks (#121):
                            // asking for a plan over five files sent the
                            // provider the thousand files in the directory,
                            // which is more than the human pointed at.
                            // `marked_entries` and not `marked_paths`: the
                            // latter falls back to the cursor when there are
                            // no marks, and "nothing marked" means the whole
                            // directory.
                            let mark_count = app.focused().marked_entries().len();
                            let marked: Vec<String> = app
                                .focused()
                                .marked_entries()
                                .iter()
                                .filter_map(|e| e.path.file_name())
                                .filter_map(|s| String::from_utf8(s.as_bytes().to_vec()).ok())
                                .collect();
                            // If EVERYTHING marked is names that are not
                            // text, the list ends up empty — and empty means
                            // "the whole directory", i.e. exactly the
                            // opposite of what was asked for. It refuses and
                            // says so: silently widening the scope is what
                            // this field exists to not do.
                            if mark_count > 0 && marked.is_empty() {
                                app.message = Some(t("msg-ai-rename-marks-not-text"));
                                app.ai_rename_submitted();
                                return;
                            }
                            let handle = tokio::spawn(async move {
                                b.ai_rename_plan(&d, &instruction, &marked).await
                            });
                            // Relaunching with a live run ABORTS it
                            // (dropping the handle only detaches it):
                            // at most one request in flight.
                            let names: Vec<Vec<u8>> = app
                                .focused()
                                .entries()
                                .iter()
                                .filter_map(|e| e.path.file_name().map(|s| s.as_bytes().to_vec()))
                                .collect();
                            let run = AiRenameRun { handle, dir, names };
                            if let Some(old) = work.ai_rename.replace(run) {
                                old.handle.abort();
                            }
                            // Invariant: launching CLEARS the stash — a
                            // plan held from a PREVIOUS request must never
                            // be opened as if it were this one's.
                            work.pending_ai_plan = None;
                            app.message = Some(t("msg-ai-rename-running"));
                            app.ai_rename_submitted();
                        }
                    }
                    // #310: the already-validated template produces the
                    // pairs HERE —without going out to ask anyone— and from
                    // there the path is the SAME as the AI plan's:
                    // `fs.rename_batch_plan` is requested, the modal opens
                    // in `Pending` and the harvest fills it.
                    PromptKind::RenameBatch => {
                        if let Some(pattern) = app.rename_batch_confirm() {
                            let names = app.rename_batch_names();
                            let dir = app.focused().dir().clone();
                            let entries: Vec<norte_proto::methods::AiRenameEntry> =
                                norte_frontend::rename_pattern::plan(&pattern, &names, 1)
                                    .into_iter()
                                    .map(|(from, to)| norte_proto::methods::AiRenameEntry {
                                        from,
                                        to,
                                    })
                                    .collect();
                            app.rename_batch_submitted();
                            if entries.is_empty() {
                                // A template that leaves everything the same
                                // is not an error: there is nothing to
                                // rename and it says so.
                                app.message = Some(t("msg-rename-batch-no-changes"));
                            } else if let Some(pairs) = norte_frontend::rename_pairs(&entries) {
                                let b = backend.clone();
                                let d = dir.clone();
                                let handle =
                                    tokio::spawn(
                                        async move { b.rename_batch_plan(&d, &pairs).await },
                                    );
                                if let Some(old) =
                                    work.rename_batch.replace(RenameBatchRun { handle })
                                {
                                    old.handle.abort();
                                }
                                app.modal = Some(Modal::AiRenamePlan {
                                    dir,
                                    entries,
                                    offset: 0,
                                    // The first window has already been seen
                                    // as soon as the modal opens.
                                    seen: norte_frontend::AI_RENAME_PAIR_LIMIT,
                                    plan: norte_frontend::BatchPlan::Pending,
                                });
                            } else {
                                // A pair that is not a `Segment` is a bug of
                                // OURS —the template built them— and not a
                                // hostile response: it is stated and no plan
                                // is requested.
                                app.message = Some(t("msg-rename-pattern-bad-result"));
                            }
                        }
                    }
                    PromptKind::Semantic => {
                        if let Some(query) = app.semantic_confirm() {
                            let b = backend.clone();
                            let handle = tokio::spawn(async move {
                                b.index_search_semantic(None, &query, SEMANTIC_K).await
                            });
                            // Relaunching with a live run ABORTS it
                            // (dropping the handle only detaches it):
                            // at most one query in flight.
                            if let Some(old) = work.semantic.replace(SemanticRun { handle }) {
                                old.handle.abort();
                            }
                            // Invariant: launching CLEARS the stash — hits
                            // held from a PREVIOUS query must never be
                            // opened as if they were this one's.
                            work.pending_semantic = None;
                            app.message = Some(t("msg-semantic-running"));
                            app.semantic_submitted();
                        }
                    }
                    PromptKind::TransferName => {
                        if let Some((kind, from, dest)) = app.transfer_name_confirm() {
                            // Closes ONLY if it was queued
                            // (MINOR-1 discipline from #104): a
                            // failed submit keeps the name; the
                            // detail stays in the bar.
                            if submit_transfer(
                                app,
                                backend,
                                kind,
                                from,
                                dest,
                                TransferOptions::default(),
                            )
                            .await
                            {
                                app.transfer_name_submitted();
                            } else {
                                app.transfer_name_set_error(t("msg-transfer-name-failed"));
                            }
                        }
                    }
                },
                _ => {}
            }
            return;
        }
        // The TOFU modal (#45) can NAVIGATE on trusting: its Cd
        // is applied the same way as a command's.
        let outcome = on_dialog_key(
            app,
            backend,
            events,
            dialog_resolver,
            key.modifiers,
            key.code,
            lang,
            help_lines,
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
    } else {
        // Esc with a Lua command in flight (BROWSE: no modal
        // or overlay, and NOT in the viewer): requests
        // cancellation (rule 3) and CONSUMES the key — it does
        // not fall through to the resolver.
        if app.viewer.is_none()
            && key.modifiers.is_empty()
            && key.code == KeyCode::Esc
            && let Some((_, token)) = &work.lua
        {
            token.cancel();
            // K3a: the key is CONSUMED here, so the resolver
            // never sees it — and a half-typed sequence (with
            // its which-key panel on top) would stay armed
            // while the reader thinks they cancelled.
            app.abandon_pending(resolver);
            return;
        }
        // Esc with ai.rename_plan in flight (BROWSE, M4-IA):
        // cancel (rule 3) and CONSUME the key. Aborting drops
        // the backend's future in the runtime → rpc.cancel
        // (remote) / stream drop (embedded).
        if app.viewer.is_none()
            && key.modifiers.is_empty()
            && key.code == KeyCode::Esc
            && let Some(run) = work.ai_rename.take()
        {
            run.handle.abort();
            app.message = None;
            // K3a: same as above — Esc consumed here also
            // cancels the in-flight sequence, never just its
            // painting.
            app.abandon_pending(resolver);
            return;
        }
        // Esc with a semantic search in flight (BROWSE,
        // M4-IA-2): same cancellation contract (rule 3).
        if app.viewer.is_none()
            && key.modifiers.is_empty()
            && key.code == KeyCode::Esc
            && let Some(run) = work.semantic.take()
        {
            run.handle.abort();
            app.message = None;
            app.abandon_pending(resolver);
            return;
        }
        // Search virtual pane (liveSearch T6): with a
        // work.search on the focused pane (and no live quick
        // search), Esc and Enter have their own semantics
        // BEFORE the resolver. The REST of the keys (cursor,
        // F5/F8/F3…) fall through to the resolver and operate
        // on the hit under the cursor.
        if app.viewer.is_none()
            && key.modifiers.is_empty()
            && app.focused().quick().is_none()
            && app.focused().virtual_search
            && work.search.as_ref().is_some_and(|s| s.pane == app.focus())
        {
            match key.code {
                KeyCode::Esc => {
                    // Task alive → cancels (hits kept, will
                    // move to Cancelled when the channel
                    // closes). Already finished → leaves
                    // virtual mode, restoring the previous
                    // dir.
                    on_search_escape(
                        app,
                        backend,
                        events,
                        &mut work.fill,
                        &mut work.decorate,
                        &mut work.probed,
                        &mut work.search,
                    )
                    .await;
                    return;
                }
                KeyCode::Enter => {
                    // Enter over a hit: cd to the hit's PARENT
                    // and cursor over it (leaves virtual mode).
                    on_search_enter(
                        app,
                        backend,
                        events,
                        &mut work.fill,
                        &mut work.decorate,
                        &mut work.probed,
                        &mut work.search,
                    )
                    .await;
                    return;
                }
                _ => {}
            }
        }
        // Quick search ACTIVE on the focused pane (BROWSE):
        // its keys are eaten BEFORE the resolver — a char
        // (including another `/`) feeds the query and never
        // re-enters the keymap (no recursion). The REST of the
        // keys (F5, F8, F3, Tab in Filter…) are NOT consumed:
        // they fall through to the resolver and operate on the
        // already-filtered `selected()` — free feed-to-listbox.
        // Conscious decision: the navigation keys NOT
        // intercepted (PageUp/PageDown/Home/End) also fall
        // through to the resolver and move the REAL CURSOR,
        // which is invisible while the filter is active; on
        // cancel (Esc) it reappears where it was left.
        // Connecting them to the filter's selection does not
        // make up for the extra state.
        if app.viewer.is_none() && app.focused().quick().is_some() {
            let jump = app
                .focused()
                .quick()
                .is_some_and(|q| q.mode() == nav::Mode::Jump);
            // SHIFT passes through (an uppercase letter
            // arrives as Char('A')+SHIFT and the char already
            // comes as-is); ctrl/alt fall through to the
            // resolver (ctrl+c still exits).
            let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
            match key.code {
                KeyCode::Char(c) if plain => {
                    app.focused_mut().quick_char(c);
                    return;
                }
                KeyCode::Backspace if plain => {
                    app.focused_mut().quick_backspace();
                    return;
                }
                KeyCode::Up if plain => {
                    app.focused_mut().quick_up();
                    return;
                }
                KeyCode::Down if plain => {
                    app.focused_mut().quick_down();
                    return;
                }
                KeyCode::Tab if plain && jump => {
                    app.focused_mut().quick_next();
                    return;
                }
                KeyCode::Esc if plain => {
                    app.focused_mut().quick_cancel();
                    return;
                }
                KeyCode::Enter if plain => {
                    // Confirms (real cursor = selected) and
                    // REUSES nav.enter's path: a dir (or
                    // container) is entered, a file is left
                    // alone. `false` = the filter had no
                    // matches: it just closes — never dispatch
                    // over an entry the user did not see
                    // (review MAJOR T4).
                    if app.focused_mut().quick_confirm() {
                        // Parity with the resolver's spot
                        // (#118 review): the quick search's
                        // Enter IS a nav.enter — entering a hit
                        // turns off the pane's virtual mode;
                        // without harvesting, the search Task
                        // stayed alive (rule 3).
                        run_command(
                            app,
                            backend,
                            events,
                            help_lines,
                            lang,
                            quick_mode,
                            confirm_quit,
                            cfg,
                            &mut work.fill,
                            &mut work.decorate,
                            &mut work.probed,
                            &mut work.search,
                            Command::NavEnter,
                        )
                        .await;
                    }
                    return;
                }
                _ => {}
            }
        }
        // `--pick` (S2, design §B): Enter/Ctrl+Enter accept
        // the selection HERE, where the key turns into a
        // command — never in a preset, which must not have
        // to know `--pick` exists. Browse only (the viewer
        // has nothing to pick); quick search and the
        // live-search pane already resolved their own Enter
        // above and `continue`d past this point, so reaching
        // here means neither is active.
        if app.pick && app.viewer.is_none() {
            let ctrl_enter = key.code == KeyCode::Enter && key.modifiers == KeyModifiers::CONTROL;
            // Plain Enter keeps navigating whenever there is
            // somewhere to go (`nav_enter_target`) — taking
            // that away would make the picker unusable for
            // reaching anything below the start directory.
            let plain_enter = key.code == KeyCode::Enter
                && key.modifiers.is_empty()
                && nav_enter_target(app).is_none();
            if ctrl_enter || plain_enter {
                app.abandon_pending(resolver);
                let _ = dispatch(
                    app,
                    backend,
                    events,
                    help_lines,
                    lang,
                    quick_mode,
                    confirm_quit,
                    cfg,
                    Command::AppPickAccept,
                )
                .await;
                return;
            }
        }
        // Active screen: the viewer has its own context.
        let active = if app.viewer.is_some() || app.key_owner() == crate::app::KeyOwner::Preview {
            &mut *viewer_resolver
        } else {
            &mut *resolver
        };
        // Keys the keymap does not model (Media, BackTab,
        // CapsLock…) do not reach the resolver as a chord, but
        // the treatment IS the same as a `Resolution::Reset`:
        // `active.reset()` breaks any sequence pending IN THE
        // RESOLVER (not just the screen's `app.pending`) —
        // before, `from_event` always pushed a chord (however
        // exotic) and the resulting `Miss` cleared the internal
        // pending; `chord_from_crossterm` returns `None`
        // instead, so the reset has to be requested explicitly,
        // never leaving a half-typed sequence alive.
        if let Some(chord) = chord_from_crossterm(key.modifiers, key.code) {
            // Read BEFORE the push: a miss that breaks a half-typed sequence
            // is not a letter typed onto the listing.
            let idle = active.pending().is_empty() && active.count().is_none();
            match active.push(chord) {
                Resolution::Run {
                    command: cmd,
                    count,
                } => {
                    // K3a: ALSO closes the which-key panel, and
                    // before `keyboard_owner(app)` — the
                    // counter's fingerprint is taken with the
                    // panel already closed, so the close does
                    // not count as "the dispatch moved the
                    // keyboard" and does not split a `5j`.
                    app.clear_pending();
                    // K2a: a counter over a command that does
                    // not accept it is NOT swallowed — it runs
                    // once and says so. It is placed BEFORE the
                    // dispatch on purpose: if the command has
                    // something to say, its message is the one
                    // that rules.
                    if let Count::Ignored(n) = count {
                        app.message = Some(count_ignored_message(&cmd, n));
                    }
                    // `lua:<name>` (M4): to the Lua dispatcher —
                    // never to `dispatch` (it is not a fixed
                    // command).
                    // A `lua:` is not in the catalogue, so its
                    // counter is always `Ignored`: it runs ONCE,
                    // no loop.
                    if let Some(name) = cmd.strip_prefix("lua:") {
                        run_lua_command(
                            app,
                            lua_host,
                            backend,
                            name,
                            &mut work.lua,
                            &mut work.lua_queue,
                        );
                        return;
                    }
                    // #112: the keymap was validated against
                    // COMMANDS on load — the parse cannot fail;
                    // defensive guard.
                    let Some(cmd) = Command::parse(&cmd) else {
                        debug_assert!(false, "keymap outside COMMANDS");
                        return;
                    };
                    // The counter repeats the DISPATCH: no
                    // command signature changes and none can
                    // forget to honor it. The whole body
                    // (outcome, cd, harvest, opener) goes
                    // INSIDE — a `dispatch` without its outcome
                    // leaves Tasks alive and panes unrefreshed.
                    // No `continue` from the outer loop lives
                    // in here: the two this arm had (the Lua
                    // branch and the parse guard) stay ABOVE,
                    // before the loop, so the counter cannot
                    // skip past them.
                    let owner_before = keyboard_owner(app);
                    for _ in 0..count.times() {
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
                        // Read BEFORE `apply_cd` consumes the
                        // outcome.
                        let stalled = nav_stalled(cmd, &outcome);
                        settle_cd(
                            app,
                            backend,
                            &mut work.fill,
                            &mut work.decorate,
                            &mut work.probed,
                            &mut work.search,
                            outcome,
                        );
                        // A cd (nav.parent…) turned off the
                        // search pane's virtual mode: releases
                        // the run and cancels.
                        reap_search_run(app, &mut work.search);
                        // #28: `pane.open` left an external
                        // command resolved — the run loop (which
                        // owns the terminal) probes the binary
                        // and launches it.
                        launch_pending(app, events, capture).await;
                        // Stop dead if the app is quitting:
                        // `9999` followed by an exit key must
                        // not queue up 9998 more exits. The
                        // outer loop checks `app.quit` after the
                        // draw, so without this break the rest
                        // of the rounds would run with the app
                        // dead. Same if the dispatch moved the
                        // keyboard to another surface (modal,
                        // viewer, overlay): whatever is left of
                        // the counter would fire commands BEHIND
                        // it (`keyboard_owner` covers all of
                        // them, not just the modal). And the
                        // same if a trail step did not land: it
                        // rewinds, so the next round would
                        // repeat the SAME remote listing.
                        if app.quit || stalled || keyboard_owner(app) != owner_before {
                            break;
                        }
                    }
                }
                // K2a: a half-typed sequence and a half-typed
                // counter are painted the SAME way and at the
                // same time — `pending_display` composes the
                // two (in `12gg` they coexist). A counter that
                // is not visible is a counter that cannot be
                // cancelled.
                //
                // K3a: and the same state opens (or does not)
                // the which-key panel. Both arms call ONE single
                // function because the bar and the panel
                // describe the SAME resolver: it is
                // `show_pending` that knows a lone counter has
                // no panel (its pending sequence is empty), not
                // this `match`. With no timer of any kind: the
                // panel appears with the key that leaves the
                // prefix pending (ADR 0006).
                Resolution::Pending(_) | Resolution::Counting(_) => {
                    app.show_pending(active, lang);
                }
                // K1 T4: the key IS bound and this build cannot
                // run what it is bound to. It used to dispatch a
                // name with no arm; now the status bar says why.
                Resolution::Unavailable { command, why } => {
                    app.clear_pending();
                    app.message = Some(unavailable_message(&command, why));
                }
                Resolution::Reset => {
                    app.clear_pending();
                    app.type_to_search(active.effective(), idle, chord);
                }
            }
        } else {
            active.reset();
            app.clear_pending();
        }
    }
}
