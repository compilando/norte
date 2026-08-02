//! Contenido de la ayuda (F1): se construye del keymap EFECTIVO (preset
//! más capas del usuario) y del catálogo Fluent (`help-cmd-*`), jamás de
//! una lista mantenida a mano. Extensible: comando nuevo = binding en el
//! preset más entrada en el catálogo (la suite de i18n obliga a la
//! segunda).

use norte_i18n::t;

use crate::keymap::{Effective, dialog_hint_id, help_id};

/// Construye las líneas de la ayuda desde los keymaps efectivos de las
/// dos pantallas: cada binding con su descripción del catálogo, en el
/// orden de precedencia real (lo que la tecla HACE, no lo que el preset
/// dice).
#[must_use]
pub fn build(browse: &Effective, viewer: &Effective, dialog: &Effective) -> Vec<String> {
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
    // #113: los verbos `dialog.*` eran invisibles en la app (los pies de
    // los overlays FILTRAN por espacio — reordenar en el picker de
    // columnas, p. ej., solo se aprendía en los docs). La ayuda no tiene
    // esa restricción: sección completa del efectivo `dialog`, con la
    // nota de que cada overlay soporta su SUBCONJUNTO (allowlists).
    out.push(String::new());
    out.push(format!("── {} ──", t("help-section-dialog")));
    out.push(format!("  {}", t("help-dialog-note")));
    for (seq, cmd) in dialog.bindings() {
        out.push(format!("  {seq:<14} {}", t(&dialog_hint_id(cmd))));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{COMMANDS, DIALOG_COMMANDS, Screen, presets};

    /// #113: la ayuda F1 lista la sección de diálogos COMPLETA del efectivo
    /// `dialog` — incluidos los verbos que los pies de overlay omiten por
    /// espacio (reordenación del picker de columnas). Única superficie
    /// in-app sin presupuesto de ancho.
    #[test]
    fn la_ayuda_incluye_los_verbos_dialog() {
        let (_, preset) = presets()
            .into_iter()
            .find(|(n, _)| *n == "orthodox")
            .expect("preset orthodox");
        let known: Vec<&str> = COMMANDS
            .iter()
            .copied()
            .chain(DIALOG_COMMANDS.iter().copied())
            .collect();
        let browse = Effective::build_for(&preset, &[], &known, Screen::Browse).unwrap();
        let viewer = Effective::build_for(&preset, &[], &known, Screen::Viewer).unwrap();
        let dialog = Effective::build_for(&preset, &[], &known, Screen::Dialog).unwrap();
        let lines = build(&browse, &viewer, &dialog);
        let all = lines.join("\n");
        assert!(
            all.contains(&t("help-section-dialog")),
            "sección de diálogos presente: {all}"
        );
        // El caso que parió #113: los verbos de reordenación del picker,
        // filtrados de su pie (101 celdas > 80), aparecen AQUÍ con chord.
        assert!(
            all.contains(&t("dialog-cmd-move-up")),
            "move-up aprendible desde la ayuda: {all}"
        );
        assert!(
            all.contains(&t("dialog-cmd-sort")),
            "sort aprendible desde la ayuda: {all}"
        );
        // La nota de que cada overlay soporta su subconjunto acompaña.
        assert!(all.contains(&t("help-dialog-note")));
    }
}
