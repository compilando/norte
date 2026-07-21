//! #61.1: N operaciones concurrentes sobre el mismo contenedor frío
//! construyen UN índice, no N (single-flight).

mod common;

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use norte_proto::VPath;
use norte_testkit::{MemProvider, ZipSmith};
use norte_vfs::Provider;
use norte_vfs_archive::{ArchiveProvider, Format, Limits};

/// Como `common::zip_provider_with_limits`, pero devolviendo también el
/// `Arc<MemProvider>` interior para poder leer sus `Faults` (el helper del
/// harness lo esconde detrás de `Arc<dyn Provider>`).
async fn zip_provider_with_mem(bytes: &[u8]) -> (ArchiveProvider, VPath, Arc<MemProvider>) {
    let (mem, path) = common::seed_container(b"fixture.zip", bytes).await;
    let root = VPath::archive_compose("zip", &path, &[]).expect("compose");
    let provider = ArchiveProvider::with_limits(
        Arc::clone(&mem) as Arc<dyn Provider>,
        Format::Zip,
        "zip+mem",
        Limits::default(),
    );
    (provider, root, mem)
}

#[tokio::test(flavor = "multi_thread")]
async fn indexado_concurrente_coalesce_en_un_build() {
    let bytes = ZipSmith::new().file(b"a.txt", b"hola").build();

    // Baseline: un build frío en solitario.
    let (provider, root, mem) = zip_provider_with_mem(&bytes).await;
    mem.faults()
        .set_latency_per_op(Some(Duration::from_millis(5)));
    let _: Vec<_> = provider
        .list(&root)
        .await
        .expect("list fría")
        .collect()
        .await;
    let baseline = mem.faults().read_calls();
    assert!(baseline > 0);

    // Provider FRESCO (caché vacía), mismos bytes: 8 lists concurrentes.
    // La latencia por op garantiza el solapamiento (los 8 llegan al miss
    // antes de que el primero termine).
    let (provider2, root2, mem2) = zip_provider_with_mem(&bytes).await;
    mem2.faults()
        .set_latency_per_op(Some(Duration::from_millis(5)));
    let p = Arc::new(provider2);
    let mut handles = Vec::new();
    for _ in 0..8 {
        let p = Arc::clone(&p);
        let root2 = root2.clone();
        handles.push(tokio::spawn(async move {
            let _: Vec<_> = p.list(&root2).await.expect("list").collect().await;
        }));
    }
    for h in handles {
        h.await.expect("join");
    }
    assert_eq!(
        mem2.faults().read_calls(),
        baseline,
        "8 lists concurrentes frías = los reads de UN solo build (single-flight)"
    );
}

/// #61.3: el `read` caliente reutiliza el `ZipArchive` ya parseado por
/// `index_for` — no vuelve a materializar el central directory.
#[tokio::test(flavor = "multi_thread")]
async fn read_caliente_no_reparsea_el_central_directory() {
    // a.txt vive en el bloque 0 del ProviderReader (BLOCK=256 KiB); el
    // relleno de 300 KB empuja el central directory a la cola (bloque >=1).
    // Sin cache, ZipArchive::new relee la cola en CADA read -> delta >= 2.
    let relleno: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let bytes = norte_testkit::ZipSmith::new()
        .file(b"a.txt", b"hola")
        .file(b"relleno.bin", &relleno)
        .build();
    assert!(
        bytes.len() > 262_144,
        "el central directory debe caer fuera del bloque 0"
    );

    let (mem, path) = common::seed_container(b"fixture.zip", &bytes).await;
    let root = VPath::archive_compose("zip", &path, &[]).expect("compose");
    let provider = ArchiveProvider::with_limits(
        Arc::clone(&mem) as Arc<dyn Provider>,
        Format::Zip,
        "zip+mem",
        Limits::default(),
    );
    let entry_path = root.join(norte_proto::Segment::new(b"a.txt".to_vec()).expect("seg"));

    // Calienta el índice (y con él, el CD cacheado).
    provider.stat(&entry_path).await.expect("stat");
    let antes = mem.faults().read_calls();
    let chunks: Vec<_> = provider
        .read(&entry_path, None)
        .await
        .expect("read")
        .collect()
        .await;
    assert!(chunks.iter().all(Result::is_ok));
    let delta = mem.faults().read_calls() - antes;
    assert_eq!(delta, 1, "read caliente = solo el bloque de datos, sin CD");
}

/// #61 MAJOR-2: un central directory por encima de `max_cd_bytes` NO se
/// cachea — cada `read` reabre el `ZipArchive` (comportamiento pre-caché),
/// aunque el ÍNDICE (que respeta sus propios límites, no ligados a bytes de
/// CD) se siga sirviendo de caché normalmente.
#[tokio::test(flavor = "multi_thread")]
async fn cd_sobre_el_tope_no_se_cachea_y_relee_en_cada_read() {
    // Mismo fixture que el test de arriba: el CD cae fuera del bloque 0.
    let relleno: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let bytes = norte_testkit::ZipSmith::new()
        .file(b"a.txt", b"hola")
        .file(b"relleno.bin", &relleno)
        .build();
    assert!(bytes.len() > 262_144);

    let (mem, path) = common::seed_container(b"fixture.zip", &bytes).await;
    let root = VPath::archive_compose("zip", &path, &[]).expect("compose");
    // Tope minúsculo a propósito: el CD real de este fixture (2 entradas,
    // ~46 bytes fijos + nombre cada una) ronda ~110 bytes — muy por encima
    // de 80, así que jamás se cachea (MAJOR-2).
    let limits = Limits {
        max_cd_bytes: 80,
        ..Limits::default()
    };
    let provider = ArchiveProvider::with_limits(
        Arc::clone(&mem) as Arc<dyn Provider>,
        Format::Zip,
        "zip+mem",
        limits,
    );
    let entry_path = root.join(norte_proto::Segment::new(b"a.txt".to_vec()).expect("seg"));

    // Calienta el ÍNDICE (que sí cachea): el `ZipArchive` no, por el tope.
    provider.stat(&entry_path).await.expect("stat");

    for intento in 0..2 {
        let antes = mem.faults().read_calls();
        let mut stream = provider.read(&entry_path, None).await.expect("read");
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk.expect("chunk ok"));
        }
        assert_eq!(out, b"hola", "contenido correcto pese a no cachear el CD");
        let delta = mem.faults().read_calls() - antes;
        assert!(
            delta >= 2,
            "intento {intento}: CD sobre el tope debe reabrir el ZipArchive \
             en cada read (delta={delta}, no cacheado)"
        );
    }
}
