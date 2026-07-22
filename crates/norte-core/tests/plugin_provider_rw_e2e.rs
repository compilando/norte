//! E2E del camino de ESCRITURA de [`PluginProvider`] (#30 stage 2b-write, ADR
//! 0032): compila el guest ESCRIBIBLE `provider-mem-rw`, lo envuelve en un
//! `norte_vfs::Provider` y verifica la proyección del `ByteSink` transaccional
//! (writer resource) + mkdir/remove/rename.
//!
//! SKIP sin el target `wasm32-wasip2`.

use std::path::PathBuf;
use std::process::Command;

use bytes::Bytes;
use futures::StreamExt;
use norte_core::plugin_provider::PluginProvider;
use norte_plugin_host::{Capabilities as HostCaps, PluginRuntime};
use norte_vfs::Provider;
use norte_vfs::proto::{CapabilityFlags, EntryKind, Error, Scheme, Segment, VPath};

fn root() -> VPath {
    VPath::root(Scheme::new("mem").expect("scheme"), None)
}

fn child(base: &VPath, name: &[u8]) -> VPath {
    base.join(Segment::new(name.to_vec()).expect("segmento"))
}

async fn read_all(p: &PluginProvider, path: &VPath) -> Result<Vec<u8>, Error> {
    let mut s = p.read(path, None).await?;
    let mut out = Vec::new();
    while let Some(c) = s.next().await {
        out.extend_from_slice(&c?);
    }
    Ok(out)
}

#[tokio::test]
async fn plugin_provider_camino_de_escritura() {
    let Some(wasm) = build_guest("provider-mem-rw") else {
        eprintln!("SKIP: target wasm32-wasip2 no instalado");
        return;
    };
    let rt = PluginRuntime::new().expect("runtime");
    let p = PluginProvider::new(rt, &wasm, HostCaps::default(), "mem").expect("adapter");
    let root = root();

    // El guest es ESCRIBIBLE: sin el flag READ_ONLY.
    assert!(!p.capabilities().flags.contains(CapabilityFlags::READ_ONLY));

    // ---- write + commit: crea el fichero con contenido byte-exacto (hostil) ----
    let nuevo = child(&root, b"nuevo.bin");
    let contenido: &[u8] = b"linea\x00\xff\xfe fin";
    // El staging es transaccional: antes del commit el path NO existe.
    let mut sink = p.write(&nuevo).await.expect("abre writer");
    assert_eq!(
        p.stat(&nuevo).await.unwrap_err(),
        Error::NotFound,
        "el path final no existe hasta commit"
    );
    sink.write(Bytes::from_static(b"linea\x00"))
        .await
        .expect("chunk 1");
    sink.write(Bytes::from_static(b"\xff\xfe fin"))
        .await
        .expect("chunk 2");
    sink.commit().await.expect("commit");
    // Ahora existe y su contenido es byte-exacto.
    let st = p.stat(&nuevo).await.expect("existe tras commit");
    assert_eq!(st.kind, EntryKind::File);
    assert_eq!(st.size, Some(contenido.len() as u64));
    assert_eq!(read_all(&p, &nuevo).await.unwrap(), contenido);

    // ---- abort: no publica nada ----
    let abortado = child(&root, b"abortado.txt");
    let mut sink = p.write(&abortado).await.expect("abre writer");
    sink.write(Bytes::from_static(b"basura"))
        .await
        .expect("chunk");
    sink.abort().await.expect("abort");
    assert_eq!(
        p.stat(&abortado).await.unwrap_err(),
        Error::NotFound,
        "abort no publica"
    );

    // ---- mkdir ----
    let dir = child(&root, b"nuevodir");
    p.mkdir(&dir).await.expect("mkdir");
    assert_eq!(p.stat(&dir).await.expect("dir existe").kind, EntryKind::Dir);
    // mkdir sobre algo existente = Conflict.
    assert!(matches!(
        p.mkdir(&dir).await.unwrap_err(),
        Error::Conflict { .. }
    ));

    // ---- remove ----
    let vacio = child(&root, b"vacio.txt");
    assert!(p.stat(&vacio).await.is_ok());
    p.remove(&vacio).await.expect("remove");
    assert_eq!(p.stat(&vacio).await.unwrap_err(), Error::NotFound);

    // ---- rename ----
    let hello = child(&child(&root, b"docs"), b"hello.txt");
    let renombrado = child(&child(&root, b"docs"), b"renombrado.txt");
    p.rename(&hello, &renombrado).await.expect("rename");
    assert_eq!(p.stat(&hello).await.unwrap_err(), Error::NotFound);
    assert_eq!(read_all(&p, &renombrado).await.unwrap(), b"hola norte\n");
}

fn build_guest(name: &str) -> Option<PathBuf> {
    if !target_installed("wasm32-wasip2") {
        eprintln!("SKIP: target wasm32-wasip2 no instalado");
        return None;
    }
    let guest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("norte-plugin-host")
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
        .expect("cargo build del guest");
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
