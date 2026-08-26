//! El plugin oficial de columnas, INSTALADO como se instalaría el de un
//! tercero, contra un repositorio de git de verdad.
//!
//! Que ese camino funcione es lo que se está probando: embeber el `.wasm` en
//! el binario probaría otra cosa. También se mide lo que cuesta una página,
//! porque la historia de rendimiento de esta interfaz no se había ejercitado
//! nunca.
//!
//! SKIP con un mensaje si falta el target `wasm32-wasip2` o si no hay `git`
//! instalado: la misma convención que el resto de los e2e de wasm.

use std::path::{Path, PathBuf};
use std::process::Command;

use norte_core::PluginRegistry;
use norte_plugin_host::PluginRuntime;
use norte_proto::VPath;

/// El manifiesto real del plugin, leído de su directorio: si el fichero que se
/// distribuye y el que se prueba pudieran divergir, este test no probaría el
/// plugin sino una copia suya.
fn manifest() -> String {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/git-status/plugin.toml");
    std::fs::read_to_string(path).expect("el manifiesto del plugin")
}

/// Compila el plugin a `wasm32-wasip2`, o `None` si el target no está.
fn build_git_status() -> Option<PathBuf> {
    if !target_installed("wasm32-wasip2") {
        eprintln!("SKIP: target wasm32-wasip2 no instalado");
        return None;
    }
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/git-status");
    let target_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("wasm-guests");
    let status = Command::new(env!("CARGO"))
        .current_dir(&dir)
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-wasip2",
            "--target-dir",
        ])
        .arg(&target_dir)
        .status()
        .expect("cargo para el plugin de git");
    assert!(status.success(), "el plugin de git no compiló");
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join("git_status.wasm");
    assert!(wasm.exists(), "no está {}", wasm.display());
    Some(wasm)
}

fn target_installed(target: &str) -> bool {
    Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l == target)
        })
}

/// Instala el plugin bajo `cfg/plugins/<id>/` y lo aprueba y activa, que es
/// lo que haría una persona en el gestor de extensiones.
fn instala_y_aprueba(cfg: &Path, wasm: &Path) -> PluginRegistry {
    let dir = cfg.join("plugins").join("org.norte.git-status");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("plugin.toml"), manifest()).expect("manifest");
    std::fs::copy(wasm, dir.join("plugin.wasm")).expect("copy wasm");
    let mut reg = PluginRegistry::discover(cfg).expect("discover");
    assert!(reg.set_approval_in_memory("org.norte.git-status", true));
    assert!(reg.set_enabled_in_memory("org.norte.git-status", true));
    reg
}

/// Un repositorio con un commit: `limpio.txt` y `sucio.txt` rastreados,
/// `nuevo.txt` sin rastrear, `basura.tmp` ignorada. `None` si no hay `git`.
fn repo_fixture(dir: &Path) -> Option<()> {
    let git = |args: &[&str]| {
        Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
    };
    git(&["init", "-q"])?;
    git(&["config", "user.email", "t@t"])?;
    git(&["config", "user.name", "t"])?;
    std::fs::write(dir.join(".gitignore"), b"*.tmp\n").ok()?;
    std::fs::write(dir.join("limpio.txt"), b"limpio\n").ok()?;
    std::fs::write(dir.join("sucio.txt"), b"antes\n").ok()?;
    git(&["add", "-A"])?;
    git(&["commit", "-qm", "uno"])?;
    // DESPUÉS del commit: lo que el índice no ha visto.
    std::fs::write(dir.join("sucio.txt"), b"despues, y mas largo\n").ok()?;
    std::fs::write(dir.join("nuevo.txt"), b"nuevo\n").ok()?;
    std::fs::write(dir.join("basura.tmp"), b"basura\n").ok()?;
    Some(())
}

fn vpath_de(path: &Path) -> VPath {
    norte_vfs_local::vpath_from_native(path).expect("vpath")
}

/// El camino entero: instalado como un plugin ajeno, aprobado por una
/// persona, corriendo sobre un repositorio real y contestando por página.
#[test]
fn el_plugin_instalado_pinta_la_columna_wasm_real() {
    let Some(wasm) = build_git_status() else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    let repo = tempfile::tempdir().expect("tempdir");
    if repo_fixture(repo.path()).is_none() {
        eprintln!("SKIP: sin `git` instalado no hay repositorio que mirar");
        return;
    }
    let reg = instala_y_aprueba(cfg.path(), &wasm);
    let (_, _, wasm_path, caps, settings) = reg
        .resolve_columns_of(Some("org.norte.git-status"), "git-status")
        .expect("el plugin resuelve tras aprobarlo");
    assert!(
        caps.location.granted() && caps.location_root_marker.as_deref() == Some(".git"),
        "el manifiesto real pide ubicación con marcador `.git`"
    );

    let nombres: Vec<Vec<u8>> = vec![
        b"limpio.txt".to_vec(),
        b"sucio.txt".to_vec(),
        b"nuevo.txt".to_vec(),
        b"basura.tmp".to_vec(),
    ];
    let runtime = PluginRuntime::new().expect("runtime");
    let valores = norte_core::plugins::run_column_values_for_test(
        &runtime,
        (
            "org.norte.git-status".to_owned(),
            "Git status".to_owned(),
            wasm_path,
            caps,
            settings,
        ),
        "git-status",
        Some(&vpath_de(repo.path())),
        true,
        &nombres,
        nombres.len(),
    );
    assert_eq!(
        valores,
        vec![
            None,
            Some("M".to_owned()),
            Some("?".to_owned()),
            Some("!".to_owned())
        ],
        "limpio, modificado, sin rastrear, ignorado"
    );
}

/// El plugin trabaja igual DENTRO del repositorio, que es el caso que el
/// marcador de raíz existe para resolver: la raíz que el host abre es el
/// repositorio y el prefijo sitúa la página.
#[test]
fn dentro_de_un_subdirectorio_tambien_wasm_real() {
    let Some(wasm) = build_git_status() else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    let repo = tempfile::tempdir().expect("tempdir");
    if repo_fixture(repo.path()).is_none() {
        eprintln!("SKIP: sin `git` instalado no hay repositorio que mirar");
        return;
    }
    let sub = repo.path().join("src/deep");
    std::fs::create_dir_all(&sub).expect("mkdir");
    std::fs::write(sub.join("hondo.txt"), b"hondo\n").expect("write");

    let reg = instala_y_aprueba(cfg.path(), &wasm);
    let (_, _, wasm_path, caps, settings) = reg
        .resolve_columns_of(Some("org.norte.git-status"), "git-status")
        .expect("resuelve");
    let runtime = PluginRuntime::new().expect("runtime");
    let valores = norte_core::plugins::run_column_values_for_test(
        &runtime,
        (
            "org.norte.git-status".to_owned(),
            "Git status".to_owned(),
            wasm_path,
            caps,
            settings,
        ),
        "git-status",
        Some(&vpath_de(&sub)),
        true,
        &[b"hondo.txt".to_vec()],
        1,
    );
    assert_eq!(
        valores,
        vec![Some("?".to_owned())],
        "un fichero nuevo tres niveles dentro sigue siendo `sin rastrear`"
    );
}

/// Lo que cuesta UNA página, medido, porque la historia de rendimiento de esta
/// interfaz no se había ejercitado nunca.
///
/// El tope es deliberadamente flojo (dos segundos para veinte celdas sobre un
/// índice de dos mil entradas): lo que este test defiende no es una cifra sino
/// el orden de magnitud — si un día se vuelve segundos por página, algo se
/// rompió, y el número medido queda impreso para saber desde dónde.
#[test]
fn una_pagina_sobre_un_indice_grande_cuesta_lo_que_debe_wasm_real() {
    let Some(wasm) = build_git_status() else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    let repo = tempfile::tempdir().expect("tempdir");
    if repo_fixture(repo.path()).is_none() {
        eprintln!("SKIP: sin `git` instalado no hay repositorio que mirar");
        return;
    }
    let muchos = repo.path().join("muchos");
    std::fs::create_dir_all(&muchos).expect("mkdir");
    for i in 0..2_000 {
        std::fs::write(muchos.join(format!("f{i:05}.txt")), b"x\n").expect("write");
    }
    let ok = Command::new("git")
        .current_dir(repo.path())
        .args(["add", "-A"])
        .output()
        .is_ok_and(|o| o.status.success())
        && Command::new("git")
            .current_dir(repo.path())
            .args(["commit", "-qm", "muchos"])
            .output()
            .is_ok_and(|o| o.status.success());
    assert!(ok, "el commit de la fixture");

    let reg = instala_y_aprueba(cfg.path(), &wasm);
    let (_, _, wasm_path, caps, settings) = reg
        .resolve_columns_of(Some("org.norte.git-status"), "git-status")
        .expect("resuelve");
    let nombres: Vec<Vec<u8>> = (0..20)
        .map(|i| format!("f{i:05}.txt").into_bytes())
        .collect();
    let runtime = PluginRuntime::new().expect("runtime");

    let t0 = std::time::Instant::now();
    let valores = norte_core::plugins::run_column_values_for_test(
        &runtime,
        (
            "org.norte.git-status".to_owned(),
            "Git status".to_owned(),
            wasm_path,
            caps,
            settings,
        ),
        "git-status",
        Some(&vpath_de(&muchos)),
        true,
        &nombres,
        nombres.len(),
    );
    let coste = t0.elapsed();
    eprintln!("una página de 20 sobre 2000 entradas: {coste:?}");
    assert_eq!(valores.len(), 20);
    assert!(
        valores.iter().all(Option::is_none),
        "recién commiteados: todos limpios"
    );
    assert!(
        coste < std::time::Duration::from_secs(2),
        "una página tardó {coste:?}: eso ya no es una columna, es una espera"
    );
}

/// La SEGUNDA página del mismo directorio no vuelve a instanciar el componente
/// ni a parsear el índice desde cero (#224).
///
/// Lo que se afirma es el HECHO —la instancia se reutilizó—, no el
/// cronómetro: un test que exija «la segunda tarda la mitad» se pone rojo el
/// día que la máquina va cargada, y eso es ruido, no una regresión. El tiempo
/// se mide y se imprime igual, que es de donde salió el 167 ms de la issue.
///
/// Y la reutilización tiene un límite que también se fija aquí: cambiar de
/// directorio NO reutiliza. La ubicación es parte de la clave porque es lo que
/// el guest cachea dentro, y un `.git/index` parseado no vale para otro
/// proyecto.
#[test]
fn la_segunda_pagina_del_mismo_directorio_reutiliza_la_instancia() {
    let Some(wasm) = build_git_status() else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    let repo = tempfile::tempdir().expect("tempdir");
    if repo_fixture(repo.path()).is_none() {
        eprintln!("SKIP: sin `git` instalado no hay repositorio que mirar");
        return;
    }
    let dir = repo.path().join("muchos");
    std::fs::create_dir_all(&dir).expect("mkdir");
    for i in 0..200 {
        std::fs::write(dir.join(format!("f{i:05}.txt")), b"x\n").expect("write");
    }
    let otro = repo.path().join("otros");
    std::fs::create_dir_all(&otro).expect("mkdir");
    std::fs::write(otro.join("a.txt"), b"x\n").expect("write");

    let reg = instala_y_aprueba(cfg.path(), &wasm);
    let resuelto = |reg: &PluginRegistry| {
        let (_, _, wasm_path, caps, settings) = reg
            .resolve_columns_of(Some("org.norte.git-status"), "git-status")
            .expect("resuelve");
        (
            "org.norte.git-status".to_owned(),
            "Git status".to_owned(),
            wasm_path,
            caps,
            settings,
        )
    };
    let runtime = PluginRuntime::new().expect("runtime");
    let pool = norte_core::plugins::ColumnPool::default();
    let pagina = |desde: usize| -> Vec<Vec<u8>> {
        (desde..desde + 20)
            .map(|i| format!("f{i:05}.txt").into_bytes())
            .collect()
    };

    let primera = pagina(0);
    let t0 = std::time::Instant::now();
    let v1 = pool.column_values_for_test(
        &runtime,
        resuelto(&reg),
        "git-status",
        Some(&vpath_de(&dir)),
        true,
        &primera,
        primera.len(),
    );
    let coste1 = t0.elapsed();
    assert_eq!(v1.len(), 20);
    assert_eq!(
        pool.reutilizadas(),
        0,
        "la primera no puede reutilizar nada"
    );

    let segunda = pagina(20);
    let t1 = std::time::Instant::now();
    let v2 = pool.column_values_for_test(
        &runtime,
        resuelto(&reg),
        "git-status",
        Some(&vpath_de(&dir)),
        true,
        &segunda,
        segunda.len(),
    );
    let coste2 = t1.elapsed();
    assert_eq!(v2.len(), 20);
    assert_eq!(
        pool.reutilizadas(),
        1,
        "la segunda página del MISMO directorio tiene que caer en la instancia viva"
    );
    eprintln!("página 1: {coste1:?} · página 2 (reutilizando): {coste2:?}");

    // Otro directorio, otra caché del guest: no se reutiliza.
    let v3 = pool.column_values_for_test(
        &runtime,
        resuelto(&reg),
        "git-status",
        Some(&vpath_de(&otro)),
        true,
        &[b"a.txt".to_vec()],
        1,
    );
    assert_eq!(v3.len(), 1);
    assert_eq!(
        pool.reutilizadas(),
        1,
        "cambiar de ubicación instancia de nuevo: la clave lleva el directorio"
    );

    // Y volver al primero SÍ, que es lo que hace de esto un pool y no un
    // recuerdo de la última llamada.
    let v4 = pool.column_values_for_test(
        &runtime,
        resuelto(&reg),
        "git-status",
        Some(&vpath_de(&dir)),
        true,
        &primera,
        primera.len(),
    );
    assert_eq!(v4, v1, "el mismo directorio da los mismos valores");
    assert_eq!(pool.reutilizadas(), 2);
}
