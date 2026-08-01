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

/// Un zip deflate REAL: los bytes comprimidos los produce flate2 y la
/// estructura la forja `ZipSmith` (#59: sin el crate `zip` ni en tests).
fn deflate_zip(name: &[u8], data: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(data).expect("deflate");
    let deflated = enc.finish().expect("finish");
    ZipSmith::new().file_deflate(name, data, &deflated).build()
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
    // #95.3: el corte por anuncio del EOCD es LimitExceeded, no Corrupt — en
    // este punto no se sabe si el EOCD miente o el zip es legítimo y enorme
    // (y precisamente se rehúsa a pagar el índice para averiguarlo).
    match p.list(&root).await.map(|_| ()) {
        Err(Error::LimitExceeded { limit }) if limit == "entries" => {}
        other => {
            panic!("esperaba LimitExceeded(entries) por EOCD anunciando de más, fue {other:?}")
        }
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
        Err(Error::Corrupt) => {}
        other => panic!("esperaba Corrupt, fue {other:?}"),
    }
    // Contenedor de 0 bytes: Io.
    let (p, root) = common::zip_provider(b"").await;
    match p.list(&root).await.map(|_| ()) {
        Err(Error::Corrupt) => {}
        other => panic!("esperaba Corrupt con 0 bytes, fue {other:?}"),
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
    let zip = deflate_zip(b"grande.bin", &data);
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
    let zip = deflate_zip(b"enorme.bin", &data);
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
async fn h1_nombres_que_colapsan_en_lossy_ya_no_colapsan() {
    // H1 (#59): el crate `zip` indexaba el CD por el nombre DECODIFICADO —
    // dos nombres crudos distintos que decodifican al mismo U+FFFD
    // colapsaban en UNA entrada (última gana) antes de que norte los viera.
    // Con el parser propio del CD ambos nombres viven byte-exactos.
    let zip = ZipSmith::new()
        .file_utf8(b"lossy-\xff.txt", b"PRIMERO")
        .file_utf8(b"lossy-\xfe.txt", b"SEGUNDO")
        .build();
    let (p, root) = common::zip_provider(&zip).await;
    assert_eq!(
        list_names(&p, &root).await,
        vec![b"lossy-\xfe.txt".to_vec(), b"lossy-\xff.txt".to_vec()],
        "las dos entradas listan byte-exactas, sin colapso lossy"
    );
    assert_eq!(
        p.list_skipped(&root).await.expect("skipped"),
        Some(0),
        "nada se omitió ni se perdió"
    );
    assert_eq!(
        read_all(&p, &root.join(seg(b"lossy-\xff.txt")), None).await,
        b"PRIMERO"
    );
    assert_eq!(
        read_all(&p, &root.join(seg(b"lossy-\xfe.txt")), None).await,
        b"SEGUNDO"
    );
}

/// Extra field 0x7075 (Info-ZIP unicode path) VÁLIDO: versión 1 + crc32 del
/// nombre del header + nombre unicode alternativo.
fn extra_7075(unicode: &[u8], header_name: &[u8]) -> Vec<u8> {
    let mut crc = flate2::Crc::new();
    crc.update(header_name);
    let mut body = vec![1u8]; // versión
    body.extend_from_slice(&crc.sum().to_le_bytes());
    body.extend_from_slice(unicode);
    let mut out = 0x7075u16.to_le_bytes().to_vec();
    out.extend_from_slice(
        &u16::try_from(body.len())
            .expect("extra corto")
            .to_le_bytes(),
    );
    out.extend_from_slice(&body);
    out
}

#[tokio::test]
async fn extra_7075_valido_jamas_sustituye_el_nombre() {
    // H3 (#59): el crate `zip` SUSTITUÍA el nombre del CD por el del extra
    // 0x7075 cuando su crc validaba. El parser propio lo ignora por diseño:
    // los bytes del CD mandan (regla 1).
    let extra = extra_7075(b"impostor.txt", b"nombre-cd.txt");
    let zip = ZipSmith::new()
        .file_with_extra(b"nombre-cd.txt", b"contenido", &extra)
        .build();
    let (p, root) = common::zip_provider(&zip).await;
    assert_eq!(
        list_names(&p, &root).await,
        vec![b"nombre-cd.txt".to_vec()],
        "el nombre CRUDO del CD manda; el 0x7075 se ignora"
    );
    assert_eq!(
        read_all(&p, &root.join(seg(b"nombre-cd.txt")), None).await,
        b"contenido"
    );
}

#[tokio::test]
async fn extra_7075_invalido_no_mata_el_archivo() {
    // H3 (#59): un 0x7075 malformado (size que desborda el blob) abortaba
    // el archive ENTERO en el crate `zip`. Ahora: record truncado = se deja
    // de caminar el blob y la entrada sobrevive con su nombre del CD.
    let mut extra = 0x7075u16.to_le_bytes().to_vec();
    extra.extend_from_slice(&200u16.to_le_bytes()); // promete 200, hay 3
    extra.extend_from_slice(&[1, 2, 3]);
    let zip = ZipSmith::new()
        .file_with_extra(b"superviviente.txt", b"vivo", &extra)
        .file(b"vecina.txt", b"tambien")
        .build();
    let (p, root) = common::zip_provider(&zip).await;
    assert_eq!(
        list_names(&p, &root).await,
        vec![b"superviviente.txt".to_vec(), b"vecina.txt".to_vec()]
    );
    assert_eq!(
        read_all(&p, &root.join(seg(b"superviviente.txt")), None).await,
        b"vivo"
    );
}

#[tokio::test]
async fn zip64_eocd_cuenta_y_lee() {
    // zip64 (#59): EOCD con marcadores → locator → EOCD64. La entrada lista
    // y lee byte-exacta.
    let zip = ZipSmith::new()
        .file(b"z64.txt", b"contenido-64")
        .build_zip64();
    let (p, root) = common::zip_provider(&zip).await;
    assert_eq!(list_names(&p, &root).await, vec![b"z64.txt".to_vec()]);
    assert_eq!(
        read_all(&p, &root.join(seg(b"z64.txt")), None).await,
        b"contenido-64"
    );

    // Y una cuenta zip64 mentirosa por encima de max_entries corta ANTES de
    // pagar el CD (el hueco del preflight u16 queda cerrado, #59).
    let liar = ZipSmith::new()
        .file(b"z64.txt", b"x")
        .build_zip64_lying_count(1_000_000);
    let limits = Limits {
        max_entries: 100,
        ..Limits::default()
    };
    let (p, root) = common::zip_provider_with_limits(&liar, limits).await;
    match p.list(&root).await.map(|_| ()) {
        Err(Error::LimitExceeded { limit }) if limit == "entries" => {}
        other => panic!("esperaba LimitExceeded(entries) por EOCD64 mentiroso, fue {other:?}"),
    }
}

#[tokio::test]
async fn crc_mentiroso_en_lectura_completa_es_corrupt() {
    // #59: la lectura COMPLETA (el camino de copia) verifica el CRC del CD
    // sobre los bytes servidos — un CD que miente termina en Err(Corrupt)
    // como ÚLTIMO item del stream, jamás datos corruptos en silencio.
    let mut bytes = ZipSmith::new().file(b"mentira.bin", b"contenido").build();
    let cd = bytes
        .windows(4)
        .position(|w| w == [0x50, 0x4b, 0x01, 0x02])
        .expect("firma del CD");
    bytes[cd + 16..cd + 20].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes()); // crc falso
    let (p, root) = common::zip_provider(&bytes).await;
    let f = root.join(seg(b"mentira.bin"));
    let (vistos, fallo) = read_hasta_fallo(&p, &f).await;
    match fallo {
        Some(Error::Corrupt) => {}
        other => panic!("esperaba Corrupt por CRC mentiroso (vistos {vistos}), fue {other:?}"),
    }
    // Un range PARCIAL de la misma entrada no puede verificarse sin
    // descomprimir la entrada entera: sirve los bytes sin CRC (documentado).
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
        b"ont"
    );
}

/// Drena un read esperando que el stream FALLE; devuelve (`bytes_ok`, error).
async fn read_hasta_fallo(p: &ArchiveProvider, f: &VPath) -> (usize, Option<Error>) {
    let mut stream = p.read(f, None).await.expect("read abre");
    let mut vistos = 0usize;
    while let Some(item) = stream.next().await {
        match item {
            Ok(chunk) => vistos += chunk.len(),
            Err(e) => return (vistos, Some(e)),
        }
    }
    (vistos, None)
}

/// Robustez: una entrada cuyo CD promete más bytes de los que el CONTENEDOR
/// tiene falla `Corrupt` (aquí ya lo cazaba el CRC del crate `zip` al agotar
/// el reader — el pin del camino silencioso es el test de abajo).
#[tokio::test]
async fn zip_entrada_que_promete_mas_bytes_que_el_contenedor_es_corrupt() {
    let mut bytes = ZipSmith::new().file(b"corta.bin", &[9u8; 100]).build();
    // Cirugía sobre el CD (única entrada): comp/uncomp pasan a 1 MiB — muy
    // por encima del final del contenedor. El local header queda como está
    // (el crate `zip` lee con los tamaños del CD).
    let cd = bytes
        .windows(4)
        .position(|w| w == [0x50, 0x4b, 0x01, 0x02])
        .expect("firma del CD");
    let lie = (1u32 << 20).to_le_bytes();
    bytes[cd + 20..cd + 24].copy_from_slice(&lie); // compressed size
    bytes[cd + 24..cd + 28].copy_from_slice(&lie); // uncompressed size
    let (p, root) = common::zip_provider(&bytes).await;
    let (vistos, fallo) = read_hasta_fallo(&p, &root.join(seg(b"corta.bin"))).await;
    match fallo {
        Some(Error::Corrupt) => {}
        other => panic!("esperaba Corrupt (vistos {vistos} bytes), fue {other:?}"),
    }
}

/// #95.4 — EL camino silencioso (paridad con el FIX-1 de targz): deflate que
/// termina LIMPIO (bloque final válido) antes del `uncompressed_size` que el
/// CD promete, con CRC consistente con los datos CORTOS. El crate `zip` no
/// tiene nada que objetar (deflate válido, CRC ok) → el take-loop ve `Ok(0)`
/// con `remaining > 0`. Antes del fix devolvía un fichero parcial SIN RUIDO;
/// ahora es `Corrupt`.
#[tokio::test]
async fn zip_deflate_corto_con_crc_consistente_es_corrupt_no_datos_cortos() {
    // Deflate raw VÁLIDO de solo 2 bytes.
    let cortos = b"AB";
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(cortos).expect("deflate");
    let deflated = enc.finish().expect("finish");
    // ZipSmith escribe comp=uncomp=len(deflated) y crc de los bytes crudos:
    // cirugía en LOCAL y CD — uncomp miente 100, crc = crc32(descomprimido).
    let mut crc = flate2::Crc::new();
    crc.update(cortos);
    let crc_ok = crc.sum().to_le_bytes();
    let mut bytes = ZipSmith::new()
        .file_raw(b"corta.bin", &deflated, 8, 0)
        .build();
    let lie = 100u32.to_le_bytes();
    let local = bytes
        .windows(4)
        .position(|w| w == [0x50, 0x4b, 0x03, 0x04])
        .expect("firma local");
    bytes[local + 14..local + 18].copy_from_slice(&crc_ok);
    bytes[local + 22..local + 26].copy_from_slice(&lie); // uncomp (local)
    let cd = bytes
        .windows(4)
        .position(|w| w == [0x50, 0x4b, 0x01, 0x02])
        .expect("firma del CD");
    bytes[cd + 16..cd + 20].copy_from_slice(&crc_ok);
    bytes[cd + 24..cd + 28].copy_from_slice(&lie); // uncomp (CD)
    let (p, root) = common::zip_provider(&bytes).await;
    let (vistos, fallo) = read_hasta_fallo(&p, &root.join(seg(b"corta.bin"))).await;
    match fallo {
        Some(Error::Corrupt) => {}
        other => {
            panic!("esperaba Corrupt, fue {other:?} con {vistos} bytes — datos cortos en silencio")
        }
    }
}

/// #95.3 (MAJOR-1 del review): el presupuesto de omitidas del zip es un
/// límite LOCAL — muchas entradas hostiles con `max_entries` apretado deben
/// fallar `LimitExceeded("entries")`, no `Corrupt` (paridad con tar/targz).
#[tokio::test]
async fn zip_presupuesto_de_omitidas_es_limit_exceeded() {
    let mut smith = ZipSmith::new();
    for i in 0..5u32 {
        // Traversal: cada una se OMITE (cuenta en skipped, no en el índice).
        smith = smith.file(format!("../evil{i}").as_bytes(), b"x");
    }
    let limits = Limits {
        max_entries: 2,
        ..Limits::default()
    };
    let (p, root) = common::zip_provider_with_limits(&smith.build(), limits).await;
    match p.list(&root).await.map(|_| ()) {
        Err(Error::LimitExceeded { limit }) if limit == "entries" => {}
        other => panic!("esperaba LimitExceeded(entries) por omitidas, fue {other:?}"),
    }
}

/// enc MAJOR-1 (review #59, bug CONFIRMADO pre-fix): una entrada STORED cuyo
/// CD miente `uncomp > comp` habría servido bytes VECINOS del contenedor en
/// un ranged read (el local header de al lado salía como contenido, en
/// silencio). APPNOTE exige comp == uncomp para stored: fail-loud `Corrupt`
/// en CUALQUIER lectura, jamás datos ajenos atribuidos a la entrada.
#[tokio::test]
async fn stored_con_uncomp_mentiroso_es_corrupt_jamas_bytes_vecinos() {
    let mut zip = ZipSmith::new().file(b"peq.txt", b"hola").build();
    // Cirugía: infla el uncomp_size del CD (offset +24 del record 0x02014b50)
    // de 4 a 40. El comp_size (+20) queda en 4: la mentira exacta del bug.
    let cd = zip
        .windows(4)
        .position(|w| w == [0x50, 0x4b, 0x01, 0x02])
        .expect("CD record");
    assert_eq!(
        u32::from_le_bytes(zip[cd + 24..cd + 28].try_into().expect("u32")),
        4,
        "uncomp original"
    );
    zip[cd + 24..cd + 28].copy_from_slice(&40u32.to_le_bytes());

    let (provider, root) = common::zip_provider(&zip).await;
    let path = root.join(Segment::new(b"peq.txt".to_vec()).expect("seg"));
    // Ranged read MÁS ALLÁ de los datos reales: antes devolvía bytes del
    // local header vecino con err=None; ahora Corrupt.
    let mut stream = provider
        .read(
            &path,
            Some(ByteRange {
                offset: 10,
                len: Some(8),
            }),
        )
        .await
        .expect("read abre (el plan se valida en el hilo)");
    let mut err = None;
    let mut got = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(b) => got.extend_from_slice(&b),
            Err(e) => {
                err = Some(e);
                break;
            }
        }
    }
    assert!(matches!(err, Some(Error::Corrupt)), "fue {err:?}");
    assert!(got.is_empty(), "ni un byte vecino en silencio: {got:x?}");
}

/// rust MAJOR-1 (review #59, regla 3): dropear el stream durante la fase de
/// DESCARTE de un deflate ranged (que no envía nada al canal) corta el hilo
/// blocking — sin el chequeo de canal cerrado, seguiría descomprimiendo el
/// skip entero para nadie.
#[tokio::test]
async fn drop_del_stream_durante_el_descarte_corta_el_hilo() {
    // 4 MiB INCOMPRESIBLES (xorshift determinista): comp ≈ uncomp, así el
    // descarte de ~4 MiB descomprimidos exige leer ~16 bloques de 256 KiB
    // del contenedor — señal medible en Faults::read_calls.
    let mut data = vec![0u8; 4 * 1024 * 1024];
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    for b in &mut data {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        #[allow(clippy::cast_possible_truncation)]
        {
            *b = x as u8;
        }
    }
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    enc.write_all(&data).expect("deflate");
    let deflated = enc.finish().expect("finish");
    let zip = ZipSmith::new()
        .file_deflate(b"big.bin", &data, &deflated)
        .build();

    let (mem, path) = common::seed_container(b"fixture.zip", &zip).await;
    let root = VPath::archive_compose("zip", &path, &[]).expect("compose");
    let provider = ArchiveProvider::with_limits(
        std::sync::Arc::clone(&mem) as std::sync::Arc<dyn Provider>,
        norte_vfs_archive::Format::Zip,
        "zip+mem",
        Limits::default(),
    );
    let entry = root.join(Segment::new(b"big.bin".to_vec()).expect("seg"));
    // Warm-up del índice (sus lecturas no cuentan para la aserción).
    let _ = provider.stat(&entry).await.expect("stat");
    let base = mem.faults().read_calls();

    // Ranged read con skip PROFUNDO… y drop inmediato del stream.
    let stream = provider
        .read(
            &entry,
            Some(ByteRange {
                offset: 3_900_000,
                len: Some(16),
            }),
        )
        .await
        .expect("read abre");
    drop(stream);

    // Espera a que el contador se ESTABILICE (el hilo blocking muere solo).
    let mut last = mem.faults().read_calls();
    for _ in 0..200 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let now = mem.faults().read_calls();
        if now == last {
            break;
        }
        last = now;
    }
    let spent = last.saturating_sub(base);
    // Con el fix: data_offset (1 bloque) + a lo sumo un par de iteraciones
    // del descarte antes de ver el canal cerrado. Sin el fix: ~16 bloques
    // del contenedor (el descarte entero).
    assert!(
        spent <= 6,
        "el descarte siguió tras el drop: {spent} lecturas"
    );
}

/// Pin de la decisión #59 (rust MAJOR-3 / enc MINOR-4): un zip con datos
/// PREPENDADOS (self-extractor) se rechaza `Corrupt` — la aceptación exige
/// la auto-consistencia exacta del EOCD (`cd_off + cd_size == pos`), que es
/// precisamente lo que hace sólido el rechazo de firmas falsas en el
/// comentario (H9). Decisión consciente, no accidente: el "offset fudge" de
/// Info-ZIP queda como feature futura si aparece demanda real.
#[tokio::test]
async fn zip_con_datos_prependados_se_rechaza_documentado() {
    let zip = ZipSmith::new().file(b"a.txt", b"x").build();
    let mut sfx = b"#!/bin/sh\necho stub\n".to_vec();
    sfx.extend_from_slice(&zip);
    let (provider, root) = common::zip_provider(&sfx).await;
    // El fallo puede salir del list directo o del stream: acepta ambos.
    if let Err(e) = provider.list(&root).await {
        assert!(matches!(e, Error::Corrupt), "fue {e:?}");
        return;
    }
    let mut stream = provider.list(&root).await.expect("list");
    let mut got_err = None;
    while let Some(item) = stream.next().await {
        if let Err(e) = item {
            got_err = Some(e);
            break;
        }
    }
    assert!(matches!(got_err, Some(Error::Corrupt)), "fue {got_err:?}");
}

// ---------- attrs zip (#108 bloque 2) ----------

#[tokio::test]
async fn attrs_zip_method_packed_crc() {
    use norte_proto::AttrValue;
    use norte_vfs::{AttrRequest, ListOptions};

    // Una entrada normal (store) y una CIFRADA (bit 0: sin locator).
    let zip = ZipSmith::new()
        .file(b"normal.txt", b"contenido")
        .file_raw(b"cifrada.txt", b"xxxx", 8, 1)
        .build();
    let (p, root) = common::zip_provider(&zip).await;
    let opt = ListOptions {
        attrs: AttrRequest::sanitized(
            ["archive.method", "archive.packed_size", "archive.crc32"].map(str::to_owned),
        ),
    };
    let e = p
        .stat_with(&root.join(seg(b"normal.txt")), &opt)
        .await
        .expect("stat_with");
    assert_eq!(
        e.attrs.get("archive.method"),
        Some(&AttrValue::Text("store".to_owned()))
    );
    assert_eq!(
        e.attrs.get("archive.packed_size"),
        Some(&AttrValue::Uint(b"contenido".len() as u64))
    );
    assert!(matches!(
        e.attrs.get("archive.crc32"),
        Some(AttrValue::Uint(_))
    ));

    // La CIFRADA (no legible, locator None) SÍ conserva sus attrs: method
    // es precisamente más interesante ahí.
    let enc = p
        .stat_with(&root.join(seg(b"cifrada.txt")), &opt)
        .await
        .expect("stat_with de cifrada");
    assert_eq!(
        enc.attrs.get("archive.method"),
        Some(&AttrValue::Text("deflate".to_owned()))
    );

    // list_with lleva lo mismo por entrada; sin pedir → nada.
    let mut s = p.list_with(&root, &opt).await.expect("list_with");
    while let Some(e) = s.next().await {
        assert!(e.expect("entrada").attrs.contains_key("archive.method"));
    }
    assert!(
        p.stat(&root.join(seg(b"normal.txt")))
            .await
            .expect("stat")
            .attrs
            .is_empty()
    );
}
