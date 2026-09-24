//! [`provider_contract!`]: the contract suite EVERY provider must pass
//! (spec §5/§12). A new provider invokes it in its integration tests and
//! inherits ~24 cases: byte-exact roundtrips, collisions, sink
//! transactionality, hostile names and case coherence. Cases that depend
//! on a capability auto-skip if the provider doesn't declare it.

/// Generates [`Provider`](crate::Provider)'s contract suite inside a test
/// module.
///
/// Requirements of the invoking crate (dev-dependencies): `tokio`
/// (features `macros`, `rt`) — the rest arrives via `norte-vfs`'s
/// internal re-exports.
///
/// - `mod`: name of the generated module.
/// - `factory`: expression that builds a FRESH provider (evaluated once
///   per test; tests share no state).
/// - `root`: expression that builds the provider's root `VPath`.
/// - `hostile_names`: `Vec<Vec<u8>>` expression with hostile names to
///   round-trip (usually `norte_testkit::corpus::hostile_names()` mapped
///   to bytes).
///
/// ```ignore
/// // In a provider's integration tests (ignore: avoids the
/// // testkit→vfs dev-dep cycle in the doctest; the real use lives in norte-testkit).
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
            #![allow(
                clippy::redundant_clone,
                reason = "depending on what the caller passes as `factory`/`root`, the clone is redundant or not: an `expect` can't hold across every instantiation"
            )]

            // The `factory`/`root`/`hostile_names` expressions are
            // evaluated in this module: imports the caller's scope.
            #[allow(unused_imports)]
            use super::*;

            use $crate::__private::bytes::Bytes;
            use $crate::__private::futures::StreamExt;
            use $crate::__private::norte_proto::{
                CapabilityFlags, ConflictKind, EntryKind, Error, Segment, VPath,
            };
            use $crate::{AttrRequest, ByteSink, ListOptions, Provider};

            fn seg(bytes: &[u8]) -> Segment {
                Segment::new(bytes.to_vec()).expect("valid contract segment")
            }

            fn child(base: &VPath, name: &[u8]) -> VPath {
                base.join(seg(name))
            }

            async fn write_all<P: Provider>(p: &P, path: &VPath, content: &[u8]) {
                let mut sink = p.write(path).await.expect("write opens");
                sink.write(Bytes::copy_from_slice(content))
                    .await
                    .expect("chunk goes in");
                sink.commit().await.expect("commit publishes");
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

            /// Declarative auto-skip: the case only applies if `flags` is
            /// (or `!flags` isn't) among the provider's capabilities.
            macro_rules! require_caps {
                ($p:expr, has: $flag:expr) => {
                    if !$p.capabilities().flags.contains($flag) {
                        eprintln!("skip: the provider does not declare {:?}", $flag);
                        return;
                    }
                };
                ($p:expr, lacks: $flag:expr) => {
                    if $p.capabilities().flags.contains($flag) {
                        eprintln!("skip: the case requires NOT having {:?}", $flag);
                        return;
                    }
                };
            }

            // ---------- stat ----------

            /// ADR 0054: `capabilities_at` may REFINE the backend's
            /// declaration, not contradict it. What doesn't depend on the
            /// location — the whole backend being read-only — has to come
            /// out the same through both doors, and a `FULL_FOLD` over
            /// something case-sensitive is an impossible answer, not a
            /// refinement.
            #[tokio::test]
            async fn contract_capabilities_at_refines_without_contradicting() {
                let p = $factory;
                let root: VPath = $root;
                let at = p
                    .capabilities_at(&root)
                    .await
                    .expect("capabilities_at answers for the root");
                assert_eq!(
                    at.flags.contains(CapabilityFlags::READ_ONLY),
                    p.capabilities().flags.contains(CapabilityFlags::READ_ONLY),
                    "READ_ONLY belongs to the backend, not the location"
                );
                assert!(
                    !(at.flags.contains(CapabilityFlags::FULL_FOLD)
                        && at.flags.contains(CapabilityFlags::CASE_SENSITIVE)),
                    "FULL_FOLD only makes sense without CASE_SENSITIVE"
                );
                // What belongs to the LOCATION isn't declared without a
                // path. With `CONFINED_WRITES` it isn't cosmetic: it's a
                // confinement promise with a kernel guarantee a caller
                // ACTS on (it skips the `lstat` walk it would otherwise
                // degrade to), and a promise like that can't come from an
                // answer that doesn't know which mount it's talking about.
                for flag in [CapabilityFlags::FULL_FOLD, CapabilityFlags::CONFINED_WRITES] {
                    assert!(
                        !p.capabilities().flags.contains(flag),
                        "{flag:?} belongs to the location: only capabilities_at answers it"
                    );
                }
            }

            /// Asking about something that doesn't exist isn't an error:
            /// `capabilities()` could never fail, and planning toward a
            /// destination that isn't there yet is a mirror's common case.
            #[tokio::test]
            async fn contract_capabilities_at_missing_path_is_not_an_error() {
                let p = $factory;
                let root: VPath = $root;
                p.capabilities_at(&child(&root, b"never-exists"))
                    .await
                    .expect("an absent path is answered the same way");
            }

            #[tokio::test]
            async fn contract_stat_root_is_dir() {
                let p = $factory;
                let root: VPath = $root;
                let e = p.stat(&root).await.expect("the root always exists");
                assert_eq!(e.kind, EntryKind::Dir);
            }

            #[tokio::test]
            async fn contract_stat_missing_is_not_found() {
                let p = $factory;
                let root: VPath = $root;
                assert_eq!(
                    p.stat(&child(&root, b"does-not-exist")).await.unwrap_err(),
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
                let f = child(&root, b"pending");
                let mut sink = p.write(&f).await.unwrap();
                sink.write(Bytes::from_static(b"data")).await.unwrap();
                assert_eq!(
                    p.stat(&f).await.unwrap_err(),
                    Error::NotFound,
                    "the final path doesn't exist until commit"
                );
                sink.commit().await.unwrap();
                assert!(p.stat(&f).await.is_ok());
            }

            #[tokio::test]
            async fn contract_abort_leaves_no_trace() {
                let p = $factory;
                let root: VPath = $root;
                let f = child(&root, b"aborted");
                let mut sink = p.write(&f).await.unwrap();
                sink.write(Bytes::from_static(b"data")).await.unwrap();
                sink.abort().await.expect("abort cleans up");
                assert_eq!(p.stat(&f).await.unwrap_err(), Error::NotFound);
                let names: Vec<Vec<u8>> = p
                    .list(&root)
                    .await
                    .expect("list root")
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
                assert!(
                    !names.contains(&b"aborted".to_vec()),
                    "not a trace of the staging in the listing"
                );
            }

            // ---------- resume (ADR 0012) ----------

            /// `open_resumable` over a destination with NO partial starts
            /// from scratch (`already == 0`) and publishes normally.
            /// Universal contract (the trait's default satisfies it).
            #[tokio::test]
            async fn contract_open_resumable_fresh_starts_at_zero() {
                let p = $factory;
                let root: VPath = $root;
                let f = child(&root, b"resumable-fresh");
                let (mut sink, already) = p.open_resumable(&f).await.expect("open_resumable");
                assert_eq!(already, 0, "no earlier partial, starts from scratch");
                sink.write(Bytes::from_static(b"whole")).await.expect("write");
                sink.commit().await.expect("commit");
                assert_eq!(read_all(&p, &f).await.expect("read"), b"whole");
            }

            /// Capability honesty (phase 10a): `SERVER_COPY` ⟺
            /// `copy_native` handles the file (`Some`); WITHOUT the cap,
            /// `None` — so the engine falls back to streaming instead of
            /// hard-failing where streaming would have copied. "No
            /// surprises" = caps that don't lie (M2's exit criterion).
            #[tokio::test]
            async fn contract_capabilities_server_copy_is_honest() {
                let p = $factory;
                let root: VPath = $root;
                let flags = p.capabilities().flags;
                // READ_ONLY goes through readonly_provider_contract! (doesn't seed).
                if flags.contains($crate::__private::norte_proto::CapabilityFlags::READ_ONLY) {
                    return;
                }
                let from = child(&root, b"cap-src.txt");
                write_all(&p, &from, b"honest").await;
                let to = child(&root, b"cap-dst.txt");
                let native = p.copy_native(&from, &to).await;
                if flags.contains($crate::__private::norte_proto::CapabilityFlags::SERVER_COPY) {
                    assert!(
                        native.is_some(),
                        "declares SERVER_COPY but copy_native returned None"
                    );
                } else {
                    assert!(
                        native.is_none(),
                        "does NOT declare SERVER_COPY but copy_native returned Some"
                    );
                }
            }

            /// `keep` + `open_resumable` RESUMES: the kept bytes are
            /// reported in `already` and the sink appends after them. A
            /// provider without resume (default `keep=abort`) auto-skips:
            /// its second `open_resumable` gives `already==0` and this
            /// test detects that and doesn't demand the impossible.
            #[tokio::test]
            async fn contract_keep_then_resume_continues() {
                let p = $factory;
                let root: VPath = $root;
                let f = child(&root, b"resumable-cont");
                // First stage: writes "hi" and KEEPS (doesn't publish).
                let (mut sink, already) = p.open_resumable(&f).await.expect("open 1");
                assert_eq!(already, 0);
                sink.write(Bytes::from_static(b"hi")).await.expect("write 1");
                sink.keep().await.expect("keep");
                // The final destination does NOT exist yet (keep doesn't publish).
                assert_eq!(p.stat(&f).await.unwrap_err(), Error::NotFound);

                // Second stage: resumes.
                let (mut sink, already) = p.open_resumable(&f).await.expect("open 2");
                if already == 0 {
                    // Provider without resume (keep=abort): recopies the whole thing.
                    eprintln!("skip: the provider does not resume (keep degrades to abort)");
                    sink.write(Bytes::from_static(b"hiworld")).await.expect("w");
                    sink.commit().await.expect("commit");
                    assert_eq!(read_all(&p, &f).await.unwrap(), b"hiworld");
                    return;
                }
                assert_eq!(already, 2, "resumes after the 2 kept bytes");
                sink.write(Bytes::from_static(b"world")).await.expect("write 2");
                sink.commit().await.expect("commit");
                assert_eq!(
                    read_all(&p, &f).await.expect("read"),
                    b"hiworld",
                    "the content is the concatenation of the two stages"
                );
            }

            /// `partial_digest` of the kept staging == SHA-256 of those
            /// bytes (#35). Permissive: a provider without a digest
            /// (default `None`) auto-skips — degrades to Length, correct.
            /// Verifies the staging↔digest invariant across the WHOLE
            /// matrix, not just on Mem.
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
                        // Provider without a staging digest (or without
                        // resume): degrades to Length, acceptable.
                        eprintln!("skip: the provider does not expose partial_digest");
                    }
                    Some(d) => {
                        let expected: [u8; 32] = Sha256::digest(b"digest me").into();
                        assert_eq!(d, expected, "the digest covers the staging's bytes");
                        // A shorter prefix hashes ONLY that prefix.
                        if let Some(d3) = p.partial_digest(&f, 3).await.expect("digest 3") {
                            let e3: [u8; 32] = Sha256::digest(b"dig").into();
                            assert_eq!(d3, e3, "the digest respects `len`");
                        }
                    }
                }
            }

            #[tokio::test]
            async fn contract_write_collision_is_conflict() {
                let p = $factory;
                let root: VPath = $root;
                let f = child(&root, b"taken");
                write_all(&p, &f, b"1").await;
                match p.write(&f).await {
                    Err(Error::Conflict { .. }) => {}
                    Err(e) => panic!("expected Conflict, was {e:?}"),
                    Ok(_) => panic!("expected Conflict, the write opened"),
                }
            }

            #[tokio::test]
            async fn contract_write_missing_parent_is_not_found() {
                let p = $factory;
                let root: VPath = $root;
                let f = child(&child(&root, b"no-dir"), b"f");
                match p.write(&f).await {
                    Err(Error::NotFound) => {}
                    Err(e) => panic!("expected NotFound, was {e:?}"),
                    Ok(_) => panic!("expected NotFound, the write opened"),
                }
            }

            #[tokio::test]
            async fn contract_read_missing_is_not_found() {
                let p = $factory;
                let root: VPath = $root;
                assert_eq!(
                    read_all(&p, &child(&root, b"nothing")).await.unwrap_err(),
                    Error::NotFound
                );
            }

            #[tokio::test]
            async fn contract_read_range_slices_exactly() {
                use $crate::__private::norte_proto::ByteRange;
                let p = $factory;
                let root: VPath = $root;
                let f = child(&root, b"range.bin");
                write_all(&p, &f, b"0123456789").await;

                let mid = ByteRange { offset: 2, len: Some(3) };
                assert_eq!(
                    read_all_range(&p, &f, Some(mid)).await.expect("mid range"),
                    b"234"
                );
                let tail = ByteRange { offset: 8, len: None };
                assert_eq!(
                    read_all_range(&p, &f, Some(tail)).await.expect("until EOF"),
                    b"89"
                );
                let past = ByteRange { offset: 100, len: Some(4) };
                assert_eq!(
                    read_all_range(&p, &f, Some(past)).await.expect("pread past-EOF"),
                    b"",
                    "offset past EOF = empty stream, not an error"
                );
                let excess = ByteRange { offset: 7, len: Some(100) };
                assert_eq!(
                    read_all_range(&p, &f, Some(excess)).await.expect("len trimmed to EOF"),
                    b"789"
                );
            }

            #[tokio::test]
            async fn contract_symlink_roundtrip_bytes() {
                use $crate::SymlinkKind;
                let p = $factory;
                require_caps!(p, has: CapabilityFlags::SYMLINKS);
                let root: VPath = $root;
                let link = child(&root, b"link");
                // Relative target with arbitrary bytes: NEVER interpreted.
                let target: &[u8] = b"destination-that-does-not-exist";
                p.symlink(&link, target, SymlinkKind::File)
                    .await
                    .expect("symlink creates");
                assert_eq!(
                    p.read_link(&link).await.expect("read_link"),
                    target,
                    "target bytes intact"
                );
                let e = p.stat(&link).await.expect("stat of the link");
                assert_eq!(e.kind, EntryKind::Symlink, "describes the LINK");
            }

            /// Hostile targets: the core guarantee is that the target's
            /// BYTES travel intact, never interpreted nor decoded. Live
            /// inline (not in names.json): a target is NOT a Segment — it
            /// allows `/`, `\`, `..`, absolute paths and non-UTF8.
            #[tokio::test]
            async fn contract_symlink_hostile_targets_roundtrip() {
                use $crate::SymlinkKind;
                let p = $factory;
                require_caps!(p, has: CapabilityFlags::SYMLINKS);
                let root: VPath = $root;
                let hostile_targets: &[(&str, &[u8])] = &[
                    ("latin1", b"caf\xE9"),
                    ("relative_deep", b"sub/dir/f"),
                    ("absolute", b"/etc/hostname"),
                    ("dotdot", b"../outside"),
                    ("windows_style", b"C:\\Users\\x"),
                    ("lone_surrogate", &[0xED, 0xA0, 0x80]),
                    ("not_wtf8", &[0xFF, 0xFE]),
                ];
                for (i, (id, target)) in hostile_targets.iter().enumerate() {
                    let link = child(&root, format!("ln{i}").as_bytes());
                    match p.symlink(&link, target, SymlinkKind::File).await {
                        Ok(()) => {
                            assert_eq!(
                                p.read_link(&link).await.expect("read_link"),
                                *target,
                                "target bytes intact: {id}"
                            );
                        }
                        // The OS may reject the target (Windows requires
                        // WTF-8): a CLEAN rejection, never lossy nor a panic.
                        Err(Error::InvalidPath) => {
                            eprintln!("skip target {id}: clean OS rejection");
                        }
                        Err(e) => panic!("symlink with target {id}: {e:?}"),
                    }
                }
            }

            #[tokio::test]
            async fn contract_symlink_over_existing_is_conflict() {
                use $crate::SymlinkKind;
                let p = $factory;
                require_caps!(p, has: CapabilityFlags::SYMLINKS);
                let root: VPath = $root;
                let f = child(&root, b"taken");
                write_all(&p, &f, b"x").await;
                match p.symlink(&f, b"target", SymlinkKind::File).await {
                    Err(Error::Conflict { .. }) => {}
                    other => panic!("expected Conflict, was {other:?}"),
                }
                assert_eq!(read_all(&p, &f).await.expect("intact"), b"x");
            }

            /// ADR 0009: with the TRASH capability, `trash()` takes the
            /// WHOLE tree with it and the path stops existing; without
            /// the capability, Unsupported (never delete in its place).
            #[tokio::test]
            async fn contract_trash_takes_the_tree_or_refuses() {
                let p = $factory;
                let root: VPath = $root;
                // A UNIQUE, recognizable name: the developer's real trash
                // accumulates these — the purge lives in the local
                // provider's tests (os_limited); macOS is accepted and
                // documented in ADR 0009.
                let name = format!("norte-contract-trash-{}", std::process::id());
                let dir = child(&root, name.as_bytes());
                p.mkdir(&dir).await.expect("mkdir");
                write_all(&p, &child(&dir, b"kid"), b"x").await;
                // And a victim with a HOSTILE name (non-UTF8): the trash
                // on all 3 OSes must swallow it or cleanly reject it,
                // never panic. (If the FS rejects the name — APFS — it
                // simply isn't there.)
                let hostile_seeded = if let Ok(mut sink) =
                    p.write(&child(&dir, b"tr\xE1sh")).await
                {
                    let _ = sink.write(Bytes::from_static(b"x")).await;
                    sink.commit().await.is_ok()
                } else {
                    false
                };
                if p.capabilities().flags.contains(CapabilityFlags::TRASH) {
                    let dest = p.trash(&dir, &$crate::trash::TrashId::new(0, 0))
                        .await
                        .expect("trash");
                    assert_eq!(
                        p.stat(&dir).await.expect_err("gone"),
                        Error::NotFound
                    );
                    // The recoverable destination and what the provider
                    // PROMISES about it can't disagree: without this
                    // pairing, a provider that answers `None` leaves undo
                    // matching by original path, which over a
                    // `trashed`+`created` pair digs up the wrong file and
                    // calls it a success.
                    assert_eq!(
                        dest.is_some(),
                        p.trash_restorable(),
                        "a trash either promises to name its destination and does, or doesn't promise at all"
                    );
                    if let Some(dest) = dest {
                        p.stat(&dest).await.expect("the recoverable destination EXISTS");
                        // And it restores EXACTLY: back to its path,
                        // without guessing which trash item it was.
                        p.restore_from(&dest, &dir).await.expect("restore_from");
                        assert!(
                            p.stat(&dir).await.is_ok(),
                            "restored from the destination the provider itself gave"
                        );
                        // And the EXACT NODE comes back, not a directory
                        // with its name (#168). A restore that recreates
                        // the folder and loses what's inside passes the
                        // `stat` above and is exactly how an undo says
                        // "done" over data that's no longer there.
                        assert_eq!(
                            read_all(&p, &child(&dir, b"kid"))
                                .await
                                .expect("the restored kid reads back"),
                            b"x".to_vec(),
                            "the kid's content comes back byte for byte"
                        );
                        // Including the NON-UTF8 name, if the FS accepted
                        // it when it was seeded: it's the one that gets
                        // lost when a provider rebuilds paths from text
                        // instead of bytes.
                        let hostile = child(&dir, b"tr\xE1sh");
                        if hostile_seeded {
                            p.stat(&hostile)
                                .await
                                .expect("the non-UTF8 name comes back with its bytes");
                        }
                    }
                } else {
                    assert!(matches!(
                        p.trash(&dir, &$crate::trash::TrashId::new(0, 0)).await,
                        Err(Error::Unsupported)
                    ));
                    assert!(p.stat(&dir).await.is_ok(), "without a trash NOTHING is touched");
                }
            }

            #[tokio::test]
            async fn contract_read_link_errors() {
                let p = $factory;
                let root: VPath = $root;
                // Nonexistent: NotFound (or Unsupported if the provider
                // doesn't know about symlinks at all).
                match p.read_link(&child(&root, b"does-not-exist")).await {
                    Err(Error::NotFound | Error::Unsupported) => {}
                    other => panic!("expected NotFound/Unsupported, was {other:?}"),
                }
                // Normal file: TypeMismatch (or Unsupported).
                let f = child(&root, b"normal");
                write_all(&p, &f, b"x").await;
                match p.read_link(&f).await {
                    Err(
                        Error::Conflict {
                            conflict: ConflictKind::TypeMismatch,
                        }
                        | Error::Unsupported,
                    ) => {}
                    other => panic!("expected TypeMismatch/Unsupported, was {other:?}"),
                }
            }

            #[tokio::test]
            async fn contract_read_dir_errors() {
                let p = $factory;
                let root: VPath = $root;
                let d = child(&root, b"dir");
                p.mkdir(&d).await.unwrap();
                assert!(read_all(&p, &d).await.is_err(), "reading a dir is an error");
            }

            // ---------- node_id (real identity, issue #16) ----------

            /// A provider WITH identity: stable between calls and
            /// distinct between distinct nodes. A provider without
            /// identity (Ok(None)) auto-skips — the trait's default is legal.
            #[tokio::test]
            async fn contract_node_id_stable_and_distinct() {
                use $crate::FollowLinks;
                let p = $factory;
                let root: VPath = $root;
                let a_path = child(&root, b"ida");
                let b_path = child(&root, b"idb");
                write_all(&p, &a_path, b"x").await;
                write_all(&p, &b_path, b"y").await;
                let Some(id_a) = p.node_id(&a_path, FollowLinks::No).await.expect("node_id a") else {
                    eprintln!("skip: the provider does not expose node identity");
                    return;
                };
                let id_a2 = p
                    .node_id(&a_path, FollowLinks::No)
                    .await
                    .expect("node_id a, 2nd")
                    .expect("with identity: always or never");
                assert_eq!(id_a, id_a2, "stable between calls");
                let id_b = p
                    .node_id(&b_path, FollowLinks::No)
                    .await
                    .expect("node_id b")
                    .expect("with identity: always or never");
                assert_ne!(id_a, id_b, "distinct nodes, distinct identities");
                // On a node that isn't a symlink, follow changes nothing.
                let id_a3 = p
                    .node_id(&a_path, FollowLinks::Yes)
                    .await
                    .expect("node_id a, follow")
                    .expect("with identity: always or never");
                assert_eq!(id_a, id_a3, "follow over a non-symlink is the same");
            }

            /// Identity survives the rename: it belongs to the NODE, not
            /// the path. (It's the basis of the engine's rename_retrying,
            /// issue #17.)
            #[tokio::test]
            async fn contract_node_id_survives_rename() {
                use $crate::FollowLinks;
                let p = $factory;
                let root: VPath = $root;
                let before = child(&root, b"id-before");
                write_all(&p, &before, b"x").await;
                let Some(id) = p.node_id(&before, FollowLinks::No).await.expect("node_id") else {
                    eprintln!("skip: the provider does not expose node identity");
                    return;
                };
                let after = child(&root, b"id-after");
                p.rename(&before, &after).await.expect("rename");
                let id2 = p
                    .node_id(&after, FollowLinks::No)
                    .await
                    .expect("node_id after rename")
                    .expect("with identity: always or never");
                assert_eq!(id, id2, "the rename moves the node, doesn't recreate it");
            }

            /// `FollowLinks::Yes` resolves the symlink (the TARGET's
            /// identity); `No` gives the link's own identity (lstat semantics).
            #[tokio::test]
            async fn contract_node_id_follow_resolves_link() {
                use $crate::{FollowLinks, SymlinkKind};
                let p = $factory;
                require_caps!(p, has: CapabilityFlags::SYMLINKS);
                let root: VPath = $root;
                let f = child(&root, b"idf");
                write_all(&p, &f, b"x").await;
                let link = child(&root, b"idlink");
                // Target relative to the link's parent: resolvable on any
                // provider (same convention as Mem's minimal resolve).
                p.symlink(&link, b"idf", SymlinkKind::File)
                    .await
                    .expect("symlink creates");
                let Some(target_id) = p.node_id(&f, FollowLinks::No).await.expect("target's id")
                else {
                    eprintln!("skip: the provider does not expose node identity");
                    return;
                };
                let followed = p
                    .node_id(&link, FollowLinks::Yes)
                    .await
                    .expect("node_id follow")
                    .expect("with identity: always or never");
                assert_eq!(followed, target_id, "Yes = the target's identity");
                let own = p
                    .node_id(&link, FollowLinks::No)
                    .await
                    .expect("node_id of the link")
                    .expect("with identity: always or never");
                assert_ne!(own, target_id, "No = the link's own identity");
            }

            /// Broken symlink: `Yes` is NotFound (no target); `No` still
            /// works (the link exists). Nonexistent path: NotFound in
            /// both modes (or Ok(None) if the provider knows nothing of identity).
            #[tokio::test]
            async fn contract_node_id_broken_and_missing() {
                use $crate::{FollowLinks, SymlinkKind};
                let p = $factory;
                let root: VPath = $root;
                match p.node_id(&child(&root, b"does-not-exist"), FollowLinks::No).await {
                    Ok(None) | Err(Error::NotFound) => {}
                    other => panic!("expected NotFound/Ok(None), was {other:?}"),
                }
                require_caps!(p, has: CapabilityFlags::SYMLINKS);
                let broken = child(&root, b"id-broken");
                p.symlink(&broken, b"nothing-here", SymlinkKind::File)
                    .await
                    .expect("symlink creates");
                match p.node_id(&broken, FollowLinks::No).await {
                    Ok(_) => {}
                    other => panic!("the link EXISTS even though it's broken, was {other:?}"),
                }
                match p.node_id(&broken, FollowLinks::Yes).await {
                    Err(Error::NotFound) => {}
                    // Without identity, None is also legal for a broken link.
                    Ok(None) => {}
                    other => panic!("expected NotFound when following a broken link, was {other:?}"),
                }
            }

            /// GUARANTEE of the trait: `remove` over a symlink deletes
            /// THE LINK, never its target (unlink semantics). The copy
            /// engine's move relies on this so it doesn't destroy targets
            /// when deleting expanded links (issue #19).
            #[tokio::test]
            async fn contract_remove_symlink_leaves_target_intact() {
                use $crate::SymlinkKind;
                let p = $factory;
                require_caps!(p, has: CapabilityFlags::SYMLINKS);
                let root: VPath = $root;
                let dir = child(&root, b"rmtarget");
                p.mkdir(&dir).await.expect("mkdir");
                write_all(&p, &child(&dir, b"kid"), b"alive").await;
                let link = child(&root, b"rmlink");
                p.symlink(&link, b"rmtarget", SymlinkKind::Dir)
                    .await
                    .expect("symlink creates");
                p.remove(&link).await.expect("remove of the LINK");
                assert_eq!(
                    p.stat(&link).await.expect_err("the link is gone"),
                    Error::NotFound
                );
                let e = p.stat(&dir).await.expect("the target LIVES");
                assert_eq!(e.kind, EntryKind::Dir);
                assert_eq!(
                    read_all(&p, &child(&dir, b"kid")).await.expect("content intact"),
                    b"alive"
                );
            }

            /// Identity works the same with HOSTILE names (non-UTF8
            /// bytes). If the FS rejects the name (APFS requires UTF-8),
            /// a clean rejection and skip — like the rest of the hostile corpus.
            #[tokio::test]
            async fn contract_node_id_with_hostile_name() {
                use $crate::FollowLinks;
                let p = $factory;
                let root: VPath = $root;
                let name: &[u8] = b"id-\xE9-latin1";
                let Ok(seg) = $crate::__private::norte_proto::Segment::new(name.to_vec()) else {
                    panic!("valid hostile segment");
                };
                let path = root.join(seg);
                let mut sink = match p.write(&path).await {
                    Ok(s) => s,
                    Err(Error::InvalidPath) => {
                        eprintln!("skip: the FS rejects the name (cleanly)");
                        return;
                    }
                    Err(e) => panic!("hostile write: {e:?}"),
                };
                sink.write(Bytes::from_static(b"x")).await.expect("chunk");
                if let Err(Error::InvalidPath) = sink.commit().await {
                    eprintln!("skip: the FS rejects the name at commit (cleanly)");
                    return;
                }
                let Some(id) = p.node_id(&path, FollowLinks::No).await.expect("node_id") else {
                    eprintln!("skip: the provider does not expose node identity");
                    return;
                };
                let id2 = p
                    .node_id(&path, FollowLinks::No)
                    .await
                    .expect("node_id, 2nd")
                    .expect("with identity: always or never");
                assert_eq!(id, id2, "stable identity with non-UTF8 bytes");
            }

            // ---------- mkdir / list ----------

            #[tokio::test]
            async fn contract_mkdir_and_stat() {
                let p = $factory;
                let root: VPath = $root;
                let d = child(&root, b"new");
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
                    other => panic!("expected Conflict, was {other:?}"),
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
                write_all(&p, &child(&root, b"one"), b"1").await;
                p.mkdir(&child(&root, b"two")).await.unwrap();
                let mut names: Vec<Vec<u8>> = p
                    .list(&root)
                    .await
                    .unwrap()
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
                assert_eq!(names, vec![b"one".to_vec(), b"two".to_vec()]);
            }

            #[tokio::test]
            async fn contract_list_missing_is_not_found() {
                let p = $factory;
                let root: VPath = $root;
                assert!(p.list(&child(&root, b"nothing")).await.is_err());
            }

            // ---------- remove ----------

            #[tokio::test]
            async fn contract_remove_file() {
                let p = $factory;
                let root: VPath = $root;
                let f = child(&root, b"removable");
                write_all(&p, &f, b"x").await;
                p.remove(&f).await.unwrap();
                assert_eq!(p.stat(&f).await.unwrap_err(), Error::NotFound);
            }

            #[tokio::test]
            async fn contract_remove_missing_is_not_found() {
                let p = $factory;
                let root: VPath = $root;
                assert_eq!(
                    p.remove(&child(&root, b"nothing")).await.unwrap_err(),
                    Error::NotFound
                );
            }

            #[tokio::test]
            async fn contract_remove_empty_dir() {
                let p = $factory;
                let root: VPath = $root;
                let d = child(&root, b"empty");
                p.mkdir(&d).await.unwrap();
                p.remove(&d).await.unwrap();
                assert_eq!(p.stat(&d).await.unwrap_err(), Error::NotFound);
            }

            #[tokio::test]
            async fn contract_remove_nonempty_dir_refused() {
                let p = $factory;
                let root: VPath = $root;
                let d = child(&root, b"full");
                p.mkdir(&d).await.unwrap();
                write_all(&p, &child(&d, b"f"), b"x").await;
                assert!(
                    p.remove(&d).await.is_err(),
                    "non-recursive remove: a dir with children is refused"
                );
                assert!(p.stat(&d).await.is_ok(), "and the dir is still there");
            }

            // ---------- rename ----------

            #[tokio::test]
            async fn contract_rename_file_moves_bytes() {
                let p = $factory;
                let root: VPath = $root;
                let a = child(&root, b"a");
                let b = child(&root, b"b");
                write_all(&p, &a, b"content").await;
                p.rename(&a, &b).await.unwrap();
                assert_eq!(p.stat(&a).await.unwrap_err(), Error::NotFound);
                assert_eq!(read_all(&p, &b).await.unwrap(), b"content");
            }

            #[tokio::test]
            async fn contract_rename_missing_is_not_found() {
                let p = $factory;
                let root: VPath = $root;
                assert_eq!(
                    p.rename(&child(&root, b"no"), &child(&root, b"there"))
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
                    other => panic!("expected Conflict, was {other:?}"),
                }
                assert_eq!(read_all(&p, &b).await.unwrap(), b"2", "the destination intact");
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

            // ---------- hostile names ----------

            #[tokio::test]
            async fn contract_hostile_names_roundtrip() {
                let p = $factory;
                let root: VPath = $root;
                let names: Vec<Vec<u8>> = $hostile;
                assert!(!names.is_empty(), "the corpus can't be empty");
                let mut created = 0usize;
                for bytes in names {
                    let f = child(&root, &bytes);
                    // The OS may reject the name (APFS requires UTF-8,
                    // NTFS forbids controls): a clean rejection = skip.
                    // The contractual guarantee is: IF it gets created,
                    // the bytes come back intact.
                    let mut sink = match p.write(&f).await {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("skip {}: {e}", f.display_lossy());
                            continue;
                        }
                    };
                    sink.write(Bytes::from_static(b"x")).await.expect("chunk");
                    // Some providers publish at commit (rename/put): the
                    // OS's rejection can also arrive here.
                    match sink.commit().await {
                        Ok(()) => {}
                        Err(Error::InvalidPath | Error::Conflict { .. }) => {
                            eprintln!("skip (commit) {}", f.display_lossy());
                            continue;
                        }
                        Err(e) => panic!("commit of {}: {e:?}", f.display_lossy()),
                    }
                    created += 1;
                    let e = p.stat(&f).await.unwrap_or_else(|err| {
                        panic!("stat of {:?} after commit: {err:?}", f.display_lossy())
                    });
                    assert_eq!(
                        e.path.file_name().expect("has a name").as_bytes(),
                        bytes.as_slice(),
                        "bytes intact for {}",
                        f.display_lossy()
                    );
                    // The REAL roundtrip test: the bytes the BACKEND
                    // returns when listing (not the input path's echo).
                    let listed: Vec<Vec<u8>> = p
                        .list(&root)
                        .await
                        .expect("list root")
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
                    let exact = listed.iter().filter(|n| n.as_slice() == bytes.as_slice()).count();
                    assert_eq!(
                        exact,
                        1,
                        "the backend must return the EXACT bytes exactly once for {}",
                        f.display_lossy()
                    );
                    assert_eq!(read_all(&p, &f).await.unwrap(), b"x");
                }
                assert!(created > 0, "not a single hostile name could be created: suspicious");
            }

            #[tokio::test]
            async fn contract_write_never_touches_siblings() {
                let p = $factory;
                let root: VPath = $root;
                // A REAL user file whose name matches a possible staging
                // scheme: writing the neighbor never touches it.
                let sibling = child(&root, b"x.norte-partial");
                write_all(&p, &sibling, b"real user content").await;
                let target = child(&root, b"x");
                write_all(&p, &target, b"new").await;
                assert_eq!(
                    read_all(&p, &sibling).await.unwrap(),
                    b"real user content",
                    "the staging overwrote a real file"
                );
                assert_eq!(read_all(&p, &target).await.unwrap(), b"new");
            }

            // ---------- conditional capabilities ----------

            #[tokio::test]
            async fn contract_rename_case_variant_when_sensitive_is_conflict() {
                let p = $factory;
                require_caps!(p, has: CapabilityFlags::CASE_SENSITIVE);
                let root: VPath = $root;
                write_all(&p, &child(&root, b"box"), b"1").await;
                write_all(&p, &child(&root, b"BOX"), b"2").await;
                // On a case-sensitive FS these are DISTINCT files: rename
                // between case variants is Conflict, never a silent replacement.
                match p.rename(&child(&root, b"box"), &child(&root, b"BOX")).await {
                    Err(Error::Conflict { .. }) => {}
                    other => panic!("expected Conflict, was {other:?}"),
                }
                assert_eq!(read_all(&p, &child(&root, b"box")).await.unwrap(), b"1");
                assert_eq!(read_all(&p, &child(&root, b"BOX")).await.unwrap(), b"2");
            }

            #[tokio::test]
            async fn contract_case_collision_when_insensitive() {
                let p = $factory;
                require_caps!(p, lacks: CapabilityFlags::CASE_SENSITIVE);
                let root: VPath = $root;
                write_all(&p, &child(&root, b"Same"), b"1").await;
                match p.write(&child(&root, b"same")).await {
                    Err(Error::Conflict {
                        conflict: ConflictKind::CaseCollision,
                    }) => {}
                    Err(e) => panic!("expected CaseCollision, was {e:?}"),
                    Ok(_) => panic!("expected CaseCollision, the write opened"),
                }
            }

            #[tokio::test]
            async fn contract_case_distinct_when_sensitive() {
                let p = $factory;
                require_caps!(p, has: CapabilityFlags::CASE_SENSITIVE);
                let root: VPath = $root;
                write_all(&p, &child(&root, b"Box"), b"1").await;
                write_all(&p, &child(&root, b"box"), b"2").await;
                assert_eq!(read_all(&p, &child(&root, b"Box")).await.unwrap(), b"1");
                assert_eq!(read_all(&p, &child(&root, b"box")).await.unwrap(), b"2");
            }

            #[tokio::test]
            async fn contract_copy_native_iff_capability() {
                let p = $factory;
                let root: VPath = $root;
                let a = child(&root, b"orig");
                let b = child(&root, b"copy");
                write_all(&p, &a, b"bytes").await;
                let declared = p
                    .capabilities()
                    .flags
                    .contains(CapabilityFlags::SERVER_COPY);
                match p.copy_native(&a, &b).await {
                    None => assert!(!declared, "SERVER_COPY declared but copy_native = None"),
                    Some(res) => {
                        assert!(declared, "copy_native without declaring SERVER_COPY");
                        res.expect("native copy ok");
                        assert_eq!(read_all(&p, &b).await.unwrap(), b"bytes");
                        // Existing destination: Conflict, never a silent
                        // overwrite (same policy as write).
                        match p.copy_native(&a, &b).await {
                            Some(Err(Error::Conflict { .. })) => {}
                            other => panic!(
                                "copy_native over an existing destination should have given Conflict, was {other:?}"
                            ),
                        }
                    }
                }
            }

            // ---------- attrs (#108 block 2, ADR 0039) ----------
            // Assertions shared with the read-only suite:
            // `__private::contract_attrs` (a divergence would silently weaken a suite).
            use $crate::__private::contract_attrs::{assert_attrs_contract, assert_catalog_sane};

            #[tokio::test]
            async fn contract_attrs_catalog_is_sane() {
                let p = $factory;
                assert_catalog_sane(p.attrs());
            }

            #[tokio::test]
            async fn contract_attrs_values_match_declared_types() {
                let p = $factory;
                if p.attrs().is_empty() {
                    eprintln!("skip: empty attrs catalogue");
                    return;
                }
                let root: VPath = $root;
                write_all(&p, &child(&root, b"attrs-probe.txt"), b"test content").await;
                let ids: Vec<String> = p.attrs().iter().map(|a| a.id.clone()).collect();
                let opt = ListOptions {
                    attrs: AttrRequest::sanitized(ids),
                };
                let catalog = p.attrs().to_vec();

                // stat_with over the seeded file.
                let e = p
                    .stat_with(&child(&root, b"attrs-probe.txt"), &opt)
                    .await
                    .expect("stat_with");
                assert_attrs_contract(&catalog, &opt.attrs, &e);

                // list_with over the root: EVERY entry complies.
                let mut stream = p.list_with(&root, &opt).await.expect("list_with");
                let mut n = 0usize;
                while let Some(e) = stream.next().await {
                    let e = e.expect("listing entry");
                    assert_attrs_contract(&catalog, &opt.attrs, &e);
                    n += 1;
                }
                assert!(n >= 1, "the listing must contain the probe");
            }

            #[tokio::test]
            async fn contract_attrs_empty_request_yields_bare_entries() {
                let p = $factory;
                let root: VPath = $root;
                write_all(&p, &child(&root, b"attrs-bare.txt"), b"x").await;
                let opt = ListOptions::default();
                let e = p
                    .stat_with(&child(&root, b"attrs-bare.txt"), &opt)
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
            async fn contract_attrs_unknown_requested_id_is_absent_not_error() {
                let p = $factory;
                let root: VPath = $root;
                write_all(&p, &child(&root, b"attrs-unk.txt"), b"x").await;
                let opt = ListOptions {
                    attrs: AttrRequest::sanitized(["zz.does-not-exist".to_owned()]),
                };
                let e = p
                    .stat_with(&child(&root, b"attrs-unk.txt"), &opt)
                    .await
                    .expect("an unknown id is never an error");
                assert!(!e.attrs.contains_key("zz.does-not-exist"));
            }
        }
    };
}
