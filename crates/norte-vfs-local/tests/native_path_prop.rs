//! Property-based tests de la frontera de paths nativos: los bytes de un
//! nombre sobreviven el viaje segmento → OS → segmento, y las conversiones
//! jamás panican con basura.

use futures::StreamExt;
use norte_proto::Segment;
use norte_testkit::strategies::{arb_hostile_filename, arb_segment_bytes};
use norte_vfs_local::LocalProvider;
use proptest::prelude::*;

proptest! {
    /// Roundtrip por el FS REAL: crear un archivo con bytes arbitrarios de
    /// nombre y recuperarlos intactos vía list. En unix todo byte válido de
    /// segmento es válido de nombre; si el OS rechaza (p. ej. APFS con
    /// no-UTF8), rechazo limpio — jamás corrupción.
    #[test]
    fn prop_filename_bytes_survive_fs(bytes in arb_segment_bytes()) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let p = LocalProvider::rooted(dir.path().to_path_buf());
            let root = LocalProvider::root();
            let seg = Segment::new(bytes.clone()).expect("estrategia genera segmentos válidos");
            let path = root.join(seg);
            // Si el OS rechaza el nombre (p. ej. APFS con no-UTF8): aceptable.
            if let Ok(mut sink) = norte_vfs::Provider::write(&p, &path).await {
                sink.write(bytes::Bytes::from_static(b"x"))
                    .await
                    .expect("chunk");
                // Con el staging corto (issue #4) el rechazo del OS al nombre
                // FINAL llega en el rename de commit: rechazo limpio = skip,
                // cualquier otro error es fallo real.
                match sink.commit().await {
                    Ok(()) => {}
                    Err(norte_proto::Error::InvalidPath | norte_proto::Error::Conflict { .. }) => {
                        return Ok(());
                    }
                    Err(e) => panic!("commit: {e:?}"),
                }
                // La prueba real: los bytes que devuelve el FS al LISTAR
                // (stat ecoa el path de entrada; eso no prueba nada).
                let listed: Vec<Vec<u8>> = norte_vfs::Provider::list(&p, &root)
                    .await
                    .expect("list")
                    .map(|e| {
                        e.expect("entrada ok")
                            .path
                            .file_name()
                            .expect("nombre")
                            .as_bytes()
                            .to_vec()
                    })
                    .collect()
                    .await;
                let exact = listed.iter().filter(|n| n.as_slice() == bytes.as_slice()).count();
                prop_assert_eq!(exact, 1, "el FS debe devolver los bytes exactos una vez");
            }
            Ok(())
        })?;
    }

    /// La conversión de nombres hostiles jamás panica, acepte o no el OS.
    #[test]
    fn prop_hostile_names_never_panic(bytes in arb_hostile_filename()) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let p = LocalProvider::rooted(dir.path().to_path_buf());
            let root = LocalProvider::root();
            let seg = Segment::new(bytes).expect("estrategia genera segmentos válidos");
            let _ = norte_vfs::Provider::stat(&p, &root.join(seg)).await;
        });
    }
}
