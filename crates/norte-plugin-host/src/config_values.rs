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

/// Valida un valor SUBMITTED POR EL WIRE (`plugin.set_config`, G3c) contra
/// `spec` y lo codifica a su forma canónica de string — la MISMA validación
/// que [`resolve_settings`] aplica a `config.toml` (fuente única, spec S2:
/// "validated against the schema BEFORE writing"), nunca una ruta paralela.
///
/// A diferencia de `config.toml` (TOML tipado: `toml::Value::Boolean`/
/// `Integer` nativos), el wire manda SIEMPRE un `String` (`PluginSetConfigParams::value`) —
/// esta función hace el parseo mínimo que el tipo declarado exige antes de
/// reusar `encode_override` (privada, mismo módulo): `bool` exige
/// EXACTAMENTE `"true"`/`"false"` (nada de `"1"`/`"yes"`, mismo rigor que
/// un `config.toml` con `clave = "true"` en vez de `clave = true` —
/// TAMBIÉN sería `WrongType` ahí), `int` exige decimal ASCII válido para
/// `i64`. `string`/`enum` pasan el texto tal cual (su propio
/// charset/longitud los valida `encode_override`).
///
/// # Errors
/// [`ConfigValueError::WrongType`] si `raw` no parsea al tipo de `spec`;
/// el resto de variantes de `encode_override` (rango, enum, longitud) tal
/// cual.
pub fn encode_wire_value(
    key: &str,
    spec: &ConfigKeySpec,
    raw: &str,
) -> Result<String, ConfigValueError> {
    let value = match spec {
        ConfigKeySpec::String { .. } | ConfigKeySpec::Enum { .. } => {
            toml::Value::String(raw.to_string())
        }
        ConfigKeySpec::Bool { .. } => match raw {
            "true" => toml::Value::Boolean(true),
            "false" => toml::Value::Boolean(false),
            _ => {
                return Err(ConfigValueError::WrongType {
                    key: key.to_string(),
                    expected: "bool",
                });
            }
        },
        ConfigKeySpec::Int { .. } => match raw.parse::<i64>() {
            Ok(i) => toml::Value::Integer(i),
            Err(_) => {
                return Err(ConfigValueError::WrongType {
                    key: key.to_string(),
                    expected: "int",
                });
            }
        },
    };
    encode_override(key, spec, value)
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

/// Revisión S, M5: ¿es seguro usar `id` como UN ÚNICO segmento de ruta
/// (`config_dir.join("plugins").join(id)`)? Rechaza vacío, `.`/`..`
/// EXACTOS, y cualquier separador de ruta embebido (`/`, y `\` por si el
/// mismo binario corriera en Windows algún día — `Path::join` en Unix trata
/// `\` como un carácter normal de nombre, pero un `plugin_id` con `\` sigue
/// siendo sospechoso ahí también). Defensa en profundidad BARATA: el
/// charset reverse-DNS del manifiesto (`manifest::is_valid_plugin_id`, que
/// corre al parsear un plugin aprobado) ya excluye todo esto en el camino
/// normal; este guard cubre al primitivo de escritura ante un caller futuro
/// que no repita esa validación.
#[must_use]
fn plugin_id_is_safe_path_segment(id: &str) -> bool {
    !id.is_empty() && id != "." && id != ".." && !id.contains(['/', '\\'])
}

/// Fija `key = value` en `config_dir/plugins/<plugin_id>/config.toml` (S2),
/// PRESERVANDO comentarios y formato (`toml_edit`) — mismo patrón que
/// `norte_config::persist_set`, adaptado a que este fichero es PLANO (P2
/// decisión 3: sin secciones anidadas, cada clave vive top-level). Crea el
/// directorio del plugin y el fichero si no existen.
///
/// `plugin_id` debe llegar YA validado por el caller (p. ej. el `id` de un
/// [`Manifest`] ya cargado, que pasó el charset reverse-DNS al parsear) —
/// esta función AÑADE un guard barato de defensa en profundidad (revisión S,
/// M5: `plugin_id_is_safe_path_segment`) porque `plugin_id` se usa DIRECTO
/// como segmento de ruta (`config_dir.join("plugins").join(plugin_id)`): sin
/// el guard, un `plugin_id` no confiable con `..`/un separador podría escapar
/// `plugins/` — el propio charset reverse-DNS del manifiesto ya lo impide
/// para un plugin APROBADO normalmente, pero este primitivo no debe confiar
/// ciegamente en que TODO caller futuro repita esa validación. Misma
/// responsabilidad que ya tiene cualquier caller de `PluginEntry::dir`.
///
/// `value` se escribe TAL CUAL como un string TOML (escapado por
/// `toml_edit`, jamás inyectado crudo) — validar `value` contra el tipo/rango
/// del esquema `[config]` del manifiesto (bool/int/enum, la misma validación
/// que hace el `encode_override` interno de este módulo) es responsabilidad
/// del CALLER, ANTES de llamar aquí (spec S2: "validated against the schema
/// BEFORE writing — never persist an invalid value"). Esta función es el
/// primitivo de escritura, no el validador: [`resolve_settings`] sigue
/// siendo la única fuente de verdad de lectura/validación, y ESA función
/// espera un `toml::Value` NATIVO (bool/int reales, no un string) para las
/// claves `bool`/`int` — un caller tipado debe escribir esos tipos con
/// `toml_edit` directamente si necesita ese round trip; este helper cubre el
/// caso plano `string`/`enum` (el único que S2 conecta de punta a punta).
///
/// # Errors
/// [`std::io::Error`] (`InvalidInput`) si `plugin_id` no pasa
/// `plugin_id_is_safe_path_segment`; (otro kind) si el `config.toml`
/// existente no parsea o falla el I/O.
pub fn persist_plugin_setting(
    config_dir: &Path,
    plugin_id: &str,
    key: &str,
    value: &str,
) -> io::Result<std::path::PathBuf> {
    persist_plugin_setting_item(config_dir, plugin_id, key, toml_edit::value(value))
}

/// Como [`persist_plugin_setting`] pero escribe `raw` con el tipo TOML NATIVO
/// que `spec` declara (bool → booleano TOML, int → entero TOML, string/enum →
/// string TOML) en vez de siempre string (G3c, `plugin.set_config`).
/// Necesario porque una re-lectura posterior vía [`resolve_settings`] espera
/// el mismo tipado NATIVO que `config.toml` de un humano produciría — un
/// `clave = "true"` (string) para una clave `bool` fallaría ahí con
/// `WrongType`, exactamente igual que si un humano lo hubiera escrito a mano.
///
/// `raw` DEBE llegar ya validado (el caller llama primero a
/// [`encode_wire_value`], que es exactamente lo que hace
/// `PluginRegistry::set_config`) — esta función no revalida el rango/enum,
/// solo tipa. Un `raw` que no parsea al tipo de `spec` (violación del
/// invariante del caller) es [`io::ErrorKind::InvalidInput`], nunca un panic.
///
/// # Errors
/// Igual que [`persist_plugin_setting`], más [`io::ErrorKind::InvalidInput`]
/// si `raw` no parsea al tipo NATIVO de `spec` (invariante del caller roto).
pub fn persist_plugin_setting_typed(
    config_dir: &Path,
    plugin_id: &str,
    key: &str,
    spec: &ConfigKeySpec,
    raw: &str,
) -> io::Result<std::path::PathBuf> {
    use std::io::{Error, ErrorKind};
    let item = match spec {
        ConfigKeySpec::String { .. } | ConfigKeySpec::Enum { .. } => toml_edit::value(raw),
        ConfigKeySpec::Bool { .. } => match raw {
            "true" => toml_edit::value(true),
            "false" => toml_edit::value(false),
            _ => {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "raw bool inválido (debió validarse antes con encode_wire_value)",
                ));
            }
        },
        ConfigKeySpec::Int { .. } => {
            let i: i64 = raw.parse().map_err(|_| {
                Error::new(
                    ErrorKind::InvalidInput,
                    "raw int inválido (debió validarse antes con encode_wire_value)",
                )
            })?;
            toml_edit::value(i)
        }
    };
    persist_plugin_setting_item(config_dir, plugin_id, key, item)
}

/// Primitivo compartido de [`persist_plugin_setting`]/
/// [`persist_plugin_setting_typed`]: valida `plugin_id` como segmento de
/// ruta, crea el directorio si falta, parsea (o crea) `config.toml` y fija
/// `key` al `item` YA construido, preservando comentarios/formato.
fn persist_plugin_setting_item(
    config_dir: &Path,
    plugin_id: &str,
    key: &str,
    item: toml_edit::Item,
) -> io::Result<std::path::PathBuf> {
    use std::io::{Error, ErrorKind};
    if !plugin_id_is_safe_path_segment(plugin_id) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "plugin_id inválido como segmento de ruta",
        ));
    }
    let dir = config_dir.join("plugins").join(plugin_id);
    std::fs::create_dir_all(&dir)?;
    // #119 (plantilla #116, `norte-config::load`): lock advisory
    // cross-process sobre el hermano DEDICADO `config.toml.lock`, tomado
    // ANTES de leer — el RMW entero es la sección crítica (GUI+TUI sobre el
    // mismo plugin). Apertura SIN truncar (#116 MAJOR-1: `CREATE_ALWAYS`
    // sobre un lock `LockFileEx` ajeno puede FALLAR en Windows en vez de
    // esperar). El SO libera al morir el proceso — sin locks rancios.
    // Duplicado deliberado de los helpers de norte-config (crates
    // desacoplados; misma doctrina que el fold core/frontend).
    let lock_file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("config.toml.lock"))?;
    lock_file.lock()?;
    let _lock = lock_file; // vivo hasta el final del RMW; drop = unlock
    let path = dir.join("config.toml");
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(s) => s.parse::<toml_edit::DocumentMut>().map_err(|_| {
            // Same #73 caution as `norte-config`'s persist helpers: never
            // echo `toml_edit`'s parse-error `Display`, which quotes the
            // offending document line — a hostile value persisted earlier
            // must not resurface verbatim.
            Error::new(
                ErrorKind::InvalidData,
                format!(
                    "{} no parsea: TOML inválido; corrígelo o bórralo",
                    path.display()
                ),
            )
        })?,
        Err(e) if e.kind() == ErrorKind::NotFound => toml_edit::DocumentMut::new(),
        Err(e) => return Err(e),
    };
    doc[key] = item;
    // #119: reemplazo ATÓMICO — tmp hermano + permisos del existente +
    // `sync_all` + rename (POSIX atómico; Windows reemplaza). Un lector
    // concurrente ve el fichero viejo o el nuevo COMPLETO, jamás un
    // truncado. Tmp huérfano de un crash = inocuo (lo pisa la siguiente
    // escritura, mismo nombre bajo el lock).
    let tmp = path.with_file_name("config.toml.tmp");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(doc.to_string().as_bytes())?;
        match std::fs::metadata(&path) {
            Ok(meta) => f.set_permissions(meta.permissions())?,
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

#[cfg(test)]
mod persist_plugin_setting_tests {
    use super::*;

    /// #119: el escritor toma el lock advisory cross-process
    /// (`config.toml.lock` del dir del plugin) ANTES de leer — con el lock
    /// en manos de otro descriptor, `persist_plugin_setting` BLOQUEA hasta
    /// la liberación (sin él, dos RMW GUI+TUI se intercalan y el segundo
    /// escribe sobre una lectura rancia).
    #[test]
    fn persist_espera_el_lock_de_otro_escritor() {
        let base = tempfile::tempdir().unwrap();
        let plugin_dir = base.path().join("plugins/org.norte.demo");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        // Mismo open SIN truncar que el lock real (#116 MAJOR-1).
        let holder = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(plugin_dir.join("config.toml.lock"))
            .unwrap();
        holder.lock().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let d = base.path().to_path_buf();
        let writer = std::thread::spawn(move || {
            let r = persist_plugin_setting(&d, "org.norte.demo", "mode", "slow");
            let _ = tx.send(());
            r
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(300))
                .is_err(),
            "persist NO debe completar mientras otro escritor tiene el lock"
        );
        assert!(
            !plugin_dir.join("config.toml").exists(),
            "nada escrito mientras el lock está en manos ajenas"
        );
        drop(holder);
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("liberado el lock, el escritor completa");
        writer.join().unwrap().expect("escritura");
    }

    /// #119: dos escritores RMW concurrentes sobre claves distintas no se
    /// pisan — ambas sobreviven con su último valor.
    #[test]
    fn escritores_concurrentes_no_pierden_claves() {
        let base = tempfile::tempdir().unwrap();
        let d1 = base.path().to_path_buf();
        let d2 = base.path().to_path_buf();
        let a = std::thread::spawn(move || {
            for i in 0..25 {
                persist_plugin_setting(&d1, "org.norte.demo", "alpha", &i.to_string()).expect("a");
            }
        });
        let b = std::thread::spawn(move || {
            for i in 0..25 {
                persist_plugin_setting(&d2, "org.norte.demo", "beta", &i.to_string()).expect("b");
            }
        });
        a.join().unwrap();
        b.join().unwrap();
        let s = std::fs::read_to_string(
            base.path()
                .join("plugins/org.norte.demo")
                .join("config.toml"),
        )
        .unwrap();
        let doc: toml_edit::DocumentMut = s.parse().expect("el fichero final parsea");
        assert_eq!(
            doc.get("alpha").and_then(|i| i.as_str()),
            Some("24"),
            "la última escritura de `alpha` sobrevive"
        );
        assert_eq!(
            doc.get("beta").and_then(|i| i.as_str()),
            Some("24"),
            "la última escritura de `beta` sobrevive"
        );
    }

    /// #119 (pin): escritura tmp hermano + rename — sin temporal residual.
    #[test]
    fn persistir_no_deja_tmp_residual() {
        let base = tempfile::tempdir().unwrap();
        persist_plugin_setting(base.path(), "org.norte.demo", "mode", "slow").unwrap();
        let plugin_dir = base.path().join("plugins/org.norte.demo");
        let residuales: Vec<String> = std::fs::read_dir(&plugin_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(residuales.is_empty(), "tmp residual: {residuales:?}");
    }

    #[test]
    fn round_trip_preservando_comentarios() {
        let base = tempfile::tempdir().unwrap();
        let plugin_dir = base.path().join("plugins/org.norte.demo");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("config.toml"),
            "# mi ajuste\ngreeting = \"hola\" # saludo\n",
        )
        .unwrap();
        persist_plugin_setting(base.path(), "org.norte.demo", "mode", "slow").unwrap();
        let s = std::fs::read_to_string(plugin_dir.join("config.toml")).unwrap();
        assert!(s.contains("# mi ajuste"), "{s}");
        assert!(s.contains("# saludo"), "{s}");
        assert!(s.contains("mode = \"slow\""), "{s}");
        assert!(
            s.contains("greeting = \"hola\""),
            "valor previo intacto: {s}"
        );
    }

    /// Revisión S, M5: par positivo/negativo de
    /// `plugin_id_is_safe_path_segment` — vacío, `.`/`..` exactos, y
    /// cualquier `plugin_id` con un separador embebido se rechazan; un id
    /// reverse-DNS normal no.
    #[test]
    fn plugin_id_is_safe_path_segment_rechaza_vacio_puntos_y_separadores() {
        for bad in ["", ".", "..", "../etc", "a/../b", "a/b", "a\\b", "/etc"] {
            assert!(
                !plugin_id_is_safe_path_segment(bad),
                "{bad:?} debería rechazarse"
            );
        }
        for good in ["org.norte.demo", "a", "a-b.c"] {
            assert!(
                plugin_id_is_safe_path_segment(good),
                "{good:?} debería aceptarse"
            );
        }
    }

    /// Revisión S, M5: `persist_plugin_setting` con un `plugin_id` hostil
    /// (`..`) es un `Err(InvalidInput)` — NUNCA escribe fuera de
    /// `config_dir/plugins/`. Pin de path traversal: sin el guard,
    /// `config_dir.join("plugins").join("..")` resuelve al propio
    /// `config_dir` — este test falla ruidosamente si esa regresión vuelve.
    #[test]
    fn persist_plugin_setting_rechaza_plugin_id_hostil() {
        let base = tempfile::tempdir().unwrap();
        let err = persist_plugin_setting(base.path(), "..", "mode", "slow")
            .expect_err("\"..\" debe rechazarse");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        // Nada se creó ni en `config_dir` ni en un ".." imaginario fuera de
        // él — el guard corre ANTES de cualquier I/O.
        assert!(
            std::fs::read_dir(base.path()).unwrap().next().is_none(),
            "config_dir debe seguir vacío"
        );
    }

    #[test]
    fn crea_el_directorio_y_fichero_si_no_existen() {
        let base = tempfile::tempdir().unwrap();
        let path =
            persist_plugin_setting(base.path(), "org.norte.nuevo", "greeting", "hi").unwrap();
        assert_eq!(
            path,
            base.path().join("plugins/org.norte.nuevo/config.toml")
        );
        let s = std::fs::read_to_string(&path).unwrap();
        assert!(s.contains("greeting = \"hi\""), "{s}");
    }

    /// Reemplaza el valor si la clave ya existía (no duplica).
    #[test]
    fn reemplaza_clave_existente() {
        let base = tempfile::tempdir().unwrap();
        persist_plugin_setting(base.path(), "org.norte.demo", "greeting", "hola").unwrap();
        persist_plugin_setting(base.path(), "org.norte.demo", "greeting", "adios").unwrap();
        let s = std::fs::read_to_string(base.path().join("plugins/org.norte.demo/config.toml"))
            .unwrap();
        assert_eq!(s.matches("greeting").count(), 1, "una sola clave: {s}");
        assert!(s.contains("adios"), "{s}");
        assert!(!s.contains("hola"), "{s}");
    }

    /// Pin encoding: un valor hostil (comilla, salto de línea, cabecera TOML
    /// embebida, override bidi) round-tripea escapado y byte-idéntico.
    #[test]
    fn valor_hostil_round_tripea_escapado() {
        let base = tempfile::tempdir().unwrap();
        let hostile = "fa\"vo\n[[evil]]\u{202E}rito";
        persist_plugin_setting(base.path(), "org.norte.demo", "greeting", hostile).unwrap();
        let path = base.path().join("plugins/org.norte.demo/config.toml");
        let s = std::fs::read_to_string(&path).unwrap();
        let doc: toml::Table = toml::from_str(&s).expect("TOML válido, sin inyección");
        assert_eq!(
            doc.get("greeting").and_then(toml::Value::as_str),
            Some(hostile),
            "valor byte-idéntico tras el round trip: {s}"
        );
    }
}
