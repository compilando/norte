//! Nadie lanza trabajo sin su span (ADR 0127).
//!
//! `tokio::spawn` y `spawn_blocking` no heredan el span de quien los llama, y
//! lo que registra la tarea lanzada sale sin su `rpc` ni su `task`. La puerta
//! es `crate::blocking::{spawn, spawn_blocking}`. `spawn_blocking` lo impide
//! además clippy (`clippy.toml`); `tokio::spawn` no puede, porque los tests de
//! integración lo usan a montones y el lint no distingue. Esto lo impide en el
//! CÓDIGO de la crate, que es donde importa.

use std::path::{Path, PathBuf};

/// Los `.rs` de `src/`, recursivamente.
fn fuentes(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).expect("src legible") {
        let p = e.expect("entrada legible").path();
        if p.is_dir() {
            fuentes(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Lo de un fichero ANTES de su primer módulo de tests: lo que se compila en
/// el binario de verdad.
fn codigo(texto: &str) -> &str {
    texto.find("#[cfg(test)]").map_or(texto, |i| &texto[..i])
}

#[test]
fn nadie_lanza_una_tarea_sin_su_span() {
    // Solo las dos puertas. Lo que no debe heredar el span va por
    // `spawn_raiz`, con su porqué escrito donde se llama.
    const PERMITIDOS: &[&str] = &["src/blocking.rs"];
    let raiz = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut ficheros = Vec::new();
    fuentes(&raiz.join("src"), &mut ficheros);
    let mut mal = Vec::new();
    for f in ficheros {
        let rel = f
            .strip_prefix(raiz)
            .expect("dentro de la crate")
            .to_string_lossy()
            .replace('\\', "/");
        if PERMITIDOS.contains(&rel.as_str()) {
            continue;
        }
        let texto = std::fs::read_to_string(&f).expect("fuente legible");
        for (n, linea) in codigo(&texto).lines().enumerate() {
            if linea.contains("tokio::spawn(") || linea.contains("tokio::task::spawn(") {
                mal.push(format!("{rel}:{}", n + 1));
            }
        }
    }
    assert!(
        mal.is_empty(),
        "lanzan una tarea sin su span; usa `crate::blocking::spawn`: {mal:#?}"
    );
}
