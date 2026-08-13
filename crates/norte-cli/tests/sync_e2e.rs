//! `norte sync`: el plan se imprime ENTERO antes de que haya pregunta, y
//! `--dry-run` es el plan sin el apply.

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

/// `--dry-run` enseña lo que haría y NO toca el destino.
#[test]
fn dry_run_ensena_el_plan_y_no_escribe() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::create_dir_all(&dst).expect("mkdir dst");
    std::fs::write(src.join("nuevo.txt"), b"contenido").expect("write");

    let assert = Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", config_dir_del_test())
        .args([
            "sync",
            "--mode",
            "update",
            "--dry-run",
            src.to_str().expect("utf8"),
            dst.to_str().expect("utf8"),
        ])
        .assert()
        .code(1);

    let salida = String::from_utf8(assert.get_output().stdout.clone()).expect("utf8");
    assert!(
        salida.contains("nuevo.txt"),
        "el plan nombra el fichero: {salida}"
    );

    assert!(
        !dst.join("nuevo.txt").exists(),
        "--dry-run no escribe en el destino"
    );
}

/// Nada que hacer: 0, y sin plan que enseñar.
#[test]
fn sin_diferencias_sale_con_cero() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::create_dir_all(&dst).expect("mkdir dst");
    std::fs::write(src.join("igual.txt"), b"x").expect("write src");
    std::fs::write(dst.join("igual.txt"), b"x").expect("write dst");

    Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", config_dir_del_test())
        .args([
            "sync",
            "--mode",
            "update",
            "--dry-run",
            src.to_str().expect("utf8"),
            dst.to_str().expect("utf8"),
        ])
        .assert()
        .code(0);
}

/// `--yes` aplica, y el destino queda con lo que el plan prometía.
#[test]
fn con_yes_aplica_y_el_destino_recibe_el_fichero() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::create_dir_all(&dst).expect("mkdir dst");
    std::fs::write(src.join("nuevo.txt"), b"contenido").expect("write");

    Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", config_dir_del_test())
        .args([
            "sync",
            "--mode",
            "update",
            "--yes",
            src.to_str().expect("utf8"),
            dst.to_str().expect("utf8"),
        ])
        .assert()
        .code(1);

    assert_eq!(
        std::fs::read(dst.join("nuevo.txt")).expect("el destino recibió el fichero"),
        b"contenido"
    );
}

/// Sin terminal NO hay pregunta que hacer, así que no se hace: se rehúsa
/// ANTES, con el código de «no ocurrió» (2) y señalando `--yes`.
///
/// Los dos códigos que importan son los que NO puede devolver. `0` diría «los
/// árboles ya están sincronizados» —que es lo que un `norte sync src dst &&
/// echo ok` en un cron leería— habiendo escrito nada; y `1` diría «se
/// resolvió». Una respuesta vacía, un EOF y un stdin cerrado son el mismo
/// hecho: nadie consintió.
#[test]
fn sin_terminal_no_pregunta_y_no_aplica() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::create_dir_all(&dst).expect("mkdir dst");
    std::fs::write(src.join("nuevo.txt"), b"contenido").expect("write");

    let assert = Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", config_dir_del_test())
        .args([
            "sync",
            "--mode",
            "update",
            src.to_str().expect("utf8"),
            dst.to_str().expect("utf8"),
        ])
        .write_stdin("\n")
        .assert()
        .code(2);

    let err = String::from_utf8(assert.get_output().stderr.clone()).expect("utf8");
    assert!(
        err.contains("--yes"),
        "el mensaje tiene que decir cuál es el remedio: {err}"
    );
    assert!(
        !dst.join("nuevo.txt").exists(),
        "sin consentimiento no se aplica nada"
    );
}

/// Un plan BLOQUEADO no es «nada que hacer».
///
/// `src/x` es un DIRECTORIO y `dst/x` un FICHERO: el transductor bloquea
/// (`TypeMismatchDir`) en vez de convertir un fichero en un árbol, y un plan
/// bloqueado viene con `executable: false` y —por invariante del wire— SIN
/// pasos. Leer esa lista vacía como «los árboles ya coinciden» y contestar 0
/// es exactamente el fallo que el tercer código existe para no cometer.
#[test]
fn un_plan_bloqueado_no_dice_que_no_hay_nada_que_hacer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(src.join("x")).expect("mkdir src/x");
    std::fs::create_dir_all(&dst).expect("mkdir dst");
    std::fs::write(src.join("x/dentro.txt"), b"a").expect("write");
    std::fs::write(dst.join("x"), b"soy un fichero").expect("write");

    Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", config_dir_del_test())
        .args([
            "sync",
            "--mode",
            "update",
            "--yes",
            src.to_str().expect("utf8"),
            dst.to_str().expect("utf8"),
        ])
        .assert()
        .code(2);

    assert_eq!(
        std::fs::read(dst.join("x")).expect("el fichero sigue ahí"),
        b"soy un fichero",
        "un plan bloqueado no escribe"
    );
}

/// El CLI DESMONTA su spool al salir, por todos los caminos.
///
/// El daemon barre al arrancar y suelta cada conexión al cerrarla; el CLI no
/// tiene ni lo uno ni lo otro, así que un `--dry-run` —que no aplica nada—
/// dejaría en el directorio de estado un fichero que nombra los DOS árboles y
/// que nadie recogería. Directorio de estado propio: aquí se MIRA el spool, y
/// el compartido lo puede estar usando otro test.
#[test]
fn dry_run_no_deja_spool_detras() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estado = dir.path().join("estado");
    std::fs::create_dir_all(&estado).expect("mkdir estado");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::create_dir_all(&dst).expect("mkdir dst");
    std::fs::write(src.join("nuevo.txt"), b"contenido").expect("write");

    Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", &estado)
        .args([
            "sync",
            "--mode",
            "update",
            "--dry-run",
            src.to_str().expect("utf8"),
            dst.to_str().expect("utf8"),
        ])
        .assert()
        .code(1);

    let quedan: Vec<String> = std::fs::read_dir(estado.join("sync-spools"))
        .map(|it| {
            it.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        quedan.is_empty(),
        "el spool tiene que quedar vacío al salir: {quedan:?}"
    );
}
