//! GUI settings view (S4): the "VSCode-style" full-view swap opened by
//! `app.settings` (F11) — search + a grouped list (General/Plugins) with
//! descriptions, mouse AND keyboard. Built on
//! [`norte_frontend::settings::SettingsState`], the SAME pure editor the
//! TUI's `app.settings` overlay (S3) uses — hoisted out of the TUI because
//! it had zero TUI-specific coupling (see that module's doc). This file
//! owns the pure state ([`SettingsView`]) and keyboard routing ([`on_key`]),
//! testable without GPUI — same split as [`crate::modal`]
//! ([`crate::modal::on_key`] here, `NorteGui::render_modal` in `main.rs`):
//! rendering ([`crate::NorteGui`]'s `render_settings`) and the async
//! persist/live-apply orchestration (`commit_settings_write`/
//! `apply_settings_write_result`) live in `main.rs`, since they need GPUI's
//! `Context`. Mouse row clicks are ALSO handled directly in `main.rs` (same
//! split as pane rows, `on_row_click`): they call [`SettingsState::set_cursor`]
//! + the same activate path `enter` uses here.

use norte_frontend::settings::{PendingWrite, SettingsEditError, SettingsState, wire_key};

/// The status line under the row list: the result of the last write
/// (saved/failed), or `None` before any edit this session. Scoped to this
/// screen — the GUI has no shared banner surface an ephemeral settings
/// message belongs on (the startup banner is a one-shot arena, see
/// `NorteGui::keymap_error`).
#[derive(Debug, Clone)]
pub struct SettingsStatus {
    /// Localized, already-safe-to-paint text (Fluent + typed data only,
    /// same discipline as the rest of the GUI's banners — never raw I/O
    /// `Display`).
    pub message: String,
    /// `true` paints it with the error role instead of the normal one.
    pub error: bool,
}

/// Full-view state: the shared pure editor plus this screen's status line.
#[derive(Debug)]
pub struct SettingsView {
    /// The shared editor (search/cursor/edit-buffer, S3/S4 hoist).
    pub state: SettingsState,
    /// Result of the last write, if any yet.
    pub status: Option<SettingsStatus>,
}

impl SettingsView {
    /// Opens the view over `rows` (a `norte_frontend::settings::build_rows`
    /// snapshot — the caller, `NorteGui::open_settings`, builds it from the
    /// cached config snapshot, never by reading disk here: rule 2).
    #[must_use]
    pub fn new(rows: Vec<norte_frontend::settings::Row>) -> Self {
        Self {
            state: SettingsState::new(rows),
            status: None,
        }
    }
}

/// What the caller (`main.rs`) must do after a key.
#[derive(Debug)]
pub enum SettingsOutcome {
    /// Nothing to do beyond a repaint.
    None,
    /// Esc while browsing: close the view.
    Close,
    /// A value cycled or an inline edit committed — persist it.
    Write(Box<PendingWrite>),
    /// An inline edit's buffer rejected (e.g. `Int` out of range) —
    /// unpersisted, buffer intact; the caller shows the message.
    Invalid(SettingsEditError),
}

/// Whether a successful write of `id` (a `norte_frontend::settings::catalog`
/// id, e.g. `"ui.theme"`) applies WITHOUT restarting the GUI (S4): almost
/// the whole catalog does — theme, fonts, reduce-motion, confirm-quit,
/// quick-search and keymap-preset all re-resolve from the freshly reloaded
/// config (`NorteGui::apply_settings_write_result`). Only `ui.lang` can't:
/// Fluent negotiates the active language once at process startup (`main`,
/// before the window opens) and this frontend has no live-reload path for
/// it. Unlike `SettingDef::applies_live` (the TUI's own view — `true` for
/// everything in v1), this is the GUI's per-entry answer; see that field's
/// doc for why the two frontends differ. Used for the static "restart
/// required" row badge (rendering) — the write path's own dispatch
/// (`apply_settings_write_result`) is the actual source of truth for what
/// it DOES apply live; keep the two in sync when a new catalog entry lands.
#[must_use]
pub fn gui_applies_live(id: &str) -> bool {
    !matches!(wire_key(id), ("ui", key) if key == "lang")
}

/// The single char `key` types, respecting layout/shift (`key_char`) with a
/// fallback to the named key itself — same fidelity contract as the pane
/// quick-search (`NorteGui::quick_key`) and the viewer's chord adapter.
/// `"space"` arrives named, not as a literal char.
fn typed_char(key: &str, key_char: Option<&str>) -> Option<char> {
    if key == "space" {
        return Some(' ');
    }
    single_char(key_char).or_else(|| single_char(Some(key)))
}

/// The first char of `s` if `s` is EXACTLY one — local copy of
/// `crate::single_char` (kept private/duplicated on purpose: a 4-line pure
/// helper isn't worth a `pub(crate)` surface on `main.rs` for one caller).
fn single_char(s: Option<&str>) -> Option<char> {
    let s = s?;
    let mut it = s.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// Keyboard routing (GPUI key names, e.g. `"escape"`, `"backspace"`, a
/// literal letter): mirrors the TUI's `on_settings_key` (same widget
/// conventions — free filter always active, Enter cycles/opens, Esc
/// closes/cancels) adapted to GPUI's string key names instead of
/// crossterm's `KeyCode`. The caller (`NorteGui::on_settings_key`) has
/// already gated ctrl/alt/platform modifiers out before calling this (a
/// ctrl-chord must never be typed into the filter/edit buffer).
pub fn on_key(view: &mut SettingsView, key: &str, key_char: Option<&str>) -> SettingsOutcome {
    let s = &mut view.state;
    if s.is_editing() {
        match key {
            "backspace" => {
                s.edit_backspace();
                SettingsOutcome::None
            }
            "escape" => {
                s.edit_cancel();
                SettingsOutcome::None
            }
            "enter" => match s.edit_commit() {
                Ok(w) => SettingsOutcome::Write(Box::new(w)),
                Err(e) => SettingsOutcome::Invalid(e),
            },
            _ => {
                if let Some(c) = typed_char(key, key_char) {
                    s.edit_push_char(c);
                }
                SettingsOutcome::None
            }
        }
    } else {
        match key {
            "escape" => SettingsOutcome::Close,
            "backspace" => {
                s.backspace();
                SettingsOutcome::None
            }
            "up" => {
                s.up();
                SettingsOutcome::None
            }
            "down" => {
                s.down();
                SettingsOutcome::None
            }
            "pageup" => {
                s.page_up(super::PAGE);
                SettingsOutcome::None
            }
            "pagedown" => {
                s.page_down(super::PAGE);
                SettingsOutcome::None
            }
            "enter" => activate(s),
            _ => {
                if let Some(c) = typed_char(key, key_char) {
                    s.push_char(c);
                }
                SettingsOutcome::None
            }
        }
    }
}

/// Cycles/opens the row under the cursor — the Enter path AND the mouse
/// click path (`main.rs`, which calls `state.set_cursor(idx)` first) share
/// this. Live theme/preset name lists are resolved HERE (like the TUI's
/// `App::open_theme_picker`/Enter branch): they can change without a
/// restart, so `&'static` would be wrong.
pub fn activate(s: &mut SettingsState) -> SettingsOutcome {
    let theme_names: Vec<String> = norte_theme::preset_names()
        .into_iter()
        .map(String::from)
        .collect();
    match s.activate(&theme_names, crate::keymap::KNOWN_PRESETS) {
        Some(w) => SettingsOutcome::Write(Box::new(w)),
        None => SettingsOutcome::None,
    }
}

/// Message for a [`SettingsEditError`] (mirrors the TUI's
/// `settings_edit_error_message`) — by CATEGORY, never the raw buffer (it's
/// user-typed but the error itself carries no user text to leak, so this is
/// belt-and-suspenders, not a real hazard).
#[must_use]
pub fn edit_error_message(e: &SettingsEditError) -> String {
    match e {
        SettingsEditError::NotAnInt => norte_i18n::t("msg-settings-invalid-int"),
        SettingsEditError::OutOfRange { min, max } => norte_i18n::ta(
            "msg-settings-invalid-range",
            &[("min", &min.to_string()), ("max", &max.to_string())],
        ),
    }
}

#[cfg(test)]
mod tests {
    use norte_frontend::settings::{SettingKind, build_rows, catalog};

    use super::*;

    fn cfg_vacia() -> norte_frontend::config::FrontendConfig {
        norte_frontend::config::load(&norte_config::Layers { dirs: vec![] })
            .expect("config vacía carga")
    }

    fn view() -> SettingsView {
        SettingsView::new(build_rows(&cfg_vacia()))
    }

    #[test]
    fn escape_navegando_cierra() {
        let mut v = view();
        assert!(matches!(
            on_key(&mut v, "escape", None),
            SettingsOutcome::Close
        ));
    }

    #[test]
    fn escritura_de_texto_filtra_y_no_activa_atajos() {
        let mut v = view();
        for c in "confirm-quit".chars() {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            assert!(matches!(on_key(&mut v, s, Some(s)), SettingsOutcome::None));
        }
        assert_eq!(v.state.visible().len(), 1);
    }

    #[test]
    fn espacio_nombrado_se_teclea_como_caracter() {
        let mut v = view();
        on_key(&mut v, "u", Some("u"));
        on_key(&mut v, "i", Some("i"));
        on_key(&mut v, "space", None);
        assert_eq!(v.state.query_display(), "ui ");
    }

    #[test]
    fn enter_sobre_bool_cicla_y_devuelve_write() {
        let mut v = view();
        for c in "reduce-motion".chars() {
            v.state.push_char(c);
        }
        assert_eq!(v.state.visible().len(), 1);
        match on_key(&mut v, "enter", None) {
            SettingsOutcome::Write(w) => {
                assert_eq!(w.section, "ui");
                assert_eq!(w.key, "reduce_motion");
                assert_eq!(w.value.as_bool(), Some(true));
            }
            other => panic!("esperaba Write, vino {other:?}"),
        }
    }

    #[test]
    fn enter_sobre_text_abre_edicion_y_escape_cancela() {
        let mut v = view();
        for c in "ui.font ".chars() {
            v.state.push_char(c);
        }
        assert!(matches!(
            on_key(&mut v, "enter", None),
            SettingsOutcome::None
        ));
        assert!(v.state.is_editing());
        assert!(matches!(
            on_key(&mut v, "escape", None),
            SettingsOutcome::None
        ));
        assert!(
            !v.state.is_editing(),
            "escape en edición cancela, no cierra la vista"
        );
    }

    #[test]
    fn editando_int_fuera_de_rango_devuelve_invalid() {
        let mut v = view();
        for c in "font-size".chars() {
            v.state.push_char(c);
        }
        on_key(&mut v, "enter", None);
        for c in "999".chars() {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            on_key(&mut v, s, Some(s));
        }
        match on_key(&mut v, "enter", None) {
            SettingsOutcome::Invalid(SettingsEditError::OutOfRange { min, max }) => {
                assert_eq!((min, max), (8, 32));
            }
            other => panic!("esperaba Invalid(OutOfRange), vino {other:?}"),
        }
        assert!(
            v.state.is_editing(),
            "el buffer se conserva tras el rechazo"
        );
    }

    #[test]
    fn gui_applies_live_es_false_solo_para_ui_lang() {
        for def in catalog() {
            assert_eq!(
                gui_applies_live(def.id),
                def.id != "ui.lang",
                "id={}",
                def.id
            );
        }
    }

    #[test]
    fn activate_sobre_preset_name_cicla_sobre_known_presets() {
        let mut v = view();
        for c in "keymap.preset".chars() {
            v.state.push_char(c);
        }
        let def = catalog()
            .iter()
            .find(|d| d.id == "keymap.preset")
            .expect("keymap.preset está en el catálogo");
        assert!(matches!(def.kind, SettingKind::PresetName));
        match activate(&mut v.state) {
            SettingsOutcome::Write(w) => assert_eq!(w.value.as_str(), Some("vim")),
            other => panic!("esperaba Write, vino {other:?}"),
        }
    }

    #[test]
    fn edit_error_message_no_esta_vacio_para_cada_variante() {
        assert!(!edit_error_message(&SettingsEditError::NotAnInt).is_empty());
        assert!(!edit_error_message(&SettingsEditError::OutOfRange { min: 1, max: 2 }).is_empty());
    }
}
