//! Alt pressed and released ALONE opens the menu bar (`[ui] alt_menu`).
//!
//! A terminal does not report a bare modifier: under the classic encoding,
//! Alt alone produces no byte at all. Only kitty's keyboard protocol counts
//! it, and only in its "report all keys" mode (flag 8), with event types
//! (flag 2) to know when it is RELEASED. kitty, foot, `WezTerm`, and Ghostty
//! implement it; tmux, xterm, and the VTE terminals do not, and there this
//! key does nothing.
//!
//! Why it ships off by default: in that mode the terminal sends the KEY and
//! not the text, and crossterm does not read the associated text (flag 16).
//! A letter composed with a dead key — the `é` of a Spanish keyboard — or a
//! symbol typed with `AltGr` — `@`, `#`, `[` — arrives as its base key: in a
//! rename, `a@b` can end up as `a2b`. Flag 4 only fixes Shift, because
//! `AltGr` is not a protocol modifier. Whoever turns it on does so knowing
//! that.
//!
//! Two pieces, and neither decides what the menu does:
//! - [`set`] requests or withdraws the protocol. Its state is PROCESS-wide,
//!   like raw mode, so suspending, quitting, and the panic hook can undo it
//!   without anyone having to pass them anything.
//! - [`AltSolo`] recognizes the gesture over the events that arrive.

use std::io::{self, IsTerminal, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, ModifierKeyCode,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};

/// Is the protocol requested right now on the terminal?
static REQUESTED: AtomicBool = AtomicBool::new(false);

/// Disambiguate (1), event types (2), alternate keys (4), and all keys as
/// escape codes (8).
///
/// The 4 is not optional: without it, with 8 set, `Shift+a` arrives as the
/// key `a` with SHIFT, and the keymap adapter — which trusts that a `Char`
/// already carries the uppercase letter — would read a lowercase one. With 4,
/// crossterm uses the shifted key and drops SHIFT, the usual way.
const FLAGS: KeyboardEnhancementFlags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
    .union(KeyboardEnhancementFlags::REPORT_EVENT_TYPES)
    .union(KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS)
    .union(KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES);

/// Requests (or withdraws) the protocol if needed. Idempotent, like
/// `mouse::Capture::set`: the config reload calls it every turn.
///
/// `supported` is only consulted when TURNING ON, and it is a function so
/// tests do not have to ask a terminal that is not there. A terminal that
/// does not support it receives nothing and the key stays without effect,
/// with no error: there is nothing the reader could fix from norte.
///
/// # Errors
/// Whatever comes from writing to `out`.
pub fn set(want: bool, supported: impl FnOnce() -> bool, out: &mut impl Write) -> io::Result<()> {
    if want == REQUESTED.load(Ordering::Relaxed) {
        return Ok(());
    }
    if want {
        if !supported() {
            return Ok(());
        }
        crossterm::execute!(out, PushKeyboardEnhancementFlags(FLAGS))?;
    } else if !YIELDED.swap(false, Ordering::Relaxed) {
        // Yielded is already off the stack: popping it again would take an
        // entry that belongs to the shell.
        crossterm::execute!(out, PopKeyboardEnhancementFlags)?;
    }
    REQUESTED.store(want, Ordering::Relaxed);
    Ok(())
}

/// Requested, but withdrawn while another program has the terminal. This is
/// what pairs [`yield_`] with [`recover`]: without it, a failure BEFORE
/// yielding left `recover` stacking a second entry that exiting does not
/// pop.
static YIELDED: AtomicBool = AtomicBool::new(false);

/// What the terminal answered, asked ONCE.
static SUPPORT: OnceLock<bool> = OnceLock::new();

/// Asks the terminal whether it speaks the protocol, and caches the answer.
///
/// Called at STARTUP, before the loop raises its event reader, and at no
/// other time. Two reasons, and both are expensive:
/// - with the reader alive, that thread holds crossterm's lock; the query
///   waits two seconds, gives up, and answers "no". A config reload used to
///   freeze the TUI for two seconds and leave the gesture off.
/// - crossterm sends the query over STDOUT if it cannot write to the control
///   terminal, and under `--pick` stdout is the caller's data pipe. With no
///   terminal on stdout, it does not ask: it assumes "no".
///
/// An error reads as "no": a presentation key must not bring down startup.
pub fn query_support() -> bool {
    *SUPPORT.get_or_init(|| {
        io::stdout().is_terminal()
            && crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false)
    })
}

/// [`query_support`]'s answer, without asking. If it was never asked,
/// "no".
#[must_use]
pub fn supported() -> bool {
    SUPPORT.get().copied().unwrap_or(false)
}

/// Returns the terminal to its usual encoding WITHOUT forgetting it was
/// requested: to yield the terminal to a shell or an editor, which do not
/// speak this protocol and would receive escapes instead of letters. Two
/// yields in a row are one.
///
/// # Errors
/// Whatever comes from writing to `out`.
pub fn yield_(out: &mut impl Write) -> io::Result<()> {
    if REQUESTED.load(Ordering::Relaxed) && !YIELDED.swap(true, Ordering::Relaxed) {
        crossterm::execute!(out, PopKeyboardEnhancementFlags)?;
    }
    Ok(())
}

/// [`yield_`]'s counterpart: on returning from a suspension. Only re-pushes
/// what was yielded.
///
/// # Errors
/// Whatever comes from writing to `out`.
pub fn recover(out: &mut impl Write) -> io::Result<()> {
    if REQUESTED.load(Ordering::Relaxed) && YIELDED.swap(false, Ordering::Relaxed) {
        crossterm::execute!(out, PushKeyboardEnhancementFlags(FLAGS))?;
    }
    Ok(())
}

/// Withdraws it from the panic hook, FORGETTING that it was requested.
///
/// A panic inside a tokio task runs the hook and the process stays alive; on
/// exit, `tty::restore` would remove the protocol again, already outside the
/// alternate screen, and would take an entry off the shell's stack.
///
/// # Errors
/// Whatever comes from writing to `out`.
pub fn drop_on_panic(out: &mut impl Write) -> io::Result<()> {
    let requested = REQUESTED.swap(false, Ordering::Relaxed);
    let yielded = YIELDED.swap(false, Ordering::Relaxed);
    if requested && !yielded {
        crossterm::execute!(out, PopKeyboardEnhancementFlags)?;
    }
    Ok(())
}

/// Is this the press of a lone modifier key? Those are not keys for the
/// keymap: letting them through would reset a half-done sequence (`g` … `g`)
/// the moment the reader brushed Alt or Shift.
#[must_use]
pub const fn es_modifier(ev: &KeyEvent) -> bool {
    matches!(ev.code, KeyCode::Modifier(_))
}

/// The gesture: Alt goes down with no other modifier and comes up with
/// nothing in between.
#[derive(Debug, Default)]
pub struct AltSolo {
    armed: bool,
}

impl AltSolo {
    /// A keyboard event. `true` = the gesture just completed.
    ///
    /// Any other keystroke disarms it, so `Alt+x` does not open the menu on
    /// releasing Alt. Repeating a held Alt does not disarm it. `AltGr` is not
    /// Alt: the terminal sends it as `IsoLevel3Shift`, and on a Spanish
    /// keyboard it types `@` and `#`.
    pub fn key(&mut self, ev: &KeyEvent) -> bool {
        let is_alt = matches!(
            ev.code,
            KeyCode::Modifier(ModifierKeyCode::LeftAlt | ModifierKeyCode::RightAlt)
        );
        match ev.kind {
            KeyEventKind::Press => {
                self.armed = is_alt && ev.modifiers.difference(KeyModifiers::ALT).is_empty();
                false
            }
            KeyEventKind::Repeat => {
                if !is_alt {
                    self.armed = false;
                }
                false
            }
            KeyEventKind::Release => {
                if !is_alt {
                    return false;
                }
                std::mem::take(&mut self.armed)
            }
        }
    }

    /// Something other than a keystroke got in the middle: a click, the
    /// wheel.
    pub const fn release(&mut self) {
        self.armed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventState;

    fn ev(code: KeyCode, kind: KeyEventKind, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind,
            state: KeyEventState::NONE,
        }
    }

    fn alt(kind: KeyEventKind) -> KeyEvent {
        // crossterm puts ALT in Alt's own modifiers.
        ev(
            KeyCode::Modifier(ModifierKeyCode::LeftAlt),
            kind,
            KeyModifiers::ALT,
        )
    }

    #[test]
    fn pressing_and_releasing_alt_is_the_gesture() {
        let mut a = AltSolo::default();
        assert!(!a.key(&alt(KeyEventKind::Press)));
        assert!(a.key(&alt(KeyEventKind::Release)));
    }

    #[test]
    fn a_held_alt_that_repeats_is_still_the_gesture() {
        let mut a = AltSolo::default();
        a.key(&alt(KeyEventKind::Press));
        a.key(&alt(KeyEventKind::Repeat));
        assert!(a.key(&alt(KeyEventKind::Release)));
    }

    #[test]
    fn alt_plus_another_key_is_not_it() {
        let mut a = AltSolo::default();
        a.key(&alt(KeyEventKind::Press));
        a.key(&ev(
            KeyCode::Char('x'),
            KeyEventKind::Press,
            KeyModifiers::ALT,
        ));
        a.key(&ev(
            KeyCode::Char('x'),
            KeyEventKind::Release,
            KeyModifiers::ALT,
        ));
        assert!(!a.key(&alt(KeyEventKind::Release)));
    }

    #[test]
    fn altgr_is_not_alt() {
        let mut a = AltSolo::default();
        let altgr = |k| {
            ev(
                KeyCode::Modifier(ModifierKeyCode::IsoLevel3Shift),
                k,
                KeyModifiers::NONE,
            )
        };
        a.key(&altgr(KeyEventKind::Press));
        assert!(!a.key(&altgr(KeyEventKind::Release)));
    }

    #[test]
    fn with_ctrl_held_it_does_not_arm() {
        let mut a = AltSolo::default();
        a.key(&ev(
            KeyCode::Modifier(ModifierKeyCode::LeftAlt),
            KeyEventKind::Press,
            KeyModifiers::ALT | KeyModifiers::CONTROL,
        ));
        assert!(!a.key(&alt(KeyEventKind::Release)));
    }

    #[test]
    fn a_click_in_the_middle_disarms_it() {
        let mut a = AltSolo::default();
        a.key(&alt(KeyEventKind::Press));
        a.release();
        assert!(!a.key(&alt(KeyEventKind::Release)));
    }

    #[test]
    fn releasing_a_letter_neither_disarms_nor_fires() {
        // A letter that was held BEFORE Alt and is released afterward.
        let mut a = AltSolo::default();
        a.key(&alt(KeyEventKind::Press));
        assert!(!a.key(&ev(
            KeyCode::Char('a'),
            KeyEventKind::Release,
            KeyModifiers::NONE
        )));
        assert!(a.key(&alt(KeyEventKind::Release)));
    }

    #[test]
    fn modifier_keys_are_not_keys_for_the_keymap() {
        assert!(es_modifier(&alt(KeyEventKind::Press)));
        assert!(!es_modifier(&ev(
            KeyCode::Char('a'),
            KeyEventKind::Press,
            KeyModifiers::NONE
        )));
    }

    /// Turning on writes the PUSH with all four flags, and turning off the
    /// POP, once each; and a terminal with no support receives nothing. One
    /// test because the state is process-wide.
    #[test]
    fn set_requests_and_withdraws_once_and_respects_support() {
        let mut out = Vec::new();
        set(false, || true, &mut out).expect("writes");
        out.clear();

        set(true, || false, &mut out).expect("writes");
        assert!(out.is_empty(), "with no support nothing is sent");

        set(true, || true, &mut out).expect("writes");
        assert_eq!(String::from_utf8_lossy(&out), "\x1b[>15u");
        out.clear();
        set(true, || panic!("does not ask again"), &mut out).expect("writes");
        assert!(out.is_empty(), "idempotent");

        // Yielding and recovering come in PAIRS: twice each is one.
        yield_(&mut out).expect("writes");
        yield_(&mut out).expect("writes");
        recover(&mut out).expect("writes");
        recover(&mut out).expect("writes");
        assert_eq!(
            String::from_utf8_lossy(&out),
            "\x1b[<1u\x1b[>15u",
            "one pop and one push"
        );
        out.clear();

        set(false, || true, &mut out).expect("writes");
        assert_eq!(String::from_utf8_lossy(&out), "\x1b[<1u");
        out.clear();
        yield_(&mut out).expect("writes");
        assert!(out.is_empty(), "when off there is nothing to yield");

        // Turning off while YIELDED removes nothing: it is already off the
        // stack.
        set(true, || true, &mut out).expect("writes");
        yield_(&mut out).expect("writes");
        out.clear();
        set(false, || true, &mut out).expect("writes");
        assert!(out.is_empty(), "yielded is not removed twice");
        recover(&mut out).expect("writes");
        assert!(out.is_empty(), "off is not recovered");

        // After a panic, exiting does not remove it again.
        set(true, || true, &mut out).expect("writes");
        out.clear();
        drop_on_panic(&mut out).expect("writes");
        set(false, || true, &mut out).expect("writes");
        assert_eq!(String::from_utf8_lossy(&out), "\x1b[<1u", "just one pop");
    }
}
