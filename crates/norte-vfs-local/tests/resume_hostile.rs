//! El staging ESTABLE del camino POR RUTA es un nombre PREDECIBLE (#298).
//!
//! `.norte-partial.` más el sha256-128 del nombre final: lo calcula cualquiera
//! que sepa a dónde vamos a copiar. Así que lo que hay al otro lado de ese
//! nombre puede haberlo puesto otro, y reanudar sobre ello publica bajo el
//! nombre legítimo un inodo ajeno —con su contenido, su dueño y sus permisos—
//! o anexa nuestros bytes al fichero de una víctima.
//!
//! Es el mismo agujero que #297 cerró para el camino CONFINADO
//! (`tests/confined.rs`), y aquí llevaba abierto desde ADR 0012 con menos
//! defensas: la apertura era `append(true).create(true)`, que sigue enlaces,
//! no mira el tipo, ni `st_nlink`, ni el dueño, y crea con `0o666`.
#![cfg(unix)]

use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::PermissionsExt as _;

use bytes::Bytes;
use norte_proto::{Error, Segment, VPath};
use norte_vfs::Provider;
use norte_vfs_local::LocalProvider;

fn provider() -> (LocalProvider, VPath, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().to_path_buf();
    let p = LocalProvider::rooted(base.clone()).with_guard(Box::new(dir));
    (p, LocalProvider::root(), base)
}

fn child(base: &VPath, name: &[u8]) -> VPath {
    base.join(Segment::new(name.to_vec()).expect("segmento válido"))
}

/// El nombre del staging, tal como lo calcula quien sepa el nombre de destino.
fn nombre_de_staging(final_name: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    let d = Sha256::digest(final_name);
    let mut hex = String::new();
    for b in &d[..16] {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    format!(".norte-partial.{hex}")
}

/// Un FICHERO REGULAR plantado con el nombre del staging no se reanuda.
///
/// Se le pone un hardlink para que «este inodo tiene otro nombre» sea
/// observable sin depender del uid: el test corre como el mismo usuario que
/// plantó el fichero, así que el dueño no distingue nada aquí.
#[tokio::test]
async fn un_staging_plantado_por_otro_no_se_reanuda_por_ruta() {
    let (p, root, base) = provider();
    let plantado = base.join(nombre_de_staging(b"grande.bin"));
    std::fs::write(&plantado, b"CONTENIDO AJENO").expect("plantado");
    std::fs::hard_link(&plantado, base.join("lo-mio.txt")).expect("hardlink");

    let Err(err) = p.open_resumable(&child(&root, b"grande.bin")).await else {
        panic!("reanudó sobre un fichero que no es suyo");
    };
    assert!(
        matches!(err, Error::Conflict { .. }),
        "tiene que ser un conflicto, no un error de E/S: {err:?}"
    );
    assert_eq!(
        std::fs::read(&plantado).expect("sigue"),
        b"CONTENIDO AJENO",
        "y no se le anexó nada"
    );
}

/// Un SYMLINK con el nombre del staging tampoco: seguirlo anexa nuestros bytes
/// al fichero de la víctima y luego el `commit` lo publica bajo el nombre
/// legítimo.
#[tokio::test]
async fn un_symlink_con_el_nombre_del_staging_no_se_sigue() {
    let (p, root, base) = provider();
    let victima = base.join("victima.txt");
    std::fs::write(&victima, b"DE LA VICTIMA").expect("víctima");
    std::os::unix::fs::symlink(&victima, base.join(nombre_de_staging(b"grande.bin")))
        .expect("symlink");

    let Err(err) = p.open_resumable(&child(&root, b"grande.bin")).await else {
        panic!("siguió un enlace hasta el fichero de otro");
    };
    assert!(
        !matches!(err, Error::NotFound),
        "el enlace existe: el error tiene que hablar de él, no de un ausente: {err:?}"
    );
    assert_eq!(
        std::fs::read(&victima).expect("sigue"),
        b"DE LA VICTIMA",
        "y la víctima intacta"
    );
}

/// Un FIFO con el nombre del staging no cuelga el abrir: sin `O_NONBLOCK` el
/// `open` se queda esperando un lector PARA SIEMPRE dentro del pool de
/// bloqueo, y el token de cancelación no puede interrumpir un `open` en curso.
#[tokio::test]
async fn un_fifo_con_el_nombre_del_staging_no_cuelga_el_abrir_por_ruta() {
    let (p, root, base) = provider();
    let fifo = base.join(nombre_de_staging(b"grande.bin"));
    let c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("cstring");
    // SAFETY: `c` es una CString viva y NUL-terminada; `mkfifo` no requiere
    // privilegio y solo escribe en el sistema de ficheros.
    let rc = unsafe { libc::mkfifo(c.as_ptr(), 0o666) };
    assert_eq!(rc, 0, "mkfifo: {}", std::io::Error::last_os_error());

    let r = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        p.open_resumable(&child(&root, b"grande.bin")),
    )
    .await
    .expect("el abrir tiene que volver, no colgarse");
    assert!(r.is_err(), "un FIFO no es un staging");
}

/// El staging se crea `0o600` y no `0o666`: mientras dure es nuestro y de
/// nadie más.
#[tokio::test]
async fn el_staging_por_ruta_se_crea_solo_para_nosotros() {
    let (p, root, base) = provider();
    let (mut sink, _) = p
        .open_resumable(&child(&root, b"grande.bin"))
        .await
        .expect("abre");
    sink.write(Bytes::from_static(b"abc")).await.expect("mitad");
    sink.keep().await.expect("conserva");

    let modo = std::fs::metadata(base.join(nombre_de_staging(b"grande.bin")))
        .expect("staging")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(modo, 0o600, "un parcial de otro no se escribe ni se lee");
}

/// Y el camino normal sigue funcionando: un staging que creamos nosotros se
/// reanuda. La defensa no puede costar la operación que existe para proteger.
#[tokio::test]
async fn un_staging_propio_si_se_reanuda_por_ruta() {
    let (p, root, _base) = provider();
    let destino = child(&root, b"grande.bin");
    let (mut sink, ya) = p.open_resumable(&destino).await.expect("abre");
    assert_eq!(ya, 0);
    sink.write(Bytes::from_static(b"abc")).await.expect("mitad");
    sink.keep().await.expect("conserva");

    let (_sink, ya) = p.open_resumable(&destino).await.expect("reabre lo suyo");
    assert_eq!(ya, 3, "el nuestro pasa la comprobación y se continúa");
}

/// Verificar el prefijo de un fichero que NO es el que se va a continuar no
/// verifica nada: el digest de un parcial ajeno no se devuelve, y el engine
/// degrada a `Length` en vez de creerse un prefijo que no es suyo.
#[tokio::test]
async fn el_digest_de_un_parcial_ajeno_no_se_devuelve() {
    let (p, root, base) = provider();
    let plantado = base.join(nombre_de_staging(b"grande.bin"));
    std::fs::write(&plantado, b"CONTENIDO AJENO").expect("plantado");
    std::fs::hard_link(&plantado, base.join("lo-mio.txt")).expect("hardlink");

    let d = p
        .partial_digest(&child(&root, b"grande.bin"), 3)
        .await
        .expect("no es un error de E/S");
    assert!(d.is_none(), "no es nuestro parcial: no hay digest que dar");
}
