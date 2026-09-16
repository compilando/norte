//! El gestor de extensiones (G3): la lista de plugins, su ayuda de una tecla y
//! el panel de `[config]` de cada uno.
//!
//! Vivía en el root del binario `ntc` —un crate DISTINTO de esta lib—, así que
//! ni los tests de integración podían meterle una tecla ni afirmar su ayuda sin
//! que el bucle de eventos hiciera de intermediario.
//!
//! Un plugin es código de TERCEROS: todo lo que llega de él —id, nombre,
//! salida, valores de `[config]`— pasa por `detail_for_bar` antes de tocar la
//! barra (patrón #73), y la autorización de correr un comando es siempre del
//! SERVIDOR, no de la foto que este cliente tenga congelada.

use crossterm::event::{KeyCode, KeyModifiers};
use norte_core::backend::Backend;
use norte_i18n::{t, ta};

use crate::app::{
    ALLOW_EXTENSIONS, ALLOW_PLUGIN_CONFIG, App, ExtensionManager, HelpView, error_message,
};
use crate::keymap::{Resolution, Resolver, chord_from_crossterm};

/// `F1` in the extension manager: open the help on the highlighted plugin's OWN
/// page (H3e).
///
/// The manager is where a human decides whether to approve an extension, and
/// the page that argues for it is one keystroke away — from the list they are
/// already looking at, with no detour through the help's own sidebar. The
/// snapshot is the list the manager ALREADY holds, so this costs no round trip;
/// the page itself is fetched by the run loop, on demand, like any other plugin
/// node.
///
/// Order matters here and the sequence is not interchangeable:
/// `HelpState::open_as_root` refuses an id that names nothing it can show, so
/// the nodes have to be installed BEFORE the page is opened.
///
/// The manager CLOSES, as the palette does for its own `F1` bridge: its arm
/// sits ahead of the help in the run loop's key chain, so an overlay left open
/// underneath would eat every key meant for the page.
///
/// A plugin with no `help.md` gets a status line rather than silence — the row
/// looks exactly like one that does, and a key that appears to do nothing reads
/// as a broken app. `over_modal` is `false`: this arm only runs with no modal on
/// screen (`modal_wins`).
pub fn extensions_help(
    app: &mut App,
    lang: norte_help::Lang,
    help_lines: &[ratatui::text::Line<'static>],
) {
    let Some(plugin) = app.extensions.as_ref().and_then(ExtensionManager::selected) else {
        return;
    };
    if !plugin.has_help {
        app.message = Some(t("msg-extensions-no-help"));
        return;
    }
    let id = norte_help::TopicId::new(&plugin.id);
    let Some(plugins) = app.extensions.take().map(|m| m.plugins) else {
        return;
    };
    // H3d: el mismo congelado que `open_contextual_help` — la ayuda que se abre
    // desde el gestor es la misma ayuda.
    app.freeze_help_facts();
    app.help = Some(HelpView::new(lang, help_lines.to_vec()));
    app.freeze_help_plugins(&plugins);
    if let Some(help) = app.help.as_mut() {
        help.state.open_as_root(&id);
    }
}

/// Teclas del overlay de extensiones (M4-P3), resueltas contra el contexto
/// `dialog` del keymap (H1 T2, issue #24); `ctrl+c` conserva su salida
/// global, hardcodeado ANTES de resolver. Regla 7: aprobar/activar viaja al
/// core por el `Backend`; el bool LOCAL solo se togglea tras un OK (feedback
/// inmediato sin relistar). El id y el estado se toman ANTES del `.await`
/// (el borrow del `mgr` se suelta durante la llamada al backend y se
/// re-obtiene después para reflejar el resultado). Allowlist de este
/// overlay: `dialog.up/down/cancel/approve/toggle-enabled` — `approve`
/// togglea la APROBACIÓN del plugin (decisión 3 del plan H1: "aprobar un
/// plugin" reutiliza semánticamente `dialog.approve`, antes era la tecla
/// `a` hardcodeada; ahora `a` es `dialog.add`, que este overlay no soporta).
///
/// Fuera del allowlist, una sola tecla más: `app.help` (H3e) abre la página del
/// plugin resaltado — ver [`extensions_help`].
pub async fn on_extensions_key(
    app: &mut App,
    backend: &Backend,
    resolver: &mut Resolver,
    lang: norte_help::Lang,
    help_lines: &[ratatui::text::Line<'static>],
    mods: KeyModifiers,
    code: KeyCode,
) {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    if app.extensions.is_none() {
        return;
    }
    // G3c drill-down: while a `[config]` `string`/`int` edit buffer is
    // active, keys are captured RAW (same idiom as `on_nav_popup_key`'s
    // `name_input`) — bypassing the keymap resolver entirely, so typing
    // e.g. "y" edits the buffer instead of resolving to `dialog.approve`.
    let editing = app
        .extensions
        .as_ref()
        .and_then(|m| m.config.as_ref())
        .is_some_and(|p| p.state.is_editing());
    if editing {
        on_plugin_config_edit_key(app, backend, mods, code).await;
        return;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return; // tecla no modelada por el keymap: ignorar
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        // Secuencia en curso, o tecla ligada a algo que esta build no corre
        // (K1 T4): ignorar y reiniciar el estado de resolución.
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return;
        }
        Resolution::Reset => return,
    };
    let panel_open = app.extensions.as_ref().is_some_and(|m| m.config.is_some());
    // H3e: `app.help` es un comando de `[global]`, no un verbo `dialog.*`, así
    // que no está en ningún allowlist de este overlay y sin esta rama F1 sería
    // inerte aquí. Se resuelve por el keymap como todo lo demás (un rebind de
    // `app.help` mueve también este puente); lo cableado es el significado, no
    // la tecla. Mismo criterio que la rama `app.help` de `on_help_key` y que F9
    // en `on_theme_picker_key`. NO cuando el panel de `[config]` está abierto:
    // ahí el lector está editando valores, y perder el panel para leer prosa no
    // es lo que pidió.
    if cmd == "app.help" && !panel_open {
        extensions_help(app, lang, help_lines);
        return;
    }
    // H1 T3: el MISMO allowlist que consume el hint generado
    // (`hints::DialogHints::build`) — una sola fuente para dispatch y footer.
    // G3c: qué allowlist aplica depende de si el panel de `[config]` está
    // abierto.
    let allow: &[&str] = if panel_open {
        ALLOW_PLUGIN_CONFIG
    } else {
        ALLOW_EXTENSIONS
    };
    if !allow.contains(&cmd.as_str()) {
        return; // fuera del allowlist de este contexto: inerte
    }
    if panel_open {
        on_plugin_config_panel_cmd(app, backend, &cmd).await;
    } else {
        on_extensions_list_cmd(app, backend, &cmd).await;
    }
}

/// Un clic en el gestor: un botón de la ficha, o la fila ya elegida.
///
/// El MISMO despacho que la tecla (`on_extensions_list_cmd`, y para
/// `app.help` el mismo puente que `F1`): un botón que encendiera una
/// extensión por un camino y la tecla por otro sería dos gestores que
/// divergen en cuanto uno crece un detalle (ADR 0077, dentro de un solo
/// frontend). Lo que el botón NO hace es pasar por el allowlist del panel
/// de ajustes: el clic es explícito, y apagar una extensión con sus
/// ajustes a la vista es exactamente lo que el lector pidió.
pub async fn on_extensions_click(
    app: &mut App,
    backend: &Backend,
    lang: norte_help::Lang,
    help_lines: &[ratatui::text::Line<'static>],
    cmd: &str,
) {
    if app.extensions.is_none() {
        return;
    }
    if cmd == "app.help" {
        extensions_help(app, lang, help_lines);
        return;
    }
    if !ALLOW_EXTENSIONS.contains(&cmd) {
        return;
    }
    on_extensions_list_cmd(app, backend, cmd).await;
}

/// G3c: teclas RAW mientras un `string`/`int` de `[config]` se edita
/// (`on_extensions_key`'s guard `editing`) — mismo idioma que
/// `on_nav_popup_key`'s `name_input`.
async fn on_plugin_config_edit_key(
    app: &mut App,
    backend: &Backend,
    mods: KeyModifiers,
    code: KeyCode,
) {
    let plain = mods.is_empty() || mods == KeyModifiers::SHIFT;
    match code {
        KeyCode::Char(c) if plain => {
            if let Some(panel) = app.extensions.as_mut().and_then(|m| m.config.as_mut()) {
                panel.state.edit_push_char(c);
            }
        }
        KeyCode::Backspace if plain => {
            if let Some(panel) = app.extensions.as_mut().and_then(|m| m.config.as_mut()) {
                panel.state.edit_backspace();
            }
        }
        KeyCode::Esc => {
            if let Some(panel) = app.extensions.as_mut().and_then(|m| m.config.as_mut()) {
                panel.state.edit_cancel();
            }
        }
        KeyCode::Enter => {
            let Some(panel) = app.extensions.as_mut().and_then(|m| m.config.as_mut()) else {
                return;
            };
            match panel.state.edit_commit() {
                Ok(write) => {
                    let id = panel.plugin_id.clone();
                    commit_plugin_config_write(app, backend, &id, write).await;
                }
                Err(err) => {
                    app.message = Some(norte_frontend::settings::edit_error_message(&err));
                }
            }
        }
        _ => {}
    }
}

/// G3c: comandos resueltos (`up`/`down`/`confirm`/`cancel`) mientras el
/// panel de `[config]` está abierto y NADA se edita (`on_extensions_key`,
/// `panel_open` branch — `allow == ALLOW_PLUGIN_CONFIG`).
async fn on_plugin_config_panel_cmd(app: &mut App, backend: &Backend, cmd: &str) {
    let Some(panel) = app.extensions.as_mut().and_then(|m| m.config.as_mut()) else {
        return;
    };
    match cmd {
        "dialog.up" => panel.state.up(),
        "dialog.down" => panel.state.down(),
        "dialog.cancel" => {
            if let Some(mgr) = &mut app.extensions {
                mgr.config = None;
            }
        }
        "dialog.confirm" => {
            if let Some(write) = panel.state.activate() {
                let id = panel.plugin_id.clone();
                commit_plugin_config_write(app, backend, &id, write).await;
            }
        }
        _ => {}
    }
}

/// El resto de `on_extensions_key`: comandos sobre la LISTA de plugins
/// (`panel_open == false`, `allow == ALLOW_EXTENSIONS`) — navegar,
/// aprobar/activar, y `dialog.confirm` (G3c) abre el panel de `[config]`
/// del plugin resaltado SI declara alguna clave. Enter NUNCA aprueba (pin
/// P1): solo entra en un submenú.
async fn on_extensions_list_cmd(app: &mut App, backend: &Backend, cmd: &str) {
    let Some(mgr) = &mut app.extensions else {
        return;
    };
    match cmd {
        "dialog.up" => mgr.up(),
        "dialog.down" => mgr.down(),
        "dialog.cancel" => app.extensions = None,
        // CONCEDER pregunta; REVOCAR no (#280). La asimetría es la de todo
        // este árbol: lo que va en la dirección segura no necesita permiso, y
        // conceder capabilities es LA decisión de seguridad del sistema de
        // extensiones — la ventana gráfica ya preguntaba y aquí se aprobaba
        // con una tecla, enumerando nada.
        "dialog.approve" => {
            let Some(sel) = mgr.selected() else {
                return;
            };
            if sel.approved {
                let id = sel.id.clone();
                revocar_o_decir(app, backend, &id).await;
                return;
            }
            let (id, name) = (sel.id.clone(), sel.name.clone());
            let digest = sel.manifest_digest.clone();
            // Las capabilities, cada una enmascarada POR SU CUENTA y con su
            // bandera: son texto de un tercero, y pegarlas en una frase deja
            // que una finja ser otra.
            let caps: Vec<(String, bool)> = sel
                .capabilities
                .iter()
                .map(|c| norte_frontend::help_badge::plugin_label_flagged(c))
                .collect();
            let (nombre, nombre_hostil) = crate::app::display_name(name.as_bytes());
            app.modal = Some(crate::app::Modal::ConfirmPluginApproval {
                id,
                name: nombre,
                name_hostile: nombre_hostil,
                caps,
                digest,
            });
        }
        "dialog.toggle-enabled" => {
            let Some((id, cur, aprobado)) = mgr
                .selected()
                .map(|p| (p.id.clone(), p.enabled, p.approved))
            else {
                return;
            };
            // Encender lo que no está aprobado, no. Apagar lo que sí lo está
            // —aunque le hayan revocado la aprobación—, sí: apagar siempre va
            // en la dirección segura.
            if !cur && !aprobado {
                app.message = Some(t("msg-plugin-not-approved"));
                return;
            }
            match backend.plugins_set_enabled(&id, !cur).await {
                Ok(()) => relistar_extensiones(app, backend).await,
                Err(e) => app.message = Some(error_message(&e)),
            }
        }
        // Desinstalar (ADR 0104) SIEMPRE pregunta: borra ficheros y retira
        // el consentimiento, y no tiene vuelta. Mismo molde que conceder —el
        // nombre saneado y con su bandera, el id aparte— y misma puerta de
        // confirmación que borrar ficheros.
        "dialog.remove" => {
            let Some((id, name)) = mgr.selected().map(|p| (p.id.clone(), p.name.clone())) else {
                return;
            };
            let (nombre, nombre_hostil) = crate::app::display_name(name.as_bytes());
            app.modal = Some(crate::app::Modal::ConfirmPluginUninstall {
                id,
                name: nombre,
                name_hostile: nombre_hostil,
            });
        }
        "dialog.confirm" => {
            let Some((id, name)) = mgr.selected().map(|p| (p.id.clone(), p.name.clone())) else {
                return;
            };
            match backend.plugin_get_config(&id).await {
                Ok(result) if !result.keys.is_empty() => {
                    let rows = norte_frontend::plugin_config::sanitize_config_keys(&result.keys);
                    let (plugin_name, _) = crate::app::display_name(name.as_bytes());
                    if let Some(mgr) = &mut app.extensions {
                        mgr.config = Some(crate::app::PluginConfigPanel {
                            plugin_id: id,
                            plugin_name,
                            state: norte_frontend::plugin_config::PluginConfigState::new(rows),
                        });
                    }
                }
                Ok(_) => app.message = Some(t("msg-plugin-config-empty")),
                Err(e) => app.message = Some(error_message(&e)),
            }
        }
        _ => {} // fuera del allowlist de este overlay: inerte
    }
}

/// Persiste UN [`norte_frontend::plugin_config::PendingConfigWrite`] vía
/// `Backend::plugin_set_config` y anuncia el resultado (G3c) — factorizado
/// fuera de [`on_extensions_key`] porque el mismo commit ocurre desde DOS
/// sitios (edición inline confirmada con Enter, y un `bool`/`enum` que
/// cicla de inmediato en `dialog.confirm`).
async fn commit_plugin_config_write(
    app: &mut App,
    backend: &Backend,
    plugin_id: &str,
    write: norte_frontend::plugin_config::PendingConfigWrite,
) {
    match backend
        .plugin_set_config(plugin_id, &write.key, &write.value)
        .await
    {
        Ok(()) => {
            app.message = Some(ta(
                "msg-plugin-config-saved",
                &[("key", &write.key), ("value", &write.display)],
            ));
            // Un ajuste puede cambiar lo que un decorador pinta —el estilo
            // de los iconos—: los listados se vuelven a pedir.
            app.redecorate = true;
        }
        Err(e) => app.message = Some(error_message(&e)),
    }
}

#[cfg(test)]
mod extensions_help_tests {
    use super::{App, ExtensionManager, extensions_help};
    use crate::app::Pane;
    use norte_vfs::VPath;

    fn app_con(plugins: Vec<norte_proto::methods::PluginInfo>) -> App {
        let d = VPath::parse("file:///x").expect("wire de test");
        let mut app = App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()));
        app.extensions = Some(ExtensionManager {
            plugins,
            errors: Vec::new(),
            cursor: 0,
            config: None,
        });
        app
    }

    fn plugin(id: &str, has_help: bool) -> norte_proto::methods::PluginInfo {
        norte_proto::methods::PluginInfo {
            id: id.to_owned(),
            name: id.to_owned(),
            publisher: "ACME".to_owned(),
            version: "1.0.0".to_owned(),
            category: "command".to_owned(),
            capabilities: Vec::new(),
            approved: true,
            enabled: true,
            description: None,
            commands: Vec::new(),
            columns: Vec::new(),
            panels: Vec::new(),
            has_help,
            manifest_digest: None,
        }
    }

    /// H3e: `F1` sobre la fila de un plugin con `help.md` abre la ayuda EN SU
    /// página, con el catálogo que el gestor ya tenía — sin pasar por la
    /// lateral y sin una segunda ida al daemon.
    #[test]
    fn f1_sobre_un_plugin_con_ayuda_abre_su_pagina() {
        let mut app = app_con(vec![plugin("acme.ftp", true)]);
        extensions_help(&mut app, norte_help::Lang::En, &[]);
        let help = app.help.as_ref().expect("la ayuda se abrió");
        assert_eq!(help.state.current().as_str(), "acme.ftp");
        assert!(
            app.extensions.is_none(),
            "el gestor se cierra: su rama va ANTES en la cadena de teclas y se \
             comería las teclas de la página"
        );
        // La página llega como RAÍZ del rastro: al lector lo PUSIERON ahí, así
        // que un `Esc` tiene que salir, no volver a un índice que no visitó.
        assert!(!app.help.as_mut().expect("abierta").state.back());
    }

    /// Y sobre una fila sin `help.md` se DICE. La fila es idéntica a una que sí
    /// la tiene, y una tecla que calla no se distingue de una rota.
    #[test]
    fn f1_sobre_un_plugin_sin_ayuda_lo_dice_y_no_cierra_el_gestor() {
        let mut app = app_con(vec![plugin("acme.ftp", false)]);
        extensions_help(&mut app, norte_help::Lang::En, &[]);
        assert!(app.help.is_none(), "no hay página que abrir");
        assert!(app.extensions.is_some(), "el gestor se queda donde estaba");
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("msg-extensions-no-help").as_str())
        );
    }
}

/// Revoca la aprobación de un plugin y RELISTA.
///
/// Revocar va en la dirección segura, así que no pregunta.
async fn revocar_o_decir(app: &mut App, backend: &Backend, id: &str) {
    // Sin ancla a propósito (#282): revocar no concede nada, y rehusarlo por
    // un digest rancio dejaría vivo justo el permiso que se quiere quitar.
    match backend.plugins_set_approval(id, false, None).await {
        Ok(()) => relistar_extensiones(app, backend).await,
        Err(e) => app.message = Some(error_message(&e)),
    }
}

/// Desinstala —ya confirmado por un humano que leyó qué se pierde (ADR
/// 0104)— y RELISTA. Un fallo se dice y se relista igual, por la misma
/// razón que al conceder: la pantalla enseña lo que el core cree.
pub(crate) async fn desinstalar_confirmada(app: &mut App, backend: &Backend, id: &str) {
    match backend.plugins_uninstall(id).await {
        Ok(_) => relistar_extensiones(app, backend).await,
        Err(e) => {
            app.message = Some(error_message(&e));
            relistar_extensiones(app, backend).await;
        }
    }
}

/// Concede la aprobación —ya confirmada por un humano— y RELISTA.
pub(crate) async fn conceder_aprobacion(
    app: &mut App,
    backend: &Backend,
    id: &str,
    digest: Option<&str>,
) {
    match backend.plugins_set_approval(id, true, digest).await {
        Ok(()) => relistar_extensiones(app, backend).await,
        Err(e) => {
            // Un fallo se DICE **y** se relista: un plazo vencido, o un
            // daemon que rehúsa, no es «no pasó nada» — y la pantalla tiene
            // que enseñar lo que el core cree, no lo que este proceso
            // esperaba.
            app.message = Some(error_message(&e));
            relistar_extensiones(app, backend).await;
        }
    }
}

/// Vuelve a pedirle el catálogo al core y repinta la pantalla con ÉL.
///
/// El camino anterior era `set_local_approved`: un `bool` de este proceso que
/// el daemon no había confirmado. La razón por la que no vale está escrita en
/// la ventana gráfica, que ya lo hacía así — «un optimismo local que el
/// daemon no confirmó es una pantalla que miente sobre quién puede leer tus
/// ficheros» (#280).
async fn relistar_extensiones(app: &mut App, backend: &Backend) {
    // Lo que los plugins dijeron de cada listado lo dijeron con el catálogo
    // de antes: el bucle lo olvida y lo vuelve a pedir.
    app.redecorate = true;
    let cursor = app.extensions.as_ref().map_or(0, |m| m.cursor);
    match backend.plugins_list().await {
        Ok(list) => {
            let mut plugins = list.plugins;
            crate::app::clamp_plugin_descriptions(&mut plugins);
            // Este es el refresco de después de aprobar, activar o desinstalar
            // (fase 3): si el catálogo se relee, lo aportado se redeclara.
            app.kinds.insert_panels(&plugins);
            let config = app.extensions.as_mut().and_then(|m| m.config.take());
            let tope = plugins.len().saturating_sub(1);
            app.extensions = Some(crate::app::ExtensionManager {
                plugins,
                errors: list.errors,
                cursor: cursor.min(tope),
                config,
            });
        }
        Err(e) => app.message = Some(error_message(&e)),
    }
}

/// Conceder capabilities PREGUNTA (#280).
#[cfg(test)]
mod aprobacion_tests {
    use super::{App, ExtensionManager};
    use crate::app::{Modal, Pane};
    use norte_vfs::VPath;

    fn app_con(p: norte_proto::methods::PluginInfo) -> App {
        let d = VPath::parse("file:///x").expect("wire de test");
        let mut app = App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()));
        app.extensions = Some(ExtensionManager {
            plugins: vec![p],
            errors: Vec::new(),
            cursor: 0,
            config: None,
        });
        app
    }

    fn plugin(approved: bool, enabled: bool) -> norte_proto::methods::PluginInfo {
        norte_proto::methods::PluginInfo {
            id: "org.acme.demo".to_owned(),
            name: "Demo".to_owned(),
            publisher: "ACME".to_owned(),
            version: "1.0.0".to_owned(),
            category: "command".to_owned(),
            capabilities: vec!["location".to_owned(), "process".to_owned()],
            approved,
            enabled,
            description: None,
            commands: Vec::new(),
            columns: Vec::new(),
            panels: Vec::new(),
            has_help: false,
            manifest_digest: None,
        }
    }

    /// `dialog.approve` sobre una extensión SIN aprobar abre la pregunta y no
    /// concede nada todavía. Con el camino viejo esto llamaba al daemon en la
    /// misma tecla, sin enumerar una sola capability.
    #[tokio::test]
    async fn conceder_abre_la_pregunta_y_enumera_las_capabilities() {
        let mut app = app_con(plugin(false, false));
        // Un engine embebido y vacío: estos dos tests comprueban que NO se
        // llama al daemon, así que lo que haya detrás da igual mientras
        // exista.
        let backend =
            norte_core::backend::Backend::Embedded(std::sync::Arc::new(norte_core::Engine::new()));

        super::on_extensions_list_cmd(&mut app, &backend, "dialog.approve").await;

        let Some(Modal::ConfirmPluginApproval { id, caps, .. }) = &app.modal else {
            panic!("conceder tiene que preguntar: {:?}", app.modal);
        };
        assert_eq!(id, "org.acme.demo");
        assert_eq!(caps.len(), 2, "una línea por capability: {caps:?}");
    }

    /// Y encender lo que no está aprobado se REHÚSA: un plugin apagado y sin
    /// aprobar no puede saltarse la pregunta por la otra tecla.
    #[tokio::test]
    async fn encender_sin_aprobar_se_rehusa() {
        let mut app = app_con(plugin(false, false));
        // Un engine embebido y vacío: estos dos tests comprueban que NO se
        // llama al daemon, así que lo que haya detrás da igual mientras
        // exista.
        let backend =
            norte_core::backend::Backend::Embedded(std::sync::Arc::new(norte_core::Engine::new()));

        super::on_extensions_list_cmd(&mut app, &backend, "dialog.toggle-enabled").await;

        assert!(app.modal.is_none(), "no abre ninguna pregunta");
        assert!(app.message.is_some(), "y lo DICE en vez de callarse");
    }
}
