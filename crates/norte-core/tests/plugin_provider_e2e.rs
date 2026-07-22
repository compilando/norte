//! E2E del adapter [`PluginProvider`] (#30 stage 2b, ADR 0032): compila el
//! guest `provider-mem` a `wasm32-wasip2`, lo envuelve en un
//! `norte_vfs::Provider` real y verifica el CONTRATO DE LECTURA — las mismas
//! comprobaciones que `readonly_provider_contract!` (stat/list/read/rangos/
//! nombres hostiles/caps/mutaciones=Unsupported), demostrando que la proyección
//! async→sync reensambla los streams correctamente.
//!
//! No invoca el macro literal porque este requiere el target `wasm32-wasip2` en
//! tiempo de test (dependencia de entorno) y el macro no sabe hacer SKIP; aquí
//! se hace SKIP explícito sin el target (consistente con los demás E2E de wasm).

use std::path::PathBuf;
use std::process::Command;

use futures::StreamExt;
use norte_core::plugin_provider::PluginProvider;
use norte_plugin_host::{Capabilities as HostCaps, PluginRuntime};
use norte_vfs::Provider;
use norte_vfs::proto::{ByteRange, CapabilityFlags, EntryKind, Error, Scheme, Segment, VPath};

fn root() -> VPath {
    VPath::root(Scheme::new("mem").expect("scheme mem"), None)
}

fn child(base: &VPath, name: &[u8]) -> VPath {
    base.join(Segment::new(name.to_vec()).expect("segmento"))
}

async fn read_all(
    p: &PluginProvider,
    path: &VPath,
    range: Option<ByteRange>,
) -> Result<Vec<u8>, Error> {
    let mut s = p.read(path, range).await?;
    let mut out = Vec::new();
    while let Some(c) = s.next().await {
        out.extend_from_slice(&c?);
    }
    Ok(out)
}

async fn list_names(p: &PluginProvider, dir: &VPath) -> Vec<Vec<u8>> {
    let mut names: Vec<Vec<u8>> = p
        .list(dir)
        .await
        .expect("list abre")
        .map(|e| {
            e.expect("entrada ok")
                .path
                .file_name()
                .expect("con nombre")
                .as_bytes()
                .to_vec()
        })
        .collect()
        .await;
    names.sort();
    names
}

// Un solo test (una compilación del guest) que recorre todo el contrato de
// lectura — de ahí su longitud.
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn plugin_provider_satisface_el_contrato_de_lectura() {
    let Some(wasm) = build_guest("provider-mem") else {
        eprintln!("SKIP: target wasm32-wasip2 no instalado");
        return;
    };
    let rt = PluginRuntime::new().expect("runtime");
    let p = PluginProvider::new(rt, &wasm, HostCaps::default(), "mem").expect("adapter");
    let root = root();

    // ---- capabilities: READ_ONLY, sin flags de escritura ----
    let flags = p.capabilities().flags;
    assert!(flags.contains(CapabilityFlags::READ_ONLY));
    for prohibido in [
        CapabilityFlags::RENAME_ATOMIC,
        CapabilityFlags::SERVER_COPY,
        CapabilityFlags::APPEND,
        CapabilityFlags::RANDOM_WRITE,
        CapabilityFlags::TRASH,
    ] {
        assert!(
            !flags.contains(prohibido),
            "READ_ONLY excluye {prohibido:?}"
        );
    }

    // ---- stat ----
    assert_eq!(
        p.stat(&root).await.expect("raíz existe").kind,
        EntryKind::Dir
    );
    assert_eq!(
        p.stat(&child(&root, b"no-existe")).await.unwrap_err(),
        Error::NotFound
    );
    let f = p
        .stat(&child(&child(&root, b"docs"), b"hello.txt"))
        .await
        .expect("hello.txt");
    assert_eq!(f.kind, EntryKind::File);
    assert_eq!(f.size, Some(11));

    // ---- list: árbol byte-exacto + kinds coherentes con stat ----
    assert_eq!(
        list_names(&p, &root).await,
        vec![b"docs".to_vec(), b"hostile".to_vec(), b"vacio.txt".to_vec()]
    );
    let docs = child(&root, b"docs");
    assert_eq!(
        list_names(&p, &docs).await,
        vec![b"hello.txt".to_vec(), b"sub".to_vec()]
    );
    let entries: Vec<_> = p
        .list(&docs)
        .await
        .expect("list docs")
        .map(|e| e.expect("entrada"))
        .collect()
        .await;
    for e in entries {
        assert_eq!(
            e.kind,
            p.stat(&e.path).await.expect("stat").kind,
            "{:?}",
            e.path
        );
    }
    // list de inexistente = NotFound; de un fichero = error.
    assert_eq!(
        p.list(&child(&root, b"no-existe")).await.err(),
        Some(Error::NotFound)
    );
    assert!(p.list(&child(&root, b"vacio.txt")).await.is_err());

    // ---- read: completo, vacío, rangos, past-EOF, missing, dir ----
    let hello = child(&child(&root, b"docs"), b"hello.txt");
    assert_eq!(read_all(&p, &hello, None).await.unwrap(), b"hola norte\n");
    assert_eq!(
        read_all(&p, &child(&child(&docs, b"sub"), b"nested.bin"), None)
            .await
            .unwrap(),
        b"\x00\x01\x02\xff"
    );
    assert_eq!(
        read_all(&p, &child(&root, b"vacio.txt"), None)
            .await
            .unwrap(),
        b""
    );
    // rango medio (offset 2, len 3) = "la ".
    assert_eq!(
        read_all(
            &p,
            &hello,
            Some(ByteRange {
                offset: 2,
                len: Some(3)
            })
        )
        .await
        .unwrap(),
        b"la "
    );
    // offset 8, hasta EOF = "te\n".
    assert_eq!(
        read_all(
            &p,
            &hello,
            Some(ByteRange {
                offset: 8,
                len: None
            })
        )
        .await
        .unwrap(),
        b"te\n"
    );
    // offset más allá de EOF = vacío, no error.
    assert_eq!(
        read_all(
            &p,
            &hello,
            Some(ByteRange {
                offset: 100,
                len: Some(4)
            })
        )
        .await
        .unwrap(),
        b""
    );
    // len que rebasa EOF = recortado.
    assert_eq!(
        read_all(
            &p,
            &hello,
            Some(ByteRange {
                offset: 7,
                len: Some(100)
            })
        )
        .await
        .unwrap(),
        b"rte\n"
    );
    assert_eq!(
        read_all(&p, &child(&root, b"no-existe"), None)
            .await
            .unwrap_err(),
        Error::NotFound
    );
    assert!(
        read_all(&p, &docs, None).await.is_err(),
        "leer un dir es error"
    );

    // ---- nombres hostiles byte-exactos ----
    // El guest siembra `a\xff\xfeb` (no-UTF8, VÁLIDO como Segment) y `a/b` (con
    // `/` INTERIOR — válido en el WIT pero NO representable como VPath): el
    // adapter OMITE el segundo (no hay ruta donde colgarlo), así que el listado
    // a nivel VPath solo trae el primero.
    let hostile = b"a\xff\xfeb".to_vec();
    let hdir = child(&root, b"hostile");
    assert_eq!(
        list_names(&p, &hdir).await,
        vec![hostile.clone()],
        "el nombre no-UTF8 sobrevive; el que lleva `/` se omite (no es VPath)"
    );
    assert_eq!(
        read_all(&p, &child(&hdir, &hostile), None).await.unwrap(),
        hostile,
        "contenido = bytes del nombre"
    );

    // ---- mutaciones: Unsupported (guest read-only) ----
    let nuevo = child(&root, b"nuevo");
    let existente = child(&root, b"vacio.txt");
    assert!(matches!(
        p.write(&nuevo).await.err(),
        Some(Error::Unsupported)
    ));
    assert!(matches!(
        p.write(&existente).await.err(),
        Some(Error::Unsupported)
    ));
    assert!(matches!(
        p.mkdir(&nuevo).await.err(),
        Some(Error::Unsupported)
    ));
    assert!(matches!(
        p.remove(&existente).await.err(),
        Some(Error::Unsupported)
    ));
    assert!(matches!(
        p.rename(&existente, &nuevo).await.err(),
        Some(Error::Unsupported)
    ));
    assert!(matches!(
        p.trash(&existente).await.err(),
        Some(Error::Unsupported)
    ));
    assert!(
        p.copy_native(&existente, &nuevo).await.is_none(),
        "sin SERVER_COPY, copy_native = None"
    );
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
