//! Valores EFECTIVOS de `[config]` (P2 decisión 3): fusiona los defaults del
//! esquema `[config]` del manifiesto con
//! `config_dir/plugins/<id>/config.toml` (FLAT `key = value`), validando
//! fail-closed cada clave presente contra el esquema. Un `config.toml`
//! ausente ⇒ todos los defaults. Un `[config]` ausente del manifiesto ⇒
//! mapa vacío (mismo criterio que [`Manifest::config`]).

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use crate::manifest::{CONFIG_STRING_MAX_CHARS, ConfigKeySpec, Manifest, is_valid_config_key};

/// Tope de tamaño de `config.toml` EN DISCO, comprobado ANTES de leerlo
/// (anti-DoS, mismo criterio que `MAX_ARTIFACT_BYTES` de `runtime.rs` para el
/// `.wasm`): un fichero de valores de usuario no tiene motivo legítimo para
/// pesar más que esto — como mucho unas pocas decenas de claves de a lo sumo
/// [`CONFIG_STRING_MAX_CHARS`] caracteres cada una. Por encima se rechaza
/// fail-loud sin llegar a `read_to_string`/`toml::from_str` (security review
/// P2 Task 4a).
pub const CONFIG_VALUES_MAX_BYTES: u64 = 64 * 1024;

/// Fallo al resolver los valores de `[config]` desde `config.toml` contra el
/// esquema del manifiesto. Cada variante que identifica una clave lleva la
/// CLAVE culpable; NINGUNA lleva el VALOR (issue #73): `config.toml` es de
/// usuario/plugin, no confiable, y el valor no debe llegar a logs/UI/mensajes
/// de error. La CLAVE en sí puede TAMBIÉN ser hostil (a diferencia de una
/// clave DECLARADA en el esquema, ya validada contra
/// `[a-z0-9-]{1,32}` en Task 1, la clave de un `config.toml` AJENO al
/// esquema —`UnknownKey`— es TOML de usuario sin esa garantía): ver el doc de
/// [`ConfigValueError::UnknownKey`] (security review P2 Task 4a, issue #73).
#[derive(Debug, thiserror::Error)]
pub enum ConfigValueError {
    /// `config.toml` no parsea como TOML. El mensaje es un resumen
    /// CONTENT-FREE (nunca la forma completa de `toml::de::Error`, que
    /// ECHOES el fragmento crudo de la línea ofensora de la fuente — un
    /// valor hostil o un secreto pegado por error NO debe llegar a
    /// logs/UI/wire; mismo criterio que `connections-parse` en
    /// `norte-cli::doctor`, security review P2 Task 4a).
    #[error("config.toml inválido: {summary}")]
    Toml {
        /// Resumen SIN fragmento de fuente (`toml::de::Error::message`, o su
        /// defecto la primera línea de su `Display`, que en esta versión de
        /// la crate `toml` es solo `"TOML parse error at line N, column M"`
        /// — posición, jamás contenido).
        summary: String,
        /// La causa real, para `Error::source()`/depuración interna — JAMÁS
        /// se interpola en el mensaje.
        #[source]
        source: toml::de::Error,
    },
    /// Error de I/O al leer `config.toml` DISTINTO de "no existe" (eso son
    /// defaults, no error — ver [`resolve_settings`]).
    #[error("config.toml ilegible: {0}")]
    Io(#[source] io::Error),
    /// `config.toml` EN DISCO supera [`CONFIG_VALUES_MAX_BYTES`]: se rechaza
    /// antes de leerlo por completo.
    #[error("config.toml demasiado grande: {len} bytes (máx {cap})")]
    TooLarge {
        /// Tamaño real en disco.
        len: u64,
        /// Tope permitido ([`CONFIG_VALUES_MAX_BYTES`]).
        cap: u64,
    },
    /// Una clave de `config.toml` no está declarada en `[config]` del
    /// manifiesto.
    ///
    /// `key` es la clave CRUDA — úsala para comparar programáticamente
    /// (tests, lógica), JAMÁS asumas que el mensaje la muestra: a diferencia
    /// de una clave DECLARADA en el esquema (validada contra
    /// `[a-z0-9-]{1,32}` al parsear el manifiesto, Task 1), esta clave viene
    /// de `config.toml`, que es TOML de USUARIO — una clave entrecomillada
    /// puede llevar CUALQUIER texto (controles, overrides bidi, kilométrica).
    /// El mensaje solo la interpola cuando ADEMÁS respeta ese mismo charset
    /// seguro; si no, queda genérico. Este mensaje cruza tanto a
    /// `norte doctor` como al WIRE (`PluginLoadError.reason` vía
    /// `plugin.list`, legible por un agente) — security review P2 Task 4a.
    #[error("clave desconocida en config.toml{}", display_key_suffix(key))]
    UnknownKey {
        /// La clave desconocida CRUDA.
        key: String,
    },
    /// El tipo TOML del valor no casa el tipo declarado en el esquema.
    /// Cubre también una tabla anidada bajo una clave escalar: `config.toml`
    /// es PLANO (decisión 3) — una tabla nunca es un tipo válido para
    /// `string`/`bool`/`int`/`enum`, así que no hace falta un caso aparte
    /// para "no aplanado": cae aquí de forma natural. `key` aquí SIEMPRE es
    /// una clave DECLARADA (ya pasó `manifest.config.get`), por tanto ya
    /// validada contra el charset seguro al parsear el manifiesto — segura de
    /// interpolar tal cual, a diferencia de [`Self::UnknownKey`].
    #[error("tipo TOML incorrecto para `{key}` (se esperaba {expected})")]
    WrongType {
        /// La clave con el tipo incorrecto (DECLARADA, charset seguro).
        key: String,
        /// El tipo TOML esperado (`string`/`bool`/`int`/`enum (string)`).
        expected: &'static str,
    },
    /// El valor `int` cae fuera de `[min, max]` declarado en el esquema.
    /// `key` es DECLARADA (ver el doc de [`Self::WrongType`]).
    #[error("`{key}` fuera del rango declarado")]
    IntOutOfRange {
        /// La clave fuera de rango.
        key: String,
    },
    /// El valor `enum` no está entre los `values` declarados en el esquema.
    /// `key` es DECLARADA (ver el doc de [`Self::WrongType`]).
    #[error("`{key}` no está entre los valores permitidos")]
    EnumNotMember {
        /// La clave con un valor no permitido.
        key: String,
    },
    /// El valor `string` supera [`CONFIG_STRING_MAX_CHARS`] caracteres —
    /// mismo tope que un `default` de tipo `string` en el esquema (decisión
    /// 1), aplicado TAMBIÉN a los overrides de `config.toml` (anti-DoS: sin
    /// este tope, un override arbitrariamente largo abultaría logs/UI/wire
    /// igual que un default largo ya estaba prohibido de hacer). `key` es
    /// DECLARADA (ver el doc de [`Self::WrongType`]).
    #[error("`{key}` (string) excede el tope de {CONFIG_STRING_MAX_CHARS} caracteres")]
    ValueTooLong {
        /// La clave con el valor demasiado largo.
        key: String,
    },
}

impl From<toml::de::Error> for ConfigValueError {
    fn from(source: toml::de::Error) -> Self {
        let summary = toml_error_summary(&source);
        ConfigValueError::Toml { summary, source }
    }
}

/// Extrae un resumen CONTENT-FREE de un fallo de parseo TOML (security review
/// P2 Task 4a): `toml::de::Error::message` ya es content-free (nunca incluye
/// el fragmento de fuente, solo la descripción corta — p. ej. "duplicate
/// key" o "invalid basic string, expected a closing quote"), pero por si una
/// versión futura de la crate lo dejara vacío en algún caso, el fallback es
/// la PRIMERA línea de su `Display`, que en esta versión es solo "TOML parse
/// error at line N, column M" (posición, jamás contenido — verificado
/// empíricamente contra varios fallos de parseo, incluida una cadena sin
/// cerrar).
fn toml_error_summary(e: &toml::de::Error) -> String {
    let msg = e.message();
    if msg.is_empty() {
        e.to_string()
            .lines()
            .next()
            .unwrap_or("TOML parse error")
            .to_string()
    } else {
        msg.to_string()
    }
}

/// Sufijo de display para [`ConfigValueError::UnknownKey`]: cadena vacía si
/// `key` NO respeta el charset seguro de mostrar (`[a-z0-9-]{1,32}`, ver
/// [`is_valid_config_key`]); si lo respeta, un sufijo con la clave
/// entrecomillada.
fn display_key_suffix(key: &str) -> String {
    if is_valid_config_key(key) {
        format!(": `{key}`")
    } else {
        String::new()
    }
}

/// Codifica el DEFAULT de `spec` a su forma canónica de string (decisión 4,
/// la misma que consume el WIT `host-config` de Task 3): `bool` →
/// `"true"`/`"false"`; `int` → decimal; `string`/`enum` → el texto crudo.
fn default_string(spec: &ConfigKeySpec) -> String {
    match spec {
        ConfigKeySpec::String { default, .. } | ConfigKeySpec::Enum { default, .. } => {
            default.clone()
        }
        ConfigKeySpec::Bool { default, .. } => default.to_string(),
        ConfigKeySpec::Int { default, .. } => default.to_string(),
    }
}

/// Valida `value` (crudo de `config.toml`) contra `spec` y lo codifica a su
/// forma canónica de string. `key` viaja SOLO para nombrar el error (nunca
/// se interpola `value`, #73).
fn encode_override(
    key: &str,
    spec: &ConfigKeySpec,
    value: toml::Value,
) -> Result<String, ConfigValueError> {
    match spec {
        ConfigKeySpec::String { .. } => match value {
            toml::Value::String(s) => {
                // Mismo tope que un `default` de tipo string en el esquema
                // (decisión 1, security review P2 Task 4a): un override no
                // paga menos que un default.
                if s.chars().count() > CONFIG_STRING_MAX_CHARS {
                    return Err(ConfigValueError::ValueTooLong {
                        key: key.to_string(),
                    });
                }
                Ok(s)
            }
            _ => Err(ConfigValueError::WrongType {
                key: key.to_string(),
                expected: "string",
            }),
        },
        ConfigKeySpec::Bool { .. } => match value {
            toml::Value::Boolean(b) => Ok(b.to_string()),
            _ => Err(ConfigValueError::WrongType {
                key: key.to_string(),
                expected: "bool",
            }),
        },
        ConfigKeySpec::Int { min, max, .. } => match value {
            toml::Value::Integer(i) => {
                if min.is_some_and(|m| i < m) || max.is_some_and(|m| i > m) {
                    return Err(ConfigValueError::IntOutOfRange {
                        key: key.to_string(),
                    });
                }
                Ok(i.to_string())
            }
            _ => Err(ConfigValueError::WrongType {
                key: key.to_string(),
                expected: "int",
            }),
        },
        ConfigKeySpec::Enum { values, .. } => match value {
            toml::Value::String(s) => {
                if values.iter().any(|v| v == &s) {
                    Ok(s)
                } else {
                    Err(ConfigValueError::EnumNotMember {
                        key: key.to_string(),
                    })
                }
            }
            _ => Err(ConfigValueError::WrongType {
                key: key.to_string(),
                expected: "enum (string)",
            }),
        },
    }
}

/// Resuelve los valores EFECTIVOS de `[config]` para un plugin instalado en
/// `dir`: arranca de los DEFAULTS del esquema (`manifest.config`) y les
/// superpone `dir/config.toml` si existe, validando fail-closed cada clave
/// presente (P2 decisión 3). `dir` es el directorio del plugin
/// (`config_dir/plugins/<id>/`, mismo que [`crate::PluginEntry::dir`]), NO
/// `config_dir` en sí.
///
/// Un `config.toml` AUSENTE no es error: todos los defaults. Un `[config]`
/// vacío/ausente en el manifiesto ⇒ mapa vacío, con o sin `config.toml`
/// (nada que resolver: cualquier clave en el fichero sería `UnknownKey`).
///
/// # Errors
/// [`ConfigValueError`] si `config.toml` supera [`CONFIG_VALUES_MAX_BYTES`]
/// en disco, no parsea, declara una clave que el esquema no conoce, un valor
/// de tipo TOML incorrecto (incluida una tabla anidada bajo una clave
/// escalar: el fichero de valores es PLANO), un `int` fuera de `[min, max]`,
/// un `enum` fuera de `values`, o un `string` que supera
/// [`CONFIG_STRING_MAX_CHARS`] caracteres.
pub fn resolve_settings(
    manifest: &Manifest,
    dir: &Path,
) -> Result<BTreeMap<String, String>, ConfigValueError> {
    let mut out: BTreeMap<String, String> = manifest
        .config
        .iter()
        .map(|(key, spec)| (key.clone(), default_string(spec)))
        .collect();

    let path = dir.join("config.toml");
    // Tope de tamaño ANTES de leer (security review P2 Task 4a, anti-DoS —
    // mismo criterio que `check_artifact_size` de `runtime.rs` para el
    // `.wasm`): `metadata` es barata y no lee el contenido.
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(ConfigValueError::Io(e)),
    };
    if meta.len() > CONFIG_VALUES_MAX_BYTES {
        return Err(ConfigValueError::TooLarge {
            len: meta.len(),
            cap: CONFIG_VALUES_MAX_BYTES,
        });
    }
    let src = std::fs::read_to_string(&path).map_err(ConfigValueError::Io)?;
    let table: toml::Table = toml::from_str(&src)?;
    // `toml::Table` itera en orden de CLAVE (BTreeMap por dentro):
    // determinista, no depende del orden en el fichero.
    for (key, value) in table {
        let Some(spec) = manifest.config.get(&key) else {
            return Err(ConfigValueError::UnknownKey { key });
        };
        let encoded = encode_override(&key, spec, value)?;
        out.insert(key, encoded);
    }
    Ok(out)
}
