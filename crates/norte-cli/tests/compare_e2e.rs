//! `norte compare`: el veredicto va en el CÓDIGO DE SALIDA, que es lo que un
//! script lee sin parsear nada.

use assert_cmd::Command;

/// El directorio de estado de ESTE proceso de test, y nunca el del que corre
/// la suite (mismo criterio que `smoke.rs::config_dir_del_test`).
fn config_dir_del_test() -> &'static std::path::Path {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        tempfile::TempDir::new_in(env!("CARGO_TARGET_TMPDIR")).expect("tempdir de estado")
    })
    .path()
}

/// Dos árboles idénticos: 0, como `diff`.
#[test]
fn dos_arboles_iguales_salen_con_cero() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir_all(&a).expect("mkdir a");
    std::fs::create_dir_all(&b).expect("mkdir b");
    std::fs::write(a.join("x.txt"), b"mismo").expect("write a");
    std::fs::write(b.join("x.txt"), b"mismo").expect("write b");

    Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", config_dir_del_test())
        .args([
            "compare",
            a.to_str().expect("utf8"),
            b.to_str().expect("utf8"),
        ])
        .assert()
        .code(0);
}

/// Un fichero que solo está en un lado: 1. NO es un error — es la respuesta.
#[test]
fn dos_arboles_distintos_salen_con_uno() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir_all(&a).expect("mkdir a");
    std::fs::create_dir_all(&b).expect("mkdir b");
    std::fs::write(a.join("solo-aqui.txt"), b"x").expect("write a");

    Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", config_dir_del_test())
        .args([
            "compare",
            a.to_str().expect("utf8"),
            b.to_str().expect("utf8"),
        ])
        .assert()
        .code(1);
}

/// `--json` sale sin traducir y una línea por fila, para que un script no tenga
/// que adivinar el idioma del que lo corre.
#[test]
fn json_no_lleva_idioma() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir_all(&a).expect("mkdir a");
    std::fs::create_dir_all(&b).expect("mkdir b");
    std::fs::write(a.join("solo-aqui.txt"), b"x").expect("write a");

    let out = Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", config_dir_del_test())
        .env("NORTE_LANG", "es")
        .args([
            "compare",
            "--json",
            a.to_str().expect("utf8"),
            b.to_str().expect("utf8"),
        ])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let texto = String::from_utf8(out).expect("utf8");
    let primera = texto.lines().next().expect("al menos una fila");
    let fila: serde_json::Value = serde_json::from_str(primera).expect("json por línea");
    assert!(
        fila.get("verdict").is_some(),
        "cada línea es un CompareRow serializado: {primera}"
    );
}

/// Corpus hostil (CLAUDE.md: test-first en encoding). `norte compare`
/// imprime el nombre del árbol del OTRO lado, que este proceso no controla:
/// un ESC crudo o una RTL override (corpus `control_escape`/`rtl_override`)
/// spoofearían la salida si llegaran sin enmascarar — la comprobación es
/// sobre la salida REAL de `main.rs::compare_cmd`, no sobre
/// `norte_frontend::display_name` en aislamiento (eso ya lo testea
/// `norte-frontend`).
#[test]
#[cfg(unix)]
fn nombres_hostiles_salen_enmascarados_y_marcados() {
    use std::os::unix::ffi::OsStrExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir_all(&a).expect("mkdir a");
    std::fs::create_dir_all(&b).expect("mkdir b");

    let mut escritos = 0;
    for id in ["rtl_override", "control_escape"] {
        let name = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("{id} debe estar en el corpus"));
        let path = a.join(std::ffi::OsStr::from_bytes(&name.bytes));
        // El OS puede rechazar el nombre; no es lo que este test prueba
        // (ver la doc de `list_lazy_stat_hidrata_nombres_del_corpus` en
        // `norte-vfs-local` para el mismo criterio de skip).
        if std::fs::write(&path, b"x").is_ok() {
            escritos += 1;
        }
    }
    assert!(
        escritos > 0,
        "al menos una fixture hostil debe sobrevivir a este filesystem"
    );

    let out = Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", config_dir_del_test())
        .args([
            "compare",
            a.to_str().expect("utf8"),
            b.to_str().expect("utf8"),
        ])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let texto = String::from_utf8(out).expect(
        "la salida humana es UTF-8 válido — el enmascarado la hace así aunque el nombre no lo sea",
    );

    assert!(
        !texto.contains('\u{1b}'),
        "ESC no debe llegar crudo a la terminal: {texto:?}"
    );
    assert!(
        !texto.contains('\u{202e}'),
        "la RTL override no debe llegar cruda a la terminal: {texto:?}"
    );
    for linea in texto.lines() {
        // `<` veredicto (solo a la izquierda) + `!` confianza (`Certain`: la
        // presencia se prueba) + espacio + el `!` del enmascarado, que es el
        // que este test vigila.
        assert!(
            linea.starts_with("<! !"),
            "un nombre hostil sale MARCADO con '!', como en `ai_cmd`: {linea:?}"
        );
    }
}

/// `--json` es la forma WIRE (percent-encoded, lossless, ADR 0001): ni
/// siquiera un nombre NO-UTF8 se convierte con pérdida, a diferencia de la
/// salida humana de arriba.
#[test]
#[cfg(unix)]
fn json_conserva_el_nombre_no_utf8_sin_perdida() {
    use std::os::unix::ffi::OsStrExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir_all(&a).expect("mkdir a");
    std::fs::create_dir_all(&b).expect("mkdir b");

    let name = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "latin1_e_acute")
        .expect("latin1_e_acute debe estar en el corpus");
    let path = a.join(std::ffi::OsStr::from_bytes(&name.bytes));
    std::fs::write(&path, b"x").expect("un byte suelto es un nombre válido en ext4");

    let out = Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", config_dir_del_test())
        .args([
            "compare",
            "--json",
            a.to_str().expect("utf8"),
            b.to_str().expect("utf8"),
        ])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let texto = String::from_utf8(out).expect("--json es JSON válido, luego UTF-8");
    let fila: serde_json::Value =
        serde_json::from_str(texto.lines().next().expect("una fila")).expect("json por línea");
    let path_wire = fila["left"]["path"].as_str().expect("left.path es texto");
    // El byte suelto 0xE9 percent-encodea a `%E9` (hex MAYÚSCULA) en la forma
    // wire — `vpath_codec::encode_segment`.
    assert!(
        path_wire.contains("%E9"),
        "el byte no-UTF8 debe sobrevivir percent-encodeado en el wire: {path_wire}"
    );
}

/// M1: un subdirectorio ILEGIBLE es «no se pudo saber», y esa respuesta gana a
/// la fila de al lado que sí difiere. Una comparación incompleta que
/// contestara 1 («difieren») mentiría por omisión igual que si contestara 0:
/// lo que no se leyó pudo ser cualquier cosa.
#[cfg(unix)]
#[test]
fn un_subdirectorio_ilegible_sale_con_dos() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir_all(a.join("sub")).expect("mkdir a/sub");
    std::fs::create_dir_all(b.join("sub")).expect("mkdir b/sub");
    std::fs::write(a.join("sub/dentro.txt"), b"x").expect("write");
    // Una diferencia de verdad AL LADO del agujero: sin ella el test no
    // distinguiría «gana no-se-sabe» de «no hubo más filas».
    std::fs::write(a.join("solo-aqui.txt"), b"x").expect("write");

    let cerrado = a.join("sub");
    std::fs::set_permissions(&cerrado, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    if std::fs::read_dir(&cerrado).is_ok() {
        // root ignora el modo: aquí no hay nada que comprobar.
        std::fs::set_permissions(&cerrado, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        return;
    }

    let salida = Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", config_dir_del_test())
        .args([
            "compare",
            a.to_str().expect("utf8"),
            b.to_str().expect("utf8"),
        ])
        .output()
        .expect("run");
    // ANTES del assert: un `TempDir` no puede borrar un directorio sin
    // permisos, y un panic aquí dejaría basura en /tmp para siempre.
    std::fs::set_permissions(&cerrado, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    assert_eq!(
        salida.status.code(),
        Some(2),
        "un listado ilegible no se puede resumir en «iguales» ni en «difieren»"
    );
}

/// M1: la CONFIANZA también decide. Dos sockets del mismo nombre son del mismo
/// kind y ahí se acaba lo que se sabe (`Same`/`Unknown`, `cascade.rs`): su
/// contenido no se comparó, así que contestar 0 —«los árboles coinciden»—
/// sería afirmar lo que nadie miró.
#[cfg(unix)]
#[test]
fn un_par_de_sockets_sale_con_dos() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir_all(&a).expect("mkdir a");
    std::fs::create_dir_all(&b).expect("mkdir b");
    // `bind` crea el fichero de socket; el listener se suelta al acabar el
    // test y el nodo se queda, que es justo lo que hace falta.
    let _izq = std::os::unix::net::UnixListener::bind(a.join("s")).expect("socket a");
    let _der = std::os::unix::net::UnixListener::bind(b.join("s")).expect("socket b");

    Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", config_dir_del_test())
        .args([
            "compare",
            a.to_str().expect("utf8"),
            b.to_str().expect("utf8"),
        ])
        .assert()
        .code(2);
}
