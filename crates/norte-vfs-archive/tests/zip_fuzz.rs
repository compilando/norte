//! Fuzz corto (proptest) del provider zip (spec §12, threat model §14): un
//! contenedor de bytes ARBITRARIOS jamás hace `panic` —Ok o `Error` tipado— y
//! un nombre de entrada arbitrario sobrevive byte-a-byte (regla 1). Corre en el
//! gate de PR; el nightly amplía casos.

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use norte_proto::{Segment, VPath};
use norte_testkit::{MemProvider, ZipSmith};
use norte_vfs::Provider;
use norte_vfs_archive::{ArchiveProvider, Format};
use proptest::prelude::*;

/// Un provider zip sobre `bytes` sembrados en un Mem, con su raíz interior.
async fn zip_provider(bytes: &[u8]) -> (ArchiveProvider, VPath) {
    let mem = Arc::new(MemProvider::new());
    let path = MemProvider::root().join(Segment::new(b"f.zip".to_vec()).expect("seg"));
    let mut sink = mem.write(&path).await.expect("write");
    sink.write(Bytes::copy_from_slice(bytes))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
    let root = VPath::archive_compose("zip", &path, &[]).expect("compose");
    (ArchiveProvider::new(mem, Format::Zip, "zip+mem"), root)
}

/// Recorre TODO el árbol interior (list recursivo), forzando la decodificación
/// de cada nombre. Devuelve los nombres finales o el primer error.
async fn walk(p: &ArchiveProvider, dir: &VPath) -> Result<Vec<Vec<u8>>, norte_proto::Error> {
    let mut out = Vec::new();
    let mut stream = p.list(dir).await?;
    while let Some(e) = stream.next().await {
        let e = e?;
        if let Some(name) = e.path.file_name() {
            out.push(name.as_bytes().to_vec());
        }
        if e.kind == norte_proto::EntryKind::Dir {
            out.extend(Box::pin(walk(p, &e.path)).await?);
        }
    }
    Ok(out)
}

proptest! {
    /// Bytes arbitrarios como "zip": construir + listar NUNCA hace panic. El
    /// resultado es Ok (zip válido por casualidad) o un Error tipado, jamás un
    /// crash (anti-bomba/anti-malformado de la spec).
    #[test]
    fn arbitrary_bytes_never_panic(data in proptest::collection::vec(any::<u8>(), 0..8192)) {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(async {
            let (p, root) = zip_provider(&data).await;
            // list de la raíz + walk: Ok o Err, nunca panic.
            let _ = walk(&p, &root).await;
        });
    }
}

proptest! {
    // Menos casos: cada uno construye un zip real + runtime.
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Un nombre de entrada arbitrario (sin `/` ni `\0`, no vacío, no `.`/`..`,
    /// no el marcador reservado `!`) vuelve del listado BYTE A BYTE — el bit
    /// 11/cp437 es metadato, el nombre crudo manda (regla 1, ADR 0018).
    ///
    /// El segmento `!` queda FUERA a sabiendas: ADR 0018 lo reserva como marcador
    /// del scheme compuesto, así que `archive_compose` lo rechaza
    /// (`ArchiveAddressing`) y el índice lo omite como indireccionable
    /// (`index.rs`, "componente `!` (marcador ADR 0018)"). Un `!` a solas no es
    /// direccionable en una ruta compuesta —limitación documentada, no pérdida
    /// silenciosa (se registra y se cuenta como `skipped`)—, y el generador debe
    /// respetar el mismo invariante que ya respeta para `.`/`..`.
    #[test]
    fn arbitrary_name_roundtrips_byte_exact(
        raw in proptest::collection::vec(any::<u8>(), 1..40)
            .prop_filter("nombre legal de segmento (no marcador `!`)", |b| {
                !b.contains(&b'/')
                    && !b.contains(&0)
                    && b.as_slice() != b"."
                    && b.as_slice() != b".."
                    && b.as_slice() != b"!"
            }),
    ) {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(async {
            let zip = ZipSmith::new().file(&raw, b"contenido").build();
            let (p, root) = zip_provider(&zip).await;
            let names = walk(&p, &root).await.expect("zip válido lista");
            prop_assert!(
                names.iter().any(|n| n == &raw),
                "nombre {raw:?} no volvió byte-exacto; llegó {names:?}"
            );
            Ok(())
        })?;
    }
}
