//! Modelo del host de plugins (ADR 0022, M4-P1): manifiesto, capabilities,
//! catálogo. Sin runtime WASM (M4-P2).

use norte_plugin_host::{Catalog, Category, Manifest, ManifestError, Scope};

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
