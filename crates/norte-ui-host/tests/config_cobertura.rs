//! What the WINDOW does with each configuration key (ADR 0097, decision 2).
//!
//! The terminal already had this guard — `App::desde_config`, in
//! `norte-tui/src/app/profile.rs`, destructures `CommonConfig` with no `..`
//! — and the window did not. Six of the findings from the 2026-09-05 parity
//! audit are keys that should have been classified here and instead went
//! unread without anyone noticing, among them `openers.toml`, which is a
//! whole documented feature.
//!
//! **No `..`, on purpose.** A new field makes this fail to COMPILE, which is
//! stronger than an assert: it forces a decision on what the window does
//! with the key right when someone is adding it, instead of letting "nobody
//! reads it" and "it is read at startup" be indistinguishable.
//!
//! Lives in `norte-ui-host` and not in `norte-gui-tauri` so the usual gate
//! runs it (`just t` / `just ci-fast`): a new key is added in `norte-config`,
//! which does not touch the window, so a guard that only ran in `gui-ci`
//! would find out late. Some of the keys are read by the shell
//! (`norte-gui-tauri/src/startup.rs`) and not the host; each group says so.
//!
//! **This does NOT assert that what is classified is right.** It says it has
//! been decided. The ones decided today as "nobody reads it" are named debt,
//! and the plan `docs/superpowers/plans/2026-09-05-paridad-tui-ventana.md`
//! lists them.

/// Every `CommonConfig` key is classified by what the window does with it.
#[test]
fn every_config_key_is_classified_for_the_window() {
    let c = norte_config::load(&norte_config::Layers { dirs: Vec::new() }).expect("empty config");
    let norte_config::CommonConfig {
        // ─── The shell reads these at startup and they travel in
        //     `UiHostOptions`. `norte-gui-tauri/src/startup.rs`.
        preset: _,
        ui_lang: _,
        ui_layout: _,
        log:
            norte_config::LogSettings {
                dir: _,
                retain: _,
                format: _,
            },
        daemon:
            norte_config::DaemonSettings {
                socket: _,
                // From the TERMINAL: the window ALWAYS talks to a daemon
                // (ADR 0066, D10), so there is no mode to choose.
                mode: _,
            },

        // ─── The host reads these from `self.config`, live against its own
        //     state (a new slot re-reads them).
        quick_search: _,
        ui_show_hidden: _,
        ui_parent_entry: _,
        ui_menu_bar: _,
        ui_panel_bar: _,
        ui_columns: _,
        hotlist: _,
        ui_diff: _,
        ui_diff_detached: _,
        // The chrome (spec 2026-09-10): the key bar, the panel bar's style,
        // the panel's footer, the date format, notice expiry and the dialog
        // buttons. Every snapshot reads them.
        ui_chrome: _,
        // The bar's plugin items (ADR 0137): every snapshot reads them
        // (`elementos_de_estado`) and every listing round asks for their
        // columns (`columns::plugin_requests`).
        ui_status_plugins: _,

        // ─── Read by the shell at STARTUP with the shared resolver, so it
        //     accepts a preset or the path to a `.toml` (ADR 0020).
        //
        //     There is still half a debt, and it has a name: on PROFILE
        //     CHANGE the host only applies presets (`aplicar_tema`), because
        //     resolving a path requires reading a file and that runs inside
        //     the actor (rule 2). Plan, phase 3.
        ui_theme: _,
        //     The desktop-scheme variants (spec 2026-09-11, V6): the shell
        //     resolves them at startup and they travel in the catalogue
        //     already as variables; the renderer picks by
        //     `prefers-color-scheme`.
        ui_theme_light: _,
        ui_theme_dark: _,

        // ─── The host reads these to launch a program: `openers.toml`
        //     rules `pane.open` and `[ui] editor` rules `pane.edit`, with the
        //     desktop handler as the last resort in both.
        //
        //     What is left OUT is `$EDITOR`, and it is deliberate (#290): it
        //     is a terminal editor and this window has no terminal to put it
        //     in.
        ui_editor: _,
        ui_editor_detached: _,

        // ─── The host reads this when asked to quit
        //     (`UiAction::RequestQuit`): the decision on whether to ask is
        //     shared (`settings::quit_needs_confirm`), and "work is pending"
        //     here means some task is still alive.
        ui_confirm_quit: _,

        // ─── From the TERMINAL, and for a reason.
        //
        //     `ui_mouse`: turning the mouse on is a terminal emulator's
        //     decision; a window always has one. (`daemon.mode` is also from
        //     here; it is above, with the rest of `daemon`.)
        ui_mouse: _,
        //     `ui_alt_menu`: a window ALWAYS has Alt on its own; the key
        //     exists because in a terminal it costs a keyboard protocol that
        //     eats a dead key's accents.
        ui_alt_menu: _,

        // ─── From the DAEMON: applied by the process that serves, not the
        //     one that paints. They arrive over the socket already in
        //     effect.
        archive:
            norte_config::ArchiveSettings {
                max_entries: _,
                max_decompressed_bytes: _,
                max_nesting: _,
                rar_delegate: _,
            },
        ai: _,

        // ─── From the startup CATALOGUE, not the snapshot: all four cross
        //     over in `HostCatalog::appearance` and the renderer plugs them
        //     in as CSS variables. The size also moves the grid — this
        //     window is laid out in cells — and `reduce_motion` can only ADD
        //     to the desktop's request, never contradict it (spec §17).
        //
        //     A terminal does not choose its font nor animate anything, so
        //     in the terminal these still go unapplied and that is stated in
        //     its exclusion list.
        ui_font: _,
        ui_mono_font: _,
        ui_font_size: _,
        ui_reduce_motion: _,

        // ─── Read at STARTUP and on profile change, not in the snapshot:
        //     `Estado::siembra_de_perfil` fills in the slot the session
        //     knows nothing about, once (ADR 0098).
        profile_start: _,

        // ─── LOAD diagnostics, not settings.
        //
        //     `project_warnings` and `profile_warnings` are both shown, and
        //     by the same path: the count to the bar from
        //     `aviso_de_arranque` and each reason to the log. The profile one
        //     is also repeated on every profile change.
        //     `sources` is for the config watcher, which the window does not
        //     have: its "where does this live" view is built from `capas`.
        //     `profile_title` is re-read by the file's profile picker, not
        //     from this field.
        sources: _,
        project_warnings: _,
        profile_warnings: _,
        profile_title: _,
    } = c;
}

/// And what is NOT `CommonConfig`, too.
///
/// The guard above only destructured the scalars, and `openers.toml` — a
/// whole documented feature — is not one: it lives in `FrontendConfig`. In
/// other words the guard itself had the gap the thing it came to watch for
/// had slipped through. Closed here.
#[test]
fn every_frontend_config_field_is_classified_for_the_window() {
    let cfg = norte_ui_host::ajustes_por_defecto();
    let norte_frontend::config::FrontendConfig {
        // The scalars, with their own guard above.
        common: _,

        // ─── Read by the shell at startup (`startup.rs::keymaps`) and they
        //     travel merged in `UiHostOptions`; the shortcut editor looks at
        //     them again to know which layer to write to.
        keymap_layers: _,
        keymap_layer_kinds: _,
        keymap_layer_dirs: _,

        // ─── The host reads it: `pane.open` resolves by mimetype before
        //     falling back to the desktop handler.
        openers: _,

        // ─── The host reads it: `pane.quick-search` starts in whichever
        //     mode the key says, same as the terminal.
        quick_search_mode: _,

        // ─── The host reads it: the theme picker, the wizard and the
        //     settings screen offer the user's themes, same as the terminal.
        user_themes: _,
    } = cfg;
}
