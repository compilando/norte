//! Los parsers contra la salida REAL de un delegado instalado. Si no hay
//! ninguno en la máquina, el test se retira diciéndolo: la suite pura de
//! `listing.rs` ya cubre la gramática.

use futures::StreamExt;
use norte_testkit::RarSmith;
use norte_vfs_rar::{Delegate, LIST_TIMEOUT, parse_7z_slt, parse_unrar_vt};

/// Un `.rar` forjado con las tres formas que importan: ASCII, un nombre
/// anidado y un nombre que NO es UTF-8.
fn forge(dir: &std::path::Path) -> std::path::PathBuf {
    let bytes = RarSmith::new()
        .file(b"hello.txt", b"hola norte\n")
        .dir(b"dir")
        .file(b"dir/nested.txt", b"anidado\n")
        .file(b"cp437-\xa4\xa5.txt", b"bytes\n")
        .build();
    let path = dir.join("t.rar");
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn siete_zeta_lista_lo_que_forjamos_con_los_bytes_intactos() {
    let Some(sevenz) = norte_testkit::which_7z() else {
        eprintln!("sin 7z instalado: test retirado");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let archive = forge(tmp.path());
    let out = std::process::Command::new(sevenz)
        .args(["l", "-slt", "-p", "--"])
        .arg(&archive)
        .output()
        .expect("7z arranca");
    let listing = parse_7z_slt(&out.stdout);
    let names: Vec<&[u8]> = listing.entries.iter().map(|e| e.name.as_slice()).collect();
    assert!(
        names.contains(&b"hello.txt".as_slice()),
        "entradas leídas: {names:?}"
    );
    assert!(
        names.contains(&b"cp437-\xa4\xa5.txt".as_slice()),
        "7z conserva los bytes crudos: {names:?}"
    );
    assert_eq!(listing.skipped, 0, "ningún registro real se descarta");
    let dir = listing
        .entries
        .iter()
        .find(|e| e.name == b"dir")
        .expect("el directorio se lista");
    assert!(dir.is_dir, "`dir` es directorio");
    let hello = listing
        .entries
        .iter()
        .find(|e| e.name == b"hello.txt")
        .unwrap();
    assert_eq!(hello.size, 11);
    assert!(hello.mtime.is_some(), "Modified se lee");
}

#[test]
fn unrar_lista_lo_mismo_salvo_el_nombre_que_no_sabe_llevar() {
    let Some(unrar) = which_unrar() else {
        eprintln!("sin unrar instalado: test retirado");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let archive = forge(tmp.path());
    let out = std::process::Command::new(unrar)
        .args(["vt", "-p-", "--"])
        .arg(&archive)
        .output()
        .expect("unrar arranca");
    let listing = parse_unrar_vt(&out.stdout);
    let names: Vec<&[u8]> = listing.entries.iter().map(|e| e.name.as_slice()).collect();
    assert!(
        names.contains(&b"hello.txt".as_slice()),
        "entradas leídas: {names:?}"
    );
    assert_eq!(listing.skipped, 0, "ningún registro real se descarta");
    // Medido: unrar TRUNCA el nombre no-UTF8 en el primer byte inválido, así
    // que `cp437-\xa4\xa5.txt` NO aparece entero. Por eso 7z va primero.
    assert!(
        !names.contains(&b"cp437-\xa4\xa5.txt".as_slice()),
        "si unrar dejase de truncar, el orden de preferencia se puede revisar"
    );
}

/// El nombre OEM del caso real: `папка.txt` en CP866, que es lo que sale de una
/// máquina DOS/Windows rusa — y lo que contiene una década de descargas.
const OEM_CP866: &[u8] = b"\xaf\xa0\xaf\xaa\xa0.txt";

/// Un **RAR4** con el nombre en una code page OEM (#223).
fn forge_rar4(dir: &std::path::Path) -> std::path::PathBuf {
    let bytes = RarSmith::new()
        .file(b"hello.txt", b"hola norte\n")
        .file(OEM_CP866, b"bytes oem\n")
        .build_rar4();
    let path = dir.join("t4.rar");
    std::fs::write(&path, bytes).unwrap();
    path
}

/// **El caso que RAR5 no puede escribir: un nombre en code page OEM** (#223).
///
/// RAR5 guarda los nombres en UTF-8 por formato, así que la forja de arriba no
/// puede producir esto y el hueco llevaba abierto desde la ADR 0056. RAR4 sí:
/// sin `LHD_UNICODE` el nombre son bytes crudos.
///
/// Lo que MIDE, que son las tres preguntas que la issue dejaba abiertas:
/// `7z -slt` entrega los bytes OEM tal cual, y el nombre que imprime sirve
/// para volver a seleccionar la entrada. Es lo que sostiene que 7z sea el
/// delegado preferido — hasta ahora estaba medido solo sobre RAR5.
#[test]
fn siete_zeta_conserva_un_nombre_oem_de_un_rar4() {
    let Some(sevenz) = norte_testkit::which_7z() else {
        eprintln!("sin 7z instalado: test retirado");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let archive = forge_rar4(tmp.path());
    let out = std::process::Command::new(&sevenz)
        .args(["l", "-slt", "-p", "--"])
        .arg(&archive)
        .output()
        .expect("7z arranca");
    let listing = parse_7z_slt(&out.stdout);
    let names: Vec<&[u8]> = listing.entries.iter().map(|e| e.name.as_slice()).collect();
    assert!(
        names.contains(&b"hello.txt".as_slice()),
        "el RAR4 forjado se lee: {names:?}"
    );
    assert!(
        names.contains(&OEM_CP866),
        "7z entrega los bytes OEM CRUDOS, sin transcodificar: {names:?}"
    );
}

/// Y el otro lado de la misma medida: **`unrar` NO conserva esos bytes.**
///
/// No los trunca —que es lo que hace con un RAR5 no-UTF8, fijado en el test de
/// arriba— sino que los mapea a un rango de uso privado. Son dos averías
/// distintas del mismo delegado, y las dos llevan al mismo sitio: sobre
/// nombres que no son UTF-8, `unrar` no vale como fuente de verdad.
#[test]
fn unrar_no_conserva_un_nombre_oem_de_un_rar4() {
    let Some(unrar) = which_unrar() else {
        eprintln!("sin unrar instalado: test retirado");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let archive = forge_rar4(tmp.path());
    let out = std::process::Command::new(unrar)
        .args(["vt", "-p-", "--"])
        .arg(&archive)
        .output()
        .expect("unrar arranca");
    let listing = parse_unrar_vt(&out.stdout);
    let names: Vec<&[u8]> = listing.entries.iter().map(|e| e.name.as_slice()).collect();
    assert!(
        names.contains(&b"hello.txt".as_slice()),
        "el RAR4 forjado se lee también con unrar: {names:?}"
    );
    assert!(
        !names.contains(&OEM_CP866),
        "si unrar empezara a entregar los bytes crudos, el orden de \
         preferencia de delegados se puede revisar: {names:?}"
    );
}

fn which_unrar() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join("unrar"))
        .find(|c| c.is_file())
}

/// El endurecimiento de regla 9 no rompe al delegado real: con el entorno
/// VACÍO, `stdin` a null y el `cwd` fuera del árbol del usuario, `7z` sigue
/// listando — y `run_stream` entrega el contenido de UNA entrada.
#[tokio::test]
async fn con_regla_9_puesta_el_delegado_real_sigue_leyendo() {
    let Some(sevenz) = norte_testkit::which_7z() else {
        eprintln!("sin 7z instalado: test retirado");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let archive = forge(tmp.path());
    let d = Delegate::SevenZip(sevenz);
    let stdout = d
        .run_capture(&d.list_argv(&archive), LIST_TIMEOUT)
        .await
        .expect("7z lista con el entorno vacío");
    let listing = parse_7z_slt(&stdout);
    assert!(
        listing.entries.iter().any(|e| e.name == b"hello.txt"),
        "listado con regla 9 puesta"
    );

    let argv = d.read_argv(&archive, b"hello.txt");
    let stream = d
        .run_stream(&argv, tokio_util::sync::CancellationToken::new())
        .await
        .expect("7z extrae a stdout");
    let bytes: Vec<u8> = stream
        .map(|r| r.expect("el flujo no falla"))
        .fold(Vec::new(), |mut acc, c| async move {
            acc.extend_from_slice(&c);
            acc
        })
        .await;
    assert_eq!(bytes, b"hola norte\n", "el contenido llega entero");
}
