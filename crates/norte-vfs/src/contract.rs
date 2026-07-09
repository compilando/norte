//! [`provider_contract!`]: la suite contractual que TODO provider debe pasar
//! (spec §5/§12). Un provider nuevo la invoca en sus tests de integración y
//! hereda ~24 casos: roundtrips byte-exactos, colisiones, transaccionalidad
//! del sink, nombres hostiles y coherencia de caja. Los casos que dependen de
//! una capability se auto-saltan si el provider no la declara.

/// Genera la suite contractual de [`Provider`](crate::Provider) dentro de un
/// módulo de test.
///
/// Requisitos del crate invocante (dev-dependencies): `tokio` (features
/// `macros`, `rt`) — el resto llega vía re-exports internos de `norte-vfs`.
///
/// - `mod`: nombre del módulo generado.
/// - `factory`: expresión que construye un provider FRESCO (se evalúa una vez
///   por test; los tests no comparten estado).
/// - `root`: expresión que construye el `VPath` raíz del provider.
/// - `hostile_names`: expresión `Vec<Vec<u8>>` con nombres hostiles a
///   roundtripear (normalmente `norte_testkit::corpus::hostile_names()`
///   mapeado a bytes).
///
/// ```ignore
/// // En tests de integración de un provider (ignore: evita el ciclo
/// // dev-dep testkit→vfs en el doctest; el uso real vive en norte-testkit).
/// norte_vfs::provider_contract! {
///     mod contract_mem,
///     factory: norte_testkit::MemProvider::new(),
///     root: norte_testkit::MemProvider::root(),
///     hostile_names: norte_testkit::corpus::hostile_names()
///         .into_iter()
///         .map(|n| n.bytes)
///         .collect(),
/// }
/// ```
#[macro_export]
macro_rules! provider_contract {
    (
        mod $name:ident,
        factory: $factory:expr,
        root: $root:expr,
        hostile_names: $hostile:expr $(,)?
    ) => {
        mod $name {
            #![allow(clippy::redundant_clone)]

            // Las expresiones `factory`/`root`/`hostile_names` se evalúan en
            // este módulo: importa el scope del invocante.
            #[allow(unused_imports)]
            use super::*;

            use $crate::__private::bytes::Bytes;
            use $crate::__private::futures::StreamExt;
            use $crate::__private::norte_proto::{
                CapabilityFlags, ConflictKind, EntryKind, Error, Segment, VPath,
            };
            use $crate::{ByteSink, Provider};

            fn seg(bytes: &[u8]) -> Segment {
                Segment::new(bytes.to_vec()).expect("segmento válido de contrato")
            }

            fn child(base: &VPath, name: &[u8]) -> VPath {
                base.join(seg(name))
            }

            async fn write_all<P: Provider>(p: &P, path: &VPath, content: &[u8]) {
                let mut sink = p.write(path).await.expect("write abre");
                sink.write(Bytes::copy_from_slice(content))
                    .await
                    .expect("chunk entra");
                sink.commit().await.expect("commit publica");
            }

            async fn read_all<P: Provider>(p: &P, path: &VPath) -> Result<Vec<u8>, Error> {
                let mut stream = p.read(path).await?;
                let mut out = Vec::new();
                while let Some(chunk) = stream.next().await {
                    out.extend_from_slice(&chunk?);
                }
                Ok(out)
            }

            /// Auto-skip declarativo: el caso solo aplica si `flags` está
            /// (o `!flags` no está) en las capabilities del provider.
            macro_rules! require_caps {
                ($p:expr, has: $flag:expr) => {
                    if !$p.capabilities().flags.contains($flag) {
                        eprintln!("skip: el provider no declara {:?}", $flag);
                        return;
                    }
                };
                ($p:expr, lacks: $flag:expr) => {
                    if $p.capabilities().flags.contains($flag) {
                        eprintln!("skip: el caso exige NO tener {:?}", $flag);
                        return;
                    }
                };
            }

            // ---------- stat ----------

            #[tokio::test]
            async fn contract_stat_root_is_dir() {
                let p = $factory;
                let root: VPath = $root;
                let e = p.stat(&root).await.expect("la raíz siempre existe");
                assert_eq!(e.kind, EntryKind::Dir);
            }

            #[tokio::test]
            async fn contract_stat_missing_is_not_found() {
                let p = $factory;
                let root: VPath = $root;
                assert_eq!(
                    p.stat(&child(&root, b"no-existe")).await.unwrap_err(),
                    Error::NotFound
                );
            }

            // ---------- write / read ----------

            #[tokio::test]
            async fn contract_write_read_roundtrip() {
                let p = $factory;
                let root: VPath = $root;
                let f = child(&root, b"f.bin");
                let content: Vec<u8> = (0u16..2048).map(|i| (i % 251) as u8).collect();
                write_all(&p, &f, &content).await;
                assert_eq!(read_all(&p, &f).await.unwrap(), content);
                let e = p.stat(&f).await.unwrap();
                assert_eq!(e.kind, EntryKind::File);
                assert_eq!(e.size, Some(content.len() as u64));
            }

            #[tokio::test]
            async fn contract_write_invisible_before_commit() {
                let p = $factory;
                let root: VPath = $root;
                let f = child(&root, b"pendiente");
                let mut sink = p.write(&f).await.unwrap();
                sink.write(Bytes::from_static(b"data")).await.unwrap();
                assert_eq!(
                    p.stat(&f).await.unwrap_err(),
                    Error::NotFound,
                    "el path final no existe hasta commit"
                );
                sink.commit().await.unwrap();
                assert!(p.stat(&f).await.is_ok());
            }

            #[tokio::test]
            async fn contract_abort_leaves_no_trace() {
                let p = $factory;
                let root: VPath = $root;
                let f = child(&root, b"abortado");
                let mut sink = p.write(&f).await.unwrap();
                sink.write(Bytes::from_static(b"data")).await.unwrap();
                sink.abort().await.expect("abort limpia");
                assert_eq!(p.stat(&f).await.unwrap_err(), Error::NotFound);
                let names: Vec<Vec<u8>> = p
                    .list(&root)
                    .await
                    .expect("list raíz")
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
                assert!(
                    !names.contains(&b"abortado".to_vec()),
                    "ni rastro del staging en el listado"
                );
            }

            #[tokio::test]
            async fn contract_write_collision_is_conflict() {
                let p = $factory;
                let root: VPath = $root;
                let f = child(&root, b"ocupado");
                write_all(&p, &f, b"1").await;
                match p.write(&f).await {
                    Err(Error::Conflict { .. }) => {}
                    Err(e) => panic!("esperaba Conflict, fue {e:?}"),
                    Ok(_) => panic!("esperaba Conflict, el write abrió"),
                }
            }

            #[tokio::test]
            async fn contract_write_missing_parent_is_not_found() {
                let p = $factory;
                let root: VPath = $root;
                let f = child(&child(&root, b"no-dir"), b"f");
                match p.write(&f).await {
                    Err(Error::NotFound) => {}
                    Err(e) => panic!("esperaba NotFound, fue {e:?}"),
                    Ok(_) => panic!("esperaba NotFound, el write abrió"),
                }
            }

            #[tokio::test]
            async fn contract_read_missing_is_not_found() {
                let p = $factory;
                let root: VPath = $root;
                assert_eq!(
                    read_all(&p, &child(&root, b"nada")).await.unwrap_err(),
                    Error::NotFound
                );
            }

            #[tokio::test]
            async fn contract_read_dir_errors() {
                let p = $factory;
                let root: VPath = $root;
                let d = child(&root, b"dir");
                p.mkdir(&d).await.unwrap();
                assert!(read_all(&p, &d).await.is_err(), "leer un dir es error");
            }

            // ---------- mkdir / list ----------

            #[tokio::test]
            async fn contract_mkdir_and_stat() {
                let p = $factory;
                let root: VPath = $root;
                let d = child(&root, b"nuevo");
                p.mkdir(&d).await.unwrap();
                assert_eq!(p.stat(&d).await.unwrap().kind, EntryKind::Dir);
            }

            #[tokio::test]
            async fn contract_mkdir_existing_is_conflict() {
                let p = $factory;
                let root: VPath = $root;
                let d = child(&root, b"dup");
                p.mkdir(&d).await.unwrap();
                match p.mkdir(&d).await {
                    Err(Error::Conflict { .. }) => {}
                    other => panic!("esperaba Conflict, fue {other:?}"),
                }
            }

            #[tokio::test]
            async fn contract_mkdir_missing_parent_is_not_found() {
                let p = $factory;
                let root: VPath = $root;
                let d = child(&child(&root, b"no"), b"sub");
                assert_eq!(p.mkdir(&d).await.unwrap_err(), Error::NotFound);
            }

            #[tokio::test]
            async fn contract_list_sees_children_byte_exact() {
                let p = $factory;
                let root: VPath = $root;
                write_all(&p, &child(&root, b"uno"), b"1").await;
                p.mkdir(&child(&root, b"dos")).await.unwrap();
                let mut names: Vec<Vec<u8>> = p
                    .list(&root)
                    .await
                    .unwrap()
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
                assert_eq!(names, vec![b"dos".to_vec(), b"uno".to_vec()]);
            }

            #[tokio::test]
            async fn contract_list_missing_is_not_found() {
                let p = $factory;
                let root: VPath = $root;
                assert!(p.list(&child(&root, b"nada")).await.is_err());
            }

            // ---------- remove ----------

            #[tokio::test]
            async fn contract_remove_file() {
                let p = $factory;
                let root: VPath = $root;
                let f = child(&root, b"borrable");
                write_all(&p, &f, b"x").await;
                p.remove(&f).await.unwrap();
                assert_eq!(p.stat(&f).await.unwrap_err(), Error::NotFound);
            }

            #[tokio::test]
            async fn contract_remove_missing_is_not_found() {
                let p = $factory;
                let root: VPath = $root;
                assert_eq!(
                    p.remove(&child(&root, b"nada")).await.unwrap_err(),
                    Error::NotFound
                );
            }

            #[tokio::test]
            async fn contract_remove_empty_dir() {
                let p = $factory;
                let root: VPath = $root;
                let d = child(&root, b"vacio");
                p.mkdir(&d).await.unwrap();
                p.remove(&d).await.unwrap();
                assert_eq!(p.stat(&d).await.unwrap_err(), Error::NotFound);
            }

            #[tokio::test]
            async fn contract_remove_nonempty_dir_refused() {
                let p = $factory;
                let root: VPath = $root;
                let d = child(&root, b"lleno");
                p.mkdir(&d).await.unwrap();
                write_all(&p, &child(&d, b"f"), b"x").await;
                assert!(
                    p.remove(&d).await.is_err(),
                    "remove no recursivo: dir con hijos se niega"
                );
                assert!(p.stat(&d).await.is_ok(), "y el dir sigue ahí");
            }

            // ---------- rename ----------

            #[tokio::test]
            async fn contract_rename_file_moves_bytes() {
                let p = $factory;
                let root: VPath = $root;
                let a = child(&root, b"a");
                let b = child(&root, b"b");
                write_all(&p, &a, b"contenido").await;
                p.rename(&a, &b).await.unwrap();
                assert_eq!(p.stat(&a).await.unwrap_err(), Error::NotFound);
                assert_eq!(read_all(&p, &b).await.unwrap(), b"contenido");
            }

            #[tokio::test]
            async fn contract_rename_missing_is_not_found() {
                let p = $factory;
                let root: VPath = $root;
                assert_eq!(
                    p.rename(&child(&root, b"no"), &child(&root, b"da"))
                        .await
                        .unwrap_err(),
                    Error::NotFound
                );
            }

            #[tokio::test]
            async fn contract_rename_collision_is_conflict() {
                let p = $factory;
                let root: VPath = $root;
                let a = child(&root, b"a");
                let b = child(&root, b"b");
                write_all(&p, &a, b"1").await;
                write_all(&p, &b, b"2").await;
                match p.rename(&a, &b).await {
                    Err(Error::Conflict { .. }) => {}
                    other => panic!("esperaba Conflict, fue {other:?}"),
                }
                assert_eq!(read_all(&p, &b).await.unwrap(), b"2", "el destino intacto");
            }

            #[tokio::test]
            async fn contract_rename_dir_moves_subtree() {
                let p = $factory;
                let root: VPath = $root;
                let src = child(&root, b"src");
                let dst = child(&root, b"dst");
                p.mkdir(&src).await.unwrap();
                write_all(&p, &child(&src, b"f"), b"x").await;
                p.rename(&src, &dst).await.unwrap();
                assert_eq!(read_all(&p, &child(&dst, b"f")).await.unwrap(), b"x");
            }

            // ---------- nombres hostiles ----------

            #[tokio::test]
            async fn contract_hostile_names_roundtrip() {
                let p = $factory;
                let root: VPath = $root;
                let names: Vec<Vec<u8>> = $hostile;
                assert!(!names.is_empty(), "el corpus no puede estar vacío");
                let mut created = 0usize;
                for bytes in names {
                    let f = child(&root, &bytes);
                    // El OS puede rechazar el nombre (APFS exige UTF-8, NTFS
                    // prohíbe controles): rechazo limpio = skip. La garantía
                    // contractual es: SI se crea, los bytes vuelven intactos.
                    let mut sink = match p.write(&f).await {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("skip {}: {e}", f.display_lossy());
                            continue;
                        }
                    };
                    sink.write(Bytes::from_static(b"x")).await.expect("chunk");
                    // Algunos providers publican en commit (rename/put): el
                    // rechazo del OS también puede llegar aquí.
                    match sink.commit().await {
                        Ok(()) => {}
                        Err(Error::InvalidPath | Error::Conflict { .. }) => {
                            eprintln!("skip (commit) {}", f.display_lossy());
                            continue;
                        }
                        Err(e) => panic!("commit de {}: {e:?}", f.display_lossy()),
                    }
                    created += 1;
                    let e = p.stat(&f).await.unwrap_or_else(|err| {
                        panic!("stat de {:?} tras commit: {err:?}", f.display_lossy())
                    });
                    assert_eq!(
                        e.path.file_name().expect("con nombre").as_bytes(),
                        bytes.as_slice(),
                        "bytes intactos para {}",
                        f.display_lossy()
                    );
                    // La prueba REAL de roundtrip: los bytes que devuelve el
                    // BACKEND al listar (no el eco del path de entrada).
                    let listed: Vec<Vec<u8>> = p
                        .list(&root)
                        .await
                        .expect("list raíz")
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
                    let exact = listed.iter().filter(|n| n.as_slice() == bytes.as_slice()).count();
                    assert_eq!(
                        exact,
                        1,
                        "el backend debe devolver los bytes EXACTOS una vez para {}",
                        f.display_lossy()
                    );
                    assert_eq!(read_all(&p, &f).await.unwrap(), b"x");
                }
                assert!(created > 0, "ningún nombre hostil se pudo crear: sospechoso");
            }

            #[tokio::test]
            async fn contract_write_never_touches_siblings() {
                let p = $factory;
                let root: VPath = $root;
                // Un archivo REAL del usuario cuyo nombre coincide con un
                // posible esquema de staging: escribir el vecino jamás lo toca.
                let sibling = child(&root, b"x.norte-partial");
                write_all(&p, &sibling, b"contenido real del usuario").await;
                let target = child(&root, b"x");
                write_all(&p, &target, b"nuevo").await;
                assert_eq!(
                    read_all(&p, &sibling).await.unwrap(),
                    b"contenido real del usuario",
                    "el staging pisó un archivo real"
                );
                assert_eq!(read_all(&p, &target).await.unwrap(), b"nuevo");
            }

            // ---------- capabilities condicionales ----------

            #[tokio::test]
            async fn contract_rename_case_variant_when_sensitive_is_conflict() {
                let p = $factory;
                require_caps!(p, has: CapabilityFlags::CASE_SENSITIVE);
                let root: VPath = $root;
                write_all(&p, &child(&root, b"caja"), b"1").await;
                write_all(&p, &child(&root, b"CAJA"), b"2").await;
                // En FS case-sensitive son archivos DISTINTOS: rename entre
                // variantes de caja es Conflict, jamás un reemplazo silencioso.
                match p.rename(&child(&root, b"caja"), &child(&root, b"CAJA")).await {
                    Err(Error::Conflict { .. }) => {}
                    other => panic!("esperaba Conflict, fue {other:?}"),
                }
                assert_eq!(read_all(&p, &child(&root, b"caja")).await.unwrap(), b"1");
                assert_eq!(read_all(&p, &child(&root, b"CAJA")).await.unwrap(), b"2");
            }

            #[tokio::test]
            async fn contract_case_collision_when_insensitive() {
                let p = $factory;
                require_caps!(p, lacks: CapabilityFlags::CASE_SENSITIVE);
                let root: VPath = $root;
                write_all(&p, &child(&root, b"Mismo"), b"1").await;
                match p.write(&child(&root, b"mismo")).await {
                    Err(Error::Conflict {
                        conflict: ConflictKind::CaseCollision,
                    }) => {}
                    Err(e) => panic!("esperaba CaseCollision, fue {e:?}"),
                    Ok(_) => panic!("esperaba CaseCollision, el write abrió"),
                }
            }

            #[tokio::test]
            async fn contract_case_distinct_when_sensitive() {
                let p = $factory;
                require_caps!(p, has: CapabilityFlags::CASE_SENSITIVE);
                let root: VPath = $root;
                write_all(&p, &child(&root, b"Caja"), b"1").await;
                write_all(&p, &child(&root, b"caja"), b"2").await;
                assert_eq!(read_all(&p, &child(&root, b"Caja")).await.unwrap(), b"1");
                assert_eq!(read_all(&p, &child(&root, b"caja")).await.unwrap(), b"2");
            }

            #[tokio::test]
            async fn contract_copy_native_iff_capability() {
                let p = $factory;
                let root: VPath = $root;
                let a = child(&root, b"orig");
                let b = child(&root, b"copia");
                write_all(&p, &a, b"bytes").await;
                let declared = p
                    .capabilities()
                    .flags
                    .contains(CapabilityFlags::SERVER_COPY);
                match p.copy_native(&a, &b).await {
                    None => assert!(!declared, "SERVER_COPY declarado pero copy_native = None"),
                    Some(res) => {
                        assert!(declared, "copy_native sin declarar SERVER_COPY");
                        res.expect("copia nativa ok");
                        assert_eq!(read_all(&p, &b).await.unwrap(), b"bytes");
                    }
                }
            }
        }
    };
}
