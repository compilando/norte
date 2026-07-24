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
    assert!(matches!(err, ConfigValueError::Toml { .. }), "{err:?}");
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

// --- security review P2 Task 4a --------------------------------------

/// Una clave DESCONOCIDA de `config.toml` que lleva un hazard de terminal
/// (ESC) NO debe aparecer cruda en el mensaje del error — a diferencia de una
/// clave declarada en el esquema (siempre `[a-z0-9-]{1,32}`, segura), esta
/// clave viene de TOML de usuario sin esa garantía.
#[test]
fn clave_desconocida_hostil_no_aparece_en_el_mensaje() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    // Clave TOML entrecomillada con un ESC embebido (``, escape TOML
    // válido → byte de control real en la clave).
    write_values(dir.path(), "\"mal\\u001bicious\" = \"x\"\n");
    let err = resolve_settings(&manifest, dir.path()).unwrap_err();
    let msg = err.to_string();
    assert!(
        !msg.contains('\u{1b}'),
        "el ESC de la clave no debe llegar al mensaje: {msg:?}"
    );
    assert!(
        matches!(&err, ConfigValueError::UnknownKey { key } if key.contains('\u{1b}')),
        "pero el campo `key` SIGUE llevando la clave cruda (uso programático): {err:?}"
    );
}

/// Una clave DESCONOCIDA "segura" (charset `[a-z0-9-]{1,32}`, la MISMA
/// exigencia que ya cumple cualquier clave declarada) SÍ se muestra —
/// mensajes útiles para el caso común, sin sacrificar seguridad.
#[test]
fn clave_desconocida_segura_si_aparece_en_el_mensaje() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_values(dir.path(), "no-declarada = \"x\"\n");
    let err = resolve_settings(&manifest, dir.path()).unwrap_err();
    assert!(
        err.to_string().contains("no-declarada"),
        "una clave charset-segura sí se muestra: {err}"
    );
}

/// Un fallo de parseo TOML sobre una fuente con un fragmento "secreto" no
/// debe filtrar ese fragmento en el mensaje — solo un resumen content-free.
#[test]
fn error_de_parseo_toml_no_filtra_el_fragmento_de_fuente() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let secreto = "hunter2-super-secret-marker";
    // Cadena SIN CERRAR: el parseo falla, pero el fragmento de la línea
    // (incluido el secreto) no debe aparecer en el mensaje.
    write_values(dir.path(), &format!("greeting = \"{secreto}\n"));
    let err = resolve_settings(&manifest, dir.path()).unwrap_err();
    let msg = err.to_string();
    assert!(
        !msg.contains(secreto),
        "el resumen content-free no debe llevar el fragmento de fuente: {msg:?}"
    );
    assert!(matches!(err, ConfigValueError::Toml { .. }));
}

/// `config.toml` que supera el tope de tamaño se rechaza ANTES de parsear.
#[test]
fn config_toml_demasiado_grande_se_rechaza() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    // Contenido dummy que supera el tope, aunque no sea TOML válido: el
    // tamaño se comprueba ANTES de intentar parsear.
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

/// `config.toml` justo EN el tope de tamaño sí se acepta (parsea como TOML
/// inválido, pero no por `TooLarge` — el tope es EXCLUSIVO por encima).
#[test]
fn config_toml_en_el_tope_exacto_no_es_too_large() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    // Comentario TOML válido relleno hasta EXACTAMENTE el tope.
    let cap_usize = usize::try_from(norte_plugin_host::CONFIG_VALUES_MAX_BYTES).unwrap();
    let mut src = "#".repeat(cap_usize);
    // Un comentario de puro `#` es TOML válido (línea vacía de comentario).
    src.truncate(cap_usize);
    write_values(dir.path(), &src);
    let result = resolve_settings(&manifest, dir.path());
    assert!(
        !matches!(result, Err(ConfigValueError::TooLarge { .. })),
        "{result:?}"
    );
}

/// Un valor `string` que supera `CONFIG_STRING_MAX_CHARS` se rechaza
/// nombrando la clave — mismo tope que un `default` (decisión 1), aplicado
/// también a overrides de usuario.
#[test]
fn valor_string_281_chars_se_rechaza_nombrando_la_clave() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let largo = "a".repeat(281);
    write_values(dir.path(), &format!("greeting = \"{largo}\"\n"));
    let err = resolve_settings(&manifest, dir.path()).unwrap_err();
    assert!(
        matches!(&err, ConfigValueError::ValueTooLong { key } if key == "greeting"),
        "{err:?}"
    );
}

/// Un valor `string` de exactamente 280 chars (el tope) SÍ se acepta.
#[test]
fn valor_string_280_chars_es_el_tope_exacto() {
    let manifest = Manifest::from_toml(WITH_CONFIG).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let limite = "a".repeat(280);
    write_values(dir.path(), &format!("greeting = \"{limite}\"\n"));
    let settings = resolve_settings(&manifest, dir.path()).unwrap();
    assert_eq!(settings.get("greeting"), Some(&limite));
}
