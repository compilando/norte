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

/// Cap de columnas `plugin:` PEDIBLES por lista pintada (#117-follow-up) —
/// espejo del [`norte_proto::ATTRS_MAX_REQUEST`] de los attrs, pero local
/// del frontend: cada columna de plugin cuesta UNA RPC
/// `plugin.column_values` por listado, así que el cap acota trabajo, no
/// wire. Pintado == pedido; el excedente es diagnóstico
/// (`plugins_over_cap`, doctor lo nombra), jamás una columna en blanco.
///
/// Es el tope de las PINTADAS. Lo que un listado pide en total
/// ([`plugin_requests`]) suma los elementos de la barra de estado (ADR
/// 0137), hasta `STATUS_PLUGINS_MAX` más: doce llamadas como mucho.
pub const PLUGIN_COLUMNS_MAX_REQUEST: usize = 8;

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
        let mut truncated: String = masked.chars().take(COLUMN_VALUE_MAX_CHARS).collect();
        // El corte se MARCA. Sin la marca, un valor recortado y uno completo
        // se pintan idénticos, y quien lee la hoja de atributos —que existe
        // justo para ver el valor entero— no puede saber cuál está mirando.
        // Es la regla de `truncation_twins` del corpus: lo que se debe es que
        // el corte se vea, no que quepa.
        if masked.chars().nth(COLUMN_VALUE_MAX_CHARS).is_some() {
            truncated.push('…');
        }
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

/// Id Display de una columna `plugin:` (#117-follow-up, audit F5): pasa
/// por el `Display` REAL de [`ColumnId`] — jamás un `format!` ad-hoc que
/// pueda derivar del parser (un drift = celdas permanentemente en blanco
/// sin diagnóstico, la clave del side-map dejaría de casar).
#[must_use]
pub fn plugin_display_id(plugin: &str, column: &str) -> String {
    ColumnId::Plugin {
        plugin: plugin.to_owned(),
        column: column.to_owned(),
    }
    .to_string()
}

/// Las columnas de plugin que un listado de `scheme` tiene que PEDIR: las
/// que se pintan como columna más las que la barra de estado enseña
/// (`[ui] status_plugins`, ADR 0137), sin repetir, y en ese orden.
///
/// Una sola lista para los dos frontends: el elemento de estado es el valor
/// de la columna para la entrada bajo el cursor, así que tiene que llegar
/// por el MISMO viaje que las celdas —y con la misma validación de
/// consentimiento, [`validated_plugin_requests`]—. Con una lista por
/// frontend, uno pediría la columna y el otro enseñaría un elemento vacío.
#[must_use]
pub fn plugin_requests(
    settings: &ColumnsSettings,
    status: &[(String, String)],
    scheme: &str,
) -> Vec<(String, String)> {
    let mut out = settings.plugin_ids_for(scheme);
    for par in status.iter().take(norte_config::load::STATUS_PLUGINS_MAX) {
        if !out.contains(par) {
            out.push(par.clone());
        }
    }
    out
}

/// Filtra los pares (plugin, columna) CONFIGURADOS contra el catálogo vivo
/// (#117-follow-up, review MAJOR-1: única definición para ambos frontends —
/// la validación de PERTENENCIA es lo que impide que un id configurado
/// pinte la columna de un plugin que jamás la declaró): sobrevive un par
/// solo si su plugin está aprobado + habilitado Y declara ESA columna.
/// Además DEDUPLICA por id bare de columna (review MAJOR-2): el wire
/// `plugin.column_values` resuelve first-match por id bare entre plugins —
/// dos pares consentidos con la misma columna servirían los MISMOS valores
/// bajo dos cabeceras distintas (datos mal atribuidos); se conserva el
/// primero y el resto queda en blanco (ausencia visible, jamás atribución
/// falsa; desambiguación real = issue #120).
#[must_use]
pub fn validated_plugin_requests(
    requested: &[(String, String)],
    plugins: &[norte_proto::methods::PluginInfo],
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (plugin, column) in requested {
        let declared = plugins.iter().any(|p| {
            p.approved && p.enabled && p.id == *plugin && p.columns.iter().any(|c| c.id == *column)
        });
        // Dedup por la PAREJA, no por el id bare (#120). Dos plugins
        // consentidos pueden declarar `status` los dos, y desde 0.35.0 el wire
        // sabe distinguirlos: deduplicar por columna a secas tiraría en
        // silencio la segunda que el usuario configuró a propósito.
        if declared && !out.iter().any(|(p, c)| p == plugin && c == column) {
            out.push((plugin.clone(), column.clone()));
        }
    }
    out
}

#[cfg(test)]
mod validated_plugin_requests_tests {
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
            panels: Vec::new(),
            has_help: false,
            manifest_digest: None,
        }
    }

    /// Pertenencia: solo sobrevive el par cuyo plugin (aprobado+habilitado)
    /// declara ESA columna — ni columnas ajenas ni plugins sin consentir.
    #[test]
    fn filtra_por_pertenencia_y_consentimiento() {
        let plugins = vec![
            plugin("git", &["branch"], true, true),
            plugin("otro", &["status"], false, true),
            plugin("apagado", &["x"], true, false),
        ];
        let requested = vec![
            ("git".to_owned(), "branch".to_owned()),
            ("git".to_owned(), "status".to_owned()), // git NO declara status
            ("otro".to_owned(), "status".to_owned()), // sin aprobar
            ("apagado".to_owned(), "x".to_owned()),  // deshabilitado
            ("fantasma".to_owned(), "y".to_owned()), // no existe
        ];
        assert_eq!(
            validated_plugin_requests(&requested, &plugins),
            vec![("git".to_owned(), "branch".to_owned())]
        );
    }

    /// Dos plugins consentidos con el MISMO id bare de columna: AMBOS se
    /// sirven (#120 cerrada).
    ///
    /// Hasta 0.35.0 el wire llevaba solo el id bare y el host resolvía a la
    /// primera que casara, así que servir los dos habría pintado los valores
    /// de uno bajo la cabecera del otro; se conservaba el primero y el segundo
    /// quedaba en blanco — ausencia visible antes que atribución falsa. Ahora
    /// la petición nombra al plugin, el host sirve ESE o ninguno, y quedarse
    /// con uno solo tiraría en silencio una columna que el usuario configuró.
    #[test]
    fn colision_de_id_bare_sirve_a_los_dos_plugins() {
        let plugins = vec![
            plugin("a", &["branch"], true, true),
            plugin("b", &["branch"], true, true),
        ];
        let requested = vec![
            ("a".to_owned(), "branch".to_owned()),
            ("b".to_owned(), "branch".to_owned()),
        ];
        assert_eq!(
            validated_plugin_requests(&requested, &plugins),
            requested,
            "cada par va con su plugin: el wire ya sabe distinguirlos (#120)"
        );
    }

    /// Lo que sigue deduplicándose es la pareja REPETIDA: configurar dos veces
    /// `plugin:a/branch` es una columna, no dos peticiones al mismo guest.
    #[test]
    fn la_pareja_repetida_se_deduplica() {
        let plugins = vec![plugin("a", &["branch"], true, true)];
        let requested = vec![
            ("a".to_owned(), "branch".to_owned()),
            ("a".to_owned(), "branch".to_owned()),
        ];
        assert_eq!(
            validated_plugin_requests(&requested, &plugins),
            vec![("a".to_owned(), "branch".to_owned())]
        );
    }
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

    /// Trunca tras enmascarar, y MARCA el corte.
    ///
    /// La marca no es cosmética: la hoja de atributos existe para ver el
    /// valor entero, y sin ella un valor recortado y uno completo se pintan
    /// idénticos.
    #[test]
    fn sanitize_cell_trunca_tras_enmascarar_y_marca_el_corte() {
        let largo = "a".repeat(1000);
        let out = sanitize_cell(Some(&largo)).unwrap();
        assert!(out.ends_with('…'), "el corte se ve: {out}");
        assert_eq!(
            out.chars().count(),
            COLUMN_VALUE_MAX_CHARS + 1,
            "los caracteres del tope más la marca"
        );

        // Uno que cabe JUSTO no se marca: no hay nada cortado que decir.
        let justo = "a".repeat(COLUMN_VALUE_MAX_CHARS);
        let out = sanitize_cell(Some(&justo)).unwrap();
        assert_eq!(out, justo);
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
    /// Corto, binario y sin espacio (`80K`, `1.3M`, `512B`): nunca más de
    /// cinco celdas. No es un formato de configuración: es el que pone
    /// [`fitted_columns`] cuando el nombre necesita el sitio.
    Short,
}

/// Formato de tiempo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeFormat {
    /// «hace 2h», por Fluent (`col-time-*`). Necesita el `now` del caller.
    Relative,
    /// RFC 3339 UTC al minuto (`2026-07-31T09:41Z`) — libre de locale.
    Iso,
    /// Hora LOCAL con la precisión que la distancia pide (spec 2026-09-10):
    /// `14:02` si es de hoy, `09-10 14:02` si es de este año, `2025-09-10`
    /// si es anterior. Numérico a propósito: cabe en 11 celdas en cualquier
    /// idioma y se compara a ojo. Necesita el `now` del caller Y la zona
    /// ([`format_mtime_tz`] para fijarla; [`format_mtime_in`] usa la del
    /// sistema).
    Smart,
    /// Lo de [`Self::Smart`] en cinco celdas: `14:02` si es de hoy, `09-10`
    /// si es de este año, `2025` si es anterior. Como [`SizeFormat::Short`],
    /// lo pone [`fitted_columns`], no la configuración.
    Short,
}

/// Formato de un word de modo POSIX (#117).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeFormat {
    /// `-rw-r--r--` (tipo + rwx, setuid/sticky incluidos).
    Rwx,
    /// Octal (`644`).
    Octal,
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
            #[expect(clippy::cast_precision_loss, reason = "magnitudes lejos de 2^53")]
            let mut value = n as f64 / 1000.0;
            let mut unit = 0usize;
            while (value * 10.0).round() >= 10000.0 && unit + 1 < UNITS.len() {
                value /= 1000.0;
                unit += 1;
            }
            format!("{value:.1} {}", UNITS[unit])
        }
        SizeFormat::Short => short_size(n),
    }
}

/// [`SizeFormat::Short`]: `512B`, `9.5K`, `80K`, `1.3M`. Una cifra decimal
/// solo por debajo de 10, que es donde cambia la lectura; el redondeo que
/// llega a 1000 sube de unidad, así que jamás pasa de cinco celdas.
fn short_size(n: u64) -> String {
    const UNITS: [char; 6] = ['K', 'M', 'G', 'T', 'P', 'E'];
    if n < 1024 {
        return format!("{n}B");
    }
    #[expect(clippy::cast_precision_loss, reason = "magnitudes lejos de 2^53")]
    let mut value = n as f64 / 1024.0;
    let mut unit = 0usize;
    while value.round() >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if (value * 10.0).round() < 100.0 {
        format!("{value:.1}{}", UNITS[unit])
    } else {
        format!("{value:.0}{}", UNITS[unit])
    }
}

/// Tiempo según formato (#108). `now_ms` lo aporta el caller (fn pura —
/// testeable y estable en snapshots); negativos pre-1970 válidos.
///
/// En la lengua AMBIENTE. Envoltorio de [`format_mtime_in`] para quien no
/// tiene un `lang` que pasar; **una ventana siempre lo tiene**, y llamar a
/// ésta desde ella pintaba cada celda de fecha del listado en el idioma del
/// PROCESO, bajo una cabecera en el del host.
#[must_use]
pub fn format_mtime(mtime_ms: i64, fmt: TimeFormat, now_ms: i64) -> String {
    format_mtime_in(mtime_ms, fmt, now_ms, norte_i18n::active())
}

/// [`format_mtime`] en un idioma DADO.
///
/// La rama relativa es la que traduce, y no se puede esquivar con
/// configuración: la ventana ignora `time-format`, así que está siempre viva.
#[must_use]
pub fn format_mtime_in(
    mtime_ms: i64,
    fmt: TimeFormat,
    now_ms: i64,
    lang: norte_i18n::Lang,
) -> String {
    format_mtime_tz(mtime_ms, fmt, now_ms, lang, &jiff::tz::TimeZone::system())
}

/// [`format_mtime_in`] con la ZONA dada — la que [`TimeFormat::Smart`]
/// necesita para saber qué es «hoy». Los frontends pasan la del sistema;
/// los tests, una fija, para que una foto no dependa de la máquina.
#[must_use]
pub fn format_mtime_tz(
    mtime_ms: i64,
    fmt: TimeFormat,
    now_ms: i64,
    lang: norte_i18n::Lang,
    tz: &jiff::tz::TimeZone,
) -> String {
    match fmt {
        TimeFormat::Iso => iso_utc_minutes(mtime_ms),
        TimeFormat::Smart => smart_local(mtime_ms, now_ms, tz),
        TimeFormat::Short => short_local(mtime_ms, now_ms, tz),
        TimeFormat::Relative => {
            let delta_s = (now_ms.saturating_sub(mtime_ms)) / 1000;
            if delta_s < 60 {
                return norte_i18n::t_in(lang, "col-time-now");
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
            norte_i18n::ta_in(lang, key, &[("n", &n.to_string())])
        }
    }
}

/// [`TimeFormat::Smart`]: hora local con la precisión que la distancia pide.
/// Un instante fuera del rango de `jiff` (±9999 años: mtime basura de un
/// provider hostil) cae al ISO UTC, que sabe pintar cualquier `i64` —
/// jamás un panic ni una celda vacía.
fn smart_local(mtime_ms: i64, now_ms: i64, tz: &jiff::tz::TimeZone) -> String {
    let (Ok(ts), Ok(now)) = (
        jiff::Timestamp::from_millisecond(mtime_ms),
        jiff::Timestamp::from_millisecond(now_ms),
    ) else {
        return iso_utc_minutes(mtime_ms);
    };
    let z = ts.to_zoned(tz.clone());
    let n = now.to_zoned(tz.clone());
    if z.date() == n.date() {
        format!("{:02}:{:02}", z.hour(), z.minute())
    } else if z.year() == n.year() {
        format!(
            "{:02}-{:02} {:02}:{:02}",
            z.month(),
            z.day(),
            z.hour(),
            z.minute()
        )
    } else {
        format!("{:04}-{:02}-{:02}", z.year(), z.month(), z.day())
    }
}

/// [`TimeFormat::Short`]: la parte de [`smart_local`] que distingue a esa
/// distancia, y nada más. Fuera del rango de `jiff`, el año del ISO.
fn short_local(mtime_ms: i64, now_ms: i64, tz: &jiff::tz::TimeZone) -> String {
    let (Ok(ts), Ok(now)) = (
        jiff::Timestamp::from_millisecond(mtime_ms),
        jiff::Timestamp::from_millisecond(now_ms),
    ) else {
        // El AÑO del ISO, con su signo: `iso_utc_minutes` pone el signo
        // fuera del ancho, y cortar a cuatro caracteres dejaba `-000` para
        // cualquier año negativo.
        let iso = iso_utc_minutes(mtime_ms);
        let fin = iso
            .char_indices()
            .skip(1)
            .find(|(_, c)| *c == '-')
            .map_or(iso.len(), |(i, _)| i);
        return iso[..fin].to_owned();
    };
    let z = ts.to_zoned(tz.clone());
    let n = now.to_zoned(tz.clone());
    if z.date() == n.date() {
        format!("{:02}:{:02}", z.hour(), z.minute())
    } else if z.year() == n.year() {
        format!("{:02}-{:02}", z.month(), z.day())
    } else {
        format!("{:04}", z.year())
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
///
/// La primera letra es la CLASE del nodo, con las siete que define `S_IFMT`
/// y una octava que no la define: `?`.
///
/// El `?` es el hallazgo de la auditoría de codificación, y es el que
/// importa. Un modo cuyos bits de tipo son cero —lo que manda un servidor
/// SFTP que solo reporta los permisos, y lo que emite `MemProvider`— no es
/// un fichero regular: es un modo que no dice de qué clase es. Pintarlo `-`
/// enseñaba un directorio como fichero en la misma fila en la que el icono y
/// la `/` decían que era un directorio, y de dos superficies que se
/// contradicen la que mentía era ésta. `?` es una pregunta sobre la que el
/// lector puede actuar; `-` era una respuesta equivocada.
///
/// ```
/// use norte_frontend::columns::format_mode_rwx;
/// assert_eq!(format_mode_rwx(0o100_644), "-rw-r--r--");
/// assert_eq!(format_mode_rwx(0o040_755), "drwxr-xr-x");
/// assert_eq!(format_mode_rwx(0o010_644), "prw-r--r--", "fifo");
/// assert_eq!(format_mode_rwx(0o020_666), "crw-rw-rw-", "dispositivo de caracteres");
/// assert_eq!(format_mode_rwx(0o060_660), "brw-rw----", "dispositivo de bloques");
/// assert_eq!(format_mode_rwx(0o140_755), "srwxr-xr-x", "socket");
/// // Sin bits de clase: NO es un fichero regular, es un modo que no lo dice.
/// assert_eq!(format_mode_rwx(0o644), "?rw-r--r--");
/// // Y siempre diez celdas, sea cual sea la entrada.
/// assert_eq!(format_mode_rwx(u32::MAX).chars().count(), 10);
/// ```
#[must_use]
pub fn format_mode_rwx(mode: u32) -> String {
    let tipo = match mode & 0o170_000 {
        0o140_000 => 's',
        0o120_000 => 'l',
        0o100_000 => '-',
        0o060_000 => 'b',
        0o040_000 => 'd',
        0o020_000 => 'c',
        0o010_000 => 'p',
        _ => '?',
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
        // Sin catálogo no hay columna de permisos: la pone el listado, y el
        // listado no la enseña hasta saber que el backend la contesta.
        let w = column_widths(&s, "file", 80, None);
        let cols: Vec<ColumnId> = w.iter().map(|(id, _)| id.clone()).collect();
        assert_eq!(
            cols,
            vec![
                ColumnId::Builtin(Builtin::Name),
                ColumnId::Builtin(Builtin::Size),
                ColumnId::Builtin(Builtin::Mtime),
            ]
        );
        // El nombre absorbe el resto: suma == disponible.
        assert_eq!(w.iter().map(|(_, x)| *x).sum::<u16>(), 80);
    }

    #[test]
    fn column_widths_estrecho_solo_nombre() {
        let s = ColumnsSettings::default();
        let w = column_widths(&s, "file", 12, None);
        assert_eq!(
            w.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
            vec![ColumnId::Builtin(Builtin::Name)]
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
        // #117 y follow-up: attr: y plugin: se pintan ambos — el único
        // diagnóstico de cap presente aquí debe estar vacío.
        assert!(st.plugins_over_cap.is_empty(), "{:?}", st.plugins_over_cap);

        // Default: size + attr → name ANTEPUESTO (jamás sin nombre).
        let items = st.layout_items_for("file");
        let cols: Vec<String> = items.iter().map(|(id, _)| id.to_string()).collect();
        assert_eq!(cols, vec!["name", "size", "attr:posix.mode"]);

        // Scheme: reemplaza la lista entera.
        let items = st.layout_items_for("sftp");
        let cols: Vec<String> = items.iter().map(|(id, _)| id.to_string()).collect();
        assert_eq!(cols, vec!["name", "kind"]);

        // Sort: global mtime/desc; sftp hereda el global (sin override).
        let s = st.sort_for("file");
        assert_eq!((s.column, s.dir), (SortColumn::Mtime, SortDir::Desc));
        let s = st.sort_for("sftp");
        assert_eq!((s.column, s.dir), (SortColumn::Mtime, SortDir::Desc));
    }

    /// Las DOS tablas de anchos dicen lo mismo: la de serie
    /// (`default_layout_items`) y la que usa `[ui.columns]` configurado
    /// (`builtin_layout_item`). Se separaron una vez —12 y 10 para la fecha—
    /// y la columna configurada salía cortada («09-10 20:», 2026-09-11).
    #[test]
    fn las_dos_tablas_de_anchos_coinciden() {
        for (b, item) in default_layout_items() {
            assert_eq!(
                builtin_layout_item(b).policy,
                item.policy,
                "{b:?}: `builtin_layout_item` difiere de `default_layout_items`"
            );
        }
    }

    #[test]
    fn layout_items_normaliza_name_al_frente() {
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec!["size".into(), "name".into()]),
            ..Default::default()
        };
        let s = ColumnsSettings::resolve(&cfg);
        let items = s.layout_items_for("file");
        assert_eq!(items[0].0, ColumnId::Builtin(Builtin::Name));
        assert_eq!(items[1].0, ColumnId::Builtin(Builtin::Size));
    }

    #[test]
    fn sin_config_todo_es_default() {
        let st = ColumnsSettings::resolve(&norte_config::ColumnsConfig::default());
        assert_eq!(st.sort_for("file"), crate::sort::SortSpec::default());
        let items = st.layout_items_for("file");
        assert_eq!(items.len(), 4, "name+size+mtime+permisos");
        // Y en un scheme sin permisos POSIX, las tres de siempre: la cuarta
        // no se pone donde el backend no la puede contestar.
        assert_eq!(st.layout_items_for("s3").len(), 3, "name+size+mtime");
        assert!(st.invalid.is_empty() && st.plugins_over_cap.is_empty());
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

    /// El corpus canónico de modos, contra el formateador (spec 2026-09-20).
    ///
    /// Vive en `norte-testkit` y no aquí porque la columna dejó de ser
    /// opcional: el mismo corpus lo tienen que poder gastar los tests del
    /// provider local y los de la ventana, y una tabla copiada en tres
    /// sitios se separa en el primer modo nuevo.
    #[test]
    fn el_corpus_de_modos_se_pinta_entero() {
        for m in norte_testkit::corpus::posix_modes() {
            let pintado = format_mode_rwx(u32::try_from(m.mode).expect("cabe en u32"));
            assert_eq!(pintado, m.rwx, "{}: {}", m.id, m.why);
            assert_eq!(pintado.chars().count(), 10, "{} no mide diez", m.id);
        }
    }

    /// El gemelo de `no_type_bits` SÍ es un fichero regular, y los dos se
    /// tienen que poder distinguir. Era el fallo: los dos pintaban `-`.
    #[test]
    fn un_modo_sin_clase_no_se_confunde_con_un_fichero() {
        let corpus = norte_testkit::corpus::posix_modes();
        let sin = corpus
            .iter()
            .find(|m| m.id == "no_type_bits")
            .expect("el corpus lo trae");
        let gemelo = sin.twin.expect("la colisión necesita dos");
        let a = format_mode_rwx(u32::try_from(sin.mode).expect("cabe"));
        let b = format_mode_rwx(u32::try_from(gemelo).expect("cabe"));
        assert_ne!(a, b, "un directorio por SFTP se leía como fichero regular");
        assert!(a.starts_with('?'), "la clase que falta se PREGUNTA: {a}");
        assert!(b.starts_with('-'), "la que está se afirma: {b}");
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

    /// `Smart` (spec 2026-09-10): tres precisiones según la distancia, en la
    /// zona DADA — aquí UTC+2, para que «hoy» se decida en local y no en UTC
    /// (a las 23:30 UTC del 2 de julio son las 01:30 del 3 en Madrid).
    #[test]
    fn smart_es_hora_local_con_tres_precisiones() {
        let tz = jiff::tz::TimeZone::fixed(jiff::tz::offset(2));
        let en = norte_i18n::Lang::En;
        let now = 1_720_000_000_000; // 2024-07-03T09:46:40Z → 11:46 local
        let f = |ms| format_mtime_tz(ms, TimeFormat::Smart, now, en, &tz);
        assert_eq!(f(now), "11:46", "hoy: solo la hora, local");
        // 23:30Z del 2 de julio = 01:30 del 3 en local: SIGUE siendo hoy.
        assert_eq!(f(1_719_963_000_000), "01:30");
        // 21:30Z del 2 de julio = 23:30 del 2: ayer → mes-día y hora.
        assert_eq!(f(1_719_955_800_000), "07-02 23:30");
        // Otro año: solo la fecha.
        assert_eq!(f(951_782_400_000), "2000-02-29");
        assert!(f(now).len() <= 11 && f(1_719_955_800_000).len() <= 11);
        // Fuera del rango de jiff: cae al ISO UTC, que sabe pintar todo.
        assert!(f(i64::MIN).ends_with('Z'));
        assert!(f(i64::MAX).ends_with('Z'));
    }

    /// #117 encoding-audit L3: tiempos EXTREMOS (mtime basura de un
    /// provider hostil) — jamás un panic, siempre una cadena con forma.
    #[test]
    fn tiempos_extremos_sin_panic_y_con_forma() {
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        // ISO en ambos extremos del rango: forma RFC-3339 (año expandido
        // en el negativo), nunca vacío.
        let min = iso_utc_minutes(i64::MIN);
        assert!(min.starts_with('-') && min.ends_with('Z'), "{min}");
        let max = iso_utc_minutes(i64::MAX);
        assert!(max.ends_with('Z') && max.contains('T'), "{max}");
        // Relative con delta saturante en ambos sentidos: pasado remoto =
        // años; futuro remoto (delta negativo) = «now».
        let pasado = format_mtime(i64::MIN, TimeFormat::Relative, i64::MAX);
        assert!(pasado.contains('y'), "{pasado}");
        assert_eq!(
            format_mtime(i64::MAX, TimeFormat::Relative, i64::MIN),
            "now"
        );
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
        // 12 = «09-10 14:02» (11, el `Smart` de este año) + separador —
        // «hace 364d» (9) cabe de sobra.
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
                policy: WidthPolicy::Fixed(12),
                measured: 0,
                is_name: false,
            },
        ),
    ]
}

/// El atributo que lleva el modo POSIX de una entrada, y que la columna de
/// PERMISOS pinta como `drwxr-xr-x` (spec 2026-09-20).
///
/// Es el mismo id que anuncian `norte-vfs-local` y `norte-vfs-sftp` en su
/// catálogo de atributos; aquí está escrito una vez para que la columna por
/// defecto y su peldaño de la escalera nombren lo MISMO.
pub const POSIX_MODE_ATTR: &str = "posix.mode";

/// Los schemes cuyo listado enseña la columna de permisos SIN que nadie la
/// pida (spec 2026-09-20): los dos que tienen permisos POSIX de verdad.
///
/// La lista es corta y explícita a propósito. Lo alternativo —enseñarla
/// siempre y dejarla vacía donde el backend no contesta— gasta ancho del
/// nombre en un bucket de objetos o dentro de un `.zip` para no decir nada,
/// que es justo lo que ADR 0124 vino a arreglar.
const SCHEMES_CON_PERMISOS: &[&str] = &["file", "sftp"];

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
    /// Formato de modo (solo lo leen celdas attr con hint `Mode`).
    pub mode_format: ModeFormat,
    /// Hint del catálogo para columnas attr (`Opaque` si no hay catálogo
    /// o la columna es builtin — los builtin no lo leen).
    pub hint: norte_proto::attrs::AttrHint,
    /// Alineación efectiva.
    pub align: Align,
    /// Cabecera propia (saneada, ≤ [`HEADER_MAX_CHARS`]); `None` = la
    /// etiqueta Fluent de siempre.
    pub header: Option<String>,
}

impl ColumnStyle {
    /// El mismo estilo en formato corto, si `compact` (lo que dice
    /// [`Fitted::compact`]). Toca los dos formatos: cada columna lee el suyo.
    #[must_use]
    pub fn compacted(mut self, compact: bool) -> Self {
        if compact {
            self.size_format = SizeFormat::Short;
            self.time_format = TimeFormat::Short;
        }
        self
    }

    /// Los defaults del builtin sin spec: iec/relative, nombre a la
    /// izquierda y el resto a la derecha — la convención que ya pintaban
    /// ambos frontends.
    #[must_use]
    pub fn default_for(b: Builtin) -> Self {
        Self {
            size_format: SizeFormat::Iec,
            time_format: TimeFormat::Relative,
            mode_format: ModeFormat::Rwx,
            hint: norte_proto::attrs::AttrHint::Opaque,
            align: if b == Builtin::Name {
                Align::Left
            } else {
                Align::Right
            },
            header: None,
        }
    }

    /// Defaults de CUALQUIER columna (#117): builtin = `default_for`;
    /// attr = alineación y hint del catálogo (`Opaque`/izquierda sin él,
    /// Size/Timestamp/Mode a la derecha); plugin = texto a la izquierda.
    #[must_use]
    pub fn default_for_id(id: &ColumnId, catalog: Option<&norte_proto::AttrCatalog>) -> Self {
        use norte_proto::attrs::AttrHint;
        match id {
            ColumnId::Builtin(b) => Self::default_for(*b),
            ColumnId::Attr(aid) => {
                let hint = catalog
                    .and_then(|c| c.iter().find(|i| i.id == *aid))
                    .map_or(AttrHint::Opaque, |i| i.hint);
                Self {
                    align: match hint {
                        AttrHint::Size | AttrHint::Timestamp | AttrHint::Mode => Align::Right,
                        _ => Align::Left,
                    },
                    hint,
                    // `default_for(Kind)` solo aporta los campos no-align
                    // (iec/relative/rwx, header None); su align se pisa.
                    ..Self::default_for(Builtin::Kind)
                }
            }
            ColumnId::Plugin { .. } => Self {
                align: Align::Left,
                ..Self::default_for(Builtin::Kind)
            },
        }
    }
}

/// Tabla ÚNICA str ↔ enum del vocabulario de formatos de `Size` (#108 7b
/// m3), en el ORDEN del ciclo del picker. La consumen `format_fits`,
/// `next_format`, `format_name` y `style_for` — antes eran CUATRO copias
/// (config aparte); el bloque 2 (attrs) extiende tablas, no matches
/// dispersos. Las cadenas deben ser subconjunto del vocabulario global que
/// acepta la config (`norte-config/src/load.rs`, parse del spec) — pineado
/// por test.
const SIZE_FORMATS: &[(&str, SizeFormat)] = &[
    ("iec", SizeFormat::Iec),
    ("si", SizeFormat::Si),
    ("exact", SizeFormat::Exact),
];

/// Tabla str ↔ enum de `Mtime` — mismas reglas que [`SIZE_FORMATS`].
const TIME_FORMATS: &[(&str, TimeFormat)] = &[
    ("relative", TimeFormat::Relative),
    ("iso", TimeFormat::Iso),
    ("smart", TimeFormat::Smart),
];

/// [`TimeFormat`] de un `[ui] date_format` (spec 2026-09-10).
#[must_use]
pub fn time_format_of(f: norte_config::DateFormat) -> TimeFormat {
    match f {
        norte_config::DateFormat::Smart => TimeFormat::Smart,
        norte_config::DateFormat::Relative => TimeFormat::Relative,
        norte_config::DateFormat::Iso => TimeFormat::Iso,
    }
}

/// Tabla str ↔ enum de columnas con hint `Mode` — mismas reglas que
/// [`SIZE_FORMATS`]. Las cadenas entran al vocabulario global de config
/// en la tarea 4 de #117.
const MODE_FORMATS: &[(&str, ModeFormat)] =
    &[("rwx", ModeFormat::Rwx), ("octal", ModeFormat::Octal)];

/// ¿Casa `fmt` (vocabulario global YA validado en config) con la columna?
/// SOLO builtins: `Name`/`Kind` no admiten formato alguno. Los formatos de
/// attrs NO pasan por aquí a propósito (#117): despachan por el hint del
/// catálogo al plegar (`style_for_id`) y una palabra que no casa conserva
/// el default — no extender esta función para attrs.
fn format_fits(b: Builtin, fmt: &str) -> bool {
    match b {
        Builtin::Size => SIZE_FORMATS.iter().any(|(s, _)| *s == fmt),
        Builtin::Mtime => TIME_FORMATS.iter().any(|(s, _)| *s == fmt),
        Builtin::Name | Builtin::Kind => false,
    }
}

/// El siguiente formato del ciclo del picker (#108 7b): rota la tabla del
/// builtin; `None` = la columna no admite formato (name/kind) o `current`
/// no está en la tabla.
#[must_use]
pub fn next_format(b: Builtin, current: &str) -> Option<&'static str> {
    fn advance<T>(tab: &'static [(&'static str, T)], cur: &str) -> Option<&'static str> {
        let i = tab.iter().position(|(s, _)| *s == cur)?;
        Some(tab[(i + 1) % tab.len()].0)
    }
    match b {
        Builtin::Size => advance(SIZE_FORMATS, current),
        Builtin::Mtime => advance(TIME_FORMATS, current),
        Builtin::Name | Builtin::Kind => None,
    }
}

/// El siguiente formato del ciclo del picker para CUALQUIER columna
/// (#117): builtin por su tabla; attr por la tabla de su hint (Size/
/// Timestamp/Mode); el resto no admite formato.
#[must_use]
pub fn next_format_id(
    id: &ColumnId,
    hint: norte_proto::attrs::AttrHint,
    current: &str,
) -> Option<&'static str> {
    use norte_proto::attrs::AttrHint;
    fn advance<T>(tab: &'static [(&'static str, T)], cur: &str) -> Option<&'static str> {
        let i = tab.iter().position(|(s, _)| *s == cur)?;
        Some(tab[(i + 1) % tab.len()].0)
    }
    match id {
        ColumnId::Builtin(b) => next_format(*b, current),
        ColumnId::Attr(_) => match hint {
            AttrHint::Size => advance(SIZE_FORMATS, current),
            AttrHint::Timestamp => advance(TIME_FORMATS, current),
            AttrHint::Mode => advance(MODE_FORMATS, current),
            _ => None,
        },
        ColumnId::Plugin { .. } => None,
    }
}

/// El nombre-str del formato vigente para cualquier columna (#117) — seed
/// del picker, dirección enum → str, por el hint DEL PROPIO estilo en
/// attrs (`style.hint`: un solo origen, sin param que pueda desincronizar).
#[must_use]
pub fn format_name_id(id: &ColumnId, style: &ColumnStyle) -> Option<&'static str> {
    use norte_proto::attrs::AttrHint;
    match id {
        ColumnId::Builtin(b) => format_name(*b, style),
        ColumnId::Attr(_) => match style.hint {
            AttrHint::Size => SIZE_FORMATS
                .iter()
                .find(|(_, f)| *f == style.size_format)
                .map(|(s, _)| *s),
            AttrHint::Timestamp => TIME_FORMATS
                .iter()
                .find(|(_, f)| *f == style.time_format)
                .map(|(s, _)| *s),
            AttrHint::Mode => MODE_FORMATS
                .iter()
                .find(|(_, f)| *f == style.mode_format)
                .map(|(s, _)| *s),
            _ => None,
        },
        ColumnId::Plugin { .. } => None,
    }
}

/// El nombre-str del formato vigente de un estilo (seed del picker): la
/// misma tabla, en dirección enum → str.
#[must_use]
pub fn format_name(b: Builtin, style: &ColumnStyle) -> Option<&'static str> {
    match b {
        Builtin::Size => SIZE_FORMATS
            .iter()
            .find(|(_, f)| *f == style.size_format)
            .map(|(s, _)| *s),
        Builtin::Mtime => TIME_FORMATS
            .iter()
            .find(|(_, f)| *f == style.time_format)
            .map(|(s, _)| *s),
        Builtin::Name | Builtin::Kind => None,
    }
}

/// Config de columnas RESUELTA (#108 bloque 4): ids parseados, sort
/// mapeado, overrides por scheme. Los ids que no parsean van a
/// [`ColumnsSettings::invalid`] — se saltan al pintar y los reporta
/// `norte doctor` (jamás un drop silencioso ni un error de arranque).
/// Los `attr:` (#117) y los `plugin:` (follow-up) se pintan ambos por el
/// funnel generalizado, cada familia con su cap pintado == pedido.
#[derive(Debug, Clone, Default)]
pub struct ColumnsSettings {
    default_set: Option<Vec<ColumnId>>,
    default_sort: crate::sort::SortSpec,
    /// `[ui] date_format` (spec 2026-09-10): el formato de las columnas de
    /// tiempo cuando ningún spec lo fija. `None` = el de siempre (`relative`),
    /// que es lo que un `ColumnsSettings` de test o de `doctor` quiere: la
    /// hora local de `smart` depende de la máquina, y una foto no puede.
    /// Los frontends la reciben por [`Self::with_date_format`].
    default_time: Option<TimeFormat>,
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
    /// El rótulo que su MANIFIESTO le puso a cada columna de plugin, por id
    /// de columna (`plugin:acme.git/status`), ya saneado. No sale de la
    /// configuración sino del catálogo vivo de plugins, así que lo instala
    /// el frontend cuando lo recibe ([`Self::apply_plugin_headers`]); sin
    /// él la cabecera enseñaría el id, que es lo que el lector menos
    /// reconoce.
    plugin_headers: std::collections::BTreeMap<String, String>,
    /// Ids configurados que NO parsean (diagnóstico para doctor).
    pub invalid: Vec<String>,
    /// Ids `plugin:` configurados MÁS ALLÁ del cap de petición
    /// ([`PLUGIN_COLUMNS_MAX_REQUEST`]) en alguna lista: el funnel no los
    /// pinta y el pane no los pide (pintado == pedido); doctor los nombra
    /// (`columns-plugins-over-cap`).
    pub plugins_over_cap: Vec<String>,
    /// Ids `attr:` configurados MÁS ALLÁ del cap de petición
    /// ([`norte_proto::ATTRS_MAX_REQUEST`]) en alguna lista (#117 review):
    /// el funnel no los pinta y el pane no los pide — pintado == pedido,
    /// jamás una columna permanentemente en blanco; doctor los nombra
    /// (`columns-attrs-over-cap`).
    pub attrs_over_cap: Vec<String>,
    /// Ids `attr:` que parsean como columna pero cuyo id attr NO es legal
    /// en el wire (#117 encoding-audit M1,
    /// [`norte_proto::attrs::is_valid_attr_id`]: minúsculas con namespace —
    /// `attr:Posix.Mode` parsea y aun así el daemon lo rechazaría con
    /// -32602 tumbando el `fs.list` ENTERO). El funnel los salta y el pane
    /// no los pide (pintado == pedido, jamás una columna en blanco ni un
    /// listado muerto); la forma cruda se preserva para picker/persist y
    /// doctor los nombra (`columns-attr-id-not-wire-safe`).
    pub attrs_not_wire_safe: Vec<String>,
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
    /// Los ids `attr:`/`plugin:` se retienen tal cual (#117): sus formatos
    /// pasan sin validar aquí — al plegar (`style_for_id`) solo casan las
    /// palabras de [`MODE_FORMATS`]/[`SIZE_FORMATS`]/[`TIME_FORMATS`], una
    /// desconocida conserva el default.
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

    /// El estilo efectivo de CUALQUIER columna en `scheme` (#117):
    /// defaults ← spec global ← spec del scheme, campo a campo. El formato
    /// str se pliega por la tabla que lo contenga (size/time/mode) — en
    /// builtins resolve ya validó el encaje; en attrs el hint decide qué
    /// campo se LEE al pintar, así que plegar los tres es inocuo.
    #[must_use]
    pub fn style_for_id(
        &self,
        scheme: &str,
        id: &ColumnId,
        catalog: Option<&norte_proto::AttrCatalog>,
    ) -> ColumnStyle {
        let key = id.to_string();
        let mut style = ColumnStyle::default_for_id(id, catalog);
        if let Some(t) = self.default_time {
            style.time_format = t;
        }
        // El rótulo del manifiesto, DEBAJO de los specs: `[ui.columns]
        // header` sigue mandando, y el id solo aparece cuando no hay ni lo
        // uno ni lo otro.
        if matches!(id, ColumnId::Plugin { .. }) {
            style
                .header
                .clone_from(&self.plugin_headers.get(&key).cloned());
        }
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
                if let Some((_, f)) = SIZE_FORMATS.iter().find(|(s, _)| *s == fmt) {
                    style.size_format = *f;
                } else if let Some((_, f)) = TIME_FORMATS.iter().find(|(s, _)| *s == fmt) {
                    style.time_format = *f;
                } else if let Some((_, f)) = MODE_FORMATS.iter().find(|(s, _)| *s == fmt) {
                    style.mode_format = *f;
                }
            }
            if spec.header.is_some() {
                style.header.clone_from(&spec.header);
            }
        }
        style
    }

    /// Fija el formato de tiempo por defecto desde `[ui] date_format`
    /// (spec 2026-09-10). Un spec de columna sigue ganando.
    #[must_use]
    pub fn with_date_format(mut self, f: norte_config::DateFormat) -> Self {
        self.default_time = Some(time_format_of(f));
        self
    }

    /// [`Self::style_for_id`] para un builtin — la firma histórica (7b).
    #[must_use]
    pub fn style_for(&self, scheme: &str, builtin: Builtin) -> ColumnStyle {
        self.style_for_id(scheme, &ColumnId::Builtin(builtin), None)
    }

    /// Aplica EN MEMORIA un formato elegido en el picker (#108 7b):
    /// actualiza (o crea) la entrada retenida de `specs_global` para `id` —
    /// el mismo lockstep sesión↔disco que [`Self::apply_picked`] frente a
    /// `persist_column_format`, para que `style_for` lo vea al instante sin
    /// esperar al hot-reload. `format` llega del vocabulario cerrado del
    /// picker (ya encaja con su columna): sin re-saneo. Un spec del MISMO id
    /// a nivel de scheme sigue ganando (persistencia por scheme = diferido).
    pub fn apply_format(&mut self, id: &str, format: &str) {
        self.specs_global.entry(id.to_owned()).or_default().format = Some(format.to_owned());
    }

    /// Instala los rótulos que los MANIFIESTOS dan a las columnas de plugin
    /// (spec 2026-09-11): `{ "plugin:acme.git/status": "Estado" }`, ya
    /// saneados por quien los recibe del catálogo. Los dos frontends llaman
    /// aquí con lo que su sonda de columnas ya pide, y así la cabecera se
    /// llama igual en los dos — `[ui.columns] header` sigue por encima.
    ///
    /// Solo AÑADE: un catálogo que llega sin una columna no borra su rótulo,
    /// porque el catálogo se pide por tandas y una tanda no es la verdad
    /// entera.
    pub fn apply_plugin_headers(
        &mut self,
        headers: impl IntoIterator<Item = (String, String)>,
    ) -> bool {
        let mut cambio = false;
        for (id, rotulo) in headers {
            if self.plugin_headers.get(&id) != Some(&rotulo) {
                self.plugin_headers.insert(id, rotulo);
                cambio = true;
            }
        }
        cambio
    }

    /// Aplica EN MEMORIA un ancho fijo (spec 2026-09-11, V2: arrastrar el
    /// borde de una cabecera): la pareja de `persist_column_width`, con el
    /// mismo lockstep sesión↔disco que [`Self::apply_format`]. `cells` se
    /// acota aquí a `[1, 64]`, el rango que el loader acepta: un ancho fuera
    /// de él escrito a disco dejaría el `norte.toml` entero sin cargar.
    pub fn apply_width(&mut self, id: &str, cells: u16) -> u16 {
        let cells = cells.clamp(1, 64);
        self.specs_global.entry(id.to_owned()).or_default().width =
            Some(norte_config::WidthChoice::Fixed(cells));
        cells
    }

    /// ¿Fija un spec DEL SCHEME el formato de `builtin`? (#108 7b m2). Con
    /// un override así, ciclar en el picker escribiría el spec GLOBAL que
    /// el del scheme seguiría enmascarando — sesión y disco «consistentes»
    /// pero invisibles (toast mentiroso) y con fuga a otros schemes: el
    /// picker BLOQUEA esas filas. El formato retenido ya está validado
    /// contra su columna en [`Self::resolve`].
    #[must_use]
    pub fn format_pinned_by_scheme(&self, scheme: &str, builtin: Builtin) -> bool {
        self.format_pinned_by_scheme_id(scheme, &ColumnId::Builtin(builtin))
    }

    /// [`Self::format_pinned_by_scheme`] para cualquier id (#117).
    #[must_use]
    pub fn format_pinned_by_scheme_id(&self, scheme: &str, id: &ColumnId) -> bool {
        let key = id.to_string();
        self.specs_schemes
            .get(scheme)
            .and_then(|m| m.get(&key))
            .is_some_and(|sp| sp.format.is_some())
    }

    /// ¿Tiene `id` un `width` en algún spec (global o del scheme)? Es lo que
    /// escribe arrastrar un borde, así que es «el usuario lo tocó», y
    /// [`fitted_columns`] no lo mueve.
    #[must_use]
    pub fn width_pinned(&self, scheme: &str, id: &ColumnId) -> bool {
        self.spec_field(scheme, id, |s| s.width.is_some())
    }

    /// La lista de columnas que alguien ESCRIBIÓ para `scheme`, si la hay:
    /// la del scheme si la tiene, y si no la global.
    ///
    /// Fuente única de una pregunta que se hacía en dos sitios con la misma
    /// expresión copiada (`layout_items_for` y el predicado de abajo). Dos
    /// copias que hoy coinciden son una invariante sostenida por un
    /// pegado, y la primera que se toque las separa.
    fn configured_ids(&self, scheme: &str) -> Option<&Vec<ColumnId>> {
        self.schemes
            .get(scheme)
            .and_then(|(c, _)| c.as_ref())
            .or(self.default_set.as_ref())
    }

    /// ¿Pinta este scheme una lista de columnas que ESCRIBIÓ alguien, en vez
    /// del set por defecto? (spec 2026-09-20)
    ///
    /// Lo preguntan la escalera de [`fitted_columns`] —la columna de
    /// permisos cede sitio cuando la puso norte y no cuando la puso el
    /// usuario— y el selector de columnas, que necesita saber si enseñarla
    /// encendida.
    #[must_use]
    pub fn has_user_columns(&self, scheme: &str) -> bool {
        self.configured_ids(scheme).is_some()
    }

    /// ¿Tiene `id` un `format` en algún spec (global o del scheme)? Quien
    /// eligió un formato no quiere que se lo cambien por el corto.
    #[must_use]
    pub fn format_pinned(&self, scheme: &str, id: &ColumnId) -> bool {
        self.spec_field(scheme, id, |s| s.format.is_some())
    }

    fn spec_field(
        &self,
        scheme: &str,
        id: &ColumnId,
        campo: impl Fn(&norte_config::ColumnSpec) -> bool,
    ) -> bool {
        let key = id.to_string();
        let global = self.specs_global.get(&key);
        let scoped = self.specs_schemes.get(scheme).and_then(|m| m.get(&key));
        [global, scoped].into_iter().flatten().any(campo)
    }

    fn collect_diagnostics(&mut self, ids: &[String]) {
        // #117 review: el cap de attrs pedibles es POR LISTA pintada
        // (default o scheme), con el mismo dedup que `layout_items_for` —
        // un dup no consume hueco. Para un `attr:` la forma cruda y la
        // Display coinciden, así que el dedup por raw basta.
        let mut attrs_vistos: Vec<&String> = Vec::new();
        let mut plugins_vistos: Vec<&String> = Vec::new();
        for raw in ids {
            match raw.parse::<ColumnId>() {
                Err(_) => {
                    if !self.invalid.contains(raw) {
                        self.invalid.push(raw.clone());
                    }
                }
                // #117-follow-up: los `plugin:` YA se pintan — el único
                // diagnóstico que les queda es el cap (espejo de los attrs).
                Ok(ColumnId::Plugin { .. }) => {
                    if !plugins_vistos.contains(&raw) {
                        plugins_vistos.push(raw);
                        if plugins_vistos.len() > PLUGIN_COLUMNS_MAX_REQUEST
                            && !self.plugins_over_cap.contains(raw)
                        {
                            self.plugins_over_cap.push(raw.clone());
                        }
                    }
                }
                Ok(ColumnId::Attr(a)) => {
                    // #117 encoding-audit M1: un id que parsea como columna
                    // pero no es legal en el wire jamás se pinta ni se pide
                    // (`layout_items_for` lo salta) — no consume hueco del
                    // cap, igual que no consume columna.
                    if !norte_proto::attrs::is_valid_attr_id(&a) {
                        if !self.attrs_not_wire_safe.contains(raw) {
                            self.attrs_not_wire_safe.push(raw.clone());
                        }
                    } else if !attrs_vistos.contains(&raw) {
                        attrs_vistos.push(raw);
                        if attrs_vistos.len() > norte_proto::ATTRS_MAX_REQUEST
                            && !self.attrs_over_cap.contains(raw)
                        {
                            self.attrs_over_cap.push(raw.clone());
                        }
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

    /// Los items de layout para un pane en `scheme`, en orden de pintado
    /// (#117 y follow-up: builtins, attrs Y `plugin:` — cada familia con su
    /// cap pintado == pedido). Dedup por id; el nombre jamás
    /// desaparece ni deja de ir primero. El `width` de un
    /// `[[ui.columns.spec]]` (global ← scheme) SUSTITUYE la política del
    /// item — sobre cualquier columna, también attrs. Un width sobre `name`
    /// se aplica pero conserva `is_name: true`: las reglas de suelo del
    /// nombre de [`layout`] (regla 3, [`NAME_MIN`], jamás descartado)
    /// siguen ganando.
    #[must_use]
    pub fn layout_items_for(&self, scheme: &str) -> Vec<(ColumnId, LayoutItem)> {
        let ids = self.configured_ids(scheme);
        let mut out = match ids {
            None => {
                let mut v: Vec<(ColumnId, LayoutItem)> = default_layout_items()
                    .into_iter()
                    .map(|(b, it)| (ColumnId::Builtin(b), it))
                    .collect();
                // La columna de PERMISOS, donde hay permisos (spec
                // 2026-09-20). Va solo en el set por defecto: en cuanto
                // alguien escribe sus columnas manda su lista, y si no la
                // nombró es que no la quiere.
                if SCHEMES_CON_PERMISOS.contains(&scheme) {
                    v.push((
                        ColumnId::Attr(POSIX_MODE_ATTR.to_owned()),
                        attr_layout_item(),
                    ));
                }
                v
            }
            Some(ids) => {
                let mut out: Vec<(ColumnId, LayoutItem)> = Vec::new();
                for id in ids {
                    match id {
                        _ if out.iter().any(|(x, _)| x == id) => {}
                        // #117-follow-up: mismo trato que los attrs — item
                        // por defecto (width override por spec gratis) y cap
                        // pintado == pedido (cada columna es una RPC).
                        ColumnId::Plugin { .. } => {
                            let plugins = out
                                .iter()
                                .filter(|(x, _)| matches!(x, ColumnId::Plugin { .. }))
                                .count();
                            if plugins < PLUGIN_COLUMNS_MAX_REQUEST {
                                out.push((id.clone(), plugin_layout_item()));
                            }
                        }
                        ColumnId::Builtin(b) => out.push((id.clone(), builtin_layout_item(*b))),
                        // #117 review: a lo sumo ATTRS_MAX_REQUEST attrs —
                        // pintado == pedido (`attr_ids_for`); el resto es
                        // diagnóstico (`attrs_over_cap`, doctor lo nombra),
                        // jamás una columna permanentemente en blanco.
                        // Encoding-audit M1: solo ids LEGALES del wire — un
                        // `attr:Posix.Mode` pedido a un daemon sería -32602
                        // y tumbaría el listado entero; se salta aquí
                        // (diagnóstico `attrs_not_wire_safe`).
                        ColumnId::Attr(a) => {
                            if norte_proto::attrs::is_valid_attr_id(a) {
                                let attrs = out
                                    .iter()
                                    .filter(|(x, _)| matches!(x, ColumnId::Attr(_)))
                                    .count();
                                if attrs < norte_proto::ATTRS_MAX_REQUEST {
                                    out.push((id.clone(), attr_layout_item()));
                                }
                            }
                        }
                    }
                }
                let name = ColumnId::Builtin(Builtin::Name);
                match out.iter().position(|(id, _)| *id == name) {
                    Some(pos) if pos > 0 => {
                        let n = out.remove(pos);
                        out.insert(0, n);
                    }
                    Some(_) => {}
                    None => out.insert(0, (name, builtin_layout_item(Builtin::Name))),
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
    fn apply_width_overrides(&self, scheme: &str, items: &mut [(ColumnId, LayoutItem)]) {
        for (id, item) in items.iter_mut() {
            let key = id.to_string();
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

    /// Los ids attr CONFIGURADOS y renderizables de `scheme` (#117): lo
    /// que el pane pide en `fs.list`. Dedup del funnel; el cap a
    /// [`norte_proto::ATTRS_MAX_REQUEST`] ya lo aplica `layout_items_for`
    /// (pintado == pedido) — el `take` de aquí es cinturón (el daemon
    /// rechazaría más).
    #[must_use]
    pub fn attr_ids_for(&self, scheme: &str) -> Vec<String> {
        self.layout_items_for(scheme)
            .into_iter()
            .filter_map(|(id, _)| match id {
                ColumnId::Attr(a) => Some(a),
                _ => None,
            })
            .take(norte_proto::ATTRS_MAX_REQUEST)
            .collect()
    }

    /// Huella ORDENADA de [`Self::attr_ids_for`] (#117, review tarea 3):
    /// decide si un cambio de columnas exige re-listar. Ordenada porque un
    /// mero reorden de columnas no cambia QUÉ valores hay que pedir — ambos
    /// frontends comparan huellas antes/después con esta única definición.
    #[must_use]
    pub fn attr_fingerprint(&self, scheme: &str) -> Vec<String> {
        let mut ids = self.attr_ids_for(scheme);
        ids.sort_unstable();
        ids
    }

    /// Las columnas `plugin:` CONFIGURADAS y pintables de `scheme`
    /// (#117-follow-up), como pares `(plugin, columna)` en orden de
    /// pintado — lo que el frontend pide vía `plugin.column_values` (el
    /// id del wire es la COLUMNA bare; el plugin valida pertenencia
    /// contra el catálogo `plugin.list`). Cap ya aplicado por
    /// `layout_items_for` (pintado == pedido).
    #[must_use]
    pub fn plugin_ids_for(&self, scheme: &str) -> Vec<(String, String)> {
        self.layout_items_for(scheme)
            .into_iter()
            .filter_map(|(id, _)| match id {
                ColumnId::Plugin { plugin, column } => Some((plugin, column)),
                _ => None,
            })
            .take(PLUGIN_COLUMNS_MAX_REQUEST)
            .collect()
    }

    /// Huella ORDENADA de [`Self::plugin_ids_for`] — espejo de
    /// [`Self::attr_fingerprint`]: decide si un cambio de columnas exige
    /// re-pedir valores de plugin (un reorden no).
    #[must_use]
    pub fn plugin_fingerprint(&self, scheme: &str) -> Vec<(String, String)> {
        let mut ids = self.plugin_ids_for(scheme);
        ids.sort_unstable();
        ids
    }

    /// Huella COMBINADA attr+plugin de un pane (#117-follow-up, review
    /// MAJOR-1): la ÚNICA definición de «¿cambió lo que este pane pide?»
    /// para ambos frontends — attr ids más los display ids `plugin:` en
    /// forma ordenada. Si divergiera por frontend, uno dejaría de
    /// re-listar ante un cambio solo-de-plugins y la columna nueva
    /// quedaría permanentemente en blanco.
    #[must_use]
    pub fn pane_fingerprint(&self, scheme: &str) -> Vec<String> {
        let mut ids = self.attr_fingerprint(scheme);
        ids.extend(
            self.plugin_fingerprint(scheme)
                .into_iter()
                .map(|(p, c)| plugin_display_id(&p, &c)),
        );
        ids
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
        // 12 = «09-10 14:02» (11, el `smart` de este año) + separador. El
        // MISMO número que `default_layout_items`: con `[ui.columns]`
        // configurado se pasaba por aquí, y aquí seguía en 10 —la fecha
        // salía «09-10 20:» (2026-09-11). Hay test que ata las dos tablas.
        Builtin::Mtime => LayoutItem {
            policy: WidthPolicy::Fixed(12),
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

/// Item de layout por defecto de una columna attr (#117): `Fixed(12)`
/// (separador incluido) — el ancho fino se ajusta con el width override
/// del spec, que llega gratis por `apply_width_overrides`.
#[must_use]
pub fn attr_layout_item() -> LayoutItem {
    LayoutItem {
        policy: WidthPolicy::Fixed(12),
        measured: 0,
        is_name: false,
    }
}

/// Item de layout por defecto de una columna `plugin:` (#117-follow-up):
/// mismo `Fixed(12)` que los attrs — los valores están capados a
/// [`COLUMN_VALUE_MAX_CHARS`] en el ingest y el ancho fino se ajusta con
/// el width override del spec.
#[must_use]
pub fn plugin_layout_item() -> LayoutItem {
    LayoutItem {
        policy: WidthPolicy::Fixed(12),
        measured: 0,
        is_name: false,
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
            norte_config::SortColumnKey::Extension => SortColumn::Extension,
        },
        dir: if s.descending {
            SortDir::Desc
        } else {
            SortDir::Asc
        },
        dirs_first: s.dirs_first,
    }
}

/// ¿Pone este listado la columna de permisos por su cuenta?
///
/// Tres condiciones, y las tres son necesarias:
///
/// 1. nadie escribió sus propias columnas para este scheme — si lo hizo,
///    manda su lista, y si no la nombró es que no la quiere;
/// 2. el scheme es de los que tienen permisos POSIX;
/// 3. el provider CONTESTÓ y dice que tiene `posix.mode`, con el hint que
///    dice que es un modo. `None` —aún no ha contestado— es que no.
///
/// La tercera exige el hint y no solo el id porque el id es un nombre que
/// cualquier provider puede usar para lo que quiera: sin comprobarlo, norte
/// pondría su cabecera traducida «Modo» encima de una cadena ajena que nadie
/// pidió ver. La cabecera es de norte, así que el valor tiene que serlo
/// también.
///
/// Es pública porque la respuesta la necesitan DOS superficies y tiene que
/// ser la misma: el listado, que la pinta, y el selector de columnas, que
/// tiene que enseñarla encendida — decir que está apagada mientras se pinta
/// convierte confirmar el diálogo en borrarla sin avisar.
#[must_use]
pub fn pone_los_permisos(
    settings: &ColumnsSettings,
    scheme: &str,
    catalog: Option<&norte_proto::AttrCatalog>,
) -> bool {
    !settings.has_user_columns(scheme)
        && SCHEMES_CON_PERMISOS.contains(&scheme)
        && catalog.is_some_and(|c| {
            c.iter()
                .any(|a| a.id == POSIX_MODE_ATTR && a.hint == norte_proto::AttrHint::Mode)
        })
}

/// Las columnas que este listado va a PINTAR: las configuradas, menos la de
/// permisos que puso norte si el backend no la puede contestar.
///
/// Un único sitio donde se decide, porque lo preguntan dos funciones y la
/// respuesta tiene que ser la misma en las dos (spec 2026-09-20).
fn items_pintables(
    settings: &ColumnsSettings,
    scheme: &str,
    catalog: Option<&norte_proto::AttrCatalog>,
) -> Vec<(ColumnId, LayoutItem)> {
    let mut set = settings.layout_items_for(scheme);
    // Una columna que pidió el usuario se queda aunque salga vacía: es su
    // elección, y borrársela sería contestarle que no.
    if !settings.has_user_columns(scheme) && !pone_los_permisos(settings, scheme, catalog) {
        set.retain(|(id, _)| !matches!(id, ColumnId::Attr(a) if a == POSIX_MODE_ATTR));
    }
    set
}

/// Anchos de las columnas (#108) para un ancho interior en CELDAS:
/// `(id, ancho)` de las columnas VIVAS de `settings` para `scheme`,
/// en orden de pintado — una columna sin sitio no aparece. Compartido
/// TUI/GUI: ambos frontends pintan el MISMO conjunto del mismo [`layout`].
#[must_use]
pub fn column_widths(
    settings: &ColumnsSettings,
    scheme: &str,
    inner_width: u16,
    // Lo mismo que en [`fitted_columns`], y por la misma razón: dos
    // respuestas distintas a «¿qué columnas hay?» dejarían el borde que
    // arrastra el ratón agarrando la columna de al lado.
    catalog: Option<&norte_proto::AttrCatalog>,
) -> Vec<(ColumnId, u16)> {
    let set = items_pintables(settings, scheme, catalog);
    let items: Vec<_> = set.iter().map(|(_, it)| *it).collect();
    let placed = layout(inner_width, &items);
    set.into_iter()
        .zip(placed)
        .filter_map(|((id, _), w)| w.map(|w| (id, w)))
        .collect()
}

/// Ancho de una columna compacta, separador incluido: `1023B`, `1.3M`,
/// `22:19` y `09-16` caben en cinco celdas.
pub const COMPACT_WIDTH: u16 = 6;

/// Una columna tal como se pinta tras [`fitted_columns`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fitted {
    /// Qué columna.
    pub id: ColumnId,
    /// Su ancho en celdas, separador incluido.
    pub width: u16,
    /// Va en formato corto ([`SizeFormat::Short`] / [`TimeFormat::Short`]):
    /// quien pinta lo aplica al estilo con [`ColumnStyle::compacted`].
    pub compact: bool,
}

/// Un peldaño de la escalera de [`fitted_columns`].
#[derive(Debug, Clone, Copy)]
enum Peldano {
    Ocultar,
    Compactar,
}

/// A qué columna apunta un peldaño. Existe porque desde la spec 2026-09-20
/// la escalera tiene un peldaño que NO es un builtin: la columna de permisos
/// que el listado pone por su cuenta.
#[derive(Debug, Clone, Copy)]
enum Cede {
    Builtin(Builtin),
    /// Un atributo, nombrado por su id. Solo el que el propio listado añade
    /// por defecto: uno que el usuario pidió sigue exento (ADR 0124).
    PorDefecto(&'static str),
}

impl Cede {
    fn id(self) -> ColumnId {
        match self {
            Self::Builtin(b) => ColumnId::Builtin(b),
            Self::PorDefecto(a) => ColumnId::Attr(a.to_owned()),
        }
    }
}

/// El orden en que las columnas ceden sitio al nombre. Primero lo que ya se
/// dice de otra forma —la clase la cuentan el icono, el color y la `/`—,
/// luego lo que se puede decir más corto, y solo al final lo que se pierde.
///
/// Los permisos van los PRIMEROS de todos, y no por ser menos útiles: son la
/// columna que el listado pone sin que nadie la pida, así que es la primera
/// que tiene que irse cuando el sitio no da. ADR 0124 exime a las columnas
/// `attr:` de la escalera «porque las pidió alguien a propósito»; ésta no la
/// pidió nadie, y por eso la excepción no la alcanza (ver
/// [`ColumnsSettings::pone_sus_columnas`]).
const ESCALERA: [(Cede, Peldano); 6] = [
    (Cede::PorDefecto(POSIX_MODE_ATTR), Peldano::Ocultar),
    (Cede::Builtin(Builtin::Kind), Peldano::Ocultar),
    (Cede::Builtin(Builtin::Mtime), Peldano::Compactar),
    (Cede::Builtin(Builtin::Size), Peldano::Compactar),
    (Cede::Builtin(Builtin::Mtime), Peldano::Ocultar),
    (Cede::Builtin(Builtin::Size), Peldano::Ocultar),
];

/// Anchos que dan prioridad a LEER el nombre.
///
/// [`column_widths`] reparte sin mirar lo que hay en el directorio: el nombre
/// se queda con lo que sobra y, en un panel estrecho, eso es `Cap….png`.
/// Aquí el nombre tiene un objetivo, `name_wanted` (lo que quien pinta midió
/// del listado, ver [`name_width_p80`]), acotado a 3/5 del ancho para que
/// un nombre larguísimo no se coma todas las demás. Mientras no llega, las
/// demás columnas ceden un peldaño cada vez —ocultar la clase, fecha corta,
/// tamaño corto, ocultar la fecha, ocultar el tamaño— y se para en cuanto
/// llega: un panel ancho no pierde nada.
///
/// Solo cede lo que el usuario no ha tocado: una columna con `width` en su
/// spec no se compacta ni se oculta, una con `format` no se compacta, y un
/// nombre con ancho fijo desactiva la escalera entera. Las columnas `attr:`
/// y `plugin:` no están en la escalera: las pidió alguien a propósito.
#[must_use]
pub fn fitted_columns(
    settings: &ColumnsSettings,
    scheme: &str,
    inner_width: u16,
    name_wanted: u16,
    // El catálogo de atributos del provider conectado, si ya se sabe (spec
    // 2026-09-20). Solo decide una cosa: si se pinta la columna de permisos
    // que pone el propio listado.
    //
    // `None` —todavía no contestó— es NO. La columna la puso norte, así que
    // norte es quien tiene que no enseñarla vacía: una cabecera «Modo» sobre
    // doce celdas en blanco es exactamente el ancho de nombre que ADR 0124
    // vino a recuperar. Aparecer un fotograma tarde es barato; estar ahí sin
    // poder decir nada, no.
    //
    // Y no es hipotético: `file` en Windows no anuncia `posix.mode`, y ese
    // listado no lo anunciará nunca.
    catalog: Option<&norte_proto::AttrCatalog>,
) -> Vec<Fitted> {
    let nombre = ColumnId::Builtin(Builtin::Name);
    let techo = u16::try_from(u32::from(inner_width) * 3 / 5).unwrap_or(u16::MAX);
    let objetivo = name_wanted.min(techo).max(NAME_MIN);
    let libre = !settings.width_pinned(scheme, &nombre);
    let mut set = items_pintables(settings, scheme, catalog);
    let mut vivas = vec![true; set.len()];
    let mut cortas = vec![false; set.len()];
    let mut escalera = ESCALERA.iter();
    loop {
        let items: Vec<LayoutItem> = set
            .iter()
            .zip(&vivas)
            .filter(|(_, v)| **v)
            .map(|((_, it), _)| *it)
            .collect();
        let mut anchos = layout(inner_width, &items).into_iter();
        let out: Vec<Fitted> = set
            .iter()
            .enumerate()
            .filter(|(i, _)| vivas[*i])
            .filter_map(|(i, (id, _))| {
                anchos.next().flatten().map(|width| Fitted {
                    id: id.clone(),
                    width,
                    compact: cortas[i],
                })
            })
            .collect();
        let tiene = out.iter().find(|f| f.id == nombre).map_or(0, |f| f.width);
        if !libre || tiene >= objetivo {
            return out;
        }
        // El siguiente peldaño que se PUEDE dar; sin ninguno, esto es lo
        // mejor que cabe.
        loop {
            let Some(&(cede, peldano)) = escalera.next() else {
                return out;
            };
            // Un peldaño que apunta a una columna por defecto no se da si
            // este listado pinta las columnas que alguien escribió: allí esa
            // columna la pidió una persona, y ADR 0124 la deja quieta.
            if matches!(cede, Cede::PorDefecto(_)) && settings.has_user_columns(scheme) {
                continue;
            }
            let id = cede.id();
            let Some(i) = set.iter().position(|(c, _)| *c == id) else {
                continue;
            };
            if !vivas[i] || settings.width_pinned(scheme, &id) {
                continue;
            }
            match peldano {
                Peldano::Ocultar => vivas[i] = false,
                Peldano::Compactar => {
                    if cortas[i] || settings.format_pinned(scheme, &id) {
                        continue;
                    }
                    cortas[i] = true;
                    set[i].1.policy = WidthPolicy::Fixed(COMPACT_WIDTH);
                }
            }
            break;
        }
    }
}

/// El ancho que cubre al 80% de los nombres, a partir de un histograma de
/// anchos (`counts[w]` = cuántos nombres miden `w` celdas; el último cubo
/// cuenta también a los más anchos). El 80% y no el máximo: un solo nombre
/// kilométrico no debe dejar al resto del listado sin fecha.
#[must_use]
pub fn name_width_p80(counts: &[u32]) -> u16 {
    let total: u64 = counts.iter().map(|c| u64::from(*c)).sum();
    if total == 0 {
        return 0;
    }
    let meta = (total * 4).div_ceil(5);
    let mut acumulado = 0u64;
    for (w, c) in counts.iter().enumerate() {
        acumulado += u64::from(*c);
        if acumulado >= meta {
            return u16::try_from(w).unwrap_or(u16::MAX);
        }
    }
    u16::try_from(counts.len().saturating_sub(1)).unwrap_or(u16::MAX)
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

/// [`sort_column`] para cualquier id: attr/plugin no son ordenables (el
/// vocabulario de sort es cerrado: name/size/mtime — spec Layer 7 nota
/// #117).
#[must_use]
pub fn sort_column_id(id: &ColumnId) -> Option<crate::sort::SortColumn> {
    match id {
        ColumnId::Builtin(b) => sort_column(*b),
        ColumnId::Attr(_) | ColumnId::Plugin { .. } => None,
    }
}

/// Texto de la celda de una columna no-nombre (#108 L5, #117 sobre
/// [`ColumnId`]) con un [`ColumnStyle`] resuelto (7b): `None` = ausencia
/// (un dir sin size, un attr que el provider no mandó) — se pinta blanco,
/// jamás un `0` fabricado. `now_ms` lo inyecta el caller (estabilidad de
/// snapshots y pureza). Los `plugin:` devuelven `None` AQUÍ a propósito:
/// sus valores no viven en la `Entry` sino en el side-map del pane
/// (`PaneState::plugin_cell`) — el render los resuelve por ese camino.
#[must_use]
pub fn styled_cell(
    entry: &norte_proto::Entry,
    col: &ColumnId,
    now_ms: i64,
    style: &ColumnStyle,
) -> Option<String> {
    styled_cell_in(entry, col, now_ms, style, norte_i18n::active())
}

/// [`styled_cell`] en un idioma DADO.
///
/// Tres de sus celdas traducen —la clase, un booleano y la fecha relativa— y
/// las tres salían en el idioma del PROCESO cuando quien pintaba era la
/// ventana: media pantalla en cada idioma es peor que ninguna traducción.
#[must_use]
pub fn styled_cell_in(
    entry: &norte_proto::Entry,
    col: &ColumnId,
    now_ms: i64,
    style: &ColumnStyle,
    lang: norte_i18n::Lang,
) -> Option<String> {
    match col {
        ColumnId::Builtin(b) => match b {
            Builtin::Name => None, // el nombre lo pinta el frontend
            Builtin::Kind => Some(norte_i18n::t_in(
                lang,
                match entry.kind {
                    norte_proto::EntryKind::Dir => "col-kind-dir",
                    norte_proto::EntryKind::File => "col-kind-file",
                    norte_proto::EntryKind::Symlink => "col-kind-symlink",
                    norte_proto::EntryKind::Other => "col-kind-other",
                },
            )),
            Builtin::Size => entry.size.map(|n| format_size(n, style.size_format)),
            Builtin::Mtime => entry
                .mtime_ms
                .map(|ms| format_mtime_in(ms, style.time_format, now_ms, lang)),
        },
        ColumnId::Attr(id) => entry
            .attrs
            .get(id)
            .and_then(|v| attr_cell(v, style, now_ms, lang)),
        ColumnId::Plugin { .. } => None,
    }
}

/// [`styled_cell`] con los defaults del builtin (iec/relative) — la firma
/// histórica pre-7b, conducta idéntica (pineada por los tests existentes).
#[must_use]
pub fn builtin_cell(entry: &norte_proto::Entry, col: Builtin, now_ms: i64) -> Option<String> {
    styled_cell(
        entry,
        &ColumnId::Builtin(col),
        now_ms,
        &ColumnStyle::default_for(col),
    )
}

/// Celda de un valor attr (#117): el TAG del valor decide (ADR 0039 §1 —
/// jamás coaccionado al tipo declarado); el hint del estilo refina los
/// numéricos. Text/Bytes son de TERCEROS: enmascarados y capados por
/// [`sanitize_cell`]; Bytes pasa antes por el lossy MARCADO de
/// [`crate::display_name`] (regla 1: los bytes originales no se tocan).
fn attr_cell(
    v: &norte_proto::AttrValue,
    style: &ColumnStyle,
    now_ms: i64,
    lang: norte_i18n::Lang,
) -> Option<String> {
    use norte_proto::attrs::{AttrHint, AttrValue};
    match v {
        AttrValue::Uint(n) => Some(match style.hint {
            AttrHint::Size => format_size(*n, style.size_format),
            AttrHint::Mode => format_mode(*n, style.mode_format),
            AttrHint::Timestamp => i64::try_from(*n).map_or_else(
                |_| n.to_string(),
                |ms| format_mtime(ms, style.time_format, now_ms),
            ),
            _ => n.to_string(),
        }),
        AttrValue::Int(i) => Some(match style.hint {
            AttrHint::Timestamp => format_mtime(*i, style.time_format, now_ms),
            _ => i.to_string(),
        }),
        AttrValue::TimeMs(ms) => Some(format_mtime(*ms, style.time_format, now_ms)),
        // Presente-pero-impintable (enmascara a vacío) = «?» visible: el
        // blanco queda RESERVADO para AUSENTE (#117 review).
        AttrValue::Text(s) => sanitize_cell(Some(s)).or_else(|| Some("?".to_owned())),
        AttrValue::Bytes(b) => {
            let (shown, _hostil) = crate::display_name(b);
            sanitize_cell(Some(&shown)).or_else(|| Some("?".to_owned()))
        }
        AttrValue::Bool(b) => Some(norte_i18n::t_in(
            lang,
            if *b { "col-cell-yes" } else { "col-cell-no" },
        )),
        // Una celda mala cuesta una celda: visible, jamás blanco (blanco =
        // AUSENTE).
        AttrValue::Unknown => Some("?".to_owned()),
    }
}

/// Modo POSIX según formato. Un valor que no cabe en u32 no es un modo:
/// decimal crudo, jamás un panic ni un truncado silencioso.
fn format_mode(n: u64, fmt: ModeFormat) -> String {
    match u32::try_from(n) {
        Ok(m) => match fmt {
            ModeFormat::Rwx => format_mode_rwx(m),
            ModeFormat::Octal => format_mode_octal(m),
        },
        Err(_) => n.to_string(),
    }
}

/// Etiqueta de cabecera de CUALQUIER columna (#117), compartida TUI/GUI:
/// el `header` custom del spec gana (YA saneado al resolver); builtin →
/// Fluent; attr → Fluent por id de primera parte (`col-attr-posix-mode`),
/// si no el label del catálogo ENMASCARADO, si no el id saneado. `t()`
/// devuelve la clave cuando falta: se detecta comparando.
#[must_use]
pub fn header_label(
    id: &ColumnId,
    style: &ColumnStyle,
    catalog: Option<&norte_proto::AttrCatalog>,
) -> String {
    header_label_in(id, style, catalog, norte_i18n::active())
}

/// La cabecera de una columna en un idioma CONCRETO.
///
/// Existe porque quien guarda el idioma en un campo —el host de la ventana,
/// que lo recibe al arrancar— no puede usar el global: [`norte_i18n::t`] lee
/// la negociación del proceso, así que la mitad fija de la hoja de atributos
/// salía en el idioma pedido y las cabeceras de los atributos en el del
/// sistema, en la misma pantalla.
///
/// [`header_label`] es esto con el idioma global, que es lo que quiere un
/// terminal: ahí los dos coinciden siempre.
#[must_use]
pub fn header_label_in(
    id: &ColumnId,
    style: &ColumnStyle,
    catalog: Option<&norte_proto::AttrCatalog>,
    lang: norte_i18n::Lang,
) -> String {
    if let Some(h) = &style.header {
        return h.clone();
    }
    match id {
        ColumnId::Builtin(b) => norte_i18n::t_in(
            lang,
            match b {
                Builtin::Name => "col-header-name",
                Builtin::Size => "col-header-size",
                Builtin::Mtime => "col-header-mtime",
                Builtin::Kind => "col-header-kind",
            },
        ),
        ColumnId::Attr(aid) => {
            // #117 review: la clave Fluent solo se deriva para namespaces
            // de PRIMERA parte — un id de provider como `posix-mode`
            // aplanaría al MISMO `col-attr-posix-mode` y robaría la
            // traducción de `posix.mode`.
            const FIRST_PARTY: &[&str] = &["posix.", "win.", "s3.", "archive."];
            if FIRST_PARTY.iter().any(|ns| aid.starts_with(ns)) {
                let key = format!("col-attr-{}", aid.replace(['.', '_'], "-"));
                let loc = norte_i18n::t_in(lang, &key);
                if loc != key {
                    return loc;
                }
            }
            if let Some(info) = catalog.and_then(|c| c.iter().find(|i| i.id == *aid)) {
                let sane: String = sanitize_header(&info.label)
                    .chars()
                    .take(HEADER_MAX_CHARS)
                    .collect();
                if !sane.is_empty() {
                    return sane;
                }
            }
            sanitize_header(aid)
                .chars()
                .take(HEADER_MAX_CHARS)
                .collect()
        }
        ColumnId::Plugin { plugin, column } => sanitize_header(&format!("{plugin}/{column}"))
            .chars()
            .take(HEADER_MAX_CHARS)
            .collect(),
    }
}

#[cfg(test)]
mod style_tests {
    use super::*;

    /// El rótulo del MANIFIESTO de una columna de plugin se usa en la
    /// cabecera; `[ui.columns] header` sigue ganándole, y sin ninguno de los
    /// dos queda el id, que es lo que se enseñaba siempre.
    #[test]
    fn el_rotulo_del_manifiesto_nombra_una_columna_de_plugin() {
        let id: ColumnId = "plugin:acme.git/status".parse().expect("id de plugin");
        let mut s = ColumnsSettings::resolve(&norte_config::ColumnsConfig::default());
        let rotulo = |s: &ColumnsSettings| {
            header_label_in(
                &id,
                &s.style_for_id("file", &id, None),
                None,
                norte_i18n::Lang::Es,
            )
        };
        assert_eq!(rotulo(&s), "acme.git/status", "sin catálogo, el id");

        assert!(
            s.apply_plugin_headers([("plugin:acme.git/status".to_owned(), "Estado".to_owned())])
        );
        assert_eq!(rotulo(&s), "Estado");
        assert!(
            !s.apply_plugin_headers([("plugin:acme.git/status".to_owned(), "Estado".to_owned())]),
            "el mismo rótulo otra vez no es un cambio"
        );

        // El del usuario manda sobre el del manifiesto.
        let mut cfg = norte_config::ColumnsConfig::default();
        cfg.specs.insert(
            "plugin:acme.git/status".to_owned(),
            norte_config::ColumnSpec {
                header: Some("Git".to_owned()),
                ..Default::default()
            },
        );
        let mut con_spec = ColumnsSettings::resolve(&cfg);
        con_spec.apply_plugin_headers([("plugin:acme.git/status".to_owned(), "Estado".to_owned())]);
        assert_eq!(rotulo(&con_spec), "Git");
    }

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
            .find(|(id, _)| *id == ColumnId::Builtin(Builtin::Size))
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
            styled_cell(&e, &ColumnId::Builtin(Builtin::Size), 0, &styled).as_deref(),
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

    /// #108 7b: `apply_format` actualiza el spec retenido y `style_for` lo
    /// ve al instante (lockstep sesión↔disco del picker); una entrada
    /// existente conserva sus otros campos (header).
    #[test]
    fn apply_format_actualiza_el_estilo_en_sesion() {
        let cfg = norte_config::ColumnsConfig {
            specs: [(
                "size".to_owned(),
                norte_config::ColumnSpec {
                    header: Some("Peso".to_owned()),
                    ..Default::default()
                },
            )]
            .into(),
            ..Default::default()
        };
        let mut s = ColumnsSettings::resolve(&cfg);
        assert_eq!(
            s.style_for("file", Builtin::Size).size_format,
            SizeFormat::Iec
        );
        s.apply_format("size", "si");
        let estilo = s.style_for("file", Builtin::Size);
        assert_eq!(estilo.size_format, SizeFormat::Si, "visible al instante");
        assert_eq!(
            estilo.header.as_deref(),
            Some("Peso"),
            "los otros campos del spec sobreviven"
        );
        // Sin entrada previa: se crea.
        s.apply_format("mtime", "iso");
        assert_eq!(
            s.style_for("file", Builtin::Mtime).time_format,
            TimeFormat::Iso
        );
    }

    /// V2 (spec 2026-09-11): `apply_width` fija la política en sesión, se
    /// acota al rango del loader, y conserva los otros campos del spec.
    #[test]
    fn apply_width_fija_la_politica_en_sesion_y_acota() {
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
        let mut s = ColumnsSettings::resolve(&cfg);
        let politica = |s: &ColumnsSettings| {
            s.layout_items_for("file")
                .into_iter()
                .find(|(id, _)| *id == ColumnId::Builtin(Builtin::Size))
                .map(|(_, item)| item.policy)
        };
        assert_ne!(politica(&s), Some(WidthPolicy::Fixed(12)));
        assert_eq!(s.apply_width("size", 12), 12);
        assert_eq!(politica(&s), Some(WidthPolicy::Fixed(12)));
        assert_eq!(
            s.style_for("file", Builtin::Size).size_format,
            SizeFormat::Si,
            "el formato del spec sobrevive"
        );
        assert_eq!(s.apply_width("size", 0), 1, "suelo del loader");
        assert_eq!(s.apply_width("size", 900), 64, "techo del loader");
        assert_eq!(politica(&s), Some(WidthPolicy::Fixed(64)));
    }

    /// m3 revisión 7b: las cadenas de las tablas del frontend son
    /// SUBCONJUNTO del vocabulario global que acepta la config
    /// (`norte-config/src/load.rs`, `parse del spec`: `"exact" | "iec" |
    /// "si" | "relative" | "iso" | "octal" | "rwx"` — hardcodeado aquí
    /// porque config no puede depender del frontend para compartir la
    /// const). Un nombre nuevo en la tabla sin su lado config sería un
    /// spec imposible de escribir.
    #[test]
    fn la_tabla_de_formatos_es_subconjunto_del_vocabulario_de_config() {
        let config_vocab = [
            "exact", "iec", "si", "relative", "iso", "smart", "octal", "rwx",
        ];
        for (s, _) in SIZE_FORMATS {
            assert!(config_vocab.contains(s), "{s} no está en config");
        }
        for (s, _) in TIME_FORMATS {
            assert!(config_vocab.contains(s), "{s} no está en config");
        }
        for (s, _) in MODE_FORMATS {
            assert!(config_vocab.contains(s), "{s} no está en config");
        }
        // Y la dirección enum→str cubre TODO valor de los enums (un enum
        // nuevo sin fila en la tabla rompería el seed del picker).
        for f in [SizeFormat::Exact, SizeFormat::Iec, SizeFormat::Si] {
            let style = ColumnStyle {
                size_format: f,
                ..ColumnStyle::default_for(Builtin::Size)
            };
            assert!(format_name(Builtin::Size, &style).is_some(), "{f:?}");
        }
        for f in [TimeFormat::Relative, TimeFormat::Iso, TimeFormat::Smart] {
            let style = ColumnStyle {
                time_format: f,
                ..ColumnStyle::default_for(Builtin::Mtime)
            };
            assert!(format_name(Builtin::Mtime, &style).is_some(), "{f:?}");
        }
        // #117: la dirección enum→str de Mode va por el hint DEL estilo
        // (attrs).
        let mode_id = ColumnId::Attr("posix.mode".into());
        for f in [ModeFormat::Rwx, ModeFormat::Octal] {
            let style = ColumnStyle {
                mode_format: f,
                hint: norte_proto::attrs::AttrHint::Mode,
                ..ColumnStyle::default_for_id(&mode_id, None)
            };
            assert!(format_name_id(&mode_id, &style).is_some(), "{f:?}");
        }
    }

    /// #117 review tarea 4: las TRES tablas de formato son DISJUNTAS entre
    /// sí. El pliegue de `style_for_id` busca la palabra en size→time→mode
    /// y asigna al primer campo que case: una palabra repetida en dos
    /// tablas escribiría el campo equivocado en silencio.
    #[test]
    fn las_tablas_de_formatos_no_comparten_palabras() {
        let todas: Vec<&str> = SIZE_FORMATS
            .iter()
            .map(|(s, _)| *s)
            .chain(TIME_FORMATS.iter().map(|(s, _)| *s))
            .chain(MODE_FORMATS.iter().map(|(s, _)| *s))
            .collect();
        let unicas: std::collections::BTreeSet<&str> = todas.iter().copied().collect();
        assert_eq!(unicas.len(), todas.len(), "palabra duplicada: {todas:?}");
    }
}

#[cfg(test)]
mod attr_funnel_tests {
    use super::*;
    use norte_proto::attrs::{AttrHint, AttrInfo, AttrType, AttrValue};

    fn catalog() -> norte_proto::AttrCatalog {
        norte_proto::AttrCatalog::new(vec![
            AttrInfo {
                id: "mem.mode".into(),
                label: "Mode".into(),
                ty: AttrType::Uint,
                hint: AttrHint::Mode,
            },
            AttrInfo {
                id: "mem.owner".into(),
                label: "Owner\u{202e}evil".into(),
                ty: AttrType::Bytes,
                hint: AttrHint::Identity,
            },
        ])
    }

    fn entry_with(attrs: &[(&str, AttrValue)]) -> norte_proto::Entry {
        let mut e = norte_proto::Entry {
            attrs: std::collections::BTreeMap::new(),
            path: norte_proto::VPath::parse("mem:///a.bin").unwrap(),
            kind: norte_proto::EntryKind::File,
            size: Some(1),
            mtime_ms: Some(0),
        };
        for (k, v) in attrs {
            e.attrs.insert((*k).to_owned(), v.clone());
        }
        e
    }

    #[test]
    fn layout_items_for_incluye_attrs_y_deduplica() {
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec![
                "name".into(),
                "attr:mem.mode".into(),
                "attr:mem.mode".into(), // dup: una sola columna
                "size".into(),
            ]),
            ..Default::default()
        };
        let st = ColumnsSettings::resolve(&cfg);
        let items = st.layout_items_for("file");
        let ids: Vec<String> = items.iter().map(|(id, _)| id.to_string()).collect();
        assert_eq!(ids, vec!["name", "attr:mem.mode", "size"]);
        // attr default: Fixed(12), no-nombre.
        let attr = &items[1].1;
        assert_eq!(attr.policy, WidthPolicy::Fixed(12));
        assert!(!attr.is_name);
    }

    #[test]
    fn attr_ids_for_devuelve_los_configurados_del_scheme() {
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec!["name".into(), "attr:mem.mode".into()]),
            ..Default::default()
        };
        let st = ColumnsSettings::resolve(&cfg);
        assert_eq!(st.attr_ids_for("file"), vec!["mem.mode".to_owned()]);
        // Sin configurar, el único attr que se pide es el modo POSIX, y solo
        // donde hay permisos (spec 2026-09-20): la columna la pone el propio
        // listado, así que el listado también paga su hueco del `fs.list`.
        let st2 = ColumnsSettings::resolve(&norte_config::ColumnsConfig::default());
        assert_eq!(st2.attr_ids_for("file"), vec![POSIX_MODE_ATTR.to_owned()]);
        assert_eq!(st2.attr_ids_for("sftp"), vec![POSIX_MODE_ATTR.to_owned()]);
        // Y donde no los hay no se pide nada: una columna que el backend no
        // sabe contestar es ancho del nombre gastado en un hueco en blanco.
        assert!(st2.attr_ids_for("s3").is_empty());
        assert!(st2.attr_ids_for("zip").is_empty());
    }

    /// La columna de permisos que pone norte CEDE sitio al nombre, y la que
    /// pide el usuario no (spec 2026-09-20, enmienda de ADR 0124).
    ///
    /// Es la diferencia entera entre las dos: la escalera existe para que un
    /// panel estrecho siga dejando leer el nombre, y una columna que nadie
    /// pidió no puede ser la que lo impida. Una pedida a mano sí, porque
    /// quitarla sería desobedecer.
    /// Un catálogo que SÍ anuncia el modo POSIX, como el del provider local.
    fn catalogo_con_modo() -> norte_proto::AttrCatalog {
        use norte_proto::attrs::{AttrHint, AttrInfo, AttrType};
        norte_proto::AttrCatalog::new(vec![AttrInfo {
            id: POSIX_MODE_ATTR.into(),
            label: "Mode".into(),
            ty: AttrType::Uint,
            hint: AttrHint::Mode,
        }])
    }

    #[test]
    fn los_permisos_por_defecto_ceden_y_los_pedidos_no() {
        let modo = ColumnId::Attr(POSIX_MODE_ATTR.to_owned());
        let cat = catalogo_con_modo();
        // Por defecto: en un panel ancho está, y en uno estrecho se va.
        let st = ColumnsSettings::resolve(&norte_config::ColumnsConfig::default());
        let ancho = fitted_columns(&st, "file", 100, 30, Some(&cat));
        assert!(ancho.iter().any(|f| f.id == modo), "cabe: se enseña");
        let estrecho = fitted_columns(&st, "file", 40, 30, Some(&cat));
        assert!(
            !estrecho.iter().any(|f| f.id == modo),
            "no cabe: la primera que cede es la que nadie pidió"
        );
        // Pedida a mano: se queda, aunque apriete.
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec![
                "name".into(),
                "size".into(),
                "mtime".into(),
                format!("attr:{POSIX_MODE_ATTR}"),
            ]),
            ..Default::default()
        };
        let suyo = ColumnsSettings::resolve(&cfg);
        let estrecho = fitted_columns(&suyo, "file", 40, 30, Some(&cat));
        assert!(
            estrecho.iter().any(|f| f.id == modo),
            "la pidió alguien a propósito (ADR 0124)"
        );
    }

    /// Un backend que no anuncia permisos no gasta ancho del nombre en una
    /// columna «Modo» en blanco — ni siquiera mientras no ha contestado.
    ///
    /// Es la mitad honesta del automatismo: poner la columna por el scheme es
    /// una apuesta, y `file` en Windows la pierde siempre.
    #[test]
    fn sin_catalogo_o_sin_modo_no_se_pone_la_columna() {
        let modo = ColumnId::Attr(POSIX_MODE_ATTR.to_owned());
        let st = ColumnsSettings::resolve(&norte_config::ColumnsConfig::default());
        // Todavía no contestó.
        let f = fitted_columns(&st, "file", 100, 30, None);
        assert!(!f.iter().any(|x| x.id == modo));
        // Contestó, y no tiene permisos POSIX.
        let vacio = norte_proto::AttrCatalog::new(vec![]);
        let f = fitted_columns(&st, "file", 100, 30, Some(&vacio));
        assert!(!f.iter().any(|x| x.id == modo));
        // Pero si la pidió el usuario, se queda: vacía es SU respuesta.
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec!["name".into(), format!("attr:{POSIX_MODE_ATTR}")]),
            ..Default::default()
        };
        let suyo = ColumnsSettings::resolve(&cfg);
        let f = fitted_columns(&suyo, "file", 100, 30, Some(&vacio));
        assert!(f.iter().any(|x| x.id == modo));
    }

    /// #117 encoding-audit M1: un `attr:` que parsea pero no es un id
    /// LEGAL del wire (`is_valid_attr_id` — aquí una typo de caja) ni se
    /// pinta ni se pide: pedido a un daemon sería -32602 y tumbaría el
    /// `fs.list` entero. Va al diagnóstico y la forma cruda se preserva
    /// para el picker.
    #[test]
    fn attr_id_no_wire_safe_ni_se_pinta_ni_se_pide() {
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec![
                "name".into(),
                "attr:Posix.Mode".into(),
                "attr:mem.mode".into(),
            ]),
            ..Default::default()
        };
        let st = ColumnsSettings::resolve(&cfg);
        assert_eq!(st.attr_ids_for("file"), vec!["mem.mode".to_owned()]);
        let pintadas: Vec<String> = st
            .layout_items_for("file")
            .iter()
            .map(|(id, _)| id.to_string())
            .collect();
        assert!(
            !pintadas.contains(&"attr:Posix.Mode".to_owned()),
            "{pintadas:?}"
        );
        assert_eq!(st.attrs_not_wire_safe, vec!["attr:Posix.Mode".to_owned()]);
        // No es `invalid` (parsea) y la forma cruda sigue para el picker.
        assert!(st.invalid.is_empty());
        assert!(
            st.raw_ids_for("file")
                .contains(&"attr:Posix.Mode".to_owned())
        );
    }

    /// #117 encoding-audit M1, corpus completo: un nombre hostil del corpus
    /// canónico configurado como `attr:<hostil>` jamás llega a `attr_ids_for`
    /// (el pane no lo pide) y siempre queda diagnosticado.
    ///
    /// **La regla la pone `is_valid_attr_id`, no la lista de fixtures.** El
    /// corpus dejó de ser hostil-en-todos-sus-bytes cuando #129 metió
    /// GEMELAS de plegado: `strasse.txt` entra por lo que significa AL LADO de
    /// `straße.txt`, y por sí solo es un nombre ASCII corriente y un id de attr
    /// perfectamente legal. Aceptarlo es lo correcto, así que el bucle mide
    /// contra el validador del wire en vez de suponer que ningún nombre del
    /// corpus pasa — que era cierto por accidente y dejó de serlo.
    #[test]
    fn corpus_hostil_como_attr_id_jamas_llega_al_wire() {
        let mut rechazados = 0;
        for fixture in norte_testkit::corpus::hostile_names() {
            // Solo los UTF-8: un id de columna es String de config.
            let Ok(name) = String::from_utf8(fixture.bytes.clone()) else {
                continue;
            };
            if norte_proto::attrs::is_valid_attr_id(&name) {
                // Gemela benigna (#129): el funnel la acepta, y debe.
                continue;
            }
            rechazados += 1;
            let id = format!("attr:{name}");
            let cfg = norte_config::ColumnsConfig {
                default_columns: Some(vec!["name".into(), id.clone()]),
                ..Default::default()
            };
            let st = ColumnsSettings::resolve(&cfg);
            assert!(
                st.attr_ids_for("file").is_empty(),
                "{} llegaría al wire",
                fixture.id
            );
            assert!(
                st.attrs_not_wire_safe.contains(&id),
                "{} sin diagnóstico",
                fixture.id
            );
        }
        // Y el bucle tiene que haber medido algo: un `continue` que se tragara
        // el corpus entero dejaría el test verde sin probar nada.
        assert!(
            rechazados >= 20,
            "solo {rechazados} nombres del corpus llegaron al funnel"
        );
    }

    /// Pin del invariante de `attr_fingerprint` (#117 review tarea 3): la
    /// huella va ORDENADA — reordenar columnas produce la MISMA huella (no
    /// re-lista), quitar/añadir un attr la cambia.
    #[test]
    fn attr_fingerprint_es_orden_estable() {
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec![
                "name".into(),
                "attr:mem.owner".into(),
                "attr:mem.mode".into(),
            ]),
            ..Default::default()
        };
        let st = ColumnsSettings::resolve(&cfg);
        let huella = st.attr_fingerprint("file");
        assert_eq!(huella, vec!["mem.mode".to_owned(), "mem.owner".to_owned()]);
        let reordenada = norte_config::ColumnsConfig {
            default_columns: Some(vec![
                "attr:mem.mode".into(),
                "name".into(),
                "attr:mem.owner".into(),
            ]),
            ..Default::default()
        };
        assert_eq!(
            ColumnsSettings::resolve(&reordenada).attr_fingerprint("file"),
            huella,
            "reorden = misma huella"
        );
    }

    /// #117-follow-up: los `plugin:` ya tienen renderer — entran al layout
    /// como los attrs (item por defecto, width override por spec) y dejan
    /// de ser diagnóstico. El campo `unrenderable` murió con ellos.
    #[test]
    fn plugin_entra_al_layout_como_columna() {
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec![
                "name".into(),
                "attr:posix.mode".into(),
                "plugin:git/branch".into(),
            ]),
            ..Default::default()
        };
        let st = ColumnsSettings::resolve(&cfg);
        let items = st.layout_items_for("file");
        assert!(
            items.iter().any(|(id, _)| matches!(
                id,
                ColumnId::Plugin { plugin, column } if plugin == "git" && column == "branch"
            )),
            "plugin:git/branch pintable: {items:?}"
        );
        // Petición: qué columnas de plugin pedir para el scheme, en forma
        // (plugin, columna) — espejo de `attr_ids_for`.
        assert_eq!(
            st.plugin_ids_for("file"),
            vec![("git".to_owned(), "branch".to_owned())]
        );
        // Huella orden-insensible, espejo de `attr_fingerprint`.
        let reordenada = norte_config::ColumnsConfig {
            default_columns: Some(vec![
                "plugin:git/branch".into(),
                "name".into(),
                "attr:posix.mode".into(),
            ]),
            ..Default::default()
        };
        assert_eq!(
            ColumnsSettings::resolve(&reordenada).plugin_fingerprint("file"),
            st.plugin_fingerprint("file"),
            "reorden = misma huella"
        );
    }

    /// #117-follow-up: cap de columnas de plugin por lista pintada
    /// ([`PLUGIN_COLUMNS_MAX_REQUEST`]) — pintado == pedido (cada columna
    /// es una RPC `plugin.column_values`); el resto es diagnóstico, jamás
    /// una columna permanentemente en blanco.
    #[test]
    fn plugin_over_cap_diagnosticado_y_no_pintado() {
        let mut ids = vec!["name".to_owned()];
        for i in 0..=PLUGIN_COLUMNS_MAX_REQUEST {
            ids.push(format!("plugin:p/c{i}"));
        }
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(ids),
            ..Default::default()
        };
        let st = ColumnsSettings::resolve(&cfg);
        let pintadas = st
            .layout_items_for("file")
            .iter()
            .filter(|(id, _)| matches!(id, ColumnId::Plugin { .. }))
            .count();
        assert_eq!(pintadas, PLUGIN_COLUMNS_MAX_REQUEST);
        assert_eq!(st.plugin_ids_for("file").len(), PLUGIN_COLUMNS_MAX_REQUEST);
        assert_eq!(
            st.plugins_over_cap,
            vec![format!("plugin:p/c{PLUGIN_COLUMNS_MAX_REQUEST}")],
            "el excedente se nombra, no se silencia"
        );
    }

    #[test]
    fn styled_cell_attr_por_tag_del_valor_con_hint() {
        let cat = catalog();
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec!["name".into(), "attr:mem.mode".into()]),
            ..Default::default()
        };
        let st = ColumnsSettings::resolve(&cfg);
        let id = ColumnId::Attr("mem.mode".into());
        let style = st.style_for_id("file", &id, Some(&cat));
        assert_eq!(style.hint, AttrHint::Mode);
        assert_eq!(style.align, Align::Right);
        let e = entry_with(&[("mem.mode", AttrValue::Uint(0o100_644))]);
        assert_eq!(
            styled_cell(&e, &id, 0, &style).as_deref(),
            Some("-rw-r--r--")
        );
        // Ausente → None (blanco), jamás un valor fabricado.
        let vacio = entry_with(&[]);
        assert_eq!(styled_cell(&vacio, &id, 0, &style), None);
        // Unknown → "?" (una celda mala cuesta una celda).
        let raro = entry_with(&[("mem.mode", AttrValue::Unknown)]);
        assert_eq!(styled_cell(&raro, &id, 0, &style).as_deref(), Some("?"));
    }

    /// #117 encoding-audit L3: un `Uint` que no cabe en i64 bajo hint
    /// Timestamp cae a decimal crudo — jamás un panic ni un tiempo
    /// fabricado por truncado.
    #[test]
    fn uint_desbordado_bajo_hint_timestamp_cae_a_decimal() {
        let id = ColumnId::Attr("mem.stamp".into());
        let style = ColumnStyle {
            hint: AttrHint::Timestamp,
            ..ColumnStyle::default_for_id(&id, None)
        };
        let e = entry_with(&[("mem.stamp", AttrValue::Uint(u64::MAX))]);
        assert_eq!(
            styled_cell(&e, &id, 0, &style).as_deref(),
            Some(u64::MAX.to_string().as_str())
        );
    }

    #[test]
    fn attr_text_y_bytes_hostiles_se_enmascaran() {
        let id = ColumnId::Attr("mem.note".into());
        let style = ColumnStyle::default_for_id(&id, None);
        let e = entry_with(&[(
            "mem.note",
            AttrValue::Text("\u{202e}atón\u{202c} a\u{200d}b".into()),
        )]);
        let cell = styled_cell(&e, &id, 0, &style).expect("celda");
        assert!(
            !cell.chars().any(norte_encoding::is_terminal_hazard),
            "{cell:?}"
        );
        let id2 = ColumnId::Attr("mem.owner".into());
        let e2 = entry_with(&[("mem.owner", AttrValue::Bytes(b"due\xf1o-\xff\xfe".to_vec()))]);
        let cell2 = styled_cell(&e2, &id2, 0, &style).expect("celda");
        assert!(
            !cell2.chars().any(norte_encoding::is_terminal_hazard),
            "{cell2:?}"
        );
        assert!(cell2.contains('\u{FFFD}'), "lossy marcado: {cell2:?}");
        // Presente-pero-impintable (enmascara/trunca a vacío) → «?» — el
        // blanco queda reservado para AUSENTE. Un valor todo-hostil NO
        // enmascara a vacío (cada hazard pasa a U+FFFD visible): el caso
        // vacío real es la cadena vacía presente.
        let vacio = entry_with(&[("mem.note", AttrValue::Text(String::new()))]);
        assert_eq!(styled_cell(&vacio, &id, 0, &style).as_deref(), Some("?"));
        let vacio2 = entry_with(&[("mem.owner", AttrValue::Bytes(Vec::new()))]);
        assert_eq!(styled_cell(&vacio2, &id2, 0, &style).as_deref(), Some("?"));
        let hostil = entry_with(&[("mem.note", AttrValue::Text("\u{202e}\u{200b}".into()))]);
        assert_eq!(
            styled_cell(&hostil, &id, 0, &style).as_deref(),
            Some("\u{FFFD}\u{FFFD}"),
            "todo-hostil = enmascarado VISIBLE, no vacío"
        );
    }

    #[test]
    fn header_label_fluent_catalogo_o_id_enmascarado() {
        let cat = catalog();
        // Primera-parte: clave Fluent (existe col-attr-posix-mode).
        let id = ColumnId::Attr("posix.mode".into());
        let style = ColumnStyle::default_for_id(&id, None);
        let h = header_label(&id, &style, None);
        assert_ne!(h, "col-attr-posix-mode", "clave Fluent debe existir");
        assert!(!h.starts_with("col-attr-"), "{h:?}");
        // Desconocido con catálogo: label del provider ENMASCARADO.
        let id2 = ColumnId::Attr("mem.owner".into());
        let h2 = header_label(
            &id2,
            &ColumnStyle::default_for_id(&id2, Some(&cat)),
            Some(&cat),
        );
        assert!(
            !h2.chars().any(norte_encoding::is_terminal_hazard),
            "{h2:?}"
        );
        // Desconocido sin catálogo: el id (charset seguro tras sanitize).
        let id3 = ColumnId::Attr("mem.stamp".into());
        let h3 = header_label(&id3, &ColumnStyle::default_for_id(&id3, None), None);
        assert_eq!(h3, "mem.stamp");
        // El header custom del spec GANA siempre.
        let mut st = ColumnStyle::default_for_id(&id3, None);
        st.header = Some("Custom".into());
        assert_eq!(header_label(&id3, &st, None), "Custom");
    }

    /// Audit F2 (#117-follow-up): un id `plugin:` HOSTIL de una config de
    /// capa de proyecto (RLO/ZWSP parsean — `from_str` acepta cualquier
    /// segmento no vacío) jamás llega crudo a la cabecera: el brazo Plugin
    /// de `header_label` enmascara — y ni TUI ni GUI re-enmascaran después
    /// (confían en este choke point; una regresión aquí desaparecería la
    /// línea de cabeceras entera en ratatui).
    #[test]
    fn header_label_plugin_enmascara_id_hostil() {
        let id: ColumnId = "plugin:e\u{202E}vil/c\u{200B}ol".parse().expect("parsea");
        let h = header_label(&id, &ColumnStyle::default_for_id(&id, None), None);
        assert!(
            !h.chars().any(norte_encoding::is_terminal_hazard),
            "hazard crudo en la cabecera: {h:?}"
        );
        assert!(h.contains('\u{FFFD}'), "enmascarado visible: {h:?}");
        assert!(h.contains("vil/c"), "el resto del id sobrevive: {h:?}");
    }

    #[test]
    fn attr_cell_todos_los_tags() {
        let opaco = ColumnStyle::default_for_id(&ColumnId::Attr("x.y".into()), None);
        let e = |v: AttrValue| entry_with(&[("x.y", v)]);
        let id = ColumnId::Attr("x.y".into());
        assert_eq!(
            styled_cell(&e(AttrValue::Uint(42)), &id, 0, &opaco).as_deref(),
            Some("42")
        );
        assert_eq!(
            styled_cell(&e(AttrValue::Int(-5)), &id, 0, &opaco).as_deref(),
            Some("-5")
        );
        assert_eq!(
            styled_cell(&e(AttrValue::Bool(true)), &id, 0, &opaco),
            Some(norte_i18n::t("col-cell-yes"))
        );
        // TimeMs siempre formatea como tiempo, con o sin hint.
        let t = styled_cell(&e(AttrValue::TimeMs(0)), &id, 60_000, &opaco).expect("celda");
        assert!(!t.is_empty());
    }

    /// #117 review: el formato `octal` del hint Mode y el fallback decimal
    /// de un word que no cabe en u32 (no es un modo — jamás un panic).
    #[test]
    fn modo_octal_y_desbordado_a_decimal() {
        let id = ColumnId::Attr("x.m".into());
        let mut style = ColumnStyle::default_for_id(&id, None);
        style.hint = AttrHint::Mode;
        style.mode_format = ModeFormat::Octal;
        let e = entry_with(&[("x.m", AttrValue::Uint(0o100_644))]);
        assert_eq!(styled_cell(&e, &id, 0, &style).as_deref(), Some("0644"));
        let gordo = u64::from(u32::MAX) + 1;
        let esperado = gordo.to_string();
        for fmt in [ModeFormat::Rwx, ModeFormat::Octal] {
            style.mode_format = fmt;
            let e2 = entry_with(&[("x.m", AttrValue::Uint(gordo))]);
            assert_eq!(
                styled_cell(&e2, &id, 0, &style).as_deref(),
                Some(esperado.as_str()),
                "{fmt:?}"
            );
        }
    }

    /// #117 review: attrs por encima de [`norte_proto::ATTRS_MAX_REQUEST`]
    /// ni se pintan ni se piden (pintado == pedido — sin columnas
    /// permanentemente en blanco) y quedan diagnosticados.
    #[test]
    fn attrs_sobre_el_cap_pintado_igual_a_pedido() {
        let mut ids: Vec<String> = vec!["name".into()];
        ids.extend((0..17).map(|i| format!("attr:mem.a{i:02}")));
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(ids),
            ..Default::default()
        };
        let st = ColumnsSettings::resolve(&cfg);
        let painted: Vec<String> = st
            .layout_items_for("file")
            .into_iter()
            .filter_map(|(id, _)| match id {
                ColumnId::Attr(a) => Some(a),
                _ => None,
            })
            .collect();
        assert_eq!(painted.len(), norte_proto::ATTRS_MAX_REQUEST);
        assert_eq!(painted, st.attr_ids_for("file"), "pintado == pedido");
        assert_eq!(st.attrs_over_cap, vec!["attr:mem.a16".to_owned()]);
    }
}

#[cfg(test)]
mod fit_tests {
    use super::*;

    fn con_clase() -> ColumnsSettings {
        ColumnsSettings::resolve(&norte_config::ColumnsConfig {
            default_columns: Some(
                ["name", "size", "mtime", "kind"]
                    .map(str::to_owned)
                    .to_vec(),
            ),
            ..Default::default()
        })
    }

    fn con_spec(id: &str, spec: norte_config::ColumnSpec) -> ColumnsSettings {
        let mut cfg = norte_config::ColumnsConfig {
            default_columns: Some(
                ["name", "size", "mtime", "kind"]
                    .map(str::to_owned)
                    .to_vec(),
            ),
            ..Default::default()
        };
        cfg.specs.insert(id.to_owned(), spec);
        ColumnsSettings::resolve(&cfg)
    }

    fn resumen(f: &[Fitted]) -> Vec<String> {
        f.iter()
            .map(|c| format!("{}{}", c.id, if c.compact { "~" } else { "" }))
            .collect()
    }

    fn nombre(f: &[Fitted]) -> u16 {
        f.iter()
            .find(|c| c.id == ColumnId::Builtin(Builtin::Name))
            .map_or(0, |c| c.width)
    }

    /// La captura que motivó esto: un panel de 46 celdas con clase, tamaño
    /// y fecha dejaba 14 al nombre. Con nombres de ~20, cede la clase —que
    /// ya dicen el icono y la `/`— y nada más.
    #[test]
    fn la_clase_cede_primero_y_basta() {
        let f = fitted_columns(&con_clase(), "file", 46, 20, None);
        assert_eq!(resumen(&f), ["name", "size", "mtime"]);
        assert!(nombre(&f) >= 20, "{f:?}");
    }

    /// Nombres largos (capturas de pantalla): tras la clase se compacta la
    /// fecha, y se para en cuanto el nombre llega a su objetivo acotado.
    #[test]
    fn nombres_largos_compactan_la_fecha() {
        let f = fitted_columns(&con_clase(), "file", 46, 40, None);
        assert_eq!(resumen(&f), ["name", "size", "mtime~"]);
        // Objetivo acotado a 3/5 de 46 = 27.
        assert!(nombre(&f) >= 27, "{f:?}");
    }

    /// Un panel ancho no pierde nada: la escalera solo se sube si hace falta.
    #[test]
    fn ancho_de_sobra_no_toca_nada() {
        let f = fitted_columns(&con_clase(), "file", 120, 30, None);
        assert_eq!(resumen(&f), ["name", "size", "mtime", "kind"]);
    }

    /// Una columna con ancho fijado por el usuario no se oculta ni se
    /// compacta; la escalera salta al peldaño siguiente.
    #[test]
    fn lo_que_el_usuario_fijo_no_cede() {
        let s = con_spec(
            "kind",
            norte_config::ColumnSpec {
                width: Some(norte_config::WidthChoice::Fixed(9)),
                ..Default::default()
            },
        );
        let f = fitted_columns(&s, "file", 46, 20, None);
        assert_eq!(resumen(&f), ["name", "size", "mtime~", "kind"]);
    }

    /// Un formato elegido tampoco se cambia por el corto.
    #[test]
    fn un_formato_elegido_no_se_compacta() {
        let s = con_spec(
            "mtime",
            norte_config::ColumnSpec {
                format: Some("iso".to_owned()),
                ..Default::default()
            },
        );
        let f = fitted_columns(&s, "file", 46, 40, None);
        assert_eq!(resumen(&f), ["name", "size~", "mtime"]);
    }

    /// Con el nombre de ancho fijo no hay escalera: es exactamente el
    /// reparto de siempre.
    #[test]
    fn nombre_fijo_desactiva_la_escalera() {
        let s = con_spec(
            "name",
            norte_config::ColumnSpec {
                width: Some(norte_config::WidthChoice::Fixed(12)),
                ..Default::default()
            },
        );
        let f = fitted_columns(&s, "file", 46, 40, None);
        let antes: Vec<_> = column_widths(&s, "file", 46, None)
            .into_iter()
            .map(|(id, width)| Fitted {
                id,
                width,
                compact: false,
            })
            .collect();
        assert_eq!(f, antes);
    }

    #[test]
    fn el_p80_ignora_la_cola_larga() {
        let mut counts = [0u32; 65];
        counts[8] = 8;
        counts[64] = 2;
        assert_eq!(name_width_p80(&counts), 8);
        assert_eq!(name_width_p80(&[0; 65]), 0);
    }

    #[test]
    fn tamanos_cortos_caben_en_cinco() {
        for (n, esperado) in [
            (0, "0B"),
            (1023, "1023B"),
            (1024, "1.0K"),
            (9 * 1024 + 900, "9.9K"),
            (81_715, "80K"),
            (1_023 * 1024 + 1000, "1.0M"),
            (1_363_149, "1.3M"),
            (u64::MAX, "16E"),
        ] {
            let s = format_size(n, SizeFormat::Short);
            assert_eq!(s, esperado, "{n}");
            assert!(s.chars().count() <= 5, "{s}");
        }
    }

    #[test]
    fn fechas_cortas_caben_en_cinco() {
        let tz = jiff::tz::TimeZone::UTC;
        let ahora = 1_789_000_000_000; // 2026-09-10 00:26 UTC
        let lang = norte_i18n::Lang::Es;
        let hoy = format_mtime_tz(ahora - 60_000, TimeFormat::Short, ahora, lang, &tz);
        let este_ano =
            format_mtime_tz(ahora - 40 * 86_400_000, TimeFormat::Short, ahora, lang, &tz);
        let antes = format_mtime_tz(
            ahora - 400 * 86_400_000,
            TimeFormat::Short,
            ahora,
            lang,
            &tz,
        );
        assert_eq!(hoy, "00:25");
        assert_eq!(este_ano, "08-01");
        assert_eq!(antes, "2025");
    }
}
