//! Hints de pie de página de los overlays de diálogo (H1 T3, issue #24 —
//! CIERRA): mismo patrón que la ayuda F1 (`help.rs`), pero por comando del
//! contexto `dialog`. Un hint es el JOIN de los comandos SOPORTADOS por un
//! overlay concreto × el keymap `dialog` EFECTIVO × las etiquetas Fluent
//! `dialog-cmd-*` — jamás una cadena estática mantenida a mano: un rebind
//! ya no puede desincronizar el pie de página de lo que la tecla hace de
//! verdad.

use std::collections::HashSet;

use norte_i18n::t;

use crate::keymap::{Effective, dialog_hint_id};

/// Footer hint for an overlay: the join of its SUPPORTED dialog commands ×
/// the effective dialog keymap × Fluent labels — same invariant as F1 help
/// (#24: a rebind can never desync the hint again).
///
/// El ORDEN sale del keymap EFECTIVO (`eff.bindings()`, en precedencia
/// real), no del array `supported`: un comando SIN binding en el efectivo
/// (rebindeado a nada, o simplemente jamás ligado en una capa exótica)
/// queda fuera — honesto, sin tecla fantasma. La PRIMERA chord de cada
/// comando en ese orden es la que se muestra (p. ej. en `vim`, `up`/`down`
/// ganan a los `k`/`j` añadidos después en el preset).
#[must_use]
pub fn dialog_hints(supported: &[&str], eff: &Effective) -> String {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out = Vec::new();
    for (chord, cmd) in eff.bindings() {
        if supported.contains(&cmd) && seen.insert(cmd) {
            // RENDER-side duty (encoding audit H1): `chord` viene de un
            // keymap potencialmente hostil (`./.norte/keymap.toml`, capa de
            // PROYECTO sin trust — `parse_chord` acepta CUALQUIER
            // codepoint suelto como `KeyCode::Char`). `Chord`'s `Display`
            // lo escribe crudo A PROPÓSITO (logs/debug quieren el chord
            // real); este hint SÍ se pinta en el pie de modales de
            // seguridad, así que se enmascara aquí, no en el motor. Mismo
            // mecanismo que `App::query_display`.
            let chord = norte_encoding::mask_terminal_hazards(&chord);
            out.push(format!("[{chord}] {}", t(&dialog_hint_id(cmd))));
        }
    }
    out.join(" ")
}

/// Hints precomputados de TODOS los overlays de diálogo, uno por campo.
/// Se reconstruyen en el arranque y en cada hot-reload OK (`main.rs`),
/// igual que `help_lines` (`help::build`), a partir del MISMO efectivo
/// `dialog` que consume el `Resolver` compartido — ANTES de que ese
/// efectivo se mueva al `Resolver` (`Effective` es `Clone`, pero
/// `DialogHints::build` solo toma prestado: no hace falta clonar).
/// `ui::draw_*` los lee en vez de una clave Fluent estática. El único punto
/// de acoplamiento con la semántica de SEGURIDAD (qué comandos acepta cada
/// overlay) son los ALLOWLIST de `app.rs` — la MISMA lista que filtra el
/// despacho, jamás una copia.
#[derive(Debug, Clone, Default)]
pub struct DialogHints {
    /// `Modal::ConfirmDelete`/`Modal::ConfirmTransfer`.
    pub confirm: String,
    /// `Modal::Collision`.
    pub collision: String,
    /// `Modal::ApproveAgentOp`.
    pub approval: String,
    /// `Modal::TrustHostKey`.
    pub trust_host: String,
    /// Selector de tema (`App::theme_picker`).
    pub picker: String,
    /// Gestor de extensiones (`App::extensions`).
    pub extensions: String,
    /// Popup de navegación en modo hotlist (`App::nav_popup`,
    /// `NavPopupKind::Hotlist`) — el historial no pinta footer, igual que
    /// antes de H1.
    pub nav_list: String,
}

impl DialogHints {
    /// Reconstruye los siete hints del efectivo `dialog` vigente.
    #[must_use]
    pub fn build(eff: &Effective) -> Self {
        use crate::app::{
            ALLOW_APPROVAL, ALLOW_COLLISION, ALLOW_CONFIRM, ALLOW_EXTENSIONS, ALLOW_NAV_HOTLIST,
            ALLOW_PICKER, ALLOW_TRUST_HOST,
        };
        Self {
            confirm: dialog_hints(ALLOW_CONFIRM, eff),
            collision: dialog_hints(ALLOW_COLLISION, eff),
            approval: dialog_hints(ALLOW_APPROVAL, eff),
            trust_host: dialog_hints(ALLOW_TRUST_HOST, eff),
            picker: dialog_hints(ALLOW_PICKER, eff),
            extensions: dialog_hints(ALLOW_EXTENSIONS, eff),
            nav_list: dialog_hints(ALLOW_NAV_HOTLIST, eff),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{Screen, parse_keymap};

    /// Encoding audit H1: un `./.norte/keymap.toml` de PROYECTO (sin trust)
    /// puede ligar un chord hostil (RLO/ZWSP/LRM/BEL, corpus
    /// `norte_testkit::corpus::hostile_chords`) a un comando `dialog.*`
    /// soportado vía `prepend_keymap` — capa de usuario, gana al preset. El
    /// hint generado (`dialog_hints`) es lo que se pinta en el pie de
    /// modales de SEGURIDAD (`ApproveAgentOp`/`TrustHostKey`/
    /// `ConfirmDelete`-permanente): ningún hazard puede sobrevivir crudo.
    #[test]
    fn dialog_hints_enmascara_chords_hostiles_de_una_capa() {
        let preset = parse_keymap(
            r#"
            [dialog]
            keymap = [{ on = ["y"], run = "dialog.approve" }]
        "#,
        )
        .unwrap();
        let known = ["dialog.approve", "dialog.deny"];
        for hazard in norte_testkit::corpus::hostile_chords() {
            // Escape `\uXXXX` de TOML (spec v1.0.0): un control C0 crudo
            // como BEL (U+0007) es sintaxis inválida dentro de una basic
            // string TOML, así que el token va SIEMPRE escapado, no crudo.
            let token_esc = format!("\\u{:04X}", hazard.token as u32);
            let layer_src = format!(
                r#"
                [dialog]
                prepend_keymap = [{{ on = ["{token_esc}"], run = "dialog.approve" }}]
                "#,
            );
            let layer = parse_keymap(&layer_src).unwrap();
            let eff = Effective::build_for(&preset, &[layer], &known, Screen::Dialog)
                .unwrap_or_else(|e| panic!("[{}] keymap efectivo: {e}", hazard.id));
            let hint = dialog_hints(&["dialog.approve", "dialog.deny"], &eff);
            assert!(
                !hint.chars().any(norte_encoding::is_terminal_hazard),
                "[{}] hazard crudo en el hint: {hint:?}",
                hazard.id
            );
            assert!(
                hint.contains('\u{FFFD}'),
                "[{}] el hazard debe enmascararse a U+FFFD: {hint:?}",
                hazard.id
            );
        }
    }

    #[test]
    fn dialog_hints_omite_comandos_sin_binding() {
        let preset = parse_keymap(
            r#"
            [dialog]
            keymap = [{ on = ["y"], run = "dialog.approve" }]
        "#,
        )
        .unwrap();
        let known = ["dialog.approve", "dialog.deny"];
        let eff = Effective::build_for(&preset, &[], &known, Screen::Dialog).unwrap();
        let hint = dialog_hints(&["dialog.approve", "dialog.deny"], &eff);
        assert_eq!(hint, "[y] approve");
    }

    #[test]
    fn dialog_hints_respeta_el_orden_del_efectivo_no_del_allowlist() {
        let preset = parse_keymap(
            r#"
            [dialog]
            keymap = [
                { on = ["esc"], run = "dialog.cancel" },
                { on = ["enter"], run = "dialog.confirm" },
            ]
        "#,
        )
        .unwrap();
        let known = ["dialog.confirm", "dialog.cancel"];
        let eff = Effective::build_for(&preset, &[], &known, Screen::Dialog).unwrap();
        // El allowlist pide confirm-antes-que-cancel; el efectivo declara
        // cancel primero — el hint sigue al efectivo.
        let hint = dialog_hints(&["dialog.confirm", "dialog.cancel"], &eff);
        assert_eq!(hint, "[esc] cancel [enter] confirm");
    }

    #[test]
    fn dialog_hints_usa_la_primera_chord_ante_un_duplicado() {
        let preset = parse_keymap(
            r#"
            [dialog]
            keymap = [
                { on = ["up"], run = "dialog.up" },
                { on = ["k"], run = "dialog.up" },
            ]
        "#,
        )
        .unwrap();
        let known = ["dialog.up"];
        let eff = Effective::build_for(&preset, &[], &known, Screen::Dialog).unwrap();
        let hint = dialog_hints(&["dialog.up"], &eff);
        assert_eq!(hint, "[up] up");
    }

    #[test]
    fn dialog_hints_string_vacia_sin_soportados_ligados() {
        let preset = parse_keymap(
            r#"
            [dialog]
            keymap = [{ on = ["y"], run = "dialog.approve" }]
        "#,
        )
        .unwrap();
        let known = ["dialog.approve"];
        let eff = Effective::build_for(&preset, &[], &known, Screen::Dialog).unwrap();
        let hint = dialog_hints(&["dialog.rename"], &eff);
        assert_eq!(hint, "");
    }
}
