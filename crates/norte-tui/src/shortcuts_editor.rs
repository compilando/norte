//! The shortcuts editor: what can be bound, to what, and what gets written
//! to disk.
//!
//! It used to live in the `ntc` binary's root — a crate DISTINCT from this
//! lib — with its 419 lines of test inside `main.rs`. And it could not leave
//! before [`crate::paste::route_paste`]: those tests check that a paste over
//! the field capturing a chord does not go in as text, so the editor and the
//! paste router leave in that order and no other.
//!
//! [`Maps`] is the trio of LIVE maps. The editor reads its rows and every
//! verdict off them, never off a copy taken when it opened: a hot reload
//! replaces all three, and a verdict read from a stale map is a verdict
//! about somebody else's keyboard.

use crossterm::event::{KeyCode, KeyModifiers};
use norte_i18n::{t, ta};

use crate::app::{App, KeymapsError, PAGE, Shortcuts, io_error_category};
use crate::config;
use crate::keymap::{
    COMMANDS, DIALOG_COMMANDS, Effective, RebindWrite, Screen, UnbindWrite, chord_from_crossterm,
    presets,
};

/// The three LIVE effective maps, borrowed from the resolvers that own them.
///
/// The shortcut editor reads its rows and every verdict off these, never off a
/// copy taken when it opened: a hot reload replaces all three (`reload_config`)
/// and a verdict read from a stale map is a verdict about somebody else's
/// keyboard.
pub struct Maps<'a> {
    /// The browse screen's effective map.
    pub browse: &'a Effective,
    /// The viewer's.
    pub viewer: &'a Effective,
    /// The dialogs'.
    pub dialog: &'a Effective,
}

impl Maps<'_> {
    /// The map of `screen` — the one a row of that screen was built from, and
    /// the one its verdict must be read off.
    fn of(&self, screen: Screen) -> &Effective {
        match screen {
            Screen::Browse => self.browse,
            Screen::Viewer => self.viewer,
            Screen::Dialog => self.dialog,
        }
    }
}

/// The command set a keymap for `screen` is VALIDATED against — what
/// [`build_keymaps`] passes to `Effective::build_for`, and what the shortcut
/// editor's dry run must pass too.
///
/// Wider than what the screen dispatches, deliberately, and only for
/// [`Screen::Dialog`]: that map merges `[global]` (ADR 0006/H1 T1), so a
/// binding like `ctrl+c → app.quit` would validate as `UnknownCommand` against
/// the dialog verbs alone and take the WHOLE layer down with it. The editor's
/// dry run runs the real loader, so it needs the real set — the narrower "what
/// may be bound here" question is `bindable_commands` (privada)'s.
///
/// Carries [`norte_frontend::keymap::LUA_HOST`] on every screen: this frontend
/// runs `lua:` bindings, and without the marker they would resolve as not
/// available here (ADR 0110).
#[must_use]
pub fn known_commands(screen: Screen) -> Vec<&'static str> {
    let lua = std::iter::once(norte_frontend::keymap::LUA_HOST);
    match screen {
        Screen::Dialog => COMMANDS
            .iter()
            .copied()
            .chain(DIALOG_COMMANDS.iter().copied())
            .chain(lua)
            .collect(),
        Screen::Browse | Screen::Viewer => COMMANDS.iter().copied().chain(lua).collect(),
    }
}

/// The commands the shortcut editor offers as UNBOUND rows for `screen` — what
/// this frontend actually dispatches there.
///
/// Not [`known_commands`], and the difference is the point of the list: that
/// set answers "would the layer load", this one answers "will the key do
/// something". The browse screen dispatches everything; the viewer owns the
/// keyboard while it is open and dispatches its own verbs plus the `app.*` ones
/// that reach it through `[global]` (the palette opens from the viewer); an
/// overlay dispatches its `dialog.*` allowlist. Offering `pane.copy` as a
/// bindable viewer command would answer "how do I press X" with a key that does
/// nothing there.
fn bindable_commands(screen: Screen) -> Vec<&'static str> {
    match screen {
        Screen::Viewer => COMMANDS
            .iter()
            .copied()
            .filter(|c| c.starts_with("viewer.") || c.starts_with("app."))
            .collect(),
        Screen::Dialog => DIALOG_COMMANDS.to_vec(),
        Screen::Browse => COMMANDS.to_vec(),
    }
}

/// The editor's rows for the three screens, in the order the help page uses
/// (browse, viewer, dialog) — rebuilt whole, never patched row by row, exactly
/// like `help_lines` and the palette's rows.
pub fn shortcut_rows(maps: &Maps<'_>) -> Vec<norte_frontend::shortcuts::ShortcutRow> {
    let lang = norte_i18n::active();
    let bindable: Vec<Vec<&'static str>> = [Screen::Browse, Screen::Viewer, Screen::Dialog]
        .into_iter()
        .map(bindable_commands)
        .collect();
    let screens: Vec<norte_frontend::shortcuts::ScreenKeys<'_>> =
        [Screen::Browse, Screen::Viewer, Screen::Dialog]
            .into_iter()
            .zip(&bindable)
            .map(|(screen, bindable)| norte_frontend::shortcuts::ScreenKeys {
                screen,
                eff: maps.of(screen),
                bindable,
            })
            .collect();
    norte_frontend::shortcuts::build_rows(&screens, lang)
}

/// The gate, as called by THIS frontend: the active preset's name, the
/// loaded layers and the set that screen is validated against.
///
/// The gate itself lives in `norte_frontend::shortcuts::plan_rebind` — the
/// layer cut (`RebindSources::split_at`) is not the frontend's, and a GUI
/// that redid it by hand is exactly the reader its documentation warns will
/// get it silently wrong. What is left here is only what belongs to the
/// TUI: where the preset name comes from and which commands each screen
/// validates.
/// Where a shortcut's write lands.
///
/// From the SAME cut that decides the destination, not from
/// `user_config_dir()` separately: D10 moved the destination to the active
/// profile's `keymap.toml` and this writer kept resolving the user's
/// directory on its own, so the gate planned against one file and the write
/// landed in another, where the profile shadowed it — "visibly saved, and
/// doing nothing" (#305).
///
/// When the cut points at no layer the write CREATES a file, and it is then
/// created in the active profile's, if there is one: rebinding a key inside
/// a workspace means that key in that workspace, whether the profile
/// already carries it or not.
fn rebind_dir(
    app: &App,
    cfg: &config::LoadedConfig,
    cli_preset: Option<&str>,
    screen: Screen,
) -> Option<std::path::PathBuf> {
    let preset_name = cli_preset.unwrap_or(&cfg.common.preset);
    let known = known_commands(screen);
    let idx = norte_frontend::shortcuts::rebind_target_index(
        preset_name,
        &cfg.keymap_layer_kinds,
        &cfg.keymap_layers,
        &known,
        screen,
    );
    if let Some(i) = idx
        && let Some(dir) = cfg.keymap_layer_dirs.get(i)
    {
        return Some(dir.clone());
    }
    match &app.active_profile {
        Some(name) => norte_config::profile_dir_from(&|k| std::env::var_os(k), name),
        None => config::user_config_dir(),
    }
}

fn plan_rebind(
    cfg: &config::LoadedConfig,
    cli_preset: Option<&str>,
    screen: Screen,
    seq: &[crate::keymap::Chord],
    command: &str,
) -> Result<RebindWrite, norte_frontend::shortcuts::PlanError> {
    let preset_name = cli_preset.unwrap_or(&cfg.common.preset);
    let known = known_commands(screen);
    norte_frontend::shortcuts::plan_rebind(
        preset_name,
        &cfg.keymap_layer_kinds,
        &cfg.keymap_layers,
        &known,
        screen,
        seq,
        command,
    )
}

/// The unbind's own door call — same cut, same preset lookup as
/// [`plan_rebind`], for the removal instead of the write. See that
/// function's doc for why the split is not this frontend's to redo.
fn plan_unbind(
    cfg: &config::LoadedConfig,
    cli_preset: Option<&str>,
    screen: Screen,
    seq: &[crate::keymap::Chord],
) -> Result<UnbindWrite, norte_frontend::shortcuts::PlanError> {
    let preset_name = cli_preset.unwrap_or(&cfg.common.preset);
    let known = known_commands(screen);
    norte_frontend::shortcuts::plan_unbind(
        preset_name,
        &cfg.keymap_layer_kinds,
        &cfg.keymap_layers,
        &known,
        screen,
        seq,
    )
}

/// Shortcuts editor keys (K3c, `app.shortcuts`): same approach as the
/// settings overlay above — fixed keys, hardcoded here.
///
/// CAPTURE mode is what did not fit as one more arm of `on_settings_key`:
/// while it is active EVERY key is the chord being captured, not a screen
/// shortcut. Only `Esc` stays out, because it is what cancels — and that is
/// why it is the one chord this editor cannot capture, something the screen
/// SAYS instead of leaving the reader to press it.
///
/// `Enter` CAN be captured: in the waiting phase it is a key like any other,
/// and only confirms AFTERWARD, with a verdict already on screen.
pub async fn on_shortcuts_key(
    app: &mut App,
    cfg: &config::LoadedConfig,
    cli_preset: Option<&str>,
    maps: &Maps<'_>,
    mods: KeyModifiers,
    code: KeyCode,
) {
    // The global emergency exit, EXCEPT while capturing: `ctrl+c` is a chord
    // a CUA convert wants to bind (it is their "copy"), and in capture mode
    // the reader is pressing keys blind by design — quitting norte there
    // would be the worst possible reading of a key the editor asked for.
    // With capture open the exit is `esc`, which is what the screen says.
    let capturing = app.shortcuts.as_ref().is_some_and(Shortcuts::is_capturing);
    if !capturing && mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    let Some(sc) = &mut app.shortcuts else {
        return;
    };
    match shortcuts_key(sc, maps, mods, code) {
        ShortcutsKeyOutcome::None => {}
        ShortcutsKeyOutcome::Close => app.shortcuts = None,
        ShortcutsKeyOutcome::Confirm => confirm_shortcut(app, cfg, cli_preset).await,
        ShortcutsKeyOutcome::Unbind => unbind_shortcut(app, cfg, cli_preset).await,
        ShortcutsKeyOutcome::NotBindable => app.message = Some(t("msg-shortcut-not-bindable")),
        ShortcutsKeyOutcome::RowIsGlobal => app.message = Some(t("shortcuts-row-global")),
    }
}

/// What a shortcuts editor key requested — the PURE computation, inside the
/// `app.shortcuts` borrow, separated from the async I/O the same way
/// [`SettingsKeyOutcome`] separates it in the settings overlay.
enum ShortcutsKeyOutcome {
    /// Consumed with nothing pending (navigation, filter, capture).
    None,
    /// `Esc` outside capture: closes the screen.
    Close,
    /// Write the capture, if the gate lets it through.
    Confirm,
    /// Remove the binding on the row under the cursor.
    Unbind,
    /// The captured key is not modeled by the keymap (Media, `CapsLock`…):
    /// there is no chord to capture, and faking one would bind something
    /// else.
    NotBindable,
    /// The row under the cursor is `[global]`'s (#141): neither rebind nor
    /// unbind can write there from a row that names a single screen.
    RowIsGlobal,
}

/// The editor's state after a key. Pure and synchronous: this is where the
/// capture rule lives, and it is what tests can drive with no runtime.
fn shortcuts_key(
    sc: &mut Shortcuts,
    maps: &Maps<'_>,
    mods: KeyModifiers,
    code: KeyCode,
) -> ShortcutsKeyOutcome {
    let plain = mods.is_empty() || mods == KeyModifiers::SHIFT;
    if let Some(capture) = sc.capture() {
        let waiting = capture.is_waiting();
        let screen = capture.screen();
        match code {
            // ALWAYS cancels, in both phases, which is why `esc` is the one
            // chord that cannot be captured. BARE `esc`: the screen's
            // contract is "esc cancels", not "anything ending in esc", so
            // `shift+esc` and `alt+esc` are still bindable chords.
            KeyCode::Esc if plain => sc.cancel_capture(),
            KeyCode::Enter if !waiting => return ShortcutsKeyOutcome::Confirm,
            KeyCode::Backspace if !waiting => sc.recapture(),
            // A dangerous codepoint does not come from a key: it comes from
            // a PASTE. Since #143 the PRIMARY defense is `route_paste`,
            // which intercepts the whole `Event::Paste` BEFORE it reaches
            // here (while `waiting`, it rejects it whole — a capture answers
            // to ONE physical key, never a paste). This arm stays alive as a
            // BACKUP: a terminal or multiplexer that does not honor
            // `\e[?2004h` still delivers the paste as loose `Char`s, one by
            // one, and without this guard `parse_chord` would accept it and
            // the writer would leave it raw in the user's `keymap.toml` — a
            // file no norte screen paints raw, but their text editor does.
            // `a_pasted_dangerous_codepoint_is_not_captured` tests THIS
            // branch directly (via `Event::Key`), independent of
            // `route_paste`, so a regression in the primary defense does not
            // also leave the backup uncovered.
            _ if waiting && hostile_key(code) => return ShortcutsKeyOutcome::NotBindable,
            _ if waiting => match chord_from_crossterm(mods, code) {
                // The map is THE ROW's (`maps.of`), not the screen the
                // reader was looking at: `Tab` is free in the viewer and
                // reserved in the browser, and the verdict has to talk about
                // the keyboard that is about to be edited.
                Some(chord) => sc.capture_chord(chord, maps.of(screen)),
                None => return ShortcutsKeyOutcome::NotBindable,
            },
            // With a verdict on screen, the rest of the keys do nothing:
            // confirm, recapture or cancel are the three exits, and the
            // footer names them.
            _ => {}
        }
        return ShortcutsKeyOutcome::None;
    }
    match code {
        KeyCode::Char('u') if mods == KeyModifiers::CONTROL => return ShortcutsKeyOutcome::Unbind,
        KeyCode::Char(c) if plain => sc.push_char(c),
        KeyCode::Backspace if plain => sc.backspace(),
        KeyCode::Esc if plain => return ShortcutsKeyOutcome::Close,
        KeyCode::Up if plain => sc.up(),
        KeyCode::Down if plain => sc.down(),
        KeyCode::PageUp if plain => sc.page_up(PAGE),
        KeyCode::PageDown if plain => sc.page_down(PAGE),
        // `is_editable` false means [`ShortcutsState::begin_capture`] would
        // silently refuse anyway (#141) — checked here too so the reader
        // gets told WHY instead of nothing happening.
        KeyCode::Enter if plain && sc.selected().is_some_and(|r| !r.is_editable()) => {
            return ShortcutsKeyOutcome::RowIsGlobal;
        }
        KeyCode::Enter if plain => {
            sc.begin_capture();
        }
        _ => {}
    }
    ShortcutsKeyOutcome::None
}

/// Is this key a codepoint that must not end up raw in a configuration
/// file? Only reachable by paste — no physical key delivers an RLO — and
/// that is why it is REJECTED instead of masked: masking would bind a
/// different chord from the one the file would say.
fn hostile_key(code: KeyCode) -> bool {
    matches!(code, KeyCode::Char(c) if norte_encoding::is_terminal_hazard(c))
}

/// Confirms the capture: the gate ([`plan_rebind`]) and, only if it passes,
/// the writer — in `spawn_blocking` (rule 2: `persist_keymap_bind` takes a
/// file lock and does synchronous I/O, and this runs on the UI thread).
///
/// What reaches the writer is what the gate returned, AS IS: the section,
/// the list (`prepend_keymap` — an `append` does not shadow the preset and
/// would never fire), and the chords' spelling. Re-rendering the captured
/// sequence here would reopen exactly the hole the gate closes.
///
/// The written file is seen by `keymap.toml`'s watcher, which fires
/// `reload_config`: that is where the LIVE effect comes from, and that is
/// also where this screen's refresh comes from.
async fn confirm_shortcut(app: &mut App, cfg: &config::LoadedConfig, cli_preset: Option<&str>) {
    let captured = app.shortcuts.as_ref().and_then(|sc| {
        sc.confirmable()
            .map(|(screen, command, seq)| (screen, command.to_owned(), seq.to_vec()))
    });
    // `None` = a rejection verdict (or nothing captured): nothing is
    // written and the capture stays alive so the reader can try another
    // key — but the Enter they just pressed cannot stay mute: the verdict is
    // repeated in the bar, which is the reason it was not saved.
    let Some((screen, command, seq)) = captured else {
        if let Some(v) = app
            .shortcuts
            .as_ref()
            .and_then(norte_frontend::shortcuts::ShortcutsState::capture)
            .and_then(norte_frontend::shortcuts::Capture::verdict)
        {
            app.message = Some(norte_frontend::shortcuts::verdict_message(
                v,
                norte_i18n::active(),
            ));
        }
        return;
    };
    let Some(dir) = rebind_dir(app, cfg, cli_preset, screen) else {
        app.message = Some(t("msg-settings-no-config-dir"));
        return;
    };
    let write = match plan_rebind(cfg, cli_preset, screen, &seq, &command) {
        Ok(w) => w,
        Err(e) => {
            app.message = Some(norte_frontend::shortcuts::plan_error_message(
                &e,
                norte_i18n::active(),
            ));
            if let Some(sc) = &mut app.shortcuts {
                sc.cancel_capture();
            }
            return;
        }
    };
    let painted = crate::keymap::paint_chord(&write.chords.join(" "));
    let label = norte_frontend::whichkey::command_label(&command, norte_i18n::active());
    let res = tokio::task::spawn_blocking(move || {
        config::persist_keymap_bind(
            &dir,
            write.section,
            write.list,
            &write.chords,
            &write.command,
        )
    })
    .await;
    app.message = Some(match res {
        Ok(Ok(_)) => ta(
            "msg-shortcut-bound",
            &[("chord", &painted), ("command", &label)],
        ),
        Ok(Err(e)) => ta(
            "msg-settings-save-failed",
            &[("error", &io_error_category(&e))],
        ),
        // A panic in the write is our bug: it must not take down the TUI
        // (same discipline as `persist_setting`).
        Err(e) => {
            tracing::error!(error = %e, "persist_keymap_bind's background task did not finish");
            t("msg-settings-save-crashed")
        }
    });
    if let Some(sc) = &mut app.shortcuts {
        sc.cancel_capture();
    }
}

/// Removes the binding on the row under the cursor — the reason c1 wrote
/// `persist_keymap_unbind`: an editor that only adds is an editor that fixes
/// no mistake.
///
/// Now goes through the same gate as the bind ([`plan_unbind`] /
/// `unbind_dry_run`, #141): it matches by PARSED sequence, not by bytes, so
/// a hand-written twin (`mod+p` for `ctrl+p`) is found and written with ITS
/// OWN spelling; and the message comes from the RECONSTRUCTED map — what the
/// key runs NOW — instead of "removed from your keymap.toml", which was
/// true and useless the moment another layer kept binding it. A `[global]`
/// row does not even reach the gate: it is reflected in the row itself
/// (`ShortcutRow::is_editable`) and rejected earlier, with its own message.
async fn unbind_shortcut(app: &mut App, cfg: &config::LoadedConfig, cli_preset: Option<&str>) {
    let Some(row) = app
        .shortcuts
        .as_ref()
        .and_then(norte_frontend::shortcuts::ShortcutsState::selected)
    else {
        return;
    };
    if !row.is_editable() {
        app.message = Some(t("shortcuts-row-global"));
        return;
    }
    if !row.is_bound() {
        app.message = Some(t("msg-shortcut-nothing-to-unbind"));
        return;
    }
    let screen = row.screen;
    let seq: Vec<crate::keymap::Chord> = row.seq.clone();
    let painted = row.chord.clone();
    let write = match plan_unbind(cfg, cli_preset, screen, &seq) {
        Ok(w) => w,
        Err(e) => {
            app.message = Some(norte_frontend::shortcuts::plan_error_message(
                &e,
                norte_i18n::active(),
            ));
            return;
        }
    };
    if matches!(write.outcome, crate::keymap::UnbindOutcome::NotBound) {
        // Nothing to write: the door itself already saw this layer did not
        // have the sequence (a row from another layer, or a stale read).
        app.message = Some(t("msg-shortcut-nothing-to-unbind"));
        return;
    }
    let Some(dir) = rebind_dir(app, cfg, cli_preset, screen) else {
        app.message = Some(t("msg-settings-no-config-dir"));
        return;
    };
    let section = write.section;
    let chords = write.chords.clone();
    let command = write.command.clone();
    let res = tokio::task::spawn_blocking(move || {
        config::persist_keymap_unbind(&dir, section, &chords, &command)
    })
    .await;
    app.message = Some(match res {
        // `w.changed` is the WRITER's truth (re-read under its lock) about
        // whether something was removed; `write.outcome` is the DOOR's,
        // read from the in-memory config before the `spawn_blocking`. If
        // the file changed in exactly that gap (another process, a manual
        // edit) `w.changed` is still true — no change that did not happen
        // is invented — but `outcome`'s TEXT can describe a map that is no
        // longer the one on disk: the same window `rebind_dry_run` already
        // documents for the bind (the writer takes the file's lock, this
        // does not).
        Ok(Ok(w)) if w.changed => norte_frontend::shortcuts::unbind_outcome_message(
            &write.outcome,
            &painted,
            norte_i18n::active(),
        ),
        Ok(Ok(_)) => t("msg-shortcut-nothing-to-unbind"),
        Ok(Err(e)) => ta(
            "msg-settings-save-failed",
            &[("error", &io_error_category(&e))],
        ),
        Err(e) => {
            tracing::error!(error = %e, "persist_keymap_unbind's background task did not finish");
            t("msg-settings-save-crashed")
        }
    });
}
/// Resolves the preset (flag > config > default) and folds the keymap
/// layers (ADR 0007) for the THREE screens (browse, viewer, dialog — H1
/// T2). The typed error ([`KeymapsError`], #73) lives in `crate::app`
/// next to its Fluent category.
///
/// # Errors
///
/// [`KeymapsError`] if the requested preset does not exist or a layer
/// cannot be folded. Typed and not `anyhow` on purpose: its Fluent category
/// lives next to the error, and the shortcuts editor runs a dry run with
/// this same function to decide whether a key can be bound — it needs the
/// reason, not a text.
pub fn build_keymaps(
    cfg: &config::LoadedConfig,
    cli_preset: Option<&str>,
) -> Result<(Effective, Effective, Effective), KeymapsError> {
    let preset_name = cli_preset.unwrap_or(&cfg.common.preset);
    let presets = presets();
    let (_, preset) = presets
        .iter()
        .find(|(n, _)| *n == preset_name)
        .ok_or_else(|| KeymapsError::UnknownPreset {
            name: preset_name.to_owned(),
            available: presets
                .iter()
                .map(|(n, _)| *n)
                .collect::<Vec<_>>()
                .join(", "),
        })?;
    let invalid = |e: crate::keymap::KeymapError| KeymapsError::Invalid {
        detail: e.to_string(),
    };
    let browse_known = known_commands(Screen::Browse);
    let browse = Effective::build_for(preset, &cfg.keymap_layers, &browse_known, Screen::Browse)
        .map_err(invalid)?;
    let viewer_known = known_commands(Screen::Viewer);
    let viewer = Effective::build_for(preset, &cfg.keymap_layers, &viewer_known, Screen::Viewer)
        .map_err(invalid)?;
    // Screen::Dialog merges `[dialog] ∪ [global]` (ADR 0006/H1 T1):
    // `build_for_impl` validates the WHOLE merged effective map against
    // `known_commands`, so a GLOBAL binding (e.g. `ctrl+c →
    // app.quit`) would validate as `UnknownCommand` if we only passed
    // `DIALOG_COMMANDS`. The UNION with `COMMANDS` is the simple option (T1
    // settled on it): harmless because each overlay ALLOWLISTS only its own
    // supported `dialog.*` (`app::dialog_action` and this module's ad hoc
    // resolutions) and drops any other resolved command.
    //
    // K3c: that union now lives in `known_commands`, because the shortcuts
    // editor's gate has to hand the loader EXACTLY the same set that was
    // handed to it here — with a narrower one, the rebind's dry run would
    // reject a global binding that loads perfectly fine.
    let dialog_known = known_commands(Screen::Dialog);
    let dialog = Effective::build_for(preset, &cfg.keymap_layers, &dialog_known, Screen::Dialog)
        .map_err(invalid)?;
    Ok((browse, viewer, dialog))
}

/// K3c: the shortcuts editor, driven through the same path as the keys —
/// [`shortcuts_key`] — and carried all the way to disk and back.
///
/// The test that matters is the full round trip: `reload_config` applies
/// ALL or NOTHING, so a write that produced an invalid layer would leave
/// the old map in place, the editor would say "saved" and the new key would
/// do nothing. That does not show up in any test that stops at the gate.
#[cfg(test)]
mod shortcuts_editor_tests {
    use super::{Maps, ShortcutsKeyOutcome, build_keymaps, plan_rebind, shortcut_rows};
    use crate::app::Pane;
    use crate::app::Shortcuts;
    use crate::config::{self, Layer, Layers};
    use crate::keymap::Screen;
    use crate::keymap::{Effective, parse_chord};
    use crate::overlays::close_stale_overlays;
    use crate::paste::route_paste;
    use crossterm::event::{KeyCode, KeyModifiers};
    use norte_frontend::shortcuts::PlanError;

    /// An empty config directory as the USER layer: a fresh install's first
    /// rebind, which is the case `split_at` can silently model wrong.
    fn config_in(dir: &std::path::Path) -> (Layers, config::LoadedConfig) {
        let layers = Layers {
            dirs: vec![(dir.to_path_buf(), Layer::User)],
        };
        let cfg = config::load(&layers).expect("an empty layer loads");
        (layers, cfg)
    }

    fn maps(cfg: &config::LoadedConfig) -> (Effective, Effective, Effective) {
        build_keymaps(cfg, None).expect("the active preset's three maps")
    }

    /// With an active profile carrying a `keymap.toml`, the shortcut is
    /// written to the PROFILE's file.
    ///
    /// D10 moved the destination there and this writer kept resolving the
    /// user's directory on its own: the gate planned against the profile's
    /// file and the write landed in the user's, where the profile shadows
    /// it — "visibly saved, and doing nothing" (#305).
    #[test]
    fn with_an_active_profile_the_shortcut_is_written_to_the_profiles_file() {
        let user = tempfile::tempdir().expect("tempdir");
        let profile = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            profile.path().join("keymap.toml"),
            "[pane]\nprepend_keymap = [{ on = [\"ctrl+j\"], run = \"cursor.top\" }]\n",
        )
        .expect("write");
        let layers = Layers {
            dirs: vec![
                (user.path().to_path_buf(), Layer::User),
                (profile.path().to_path_buf(), Layer::Profile),
            ],
        };
        let cfg = config::load(&layers).expect("load");
        let mut app = app_vacia();
        app.active_profile = Some(std::ffi::OsString::from("work"));

        assert_eq!(
            super::rebind_dir(&app, &cfg, None, Screen::Browse).as_deref(),
            Some(profile.path()),
            "the destination comes from the CUT, not from user_config_dir()"
        );
    }

    /// With no profile, it still goes to the user's: this must not change
    /// what already worked.
    #[test]
    fn with_no_profile_the_shortcut_still_goes_to_the_users() {
        let user = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            user.path().join("keymap.toml"),
            "[pane]\nprepend_keymap = [{ on = [\"ctrl+j\"], run = \"cursor.top\" }]\n",
        )
        .expect("write");
        let layers = Layers {
            dirs: vec![(user.path().to_path_buf(), Layer::User)],
        };
        let cfg = config::load(&layers).expect("load");
        let app = app_vacia();

        assert_eq!(
            super::rebind_dir(&app, &cfg, None, Screen::Browse).as_deref(),
            Some(user.path())
        );
    }

    fn chord(s: &str) -> crate::keymap::Chord {
        parse_chord(s).expect("chord")
    }

    fn app_vacia() -> super::App {
        let d = norte_proto::VPath::parse("file:///x").expect("test wire");
        super::App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
    }

    /// Places the cursor on `command`'s row in `screen` and returns the
    /// editor ready to capture.
    fn editor_at(
        browse: &Effective,
        viewer: &Effective,
        dialog: &Effective,
        screen: Screen,
        command: &str,
    ) -> Shortcuts {
        let rows = shortcut_rows(&Maps {
            browse,
            viewer,
            dialog,
        });
        let idx = rows
            .iter()
            .position(|r| r.screen == screen && r.command == command)
            .expect("the command's row exists");
        let mut sc = Shortcuts::new(rows);
        for _ in 0..idx {
            sc.down();
        }
        assert_eq!(
            sc.selected().map(|r| r.command.as_str()),
            Some(command),
            "the cursor is where the test thinks it is"
        );
        sc
    }

    /// THE whole path: capture, pass the gate, write, RELOAD the way the
    /// watcher does, and check that the key does something else.
    ///
    /// Over a key the PRESET already binds, which is where `append_keymap`
    /// would have loaded, validated and never fired: if the gate returned
    /// the wrong list, this test would still write a legal file and the
    /// last line would fail.
    #[test]
    fn a_confirmed_capture_is_written_loads_and_the_key_changes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (layers, cfg) = config_in(dir.path());
        let (browse, viewer, dialog) = maps(&cfg);
        let f5 = chord("f5");
        let before = browse
            .bindings_all_seq()
            .into_iter()
            .find(|(seq, _, _)| *seq == [f5])
            .map(|(_, run, _)| run.to_owned())
            .expect("the active preset binds F5");
        assert_ne!(before, "pane.mkdir", "otherwise the test proves nothing");

        let mut sc = editor_at(&browse, &viewer, &dialog, Screen::Browse, "pane.mkdir");
        let m = Maps {
            browse: &browse,
            viewer: &viewer,
            dialog: &dialog,
        };
        super::shortcuts_key(&mut sc, &m, KeyModifiers::NONE, KeyCode::Enter);
        assert!(sc.is_capturing());
        super::shortcuts_key(&mut sc, &m, KeyModifiers::NONE, KeyCode::F(5));
        let (screen, command, seq) = sc.confirmable().expect("F5 can be reassigned");
        let command = command.to_owned();
        let seq = seq.to_vec();

        let w = plan_rebind(&cfg, None, screen, &seq, &command).expect("the gate lets it through");
        assert_eq!(
            w.list,
            config::KeymapList::Prepend,
            "an append would not shadow the preset"
        );
        config::persist_keymap_bind(dir.path(), w.section, w.list, &w.chords, &w.command)
            .expect("the writer writes");

        // What the watcher does: reload the config and rebuild the maps.
        // `reload_config` is all-or-nothing, so a file that failed to load
        // would show up here as an `Err` — and in the TUI, as an intact old
        // map.
        let (_, cfg2) = config_in(dir.path());
        drop(layers);
        let (browse2, _, _) = maps(&cfg2);
        assert!(
            browse2.single_chord_runs(f5, "pane.mkdir"),
            "the new key does what the editor said"
        );

        // And unbinding returns it to the preset — the reason c1 wrote
        // `persist_keymap_unbind`: an editor that only adds fixes nothing.
        // The same arguments `unbind_shortcut` builds from the row.
        let row_seq: Vec<String> = seq.iter().map(ToString::to_string).collect();
        let removed = config::persist_keymap_unbind(dir.path(), w.section, &row_seq, &command)
            .expect("removes");
        assert!(removed.changed, "there was something to remove");
        let (_, cfg3) = config_in(dir.path());
        let (browse3, _, _) = maps(&cfg3);
        assert!(
            browse3.single_chord_runs(f5, &before),
            "without the user layer the preset rules again"
        );
    }

    /// A paste cannot bind a chord (#143): a capture answers ONE physical
    /// key, and a paste is never that — not even a one-character paste,
    /// which crossterm hands the router as `Event::Paste`, never as the
    /// `Event::Key` a keystroke would be. It gets the same outcome a
    /// hostile keystroke gets there (`hostile_key`, `msg-shortcut-not-
    /// bindable`), not a chord silently bound to whatever it pasted.
    #[test]
    fn a_paste_while_capturing_a_chord_is_rejected_not_bound() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_layers, cfg) = config_in(dir.path());
        let (browse, viewer, dialog) = maps(&cfg);
        let sc = editor_at(&browse, &viewer, &dialog, Screen::Browse, "pane.mkdir");
        let mut app = app_vacia();
        app.shortcuts = Some(sc);
        let m = Maps {
            browse: &browse,
            viewer: &viewer,
            dialog: &dialog,
        };
        super::shortcuts_key(
            app.shortcuts.as_mut().expect("open"),
            &m,
            KeyModifiers::NONE,
            KeyCode::Enter,
        );
        assert!(app.shortcuts.as_ref().expect("open").is_capturing());

        route_paste(&mut app, "p");

        assert!(
            app.shortcuts.as_ref().expect("still open").is_capturing(),
            "the capture must still be waiting — a paste cannot have satisfied it"
        );
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("msg-shortcut-not-bindable").as_str()),
            "same message a hostile keystroke gets there"
        );
    }

    /// A sacred key (§12) that gets captured is NOT confirmable — and the
    /// gate, if someone skipped it, would not let it through either. Both
    /// halves, because the capture's verdict is a convenience and the gate
    /// is the guarantee.
    #[test]
    fn a_sacred_key_is_not_confirmable_nor_does_it_pass_the_gate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_layers, cfg) = config_in(dir.path());
        let (browse, viewer, dialog) = maps(&cfg);
        let mut sc = editor_at(&browse, &viewer, &dialog, Screen::Browse, "pane.mkdir");
        let m = Maps {
            browse: &browse,
            viewer: &viewer,
            dialog: &dialog,
        };
        super::shortcuts_key(&mut sc, &m, KeyModifiers::NONE, KeyCode::Enter);
        super::shortcuts_key(&mut sc, &m, KeyModifiers::NONE, KeyCode::Tab);
        assert!(
            sc.capture().and_then(|c| c.verdict()).is_some(),
            "the verdict is seen BEFORE confirming"
        );
        assert!(sc.confirmable().is_none(), "Tab does not sell out");
        assert!(matches!(
            plan_rebind(&cfg, None, Screen::Browse, &[chord("tab")], "pane.mkdir"),
            Err(PlanError::Door(_))
        ));
        // And with a rejection verdict on screen, Enter does not ask to write.
        assert!(matches!(
            super::shortcuts_key(&mut sc, &m, KeyModifiers::NONE, KeyCode::Enter),
            ShortcutsKeyOutcome::Confirm
        ));
        assert!(
            sc.confirmable().is_none(),
            "and `confirm_shortcut` has nothing to write"
        );
    }

    /// `Esc` cancels the capture in both phases — which is why it is the one
    /// chord this editor cannot capture, and why the screen says so.
    /// `Enter`, on the other hand, CAN be captured: in the waiting phase it
    /// is a key like any other and only confirms afterward.
    #[test]
    fn esc_cancels_and_enter_can_be_captured() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_layers, cfg) = config_in(dir.path());
        let (browse, viewer, dialog) = maps(&cfg);
        let m = Maps {
            browse: &browse,
            viewer: &viewer,
            dialog: &dialog,
        };
        let mut sc = editor_at(&browse, &viewer, &dialog, Screen::Browse, "pane.mkdir");
        super::shortcuts_key(&mut sc, &m, KeyModifiers::NONE, KeyCode::Enter);
        super::shortcuts_key(&mut sc, &m, KeyModifiers::NONE, KeyCode::Esc);
        assert!(!sc.is_capturing(), "esc cancels the wait");
        // And with the capture closed, `Esc` closes the screen.
        assert!(matches!(
            super::shortcuts_key(&mut sc, &m, KeyModifiers::NONE, KeyCode::Esc),
            ShortcutsKeyOutcome::Close
        ));

        super::shortcuts_key(&mut sc, &m, KeyModifiers::NONE, KeyCode::Enter);
        super::shortcuts_key(&mut sc, &m, KeyModifiers::NONE, KeyCode::Enter);
        assert_eq!(
            sc.capture().map(|c| c.seq().to_vec()),
            Some(vec![chord("enter")]),
            "the first Enter opens the capture and the second IS the key"
        );
        super::shortcuts_key(&mut sc, &m, KeyModifiers::NONE, KeyCode::Esc);
        assert!(!sc.is_capturing(), "esc cancels with a verdict too");
    }

    /// `Ctrl+C` does NOT quit norte while capturing: it is a chord a CUA
    /// convert wants to bind, and in capture mode the reader is pressing
    /// blind because the editor asked for it. Outside the capture it stays
    /// the usual emergency exit.
    #[tokio::test]
    async fn ctrl_c_while_capturing_is_a_chord_not_an_exit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_layers, cfg) = config_in(dir.path());
        let (browse, viewer, dialog) = maps(&cfg);
        let m = Maps {
            browse: &browse,
            viewer: &viewer,
            dialog: &dialog,
        };
        let mut app = app_vacia();
        app.shortcuts = Some(editor_at(
            &browse,
            &viewer,
            &dialog,
            Screen::Browse,
            "pane.mkdir",
        ));
        super::on_shortcuts_key(&mut app, &cfg, None, &m, KeyModifiers::NONE, KeyCode::Enter).await;
        super::on_shortcuts_key(
            &mut app,
            &cfg,
            None,
            &m,
            KeyModifiers::CONTROL,
            KeyCode::Char('c'),
        )
        .await;
        assert!(
            !app.quit,
            "while capturing, ctrl+c is the key being captured"
        );
        assert_eq!(
            app.shortcuts
                .as_ref()
                .and_then(Shortcuts::capture)
                .map(|c| c.seq().to_vec()),
            Some(vec![chord("ctrl+c")])
        );
        // With the capture cancelled, it goes back to being the global exit.
        super::on_shortcuts_key(&mut app, &cfg, None, &m, KeyModifiers::NONE, KeyCode::Esc).await;
        super::on_shortcuts_key(
            &mut app,
            &cfg,
            None,
            &m,
            KeyModifiers::CONTROL,
            KeyCode::Char('c'),
        )
        .await;
        assert!(app.quit);
    }

    /// A dangerous codepoint can only arrive PASTED (norte does not enable
    /// bracketed paste), and it is not captured: `parse_chord` would accept
    /// it and the writer would leave it raw in the user's `keymap.toml`.
    #[test]
    fn a_pasted_dangerous_codepoint_is_not_captured() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_layers, cfg) = config_in(dir.path());
        let (browse, viewer, dialog) = maps(&cfg);
        let m = Maps {
            browse: &browse,
            viewer: &viewer,
            dialog: &dialog,
        };
        let mut sc = editor_at(&browse, &viewer, &dialog, Screen::Browse, "pane.mkdir");
        super::shortcuts_key(&mut sc, &m, KeyModifiers::NONE, KeyCode::Enter);
        assert!(matches!(
            super::shortcuts_key(
                &mut sc,
                &m,
                KeyModifiers::NONE,
                // U+202E RIGHT-TO-LEFT OVERRIDE.
                KeyCode::Char('\u{202e}')
            ),
            ShortcutsKeyOutcome::NotBindable
        ));
        assert!(
            sc.capture().expect("still capturing").is_waiting(),
            "nothing was captured"
        );
    }

    /// A modal that arrives ALONE (a policy approval, a collision) claims
    /// the keyboard: the editor stops asking for a key blind, and the
    /// modal's arm retires it entirely.
    #[test]
    fn a_modal_arriving_alone_abandons_the_capture() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_layers, cfg) = config_in(dir.path());
        let (browse, viewer, dialog) = maps(&cfg);
        let mut app = app_vacia();
        let mut sc = editor_at(&browse, &viewer, &dialog, Screen::Browse, "pane.mkdir");
        assert!(sc.begin_capture());
        app.shortcuts = Some(sc);
        app.pending_approvals
            .push_back(norte_proto::methods::PolicyApprovalRequired {
                approval_id: 1,
                session: Some("s1".into()),
                op: "copy".into(),
                paths: vec!["mem:///a".into()],
                paths_total: 0,
                ttl_ms: 60_000,
                detail: norte_proto::methods::ApprovalDetail::default(),
            });
        app.open_next_pending();
        assert!(app.modal.is_some());
        assert!(
            !app.shortcuts.as_ref().is_some_and(Shortcuts::is_capturing),
            "a key is no longer requested blind"
        );
        close_stale_overlays(&mut app);
        assert!(app.shortcuts.is_none(), "and the modal's arm retires it");
    }

    /// The editor lists what the reference sheet cannot: a command that no
    /// key presses. Without that row, "how do I press X" has no answer.
    #[test]
    fn a_command_with_no_key_has_a_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_layers, cfg) = config_in(dir.path());
        let (browse, viewer, dialog) = maps(&cfg);
        let rows = shortcut_rows(&Maps {
            browse: &browse,
            viewer: &viewer,
            dialog: &dialog,
        });
        assert!(
            rows.iter().any(|r| !r.is_bound()),
            "the active preset does not bind EVERYTHING the TUI dispatches"
        );
        for screen in [Screen::Browse, Screen::Viewer, Screen::Dialog] {
            assert!(
                rows.iter().any(|r| r.screen == screen),
                "{screen:?} has rows"
            );
        }
        // And the viewer's rows do not offer pane commands: binding
        // `pane.copy` there would write a key that does nothing in the
        // viewer.
        assert!(
            !rows
                .iter()
                .any(|r| r.screen == Screen::Viewer && !r.is_bound() && r.command == "pane.copy"),
            "the viewer does not dispatch pane commands"
        );
    }

    /// Every message on this screen exists in BOTH locales: what shows in
    /// the bar if a key is missing is the raw id.
    #[test]
    fn the_screens_keys_exist_in_both_locales() {
        for lang in [norte_i18n::Lang::Es, norte_i18n::Lang::En] {
            for id in [
                "shortcuts-title",
                "shortcuts-hint",
                "shortcuts-capture-hint",
                "shortcuts-capture-note",
                "shortcuts-confirm-hint",
                "shortcuts-no-key",
                "shortcuts-refused-preset",
                "msg-shortcut-bound",
                "msg-shortcut-unbound",
                "msg-shortcut-unbound-cleared",
                "msg-shortcut-nothing-to-unbind",
                "msg-shortcut-not-bindable",
                "shortcuts-row-global",
            ] {
                assert_ne!(norte_i18n::t_in(lang, id), id, "{id} missing in {lang:?}");
            }
        }
    }
}
