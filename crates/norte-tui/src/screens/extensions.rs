//! The extension manager (G3): the plugin list, its one-key help, and each
//! plugin's `[config]` panel.
//!
//! It used to live in the `ntc` binary's root — a crate DISTINCT from this
//! lib — so neither the integration tests could feed it a key nor assert its
//! help without the event loop acting as go-between.
//!
//! A plugin is THIRD-PARTY code: everything that arrives from it — id, name,
//! output, `[config]` values — goes through `detail_for_bar` before touching
//! the bar (#73 pattern), and authorization to run a command always belongs
//! to the SERVER, never to whatever snapshot this client has frozen.

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
    // H3d: the same freeze as `open_contextual_help` — the help opened from
    // the manager is the same help.
    app.freeze_help_facts();
    app.help = Some(HelpView::new(lang, help_lines.to_vec()));
    app.freeze_help_plugins(&plugins);
    if let Some(help) = app.help.as_mut() {
        help.state.open_as_root(&id);
    }
}

/// Keys of the extensions overlay (M4-P3), resolved against the keymap's
/// `dialog` context (H1 T2, issue #24); `ctrl+c` keeps its global quit,
/// hardcoded BEFORE resolving. Rule 7: approving/enabling travels to the
/// core through the `Backend`; the LOCAL bool only toggles after an OK
/// (immediate feedback with no re-listing). The id and state are taken
/// BEFORE the `.await` (the `mgr` borrow is released during the backend
/// call and reacquired afterward to reflect the result). This overlay's
/// allowlist: `dialog.up/down/cancel/approve/toggle-enabled` — `approve`
/// toggles the plugin's APPROVAL (decision 3 of the H1 plan: "approving a
/// plugin" semantically reuses `dialog.approve`; it used to be the
/// hardcoded `a` key, now `a` is `dialog.add`, which this overlay does not
/// support).
///
/// Outside the allowlist, one more key: `app.help` (H3e) opens the
/// highlighted plugin's page — see [`extensions_help`].
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
    if button_arrow(app, mods, code) {
        resolver.reset();
        return;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return; // key not modeled by the keymap: ignore
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        // Sequence in progress, or a key bound to something this build does
        // not run (K1 T4): ignore and reset the resolution state.
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return;
        }
        Resolution::Reset => return,
    };
    let panel_open = app.extensions.as_ref().is_some_and(|m| m.config.is_some());
    // With focus on a button of the card (`tab`), Enter fires THAT button.
    // Done by SUBSTITUTING the command here, before the allowlist, and not
    // by calling dispatch a second time: one of the buttons is
    // `dialog.confirm` — the settings one — so re-entering would be a loop.
    // Substituted once, `dialog.confirm` goes back to meaning what it means
    // in the list, which is exactly what that button does.
    let cmd = focused_button(app)
        .filter(|_| !panel_open && cmd == "dialog.confirm")
        .unwrap_or(cmd);
    // H3e: `app.help` is a `[global]` command, not a `dialog.*` verb, so it
    // is not in any allowlist of this overlay and without this branch F1
    // would be inert here. It resolves through the keymap like everything
    // else (a rebind of `app.help` also moves this bridge); what is wired
    // is the meaning, not the key. Same criterion as `on_help_key`'s
    // `app.help` branch and F9 in `on_theme_picker_key`. NOT when the
    // `[config]` panel is open: there the reader is editing values, and
    // losing the panel to read prose is not what they asked for.
    if cmd == "app.help" && !panel_open {
        extensions_help(app, lang, help_lines);
        return;
    }
    // H1 T3: the SAME allowlist the generated hint consumes
    // (`hints::DialogHints::build`) — a single source for dispatch and
    // footer. G3c: which allowlist applies depends on whether the
    // `[config]` panel is open.
    let allow: &[&str] = if panel_open {
        ALLOW_PLUGIN_CONFIG
    } else {
        ALLOW_EXTENSIONS
    };
    if !allow.contains(&cmd.as_str()) {
        return; // outside this context's allowlist: inert
    }
    if panel_open {
        on_plugin_config_panel_cmd(app, backend, &cmd).await;
    } else {
        on_extensions_list_cmd(app, backend, &cmd).await;
    }
}

/// `←`/`→` with no modifiers cycle the card's button ring, like `tab`
/// forward and backward. Returns whether it consumed the key.
///
/// Wired to the key, not the keymap, on purpose: no preset binds
/// `left`/`right` in `[dialog]`, and a button row that does not cycle with
/// the arrows is what is strange, not the configurable part. With the
/// settings panel open, `←` is "back": it closes it and leaves focus on the
/// button it was entered through; `→` does nothing there.
fn button_arrow(app: &mut App, mods: KeyModifiers, code: KeyCode) -> bool {
    if !mods.is_empty() || !matches!(code, KeyCode::Left | KeyCode::Right) {
        return false;
    }
    let Some(mgr) = &mut app.extensions else {
        return false;
    };
    if mgr.config.is_some() {
        if code == KeyCode::Left {
            mgr.config = None;
        }
        return true;
    }
    let buttons = crate::mouse::painted_extension_buttons(app).len();
    if let Some(mgr) = &mut app.extensions {
        mgr.foco = if code == KeyCode::Right {
            crate::app::siguiente_foco(mgr.foco, buttons)
        } else {
            crate::app::anterior_foco(mgr.foco, buttons)
        };
    }
    true
}

/// The command of the button focus points at, if focus is on one and that
/// button was painted the last frame.
///
/// `None` when focus is on the list — the usual case — and also when it
/// points past the painted buttons: the card could have shrunk between the
/// frame and the key, and firing "the fourth button" of a card that now has
/// three would fire a different verb from the one that was read.
fn focused_button(app: &App) -> Option<String> {
    let crate::app::ExtFoco::Boton(i) = app.extensions.as_ref()?.foco else {
        return None;
    };
    crate::mouse::painted_extension_buttons(app)
        .get(i)
        .map(|c| (*c).to_owned())
}

/// A click in the manager: a card button, or the row already chosen.
///
/// The SAME dispatch as the key (`on_extensions_list_cmd`, and for
/// `app.help` the same bridge as `F1`): a button that turned on an
/// extension one way and the key another would be two managers drifting
/// apart as soon as one grows a detail (ADR 0077, within a single
/// frontend). What the button does NOT do is go through the settings
/// panel's allowlist: the click is explicit, and turning off an extension
/// with its settings on screen is exactly what the reader asked for.
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

/// G3c: RAW keys while a `[config]` `string`/`int` is being edited
/// (`on_extensions_key`'s `editing` guard) — same idiom as
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

/// G3c: resolved commands (`up`/`down`/`confirm`/`cancel`) while the
/// `[config]` panel is open and NOTHING is being edited
/// (`on_extensions_key`'s `panel_open` branch — `allow == ALLOW_PLUGIN_CONFIG`).
async fn on_plugin_config_panel_cmd(app: &mut App, backend: &Backend, cmd: &str) {
    let Some(panel) = app.extensions.as_mut().and_then(|m| m.config.as_mut()) else {
        return;
    };
    match cmd {
        "dialog.up" => panel.state.up(),
        "dialog.down" => panel.state.down(),
        // `tab` exits just like `Esc`: the panel is entered from the button
        // ring with `tab`, and without this it was the only key on the
        // circuit that left the reader locked inside.
        "dialog.cancel" | "dialog.pane" => {
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

/// The rest of `on_extensions_key`: commands over the plugin LIST
/// (`panel_open == false`, `allow == ALLOW_EXTENSIONS`) — navigating,
/// approving/enabling, and `dialog.confirm` (G3c) opens the highlighted
/// plugin's `[config]` panel IF it declares any key. Enter NEVER approves
/// (pin P1): it only enters a submenu.
async fn on_extensions_list_cmd(app: &mut App, backend: &Backend, cmd: &str) {
    // `tab`: the list, each card button, the list. The stops are whatever
    // the last frame PAINTED (`painted_extension_buttons`), so with a
    // narrow box — no card — the ring has only one and the key does
    // nothing, instead of moving an invisible focus.
    if cmd == "dialog.pane" {
        let buttons = crate::mouse::painted_extension_buttons(app).len();
        if let Some(mgr) = &mut app.extensions {
            mgr.foco = crate::app::siguiente_foco(mgr.foco, buttons);
        }
        return;
    }
    let Some(mgr) = &mut app.extensions else {
        return;
    };
    match cmd {
        "dialog.up" => mgr.up(),
        "dialog.down" => mgr.down(),
        "dialog.cancel" => app.extensions = None,
        // GRANTING asks; REVOKING does not (#280). The asymmetry is that of
        // this whole tree: what goes in the safe direction needs no
        // permission, and granting capabilities is THE security decision of
        // the extension system — the graphical window already asked, and
        // here it was approved with a key, enumerating nothing.
        "dialog.approve" => {
            let Some(sel) = mgr.selected() else {
                if mgr.selected_broken().is_some() {
                    app.message = Some(t("ext-broken-only-uninstall"));
                }
                return;
            };
            if sel.approved {
                let id = sel.id.clone();
                revoke_or_say(app, backend, &id).await;
                return;
            }
            let (id, name) = (sel.id.clone(), sel.name.clone());
            let digest = sel.manifest_digest.clone();
            // The capabilities, each one masked ON ITS OWN and with its
            // flag: they are third-party text, and pasting them into one
            // sentence lets one impersonate another.
            let caps: Vec<(String, bool)> = sel
                .capabilities
                .iter()
                .map(|c| norte_frontend::help_badge::plugin_label_flagged(c))
                .collect();
            let (name, hostile) = crate::app::display_name(name.as_bytes());
            app.modal = Some(crate::app::Modal::ConfirmPluginApproval {
                id,
                name,
                name_hostile: hostile,
                caps,
                digest,
            });
        }
        "dialog.toggle-enabled" => {
            let Some((id, cur, approved)) = mgr
                .selected()
                .map(|p| (p.id.clone(), p.enabled, p.approved))
            else {
                if mgr.selected_broken().is_some() {
                    app.message = Some(t("ext-broken-only-uninstall"));
                }
                return;
            };
            // Turning on what is not approved, no. Turning off what is —
            // even if its approval was revoked —, yes: turning off always
            // goes in the safe direction.
            if !cur && !approved {
                app.message = Some(t("msg-plugin-not-approved"));
                return;
            }
            match backend.plugins_set_enabled(&id, !cur).await {
                Ok(()) => relist_extensions(app, backend).await,
                Err(e) => app.message = Some(error_message(&e)),
            }
        }
        // Uninstalling (ADR 0104) ALWAYS asks: it deletes files and
        // withdraws consent, with no going back. Same mold as granting —
        // the sanitized name with its flag, the id apart — and same
        // confirmation gate as deleting files.
        "dialog.remove" => {
            let Some((id, name)) = mgr.selected().map(|p| (p.id.clone(), p.name.clone())) else {
                ask_uninstall_broken(app);
                return;
            };
            let (name, hostile) = crate::app::display_name(name.as_bytes());
            app.modal = Some(crate::app::Modal::ConfirmPluginUninstall {
                id,
                name,
                name_hostile: hostile,
            });
        }
        "dialog.confirm" => {
            let Some((id, name)) = mgr.selected().map(|p| (p.id.clone(), p.name.clone())) else {
                if mgr.selected_broken().is_some() {
                    app.message = Some(t("ext-broken-only-uninstall"));
                }
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
        _ => {} // outside this overlay's allowlist: inert
    }
}

/// Persists ONE [`norte_frontend::plugin_config::PendingConfigWrite`] via
/// `Backend::plugin_set_config` and announces the result (G3c) — factored
/// out of [`on_extensions_key`] because the same commit happens from TWO
/// places (inline editing confirmed with Enter, and a `bool`/`enum` that
/// cycles immediately on `dialog.confirm`).
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
            // A setting can change what a decorator paints — icon style —:
            // listings are requested again.
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

    fn app_with(plugins: Vec<norte_proto::methods::PluginInfo>) -> App {
        let d = VPath::parse("file:///x").expect("test wire");
        let mut app = App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()));
        app.extensions = Some(ExtensionManager {
            plugins,
            errors: Vec::new(),
            cursor: 0,
            config: None,
            foco: crate::app::ExtFoco::Lista,
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

    /// H3e: `F1` on a plugin row that has `help.md` opens help ON ITS PAGE,
    /// with the catalogue the manager already had — with no detour through
    /// the sidebar and no second round trip to the daemon.
    #[test]
    fn f1_on_a_plugin_with_help_opens_its_page() {
        let mut app = app_with(vec![plugin("acme.ftp", true)]);
        extensions_help(&mut app, norte_help::Lang::En, &[]);
        let help = app.help.as_ref().expect("help opened");
        assert_eq!(help.state.current().as_str(), "acme.ftp");
        assert!(
            app.extensions.is_none(),
            "the manager closes: its branch goes BEFORE in the key chain and \
             would eat the page's keys"
        );
        // The page arrives as the trail's ROOT: the reader was PUT there,
        // so an `Esc` has to exit, not go back to an index they never
        // visited.
        assert!(!app.help.as_mut().expect("open").state.back());
    }

    /// And over a row with no `help.md` it IS SAID. The row is identical to
    /// one that has it, and a key that stays silent is indistinguishable
    /// from a broken one.
    #[test]
    fn f1_on_a_plugin_with_no_help_says_so_and_does_not_close_the_manager() {
        let mut app = app_with(vec![plugin("acme.ftp", false)]);
        extensions_help(&mut app, norte_help::Lang::En, &[]);
        assert!(app.help.is_none(), "there is no page to open");
        assert!(app.extensions.is_some(), "the manager stays where it was");
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("msg-extensions-no-help").as_str())
        );
    }
}

/// Revokes a plugin's approval and RE-LISTS.
///
/// Revoking goes in the safe direction, so it does not ask.
async fn revoke_or_say(app: &mut App, backend: &Backend, id: &str) {
    // No anchor on purpose (#282): revoking grants nothing, and refusing it
    // over a stale digest would leave alive exactly the permission being
    // withdrawn.
    match backend.plugins_set_approval(id, false, None).await {
        Ok(()) => relist_extensions(app, backend).await,
        Err(e) => app.message = Some(error_message(&e)),
    }
}

/// Uninstalls — already confirmed by a human who read what is lost (ADR
/// 0104) — and RE-LISTS. A failure is reported and re-listed anyway, for
/// the same reason as granting: the screen shows what the core believes.
pub(crate) async fn desinstalar_confirmada(app: &mut App, backend: &Backend, id: &str) {
    match backend.plugins_uninstall(id).await {
        Ok(_) => relist_extensions(app, backend).await,
        Err(e) => {
            app.message = Some(error_message(&e));
            relist_extensions(app, backend).await;
        }
    }
}

/// Grants approval — already confirmed by a human — and RE-LISTS.
pub(crate) async fn conceder_aprobacion(
    app: &mut App,
    backend: &Backend,
    id: &str,
    digest: Option<&str>,
) {
    match backend.plugins_set_approval(id, true, digest).await {
        Ok(()) => relist_extensions(app, backend).await,
        Err(e) => {
            // A failure is REPORTED **and** re-listed: an expired deadline,
            // or a daemon that refuses, is not "nothing happened" — and the
            // screen has to show what the core believes, not what this
            // process expected.
            app.message = Some(error_message(&e));
            relist_extensions(app, backend).await;
        }
    }
}

/// `dialog.remove` on an extension that did NOT load: it is uninstalled by
/// its directory if it is named like an id — that is what `plugin.uninstall`
/// deletes —, and if not, it says why not. The question is the same as for
/// a loaded one.
fn ask_uninstall_broken(app: &mut App) {
    let Some(mgr) = app.extensions.as_ref() else {
        return;
    };
    let Some(broken) = mgr.selected_broken() else {
        return;
    };
    let Some(id) = norte_frontend::broken_plugin::uninstallable_id(broken, &mgr.plugins) else {
        app.message = Some(t("ext-broken-not-id"));
        return;
    };
    let (name, hostile) =
        crate::app::display_name(broken.dir_bytes.as_deref().unwrap_or(broken.dir.as_bytes()));
    app.modal = Some(crate::app::Modal::ConfirmPluginUninstall {
        id,
        name,
        name_hostile: hostile,
    });
}

/// What a row that did not load is recognized by between two listings: the
/// directory's BYTES if the peer sends them (#265), and its string if not.
/// Not the id — a broken one may not have one — nor the position.
fn broken_key(e: &norte_proto::methods::PluginLoadError) -> Vec<u8> {
    e.dir_bytes
        .clone()
        .unwrap_or_else(|| e.dir.as_bytes().to_vec())
}

/// Which row the cursor points at in the NEW catalogue.
///
/// Identity first — the loaded one's id, the broken one's directory bytes —
/// and the bounded position only if what was chosen is no longer there. The
/// catalogue gets reordered (the core sorts it by category and id) and
/// uninstalling removes a row, so a cursor by position leaves the reader
/// pointing at another, and the next `e` would enable an extension nobody
/// chose. It is what the host already does in `Extensiones::set_catalogo`.
fn cursor_after_relist(
    chosen_id: Option<&str>,
    chosen_broken: Option<&[u8]>,
    plugins: &[norte_proto::methods::PluginInfo],
    errors: &[norte_proto::methods::PluginLoadError],
    previous: usize,
) -> usize {
    chosen_id
        .and_then(|id| plugins.iter().position(|p| p.id == id))
        .or_else(|| {
            chosen_broken.and_then(|key| {
                errors
                    .iter()
                    .position(|e| broken_key(e) == key)
                    .map(|j| plugins.len() + j)
            })
        })
        .unwrap_or_else(|| previous.min((plugins.len() + errors.len()).saturating_sub(1)))
}

/// Asks the core for the catalogue again and repaints the screen with IT.
///
/// The earlier path was `set_local_approved`: a `bool` of this process the
/// daemon had not confirmed. Why that is not good enough is written in the
/// graphical window, which already did it this way — "a local optimism the
/// daemon did not confirm is a screen lying about who can read your files"
/// (#280).
async fn relist_extensions(app: &mut App, backend: &Backend) {
    // What the plugins said about each listing, they said with the earlier
    // catalogue: the loop forgets it and asks again.
    app.redecorate = true;
    let cursor = app.extensions.as_ref().map_or(0, |m| m.cursor);
    // Who was chosen, BY IDENTITY (see `cursor_after_relist`): the id if it
    // was a loaded one, the directory bytes if it was one that did not
    // load.
    let (chosen_id, chosen_broken) = app.extensions.as_ref().map_or((None, None), |m| {
        (
            m.selected().map(|p| p.id.clone()),
            m.selected_broken().map(broken_key),
        )
    });
    // Focus travels by hand, like the cursor and for the same reason: this
    // re-listing is the one AFTER pressing a button, and losing focus here
    // would return the keyboard to the list right when the reader just used
    // the card. What the button says can change (turn on ↔ turn off); how
    // many there are, not.
    let foco = app
        .extensions
        .as_ref()
        .map_or_else(Default::default, |m| m.foco);
    match backend.plugins_list().await {
        Ok(list) => {
            let mut plugins = list.plugins;
            crate::app::clamp_plugin_descriptions(&mut plugins);
            // This is the refresh after approving, enabling or uninstalling
            // (phase 3): if the catalogue is reread, what it contributes is
            // redeclared.
            app.kinds.insert_panels(&plugins);
            let config = app.extensions.as_mut().and_then(|m| m.config.take());
            let cursor = cursor_after_relist(
                chosen_id.as_deref(),
                chosen_broken.as_deref(),
                &plugins,
                &list.errors,
                cursor,
            );
            app.extensions = Some(crate::app::ExtensionManager {
                plugins,
                errors: list.errors,
                cursor,
                config,
                foco,
            });
        }
        Err(e) => app.message = Some(error_message(&e)),
    }
}

/// The manager's cursor survives a re-list by IDENTITY, not by position.
#[cfg(test)]
mod cursor_after_relist_tests {
    use super::cursor_after_relist;
    use norte_proto::methods::{PluginInfo, PluginLoadError};

    fn plugin(id: &str) -> PluginInfo {
        PluginInfo {
            id: id.to_owned(),
            name: id.to_owned(),
            publisher: String::new(),
            version: "1.0.0".to_owned(),
            category: "command".to_owned(),
            capabilities: Vec::new(),
            approved: true,
            enabled: true,
            description: None,
            commands: Vec::new(),
            columns: Vec::new(),
            panels: Vec::new(),
            has_help: false,
            manifest_digest: None,
        }
    }

    fn broken(dir: &str) -> PluginLoadError {
        PluginLoadError {
            dir: dir.to_owned(),
            reason: "did not load".to_owned(),
            dir_bytes: Some(dir.as_bytes().to_vec()),
        }
    }

    /// The case that brought this on: cursor on the only broken row, it
    /// gets uninstalled, and the catalogue comes back with the two loaded
    /// ones. By position the cursor landed on the SECOND loaded one, and
    /// the next `e` enabled it without anyone choosing it.
    #[test]
    fn uninstalling_the_broken_one_does_not_leave_the_cursor_on_a_loaded_one() {
        let plugins = [plugin("org.a"), plugin("org.b")];
        let cursor = cursor_after_relist(None, Some(b"org.rota"), &plugins, &[], 2);
        assert_eq!(cursor, 1, "the ceiling, not a choice");
        // And what matters: it does not point at anything that was chosen.
        assert!(cursor < plugins.len());
    }

    /// Approving reorders the catalogue (the core sorts by category and
    /// id): the cursor follows ITS extension, it does not stay on the row.
    #[test]
    fn the_cursor_follows_the_id_when_the_catalogue_is_reordered() {
        let before = [plugin("org.b")];
        let after = [plugin("org.a"), plugin("org.b")];
        assert_eq!(
            cursor_after_relist(Some(&before[0].id), None, &after, &[], 0),
            1
        );
    }

    /// A broken one is recognized by its directory's BYTES, and stays
    /// behind the loaded ones even if its position changes.
    #[test]
    fn the_cursor_follows_the_broken_one_by_its_directory() {
        let plugins = [plugin("org.a")];
        let errors = [broken("other"), broken("org.rota")];
        assert_eq!(
            cursor_after_relist(None, Some(b"org.rota"), &plugins, &errors, 1),
            plugins.len() + 1
        );
    }

    /// Empty catalogue: there is no row to point at and the cursor does not
    /// go out of bounds.
    #[test]
    fn an_empty_catalogue_leaves_the_cursor_at_zero() {
        assert_eq!(cursor_after_relist(Some("org.a"), None, &[], &[], 3), 0);
    }
}

/// Granting capabilities ASKS (#280).
#[cfg(test)]
mod approval_tests {
    use super::{App, ExtensionManager};
    use crate::app::{Modal, Pane};
    use norte_vfs::VPath;

    fn app_with(p: norte_proto::methods::PluginInfo) -> App {
        let d = VPath::parse("file:///x").expect("test wire");
        let mut app = App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()));
        app.extensions = Some(ExtensionManager {
            plugins: vec![p],
            errors: Vec::new(),
            cursor: 0,
            config: None,
            foco: crate::app::ExtFoco::Lista,
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

    /// `dialog.approve` on an UNAPPROVED extension opens the question and
    /// grants nothing yet. With the old path this called the daemon on the
    /// same key, without enumerating a single capability.
    #[tokio::test]
    async fn granting_opens_the_question_and_enumerates_the_capabilities() {
        let mut app = app_with(plugin(false, false));
        // An embedded, empty engine: these two tests check that the daemon
        // is NOT called, so whatever is behind it does not matter as long
        // as it exists.
        let backend =
            norte_core::backend::Backend::Embedded(std::sync::Arc::new(norte_core::Engine::new()));

        super::on_extensions_list_cmd(&mut app, &backend, "dialog.approve").await;

        let Some(Modal::ConfirmPluginApproval { id, caps, .. }) = &app.modal else {
            panic!("granting has to ask: {:?}", app.modal);
        };
        assert_eq!(id, "org.acme.demo");
        assert_eq!(caps.len(), 2, "one line per capability: {caps:?}");
    }

    /// And turning on something not approved is REFUSED: a disabled,
    /// unapproved plugin cannot skip the question through the other key.
    #[tokio::test]
    async fn enabling_without_approval_is_refused() {
        let mut app = app_with(plugin(false, false));
        // An embedded, empty engine: these two tests check that the daemon
        // is NOT called, so whatever is behind it does not matter as long
        // as it exists.
        let backend =
            norte_core::backend::Backend::Embedded(std::sync::Arc::new(norte_core::Engine::new()));

        super::on_extensions_list_cmd(&mut app, &backend, "dialog.toggle-enabled").await;

        assert!(app.modal.is_none(), "opens no question");
        assert!(
            app.message.is_some(),
            "and SAYS so instead of staying quiet"
        );
    }

    fn broken(dir: &str) -> norte_proto::methods::PluginLoadError {
        norte_proto::methods::PluginLoadError {
            dir: dir.to_owned(),
            reason: "the manifest does not parse".to_owned(),
            dir_bytes: Some(dir.as_bytes().to_vec()),
        }
    }

    /// An extension that did NOT LOAD is one more row: the cursor goes down
    /// to it, `dialog.remove` asks about it, and the other verbs say so.
    /// Before, the cursor stopped at the last loaded one and a broken one
    /// could only be removed by hand.
    #[tokio::test]
    async fn a_broken_extension_is_pointed_at_and_only_uninstalls() {
        let mut app = app_with(plugin(true, true));
        if let Some(mgr) = &mut app.extensions {
            mgr.errors = vec![broken("org.acme.roto"), broken("not an id")];
            mgr.down();
        }
        let backend =
            norte_core::backend::Backend::Embedded(std::sync::Arc::new(norte_core::Engine::new()));

        super::on_extensions_list_cmd(&mut app, &backend, "dialog.approve").await;
        assert!(app.modal.is_none(), "approving a broken one asks nothing");
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("ext-broken-only-uninstall").as_str())
        );

        super::on_extensions_list_cmd(&mut app, &backend, "dialog.remove").await;
        let Some(Modal::ConfirmPluginUninstall { id, .. }) = &app.modal else {
            panic!("uninstalling a broken one asks: {:?}", app.modal);
        };
        assert_eq!(id, "org.acme.roto");

        app.modal = None;
        app.message = None;
        if let Some(mgr) = &mut app.extensions {
            mgr.down();
        }
        super::on_extensions_list_cmd(&mut app, &backend, "dialog.remove").await;
        assert!(app.modal.is_none(), "with no id there is nothing to ask");
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("ext-broken-not-id").as_str())
        );
    }
}

/// The manager's `tab` ring, with the screen already in front of it: which
/// stops it has and what Enter fires at each one.
#[cfg(test)]
mod foco_tests {
    use super::App;
    use crate::app::{ExtFoco, ExtensionManager, Modal, Pane};
    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::layout::Rect;

    /// An app with the manager open and the last frame's zones already fed
    /// back to the model, which is where the ring gets its stops from.
    /// `width` decides whether there is a card: below `EXTENSIONS_WIDE_MIN`
    /// the box paints a single column and there is no button at all.
    fn painted_app(width: u16) -> App {
        let _ = norte_i18n::force(norte_i18n::Lang::Es);
        let d = norte_vfs::VPath::parse("file:///x").expect("test wire");
        let mut app = App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()));
        app.extensions = Some(ExtensionManager {
            plugins: vec![norte_proto::methods::PluginInfo {
                id: "org.acme.demo".to_owned(),
                name: "Demo".to_owned(),
                publisher: "ACME".to_owned(),
                version: "1.0.0".to_owned(),
                category: "previewer".to_owned(),
                capabilities: vec!["fs-read".to_owned()],
                approved: false,
                enabled: false,
                description: None,
                commands: Vec::new(),
                columns: Vec::new(),
                panels: Vec::new(),
                has_help: false,
                manifest_digest: None,
            }],
            errors: Vec::new(),
            cursor: 0,
            config: None,
            foco: ExtFoco::Lista,
        });
        let area = Rect {
            x: 0,
            y: 0,
            width,
            height: 24,
        };
        let zones = crate::ui::extension_zones(&app, area);
        crate::mouse::after_frame(
            &mut app,
            None,
            crate::mouse::FrameZones {
                extensions: zones,
                ..crate::mouse::FrameZones::default()
            },
        );
        app
    }

    fn backend() -> norte_core::backend::Backend {
        // Empty on purpose: none of these tests reach the daemon.
        norte_core::backend::Backend::Embedded(std::sync::Arc::new(norte_core::Engine::new()))
    }

    fn foco(app: &App) -> ExtFoco {
        app.extensions.as_ref().expect("manager open").foco
    }

    /// `tab` moves focus from the list to the card's first button. Before
    /// the ring this key was inert here — the command existed in the
    /// catalogue and the screen did not handle it — and the buttons only
    /// answered to the mouse.
    #[tokio::test]
    async fn tab_moves_focus_to_the_first_button() {
        let mut app = painted_app(100);
        assert_eq!(foco(&app), ExtFoco::Lista);

        super::on_extensions_list_cmd(&mut app, &backend(), "dialog.pane").await;

        assert_eq!(foco(&app), ExtFoco::Boton(0));
    }

    /// With no card painted — narrow box — `tab` moves nothing: the stops
    /// come from what the frame painted, not from what the card would have
    /// if it fit.
    #[tokio::test]
    async fn with_no_card_tab_does_not_move_focus() {
        let mut app = painted_app(40);
        assert!(
            crate::mouse::painted_extension_buttons(&app).is_empty(),
            "a 40-cell box paints no card"
        );

        super::on_extensions_list_cmd(&mut app, &backend(), "dialog.pane").await;

        assert_eq!(foco(&app), ExtFoco::Lista);
    }

    /// With focus on the SECOND button, Enter fires that button —
    /// approving, which asks — and not settings, which is what Enter means
    /// in the list.
    #[tokio::test]
    async fn enter_on_a_button_fires_that_button() {
        let mut app = painted_app(100);
        for _ in 0..2 {
            super::on_extensions_list_cmd(&mut app, &backend(), "dialog.pane").await;
        }
        assert_eq!(foco(&app), ExtFoco::Boton(1));
        let cmd = super::focused_button(&app).expect("focus points at a painted button");
        assert_eq!(cmd, "dialog.approve", "the card's second button");

        super::on_extensions_list_cmd(&mut app, &backend(), &cmd).await;

        assert!(
            matches!(app.modal, Some(Modal::ConfirmPluginApproval { .. })),
            "approving asks: {:?}",
            app.modal
        );
    }

    /// A focus pointing past the painted buttons fires NOTHING: Enter goes
    /// back to meaning what it means in the list. Firing "button n" of a
    /// card that no longer has an n would run a verb the reader never read,
    /// and uninstalling is among those verbs.
    #[tokio::test]
    async fn an_out_of_bounds_focus_fires_no_other_verb() {
        let mut app = painted_app(100);
        if let Some(mgr) = &mut app.extensions {
            mgr.foco = ExtFoco::Boton(99);
        }

        assert_eq!(super::focused_button(&app), None);
    }

    /// `→` cycles the buttons like `tab`, and `←` goes back: from the list
    /// it jumps to the last one. Up/down are still the list.
    #[tokio::test]
    async fn the_arrows_cycle_the_buttons() {
        let mut app = painted_app(100);
        let buttons = crate::mouse::painted_extension_buttons(&app).len();
        assert!(buttons >= 2, "the card paints several buttons");

        assert!(super::button_arrow(
            &mut app,
            KeyModifiers::NONE,
            KeyCode::Right
        ));
        assert_eq!(foco(&app), ExtFoco::Boton(0));
        assert!(super::button_arrow(
            &mut app,
            KeyModifiers::NONE,
            KeyCode::Right
        ));
        assert_eq!(foco(&app), ExtFoco::Boton(1));
        assert!(super::button_arrow(
            &mut app,
            KeyModifiers::NONE,
            KeyCode::Left
        ));
        assert!(super::button_arrow(
            &mut app,
            KeyModifiers::NONE,
            KeyCode::Left
        ));
        assert_eq!(foco(&app), ExtFoco::Lista);
        assert!(super::button_arrow(
            &mut app,
            KeyModifiers::NONE,
            KeyCode::Left
        ));
        assert_eq!(foco(&app), ExtFoco::Boton(buttons - 1));

        assert!(
            !super::button_arrow(&mut app, KeyModifiers::NONE, KeyCode::Down),
            "down is not one of the buttons'"
        );
        assert!(
            !super::button_arrow(&mut app, KeyModifiers::CONTROL, KeyCode::Right),
            "with a modifier the key still goes to the keymap"
        );
    }

    /// Inside a plugin's settings, `tab` and `←` return to the button ring,
    /// and focus stays on the button it was entered through. Before, only
    /// `Esc` exited, and a reader who entered with `tab` stayed stuck
    /// inside.
    #[tokio::test]
    async fn tab_and_arrow_exit_settings() {
        for exit in ["tab", "left"] {
            let mut app = painted_app(100);
            if let Some(mgr) = &mut app.extensions {
                mgr.foco = ExtFoco::Boton(2);
                mgr.config = Some(crate::app::PluginConfigPanel {
                    plugin_id: "org.acme.demo".into(),
                    plugin_name: "Demo".into(),
                    state: norte_frontend::plugin_config::PluginConfigState::new(Vec::new()),
                });
            }

            if exit == "tab" {
                super::on_plugin_config_panel_cmd(&mut app, &backend(), "dialog.pane").await;
            } else {
                assert!(super::button_arrow(
                    &mut app,
                    KeyModifiers::NONE,
                    KeyCode::Left
                ));
            }

            let mgr = app.extensions.as_ref().expect("manager still open");
            assert!(mgr.config.is_none(), "{exit} closes settings");
            assert_eq!(mgr.foco, ExtFoco::Boton(2), "{exit}: focus is not lost");
        }
    }

    /// Moving the cursor returns focus to the list: the buttons belong to
    /// the chosen plugin, and one staying focused while the cursor moves to
    /// another would be a button of what is no longer being looked at.
    #[tokio::test]
    async fn moving_down_the_list_returns_focus() {
        let mut app = painted_app(100);
        super::on_extensions_list_cmd(&mut app, &backend(), "dialog.pane").await;
        assert_eq!(foco(&app), ExtFoco::Boton(0));

        super::on_extensions_list_cmd(&mut app, &backend(), "dialog.down").await;

        assert_eq!(foco(&app), ExtFoco::Lista);
    }
}
