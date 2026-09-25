//! The TUI binary (phases 3-4 M1): an async event loop over the EMBEDDED
//! core or against the DAEMON (phase 3 M2, via `[daemon] mode` or
//! `--daemon`), with the keymap engine (ADR 0006). Rule 7: only the
//! transport changes.
//! `norte_tui::tty::init/restore` manage raw mode + the alternate screen
//! with a panic hook included: the user's terminal is NEVER left broken.
//! They paint over the CONTROL terminal (`tty.rs`), not over stdout: from
//! `--pick` stdout carries data, not escape sequences.
#![forbid(unsafe_code)]

use anyhow::{Context, Result};
use norte_core::backend::Backend;
use norte_i18n::{t, ta};
use norte_proto::VPath;
use norte_tui::app::App;
use norte_tui::config::{self, WatchMode};
use norte_tui::event_loop::run;
use norte_tui::help::TuiChords;
use norte_tui::hints::DialogHints;
use norte_tui::keymap::Resolver;
use norte_tui::listing::initial_pane;
use norte_tui::mouse;
use norte_tui::navigate::cache_capabilities;
use norte_tui::screens::{apply_theme, drain_places_drives};
use norte_tui::session_push::restore_session;
use norte_tui::shortcuts_editor::build_keymaps;
use norte_tui::tty;
use std::sync::Arc;

#[tokio::main]
#[expect(
    clippy::too_many_lines,
    reason = "binary wiring, not API — same criterion as `run`/`dispatch`"
)]
async fn main() -> Result<()> {
    // Args: positional DIR + `--preset`/`--daemon`/`--socket`. `--help` and
    // `--version` exit BEFORE touching the terminal (they used to be ignored
    // as an unknown flag and the binary died unable to open the TTY).
    let parsed = norte_frontend::cli::parse(std::env::args_os().skip(1), BOOL_FLAGS, VALUE_FLAGS);
    let Some(args) = args_or_exit(parsed)? else {
        return Ok(()); // `--help`/`--version`: already printed.
    };
    // `--setup` (spec 2026-09-10): reopen the first-run wizard. Read
    // BEFORE `args` gets dismantled into fields.
    let cli_setup = args.has("--setup");
    // `--no-splash` (spec 2026-09-15): no splash screen on THIS startup,
    // whatever `[ui] splash` says. The same thing `NORTE_NO_SPLASH` does for
    // pilots and the tests that open a real `ntc`.
    let cli_no_splash = args.has("--no-splash");
    // `--attach` (phase 9): this startup is the other end of a HANDOVER
    // (`app.handoff`), so besides the screen it also claims the MARKED
    // items the frontend that left kept in the session. Without the flag, a
    // body carrying marks — because a handover was left half-done — does not
    // get them back: an ordinary startup is not a handover, and resurrecting
    // yesterday's selection is putting an `F8` over whatever was marked
    // back then.
    let cli_attach = args.has("--attach");
    // `--lang`: this run's language, above `NORTE_LANG` and `[ui] lang`.
    // Checked here, before the terminal is taken, so a typo is a readable
    // error and not a quiet English.
    let cli_lang = match args.text("--lang") {
        Some(v) => Some(
            norte_i18n::Lang::from_flag(&v)
                .with_context(|| format!("`--lang {v}`: the languages are `es` and `en`"))?,
        ),
        None => None,
    };
    let (cli_preset, cli_layout, cli_profile, cli_daemon, cli_socket, cli_pick, cli_cd_file) = (
        args.text("--preset"),
        // `--layout` is NOT text by contract: it ends up as a file name,
        // and via `to_string_lossy` two different invalid bytes opened the
        // same `\u{FFFD}.toml` (#246 M1).
        args.os_text("--layout").map(std::ffi::OsString::from),
        // `--profile` is not text by contract either: it ends up as
        // `profiles/<name>/`, which is a DIRECTORY. Same reason and same
        // #246 as `--layout`.
        args.os_text("--profile").map(std::ffi::OsString::from),
        args.has("--daemon"),
        args.path("--socket"),
        args.has("--pick"),
        args.path("--cd-file"),
    );
    // An EXPLICIT `--profile` is known before connecting to anything, so it
    // enters on the FIRST load and not through the hot reload: that way it
    // applies even to `[ui] lang`, the one thing a live change cannot
    // (`norte_i18n::force` runs once). The STICKY profile cannot do this —
    // it lives in the session, and the session is the daemon's, which is
    // reached with the config we are loading now — and that is why it
    // arrives through the other path.
    //
    // And if the one you named cannot be used, this ABORTS (ADR 0079, D7):
    // you asked for that profile, and starting as something else would be
    // answering a different question. `load_async` already names the
    // culprit file.
    let layers = match &cli_profile {
        Some(name) => {
            // The name has to be in the LISTING, byte for byte. Looking only
            // at whether the resolver produced a layer is NOT enough:
            // `splice` adds it as soon as the name is legal and there is a
            // user dir, whether or not the directory exists — and then
            // `load` treats it as an absent layer, which is not an error,
            // and `--profile ghost` started up as if nothing happened.
            // Which is exactly what D7 declares fatal for a profile the
            // reader named.
            let dir = norte_config::profiles_dir_from(&|k| std::env::var_os(k))
                .context("no config directory to hang a profile off")?;
            let exists = norte_config::list_profiles(&dir)
                .unwrap_or_default()
                .iter()
                .any(|n| n == name);
            anyhow::ensure!(
                exists,
                "no profile named \"{}\" in {}",
                name.to_string_lossy(),
                dir.display()
            );
            config::standard_layers_with_profile(Some(name))
        }
        None => config::standard_layers(),
    };
    let cfg = config::load_async(layers.clone())
        .await
        .context("invalid config")?;
    // Language: --lang > NORTE_LANG > the config's [ui] lang > environment.
    let lang = norte_i18n::Lang::resolve(
        cli_lang,
        std::env::var("NORTE_LANG").ok().as_deref(),
        cfg.common.ui_lang.as_deref(),
        norte_i18n::Lang::from_env(),
    );
    let _ = norte_i18n::force(lang);
    // Keys are NAMED in the same language as the rest of the screen:
    // "[Backspace] back" stays consistent instead of a mixed-language chord
    // in a Spanish interface.
    let _ = norte_frontend::keymap::set_chord_lang(lang);
    // Roadmap item 9: the log goes to the FILE and only the file. Until now
    // this binary installed no subscriber at all and said so in a comment
    // further below: an `fmt` to stderr fights the alternate screen, so
    // every `tracing::warn!` from the TUI was silently dropped.
    //
    // Goes AFTER loading the config because `[log] dir` comes from it,
    // which means a `--help`/`--version` — which exit earlier — leave no
    // trace. Correct: they do nothing worth a log.
    //
    // AND ALSO to an in-memory ring, which is what `panel.log` paints
    // (#323). The file is for investigating afterward; the ring, for
    // seeing what is happening without leaving the TUI — which is where the
    // gap was noticed: a connection failing in 240 ms leaves a "permission
    // denied" that says nothing while the exact reason gets written to a
    // file on another terminal.
    let log_ring = norte_core::logging::init_to_file_with_ring(
        norte_core::logging::LogConfig {
            dir: cfg.common.log.dir.as_deref(),
            retain: cfg.common.log.retain,
            // The shared file: the CLI, the daemon and the terminal do not
            // coexist live over the same state the way the daemon and the
            // window do.
            prefix: None,
            format: cfg.common.log.format,
        },
        norte_config::logring::RING_DEFAULT,
    );
    let (browse_eff, viewer_eff, dialog_eff) = build_keymaps(&cfg, cli_preset.as_deref())?;
    // `lua:` bindings dropped from the PROJECT keymap.toml (security,
    // M4 Lua review): warned about after creating the App, never a silent
    // drop. The `global` context is merged into all THREE screens (H1 T2
    // adds dialog), so the maximum is the count with no doubles (a global
    // binding counts in all of them).
    let discarded_lua = browse_eff
        .discarded_lua_bindings()
        .max(viewer_eff.discarded_lua_bindings())
        .max(dialog_eff.discarded_lua_bindings());

    let mut backend = make_backend(&cfg, cli_daemon, cli_socket).await?;

    // The positional DIR overrides `cwd`; validated here to give a clear
    // error instead of a failed listing inside the already-started TUI.
    let explicit_dir = args.dir.is_some();
    let start = start_dir(args.dir)?;
    // #108 b4: columns and order from `[ui.columns]` — resolved ONCE;
    // invalid ids do not break startup (doctor reports them). BEFORE the
    // initial listings (#117): they also request the configured attrs —
    // without this, attr cells are born blank until the first cd/refresh.
    let columns = norte_frontend::columns::ColumnsSettings::resolve(&cfg.common.ui_columns)
        .with_date_format(cfg.common.ui_chrome.date_format());
    let start_attrs = columns.attr_ids_for(start.scheme());
    let left = initial_pane(&backend, &start, &start_attrs).await?;
    let right = initial_pane(&backend, &start, &start_attrs).await?;
    let mut app = App::new(left, right);
    // The binary's revision, for the help (F1). Empty in tests, which build
    // `App` without going through here and take help snapshots.
    app.version_line = norte_frontend::version::VERSION_LINE;
    // `None` if there was already a subscriber: then nobody writes to the
    // ring and the panel SAYS so, instead of showing an emptiness that looks
    // like nothing is happening.
    app.log_ring = log_ring;
    // An explicit `--profile` is already APPLIED (it entered the first
    // load's layers), so it is declared active here and not through the hot
    // reload path. Along the way, that is what keeps the session's sticky
    // profile from overriding it: the reader named one for this time.
    app.active_profile.clone_from(&cli_profile);
    app.pick = cli_pick; // `--pick` (S2): see the field's rustdoc (`app.rs`).
    app.session.attach = cli_attach; // `--attach` (phase 9): see its rustdoc.
    app.columns = columns;
    app.user_themes.clone_from(&cfg.user_themes);
    // Syncing needs journal AND spool (hard rule 4: `sync.apply` opens an
    // undoable batch and refuses without one; `sync.plan` refuses with no
    // spool). Since #167 the embedded arm DOES carry the state directory's
    // journal (which since #177 opens on its first mutation), but still has
    // no spool, so `is_journalled()` keeps saying no — and tells the truth
    // about the one thing it mitigates, which is syncing. Decided ONCE,
    // here, because the `Backend` does not change arms during the
    // process's life.
    app.backend_journalled = backend.is_journalled();
    // Phase 9: the handover's two blockers, decided once. With no daemon
    // there is nobody to hand the screen to; with no desktop there is
    // nowhere to put it, which is an SSH session's case. Both are STATED
    // before the key, with their reason, instead of failing afterward.
    app.backend_daemon = cli_daemon;
    app.has_desktop = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
        || std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty());
    // And whether there is a daemon to talk about (#328). Here and only
    // once, for the same reason as the line above: the `Backend` does not
    // change arms during the process's life. Without this, a plain `ntc` —
    // with no daemon at all — opened the log panel and its border ended up
    // saying "this daemon does not serve its log", which is a sentence
    // about someone who does not exist.
    app.log_remote.hay_daemon = backend.is_remote();
    // #117: the startup scheme's catalogue — unconditional, like the cd
    // (once per scheme and session; task 4's picker wants it even with no
    // attr columns configured); a failure does NOT bring down startup —
    // with no catalogue it paints with Opaque defaults.
    // H3d: the SAME response carries the caps (`fs.capabilities` returns
    // both halves), so they are cached together — without them the first
    // F1's help would fall back to the syntactic criterion with the data
    // within reach.
    if let Ok(both) = backend.capabilities_and_attrs(&start).await {
        cache_capabilities(&mut app, &start, both);
    }
    for i in 0..app.panes.len() {
        app.apply_scheme_sort(i);
    }
    // #107: `[ui] show_hidden` seeds both panes' INITIAL state; Ctrl+H
    // changes it per pane at runtime (the hot reload does not override it —
    // a user toggle must not be undone because some other field changed).
    if let Some(show) = cfg.common.ui_show_hidden {
        for pane in &mut app.panes {
            pane.set_show_hidden(show);
        }
    }
    // `[ui] menu_bar`: pinned unless told otherwise. Absent = `true`, the
    // opposite criterion from almost every other key, and deliberate: the
    // menu was the only door to several commands and there was nothing on
    // screen saying it existed.
    app.menu_bar = cfg.common.ui_menu_bar.unwrap_or(true);
    app.panel_bar = cfg.common.ui_panel_bar.unwrap_or(true);
    app.chrome = cfg.common.ui_chrome;
    app.status_plugins.clone_from(&cfg.common.ui_status_plugins);
    app.history.set_capacity(app.chrome.history_size());
    app.set_parent_row(cfg.common.ui_parent_entry.unwrap_or(true));
    // `[ui] layout`: a saved layout. A layout that fails to load does NOT
    // leave norte with no screen — it warns through the bar and starts with
    // `orthodox`, which is what the user had before writing the key.
    // `--layout` beats `[ui] layout`: choosing a layout for ONE startup must
    // not touch your config, which is exactly what the key does.
    //
    // `orthodox` is NOT filtered here. It used to be — `App` is already born
    // with that tree, so loading it looked like extra work — and that made a
    // user's `layouts/orthodox.toml` honored by the window and ignored by
    // the terminal: the same config layer saying two things depending on
    // where you come in from.
    let layout_name: Option<std::ffi::OsString> = cli_layout
        .clone()
        .or_else(|| cfg.common.ui_layout.clone().map(std::ffi::OsString::from));
    if let Some(name) = layout_name {
        // The file is read OUTSIDE the runtime (rule 2), and with no config
        // directory there is no file to speak of: the preset with that name
        // is kept.
        let loaded = match config::user_config_dir() {
            Some(dir) => {
                let n = name.clone();
                tokio::task::spawn_blocking(move || norte_frontend::layout::config::load(&dir, &n))
                    .await
                    .unwrap_or_else(|_| {
                        Err(norte_frontend::layout::LayoutError::NotFound(String::new()))
                    })
            }
            None => Err(norte_frontend::layout::LayoutError::NotFound(String::new())),
        };
        app.apply_loaded_layout(&name, loaded);
    }
    // `[ui] confirm_quit` also in the model: quitting from inside a side
    // panel is decided by `App`, not the run loop's dispatch.
    app.confirm_quit = cfg.common.ui_confirm_quit;
    // L2: the screen you left. Goes AFTER `[ui] layout` on purpose — a saved
    // session is more specific than a config preference, and it is the one
    // that wins — and before the theme, which depends on neither. A
    // failure does NOT bring down startup: it continues with the config's
    // screen.
    restore_session(
        &mut app,
        &backend,
        explicit_dir.then_some(&start),
        &cfg.common.profile_start,
    )
    .await;
    apply_theme(&mut app, &cfg);
    // Copy of the hotlist into the App (spec 2026-07-18): the `Ctrl+D`
    // popup's source; refreshed on every OK hot reload (`reload_config`).
    app.set_hotlist(cfg.common.hotlist.clone());
    // If the layout already carries the sidebar — `full`, `explorer`,
    // yesterday's session — mounting it left the requested drives. They are
    // served HERE and not in the loop's first turn because the loop paints
    // before handling anything, and the first frame would show the section
    // blank.
    drain_places_drives(&mut app, &backend).await;
    // Overlay footer hints (H1 T3, #24): PRECOMPUTED from the effective
    // `dialog` BEFORE it moves into the `Resolver` below — same as
    // `help_lines`, rebuilt on every OK hot reload.
    app.dialog_hints = DialogHints::build(&dialog_eff);
    // `[ui] dialog_buttons` (spec 2026-09-10): the key row as buttons. The
    // config knows this, not the effective map.
    app.dialog_hints.buttons = app.chrome.dialog_buttons();
    // The key bar (spec 2026-09-10): from the THREE effectives, here and on
    // every OK hot reload, for the same reason as the hints.
    app.key_bars = norte_tui::app::KeyBars::build(&browse_eff, &viewer_eff);
    app.chord_split_h = norte_frontend::palette::first_chord("layout.split-h", &browse_eff);
    // #142: the chord that returns the panels, from the SAME effective map
    // and at the same moment as above. If a rebind did not reach here, the
    // key that opens the subshell and the one that closes it would be
    // different.
    app.subshell_chord = norte_frontend::subshell::detach_chord(&browse_eff);
    app.terminal_chord = browse_eff.lone_chord(norte_tui::termpanel::COMMAND);
    // Declarative openers (#28): `pane.open`'s source (F4).
    app.openers = cfg.openers.clone();
    // `[ui] editor` (#133): norte's editor, if the config names one.
    // Without it, `$VISUAL`/`$EDITOR` rule, as usual.
    app.editor = cfg
        .common
        .ui_editor
        .clone()
        .map(|command| norte_tui::app::EditorSpec {
            command,
            detached: cfg.common.ui_editor_detached.unwrap_or(false),
        });
    // `[ui] diff` (#312): the two-file comparator. Without it, `diff -u`.
    app.diff = cfg
        .common
        .ui_diff
        .clone()
        .map(|command| norte_tui::app::EditorSpec {
            command,
            detached: cfg.common.ui_diff_detached.unwrap_or(false),
        });
    // Daemon-mode channels (None when embedded): other frontends' tasks and
    // (re)connection notices — drained in the main loop.
    let foreign_tasks = backend.take_foreign_tasks();
    let conn_events = backend.take_conn_events();
    let approvals = backend.take_approvals();
    // #44: `connection.degraded` notices from the daemon → persistent
    // indicator.
    let degraded = backend.take_degraded();
    // #322: `connection.failed` notices → WHY one could not be opened.
    // Separate channel from the one above and not an enum: they are two
    // different facts — an open session traveling badly, and one that never
    // got to open — and mixing them makes one paint as the other.
    let failed = backend.take_failed();
    // ADR 0100: what a `hook` plugin wanted to say about a mutation already
    // recorded, or that its hooks got turned off. When embedded this STARTS
    // the hook dispatcher over this session's journal.
    let plugin_notices = backend.take_plugin_notices();
    // #167/#177: the embedded arm opens the journal on its first mutation,
    // and if it turns out another process has it, this session mutates WITH
    // NO record. That is said IN the session and the instant it happens: a
    // startup `eprintln!` would be covered by the alternate screen a second
    // later, and it is not even known at startup time anyway. (A permanent
    // bar indicator would be better than a message the next one erases;
    // still pending.)
    let journal_warnings = backend.take_journal_warnings();
    let mut help_lines = norte_tui::help::build(&browse_eff, &viewer_eff, &dialog_eff);
    // H3b: the chord resolver the help corpus is rendered through. Built from
    // the SAME three effectives as `help_lines` and BEFORE they move into the
    // `Resolver`s below (it borrows), and rebuilt alongside them on every hot
    // reload — the obligation `TuiChords`' own rustdoc states: a rebind that
    // does not reach this resolver is a page that teaches the OLD key.
    app.help_chords = Arc::new(TuiChords::new(&browse_eff, &viewer_eff, &dialog_eff, lang));
    // Command palette rows (H1 T4): PRECOMPUTED from the browse/viewer
    // effectives BEFORE they move into the `Resolver` below — same
    // criterion as `help_lines`/`dialog_hints`.
    app.palette_rows = norte_tui::palette::build_rows(&browse_eff, &viewer_eff);
    // The first-run wizard (spec 2026-09-10): with no user `norte.toml`, or
    // with `--setup`. Never under `--pick`.
    let wizard = norte_tui::wizard::should_open(cli_setup, cli_pick).await;
    if wizard {
        norte_tui::wizard::open(&mut app, &cfg);
    }
    // The splash screen (spec 2026-09-15, phase 2). AFTER the wizard and
    // knowing whether it opened: both would cover the first frame, and
    // whichever one asks something rules.
    let splash_mode = cfg.common.ui_chrome.splash();
    if norte_tui::splash::should_open(splash_mode, cli_no_splash, cli_pick, wizard) {
        norte_tui::splash::open(&mut app, splash_mode, &cfg);
    }
    let mut resolver = Resolver::new(browse_eff);
    let mut viewer_resolver = Resolver::new(viewer_eff);
    // H1 T2: a resolver shared by ALL overlays (modal, theme picker,
    // extensions, nav popup) — mutually exclusive in the run loop (the
    // `if`/`else if` further below), so a single sequence state is enough.
    // The `[dialog]` presets are ONE-chord; a `Resolution::Pending` (only
    // possible with a multi-key sequence from a user layer) is treated as
    // ignore-and-reset in every handler — no overlay semantics defined for
    // that yet.
    let mut dialog_resolver = Resolver::new(dialog_eff);

    // Hot reload: watching the layers, with a notice if it degrades to
    // polling.
    let (cfg_tx, cfg_rx) = tokio::sync::mpsc::channel(8);
    // ALL profiles are watched, not just the active one.
    //
    // The watcher is created once, here, and switching profiles on a hot
    // reload rebuilds the loop's layers but CANNOT rebuild it. Watching only
    // startup's layers, a setting written in the profile you just switched
    // to — the theme, for one — did not trigger a reload: it was saved in
    // the right place and the screen did not change.
    //
    // Watching all of them costs one watch per directory and is plenty for
    // what there is (a handful of profiles), and along the way it makes
    // editing a profile's file by hand reload just like editing your own
    // does, which is what norte promises about its config.
    let mut watch_dirs = layers.dirs.clone();
    if let Some(root) = norte_config::profiles_dir_from(&|k| std::env::var_os(k)) {
        for name in norte_config::list_profiles(&root).unwrap_or_default() {
            let dir = root.join(name);
            if !watch_dirs.iter().any(|(d, _)| *d == dir) {
                watch_dirs.push((dir, config::Layer::Profile));
            }
        }
    }
    let watch = config::watch(&config::Layers { dirs: watch_dirs }, cfg_tx).await;
    if watch.mode == WatchMode::Polling {
        app.message = Some(t("msg-config-polling"));
    }
    // A project layer that failed to load (#260): before Lua's, which is
    // the one that must not be overridden.
    if !cfg.common.project_warnings.is_empty() {
        app.message = Some(ta(
            "msg-project-config-skipped",
            &[("n", &cfg.common.project_warnings.len().to_string())],
        ));
        for reason in &cfg.common.project_warnings {
            tracing::warn!(reason = %reason, "project layer ignored");
        }
    }
    // And the PROFILE lines that make no sense, with the same split: the
    // count to the bar, the reason to the log. They had been computed since
    // profiles existed and nobody showed them, which is what left a
    // mistyped `[profile.start]` unseeded and unmentioned.
    //
    // AFTER the project's and BEFORE Lua's, which is the window's order and
    // the same criterion: the last one wins, and the security one — a
    // foreign repository choosing what code a key runs — is the one that
    // must not be overridden.
    if !cfg.common.profile_warnings.is_empty() {
        app.message = Some(ta(
            "msg-profile-config-ignored",
            &[
                (
                    "profile",
                    &app.active_profile
                        .as_ref()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                ),
                ("n", &cfg.common.profile_warnings.len().to_string()),
            ],
        ));
        for reason in &cfg.common.profile_warnings {
            tracing::warn!(reason = %reason, "profile line ignored");
        }
    }
    // AFTER the polling notice: the security one must not be overridden.
    if discarded_lua > 0 {
        app.message = Some(ta(
            "msg-lua-keymap-project",
            &[("n", &discarded_lua.to_string())],
        ));
    }

    let (tty_out, mut mouse_out) = open_terminal_or_exit()?;
    let mut terminal = tty::init(tty_out)?;
    let mut capture = arm_mouse(&cfg, &mut app, &mut mouse_out);
    // The kitty protocol support probe, ONCE and here: with raw mode
    // already set and before `run` raises the event reader, which would
    // hold crossterm's lock and leave it answering "no" after two seconds.
    // It is asked even with the key off, so turning it on from settings
    // works with no restart.
    let _ = norte_tui::alt_menu::query_support();
    // The same question for GRAPHICS, at the same spot and for the same two
    // reasons: the event reader does not exist yet and stdout is still the
    // terminal. Nobody paints anything with the answer yet — that belongs
    // to another task in this phase.
    let _ = norte_tui::kitty_graphics::query_support();
    // `[ui] alt_menu`: a terminal with no protocol receives nothing, and a
    // write failure leaves the TUI without the gesture, not without
    // starting.
    if let Err(e) = norte_tui::alt_menu::set(
        cfg.common.ui_alt_menu.unwrap_or(false),
        norte_tui::alt_menu::supported,
        &mut mouse_out,
    ) {
        tracing::warn!(error = %e, "could not request kitty's keyboard protocol");
    }
    let res = run(
        &mut terminal,
        &mut capture,
        &mut app,
        &backend,
        &mut resolver,
        &mut viewer_resolver,
        &mut dialog_resolver,
        &mut help_lines,
        lang,
        layers,
        cli_preset,
        cfg.quick_search_mode,
        cfg.common.ui_confirm_quit,
        cfg,
        cfg_rx,
        foreign_tasks,
        conn_events,
        approvals,
        degraded,
        failed,
        plugin_notices,
        journal_warnings,
    )
    .await;
    let _ = capture.set(false, terminal.backend_mut());
    restore_terminal(&mut terminal);
    drop(watch);
    res?; // A broken run loop is not a cancelled `--pick`.
    write_cd_file(&app, cli_cd_file.as_deref());
    finish_pick(&mut app);
    Ok(())
}

/// `--cd-file` (S3): writes the final directory for the `norte shell-init`
/// wrapper to read, on a clean quit only. A no-op when the flag was never
/// passed.
///
/// Placed after `res?`, not before: a run loop that returned an error bails
/// out of `main` right there and never reaches this call, so a crash writes
/// nothing to the cd-file — the design's own rule (§C: "the write happens at
/// the end", so the shell stays where it was).
///
/// Placed after [`restore_terminal`] too, deliberately DIFFERENT from the
/// design note's "before restoring the terminal": [`finish_pick`]
/// establishes, for the exact same shutdown window, that nothing must be
/// printed before the alternate screen is left or the terminal swallows it.
/// The `msg-cd-not-local` line below is exactly such a print, so it follows
/// `finish_pick`'s placement, not the design prose. Still runs BEFORE
/// `finish_pick` itself, whose `std::process::exit` would otherwise skip
/// this entirely when both `--pick` and `--cd-file` are given.
///
/// A write failure is printed and swallowed, the same shape as
/// `restore_terminal`'s own failure: `--cd-file`'s exit codes are not
/// contracted the way `--pick`'s are (design §B's 0/1/2 table is that flag's
/// alone), and nothing downstream is waiting on this process's exit code the
/// way a shell wrapper waits on `--pick`'s.
fn write_cd_file(app: &App, cd_file: Option<&std::path::Path>) {
    let Some(path) = cd_file else { return };
    if let Some(bytes) = norte_frontend::shell::cd_bytes(app.focused().dir()) {
        use std::io::Write as _;
        let wrote = std::fs::File::create(path)
            .and_then(|mut f| f.write_all(&bytes).and_then(|()| f.flush()));
        if let Err(e) = wrote {
            eprintln!("ntc: failed to write --cd-file: {e}");
        }
    } else {
        // Same masking convention as the CLI's own plain-text output
        // (`norte-cli/src/main.rs`'s semantic-search listing): `path_display`
        // gives the badge as a bool because a raw stderr line has no
        // styling to hang it on, so a hostile name is marked with a
        // leading `!` instead of colour.
        let (text, hostile) = norte_frontend::path_display(app.focused().dir());
        let marked = if hostile { format!("!{text}") } else { text };
        eprintln!("{}", ta("msg-cd-not-local", &[("path", &marked)]));
    }
}

/// `--pick` (S2): the picker's exit, decided AFTER the terminal is restored
/// — never before, or the alternate screen swallows every byte (the whole
/// point of Task 1). Exit codes per the design's table: 0 accepted
/// (written), 1 cancelled (nothing written — `q`/`F10` under `--pick` never
/// populate `app.picked`), 2 reserved for the no-tty error in
/// [`open_terminal_or_exit`] and, here, a write failure the caller needs to
/// tell apart from "user picked nothing".
///
/// Returns normally only when `--pick` was never passed: every other path
/// exits the process directly, so `main` never reaches its own `Ok(())`
/// with a pick outstanding.
fn finish_pick(app: &mut App) {
    if let Some(paths) = app.picked.take() {
        use std::io::Write as _;
        let bytes = norte_frontend::shell::pick_bytes(&paths);
        let mut stdout = std::io::stdout();
        if let Err(e) = stdout.write_all(&bytes).and_then(|()| stdout.flush()) {
            eprintln!("ntc: failed to write the pick: {e}");
            std::process::exit(2);
        }
        std::process::exit(0);
    }
    if app.pick {
        std::process::exit(1);
    }
}

/// Undoes [`tty::init`]. The same thing `ratatui::restore()` used to do:
/// there is not much to do if it fails, so it is printed and exiting
/// continues.
fn restore_terminal(terminal: &mut tty::Tui) {
    if let Err(e) = tty::restore(terminal) {
        eprintln!("ntc: failed to restore terminal: {e}");
    }
}

/// Opens the CONTROL terminal (`tty.rs`, not stdout — from `--pick` stdout
/// carries data) and a second, duplicated descriptor for `arm_mouse`, which
/// is called before `run` receives the `Tui` and therefore cannot borrow the
/// handle that moves into [`tty::init`] (the backend's sole owner).
///
/// With no controlling terminal (cron, both ends with a pipe) it is a
/// readable error and exit code 2 — never a screenful of escapes in whoever
/// invoked us's pipe.
fn open_terminal_or_exit() -> Result<(tty::TtyOut, tty::TtyOut)> {
    let out = match tty::open_controlling_terminal() {
        Ok(out) => out,
        Err(e) => {
            eprintln!("ntc: no controlling terminal: {e}");
            std::process::exit(2);
        }
    };
    let mouse_out = out
        .try_clone()
        .context("could not duplicate the terminal's descriptor")?;
    Ok((out, mouse_out))
}

/// Requests mouse capture unless `[ui] mouse` disables it (default ON).
///
/// Requested BEFORE entering the run loop, and `main` ALWAYS retracts it on
/// the way out, whatever happens: a terminal returned in mouse mode spits
/// escape sequences the moment the user moves the pointer, and by then
/// nobody is left listening to them.
///
/// An emulator that does not accept the sequence is not a reason not to
/// start: it continues with no mouse and says so through the bar, never in
/// silence (the user will click and nothing will happen).
fn arm_mouse(cfg: &config::LoadedConfig, app: &mut App, out: &mut tty::TtyOut) -> mouse::Capture {
    // `tty::init`'s panic hook already releases the alternate screen, raw
    // mode AND the mouse over its own handle — but mouse capture is a
    // DECSET for the WHOLE terminal, not something that leaves with the
    // screen, so it is wrapped again here in case this function ever arms
    // something `tty::init` does not know how to undo. Same pattern: the
    // current hook is taken and replaced with one that first releases the
    // mouse and THEN calls it. The closure cannot borrow `out` (the borrow
    // does not survive this function), so it opens a new handle to the
    // control terminal at the moment of the panic — just like `tty::init`
    // does.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Ok(mut tty_out) = tty::open_controlling_terminal() {
            let _ = crossterm::execute!(tty_out, crossterm::event::DisableMouseCapture);
        }
        previous(info);
    }));
    let mut capture = mouse::Capture::new();
    if let Err(e) = capture.set(cfg.common.ui_mouse.unwrap_or(true), out) {
        tracing::warn!(error = %e, "could not enable mouse capture");
        app.message = Some(t("msg-mouse-capture-failed"));
    }
    capture
}

/// The TUI's boolean flags.
const BOOL_FLAGS: &[&str] = &["--daemon", "--pick", "--setup", "--no-splash", "--attach"];
/// The TUI's value flags.
const VALUE_FLAGS: &[&str] = &[
    "--preset",
    "--layout",
    "--profile",
    "--socket",
    "--cd-file",
    "--lang",
];

/// `--help`'s text. In ENGLISH and with no Fluent on purpose: printed
/// BEFORE the language is negotiated (which comes from the config, not yet
/// read).
const USAGE: &str = "\
ntc — orthodox file manager, terminal frontend

Usage: ntc [OPTIONS] [DIR]

Arguments:
  [DIR]  Directory to start in (default: the current directory)

Options:
      --preset <NAME>    Keymap preset (orthodox|vim|cua); overrides norte.toml
      --layout <NAME>    Layout for this run (orthodox|simple|krusader|explorer|full,
                         or one of your own under `layouts/`); overrides norte.toml
      --profile <NAME>   Start in this profile — a directory under `profiles/`
                         in your config dir. Overrides the one you were last
                         in; refuses to start if it cannot be used
      --lang <LANG>      Language for this run (es|en); overrides NORTE_LANG
                         and [ui] lang in norte.toml
      --daemon           Talk to the daemon instead of the embedded core
      --socket <PATH>    Daemon socket (default: $XDG_RUNTIME_DIR/norte/daemon.sock)
      --pick             print the selection, NUL-terminated, and exit
      --cd-file PATH     write the final directory here, NUL-terminated
                         (used by the `norte shell-init` wrapper)
      --setup            Run the first-start wizard again (keys, theme, icons).
                         It also runs on its own when you have no norte.toml;
                         NORTE_NO_WIZARD=1 keeps it closed
      --no-splash        No start screen this run, whatever `[ui] splash` says
                         (NORTE_NO_SPLASH=1 does the same)
      --attach           Take over the screen another frontend just handed off
                         (`app.handoff`): its marks come back too. Without it,
                         a start is a start and marks stay where they were
  -h, --help             Print help
  -V, --version          Print version
";

/// Startup directory as a [`VPath`]: the command line's `[DIR]` if it came,
/// otherwise `cwd`. Validated BEFORE taking the terminal, to give a
/// readable error instead of a failed listing inside the already-started
/// TUI.
///
/// A Windows UNC cwd (`\\server\share`, `\\wsl$\…`) already round-trips:
/// `vpath_from_native` puts it in as the first segment and `to_native`
/// restores it as the base for the OS's root (#22). An unrepresentable one
/// gives a clear error, never a panic.
fn start_dir(dir: Option<std::path::PathBuf>) -> Result<VPath> {
    let native = match dir {
        Some(d) => {
            let meta =
                std::fs::metadata(&d).with_context(|| format!("cannot open {}", d.display()))?;
            anyhow::ensure!(meta.is_dir(), "{} is not a directory", d.display());
            std::path::absolute(&d).unwrap_or(d)
        }
        None => std::env::current_dir().context("cwd")?,
    };
    norte_vfs_local::vpath_from_native(&native)
        .map_err(|e| anyhow::anyhow!("{} not representable as a VPath: {e}", native.display()))
}

/// Resolves the "exit immediately" arguments: prints `--help`/`--version`
/// (returning `None`, the caller exits) and turns an unknown flag into an
/// error. `--help` used to fall into the "ignore" arm and the binary kept
/// going until it tried to take the TTY, where it died with a ratatui
/// panic.
fn args_or_exit(args: norte_frontend::cli::Cli) -> Result<Option<norte_frontend::cli::Cli>> {
    if args.help {
        print!("{USAGE}");
        return Ok(None);
    }
    if args.version {
        println!("ntc {}", norte_frontend::version::VERSION_LINE);
        return Ok(None);
    }
    if let Some(flag) = &args.unknown {
        anyhow::bail!("unknown flag `{flag}` — try `ntc --help`");
    }
    Ok(Some(args))
}

/// Chooses the transport (rule 7): `--daemon` or `[daemon] mode = daemon`
/// connects to the socket (launching `norte daemon run` if needed);
/// anything else = embedded (instant startup, the default).
async fn make_backend(
    cfg: &config::LoadedConfig,
    cli_daemon: bool,
    cli_socket: Option<std::path::PathBuf>,
) -> Result<Backend> {
    let want_daemon = cli_daemon || cfg.common.daemon.mode == Some(config::DaemonMode::Daemon);
    if !want_daemon {
        // #167: the embedded transport records its mutations (hard rule 4)
        // in the state directory's journal, the same one the daemon opens.
        // If another process holds the exclusive lock it continues without
        // it, warning — see `norte_core::embedded`.
        //
        // Building it does NOT open the file (#177): an `ntc` that only
        // navigates does not take the journal away from the daemon or a
        // `norte audit`. The lock is taken on the first mutation, and the
        // notice — if there is one — arrives via `take_journal_warnings`'s
        // channel, already inside the session.
        let engine = norte_core::embedded::engine_in(&norte_core::connect::config_dir());
        // #95.2: archive anti-bomb limits from `[archive]` (user layers,
        // never the project one). Before any navigation: composite
        // providers are cached with the limits from their first use.
        // rust review item 3 (C1): the override+saturation conversion lived
        // duplicated here and in `norte_core::archive_config` — a single
        // home in the core (`limits_from_overrides`) so the TUI and the
        // daemon never diverge on the anti-bomb limits.
        if let Some(limits) = norte_core::archive_config::limits_from_overrides(
            cfg.common.archive.max_entries,
            cfg.common.archive.max_decompressed_bytes,
            cfg.common.archive.max_nesting,
        ) {
            engine.set_archive_limits(limits);
        }
        // Roadmap item 11: the program that reads RARs, if the config sets
        // one. Comes from the same `cfg.common` already loaded, so it does
        // not honor the Project layer — and here that is not a preference,
        // it is that a foreign repo does not choose which binary gets
        // launched.
        engine.set_rar_delegate(
            cfg.common
                .archive
                .rar_delegate
                .as_ref()
                .map(std::path::PathBuf::from),
        );
        // Local provider, connector (an sftp://…/ftp://… path, navigable if
        // the host key is already trusted) and opt-in AI: what the whole
        // engine carries, decided in `norte_core::team` and not here
        // (rule 7).
        //
        // Notices go to the log and not to stderr: ratatui is about to take
        // the screen, and this binary already installs a subscriber
        // (`init_to_file`, roadmap item 9).
        let equipped = norte_core::team::equipar(
            &engine,
            &norte_core::connect::config_dir(),
            norte_core::team::Ia::ALL,
        )
        .await;
        for notice in equipped.notices {
            tracing::warn!("{notice}");
        }
        return Ok(Backend::Embedded(Arc::new(engine)));
    }
    #[cfg(not(unix))]
    {
        let _ = (cli_socket, cfg);
        anyhow::bail!("daemon mode is not available on Windows yet (issue #33)");
    }
    #[cfg(unix)]
    {
        use norte_core::backend::remote::RemoteBackend;
        let socket = match cli_socket.or_else(|| cfg.common.daemon.socket.clone()) {
            Some(s) => s,
            None => tokio::task::spawn_blocking(|| norte_core::daemon::default_socket_path(None))
                .await
                .context("socket resolution")?,
        };
        let exe = std::env::current_exe().context("current_exe")?;
        // The daemon's binary is `norte` (the CLI), not `norte-tui`: next
        // to the current executable inside the same install directory.
        let daemon_bin = exe.with_file_name("norte");
        // The argv is the shared one: whatever starts this terminal only
        // shuts down once its last client leaves.
        let spawn_cmd = norte_core::daemon::daemon_run_argv(daemon_bin, &socket);
        let remote = RemoteBackend::connect(
            socket,
            Some(spawn_cmd),
            norte_proto::methods::ClientInfo {
                // The BINARY's name, not the crate's: this is what the
                // daemon records and what a human reads in a log or in
                // `norte doctor`, and the program that started up has to
                // show up there.
                name: "ntc".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("could not talk to the daemon")?;
        Ok(Backend::Remote(remote))
    }
}

#[cfg(test)]
mod tests {
    use super::{BOOL_FLAGS, USAGE, VALUE_FLAGS};

    /// `ntc` ACCEPTS what the window passes it in a handover (phase 9).
    ///
    /// The other half of the test lives in the window, and for the same
    /// bug: the handover's `argv` was built in one binary and parsed in the
    /// other with nothing tying them together, and the window died silently
    /// on a flag it did not know. Both now pull their flags from
    /// `norte_frontend::handoff`, and each one tests with ITS parser what
    /// the other builds.
    #[test]
    fn ntc_accepts_the_handovers_argv() {
        for daemon in [true, false] {
            let parsed = norte_frontend::cli::parse(
                norte_frontend::handoff::terminal_args(daemon),
                BOOL_FLAGS,
                VALUE_FLAGS,
            );
            assert_eq!(parsed.unknown, None, "daemon={daemon}");
            assert!(parsed.has(norte_frontend::handoff::ATTACH));
        }
    }

    /// Every flag this binary READS is registered, and shows up in
    /// `--help`.
    ///
    /// The three lists are one single thing written three times — the
    /// parser's table, the reading in `main`, and the help text — and
    /// nothing ties them together. `--profile` was added by reading it in
    /// `main` with no registration, so the parser rejected it as unknown:
    /// the flag existed, the code that used it existed, and `ntc --profile
    /// work` answered "unknown flag". No suite saw it; running it did.
    #[test]
    fn every_registered_flag_shows_up_in_help() {
        for f in BOOL_FLAGS.iter().chain(VALUE_FLAGS.iter()) {
            assert!(
                USAGE.contains(f),
                "{f} is registered and missing from --help"
            );
        }
    }

    /// And the other way around: every long flag help promises is
    /// registered, or the parser will reject it as unknown right when
    /// someone copies it from there.
    #[test]
    fn every_flag_in_help_is_registered() {
        let registered: Vec<&str> = BOOL_FLAGS
            .iter()
            .chain(VALUE_FLAGS.iter())
            .copied()
            .collect();
        for line in USAGE.lines() {
            for word in line.split_whitespace() {
                let clean = word.trim_end_matches(',');
                // `--help`/`--version` are served by the parser itself, not
                // these tables.
                if clean.starts_with("--")
                    && clean.len() > 2
                    && !matches!(clean, "--help" | "--version")
                {
                    assert!(
                        registered.contains(&clean),
                        "--help promises {clean} and the parser does not know it"
                    );
                }
            }
        }
    }
}
