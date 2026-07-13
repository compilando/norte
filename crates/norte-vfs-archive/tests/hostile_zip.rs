//! Casos hostiles del provider zip: encoding de nombres (bit 11, cp437),
//! zip-slip, EOCD mentiroso, cifrado/métodos raros, deflate real y rangos.

mod common;

use std::io::Write as _;

use futures::StreamExt;
use norte_proto::{ByteRange, Error, Segment, VPath};
use norte_testkit::ZipSmith;
use norte_vfs::Provider;
use norte_vfs_archive::{ArchiveProvider, Limits};

fn seg(b: &[u8]) -> Segment {
    Segment::new(b.to_vec()).expect("seg")
}

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

/// Un zip deflate REAL (escrito por el crate `zip`): `ZipSmith` solo forja
/// stored; la descompresión de verdad se ejercita con esto.
fn deflate_zip(name: &str, data: &[u8]) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    let mut w = zip::ZipWriter::new(&mut cursor);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    w.start_file(name, opts).expect("start_file");
    w.write_all(data).expect("write_all");
    w.finish().expect("finish");
    cursor.into_inner()
}

#[tokio::test]
async fn cp437_crudo_bit11_off_se_lista_byte_exacto() {
    // CAFÉ.TXT en cp437 (É = 0x90): bytes crudos, jamás decodificados.
    let zip = ZipSmith::new().file(b"CAF\x90.TXT", b"1980s").build();
    let (p, root) = common::zip_provider(&zip).await;
    assert_eq!(list_names(&p, &root).await, vec![b"CAF\x90.TXT".to_vec()]);
    assert_eq!(
        read_all(&p, &root.join(seg(b"CAF\x90.TXT")), None).await,
        b"1980s"
    );
}

#[tokio::test]
async fn bit11_mentiroso_no_decodifica_ni_panica() {
    // Bit 11 dice "UTF-8" pero los bytes NO lo son: el flag es un anuncio,
    // los bytes mandan (regla 1).
    let zip = ZipSmith::new()
        .file_utf8(b"lie-\xff\xfe.txt", b"liar")
        .build();
    let (p, root) = common::zip_provider(&zip).await;
    assert_eq!(
        list_names(&p, &root).await,
        vec![b"lie-\xff\xfe.txt".to_vec()]
    );
    assert_eq!(
        read_all(&p, &root.join(seg(b"lie-\xff\xfe.txt")), None).await,
        b"liar"
    );
}

#[tokio::test]
async fn zip_slip_y_marcador_se_omiten() {
    let zip = ZipSmith::new()
        .file(b"../evil", b"slip")
        .file(b"/etc/passwd", b"abs")
        .file(b"a/../b", b"dotdot")
        .file(b"!", b"marker")
        .file(b"a/!/b", b"marker-dentro")
        .file(b"ok.txt", b"bien")
        .build();
    let (p, root) = common::zip_provider(&zip).await;
    // Solo sobreviven `ok.txt` y el dir implícito `a` (de `a/../b` nada:
    // la entrada entera se omite; `a` existe por `a/!/b`… tampoco: también
    // se omite entera. Verifica exactamente qué queda.)
    assert_eq!(list_names(&p, &root).await, vec![b"ok.txt".to_vec()]);
}

#[tokio::test]
async fn duplicado_y_file_vs_dir() {
    let zip = ZipSmith::new()
        .file(b"x", b"uno")
        .file(b"x", b"dos!!")
        .file(b"d", b"soy-file")
        .dir(b"d")
        .file(b"d/hijo", b"vivo")
        .build();
    let (p, root) = common::zip_provider(&zip).await;
    assert_eq!(read_all(&p, &root.join(seg(b"x")), None).await, b"dos!!");
    assert_eq!(
        read_all(&p, &root.join(seg(b"d")).join(seg(b"hijo")), None).await,
        b"vivo",
        "el dir gana al file homónimo y el subárbol sobrevive"
    );
}

#[tokio::test]
async fn eocd_mentiroso_corta_sin_pagar_el_indice() {
    let zip = ZipSmith::new().file(b"x", b"1").build_lying_eocd(60_000);
    let limits = Limits {
        max_entries: 100,
        ..Limits::default()
    };
    let (p, root) = common::zip_provider_with_limits(&zip, limits).await;
    match p.list(&root).await.map(|_| ()) {
        Err(Error::Io { retryable: false }) => {}
        other => panic!("esperaba Io por EOCD mentiroso, fue {other:?}"),
    }
}

#[tokio::test]
async fn zip_vacio_y_basura() {
    // Un zip sin entradas es VÁLIDO: raíz vacía.
    let (p, root) = common::zip_provider(&ZipSmith::new().build()).await;
    assert!(list_names(&p, &root).await.is_empty());
    // Basura sin EOCD: Io.
    let (p, root) = common::zip_provider(b"no soy un zip").await;
    match p.list(&root).await.map(|_| ()) {
        Err(Error::Io { retryable: false }) => {}
        other => panic!("esperaba Io, fue {other:?}"),
    }
    // Contenedor de 0 bytes: Io.
    let (p, root) = common::zip_provider(b"").await;
    match p.list(&root).await.map(|_| ()) {
        Err(Error::Io { retryable: false }) => {}
        other => panic!("esperaba Io con 0 bytes, fue {other:?}"),
    }
}

#[tokio::test]
async fn cifrado_y_metodo_raro_se_listan_pero_no_se_leen() {
    let zip = ZipSmith::new()
        .file_raw(b"secreto.bin", b"garbage", 0, 1) // bit 0: cifrado
        .file_raw(b"exotico.bin", b"garbage", 12, 0) // método 12 (bzip2, sin feature)
        .file(b"normal.txt", b"ok")
        .build();
    let (p, root) = common::zip_provider(&zip).await;
    assert_eq!(
        list_names(&p, &root).await,
        vec![
            b"exotico.bin".to_vec(),
            b"normal.txt".to_vec(),
            b"secreto.bin".to_vec()
        ],
        "cifradas/exóticas SE LISTAN (metadatos)"
    );
    for name in [b"secreto.bin".as_slice(), b"exotico.bin"] {
        match p.read(&root.join(seg(name)), None).await.map(|_| ()) {
            Err(Error::Unsupported) => {}
            other => panic!("esperaba Unsupported en {name:?}, fue {other:?}"),
        }
    }
    assert_eq!(
        read_all(&p, &root.join(seg(b"normal.txt")), None).await,
        b"ok"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn deflate_real_roundtrip_y_rangos() {
    // Datos compresibles y grandes (varios chunks de 64 KiB del lector).
    let data: Vec<u8> = (0..500_000u32).map(|i| (i % 7) as u8).collect();
    let zip = deflate_zip("grande.bin", &data);
    let (p, root) = common::zip_provider(&zip).await;
    let f = root.join(seg(b"grande.bin"));
    assert_eq!(
        p.stat(&f).await.expect("stat").size,
        Some(data.len() as u64)
    );
    assert_eq!(read_all(&p, &f, None).await, data);
    // Range sobre bytes DESCOMPRIMIDOS.
    assert_eq!(
        read_all(
            &p,
            &f,
            Some(ByteRange {
                offset: 1,
                len: Some(3)
            })
        )
        .await,
        &data[1..4]
    );
    assert_eq!(
        read_all(
            &p,
            &f,
            Some(ByteRange {
                offset: 499_998,
                len: None
            })
        )
        .await,
        &data[499_998..]
    );
    assert_eq!(
        read_all(
            &p,
            &f,
            Some(ByteRange {
                offset: 999_999,
                len: Some(1)
            })
        )
        .await,
        b"",
        "past-EOF descomprimido: stream vacío"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn drop_del_stream_cancela_la_descompresion() {
    let data: Vec<u8> = (0..4_000_000u32).map(|i| (i % 13) as u8).collect();
    let zip = deflate_zip("enorme.bin", &data);
    let (p, root) = common::zip_provider(&zip).await;
    let f = root.join(seg(b"enorme.bin"));
    let mut stream = p.read(&f, None).await.expect("read");
    let first = stream.next().await.expect("hay chunk").expect("ok");
    assert!(!first.is_empty());
    drop(stream); // el hilo blocking muere al siguiente send (canal cerrado)
    // Regla 3: nada cuelga — el test termina; el hilo huérfano está acotado
    // a 4 chunks en vuelo. Releer entera verifica que nada quedó roto.
    assert_eq!(read_all(&p, &f, None).await, data);
}

#[tokio::test]
async fn backslash_final_es_file_legible() {
    // H2 (auditoría 8e): `\` final NO es separador ni marca de dir — el
    // crate zip lo decodifica como dir; norte decide por BYTES.
    let zip = ZipSmith::new().file(b"trailing\\", b"data").build();
    let (p, root) = common::zip_provider(&zip).await;
    let f = root.join(seg(b"trailing\\"));
    let e = p.stat(&f).await.expect("stat");
    assert_eq!(e.kind, norte_proto::EntryKind::File);
    assert_eq!(read_all(&p, &f, None).await, b"data");
}

#[tokio::test]
async fn file_primero_hijo_despues_asciende_a_dir() {
    // H4 (auditoría 8e): `file a` + `a/hijo` — el file asciende a dir y el
    // subárbol queda visible (antes: TypeMismatch y subárbol huérfano).
    let zip = ZipSmith::new()
        .file(b"a", b"soy-file")
        .file(b"a/hijo", b"vivo")
        .build();
    let (p, root) = common::zip_provider(&zip).await;
    let a = root.join(seg(b"a"));
    assert_eq!(
        p.stat(&a).await.expect("stat").kind,
        norte_proto::EntryKind::Dir
    );
    assert_eq!(list_names(&p, &a).await, vec![b"hijo".to_vec()]);
    assert_eq!(read_all(&p, &a.join(seg(b"hijo")), None).await, b"vivo");
}

#[tokio::test]
async fn nombre_con_nul_se_omite() {
    // H7: el único NUL posible en zip viene forjado — se omite limpio.
    let zip = ZipSmith::new()
        .file(b"nul\x00byte.txt", b"x")
        .file(b"ok.txt", b"bien")
        .build();
    let (p, root) = common::zip_provider(&zip).await;
    assert_eq!(list_names(&p, &root).await, vec![b"ok.txt".to_vec()]);
}

#[tokio::test]
async fn firma_eocd_falsa_en_el_comentario_no_rompe() {
    // H9: un comentario que CONTIENE una firma EOCD (con contador enorme)
    // no debe hacer que el preflight rechace un zip válido.
    let mut fake = b"PK\x05\x06".to_vec();
    fake.extend_from_slice(&[0u8; 6]);
    fake.extend_from_slice(&60_000u16.to_le_bytes()); // count_total falso
    fake.extend_from_slice(&60_000u16.to_le_bytes());
    fake.extend_from_slice(&[0u8; 6]);
    let zip = ZipSmith::new()
        .file(b"real.txt", b"ok")
        .comment(&fake)
        .build();
    let limits = Limits {
        max_entries: 100,
        ..Limits::default()
    };
    let (p, root) = common::zip_provider_with_limits(&zip, limits).await;
    assert_eq!(list_names(&p, &root).await, vec![b"real.txt".to_vec()]);
}

#[tokio::test]
async fn pin_h1_colapso_lossy_del_crate_zip() {
    // PIN de bug upstream (H1, issue de deuda 8g): zip 5.x indexa el CD por
    // el nombre DECODIFICADO — dos nombres crudos distintos que decodifican
    // al mismo U+FFFD colapsan en UNA entrada (última gana) ANTES de que
    // norte los vea. Cuando un upgrade del crate lo arregle, este test se
    // pondrá rojo y podremos listar ambas byte-exactas.
    let zip = ZipSmith::new()
        .file_utf8(b"lossy-\xff.txt", b"PRIMERO")
        .file_utf8(b"lossy-\xfe.txt", b"SEGUNDO")
        .build();
    let (p, root) = common::zip_provider(&zip).await;
    let names = list_names(&p, &root).await;
    assert_eq!(names.len(), 1, "el crate colapsó las dos entradas (pin)");
    assert_eq!(
        read_all(&p, &root.join(seg(&names[0])), None).await,
        b"SEGUNDO",
        "última gana dentro del crate (shadowing documentado)"
    );
}
