//! Contenido de la ayuda (F1): se construye del keymap EFECTIVO (preset
//! más capas del usuario) y del catálogo Fluent (`help-cmd-*`), jamás de
//! una lista mantenida a mano. Extensible: comando nuevo = binding en el
//! preset más entrada en el catálogo (la suite de i18n obliga a la
//! segunda).

use norte_i18n::t;

use crate::keymap::{Effective, help_id};

/// Construye las líneas de la ayuda desde los keymaps efectivos de las
/// dos pantallas: cada binding con su descripción del catálogo, en el
/// orden de precedencia real (lo que la tecla HACE, no lo que el preset
/// dice).
#[must_use]
pub fn build(browse: &Effective, viewer: &Effective) -> Vec<String> {
    let mut out = Vec::new();
    for (titulo, eff) in [
        (t("help-section-browse"), browse),
        (t("help-section-viewer"), viewer),
    ] {
        out.push(String::new());
        out.push(format!("── {titulo} ──"));
        for (seq, cmd) in eff.bindings() {
            out.push(format!("  {seq:<14} {}", t(&help_id(cmd))));
        }
    }
    out
}
