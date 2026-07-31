//! Column cells contributed by a `columns` plugin (G3c: the GUI-side half
//! of the G3b deferral — `PluginInfo::columns` discovery landed in G3c
//! too, see [`norte_proto::methods::PluginColumnInfo`]). Masking for a
//! column's HEADER and its per-entry cell values, same untrusted-text
//! criterion the crate's decoration module already applies to a badge — a
//! `columns` plugin's `header` and cell text are THIRD-PARTY, never
//! painted raw.

use std::collections::HashMap;

use norte_proto::VPath;

/// Cap on a cell value AFTER masking, in CHARACTERS — same "short and
/// monospaced" criterion as a badge
/// ([`crate::decoration::BADGE_MAX_CHARS`]), but a column cell needs a bit
/// more room (e.g. `"modified 2h ago"`). The actual FIXED render width is
/// each frontend's call; this cap is an anti-DoS backstop, not the paint
/// width.
pub const COLUMN_VALUE_MAX_CHARS: usize = 32;

/// Masks a column HEADER ([`norte_proto::methods::PluginColumnInfo::header`],
/// plugin text — untrusted).
#[must_use]
pub fn sanitize_header(header: &str) -> String {
    crate::display_name(header.as_bytes()).0
}

/// Masks and truncates ONE cell value to [`COLUMN_VALUE_MAX_CHARS`]
/// characters AFTER masking (truncating before masking could cut a
/// multi-byte hazard mid-sequence); `None` (the column doesn't apply to
/// that entry) and an all-hazard value that masks to empty both collapse
/// to `None` — a frontend paints nothing for either, same as a badge.
#[must_use]
pub fn sanitize_cell(value: Option<&str>) -> Option<String> {
    value.and_then(|v| {
        let masked = crate::display_name(v.as_bytes()).0;
        let truncated: String = masked.chars().take(COLUMN_VALUE_MAX_CHARS).collect();
        (!truncated.is_empty()).then_some(truncated)
    })
}

/// Flattens the POSITIONAL `values` (1:1 with `paths`, the
/// `PLUGIN_COLUMN_VALUES` wire contract) into a `HashMap<VPath, String>` of
/// already-sanitized cells, one entry per path that got a non-empty cell —
/// same "flatten the wire's positional contract to a map keyed by path"
/// shape as [`crate::merge_decorations`]. `paths`/`values` are walked with
/// `zip` (stops at the shorter): defense in depth if a remote daemon broke
/// the 1:1 contract (already validated server-side, but a client never
/// trusts blindly).
#[must_use]
pub fn sanitize_column_values(
    paths: &[VPath],
    values: &[Option<String>],
) -> HashMap<VPath, String> {
    paths
        .iter()
        .zip(values.iter())
        .filter_map(|(p, v)| sanitize_cell(v.as_deref()).map(|s| (p.clone(), s)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(s: &str) -> VPath {
        VPath::parse(s).unwrap()
    }

    #[test]
    fn sanitize_header_enmascara_hostil() {
        let hostil = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "rtl_override")
            .expect("fixture del corpus");
        let header = String::from_utf8_lossy(&hostil.bytes).into_owned();
        let out = sanitize_header(&header);
        assert!(!out.chars().any(norte_encoding::is_terminal_hazard));
    }

    #[test]
    fn sanitize_cell_none_pasa_a_none() {
        assert_eq!(sanitize_cell(None), None);
    }

    #[test]
    fn sanitize_cell_trunca_tras_enmascarar() {
        let largo = "a".repeat(1000);
        let out = sanitize_cell(Some(&largo)).unwrap();
        assert_eq!(out.chars().count(), COLUMN_VALUE_MAX_CHARS);
    }

    #[test]
    fn sanitize_cell_vacio_tras_enmascarar_es_none() {
        assert_eq!(sanitize_cell(Some("")), None);
    }

    #[test]
    fn sanitize_column_values_posicional_con_none_intercalado() {
        let paths = vec![vp("mem:///a.rs"), vp("mem:///b.rs"), vp("mem:///c.rs")];
        let values = vec![Some("modified".to_owned()), None, Some(String::new())];
        let map = sanitize_column_values(&paths, &values);
        assert_eq!(
            map.get(&vp("mem:///a.rs")).map(String::as_str),
            Some("modified")
        );
        assert!(!map.contains_key(&vp("mem:///b.rs")));
        assert!(
            !map.contains_key(&vp("mem:///c.rs")),
            "cadena vacía tras enmascarar no entra en el mapa"
        );
    }

    #[test]
    fn sanitize_column_values_longitudes_distintas_no_panica() {
        let paths = vec![vp("mem:///a.rs"), vp("mem:///b.rs")];
        let values = vec![Some("x".to_owned())];
        let map = sanitize_column_values(&paths, &values);
        assert_eq!(map.len(), 1);
    }
}

// ---------------------------------------------------------------------------
// #108 bloque 3.2: el MODELO de columnas compartido (spec 2026-07-24, L1).
// Tipos + layout + formatters; catálogo/config/picker llegan en los bloques
// 4/6/7. Todo puro: los frontends solo pintan (regla 7).
// ---------------------------------------------------------------------------

/// Columna built-in, derivada de la `Entry` tal cual existe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    /// El nombre (último segmento). Nunca se descarta en el layout.
    Name,
    /// `Entry.size`.
    Size,
    /// `Entry.mtime_ms`.
    Mtime,
    /// `Entry.kind`, como texto localizado.
    Kind,
}

/// Identidad de una columna (#108): built-in, atributo de provider
/// (`attr:posix.mode`, bloque 2) o columna de plugin
/// (`plugin:git-status/branch`, ADR 0037). Forma string ESTABLE de config
/// vía `FromStr`/`Display` (round-trip pineado).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnId {
    /// Built-in (`"name"`, `"size"`, `"mtime"`, `"kind"`).
    Builtin(Builtin),
    /// Atributo de provider por id namespaced (`"attr:<id>"`).
    Attr(String),
    /// Columna de plugin (`"plugin:<plugin>/<column>"`).
    Plugin {
        /// Id reverse-DNS del plugin.
        plugin: String,
        /// Id de la columna dentro del plugin.
        column: String,
    },
}

/// Un id de columna que no parsea (#108): valor de DIAGNÓSTICO — jamás un
/// panic y jamás un drop silencioso (doctor lo reporta, bloque 4).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("column id inválido: {reason}")]
pub struct ColumnIdError {
    /// Por qué no parsea (texto NEUTRO: no interpola el input del usuario —
    /// el diagnóstico completo con la fuente lo arma doctor).
    pub reason: &'static str,
}

impl std::str::FromStr for ColumnId {
    type Err = ColumnIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "name" => return Ok(Self::Builtin(Builtin::Name)),
            "size" => return Ok(Self::Builtin(Builtin::Size)),
            "mtime" => return Ok(Self::Builtin(Builtin::Mtime)),
            "kind" => return Ok(Self::Builtin(Builtin::Kind)),
            _ => {}
        }
        if let Some(attr) = s.strip_prefix("attr:") {
            if attr.is_empty() {
                return Err(ColumnIdError {
                    reason: "attr: sin id",
                });
            }
            return Ok(Self::Attr(attr.to_owned()));
        }
        if let Some(rest) = s.strip_prefix("plugin:") {
            let Some((plugin, column)) = rest.split_once('/') else {
                return Err(ColumnIdError {
                    reason: "plugin: sin '/' entre plugin y columna",
                });
            };
            if plugin.is_empty() || column.is_empty() {
                return Err(ColumnIdError {
                    reason: "plugin: id o columna vacíos",
                });
            }
            return Ok(Self::Plugin {
                plugin: plugin.to_owned(),
                column: column.to_owned(),
            });
        }
        Err(ColumnIdError {
            reason: "ni built-in ni attr:/plugin:",
        })
    }
}

impl std::fmt::Display for ColumnId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Builtin(Builtin::Name) => f.write_str("name"),
            Self::Builtin(Builtin::Size) => f.write_str("size"),
            Self::Builtin(Builtin::Mtime) => f.write_str("mtime"),
            Self::Builtin(Builtin::Kind) => f.write_str("kind"),
            Self::Attr(id) => write!(f, "attr:{id}"),
            Self::Plugin { plugin, column } => write!(f, "plugin:{plugin}/{column}"),
        }
    }
}

/// Política de ancho de una columna (celdas de terminal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidthPolicy {
    /// Ancho fijo.
    Fixed(u16),
    /// El de la celda más ancha de la página actual, con techo
    /// [`AUTO_CEILING`].
    Auto,
    /// Reparte el espacio sobrante por peso, nunca por debajo de `min`.
    Flex {
        /// Suelo en celdas.
        min: u16,
        /// Peso relativo del reparto.
        weight: u16,
    },
}

/// Techo de una columna `Auto` (anti-DoS de render: una celda kilométrica
/// hostil no roba el pane).
pub const AUTO_CEILING: u16 = 32;

/// Suelo del NOMBRE: nunca se descarta y nunca baja de aquí.
pub const NAME_MIN: u16 = 10;

/// Alineación de una celda.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// Izquierda (texto).
    Left,
    /// Derecha (números).
    Right,
}

/// Cómo se trunca una celda que no cabe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Truncate {
    /// Cola fuera.
    End,
    /// Elipsis central (paths/nombres).
    Middle,
}

/// Formato de tamaño.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeFormat {
    /// Dígitos crudos, sin separadores — libre de locale, estable en
    /// snapshots.
    Exact,
    /// Binario (`KiB`/`MiB`), vía [`crate::human_bytes`].
    Iec,
    /// Decimal (`kB`/`MB`).
    Si,
}

/// Formato de tiempo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeFormat {
    /// «hace 2h», por Fluent (`col-time-*`). Necesita el `now` del caller.
    Relative,
    /// RFC 3339 UTC al minuto (`2026-07-31T09:41Z`) — libre de locale.
    Iso,
}

/// Entrada del [`layout`]: política + medida de la página (para `Auto`) +
/// si es la columna del NOMBRE (jamás se descarta).
#[derive(Debug, Clone, Copy)]
pub struct LayoutItem {
    /// Política de ancho.
    pub policy: WidthPolicy,
    /// Celdas de la celda más ancha medida en la página (solo `Auto` la lee).
    pub measured: u16,
    /// ¿Es la columna del nombre?
    pub is_name: bool,
}

/// Reparte `available` celdas entre las columnas (#108 L1, puro y
/// compartido por ambos frontends). `None` = columna DESCARTADA (no cupo).
/// Reglas, todas pineadas:
/// 1. `Fixed` toma su ancho; `Auto` la medida con techo [`AUTO_CEILING`];
///    `Flex` parte de su `min` y el sobrante se reparte por peso.
/// 2. Si el total no cabe, se descartan columnas desde la MÁS A LA DERECHA
///    de MENOR peso (las `Fixed`/`Auto` cuentan como peso 0) hasta caber.
/// 3. El NOMBRE jamás se descarta y jamás baja de [`NAME_MIN`] (si ni eso
///    cabe, se lleva `available.max(1)` — con `available == 0` devuelve 1:
///    un nombre de ancho cero es impintable).
/// 4. Suma de anchos devueltos ≤ `available` SALVO la excepción del suelo
///    del nombre de la regla 3; cada ancho devuelto ≥ 1.
#[must_use]
pub fn layout(available: u16, items: &[LayoutItem]) -> Vec<Option<u16>> {
    debug_assert!(
        items.iter().filter(|it| it.is_name).count() <= 1,
        "a lo sumo UNA columna de nombre (review m2)"
    );
    let mut alive: Vec<bool> = items.iter().map(|_| true).collect();
    loop {
        // Base de cada columna viva.
        let base: Vec<u16> = items
            .iter()
            .map(|it| {
                // El suelo del nombre aplica bajo CUALQUIER política
                // (review m2): el contrato de la regla 3 no es Flex-only.
                let floor = if it.is_name { NAME_MIN } else { 1 };
                match it.policy {
                    WidthPolicy::Fixed(w) => w.max(floor),
                    WidthPolicy::Auto => it.measured.clamp(floor, AUTO_CEILING.max(floor)),
                    WidthPolicy::Flex { min, .. } => min.max(floor),
                }
            })
            .collect();
        let total: u32 = base
            .iter()
            .zip(&alive)
            .filter(|(_, a)| **a)
            .map(|(w, _)| u32::from(*w))
            .sum();
        if total <= u32::from(available) {
            // Cabe: reparte el sobrante entre las Flex vivas por peso.
            let sobrante = u32::from(available) - total;
            let peso_total: u64 = items
                .iter()
                .zip(&alive)
                .filter(|(_, a)| **a)
                .map(|(it, _)| match it.policy {
                    WidthPolicy::Flex { weight, .. } => u64::from(weight),
                    _ => 0,
                })
                .sum();
            let mut out = Vec::with_capacity(items.len());
            let mut repartido = 0u64;
            let mut flex_vistos = 0u64;
            for (i, it) in items.iter().enumerate() {
                if !alive[i] {
                    out.push(None);
                    continue;
                }
                let extra = match it.policy {
                    WidthPolicy::Flex { weight, .. } if peso_total > 0 => {
                        flex_vistos += u64::from(weight);
                        // Reparto acumulativo sin restos perdidos. En u64
                        // (review M1): sobrante(≤65535) × pesos acumulados
                        // (sin tope: config/plugins) desbordaba u32 con
                        // pesos grandes — panic en debug, anchos basura en
                        // release.
                        let hasta = u64::from(sobrante) * flex_vistos / peso_total;
                        let e = hasta - repartido;
                        repartido = hasta;
                        e
                    }
                    _ => 0,
                };
                let w = u64::from(base[i]) + extra;
                out.push(Some(u16::try_from(w).unwrap_or(u16::MAX)));
            }
            return out;
        }
        // No cabe: descarta la más a la derecha de menor peso (jamás el
        // nombre). Si solo queda el nombre, dale todo lo disponible.
        let victima = items
            .iter()
            .enumerate()
            .filter(|(i, it)| alive[*i] && !it.is_name)
            .min_by_key(|(i, it)| {
                let peso = match it.policy {
                    WidthPolicy::Flex { weight, .. } => weight,
                    _ => 0,
                };
                (peso, std::cmp::Reverse(*i))
            })
            .map(|(i, _)| i);
        match victima {
            Some(i) => alive[i] = false,
            None => {
                // Solo el nombre (o nada) vive: todo para él.
                return items
                    .iter()
                    .enumerate()
                    .map(|(i, it)| (alive[i] && it.is_name).then_some(available.max(1)))
                    .collect();
            }
        }
    }
}

/// Tamaño según formato (#108). `Exact` son dígitos crudos; `Iec` reusa
/// [`crate::human_bytes`]; `Si` decimal con una cifra.
#[must_use]
pub fn format_size(n: u64, fmt: SizeFormat) -> String {
    match fmt {
        SizeFormat::Exact => n.to_string(),
        SizeFormat::Iec => crate::human_bytes(n),
        SizeFormat::Si => {
            const UNITS: [&str; 6] = ["kB", "MB", "GB", "TB", "PB", "EB"];
            if n < 1000 {
                return format!("{n} B");
            }
            #[allow(clippy::cast_precision_loss)]
            let mut value = n as f64 / 1000.0;
            let mut unit = 0usize;
            while (value * 10.0).round() >= 10000.0 && unit + 1 < UNITS.len() {
                value /= 1000.0;
                unit += 1;
            }
            format!("{value:.1} {}", UNITS[unit])
        }
    }
}

/// Tiempo según formato (#108). `now_ms` lo aporta el caller (fn pura —
/// testeable y estable en snapshots); negativos pre-1970 válidos.
#[must_use]
pub fn format_mtime(mtime_ms: i64, fmt: TimeFormat, now_ms: i64) -> String {
    match fmt {
        TimeFormat::Iso => iso_utc_minutes(mtime_ms),
        TimeFormat::Relative => {
            let delta_s = (now_ms.saturating_sub(mtime_ms)) / 1000;
            if delta_s < 60 {
                return norte_i18n::t("col-time-now");
            }
            let (n, key) = if delta_s < 3600 {
                (delta_s / 60, "col-time-min")
            } else if delta_s < 86_400 {
                (delta_s / 3600, "col-time-hour")
            } else if delta_s < 365 * 86_400 {
                (delta_s / 86_400, "col-time-day")
            } else {
                (delta_s / (365 * 86_400), "col-time-year")
            };
            norte_i18n::ta(key, &[("n", &n.to_string())])
        }
    }
}

/// RFC 3339 UTC al minuto, sin dependencia de calendario externa: algoritmo
/// de días civiles (Howard Hinnant) sobre el epoch. Pineado contra fechas
/// conocidas, negativos incluidos.
fn iso_utc_minutes(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (hour, min) = (sod / 3600, (sod % 3600) / 60);
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let doe = shifted.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year_base = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let month_shift = (5 * doy + 2) / 153;
    let day = doy - (153 * month_shift + 2) / 5 + 1;
    let month = if month_shift < 10 {
        month_shift + 3
    } else {
        month_shift - 9
    };
    let year = if month <= 2 { year_base + 1 } else { year_base };
    // Años negativos (mtime basura de un archivo corrupto): forma ISO 8601
    // expandida `-0005-…` — `{:04}` a secas contaría el signo dentro del
    // ancho (review m4).
    if year < 0 {
        format!(
            "-{:04}-{month:02}-{day:02}T{hour:02}:{min:02}Z",
            year.unsigned_abs()
        )
    } else {
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}Z")
    }
}

/// Modo POSIX en octal (`0644`) — para el bloque 2 (attrs); vive aquí para
/// que los formatters nazcan juntos y testeados.
#[must_use]
pub fn format_mode_octal(mode: u32) -> String {
    format!("{:04o}", mode & 0o7777)
}

/// Modo POSIX estilo `ls` (`-rw-r--r--`).
#[must_use]
pub fn format_mode_rwx(mode: u32) -> String {
    let tipo = match mode & 0o170_000 {
        0o040_000 => 'd',
        0o120_000 => 'l',
        _ => '-',
    };
    let mut out = String::with_capacity(10);
    out.push(tipo);
    // (shift, bit especial, letra con x, letra sin x): setuid/setgid/sticky
    // como `ls` de verdad (review m5) — un setuid jamás se pinta ordinario.
    for (shift, special, low, up) in [
        (6u32, 0o4000u32, 's', 'S'),
        (3, 0o2000, 's', 'S'),
        (0, 0o1000, 't', 'T'),
    ] {
        let bits = (mode >> shift) & 0o7;
        out.push(if bits & 0o4 != 0 { 'r' } else { '-' });
        out.push(if bits & 0o2 != 0 { 'w' } else { '-' });
        let x = bits & 0o1 != 0;
        out.push(match (mode & special != 0, x) {
            (true, true) => low,
            (true, false) => up,
            (false, true) => 'x',
            (false, false) => '-',
        });
    }
    out
}

#[cfg(test)]
mod settings_tests {
    use super::*;
    use crate::sort::{SortColumn, SortDir};

    #[test]
    fn column_widths_conjunto_default_a_80_celdas() {
        let s = ColumnsSettings::default();
        let w = column_widths(&s, "file", 80);
        let cols: Vec<Builtin> = w.iter().map(|(b, _)| *b).collect();
        assert_eq!(cols, vec![Builtin::Name, Builtin::Size, Builtin::Mtime]);
        // El nombre absorbe el resto: suma == disponible.
        assert_eq!(w.iter().map(|(_, x)| *x).sum::<u16>(), 80);
    }

    #[test]
    fn column_widths_estrecho_solo_nombre() {
        let s = ColumnsSettings::default();
        let w = column_widths(&s, "file", 12);
        assert_eq!(
            w.iter().map(|(b, _)| *b).collect::<Vec<_>>(),
            vec![Builtin::Name]
        );
    }

    #[test]
    fn sort_column_mapea_builtins_ordenables() {
        use crate::sort::SortColumn;
        assert_eq!(sort_column(Builtin::Name), Some(SortColumn::Name));
        assert_eq!(sort_column(Builtin::Size), Some(SortColumn::Size));
        assert_eq!(sort_column(Builtin::Mtime), Some(SortColumn::Mtime));
        assert_eq!(sort_column(Builtin::Kind), None);
    }

    #[test]
    fn resolve_parsea_diagnostica_y_resuelve_por_scheme() {
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec![
                "size".into(),
                "rota!!".into(),
                "attr:posix.mode".into(),
            ]),
            sort: Some(norte_config::SortChoice {
                column: norte_config::SortColumnKey::Mtime,
                descending: true,
                dirs_first: true,
            }),
            schemes: [(
                "sftp".to_owned(),
                norte_config::SchemeColumns {
                    columns: Some(vec!["name".into(), "kind".into()]),
                    sort: None,
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let st = ColumnsSettings::resolve(&cfg);
        assert_eq!(
            st.invalid,
            vec!["rota!!".to_owned()],
            "diagnóstico, no drop mudo"
        );
        assert_eq!(st.unrenderable, vec!["attr:posix.mode".to_owned()]);

        // Default: size + (attr saltado) → name ANTEPUESTO (jamás sin nombre).
        let items = st.layout_items_for("file");
        let cols: Vec<Builtin> = items.iter().map(|(b, _)| *b).collect();
        assert_eq!(cols, vec![Builtin::Name, Builtin::Size]);

        // Scheme: reemplaza la lista entera.
        let items = st.layout_items_for("sftp");
        let cols: Vec<Builtin> = items.iter().map(|(b, _)| *b).collect();
        assert_eq!(cols, vec![Builtin::Name, Builtin::Kind]);

        // Sort: global mtime/desc; sftp hereda el global (sin override).
        let s = st.sort_for("file");
        assert_eq!((s.column, s.dir), (SortColumn::Mtime, SortDir::Desc));
        let s = st.sort_for("sftp");
        assert_eq!((s.column, s.dir), (SortColumn::Mtime, SortDir::Desc));
    }

    #[test]
    fn layout_items_normaliza_name_al_frente() {
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec!["size".into(), "name".into()]),
            ..Default::default()
        };
        let s = ColumnsSettings::resolve(&cfg);
        let items = s.layout_items_for("file");
        assert_eq!(items[0].0, Builtin::Name);
        assert_eq!(items[1].0, Builtin::Size);
    }

    #[test]
    fn sin_config_todo_es_default() {
        let st = ColumnsSettings::resolve(&norte_config::ColumnsConfig::default());
        assert_eq!(st.sort_for("file"), crate::sort::SortSpec::default());
        let items = st.layout_items_for("file");
        assert_eq!(items.len(), 3, "name+size+mtime");
        assert!(st.invalid.is_empty() && st.unrenderable.is_empty());
    }
}

#[cfg(test)]
mod model_tests {
    use super::*;
    use std::str::FromStr as _;

    #[test]
    fn column_id_round_trip_y_errores() {
        for s in [
            "name",
            "size",
            "mtime",
            "kind",
            "attr:posix.mode",
            "plugin:git-status/branch",
        ] {
            let id = ColumnId::from_str(s).expect(s);
            assert_eq!(id.to_string(), s, "round-trip");
        }
        for bad in [
            "",
            "sise",
            "attr:",
            "plugin:",
            "plugin:solo",
            "plugin:/col",
            "plugin:p/",
        ] {
            assert!(ColumnId::from_str(bad).is_err(), "{bad:?} debe fallar");
        }
    }

    fn it(policy: WidthPolicy, measured: u16, is_name: bool) -> LayoutItem {
        LayoutItem {
            policy,
            measured,
            is_name,
        }
    }

    #[test]
    fn layout_reparte_y_descarta_por_la_derecha() {
        // name flex + size fija 9 + mtime fija 12, en 80 celdas.
        let items = [
            it(WidthPolicy::Flex { min: 10, weight: 1 }, 0, true),
            it(WidthPolicy::Fixed(9), 0, false),
            it(WidthPolicy::Fixed(12), 0, false),
        ];
        let w = layout(80, &items);
        assert_eq!(w[1], Some(9));
        assert_eq!(w[2], Some(12));
        assert_eq!(w[0], Some(80 - 9 - 12), "el nombre absorbe el sobrante");

        // En 25 celdas no caben las tres: cae la de la DERECHA (peso 0).
        let w = layout(25, &items);
        assert_eq!(w[2], None, "la más a la derecha de menor peso cae");
        assert_eq!(w[1], Some(9));
        assert_eq!(w[0], Some(16));

        // En 12 celdas solo vive el nombre, con todo.
        let w = layout(12, &items);
        assert_eq!(w, vec![Some(12), None, None]);
    }

    /// Barrido: suma ≤ available, nombre jamás descartado, anchos ≥ 1.
    #[test]
    fn layout_invariantes_en_barrido() {
        let policies = [
            WidthPolicy::Fixed(7),
            WidthPolicy::Auto,
            WidthPolicy::Flex { min: 4, weight: 2 },
        ];
        for avail in [0u16, 1, 5, 10, 20, 40, 79, 80, 200] {
            for p1 in policies {
                for p2 in policies {
                    let items = [
                        it(WidthPolicy::Flex { min: 10, weight: 1 }, 0, true),
                        it(p1, 15, false),
                        it(p2, 3, false),
                    ];
                    let w = layout(avail, &items);
                    assert!(w[0].is_some(), "nombre vivo: avail={avail} {p1:?} {p2:?}");
                    let suma: u32 = w.iter().flatten().map(|x| u32::from(*x)).sum();
                    // La única excepción documentada: el suelo del nombre
                    // puede exceder un available minúsculo (regla 3).
                    if avail >= NAME_MIN {
                        assert!(suma <= u32::from(avail.max(1)), "suma {suma} > {avail}");
                    }
                    assert!(w.iter().flatten().all(|x| *x >= 1));
                }
            }
        }
    }

    #[test]
    fn format_size_fronteras() {
        assert_eq!(format_size(0, SizeFormat::Exact), "0");
        assert_eq!(
            format_size(u64::MAX, SizeFormat::Exact),
            u64::MAX.to_string()
        );
        assert_eq!(format_size(1536, SizeFormat::Iec), "1.5 KiB");
        assert_eq!(format_size(999, SizeFormat::Si), "999 B");
        assert_eq!(format_size(1000, SizeFormat::Si), "1.0 kB");
        assert_eq!(
            format_size(999_950, SizeFormat::Si),
            "1.0 MB",
            "promoción redondeada"
        );
        assert_eq!(format_size(u64::MAX, SizeFormat::Si), "18.4 EB");
    }

    #[test]
    fn iso_utc_fechas_conocidas() {
        assert_eq!(iso_utc_minutes(0), "1970-01-01T00:00Z");
        assert_eq!(iso_utc_minutes(1_720_000_000_000), "2024-07-03T09:46Z");
        // Pre-1970 (negativo): 1969-12-31 23:59.
        assert_eq!(iso_utc_minutes(-60_000), "1969-12-31T23:59Z");
        // Bisiesto.
        assert_eq!(iso_utc_minutes(951_782_400_000), "2000-02-29T00:00Z");
    }

    #[test]
    fn modos_posix() {
        assert_eq!(format_mode_octal(0o100_644), "0644");
        assert_eq!(format_mode_rwx(0o100_644), "-rw-r--r--");
        assert_eq!(format_mode_rwx(0o040_755), "drwxr-xr-x");
        assert_eq!(format_mode_rwx(0o120_777), "lrwxrwxrwx");
        // Review m5: setuid/setgid/sticky como ls — jamás ordinarios.
        assert_eq!(format_mode_rwx(0o104_755), "-rwsr-xr-x");
        assert_eq!(format_mode_rwx(0o102_745), "-rwxr-Sr-x");
        assert_eq!(format_mode_rwx(0o041_775), "drwxrwxr-t");
        assert_eq!(format_mode_rwx(0o041_774), "drwxrwxr-T");
    }

    /// Review m3: available=0 → el nombre recibe 1 (impintable a 0), la
    /// excepción documentada de la regla 3/4. Y m1: pesos enormes no
    /// desbordan (u64).
    #[test]
    fn layout_bordes_cero_y_pesos_enormes() {
        let items = [
            it(WidthPolicy::Flex { min: 10, weight: 1 }, 0, true),
            it(WidthPolicy::Fixed(9), 0, false),
        ];
        assert_eq!(layout(0, &items), vec![Some(1), None]);

        let gordos = [
            it(
                WidthPolicy::Flex {
                    min: 10,
                    weight: u16::MAX,
                },
                0,
                true,
            ),
            it(
                WidthPolicy::Flex {
                    min: 4,
                    weight: u16::MAX,
                },
                0,
                false,
            ),
            it(
                WidthPolicy::Flex {
                    min: 4,
                    weight: u16::MAX,
                },
                0,
                false,
            ),
        ];
        let w = layout(u16::MAX, &gordos);
        let suma: u32 = w.iter().flatten().map(|x| u32::from(*x)).sum();
        assert!(
            u16::try_from(suma).is_ok(),
            "sin overflow del reparto: {w:?}"
        );
    }

    /// Review m4: año negativo en forma ISO expandida, ancho 4 + signo.
    #[test]
    fn iso_utc_anio_negativo() {
        // ~ -63_113_904_000_000 ms ≈ año -31 (aprox); pinea el FORMATO.
        let s = iso_utc_minutes(-63_200_000_000_000);
        assert!(s.starts_with('-'), "{s}");
        let year_part = &s[1..5];
        assert!(year_part.chars().all(|c| c.is_ascii_digit()), "{s}");
    }

    /// `Relative` por Fluent, `now` inyectado: puro y estable.
    #[test]
    fn format_mtime_relative_e_iso() {
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let now = 1_720_000_000_000i64;
        assert_eq!(format_mtime(now - 30_000, TimeFormat::Relative, now), "now");
        assert_eq!(
            format_mtime(now - 5 * 60_000, TimeFormat::Relative, now),
            "5m ago"
        );
        assert_eq!(
            format_mtime(now - 3 * 3_600_000, TimeFormat::Relative, now),
            "3h ago"
        );
        // Futuro (reloj torcido): saturación a «now», jamás un panic.
        assert_eq!(format_mtime(now + 10_000, TimeFormat::Relative, now), "now");
        assert_eq!(format_mtime(now, TimeFormat::Iso, now), "2024-07-03T09:46Z");
    }
}

/// El set de columnas POR DEFECTO (#108 L4): `name`, `size`, `mtime` con
/// los formatos por hint del spec (size → iec/derecha, mtime → relative).
/// El bloque 4 (config `[ui.columns]`) lo sustituirá por el del usuario;
/// hasta entonces ambos frontends pintan esto.
#[must_use]
pub fn default_layout_items() -> Vec<(Builtin, LayoutItem)> {
    vec![
        (
            Builtin::Name,
            LayoutItem {
                policy: WidthPolicy::Flex { min: 10, weight: 1 },
                measured: 0,
                is_name: true,
            },
        ),
        // Los anchos de las columnas no-nombre INCLUYEN su separador (1
        // celda a la izquierda): el layout presupuesta el ancho TOTAL de la
        // fila — sin esto, la última columna desbordaba el pane y el
        // terminal la recortaba. 11 = «1023.9 GiB» (10) + separador;
        // 10 = «hace 364d» (9) + separador.
        (
            Builtin::Size,
            LayoutItem {
                policy: WidthPolicy::Fixed(11),
                measured: 0,
                is_name: false,
            },
        ),
        (
            Builtin::Mtime,
            LayoutItem {
                policy: WidthPolicy::Fixed(10),
                measured: 0,
                is_name: false,
            },
        ),
    ]
}

/// Tope de una cabecera custom (#108 7b), en caracteres TRAS enmascarar.
pub const HEADER_MAX_CHARS: usize = 24;

/// Estilo RESUELTO de una columna (#108 7b): lo que el spec fija más los
/// defaults del builtin. El `header` llega YA saneado y capado a
/// [`HEADER_MAX_CHARS`] — el único choke point es
/// [`ColumnsSettings::resolve`], nunca el render (que va por frame).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnStyle {
    /// Formato de tamaño (solo lo lee `Size`).
    pub size_format: SizeFormat,
    /// Formato de tiempo (solo lo lee `Mtime`).
    pub time_format: TimeFormat,
    /// Alineación efectiva.
    pub align: Align,
    /// Cabecera propia (saneada, ≤ [`HEADER_MAX_CHARS`]); `None` = la
    /// etiqueta Fluent de siempre.
    pub header: Option<String>,
}

impl ColumnStyle {
    /// Los defaults del builtin sin spec: iec/relative, nombre a la
    /// izquierda y el resto a la derecha — la convención que ya pintaban
    /// ambos frontends.
    #[must_use]
    pub fn default_for(b: Builtin) -> Self {
        Self {
            size_format: SizeFormat::Iec,
            time_format: TimeFormat::Relative,
            align: if b == Builtin::Name {
                Align::Left
            } else {
                Align::Right
            },
            header: None,
        }
    }
}

/// ¿Casa `fmt` (vocabulario global YA validado en config) con la columna?
/// `Name`/`Kind` no admiten formato alguno; los de `mode`/attrs llegan con
/// el bloque 2.
fn format_fits(b: Builtin, fmt: &str) -> bool {
    match b {
        Builtin::Size => matches!(fmt, "exact" | "iec" | "si"),
        Builtin::Mtime => matches!(fmt, "relative" | "iso"),
        Builtin::Name | Builtin::Kind => false,
    }
}

/// Config de columnas RESUELTA (#108 bloque 4): ids parseados, sort
/// mapeado, overrides por scheme. Los ids que no parsean van a
/// [`ColumnsSettings::invalid`] — se saltan al pintar y los reporta
/// `norte doctor` (jamás un drop silencioso ni un error de arranque).
/// Los `attr:`/`plugin:` parsean pero aún no tienen renderer (bloques
/// 2/6/7): se listan en [`ColumnsSettings::unrenderable`].
#[derive(Debug, Clone, Default)]
pub struct ColumnsSettings {
    default_set: Option<Vec<ColumnId>>,
    default_sort: crate::sort::SortSpec,
    schemes:
        std::collections::BTreeMap<String, (Option<Vec<ColumnId>>, Option<crate::sort::SortSpec>)>,
    /// La lista `default` CRUDA tal cual vino de config (#108 7a): el picker
    /// preserva y re-persiste los ids que no parsean o no tienen renderer —
    /// paralela a `default_set`, [`Self::apply_picked`] las mantiene en paso.
    raw_default: Option<Vec<String>>,
    /// Listas crudas por scheme; solo hay entrada si el scheme configuró
    /// `columns` (un override solo-sort no lista aquí). Paralela a `schemes`.
    raw_schemes: std::collections::BTreeMap<String, Vec<String>>,
    /// Specs globales retenidos al resolver (#108 7b), YA saneados: header
    /// enmascarado y capado a [`HEADER_MAX_CHARS`], formatos que no casan
    /// con su columna retirados (y diagnosticados en [`Self::bad_specs`]).
    /// Único choke point — [`Self::style_for`] solo pliega campos.
    specs_global: std::collections::BTreeMap<String, norte_config::ColumnSpec>,
    /// Specs por scheme, mismos saneos; al plegar GANAN sobre los globales
    /// campo a campo.
    specs_schemes: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, norte_config::ColumnSpec>,
    >,
    /// Ids configurados que NO parsean (diagnóstico para doctor).
    pub invalid: Vec<String>,
    /// Ids válidos sin renderer todavía (`attr:`/`plugin:`).
    pub unrenderable: Vec<String>,
    /// Specs `[[ui.columns.spec]]` con id imposible o con un formato que no
    /// casa con su columna (#108 7b): se aplica el default y doctor lo
    /// reporta (`columns-bad-spec`) — jamás un drop mudo ni un fallo de
    /// arranque.
    pub bad_specs: Vec<String>,
}

impl ColumnsSettings {
    /// Resuelve la config cruda (#108). Nunca falla: lo inválido se
    /// acumula como diagnóstico.
    #[must_use]
    pub fn resolve(cfg: &norte_config::ColumnsConfig) -> Self {
        let mut out = Self {
            default_sort: map_sort(cfg.sort.as_ref()),
            ..Self::default()
        };
        out.default_set = cfg.default_columns.as_ref().map(|ids| parse_ids(ids));
        out.raw_default.clone_from(&cfg.default_columns);
        for (scheme, sc) in &cfg.schemes {
            let cols = sc.columns.as_ref().map(|ids| parse_ids(ids));
            let sort = sc.sort.as_ref().map(|s| map_sort(Some(s)));
            out.schemes.insert(scheme.clone(), (cols, sort));
            if let Some(raw) = &sc.columns {
                out.raw_schemes.insert(scheme.clone(), raw.clone());
            }
        }
        if let Some(ids) = &cfg.default_columns {
            out.collect_diagnostics(ids);
        }
        for sc in cfg.schemes.values() {
            if let Some(ids) = &sc.columns {
                out.collect_diagnostics(ids);
            }
        }
        // #108 7b: retén los specs SANEADOS aquí (choke point único) —
        // `style_for` va por frame y no debe volver a sanear ni diagnosticar.
        out.specs_global = out.sanitize_specs(&cfg.specs);
        for (scheme, sc) in &cfg.schemes {
            if !sc.specs.is_empty() {
                let sane = out.sanitize_specs(&sc.specs);
                out.specs_schemes.insert(scheme.clone(), sane);
            }
        }
        out
    }

    /// Saneo de un mapa de specs al resolver (#108 7b): id imposible →
    /// [`Self::bad_specs`] y fuera; formato que no casa con su builtin →
    /// [`Self::bad_specs`] y campo retirado (se aplicará el default);
    /// header enmascarado ([`sanitize_header`]) y capado a
    /// [`HEADER_MAX_CHARS`] (vacío tras enmascarar = `None`, cae a Fluent).
    /// Los ids `attr:`/`plugin:` se retienen tal cual: sus formatos llegan
    /// con sus renderers (bloques 2/6/7) y hoy nadie los pinta.
    fn sanitize_specs(
        &mut self,
        specs: &std::collections::BTreeMap<String, norte_config::ColumnSpec>,
    ) -> std::collections::BTreeMap<String, norte_config::ColumnSpec> {
        let mut out = std::collections::BTreeMap::new();
        for (raw, spec) in specs {
            let mut spec = spec.clone();
            match raw.parse::<ColumnId>() {
                Err(_) => {
                    self.push_bad_spec(raw);
                    continue; // id imposible: el spec entero es diagnóstico
                }
                Ok(ColumnId::Builtin(b)) => {
                    if let Some(fmt) = spec.format.as_deref()
                        && !format_fits(b, fmt)
                    {
                        self.push_bad_spec(raw);
                        spec.format = None; // default, jamás el spec roto
                    }
                }
                Ok(ColumnId::Attr(_) | ColumnId::Plugin { .. }) => {}
            }
            if let Some(h) = spec.header.as_deref() {
                let sane: String = sanitize_header(h).chars().take(HEADER_MAX_CHARS).collect();
                spec.header = (!sane.is_empty()).then_some(sane);
            }
            out.insert(raw.clone(), spec);
        }
        out
    }

    /// Añade un diagnóstico de spec, deduplicado por id (una entrada por id
    /// ofensor, venga del mapa global o de un scheme).
    fn push_bad_spec(&mut self, raw: &str) {
        if !self.bad_specs.iter().any(|b| b == raw) {
            self.bad_specs.push(raw.to_owned());
        }
    }

    /// El estilo efectivo de un builtin en `scheme` (#108 7b): defaults del
    /// builtin ← spec global ← spec del scheme, campo a campo (`Some`
    /// gana). Los formatos ya vienen validados y con encaje comprobado en
    /// [`Self::resolve`]; aquí el fallback conserva el default — jamás un
    /// panic.
    #[must_use]
    pub fn style_for(&self, scheme: &str, builtin: Builtin) -> ColumnStyle {
        let key = ColumnId::Builtin(builtin).to_string();
        let mut style = ColumnStyle::default_for(builtin);
        let global = self.specs_global.get(&key);
        let scoped = self.specs_schemes.get(scheme).and_then(|m| m.get(&key));
        for spec in [global, scoped].into_iter().flatten() {
            if let Some(a) = spec.align {
                style.align = match a {
                    norte_config::AlignChoice::Left => Align::Left,
                    norte_config::AlignChoice::Right => Align::Right,
                };
            }
            if let Some(fmt) = spec.format.as_deref() {
                match builtin {
                    Builtin::Size => {
                        style.size_format = match fmt {
                            "exact" => SizeFormat::Exact,
                            "si" => SizeFormat::Si,
                            _ => SizeFormat::Iec,
                        };
                    }
                    Builtin::Mtime => {
                        style.time_format = match fmt {
                            "iso" => TimeFormat::Iso,
                            _ => TimeFormat::Relative,
                        };
                    }
                    // Sin formato posible: resolve ya lo retiró y diagnosticó.
                    Builtin::Name | Builtin::Kind => {}
                }
            }
            if spec.header.is_some() {
                style.header.clone_from(&spec.header);
            }
        }
        style
    }

    fn collect_diagnostics(&mut self, ids: &[String]) {
        for raw in ids {
            match raw.parse::<ColumnId>() {
                Err(_) => {
                    if !self.invalid.contains(raw) {
                        self.invalid.push(raw.clone());
                    }
                }
                Ok(ColumnId::Attr(_) | ColumnId::Plugin { .. }) => {
                    if !self.unrenderable.contains(raw) {
                        self.unrenderable.push(raw.clone());
                    }
                }
                Ok(ColumnId::Builtin(_)) => {}
            }
        }
    }

    /// El orden para un pane en `scheme` (#108): el del scheme, o el
    /// global, o el histórico.
    #[must_use]
    pub fn sort_for(&self, scheme: &str) -> crate::sort::SortSpec {
        self.schemes
            .get(scheme)
            .and_then(|(_, s)| *s)
            .unwrap_or(self.default_sort)
    }

    /// Los items de layout BUILTIN para un pane en `scheme`, en orden de
    /// pintado. Los `attr:`/`plugin:` configurados se SALTAN (sin renderer
    /// aún — doctor los nombra); el nombre jamás desaparece NI deja de ir
    /// primero — la TUI presupuesta la primera columna como el nombre
    /// (#108 7a: un `name` a mitad de lista se normaliza al frente).
    ///
    /// #108 7b: el `width` de un `[[ui.columns.spec]]` (global ← scheme)
    /// SUSTITUYE la política del item — también sobre el set por defecto.
    /// Un width sobre `name` se aplica pero conserva `is_name: true`: las
    /// reglas de suelo del nombre de [`layout`] (regla 3, [`NAME_MIN`],
    /// jamás descartado) siguen ganando — un `fixed = 1` en el nombre no lo
    /// vuelve impintable.
    #[must_use]
    pub fn layout_items_for(&self, scheme: &str) -> Vec<(Builtin, LayoutItem)> {
        let ids = self
            .schemes
            .get(scheme)
            .and_then(|(c, _)| c.as_ref())
            .or(self.default_set.as_ref());
        let mut out = match ids {
            None => default_layout_items(),
            Some(ids) => {
                let mut out: Vec<(Builtin, LayoutItem)> = Vec::new();
                for id in ids {
                    if let ColumnId::Builtin(b) = id {
                        if out.iter().any(|(x, _)| x == b) {
                            continue;
                        }
                        out.push((*b, builtin_layout_item(*b)));
                    }
                }
                match out.iter().position(|(b, _)| *b == Builtin::Name) {
                    Some(pos) if pos > 0 => {
                        let name = out.remove(pos);
                        out.insert(0, name);
                    }
                    Some(_) => {}
                    None => out.insert(0, (Builtin::Name, builtin_layout_item(Builtin::Name))),
                }
                out
            }
        };
        self.apply_width_overrides(scheme, &mut out);
        out
    }

    /// Pliega el `width` de los specs (global ← scheme, `Some` gana) sobre
    /// la política de cada item (#108 7b). `is_name` no se toca: los suelos
    /// del nombre en [`layout`] mandan.
    fn apply_width_overrides(&self, scheme: &str, items: &mut [(Builtin, LayoutItem)]) {
        for (b, item) in items.iter_mut() {
            let key = ColumnId::Builtin(*b).to_string();
            let global = self.specs_global.get(&key).and_then(|s| s.width);
            let scoped = self
                .specs_schemes
                .get(scheme)
                .and_then(|m| m.get(&key))
                .and_then(|s| s.width);
            if let Some(w) = scoped.or(global) {
                item.policy = match w {
                    norte_config::WidthChoice::Auto => WidthPolicy::Auto,
                    norte_config::WidthChoice::Fixed(n) => WidthPolicy::Fixed(n),
                    norte_config::WidthChoice::Flex { min, weight } => {
                        WidthPolicy::Flex { min, weight }
                    }
                };
            }
        }
    }

    /// La lista de ids CONFIGURADA efectiva para `scheme` en forma Display,
    /// override del scheme > default > set built-in (#108 7a). Preserva los
    /// ids sin renderer y los que no parsean: el picker los enseña y los
    /// re-persiste ENTEROS — limpiar la config del usuario no es su trabajo
    /// (doctor los reporta).
    #[must_use]
    pub fn raw_ids_for(&self, scheme: &str) -> Vec<String> {
        if let Some(ids) = self.raw_schemes.get(scheme) {
            return ids.clone();
        }
        if let Some(ids) = &self.raw_default {
            return ids.clone();
        }
        default_layout_items()
            .iter()
            .map(|(b, _)| ColumnId::Builtin(*b).to_string())
            .collect()
    }

    /// ¿Tiene `scheme` una entrada propia en la config (columns o sort)?
    /// Decide el TARGET del picker: con entrada, el save escribe el scheme;
    /// sin ella, el default (#108 7a — una regla, dicha en el título).
    #[must_use]
    pub fn has_scheme_entry(&self, scheme: &str) -> bool {
        self.schemes.contains_key(scheme)
    }

    /// Aplica el resultado del picker EN MEMORIA (#108 7a): mismas semánticas
    /// que el write-back a disco (`persist_columns`) para que la sesión y el
    /// fichero no diverjan mientras llega el hot-reload. Mantiene en paso las
    /// listas crudas y las parseadas.
    pub fn apply_picked(
        &mut self,
        target: Option<&str>,
        ids: &[String],
        sort: crate::sort::SortSpec,
    ) {
        // M3 revisión 7a: los diagnósticos de doctor (`invalid`/
        // `unrenderable`) NO se recalculan aquí — `collect_diagnostics` solo
        // AÑADE, jamás retira entradas rancias, así que llamarla mentiría;
        // el re-resolve del hot-reload es quien los refresca honestos.
        // #108 7b: los mapas de specs retenidos (`specs_global`/
        // `specs_schemes`) son independientes de la LISTA de columnas — un
        // spec estiliza su id «allí donde aparezca», así que elegir columnas
        // en el picker no los toca y no hay nada que mantener en paso aquí.
        let parsed = parse_ids(ids);
        if let Some(s) = target {
            self.raw_schemes.insert(s.to_owned(), ids.to_vec());
            self.schemes
                .insert(s.to_owned(), (Some(parsed), Some(sort)));
        } else {
            self.raw_default = Some(ids.to_vec());
            self.default_set = Some(parsed);
            self.default_sort = sort;
        }
    }
}

/// Item de layout por defecto de cada builtin (#108): mismos anchos que
/// [`default_layout_items`] — separador INCLUIDO en las no-nombre.
#[must_use]
pub fn builtin_layout_item(b: Builtin) -> LayoutItem {
    match b {
        Builtin::Name => LayoutItem {
            policy: WidthPolicy::Flex { min: 10, weight: 1 },
            measured: 0,
            is_name: true,
        },
        Builtin::Size => LayoutItem {
            policy: WidthPolicy::Fixed(11),
            measured: 0,
            is_name: false,
        },
        Builtin::Mtime => LayoutItem {
            policy: WidthPolicy::Fixed(10),
            measured: 0,
            is_name: false,
        },
        // «dir»/«file»/«symlink»/«other» localizados; 9 = «symlink»(7)+sep
        // con margen.
        Builtin::Kind => LayoutItem {
            policy: WidthPolicy::Fixed(9),
            measured: 0,
            is_name: false,
        },
    }
}

fn parse_ids(ids: &[String]) -> Vec<ColumnId> {
    ids.iter()
        .filter_map(|raw| raw.parse::<ColumnId>().ok())
        .collect()
}

fn map_sort(s: Option<&norte_config::SortChoice>) -> crate::sort::SortSpec {
    use crate::sort::{SortColumn, SortDir, SortSpec};
    let Some(s) = s else {
        return SortSpec::default();
    };
    SortSpec {
        column: match s.column {
            norte_config::SortColumnKey::Name => SortColumn::Name,
            norte_config::SortColumnKey::Size => SortColumn::Size,
            norte_config::SortColumnKey::Mtime => SortColumn::Mtime,
        },
        dir: if s.descending {
            SortDir::Desc
        } else {
            SortDir::Asc
        },
        dirs_first: s.dirs_first,
    }
}

/// Anchos de las columnas (#108) para un ancho interior en CELDAS:
/// `(builtin, ancho)` de las columnas VIVAS de `settings` para `scheme`,
/// en orden de pintado — una columna sin sitio no aparece. Compartido
/// TUI/GUI: ambos frontends pintan el MISMO conjunto del mismo [`layout`].
#[must_use]
pub fn column_widths(
    settings: &ColumnsSettings,
    scheme: &str,
    inner_width: u16,
) -> Vec<(Builtin, u16)> {
    let set = settings.layout_items_for(scheme);
    let items: Vec<_> = set.iter().map(|(_, it)| *it).collect();
    let placed = layout(inner_width, &items);
    set.iter()
        .zip(placed)
        .filter_map(|((b, _), w)| w.map(|w| (*b, w)))
        .collect()
}

/// La columna de orden que corresponde a un builtin, si es ordenable.
/// `Kind` no lo es (no hay `SortColumn::Kind`): su cabecera no lleva
/// flecha ni es clicable.
#[must_use]
pub fn sort_column(b: Builtin) -> Option<crate::sort::SortColumn> {
    use crate::sort::SortColumn;
    match b {
        Builtin::Name => Some(SortColumn::Name),
        Builtin::Size => Some(SortColumn::Size),
        Builtin::Mtime => Some(SortColumn::Mtime),
        Builtin::Kind => None,
    }
}

/// Texto de la celda de una columna BUILTIN no-nombre (#108 L5) con un
/// [`ColumnStyle`] resuelto (7b): `None` = ausencia (un dir sin size, un
/// mtime desconocido) — se pinta blanco, jamás un `0` fabricado. `now_ms`
/// lo inyecta el caller (estabilidad de snapshots y pureza).
#[must_use]
pub fn styled_cell(
    entry: &norte_proto::Entry,
    col: Builtin,
    now_ms: i64,
    style: &ColumnStyle,
) -> Option<String> {
    match col {
        Builtin::Name => None, // el nombre lo pinta el frontend
        Builtin::Kind => Some(norte_i18n::t(match entry.kind {
            norte_proto::EntryKind::Dir => "col-kind-dir",
            norte_proto::EntryKind::File => "col-kind-file",
            norte_proto::EntryKind::Symlink => "col-kind-symlink",
            norte_proto::EntryKind::Other => "col-kind-other",
        })),
        Builtin::Size => entry.size.map(|n| format_size(n, style.size_format)),
        Builtin::Mtime => entry
            .mtime_ms
            .map(|ms| format_mtime(ms, style.time_format, now_ms)),
    }
}

/// [`styled_cell`] con los defaults del builtin (iec/relative) — la firma
/// histórica pre-7b, conducta idéntica (pineada por los tests existentes).
#[must_use]
pub fn builtin_cell(entry: &norte_proto::Entry, col: Builtin, now_ms: i64) -> Option<String> {
    styled_cell(entry, col, now_ms, &ColumnStyle::default_for(col))
}

#[cfg(test)]
mod style_tests {
    use super::*;

    #[test]
    fn style_for_aplica_spec_global_y_scheme_gana() {
        let mut cfg = norte_config::ColumnsConfig::default();
        cfg.specs.insert(
            "size".into(),
            norte_config::ColumnSpec {
                format: Some("si".into()),
                header: Some("Peso".into()),
                width: Some(norte_config::WidthChoice::Fixed(9)),
                ..Default::default()
            },
        );
        let mut sc = norte_config::SchemeColumns::default();
        sc.specs.insert(
            "size".into(),
            norte_config::ColumnSpec {
                format: Some("exact".into()),
                ..Default::default()
            },
        );
        cfg.schemes.insert("sftp".into(), sc);
        let s = ColumnsSettings::resolve(&cfg);
        assert_eq!(
            s.style_for("file", Builtin::Size).size_format,
            SizeFormat::Si
        );
        assert_eq!(
            s.style_for("sftp", Builtin::Size).size_format,
            SizeFormat::Exact
        );
        // header del global sobrevive en el scheme (last-wins POR CAMPO).
        assert_eq!(
            s.style_for("sftp", Builtin::Size).header.as_deref(),
            Some("Peso")
        );
        // width override llega al layout.
        let items = s.layout_items_for("file");
        let size = items
            .iter()
            .find(|(b, _)| *b == Builtin::Size)
            .expect("size");
        assert_eq!(size.1.policy, WidthPolicy::Fixed(9));
    }

    #[test]
    fn spec_formato_que_no_casa_es_diagnostico_no_aplicado() {
        let mut cfg = norte_config::ColumnsConfig::default();
        cfg.specs.insert(
            "mtime".into(),
            norte_config::ColumnSpec {
                format: Some("iec".into()), // iec en un timestamp: no casa
                ..Default::default()
            },
        );
        let s = ColumnsSettings::resolve(&cfg);
        assert_eq!(
            s.style_for("file", Builtin::Mtime).time_format,
            TimeFormat::Relative
        );
        assert!(s.bad_specs.iter().any(|b| b.contains("mtime")));
    }

    #[test]
    fn spec_id_que_no_parsea_es_diagnostico() {
        let mut cfg = norte_config::ColumnsConfig::default();
        cfg.specs.insert(
            "rota!!".into(),
            norte_config::ColumnSpec {
                header: Some("X".into()),
                ..Default::default()
            },
        );
        let s = ColumnsSettings::resolve(&cfg);
        assert!(s.bad_specs.iter().any(|b| b.contains("rota!!")));
    }

    #[test]
    fn spec_header_hostil_se_sanea_y_capa_al_resolver() {
        let mut cfg = norte_config::ColumnsConfig::default();
        cfg.specs.insert(
            "size".into(),
            norte_config::ColumnSpec {
                header: Some(format!("A\u{202E}{}", "x".repeat(60))),
                ..Default::default()
            },
        );
        let s = ColumnsSettings::resolve(&cfg);
        let h = s.style_for("file", Builtin::Size).header.expect("header");
        assert!(!h.chars().any(norte_encoding::is_terminal_hazard));
        assert!(h.chars().count() <= HEADER_MAX_CHARS);
    }

    #[test]
    fn builtin_cell_honra_el_formato() {
        let e = norte_proto::Entry {
            attrs: std::collections::BTreeMap::new(),
            path: norte_proto::VPath::parse("mem:///a.bin").unwrap(),
            kind: norte_proto::EntryKind::File,
            size: Some(2048),
            mtime_ms: Some(0),
        };
        let styled = ColumnStyle {
            size_format: SizeFormat::Exact,
            ..ColumnStyle::default_for(Builtin::Size)
        };
        assert_eq!(
            styled_cell(&e, Builtin::Size, 0, &styled).as_deref(),
            Some("2048")
        );
        // El wrapper por defecto no cambia de conducta (Iec).
        assert_eq!(
            builtin_cell(&e, Builtin::Size, 0).as_deref(),
            Some("2.0 KiB")
        );
    }

    #[test]
    fn default_for_alineaciones() {
        assert_eq!(ColumnStyle::default_for(Builtin::Name).align, Align::Left);
        assert_eq!(ColumnStyle::default_for(Builtin::Size).align, Align::Right);
        assert_eq!(ColumnStyle::default_for(Builtin::Mtime).align, Align::Right);
        assert_eq!(ColumnStyle::default_for(Builtin::Kind).align, Align::Right);
    }
}
