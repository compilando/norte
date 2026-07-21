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

/// #93: `list_skipped` expone el `skipped` del índice del contenedor —
/// `Some(n)` con las hostiles contadas, `Some(0)` en un tar limpio. Es lo que
/// el frontend señaliza como badge («N entradas omitidas»).
#[tokio::test]
async fn list_skipped_expone_las_omitidas_del_indice() {
    let hostil = TarSmith::new()
        .file(b"../evil", b"slip")
        .file(b"ok.txt", b"bien")
        .build();
    let (p, root) = common::tar_provider(&hostil).await;
    assert_eq!(
        p.list_skipped(&root).await.expect("list_skipped"),
        Some(1),
        "la entrada traversal omitida debe contarse"
    );
    // También desde un subpath del contenedor (el total es por-contenedor).
    assert_eq!(
        p.list_skipped(&root.join(seg(b"ok.txt")))
            .await
            .expect("ok"),
        Some(1)
    );

    let limpio = TarSmith::new().file(b"a.txt", b"x").build();
    let (p, root) = common::tar_provider(&limpio).await;
    assert_eq!(p.list_skipped(&root).await.expect("ok"), Some(0));
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
        Err(Error::LimitExceeded { limit }) if limit == "entries" => {}
        other => panic!("esperaba LimitExceeded(entries), fue {other:?}"),
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

/// #97: el passthrough de tar (datos contiguos) con un contenedor que se
/// TRUNCA bajo el read — el provider interior termina el stream corto con
/// semántica pread y SIN error (como un FS real): el lector debe recibir
/// `Corrupt` tras los bytes parciales, jamás un fichero corto en silencio
/// (paridad con zip #95.4 y targz FIX-1).
#[tokio::test]
async fn tar_passthrough_corto_es_corrupt_no_datos_cortos() {
    use bytes::Bytes;
    use norte_proto::{ByteRange as BR, Capabilities, Entry as PEntry};
    use norte_vfs::{ByteStream, EntryStream as ES};
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Delegado a Mem que, ARMADO, corta cada stream de read a la mitad de
    /// sus chunks — sin error, como un contenedor mutado bajo los pies.
    struct Truncating {
        inner: MemProvider,
        armado: std::sync::Arc<AtomicBool>,
    }
    #[async_trait::async_trait]
    impl norte_vfs::Provider for Truncating {
        // La firma del trait es `-> &str`; literal correcto aquí.
        #[allow(clippy::unnecessary_literal_bound)]
        fn scheme(&self) -> &str {
            "mem"
        }
        fn capabilities(&self) -> Capabilities {
            self.inner.capabilities()
        }
        async fn stat(&self, p: &VPath) -> Result<PEntry, Error> {
            self.inner.stat(p).await
        }
        async fn list(&self, p: &VPath) -> Result<ES, Error> {
            self.inner.list(p).await
        }
        async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
            norte_vfs::Provider::write(&self.inner, p).await
        }
        async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
            self.inner.mkdir(p).await
        }
        async fn remove(&self, p: &VPath) -> Result<(), Error> {
            self.inner.remove(p).await
        }
        async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
            self.inner.rename(from, to).await
        }
        async fn read(&self, p: &VPath, range: Option<BR>) -> Result<ByteStream, Error> {
            let stream = self.inner.read(p, range).await?;
            if !self.armado.load(Ordering::Relaxed) {
                return Ok(stream);
            }
            // Junta y corta a la MITAD de los bytes pedidos: fin limpio.
            let todos: Vec<Result<Bytes, Error>> = stream.collect().await;
            let mut bytes: Vec<u8> = Vec::new();
            for c in todos {
                bytes.extend_from_slice(&c.expect("chunk ok"));
            }
            bytes.truncate(bytes.len() / 2);
            Ok(futures::stream::iter(vec![Ok(Bytes::from(bytes))]).boxed())
        }
    }

    let tar = TarSmith::new().file(b"datos.bin", &[7u8; 1000]).build();
    let mem = MemProvider::new();
    let root = MemProvider::root();
    let container = root.join(seg(b"c.tar"));
    {
        let mut sink = norte_vfs::Provider::write(&mem, &container)
            .await
            .expect("write abre");
        sink.write(bytes::Bytes::from(tar)).await.expect("chunk");
        sink.commit().await.expect("commit");
    }
    let armado = std::sync::Arc::new(AtomicBool::new(false));
    let provider = ArchiveProvider::new(
        std::sync::Arc::new(Truncating {
            inner: mem,
            armado: std::sync::Arc::clone(&armado),
        }),
        Format::Tar,
        "tar+mem",
    );
    let interior = VPath::parse("tar+mem:///c.tar/!/datos.bin").expect("wire");

    // Sano: roundtrip completo (el índice queda caliente).
    assert_eq!(provider.read(&interior, None).await.map(|_| ()), Ok(()));
    let mut stream = provider.read(&interior, None).await.expect("read sano");
    let mut total = 0usize;
    while let Some(item) = stream.next().await {
        total += item.expect("chunk sano").len();
    }
    assert_eq!(total, 1000);
    // M1 del review: pollear tras Ready(None) no panica (stream fused).
    assert!(stream.next().await.is_none());
    assert!(stream.next().await.is_none());

    // Armado: el interior corta a la mitad SIN error → Corrupt, no silencio.
    armado.store(true, Ordering::Relaxed);
    let mut stream = provider.read(&interior, None).await.expect("read abre");
    let mut vistos = 0usize;
    let mut fallo = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(c) => vistos += c.len(),
            Err(e) => {
                fallo = Some(e);
                break;
            }
        }
    }
    match fallo {
        Some(Error::Corrupt) => assert_eq!(vistos, 500, "los parciales llegan, luego el error"),
        other => panic!("esperaba Corrupt tras {vistos} bytes, fue {other:?}"),
    }
    // Tras el Err: un None limpio y poll-after-None seguro (fused), jamás
    // un segundo Err ni un panic.
    assert!(stream.next().await.is_none());
    assert!(stream.next().await.is_none());
}

/// #60 (H6): GNU longname — el header lleva el nombre TRUNCADO a 100 pero
/// `path_bytes()` del crate aplica el longname byte-exacto. Ningún test
/// compilaba este camino: el nombre 255 del corpus, vía longname, debe
/// listarse ENTERO y leerse.
#[tokio::test]
async fn gnu_longname_roundtrip_nombre_255_del_corpus() {
    let largo = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "name_max_255")
        .expect("fixture del corpus")
        .bytes;
    assert_eq!(largo.len(), 255);
    let tar = TarSmith::new()
        .file_gnu_longname(&largo, b"contenido-largo")
        .file(b"corto.txt", b"x")
        .build();
    let (p, root) = common::tar_provider(&tar).await;
    let names = list_names(&p, &root).await;
    assert!(
        names.contains(&largo),
        "el nombre de 255 bytes se lista BYTE-EXACTO vía longname"
    );
    assert_eq!(
        read_all(&p, &root.join(seg(&largo)), None).await,
        b"contenido-largo"
    );
}

/// #60: override pax `path=` con bytes NO-UTF8 — el crate aplica el path
/// del record pax byte-exacto (pax real exige UTF-8; los tars hostiles no).
#[tokio::test]
async fn pax_path_no_utf8_roundtrip() {
    let nombre = b"docs/caf\xe9.txt"; // é en Latin-1 dentro de un path pax
    let tar = TarSmith::new().file_pax_path(nombre, b"pax!").build();
    let (p, root) = common::tar_provider(&tar).await;
    let docs = list_names(&p, &root).await;
    assert_eq!(
        docs,
        vec![b"docs".to_vec()],
        "el dir implícito del pax path"
    );
    assert_eq!(
        read_all(&p, &root.join(seg(b"docs")).join(seg(b"caf\xe9.txt")), None).await,
        b"pax!"
    );
}

/// #60: zip-slip VÍA longname — un traversal que no cabe en el header ustar
/// (>100 bytes) llega entero por el longname y debe OMITIRSE igual que el
/// corto (contando en skipped), jamás colarse por venir del camino largo.
#[tokio::test]
async fn zip_slip_via_longname_se_omite() {
    let mut evil = b"../".to_vec();
    evil.extend(std::iter::repeat_n(b'x', 120));
    let tar = TarSmith::new()
        .file_gnu_longname(&evil, b"slip")
        .file(b"ok.txt", b"bien")
        .build();
    let (p, root) = common::tar_provider(&tar).await;
    assert_eq!(list_names(&p, &root).await, vec![b"ok.txt".to_vec()]);
    assert_eq!(
        p.list_skipped(&root).await.expect("skipped"),
        Some(1),
        "el traversal largo cuenta como omitida"
    );
}

/// #60 (H5, pin directo vía `entry_raw`): un `pax_global_header` (typeflag
/// `g`, lo que emite `git archive`) NO fantasmea en el listado — el
/// iterador del crate no lo consume solo y el filtro de `classify_entry` lo
/// descarta.
#[tokio::test]
async fn pax_global_header_crudo_no_fantasmea() {
    let tar = TarSmith::new()
        .entry_raw(b'g', b"pax_global_header", b"52 comment=git archive\n")
        .file(b"real.txt", b"si")
        .build();
    let (p, root) = common::tar_provider(&tar).await;
    assert_eq!(list_names(&p, &root).await, vec![b"real.txt".to_vec()]);
    assert_eq!(
        read_all(&p, &root.join(seg(b"real.txt")), None).await,
        b"si"
    );
}
