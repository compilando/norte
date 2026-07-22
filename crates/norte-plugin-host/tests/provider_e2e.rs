//! E2E de la interfaz WIT `provider` (#30 stage 2, ADR 0032): compila el guest
//! REAL `examples-wasm/provider-mem` a `wasm32-wasip2` y lo ejecuta a través de
//! [`PluginRuntime::instantiate_provider`], verificando la proyección
//! async→sync del trait `Provider`:
//!
//! - `capabilities` → `read-only`.
//! - `stat` de fichero/dir/inexistente.
//! - `list-dir` PAGINADO (páginas de 2 → dos llamadas con cursor).
//! - `read` por RANGO (offset+len, chunk corto = EOF).
//! - un nombre HOSTIL (`a\xff\xfeb`) round-trip BYTE-EXACTO (regla 1).
//!
//! SKIP si el target `wasm32-wasip2` no está instalado.

use std::path::PathBuf;
use std::process::Command;

use norte_plugin_host::{Capabilities, PluginRuntime, provider_iface::EntryKind};

/// Segmentos de un path desde `&[&[u8]]`.
fn segs(parts: &[&[u8]]) -> Vec<Vec<u8>> {
    parts.iter().map(|p| p.to_vec()).collect()
}

#[test]
fn provider_wit_e2e_wasm_real() {
    let Some(wasm) = build_guest("provider-mem") else {
        eprintln!("SKIP: target wasm32-wasip2 no instalado");
        return;
    };

    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut inst = rt
        .instantiate_provider(&wasm, Capabilities::default())
        .expect("instanciar el provider");

    // capabilities: read-only.
    assert!(
        inst.capabilities().expect("capabilities").read_only,
        "el provider-mem se declara read-only"
    );

    // stat de un fichero: kind File + size del contenido "hola norte\n" (11).
    let st = inst
        .stat(&segs(&[b"docs", b"hello.txt"]))
        .expect("stat sin trap")
        .expect("hello.txt existe");
    assert_eq!(st.kind, EntryKind::File);
    assert_eq!(st.name, b"hello.txt");
    assert_eq!(st.size, Some(11));

    // stat de un dir: kind Dir.
    let d = inst
        .stat(&segs(&[b"docs"]))
        .expect("stat sin trap")
        .expect("docs existe");
    assert_eq!(d.kind, EntryKind::Dir);

    // stat inexistente: VfsError::NotFound (lógico, no trap).
    assert!(
        inst.stat(&segs(&[b"no_existe"]))
            .expect("stat sin trap")
            .is_err(),
        "un path inexistente da error lógico"
    );

    // list-dir PAGINADO de la raíz: 3 hijos (docs, vacio.txt, hostile) en
    // páginas de 2 → primera página 2 + cursor, segunda 1 + None.
    let p1 = inst
        .list_dir(&[], None)
        .expect("list sin trap")
        .expect("raíz lista");
    assert_eq!(p1.entries.len(), 2, "primera página = 2 entradas");
    let cursor = p1.next_cursor.expect("hay segunda página");
    let p2 = inst
        .list_dir(&[], Some(cursor))
        .expect("list sin trap")
        .expect("raíz página 2");
    assert_eq!(p2.entries.len(), 1, "segunda página = 1 entrada");
    assert!(p2.next_cursor.is_none(), "no hay tercera página");
    // Reensamblado: los 3 nombres.
    let mut names: Vec<Vec<u8>> = p1
        .entries
        .iter()
        .chain(&p2.entries)
        .map(|e| e.name.clone())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![b"docs".to_vec(), b"hostile".to_vec(), b"vacio.txt".to_vec()]
    );

    // read por RANGO: "hola norte\n" → offset 5, len 5 = "norte".
    let chunk = inst
        .read(&segs(&[b"docs", b"hello.txt"]), 5, 5)
        .expect("read sin trap")
        .expect("hello.txt legible");
    assert_eq!(chunk, b"norte");
    // len que rebasa el fin → chunk corto (EOF): desde 5, pide 100, da 6.
    let tail = inst
        .read(&segs(&[b"docs", b"hello.txt"]), 5, 100)
        .expect("read sin trap")
        .expect("legible");
    assert_eq!(tail, b"norte\n");
    // offset en el fin → chunk vacío (EOF).
    let eof = inst
        .read(&segs(&[b"docs", b"hello.txt"]), 11, 10)
        .expect("read sin trap")
        .expect("legible");
    assert!(eof.is_empty(), "offset en EOF = chunk vacío");

    // fichero vacío: read da vacío; stat da size 0.
    assert_eq!(
        inst.stat(&segs(&[b"vacio.txt"])).unwrap().unwrap().size,
        Some(0)
    );

    // NOMBRE HOSTIL byte-exacto (regla 1): /hostile/<a\xff\xfeb> existe, se
    // lista con sus bytes crudos, y su contenido son esos mismos bytes.
    let hostile: &[u8] = b"a\xff\xfeb";
    let hp = inst
        .list_dir(&segs(&[b"hostile"]), None)
        .expect("list sin trap")
        .expect("hostile lista");
    assert_eq!(hp.entries.len(), 1);
    assert_eq!(
        hp.entries[0].name, hostile,
        "el nombre no-UTF8 cruza el WIT byte a byte"
    );
    let hbytes = inst
        .read(&segs(&[b"hostile", hostile]), 0, 100)
        .expect("read sin trap")
        .expect("legible");
    assert_eq!(
        hbytes, hostile,
        "el contenido hostil round-trip byte-exacto"
    );
}

/// Compila `examples-wasm/<name>/` a `wasm32-wasip2` (release). `None` (SKIP) si
/// el target no está; si está pero no compila, es fallo real.
fn build_guest(name: &str) -> Option<PathBuf> {
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
    assert!(status.success(), "el guest {name} no compiló");
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join(format!("{}.wasm", name.replace('-', "_")));
    assert!(wasm.exists(), "no se encontró {}", wasm.display());
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
