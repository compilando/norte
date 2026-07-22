//! Spool de tar.gz calientes (#95.1): a partir de la segunda lectura de un
//! mismo contenedor, el provider descomprime el stream entero UNA vez a un
//! tempfile anónimo y las siguientes lecturas son seeks locales — cero
//! lecturas del contenedor (verificado con el contador de Faults del
//! `MemProvider`). Generación, presupuesto 0 y sobre-presupuesto degradan al
//! forward-decode de siempre, byte-exacto.

mod common;

use std::sync::Arc;

use futures::StreamExt;
use norte_proto::{ByteRange, Segment, VPath};
use norte_testkit::{MemProvider, TarSmith};
use norte_vfs::Provider;
use norte_vfs_archive::{ArchiveProvider, Format, Limits};

fn seg(b: &[u8]) -> Segment {
    Segment::new(b.to_vec()).expect("seg")
}

/// Ruido xorshift32: incompresible de verdad — el gz resultante es
/// ~proporcional al descomprimido, así que una lectura forward-decode paga
/// varios bloques de 256 KiB del `ProviderReader` (medibles en Faults).
fn noise(len: usize, seed: u32) -> Vec<u8> {
    let mut state = seed;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state & 0xFF) as u8
        })
        .collect()
}

async fn read_all(p: &ArchiveProvider, f: &VPath, range: Option<ByteRange>) -> Vec<u8> {
    let mut stream = p.read(f, range).await.expect("read");
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk ok"));
    }
    out
}

/// Contenedor con DOS entradas de ruido (a.bin cruza varios bloques de
/// 256 KiB) + provider tar.gz con `limits`, conservando el Mem (Faults) y
/// el path del contenedor (para reescribirlo).
async fn setup(
    data_a: &[u8],
    data_b: &[u8],
    limits: Limits,
) -> (Arc<MemProvider>, ArchiveProvider, VPath, VPath) {
    let tar = TarSmith::new()
        .file(b"a.bin", data_a)
        .file(b"b.bin", data_b)
        .build();
    let gz = common::gzip(&tar);
    let (mem, container) = common::seed_container(b"fixture.tar.gz", &gz).await;
    let root = VPath::archive_compose("tar+gz", &container, &[]).expect("compose");
    let provider = ArchiveProvider::with_limits(
        Arc::clone(&mem) as Arc<dyn Provider>,
        Format::TarGz,
        "tar+gz+mem",
        limits,
    );
    (mem, provider, root, container)
}

/// El corazón de #95.1: dos lecturas completas calientan el contenedor y
/// construyen el spool; la TERCERA no paga NI UNA lectura del contenedor
/// (delta de `read_calls` ≤ 1) y sigue siendo byte-exacta.
#[tokio::test(flavor = "multi_thread")]
async fn segunda_lectura_caliente_usa_el_spool() {
    let data_a = noise(600_000, 0x2545_F491);
    let data_b = noise(50_000, 0x9E37_79B9);
    let (mem, p, root, _) = setup(&data_a, &data_b, Limits::default()).await;
    let f = root.join(seg(b"a.bin"));

    let primera = read_all(&p, &f, None).await;
    assert_eq!(primera, data_a, "primera lectura byte-exacta");
    // Segunda lectura: cruza el umbral de calor — construye el spool y
    // sirve desde él DENTRO de la misma lectura.
    assert_eq!(read_all(&p, &f, None).await, primera, "segunda == primera");

    let antes = mem.faults().read_calls();
    let tercera = read_all(&p, &f, None).await;
    let delta = mem.faults().read_calls() - antes;
    assert_eq!(tercera, primera, "tercera lectura byte-exacta");
    assert!(
        delta <= 1,
        "la tercera lectura debe salir del spool, no del contenedor \
         (delta de read_calls = {delta})"
    );
}

/// Mutar el contenedor invalida el spool: la lectura posterior sirve el
/// contenido NUEVO (jamás el spool rancio) y el calor arranca de cero.
#[tokio::test(flavor = "multi_thread")]
async fn spool_respeta_generation() {
    let data_a = noise(400_000, 0xDEAD_BEE5);
    let data_b = noise(30_000, 0x0BAD_F00D);
    let (mem, p, root, container) = setup(&data_a, &data_b, Limits::default()).await;
    let f = root.join(seg(b"a.bin"));

    // Calienta hasta construir el spool de la generación vieja.
    assert_eq!(read_all(&p, &f, None).await, data_a);
    assert_eq!(read_all(&p, &f, None).await, data_a);

    // Reescribe el contenedor: mismo nombre de entrada, contenido y TAMAÑO
    // distintos (la generación cambia seguro aunque el mtime sea grueso).
    let data_a2 = noise(500_000, 0x1234_5678);
    let tar2 = TarSmith::new()
        .file(b"a.bin", &data_a2)
        .file(b"b.bin", &data_b)
        .build();
    common::write_file(mem.as_ref(), &container, &common::gzip(&tar2)).await;

    assert_eq!(
        read_all(&p, &f, None).await,
        data_a2,
        "tras mutar el contenedor se sirve el contenido NUEVO, no el spool rancio"
    );
    // Y el spool de la generación nueva vuelve a funcionar al recalentar.
    assert_eq!(read_all(&p, &f, None).await, data_a2);
    let antes = mem.faults().read_calls();
    assert_eq!(read_all(&p, &f, None).await, data_a2);
    assert!(
        mem.faults().read_calls() - antes <= 1,
        "la generación nueva se spoolea igual que la vieja"
    );
}

/// `spool_max_bytes = 0` desactiva el spool: la tercera lectura sigue
/// pagando el contenedor (forward-decode) — y sigue siendo correcta.
#[tokio::test(flavor = "multi_thread")]
async fn presupuesto_cero_desactiva_el_spool() {
    let data_a = noise(600_000, 0xACED_C0DE);
    let data_b = noise(20_000, 0xFEED_FACE);
    let limits = Limits {
        spool_max_bytes: 0,
        ..Limits::default()
    };
    let (mem, p, root, _) = setup(&data_a, &data_b, limits).await;
    let f = root.join(seg(b"a.bin"));

    assert_eq!(read_all(&p, &f, None).await, data_a);
    assert_eq!(read_all(&p, &f, None).await, data_a);
    let antes = mem.faults().read_calls();
    assert_eq!(read_all(&p, &f, None).await, data_a);
    let delta = mem.faults().read_calls() - antes;
    assert!(
        delta > 1,
        "con presupuesto 0 la tercera lectura sigue leyendo el contenedor \
         (delta de read_calls = {delta})"
    );
}

/// Descomprimido > `spool_max_bytes`: el build aborta (negative-cache), no
/// hay spool, y TODAS las lecturas siguen correctas por forward-decode.
#[tokio::test(flavor = "multi_thread")]
async fn sobre_presupuesto_no_spoolea() {
    let data_a = noise(300_000, 0x5EED_5EED);
    let data_b = noise(10_000, 0xB16B_00B5);
    let limits = Limits {
        spool_max_bytes: 1024, // el descomprimido del contenedor lo supera
        ..Limits::default()
    };
    let (mem, p, root, _) = setup(&data_a, &data_b, limits).await;
    let f = root.join(seg(b"a.bin"));

    assert_eq!(read_all(&p, &f, None).await, data_a);
    // La segunda dispara el build, que aborta por presupuesto y degrada a
    // forward-decode — byte-exacta igualmente.
    assert_eq!(read_all(&p, &f, None).await, data_a);
    let antes = mem.faults().read_calls();
    assert_eq!(read_all(&p, &f, None).await, data_a);
    let delta = mem.faults().read_calls() - antes;
    assert!(
        delta > 1,
        "sin spool (no-spooleable), la tercera lectura paga el contenedor \
         (delta de read_calls = {delta})"
    );
}

/// Lecturas RANGED servidas del spool: byte-exactas con offset en medio de
/// la entrada, cruzando bloques, de cola, sobre la OTRA entrada del mismo
/// contenedor, y past-EOF — todo sin tocar el contenedor.
#[tokio::test(flavor = "multi_thread")]
async fn rangos_desde_el_spool_son_byte_exactos() {
    let data_a = noise(600_000, 0xCAFE_BABE);
    let data_b = noise(50_000, 0x8BAD_BEEF);
    let (mem, p, root, _) = setup(&data_a, &data_b, Limits::default()).await;
    let a = root.join(seg(b"a.bin"));
    let b = root.join(seg(b"b.bin"));

    // Calienta el contenedor (el calor es POR CONTENEDOR): el spool cubre
    // ambas entradas.
    assert_eq!(read_all(&p, &a, None).await, data_a);
    assert_eq!(read_all(&p, &a, None).await, data_a);

    let antes = mem.faults().read_calls();
    let mid = read_all(
        &p,
        &a,
        Some(ByteRange {
            offset: 300_123,
            len: Some(10_000),
        }),
    )
    .await;
    assert_eq!(mid, &data_a[300_123..310_123], "rango en medio de a.bin");
    let cola = read_all(
        &p,
        &a,
        Some(ByteRange {
            offset: 599_995,
            len: None,
        }),
    )
    .await;
    assert_eq!(cola, &data_a[599_995..], "rango de cola sin len");
    let otra = read_all(
        &p,
        &b,
        Some(ByteRange {
            offset: 1_000,
            len: Some(500),
        }),
    )
    .await;
    assert_eq!(
        otra,
        &data_b[1_000..1_500],
        "la OTRA entrada del contenedor también sale del spool"
    );
    let past = read_all(
        &p,
        &a,
        Some(ByteRange {
            offset: 999_999_999,
            len: Some(1),
        }),
    )
    .await;
    assert_eq!(past, b"", "past-EOF de la ENTRADA: stream vacío");
    let delta = mem.faults().read_calls() - antes;
    assert!(
        delta <= 1,
        "todos los rangos salen del spool, no del contenedor \
         (delta de read_calls = {delta})"
    );
}
