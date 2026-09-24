//! EFFECTIVE `[config]` values (P2 decision 3): merges the manifest's
//! `[config]` schema defaults with `config_dir/plugins/<id>/config.toml`
//! (FLAT `key = value`), validating fail-closed each key that is present.
//! An absent `config.toml` ⇒ all defaults. An absent manifest `[config]` ⇒
//! empty map (same criterion as [`Manifest::config`]).

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use crate::manifest::{CONFIG_STRING_MAX_CHARS, ConfigKeySpec, Manifest, is_valid_config_key};

/// Size cap for `config.toml` ON DISK, checked BEFORE reading it (anti-DoS,
/// same criterion as `runtime.rs`'s `MAX_ARTIFACT_BYTES` for the `.wasm`): a
/// file of user values has no legitimate reason to weigh more than this —
/// at most a few dozen keys of at most [`CONFIG_STRING_MAX_CHARS`]
/// characters each. Above it, it is rejected fail-loud without ever
/// reaching `read_to_string`/`toml::from_str` (security review P2 Task 4a).
pub const CONFIG_VALUES_MAX_BYTES: u64 = 64 * 1024;

/// Failure resolving `[config]` values from `config.toml` against the
/// manifest's schema. Every variant that identifies a key carries the
/// culprit KEY; NONE carries the VALUE (issue #73): `config.toml` is
/// user/plugin data, untrusted, and the value must not reach logs/UI/error
/// messages. The KEY itself CAN ALSO be hostile (unlike a key DECLARED in
/// the schema, already validated against `[a-z0-9-]{1,32}` in Task 1, a key
/// from a `config.toml` FOREIGN to the schema —`UnknownKey`— is user TOML
/// with no such guarantee): see [`ConfigValueError::UnknownKey`]'s doc
/// (security review P2 Task 4a, issue #73).
#[derive(Debug, thiserror::Error)]
pub enum ConfigValueError {
    /// `config.toml` does not parse as TOML. The message is a
    /// CONTENT-FREE summary (never the full form of `toml::de::Error`,
    /// which ECHOES the raw fragment of the offending source line — a
    /// hostile value or a secret pasted by mistake must NOT reach
    /// logs/UI/wire; same criterion as `connections-parse` in
    /// `norte-cli::doctor`, security review P2 Task 4a).
    #[error("invalid config.toml: {summary}")]
    Toml {
        /// Summary WITHOUT a source fragment (`toml::de::Error::message`,
        /// or failing that the first line of its `Display`, which in this
        /// version of the `toml` crate is only
        /// `"TOML parse error at line N, column M"` — position, never
        /// content).
        summary: String,
        /// The real cause, for `Error::source()`/internal debugging —
        /// NEVER interpolated into the message.
        #[source]
        source: toml::de::Error,
    },
    /// I/O error reading `config.toml`, OTHER than "does not exist" (that
    /// means defaults, not an error — see [`resolve_settings`]).
    #[error("unreadable config.toml: {0}")]
    Io(#[source] io::Error),
    /// `config.toml` ON DISK exceeds [`CONFIG_VALUES_MAX_BYTES`]: rejected
    /// before reading it in full.
    #[error("config.toml too large: {len} bytes (max {cap})")]
    TooLarge {
        /// Actual size on disk.
        len: u64,
        /// Allowed cap ([`CONFIG_VALUES_MAX_BYTES`]).
        cap: u64,
    },
    /// A `config.toml` key is not declared in the manifest's `[config]`.
    ///
    /// `key` is the RAW key — use it to compare programmatically (tests,
    /// logic), NEVER assume the message shows it: unlike a key DECLARED in
    /// the schema (validated against `[a-z0-9-]{1,32}` while parsing the
    /// manifest, Task 1), this key comes from `config.toml`, which is
    /// USER TOML — a quoted key can carry ANY text (control characters,
    /// bidi overrides, absurdly long). The message only interpolates it
    /// when it ALSO respects that same safe charset; otherwise it stays
    /// generic. This message crosses over to both `norte doctor` and the
    /// WIRE (`PluginLoadError.reason` via `plugin.list`, readable by an
    /// agent) — security review P2 Task 4a.
    #[error("unknown key in config.toml{}", display_key_suffix(key))]
    UnknownKey {
        /// The RAW unknown key.
        key: String,
    },
    /// The value's TOML type does not match the type declared in the
    /// schema. Also covers a nested table under a scalar key: `config.toml`
    /// is FLAT (decision 3) — a table is never a valid type for
    /// `string`/`bool`/`int`/`enum`, so no separate case is needed for
    /// "not flattened": it lands here naturally. `key` here is ALWAYS a
    /// DECLARED key (it already passed `manifest.config.get`), therefore
    /// already validated against the safe charset while parsing the
    /// manifest — safe to interpolate as-is, unlike [`Self::UnknownKey`].
    #[error("wrong TOML type for `{key}` (expected {expected})")]
    WrongType {
        /// The key with the wrong type (DECLARED, safe charset).
        key: String,
        /// The expected TOML type (`string`/`bool`/`int`/`enum (string)`).
        expected: &'static str,
    },
    /// The `int` value falls outside the `[min, max]` declared in the
    /// schema. `key` is DECLARED (see [`Self::WrongType`]'s doc).
    #[error("`{key}` outside the declared range")]
    IntOutOfRange {
        /// The out-of-range key.
        key: String,
    },
    /// The `enum` value is not among the `values` declared in the schema.
    /// `key` is DECLARED (see [`Self::WrongType`]'s doc).
    #[error("`{key}` is not among the allowed values")]
    EnumNotMember {
        /// The key with a disallowed value.
        key: String,
    },
    /// The `string` value exceeds [`CONFIG_STRING_MAX_CHARS`] characters —
    /// the same cap as a `string`-typed `default` in the schema (decision
    /// 1), applied ALSO to `config.toml` overrides (anti-DoS: without this
    /// cap, an arbitrarily long override would bloat logs/UI/wire the same
    /// way a long default was already forbidden from doing). `key` is
    /// DECLARED (see [`Self::WrongType`]'s doc).
    #[error("`{key}` (string) exceeds the {CONFIG_STRING_MAX_CHARS}-character cap")]
    ValueTooLong {
        /// The key with the too-long value.
        key: String,
    },
}

impl From<toml::de::Error> for ConfigValueError {
    fn from(source: toml::de::Error) -> Self {
        let summary = toml_error_summary(&source);
        ConfigValueError::Toml { summary, source }
    }
}

/// Extracts a CONTENT-FREE summary from a TOML parse failure (security
/// review P2 Task 4a): `toml::de::Error::message` is already content-free
/// (it never includes the source fragment, only the short description —
/// e.g. "duplicate key" or "invalid basic string, expected a closing
/// quote"), but in case a future version of the crate left it empty in
/// some case, the fallback is the FIRST line of its `Display`, which in
/// this version is only "TOML parse error at line N, column M" (position,
/// never content — verified empirically against several parse failures,
/// including an unclosed string).
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

/// Display suffix for [`ConfigValueError::UnknownKey`]: empty string if
/// `key` does NOT respect the safe display charset (`[a-z0-9-]{1,32}`, see
/// [`is_valid_config_key`]); if it does, a suffix with the quoted key.
fn display_key_suffix(key: &str) -> String {
    if is_valid_config_key(key) {
        format!(": `{key}`")
    } else {
        String::new()
    }
}

/// Encodes `spec`'s DEFAULT to its canonical string form (decision 4, the
/// same one Task 3's WIT `host-config` consumes): `bool` →
/// `"true"`/`"false"`; `int` → decimal; `string`/`enum` → the raw text.
fn default_string(spec: &ConfigKeySpec) -> String {
    match spec {
        ConfigKeySpec::String { default, .. } | ConfigKeySpec::Enum { default, .. } => {
            default.clone()
        }
        ConfigKeySpec::Bool { default, .. } => default.to_string(),
        ConfigKeySpec::Int { default, .. } => default.to_string(),
    }
}

/// Validates a value SUBMITTED OVER THE WIRE (`plugin.set_config`, G3c)
/// against `spec` and encodes it to its canonical string form — the SAME
/// validation [`resolve_settings`] applies to `config.toml` (single
/// source, spec S2: "validated against the schema BEFORE writing"), never
/// a parallel path.
///
/// Unlike `config.toml` (typed TOML: native `toml::Value::Boolean`/
/// `Integer`), the wire ALWAYS sends a `String`
/// (`PluginSetConfigParams::value`) — this function does the minimal
/// parsing the declared type requires before reusing `encode_override`
/// (private, same module): `bool` requires EXACTLY `"true"`/`"false"`
/// (never `"1"`/`"yes"`, the same rigor as a `config.toml` with
/// `key = "true"` instead of `key = true` — that would ALSO be
/// `WrongType` there), `int` requires a decimal ASCII string valid for
/// `i64`. `string`/`enum` pass the text as-is (their own
/// charset/length are validated by `encode_override`).
///
/// # Errors
/// [`ConfigValueError::WrongType`] if `raw` does not parse to `spec`'s
/// type; the rest of `encode_override`'s variants (range, enum, length)
/// as-is.
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

/// Validates `value` (raw, from `config.toml`) against `spec` and encodes
/// it to its canonical string form. `key` travels ONLY to name the error
/// (`value` is never interpolated, #73).
fn encode_override(
    key: &str,
    spec: &ConfigKeySpec,
    value: toml::Value,
) -> Result<String, ConfigValueError> {
    match spec {
        ConfigKeySpec::String { .. } => match value {
            toml::Value::String(s) => {
                // Same cap as a string-typed `default` in the schema
                // (decision 1, security review P2 Task 4a): an override
                // does not pay less than a default.
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

/// Resolves the EFFECTIVE `[config]` values for a plugin installed at
/// `dir`: starts from the schema's DEFAULTS (`manifest.config`) and
/// overlays `dir/config.toml` if it exists, validating fail-closed each
/// key present (P2 decision 3). `dir` is the plugin's directory
/// (`config_dir/plugins/<id>/`, the same as [`crate::PluginEntry::dir`]),
/// NOT `config_dir` itself.
///
/// An ABSENT `config.toml` is not an error: all defaults. An empty/absent
/// manifest `[config]` ⇒ empty map, with or without `config.toml` (nothing
/// to resolve: any key in the file would be `UnknownKey`).
///
/// # Errors
/// [`ConfigValueError`] if `config.toml` exceeds [`CONFIG_VALUES_MAX_BYTES`]
/// on disk, does not parse, declares a key the schema does not know, a
/// value of the wrong TOML type (including a nested table under a scalar
/// key: the values file is FLAT), an `int` outside `[min, max]`, an `enum`
/// outside `values`, or a `string` exceeding [`CONFIG_STRING_MAX_CHARS`]
/// characters.
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
    // Size cap BEFORE reading (security review P2 Task 4a, anti-DoS — same
    // criterion as `runtime.rs`'s `check_artifact_size` for the `.wasm`):
    // `metadata` is cheap and does not read the content.
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
    // `toml::Table` iterates in KEY order (a BTreeMap internally):
    // deterministic, independent of the order in the file.
    for (key, value) in table {
        let Some(spec) = manifest.config.get(&key) else {
            return Err(ConfigValueError::UnknownKey { key });
        };
        let encoded = encode_override(&key, spec, value)?;
        out.insert(key, encoded);
    }
    Ok(out)
}

/// Review S, M5: is it safe to use `id` as A SINGLE path segment
/// (`config_dir.join("plugins").join(id)`)? Rejects empty, EXACT `.`/`..`,
/// and any embedded path separator (`/`, and `\` in case this same binary
/// ever ran on Windows one day — `Path::join` on Unix treats `\` as an
/// ordinary name character, but a `plugin_id` with `\` remains suspicious
/// there too). CHEAP defense in depth: the manifest's reverse-DNS charset
/// (`manifest::is_valid_plugin_id`, which runs while parsing an approved
/// plugin) already excludes all of this on the normal path; this guard
/// covers the write primitive against a future caller that does not repeat
/// that validation.
#[must_use]
fn plugin_id_is_safe_path_segment(id: &str) -> bool {
    !id.is_empty() && id != "." && id != ".." && !id.contains(['/', '\\'])
}

/// Sets `key = value` in `config_dir/plugins/<plugin_id>/config.toml`
/// (S2), PRESERVING comments and formatting (`toml_edit`) — the same
/// pattern as `norte_config::persist_set`, adapted to this file being FLAT
/// (P2 decision 3: no nested sections, every key lives top-level). Creates
/// the plugin's directory and the file if they don't exist.
///
/// `plugin_id` must arrive ALREADY validated by the caller (e.g. the `id`
/// of an already-loaded [`Manifest`], which passed the reverse-DNS
/// charset while parsing) — this function ADDS a cheap defense-in-depth
/// guard (review S, M5: `plugin_id_is_safe_path_segment`) because
/// `plugin_id` is used DIRECTLY as a path segment
/// (`config_dir.join("plugins").join(plugin_id)`): without the guard, an
/// untrusted `plugin_id` with `..`/a separator could escape `plugins/` —
/// the manifest's own reverse-DNS charset already prevents this for a
/// normally APPROVED plugin, but this primitive must not blindly trust
/// that EVERY future caller repeats that validation. Same responsibility
/// any caller of `PluginEntry::dir` already has.
///
/// `value` is written AS-IS as a TOML string (escaped by `toml_edit`,
/// never injected raw) — validating `value` against the manifest's
/// `[config]` schema type/range (bool/int/enum, the same validation this
/// module's internal `encode_override` performs) is the CALLER's
/// responsibility, BEFORE calling here (spec S2: "validated against the
/// schema BEFORE writing — never persist an invalid value"). This function
/// is the write primitive, not the validator: [`resolve_settings`] remains
/// the sole source of truth for reading/validating, and THAT function
/// expects a NATIVE `toml::Value` (real bool/int, not a string) for
/// `bool`/`int` keys — a typed caller must write those types with
/// `toml_edit` directly if it needs that round trip; this helper covers
/// the flat `string`/`enum` case (the only one S2 wires end to end).
///
/// # Errors
/// [`std::io::Error`] (`InvalidInput`) if `plugin_id` does not pass
/// `plugin_id_is_safe_path_segment`; (another kind) if the existing
/// `config.toml` fails to parse or I/O fails.
pub fn persist_plugin_setting(
    config_dir: &Path,
    plugin_id: &str,
    key: &str,
    value: &str,
) -> io::Result<std::path::PathBuf> {
    persist_plugin_setting_item(config_dir, plugin_id, key, toml_edit::value(value))
}

/// Like [`persist_plugin_setting`] but writes `raw` with the NATIVE TOML
/// type `spec` declares (bool → TOML boolean, int → TOML integer,
/// string/enum → TOML string) instead of always a string (G3c,
/// `plugin.set_config`). Necessary because a later re-read via
/// [`resolve_settings`] expects the same NATIVE typing a human-written
/// `config.toml` would produce — a `key = "true"` (string) for a `bool`
/// key would fail there with `WrongType`, exactly as it would if a human
/// had written it by hand.
///
/// `raw` MUST arrive already validated (the caller calls
/// [`encode_wire_value`] first, which is exactly what
/// `PluginRegistry::set_config` does) — this function does not revalidate
/// range/enum, it only types. A `raw` that does not parse to `spec`'s type
/// (a broken caller invariant) is [`io::ErrorKind::InvalidInput`], never a
/// panic.
///
/// # Errors
/// Same as [`persist_plugin_setting`], plus
/// [`io::ErrorKind::InvalidInput`] if `raw` does not parse to `spec`'s
/// NATIVE type (broken caller invariant).
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
                    "invalid raw bool (should have been validated earlier with encode_wire_value)",
                ));
            }
        },
        ConfigKeySpec::Int { .. } => {
            let i: i64 = raw.parse().map_err(|_| {
                Error::new(
                    ErrorKind::InvalidInput,
                    "invalid raw int (should have been validated earlier with encode_wire_value)",
                )
            })?;
            toml_edit::value(i)
        }
    };
    persist_plugin_setting_item(config_dir, plugin_id, key, item)
}

/// Shared primitive for [`persist_plugin_setting`]/
/// [`persist_plugin_setting_typed`]: validates `plugin_id` as a path
/// segment, creates the directory if missing, parses (or creates)
/// `config.toml` and sets `key` to the ALREADY-built `item`, preserving
/// comments/formatting.
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
            "invalid plugin_id as a path segment",
        ));
    }
    let dir = config_dir.join("plugins").join(plugin_id);
    std::fs::create_dir_all(&dir)?;
    // #119 (template #116, `norte-config::load`): cross-process advisory
    // lock on the DEDICATED sibling `config.toml.lock`, taken BEFORE
    // reading — the whole RMW is the critical section (GUI+TUI over the
    // same plugin). Opened WITHOUT truncating (#116 MAJOR-1:
    // `CREATE_ALWAYS` over someone else's `LockFileEx` lock can FAIL on
    // Windows instead of waiting). The OS releases it when the process
    // dies — no stale locks.
    // Deliberately duplicated from norte-config's helpers (decoupled
    // crates; same doctrine as the core/frontend fold).
    let lock_file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("config.toml.lock"))?;
    lock_file.lock()?;
    let _lock = lock_file; // alive until the end of the RMW; drop = unlock
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
                    "{} fails to parse: invalid TOML; fix it or delete it",
                    path.display()
                ),
            )
        })?,
        Err(e) if e.kind() == ErrorKind::NotFound => toml_edit::DocumentMut::new(),
        Err(e) => return Err(e),
    };
    doc[key] = item;
    // #119: ATOMIC replacement — sibling tmp + existing file's permissions
    // + `sync_all` + rename (POSIX atomic; Windows replaces). A concurrent
    // reader sees either the old file or the COMPLETE new one, never a
    // truncated one. An orphaned tmp from a crash = harmless (the next
    // write overwrites it, same name under the lock).
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

    /// #119: the writer takes the cross-process advisory lock (the
    /// plugin dir's `config.toml.lock`) BEFORE reading — with the lock
    /// held by another descriptor, `persist_plugin_setting` BLOCKS until
    /// it is released (without it, two GUI+TUI RMWs interleave and the
    /// second one writes over a stale read).
    #[test]
    fn persist_waits_for_another_writers_lock() {
        let base = tempfile::tempdir().unwrap();
        let plugin_dir = base.path().join("plugins/org.norte.demo");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        // Same open WITHOUT truncating as the real lock (#116 MAJOR-1).
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
            "persist must NOT complete while another writer holds the lock"
        );
        assert!(
            !plugin_dir.join("config.toml").exists(),
            "nothing written while the lock is held elsewhere"
        );
        drop(holder);
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("lock released, the writer completes");
        writer.join().unwrap().expect("write");
    }

    /// #119: two concurrent RMW writers over different keys do not step on
    /// each other — both survive with their last value.
    #[test]
    fn concurrent_writers_do_not_lose_keys() {
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
        let doc: toml_edit::DocumentMut = s.parse().expect("the final file parses");
        assert_eq!(
            doc.get("alpha").and_then(|i| i.as_str()),
            Some("24"),
            "the last write to `alpha` survives"
        );
        assert_eq!(
            doc.get("beta").and_then(|i| i.as_str()),
            Some("24"),
            "the last write to `beta` survives"
        );
    }

    /// #119 (pin): sibling tmp write + rename — no leftover temp file.
    #[test]
    fn persisting_leaves_no_leftover_tmp() {
        let base = tempfile::tempdir().unwrap();
        persist_plugin_setting(base.path(), "org.norte.demo", "mode", "slow").unwrap();
        let plugin_dir = base.path().join("plugins/org.norte.demo");
        let leftovers: Vec<String> = std::fs::read_dir(&plugin_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(leftovers.is_empty(), "leftover tmp: {leftovers:?}");
    }

    #[test]
    fn round_trip_preserves_comments() {
        let base = tempfile::tempdir().unwrap();
        let plugin_dir = base.path().join("plugins/org.norte.demo");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("config.toml"),
            "# my setting\ngreeting = \"hola\" # greeting\n",
        )
        .unwrap();
        persist_plugin_setting(base.path(), "org.norte.demo", "mode", "slow").unwrap();
        let s = std::fs::read_to_string(plugin_dir.join("config.toml")).unwrap();
        assert!(s.contains("# my setting"), "{s}");
        assert!(s.contains("# greeting"), "{s}");
        assert!(s.contains("mode = \"slow\""), "{s}");
        assert!(
            s.contains("greeting = \"hola\""),
            "previous value intact: {s}"
        );
    }

    /// Review S, M5: positive/negative pair for
    /// `plugin_id_is_safe_path_segment` — empty, exact `.`/`..`, and any
    /// `plugin_id` with an embedded separator are rejected; a normal
    /// reverse-DNS id is not.
    #[test]
    fn plugin_id_is_safe_path_segment_rejects_empty_dots_and_separators() {
        for bad in ["", ".", "..", "../etc", "a/../b", "a/b", "a\\b", "/etc"] {
            assert!(
                !plugin_id_is_safe_path_segment(bad),
                "{bad:?} should be rejected"
            );
        }
        for good in ["org.norte.demo", "a", "a-b.c"] {
            assert!(
                plugin_id_is_safe_path_segment(good),
                "{good:?} should be accepted"
            );
        }
    }

    /// Review S, M5: `persist_plugin_setting` with a hostile `plugin_id`
    /// (`..`) is an `Err(InvalidInput)` — it NEVER writes outside
    /// `config_dir/plugins/`. Path traversal pin: without the guard,
    /// `config_dir.join("plugins").join("..")` resolves to `config_dir`
    /// itself — this test fails loudly if that regression comes back.
    #[test]
    fn persist_plugin_setting_rejects_hostile_plugin_id() {
        let base = tempfile::tempdir().unwrap();
        let err = persist_plugin_setting(base.path(), "..", "mode", "slow")
            .expect_err("\"..\" should be rejected");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        // Nothing was created in `config_dir` nor in an imaginary ".."
        // outside it — the guard runs BEFORE any I/O.
        assert!(
            std::fs::read_dir(base.path()).unwrap().next().is_none(),
            "config_dir must remain empty"
        );
    }

    #[test]
    fn creates_the_directory_and_file_if_missing() {
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

    /// Replaces the value if the key already existed (does not duplicate).
    #[test]
    fn replaces_existing_key() {
        let base = tempfile::tempdir().unwrap();
        persist_plugin_setting(base.path(), "org.norte.demo", "greeting", "hola").unwrap();
        persist_plugin_setting(base.path(), "org.norte.demo", "greeting", "adios").unwrap();
        let s = std::fs::read_to_string(base.path().join("plugins/org.norte.demo/config.toml"))
            .unwrap();
        assert_eq!(s.matches("greeting").count(), 1, "a single key: {s}");
        assert!(s.contains("adios"), "{s}");
        assert!(!s.contains("hola"), "{s}");
    }

    /// Encoding pin: a hostile value (quote, newline, embedded TOML
    /// header, bidi override) round-trips escaped and byte-identical.
    #[test]
    fn hostile_value_round_trips_escaped() {
        let base = tempfile::tempdir().unwrap();
        let hostile = "fa\"vo\n[[evil]]\u{202E}rito";
        persist_plugin_setting(base.path(), "org.norte.demo", "greeting", hostile).unwrap();
        let path = base.path().join("plugins/org.norte.demo/config.toml");
        let s = std::fs::read_to_string(&path).unwrap();
        let doc: toml::Table = toml::from_str(&s).expect("valid TOML, no injection");
        assert_eq!(
            doc.get("greeting").and_then(toml::Value::as_str),
            Some(hostile),
            "byte-identical value after the round trip: {s}"
        );
    }
}
