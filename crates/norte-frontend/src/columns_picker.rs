//! Modelo PURO del picker de columnas (#108 bloque 7a): estado y
//! transiciones sin IO ni render — la TUI lo envuelve en un overlay y la
//! GUI (7c) en un panel, misma máquina. Regla 7: la lógica vive aquí.

use crate::columns::{Builtin, ColumnId, ColumnsSettings, sort_column};
use crate::sort::SortSpec;

/// Una fila del picker: el id en forma Display (lo que se persiste),
/// su builtin si lo es (etiqueta Fluent + ordenable), y si está activa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerRow {
    /// Id tal cual viaja a la config (`"size"`, `"attr:posix.mode"`…).
    pub id: String,
    /// `Some` para los builtin (etiqueta localizada, sort); `None` para
    /// ids sin renderer o que no parsean — se enseñan y preservan.
    pub builtin: Option<Builtin>,
    /// Activa = aparece en la lista persistida.
    pub enabled: bool,
}

/// Resultado de confirmar el picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picked {
    /// Ids habilitados, en orden de pintado.
    pub ids: Vec<String>,
    /// El orden elegido.
    pub sort: SortSpec,
    /// `Some(scheme)` si el save va al override del scheme; `None` = default.
    pub scheme_target: Option<String>,
}

/// Estado del picker. `open` parte del set EFECTIVO del pane (override del
/// scheme > default) y cierra la lista con el resto del catálogo builtin
/// deshabilitado. `name` va primero y es inmutable (ni toggle ni desplazable
/// de la cabeza: la primera columna ES el nombre por contrato del render).
#[derive(Debug, Clone)]
pub struct ColumnsPicker {
    rows: Vec<PickerRow>,
    cursor: usize,
    sort: SortSpec,
    scheme: String,
    scheme_override: bool,
}

impl ColumnsPicker {
    /// Construye el picker para el pane en `scheme` con su orden actual.
    #[must_use]
    pub fn open(settings: &ColumnsSettings, scheme: &str, current_sort: SortSpec) -> Self {
        let raw = settings.raw_ids_for(scheme);
        let mut rows: Vec<PickerRow> = Vec::new();
        for id in &raw {
            let builtin = match id.parse::<ColumnId>() {
                Ok(ColumnId::Builtin(b)) => Some(b),
                _ => None,
            };
            if builtin.is_some() && rows.iter().any(|r| r.builtin == builtin) {
                continue; // dedup builtin, mismo criterio que layout_items_for
            }
            rows.push(PickerRow {
                id: id.clone(),
                builtin,
                enabled: true,
            });
        }
        // name primero e inmutable (contrato del render).
        if let Some(pos) = rows.iter().position(|r| r.builtin == Some(Builtin::Name)) {
            let name = rows.remove(pos);
            rows.insert(0, name);
        } else {
            rows.insert(
                0,
                PickerRow {
                    id: ColumnId::Builtin(Builtin::Name).to_string(),
                    builtin: Some(Builtin::Name),
                    enabled: true,
                },
            );
        }
        // Catálogo restante, deshabilitado, en orden canónico.
        for b in [Builtin::Size, Builtin::Mtime, Builtin::Kind] {
            if !rows.iter().any(|r| r.builtin == Some(b)) {
                rows.push(PickerRow {
                    id: ColumnId::Builtin(b).to_string(),
                    builtin: Some(b),
                    enabled: false,
                });
            }
        }
        Self {
            rows,
            cursor: 0,
            sort: current_sort,
            scheme: scheme.to_owned(),
            scheme_override: settings.has_scheme_entry(scheme),
        }
    }

    /// Filas en orden de pintado.
    #[must_use]
    pub fn rows(&self) -> &[PickerRow] {
        &self.rows
    }

    /// Fila bajo el cursor.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// El orden en curso (se aplica al confirmar, no antes).
    #[must_use]
    pub fn sort(&self) -> SortSpec {
        self.sort
    }

    /// Scheme del pane que abrió el picker.
    #[must_use]
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// ¿El save irá al override del scheme?
    #[must_use]
    pub fn scheme_override(&self) -> bool {
        self.scheme_override
    }

    /// Cursor arriba (saturante).
    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Cursor abajo (saturante).
    pub fn down(&mut self) {
        self.cursor = (self.cursor + 1).min(self.rows.len().saturating_sub(1));
    }

    /// Activa/desactiva la fila bajo el cursor. `name` es inmutable.
    pub fn toggle(&mut self) {
        if self.cursor == 0 {
            return;
        }
        if let Some(r) = self.rows.get_mut(self.cursor) {
            r.enabled = !r.enabled;
        }
    }

    /// Sube la fila bajo el cursor un puesto (jamás por encima de `name`);
    /// el cursor la sigue.
    pub fn move_up(&mut self) {
        if self.cursor > 1 {
            self.rows.swap(self.cursor, self.cursor - 1);
            self.cursor -= 1;
        }
    }

    /// Baja la fila bajo el cursor un puesto; el cursor la sigue.
    pub fn move_down(&mut self) {
        if self.cursor >= 1 && self.cursor + 1 < self.rows.len() {
            self.rows.swap(self.cursor, self.cursor + 1);
            self.cursor += 1;
        }
    }

    /// Ordena por la columna bajo el cursor con la semántica del click de
    /// cabecera ([`SortSpec::after_click`]); no-op en filas no ordenables.
    pub fn sort_current(&mut self) {
        if let Some(sc) = self
            .rows
            .get(self.cursor)
            .and_then(|r| r.builtin)
            .and_then(sort_column)
        {
            self.sort = self.sort.after_click(sc);
        }
    }

    /// El resultado a aplicar/persistir al confirmar.
    #[must_use]
    pub fn finish(&self) -> Picked {
        Picked {
            ids: self
                .rows
                .iter()
                .filter(|r| r.enabled)
                .map(|r| r.id.clone())
                .collect(),
            sort: self.sort,
            scheme_target: self.scheme_override.then(|| self.scheme.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::columns::{Builtin, ColumnsSettings};
    use crate::sort::{SortColumn, SortDir, SortSpec};

    fn settings_vacios() -> ColumnsSettings {
        ColumnsSettings::resolve(&norte_config::ColumnsConfig::default())
    }

    #[test]
    fn abre_con_el_set_efectivo_y_el_catalogo_restante() {
        let p = ColumnsPicker::open(&settings_vacios(), "file", SortSpec::default());
        // Default efectivo: name+size+mtime habilitadas; kind presente deshabilitada.
        let estados: Vec<(Option<Builtin>, bool)> =
            p.rows().iter().map(|r| (r.builtin, r.enabled)).collect();
        assert_eq!(
            estados,
            vec![
                (Some(Builtin::Name), true),
                (Some(Builtin::Size), true),
                (Some(Builtin::Mtime), true),
                (Some(Builtin::Kind), false),
            ]
        );
        assert!(!p.scheme_override(), "sin config no hay override de scheme");
    }

    #[test]
    fn toggle_apaga_y_enciende_pero_name_es_inmutable() {
        let mut p = ColumnsPicker::open(&settings_vacios(), "file", SortSpec::default());
        p.toggle(); // cursor 0 = name
        assert!(p.rows()[0].enabled, "name jamás se apaga");
        p.down();
        p.toggle();
        assert!(!p.rows()[1].enabled, "size se apaga");
        p.toggle();
        assert!(p.rows()[1].enabled);
    }

    #[test]
    fn mover_reordena_pero_jamas_por_encima_de_name() {
        let mut p = ColumnsPicker::open(&settings_vacios(), "file", SortSpec::default());
        p.down(); // size
        p.down(); // mtime
        p.move_up(); // mtime <-> size
        let orden: Vec<Option<Builtin>> = p.rows().iter().map(|r| r.builtin).collect();
        assert_eq!(orden[1], Some(Builtin::Mtime));
        assert_eq!(orden[2], Some(Builtin::Size));
        assert_eq!(p.cursor(), 1, "el cursor sigue a la fila movida");
        p.move_up(); // ya toca name: no-op
        assert_eq!(p.rows()[0].builtin, Some(Builtin::Name));
        assert_eq!(p.cursor(), 1);
    }

    #[test]
    fn sort_current_aplica_after_click_sobre_la_fila() {
        let mut p = ColumnsPicker::open(&settings_vacios(), "file", SortSpec::default());
        p.down(); // size
        p.sort_current();
        assert_eq!(p.sort().column, SortColumn::Size);
        assert_eq!(p.sort().dir, SortDir::Asc);
        p.sort_current(); // segunda vez invierte
        assert_eq!(p.sort().dir, SortDir::Desc);
    }

    #[test]
    fn sort_current_en_fila_no_ordenable_es_noop() {
        let mut p = ColumnsPicker::open(&settings_vacios(), "file", SortSpec::default());
        for _ in 0..3 {
            p.down(); // kind
        }
        let antes = p.sort();
        p.sort_current();
        assert_eq!(p.sort(), antes, "kind no es ordenable");
    }

    #[test]
    fn finish_emite_solo_las_habilitadas_en_orden() {
        let mut p = ColumnsPicker::open(&settings_vacios(), "file", SortSpec::default());
        p.down();
        p.toggle(); // size fuera
        let picked = p.finish();
        assert_eq!(picked.ids, vec!["name".to_owned(), "mtime".to_owned()]);
        assert_eq!(picked.scheme_target, None, "sin override → default");
    }

    #[test]
    fn ids_opacos_se_preservan_y_viajan_enteros() {
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec![
                "name".into(),
                "attr:posix.mode".into(),
                "size".into(),
                "no parsea!".into(),
            ]),
            ..Default::default()
        };
        let s = ColumnsSettings::resolve(&cfg);
        let mut p = ColumnsPicker::open(&s, "file", SortSpec::default());
        // Los opacos están, habilitados, en su posición; el catálogo restante
        // (mtime, kind) cierra la lista deshabilitado.
        let ids: Vec<&str> = p.rows().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids[..4], ["name", "attr:posix.mode", "size", "no parsea!"]);
        let picked = p.finish();
        assert!(picked.ids.contains(&"attr:posix.mode".to_owned()));
        assert!(picked.ids.contains(&"no parsea!".to_owned()));
        // Y son toggleables: apagar el attr lo saca del resultado.
        p.down();
        p.toggle();
        assert!(!p.finish().ids.contains(&"attr:posix.mode".to_owned()));
    }

    #[test]
    fn scheme_con_override_apunta_al_scheme() {
        let cfg = norte_config::ColumnsConfig {
            schemes: [(
                "sftp".to_owned(),
                norte_config::SchemeColumns {
                    columns: Some(vec!["name".into(), "mtime".into()]),
                    sort: None,
                },
            )]
            .into(),
            ..Default::default()
        };
        let s = ColumnsSettings::resolve(&cfg);
        let p = ColumnsPicker::open(&s, "sftp", SortSpec::default());
        assert!(p.scheme_override());
        assert_eq!(p.finish().scheme_target.as_deref(), Some("sftp"));
        let habilitadas: Vec<&str> = p
            .rows()
            .iter()
            .filter(|r| r.enabled)
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(habilitadas, ["name", "mtime"]);
    }
}
