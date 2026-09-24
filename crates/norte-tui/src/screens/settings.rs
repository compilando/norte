//! The settings overlay (S3): one row per setting, with the palette's free
//! filter and inline value editing.
//!
//! It used to live in the `ntc` binary's root — a crate DISTINCT from this
//! lib — so not even the integration tests could feed it a key without the
//! event loop acting as go-between.
//!
//! A file separate from [`crate::settings`], which is the MODEL (the rows,
//! the filter, the edit buffer); this is the one that reads its keys and
//! carries the result to disk.

use crossterm::event::{KeyCode, KeyModifiers};
use norte_core::backend::Backend;
use norte_i18n::{t, ta};

use crate::app::{App, PAGE, PendingWrite, SettingsEditError, Shortcuts, io_error_category};
use crate::config;
use crate::keymap::presets;
use crate::shortcuts_editor::{Maps, shortcut_rows};

/// What to do after processing a settings-overlay key — separates the PURE
/// computation (inside `app.settings`'s borrow, `on_settings_key`) from the
/// async I/O (`persist_setting`, outside that borrow): `Settings::activate`/
/// `edit_commit` cannot return directly and persist in the same step because
/// they already take `&mut app.settings` — splitting it into an enum avoids
/// borrowing `app` twice at once.
enum SettingsKeyOutcome {
    /// The key was consumed with nothing to persist (navigation/filter/
    /// buffer editing in progress).
    None,
    /// Esc outside of editing: closes the overlay.
    Close,
    /// A setting changed — persist and announce it. Boxed: `PendingWrite`
    /// carries its own `toml_edit::Value` and makes this arm much bigger
    /// than the rest (clippy `large_enum_variant`) — indirection, not a
    /// different type.
    Write(Box<PendingWrite>),
    /// `Ctrl+R`: remove this setting's key from the write layer. Boxed for
    /// the same reason as [`Self::Write`].
    Reset(Box<norte_frontend::settings::PendingReset>),
    /// `Settings::edit_commit` rejected the buffer — announce the error,
    /// touching nothing (the buffer stays, `Settings` already keeps it).
    Invalid(SettingsEditError),
    /// K3c: `Ctrl+K` opens the shortcuts editor ON TOP of this overlay,
    /// which stays open behind it. It comes out as an outcome and not as an
    /// assignment inside the `match` because the rows are built from the
    /// LIVE effective ones ([`Maps`]) and that borrow does not fit inside
    /// `app.settings`'s.
    OpenShortcuts,
}

/// Keys of the settings overlay (`app.settings`, S3): same criterion as the
/// palette (decision 8 of the H1 plan) — free-filter editor, it does NOT
/// resolve through the `dialog` context; its keys stay hardcoded here.
/// `ctrl+c` keeps its global quit. While `Settings::is_editing()` the keys
/// go to the inline edit buffer (same pattern as the navigation popup's
/// `name_input`: raw printables/backspace, Enter confirms, Esc cancels);
/// otherwise they navigate/filter like the palette and Enter activates the
/// row under the cursor (`Settings::activate` — already cycles for
/// `Bool`/`Enum`/`ThemeName`/`PresetName`, or opens the buffer for
/// `Text`/`Int`).
pub async fn on_settings_key(app: &mut App, maps: &Maps<'_>, mods: KeyModifiers, code: KeyCode) {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    let plain = mods.is_empty() || mods == KeyModifiers::SHIFT;
    let outcome = {
        let Some(settings) = &mut app.settings else {
            return;
        };
        if settings.is_editing() {
            match code {
                KeyCode::Char(c) if plain => {
                    settings.edit_push_char(c);
                    SettingsKeyOutcome::None
                }
                KeyCode::Backspace if plain => {
                    settings.edit_backspace();
                    SettingsKeyOutcome::None
                }
                KeyCode::Esc => {
                    settings.edit_cancel();
                    SettingsKeyOutcome::None
                }
                KeyCode::Enter => match settings.edit_commit() {
                    Ok(write) => SettingsKeyOutcome::Write(Box::new(write)),
                    Err(e) => SettingsKeyOutcome::Invalid(e),
                },
                _ => SettingsKeyOutcome::None,
            }
        } else {
            match code {
                // BEFORE the generic `Char` arm, which would swallow them:
                // this overlay's filter captures EVERY printable. These are
                // LOCAL keys, not catalogue commands — a command would force
                // binding it in the seven presets, and the price paid here
                // is that a bracket cannot be searched for, since it does
                // not appear in any setting's name.
                // Switches side, as in help. `tab` in the catalogue's
                // `dialog` screen is `dialog.pane`, and this does the same
                // under a different name: it is a LOCAL key of the overlay,
                // which swallows keys before the dispatcher does.
                KeyCode::Tab if plain => {
                    settings.toggle_focus();
                    SettingsKeyOutcome::None
                }
                KeyCode::Char('[') if plain => {
                    settings.step_section(-1);
                    SettingsKeyOutcome::None
                }
                KeyCode::Char(']') if plain => {
                    settings.step_section(1);
                    SettingsKeyOutcome::None
                }
                KeyCode::Char(c) if plain => {
                    settings.push_char(c);
                    SettingsKeyOutcome::None
                }
                KeyCode::Backspace if plain => {
                    settings.backspace();
                    SettingsKeyOutcome::None
                }
                KeyCode::Esc if plain => SettingsKeyOutcome::Close,
                // K3c: the shortcuts editor. `Ctrl+K` and not a bare letter
                // because this overlay's filter swallows EVERY printable
                // (decision 8 of the H1 plan) — a `k` is text here.
                KeyCode::Char('k') if mods == KeyModifiers::CONTROL => {
                    SettingsKeyOutcome::OpenShortcuts
                }
                // Reset. `Ctrl+R` and not a bare letter for the same reason
                // as `Ctrl+K`: here an `r` is filter text.
                KeyCode::Char('r') if mods == KeyModifiers::CONTROL => match settings.reset() {
                    Some(reset) => SettingsKeyOutcome::Reset(Box::new(reset)),
                    None => SettingsKeyOutcome::None,
                },
                KeyCode::Up if plain => {
                    settings.up();
                    SettingsKeyOutcome::None
                }
                KeyCode::Down if plain => {
                    settings.down();
                    SettingsKeyOutcome::None
                }
                KeyCode::PageUp if plain => {
                    settings.page_up(PAGE);
                    SettingsKeyOutcome::None
                }
                KeyCode::PageDown if plain => {
                    settings.page_down(PAGE);
                    SettingsKeyOutcome::None
                }
                KeyCode::Enter if plain => {
                    // LIVE lists for `ThemeName`/`PresetName` (same criterion
                    // as `App::open_theme_picker`): resolved here, not
                    // `&'static` — the effective theme/keymap can change on
                    // the fly.
                    let theme_names = norte_frontend::theme::theme_names(&app.user_themes);
                    let all_presets = presets();
                    let preset_names: Vec<&str> = all_presets.iter().map(|(n, _)| *n).collect();
                    match settings.activate(&theme_names, &preset_names) {
                        Some(write) => SettingsKeyOutcome::Write(Box::new(write)),
                        None => SettingsKeyOutcome::None,
                    }
                }
                _ => SettingsKeyOutcome::None,
            }
        }
    };
    match outcome {
        SettingsKeyOutcome::None => {}
        SettingsKeyOutcome::Close => app.settings = None,
        SettingsKeyOutcome::Write(write) => persist_setting(app, *write).await,
        SettingsKeyOutcome::Reset(reset) => reset_setting(app, *reset).await,
        SettingsKeyOutcome::Invalid(e) => app.message = Some(settings_edit_error_message(&e)),
        SettingsKeyOutcome::OpenShortcuts => {
            app.shortcuts = Some(Shortcuts::new(shortcut_rows(maps)));
        }
    }
}

/// Removes a setting's key from the write layer (`Ctrl+R`) and says WHAT
/// really happened.
///
/// Removing the key from your layer does not always give back the factory
/// value: if the system, the profile or the project set the same one, the
/// value changes and is still not the default. So after writing, the
/// configuration is reread and the row is checked: if it is still modified,
/// it says so. Without this, "reset" would be a lie half the time and the
/// reader would go looking for the bug in the wrong place.
async fn reset_setting(app: &mut App, reset: norte_frontend::settings::PendingReset) {
    let Some(dir) = app.config_write_dir() else {
        app.message = Some(t("msg-settings-no-config-dir"));
        return;
    };
    let norte_frontend::settings::PendingReset {
        section,
        key,
        id,
        name,
    } = reset;
    // The SAME layers used to write, profile included: rereading with
    // others would answer about a configuration this process does not use.
    let layers = config::standard_layers_with_profile(app.active_profile.as_deref());
    match tokio::task::spawn_blocking(move || {
        config::persist_unset(&dir, section, &key).map(|out| {
            // The reread happens on the SAME background thread: it is file
            // I/O and this is where it can be done (rule 2).
            let still_set = config::load(&layers).ok().map(|cfg| {
                norte_frontend::settings::build_rows(&cfg, &[])
                    .into_iter()
                    .find(|r| r.id() == Some(id))
                    .is_some_and(|r| r.modified)
            });
            (out, still_set)
        })
    })
    .await
    {
        Ok(Ok((_out, still_set))) => {
            // `None` = the reread failed; say what IS known (the key was
            // removed) instead of asserting where the value comes from.
            let key = if still_set == Some(true) {
                "settings-still-set-elsewhere"
            } else {
                "settings-reset-done"
            };
            app.message = Some(ta(key, &[("name", &name)]));
        }
        Ok(Err(e)) => {
            app.message = Some(ta(
                "msg-settings-save-failed",
                &[("error", &io_error_category(&e))],
            ));
        }
        Err(e) => {
            tracing::error!(error = %e, "background reset_setting task did not finish");
            app.message = Some(t("msg-settings-save-crashed"));
        }
    }
}

/// Persists a [`PendingWrite`] (S3) — `spawn_blocking` (rule 2), same
/// pattern as the theme picker's persist (`on_theme_picker_key` above): it
/// resolves the directory by hand instead of reusing
/// `config::persist_ui_theme` (that wrapper does not take `section`/`key` —
/// S2 only gave the generic `persist_set(dir, ...)` with an explicit `dir`).
///
/// And that directory is the active PROFILE's if there is one
/// ([`App::config_write_dir`]): the profile sits above the user's layer, so
/// a setting written below that the profile also sets ends up covered —
/// saved and with no effect (ADR 0079).
pub async fn persist_setting(app: &mut App, write: PendingWrite) {
    let Some(dir) = app.config_write_dir() else {
        app.message = Some(t("msg-settings-no-config-dir"));
        return;
    };
    let PendingWrite {
        section,
        key,
        value,
        name,
        display,
    } = write;
    match tokio::task::spawn_blocking(move || config::persist_set(&dir, section, &key, value)).await
    {
        Ok(Ok(_path)) => {
            app.message = Some(ta(
                "msg-settings-saved",
                &[("name", &name), ("value", &display)],
            ));
        }
        Ok(Err(e)) => {
            app.message = Some(ta(
                "msg-settings-save-failed",
                &[("error", &io_error_category(&e))],
            ));
        }
        // Review S I1: the `spawn_blocking` task panicked or was cancelled
        // (before: total silence — `Settings::commit_row`'s optimistic row
        // was left LYING "edited" even though nothing was written). It must
        // not take the TUI down: it is announced on the bar (generic
        // category, no `{$error}` — a `JoinError` does not carry a clean
        // category) and leaves a trace with `tracing` for diagnosis — never
        // `eprintln!` here, which would corrupt ratatui's alternate screen
        // while the TUI is still alive.
        Err(e) => {
            tracing::error!(error = %e, "background persist_setting task did not finish");
            app.message = Some(t("msg-settings-save-crashed"));
        }
    }
}

/// Bar message for a [`SettingsEditError`] (S3) — by Fluent CATEGORY, never
/// ad hoc text (#73 pattern). Thin wrapper (review S, M6): byte-identical
/// to the GUI's (`settings_view::edit_error_message`) — hoisted to
/// [`norte_frontend::settings::edit_error_message`].
fn settings_edit_error_message(e: &SettingsEditError) -> String {
    norte_frontend::settings::edit_error_message(e)
}

/// Builds the Plugins-section summaries for the settings overlay (G3c):
/// `plugins_list` (approved+enabled only — same gate the palette's
/// `plugin_rows` and the extension manager's actionable rows use) then one
/// `plugin.get_config` PER surviving plugin, keeping only those with at
/// least one `[config.<key>]` (nothing to summarize/drill into otherwise).
/// Best-effort: a plugin whose `get_config` call fails (daemon hiccup, a
/// remote N-1 without the method) is simply DROPPED from the section — an
/// enrichment lost, never a hard error that would block opening settings
/// at all (same fallback contract as `plugin.decorate`/`column_values`).
/// `name` is masked here (plugin text, untrusted) — the ONLY point this
/// summary crosses into `norte_frontend::settings::Row`.
pub async fn plugin_config_summaries(
    backend: &Backend,
) -> Vec<norte_frontend::settings::PluginConfigSummary> {
    let Ok(list) = backend.plugins_list().await else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for p in list.plugins.iter().filter(|p| p.approved && p.enabled) {
        let Ok(cfg) = backend.plugin_get_config(&p.id).await else {
            continue;
        };
        if cfg.keys.is_empty() {
            continue;
        }
        let (name, _) = crate::app::display_name(p.name.as_bytes());
        out.push(norte_frontend::settings::PluginConfigSummary {
            plugin_id: p.id.clone(),
            name,
            key_count: cfg.keys.len(),
        });
    }
    out
}

#[cfg(test)]
mod settings_message_tests {
    use norte_i18n::{Lang, t_in};

    /// Review S I1: `msg-settings-save-crashed` (the `Err(_)` arm of
    /// `persist_setting`, see its doc) resolves to REAL text in both
    /// locales — not the raw id, which is what would show on the bar if the
    /// key were missing from some `.ftl`. Same coverage criterion as
    /// `norte_frontend::settings`'s `fluent_keys_exist_in_both_locales_
    /// for_each_entry`.
    #[test]
    fn msg_settings_save_crashed_exists_in_both_locales() {
        for lang in [Lang::Es, Lang::En] {
            assert_ne!(
                t_in(lang, "msg-settings-save-crashed"),
                "msg-settings-save-crashed",
                "missing the key in {lang:?}"
            );
        }
    }

    /// The two reset keys, for the same reason: the one that says another
    /// layer sets it is exactly the one nobody tries by hand.
    #[test]
    fn reset_keys_exist_in_both_locales() {
        for key in ["settings-reset-done", "settings-still-set-elsewhere"] {
            for lang in [Lang::Es, Lang::En] {
                assert_ne!(t_in(lang, key), key, "missing \"{key}\" in {lang:?}");
            }
        }
    }
}
