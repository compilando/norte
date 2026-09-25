//! The first-run wizard in the TUI (spec 2026-09-10): when it opens, what
//! each key does, and how the chosen settings get written.
//!
//! The model is the shared one (`norte_frontend::wizard`); what is here is
//! what only the terminal knows: the live theme preview, and that writing a
//! setting is a `persist_set` on a blocking thread, the same as the settings
//! screen does.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use norte_frontend::wizard::{Outcome, Wizard};
use norte_i18n::t;

use crate::app::App;
use crate::config;

/// Should the wizard open? Yes with `--setup`, or when there is no user
/// `norte.toml` and nobody has turned it off (`NORTE_NO_WIZARD`, which is
/// what the harnesses and the tests that start a real `ntc` set). Never in
/// `--pick` mode: there, the output is for another program.
pub async fn should_open(setup: bool, pick: bool) -> bool {
    if pick {
        return false;
    }
    if setup {
        return true;
    }
    if std::env::var_os("NORTE_NO_WIZARD").is_some() {
        return false;
    }
    let Some(dir) = config::user_config_dir() else {
        return false;
    };
    !tokio::fs::try_exists(dir.join("norte.toml"))
        .await
        .unwrap_or(true)
}

/// Opens the wizard with the presets and themes that exist, starting on the
/// current one.
pub fn open(app: &mut App, cfg: &config::LoadedConfig) {
    let presets = norte_frontend::keymap::presets::NAMES;
    let names = norte_frontend::theme::theme_names(&cfg.user_themes);
    let themes: Vec<&str> = names.iter().map(String::as_str).collect();
    let theme = cfg.common.ui_theme.as_deref().unwrap_or("default");
    app.wizard = Some(Wizard::new(presets, &themes, &cfg.common.preset, theme));
}

/// A key with the wizard open: up, down, confirm, back, or exit. Returns what
/// must be WRITTEN, if the step was the last one or the reader exited; `None`
/// = still open. Pure except for the theme preview, which applies to the
/// `App` immediately: what you see while moving is what you are choosing.
pub fn apply_key(app: &mut App, key: KeyEvent) -> Option<Outcome> {
    let plain = key.modifiers.is_empty();
    let w = app.wizard.as_mut()?;
    let outcome = match key.code {
        KeyCode::Up if plain => {
            w.up();
            None
        }
        KeyCode::Down if plain => {
            w.down();
            None
        }
        KeyCode::Backspace if plain => {
            w.back();
            None
        }
        KeyCode::Enter if plain => match w.confirm() {
            Outcome::Continue => None,
            done => Some(done),
        },
        KeyCode::Esc if plain => Some(w.dismiss()),
        KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => {
            app.quit = true;
            return None;
        }
        _ => None,
    };
    if outcome.is_some() {
        app.wizard = None;
    } else {
        preview(app);
    }
    outcome
}

/// The live preview of the theme under the cursor.
fn preview(app: &mut App) {
    let Some(name) = app.wizard.as_ref().and_then(Wizard::preview_theme) else {
        return;
    };
    // A preset or an already-loaded user theme: no disk access on a keypress.
    if let Some(theme) = norte_frontend::theme::theme_by_name(name, &app.user_themes) {
        app.theme = crate::theme::TuiTheme::new(theme, app.theme.depth());
    }
}

/// Writes what was chosen. With `Dismissed`, writes ONLY the current theme,
/// so the file exists and is not asked about again: the "already answered"
/// mark is the file itself. Icons go to the `file-icons` plugin through the
/// daemon; if it is not installed, the refusal is ignored.
pub async fn finish(app: &mut App, backend: &norte_core::backend::Backend, outcome: Outcome) {
    let Some(dir) = app.config_write_dir() else {
        app.message = Some(t("msg-settings-no-config-dir"));
        return;
    };
    // With `Dismissed` the theme the wizard was OPENED with is written, not
    // `default`: `ntc --setup` + Esc over a `theme = "nord"` used to leave it
    // at `default`, burying a system layer's theme (review B1).
    let (preset, theme, icons) = match outcome {
        Outcome::Done(c) => (
            c.preset,
            c.theme.or_else(|| Some("default".to_owned())),
            c.icons,
        ),
        Outcome::Dismissed { keep_theme } => (None, Some(keep_theme), None),
        Outcome::Continue => (None, None, None),
    };
    let write = tokio::task::spawn_blocking(move || {
        let mut res = Ok(());
        if let Some(p) = preset {
            res = res.and(config::persist_set(&dir, "keymap", "preset", p.into()).map(|_| ()));
        }
        if let Some(t) = theme {
            res = res.and(config::persist_set(&dir, "ui", "theme", t.into()).map(|_| ()));
        }
        res
    })
    .await;
    match write {
        Ok(Ok(())) => app.message = Some(t("wizard-done")),
        Ok(Err(e)) => {
            app.message = Some(norte_i18n::ta(
                "msg-settings-save-failed",
                &[("error", &crate::app::io_error_category(&e))],
            ));
        }
        Err(e) => {
            tracing::error!(error = %e, "the wizard's write did not finish");
            app.message = Some(t("msg-settings-save-crashed"));
        }
    }
    if let Some(icons) = icons {
        let style = if icons { "emoji" } else { "ascii" };
        if let Err(e) = backend
            .plugin_set_config("file-icons", "style", style)
            .await
        {
            tracing::info!(error = %e, "no file-icons plugin: the wizard's icon choice does not apply");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::app_two_panes;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Keys move the model, the theme is previewed live, Esc closes with
    /// `Dismissed`, and Enter on the last step closes with `Done`.
    #[test]
    fn keys_move_the_wizard_and_the_theme_is_seen_live() {
        let mut app = app_two_panes();
        app.wizard = Some(Wizard::new(
            &["orthodox", "vim"],
            &["default", "nord"],
            "orthodox",
            "default",
        ));
        assert_eq!(
            apply_key(&mut app, key(KeyCode::Enter)),
            None,
            "at the theme step"
        );
        let before = app.theme.role(norte_theme::Role::Selection);
        assert_eq!(apply_key(&mut app, key(KeyCode::Down)), None);
        assert_ne!(
            app.theme.role(norte_theme::Role::Selection),
            before,
            "nord is visible while it is being chosen"
        );
        assert_eq!(
            apply_key(&mut app, key(KeyCode::Enter)),
            None,
            "at the icons step"
        );
        let done = apply_key(&mut app, key(KeyCode::Enter)).expect("last step");
        assert!(matches!(done, Outcome::Done(ref c) if c.theme.as_deref() == Some("nord")));
        assert!(app.wizard.is_none(), "closed on finishing");

        app.wizard = Some(Wizard::new(
            &["orthodox"],
            &["default"],
            "orthodox",
            "default",
        ));
        assert_eq!(
            apply_key(&mut app, key(KeyCode::Esc)),
            Some(Outcome::Dismissed {
                keep_theme: "default".into()
            })
        );
        assert!(app.wizard.is_none());
    }
}
