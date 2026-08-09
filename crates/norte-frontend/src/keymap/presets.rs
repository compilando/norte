//! The embedded factory presets: sources, the name catalogue, and the lookup
//! each frontend uses for its "known preset" list.
//!
//! Bundled keymap presets (ADR 0006), shared by every frontend. Each
//! frontend validates against ITS OWN command set — via
//! [`Effective::build_for`](super::Effective::build_for) (strict) or
//! [`Effective::build_for_subset`](super::Effective::build_for_subset)
//! (preset bindings to commands the frontend lacks are skipped).

/// The default orthodox preset.
pub const ORTHODOX: &str = include_str!("../../presets/keymap/orthodox.toml");
/// Vim-style preset.
pub const VIM: &str = include_str!("../../presets/keymap/vim.toml");
/// CUA preset.
pub const CUA: &str = include_str!("../../presets/keymap/cua.toml");

/// Names of every embedded preset (final review MINOR 4: this catalog
/// used to be mirrored as a hardcoded `&[&str]` in each frontend that
/// needs a "known preset" list or an "available presets" banner message
/// — TUI, GUI — with no single source of truth. `source()` and `NAMES`
/// are now tested against each other below, so a preset added to one and
/// not the other fails CI instead of drifting silently.
pub const NAMES: &[&str] = &["orthodox", "vim", "cua"];

/// Preset source by name; `None` if unknown (caller falls back +
/// reports, same contract the TUI had).
#[must_use]
pub fn source(name: &str) -> Option<&'static str> {
    match name {
        "orthodox" => Some(ORTHODOX),
        "vim" => Some(VIM),
        "cua" => Some(CUA),
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
    #[test]
    fn names_tiene_los_tres_presets_de_fabrica() {
        assert_eq!(NAMES.len(), 3);
    }
}
