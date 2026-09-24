//! Config hot-reload (ADR 0007): rereads all layers and applies ALL of them
//! or NONE.
//!
//! Used to live in the `ntc` binary's root — a crate DIFFERENT from this
//! lib — and is the branch's function with the most parameters (twelve):
//! everything the event loop retains that a reload can replace. It is not
//! API, it is wiring, and that is why it has carried its
//! `#[expect(clippy::too_many_arguments)]` since before it moved.
//!
//! The criterion that orders the whole body: nothing is applied until all
//! three keymaps have been built successfully. A TOML half-saved — and the
//! watcher does see them half-saved — leaves the session EXACTLY as it was,
//! with a warning on the bar.

use std::sync::Arc;

use norte_core::backend::Backend;
use norte_i18n::{t, ta};

use crate::app::{App, config_error_category, keymaps_error_category};
use crate::config::{self, Layers};
use crate::help::TuiChords;
use crate::hints::DialogHints;
use crate::keymap::Resolver;
use crate::nav;
use crate::screens::pickers::apply_theme;
use crate::screens::settings::plugin_config_summaries;
use crate::shortcuts_editor::{Maps, build_keymaps, shortcut_rows};

/// Hot-reload (ADR 0007): rereads ALL layers; on ANY error the current config
/// is kept and a warning goes on the bar — never break a running session over
/// a half-saved TOML.
///
/// Returns whether the reload APPLIED. The watcher is satisfied with the
/// bar's warning, but a PROFILE switch (ADR 0079, D8) is not: its step 2 is
/// this very reload with different layers, and what follows — mounting the
/// new profile's layout, seeding its slots, marking it active — can only
/// happen if this applied. A half-applied profile is not a state that design
/// admits, and the "all or nothing" this function already had is exactly the
/// semantics needed.
#[expect(clippy::too_many_arguments, reason = "hot-reload wiring, not API")]
pub async fn reload_config(
    app: &mut App,
    backend: &Backend,
    resolver: &mut Resolver,
    viewer_resolver: &mut Resolver,
    dialog_resolver: &mut Resolver,
    help_lines: &mut Vec<ratatui::text::Line<'static>>,
    // H3b: the negotiated language, so the rebuilt `TuiChords` answers in the
    // same locale it did at startup. Session-fixed (`norte_i18n::force` runs
    // once), so a `[ui] lang` edited in the file does NOT take effect here —
    // the same restriction the rest of the i18n already has.
    lang: norte_i18n::Lang,
    layers: &Layers,
    cli_preset: Option<&str>,
    quick_mode: &mut nav::Mode,
    confirm_quit: &mut config::ConfirmQuit,
    // S3 (`app.settings`): the COMPLETE snapshot `run()` retains to
    // build/refresh the settings overlay — replaced WHOLESALE only if the
    // ENTIRE reload applied (same criterion as the rest of this function); a
    // failed reload leaves the CURRENT config, never a halfway one.
    cfg_out: &mut config::LoadedConfig,
) -> bool {
    match config::load_async(layers.clone()).await {
        Ok(cfg) => match build_keymaps(&cfg, cli_preset) {
            Ok((browse, viewer, dialog)) => {
                // The quick search mode follows the current config (only
                // affects NEW quick searches; an open one keeps its own).
                // Same criterion as the theme: only if EVERYTHING applied.
                *quick_mode = cfg.quick_search_mode;
                // `[ui] confirm_quit` (S2): same criterion — only affects
                // NEW `app.quit`s (one already open as `Modal::ConfirmQuit`
                // keeps its decision until the user answers).
                *confirm_quit = cfg.common.ui_confirm_quit;
                app.confirm_quit = cfg.common.ui_confirm_quit;
                // The hotlist copy too (an open popup keeps its snapshot
                // until reopened — items frozen on purpose).
                app.set_hotlist(cfg.common.hotlist.clone());
                // `[ui] menu_bar` hot: each frame's layout reads it, so the
                // bar appears or disappears on the next paint — and the mouse
                // follows it, because it reads that same layout.
                app.menu_bar = cfg.common.ui_menu_bar.unwrap_or(true);
                app.panel_bar = cfg.common.ui_panel_bar.unwrap_or(true);
                // The whole chrome, for the same reason: every frame reads it.
                app.chrome = cfg.common.ui_chrome;
                // Plugin status items (ADR 0137): the bar reads them every
                // frame, and the next listing asks for their columns.
                app.status_plugins.clone_from(&cfg.common.ui_status_plugins);
                // Finding 3 (branch review, phase 5): `[ui] images` lives in
                // the chrome just reassigned above — `App::viewer_modo` was
                // set when the viewer OPENED, and without this cut a `Kitty`
                // that stops being one on the fly left the pixels already
                // placed on screen forever. See the rustdoc of
                // `App::soltar_miniatura_si_deja_de_ser_kitty` for why the
                // opposite direction is NOT followed here.
                app.soltar_miniatura_si_deja_de_ser_kitty(crate::viewer_open::modo_efectivo(
                    app.chrome.images(),
                    crate::kitty_graphics::soportado(),
                ));
                // The history cap, also hot: lowering it drops the farthest
                // entries, never what the reader just walked.
                app.history.set_capacity(app.chrome.history_size());
                // The `..` row, also hot: it is presentation, and the pane
                // adds or removes it without touching the listing.
                app.set_parent_row(cfg.common.ui_parent_entry.unwrap_or(true));
                // Openers (#28): reloaded with the rest of the config.
                app.openers = cfg.openers.clone();
                // And the `[ui] editor` editor, for the same reason: whoever
                // changes it in the file need not restart norte.
                app.editor = cfg
                    .common
                    .ui_editor
                    .clone()
                    .map(|command| crate::app::EditorSpec {
                        command,
                        detached: cfg.common.ui_editor_detached.unwrap_or(false),
                    });
                // And the `[ui] diff` comparator (#312), for the same reason.
                app.diff = cfg
                    .common
                    .ui_diff
                    .clone()
                    .map(|command| crate::app::EditorSpec {
                        command,
                        detached: cfg.common.ui_diff_detached.unwrap_or(false),
                    });
                // #108 7a: `[ui.columns]` edited externally also refreshes
                // the session (it used to only apply at startup); the re-sort
                // keeps the panes consistent with the file — the picker's
                // persist triggers this very path and is idempotent with what
                // is already applied in memory.
                app.columns =
                    norte_frontend::columns::ColumnsSettings::resolve(&cfg.common.ui_columns)
                        .with_date_format(cfg.common.ui_chrome.date_format());
                for i in 0..app.panes.len() {
                    app.apply_scheme_sort(i);
                }
                // The user's themes, for the same reason: a theme just
                // dropped into `themes/` shows up in the selector without a
                // restart. The reload already runs where disk can be read;
                // the selector only reads this.
                app.user_themes.clone_from(&cfg.user_themes);
                // `lua:` bindings discarded from the PROJECT keymap
                // (security — same warning as at startup; the maximum,
                // because `global` merges into all three screens, H1 T2 adds
                // dialog).
                let discarded_lua = browse
                    .discarded_lua_bindings()
                    .max(viewer.discarded_lua_bindings())
                    .max(dialog.discarded_lua_bindings());
                // Help reflects the CURRENT keymap: rebuilt here.
                *help_lines = crate::help::build(&browse, &viewer, &dialog);
                // H3b: and so does the resolver the CORPUS is rendered
                // through — same effectives, same moment, before they move
                // into the resolvers below (`TuiChords` borrows). A rebind
                // that reached `help_lines` but not this one would leave the
                // generated keyboard page right and every `{{cmd:…}}` mark in
                // the prose teaching the OLD key.
                app.help_chords = Arc::new(TuiChords::new(&browse, &viewer, &dialog, lang));
                app.help = None;
                // Palette rows (H1 T4): rebuilt from the CURRENT keymap,
                // BEFORE it moves into the resolver below — same criterion as
                // help_lines. An open palette is closed (like help): its
                // frozen rows could point at descriptions/chords already
                // stale.
                app.palette_rows = crate::palette::build_rows(&browse, &viewer);
                app.palette = None;
                // Overlay hints (H1 T3, #24): rebuilt from the CURRENT
                // effective `dialog`, BEFORE it moves into the resolver
                // below — same criterion as help_lines.
                app.dialog_hints = DialogHints::build(&dialog);
                app.dialog_hints.buttons = cfg.common.ui_chrome.dialog_buttons();
                // And the key bar, from all three (spec 2026-09-10).
                app.key_bars = crate::app::KeyBars::build(&browse, &viewer);
                app.chord_split_h = norte_frontend::palette::first_chord("layout.split-h", &browse);
                // #142: the subshell's return chord, from the CURRENT
                // effective `browse` and before it moves to the resolver —
                // same criterion. A rebind that did not reach here would
                // leave the reader inside the shell pressing the new key.
                app.subshell_chord = norte_frontend::subshell::detach_chord(&browse);
                app.terminal_chord = browse.lone_chord(crate::termpanel::COMANDO);
                // K3c: the shortcuts editor, if open, is REFRESHED (not
                // closed like `help`/`palette`): this reload is usually its
                // own write coming back through the watcher, and an editor
                // that closed on every rebind would not serve for the second
                // one. Its rows come from the CURRENT effectives, before they
                // move into the resolvers — same criterion as `help_lines`.
                // The in-flight CAPTURE, however, does not survive: its
                // verdict was read from the map that has just been replaced
                // (`ShortcutsState::refresh`).
                if let Some(sc) = &mut app.shortcuts {
                    sc.refresh(shortcut_rows(&Maps {
                        browse: &browse,
                        viewer: &viewer,
                        dialog: &dialog,
                    }));
                }
                *resolver = Resolver::new(browse);
                *viewer_resolver = Resolver::new(viewer);
                *dialog_resolver = Resolver::new(dialog);
                // K3a: and the which-key panel goes with the bar — its rows
                // came from the effective keymap that has just been replaced,
                // so a surviving panel would show keys that no longer exist.
                app.clear_pending();
                app.message = Some(t("msg-config-reloaded"));
                // The theme is also hot-reloadable (ADR 0020): if it fails,
                // the theme's error message overrides "config reloaded".
                apply_theme(app, &cfg);
                // LAST: the security warning must not end up overridden.
                if discarded_lua > 0 {
                    app.message = Some(ta(
                        "msg-lua-keymap-project",
                        &[("n", &discarded_lua.to_string())],
                    ));
                }
                // S3: the settings overlay, if open, is REFRESHED (not closed
                // like `help`/`palette` above) — its rows are just
                // `(name, description, value)` read from `cfg`, safe to
                // recompute without discarding the user's current
                // filter/edit (`Settings::refresh`).
                if let Some(settings) = &mut app.settings {
                    let summaries = plugin_config_summaries(backend).await;
                    settings.refresh(crate::settings::build_rows(&cfg, &summaries));
                }
                *cfg_out = cfg;
                true
            }
            Err(e) => {
                app.message = Some(ta(
                    "msg-config-not-applied",
                    &[("error", &keymaps_error_category(&e))],
                ));
                false
            }
        },
        Err(e) => {
            app.message = Some(ta(
                "msg-config-not-applied",
                &[("error", &config_error_category(&e))],
            ));
            false
        }
    }
}
