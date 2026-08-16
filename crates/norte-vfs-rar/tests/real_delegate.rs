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
