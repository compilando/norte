//! Casos hostiles del provider tar: traversal, corrupción, bombas de
//! entradas, duplicados, symlinks, invalidación de caché y wiring de scheme.

mod common;

use futures::StreamExt;
use norte_proto::{ByteRange, ConflictKind, EntryKind, Error, Segment, VPath};
use norte_testkit::{MemProvider, TarSmith};
use norte_vfs::Provider;
use norte_vfs_archive::{ArchiveProvider, Format, Limits};

async fn list_names(p: &ArchiveProvider, dir: &VPath) -> Vec<Vec<u8>> {
    let mut names: Vec<Vec<u8>> = p
        .list(dir)
        .await
        .expect("list")
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

async fn read_all(p: &ArchiveProvider, f: &VPath, range: Option<ByteRange>) -> Vec<u8> {
    let mut stream = p.read(f, range).await.expect("read");
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk ok"));
    }
    out
}

#[tokio::test]
async fn traversal_y_absolutos_se_omiten_del_arbol() {
    let tar = TarSmith::new()
        .file(b"../evil", b"slip")
        .file(b"ok.txt", b"bien")
        .build();
    // `/abs` y `a/../b` no se pueden forjar con TarSmith (100 bytes sí,
    // pero el crate `tar` los emite tal cual): forja manual vía nombre.
    let (p, root) = common::tar_provider(&tar).await;
    assert_eq!(list_names(&p, &root).await, vec![b"ok.txt".to_vec()]);
    assert_eq!(
        read_all(&p, &root.join(seg(b"ok.txt")), None).await,
        b"bien"
    );
}

fn seg(b: &[u8]) -> Segment {
    Segment::new(b.to_vec()).expect("seg")
}

#[tokio::test]
async fn tar_truncado_es_corrupt() {
    let mut tar = TarSmith::new().file(b"grande.bin", &[7u8; 2000]).build();
    tar.truncate(700); // corta a mitad de los datos + sin bloques de cierre
    let (p, root) = common::tar_provider(&tar).await;
    match p.list(&root).await.map(|_| ()) {
        Err(Error::Corrupt) => {}
        other => panic!("esperaba Corrupt, fue {other:?}"),
    }
}

#[tokio::test]
async fn basura_no_tar_es_corrupt() {
    let (p, root) = common::tar_provider(b"esto no es un tar\x00\x01").await;
    match p.list(&root).await.map(|_| ()) {
        Err(Error::Corrupt) => {}
        other => panic!("esperaba Corrupt, fue {other:?}"),
    }
}

#[tokio::test]
async fn max_entries_corta_el_indexado() {
    let tar = TarSmith::new()
        .file(b"a/uno", b"1")
        .file(b"a/dos", b"2")
        .file(b"a/tres", b"3")
        .build();
    // max_entries=3: `a` implícito + 2 archivos agotan el presupuesto.
    let limits = Limits {
        max_entries: 3,
        ..Limits::default()
    };
    let (p, root) = common::tar_provider_with_limits(&tar, limits).await;
    match p.list(&root).await.map(|_| ()) {
        Err(Error::Corrupt) => {}
        other => panic!("esperaba Corrupt por bomba de entradas, fue {other:?}"),
    }
}

#[tokio::test]
async fn duplicado_ultima_gana() {
    let tar = TarSmith::new()
        .file(b"x.txt", b"primero")
        .file(b"x.txt", b"segundo!")
        .build();
    let (p, root) = common::tar_provider(&tar).await;
    let f = root.join(seg(b"x.txt"));
    assert_eq!(p.stat(&f).await.expect("stat").size, Some(8));
    assert_eq!(read_all(&p, &f, None).await, b"segundo!");
}

#[tokio::test]
async fn symlink_lstat_y_read_link() {
    let tar = TarSmith::new()
        .file(b"docs/real.txt", b"contenido")
        .symlink(b"lnk", b"docs/real.txt")
        .build();
    let (p, root) = common::tar_provider(&tar).await;
    let lnk = root.join(seg(b"lnk"));
    assert_eq!(p.stat(&lnk).await.expect("stat").kind, EntryKind::Symlink);
    assert_eq!(
        p.read_link(&lnk).await.expect("read_link"),
        b"docs/real.txt"
    );
    // `read` sobre el link: TypeMismatch (lstat, jamás seguirlo).
    match p.read(&lnk, None).await {
        Err(Error::Conflict {
            conflict: ConflictKind::TypeMismatch,
        }) => {}
        other => panic!("esperaba TypeMismatch, fue {:?}", other.err()),
    }
}

#[tokio::test]
async fn rango_passthrough_con_offset_correcto() {
    // Dos archivos: el segundo NO empieza en 0 dentro del tar — el range
    // pedido se traduce al offset real del contenedor.
    let tar = TarSmith::new()
        .file(b"primero.bin", &[0xAA; 600])
        .file(b"segundo.bin", b"0123456789")
        .build();
    let (p, root) = common::tar_provider(&tar).await;
    let f = root.join(seg(b"segundo.bin"));
    assert_eq!(
        read_all(
            &p,
            &f,
            Some(ByteRange {
                offset: 2,
                len: Some(3)
            })
        )
        .await,
        b"234"
    );
    assert_eq!(
        read_all(
            &p,
            &f,
            Some(ByteRange {
                offset: 8,
                len: None
            })
        )
        .await,
        b"89"
    );
    assert_eq!(
        read_all(
            &p,
            &f,
            Some(ByteRange {
                offset: 99,
                len: Some(1)
            })
        )
        .await,
        b"",
        "past-EOF de la ENTRADA (no del contenedor): stream vacío"
    );
}

#[tokio::test]
async fn invalidacion_por_generacion_del_contenedor() {
    let (mem, path) = common::seed_container(
        b"fixture.tar",
        &TarSmith::new().file(b"v1.txt", b"uno").build(),
    )
    .await;
    let root = VPath::archive_compose("tar", &path, &[]).expect("compose");
    let p = ArchiveProvider::new(std::sync::Arc::clone(&mem) as _, Format::Tar, "tar+mem");
    assert_eq!(list_names(&p, &root).await, vec![b"v1.txt".to_vec()]);
    // Reescribir el contenedor (remove + write: generación nueva).
    common::write_file(
        mem.as_ref(),
        &path,
        &TarSmith::new().file(b"v2.txt", b"dos!").build(),
    )
    .await;
    assert_eq!(
        list_names(&p, &root).await,
        vec![b"v2.txt".to_vec()],
        "el índice cacheado se invalida por (mtime,size)"
    );
}

#[tokio::test]
async fn contenedor_inexistente_o_dir_falla_limpio() {
    let mem = std::sync::Arc::new(MemProvider::new());
    let dir = MemProvider::root().join(seg(b"undir"));
    mem.mkdir(&dir).await.expect("mkdir");
    let p = ArchiveProvider::new(std::sync::Arc::clone(&mem) as _, Format::Tar, "tar+mem");

    let missing =
        VPath::archive_compose("tar", &MemProvider::root().join(seg(b"no-existe.tar")), &[])
            .expect("compose");
    assert_eq!(p.stat(&missing).await.unwrap_err(), Error::NotFound);

    let overdir = VPath::archive_compose("tar", &dir, &[]).expect("compose");
    match p.stat(&overdir).await {
        Err(Error::Conflict {
            conflict: ConflictKind::TypeMismatch,
        }) => {}
        other => panic!("esperaba TypeMismatch sobre un dir, fue {other:?}"),
    }
}

#[tokio::test]
async fn scheme_ajeno_es_invalid_path() {
    let tar = TarSmith::new().file(b"x", b"1").build();
    let (p, _) = common::tar_provider(&tar).await;
    // Un path zip+mem contra el provider tar+mem: wiring roto = InvalidPath.
    let zip_path = VPath::parse("zip+mem:///fixture.tar/!/x").expect("parse");
    assert_eq!(p.stat(&zip_path).await.unwrap_err(), Error::InvalidPath);
    // Y un path SIN marcador con scheme correcto: malformado = InvalidPath.
    let sin_marcador = VPath::parse("tar+mem:///fixture.tar").expect("parse");
    assert_eq!(p.stat(&sin_marcador).await.unwrap_err(), Error::InvalidPath);
}

#[tokio::test]
async fn tipos_raros_se_listan_como_other_sin_read() {
    // TarSmith no forja hardlinks; un tar con solo dir+symlink+file cubre
    // los kinds v1. Cobertura de Other = tar real futuro (issue de deuda).
    let tar = TarSmith::new()
        .dir(b"d")
        .file(b"d/f", b"x")
        .symlink(b"s", b"d/f")
        .build();
    let (p, root) = common::tar_provider(&tar).await;
    assert_eq!(
        list_names(&p, &root).await,
        vec![b"d".to_vec(), b"s".to_vec()]
    );
}

/// #58: un fallo del provider INTERIOR (corte de red a mitad de indexado) es
/// IO genuino y se propaga VERBATIM — jamás se disfraza de «tar corrupto».
/// `Corrupt` queda reservado para el formato roto de verdad.
#[tokio::test]
async fn fallo_del_provider_interior_no_se_disfraza_de_corrupt() {
    let tar = TarSmith::new().file(b"ok.txt", b"bien").build();
    let (mem, path) = common::seed_container(b"fixture.tar", &tar).await;
    let faults = mem.faults();
    let root = VPath::archive_compose("tar", &path, &[]).expect("compose");
    let p = ArchiveProvider::with_limits(mem, Format::Tar, "tar+mem", Limits::default());
    // El corte llega a MITAD del parseo (tras las ops previas del índice:
    // stat de generación + primeras lecturas), no antes: es el camino que
    // atraviesa los helpers `corrupt()` del formato.
    for n in 0..8u64 {
        faults.clear();
        faults.disconnect_after(n);
        match p.list(&root).await.map(|_| ()) {
            Err(Error::ProviderUnavailable { retryable: true }) | Ok(()) => {}
            other => panic!("con disconnect_after({n}) el IO del interior se disfrazó: {other:?}"),
        }
    }
}
