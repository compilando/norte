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
