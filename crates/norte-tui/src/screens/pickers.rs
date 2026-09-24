//! The five cursor-list pickers: theme, connections, layout and columns,
//! plus the `[ui].theme` that the first one applies.
//!
//! The four are the same thing with different content — a cursor list that
//! resolves the key against the keymap's `dialog` context and filters it by
//! its overlay's ALLOWLIST — and all four used to live in the `ntc` binary's
//! root, a crate DISTINCT from this lib.
//!
//! There seemed to be a cycle with [`crate::screens::settings`] — the columns
//! picker "persists as `persist_setting`" and the settings overlay "opens the
//! theme one" — and there is not: the two references are mentions in
//! comments, not calls. A cycle between modules of the SAME crate would be
//! legal in Rust anyway, so the two files went out in a single commit.
//!
//! The rustdoc of [`on_layout_picker_key`] was STACKED on top of
//! `on_connections_picker_key` in `main.rs`, two doc comments in a row in
//! front of a single function: an earlier move left the other one's
//! documentation behind. Here it goes back to its own, without a word
//! changed.

use crossterm::event::{KeyCode, KeyModifiers};
use norte_i18n::{t, ta};

use crate::app::{
    ALLOW_COLUMNS, ALLOW_PICKER, App, PickerAction, detail_for_bar, io_error_category,
    theme_error_category,
};
use crate::config;
use crate::keymap::{Resolution, Resolver, chord_from_crossterm};

/// Resolves `[ui].theme` (preset or path) and applies it to the `App`; on
/// error it degrades to the default preset and warns (ADR 0020). The
/// frontend never blows up over a bad theme.
pub fn apply_theme(app: &mut App, cfg: &config::LoadedConfig) {
    let depth = crate::theme::detect_depth();
    match crate::theme::resolve(cfg.common.ui_theme.as_deref(), depth) {
        Ok(theme) => app.theme = theme,
        Err(e) => {
            app.theme = crate::theme::TuiTheme::default();
            // By Fluent category (#73): never the OS's Display nor the raw
            // diagnostic (the spec can come from someone else's `./.norte`).
            app.message = Some(theme_error_category(&e));
        }
    }
}

/// Translates the theme popup's keys into a domain action (the logic lives
/// in `App`, testable) by resolving against the keymap's `dialog` context
/// (H1 T2, issue #24 — rebindable). `ctrl+c` keeps its global quit,
/// hardcoded BEFORE resolving, like the other overlays. `F9` closes the
/// picker as a SPECIFIC shortcut of this overlay (it is not a `dialog.*`
/// binding of the preset): it stays hardcoded. On confirm, it PERSISTS the
/// choice to the user's `norte.toml` (ADR 0020), without blocking the
/// runtime.
pub async fn on_theme_picker_key(
    app: &mut App,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    if mods.is_empty() && code == KeyCode::F(9) {
        app.theme_picker_input(PickerAction::Cancel);
        return;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return; // key not modeled by the keymap: ignore
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        // No sequence semantics defined for overlays (T2), and likewise for
        // a key bound to something this build does not run (K1 T4): ignore
        // and reset the resolution state.
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return;
        }
        Resolution::Reset => return,
    };
    // H1 T3: the SAME allowlist the generated hint consumes
    // (`hints::DialogHints::build`) — a single source for dispatch and
    // footer. The match stays exhaustive for defense in depth.
    if !ALLOW_PICKER.contains(&cmd.as_str()) {
        return; // outside this overlay's allowlist: inert
    }
    let action = match cmd.as_str() {
        "dialog.up" => PickerAction::Up,
        "dialog.down" => PickerAction::Down,
        "dialog.confirm" => PickerAction::Confirm,
        "dialog.cancel" => PickerAction::Cancel,
        _ => return, // already filtered by ALLOW_PICKER; unreachable in practice
    };
    // The name to persist is taken BEFORE Confirm closes the popup.
    let confirmed = (action == PickerAction::Confirm)
        .then(|| {
            app.theme_picker
                .as_ref()
                .and_then(|p| p.selected().map(String::from))
        })
        .flatten();
    app.theme_picker_input(action);
    if let Some(name) = confirmed {
        // I/O in spawn_blocking: the runtime never blocks (rule 2).
        //
        // To the active PROFILE's directory if there is one, and not always
        // to the user's: the profile sits above it, so a theme written below
        // ends up COVERED by whatever the profile sets. It was saving, the
        // bar said "config reloaded", and the screen did not change color
        // (ADR 0079).
        let n = name.clone();
        let Some(dir) = app.config_write_dir() else {
            app.message = Some(t("msg-settings-no-config-dir"));
            return;
        };
        match tokio::task::spawn_blocking(move || config::persist_ui_theme_to(&dir, &n)).await {
            Ok(Ok(path)) => {
                // The path derives from XDG_CONFIG_HOME/APPDATA (environment):
                // sanitized like any other detail (#73).
                app.message = Some(ta(
                    "msg-theme-saved",
                    &[
                        ("name", &name),
                        ("path", &detail_for_bar(&path.display().to_string())),
                    ],
                ));
            }
            Ok(Err(e)) => {
                // The theme was ALREADY applied (session); it just could not
                // be saved. Only the CATEGORY goes to the bar, never the
                // OS's Display (#73).
                app.message = Some(ta(
                    "msg-theme-save-failed",
                    &[("error", &io_error_category(&e))],
                ));
            }
            // A panic in the write is our bug: it must not take the TUI down.
            Err(_) => {}
        }
    }
}

/// Keys for the connections picker (#140): same layout and same allowlist as
/// the layout one. Returns the chosen URL, if confirmed.
pub fn on_connections_picker_key(
    app: &mut App,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) -> Option<String> {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return None;
    }
    let chord = chord_from_crossterm(mods, code)?;
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return None;
        }
        Resolution::Reset => return None,
    };
    if !ALLOW_PICKER.contains(&cmd.as_str()) {
        return None;
    }
    let action = match cmd.as_str() {
        "dialog.up" => PickerAction::Up,
        "dialog.down" => PickerAction::Down,
        "dialog.confirm" => PickerAction::Confirm,
        "dialog.cancel" => PickerAction::Cancel,
        _ => return None,
    };
    app.connections_picker_input(action)
}

/// Keys for the layout picker: resolves through the keymap (`dialog`
/// screen) and filters by [`ALLOW_PICKER`], the SAME allowlist as the theme
/// picker — both are a cursor list that mutates nothing outside itself, so
/// Enter does fire. `ctrl+c` keeps its global quit, hardcoded before
/// resolving, and `F9` closes it like the theme one.
///
/// It is not `async` and persists nothing: choosing a layout only holds for
/// this session, and what pins it across launches is `[ui] layout` in your
/// config. Saving it on the fly would turn a trial into a permanent change.
pub fn on_layout_picker_key(
    app: &mut App,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    if mods.is_empty() && code == KeyCode::F(9) {
        app.layout_picker = None;
        return;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return; // key not modeled by the keymap: ignore
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return;
        }
        Resolution::Reset => return,
    };
    if !ALLOW_PICKER.contains(&cmd.as_str()) {
        return; // outside this overlay's allowlist: inert
    }
    let action = match cmd.as_str() {
        "dialog.up" => PickerAction::Up,
        "dialog.down" => PickerAction::Down,
        "dialog.confirm" => PickerAction::Confirm,
        "dialog.cancel" => PickerAction::Cancel,
        _ => return, // already filtered by ALLOW_PICKER; unreachable in practice
    };
    app.layout_picker_input(action);
}

/// Keys for the PROFILE picker (ADR 0079). Same discipline as the layout
/// one: resolves through the keymap in the `dialog` screen and filters by
/// the same allowlist, with `ctrl+c` keeping its global quit before anything
/// else.
pub fn on_profile_picker_key(
    app: &mut App,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return; // key not modeled by the keymap: ignore
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return;
        }
        Resolution::Reset => return,
    };
    if !ALLOW_PICKER.contains(&cmd.as_str()) {
        return; // outside this overlay's allowlist: inert
    }
    let action = match cmd.as_str() {
        "dialog.up" => PickerAction::Up,
        "dialog.down" => PickerAction::Down,
        "dialog.confirm" => PickerAction::Confirm,
        "dialog.cancel" => PickerAction::Cancel,
        _ => return,
    };
    app.profile_picker_input(action);
}

/// Keys for the columns picker (#108 7a): resolves through the keymap
/// (`dialog` screen) and filters by [`ALLOW_COLUMNS`] — the same
/// single-source discipline as the rest of the overlays (#24). `ctrl+c`
/// keeps its global quit, hardcoded BEFORE resolving, like the other
/// overlays. Returns `true` if a confirm changed the painted attr set
/// (#117): the run loop then re-lists (same path as after a mutation).
pub async fn on_columns_key(
    app: &mut App,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) -> bool {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return false;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return false; // key not modeled by the keymap: ignore
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        // No sequence semantics defined for overlays (T2), and likewise for
        // a key bound to something this build does not run (K1 T4): ignore
        // and reset the resolution state.
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return false;
        }
        Resolution::Reset => return false,
    };
    if !ALLOW_COLUMNS.contains(&cmd.as_str()) {
        return false; // outside this overlay's allowlist: inert
    }
    let Some(p) = app.columns_picker.as_mut() else {
        return false;
    };
    match cmd.as_str() {
        "dialog.up" => p.up(),
        "dialog.down" => p.down(),
        "dialog.toggle-enabled" => p.toggle(),
        "dialog.move-up" => p.move_up(),
        "dialog.move-down" => p.move_down(),
        "dialog.sort" => p.sort_current(),
        "dialog.cycle-format" => p.cycle_format(),
        "dialog.cancel" => app.columns_picker = None,
        "dialog.confirm" => {
            let picked = p.finish();
            app.columns_picker = None;
            return apply_picked_columns(app, picked).await;
        }
        _ => {} // already filtered by ALLOW_COLUMNS; unreachable in practice
    }
    false
}

/// The CONFIGURED attr ids of each visible pane (#117): the fingerprint that
/// decides whether a columns change forces a re-list — attr values only
/// arrive by asking for them in `fs.list`, so a new id with the old listing
/// would paint blank (absence) until the next cd. The ordered fingerprint
/// lives in the model (a single definition for both frontends).
#[must_use]
pub fn pane_attr_ids(app: &App) -> Vec<Vec<String>> {
    // #117-follow-up (review MAJOR-1): COMBINED attr+plugin fingerprint, a
    // single definition in the model (`pane_fingerprint`) for both
    // frontends — a change to plugins ALONE also re-lists (the re-list
    // respawns the value fetch; without it the new column would stay blank).
    app.panes
        .iter()
        .map(|p| app.columns.pane_fingerprint(p.dir().scheme()))
        .collect()
}

/// Applies the picker's result (#108 7a): session first (in-memory settings
/// + re-sort of EVERY pane, `apply_scheme_sort` is a no-op where the spec
/// does not change), disk after (`config::persist_columns` in
/// `spawn_blocking` — rule 2). Only the error's CATEGORY goes to the bar,
/// never the OS's Display (#73). Returns `true` if the painted attr set of
/// some visible pane changed (#117): the caller then re-lists through the
/// same path as after a mutation.
async fn apply_picked_columns(
    app: &mut App,
    picked: norte_frontend::columns_picker::Picked,
) -> bool {
    let attrs_before = pane_attr_ids(app);
    app.columns.apply_picked(
        picked.scheme_target.as_deref(),
        &picked.ids,
        picked.sort.clone(),
    );
    // #108 7b: cycled formats also go IN SESSION before disk — same
    // lockstep (`apply_format` touches the retained spec that `style_for`
    // reads).
    for (id, fmt) in &picked.formats {
        app.columns.apply_format(id, fmt);
    }
    for i in 0..app.panes.len() {
        app.apply_scheme_sort(i);
    }
    let needs_refresh = pane_attr_ids(app) != attrs_before;
    // To the active PROFILE if there is one: a profile sits above the
    // user's layer, so writing there what the profile also sets leaves it
    // covered — saved and with no effect (ADR 0079).
    let Some(dir) = app.config_write_dir() else {
        app.message = Some(t("msg-settings-no-config-dir"));
        return needs_refresh;
    };
    let ids = picked.ids.clone();
    let scheme = picked.scheme_target.clone();
    let sort = picked.sort.clone();
    // Checked NOW: `sort` moves into the background thread, and the outcome
    // needs to know whether it saved the order or left it for the session.
    let session_only_order = matches!(sort.column, norte_frontend::SortColumn::Attr(_));
    let formats = picked.formats.clone();
    let res = tokio::task::spawn_blocking(move || {
        // All writes in ONE background task, sequential over the same file
        // (#108 7b): the listing+sort and then each cycled format — one
        // outcome, one toast.
        config::persist_columns(
            &dir,
            scheme.as_deref(),
            &ids,
            // An order by ATTRIBUTE has no shape in `norte.toml` (`[ui] sort`
            // only names built-ins, ADR 0144): it stays in the session and
            // the file's `sort` key is left untouched. The outcome below
            // gives the notice.
            match sort.column {
                norte_frontend::SortColumn::Name => Some("name"),
                norte_frontend::SortColumn::Size => Some("size"),
                norte_frontend::SortColumn::Mtime => Some("mtime"),
                norte_frontend::SortColumn::Extension => Some("extension"),
                norte_frontend::SortColumn::Attr(_) => None,
            }
            .map(|column| config::PersistSort {
                column,
                descending: sort.dir == norte_frontend::SortDir::Desc,
                dirs_first: sort.dirs_first,
            }),
        )?;
        for (id, fmt) in &formats {
            config::persist_column_format(&dir, id, fmt)?;
        }
        Ok::<_, std::io::Error>(())
    })
    .await;
    match res {
        // If the order was by attribute, say so: a bare "saved" would give
        // the impression that reopening will still be sorted this way.
        Ok(Ok(())) => {
            app.message = Some(t(if session_only_order {
                "msg-columns-sort-session-only"
            } else {
                "msg-columns-saved"
            }));
        }
        Ok(Err(e)) => {
            app.message = Some(ta(
                "msg-settings-save-failed",
                &[("error", &io_error_category(&e))],
            ));
        }
        // A panic in the write is our bug: it must not take the TUI down
        // (same discipline as `persist_setting`) — it is announced and
        // leaves a trace.
        Err(e) => {
            tracing::error!(error = %e, "background persist_columns task did not finish");
            app.message = Some(t("msg-settings-save-crashed"));
        }
    }
    needs_refresh
}
