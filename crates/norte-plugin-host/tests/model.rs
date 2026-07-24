//! Modelo del host de plugins (ADR 0022, M4-P1): manifiesto, capabilities,
//! catálogo. Sin runtime WASM (M4-P2).

use norte_plugin_host::{
    COMMAND_ID_MAX_CHARS, COMMAND_TITLE_MAX_CHARS, CONFIG_DESCRIPTION_MAX_CHARS,
    CONFIG_ENUM_MAX_VALUES, CONFIG_MAX_KEYS, CONFIG_STRING_MAX_CHARS, Catalog, Category,
    ConfigKeySpec, Manifest, ManifestError, Scope,
};

const SYNTAX_PREVIEW: &str = r#"
[plugin]
id = "org.norte.syntax-preview"
name = "Syntax Preview"
publisher = "norte"
version = "0.1.0"
category = "previewer"

[contributions]
previewer = [{ mimetypes = ["text/*", "application/json"] }]

[capabilities]
fs-read = "scoped"
"#;

#[test]
fn manifiesto_completo_parsea() {
    let m = Manifest::from_toml(SYNTAX_PREVIEW).unwrap();
    assert_eq!(m.id, "org.norte.syntax-preview");
    assert_eq!(m.category, Category::Previewer);
    assert_eq!(m.contributions.previewer.len(), 1);
    assert_eq!(
        m.contributions.previewer[0].mimetypes,
        vec!["text/*", "application/json"]
    );
    assert_eq!(m.capabilities.fs_read, Scope::Scoped);
    assert_eq!(m.capabilities.fs_write, Scope::None);
    assert_eq!(m.capabilities.badges(), vec!["fs-read"]);
}

#[test]
fn capabilities_ausentes_son_none() {
    let m = Manifest::from_toml(
        r#"
        [plugin]
        id = "org.x.y"
        name = "Y"
        publisher = "x"
        version = "0.1.0"
        category = "command"
    "#,
    )
    .unwrap();
    assert_eq!(m.capabilities.fs_read, Scope::None);
    assert!(m.capabilities.badges().is_empty());
    assert!(m.capabilities.net.is_none());
}

#[test]
fn exec_distinto_de_none_se_rechaza() {
    let src = r#"
        [plugin]
        id = "org.evil.plugin"
        name = "Evil"
        publisher = "evil"
        version = "0.1.0"
        category = "command"
        [capabilities]
        exec = "shell"
    "#;
    assert!(matches!(
        Manifest::from_toml(src),
        Err(ManifestError::ExecForbidden)
    ));
    // `exec = "none"` explícito SÍ vale.
    let ok = src.replace(r#"exec = "shell""#, r#"exec = "none""#);
    assert!(Manifest::from_toml(&ok).is_ok());
}

#[test]
fn id_no_reverse_dns_se_rechaza() {
    let src = r#"
        [plugin]
        id = "sinpunto"
        name = "N"
        publisher = "p"
        version = "0.1.0"
        category = "hook"
    "#;
    assert!(matches!(Manifest::from_toml(src), Err(ManifestError::Id)));
}

#[test]
fn id_charset_reverse_dns_estricto() {
    // `id_literal` = el texto EXACTO del valor TOML (ya escapado). Permite meter
    // `\n` (escape TOML → salto de línea real en el valor) o `\"` (comilla).
    let with_id = |id_literal: &str| {
        format!(
            r#"
        [plugin]
        id = {id_literal}
        name = "N"
        publisher = "p"
        version = "0.1.0"
        category = "hook"
    "#
        )
    };

    // Ids válidos: segmentos alfanuméricos con guiones, con al menos un punto.
    assert!(Manifest::from_toml(&with_id(r#""org.norte.demo""#)).is_ok());
    assert!(Manifest::from_toml(&with_id(r#""org.foo-bar.baz""#)).is_ok());

    // Ids hostiles que SÍ parsean como TOML pero fallan el charset ⇒
    // `ManifestError::Id` (no llegan al log ni al modal de aprobación T5):
    //  - `\n` (escape TOML) = salto de línea real en el valor → inyección de log.
    //  - `\"` (escape TOML) = comilla en el valor → spoofing del diálogo.
    //  - espacios, guion-bajo, no-ASCII, punto inicial/final, segmento vacío.
    for bad_literal in [
        r#""org.norte.de\nmo""#,
        r#""org.\"norte\".demo""#,
        r#""org norte demo""#,
        r#""org.norte.de_mo""#,
        r#""org.norte.デモ""#,
        r#"".org.norte""#,
        r#""org.norte.""#,
        r#""org..norte""#,
        r#""orgnorte""#,
    ] {
        assert!(
            matches!(
                Manifest::from_toml(&with_id(bad_literal)),
                Err(ManifestError::Id)
            ),
            "id hostil debe rechazarse como Id: {bad_literal}"
        );
    }

    // Longitud total > 128 se rechaza.
    let long = format!(r#""org.norte.{}""#, "a".repeat(120));
    assert!(matches!(
        Manifest::from_toml(&with_id(&long)),
        Err(ManifestError::Id)
    ));
}

#[test]
fn description_ausente_es_none() {
    let m = Manifest::from_toml(SYNTAX_PREVIEW).unwrap();
    assert_eq!(m.description, None);
}

#[test]
fn description_presente_se_parsea() {
    let src = r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
        description = "Genera previews de Markdown en línea."
    "#;
    let m = Manifest::from_toml(src).unwrap();
    assert_eq!(
        m.description.as_deref(),
        Some("Genera previews de Markdown en línea.")
    );
}

#[test]
fn description_280_chars_es_el_tope_exacto() {
    let d = "a".repeat(280);
    let src = format!(
        r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
        description = "{d}"
    "#
    );
    let m = Manifest::from_toml(&src).unwrap();
    assert_eq!(m.description.as_deref(), Some(d.as_str()));
}

#[test]
fn description_281_chars_se_rechaza() {
    let d = "a".repeat(281);
    let src = format!(
        r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
        description = "{d}"
    "#
    );
    assert!(matches!(
        Manifest::from_toml(&src),
        Err(ManifestError::DescriptionTooLong)
    ));
}

#[test]
fn description_cuenta_caracteres_no_bytes() {
    // 280 caracteres NO-ASCII (multi-byte en UTF-8): el tope es de CHARS, no de
    // bytes, o un manifiesto legítimo en un idioma no-ASCII se rechazaría antes
    // de tiempo.
    let d = "á".repeat(280);
    let src = format!(
        r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
        description = "{d}"
    "#
    );
    assert!(Manifest::from_toml(&src).is_ok());
}

#[test]
fn description_editada_no_mueve_el_approval_digest() {
    // Precedente de manifest.rs:296-307 (name/publisher/version cosméticos):
    // description es TAMBIÉN cosmética — editarla NO debe reinvalidar
    // capabilities ya aprobadas por el humano.
    let base = |desc: Option<&str>| {
        let d = desc.map_or_else(String::new, |d| format!(r#"description = "{d}""#));
        Manifest::from_toml(&format!(
            r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
        {d}
        [capabilities]
        fs-read = "scoped"
    "#
        ))
        .unwrap()
    };
    let sin_desc = base(None);
    let con_desc = base(Some("Una descripción cualquiera."));
    let con_otra_desc = base(Some("Una descripción TOTALMENTE distinta."));
    assert_eq!(
        sin_desc.approval_digest(),
        con_desc.approval_digest(),
        "añadir description no debe mover el digest"
    );
    assert_eq!(
        con_desc.approval_digest(),
        con_otra_desc.approval_digest(),
        "editar description no debe mover el digest"
    );
}

/// P1 encoding audit M2: manifiesto con UN `contributions.command`, `id`/
/// `title` parametrizados — para probar los topes 120/64 (chars) sin
/// repetir el boilerplate del `[plugin]`.
fn manifest_con_comando(id: &str, title: &str) -> Result<Manifest, ManifestError> {
    Manifest::from_toml(&format!(
        r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
        [contributions]
        command = [{{ id = "{id}", title = "{title}" }}]
    "#
    ))
}

#[test]
fn command_title_120_chars_es_el_tope_exacto() {
    let title = "a".repeat(COMMAND_TITLE_MAX_CHARS);
    let m = manifest_con_comando("cmd", &title).unwrap();
    assert_eq!(m.contributions.command[0].title, title);
}

#[test]
fn command_title_121_chars_se_rechaza() {
    let title = "a".repeat(COMMAND_TITLE_MAX_CHARS + 1);
    assert!(matches!(
        manifest_con_comando("cmd", &title),
        Err(ManifestError::CommandTitleTooLong)
    ));
}

#[test]
fn command_id_64_chars_es_el_tope_exacto() {
    let id = "a".repeat(COMMAND_ID_MAX_CHARS);
    let m = manifest_con_comando(&id, "Title").unwrap();
    assert_eq!(m.contributions.command[0].id, id);
}

#[test]
fn command_id_65_chars_se_rechaza() {
    let id = "a".repeat(COMMAND_ID_MAX_CHARS + 1);
    assert!(matches!(
        manifest_con_comando(&id, "Title"),
        Err(ManifestError::CommandIdTooLong)
    ));
}

/// El tope es de PARSEO, no de digest: `title` SÍ entra en `approval_digest`
/// (decide cuándo/cómo se dispara el comando), pero eso ya estaba probado
/// por `approval_digest_incluye_category_y_contributions_no_solo_capabilities`
/// — el tope NUEVO solo rechaza manifiestos NUEVOS que lo excedan, jamás
/// reinterpreta un digest ya calculado para uno viejo dentro del tope (el
/// digest hashea el VALOR de `title`, no el tope contra el que se validó al
/// parsear).
#[test]
fn command_dentro_del_tope_no_cambia_el_criterio_del_digest() {
    let a = manifest_con_comando("cmd", "Title A").unwrap();
    let b = manifest_con_comando("cmd", "Title B").unwrap();
    assert_ne!(
        a.approval_digest(),
        b.approval_digest(),
        "title distinto SÍ debe mover el digest (no es cosmético como description)"
    );
}

#[test]
fn net_capability_lista_hosts() {
    let m = Manifest::from_toml(
        r#"
        [plugin]
        id = "org.norte.webdav"
        name = "WebDAV"
        publisher = "norte"
        version = "0.1.0"
        category = "provider"
        [contributions]
        provider = [{ scheme = "webdav" }]
        [capabilities]
        net = { hosts = ["dav.example.com"] }
    "#,
    )
    .unwrap();
    assert_eq!(m.contributions.provider[0].scheme, "webdav");
    assert_eq!(
        m.capabilities.net.as_ref().unwrap().hosts,
        ["dav.example.com"]
    );
    assert_eq!(m.capabilities.badges(), vec!["net"]);
}

fn write_plugin(root: &std::path::Path, id: &str, toml: &str) {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("plugin.toml"), toml).unwrap();
    // el .wasm real llega en M4-P2; el catálogo solo exige el manifiesto.
}

#[test]
fn catalogo_descubre_ordena_y_agrupa() {
    let root = tempfile::tempdir().unwrap();
    write_plugin(root.path(), "org.norte.syntax-preview", SYNTAX_PREVIEW);
    write_plugin(
        root.path(),
        "org.norte.bulk-rename",
        r#"
        [plugin]
        id = "org.norte.bulk-rename"
        name = "Bulk Rename"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
        [capabilities]
        fs-write = "scoped"
    "#,
    );
    // Uno inválido: no debe desaparecer en silencio, va a `errors`.
    write_plugin(
        root.path(),
        "org.bad.exec",
        r#"
        [plugin]
        id = "org.bad.exec"
        name = "Bad"
        publisher = "bad"
        version = "0.1.0"
        category = "command"
        [capabilities]
        exec = "shell"
    "#,
    );

    let cat = Catalog::load_dir(root.path());
    assert_eq!(cat.plugins.len(), 2, "dos válidos");
    assert_eq!(
        cat.errors.len(),
        1,
        "el exec-shell va a errors, no se oculta"
    );

    // Agrupado por categoría, en orden (command antes que previewer no —
    // previewer va primero en el ORDER).
    let groups = cat.by_category();
    let cats: Vec<Category> = groups.iter().map(|(c, _)| *c).collect();
    assert_eq!(cats, vec![Category::Previewer, Category::Command]);
    assert_eq!(groups[1].1[0].manifest.id, "org.norte.bulk-rename");
}

#[test]
fn catalogo_dir_inexistente_es_vacio() {
    let cat = Catalog::load_dir(std::path::Path::new("/no/existe/seguro/norte"));
    assert!(cat.plugins.is_empty() && cat.errors.is_empty());
}

#[test]
fn approval_digest_incluye_category_y_contributions_no_solo_capabilities() {
    // Issue #69 (MINOR 1): el digest de aprobación cubre category + contributions
    // (cuándo/cómo se dispara), no solo [capabilities]. Un manifiesto reeditado
    // que cambie esos campos MANTENIENDO las capabilities debe mover el digest
    // (→ re-consentimiento fail-closed), o pasaría a auto-dispararse sin aprobar.
    let command = Manifest::from_toml(
        r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
        [capabilities]
        fs-read = "scoped"
    "#,
    )
    .unwrap();

    // MISMAS capabilities, pero category previewer (con un entry de mimetypes).
    let previewer = Manifest::from_toml(
        r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "previewer"
        [contributions]
        previewer = [{ mimetypes = ["text/*"] }]
        [capabilities]
        fs-read = "scoped"
    "#,
    )
    .unwrap();

    // MISMA category previewer + mismas capabilities, pero mimetypes AMPLIADOS.
    let previewer_wide = Manifest::from_toml(
        r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "previewer"
        [contributions]
        previewer = [{ mimetypes = ["text/*", "application/*"] }]
        [capabilities]
        fs-read = "scoped"
    "#,
    )
    .unwrap();

    assert_eq!(
        command.capabilities.digest(),
        previewer.capabilities.digest(),
        "las capabilities son idénticas (control del test)"
    );
    assert_ne!(
        command.approval_digest(),
        previewer.approval_digest(),
        "cambiar la category mueve el digest de aprobación"
    );
    assert_ne!(
        previewer.approval_digest(),
        previewer_wide.approval_digest(),
        "ampliar los mimetypes del entry mueve el digest de aprobación"
    );
    // Determinista y estable para un manifiesto dado.
    assert_eq!(command.approval_digest(), command.approval_digest());
}

#[test]
fn catalogo_rechaza_ids_duplicados_en_dos_directorios() {
    // Issue #69: dos directorios distintos declaran el MISMO `plugin.id`. Un
    // segundo dir no puede reclamar la aprobación del primero para colar su
    // `plugin.wasm`. Se rechazan AMBOS (fail-closed), no se elige "el primero".
    let root = tempfile::tempdir().unwrap();
    let dupe = r#"
        [plugin]
        id = "org.norte.clash"
        name = "Clash"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
    "#;
    // Dos subdirectorios con nombres distintos pero el mismo id declarado.
    write_plugin(root.path(), "dir-a", dupe);
    write_plugin(root.path(), "dir-b", dupe);
    // Y uno legítimo con id único: no debe verse afectado por la colisión ajena.
    write_plugin(
        root.path(),
        "solo",
        r#"
        [plugin]
        id = "org.norte.solo"
        name = "Solo"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
    "#,
    );

    let cat = Catalog::load_dir(root.path());
    assert_eq!(
        cat.plugins.len(),
        1,
        "solo el id único carga; los colisionantes se rechazan"
    );
    assert_eq!(cat.plugins[0].manifest.id, "org.norte.solo");
    assert_eq!(cat.errors.len(), 2, "ambos directorios del id duplicado");
    assert!(
        cat.errors
            .iter()
            .all(|e| matches!(&e.error, ManifestError::DuplicateId(id) if id == "org.norte.clash")),
        "los dos errores son DuplicateId del id colisionante"
    );
}

/// P2: pin de no-regresión. El digest de `SYNTAX_PREVIEW` (sin `[config]`)
/// capturado ANTES de introducir el esquema `[config]` en la forma canónica
/// del digest (commit previo a este). Si este test se rompe, la extensión de
/// P2 movió el digest de un manifiesto SIN `[config]` — eso resetearía TODAS
/// las aprobaciones humanas existentes de plugins que no usan `[config]`,
/// que es exactamente lo que la decisión 2 del plan P2 prohíbe.
#[test]
fn manifest_sin_config_digesta_identico_a_pre_p2() {
    const DIGEST_PRE_P2: &str = "9ba598fcee4cb10e91a2de3683287a11af83c9bdd7bc18ecb2df79570f9c0d5f";
    let m = Manifest::from_toml(SYNTAX_PREVIEW).unwrap();
    assert_eq!(
        m.approval_digest(),
        DIGEST_PRE_P2,
        "un manifiesto sin [config] debe digestar IGUAL que antes de P2 \
         (o resetea aprobaciones existentes)"
    );
}

// --- P2: esquema `[config]` del manifiesto (dentro del approval digest) ---

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
description = "Saludo mostrado al arrancar."

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

/// Boilerplate mínimo de `[plugin]` + las entradas `[config.*]` que se le
/// inyecten, para probar los topes de P2 sin repetir el resto del manifiesto.
fn manifest_con_config(entries: &str) -> Result<Manifest, ManifestError> {
    Manifest::from_toml(&format!(
        r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
        {entries}
    "#
    ))
}

#[test]
fn config_los_4_tipos_parsean() {
    let m = Manifest::from_toml(WITH_CONFIG).unwrap();
    assert_eq!(m.config.len(), 4);
    assert_eq!(
        m.config.get("greeting"),
        Some(&ConfigKeySpec::String {
            default: "hola".into(),
            description: Some("Saludo mostrado al arrancar.".into()),
        })
    );
    assert_eq!(
        m.config.get("enabled"),
        Some(&ConfigKeySpec::Bool {
            default: true,
            description: None,
        })
    );
    assert_eq!(
        m.config.get("retries"),
        Some(&ConfigKeySpec::Int {
            default: 3,
            min: Some(0),
            max: Some(10),
            description: None,
        })
    );
    assert_eq!(
        m.config.get("mode"),
        Some(&ConfigKeySpec::Enum {
            default: "fast".into(),
            values: vec!["fast".into(), "slow".into()],
            description: None,
        })
    );
}

#[test]
fn config_ausente_es_mapa_vacio() {
    let m = Manifest::from_toml(SYNTAX_PREVIEW).unwrap();
    assert!(m.config.is_empty());
}

#[test]
fn config_33_claves_se_rechaza() {
    use std::fmt::Write as _;
    let mut entries = String::new();
    for i in 0..=CONFIG_MAX_KEYS {
        let _ = write!(
            entries,
            "\n[config.k{i}]\ntype = \"bool\"\ndefault = true\n"
        );
    }
    assert!(matches!(
        manifest_con_config(&entries),
        Err(ManifestError::ConfigTooManyKeys)
    ));
}

#[test]
fn config_32_claves_es_el_tope_exacto() {
    use std::fmt::Write as _;
    let mut entries = String::new();
    for i in 0..CONFIG_MAX_KEYS {
        let _ = write!(
            entries,
            "\n[config.k{i}]\ntype = \"bool\"\ndefault = true\n"
        );
    }
    let m = manifest_con_config(&entries).unwrap();
    assert_eq!(m.config.len(), CONFIG_MAX_KEYS);
}

#[test]
fn config_clave_con_mayuscula_se_rechaza() {
    let entries = "\n[config.Bad]\ntype = \"bool\"\ndefault = true\n";
    assert!(matches!(
        manifest_con_config(entries),
        Err(ManifestError::ConfigKeyCharset)
    ));
}

#[test]
fn config_clave_con_guion_bajo_se_rechaza() {
    let entries = "\n[config.has_underscore]\ntype = \"bool\"\ndefault = true\n";
    assert!(matches!(
        manifest_con_config(entries),
        Err(ManifestError::ConfigKeyCharset)
    ));
}

#[test]
fn config_clave_33_chars_se_rechaza() {
    let key = "a".repeat(33);
    let entries = format!("\n[config.{key}]\ntype = \"bool\"\ndefault = true\n");
    assert!(matches!(
        manifest_con_config(&entries),
        Err(ManifestError::ConfigKeyCharset)
    ));
}

#[test]
fn config_clave_32_chars_es_el_tope_exacto() {
    let key = "a".repeat(32);
    let entries = format!("\n[config.{key}]\ntype = \"bool\"\ndefault = true\n");
    let m = manifest_con_config(&entries).unwrap();
    assert!(m.config.contains_key(&key));
}

#[test]
fn config_description_281_chars_se_rechaza() {
    let d = "a".repeat(CONFIG_DESCRIPTION_MAX_CHARS + 1);
    let entries = format!(
        "\n[config.greeting]\ntype = \"string\"\ndefault = \"hi\"\ndescription = \"{d}\"\n"
    );
    assert!(matches!(
        manifest_con_config(&entries),
        Err(ManifestError::ConfigDescriptionTooLong)
    ));
}

#[test]
fn config_description_280_chars_es_el_tope_exacto() {
    let d = "a".repeat(CONFIG_DESCRIPTION_MAX_CHARS);
    let entries = format!(
        "\n[config.greeting]\ntype = \"string\"\ndefault = \"hi\"\ndescription = \"{d}\"\n"
    );
    let m = manifest_con_config(&entries).unwrap();
    assert_eq!(
        m.config.get("greeting"),
        Some(&ConfigKeySpec::String {
            default: "hi".into(),
            description: Some(d),
        })
    );
}

#[test]
fn config_string_default_281_chars_se_rechaza() {
    let d = "a".repeat(CONFIG_STRING_MAX_CHARS + 1);
    let entries = format!("\n[config.greeting]\ntype = \"string\"\ndefault = \"{d}\"\n");
    assert!(matches!(
        manifest_con_config(&entries),
        Err(ManifestError::ConfigDefaultTooLong)
    ));
}

#[test]
fn config_int_default_por_encima_del_maximo_se_rechaza() {
    let entries = "\n[config.retries]\ntype = \"int\"\ndefault = 20\nmin = 0\nmax = 10\n";
    assert!(matches!(
        manifest_con_config(entries),
        Err(ManifestError::ConfigIntDefaultOutOfRange)
    ));
}

#[test]
fn config_int_default_bajo_el_minimo_se_rechaza() {
    let entries = "\n[config.retries]\ntype = \"int\"\ndefault = -1\nmin = 0\nmax = 10\n";
    assert!(matches!(
        manifest_con_config(entries),
        Err(ManifestError::ConfigIntDefaultOutOfRange)
    ));
}

#[test]
fn config_int_default_en_el_borde_es_valido() {
    let entries = "\n[config.retries]\ntype = \"int\"\ndefault = 10\nmin = 0\nmax = 10\n";
    let m = manifest_con_config(entries).unwrap();
    assert_eq!(
        m.config.get("retries"),
        Some(&ConfigKeySpec::Int {
            default: 10,
            min: Some(0),
            max: Some(10),
            description: None,
        })
    );
}

#[test]
fn config_enum_default_ausente_de_values_se_rechaza() {
    let entries =
        "\n[config.mode]\ntype = \"enum\"\ndefault = \"turbo\"\nvalues = [\"fast\", \"slow\"]\n";
    assert!(matches!(
        manifest_con_config(entries),
        Err(ManifestError::ConfigEnumDefaultNotInValues)
    ));
}

#[test]
fn config_enum_17_values_se_rechaza() {
    let values: Vec<String> = (0..=CONFIG_ENUM_MAX_VALUES)
        .map(|i| format!("\"v{i}\""))
        .collect();
    let entries = format!(
        "\n[config.mode]\ntype = \"enum\"\ndefault = \"v0\"\nvalues = [{}]\n",
        values.join(", ")
    );
    assert!(matches!(
        manifest_con_config(&entries),
        Err(ManifestError::ConfigEnumTooManyValues)
    ));
}

#[test]
fn config_enum_16_values_es_el_tope_exacto() {
    let values: Vec<String> = (0..CONFIG_ENUM_MAX_VALUES)
        .map(|i| format!("\"v{i}\""))
        .collect();
    let entries = format!(
        "\n[config.mode]\ntype = \"enum\"\ndefault = \"v0\"\nvalues = [{}]\n",
        values.join(", ")
    );
    let m = manifest_con_config(&entries).unwrap();
    match m.config.get("mode").unwrap() {
        ConfigKeySpec::Enum { values, .. } => assert_eq!(values.len(), CONFIG_ENUM_MAX_VALUES),
        other => panic!("se esperaba Enum, se obtuvo {other:?}"),
    }
}

#[test]
fn config_enum_value_281_chars_se_rechaza() {
    let long_value = "a".repeat(CONFIG_STRING_MAX_CHARS + 1);
    let entries = format!(
        "\n[config.mode]\ntype = \"enum\"\ndefault = \"{long_value}\"\nvalues = [\"{long_value}\"]\n"
    );
    assert!(matches!(
        manifest_con_config(&entries),
        Err(ManifestError::ConfigEnumValueTooLong)
    ));
}

#[test]
fn config_presente_mueve_el_approval_digest() {
    let sin_config = manifest_con_config("").unwrap();
    let con_config =
        manifest_con_config("\n[config.greeting]\ntype = \"string\"\ndefault = \"hola\"\n")
            .unwrap();
    assert_ne!(
        sin_config.approval_digest(),
        con_config.approval_digest(),
        "declarar [config] debe mover el digest de aprobación (decisión 2)"
    );
}

#[test]
fn config_default_distinto_mueve_el_approval_digest() {
    let a = manifest_con_config("\n[config.greeting]\ntype = \"string\"\ndefault = \"hola\"\n")
        .unwrap();
    let b = manifest_con_config("\n[config.greeting]\ntype = \"string\"\ndefault = \"adios\"\n")
        .unwrap();
    assert_ne!(
        a.approval_digest(),
        b.approval_digest(),
        "un default distinto es comportamiento distinto: debe mover el digest"
    );
}

#[test]
fn config_description_editada_no_mueve_el_approval_digest() {
    // Mismo criterio que `plugin.description` (cosmética): editarla no
    // reinvalida capabilities ya aprobadas.
    let a = manifest_con_config(
        "\n[config.greeting]\ntype = \"string\"\ndefault = \"hola\"\ndescription = \"uno\"\n",
    )
    .unwrap();
    let b = manifest_con_config(
        "\n[config.greeting]\ntype = \"string\"\ndefault = \"hola\"\ndescription = \"dos, muy distinta\"\n",
    )
    .unwrap();
    assert_eq!(
        a.approval_digest(),
        b.approval_digest(),
        "editar la description de una clave de config no debe mover el digest"
    );
}

#[test]
fn config_tabla_vacia_digesta_igual_que_ausente() {
    // decisión 2: la sección `config:` solo se añade al digest si el mapa NO
    // está vacío — una tabla `[config]` presente pero sin claves debe digestar
    // igual que su ausencia total.
    let sin_tabla = manifest_con_config("").unwrap();
    let tabla_vacia = manifest_con_config("\n[config]\n").unwrap();
    assert_eq!(sin_tabla.approval_digest(), tabla_vacia.approval_digest());
}
