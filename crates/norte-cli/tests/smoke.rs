//! Smoke tests del CLI (`assert_cmd`): los cuatro subcomandos contra un
//! tempdir real, incluida la cancelación por SIGINT (unix).

use assert_cmd::Command;

/// El directorio de estado de ESTE proceso de test, y nunca el del que corre la
/// suite.
///
/// Desde #167 un `norte cp`/`mv`/`rm`/`mkdir` embebido abre
/// `<estado>/journal.db` al mutar (#177: no antes) y se queda su lock EXCLUSIVO
/// mientras vive. Sin este
/// override eso sería el journal de verdad del desarrollador: la suite le
/// escribiría filas en su cadena de hashes y le disputaría el lock a su TUI o a
/// su daemon. El mismo criterio que las tres pruebas de shell (7b0655c) — el
/// sujeto es el binario, jamás la configuración de quien lo ejecuta.
///
/// Uno por proceso: nextest da un proceso por test, así que cada test acaba con
/// el suyo. Bajo `cargo test` (varios tests por proceso) lo comparten, y el
/// segundo en llegar no espera: se lleva `Busy` a los 250 ms y sigue sin
/// registrar, que es lo que este cambio tolera por diseño.
///
/// Vive bajo `CARGO_TARGET_TMPDIR` y no bajo `/tmp`: un `static` no ejecuta
/// `Drop`, así que el directorio sobrevive al proceso — ahí lo barren
/// `cargo clean` y `just prune`, en `/tmp` no lo barre nadie.
fn config_dir_del_test() -> &'static std::path::Path {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        tempfile::TempDir::new_in(env!("CARGO_TARGET_TMPDIR")).expect("tempdir de estado")
    })
    .path()
}

fn norte() -> Command {
    let mut c = Command::cargo_bin("norte").expect("binario norte compilado");
    c.env("NORTE_CONFIG_DIR", config_dir_del_test());
    c
}

/// #177: navegar NO abre —ni siquiera crea— el journal, y mutar SÍ.
///
/// Es la prueba a nivel de proceso de lo que #177 arregla: mientras un frontend
/// embebido se limitaba a mirar, `journal.db` seguía suyo y ni `norte daemon
/// run` podía arrancar ni `norte audit` leer. Los dos comandos van en el mismo
/// test a propósito: «no lo abre» solo significa algo si se enseña al lado del
/// que sí lo abre.
///
/// Con directorio de estado PROPIO, y no el compartido de
/// `config_dir_del_test`: lo que se afirma es que un fichero NO existe, y bajo
/// `cargo test` —un proceso para toda la suite— el `cp` de otro test ya lo
/// habría creado.
#[test]
fn ls_no_abre_el_journal_y_mkdir_si() {
    let estado = tempfile::tempdir().unwrap();
    let arbol = tempfile::tempdir().unwrap();
    let journal = estado.path().join("journal.db");

    let out = Command::cargo_bin("norte")
        .expect("binario norte compilado")
        .env("NORTE_CONFIG_DIR", estado.path())
        .arg("ls")
        .arg(arbol.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !journal.exists(),
        "listar un directorio no puede quedarse el journal (#177)"
    );

    let out = Command::cargo_bin("norte")
        .expect("binario norte compilado")
        .env("NORTE_CONFIG_DIR", estado.path())
        .arg("mkdir")
        .arg(arbol.path().join("nuevo"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        journal.exists(),
        "crear un directorio sí queda registrado (regla dura 4, #167)"
    );
}

#[test]
fn ls_json_lists_entries() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("uno.txt"), b"1").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();

    let out = norte()
        .arg("ls")
        .arg(dir.path())
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON válido");
    let entries = parsed.as_array().expect("array de entradas");
    assert_eq!(entries.len(), 2);
    let kinds: Vec<&str> = entries
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"file") && kinds.contains(&"dir"));
    // Los paths van en forma wire.
    assert!(
        entries
            .iter()
            .all(|e| e["path"].as_str().unwrap().starts_with("file:///"))
    );
}

/// #52: el listado local es lazy (`size`/`mtime_ms` en `None`); `ls`
/// hidrata con un stat serial antes de imprimir — MAJOR-2, restaura el
/// output pre-#52. Cubre --json (campo no-null) y texto (columna no vacía).
#[test]
fn ls_hidrata_size_lazy() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("uno.txt"), b"contenido").unwrap();

    let json_out = norte()
        .arg("ls")
        .arg(dir.path())
        .arg("--json")
        .output()
        .unwrap();
    assert!(json_out.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&json_out.stdout).expect("JSON válido");
    let entries = parsed.as_array().expect("array de entradas");
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0]["size"].as_u64(),
        Some(9),
        "size hidratado, no null: {entries:?}"
    );

    let text_out = norte().arg("ls").arg(dir.path()).output().unwrap();
    assert!(text_out.status.success());
    let stdout = String::from_utf8_lossy(&text_out.stdout);
    let line = stdout.lines().next().expect("una línea de salida");
    let cols: Vec<&str> = line.split('\t').collect();
    assert_eq!(cols.first(), Some(&"-"), "marker de File");
    assert_eq!(cols.get(1), Some(&"9"), "columna size no vacía: {line}");
}

#[test]
fn ls_missing_dir_fails() {
    let dir = tempfile::tempdir().unwrap();
    let out = norte()
        .arg("ls")
        .arg(dir.path().join("no-existe"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not found"), "stderr: {stderr}");
}

#[test]
fn cp_file_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("origen.bin");
    let dst = dir.path().join("copia.bin");
    let content = vec![0xC5u8; 100_000];
    std::fs::write(&src, &content).unwrap();

    norte().arg("cp").arg(&src).arg(&dst).assert().success();
    assert_eq!(std::fs::read(&dst).unwrap(), content);
    assert_eq!(std::fs::read(&src).unwrap(), content, "el origen queda");
}

#[test]
fn cp_collision_refused_and_dest_intact() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("a");
    let dst = dir.path().join("b");
    std::fs::write(&src, b"nuevo").unwrap();
    std::fs::write(&dst, b"previo").unwrap();

    let out = norte().arg("cp").arg(&src).arg(&dst).output().unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("conflict"), "stderr: {stderr}");
    assert_eq!(std::fs::read(&dst).unwrap(), b"previo");
}

#[test]
fn cp_dir_recursive() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("arbol");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("f1"), b"1").unwrap();
    std::fs::write(src.join("sub/f2"), b"2").unwrap();
    let dst = dir.path().join("copia");

    norte().arg("cp").arg(&src).arg(&dst).assert().success();
    assert_eq!(std::fs::read(dst.join("f1")).unwrap(), b"1");
    assert_eq!(std::fs::read(dst.join("sub/f2")).unwrap(), b"2");
}

#[test]
fn mv_moves_and_rm_deletes() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::write(&a, b"x").unwrap();

    norte().arg("mv").arg(&a).arg(&b).assert().success();
    assert!(!a.exists());
    assert_eq!(std::fs::read(&b).unwrap(), b"x");

    norte().arg("rm").arg(&b).assert().success();
    assert!(!b.exists());
}

#[test]
fn rm_recursive_tree() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("arbol");
    std::fs::create_dir_all(root.join("s1/s2")).unwrap();
    std::fs::write(root.join("s1/s2/f"), b"x").unwrap();

    norte().arg("rm").arg(&root).assert().success();
    assert!(!root.exists());
}

#[cfg(unix)]
#[test]
fn cp_sigint_cancels_cleanly() {
    use std::io::Write;
    // Árbol grande para que la copia dure lo bastante como para señalarla.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("grande");
    std::fs::create_dir(&src).unwrap();
    let payload = vec![0x42u8; 64 * 1024];
    for i in 0..400 {
        let mut f = std::fs::File::create(src.join(format!("f{i:04}"))).unwrap();
        f.write_all(&payload).unwrap();
    }
    let dst = dir.path().join("copia");

    let bin = assert_cmd::cargo::cargo_bin("norte");
    let mut child = std::process::Command::new(bin)
        // Este test NO pasa por `norte()` (necesita `spawn`, no `assert`), así
        // que el override del directorio de estado se repite AQUÍ. Sin él el
        // `cp` abriría el journal de verdad de quien corre la suite — ver
        // `config_dir_del_test`.
        .env("NORTE_CONFIG_DIR", config_dir_del_test())
        .arg("cp")
        .arg(&src)
        .arg(&dst)
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // Espera a que la copia haya ARRANCADO (el dst aparece) antes de señalar:
    // un sleep fijo era flaky — bajo carga, SIGINT podía llegar ANTES de que
    // el CLI instalara su handler de Ctrl-C, matándolo por señal (sin exit
    // code). Cuando el dst existe, el proceso booteó y el handler está vivo.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut started = false;
    loop {
        if dst.exists() {
            started = true;
            break;
        }
        // ¿Ya terminó (400 ficheros pequeños: carrera legítima)?
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    if started {
        // SIGINT al proceso, como un Ctrl-C real.
        // Invariante: pid de un hijo recién creado siempre es válido.
        unsafe_free_kill(child.id());
    }
    let status = child.wait().unwrap();

    match status.code() {
        Some(130) => {
            // Cancelado: puede haber árbol parcial, pero JAMÁS staging huérfano.
            let mut pending = vec![dir.path().to_path_buf()];
            while let Some(d) = pending.pop() {
                for e in std::fs::read_dir(&d).unwrap().flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    assert!(
                        !name.contains(".norte-partial"),
                        "staging huérfano tras SIGINT: {name}"
                    );
                    if e.file_type().unwrap().is_dir() {
                        pending.push(e.path());
                    }
                }
            }
        }
        Some(0) => {
            // Carrera legítima: la copia terminó antes de la señal.
        }
        other => panic!("exit code inesperado tras SIGINT: {other:?}"),
    }
}

/// `kill(pid, SIGINT)` sin dependencias: /proc no sirve para señales, así que
/// usamos el comando `kill` del sistema (portátil en unix).
#[cfg(unix)]
fn unsafe_free_kill(pid: u32) {
    let status = std::process::Command::new("kill")
        .arg("-INT")
        .arg(pid.to_string())
        .status()
        .expect("kill disponible");
    assert!(status.success(), "kill -INT falló");
}

/// ADR 0104 lo dejó escrito como hueco: `norte plugin uninstall` borraba el
/// disco por detrás de un daemon vivo, que descubre el catálogo al arrancar y
/// no vigila el directorio. ADR 0113: con `--daemon` va POR él; sin él borra
/// en el directorio propio y AVISA — y jamás toca el del daemon, que puede
/// ser otro: el socket por defecto no depende de `NORTE_CONFIG_DIR`.
///
/// Un plugin ROTO basta: el daemon lo cuenta en `errors` y `plugin list` lo
/// dice por stderr («`norte doctor` dice por qué», en los dos idiomas). La
/// afirmación de ANTES es la que hace que la de después signifique algo.
#[cfg(unix)]
#[test]
fn plugin_uninstall_va_por_el_daemon_solo_con_daemon() {
    /// El daemon muere con el test, también si una aserción revienta.
    struct Hijo(std::process::Child);
    impl Drop for Hijo {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    // Dos directorios con el MISMO plugin: el del daemon y el del CLI.
    let con_roto = || {
        let dir =
            tempfile::TempDir::new_in(env!("CARGO_TARGET_TMPDIR")).expect("tempdir de estado");
        let roto = dir.path().join("plugins").join("org.test.roto");
        std::fs::create_dir_all(&roto).unwrap();
        std::fs::write(roto.join("plugin.toml"), "esto no es un manifiesto").unwrap();
        (dir, roto)
    };
    let (del_daemon, roto_del_daemon) = con_roto();
    let (del_cli, roto_del_cli) = con_roto();
    // El socket en `/tmp` y no bajo el target: `sun_path` tiene 108 bytes.
    let run = tempfile::tempdir().unwrap();
    let socket = run.path().join("d.sock");

    let bin = assert_cmd::cargo::cargo_bin("norte");
    let _daemon = Hijo(
        std::process::Command::new(&bin)
            .env("NORTE_CONFIG_DIR", del_daemon.path())
            .args(["daemon", "run", "--idle-timeout", "0", "--socket"])
            .arg(&socket)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    // Hasta que ACEPTA, no hasta que el fichero existe: un `--daemon` que no
    // conecta arranca otro daemon por su cuenta, y el test mediría a ese.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::os::unix::net::UnixStream::connect(&socket).is_err() {
        assert!(
            std::time::Instant::now() < deadline,
            "el daemon no aceptó en 15 s"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let norte_en = |dir: &std::path::Path| {
        let mut c = Command::cargo_bin("norte").expect("binario norte compilado");
        c.env("NORTE_CONFIG_DIR", dir);
        c.arg("--socket").arg(&socket);
        c
    };
    let rotos_segun_el_daemon = || {
        let out = norte_en(del_daemon.path())
            .args(["--daemon", "plugin", "list"])
            .assert()
            .success();
        String::from_utf8_lossy(&out.get_output().stderr).contains("doctor")
    };

    assert!(
        rotos_segun_el_daemon(),
        "el daemon debía contar el plugin roto antes"
    );

    // Sin `--daemon`, desde OTRO directorio: borra el suyo, avisa, y el del
    // daemon —con el mismo id— sigue ahí. Antes de ADR 0113 esta orden iba
    // por el daemon que escuchaba y borraba en SU directorio.
    let sin = norte_en(del_cli.path())
        .args(["plugin", "uninstall", "org.test.roto"])
        .assert()
        .success();
    assert!(!roto_del_cli.exists(), "el plugin propio sigue en disco");
    assert!(
        roto_del_daemon.exists(),
        "sin --daemon se borró en el directorio del daemon"
    );
    assert!(
        String::from_utf8_lossy(&sin.get_output().stderr).contains("--daemon"),
        "avisa de que el daemon lo seguirá listando"
    );

    // Con `--daemon`: por él, en su directorio, y deja de anunciarlo.
    norte_en(del_daemon.path())
        .args(["--daemon", "plugin", "uninstall", "org.test.roto"])
        .assert()
        .success();
    assert!(
        !roto_del_daemon.exists(),
        "el plugin del daemon sigue en disco"
    );
    assert!(
        !rotos_segun_el_daemon(),
        "el daemon sigue anunciando un plugin desinstalado"
    );

    // Y lo que el daemon ya no tiene se dice contra SU catálogo.
    norte_en(del_cli.path())
        .args(["--daemon", "plugin", "uninstall", "org.test.roto"])
        .assert()
        .failure();
}

/// M3-4 T6: `policy grant` sin daemon en marcha falla LIMPIO (sin autoarranque
/// — conceder un scope a un daemon que no existe no tiene sentido). El socket
/// apunta a un path muerto en un tempdir.
#[cfg(unix)]
#[test]
fn policy_grant_sin_daemon_falla_claro() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("muerto.sock");
    let out = norte()
        .arg("--socket")
        .arg(&socket)
        .arg("policy")
        .arg("grant")
        .arg("1")
        .output()
        .unwrap();
    assert!(!out.status.success(), "sin daemon debe fallar");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("daemon") || err.contains("socket"),
        "error orientativo, fue: {err}"
    );
}

/// `mcp serve --help` lista la opción de sesión (el subcomando existe y es
/// coherente sin necesitar un daemon).
#[cfg(unix)]
#[test]
fn mcp_serve_help_menciona_session() {
    let out = norte()
        .arg("mcp")
        .arg("serve")
        .arg("--help")
        .output()
        .unwrap();
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("--session"), "help: {help}");
}

// ---------- ls --attrs (#108 bloque 2) ----------

#[cfg(unix)]
#[test]
fn ls_attrs_posix_via_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.txt"), b"hola").expect("seed");
    let out = norte()
        .args(["ls", "--json", "--attrs", "posix.mode"])
        .arg(dir.path())
        .assert()
        .success();
    let v: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("json válido");
    // Forma wire del bloque 1: {"posix.mode": {"uint": N}}.
    let mode = &v[0]["attrs"]["posix.mode"]["uint"];
    assert!(mode.is_u64(), "posix.mode uint presente: {v}");

    // Y en humano: columna `posix.mode=N` al final de la línea.
    let out = norte()
        .args(["ls", "--attrs", "posix.mode"])
        .arg(dir.path())
        .assert()
        .success();
    let text = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(text.contains("posix.mode="), "columna humana: {text}");
}

// ---------- index semantic embebido cablea el proveedor (M4-IA-2) ----------

/// Fix follow-up M4-IA-2: el path embebido de `run()` CABLEA el proveedor de
/// embeddings. Contraste en dos corridas sobre el mismo binario:
/// (a) sin `[ai]` → el engine no tiene proveedor: "Unsupported";
/// (b) con `[ai]` + `embed_provider` hacia un endpoint MUERTO → el fallo viene
///     del PROVEEDOR (ya cableado), jamás "Unsupported". El secreto va por
///     env (`NORTE_SECRET_AI_EMB`) para no tocar el keyring del OS en CI.
#[test]
fn index_semantic_embebido_cablea_proveedor() {
    // (a) config dir vacío: sin [ai] no hay proveedor → Unsupported.
    let vacio = tempfile::tempdir().expect("tempdir");
    let out = norte()
        .env("NORTE_CONFIG_DIR", vacio.path())
        .args(["index", "semantic", "hola"])
        .output()
        .expect("run");
    assert!(!out.status.success(), "sin [ai] debe fallar");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.to_ascii_lowercase().contains("unsupported"),
        "sin [ai] el engine no tiene proveedor: {stderr}"
    );

    // (b) [ai] + embed_provider hacia un endpoint muerto: el wiring instala
    // el proveedor y el error es SUYO (conexión), no Unsupported. Puerto
    // HERMÉTICO: bind a :0 (el OS elige uno libre) y drop inmediato — el
    // connect posterior es un refuse determinista y rápido, sin depender de
    // que un puerto fijo esté libre (o peor, escuchando) en la máquina de CI.
    let muerto = std::net::TcpListener::bind("127.0.0.1:0").expect("bind :0");
    let addr = muerto.local_addr().expect("addr");
    drop(muerto);
    let cfg = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        cfg.path().join("norte.toml"),
        format!(
            "[ai]\nenabled = true\nembed_provider = \"emb\"\n\n\
             [ai.providers.emb]\nkind = \"ollama\"\nmodel = \"m\"\n\
             base_url = \"http://{addr}\"\n"
        ),
    )
    .expect("norte.toml");
    let out = norte()
        .env("NORTE_CONFIG_DIR", cfg.path())
        .env("NORTE_SECRET_AI_EMB", "x")
        .args(["index", "semantic", "hola"])
        .output()
        .expect("run");
    assert!(!out.status.success(), "endpoint muerto debe fallar");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.to_ascii_lowercase().contains("unsupported"),
        "con [ai]+embed_provider el proveedor está CABLEADO (el fallo es del \
         proveedor, no Unsupported): {stderr}"
    );
}

/// `norte paths` contesta con el directorio que MANDA, no con el de siempre.
///
/// Es todo el valor del comando: quien pregunta dónde está su config suele
/// preguntarlo justo porque no está donde creía. Un `paths` que ignorase
/// `NORTE_CONFIG_DIR` daría la respuesta bonita y equivocada, que es peor que
/// no tener comando. Se comprueba a nivel de PROCESO porque la resolución vive
/// en el entorno, que es lo único que un test de unidad no puede tocar.
#[test]
fn paths_respeta_el_config_dir_del_entorno() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("connections.toml"), "").unwrap();

    let out = Command::cargo_bin("norte")
        .expect("binario norte compilado")
        .env("NORTE_CONFIG_DIR", dir.path())
        .args(["paths", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
    let fila = |id: &str| {
        json.as_array()
            .expect("lista")
            .iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("falta la fila «{id}»: {json}"))
            .clone()
    };

    let conexiones = fila("connections");
    assert_eq!(
        conexiones["path"].as_str().expect("path"),
        dir.path().join("connections.toml").display().to_string(),
        "la ruta debe salir del NORTE_CONFIG_DIR, no del dir del usuario"
    );
    // `exists` es un hecho comprobado: el que se escribió consta, el que no,
    // no. Sin esto la columna podría ser un adorno constante.
    assert_eq!(conexiones["exists"], serde_json::json!(true));
    assert_eq!(fila("policy")["exists"], serde_json::json!(false));
    // Preguntar no crea nada (mismo criterio que `doctor`).
    assert!(
        !dir.path().join("policy.toml").exists(),
        "`paths` no puede crear lo que dice que falta"
    );
}
