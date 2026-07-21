//! Casos hostiles/roundtrip del provider tar.gz (#55, ADR 0028): la capa gz
//! añade dos riesgos que tar plano no tiene — no es seekable (forward-decode
//! O(offset) por read) y es forward-only en el índice (miembros gzip
//! concatenados, truncamiento, gzip bombs). El corpus hostil de NOMBRES
//! (traversal, absolutos, marcador `!`…) ya lo cubre `readonly_provider_contract!`
//! en `contract.rs` reutilizando el mismo árbol canónico gzipeado.

mod common;

use std::io::Write as _;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use norte_proto::{ByteRange, Error, Segment, VPath};
use norte_testkit::TarSmith;
use norte_vfs::Provider;
use norte_vfs_archive::{ArchiveProvider, Format, Limits};

fn seg(b: &[u8]) -> Segment {
    Segment::new(b.to_vec()).expect("seg")
}

async fn read_all(p: &ArchiveProvider, f: &VPath, range: Option<ByteRange>) -> Vec<u8> {
    let mut stream = p.read(f, range).await.expect("read");
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk ok"));
    }
    out
}

/// Round-trip completo + range en medio, con contenido que CRUZA el bloque
/// de 256 KiB del `ProviderReader` (regla del descarte forward-decode: el
/// offset se sirve descartando bytes descomprimidos, no con `Seek`).
#[tokio::test(flavor = "multi_thread")]
async fn roundtrip_grande_y_range_en_medio() {
    let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let tar = TarSmith::new()
        .file(b"primero.bin", b"cabecera")
        .file(b"grande.bin", &data)
        .build();
    let gz = common::gzip(&tar);
    let (p, root) = common::targz_provider(&gz).await;
    let f = root.join(seg(b"grande.bin"));

    assert_eq!(
        p.stat(&f).await.expect("stat").size,
        Some(data.len() as u64)
    );
    assert_eq!(
        read_all(&p, &f, None).await,
        data,
        "lectura completa byte-exacta"
    );

    // Range con offset EN MEDIO del archivo (fuerza el descarte forward).
    assert_eq!(
        read_all(
            &p,
            &f,
            Some(ByteRange {
                offset: 262_100,
                len: Some(100)
            })
        )
        .await,
        &data[262_100..262_200],
        "range en medio, cruzando el bloque de 256 KiB del ProviderReader"
    );
    assert_eq!(
        read_all(
            &p,
            &f,
            Some(ByteRange {
                offset: 299_995,
                len: None
            })
        )
        .await,
        &data[299_995..],
        "range de cola sin len"
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
        "past-EOF de la ENTRADA: stream vacío"
    );
}

/// Miembros gzip CONCATENADOS (tgz reales los tienen — p. ej. `git archive
/// | gzip` puede producir varios, y herramientas que anexan datos también):
/// `MultiGzDecoder` los decodifica como un stream continuo y el tar completo
/// se indexa igual que con un único miembro.
#[tokio::test]
async fn multi_member_gzip_se_indexa_completo() {
    let tar = TarSmith::new()
        .file(b"a.txt", b"primero")
        .file(b"b.txt", b"segundo")
        .build();
    // Parte el tar PLANO a mitad de bytes (no de entrada) y gzipea cada
    // mitad por separado: dos miembros gzip concatenados que, decodificados
    // en serie, reproducen el tar original byte a byte.
    let mid = tar.len() / 2;
    let mut multi = common::gzip(&tar[..mid]);
    multi.extend_from_slice(&common::gzip(&tar[mid..]));

    let (p, root) = common::targz_provider(&multi).await;
    let mut names: Vec<Vec<u8>> = p
        .list(&root)
        .await
        .expect("list")
        .map(|e| {
            e.expect("ok")
                .path
                .file_name()
                .expect("nombre")
                .as_bytes()
                .to_vec()
        })
        .collect()
        .await;
    names.sort();
    assert_eq!(names, vec![b"a.txt".to_vec(), b"b.txt".to_vec()]);
    assert_eq!(
        read_all(&p, &root.join(seg(b"b.txt")), None).await,
        b"segundo"
    );
}

/// Contenedor tar.gz cortado a mitad: PINEA el comportamiento — el índice
/// falla `Corrupt` (jamás datos cortos en silencio). Cortar a mitad de los
/// BYTES COMPRIMIDOS deja al decoder gzip con un miembro incompleto (CRC/
/// stream truncado) o, si el corte cae dentro del tar ya descomprimido, al
/// iterador de `tar` con una entrada sin datos suficientes para saltar.
/// Ambos casos son errores de IO genuinos del decoder que `corrupt()`
/// traduce a `Corrupt`.
#[tokio::test]
async fn tar_gz_truncado_es_corrupt() {
    let tar = TarSmith::new().file(b"grande.bin", &[7u8; 4000]).build();
    let gz = common::gzip(&tar);
    let cortado = &gz[..gz.len() / 2];
    let (p, root) = common::targz_provider(cortado).await;
    match p.list(&root).await.map(|_| ()) {
        Err(Error::Corrupt) => {}
        other => panic!("esperaba Corrupt con tar.gz truncado, fue {other:?}"),
    }
}

/// Bomba clásica: un archivo grande de ceros comprime a casi nada. Con
/// `max_decompressed_bytes` pequeño, el índice corta ANTES de pagar la
/// descompresión completa — nunca cuelga.
#[tokio::test]
async fn bomba_de_descompresion_corta_por_limite() {
    let tar = TarSmith::new()
        .file(b"bomba.bin", &vec![0u8; 4_000_000])
        .build();
    let gz = common::gzip(&tar);
    let limits = Limits {
        max_decompressed_bytes: 4096,
        ..Limits::default()
    };
    let (p, root) = common::targz_provider_with_limits(&gz, limits).await;
    match p.list(&root).await.map(|_| ()) {
        Err(Error::LimitExceeded { limit }) if limit == "decompressed-bytes" => {}
        other => panic!("esperaba LimitExceeded(decompressed-bytes), fue {other:?}"),
    }
}

/// Drop del stream de lectura a mitad de la descompresión: el hilo
/// `spawn_blocking` termina en el siguiente chunk (canal cerrado, regla 3) —
/// nada cuelga y una relectura posterior sigue siendo correcta.
#[tokio::test(flavor = "multi_thread")]
async fn drop_del_stream_cancela_el_forward_decode() {
    let data: Vec<u8> = (0..4_000_000u32).map(|i| (i % 13) as u8).collect();
    let tar = TarSmith::new().file(b"enorme.bin", &data).build();
    let gz = common::gzip(&tar);
    let (p, root) = common::targz_provider(&gz).await;
    let f = root.join(seg(b"enorme.bin"));

    let mut stream = p.read(&f, None).await.expect("read");
    let first = stream.next().await.expect("hay chunk").expect("ok");
    assert!(!first.is_empty());
    drop(stream); // el hilo blocking muere al siguiente send (canal cerrado)

    assert_eq!(
        read_all(&p, &f, None).await,
        data,
        "releer entera sigue íntegra"
    );
}

/// Basura tras el gzip (trailing garbage, NO un miembro gzip válido): PINEA
/// el comportamiento de `MultiGzDecoder` — se detiene limpio en el último
/// miembro válido (la basura se ignora), no propaga error. Documentado: si
/// un upgrade del crate cambia esto, el test se pone rojo con aviso.
#[tokio::test]
async fn basura_tras_el_gzip_se_ignora() {
    let tar = TarSmith::new().file(b"x.txt", b"contenido").build();
    let mut gz = common::gzip(&tar);
    gz.extend_from_slice(b"esto no es un miembro gzip valido, es basura");
    let (p, root) = common::targz_provider(&gz).await;
    assert_eq!(
        read_all(&p, &root.join(seg(b"x.txt")), None).await,
        b"contenido",
        "el contenido válido se lee igual; la basura tras el último miembro se ignora"
    );
}

/// #58 (mismo criterio que tar/zip) + FIX-3 (rust MINOR-2, #55 review): un
/// fallo GENUINO del provider INTERIOR (desconexión a mitad del índice) se
/// propaga VERBATIM aunque haya atravesado flate2 + tar-rs (que pueden
/// reenvolver el `io::Error` original) — jamás se disfraza de `Corrupt`.
#[tokio::test]
async fn fallo_del_provider_interior_no_se_disfraza_de_corrupt() {
    let tar = TarSmith::new().file(b"ok.txt", b"bien").build();
    let gz = common::gzip(&tar);
    let (mem, path) = common::seed_container(b"fixture.tar.gz", &gz).await;
    let faults = mem.faults();
    let root = VPath::archive_compose("tar+gz", &path, &[]).expect("compose");
    let p = ArchiveProvider::with_limits(mem, Format::TarGz, "tar+gz+mem", Limits::default());
    // El corte llega a distintos puntos del parseo (índice + descompresión);
    // en NINGUNO debe verse disfrazado de "tar.gz corrupto".
    for n in 0..8u64 {
        faults.clear();
        faults.disconnect_after(n);
        match p.list(&root).await.map(|_| ()) {
            Err(Error::ProviderUnavailable { retryable: true }) | Ok(()) => {}
            other => panic!("con disconnect_after({n}) el IO del interior se disfrazó: {other:?}"),
        }
    }
}

/// FIX-2 (security MAJOR, #55 review): el semáforo de concurrencia del
/// forward-decode ENCOLA las lecturas excedentes, nunca las rechaza. Fuego
/// `GZ_READ_CONCURRENCY` (4) + 2 lecturas concurrentes de la MISMA entrada con
/// latencia inyectada en el provider interior (simula el hilo pinneado real
/// sin necesitar un contenedor gigante) y verifica que TODAS completan con
/// el contenido correcto.
#[tokio::test(flavor = "multi_thread")]
async fn concurrencia_de_lecturas_gz_se_encola_no_se_rechaza() {
    let tar = TarSmith::new().file(b"a.txt", b"contenido corto").build();
    let gz = common::gzip(&tar);
    let (mem, path) = common::seed_container(b"fixture.tar.gz", &gz).await;
    mem.faults()
        .set_latency_per_op(Some(Duration::from_millis(15)));
    let root = VPath::archive_compose("tar+gz", &path, &[]).expect("compose");
    let p = Arc::new(ArchiveProvider::with_limits(
        mem,
        Format::TarGz,
        "tar+gz+mem",
        Limits::default(),
    ));
    let f = root.join(seg(b"a.txt"));

    let mut handles = Vec::new();
    for _ in 0..6u32 {
        let p = Arc::clone(&p);
        let f = f.clone();
        handles.push(tokio::spawn(async move { read_all(&p, &f, None).await }));
    }
    for h in handles {
        assert_eq!(
            h.await.expect("join"),
            b"contenido corto",
            "toda lectura por encima del tope de concurrencia debe ENCOLARSE y completar, no fallar"
        );
    }
}

/// Content-Encoding real de flate2 vía `write::GzEncoder` en varios
/// `write_all` (no solo un buffer contiguo) — smoke test de que el helper
/// `common::gzip` no depende de escribir todo de una vez.
#[tokio::test]
async fn gzip_por_partes_produce_el_mismo_resultado() {
    let tar = TarSmith::new().file(b"p.txt", b"partes").build();
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    enc.write_all(&tar[..tar.len() / 2]).expect("parte 1");
    enc.write_all(&tar[tar.len() / 2..]).expect("parte 2");
    let gz = enc.finish().expect("finish");
    let (p, root) = common::targz_provider(&gz).await;
    assert_eq!(
        read_all(&p, &root.join(seg(b"p.txt")), None).await,
        b"partes"
    );
}

/// #60: GNU longname TAMBIÉN por el camino tar.gz (`entries()` secuencial,
/// sin Seek) — `classify_entry` es compartido pero el iterador no: el nombre
/// de 255 bytes del corpus se lista byte-exacto y se lee.
#[tokio::test]
async fn gnu_longname_roundtrip_por_targz() {
    // MEDIUM del audit: la variante MULTIBYTE (85×あ) — el truncado a 100
    // parte una secuencia UTF-8 (100 = 33×3+1): si el crate usara el nombre
    // del header en vez del longname, el listado saldría con un nombre
    // inválido-UTF8 distinto y el read fallaría — canario incorporado.
    let largo = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "name_max_255_multibyte")
        .expect("fixture del corpus")
        .bytes;
    let tar = TarSmith::new()
        .file_gnu_longname(&largo, b"gz-largo")
        .build();
    let gz = common::gzip(&tar);
    let (p, root) = common::targz_provider(&gz).await;
    let seg = norte_proto::Segment::new(largo).expect("seg");
    let f = root.join(seg);
    let mut stream = p.read(&f, None).await.expect("read");
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk ok"));
    }
    assert_eq!(out, b"gz-largo");
}
