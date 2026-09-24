//! [`readonly_provider_contract!`]: the contract suite for READ-ONLY
//! providers (ADR 0018). The RW suite (`provider_contract!`) seeds via
//! the provider's own `write` in almost every case — a `READ_ONLY`
//! provider can't pass it. This variant requires the factory to hand it a
//! PRE-SEEDED canonical tree and verifies the read contract + that EVERY
//! mutation answers `Unsupported`.

/// Generates the read-only contract suite inside a test module.
///
/// Requirements of the invoking crate (dev-dependencies): `tokio`
/// (features `macros`, `rt`) — the rest arrives via `norte-vfs`'s internal
/// re-exports.
///
/// - `mod`: name of the generated module.
/// - `factory`: expression that builds a FRESH provider already seeded
///   with the canonical tree (evaluated once per test).
/// - `root`: expression that builds the provider's root `VPath`.
/// - `hostile_names`: `Vec<Vec<u8>>` expression with the hostile names the
///   factory guarantees are present (a subset of the corpus if the format
///   has limits — e.g. tar ≤ 100 bytes).
///
/// Canonical tree the factory MUST seed:
///
/// ```text
/// /docs/hello.txt      → b"hola norte\n"
/// /docs/sub/nested.bin → b"\x00\x01\x02\xff"
/// /vacio.txt           → b""  (empty file)
/// /hostile/<name>      → content = the name's bytes  (for every
///                        hostile_name)
/// ```
///
/// ```ignore
/// // In a read-only provider's integration tests.
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
            // The `factory`/`root`/`hostile_names` expressions are
            // evaluated in this module: imports the caller's scope.
            #[allow(unused_imports)]
            use super::*;

            use $crate::__private::futures::StreamExt;
            use $crate::__private::norte_proto::{
                ByteRange, CapabilityFlags, EntryKind, Error, Segment, VPath,
            };
            use $crate::{AttrRequest, ListOptions, Provider};

            fn seg(bytes: &[u8]) -> Segment {
                Segment::new(bytes.to_vec()).expect("valid contract segment")
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
                    .expect("list opens")
                    .map(|e| {
                        e.expect("ok entry")
                            .path
                            .file_name()
                            .expect("has a name")
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
                let e = p.stat(&root).await.expect("the root always exists");
                assert_eq!(e.kind, EntryKind::Dir);
            }

            #[tokio::test]
            async fn ro_stat_missing_is_not_found() {
                let p = $factory;
                let root: VPath = $root;
                assert_eq!(
                    p.stat(&child(&root, b"does-not-exist")).await.unwrap_err(),
                    Error::NotFound
                );
                // Also under a real subdirectory.
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
                    .expect("hello.txt exists");
                assert_eq!(f.kind, EntryKind::File);
                assert_eq!(f.size, Some(11), "b\"hola norte\\n\" is 11 bytes");
                let d = p.stat(&child(&root, b"docs")).await.expect("docs exists");
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
                // The listing's kinds match stat's.
                let entries: Vec<_> = p
                    .list(&docs)
                    .await
                    .expect("list docs")
                    .map(|e| e.expect("ok entry"))
                    .collect()
                    .await;
                for e in entries {
                    let via_stat = p.stat(&e.path).await.expect("stat of the listed entry");
                    assert_eq!(e.kind, via_stat.kind, "{:?}", e.path);
                }
            }

            #[tokio::test]
            async fn ro_list_missing_is_not_found() {
                let p = $factory;
                let root: VPath = $root;
                assert_eq!(
                    p.list(&child(&root, b"does-not-exist")).await.err(),
                    Some(Error::NotFound)
                );
            }

            #[tokio::test]
            async fn ro_list_file_is_error() {
                let p = $factory;
                let root: VPath = $root;
                match p.list(&child(&root, b"vacio.txt")).await {
                    Err(Error::Conflict { .. } | Error::NotFound | Error::Io { .. }) => {}
                    Ok(_) => panic!("listing a file can't be Ok"),
                    Err(e) => panic!("unexpected error listing a file: {e:?}"),
                }
            }

            #[tokio::test]
            async fn ro_read_full_and_empty() {
                let p = $factory;
                let root: VPath = $root;
                assert_eq!(
                    read_all(&p, &child(&child(&root, b"docs"), b"hello.txt"), None)
                        .await
                        .expect("read hello.txt"),
                    b"hola norte\n"
                );
                assert_eq!(
                    read_all(
                        &p,
                        &child(&child(&child(&root, b"docs"), b"sub"), b"nested.bin"),
                        None
                    )
                    .await
                    .expect("read nested.bin"),
                    b"\x00\x01\x02\xff"
                );
                assert_eq!(
                    read_all(&p, &child(&root, b"vacio.txt"), None)
                        .await
                        .expect("read empty"),
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
                    read_all(&p, &f, Some(mid)).await.expect("mid range"),
                    b"la "
                );
                let tail = ByteRange {
                    offset: 8,
                    len: None,
                };
                assert_eq!(
                    read_all(&p, &f, Some(tail)).await.expect("until EOF"),
                    b"te\n"
                );
                let past = ByteRange {
                    offset: 100,
                    len: Some(4),
                };
                assert_eq!(
                    read_all(&p, &f, Some(past)).await.expect("pread past-EOF"),
                    b"",
                    "offset past EOF = empty stream, not an error"
                );
                let excess = ByteRange {
                    offset: 7,
                    len: Some(100),
                };
                assert_eq!(
                    read_all(&p, &f, Some(excess))
                        .await
                        .expect("len trimmed to EOF"),
                    b"rte\n"
                );
            }

            #[tokio::test]
            async fn ro_read_missing_is_not_found() {
                let p = $factory;
                let root: VPath = $root;
                assert_eq!(
                    read_all(&p, &child(&root, b"does-not-exist"), None)
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
                    "reading a dir is an error"
                );
            }

            // ---------- hostile names (byte-exact, unrepaired) ----------

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
                    "the listing preserves the corpus's bytes as is"
                );
                for name in &expected {
                    let got = read_all(&p, &child(&dir, name), None)
                        .await
                        .expect("read hostile entry");
                    assert_eq!(&got, name, "content = the name's bytes");
                }
            }

            // ---------- capabilities / mutations ----------

            #[tokio::test]
            async fn ro_caps_declare_read_only() {
                let p = $factory;
                let flags = p.capabilities().flags;
                assert!(flags.contains(CapabilityFlags::READ_ONLY));
                for forbidden in [
                    CapabilityFlags::RENAME_ATOMIC,
                    CapabilityFlags::SERVER_COPY,
                    CapabilityFlags::APPEND,
                    CapabilityFlags::RANDOM_WRITE,
                    CapabilityFlags::TRASH,
                ] {
                    assert!(
                        !flags.contains(forbidden),
                        "READ_ONLY excludes {forbidden:?}"
                    );
                }
            }

            #[tokio::test]
            async fn ro_mutations_are_unsupported() {
                use $crate::SymlinkKind;
                let p = $factory;
                let root: VPath = $root;
                let new_path = child(&root, b"new");
                let existing = child(&root, b"vacio.txt");
                assert!(matches!(
                    p.write(&new_path).await.err(),
                    Some(Error::Unsupported)
                ));
                // Also over a path that EXISTS: Unsupported, not Conflict —
                // the operation is vetoed before looking at the destination.
                assert!(matches!(
                    p.write(&existing).await.err(),
                    Some(Error::Unsupported)
                ));
                assert!(matches!(
                    p.open_resumable(&new_path).await.err(),
                    Some(Error::Unsupported)
                ));
                assert!(matches!(
                    p.mkdir(&new_path).await.err(),
                    Some(Error::Unsupported)
                ));
                assert!(matches!(
                    p.remove(&existing).await.err(),
                    Some(Error::Unsupported)
                ));
                assert!(matches!(
                    p.rename(&existing, &new_path).await.err(),
                    Some(Error::Unsupported)
                ));
                assert!(matches!(
                    p.trash(&existing, &$crate::trash::TrashId::new(0, 0))
                        .await
                        .err(),
                    Some(Error::Unsupported)
                ));
                assert!(matches!(
                    p.symlink(&new_path, b"target", SymlinkKind::File)
                        .await
                        .err(),
                    Some(Error::Unsupported)
                ));
                assert!(
                    p.copy_native(&existing, &new_path).await.is_none(),
                    "without SERVER_COPY, copy_native = None"
                );
            }

            #[tokio::test]
            async fn ro_read_link_errors() {
                let p = $factory;
                let root: VPath = $root;
                // Over a normal file: TypeMismatch or Unsupported.
                match p.read_link(&child(&root, b"vacio.txt")).await {
                    Err(Error::Unsupported | Error::Conflict { .. }) => {}
                    other => panic!("expected Unsupported/TypeMismatch, was {other:?}"),
                }
                // Over a nonexistent one: NotFound or Unsupported.
                match p.read_link(&child(&root, b"does-not-exist")).await {
                    Err(Error::Unsupported | Error::NotFound) => {}
                    other => panic!("expected Unsupported/NotFound, was {other:?}"),
                }
            }

            // ---------- attrs (#108 block 2, ADR 0039) ----------
            // Assertions shared with the RW suite:
            // `__private::contract_attrs` (a divergence would silently weaken a suite).
            use $crate::__private::contract_attrs::{assert_attrs_contract, assert_catalog_sane};

            #[tokio::test]
            async fn ro_attrs_catalog_is_sane() {
                let p = $factory;
                assert_catalog_sane(p.attrs());
            }

            #[tokio::test]
            async fn ro_attrs_values_match_declared_types() {
                let p = $factory;
                if p.attrs().is_empty() {
                    eprintln!("skip: empty attrs catalogue");
                    return;
                }
                let root: VPath = $root;
                let ids: Vec<String> = p.attrs().iter().map(|a| a.id.clone()).collect();
                let opt = ListOptions {
                    attrs: AttrRequest::sanitized(ids),
                };
                let catalog = p.attrs().to_vec();

                // stat_with over a file of the canonical tree.
                let probe = child(&child(&root, b"docs"), b"hello.txt");
                let e = p.stat_with(&probe, &opt).await.expect("stat_with");
                assert_attrs_contract(&catalog, &opt.attrs, &e);

                // list_with over the root: EVERY entry complies.
                let mut stream = p.list_with(&root, &opt).await.expect("list_with");
                let mut n = 0usize;
                while let Some(e) = stream.next().await {
                    let e = e.expect("listing entry");
                    assert_attrs_contract(&catalog, &opt.attrs, &e);
                    n += 1;
                }
                assert!(n >= 1, "the canonical listing isn't empty");
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
                assert!(e.attrs.is_empty(), "no request means no attrs");
                let mut stream = p.list_with(&root, &opt).await.expect("list_with");
                while let Some(e) = stream.next().await {
                    assert!(
                        e.expect("entry").attrs.is_empty(),
                        "no request means no attrs"
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
                    .expect("an unknown id is never an error");
                assert!(!e.attrs.contains_key("zz.does-not-exist"));
            }
        }
    };
}
