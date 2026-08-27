//! Los perfiles contra un disco de verdad: lo que no se puede preguntar con un
//! entorno inyectado.

use std::ffi::OsString;

/// Los nombres son BYTES: un directorio con bytes no-UTF-8 se LISTA (existe, el
/// lector lo creó) en vez de desaparecer del selector. #245/#246 fueron este
/// bug dos veces con los nombres de disposición.
#[test]
#[cfg(unix)]
fn un_nombre_no_utf8_se_lista_tal_cual() {
    use std::os::unix::ffi::OsStringExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let hostil = OsString::from_vec(vec![b'w', 0xFF, b'k']);
    std::fs::create_dir(dir.path().join(&hostil)).expect("mkdir");
    std::fs::create_dir(dir.path().join("work")).expect("mkdir");
    std::fs::write(dir.path().join("no-soy-un-dir"), b"x").expect("write");

    let mut got = norte_config::list_profiles(dir.path()).expect("list");
    got.sort();
    let mut want = vec![hostil, OsString::from("work")];
    want.sort();
    assert_eq!(
        got, want,
        "los ficheros no son perfiles; los bytes se respetan"
    );
}

/// El camino ENTERO: resolutor de capas real, disco real, `load` real.
///
/// Los tests de la whitelist construyen `Layers` a mano, así que ejercen el
/// merge y NO el resolutor. Este empieza donde empieza el binario —un
/// directorio de config y un nombre de perfil— y comprueba que el tema del
/// perfil llega a `CommonConfig`. Sin él, un fallo en el empalme de la capa
/// deja la suite verde y el lector viendo el tema de su capa de usuario con un
/// perfil activo que dice otro.
#[test]
fn el_tema_del_perfil_llega_por_el_camino_entero() {
    let cfg = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        cfg.path().join("norte.toml"),
        "[ui]\ntheme = \"retro-crt\"\n",
    )
    .expect("write");
    let perfil = cfg.path().join("profiles").join("fotos");
    std::fs::create_dir_all(&perfil).expect("mkdir");
    std::fs::write(
        perfil.join("norte.toml"),
        "[profile]\ntitle = \"Fotos\"\n\n[ui]\ntheme = \"gruvbox-light\"\n",
    )
    .expect("write");

    let raiz = cfg.path().to_path_buf();
    let get = move |k: &str| -> Option<OsString> {
        (k == "NORTE_CONFIG_DIR").then(|| OsString::from(raiz.as_os_str()))
    };
    let layers = norte_config::profiles::standard_layers_with_profile_on(
        false,
        &get,
        Some(std::ffi::OsStr::new("fotos")),
    );
    assert!(
        layers
            .dirs
            .iter()
            .any(|(d, k)| *k == norte_config::Layer::Profile && d == &perfil),
        "la capa del perfil está en el resolutor: {:?}",
        layers.dirs
    );

    let merged = norte_config::load(&layers).expect("carga");
    assert_eq!(
        merged.ui_theme.as_deref(),
        Some("gruvbox-light"),
        "el tema del PERFIL pisa al del usuario"
    );
}

/// Un directorio de perfiles que no existe no es un error: es «no tienes
/// perfiles todavía».
#[test]
fn sin_directorio_de_perfiles_la_lista_esta_vacia() {
    let dir = tempfile::tempdir().expect("tempdir");
    let got = norte_config::list_profiles(&dir.path().join("no-existe")).expect("list");
    assert!(got.is_empty());
}
