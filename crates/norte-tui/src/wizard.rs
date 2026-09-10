//! El asistente de primer arranque en la TUI (spec 2026-09-10): cuándo se
//! abre, qué hace cada tecla, y cómo se escribe lo elegido.
//!
//! El modelo es el compartido (`norte_frontend::wizard`); aquí va lo que
//! solo la terminal sabe: la vista previa del tema en vivo, y que escribir
//! un ajuste es `persist_set` en un hilo de bloqueo, igual que hace la
//! pantalla de ajustes.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use norte_frontend::wizard::{Outcome, Wizard};
use norte_i18n::t;

use crate::app::App;
use crate::config;

/// ¿Hay que abrir el asistente? Sí con `--setup`, o cuando no hay
/// `norte.toml` de usuario y nadie lo ha apagado (`NORTE_NO_WIZARD`, que es
/// lo que ponen los pilotos y los tests que arrancan un `ntc` de verdad).
/// Nunca en modo `--pick`: ahí la salida es para otro programa.
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

/// Abre el asistente con los presets y los temas que hay, arrancando en lo
/// vigente.
pub fn open(app: &mut App, cfg: &config::LoadedConfig) {
    let presets = norte_frontend::keymap::presets::NAMES;
    let temas = norte_theme::preset_names();
    let tema = cfg.common.ui_theme.as_deref().unwrap_or("default");
    app.wizard = Some(Wizard::new(presets, &temas, &cfg.common.preset, tema));
}

/// Una tecla con el asistente abierto: sube, baja, confirma, vuelve o sale.
/// Devuelve lo que hay que ESCRIBIR, si el paso fue el último o el lector
/// salió; `None` = sigue abierto. Pura salvo la vista previa del tema, que
/// se aplica al `App` al momento: lo que ves mientras te mueves es lo que
/// eliges.
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

/// La vista previa del tema bajo el cursor, en vivo.
fn preview(app: &mut App) {
    let Some(name) = app.wizard.as_ref().and_then(Wizard::preview_theme) else {
        return;
    };
    if let Ok(Some(theme)) = norte_theme::Theme::preset(name) {
        app.theme = crate::theme::TuiTheme::new(theme, app.theme.depth());
    }
}

/// Escribe lo elegido. Con `Dismissed` escribe SOLO el tema vigente, para
/// que exista el fichero y no se vuelva a preguntar: la marca de «ya
/// contestado» es el fichero mismo. Los iconos van al plugin `file-icons`
/// por el daemon; si no está instalado, el rehúse se ignora.
pub async fn finish(app: &mut App, backend: &norte_core::backend::Backend, outcome: Outcome) {
    let Some(dir) = app.config_write_dir() else {
        app.message = Some(t("msg-settings-no-config-dir"));
        return;
    };
    // Con `Dismissed` se escribe el tema con el que se ABRIÓ el asistente, no
    // `default`: `ntc --setup` + Esc sobre un `theme = "nord"` lo dejaba en
    // `default`, y una capa de sistema con tema quedaba tapada (revisión B1).
    let (preset, theme, icons) = match outcome {
        Outcome::Done(c) => (
            c.preset,
            c.theme.or_else(|| Some("default".to_owned())),
            c.icons,
        ),
        Outcome::Dismissed { keep_theme } => (None, Some(keep_theme), None),
        Outcome::Continue => (None, None, None),
    };
    let escritura = tokio::task::spawn_blocking(move || {
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
    match escritura {
        Ok(Ok(())) => app.message = Some(t("wizard-done")),
        Ok(Err(e)) => {
            app.message = Some(norte_i18n::ta(
                "msg-settings-save-failed",
                &[("error", &crate::app::io_error_category(&e))],
            ));
        }
        Err(e) => {
            tracing::error!(error = %e, "la escritura del asistente no terminó");
            app.message = Some(t("msg-settings-save-crashed"));
        }
    }
    if let Some(icons) = icons {
        let style = if icons { "emoji" } else { "ascii" };
        if let Err(e) = backend
            .plugin_set_config("file-icons", "style", style)
            .await
        {
            tracing::info!(error = %e, "sin plugin file-icons: los iconos del asistente no aplican");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::app_dos_panes;

    fn tecla(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Las teclas mueven el modelo, el tema se previsualiza al momento, Esc
    /// cierra con `Dismissed` y Enter en el último paso con `Done`.
    #[test]
    fn las_teclas_mueven_el_asistente_y_el_tema_se_ve_en_vivo() {
        let mut app = app_dos_panes();
        app.wizard = Some(Wizard::new(
            &["orthodox", "vim"],
            &["default", "nord"],
            "orthodox",
            "default",
        ));
        assert_eq!(
            apply_key(&mut app, tecla(KeyCode::Enter)),
            None,
            "al paso del tema"
        );
        let antes = app.theme.role(norte_theme::Role::Selection);
        assert_eq!(apply_key(&mut app, tecla(KeyCode::Down)), None);
        assert_ne!(
            app.theme.role(norte_theme::Role::Selection),
            antes,
            "nord se ve mientras se elige"
        );
        assert_eq!(
            apply_key(&mut app, tecla(KeyCode::Enter)),
            None,
            "al paso de iconos"
        );
        let done = apply_key(&mut app, tecla(KeyCode::Enter)).expect("último paso");
        assert!(matches!(done, Outcome::Done(ref c) if c.theme.as_deref() == Some("nord")));
        assert!(app.wizard.is_none(), "cerrado al terminar");

        app.wizard = Some(Wizard::new(
            &["orthodox"],
            &["default"],
            "orthodox",
            "default",
        ));
        assert_eq!(
            apply_key(&mut app, tecla(KeyCode::Esc)),
            Some(Outcome::Dismissed {
                keep_theme: "default".into()
            })
        );
        assert!(app.wizard.is_none());
    }
}
