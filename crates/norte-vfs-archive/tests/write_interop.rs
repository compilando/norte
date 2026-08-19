//! Lo que este crate escribe, leído por HERRAMIENTAS AJENAS (#250).
//!
//! `write_roundtrip.rs` lee lo escrito con nuestro propio índice, que es una
//! buena comprobación de consistencia codificador↔decodificador y CIEGA a
//! todo sitio donde el resto del mundo y nosotros discrepamos — que es
//! justamente donde vivieron los dos bloqueantes de formato de la rama de
//! #132. Durante el desarrollo el escritor de zip se validó a mano contra el
//! `zipfile` de Python, y así salió el fallo de la disposición del directorio
//! central. Esto convierte aquel ratón en un test.
//!
//! **Se SALTA con un mensaje cuando la herramienta no está** (misma convención
//! que los e2e de wasm): una máquina sin `unzip` no puede decir nada sobre
//! interoperabilidad, y fallar ahí sería llamar defecto a lo que no lo es.
//! Lo que no hace es pasar en silencio.

use std::io::Write as _;
use std::process::Command;

use norte_vfs_archive::write::{ArchiveWriter, PackEntry, PackFormat};

/// Empaqueta `entradas` y devuelve los bytes del archivo.
fn empaqueta(format: PackFormat, entradas: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    let mut w = ArchiveWriter::new(format, 6);
    let mut out = Vec::new();
    for (nombre, datos) in entradas {
        let mut e = PackEntry::file(nombre.clone(), datos.len() as u64);
        e.mtime_ms = Some(1_700_000_000_000);
        w.begin(&e).expect("abre");
        for trozo in datos.chunks(7) {
            w.data(trozo).expect("datos");
            out.extend(w.take());
        }
        w.end().expect("cierra");
        out.extend(w.take());
    }
    w.finish().expect("termina");
    out.extend(w.take());
    out
}

/// `true` si la herramienta está en el PATH. Cuando no, se DICE.
fn hay(programa: &str) -> bool {
    let ok = Command::new(programa)
        .arg("--help")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok();
    if !ok {
        eprintln!("saltado: `{programa}` no está en el PATH");
    }
    ok
}

/// Escribe `bytes` en un fichero temporal y devuelve su ruta.
fn en_disco(dir: &std::path::Path, nombre: &str, bytes: &[u8]) -> std::path::PathBuf {
    let p = dir.join(nombre);
    let mut f = std::fs::File::create(&p).expect("crea");
    f.write_all(bytes).expect("escribe");
    p
}

/// Un zip nuestro lo VERIFICA `unzip -t`, que es quien tiene la última palabra
/// sobre si el directorio central está donde el resto del mundo lo busca.
#[test]
fn un_zip_nuestro_lo_verifica_unzip() {
    if !hay("unzip") {
        return;
    }
    let dir = tempfile::tempdir().expect("tmp");
    let bytes = empaqueta(
        PackFormat::Zip,
        &[
            (b"uno.txt".to_vec(), b"contenido uno".to_vec()),
            (b"dos/tres.txt".to_vec(), b"y el de dentro".to_vec()),
            // Un nombre no-ASCII: el bit 11 dice que va en UTF-8, y quien lo
            // lee de fuera es el único que puede confirmar que se lo cree.
            ("cafe\u{301}.txt".as_bytes().to_vec(), b"nfd".to_vec()),
        ],
    );
    let zip = en_disco(dir.path(), "n.zip", &bytes);

    let salida = Command::new("unzip")
        .arg("-t")
        .arg(&zip)
        .output()
        .expect("unzip corre");
    assert!(
        salida.status.success(),
        "unzip -t rechaza nuestro zip:\n{}\n{}",
        String::from_utf8_lossy(&salida.stdout),
        String::from_utf8_lossy(&salida.stderr)
    );

    // Y que lo EXTRAE con los mismos bytes dentro, que es la pregunta de
    // verdad: `-t` valida las sumas, no que el contenido sea el nuestro.
    let fuera = dir.path().join("fuera");
    let salida = Command::new("unzip")
        .arg("-q")
        .arg(&zip)
        .arg("-d")
        .arg(&fuera)
        .output()
        .expect("unzip corre");
    assert!(salida.status.success(), "unzip no extrae");
    assert_eq!(
        std::fs::read(fuera.join("uno.txt")).expect("uno"),
        b"contenido uno"
    );
    assert_eq!(
        std::fs::read(fuera.join("dos/tres.txt")).expect("tres"),
        b"y el de dentro"
    );
}

/// Un tar nuestro lo lista `tar -tvf`, y lo extrae con los mismos bytes.
///
/// Incluye un nombre de MÁS de 100 bytes: es la frontera del `ustar` clásico,
/// donde el escritor tiene que emitir una cabecera GNU `L` — y nuestro lector
/// la entiende porque la escribimos nosotros, que es exactamente el argumento
/// circular que este fichero existe para romper.
#[test]
fn un_tar_nuestro_lo_lee_gnu_tar() {
    if !hay("tar") {
        return;
    }
    let dir = tempfile::tempdir().expect("tmp");
    let largo = format!("{}.txt", "a".repeat(120));
    let bytes = empaqueta(
        PackFormat::Tar,
        &[
            (b"uno.txt".to_vec(), b"contenido uno".to_vec()),
            (largo.as_bytes().to_vec(), b"cabecera larga".to_vec()),
        ],
    );
    let tar = en_disco(dir.path(), "n.tar", &bytes);

    let salida = Command::new("tar")
        .arg("-tvf")
        .arg(&tar)
        .output()
        .expect("tar corre");
    assert!(
        salida.status.success(),
        "tar -tvf rechaza nuestro tar:\n{}",
        String::from_utf8_lossy(&salida.stderr)
    );
    let listado = String::from_utf8_lossy(&salida.stdout).into_owned();
    assert!(listado.contains("uno.txt"), "lo lista: {listado}");
    assert!(
        listado.contains(&largo),
        "y el nombre largo entero, no truncado a 100: {listado}"
    );

    let fuera = dir.path().join("fuera");
    std::fs::create_dir_all(&fuera).expect("mkdir");
    let salida = Command::new("tar")
        .arg("-xf")
        .arg(&tar)
        .arg("-C")
        .arg(&fuera)
        .output()
        .expect("tar corre");
    assert!(
        salida.status.success(),
        "tar no extrae:\n{}",
        String::from_utf8_lossy(&salida.stderr)
    );
    assert_eq!(
        std::fs::read(fuera.join("uno.txt")).expect("uno"),
        b"contenido uno"
    );
    assert_eq!(
        std::fs::read(fuera.join(&largo)).expect("el largo"),
        b"cabecera larga"
    );
}

/// Y un `.tar.gz` nuestro, que añade una capa que `gzip` tiene que reconocer.
#[test]
fn un_targz_nuestro_lo_lee_gnu_tar() {
    if !hay("tar") {
        return;
    }
    let dir = tempfile::tempdir().expect("tmp");
    let bytes = empaqueta(
        PackFormat::TarGz,
        &[(b"uno.txt".to_vec(), b"comprimido".to_vec())],
    );
    let tgz = en_disco(dir.path(), "n.tar.gz", &bytes);

    let fuera = dir.path().join("fuera");
    std::fs::create_dir_all(&fuera).expect("mkdir");
    let salida = Command::new("tar")
        .arg("-xzf")
        .arg(&tgz)
        .arg("-C")
        .arg(&fuera)
        .output()
        .expect("tar corre");
    assert!(
        salida.status.success(),
        "tar -xzf rechaza nuestro tar.gz:\n{}",
        String::from_utf8_lossy(&salida.stderr)
    );
    assert_eq!(
        std::fs::read(fuera.join("uno.txt")).expect("uno"),
        b"comprimido"
    );
}
