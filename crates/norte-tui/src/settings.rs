//! Filas del overlay de ajustes (`app.settings`, S3): mismo criterio que
//! [`crate::palette`] — este módulo solo CONSTRUYE las filas
//! ([`build_rows`]); el editor que las consume (`app::Settings`, cursor +
//! filtro + edición) vive en `app.rs`, junto a `Palette`.
//!
//! HONEST SCOPE (S3, ver la spec `2026-07-24-settings-ui-cursor-memory-
//! design.md`): la sección General nace del catálogo curado
//! [`norte_frontend::settings::catalog`] (S2) × el valor VIGENTE de la
//! config (S2 `current_value`) × nombre/descripción Fluent — igual criterio
//! que `palette::build_rows` con `help-cmd-*`. La sección Plugins NO edita
//! valores: el wire de P2 no expone ni el esquema (`ConfigKeySpec` del
//! manifiesto) ni los valores actuales de un plugin (`settings_of`) fuera
//! del proceso que lo cargó — un frontend REMOTO (el caso general del
//! `Backend`) no tiene de dónde leerlos. [`build_rows`] añade una única fila
//! INFORMATIVA que apunta al camino manual de hoy; la edición completa llega
//! con el bump de wire de G3 (ver la spec).
use norte_frontend::settings::{catalog, current_value, fluent_desc_id, fluent_name_id};
use norte_i18n::t;

use crate::config::LoadedConfig;

/// Una fila del overlay de ajustes — construida, jamás calculada por el
/// editor ([`crate::app::Settings`] solo la consume).
#[derive(Debug, Clone)]
pub struct Row {
    /// Índice en [`norte_frontend::settings::catalog`]; `None` para la fila
    /// informativa de Plugins (ver el doc del módulo) — nunca editable, y el
    /// editor la reconoce por esto, no por texto.
    pub(crate) def_index: Option<usize>,
    /// Nombre localizado (Fluent) a pintar.
    pub name: String,
    /// Descripción localizada (Fluent) — pie de página de la fila
    /// seleccionada ([`crate::ui`]).
    pub desc: String,
    /// Valor actual en texto de presentación; vacío para la fila
    /// informativa (no tiene un valor único que mostrar).
    pub value: String,
}

impl Row {
    /// `true` para la fila informativa de Plugins (ver doc del módulo):
    /// nunca editable, el editor la salta en `activate`.
    #[must_use]
    pub fn is_plugins_note(&self) -> bool {
        self.def_index.is_none()
    }
}

/// La fila informativa de la sección Plugins (ver HONEST SCOPE arriba).
fn plugins_note_row() -> Row {
    Row {
        def_index: None,
        name: t("settings-plugins-name"),
        desc: t("settings-plugins-note"),
        value: String::new(),
    }
}

/// Construye las filas del overlay: el catálogo GENERAL (S2) × el valor
/// VIGENTE de `cfg` × nombre/descripción localizados, más la fila
/// informativa de Plugins al final. Se llama al ABRIR (`app.settings`,
/// `main::dispatch`) y en cada hot-reload OK con el `cfg` vigente
/// (`main::reload_config`) — mismo criterio que `help_lines`/`palette_rows`:
/// reconstruidas, jamás mutadas fila a fila.
#[must_use]
pub fn build_rows(cfg: &LoadedConfig) -> Vec<Row> {
    let mut rows: Vec<Row> = catalog()
        .iter()
        .enumerate()
        .map(|(i, def)| Row {
            def_index: Some(i),
            name: t(&fluent_name_id(def.id)),
            desc: t(&fluent_desc_id(def.id)),
            value: current_value(def, cfg),
        })
        .collect();
    rows.push(plugins_note_row());
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_vacia() -> LoadedConfig {
        crate::config::load(&norte_config::Layers { dirs: vec![] }).expect("config vacía carga")
    }

    /// Una fila por entrada del catálogo, más EXACTAMENTE una fila
    /// informativa al final (HONEST SCOPE del doc del módulo).
    #[test]
    fn build_rows_una_fila_por_entrada_mas_la_de_plugins() {
        let rows = build_rows(&cfg_vacia());
        assert_eq!(rows.len(), catalog().len() + 1);
        assert!(!rows[0].is_plugins_note());
        assert!(rows.last().unwrap().is_plugins_note());
    }

    /// El valor de cada fila General es EXACTAMENTE el que `current_value`
    /// (S2) resolvería para el mismo `def` — ni una copia divergente.
    #[test]
    fn build_rows_valores_coinciden_con_current_value() {
        let cfg = cfg_vacia();
        let rows = build_rows(&cfg);
        for (i, def) in catalog().iter().enumerate() {
            assert_eq!(rows[i].value, current_value(def, &cfg));
        }
    }

    /// La fila informativa de Plugins no lleva valor (nada que editar) y su
    /// nombre/descripción resuelven a texto REAL (no al id Fluent crudo) en
    /// el idioma activo de esta suite.
    #[test]
    fn plugins_note_row_sin_valor_y_con_texto_traducido() {
        let rows = build_rows(&cfg_vacia());
        let note = rows.last().unwrap();
        assert_eq!(note.value, "");
        assert_ne!(note.name, "settings-plugins-name");
        assert_ne!(note.desc, "settings-plugins-note");
    }
}
