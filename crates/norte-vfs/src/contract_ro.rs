//! [`readonly_provider_contract!`]: la suite contractual de providers
//! SOLO-LECTURA (ADR 0018). La suite RW (`provider_contract!`) siembra vía
//! `write` del propio provider en casi todos sus casos — un provider
//! `READ_ONLY` no puede pasarla. Esta variante exige al factory un árbol
//! canónico PRE-SEMBRADO y verifica el contrato de lectura + que TODA
//! mutación responda `Unsupported`.

/// Genera la suite contractual read-only dentro de un módulo de test.
///
/// Requisitos del crate invocante (dev-dependencies): `tokio` (features
/// `macros`, `rt`) — el resto llega vía re-exports internos de `norte-vfs`.
///
/// - `mod`: nombre del módulo generado.
/// - `factory`: expresión que construye un provider FRESCO ya sembrado con
///   el árbol canónico (se evalúa una vez por test).
/// - `root`: expresión que construye el `VPath` raíz del provider.
/// - `hostile_names`: expresión `Vec<Vec<u8>>` con los nombres hostiles que
///   el factory garantiza presentes (subconjunto del corpus si el formato
///   tiene límites — p. ej. tar ≤ 100 bytes).
///
/// Árbol canónico que el factory DEBE sembrar:
///
/// ```text
/// /docs/hello.txt      → b"hola norte\n"
/// /docs/sub/nested.bin → b"\x00\x01\x02\xff"
/// /vacio.txt           → b""  (archivo vacío)
/// /hostile/<name>      → contenido = los bytes del nombre  (por cada
///                        hostile_name)
/// ```
///
/// ```ignore
/// // En tests de integración de un provider read-only.
/// norte_vfs::readonly_provider_contract! {
///     mod contract_ro_zip,
///     factory: fixture_zip_provider(),
///     root: fixture_zip_root(),
///     hostile_names: norte_testkit::corpus::hostile_names()
///         .into_iter()
///         .map(|n| n.bytes)
///         .collect(),
/// }
/// ```
#[macro_export]
macro_rules! readonly_provider_contract {
    (
        mod $name:ident,
        factory: $factory:expr,
        root: $root:expr,
        hostile_names: $hostile:expr $(,)?
    ) => {
        mod $name {
            // Las expresiones `factory`/`root`/`hostile_names` se evalúan en
            // este módulo: importa el scope del invocante.
            #[allow(unused_imports)]
            use super::*;

            use $crate::__private::futures::StreamExt;
            use $crate::__private::norte_proto::{
                ATTR_BYTES_MAX, ATTR_TEXT_MAX, ATTRS_MAX_ADVERTISED, AttrType, AttrValue,
                ByteRange, CapabilityFlags, EntryKind, Error, Segment, VPath, is_valid_attr_id,
            };
            use $crate::{AttrRequest, ListOptions, Provider};

            fn seg(bytes: &[u8]) -> Segment {
                Segment::new(bytes.to_vec()).expect("segmento válido de contrato")
            }

            fn child(base: &VPath, name: &[u8]) -> VPath {
                base.join(seg(name))
            }

            async fn read_all<P: Provider>(
                p: &P,
                path: &VPath,
                range: Option<ByteRange>,
            ) -> Result<Vec<u8>, Error> {
                let mut stream = p.read(path, range).await?;
                let mut out = Vec::new();
                while let Some(chunk) = stream.next().await {
                    out.extend_from_slice(&chunk?);
                }
                Ok(out)
            }

            async fn list_names<P: Provider>(p: &P, dir: &VPath) -> Vec<Vec<u8>> {
                let mut names: Vec<Vec<u8>> = p
                    .list(dir)
                    .await
                    .expect("list abre")
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

            // ---------- stat / list / read ----------

            #[tokio::test]
            async fn ro_stat_root_is_dir() {
                let p = $factory;
                let root: VPath = $root;
                let e = p.stat(&root).await.expect("la raíz siempre existe");
                assert_eq!(e.kind, EntryKind::Dir);
            }

            #[tokio::test]
            async fn ro_stat_missing_is_not_found() {
                let p = $factory;
                let root: VPath = $root;
                assert_eq!(
                    p.stat(&child(&root, b"no-existe")).await.unwrap_err(),
                    Error::NotFound
                );
                // También bajo un subdirectorio real.
                assert_eq!(
                    p.stat(&child(&child(&root, b"docs"), b"no"))
                        .await
                        .unwrap_err(),
                    Error::NotFound
                );
            }

            #[tokio::test]
            async fn ro_stat_file_and_dir_metadata() {
                let p = $factory;
                let root: VPath = $root;
                let f = p
                    .stat(&child(&child(&root, b"docs"), b"hello.txt"))
                    .await
                    .expect("hello.txt existe");
                assert_eq!(f.kind, EntryKind::File);
                assert_eq!(f.size, Some(11), "b\"hola norte\\n\" son 11 bytes");
                let d = p.stat(&child(&root, b"docs")).await.expect("docs existe");
                assert_eq!(d.kind, EntryKind::Dir);
            }

            #[tokio::test]
            async fn ro_list_tree_byte_exact() {
                let p = $factory;
                let root: VPath = $root;
                assert_eq!(
                    list_names(&p, &root).await,
                    vec![b"docs".to_vec(), b"hostile".to_vec(), b"vacio.txt".to_vec()]
                );
                let docs = child(&root, b"docs");
                assert_eq!(
                    list_names(&p, &docs).await,
                    vec![b"hello.txt".to_vec(), b"sub".to_vec()]
                );
                // Los kinds del listado coinciden con stat.
                let entries: Vec<_> = p
                    .list(&docs)
                    .await
                    .expect("list docs")
                    .map(|e| e.expect("entrada ok"))
                    .collect()
                    .await;
                for e in entries {
                    let via_stat = p.stat(&e.path).await.expect("stat de lo listado");
                    assert_eq!(e.kind, via_stat.kind, "{:?}", e.path);
                }
            }

            #[tokio::test]
            async fn ro_list_missing_is_not_found() {
                let p = $factory;
                let root: VPath = $root;
                assert_eq!(
                    p.list(&child(&root, b"no-existe")).await.err(),
                    Some(Error::NotFound)
                );
            }

            #[tokio::test]
            async fn ro_list_file_is_error() {
                let p = $factory;
                let root: VPath = $root;
                match p.list(&child(&root, b"vacio.txt")).await {
                    Err(Error::Conflict { .. } | Error::NotFound | Error::Io { .. }) => {}
                    Ok(_) => panic!("listar un archivo no puede ser Ok"),
                    Err(e) => panic!("error inesperado listando archivo: {e:?}"),
                }
            }

            #[tokio::test]
            async fn ro_read_full_and_empty() {
                let p = $factory;
                let root: VPath = $root;
                assert_eq!(
                    read_all(&p, &child(&child(&root, b"docs"), b"hello.txt"), None)
                        .await
                        .expect("leer hello.txt"),
                    b"hola norte\n"
                );
                assert_eq!(
                    read_all(
                        &p,
                        &child(&child(&child(&root, b"docs"), b"sub"), b"nested.bin"),
                        None
                    )
                    .await
                    .expect("leer nested.bin"),
                    b"\x00\x01\x02\xff"
                );
                assert_eq!(
                    read_all(&p, &child(&root, b"vacio.txt"), None)
                        .await
                        .expect("leer vacío"),
                    b""
                );
            }

            #[tokio::test]
            async fn ro_read_range_slices_exactly() {
                let p = $factory;
                let root: VPath = $root;
                let f = child(&child(&root, b"docs"), b"hello.txt"); // "hola norte\n"
                let mid = ByteRange {
                    offset: 2,
                    len: Some(3),
                };
                assert_eq!(
                    read_all(&p, &f, Some(mid)).await.expect("rango medio"),
                    b"la "
                );
                let cola = ByteRange {
                    offset: 8,
                    len: None,
                };
                assert_eq!(
                    read_all(&p, &f, Some(cola)).await.expect("hasta EOF"),
                    b"te\n"
                );
                let pasado = ByteRange {
                    offset: 100,
                    len: Some(4),
                };
                assert_eq!(
                    read_all(&p, &f, Some(pasado))
                        .await
                        .expect("pread past-EOF"),
                    b"",
                    "offset más allá de EOF = stream vacío, no error"
                );
                let sobra = ByteRange {
                    offset: 7,
                    len: Some(100),
                };
                assert_eq!(
                    read_all(&p, &f, Some(sobra))
                        .await
                        .expect("len recortado a EOF"),
                    b"rte\n"
                );
            }

            #[tokio::test]
            async fn ro_read_missing_is_not_found() {
                let p = $factory;
                let root: VPath = $root;
                assert_eq!(
                    read_all(&p, &child(&root, b"no-existe"), None)
                        .await
                        .unwrap_err(),
                    Error::NotFound
                );
            }

            #[tokio::test]
            async fn ro_read_dir_is_error() {
                let p = $factory;
                let root: VPath = $root;
                assert!(
                    read_all(&p, &child(&root, b"docs"), None).await.is_err(),
                    "leer un dir es error"
                );
            }

            // ---------- nombres hostiles (byte-exactos, sin reparar) ----------

            #[tokio::test]
            async fn ro_hostile_names_listed_and_read_byte_exact() {
                let p = $factory;
                let root: VPath = $root;
                let mut expected: Vec<Vec<u8>> = $hostile;
                expected.sort();
                let dir = child(&root, b"hostile");
                assert_eq!(
                    list_names(&p, &dir).await,
                    expected,
                    "el listado preserva los bytes del corpus tal cual"
                );
                for name in &expected {
                    let got = read_all(&p, &child(&dir, name), None)
                        .await
                        .expect("leer entrada hostil");
                    assert_eq!(&got, name, "contenido = bytes del nombre");
                }
            }

            // ---------- capabilities / mutaciones ----------

            #[tokio::test]
            async fn ro_caps_declare_read_only() {
                let p = $factory;
                let flags = p.capabilities().flags;
                assert!(flags.contains(CapabilityFlags::READ_ONLY));
                for prohibido in [
                    CapabilityFlags::RENAME_ATOMIC,
                    CapabilityFlags::SERVER_COPY,
                    CapabilityFlags::APPEND,
                    CapabilityFlags::RANDOM_WRITE,
                    CapabilityFlags::TRASH,
                ] {
                    assert!(
                        !flags.contains(prohibido),
                        "READ_ONLY excluye {prohibido:?}"
                    );
                }
            }

            #[tokio::test]
            async fn ro_mutations_are_unsupported() {
                use $crate::SymlinkKind;
                let p = $factory;
                let root: VPath = $root;
                let nuevo = child(&root, b"nuevo");
                let existente = child(&root, b"vacio.txt");
                assert!(matches!(
                    p.write(&nuevo).await.err(),
                    Some(Error::Unsupported)
                ));
                // También sobre un path que EXISTE: Unsupported, no Conflict —
                // la operación está vetada antes de mirar el destino.
                assert!(matches!(
                    p.write(&existente).await.err(),
                    Some(Error::Unsupported)
                ));
                assert!(matches!(
                    p.open_resumable(&nuevo).await.err(),
                    Some(Error::Unsupported)
                ));
                assert!(matches!(
                    p.mkdir(&nuevo).await.err(),
                    Some(Error::Unsupported)
                ));
                assert!(matches!(
                    p.remove(&existente).await.err(),
                    Some(Error::Unsupported)
                ));
                assert!(matches!(
                    p.rename(&existente, &nuevo).await.err(),
                    Some(Error::Unsupported)
                ));
                assert!(matches!(
                    p.trash(&existente, &$crate::trash::TrashId::new(0, 0))
                        .await
                        .err(),
                    Some(Error::Unsupported)
                ));
                assert!(matches!(
                    p.symlink(&nuevo, b"target", SymlinkKind::File).await.err(),
                    Some(Error::Unsupported)
                ));
                assert!(
                    p.copy_native(&existente, &nuevo).await.is_none(),
                    "sin SERVER_COPY, copy_native = None"
                );
            }

            #[tokio::test]
            async fn ro_read_link_errors() {
                let p = $factory;
                let root: VPath = $root;
                // Sobre un archivo normal: TypeMismatch o Unsupported.
                match p.read_link(&child(&root, b"vacio.txt")).await {
                    Err(Error::Unsupported | Error::Conflict { .. }) => {}
                    other => panic!("esperaba Unsupported/TypeMismatch, fue {other:?}"),
                }
                // Sobre un inexistente: NotFound o Unsupported.
                match p.read_link(&child(&root, b"no-existe")).await {
                    Err(Error::Unsupported | Error::NotFound) => {}
                    other => panic!("esperaba Unsupported/NotFound, fue {other:?}"),
                }
            }

            // ---------- attrs (#108 bloque 2, ADR 0039) ----------

            fn attr_type_matches(ty: AttrType, v: &AttrValue) -> bool {
                matches!(
                    (ty, v),
                    (AttrType::Uint, AttrValue::Uint(_))
                        | (AttrType::Int, AttrValue::Int(_))
                        | (AttrType::Text, AttrValue::Text(_))
                        | (AttrType::Bytes, AttrValue::Bytes(_))
                        | (AttrType::TimeMs, AttrValue::TimeMs(_))
                        | (AttrType::Bool, AttrValue::Bool(_))
                )
            }

            /// Contrato por entrada: solo ids pedidos, todos anunciados, tipo
            /// declarado ⟺ variante producida, Text/Bytes dentro de tope.
            fn assert_attrs_contract(
                catalog: &[$crate::__private::norte_proto::AttrInfo],
                requested: &AttrRequest,
                entry: &$crate::__private::norte_proto::Entry,
            ) {
                for (id, v) in &entry.attrs {
                    assert!(
                        requested.wants(id),
                        "attr NO pedido en {:?}: {id:?}",
                        entry.path.display_lossy()
                    );
                    let info = catalog
                        .iter()
                        .find(|a| &a.id == id)
                        .unwrap_or_else(|| panic!("attr no anunciado: {id:?}"));
                    assert!(
                        attr_type_matches(info.ty, v),
                        "tipo declarado {:?} no casa con {v:?} para {id:?}",
                        info.ty
                    );
                    match v {
                        AttrValue::Text(s) => {
                            assert!(s.len() <= ATTR_TEXT_MAX, "Text sobre tope: {id:?}");
                        }
                        AttrValue::Bytes(b) => {
                            assert!(b.len() <= ATTR_BYTES_MAX, "Bytes sobre tope: {id:?}");
                        }
                        _ => {}
                    }
                }
            }

            #[tokio::test]
            async fn ro_attrs_catalog_is_sane() {
                let p = $factory;
                let catalog = p.attrs();
                assert!(catalog.len() <= ATTRS_MAX_ADVERTISED, "catálogo sobre tope");
                let mut seen = std::collections::BTreeSet::new();
                for info in catalog {
                    assert!(
                        is_valid_attr_id(&info.id),
                        "id inválido en catálogo: {:?}",
                        info.id
                    );
                    assert!(seen.insert(info.id.clone()), "id duplicado: {:?}", info.id);
                }
            }

            #[tokio::test]
            async fn ro_attrs_values_match_declared_types() {
                let p = $factory;
                if p.attrs().is_empty() {
                    eprintln!("skip: catálogo de attrs vacío");
                    return;
                }
                let root: VPath = $root;
                let ids: Vec<String> = p.attrs().iter().map(|a| a.id.clone()).collect();
                let opt = ListOptions {
                    attrs: AttrRequest::sanitized(ids),
                };
                let catalog = p.attrs().to_vec();

                // stat_with sobre un archivo del árbol canónico.
                let probe = child(&child(&root, b"docs"), b"hello.txt");
                let e = p.stat_with(&probe, &opt).await.expect("stat_with");
                assert_attrs_contract(&catalog, &opt.attrs, &e);

                // list_with sobre la raíz: TODA entrada cumple.
                let mut stream = p.list_with(&root, &opt).await.expect("list_with");
                let mut n = 0usize;
                while let Some(e) = stream.next().await {
                    let e = e.expect("entrada del listado");
                    assert_attrs_contract(&catalog, &opt.attrs, &e);
                    n += 1;
                }
                assert!(n >= 1, "el listado canónico no está vacío");
            }

            #[tokio::test]
            async fn ro_attrs_empty_request_yields_bare_entries() {
                let p = $factory;
                let root: VPath = $root;
                let opt = ListOptions::default();
                let e = p
                    .stat_with(&child(&root, b"vacio.txt"), &opt)
                    .await
                    .expect("stat_with");
                assert!(e.attrs.is_empty(), "sin petición no hay attrs");
                let mut stream = p.list_with(&root, &opt).await.expect("list_with");
                while let Some(e) = stream.next().await {
                    assert!(
                        e.expect("entrada").attrs.is_empty(),
                        "sin petición no hay attrs"
                    );
                }
            }

            #[tokio::test]
            async fn ro_attrs_unknown_requested_id_is_absent_not_error() {
                let p = $factory;
                let root: VPath = $root;
                let opt = ListOptions {
                    attrs: AttrRequest::sanitized(["zz.does-not-exist".to_owned()]),
                };
                let e = p
                    .stat_with(&child(&root, b"vacio.txt"), &opt)
                    .await
                    .expect("id desconocido jamás es error");
                assert!(!e.attrs.contains_key("zz.does-not-exist"));
            }
        }
    };
}
