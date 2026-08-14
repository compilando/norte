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

/// Auditoría de encoding de la revisión de rama de C2, MAJOR-2: la fila del
/// plan unía los campos EN BANDA, en la pantalla donde se teclea `y`.
///
/// `→` y `  (…)` son imprimibles corrientes que `display_name_with` no
/// enmascara, así que un nombre que los lleve dentro llega SIN el `!` de
/// `rel_marcado` y finge una fila entera: `a → mem_b.txt` (corpus
/// `arrow_join_spoof`) simula una pareja origen→destino que no existe. Aquí
/// se comprueba lo que lo cierra — un campo por LÍNEA —, porque el salto de
/// línea sí es un separador que un nombre no puede falsificar: `\n` es Cc y
/// `is_terminal_hazard` lo enmascara a `U+FFFD`.
///
/// Este fichero no tenía NINGÚN test de nombre hostil, y esa ausencia es por
/// lo que la CLI se quedó atrás cuando la GUI y la TUI se arreglaron.
#[test]
fn un_nombre_con_flecha_no_finge_una_pareja_en_el_plan() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::create_dir_all(&dst).expect("mkdir dst");
    // El nombre del corpus, tal cual: legal en ext4 y APFS.
    let hostil = "a \u{2192} mem_b.txt";
    std::fs::write(src.join(hostil), b"contenido").expect("write");

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
    let fila = salida
        .lines()
        .find(|l| l.contains("mem_b.txt"))
        .unwrap_or_else(|| panic!("el plan nombra el fichero: {salida}"));

    // El nombre entero está en UNA línea, con su flecha dentro: eso es el
    // nombre, no una pareja. Lo que no puede haber es una SEGUNDA ortografía
    // en esa misma línea, que es lo que el ` → ` en banda fabricaba.
    assert!(fila.contains(hostil), "el nombre va entero: {fila:?}");
    assert_eq!(
        fila.matches('\u{2192}').count(),
        1,
        "una sola flecha, la del NOMBRE: {fila:?}"
    );
    // Y la ortografía del destino, cuando la hay, va en su propia línea con
    // su etiqueta — nunca pegada al nombre.
    assert!(
        !fila.contains("  ("),
        "el porqué tampoco se une en banda: {fila:?}"
    );
}

// ---------- Ctrl+C durante `norte sync` (#180) ----------

/// `kill(pid, SIGINT)` sin dependencias: mismo helper que
/// `smoke.rs::unsafe_free_kill`, duplicado a propósito — cada fichero de test
/// es su propio binario y no hay una crate de soporte compartida entre ellos
/// (mismo criterio que la duplicación de `config_dir_del_test`).
#[cfg(unix)]
fn unsafe_free_kill(pid: u32) {
    let status = std::process::Command::new("kill")
        .arg("-INT")
        .arg(pid.to_string())
        .status()
        .expect("kill disponible");
    assert!(status.success(), "kill -INT falló");
}

/// #180: un Ctrl+C DURANTE la planificación no puede dejar un `.part` de
/// spool huérfano.
///
/// Antes de la corrección, `sync_plan_show_apply` drenaba el stream de
/// `sync.plan` en un `while let` sin manejador de Ctrl+C: el SIGINT mataba el
/// proceso ENTERO por el comportamiento por defecto del SO, sin darle a
/// `run_sync_plan` la ocasión de ver su `CancellationToken`, cerrar el spool
/// y borrar el `.part` (`SpoolWriter::finish`/`Drop`). El TTL solo barre
/// planes CERRADOS, así que ese fichero se quedaba para siempre.
///
/// Un árbol con muchas entradas (no bytes: lo que hace lenta la
/// PLANIFICACIÓN es listar y comparar filas, no escribir contenido — eso es
/// el apply) da tiempo a comprobar que el `.part` existe antes de señalar.
/// Si la planificación termina antes de que se detecte —carrera legítima, la
/// misma que tolera `cp_sigint_cancels_cleanly` en `smoke.rs`— la aserción de
/// abajo sigue siendo cierta trivialmente: no queda nada en el spool.
#[cfg(unix)]
#[test]
fn sigint_durante_planificacion_no_deja_part_detras() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estado = dir.path().join("estado");
    std::fs::create_dir_all(&estado).expect("mkdir estado");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::create_dir_all(&dst).expect("mkdir dst");
    for i in 0..20_000 {
        std::fs::write(src.join(format!("f{i:05}")), b"").expect("write");
    }

    let bin = assert_cmd::cargo::cargo_bin("norte");
    let mut child = std::process::Command::new(bin)
        .env("NORTE_CONFIG_DIR", &estado)
        .env("NORTE_LANG", "en")
        .arg("sync")
        .arg("--mode")
        .arg("update")
        .arg("--yes")
        .arg(&src)
        .arg(&dst)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn norte sync");

    let spool_dir = estado.join("sync-spools");
    // Espera a que el `.part` exista (la planificación ARRANCÓ y el spool se
    // creó) antes de señalar. Eso solo no basta: `.part` lo escribe la Task
    // de `sync.plan`, en un WORKER aparte del que instala `watch_ctrl_c` en
    // el proceso hijo, así que verlo no prueba que ESE manejador ya recibió
    // su primer `poll` — bajo carga (varios procesos por núcleo) el
    // scheduler puede adelantar el worker de la Task y dejar el del
    // manejador sin turno todavía. Sin este suelo, SIGINT podía llegar
    // mientras el SIGINT por defecto del SO seguía vigente, matando el
    // proceso por señal cruda y saltándose el `Drop` que borra el `.part`
    // (encontrado corriendo esta suite bajo carga de CI; nunca en una
    // corrida aislada). Es la misma condición de carrera que ya resolvió el
    // comentario de `cp_sigint_cancels_cleanly`, aquí sin un `dst.exists()`
    // causal en el que apoyarse porque la creación del `.part` es server-side
    // y no depende de que el manejador ya esté armado.
    let arranque = std::time::Instant::now();
    let suelo = std::time::Duration::from_millis(50);
    let deadline = arranque + std::time::Duration::from_secs(15);
    let mut started = false;
    loop {
        if child.try_wait().expect("try_wait").is_some() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        let tiene_part = std::fs::read_dir(&spool_dir).is_ok_and(|it| {
            it.flatten()
                .any(|e| e.file_name().to_string_lossy().ends_with(".part"))
        });
        if tiene_part && arranque.elapsed() >= suelo {
            started = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    if started {
        unsafe_free_kill(child.id());
    }
    let _status = child.wait().expect("wait");

    let quedan: Vec<String> = std::fs::read_dir(&spool_dir)
        .map(|it| {
            it.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        quedan.is_empty(),
        "un Ctrl+C durante la planificación no puede dejar nada en el spool: {quedan:?}"
    );
}
