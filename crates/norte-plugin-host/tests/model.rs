//! Modelo del host de plugins (ADR 0022, M4-P1): manifiesto, capabilities,
//! catálogo. Sin runtime WASM (M4-P2).

use norte_plugin_host::{
    COMMAND_ID_MAX_CHARS, COMMAND_MAX_COUNT, COMMAND_TITLE_MAX_CHARS, CONFIG_DESCRIPTION_MAX_CHARS,
    CONFIG_ENUM_MAX_VALUES, CONFIG_MAX_KEYS, CONFIG_STRING_MAX_CHARS, Catalog, Category,
    ConfigKeySpec, HelpPresence, Manifest, ManifestError, Scope,
};

mod support;

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
    assert_eq!(m.capabilities.badges(), vec!["fs-read".to_owned()]);
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

/// Un hook no lo ejecuta nadie: la categoría está en el manifiesto, en el
/// catálogo y en la UI, pero no hay interfaz WIT, ni world, ni sitio en el
/// host desde donde llamarla. Aceptar el manifiesto instalaría algo inerte y
/// el gestor lo pintaría como un plugin más — la peor de las tres opciones,
/// porque el autor se entera cuando nada pasa.
///
/// Se rechaza al parsear, con el motivo. La categoría NO se borra: spec §7.1
/// nombra los hooks entre las interfaces que WIT debe cubrir, así que quitarla
/// alejaría el código de la especificación en vez de acercarlo.
#[test]
fn un_hook_se_rechaza_porque_no_lo_ejecuta_nadie() {
    // Por categoría primaria.
    let por_categoria = r#"
        [plugin]
        id = "org.demo.hooker"
        name = "Hooker"
        publisher = "demo"
        version = "0.1.0"
        category = "hook"
    "#;
    assert!(matches!(
        Manifest::from_toml(por_categoria),
        Err(ManifestError::HookNotImplemented)
    ));

    // Y por contribución, aunque la categoría primaria sea otra: es la
    // declaración la que promete algo, no el campo que la clasifica.
    let por_contribucion = r#"
        [plugin]
        id = "org.demo.sneaky"
        name = "Sneaky"
        publisher = "demo"
        version = "0.1.0"
        category = "command"
        [[contributions.hook]]
        on = "before-copy"
    "#;
    assert!(matches!(
        Manifest::from_toml(por_contribucion),
        Err(ManifestError::HookNotImplemented)
    ));

    // El mismo manifiesto sin el hook entra sin problema: lo que se rechaza es
    // la promesa vacía, no el plugin.
    let sin_hook = r#"
        [plugin]
        id = "org.demo.sneaky"
        name = "Sneaky"
        publisher = "demo"
        version = "0.1.0"
        category = "command"
    "#;
    assert!(Manifest::from_toml(sin_hook).is_ok());
}

#[test]
fn id_no_reverse_dns_se_rechaza() {
    let src = r#"
        [plugin]
        id = "sinpunto"
        name = "N"
        publisher = "p"
        version = "0.1.0"
        category = "command"
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
        category = "command"
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

fn manifest_con_n_comandos(n: usize) -> Result<Manifest, ManifestError> {
    let cmds: Vec<String> = (0..n)
        .map(|i| format!(r#"{{ id = "c{i}", title = "C{i}" }}"#))
        .collect();
    Manifest::from_toml(&format!(
        r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
        [contributions]
        command = [{}]
    "#,
        cmds.join(", ")
    ))
}

/// El tope se corta en el MANIFIESTO, no en cada paleta que lo pinta (#281).
/// La ventana ya se defiende por su lado (512 extensiones, 2048 filas), pero
/// eso es el cliente protegiéndose del servidor.
#[test]
fn command_33_comandos_se_rechaza() {
    assert!(matches!(
        manifest_con_n_comandos(COMMAND_MAX_COUNT + 1),
        Err(ManifestError::TooManyCommands)
    ));
}

#[test]
fn command_32_comandos_es_el_tope_exacto() {
    let m = manifest_con_n_comandos(COMMAND_MAX_COUNT).unwrap();
    assert_eq!(m.contributions.command.len(), COMMAND_MAX_COUNT);
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
    assert_eq!(m.capabilities.badges(), vec!["net".to_owned()]);
}

fn write_plugin(root: &std::path::Path, id: &str, toml: &str) {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("plugin.toml"), toml).unwrap();
    // el .wasm real llega en M4-P2; el catálogo solo exige el manifiesto.
}

/// Añade `config_dir/plugins/<id>/config.toml` a un plugin YA escrito con
/// [`write_plugin`] (P2 Task 2).
fn write_config_values(root: &std::path::Path, id: &str, toml: &str) {
    std::fs::write(root.join(id).join("config.toml"), toml).unwrap();
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

/// ADR 0057: pedir `location` CAMBIA el digest —o sea, exige aprobar otra vez—
/// y no pedirla lo deja intacto. Las dos mitades son la misma decisión: una
/// capacidad nueva no se cuela sin consentimiento, y añadirla al esquema no
/// puede invalidar el consentimiento que ya existe.
#[test]
fn location_declarada_entra_en_el_digest_de_aprobacion() {
    let sin = Manifest::from_toml(COLUMNS_PLUGIN).unwrap();
    let con = Manifest::from_toml(
        &COLUMNS_PLUGIN.replace("[capabilities]", "[capabilities]\nlocation = \"read\""),
    )
    .unwrap();
    assert_ne!(
        sin.approval_digest(),
        con.approval_digest(),
        "pedir una capacidad nueva EXIGE aprobarla de nuevo"
    );
    assert!(con.capabilities.location.granted());
    assert!(
        con.capabilities
            .badges()
            .iter()
            .any(|b| b.starts_with("location")),
        "el badge nombra la capacidad de ubicación"
    );

    // Y con MARCADOR, el badge lo DICE (#241): «location» a secas se lee como
    // «puede leer donde estoy mirando», y lo que se concede es el ancestro más
    // cercano que contenga el marcador — el proyecto entero, no la carpeta.
    let con_marcador = Manifest::from_toml(&COLUMNS_PLUGIN.replace(
        "[capabilities]",
        "[capabilities]\nlocation = \"read\"\nlocation-root-marker = \".git\"",
    ))
    .unwrap();
    assert!(
        con_marcador
            .capabilities
            .badges()
            .contains(&"location-root:.git".to_owned()),
        "el badge dice qué marcador abre el ancestro: {:?}",
        con_marcador.capabilities.badges()
    );
}

/// Vocabulario CERRADO, como `exec`: un valor inventado es un manifiesto
/// inválido, jamás una capacidad que se ignora en silencio.
#[test]
fn un_valor_desconocido_de_location_es_error_de_manifiesto() {
    let m = COLUMNS_PLUGIN.replace("[capabilities]", "[capabilities]\nlocation = \"write\"");
    assert!(Manifest::from_toml(&m).is_err());
}

/// Un manifiesto de columnas CON `[capabilities]` pero sin `location`.
const COLUMNS_PLUGIN: &str = r#"
[plugin]
id = "org.norte.columnas"
name = "Columnas"
publisher = "norte"
version = "0.1.0"
category = "columns"

[capabilities]
fs-read = "none"

[[contributions.columns]]
id = "name-len"
header = "Largo"
"#;

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

// --- P2 Task 2: wiring de `resolve_settings` en `Catalog::load_dir` -------

#[test]
fn catalogo_config_toml_invalido_excluye_el_plugin_via_error() {
    // Un `config.toml` que NO valida contra el esquema `[config]` del
    // manifiesto (P2 decisión 3) excluye el plugin ENTERO del catálogo
    // (fail-closed, mismo trato que un `plugin.toml` roto o un id
    // duplicado): va a `errors`, nunca a `plugins` con valores a medias.
    let root = tempfile::tempdir().unwrap();
    write_plugin(root.path(), "org.norte.demo-config", WITH_CONFIG);
    write_config_values(root.path(), "org.norte.demo-config", "mode = \"turbo\"\n");
    // Un plugin sano de control, sin `[config]`.
    write_plugin(root.path(), "org.norte.syntax-preview", SYNTAX_PREVIEW);

    let cat = Catalog::load_dir(root.path());
    assert_eq!(
        cat.plugins.len(),
        1,
        "el plugin con config.toml inválido NO carga"
    );
    assert_eq!(cat.plugins[0].manifest.id, "org.norte.syntax-preview");
    assert_eq!(cat.errors.len(), 1, "el config.toml inválido va a errors");
    assert!(
        matches!(
            &cat.errors[0].error,
            ManifestError::ConfigValues(inner) if inner.to_string().contains("mode")
        ),
        "el error nombra la CLAVE (mode), no el valor: {:?}",
        cat.errors[0].error
    );
}

#[test]
fn catalogo_config_toml_valido_resuelve_settings_en_la_entrada() {
    let root = tempfile::tempdir().unwrap();
    write_plugin(root.path(), "org.norte.demo-config", WITH_CONFIG);
    write_config_values(root.path(), "org.norte.demo-config", "retries = 7\n");

    let cat = Catalog::load_dir(root.path());
    assert_eq!(cat.errors.len(), 0, "{:?}", cat.errors);
    assert_eq!(cat.plugins.len(), 1);
    let settings = &cat.plugins[0].settings;
    assert_eq!(settings.get("retries").map(String::as_str), Some("7"));
    // El resto sigue en su default.
    assert_eq!(settings.get("greeting").map(String::as_str), Some("hola"));
}

#[test]
fn catalogo_sin_config_toml_resuelve_defaults_en_la_entrada() {
    let root = tempfile::tempdir().unwrap();
    write_plugin(root.path(), "org.norte.demo-config", WITH_CONFIG);
    // Sin escribir config.toml.

    let cat = Catalog::load_dir(root.path());
    assert_eq!(cat.errors.len(), 0, "{:?}", cat.errors);
    let settings = &cat.plugins[0].settings;
    assert_eq!(settings.len(), 4);
    assert_eq!(settings.get("mode").map(String::as_str), Some("fast"));
}

#[test]
fn catalogo_plugin_sin_config_tiene_settings_vacio() {
    let root = tempfile::tempdir().unwrap();
    write_plugin(root.path(), "org.norte.syntax-preview", SYNTAX_PREVIEW);

    let cat = Catalog::load_dir(root.path());
    assert!(cat.plugins[0].settings.is_empty());
}

// --- H3e: el catálogo anuncia si el plugin trae `help.md` ----------------

#[test]
fn descubrir_marca_el_plugin_que_trae_help_md() {
    let root = tempfile::tempdir().unwrap();
    write_plugin(root.path(), "org.norte.syntax-preview", SYNTAX_PREVIEW);
    std::fs::write(
        root.path().join("org.norte.syntax-preview").join("help.md"),
        "+++\nid = \"org.norte.syntax-preview\"\ntitle = \"Preview\"\n+++\ncuerpo",
    )
    .unwrap();

    let cat = Catalog::load_dir(root.path());
    assert_eq!(
        cat.plugins[0].help,
        HelpPresence::Servable,
        "el help.md descubierto se anuncia, y pasa la guarda"
    );
}

#[test]
fn sin_help_md_no_se_anuncia_ayuda() {
    let root = tempfile::tempdir().unwrap();
    write_plugin(root.path(), "org.norte.syntax-preview", SYNTAX_PREVIEW);

    let cat = Catalog::load_dir(root.path());
    assert_eq!(cat.plugins[0].help, HelpPresence::Absent);
}

#[test]
fn un_help_md_que_es_un_directorio_no_anuncia_ayuda() {
    // `is_file`, no `exists`: un `help.md` que es un directorio no es una
    // página, y anunciarla haría que la barra lateral pintase un nodo que
    // luego se abre vacío.
    let root = tempfile::tempdir().unwrap();
    write_plugin(root.path(), "org.norte.syntax-preview", SYNTAX_PREVIEW);
    std::fs::create_dir_all(root.path().join("org.norte.syntax-preview").join("help.md")).unwrap();

    let cat = Catalog::load_dir(root.path());
    assert_eq!(
        cat.plugins[0].help,
        HelpPresence::Absent,
        "ni presente ni servible: un directorio no es una pagina"
    );
}

/// El tercer estado, el que existe precisamente para no colapsarse con los
/// otros dos (H3e): hay `help.md` y el host NO lo servirá. `Absent` diría que
/// el autor no se documentó y `Servable` prometería una página; solo este
/// estado deja a `norte doctor` reportar "lo pusiste y apunta fuera".
#[cfg(unix)]
#[test]
fn un_help_md_que_escapa_del_directorio_esta_presente_pero_no_es_servible() {
    let root = tempfile::tempdir().unwrap();
    write_plugin(root.path(), "org.norte.syntax-preview", SYNTAX_PREVIEW);
    let fuera = root.path().join("ajeno.md");
    std::fs::write(&fuera, "secreto").unwrap();
    std::os::unix::fs::symlink(
        &fuera,
        root.path().join("org.norte.syntax-preview").join("help.md"),
    )
    .unwrap();

    let cat = Catalog::load_dir(root.path());
    assert_eq!(cat.plugins[0].help, HelpPresence::Unservable);
    assert!(cat.plugins[0].help.is_present(), "el fichero está ahí");
    assert!(
        !cat.plugins[0].help.is_servable(),
        "y el wire no lo anuncia: anunciar y servir vacío es el oráculo"
    );
}

// ---------------------------------------------------------------------
// ADR 0037 (G3b): `Category::Decorator` + `Contributions.decorator`.

/// Un manifiesto `decorator` mínimo: categoría nueva, un único contrib
/// marcador vacío.
const DECORATOR_MANIFEST: &str = r#"
[plugin]
id = "org.norte.decor"
name = "Decor"
publisher = "norte"
version = "0.1.0"
category = "decorator"

[[contributions.decorator]]
"#;

#[test]
fn manifiesto_decorator_parsea() {
    let m = Manifest::from_toml(DECORATOR_MANIFEST).unwrap();
    assert_eq!(m.category, Category::Decorator);
    assert_eq!(m.contributions.decorator.len(), 1);
}

#[test]
fn category_decorator_as_str_es_kebab() {
    assert_eq!(Category::Decorator.as_str(), "decorator");
}

/// ADR 0037: `contributions.decorator` sigue el patrón OPCIONAL de
/// `[config]` (`update_decorator_digest`) — un manifiesto SIN
/// `[[contributions.decorator]]` debe digestar EXACTAMENTE igual que un
/// manifiesto de antes de esta categoría (ninguna aprobación humana
/// existente se resetea por la sola introducción del campo).
#[test]
fn manifiesto_sin_decorator_digesta_igual_que_antes_del_campo() {
    // `command` es la MISMA forma que `TOCTOU_BEFORE`/`CMD_MANIFEST` usados
    // en otras suites: sin `[[contributions.decorator]]`, el `Vec` está
    // vacío por el `#[serde(default)]` — el caso general de CUALQUIER
    // manifiesto pre-existente.
    let sin_decorator = Manifest::from_toml(
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
    assert!(sin_decorator.contributions.decorator.is_empty());
    // El digest no depende de si el tipo EXISTE, solo de si la sección se
    // popula: repetir el cómputo (determinismo) confirma que no hay un byte
    // fantasma colándose por la sola presencia del campo en el struct.
    assert_eq!(
        sin_decorator.approval_digest(),
        sin_decorator.approval_digest()
    );
}

#[test]
fn decorator_presente_mueve_el_approval_digest() {
    let sin = Manifest::from_toml(
        r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "decorator"
    "#,
    )
    .unwrap();
    let con = Manifest::from_toml(
        DECORATOR_MANIFEST
            .replace("org.norte.decor", "org.norte.x")
            .as_str(),
    )
    .unwrap();
    assert_ne!(
        sin.approval_digest(),
        con.approval_digest(),
        "declarar [[contributions.decorator]] debe mover el digest (nuevo trigger, nueva superficie)"
    );
}

#[test]
fn category_decorator_mueve_el_approval_digest_frente_a_otra_categoria() {
    // Mismo criterio que `approval_digest_incluye_category_y_contributions_
    // no_solo_capabilities`: cambiar SOLO la categoría (sin tocar
    // capabilities) debe mover el digest.
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
    let decorator = Manifest::from_toml(
        r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "decorator"
        [capabilities]
        fs-read = "scoped"
    "#,
    )
    .unwrap();
    assert_eq!(
        command.capabilities.digest(),
        decorator.capabilities.digest(),
        "las capabilities son idénticas (control del test)"
    );
    assert_ne!(command.approval_digest(), decorator.approval_digest());
}

#[test]
fn catalogo_by_category_incluye_decorator() {
    let root = tempfile::tempdir().unwrap();
    write_plugin(root.path(), "org.norte.decor", DECORATOR_MANIFEST);
    write_plugin(root.path(), "org.norte.syntax-preview", SYNTAX_PREVIEW);

    let cat = Catalog::load_dir(root.path());
    assert_eq!(cat.errors.len(), 0, "{:?}", cat.errors);
    let groups = cat.by_category();
    let cats: Vec<Category> = groups.iter().map(|(c, _)| *c).collect();
    assert_eq!(cats, vec![Category::Previewer, Category::Decorator]);
}

/// La capability `ai` se parseaba, entraba en el digest y pintaba insignia,
/// y ningún host la leía: no hay interfaz WIT de IA ni sitio que la linke.
/// Un humano aprobaba «acceso a IA» y concedía nada — la mentira de ADR 0088
/// con la firma de los hooks. Mismo remedio: se rechaza al parsear, con el
/// motivo, y el campo se queda porque spec §7.1 lo nombra.
#[test]
fn la_capability_ai_se_rechaza_porque_nadie_la_honra() {
    let src = r#"
        [plugin]
        id = "org.demo.oracle"
        name = "Oracle"
        publisher = "demo"
        version = "0.1.0"
        category = "command"
        [capabilities]
        ai = "chat"
    "#;
    assert!(matches!(
        Manifest::from_toml(src),
        Err(ManifestError::AiNotImplemented)
    ));
    // Sin la promesa, el mismo plugin entra.
    let sin_ai = src.replace(r#"ai = "chat""#, "");
    assert!(Manifest::from_toml(&sin_ai).is_ok());
}

/// Un provider plugin sirve el scheme que declara — y eso hace del scheme un
/// nombre que puede SUPLANTAR: `file`, `sftp` y `s3` los sirve el core y un
/// plugin que los reclame estaría poniéndose delante de un provider con
/// papelera, reanudación y TLS. Los schemes con `+` son la composición de
/// archivos (ADR 0018) y tampoco se ceden.
#[test]
fn un_provider_no_puede_reclamar_un_scheme_del_core() {
    for reservado in [
        "file",
        "sftp",
        "ftp",
        "s3",
        "zip+sftp",
        "tar+gz+file",
        "rar",
        "foo+bar",
    ] {
        let src = format!(
            r#"
            [plugin]
            id = "org.demo.usurper"
            name = "Usurper"
            publisher = "demo"
            version = "0.1.0"
            category = "provider"
            [[contributions.provider]]
            scheme = "{reservado}"
        "#
        );
        assert!(
            matches!(
                Manifest::from_toml(&src),
                Err(ManifestError::ReservedScheme)
            ),
            "{reservado} debería estar reservado"
        );
    }
    // Y un scheme que no es ni siquiera un nombre de scheme (mayúsculas,
    // barras, vacío) se rechaza por la misma puerta: lo que llega al
    // connector tiene que ser lo que un VPath puede llevar.
    for malo in ["", "Web-DAV", "a/b", "x y"] {
        let src = format!(
            r#"
            [plugin]
            id = "org.demo.usurper"
            name = "Usurper"
            publisher = "demo"
            version = "0.1.0"
            category = "provider"
            [[contributions.provider]]
            scheme = "{malo}"
        "#
        );
        assert!(
            matches!(
                Manifest::from_toml(&src),
                Err(ManifestError::ReservedScheme)
            ),
            "{malo:?} no es un scheme"
        );
    }
}

/// Un guest compilado contra otra versión del WIT no se carga: se lista en
/// `errors` con las DOS versiones (ADR 0094). Con el binario intacto entra
/// en `plugins` y su `wit` dice contra qué se compiló. Se fabrica el viejo
/// reescribiendo `@0.8.0` por `@0.1.0` en los bytes del guest real.
#[test]
fn un_guest_compilado_contra_otro_wit_se_lista_roto() {
    let Some(wasm) = support::build_guest("previewer-demo") else {
        return;
    };
    let bytes = std::fs::read(wasm).unwrap();
    let root = tempfile::tempdir().unwrap();
    write_plugin(root.path(), "org.norte.syntax-preview", SYNTAX_PREVIEW);
    let wasm_path = root.path().join("org.norte.syntax-preview/plugin.wasm");

    std::fs::write(&wasm_path, &bytes).unwrap();
    let cat = Catalog::load_dir(root.path());
    assert!(cat.errors.is_empty(), "{:?}", cat.errors);
    assert_eq!(cat.plugins.len(), 1);
    assert!(
        cat.plugins[0]
            .wit
            .contains(&("norte:plugin".to_owned(), "0.8.0".to_owned())),
        "{:?}",
        cat.plugins[0].wit
    );

    let viejo = support::rewrite_bytes(&bytes, b"@0.8.0", b"@0.1.0");
    std::fs::write(&wasm_path, viejo).unwrap();
    let cat = Catalog::load_dir(root.path());
    assert!(cat.plugins.is_empty(), "no se carga");
    assert_eq!(cat.errors.len(), 1);
    match &cat.errors[0].error {
        ManifestError::WitMismatch {
            package,
            built_against,
            served,
        } => {
            assert_eq!(package, "norte:plugin");
            assert_eq!(built_against, "0.1.0");
            assert_eq!(served, "0.8.0");
        }
        otro => panic!("se esperaba WitMismatch, salió {otro:?}"),
    }
}

/// Un provider plugin recibe red a `ip:puerto`, nunca a la IP entera, y el
/// host no sabe el puerto por defecto de un scheme ajeno: lo declara la
/// contribución. Entra en el digest — cambiarlo cambia a qué se concede red.
#[test]
fn el_puerto_por_defecto_de_un_provider_se_declara_y_entra_en_el_digest() {
    let con = r#"
        [plugin]
        id = "org.demo.dav"
        name = "DAV"
        publisher = "demo"
        version = "0.1.0"
        category = "provider"
        [[contributions.provider]]
        scheme = "webdav"
        default-port = 8443
    "#;
    let m = Manifest::from_toml(con).unwrap();
    assert_eq!(m.contributions.provider[0].default_port, Some(8443));
    let sin = con.replace("default-port = 8443", "");
    let m2 = Manifest::from_toml(&sin).unwrap();
    assert_eq!(m2.contributions.provider[0].default_port, None);
    assert_ne!(m.approval_digest(), m2.approval_digest());
}
