//! The embedded factory presets: sources, the name catalogue, and the lookup
//! each frontend uses for its "known preset" list.
//!
//! Bundled keymap presets (ADR 0006), shared by every frontend. Each
//! frontend declares ITS OWN command set to
//! [`Effective::build_for`](super::Effective::build_for); a preset binding to
//! a command that frontend lacks is KEPT and tagged
//! [`Availability::NotHere`](super::Availability), never dropped, so the key
//! can say why it does nothing (K1).

/// The default orthodox preset.
pub const ORTHODOX: &str = include_str!("../../presets/keymap/orthodox.toml");
/// Vim-style preset.
pub const VIM: &str = include_str!("../../presets/keymap/vim.toml");
/// CUA preset.
pub const CUA: &str = include_str!("../../presets/keymap/cua.toml");
/// Total Commander-style preset (K2b).
pub const TOTAL_COMMANDER: &str = include_str!("../../presets/keymap/total-commander.toml");
/// Krusader-style preset (K2b).
pub const KRUSADER: &str = include_str!("../../presets/keymap/krusader.toml");

/// Names of every embedded preset (final review MINOR 4: this catalog
/// used to be mirrored as a hardcoded `&[&str]` in each frontend that
/// needs a "known preset" list or an "available presets" banner message
/// — TUI, GUI — with no single source of truth. `source()` and `NAMES`
/// are now tested against each other below, so a preset added to one and
/// not the other fails CI instead of drifting silently.
pub const NAMES: &[&str] = &["orthodox", "vim", "cua", "total-commander", "krusader"];

/// Preset source by name; `None` if unknown (caller falls back +
/// reports, same contract the TUI had).
#[must_use]
pub fn source(name: &str) -> Option<&'static str> {
    match name {
        "orthodox" => Some(ORTHODOX),
        "vim" => Some(VIM),
        "cua" => Some(CUA),
        "total-commander" => Some(TOTAL_COMMANDER),
        "krusader" => Some(KRUSADER),
        _ => None,
    }
}

#[cfg(test)]
mod presets_catalog_tests {
    use super::{NAMES, source};

    /// `NAMES` es el catálogo — cada entrada debe resolver una fuente real
    /// (final review MINOR 4: sin esto, `NAMES` podría desincronizarse de
    /// `source()` sin que ningún test lo note).
    #[test]
    fn cada_nombre_de_names_resuelve_una_fuente() {
        for name in NAMES {
            assert!(
                source(name).is_some(),
                "NAMES declara {name:?} pero source({name:?}) es None"
            );
        }
    }

    /// Ancla el tamaño del catálogo: un preset nuevo debe tocar este test a
    /// propósito (y con él, el resto de frontends que consumen `NAMES`).
    /// Renombrado en K2b Task 2 (era `names_tiene_los_tres_presets_de_fabrica`):
    /// ya no son tres, y el nombre viejo mentiría sobre el tamaño real.
    #[test]
    fn names_tiene_los_presets_de_fabrica() {
        assert_eq!(NAMES.len(), 5);
    }

    /// Un preset embebido que no parsea no es un fallo ruidoso: los
    /// consumidores lo ignoran en silencio. `preset_commands` hace
    /// `let Ok(kf) = parse_keymap(src) else { continue }`, así que un typo en
    /// —por ejemplo— el `dialog_from` de un preset de K2b encogería el
    /// vocabulario que `norte doctor` y `ntc keys` tratan por conocido, y el
    /// síntoma serían avisos de «comando desconocido» en otro sitio. Que
    /// falle aquí, con el nombre del preset y el diagnóstico.
    #[test]
    fn todos_los_presets_de_fabrica_parsean() {
        for name in NAMES {
            let src = source(name).expect("NAMES resuelve");
            crate::keymap::parse_keymap(src).unwrap_or_else(|e| panic!("preset {name}: {e}"));
        }
    }
}
