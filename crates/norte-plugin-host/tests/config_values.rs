//! P2 Task 2: `resolve_settings` — EFFECTIVE `[config]` values for a
//! plugin, merging the schema's defaults with `dir/config.toml`
//! (validated fail-closed, decision 3). No catalog/registry (that is
//! covered by `tests/model.rs`/`norte-core::plugins`); just the pure
//! function.

use norte_plugin_host::{ConfigValueError, Manifest, resolve_settings};

/// Manifest with the 4 `[config]` types, same values as
/// `tests/model.rs::WITH_CONFIG` to reuse intuition between both files.
const WITH_CONFIG: &str = r#"
[plugin]
id = "org.norte.demo-config"
name = "Demo Config"
publisher = "norte"
version = "0.1.0"
category = "command"

[config.greeting]
type = "string"
default = "hola"

[config.enabled]
type = "bool"
default = true

[config.retries]
type = "int"
default = 3
min = 0
max = 10

[config.mode]
type = "enum"
default = "fast"
values = ["fast", "slow"]
"#;

/// Manifest WITHOUT `[config]` (empty map) — for the "no schema, no file"
/// case.
const NO_CONFIG: &str = r#"
[plugin]
id = "org.norte.no-config"
name = "No Config"
publisher = "norte"
version = "0.1.0"
category = "command"
"#;

fn write_values(dir: &std::path::Path, toml: &str) {
    std::fs::write(dir.join("config.toml"), toml).unwrap();
}

#[test]
fn absent_config_toml_is_all_defaults() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    // Without writing config.toml.
    let settings = resolve_settings(&manifest, dir.path()).unwrap();
    assert_eq!(settings.len(), 4);
    assert_eq!(settings.get("greeting").map(String::as_str), Some("hola"));
    assert_eq!(settings.get("enabled").map(String::as_str), Some("true"));
    assert_eq!(settings.get("retries").map(String::as_str), Some("3"));
    assert_eq!(settings.get("mode").map(String::as_str), Some("fast"));
}

#[test]
fn no_config_in_manifest_and_no_file_is_an_empty_map() {
    let manifest = Manifest::from_toml(NO_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let settings = resolve_settings(&manifest, dir.path()).unwrap();
    assert!(settings.is_empty());
}

#[test]
fn valid_overrides_replace_the_defaults() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_values(
        dir.path(),
        r#"
        greeting = "hola mundo"
        enabled = false
        retries = 7
        mode = "slow"
        "#,
    );
    let settings = resolve_settings(&manifest, dir.path()).unwrap();
    assert_eq!(
        settings.get("greeting").map(String::as_str),
        Some("hola mundo")
    );
    assert_eq!(settings.get("enabled").map(String::as_str), Some("false"));
    assert_eq!(settings.get("retries").map(String::as_str), Some("7"));
    assert_eq!(settings.get("mode").map(String::as_str), Some("slow"));
}

#[test]
fn a_partial_override_keeps_the_rest_at_default() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_values(dir.path(), r"retries = 9");
    let settings = resolve_settings(&manifest, dir.path()).unwrap();
    assert_eq!(settings.get("retries").map(String::as_str), Some("9"));
    // The rest stays at its default (not written into config.toml).
    assert_eq!(settings.get("greeting").map(String::as_str), Some("hola"));
    assert_eq!(settings.get("enabled").map(String::as_str), Some("true"));
    assert_eq!(settings.get("mode").map(String::as_str), Some("fast"));
}

#[test]
fn an_unknown_key_in_config_toml_is_rejected_naming_the_key() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_values(dir.path(), r#"not-declared = "x""#);
    let err = resolve_settings(&manifest, dir.path()).unwrap_err();
    assert!(
        matches!(&err, ConfigValueError::UnknownKey { key } if key == "not-declared"),
        "{err:?}"
    );
}

#[test]
fn wrong_toml_type_is_rejected_naming_the_key() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    // `enabled` is bool in the schema; here an integer arrives.
    write_values(dir.path(), r"enabled = 1");
    let err = resolve_settings(&manifest, dir.path()).unwrap_err();
    assert!(
        matches!(&err, ConfigValueError::WrongType { key, .. } if key == "enabled"),
        "{err:?}"
    );
}

#[test]
fn a_table_nested_under_a_scalar_key_is_rejected_config_toml_is_flat() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    // config.toml must be FLAT: a table nested under `greeting` (string)
    // is a wrong type, it is not descended into.
    write_values(
        dir.path(),
        r#"
        [greeting]
        nested = "not-flat"
        "#,
    );
    let err = resolve_settings(&manifest, dir.path()).unwrap_err();
    assert!(
        matches!(&err, ConfigValueError::WrongType { key, .. } if key == "greeting"),
        "{err:?}"
    );
}

#[test]
fn an_int_out_of_range_is_rejected_naming_the_key() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_values(dir.path(), r"retries = 99");
    let err = resolve_settings(&manifest, dir.path()).unwrap_err();
    assert!(
        matches!(&err, ConfigValueError::IntOutOfRange { key } if key == "retries"),
        "{err:?}"
    );
}

#[test]
fn an_int_at_the_range_edge_is_valid() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_values(dir.path(), r"retries = 10");
    let settings = resolve_settings(&manifest, dir.path()).unwrap();
    assert_eq!(settings.get("retries").map(String::as_str), Some("10"));
}

#[test]
fn a_non_member_enum_is_rejected_naming_the_key() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_values(dir.path(), r#"mode = "turbo""#);
    let err = resolve_settings(&manifest, dir.path()).unwrap_err();
    assert!(
        matches!(&err, ConfigValueError::EnumNotMember { key } if key == "mode"),
        "{err:?}"
    );
}

#[test]
fn a_broken_config_toml_is_not_valid_toml() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_values(dir.path(), "this is not [ valid toml =");
    let err = resolve_settings(&manifest, dir.path()).unwrap_err();
    assert!(matches!(err, ConfigValueError::Toml { .. }), "{err:?}");
}

/// The error message NEVER interpolates the hostile VALUE (#73), only the
/// key.
#[test]
fn the_error_message_never_carries_the_value() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let secret = "hunter2-super-secret-marker";
    write_values(dir.path(), &format!(r#"mode = "{secret}""#));
    let err = resolve_settings(&manifest, dir.path()).unwrap_err();
    assert!(
        !err.to_string().contains(secret),
        "the error must not carry the value: {err}"
    );
}

// --- security review P2 Task 4a --------------------------------------

/// An UNKNOWN `config.toml` key carrying a terminal hazard (ESC) must NOT
/// appear raw in the error message — unlike a key declared in the schema
/// (always `[a-z0-9-]{1,32}`, safe), this key comes from user TOML with
/// no such guarantee.
#[test]
fn a_hostile_unknown_key_does_not_appear_in_the_message() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    // Quoted TOML key with an embedded ESC (`\u001b`, valid TOML escape →
    // a real control byte in the key).
    write_values(dir.path(), "\"mal\\u001bicious\" = \"x\"\n");
    let err = resolve_settings(&manifest, dir.path()).unwrap_err();
    let msg = err.to_string();
    assert!(
        !msg.contains('\u{1b}'),
        "the key's ESC must not reach the message: {msg:?}"
    );
    assert!(
        matches!(&err, ConfigValueError::UnknownKey { key } if key.contains('\u{1b}')),
        "but the `key` field STILL carries the raw key (programmatic use): {err:?}"
    );
}

/// An UNKNOWN "safe" key (charset `[a-z0-9-]{1,32}`, the SAME requirement
/// any declared key already meets) IS shown — useful messages for the
/// common case, without sacrificing security.
#[test]
fn a_safe_unknown_key_does_appear_in_the_message() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_values(dir.path(), "not-declared = \"x\"\n");
    let err = resolve_settings(&manifest, dir.path()).unwrap_err();
    assert!(
        err.to_string().contains("not-declared"),
        "a charset-safe key is shown: {err}"
    );
}

/// A TOML parse failure over a source with a "secret" fragment must not
/// leak that fragment into the message — only a content-free summary.
#[test]
fn a_toml_parse_error_does_not_leak_the_source_fragment() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let secret = "hunter2-super-secret-marker";
    // UNCLOSED string: parsing fails, but the line's fragment (including
    // the secret) must not appear in the message.
    write_values(dir.path(), &format!("greeting = \"{secret}\n"));
    let err = resolve_settings(&manifest, dir.path()).unwrap_err();
    let msg = err.to_string();
    assert!(
        !msg.contains(secret),
        "the content-free summary must not carry the source fragment: {msg:?}"
    );
    assert!(matches!(err, ConfigValueError::Toml { .. }));
}

/// `config.toml` exceeding the size cap is rejected BEFORE parsing.
#[test]
fn a_config_toml_too_large_is_rejected() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    // Dummy content exceeding the cap, even if it is not valid TOML: the
    // size is checked BEFORE trying to parse.
    let cap_usize = usize::try_from(norte_plugin_host::CONFIG_VALUES_MAX_BYTES).unwrap();
    let oversized = "x".repeat(cap_usize + 1);
    write_values(dir.path(), &oversized);
    let err = resolve_settings(&manifest, dir.path()).unwrap_err();
    assert!(
        matches!(
            &err,
            ConfigValueError::TooLarge { len, cap }
                if *len == norte_plugin_host::CONFIG_VALUES_MAX_BYTES + 1
                    && *cap == norte_plugin_host::CONFIG_VALUES_MAX_BYTES
        ),
        "{err:?}"
    );
}

/// `config.toml` right AT the size cap is accepted (parses as invalid
/// TOML, but not via `TooLarge` — the cap is EXCLUSIVE above it).
#[test]
fn a_config_toml_at_the_exact_cap_is_not_too_large() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    // Valid TOML comment padded to EXACTLY the cap.
    let cap_usize = usize::try_from(norte_plugin_host::CONFIG_VALUES_MAX_BYTES).unwrap();
    let mut src = "#".repeat(cap_usize);
    // A pure-`#` comment is valid TOML (an empty comment line).
    src.truncate(cap_usize);
    write_values(dir.path(), &src);
    let result = resolve_settings(&manifest, dir.path());
    assert!(
        !matches!(result, Err(ConfigValueError::TooLarge { .. })),
        "{result:?}"
    );
}

/// A `string` value exceeding `CONFIG_STRING_MAX_CHARS` is rejected
/// naming the key — same cap as a `default` (decision 1), also applied to
/// user overrides.
#[test]
fn a_281_char_string_value_is_rejected_naming_the_key() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let long = "a".repeat(281);
    write_values(dir.path(), &format!("greeting = \"{long}\"\n"));
    let err = resolve_settings(&manifest, dir.path()).unwrap_err();
    assert!(
        matches!(&err, ConfigValueError::ValueTooLong { key } if key == "greeting"),
        "{err:?}"
    );
}

/// A `string` value of exactly 280 chars (the cap) IS accepted.
#[test]
fn a_280_char_string_value_is_the_exact_cap() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let limit = "a".repeat(280);
    write_values(dir.path(), &format!("greeting = \"{limit}\"\n"));
    let settings = resolve_settings(&manifest, dir.path()).unwrap();
    assert_eq!(settings.get("greeting"), Some(&limit));
}
