//! Key names in Spanish ([`set_chord_lang`]).
//!
//! In its OWN binary, and it is the only thing in it: the language is fixed
//! once per process (`OnceLock`), so alongside the tests that expect English
//! —the unit tests and `paint_chord`'s doctests— it would break them
//! depending on the order they run in.

use norte_frontend::keymap::{paint_chord, set_chord_lang, unpaint_chord};
use norte_i18n::Lang;

#[test]
fn keys_are_named_in_spanish_and_the_inverse_understands_them() {
    assert!(set_chord_lang(Lang::Es));
    assert!(!set_chord_lang(Lang::En), "fixed once, it does not change");

    assert_eq!(paint_chord("pgdn"), "AvPág");
    assert_eq!(paint_chord("pgup"), "RePág");
    assert_eq!(paint_chord("backspace"), "Retroceso");
    assert_eq!(paint_chord("enter"), "Intro");
    assert_eq!(paint_chord("up"), "↑");
    assert_eq!(paint_chord("home"), "Inicio");
    assert_eq!(paint_chord("shift+delete"), "Mayús+Supr");
    // The uppercase letter under a modifier is SPOKEN, and in the same language.
    assert_eq!(paint_chord("alt+C"), "Alt+Mayús+C");
    // Letters and the Fs are not translated: they are what the key carries.
    assert_eq!(paint_chord("ctrl+k"), "Ctrl+k");
    assert_eq!(paint_chord("f5"), "F5");

    // A chord painted with any of these names returns to its key.
    for raw in [
        "pgdn",
        "pgup",
        "home",
        "end",
        "up",
        "down",
        "left",
        "right",
        "enter",
        "backspace",
        "space",
        "delete",
        "insert",
        "shift+delete",
        "alt+C",
        "ctrl+alt+K",
        "shift+f8",
        "esc",
        "tab",
        "g g",
    ] {
        assert_eq!(unpaint_chord(&paint_chord(raw)), raw, "{raw}");
    }
    // And the inverse also understands the English names.
    assert_eq!(unpaint_chord("PgDn"), "pgdn");
    assert_eq!(unpaint_chord("Shift+Delete"), "shift+delete");
}
