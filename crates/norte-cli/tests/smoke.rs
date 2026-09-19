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

/// `norte daemon run` arranca de punta a punta —journal, spool, policy,
/// índice, `bind`— y `norte daemon stop` lo para limpio.
///
/// Es el test de proceso de `norte_core::daemon::componer`: la composición
/// del daemon vivía en la CLI (regla 7) y ningún test la ejercía entera. Se
/// espera LEYENDO la línea que dice dónde escucha, no con un `sleep`: esa
/// línea sale después del `bind`, así que su llegada es la señal exacta.
#[cfg(unix)]
#[test]
fn daemon_run_arranca_y_stop_lo_para() {
    use std::io::BufRead as _;
    let config = tempfile::tempdir().expect("config");
    let estado = tempfile::tempdir().expect("estado");
    let socket = config.path().join("d.sock");
    let bin = assert_cmd::cargo::cargo_bin("norte");
    let mut hijo = std::process::Command::new(&bin)
        .env("NORTE_CONFIG_DIR", config.path())
        .env("XDG_STATE_HOME", estado.path())
        .args(["daemon", "run", "--idle-timeout", "0", "--socket"])
        .arg(&socket)
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("arranca");
    let stderr = hijo.stderr.take().expect("stderr");
    let (tx, rx) = std::sync::mpsc::channel();
    // Se lee HASTA EL FINAL, no hasta la línea buscada: soltar el pipe antes
    // haría que el siguiente `eprintln!` del daemon fallara y lo tumbara.
    std::thread::spawn(move || {
        for linea in std::io::BufReader::new(stderr).lines() {
            let Ok(linea) = linea else { break };
            let _ = tx.send(linea);
        }
    });
    let mut visto = Vec::new();
    loop {
        match rx.recv_timeout(std::time::Duration::from_secs(20)) {
            Ok(l) if l.contains(&socket.display().to_string()) => break,
            Ok(l) => visto.push(l),
            Err(e) => {
                let _ = hijo.kill();
                panic!("el daemon no llegó a escuchar ({e}): {visto:#?}");
            }
        }
    }
    let parada = norte()
        .env("NORTE_CONFIG_DIR", config.path())
        .args(["daemon", "stop", "--socket"])
        .arg(&socket)
        .output()
        .expect("stop");
    assert!(
        parada.status.success(),
        "stop: {}",
        String::from_utf8_lossy(&parada.stderr)
    );
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(hijo.wait());
    });
    let fin = rx
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("el daemon sale tras el stop")
        .expect("wait");
    assert!(fin.success(), "sale limpio: {fin:?}");
}

/// Un Ollama de mentira en `127.0.0.1:0` que contesta UNA petición de
/// `/api/chat` con `contenido` como único delta, y se cierra.
///
/// Lee la petición entera (cabeceras y el `Content-Length` del cuerpo) antes
/// de contestar: cerrar con bytes sin leer en el socket hace que el kernel
/// mande un RST, y el cliente vería un error de conexión en vez de la
/// respuesta.
fn ollama_de_mentira(contenido: &str) -> std::net::SocketAddr {
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind :0");
    let addr = listener.local_addr().expect("addr");
    let linea = serde_json::json!({ "message": { "content": contenido } }).to_string();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut lector = BufReader::new(stream.try_clone().expect("clone"));
        let mut largo = 0usize;
        loop {
            let mut cabecera = String::new();
            lector.read_line(&mut cabecera).expect("cabecera");
            let cabecera = cabecera.trim_end();
            if cabecera.is_empty() {
                break;
            }
            if let Some((nombre, valor)) = cabecera.split_once(':')
                && nombre.eq_ignore_ascii_case("content-length")
            {
                largo = valor.trim().parse().expect("content-length");
            }
        }
        let mut peticion = vec![0u8; largo];
        lector.read_exact(&mut peticion).expect("cuerpo");
        let cuerpo = format!("{linea}\n{{\"done\":true}}\n");
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n{cuerpo}",
            cuerpo.len()
        )
        .expect("respuesta");
    });
    addr
}

/// `norte ai rename` aplica el plan como UN lote del core, y no entrada a
/// entrada (regla 7).
///
/// Un intercambio `a↔b` es el caso que lo distingue: el planificador de lotes
/// lo rompe con un temporal; un bucle de `move_` intenta `a → b` con `b`
/// todavía ahí y, o falla, o pisa `b` antes de moverlo. Es lo que la TUI y la
/// ventana ya hacían; la CLI era el único camino que no.
#[test]
fn ai_rename_aplica_un_intercambio_como_un_lote() {
    let plan = r#"[{"from":"a.txt","to":"b.txt"},{"from":"b.txt","to":"a.txt"}]"#;
    let addr = ollama_de_mentira(plan);
    let cfg = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        cfg.path().join("norte.toml"),
        format!(
            "[ai]\nenabled = true\nrename_provider = \"loc\"\n\n\
             [ai.providers.loc]\nkind = \"ollama\"\nmodel = \"m\"\n\
             base_url = \"http://{addr}\"\n"
        ),
    )
    .expect("norte.toml");
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.txt"), "era a").expect("a");
    std::fs::write(dir.path().join("b.txt"), "era b").expect("b");

    let out = Command::cargo_bin("norte")
        .expect("binario norte compilado")
        .env("NORTE_CONFIG_DIR", cfg.path())
        .env("NORTE_SECRET_AI_LOC", "x")
        .args(["ai", "rename"])
        .arg(dir.path())
        .args(["intercambia", "--yes"])
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "el intercambio se aplica: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).expect("a"),
        "era b"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("b.txt")).expect("b"),
        "era a"
    );
    assert_eq!(
        std::fs::read_dir(dir.path()).expect("ls").count(),
        2,
        "no queda ningún temporal"
    );
}
