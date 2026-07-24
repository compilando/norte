//! P2 Task 2: `resolve_settings` — valores EFECTIVOS de `[config]` para un
//! plugin, fusionando los defaults del esquema con `dir/config.toml`
//! (validado fail-closed, decisión 3). Sin catálogo/registro (eso lo cubren
//! `tests/model.rs`/`norte-core::plugins`); solo la función pura.

use norte_plugin_host::{ConfigValueError, Manifest, resolve_settings};

/// Manifiesto con los 4 tipos de `[config]`, mismos valores que
/// `tests/model.rs::WITH_CONFIG` para reusar intuición entre ambos ficheros.
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

/// Manifiesto SIN `[config]` (mapa vacío) — para el caso "sin esquema, sin
/// fichero".
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
fn config_toml_ausente_son_todos_los_defaults() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    // Sin escribir config.toml.
    let settings = resolve_settings(&manifest, dir.path()).unwrap();
    assert_eq!(settings.len(), 4);
    assert_eq!(settings.get("greeting").map(String::as_str), Some("hola"));
    assert_eq!(settings.get("enabled").map(String::as_str), Some("true"));
    assert_eq!(settings.get("retries").map(String::as_str), Some("3"));
    assert_eq!(settings.get("mode").map(String::as_str), Some("fast"));
}

#[test]
fn sin_config_en_manifiesto_y_sin_fichero_es_mapa_vacio() {
    let manifest = Manifest::from_toml(NO_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let settings = resolve_settings(&manifest, dir.path()).unwrap();
    assert!(settings.is_empty());
}

#[test]
fn overrides_validos_reemplazan_los_defaults() {
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
fn override_parcial_conserva_el_resto_en_default() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_values(dir.path(), r"retries = 9");
    let settings = resolve_settings(&manifest, dir.path()).unwrap();
    assert_eq!(settings.get("retries").map(String::as_str), Some("9"));
    // El resto sigue en su default (no se escribieron en config.toml).
    assert_eq!(settings.get("greeting").map(String::as_str), Some("hola"));
    assert_eq!(settings.get("enabled").map(String::as_str), Some("true"));
    assert_eq!(settings.get("mode").map(String::as_str), Some("fast"));
}

#[test]
fn clave_desconocida_en_config_toml_se_rechaza_nombrando_la_clave() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_values(dir.path(), r#"no-declarada = "x""#);
    let err = resolve_settings(&manifest, dir.path()).unwrap_err();
    assert!(
        matches!(&err, ConfigValueError::UnknownKey { key } if key == "no-declarada"),
        "{err:?}"
    );
}

#[test]
fn tipo_toml_incorrecto_se_rechaza_nombrando_la_clave() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    // `enabled` es bool en el esquema; aquí llega un entero.
    write_values(dir.path(), r"enabled = 1");
    let err = resolve_settings(&manifest, dir.path()).unwrap_err();
    assert!(
        matches!(&err, ConfigValueError::WrongType { key, .. } if key == "enabled"),
        "{err:?}"
    );
}

#[test]
fn tabla_anidada_bajo_una_clave_escalar_se_rechaza_config_toml_es_plano() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    // config.toml debe ser PLANO: una tabla anidada bajo `greeting` (string)
    // es un tipo incorrecto, no se desciende dentro de ella.
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
fn int_fuera_de_rango_se_rechaza_nombrando_la_clave() {
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
fn int_en_el_borde_del_rango_es_valido() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_values(dir.path(), r"retries = 10");
    let settings = resolve_settings(&manifest, dir.path()).unwrap();
    assert_eq!(settings.get("retries").map(String::as_str), Some("10"));
}

#[test]
fn enum_no_miembro_se_rechaza_nombrando_la_clave() {
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
fn config_toml_roto_no_es_toml_valido() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_values(dir.path(), "esto no es [ toml valido =");
    let err = resolve_settings(&manifest, dir.path()).unwrap_err();
    assert!(matches!(err, ConfigValueError::Toml(_)), "{err:?}");
}

/// El mensaje de error NUNCA interpola el VALOR hostil (#73), solo la clave.
#[test]
fn el_mensaje_de_error_nunca_lleva_el_valor() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let secreto = "hunter2-super-secret-marker";
    write_values(dir.path(), &format!(r#"mode = "{secreto}""#));
    let err = resolve_settings(&manifest, dir.path()).unwrap_err();
    assert!(
        !err.to_string().contains(secreto),
        "el error no debe llevar el valor: {err}"
    );
}
