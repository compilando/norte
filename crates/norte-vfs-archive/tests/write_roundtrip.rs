//! Lo que este crate ESCRIBE (#132), leído por lo que este crate ya sabía
//! leer.
//!
//! El lector de aquí es el oráculo del escritor y no al revés: si el
//! round-trip cierra, el archivo que sale es al menos tan bueno como el que
//! norte acepta de fuera. Lo que el round-trip no puede decir —si un `unzip`
//! ajeno lo abre— lo cubre el bit 11, que se asserta aparte porque nuestro
//! lector se queda los bytes crudos y no lo mira.

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt as _;
use norte_proto::{Segment, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;
use norte_vfs_archive::write::{ArchiveWriter, PackEntry, PackFormat};
use norte_vfs_archive::{ArchiveProvider, Format};

/// Empaqueta `entradas` y devuelve los bytes del archivo.
fn empaqueta(format: PackFormat, nivel: u32, entradas: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    let mut w = ArchiveWriter::new(format, nivel);
    let mut out = Vec::new();
    for (nombre, datos) in entradas {
        let mut e = PackEntry::file(nombre.clone(), datos.len() as u64);
        e.mtime_ms = Some(1_700_000_000_000);
        w.begin(&e).expect("abre");
        // A trozos, que es como llegan de un provider: el escritor tiene que
        // dar lo mismo con un chunk que con veinte.
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

/// Lee un archivo con el provider de este crate: `(nombre, contenido)` de cada
/// fichero del árbol.
async fn lee(format: Format, scheme: &str, bytes: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mem = Arc::new(MemProvider::new());
    let path = MemProvider::root().join(Segment::new(b"c.bin".to_vec()).expect("seg"));
    let mut sink = mem.write(&path).await.expect("write");
    sink.write(Bytes::copy_from_slice(bytes))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
    let token = match format {
        Format::Zip => "zip",
        Format::Tar => "tar",
        Format::TarGz => "tar+gz",
    };
    let root = VPath::archive_compose(token, &path, &[]).expect("compose");
    let p = ArchiveProvider::new(mem, format, scheme);
    let mut out = Vec::new();
    let mut stream = p.list(&root).await.expect("list");
    while let Some(e) = stream.next().await {
        let e = e.expect("entrada");
        if e.kind != norte_proto::EntryKind::File {
            continue;
        }
        let nombre = e
            .path
            .file_name()
            .map(|s| s.as_bytes().to_vec())
            .expect("nombre");
        let mut datos = Vec::new();
        let mut bs = p.read(&e.path, None).await.expect("read");
        while let Some(chunk) = bs.next().await {
            datos.extend_from_slice(&chunk.expect("chunk"));
        }
        out.push((nombre, datos));
    }
    out.sort();
    out
}

/// El caso normal, en los tres formatos y con contenido que comprime y
/// contenido que no.
#[tokio::test]
async fn los_tres_formatos_cierran_el_round_trip() {
    let entradas = vec![
        (b"hola.txt".to_vec(), b"hola que tal".to_vec()),
        (b"repe.txt".to_vec(), b"a".repeat(10_000)),
        (b"vacio.txt".to_vec(), Vec::new()),
    ];
    let mut esperado: Vec<(Vec<u8>, Vec<u8>)> = entradas.clone();
    esperado.sort();

    for (pf, f, scheme, nivel) in [
        (PackFormat::Zip, Format::Zip, "zip+mem", 6),
        (PackFormat::Zip, Format::Zip, "zip+mem", 0),
        (PackFormat::Tar, Format::Tar, "tar+mem", 0),
        (PackFormat::TarGz, Format::TarGz, "tar+gz+mem", 6),
    ] {
        let bytes = empaqueta(pf, nivel, &entradas);
        assert_eq!(
            lee(f, scheme, &bytes).await,
            esperado,
            "{pf:?} nivel {nivel}"
        );
    }
}

/// **La prueba que la regla 1 pide.** Cada nombre hostil del corpus canónico
/// sobrevive al empaquetado byte a byte, en zip y en tar.
///
/// Un nombre se empaqueta de uno en uno: el corpus tiene gemelos que
/// colisionarían entre sí en un mismo archivo, y lo que se prueba aquí es el
/// viaje del nombre, no la política de colisiones.
#[tokio::test]
async fn los_nombres_hostiles_sobreviven_al_empaquetado() {
    for name in norte_testkit::corpus::hostile_names() {
        // Los nombres que el propio direccionamiento rechaza no llegan a ser
        // entradas de un archivo de norte: el índice ya los OMITE al leer
        // (ADR 0018 C2, «omitir con señal»), así que pedirle al escritor que
        // los conserve sería pedir un round-trip que el lector no promete.
        // El marcador `!` es el caso interesante — es un `Segment` válido y
        // aun así no direcciona dentro de un archivo, de ahí que se compruebe
        // aparte. Empaquetar un fichero que se llame así es cosa del OP, que
        // lo rehúsa en vez de escribir una entrada inalcanzable.
        if Segment::new(name.bytes.clone()).is_err() || name.bytes == b"!" {
            continue;
        }
        let entradas = vec![(name.bytes.clone(), b"contenido".to_vec())];
        for (pf, f, scheme) in [
            (PackFormat::Zip, Format::Zip, "zip+mem"),
            (PackFormat::Tar, Format::Tar, "tar+mem"),
        ] {
            let bytes = empaqueta(pf, 6, &entradas);
            let leido = lee(f, scheme, &bytes).await;
            assert_eq!(leido, entradas, "{} no sobrevivió a {pf:?}", name.id);
        }
    }
}

/// El bit 11 dice la verdad, en los dos sentidos.
///
/// Nuestro lector no lo mira —se queda los bytes crudos—, así que el
/// round-trip de arriba pasaría igual con el bit puesto siempre. Pero los
/// demás programas SÍ decodifican por él, y ponerlo sobre un nombre que no es
/// UTF-8 convierte el nombre del usuario en caracteres de reemplazo en
/// cualquier unzip del mundo.
#[test]
fn el_bit_11_solo_se_pone_cuando_el_nombre_es_utf8() {
    /// Flags del header local: bytes 6..8 del fichero.
    fn flags_del_primer_header(bytes: &[u8]) -> u16 {
        u16::from_le_bytes([bytes[6], bytes[7]])
    }
    const BIT_UTF8: u16 = 1 << 11;

    let utf8 = empaqueta(
        PackFormat::Zip,
        6,
        &[(b"caf\xc3\xa9.txt".to_vec(), b"x".to_vec())],
    );
    assert_ne!(
        flags_del_primer_header(&utf8) & BIT_UTF8,
        0,
        "un nombre UTF-8 se anuncia como tal"
    );

    let crudo = empaqueta(
        PackFormat::Zip,
        6,
        &[(b"caf\xe9.txt".to_vec(), b"x".to_vec())],
    );
    assert_eq!(
        flags_del_primer_header(&crudo) & BIT_UTF8,
        0,
        "y uno que no lo es, NO: mentir aquí es perder el nombre en todo lector ajeno"
    );
}

/// Un nombre de más de 100 bytes es corriente y tar lo lleva en 100: sin la
/// extensión GNU se recortaría, y un nombre recortado es un nombre perdido.
#[tokio::test]
async fn un_nombre_largo_sobrevive_al_tar() {
    let largo = format!("{}.txt", "n".repeat(200)).into_bytes();
    let entradas = vec![(largo.clone(), b"dentro".to_vec())];
    let bytes = empaqueta(PackFormat::Tar, 0, &entradas);
    assert_eq!(lee(Format::Tar, "tar+mem", &bytes).await, entradas);
}

/// Muchas entradas: el directorio central y sus offsets tienen que cuadrar
/// más allá del caso de una.
#[tokio::test]
async fn un_zip_de_muchas_entradas_se_lee_entero() {
    let entradas: Vec<(Vec<u8>, Vec<u8>)> = (0..500)
        .map(|i| {
            (
                format!("f{i:04}.txt").into_bytes(),
                format!("contenido {i}").into_bytes(),
            )
        })
        .collect();
    let mut esperado = entradas.clone();
    esperado.sort();
    let bytes = empaqueta(PackFormat::Zip, 6, &entradas);
    assert_eq!(lee(Format::Zip, "zip+mem", &bytes).await, esperado);
}
