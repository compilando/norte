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

// ---------- Ctrl+C durante `norte sync` (#180, #187) ----------

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
    let spool_dir = estado.join("sync-spools");

    // El manejador cooperativo se ARMA en un worker del hijo, y verlo escribir
    // el `.part` no prueba que ese worker ya haya tenido turno: bajo carga
    // —esta suite corre con un proceso por núcleo— el SIGINT puede llegar
    // mientras sigue vigente el por defecto del SO, que mata el proceso en
    // crudo y se salta el `Drop` que borra el `.part`.
    //
    // El suelo de 50 ms que había aquí era una CONJETURA DE RELOJ, y bajo
    // `just ci-fast` la pierde: rojo en la suite entera, verde en aislado.
    // Esto lo hace causal — la muerte del proceso DICE cuál de los dos casos
    // fue (código de salida = cooperativa, señal cruda = el manejador no
    // estaba) — y solo reintenta el caso que no probaba nada. Tres intentos
    // sin una sola muerte cooperativa no son ruido: son un manejador que no se
    // arma, y entonces el test falla diciendo eso.
    let mut crudas = 0;
    for intento in 1..=3 {
        let mut child = std::process::Command::new(&bin)
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

        // Espera a que el `.part` exista: la planificación arrancó y el spool
        // se creó, que es lo que esta prueba necesita que haya que limpiar.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
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
            if tiene_part {
                started = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let vivo = child.try_wait().expect("try_wait").is_none();
        assert!(
            started,
            "la planificación no llegó a escribir un `.part` en 15 s (intento {intento}; \
             ¿sigue vivo el hijo? {vivo}; en el spool: {:?}): no había nada que cancelar",
            std::fs::read_dir(&spool_dir).map(|it| it
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>())
        );
        unsafe_free_kill(child.id());
        let status = child.wait().expect("wait");

        // Muerte CRUDA: el SIGINT por defecto del SO se adelantó al manejador.
        // El `Drop` no corrió porque no podía correr — no es lo que este test
        // afirma, así que se limpia y se vuelve a intentar.
        if status.code().is_none() {
            crudas += 1;
            for e in std::fs::read_dir(&spool_dir)
                .into_iter()
                .flatten()
                .flatten()
            {
                let _ = std::fs::remove_file(e.path());
            }
            continue;
        }

        let quedan: Vec<String> = std::fs::read_dir(&spool_dir)
            .map(|it| {
                it.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            quedan.is_empty(),
            "un Ctrl+C durante la planificación no puede dejar nada en el spool: {quedan:?} \
             (intento {intento}, salió con {:?}, muertes crudas hasta aquí: {crudas})",
            status.code()
        );
        return;
    }
    panic!(
        "tres intentos y las tres veces murió por señal CRUDA ({crudas}): el manejador cooperativo no se arma"
    );
}

/// #187: un `norte sync` CANCELADO llega a su informe, y el código de salida
/// se queda en 2 — nunca el 0 de `run_task` (que en `sync.apply` sería
/// mentira: la aplicación se cortó) ni un mensaje de «destino limpio», que es
/// falso para un `Mirror` cortado a medias (lo aplicado hasta el corte se
/// queda, journalizado, regla dura 4).
///
/// Un árbol grande en BYTES (y no solo en número de entradas) para que la
/// fase de apply —que sí escribe— dure lo bastante como para señalarla
/// después de que el plan ya se aprobó con `--yes`.
#[cfg(unix)]
#[test]
fn sigint_durante_apply_pide_el_informe_y_no_dice_destino_limpio() {
    use std::io::Write as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let estado = dir.path().join("estado");
    std::fs::create_dir_all(&estado).expect("mkdir estado");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::create_dir_all(&dst).expect("mkdir dst");
    let payload = vec![0x42u8; 64 * 1024];
    for i in 0..400 {
        let mut f = std::fs::File::create(src.join(format!("f{i:04}"))).expect("create");
        f.write_all(&payload).expect("write");
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
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn norte sync");

    // #201: los dos pipes se DRENAN en hilos, desde ya. Sin esto el test se
    // podía autoanular en silencio: un plan que no cabe en el buffer del pipe
    // (64 KiB en Linux) bloquea al hijo ANTES de aplicar, `dst` no se llena
    // nunca, no se manda ninguna señal y la corrida entera sale por el brazo
    // de la carrera sin haber probado nada. La fixture de hoy cabe; subirla
    // cruzaba ese umbral sin decir una palabra.
    //
    // Su gemelo `sigint_tras_planificar_termina_el_proceso` depende justo de
    // ese bloqueo para alcanzar SU ventana. El mismo mecanismo: aquí estorba,
    // allí es el sujeto. Quien toque uno lea los dos.
    let mut salida_hijo = child.stdout.take().expect("stdout piped");
    let mut error_hijo = child.stderr.take().expect("stderr piped");
    let drenador_out = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut salida_hijo, &mut buf);
        buf
    });
    let drenador_err = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut error_hijo, &mut buf);
        buf
    });

    // El apply ARRANCÓ en cuanto el destino recibe su primera entrada: la
    // planificación no escribe nada ahí. El suelo es la misma cautela que
    // `sigint_durante_planificacion_no_deja_part_detras`: ver ficheros en
    // `dst` no prueba que `watch_ctrl_c` ya recibió su primer `poll` en el
    // proceso hijo, así que bajo carga un `kill` demasiado pronto puede topar
    // con el SIGINT por defecto del SO en vez de con el manejador cooperativo.
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
        let tiene_algo = std::fs::read_dir(&dst).is_ok_and(|it| it.flatten().next().is_some());
        if tiene_algo && arranque.elapsed() >= suelo {
            started = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    // #201: que la señal se MANDÓ es la premisa del test, no una casualidad
    // afortunada. Sin esto, cualquier corrida en la que el apply no arrancara
    // pasaba sin ejercitar el arreglo.
    assert!(
        started,
        "el apply no llegó a escribir en {} en 15 s: la señal jamás se mandó y este test no probó nada",
        dst.display()
    );
    unsafe_free_kill(child.id());
    let status = child.wait().expect("wait");
    let stdout = drenador_out.join().expect("drenador de stdout");
    let stderr = drenador_err.join().expect("drenador de stderr");

    match status.code() {
        Some(2) => {
            let out = String::from_utf8_lossy(&stdout);
            let err = String::from_utf8_lossy(&stderr);
            assert!(
                out.contains("applied:"),
                "una aplicación cancelada TIENE informe: stdout={out}"
            );
            assert!(
                !err.contains("destination clean"),
                "lo aplicado hasta el corte NO es un destino limpio: stderr={err}"
            );
        }
        // Carrera legítima QUE TAMBIÉN SE COMPRUEBA: el apply terminó antes de
        // que la señal llegara. Entonces terminó DEL TODO — un `0` con la
        // mitad de los ficheros sería una aplicación que mintió sobre su
        // desenlace, y este brazo era el sitio donde eso pasaba inadvertido.
        Some(0) => {
            let copiados = std::fs::read_dir(&dst).expect("leer dst").count();
            assert_eq!(
                copiados, 400,
                "salió 0 (completo) con {copiados} de 400 ficheros en el destino"
            );
        }
        Some(1) => {
            let err = String::from_utf8_lossy(&stderr);
            assert!(
                !err.contains("destination clean"),
                "un fallo tampoco deja «destino limpio»: stderr={err}"
            );
        }
        other => panic!("código de salida inesperado tras SIGINT: {other:?}"),
    }
}

/// **BLOCKER de la revisión de rama de W2.** `Ctrl+C` DESPUÉS de planificar.
///
/// Registrar `ctrl_c()` en tokio es de PROCESO y permanente: la doc de tokio
/// dice que soltar el `Signal` no restaura el comportamiento por defecto. Con
/// un vigilante por fase, el `abort()` al acabar la planificación mataba al que
/// escuchaba y dejaba el registro puesto — así que a partir de ahí la señal la
/// consumía tokio, no la atendía nadie, y `Ctrl+C` no hacía NADA en toda la
/// ventana que va desde el fin del plan hasta el principio del apply: imprimir
/// el plan, los bloqueadores, el prompt `[y/N]` y la salida de `--dry-run`.
///
/// Aquí se provoca la ventana con `--dry-run` sobre un árbol grande y un
/// `stdout` que NADIE drena: el hijo se bloquea escribiendo el plan contra un
/// pipe lleno, que es justo el estado «planificación terminada, nada vivo que
/// cancelar». Antes del arreglo el proceso se quedaba ahí para siempre.
///
/// El prompt de verdad no se puede probar sin PTY —esta CLI no pregunta sin
/// terminal, y hay un test que lo fija— así que se prueba la MISMA ventana por
/// el lado que sí es alcanzable. Los otros dos tests de señal no la ven: los
/// dos pasan `--yes`.
#[test]
fn sigint_tras_planificar_termina_el_proceso() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estado = dir.path().join("estado");
    std::fs::create_dir_all(&estado).expect("mkdir estado");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::create_dir_all(&dst).expect("mkdir dst");
    // Bastantes ficheros para que el plan impreso NO quepa en el buffer del
    // pipe (64 KiB en Linux): así el hijo se queda bloqueado escribiéndolo.
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
        .arg("--dry-run")
        .arg(&src)
        .arg(&dst)
        // Piped y NUNCA leído: el pipe se llena y el hijo se bloquea ahí.
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn norte sync");

    // Suelo antes de señalar, por lo mismo que los otros dos: el manejador
    // vive en un worker distinto del que planifica. Aquí además hay que dejar
    // que la planificación TERMINE y que el hijo se atasque escribiendo.
    std::thread::sleep(std::time::Duration::from_millis(2500));

    // El mismo helper que los otros dos: `kill -INT` por proceso, sin añadir
    // `libc` como dependencia solo para un test (regla 8).
    unsafe_free_kill(child.id());

    // Y TIENE que morir. Antes del arreglo se quedaba bloqueado para siempre:
    // la señal la consumía tokio y no la escuchaba nadie.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => {
                assert!(
                    !status.success(),
                    "un sync interrumpido tras planificar no sale con éxito: {status:?}"
                );
                return;
            }
            None if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            None => {
                let _ = child.kill();
                panic!("Ctrl+C tras planificar no hizo nada: el proceso sigue vivo");
            }
        }
    }
}
