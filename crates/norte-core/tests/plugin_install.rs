//! `norte plugin install` (A4): traer un plugin al directorio de config.
//!
//! Lo que se prueba aquí no es la copia —es `std::fs::copy`— sino las tres
//! decisiones que la rodean: de dónde sale el id, que instalar NO consiente, y
//! que reemplazar RETIRA el consentimiento.

use std::path::Path;

use norte_core::plugins::{InstallError, PluginRegistry, install};

const MANIFEST: &str = r#"
[plugin]
id = "org.norte.demo"
name = "Demo"
publisher = "norte"
version = "0.1.0"
category = "previewer"

[contributions]
previewer = [{ mimetypes = ["text/*"] }]

[capabilities]
fs-read = "scoped"
"#;

/// Un origen con manifiesto y `.wasm` (bytes cualesquiera: install no ejecuta).
fn origen(dir: &Path, manifest: &str, wasm: &[u8]) -> std::path::PathBuf {
    let src = dir.join("src-plugin");
    std::fs::create_dir_all(&src).expect("mkdir");
    std::fs::write(src.join("plugin.toml"), manifest).expect("manifest");
    std::fs::write(src.join("plugin.wasm"), wasm).expect("wasm");
    src
}

/// El destino sale del ID DEL MANIFIESTO, no del nombre del directorio de
/// origen: es lo que el descubridor va a usar, y dejar que el nombre del
/// directorio decidiera dónde aterriza sería una vía para pisar a otro plugin.
#[test]
fn aterriza_bajo_el_id_del_manifiesto_no_del_directorio() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let src = origen(cfg.path(), MANIFEST, b"\0asm");

    let rep = install(cfg.path(), &src, false).expect("instala");
    assert_eq!(rep.id, "org.norte.demo");
    assert!(!rep.replaced);

    let dest = cfg.path().join("plugins").join("org.norte.demo");
    assert!(dest.join("plugin.toml").is_file());
    assert!(dest.join("plugin.wasm").is_file());
    // El directorio de origen se llamaba `src-plugin` y no dejó rastro.
    assert!(!cfg.path().join("plugins").join("src-plugin").exists());
}

/// Instalar NO es consentir: el plugin queda descubierto y sin aprobar. Un
/// instalador que aprobara por su cuenta convertiría «traigo este fichero» en
/// «le doy sus capabilities», que es la decisión entera.
#[test]
fn instalar_no_aprueba_ni_activa() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let src = origen(cfg.path(), MANIFEST, b"\0asm");
    install(cfg.path(), &src, false).expect("instala");

    let reg = PluginRegistry::discover(cfg.path()).expect("discover");
    let listado = reg.list();
    let p = listado
        .plugins
        .iter()
        .find(|p| p.id == "org.norte.demo")
        .expect("descubierto");
    assert!(!p.approved, "instalar no aprueba");
    assert!(!p.enabled, "instalar no activa");
}

/// Sin `force`, un id ya instalado se rechaza. Con mensaje, no en silencio.
#[test]
fn no_pisa_sin_force() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let src = origen(cfg.path(), MANIFEST, b"\0asm");
    install(cfg.path(), &src, false).expect("primera");

    match install(cfg.path(), &src, false) {
        Err(InstallError::AlreadyInstalled(id)) => assert_eq!(id, "org.norte.demo"),
        otro => panic!("se esperaba AlreadyInstalled, salió {otro:?}"),
    }
}

/// LA prueba que importa: reemplazar RETIRA el consentimiento.
///
/// El digest de aprobación cubre el manifiesto —capabilities, categoría,
/// contribuciones— y NO el `.wasm`. Sin retirarlo, instalar encima de un plugin
/// aprobado dejaría un binario distinto corriendo bajo el permiso que un humano
/// le dio a otro, con el manifiesto idéntico para que nada lo notara. Por eso
/// este test deja el manifiesto INTACTO y cambia solo el wasm.
#[test]
fn reemplazar_retira_el_consentimiento() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let src = origen(cfg.path(), MANIFEST, b"\0asm-viejo");
    install(cfg.path(), &src, false).expect("instala");

    let mut reg = PluginRegistry::discover(cfg.path()).expect("discover");
    reg.set_approval("org.norte.demo", true).expect("aprueba");
    assert!(
        reg.list()
            .plugins
            .iter()
            .any(|p| p.id == "org.norte.demo" && p.approved),
        "precondición: aprobado antes de reemplazar"
    );

    // MISMO manifiesto, otro binario.
    std::fs::write(src.join("plugin.wasm"), b"\0asm-nuevo").expect("wasm nuevo");
    let rep = install(cfg.path(), &src, true).expect("reemplaza");
    assert!(rep.replaced);

    let reg = PluginRegistry::discover(cfg.path()).expect("re-discover");
    let p = reg
        .list()
        .plugins
        .into_iter()
        .find(|p| p.id == "org.norte.demo")
        .expect("sigue descubierto");
    assert!(
        !p.approved,
        "el binario cambió bajo un manifiesto idéntico: la aprobación NO puede sobrevivir"
    );
    assert!(!p.enabled);
}

/// Un manifiesto que no valida no llega a copiar nada: se rechaza antes, así
/// que un origen roto no deja medio plugin en el directorio de config.
#[test]
fn un_manifiesto_invalido_no_deja_rastro() {
    let cfg = tempfile::tempdir().expect("tempdir");
    // `category = "hook"` es rechazado desde A2 — sirve de manifiesto inválido
    // real en vez de un TOML roto, que probaría otra cosa.
    let malo = MANIFEST.replace(r#"category = "previewer""#, r#"category = "hook""#);
    let src = origen(cfg.path(), &malo, b"\0asm");

    assert!(matches!(
        install(cfg.path(), &src, false),
        Err(InstallError::Manifest(_))
    ));
    assert!(
        !cfg.path().join("plugins").exists(),
        "un origen inválido no crea el directorio de destino"
    );
}

/// Sin `.wasm` no hay plugin, y se dice antes de copiar el manifiesto.
#[test]
fn sin_wasm_no_se_instala() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let src = cfg.path().join("src-plugin");
    std::fs::create_dir_all(&src).expect("mkdir");
    std::fs::write(src.join("plugin.toml"), MANIFEST).expect("manifest");

    assert!(matches!(
        install(cfg.path(), &src, false),
        Err(InstallError::NoWasm(_))
    ));
    assert!(!cfg.path().join("plugins").exists());
}

/// ADR 0057: la capacidad de ubicación viaja al frontend por el MISMO canal
/// que las demás — `PluginInfo::capabilities`, que es vocabulario ABIERTO.
///
/// Por eso este cambio NO toca el wire: un campo propio para `location` sería
/// una segunda forma de decir lo mismo, con su bump y sus goldens, y un
/// cliente N-1 lo pintaría igual de bien leyendo el badge que ya recibe.
#[test]
fn el_badge_de_location_llega_al_listado_sin_tocar_el_wire() {
    const CON_LOCATION: &str = r#"
[plugin]
id = "org.norte.git-status"
name = "Git status"
publisher = "norte"
version = "0.1.0"
category = "columns"

[[contributions.columns]]
id = "git-status"
header = "Git"

[capabilities]
location = "read"
"#;
    let cfg = tempfile::tempdir().expect("tempdir");
    let src = origen(cfg.path(), CON_LOCATION, b"\0asm");
    install(cfg.path(), &src, false).expect("instala");

    let reg = PluginRegistry::discover(cfg.path()).expect("descubre");
    let listado = reg.list();
    let info = listado
        .plugins
        .iter()
        .find(|p| p.id == "org.norte.git-status")
        .expect("el plugin está");
    assert!(
        info.capabilities.iter().any(|c| c == "location"),
        "el humano ve QUÉ va a aprobar: {:?}",
        info.capabilities
    );
    assert!(!info.approved, "instalar no consiente nada");
}
