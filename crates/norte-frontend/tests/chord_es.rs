//! Los nombres de las teclas en español ([`set_chord_lang`]).
//!
//! En su PROPIO binario, y es lo único que hay en él: el idioma se fija una
//! vez por proceso (`OnceLock`), así que junto a los tests que esperan inglés
//! —los unitarios y los doctests de `paint_chord`— los rompería según el orden
//! en que corran.

use norte_frontend::keymap::{paint_chord, set_chord_lang, unpaint_chord};
use norte_i18n::Lang;

#[test]
fn las_teclas_se_nombran_en_espanol_y_la_inversa_las_entiende() {
    assert!(set_chord_lang(Lang::Es));
    assert!(!set_chord_lang(Lang::En), "fijado una vez, no se cambia");

    assert_eq!(paint_chord("pgdn"), "AvPág");
    assert_eq!(paint_chord("pgup"), "RePág");
    assert_eq!(paint_chord("backspace"), "Retroceso");
    assert_eq!(paint_chord("enter"), "Intro");
    assert_eq!(paint_chord("up"), "↑");
    assert_eq!(paint_chord("home"), "Inicio");
    assert_eq!(paint_chord("shift+delete"), "Mayús+Supr");
    // La mayúscula bajo un modificador se DICE, y en el mismo idioma.
    assert_eq!(paint_chord("alt+C"), "Alt+Mayús+C");
    // Las letras y las F no se traducen: son lo que lleva la tecla.
    assert_eq!(paint_chord("ctrl+k"), "Ctrl+k");
    assert_eq!(paint_chord("f5"), "F5");

    // Un botón pintado con cualquiera de estos nombres vuelve a su tecla.
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
    // Y la inversa entiende también los nombres en inglés.
    assert_eq!(unpaint_chord("PgDn"), "pgdn");
    assert_eq!(unpaint_chord("Shift+Delete"), "shift+delete");
}
