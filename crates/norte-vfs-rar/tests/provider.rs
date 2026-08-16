//! El provider de punta a punta: con el delegado instalado donde hace falta,
//! y con el índice fijado donde la política no necesita un `.rar` que la
//! contenga.

use futures::StreamExt;
use norte_proto::{ByteRange, Error, Scheme, Segment, VPath};
use norte_testkit::RarSmith;
use norte_vfs::Provider;
use norte_vfs_rar::{ArchiveIndex, Delegate, RarLimits, RarProvider, RawEntry};

/// El `VPath` `rar+file://…/!/…` de un archivo del host.
fn rar_root(archive: &std::path::Path) -> VPath {
    use std::os::unix::ffi::OsStrExt;
    let mut outer = VPath::root(Scheme::new("file").unwrap(), None);
    for comp in archive.components().skip(1) {
        outer = outer.join(Segment::new(comp.as_os_str().as_bytes().to_vec()).unwrap());
    }
    VPath::archive_compose("rar", &outer, &[]).expect("compose")
}

fn child(base: &VPath, name: &[u8]) -> VPath {
    base.join(Segment::new(name.to_vec()).unwrap())
}

async fn names(p: &RarProvider, at: &VPath) -> Vec<Vec<u8>> {
    p.list(at)
        .await
        .expect("list")
        .map(|e| {
            e.expect("entrada")
                .path
                .file_name()
                .unwrap()
                .as_bytes()
                .to_vec()
        })
        .collect()
        .await
}

async fn read_all(p: &RarProvider, at: &VPath, range: Option<ByteRange>) -> Vec<u8> {
    p.read(at, range)
        .await
        .expect("read")
        .fold(Vec::new(), |mut acc, chunk| async move {
            acc.extend_from_slice(&chunk.expect("chunk"));
            acc
        })
        .await
}

fn write_rar(path: &std::path::Path, bytes: Vec<u8>) {
    std::fs::write(path, bytes).unwrap();
}

/// El provider del `.rar` en `path`, o `None` si esta máquina no trae ningún
/// delegado (y entonces el test se retira diciéndolo).
fn provider(path: &std::path::Path) -> Option<RarProvider> {
    let delegate = Delegate::discover().ok()?;
    Some(RarProvider::new(
        path.to_path_buf(),
        delegate,
        RarLimits::default(),
    ))
}

#[tokio::test]
async fn listar_y_leer_contra_un_delegado_real() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("t.rar");
    write_rar(
        &archive,
        RarSmith::new()
            .file(b"docs/hello.txt", b"hola norte\n")
            .file(b"cp437-\xa4\xa5.txt", b"bytes\n")
            .build(),
    );
    let Some(p) = provider(&archive) else {
        eprintln!("sin delegado instalado: test retirado");
        return;
    };
    let root = rar_root(&archive);
    let mut top = names(&p, &root).await;
    top.sort();
    assert_eq!(top, vec![b"cp437-\xa4\xa5.txt".to_vec(), b"docs".to_vec()]);

    let leaf = child(&child(&root, b"docs"), b"hello.txt");
    assert_eq!(read_all(&p, &leaf, None).await, b"hola norte\n");
    let stat = p.stat(&leaf).await.expect("stat");
    assert_eq!(stat.size, Some(11));
    assert_eq!(p.list_skipped(&root).await.unwrap(), Some(0));
}

#[tokio::test]
async fn un_rango_devuelve_el_tramo_y_no_espera_al_resto() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("t.rar");
    write_rar(
        &archive,
        RarSmith::new().file(b"hello.txt", b"hola norte\n").build(),
    );
    let Some(p) = provider(&archive) else {
        eprintln!("sin delegado instalado: test retirado");
        return;
    };
    let leaf = child(&rar_root(&archive), b"hello.txt");
    let range = Some(ByteRange {
        offset: 5,
        len: Some(5),
    });
    // `hola norte\n`: el byte 5 es la `n`, no el espacio.
    assert_eq!(read_all(&p, &leaf, range).await, b"norte");
    let hasta_el_final = Some(ByteRange {
        offset: 5,
        len: None,
    });
    assert_eq!(read_all(&p, &leaf, hasta_el_final).await, b"norte\n");
}

/// MEDIDO: los dos delegados tratan el nombre como patrón. Con el gemelo
/// dentro, pedir la entrada `star?name.txt` sacaría DOS ficheros pegados y
/// el flujo parecería sano — así que se rehúsa.
#[tokio::test]
async fn un_nombre_que_es_glob_de_otro_no_se_lee_pero_si_se_lista() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("t.rar");
    write_rar(
        &archive,
        RarSmith::new()
            .file(b"star?name.txt", b"patron\n")
            .file(b"starXname.txt", b"gemelo\n")
            .build(),
    );
    let Some(p) = provider(&archive) else {
        eprintln!("sin delegado instalado: test retirado");
        return;
    };
    let root = rar_root(&archive);
    assert_eq!(names(&p, &root).await.len(), 2, "las dos se LISTAN");
    let ambigua = child(&root, b"star?name.txt");
    assert!(
        matches!(p.read(&ambigua, None).await, Err(Error::Unsupported)),
        "la ambigua se rehúsa"
    );
    let literal = child(&root, b"starXname.txt");
    assert_eq!(read_all(&p, &literal, None).await, b"gemelo\n");
}

/// El flag viene del parser: no hace falta un `.rar` cifrado de verdad para
/// fijar la política.
#[tokio::test]
async fn una_entrada_cifrada_se_lista_y_se_niega_a_leerse() {
    let index = ArchiveIndex::from_raw(vec![RawEntry {
        name: b"secreto.txt".to_vec(),
        size: 10,
        is_dir: false,
        mtime: None,
        encrypted: true,
        solid: false,
    }]);
    let p = RarProvider::with_index_for_test(index);
    let leaf = child(
        &rar_root(std::path::Path::new("/tmp/t.rar")),
        b"secreto.txt",
    );
    assert!(p.stat(&leaf).await.is_ok(), "cifrada pero VISIBLE");
    assert!(
        matches!(p.read(&leaf, None).await, Err(Error::Unsupported)),
        "leerla es lo que no se puede, y se dice"
    );
}

/// Misma invalidación que `norte-vfs-archive`: `(mtime, size)`. Un índice
/// rancio enseña ficheros que ya no están.
#[tokio::test]
async fn tocar_el_archivo_invalida_el_indice_cacheado() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("t.rar");
    write_rar(&archive, RarSmith::new().file(b"uno.txt", b"1\n").build());
    let Some(p) = provider(&archive) else {
        eprintln!("sin delegado instalado: test retirado");
        return;
    };
    let root = rar_root(&archive);
    assert_eq!(names(&p, &root).await.len(), 1);
    write_rar(
        &archive,
        RarSmith::new()
            .file(b"uno.txt", b"1\n")
            .file(b"dos.txt", b"2\n")
            .build(),
    );
    assert_eq!(
        names(&p, &root).await.len(),
        2,
        "el índice se reconstruyó al cambiar el archivo"
    );
}

#[tokio::test]
async fn toda_mutacion_responde_unsupported() {
    let p = RarProvider::with_index_for_test(ArchiveIndex::from_raw(vec![]));
    let root = rar_root(std::path::Path::new("/tmp/t.rar"));
    let hijo = child(&root, b"x");
    assert!(matches!(
        p.write(&hijo).await.err(),
        Some(Error::Unsupported)
    ));
    assert!(matches!(p.mkdir(&hijo).await, Err(Error::Unsupported)));
    assert!(matches!(p.remove(&hijo).await, Err(Error::Unsupported)));
    assert!(matches!(
        p.rename(&hijo, &root).await,
        Err(Error::Unsupported)
    ));
    assert!(
        p.capabilities()
            .flags
            .contains(norte_proto::CapabilityFlags::READ_ONLY)
    );
}

/// Un `.rar` que no existe no es un listado vacío: es `NotFound`.
#[tokio::test]
async fn un_archivo_ausente_es_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("no-existe.rar");
    let Some(p) = provider(&archive) else {
        return;
    };
    assert!(matches!(
        p.list(&rar_root(&archive)).await,
        Err(Error::NotFound)
    ));
}
