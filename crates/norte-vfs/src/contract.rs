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
                read_all_range(p, path, None).await
            }

            async fn read_all_range<P: Provider>(
                p: &P,
                path: &VPath,
                range: Option<$crate::__private::norte_proto::ByteRange>,
            ) -> Result<Vec<u8>, Error> {
                let mut stream = p.read(path, range).await?;
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

            // ---------- resume (ADR 0012) ----------

            /// `open_resumable` sobre un destino SIN parcial empieza de cero
            /// (`already == 0`) y publica normal. Contrato universal (el
            /// default del trait lo cumple).
            #[tokio::test]
            async fn contract_open_resumable_fresh_starts_at_zero() {
                let p = $factory;
                let root: VPath = $root;
                let f = child(&root, b"resumable-nuevo");
                let (mut sink, already) = p.open_resumable(&f).await.expect("open_resumable");
                assert_eq!(already, 0, "sin parcial previo, empieza de cero");
                sink.write(Bytes::from_static(b"entero")).await.expect("write");
                sink.commit().await.expect("commit");
                assert_eq!(read_all(&p, &f).await.expect("leer"), b"entero");
            }

            /// Honestidad de capabilities (fase 10a): `SERVER_COPY` ⟺
            /// `copy_native` maneja el fichero (`Some`); SIN la cap, `None` —
            /// así el engine cae a streaming en vez de fallar en duro donde
            /// el streaming habría copiado. "Sin sorpresas" = caps que no
            /// mienten (criterio de salida M2).
            #[tokio::test]
            async fn contract_capabilities_server_copy_is_honest() {
                let p = $factory;
                let root: VPath = $root;
                let flags = p.capabilities().flags;
                // READ_ONLY va por readonly_provider_contract! (no siembra).
                if flags.contains($crate::__private::norte_proto::CapabilityFlags::READ_ONLY) {
                    return;
                }
                let from = child(&root, b"cap-src.txt");
                write_all(&p, &from, b"honesto").await;
                let to = child(&root, b"cap-dst.txt");
                let native = p.copy_native(&from, &to).await;
                if flags.contains($crate::__private::norte_proto::CapabilityFlags::SERVER_COPY) {
                    assert!(
                        native.is_some(),
                        "declara SERVER_COPY pero copy_native devolvió None"
                    );
                } else {
                    assert!(
                        native.is_none(),
                        "NO declara SERVER_COPY pero copy_native devolvió Some"
                    );
                }
            }

            /// `keep` + `open_resumable` REANUDA: los bytes conservados se
            /// reportan en `already` y el sink añade tras ellos. Un provider
            /// sin reanudación (default `keep=abort`) se auto-salta: su
            /// segundo `open_resumable` da `already==0` y este test lo
            /// detecta y no exige lo imposible.
            #[tokio::test]
            async fn contract_keep_then_resume_continues() {
                let p = $factory;
                let root: VPath = $root;
                let f = child(&root, b"resumable-cont");
                // Primer tramo: escribe "hola" y CONSERVA (no publica).
                let (mut sink, already) = p.open_resumable(&f).await.expect("open 1");
                assert_eq!(already, 0);
                sink.write(Bytes::from_static(b"hola")).await.expect("write 1");
                sink.keep().await.expect("keep");
                // El destino final NO existe todavía (keep no publica).
                assert_eq!(p.stat(&f).await.unwrap_err(), Error::NotFound);

                // Segundo tramo: reanuda.
                let (mut sink, already) = p.open_resumable(&f).await.expect("open 2");
                if already == 0 {
                    // Provider sin reanudación (keep=abort): recopia entero.
                    eprintln!("skip: el provider no reanuda (keep degrada a abort)");
                    sink.write(Bytes::from_static(b"holamundo")).await.expect("w");
                    sink.commit().await.expect("commit");
                    assert_eq!(read_all(&p, &f).await.unwrap(), b"holamundo");
                    return;
                }
                assert_eq!(already, 4, "reanuda tras los 4 bytes conservados");
                sink.write(Bytes::from_static(b"mundo")).await.expect("write 2");
                sink.commit().await.expect("commit");
                assert_eq!(
                    read_all(&p, &f).await.expect("leer"),
                    b"holamundo",
                    "el contenido es la concatenación de los dos tramos"
                );
            }

            /// `partial_digest` del staging conservado == SHA-256 de esos
            /// bytes (#35). Permisivo: un provider sin digest (default `None`)
            /// se auto-salta — degrada a Length, correcto. Verifica el
            /// invariante staging↔digest en TODA la matriz, no solo en Mem.
            #[tokio::test]
            async fn contract_partial_digest_matches_staged_bytes() {
                use sha2::{Digest, Sha256};
                let p = $factory;
                let root: VPath = $root;
                let f = child(&root, b"digest-check");
                let (mut sink, _) = p.open_resumable(&f).await.expect("open");
                sink.write(Bytes::from_static(b"digest me")).await.expect("write");
                sink.keep().await.expect("keep");

                match p.partial_digest(&f, 9).await.expect("partial_digest") {
                    None => {
                        // Provider sin digest del staging (o sin reanudación):
                        // degrada a Length, aceptable.
                        eprintln!("skip: el provider no expone partial_digest");
                    }
                    Some(d) => {
                        let expected: [u8; 32] = Sha256::digest(b"digest me").into();
                        assert_eq!(d, expected, "el digest cubre los bytes del staging");
                        // Un prefijo más corto hashea SOLO ese prefijo.
                        if let Some(d3) = p.partial_digest(&f, 3).await.expect("digest 3") {
                            let e3: [u8; 32] = Sha256::digest(b"dig").into();
                            assert_eq!(d3, e3, "el digest respeta `len`");
                        }
                    }
                }
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
            async fn contract_read_range_slices_exactly() {
                use $crate::__private::norte_proto::ByteRange;
                let p = $factory;
                let root: VPath = $root;
                let f = child(&root, b"rango.bin");
                write_all(&p, &f, b"0123456789").await;

                let mid = ByteRange { offset: 2, len: Some(3) };
                assert_eq!(
                    read_all_range(&p, &f, Some(mid)).await.expect("rango medio"),
                    b"234"
                );
                let cola = ByteRange { offset: 8, len: None };
                assert_eq!(
                    read_all_range(&p, &f, Some(cola)).await.expect("hasta EOF"),
                    b"89"
                );
                let pasado = ByteRange { offset: 100, len: Some(4) };
                assert_eq!(
                    read_all_range(&p, &f, Some(pasado)).await.expect("pread past-EOF"),
                    b"",
                    "offset más allá de EOF = stream vacío, no error"
                );
                let sobra = ByteRange { offset: 7, len: Some(100) };
                assert_eq!(
                    read_all_range(&p, &f, Some(sobra)).await.expect("len recortado a EOF"),
                    b"789"
                );
            }

            #[tokio::test]
            async fn contract_symlink_roundtrip_bytes() {
                use $crate::SymlinkKind;
                let p = $factory;
                require_caps!(p, has: CapabilityFlags::SYMLINKS);
                let root: VPath = $root;
                let link = child(&root, b"enlace");
                // Target relativo con bytes arbitrarios: JAMAS se interpreta.
                let target: &[u8] = b"destino-que-no-existe";
                p.symlink(&link, target, SymlinkKind::File)
                    .await
                    .expect("symlink crea");
                assert_eq!(
                    p.read_link(&link).await.expect("read_link"),
                    target,
                    "bytes del target intactos"
                );
                let e = p.stat(&link).await.expect("stat del link");
                assert_eq!(e.kind, EntryKind::Symlink, "describe el LINK");
            }

            /// Targets hostiles: la garantía central es que los BYTES del
            /// target viajan intactos, jamás interpretados ni decodificados.
            /// Viven inline (no en names.json): un target NO es un Segment —
            /// admite `/`, `\`, `..`, absolutos y no-UTF8.
            #[tokio::test]
            async fn contract_symlink_hostile_targets_roundtrip() {
                use $crate::SymlinkKind;
                let p = $factory;
                require_caps!(p, has: CapabilityFlags::SYMLINKS);
                let root: VPath = $root;
                let hostiles: &[(&str, &[u8])] = &[
                    ("latin1", b"caf\xE9"),
                    ("relative_deep", b"sub/dir/f"),
                    ("absolute", b"/etc/hostname"),
                    ("dotdot", b"../fuera"),
                    ("windows_style", b"C:\\Users\\x"),
                    ("lone_surrogate", &[0xED, 0xA0, 0x80]),
                    ("not_wtf8", &[0xFF, 0xFE]),
                ];
                for (i, (id, target)) in hostiles.iter().enumerate() {
                    let link = child(&root, format!("ln{i}").as_bytes());
                    match p.symlink(&link, target, SymlinkKind::File).await {
                        Ok(()) => {
                            assert_eq!(
                                p.read_link(&link).await.expect("read_link"),
                                *target,
                                "bytes del target intactos: {id}"
                            );
                        }
                        // El OS puede rechazar el target (Windows exige
                        // WTF-8): rechazo LIMPIO, jamás lossy ni panic.
                        Err(Error::InvalidPath) => {
                            eprintln!("skip target {id}: rechazo limpio del OS");
                        }
                        Err(e) => panic!("symlink con target {id}: {e:?}"),
                    }
                }
            }

            #[tokio::test]
            async fn contract_symlink_over_existing_is_conflict() {
                use $crate::SymlinkKind;
                let p = $factory;
                require_caps!(p, has: CapabilityFlags::SYMLINKS);
                let root: VPath = $root;
                let f = child(&root, b"ocupado");
                write_all(&p, &f, b"x").await;
                match p.symlink(&f, b"target", SymlinkKind::File).await {
                    Err(Error::Conflict { .. }) => {}
                    other => panic!("esperaba Conflict, fue {other:?}"),
                }
                assert_eq!(read_all(&p, &f).await.expect("intacto"), b"x");
            }

            /// ADR 0009: con capability TRASH, `trash()` se lleva el
            /// árbol ENTERO y el path deja de existir; sin capability,
            /// Unsupported (jamás borrar en su lugar).
            #[tokio::test]
            async fn contract_trash_takes_the_tree_or_refuses() {
                let p = $factory;
                let root: VPath = $root;
                // Nombre ÚNICO y reconocible: la papelera real del
                // desarrollador acumula esto — la purga vive en los tests
                // del provider local (os_limited); macOS se acepta
                // documentado en ADR 0009.
                let nombre = format!("norte-contract-trash-{}", std::process::id());
                let dir = child(&root, nombre.as_bytes());
                p.mkdir(&dir).await.expect("mkdir");
                write_all(&p, &child(&dir, b"hijo"), b"x").await;
                // Y una víctima con nombre HOSTIL (no-UTF8): el trash de
                // los 3 OS debe tragarlo o rechazar limpio, jamás panicar.
                // (Si el FS rechaza el nombre — APFS — simplemente no está.)
                if let Ok(mut sink) = p.write(&child(&dir, b"tr\xE1sh")).await {
                    let _ = sink.write(Bytes::from_static(b"x")).await;
                    let _ = sink.commit().await;
                }
                if p.capabilities().flags.contains(CapabilityFlags::TRASH) {
                    p.trash(&dir).await.expect("trash");
                    assert_eq!(
                        p.stat(&dir).await.expect_err("se fue"),
                        Error::NotFound
                    );
                } else {
                    assert!(matches!(p.trash(&dir).await, Err(Error::Unsupported)));
                    assert!(p.stat(&dir).await.is_ok(), "sin papelera NO se toca");
                }
            }

            #[tokio::test]
            async fn contract_read_link_errors() {
                let p = $factory;
                let root: VPath = $root;
                // Inexistente: NotFound (o Unsupported si el provider no
                // sabe de symlinks en absoluto).
                match p.read_link(&child(&root, b"no-existe")).await {
                    Err(Error::NotFound | Error::Unsupported) => {}
                    other => panic!("esperaba NotFound/Unsupported, fue {other:?}"),
                }
                // Archivo normal: TypeMismatch (o Unsupported).
                let f = child(&root, b"normal");
                write_all(&p, &f, b"x").await;
                match p.read_link(&f).await {
                    Err(
                        Error::Conflict {
                            conflict: ConflictKind::TypeMismatch,
                        }
                        | Error::Unsupported,
                    ) => {}
                    other => panic!("esperaba TypeMismatch/Unsupported, fue {other:?}"),
                }
            }

            #[tokio::test]
            async fn contract_read_dir_errors() {
                let p = $factory;
                let root: VPath = $root;
                let d = child(&root, b"dir");
                p.mkdir(&d).await.unwrap();
                assert!(read_all(&p, &d).await.is_err(), "leer un dir es error");
            }

            // ---------- node_id (identidad real, issue #16) ----------

            /// Un provider CON identidad: estable entre llamadas y distinta
            /// entre nodos distintos. Un provider sin identidad (Ok(None))
            /// se auto-salta — el default del trait es legal.
            #[tokio::test]
            async fn contract_node_id_stable_and_distinct() {
                use $crate::FollowLinks;
                let p = $factory;
                let root: VPath = $root;
                let a = child(&root, b"ida");
                let b = child(&root, b"idb");
                write_all(&p, &a, b"x").await;
                write_all(&p, &b, b"y").await;
                let Some(id_a) = p.node_id(&a, FollowLinks::No).await.expect("node_id a") else {
                    eprintln!("skip: el provider no expone identidad de nodo");
                    return;
                };
                let id_a2 = p
                    .node_id(&a, FollowLinks::No)
                    .await
                    .expect("node_id a, 2ª")
                    .expect("con identidad: siempre o nunca");
                assert_eq!(id_a, id_a2, "estable entre llamadas");
                let id_b = p
                    .node_id(&b, FollowLinks::No)
                    .await
                    .expect("node_id b")
                    .expect("con identidad: siempre o nunca");
                assert_ne!(id_a, id_b, "nodos distintos, identidades distintas");
                // En un nodo que no es symlink, follow no cambia nada.
                let id_a3 = p
                    .node_id(&a, FollowLinks::Yes)
                    .await
                    .expect("node_id a, follow")
                    .expect("con identidad: siempre o nunca");
                assert_eq!(id_a, id_a3, "follow sobre no-symlink es lo mismo");
            }

            /// La identidad sobrevive al rename: es del NODO, no del path.
            /// (Es la base del rename_retrying del engine, issue #17.)
            #[tokio::test]
            async fn contract_node_id_survives_rename() {
                use $crate::FollowLinks;
                let p = $factory;
                let root: VPath = $root;
                let antes = child(&root, b"id-antes");
                write_all(&p, &antes, b"x").await;
                let Some(id) = p.node_id(&antes, FollowLinks::No).await.expect("node_id") else {
                    eprintln!("skip: el provider no expone identidad de nodo");
                    return;
                };
                let despues = child(&root, b"id-despues");
                p.rename(&antes, &despues).await.expect("rename");
                let id2 = p
                    .node_id(&despues, FollowLinks::No)
                    .await
                    .expect("node_id tras rename")
                    .expect("con identidad: siempre o nunca");
                assert_eq!(id, id2, "el rename mueve el nodo, no lo recrea");
            }

            /// `FollowLinks::Yes` resuelve el symlink (identidad del DESTINO);
            /// `No` da la identidad del propio link (semántica lstat).
            #[tokio::test]
            async fn contract_node_id_follow_resolves_link() {
                use $crate::{FollowLinks, SymlinkKind};
                let p = $factory;
                require_caps!(p, has: CapabilityFlags::SYMLINKS);
                let root: VPath = $root;
                let f = child(&root, b"idf");
                write_all(&p, &f, b"x").await;
                let link = child(&root, b"idlink");
                // Target relativo al padre del link: resoluble en cualquier
                // provider (mismo convenio que el resolve mínimo de Mem).
                p.symlink(&link, b"idf", SymlinkKind::File)
                    .await
                    .expect("symlink crea");
                let Some(target_id) = p.node_id(&f, FollowLinks::No).await.expect("id del target")
                else {
                    eprintln!("skip: el provider no expone identidad de nodo");
                    return;
                };
                let followed = p
                    .node_id(&link, FollowLinks::Yes)
                    .await
                    .expect("node_id follow")
                    .expect("con identidad: siempre o nunca");
                assert_eq!(followed, target_id, "Yes = identidad del destino");
                let own = p
                    .node_id(&link, FollowLinks::No)
                    .await
                    .expect("node_id del link")
                    .expect("con identidad: siempre o nunca");
                assert_ne!(own, target_id, "No = identidad del propio link");
            }

            /// Symlink roto: `Yes` es NotFound (no hay destino); `No` sigue
            /// funcionando (el link existe). Path inexistente: NotFound en
            /// ambos modos (u Ok(None) si el provider no sabe de identidad).
            #[tokio::test]
            async fn contract_node_id_broken_and_missing() {
                use $crate::{FollowLinks, SymlinkKind};
                let p = $factory;
                let root: VPath = $root;
                match p.node_id(&child(&root, b"no-existe"), FollowLinks::No).await {
                    Ok(None) | Err(Error::NotFound) => {}
                    other => panic!("esperaba NotFound/Ok(None), fue {other:?}"),
                }
                require_caps!(p, has: CapabilityFlags::SYMLINKS);
                let roto = child(&root, b"id-roto");
                p.symlink(&roto, b"nada-aqui", SymlinkKind::File)
                    .await
                    .expect("symlink crea");
                match p.node_id(&roto, FollowLinks::No).await {
                    Ok(_) => {}
                    other => panic!("el link EXISTE aunque esté roto, fue {other:?}"),
                }
                match p.node_id(&roto, FollowLinks::Yes).await {
                    Err(Error::NotFound) => {}
                    // Sin identidad, None es legal también para un link roto.
                    Ok(None) => {}
                    other => panic!("esperaba NotFound al seguir link roto, fue {other:?}"),
                }
            }

            /// GARANTÍA del trait: `remove` sobre un symlink borra EL
            /// LINK, jamás su target (semántica unlink). El move del copy
            /// engine depende de esto para no destruir targets al borrar
            /// links expandidos (issue #19).
            #[tokio::test]
            async fn contract_remove_symlink_leaves_target_intact() {
                use $crate::SymlinkKind;
                let p = $factory;
                require_caps!(p, has: CapabilityFlags::SYMLINKS);
                let root: VPath = $root;
                let dir = child(&root, b"rmtarget");
                p.mkdir(&dir).await.expect("mkdir");
                write_all(&p, &child(&dir, b"hijo"), b"vivo").await;
                let link = child(&root, b"rmlink");
                p.symlink(&link, b"rmtarget", SymlinkKind::Dir)
                    .await
                    .expect("symlink crea");
                p.remove(&link).await.expect("remove del LINK");
                assert_eq!(
                    p.stat(&link).await.expect_err("el link se fue"),
                    Error::NotFound
                );
                let e = p.stat(&dir).await.expect("el target VIVE");
                assert_eq!(e.kind, EntryKind::Dir);
                assert_eq!(
                    read_all(&p, &child(&dir, b"hijo")).await.expect("contenido intacto"),
                    b"vivo"
                );
            }

            /// La identidad funciona igual con nombres HOSTILES (bytes
            /// no-UTF8). Si el FS rechaza el nombre (APFS exige UTF-8),
            /// rechazo limpio y skip — como el resto del corpus hostil.
            #[tokio::test]
            async fn contract_node_id_with_hostile_name() {
                use $crate::FollowLinks;
                let p = $factory;
                let root: VPath = $root;
                let name: &[u8] = b"id-\xE9-latin1";
                let Ok(seg) = $crate::__private::norte_proto::Segment::new(name.to_vec()) else {
                    panic!("segmento hostil válido");
                };
                let path = root.join(seg);
                let mut sink = match p.write(&path).await {
                    Ok(s) => s,
                    Err(Error::InvalidPath) => {
                        eprintln!("skip: el FS rechaza el nombre (limpio)");
                        return;
                    }
                    Err(e) => panic!("write hostil: {e:?}"),
                };
                sink.write(Bytes::from_static(b"x")).await.expect("chunk");
                if let Err(Error::InvalidPath) = sink.commit().await {
                    eprintln!("skip: el FS rechaza el nombre en commit (limpio)");
                    return;
                }
                let Some(id) = p.node_id(&path, FollowLinks::No).await.expect("node_id") else {
                    eprintln!("skip: el provider no expone identidad de nodo");
                    return;
                };
                let id2 = p
                    .node_id(&path, FollowLinks::No)
                    .await
                    .expect("node_id, 2ª")
                    .expect("con identidad: siempre o nunca");
                assert_eq!(id, id2, "identidad estable con bytes no-UTF8");
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
                        // Destino existente: Conflict, jamás sobrescritura
                        // silenciosa (misma política que write).
                        match p.copy_native(&a, &b).await {
                            Some(Err(Error::Conflict { .. })) => {}
                            other => panic!(
                                "copy_native sobre destino existente debía dar Conflict, fue {other:?}"
                            ),
                        }
                    }
                }
            }
        }
    };
}
