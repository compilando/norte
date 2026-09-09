//! La revisión del árbol, en tiempo de compilación, para que las dos
//! ventanas —`ntc` y la gráfica— digan QUÉ binario son y no solo qué versión
//! declara el Cargo.toml: en un equipo de desarrollo los enlaces de `just link`
//! apuntan a `target/debug`, y «0.3.0-alpha.3» es lo mismo diez commits
//! después. `git describe` da la respuesta que la versión no da.
//!
//! Sin `.git` (un tarball de release, un `cargo install` desde crates.io) o
//! sin `git` en el PATH, se respeta `NORTE_REVISION` si el empaquetador lo
//! puso y, si no, queda `unknown`: jamás falla la compilación por esto.

use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=NORTE_REVISION");
    let revision = std::env::var("NORTE_REVISION")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(git_describe)
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=NORTE_REVISION={revision}");
}

/// `git describe --tags --always --dirty --long` sobre el árbol que contiene
/// este crate, y los `rerun-if-changed` que hacen que cambie con el HEAD.
fn git_describe() -> Option<String> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let top = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&manifest)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let top = String::from_utf8(top.stdout).ok()?.trim().to_owned();
    let git_dir = Path::new(&top).join(".git");
    // HEAD cambia al mover la rama; la ref a la que apunta, al commitear;
    // packed-refs y el índice, al etiquetar o tocar el árbol (el `-dirty`).
    for f in ["HEAD", "packed-refs", "index"] {
        println!("cargo:rerun-if-changed={}", git_dir.join(f).display());
    }
    if let Ok(head) = std::fs::read_to_string(git_dir.join("HEAD"))
        && let Some(r) = head.strip_prefix("ref: ")
    {
        println!(
            "cargo:rerun-if-changed={}",
            git_dir.join(r.trim()).display()
        );
    }
    let out = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty", "--long"])
        .current_dir(&top)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let s = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (!s.is_empty()).then_some(s)
}
