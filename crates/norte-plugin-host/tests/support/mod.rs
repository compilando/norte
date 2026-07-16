//! Soporte de tests: compila un guest WASM de `examples-wasm/` a
//! `wasm32-wasip2` bajo demanda y devuelve la ruta del componente.
//!
//! Si el target `wasm32-wasip2` no está instalado, el helper hace SKIP
//! (devuelve `None`) para que el test pase en toolchains sin ese target; si el
//! target ESTÁ pero el guest no compila, es un fallo real y aborta.

use std::path::PathBuf;
use std::process::Command;

/// Compila el guest `examples-wasm/<name>/` a `wasm32-wasip2` en modo release y
/// devuelve la ruta del `.wasm` producido.
///
/// Devuelve `None` (con un aviso por `stderr`) si el target `wasm32-wasip2` no
/// está instalado.
#[must_use]
pub fn build_guest(name: &str) -> Option<PathBuf> {
    if !target_installed("wasm32-wasip2") {
        eprintln!("SKIP: target wasm32-wasip2 no instalado");
        return None;
    }

    let guest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples-wasm")
        .join(name);
    let target_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("wasm-guests");

    let status = Command::new(env!("CARGO"))
        .current_dir(&guest_dir)
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-wasip2",
            "--target-dir",
        ])
        .arg(&target_dir)
        .status()
        .expect("no se pudo lanzar cargo para compilar el guest");
    assert!(
        status.success(),
        "el guest {name} no compiló (target wasm32-wasip2 presente)"
    );

    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join(format!("{}.wasm", name.replace('-', "_")));
    assert!(
        wasm.exists(),
        "no se encontró el artefacto {}",
        wasm.display()
    );
    Some(wasm)
}

/// `true` si `rustup` reporta `target` entre los instalados.
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
