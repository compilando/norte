//! Helpers PUROS de tecleo compartidos por las vistas de la GUI (quality
//! review 78eb243 MINOR-2): antes vivían copiados en `modal`,
//! `palette_view`, `settings_view`, `extensions_view` y `main` — una sola
//! definición del contrato «qué carácter teclea esta tecla».

/// El carácter único que teclea `key`, respetando layout/shift (`key_char`)
/// con fallback al nombre de la tecla — mismo contrato de fidelidad que el
/// quick-search del pane y el adaptador de chords del visor
/// (`keymap::gpui_chord`). `"space"` llega con nombre, no como carácter
/// suelto.
pub(crate) fn typed_char(key: &str, key_char: Option<&str>) -> Option<char> {
    if key == "space" {
        return Some(' ');
    }
    single_char(key_char).or_else(|| single_char(Some(key)))
}

/// El primer carácter de `s` si `s` es EXACTAMENTE uno — un `key_char`
/// compuesto (dead-key/IME multi-codepoint) jamás produce un `Char` parcial.
pub(crate) fn single_char(s: Option<&str>) -> Option<char> {
    let s = s?;
    let mut it = s.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}
