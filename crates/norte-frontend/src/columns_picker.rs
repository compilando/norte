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
    /// attr/plugin (etiqueta en [`Self::label`], #117) y para los ids que
    /// no parsean — se enseñan y preservan.
    pub builtin: Option<Builtin>,
    /// Activa = aparece en la lista persistida.
    pub enabled: bool,
    /// Formato VIGENTE de la fila (#108 7b): vocabulario ASCII cerrado
    /// (`"iec"`…); `Some` para Size/Mtime y para attrs con hint Size/
    /// Timestamp/Mode (#117) — el resto no admite formato.
    pub format: Option<String>,
    /// BLOQUEADA (m2 revisión 7b): un spec DEL SCHEME fija el formato — el
    /// picker solo escribe el spec GLOBAL, que el override seguiría
    /// enmascarando (toast mentiroso + fuga a otros schemes). La fila
    /// enseña su formato pero el ciclo es no-op y `finish` jamás la emite.
    pub format_locked: bool,
    /// Etiqueta de display para filas NO builtin (#117): la de
    /// [`crate::columns::header_label`] al abrir (localizada / label del
    /// catálogo YA enmascarado / id saneado). `None` en builtins (Fluent
    /// en vivo) y en ids que no parsean (los frontends caen al id crudo,
    /// que ellos enmascaran).
    pub label: Option<String>,
    /// Hint del catálogo al abrir (attrs): decide la tabla del ciclo de
    /// formato. `Opaque` = sin ciclo.
    pub hint: norte_proto::attrs::AttrHint,
    /// El formato al ABRIR: [`ColumnsPicker::finish`] solo emite los
    /// CAMBIADOS. Privado: nace igual que `format` y no se toca después.
    opened_format: Option<String>,
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
    /// Formatos CAMBIADOS respecto a la apertura (#108 7b), como
    /// `(id, formato)`: solo lo tocado viaja al disco — un spec por
    /// columna, reemplazo por id (`persist_column_format`).
    pub formats: Vec<(String, String)>,
}

/// Construye una fila sembrando `format`/`opened_format` del estilo
/// RESUELTO del scheme (`style_for` ya pliega spec global ← scheme, vía la
/// tabla única [`crate::columns::format_name`], m3) — un único sitio para
/// el invariante «nacen iguales» — y el candado de scheme (m2).
fn make_row(
    id: String,
    builtin: Option<Builtin>,
    enabled: bool,
    settings: &ColumnsSettings,
    scheme: &str,
    catalog: Option<&norte_proto::AttrCatalog>,
) -> PickerRow {
    // #117: un solo parse por fila — attr y plugin dejan de ser opacos: el
    // estilo resuelto trae su hint (tabla del ciclo) y `header_label` su
    // etiqueta (ya enmascarada). Los ids que NO parsean siguen opacos.
    let parsed = id.parse::<ColumnId>().ok();
    let (hint, format, format_locked, label) = match &parsed {
        Some(cid) => {
            let style = settings.style_for_id(scheme, cid, catalog);
            let hint = style.hint;
            let format = crate::columns::format_name_id(cid, &style).map(str::to_owned);
            let locked = settings.format_pinned_by_scheme_id(scheme, cid);
            let label = matches!(cid, ColumnId::Attr(_) | ColumnId::Plugin { .. })
                .then(|| crate::columns::header_label(cid, &style, catalog));
            (hint, format, locked, label)
        }
        None => (norte_proto::attrs::AttrHint::Opaque, None, false, None),
    };
    PickerRow {
        id,
        builtin,
        enabled,
        format_locked,
        label,
        hint,
        opened_format: format.clone(),
        format,
    }
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
    /// Construye el picker para el pane en `scheme` con su orden actual,
    /// sin catálogo de provider ([`Self::open_with_catalog`] con `None`).
    #[must_use]
    pub fn open(settings: &ColumnsSettings, scheme: &str, current_sort: SortSpec) -> Self {
        Self::open_with_catalog(settings, scheme, current_sort, None, &[])
    }

    /// Construye el picker con el catálogo de attrs del provider (#117):
    /// los attrs ANUNCIADOS y no configurados se ofrecen deshabilitados al
    /// final — el picker OFRECE, no impone — y las filas attr ganan hint
    /// (ciclo de formato) y etiqueta del catálogo.
    #[must_use]
    pub fn open_with_catalog(
        settings: &ColumnsSettings,
        scheme: &str,
        current_sort: SortSpec,
        catalog: Option<&norte_proto::AttrCatalog>,
        plugins: &[norte_proto::methods::PluginInfo],
    ) -> Self {
        let raw = settings.raw_ids_for(scheme);
        let mut rows: Vec<PickerRow> = Vec::new();
        for id in &raw {
            let builtin = match id.parse::<ColumnId>() {
                Ok(ColumnId::Builtin(b)) => Some(b),
                _ => None,
            };
            // Dedup SOLO de builtins (mismo criterio que layout_items_for).
            // Los duplicados OPACOS se PRESERVAN a propósito: son intención
            // de config del usuario y el picker jamás la limpia (doctor la
            // reporta) — no "arreglar" este if para deduplicarlos.
            if builtin.is_some() && rows.iter().any(|r| r.builtin == builtin) {
                continue;
            }
            rows.push(make_row(
                id.clone(),
                builtin,
                true,
                settings,
                scheme,
                catalog,
            ));
        }
        // name primero e inmutable (contrato del render).
        if let Some(pos) = rows.iter().position(|r| r.builtin == Some(Builtin::Name)) {
            let name = rows.remove(pos);
            rows.insert(0, name);
        } else {
            rows.insert(
                0,
                make_row(
                    ColumnId::Builtin(Builtin::Name).to_string(),
                    Some(Builtin::Name),
                    true,
                    settings,
                    scheme,
                    catalog,
                ),
            );
        }
        // Catálogo restante, deshabilitado, en orden canónico.
        for b in [Builtin::Size, Builtin::Mtime, Builtin::Kind] {
            if !rows.iter().any(|r| r.builtin == Some(b)) {
                rows.push(make_row(
                    ColumnId::Builtin(b).to_string(),
                    Some(b),
                    false,
                    settings,
                    scheme,
                    catalog,
                ));
            }
        }
        // Catálogo de PROVIDER (#117): attrs anunciados y no configurados,
        // deshabilitados, tras los builtins — el picker OFRECE, no impone.
        if let Some(cat) = catalog {
            for info in cat {
                let id = format!("attr:{}", info.id);
                if !rows.iter().any(|r| r.id == id) {
                    rows.push(make_row(id, None, false, settings, scheme, catalog));
                }
            }
        }
        // Columnas DECLARADAS por plugins (#120), mismo criterio que los attrs:
        // las no configuradas se ofrecen deshabilitadas al final.
        //
        // Solo de plugins aprobados Y activados: ofrecer la columna de uno que
        // el humano no ha consentido sería invitarle a configurar algo que el
        // host se va a negar a servir, y la fila resultante pintaría en blanco
        // sin decir por qué.
        //
        // El id que se ofrece lleva el plugin dentro (`plugin:{p}/{c}`) — es
        // la forma que `[ui.columns]` guarda y la que desde 0.35.0 identifica
        // sin ambigüedad qué plugin sirve la columna cuando dos declaran el
        // mismo id bare.
        for p in plugins {
            if !p.approved || !p.enabled {
                continue;
            }
            for c in &p.columns {
                let id = format!("plugin:{}/{}", p.id, c.id);
                if !rows.iter().any(|r| r.id == id) {
                    rows.push(make_row(id, None, false, settings, scheme, catalog));
                }
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

    /// Cicla el formato de la fila bajo el cursor por su vocabulario
    /// cerrado (#108 7b, tabla única [`crate::columns::next_format_id`]):
    /// size `iec→si→exact→iec`, mtime `relative→iso→relative`, attrs por
    /// la tabla de su hint (#117: Mode `rwx→octal→rwx`…); no-op en
    /// name/kind/opacas (sin formato) y en filas BLOQUEADAS por un spec de
    /// scheme (m2 — ver [`PickerRow::format_locked`]).
    pub fn cycle_format(&mut self) {
        let Some(r) = self.rows.get_mut(self.cursor) else {
            return;
        };
        if r.format_locked {
            return;
        }
        let Ok(cid) = r.id.parse::<ColumnId>() else {
            return;
        };
        let Some(cur) = r.format.as_deref() else {
            return;
        };
        if let Some(next) = crate::columns::next_format_id(&cid, r.hint, cur) {
            r.format = Some(next.to_owned());
        }
    }

    /// Formato vigente de la fila bajo el cursor (`None` = no admite
    /// formato: name/kind/opacas).
    #[must_use]
    pub fn format_of_cursor(&self) -> Option<String> {
        self.rows.get(self.cursor).and_then(|r| r.format.clone())
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
            // Las bloqueadas jamás se emiten (m2): el ciclo ya es no-op en
            // ellas — el filtro extra es defensa en profundidad.
            formats: self
                .rows
                .iter()
                .filter(|r| !r.format_locked && r.format != r.opened_format)
                .filter_map(|r| r.format.clone().map(|f| (r.id.clone(), f)))
                .collect(),
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
    fn cycle_format_rota_el_vocabulario_de_la_columna() {
        let mut p = ColumnsPicker::open(&settings_vacios(), "file", SortSpec::default());
        p.down(); // size (formato default iec)
        p.cycle_format();
        assert_eq!(p.format_of_cursor().as_deref(), Some("si"));
        p.cycle_format();
        assert_eq!(p.format_of_cursor().as_deref(), Some("exact"));
        p.cycle_format();
        assert_eq!(p.format_of_cursor().as_deref(), Some("iec")); // vuelta completa
        // name/kind/opacos: no-op.
        p.up();
        p.cycle_format();
        assert_eq!(p.format_of_cursor(), None);
    }

    /// m2 revisión 7b: un spec DEL SCHEME con formato BLOQUEA el ciclo —
    /// el global que escribiríamos quedaría enmascarado por el override
    /// (toast mentiroso) y fugaría a otros schemes. La fila sigue
    /// enseñando el formato vigente.
    #[test]
    fn formato_fijado_por_scheme_bloquea_el_ciclo() {
        let cfg = norte_config::ColumnsConfig {
            schemes: [(
                "sftp".to_owned(),
                norte_config::SchemeColumns {
                    specs: [(
                        "size".to_owned(),
                        norte_config::ColumnSpec {
                            format: Some("exact".to_owned()),
                            ..Default::default()
                        },
                    )]
                    .into(),
                    ..Default::default()
                },
            )]
            .into(),
            ..Default::default()
        };
        let s = ColumnsSettings::resolve(&cfg);
        let mut p = ColumnsPicker::open(&s, "sftp", SortSpec::default());
        p.down(); // size
        assert_eq!(
            p.format_of_cursor().as_deref(),
            Some("exact"),
            "la fila enseña el formato resuelto del scheme"
        );
        assert!(p.rows()[p.cursor()].format_locked, "candado de scheme");
        p.cycle_format();
        assert_eq!(
            p.format_of_cursor().as_deref(),
            Some("exact"),
            "bloqueada: el ciclo es no-op"
        );
        assert!(
            p.finish().formats.is_empty(),
            "jamás se emite una bloqueada"
        );
        // El MISMO builtin en otro scheme sigue libre (el candado es del
        // scheme, no global).
        let mut libre = ColumnsPicker::open(&s, "file", SortSpec::default());
        libre.down();
        assert!(!libre.rows()[libre.cursor()].format_locked);
        libre.cycle_format();
        assert_eq!(libre.format_of_cursor().as_deref(), Some("si"));
    }

    #[test]
    fn finish_lleva_solo_los_formatos_cambiados() {
        let mut p = ColumnsPicker::open(&settings_vacios(), "file", SortSpec::default());
        p.down();
        p.cycle_format(); // size → si
        let picked = p.finish();
        assert_eq!(picked.formats, vec![("size".to_owned(), "si".to_owned())]);
        // sin cambios → vacío
        let p2 = ColumnsPicker::open(&settings_vacios(), "file", SortSpec::default());
        assert!(p2.finish().formats.is_empty());
        // Ciclar y VOLVER al valor de apertura también es "sin cambios":
        // el diff es contra opened_format, no un flag de "tocado".
        p.cycle_format(); // si → exact
        p.cycle_format(); // exact → iec (valor de apertura)
        assert!(p.finish().formats.is_empty());
    }

    /// El seed parte del estilo RESUELTO del scheme, no del default: con un
    /// spec `format = "si"` en config, el primer ciclo va si→exact.
    #[test]
    fn open_siembra_el_formato_del_estilo_resuelto() {
        let cfg = norte_config::ColumnsConfig {
            specs: [(
                "size".to_owned(),
                norte_config::ColumnSpec {
                    format: Some("si".to_owned()),
                    ..Default::default()
                },
            )]
            .into(),
            ..Default::default()
        };
        let s = ColumnsSettings::resolve(&cfg);
        let mut p = ColumnsPicker::open(&s, "file", SortSpec::default());
        p.down(); // size
        assert_eq!(p.format_of_cursor().as_deref(), Some("si"));
        p.cycle_format();
        assert_eq!(p.format_of_cursor().as_deref(), Some("exact"));
        // Y finish emite el cambio RELATIVO a la apertura (si→exact).
        assert_eq!(
            p.finish().formats,
            vec![("size".to_owned(), "exact".to_owned())]
        );
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
    fn open_with_catalog_ofrece_attrs_no_configurados() {
        use norte_proto::attrs::{AttrHint, AttrInfo, AttrType};
        let cat = norte_proto::AttrCatalog::new(vec![
            AttrInfo {
                id: "posix.mode".into(),
                label: "Mode".into(),
                ty: AttrType::Uint,
                hint: AttrHint::Mode,
            },
            AttrInfo {
                id: "posix.uid".into(),
                label: "UID".into(),
                ty: AttrType::Uint,
                hint: AttrHint::Identity,
            },
        ]);
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec!["name".into(), "attr:posix.mode".into()]),
            ..Default::default()
        };
        let st = ColumnsSettings::resolve(&cfg);
        let mut p =
            ColumnsPicker::open_with_catalog(&st, "file", SortSpec::default(), Some(&cat), &[]);
        // El configurado sigue habilitado; el anunciado no-configurado aparece
        // deshabilitado al final, UNA sola vez.
        let uid: Vec<_> = p
            .rows()
            .iter()
            .filter(|r| r.id == "attr:posix.uid")
            .collect();
        assert_eq!(uid.len(), 1);
        assert!(!uid[0].enabled);
        assert_eq!(
            p.rows()
                .iter()
                .filter(|r| r.id == "attr:posix.mode")
                .count(),
            1
        );
        // La fila attr con hint Mode cicla formato: rwx → octal.
        let modo = p
            .rows()
            .iter()
            .position(|r| r.id == "attr:posix.mode")
            .unwrap();
        assert_eq!(p.rows()[modo].format.as_deref(), Some("rwx"));
        while p.cursor() != modo {
            p.down();
        }
        p.cycle_format();
        assert_eq!(p.format_of_cursor().as_deref(), Some("octal"));
        p.cycle_format();
        assert_eq!(p.format_of_cursor().as_deref(), Some("rwx"), "vuelta");
        // Y la anunciada Identity no admite formato (sin tabla para su hint).
        while p.cursor() + 1 < p.rows().len() {
            p.down();
        }
        assert_eq!(p.rows()[p.cursor()].id, "attr:posix.uid");
        p.cycle_format();
        assert_eq!(p.format_of_cursor(), None);
    }

    #[test]
    fn open_sin_catalogo_conserva_la_conducta_historica() {
        let st = ColumnsSettings::resolve(&norte_config::ColumnsConfig::default());
        let a = ColumnsPicker::open(&st, "file", SortSpec::default());
        let b = ColumnsPicker::open_with_catalog(&st, "file", SortSpec::default(), None, &[]);
        assert_eq!(a.rows(), b.rows());
    }

    /// #117 review tarea 4 (a): una fila OFRECIDA del catálogo (nace
    /// deshabilitada) se puede encender y su id viaja en `finish().ids` —
    /// ofrecer sin poder elegir sería un picker de mentira.
    #[test]
    fn fila_ofrecida_del_catalogo_se_enciende_y_viaja_en_finish() {
        use norte_proto::attrs::{AttrHint, AttrInfo, AttrType};
        let cat = norte_proto::AttrCatalog::new(vec![AttrInfo {
            id: "posix.uid".into(),
            label: "UID".into(),
            ty: AttrType::Uint,
            hint: AttrHint::Identity,
        }]);
        let st = ColumnsSettings::resolve(&norte_config::ColumnsConfig::default());
        let mut p =
            ColumnsPicker::open_with_catalog(&st, "file", SortSpec::default(), Some(&cat), &[]);
        assert!(!p.finish().ids.contains(&"attr:posix.uid".to_owned()));
        while p.rows()[p.cursor()].id != "attr:posix.uid" {
            p.down();
        }
        p.toggle();
        assert!(p.rows()[p.cursor()].enabled);
        assert!(p.finish().ids.contains(&"attr:posix.uid".to_owned()));
    }

    /// #117 review tarea 4 (b): un label HOSTIL del catálogo llega YA
    /// enmascarado a `PickerRow.label` (`header_label` es el choke point; el
    /// re-enmascarado de los frontends es cinturón, no la defensa).
    #[test]
    fn label_hostil_del_catalogo_llega_enmascarado() {
        use norte_proto::attrs::{AttrHint, AttrInfo, AttrType};
        let cat = norte_proto::AttrCatalog::new(vec![AttrInfo {
            // `mem.` no es namespace de primera parte: sin clave Fluent, el
            // label del catálogo es el que se enseña.
            id: "mem.owner".into(),
            label: "Owner\u{202e}evil".into(),
            ty: AttrType::Bytes,
            hint: AttrHint::Identity,
        }]);
        let st = ColumnsSettings::resolve(&norte_config::ColumnsConfig::default());
        let p = ColumnsPicker::open_with_catalog(&st, "file", SortSpec::default(), Some(&cat), &[]);
        let fila = p
            .rows()
            .iter()
            .find(|r| r.id == "attr:mem.owner")
            .expect("fila ofrecida");
        let label = fila.label.as_deref().expect("label del catálogo");
        assert!(
            !label.chars().any(norte_encoding::is_terminal_hazard),
            "hazard crudo en label: {label:?}"
        );
        assert!(label.contains("Owner"), "conserva lo inocuo: {label:?}");
    }

    #[test]
    fn scheme_con_override_apunta_al_scheme() {
        let cfg = norte_config::ColumnsConfig {
            schemes: [(
                "sftp".to_owned(),
                norte_config::SchemeColumns {
                    columns: Some(vec!["name".into(), "mtime".into()]),
                    sort: None,
                    ..Default::default()
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

#[cfg(test)]
mod plugin_offer_tests {
    use super::*;

    fn plugin(
        id: &str,
        cols: &[&str],
        approved: bool,
        enabled: bool,
    ) -> norte_proto::methods::PluginInfo {
        norte_proto::methods::PluginInfo {
            id: id.to_owned(),
            name: id.to_owned(),
            publisher: String::new(),
            version: "1.0.0".to_owned(),
            category: "columns".to_owned(),
            capabilities: Vec::new(),
            approved,
            enabled,
            description: None,
            commands: Vec::new(),
            columns: cols
                .iter()
                .map(|c| norte_proto::methods::PluginColumnInfo {
                    id: (*c).to_owned(),
                    header: (*c).to_owned(),
                })
                .collect(),
            has_help: false,
        }
    }

    /// Una columna declarada por un plugin consentido se OFRECE (#120). Antes
    /// solo se llegaba a ella editando `[ui.columns]` a mano, que es tanto
    /// como no ofrecerla.
    #[test]
    fn se_ofrece_la_columna_de_un_plugin_consentido() {
        let st = ColumnsSettings::resolve(&norte_config::ColumnsConfig::default());
        let plugins = [plugin("org.norte.git", &["status"], true, true)];
        let p = ColumnsPicker::open_with_catalog(&st, "file", SortSpec::default(), None, &plugins);
        let fila = p
            .rows()
            .iter()
            .find(|r| r.id == "plugin:org.norte.git/status")
            .expect("la columna declarada debe ofrecerse");
        assert!(
            !fila.enabled,
            "se OFRECE deshabilitada: el picker ofrece, no impone"
        );
    }

    /// Un plugin sin consentir no se ofrece: configurarlo daría una columna
    /// que el host se niega a servir y que pintaría en blanco sin decir por qué.
    #[test]
    fn no_se_ofrece_lo_que_el_humano_no_ha_consentido() {
        let st = ColumnsSettings::resolve(&norte_config::ColumnsConfig::default());
        for (approved, enabled) in [(false, true), (true, false), (false, false)] {
            let plugins = [plugin("org.norte.git", &["status"], approved, enabled)];
            let p =
                ColumnsPicker::open_with_catalog(&st, "file", SortSpec::default(), None, &plugins);
            assert!(
                !p.rows().iter().any(|r| r.id.starts_with("plugin:")),
                "approved={approved} enabled={enabled} no debe ofrecerse"
            );
        }
    }

    /// Dos plugins pueden declarar el MISMO id bare, y las dos filas tienen que
    /// existir por separado: el id que se ofrece lleva el plugin dentro, que es
    /// justo lo que desde 0.35.0 el wire sabe distinguir (#120).
    #[test]
    fn dos_plugins_con_el_mismo_id_bare_dan_dos_filas() {
        let st = ColumnsSettings::resolve(&norte_config::ColumnsConfig::default());
        let plugins = [
            plugin("org.a.tool", &["status"], true, true),
            plugin("org.b.tool", &["status"], true, true),
        ];
        let p = ColumnsPicker::open_with_catalog(&st, "file", SortSpec::default(), None, &plugins);
        for id in ["plugin:org.a.tool/status", "plugin:org.b.tool/status"] {
            assert!(
                p.rows().iter().any(|r| r.id == id),
                "falta la fila `{id}`: colapsarlas escondería una columna que el \
                 usuario sí puede configurar"
            );
        }
    }
}
