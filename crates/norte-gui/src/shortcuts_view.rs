//! GUI shortcut editor (K3c c4): the same editor the TUI opened in c3,
//! painted by this frontend and fed by GPUI keystrokes.
//!
//! Everything that decides anything is
//! [`norte_frontend::shortcuts`]: what a row is, when a capture may be
//! confirmed, how a refusal is worded, and — through
//! [`norte_frontend::shortcuts::plan_rebind`] — whether a chord may reach
//! disk at all. What lives here is the part that is genuinely this
//! frontend's: turning a `gpui::Keystroke` into a [`Chord`], and deciding
//! which of this window's screens a rebind can even be about.
//!
//! Same split as [`crate::settings_view`]: the pure state and the keyboard
//! routing are here and testable without GPUI; the painting
//! (`NorteGui::render_shortcuts`) and the async write/reload orchestration
//! (`NorteGui::confirm_shortcut`) live in `main.rs`, which needs a
//! `Context`.
//!
//! # What this frontend does differently from the TUI
//!
//! - **The modifier gate.** `NorteGui::on_settings_key` drops
//!   ctrl/alt/platform before a key can reach the settings filter. Capture
//!   cannot live under that gate — a ctrl-chord IS the thing being captured
//!   — so capture is a MODE checked ahead of the match ([`on_key`]), not
//!   another branch inside it. Outside capture this screen keeps the gate,
//!   for the same reason settings has it: its filter takes free text.
//! - **This window can see ⌘.** `gpui::Modifiers::platform` reaches
//!   [`crate::keymap::gpui_chord`], so a chord captured here may be one
//!   crossterm never delivers (the TUI does not enable the Kitty protocol).
//!   [`Rebind`](norte_frontend::keymap::Rebind) has no variant for
//!   "writable, loadable, and unreachable in the other frontend" — it is
//!   not a refusal, the binding works fine in this window — so it is said
//!   on the row, at capture time, by [`captures_cmd`].
//! - **A pasted codepoint.** The TUI refuses hazardous codepoints because
//!   crossterm hands a paste over as `Char`s. GPUI has its own path to the
//!   same place — an IME or a paste can produce a `key_char` — and the file
//!   being written is read by text editors and by the TUI, so the guard is
//!   this frontend's too ([`hostile_chord`]). REFUSED, not masked: masking
//!   would bind a chord other than the one the file would name. The same
//!   refusal catches a composition that never precomposed — `é` typed as
//!   `e`+U+0301 is two codepoints, so [`crate::keymap::gpui_chord`] declines
//!   to invent one and the reader is told the key cannot be captured. That
//!   is the right answer (binding the base letter would be worse) and the
//!   only case where the wording reads as a bug rather than as a rule.
//! - **A debug build has a fourth.** With `NORTE_GUI_DEBUG` set, `f12` dumps
//!   the a11y tree from the very top of `NorteGui::on_key`, ahead of every
//!   routing branch — so under that variable `f12` is the one key besides a
//!   bare `esc` that this editor cannot capture.

use norte_frontend::keymap::{Chord, Effective, KeyCode, Mods, Screen};
use norte_frontend::shortcuts::{ScreenKeys, ShortcutRow, ShortcutsState, build_rows};

use crate::keys::typed_char;

/// The paging step of `pageup`/`pagedown`, and the FALLBACK row budget.
///
/// It is not what gets painted: `NorteGui::shortcut_rows` measures that
/// against the viewport, because a constant would let the cursor sit under
/// the bottom edge of a short window — and `ctrl+u` deletes the binding
/// under the cursor. Paging by a fixed step over a measured window is the
/// same arrangement `settings_view` has with `norte_frontend::DEFAULT_PAGE`:
/// the cursor stays visible either way, since the painted offset follows it.
pub const ROWS: usize = 20;

/// The status line under the list: the result of the last write, or `None`
/// before any this session. Scoped to this screen for the same reason
/// [`crate::settings_view::SettingsStatus`] is.
#[derive(Debug, Clone)]
pub struct ShortcutsStatus {
    /// Localized, already-safe-to-paint text (Fluent + typed data only).
    pub message: String,
    /// `true` paints it with the error role.
    pub error: bool,
}

/// The editor: the shared pure state plus this screen's status line.
#[derive(Debug)]
pub struct ShortcutsView {
    /// The shared editor (rows/filter/cursor/capture, c3's model).
    pub state: ShortcutsState,
    /// Result of the last write, if any yet.
    pub status: Option<ShortcutsStatus>,
}

impl ShortcutsView {
    /// Opens the editor over `rows` (a [`build_rows`] snapshot — the caller,
    /// `NorteGui::open_shortcuts`, builds it from the LIVE effectives, never
    /// by reading disk here: rule 2).
    #[must_use]
    pub fn new(rows: Vec<ShortcutRow>) -> Self {
        Self {
            state: ShortcutsState::new(rows),
            status: None,
        }
    }
}

/// The LIVE effectives of the screens this window resolves keys through,
/// borrowed from the resolvers that own them.
///
/// Two, not three. The GUI builds a `Screen::Dialog` effective for the
/// reference sheet (`help_view::keys_lines`) but dispatches no dialog verb
/// through it — its overlays hardcode their keys
/// (`crate::keymap::screen_commands` says so in its Dialog arm) — so a
/// dialog row in an EDITOR would offer a rebind that changes this window not
/// at all while silently changing the TUI's. The sheet answers "what does
/// this key do" and may list it; an editor answers "make this key do X" and
/// must not.
#[derive(Debug, Clone, Copy)]
pub struct Maps<'a> {
    /// The dual-pane map (`NorteGui::resolver`).
    pub browse: &'a Effective,
    /// The viewer map (`NorteGui::viewer_resolver`).
    pub viewer: &'a Effective,
}

impl<'a> Maps<'a> {
    /// The map of `screen` — the one a row of that screen was built from,
    /// and the one its verdict must be read off (`Tab` is free in the viewer
    /// and reserved in the browser).
    ///
    /// `None` for [`Screen::Dialog`]: this frontend has no dialog rows (see
    /// the type's doc), and answering with another screen's map would be a
    /// verdict about a different keyboard.
    #[must_use]
    pub fn of(&self, screen: Screen) -> Option<&'a Effective> {
        match screen {
            Screen::Browse => Some(self.browse),
            Screen::Viewer => Some(self.viewer),
            Screen::Dialog => None,
        }
    }
}

/// Every row of the editor for this frontend: the two screens it resolves
/// keys through, each with the commands IT dispatches
/// (`crate::keymap::screen_commands`) as the bindable set.
#[must_use]
pub fn rows(maps: Maps<'_>, lang: norte_i18n::Lang) -> Vec<ShortcutRow> {
    build_rows(
        &[
            ScreenKeys {
                screen: Screen::Browse,
                eff: maps.browse,
                bindable: crate::keymap::screen_commands(Screen::Browse),
            },
            ScreenKeys {
                screen: Screen::Viewer,
                eff: maps.viewer,
                bindable: crate::keymap::screen_commands(Screen::Viewer),
            },
        ],
        lang,
    )
}

/// What the caller (`main.rs`) must do after a key.
#[derive(Debug, PartialEq, Eq)]
pub enum ShortcutsOutcome {
    /// Consumed; nothing pending beyond a repaint.
    None,
    /// `esc` outside capture: close the editor (settings stays open behind).
    Close,
    /// Write the capture, if the door lets it through.
    Confirm,
    /// Remove the binding of the row under the cursor.
    Unbind,
    /// The key pressed is not one the keymap models (a media key, a dead
    /// key, a multi-codepoint IME commit) or carries a codepoint that must
    /// not land in a config file. There is no chord to capture, and
    /// inventing one would bind something else.
    NotBindable,
}

/// Does this keystroke carry the platform modifier — ⌘/Super, which this
/// window receives and a terminal does not?
///
/// Not a refusal: the binding loads, fires here, and is exactly what a user
/// on a Mac asked for. It is a fact the reader is told while they can still
/// press something else, because the alternative is finding out in the other
/// frontend, months later, that the key does nothing there.
#[must_use]
pub fn captures_cmd(seq: &[Chord]) -> bool {
    seq.iter().any(|c| c.parts().0.cmd)
}

/// A chord whose key is a codepoint that must never land raw in
/// `keymap.toml`.
///
/// Unreachable from a physical key — no keyboard has a RIGHT-TO-LEFT
/// OVERRIDE — so the way in is a paste or an IME commit, both of which
/// GPUI delivers as an ordinary `key_char`. The file is read by the user's
/// text editor and by the TUI, neither of which this window controls, and
/// `norte_encoding::is_terminal_hazard` is the same predicate every other
/// surface here uses to decide what may be painted.
fn hostile_chord(chord: Chord) -> bool {
    matches!(chord.parts().1, KeyCode::Char(c) if norte_encoding::is_terminal_hazard(c) || invisible_in_a_chord(c))
}

/// Codepoints `norte_encoding::is_terminal_hazard` deliberately ALLOWS, and
/// a chord must not.
///
/// That predicate exists for FILENAMES, where a zero-width joiner and the
/// variation selectors are how a composed emoji survives — dropping them
/// would corrupt a name. A chord is never an emoji sequence: bound to one of
/// these it is an entry no keyboard can press, invisible in `keymap.toml`,
/// invisible in the editor's row and invisible in the F1 sheet. Reachable
/// through an IME commit, which is the same door the hazards come through.
///
/// Whitespace is NOT here: `space` is an ordinary bindable key, and
/// `gpui_chord` maps it to `KeyCode::Char(' ')`.
fn invisible_in_a_chord(c: char) -> bool {
    c == '\u{200D}' || matches!(c, '\u{FE00}'..='\u{FE0F}')
}

/// Is `key` a bare modifier press?
///
/// While waiting for a chord, a modifier held down before the real key must
/// not be answered with "that key cannot be captured": the reader is halfway
/// through pressing `ctrl+shift+p` and nothing is wrong. It is not a chord
/// either — [`crate::keymap::gpui_chord`] rightly refuses to invent one —
/// so it is simply ignored.
fn is_modifier_key(key: &str) -> bool {
    matches!(
        key,
        "control" | "ctrl" | "alt" | "shift" | "platform" | "cmd" | "super" | "meta" | "function"
    )
}

/// Keyboard routing (GPUI key names, hardcoded — the convention
/// [`crate::settings_view::on_key`] established for a GUI screen).
///
/// CAPTURE FIRST, ahead of everything else, because that is what capture
/// means: while it is up every key is the chord being captured, including
/// the ctrl/alt/⌘ chords the gate below would have eaten and including the
/// letters the filter would have swallowed. Only a BARE `esc` stays out, so
/// that there is a way back — which makes `esc` the one chord this editor
/// cannot capture, a thing the screen says rather than leaving the reader
/// pressing it. `shift+esc`, `ctrl+esc` and friends are still chords.
///
/// Outside capture the gate is back: ctrl/alt/⌘ never reach the filter,
/// exactly as in settings, with one deliberate hole for `ctrl+u` (unbind)
/// — the editor that can only add is the editor that cannot fix a mistake.
pub fn on_key(
    view: &mut ShortcutsView,
    key: &str,
    key_char: Option<&str>,
    mods: Mods,
    maps: Maps<'_>,
) -> ShortcutsOutcome {
    let s = &mut view.state;
    if let Some(capture) = s.capture() {
        let waiting = capture.is_waiting();
        let screen = capture.screen();
        let bare = !mods.ctrl && !mods.alt && !mods.cmd && !mods.shift;
        match key {
            "escape" if bare => {
                s.cancel_capture();
                return ShortcutsOutcome::None;
            }
            "enter" if !waiting && bare => return ShortcutsOutcome::Confirm,
            "backspace" if !waiting && bare => {
                s.recapture();
                return ShortcutsOutcome::None;
            }
            _ => {}
        }
        if !waiting {
            // A verdict is on screen: confirm, recapture or cancel are the
            // three ways out and the footer names them. Anything else does
            // nothing rather than silently re-capturing under the reader.
            return ShortcutsOutcome::None;
        }
        if is_modifier_key(key) {
            return ShortcutsOutcome::None;
        }
        let Some(chord) = crate::keymap::gpui_chord(key, mods, key_char) else {
            return ShortcutsOutcome::NotBindable;
        };
        if hostile_chord(chord) {
            return ShortcutsOutcome::NotBindable;
        }
        // The map of THE ROW, not of the screen the reader was looking at.
        //
        // Unreachable: `rows` builds no `Screen::Dialog` row and
        // `cada_comando_despachable_tiene_fila_en_todos_los_presets` pins
        // that. Fail-closed rather than asserted away (rule 6) — but the
        // message it produces blames the KEY when the truth would be the
        // screen, so whoever gives this frontend a dialog context has to
        // give that case its own sentence too.
        let Some(eff) = maps.of(screen) else {
            debug_assert!(false, "no row of this editor belongs to {screen:?}");
            return ShortcutsOutcome::NotBindable;
        };
        s.capture_chord(chord, eff);
        return ShortcutsOutcome::None;
    }

    // Unbind BEFORE the modifier gate, and it is the only thing that gets
    // through it: the gate exists so a ctrl-chord is never typed into the
    // filter, not so that this screen has no verbs.
    if mods.ctrl && !mods.alt && !mods.cmd && key == "u" {
        return ShortcutsOutcome::Unbind;
    }
    if mods.ctrl || mods.alt || mods.cmd {
        return ShortcutsOutcome::None;
    }
    match key {
        "escape" => ShortcutsOutcome::Close,
        "backspace" => {
            s.backspace();
            ShortcutsOutcome::None
        }
        "up" => {
            s.up();
            ShortcutsOutcome::None
        }
        "down" => {
            s.down();
            ShortcutsOutcome::None
        }
        "pageup" => {
            s.page_up(ROWS);
            ShortcutsOutcome::None
        }
        "pagedown" => {
            s.page_down(ROWS);
            ShortcutsOutcome::None
        }
        "enter" => {
            s.begin_capture();
            ShortcutsOutcome::None
        }
        _ => {
            if let Some(c) = typed_char(key, key_char) {
                s.push_char(c);
            }
            ShortcutsOutcome::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Maps, ShortcutsOutcome, ShortcutsView, captures_cmd, on_key, rows};
    use norte_frontend::keymap::{Effective, Mods, Screen};

    fn effectives() -> (Effective, Effective) {
        let (browse, viewer, _dialog) = crate::keymap::build_effectives3_preset_only("orthodox");
        (browse, viewer)
    }

    fn maps<'a>(browse: &'a Effective, viewer: &'a Effective) -> Maps<'a> {
        Maps { browse, viewer }
    }

    fn view(m: Maps<'_>) -> ShortcutsView {
        ShortcutsView::new(rows(m, norte_i18n::Lang::En))
    }

    /// The editor with the cursor already on `command`'s row of `screen`.
    fn editor_on(m: Maps<'_>, screen: Screen, command: &str) -> ShortcutsView {
        let v = view(m);
        let idx = v
            .state
            .rows()
            .iter()
            .position(|r| r.screen == screen && r.command == command)
            .expect("the command has a row");
        let mut v = v;
        for _ in 0..idx {
            v.state.down();
        }
        assert_eq!(
            v.state.selected().map(|r| r.command.as_str()),
            Some(command),
            "the cursor is where the test believes"
        );
        v
    }

    fn mods(ctrl: bool, alt: bool, shift: bool, cmd: bool) -> Mods {
        Mods {
            ctrl,
            alt,
            shift,
            cmd,
        }
    }

    /// The gate this screen inherits from settings is still there when it is
    /// NOT capturing — a ctrl-chord must not be typed into the filter — with
    /// exactly one hole, and the hole is a verb rather than text.
    #[test]
    fn fuera_de_captura_el_ctrl_no_teclea_y_ctrl_u_desliga() {
        let (b, v) = effectives();
        let m = maps(&b, &v);
        let mut e = view(m);
        assert_eq!(
            on_key(&mut e, "j", Some("j"), mods(true, false, false, false), m),
            ShortcutsOutcome::None
        );
        assert!(
            e.state.query_display().is_empty(),
            "un ctrl-chord no es texto del filtro"
        );
        assert_eq!(
            on_key(&mut e, "u", Some("u"), mods(true, false, false, false), m),
            ShortcutsOutcome::Unbind
        );
        assert_eq!(
            on_key(&mut e, "escape", None, Mods::default(), m),
            ShortcutsOutcome::Close
        );
    }

    /// THE task's point: capture is a mode AHEAD of the gate, so a ctrl-chord
    /// — the whole reason a rebind screen exists — is captured instead of
    /// being dropped on the floor by `on_settings_key`'s early return.
    #[test]
    fn capturando_un_ctrl_chord_es_la_tecla_y_no_lo_come_la_puerta() {
        let (b, v) = effectives();
        let m = maps(&b, &v);
        let mut e = editor_on(m, Screen::Browse, "pane.mkdir");
        on_key(&mut e, "enter", None, Mods::default(), m);
        assert!(e.state.is_capturing());
        assert_eq!(
            on_key(&mut e, "j", Some("j"), mods(true, false, false, false), m),
            ShortcutsOutcome::None
        );
        let (screen, command, seq) = e.state.confirmable().expect("ctrl+j está libre");
        assert_eq!((screen, command), (Screen::Browse, "pane.mkdir"));
        assert_eq!(seq.len(), 1);
        assert_eq!(seq[0].to_string(), "ctrl+j");
    }

    /// This window receives ⌘ and a terminal never will. Not a refusal — the
    /// binding works here — so it is confirmable AND flagged.
    #[test]
    fn un_chord_con_cmd_se_captura_y_se_avisa() {
        let (b, v) = effectives();
        let m = maps(&b, &v);
        let mut e = editor_on(m, Screen::Browse, "pane.mkdir");
        on_key(&mut e, "enter", None, Mods::default(), m);
        on_key(&mut e, "j", Some("j"), mods(false, false, false, true), m);
        let (_, _, seq) = e.state.confirmable().expect("cmd+j es ligable aquí");
        assert!(captures_cmd(seq), "y el aviso se dispara: {seq:?}");
        // A chord without it does not raise the note.
        e.state.recapture();
        on_key(&mut e, "j", Some("j"), mods(true, false, false, false), m);
        let (_, _, seq) = e.state.confirmable().expect("ctrl+j sigue libre");
        assert!(!captures_cmd(seq));
    }

    /// A hazardous codepoint cannot come from a key: it comes from a paste
    /// or an IME commit, both of which arrive as an ordinary `key_char`.
    /// Refused, not masked — masking would bind a different chord than the
    /// one `keymap.toml` would name.
    #[test]
    fn un_codepoint_peligroso_pegado_no_se_captura() {
        let (b, v) = effectives();
        let m = maps(&b, &v);
        let mut e = editor_on(m, Screen::Browse, "pane.mkdir");
        on_key(&mut e, "enter", None, Mods::default(), m);
        // U+202E RIGHT-TO-LEFT OVERRIDE, as GPUI would deliver a paste.
        assert_eq!(
            on_key(&mut e, "\u{202e}", Some("\u{202e}"), Mods::default(), m),
            ShortcutsOutcome::NotBindable
        );
        assert!(
            e.state.capture().expect("sigue capturando").is_waiting(),
            "no se capturó nada"
        );
    }

    /// `esc` BARE cancels — which is why it is the one chord this editor
    /// cannot capture — and every other `esc` is still a chord.
    #[test]
    fn esc_pelado_cancela_y_esc_con_modificador_es_un_chord() {
        let (b, v) = effectives();
        let m = maps(&b, &v);
        let mut e = editor_on(m, Screen::Browse, "pane.mkdir");
        on_key(&mut e, "enter", None, Mods::default(), m);
        on_key(&mut e, "escape", None, Mods::default(), m);
        assert!(!e.state.is_capturing(), "esc pelado cancela la espera");

        on_key(&mut e, "enter", None, Mods::default(), m);
        on_key(&mut e, "escape", None, mods(true, false, false, false), m);
        let (_, _, seq) = e.state.confirmable().expect("ctrl+esc es un chord");
        assert_eq!(seq[0].to_string(), "ctrl+esc");
    }

    /// A modifier held down before the real key is not an error, and saying
    /// "that key cannot be captured" at it would be noise on every capture.
    #[test]
    fn un_modificador_suelto_no_es_un_rechazo() {
        let (b, v) = effectives();
        let m = maps(&b, &v);
        let mut e = editor_on(m, Screen::Browse, "pane.mkdir");
        on_key(&mut e, "enter", None, Mods::default(), m);
        for name in ["control", "shift", "alt", "platform"] {
            assert_eq!(
                on_key(&mut e, name, None, mods(true, false, false, false), m),
                ShortcutsOutcome::None,
                "{name}"
            );
        }
        assert!(e.state.capture().expect("captura viva").is_waiting());
    }

    /// A refusal is not confirmable, and `enter` on one still returns
    /// `Confirm` so the caller can echo the verdict — a key that goes
    /// silent is a key the reader repeats.
    #[test]
    fn una_tecla_sagrada_no_es_confirmable() {
        let (b, v) = effectives();
        let m = maps(&b, &v);
        let mut e = editor_on(m, Screen::Browse, "pane.mkdir");
        on_key(&mut e, "enter", None, Mods::default(), m);
        on_key(&mut e, "tab", None, Mods::default(), m);
        assert!(
            e.state
                .capture()
                .and_then(norte_frontend::shortcuts::Capture::verdict)
                .is_some(),
            "el veredicto se ve ANTES de confirmar"
        );
        assert!(e.state.confirmable().is_none(), "Tab no se vende");
        assert_eq!(
            on_key(&mut e, "enter", None, Mods::default(), m),
            ShortcutsOutcome::Confirm
        );
        assert!(e.state.confirmable().is_none());
    }

    /// The editor answers "how do I press X" for EVERY command this window
    /// dispatches, on every bundled preset — that is the question the
    /// reference sheet cannot answer and the reason an editor lists commands
    /// rather than only keys.
    ///
    /// And it lists only the two screens this window RESOLVES keys through:
    /// a dialog row would offer a rebind that changes this window not at all
    /// while silently changing the TUI's keyboard.
    ///
    /// Worth writing down, because it surprised this task: on all seven
    /// presets the unbound TAIL is empty — the GUI implements a SUBSET of the
    /// catalogue (`keymap::COMMANDS`/`VIEWER_COMMANDS`) and the presets bind
    /// all of it, so every row here is a bound key. The tail is not dead
    /// code: it is one preset trimming, or one `COMMANDS` entry, away from
    /// being the only place a new command is reachable from.
    #[test]
    fn cada_comando_despachable_tiene_fila_en_todos_los_presets() {
        for preset in crate::keymap::KNOWN_PRESETS {
            let (b, v, _d) = crate::keymap::build_effectives3_preset_only(preset);
            let all = rows(maps(&b, &v), norte_i18n::Lang::En);
            assert!(
                !all.iter().any(|r| r.screen == Screen::Dialog),
                "{preset}: esta GUI no despacha ningún verbo de diálogo por el resolver"
            );
            for screen in [Screen::Browse, Screen::Viewer] {
                for cmd in crate::keymap::screen_commands(screen) {
                    assert!(
                        all.iter().any(|r| r.screen == screen && r.command == *cmd),
                        "{preset}/{screen:?}: {cmd} no tiene fila, así que no hay forma de \
                         preguntar cómo se pulsa"
                    );
                }
            }
            // A viewer row never names a command the viewer does not
            // dispatch: binding it would write a key that does nothing there.
            assert!(
                !all.iter()
                    .any(|r| r.screen == Screen::Viewer && r.command == "pane.copy"),
                "{preset}: el viewer no despacha comandos de pane"
            );
        }
    }

    /// Every string this screen paints exists in BOTH locales: what a
    /// missing key shows the reader is the raw id.
    #[test]
    fn las_claves_de_la_pantalla_existen_en_ambos_locales() {
        for lang in [norte_i18n::Lang::Es, norte_i18n::Lang::En] {
            for id in [
                "shortcuts-title",
                "shortcuts-capture-hint",
                "shortcuts-confirm-hint",
                "shortcuts-no-key",
                "gui-shortcuts-hint",
                "gui-shortcuts-capture-note",
                "gui-shortcuts-cmd-note",
                "gui-msg-shortcut-saved-not-applied",
                "msg-shortcut-bound",
                "msg-shortcut-unbound",
                "msg-shortcut-nothing-to-unbind",
                "msg-shortcut-not-bindable",
            ] {
                assert_ne!(norte_i18n::t_in(lang, id), id, "falta {id} en {lang:?}");
            }
        }
    }
}
