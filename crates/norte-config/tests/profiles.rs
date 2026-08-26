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

/// Un directorio de perfiles que no existe no es un error: es «no tienes
/// perfiles todavía».
#[test]
fn sin_directorio_de_perfiles_la_lista_esta_vacia() {
    let dir = tempfile::tempdir().expect("tempdir");
    let got = norte_config::list_profiles(&dir.path().join("no-existe")).expect("list");
    assert!(got.is_empty());
}
