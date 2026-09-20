//! El overlay de ajustes (S3): una fila por ajuste, con el filtro libre de la
//! palette y la edición inline del valor.
//!
//! Vivía en el root del binario `ntc` —un crate DISTINTO de esta lib—, así que
//! ni los tests de integración podían meterle una tecla sin que el bucle de
//! eventos hiciera de intermediario.
//!
//! Fichero aparte de [`crate::settings`], que es el MODELO (las filas, el
//! filtro, el buffer de edición); esto es quien lee sus teclas y lleva el
//! resultado al disco.

use crossterm::event::{KeyCode, KeyModifiers};
use norte_core::backend::Backend;
use norte_i18n::{t, ta};

use crate::app::{App, PAGE, PendingWrite, SettingsEditError, Shortcuts, io_error_category};
use crate::config;
use crate::keymap::presets;
use crate::shortcuts_editor::{Maps, shortcut_rows};

/// Qué hacer tras procesar una tecla del overlay de ajustes — separa el
/// cómputo PURO (dentro del borrow de `app.settings`, `on_settings_key`) del
/// I/O async (`persist_setting`, fuera de ese borrow): `Settings::activate`/
/// `edit_commit` no pueden devolver directamente y persistir en el mismo
/// paso porque ya toman `&mut app.settings` — separarlo en un enum evita
/// pedir prestado `app` dos veces a la vez.
enum SettingsKeyOutcome {
    /// La tecla se consumió sin nada que persistir (navegación/filtro/
    /// edición de buffer en curso).
    None,
    /// Esc fuera de edición: cierra el overlay.
    Close,
    /// Un ajuste cambió — persistir y anunciar. Boxed: `PendingWrite` lleva
    /// un `toml_edit::Value` propio y hace este brazo mucho más grande que
    /// el resto (clippy `large_enum_variant`) — indirección, no un tipo
    /// distinto.
    Write(Box<PendingWrite>),
    /// `Ctrl+R`: quitar la clave de este ajuste de la capa de escritura.
    /// Boxed por lo mismo que [`Self::Write`].
    Reset(Box<norte_frontend::settings::PendingReset>),
    /// `Settings::edit_commit` rechazó el buffer — anunciar el error, sin
    /// tocar nada (el buffer se queda, `Settings` ya lo conserva).
    Invalid(SettingsEditError),
    /// K3c: `Ctrl+K` abre el editor de atajos POR ENCIMA de este overlay, que
    /// se queda abierto detrás. Sale como outcome y no como una asignación
    /// dentro del `match` porque las filas se construyen de los efectivos
    /// VIVOS ([`Maps`]) y ese borrow no cabe dentro del de `app.settings`.
    OpenShortcuts,
}

/// Teclas del overlay de ajustes (`app.settings`, S3): mismo criterio que la
/// palette (decisión 8 del plan H1) — editor de filtro libre, NO resuelve
/// por el contexto `dialog`; sus teclas quedan hardcodeadas aquí. `ctrl+c`
/// conserva su salida global. Mientras `Settings::is_editing()` las teclas
/// van al buffer de edición inline (mismo patrón que `name_input` del popup
/// de navegación: imprimibles/backspace crudos, Enter confirma, Esc
/// cancela); si no, navegan/filtran como la palette y Enter activa la fila
/// bajo el cursor (`Settings::activate` — cicla YA para `Bool`/`Enum`/
/// `ThemeName`/`PresetName`, o abre el buffer para `Text`/`Int`).
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
                // ANTES del brazo genérico de `Char`, que se las comería: el
                // filtro de este overlay captura TODO imprimible. Son teclas
                // LOCALES, no comandos del catálogo — un comando obligaría a
                // bindearlo en los siete presets, y el precio de aquí es que
                // no se puede buscar un corchete, que no aparece en el nombre
                // de ningún ajuste.
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
                // K3c: el editor de atajos. `Ctrl+K` y no una letra suelta
                // porque el filtro de este overlay se come TODO imprimible
                // (decisión 8 del plan H1) — una `k` es texto aquí.
                KeyCode::Char('k') if mods == KeyModifiers::CONTROL => {
                    SettingsKeyOutcome::OpenShortcuts
                }
                // Restablecer. `Ctrl+R` y no una letra suelta por lo mismo
                // que `Ctrl+K`: aquí una `r` es texto del filtro.
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
                    // Listas VIVAS para `ThemeName`/`PresetName` (mismo
                    // criterio que `App::open_theme_picker`): resueltas aquí,
                    // no `&'static` — el tema/keymap efectivo puede cambiar
                    // en caliente.
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

/// Quita la clave de un ajuste de la capa de escritura (`Ctrl+R`) y dice
/// QUÉ pasó de verdad.
///
/// Quitar la clave de tu capa no siempre devuelve el valor de fábrica: si el
/// sistema, el perfil o el proyecto fijan la misma, el valor cambia y sigue
/// sin ser el defecto. Así que después de escribir se relee la configuración
/// y se mira la fila: si sigue modificada, lo dice. Sin esto, «restablecido»
/// sería mentira la mitad de las veces y el lector iría a buscar el bug a
/// donde no está.
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
    // Las MISMAS capas con las que se escribe, perfil incluido: releer con
    // otras contestaría sobre una configuración que este proceso no usa.
    let capas = config::standard_layers_with_profile(app.active_profile.as_deref());
    match tokio::task::spawn_blocking(move || {
        config::persist_unset(&dir, section, &key).map(|out| {
            // La relectura va en el MISMO hilo de fondo: es I/O de fichero
            // y aquí es donde se puede hacer (regla 2).
            let sigue = config::load(&capas).ok().map(|cfg| {
                norte_frontend::settings::build_rows(&cfg, &[])
                    .into_iter()
                    .find(|r| r.id() == Some(id))
                    .is_some_and(|r| r.modified)
            });
            (out, sigue)
        })
    })
    .await
    {
        Ok(Ok((_out, sigue))) => {
            // `None` = la relectura falló; se dice lo que sí se sabe (la
            // clave se quitó) en vez de afirmar de dónde viene el valor.
            let clave = if sigue == Some(true) {
                "settings-still-set-elsewhere"
            } else {
                "settings-reset-done"
            };
            app.message = Some(ta(clave, &[("name", &name)]));
        }
        Ok(Err(e)) => {
            app.message = Some(ta(
                "msg-settings-save-failed",
                &[("error", &io_error_category(&e))],
            ));
        }
        Err(e) => {
            tracing::error!(error = %e, "tarea de fondo de reset_setting no terminó");
            app.message = Some(t("msg-settings-save-crashed"));
        }
    }
}

/// Persiste un [`PendingWrite`] (S3) — `spawn_blocking` (regla 2), mismo
/// patrón que el persist del theme picker (`on_theme_picker_key` arriba):
/// resuelve el directorio a mano en vez de reutilizar
/// `config::persist_ui_theme` (esa wrapper no toma `section`/`key` — S2 solo
/// dio el genérico `persist_set(dir, ...)` con `dir` explícito).
///
/// Y ese directorio es el del PERFIL activo si lo hay
/// ([`App::config_write_dir`]): el perfil está por encima de la capa del
/// usuario, así que un ajuste escrito abajo que el perfil también fija queda
/// tapado — guardado y sin efecto (ADR 0079).
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
        // Revisión S I1: la tarea de `spawn_blocking` panicó o se canceló
        // (antes: silencio total — la fila optimista de `Settings::
        // commit_row` quedaba MINTIENDO "editado" aunque nada se escribió).
        // No debe tumbar la TUI: se anuncia en la barra (categoría genérica,
        // sin `{$error}` — un `JoinError` no trae una categoría limpia) y se
        // deja rastro con `tracing` para diagnóstico — jamás `eprintln!`
        // aquí, que corrompería la pantalla alterna de ratatui mientras la
        // TUI sigue viva.
        Err(e) => {
            tracing::error!(error = %e, "tarea de fondo de persist_setting no terminó");
            app.message = Some(t("msg-settings-save-crashed"));
        }
    }
}

/// Mensaje de barra para un [`SettingsEditError`] (S3) — por CATEGORÍA
/// Fluent, nunca texto ad hoc (#73 pattern). Envoltorio fino (revisión S,
/// M6): byte-idéntico al de la GUI (`settings_view::edit_error_message`) —
/// hoisteado a [`norte_frontend::settings::edit_error_message`].
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

    /// Revisión S I1: `msg-settings-save-crashed` (el brazo `Err(_)` de
    /// `persist_setting`, ver su doc) resuelve a texto REAL en ambos
    /// locales — no al id crudo, que es lo que se vería en la barra si
    /// faltara la clave en algún `.ftl`. Mismo criterio de cobertura que
    /// `norte_frontend::settings`'s `fluent_keys_existen_en_ambos_locales_
    /// para_cada_entrada`.
    #[test]
    fn msg_settings_save_crashed_existe_en_ambos_locales() {
        for lang in [Lang::Es, Lang::En] {
            assert_ne!(
                t_in(lang, "msg-settings-save-crashed"),
                "msg-settings-save-crashed",
                "falta la clave en {lang:?}"
            );
        }
    }

    /// Las dos claves de restablecer, por lo mismo: la que dice que otra
    /// capa lo fija es justo la que nadie prueba a mano.
    #[test]
    fn las_claves_de_restablecer_existen_en_ambos_locales() {
        for clave in ["settings-reset-done", "settings-still-set-elsewhere"] {
            for lang in [Lang::Es, Lang::En] {
                assert_ne!(t_in(lang, clave), clave, "falta «{clave}» en {lang:?}");
            }
        }
    }
}
