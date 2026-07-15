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
