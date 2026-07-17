//! Smoke tests del CLI (`assert_cmd`): los cuatro subcomandos contra un
//! tempdir real, incluida la cancelación por SIGINT (unix).

use assert_cmd::Command;

fn norte() -> Command {
    Command::cargo_bin("norte").expect("binario norte compilado")
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
