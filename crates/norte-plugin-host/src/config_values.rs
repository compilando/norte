//! Valores EFECTIVOS de `[config]` (P2 decisión 3): fusiona los defaults del
//! esquema `[config]` del manifiesto con
//! `config_dir/plugins/<id>/config.toml` (FLAT `key = value`), validando
//! fail-closed cada clave presente contra el esquema. Un `config.toml`
//! ausente ⇒ todos los defaults. Un `[config]` ausente del manifiesto ⇒
//! mapa vacío (mismo criterio que [`Manifest::config`]).

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use crate::manifest::{ConfigKeySpec, Manifest};

/// Fallo al resolver los valores de `[config]` desde `config.toml` contra el
/// esquema del manifiesto. Cada variante lleva la CLAVE culpable; NINGUNA
/// lleva el VALOR (issue #73): `config.toml` es de usuario/plugin, no
/// confiable, y el valor no debe llegar a logs/UI/mensajes de error.
#[derive(Debug, thiserror::Error)]
pub enum ConfigValueError {
    /// `config.toml` no parsea como TOML.
    #[error("config.toml inválido: {0}")]
    Toml(#[from] toml::de::Error),
    /// Error de I/O al leer `config.toml` DISTINTO de "no existe" (eso son
    /// defaults, no error — ver [`resolve_settings`]).
    #[error("config.toml ilegible: {0}")]
    Io(#[source] io::Error),
    /// Una clave de `config.toml` no está declarada en `[config]` del
    /// manifiesto.
    #[error("clave desconocida en config.toml: `{key}`")]
    UnknownKey {
        /// La clave desconocida.
        key: String,
    },
    /// El tipo TOML del valor no casa el tipo declarado en el esquema.
    /// Cubre también una tabla anidada bajo una clave escalar: `config.toml`
    /// es PLANO (decisión 3) — una tabla nunca es un tipo válido para
    /// `string`/`bool`/`int`/`enum`, así que no hace falta un caso aparte
    /// para "no aplanado": cae aquí de forma natural.
    #[error("tipo TOML incorrecto para `{key}` (se esperaba {expected})")]
    WrongType {
        /// La clave con el tipo incorrecto.
        key: String,
        /// El tipo TOML esperado (`string`/`bool`/`int`/`enum (string)`).
        expected: &'static str,
    },
    /// El valor `int` cae fuera de `[min, max]` declarado en el esquema.
    #[error("`{key}` fuera del rango declarado")]
    IntOutOfRange {
        /// La clave fuera de rango.
        key: String,
    },
    /// El valor `enum` no está entre los `values` declarados en el esquema.
    #[error("`{key}` no está entre los valores permitidos")]
    EnumNotMember {
        /// La clave con un valor no permitido.
        key: String,
    },
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
            toml::Value::String(s) => Ok(s),
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
/// [`ConfigValueError`] si `config.toml` no parsea, declara una clave que el
/// esquema no conoce, un valor de tipo TOML incorrecto (incluida una tabla
/// anidada bajo una clave escalar: el fichero de valores es PLANO), un `int`
/// fuera de `[min, max]`, o un `enum` fuera de `values`.
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
    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(ConfigValueError::Io(e)),
    };
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
