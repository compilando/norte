//! `MemProvider` tests: contract semantics (collisions, case, subtree
//! rename, trace-free abort) and fault injection (exact byte, disconnection,
//! deterministic latency with a paused clock).

use bytes::Bytes;
use futures::StreamExt;
use norte_proto::{CapabilityFlags, ConflictKind, EntryKind, Error, Segment, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid test wire")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write opens");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk goes in");
    sink.commit().await.expect("commit publishes");
}

async fn read_all(mem: &MemProvider, wire: &str) -> Result<Vec<u8>, Error> {
    let mut stream = mem.read(&vp(wire), None).await?;
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk?);
    }
    Ok(out)
}

// ---------- roundtrip and visibility ----------

#[tokio::test]
async fn write_commit_read_roundtrip() {
    let mem = MemProvider::new();
    write_file(&mem, "mem:///f.txt", b"hola").await;
    assert_eq!(read_all(&mem, "mem:///f.txt").await.unwrap(), b"hola");
    let e = mem.stat(&vp("mem:///f.txt")).await.unwrap();
    assert_eq!(e.kind, EntryKind::File);
    assert_eq!(e.size, Some(4));
}

#[tokio::test]
async fn nothing_visible_before_commit() {
    let mem = MemProvider::new();
    let mut sink = mem.write(&vp("mem:///f")).await.unwrap();
    sink.write(Bytes::from_static(b"data")).await.unwrap();
    // No commit: the final path does not exist.
    assert_eq!(
        mem.stat(&vp("mem:///f")).await.unwrap_err(),
        Error::NotFound
    );
    sink.commit().await.unwrap();
    assert!(mem.stat(&vp("mem:///f")).await.is_ok());
}

#[tokio::test]
async fn abort_leaves_no_trace() {
    let mem = MemProvider::new();
    let mut sink = mem.write(&vp("mem:///f")).await.unwrap();
    sink.write(Bytes::from_static(b"data")).await.unwrap();
    sink.abort().await.unwrap();
    assert_eq!(
        mem.stat(&vp("mem:///f")).await.unwrap_err(),
        Error::NotFound
    );
    let mut list = mem.list(&vp("mem:///")).await.unwrap();
    assert!(list.next().await.is_none(), "empty root after abort");
}

#[tokio::test]
async fn hostile_names_roundtrip_byte_exact() {
    let mem = MemProvider::new();
    let root = MemProvider::root();
    for name in norte_testkit::corpus::hostile_names() {
        let seg = norte_proto::Segment::new(name.bytes.clone()).expect("valid segment");
        let path = root.join(seg);
        let mut sink = mem.write(&path).await.expect("write opens");
        sink.write(Bytes::from_static(b"x")).await.unwrap();
        sink.commit().await.unwrap();
        let e = mem.stat(&path).await.unwrap_or_else(|err| {
            panic!("[{}] stat after commit: {err:?}", name.id);
        });
        assert_eq!(
            e.path.file_name().unwrap().as_bytes(),
            name.bytes.as_slice(),
            "[{}] intact bytes",
            name.id
        );
    }
}

// ---------- collisions and case ----------

#[tokio::test]
async fn write_collision_is_conflict() {
    let mem = MemProvider::new();
    write_file(&mem, "mem:///f", b"1").await;
    assert_eq!(
        mem.write(&vp("mem:///f"))
            .await
            .err()
            .map(|e| format!("{e:?}")),
        Some("Conflict { conflict: Exists }".to_owned())
    );
}

#[tokio::test]
async fn case_insensitive_collision_detected() {
    let mem =
        MemProvider::with_flags(CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING);
    write_file(&mem, "mem:///File", b"1").await;
    // On a case-insensitive FS, "file" collides with "File".
    match mem.write(&vp("mem:///file")).await {
        Err(Error::Conflict { conflict }) => assert_eq!(conflict, ConflictKind::CaseCollision),
        Err(e) => panic!("expected CaseCollision, was {e:?}"),
        Ok(_) => panic!("expected CaseCollision, the write opened"),
    }
    // And it resolves to the same node when read.
    assert_eq!(read_all(&mem, "mem:///file").await.unwrap(), b"1");
}

#[tokio::test]
async fn case_sensitive_no_collision() {
    let mem = MemProvider::new(); // CASE_SENSITIVE
    write_file(&mem, "mem:///File", b"1").await;
    write_file(&mem, "mem:///file", b"2").await;
    assert_eq!(read_all(&mem, "mem:///File").await.unwrap(), b"1");
    assert_eq!(read_all(&mem, "mem:///file").await.unwrap(), b"2");
}

#[tokio::test]
async fn case_change_rename_allowed_when_insensitive() {
    let mem = MemProvider::with_flags(CapabilityFlags::CASE_PRESERVING);
    write_file(&mem, "mem:///readme", b"x").await;
    // a → A over the same node: a case rename, allowed.
    mem.rename(&vp("mem:///readme"), &vp("mem:///README"))
        .await
        .expect("case change allowed");
    let e = mem.stat(&vp("mem:///README")).await.unwrap();
    assert_eq!(e.path.file_name().unwrap().as_bytes(), b"README");
}

// ---------- structure ----------

#[tokio::test]
async fn mkdir_requires_parent() {
    let mem = MemProvider::new();
    assert_eq!(
        mem.mkdir(&vp("mem:///a/b")).await.unwrap_err(),
        Error::NotFound
    );
    mem.mkdir(&vp("mem:///a")).await.unwrap();
    mem.mkdir(&vp("mem:///a/b")).await.unwrap();
    let e = mem.stat(&vp("mem:///a/b")).await.unwrap();
    assert_eq!(e.kind, EntryKind::Dir);
}

#[tokio::test]
async fn remove_nonempty_dir_refused() {
    let mem = MemProvider::new();
    mem.mkdir(&vp("mem:///d")).await.unwrap();
    write_file(&mem, "mem:///d/f", b"x").await;
    assert!(mem.remove(&vp("mem:///d")).await.is_err());
    mem.remove(&vp("mem:///d/f")).await.unwrap();
    mem.remove(&vp("mem:///d")).await.unwrap();
    assert_eq!(
        mem.stat(&vp("mem:///d")).await.unwrap_err(),
        Error::NotFound
    );
}

#[tokio::test]
async fn rename_moves_subtree() {
    let mem = MemProvider::new();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    mem.mkdir(&vp("mem:///src/sub")).await.unwrap();
    write_file(&mem, "mem:///src/sub/f", b"x").await;
    mem.rename(&vp("mem:///src"), &vp("mem:///dst"))
        .await
        .unwrap();
    assert_eq!(
        mem.stat(&vp("mem:///src")).await.unwrap_err(),
        Error::NotFound
    );
    assert_eq!(read_all(&mem, "mem:///dst/sub/f").await.unwrap(), b"x");
}

#[tokio::test]
async fn list_is_deterministic_byte_order() {
    let mem = MemProvider::new();
    for name in ["b", "a", "c"] {
        write_file(&mem, &format!("mem:///{name}"), b"x").await;
    }
    let names: Vec<Vec<u8>> = mem
        .list(&vp("mem:///"))
        .await
        .unwrap()
        .map(|e| e.unwrap().path.file_name().unwrap().as_bytes().to_vec())
        .collect()
        .await;
    assert_eq!(names, vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);
}

#[tokio::test]
async fn logical_clock_makes_mtimes_deterministic() {
    let run = || async {
        let mem = MemProvider::new();
        write_file(&mem, "mem:///a", b"1").await;
        write_file(&mem, "mem:///b", b"2").await;
        (
            mem.stat(&vp("mem:///a")).await.unwrap().mtime_ms,
            mem.stat(&vp("mem:///b")).await.unwrap().mtime_ms,
        )
    };
    assert_eq!(run().await, run().await, "two identical runs");
}

// ---------- copy_native ----------

#[tokio::test]
async fn copy_native_none_without_capability() {
    let mem = MemProvider::new();
    write_file(&mem, "mem:///a", b"x").await;
    assert!(
        mem.copy_native(&vp("mem:///a"), &vp("mem:///b"))
            .await
            .is_none()
    );
}

#[tokio::test]
async fn copy_native_copies_when_declared() {
    let mem =
        MemProvider::with_flags(CapabilityFlags::SERVER_COPY | CapabilityFlags::CASE_SENSITIVE);
    write_file(&mem, "mem:///a", b"contenido").await;
    mem.copy_native(&vp("mem:///a"), &vp("mem:///b"))
        .await
        .expect("SERVER_COPY declared")
        .expect("copy ok");
    assert_eq!(read_all(&mem, "mem:///b").await.unwrap(), b"contenido");
}

// ---------- fault injection ----------

#[tokio::test]
async fn fail_read_at_exact_byte() {
    let mem = MemProvider::new();
    let content = vec![0xAB; 5000];
    write_file(&mem, "mem:///big", &content).await;
    mem.faults().fail_read_at(&vp("mem:///big"), 3000);

    let mut stream = mem.read(&vp("mem:///big"), None).await.unwrap();
    let mut got = Vec::new();
    let mut err = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(chunk) => got.extend_from_slice(&chunk),
            Err(e) => {
                err = Some(e);
                break;
            }
        }
    }
    assert!(got.len() < 3000 + 1, "never delivers more than N bytes");
    assert_eq!(err, Some(Error::Io { retryable: false }));
}

#[tokio::test]
async fn fail_write_at_exact_byte() {
    let mem = MemProvider::new();
    mem.faults().fail_write_at(&vp("mem:///out"), 100);
    let mut sink = mem.write(&vp("mem:///out")).await.unwrap();
    let err = sink.write(Bytes::from(vec![0u8; 200])).await.unwrap_err();
    assert_eq!(err, Error::Io { retryable: false });
    // After the failure, abort cleans up and leaves no trace.
    sink.abort().await.unwrap();
    assert_eq!(
        mem.stat(&vp("mem:///out")).await.unwrap_err(),
        Error::NotFound
    );
}

#[tokio::test]
async fn disconnect_after_n_ops() {
    let mem = MemProvider::new();
    write_file(&mem, "mem:///f", b"x").await;
    mem.faults().disconnect_after(2);
    assert!(mem.stat(&vp("mem:///f")).await.is_ok()); // op 1
    assert!(mem.stat(&vp("mem:///f")).await.is_ok()); // op 2
    assert_eq!(
        mem.stat(&vp("mem:///f")).await.unwrap_err(),
        Error::ProviderUnavailable { retryable: true }
    );
    // And it stays disconnected.
    assert!(mem.list(&vp("mem:///")).await.is_err());
    mem.faults().clear();
    assert!(mem.stat(&vp("mem:///f")).await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn latency_uses_tokio_clock() {
    let mem = MemProvider::new();
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(500)));
    let before = tokio::time::Instant::now();
    // With a paused clock, tokio advances virtual time automatically.
    let _ = mem.stat(&vp("mem:///nope")).await;
    assert!(before.elapsed() >= std::time::Duration::from_millis(500));
}

// ---------- case coherence (encoding-auditor findings, phase 6) ----------

#[tokio::test]
async fn list_resolves_case_insensitively() {
    let mem = MemProvider::with_flags(CapabilityFlags::CASE_PRESERVING);
    mem.mkdir(&vp("mem:///Dir")).await.unwrap();
    write_file(&mem, "mem:///Dir/f", b"x").await;
    // list with different case must see the same thing as stat.
    let n = mem
        .list(&vp("mem:///dir"))
        .await
        .expect("resolves like stat")
        .count()
        .await;
    assert_eq!(n, 1, "list resolves case the same way stat does");
}

#[tokio::test]
async fn mixed_case_write_lands_under_real_parent() {
    let mem = MemProvider::with_flags(CapabilityFlags::CASE_PRESERVING);
    mem.mkdir(&vp("mem:///Dir")).await.unwrap();
    // Write via different case: the child hangs off the REAL dir, never an orphan.
    write_file(&mem, "mem:///dir/g", b"x").await;
    let n = mem.list(&vp("mem:///Dir")).await.unwrap().count().await;
    assert_eq!(n, 1, "the child is visible under the real dir");
    // And the dir is no longer empty: remove must be refused.
    assert!(mem.remove(&vp("mem:///Dir")).await.is_err());
}

#[tokio::test]
async fn commit_after_parent_removed_fails() {
    let mem = MemProvider::new();
    mem.mkdir(&vp("mem:///d")).await.unwrap();
    let mut sink = mem.write(&vp("mem:///d/f")).await.unwrap();
    sink.write(Bytes::from_static(b"x")).await.unwrap();
    mem.remove(&vp("mem:///d")).await.unwrap();
    // The parent disappeared between write() and commit(): never orphans.
    assert_eq!(sink.commit().await.unwrap_err(), Error::NotFound);
}

#[tokio::test]
async fn rename_into_own_subtree_refused() {
    let mem = MemProvider::new();
    mem.mkdir(&vp("mem:///a")).await.unwrap();
    assert_eq!(
        mem.rename(&vp("mem:///a"), &vp("mem:///a/b"))
            .await
            .unwrap_err(),
        Error::InvalidPath,
        "moving a dir inside itself is EINVAL"
    );
    // The tree stays intact.
    assert!(mem.stat(&vp("mem:///a")).await.is_ok());
}

#[tokio::test]
async fn entry_paths_preserve_authority() {
    let mem = MemProvider::new();
    write_file(&mem, "mem://conn1/f", b"x").await;
    let e = mem.stat(&vp("mem://conn1/f")).await.unwrap();
    assert_eq!(
        e.path.authority(),
        Some("conn1"),
        "stat preserves authority"
    );
    let listed: Vec<_> = mem
        .list(&vp("mem://conn1/"))
        .await
        .unwrap()
        .map(|e| e.unwrap().path.authority().map(str::to_owned))
        .collect()
        .await;
    assert_eq!(listed, vec![Some("conn1".to_owned())], "list too");
}

#[tokio::test]
async fn commit_detects_late_case_collision() {
    let mem = MemProvider::with_flags(CapabilityFlags::CASE_PRESERVING);
    let mut sink = mem.write(&vp("mem:///file")).await.unwrap();
    sink.write(Bytes::from_static(b"1")).await.unwrap();
    // "File" appears between write() and commit(): a case collision, not Exists.
    write_file(&mem, "mem:///File", b"2").await;
    match sink.commit().await {
        Err(Error::Conflict { conflict }) => assert_eq!(conflict, ConflictKind::CaseCollision),
        Err(e) => panic!("expected CaseCollision, was {e:?}"),
        Ok(()) => panic!("expected CaseCollision, the commit published"),
    }
}

// ---------- NFC/NFD normalization axis (issue #7) ----------

/// APFS simulation: normalization-insensitive lookup, bytes preserved. The
/// normalization-only collision is labeled `Normalization` (#8).
#[tokio::test]
async fn normalization_insensitive_collides_with_label() {
    use norte_testkit::Normalization;
    let mem = MemProvider::new().with_normalization(Normalization::Insensitive);
    let root = MemProvider::root();
    let nfc = root.join(Segment::new(vec![0xC3, 0xA9]).unwrap()); // é NFC
    let nfd = root.join(Segment::new(vec![0x65, 0xCC, 0x81]).unwrap()); // é NFD

    let mut sink = mem.write(&nfc).await.unwrap();
    sink.write(bytes::Bytes::from_static(b"x")).await.unwrap();
    sink.commit().await.unwrap();

    // The NFD lookup resolves to the NFC file (insensitive, byte-preserving).
    let e = mem.stat(&nfd).await.expect("normalized lookup resolves");
    assert_eq!(
        e.path.file_name().unwrap().as_bytes(),
        &[0xC3, 0xA9],
        "the STORED (NFC) bytes are preserved"
    );

    // Writing the NFD variant collides with the correct label.
    match mem.write(&nfd).await {
        Err(Error::Conflict {
            conflict: ConflictKind::Normalization,
        }) => {}
        Err(other) => panic!("expected Conflict::Normalization, was {other:?}"),
        Ok(_) => panic!("expected Conflict::Normalization, was Ok(sink)"),
    }
}

/// Default (byte-exact, like ext4): NFC and NFD are DIFFERENT files.
#[tokio::test]
async fn normalization_byte_exact_keeps_both() {
    let mem = MemProvider::new();
    let root = MemProvider::root();
    let nfc = root.join(Segment::new(vec![0xC3, 0xA9]).unwrap());
    let nfd = root.join(Segment::new(vec![0x65, 0xCC, 0x81]).unwrap());
    for p in [&nfc, &nfd] {
        let mut sink = mem.write(p).await.unwrap();
        sink.write(bytes::Bytes::from_static(b"x")).await.unwrap();
        sink.commit().await.unwrap();
    }
    assert!(mem.stat(&nfc).await.is_ok());
    assert!(mem.stat(&nfd).await.is_ok());
}

/// TRANSIENT unavailability (retries issue, ADR 0005): the next n ops fail
/// retryable and the provider recovers on its own.
#[tokio::test]
async fn unavailable_for_next_recovers() {
    let mem = MemProvider::new();
    mem.faults().unavailable_for_next(2);
    let root = MemProvider::root();
    for _ in 0..2 {
        match mem.stat(&root).await {
            Err(Error::ProviderUnavailable { retryable: true }) => {}
            other => panic!("expected ProviderUnavailable retryable, was {other:?}"),
        }
    }
    assert!(mem.stat(&root).await.is_ok(), "after n ops, recovers");
}

// ---------- node identity (issue #16) ----------

/// `without_node_ids()` simulates a backend WITHOUT stable identity (object
/// storage, ftp): `node_id` = `Ok(None)` always, even if the node exists.
#[tokio::test]
async fn without_node_ids_devuelve_none() {
    use norte_vfs::FollowLinks;
    let mem = MemProvider::new().without_node_ids();
    write_file(&mem, "mem:///f", b"x").await;
    assert_eq!(
        mem.node_id(&vp("mem:///f"), FollowLinks::No).await.unwrap(),
        None
    );
}

/// Identity tells nodes apart even when PATHS fold: on a case-insensitive
/// Mem, `a` and `A` resolve to the SAME node → same id. It is what the
/// engine's guard could not know via heuristics (issue #16).
#[tokio::test]
async fn node_id_es_el_mismo_para_variantes_de_caja_plegadas() {
    use norte_vfs::FollowLinks;
    let mem =
        MemProvider::with_flags(CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING);
    write_file(&mem, "mem:///Mismo", b"x").await;
    let a = mem
        .node_id(&vp("mem:///Mismo"), FollowLinks::No)
        .await
        .unwrap()
        .expect("Mem has identity");
    let b = mem
        .node_id(&vp("mem:///mismo"), FollowLinks::No)
        .await
        .unwrap()
        .expect("Mem has identity");
    assert_eq!(a, b, "same node under any case the FS folds");
}

// ---------- symlink kind (issue #18) ----------

/// `SymlinkKind::Unknown`: the provider resolves the target in ITS tree.
/// Dir target → Dir; file target → File; broken → File (documented).
#[tokio::test]
async fn symlink_unknown_resuelve_el_kind_del_target() {
    use norte_vfs::SymlinkKind;
    let mem = MemProvider::new();
    mem.mkdir(&vp("mem:///d")).await.unwrap();
    write_file(&mem, "mem:///f", b"x").await;

    mem.symlink(&vp("mem:///ld"), b"d", SymlinkKind::Unknown)
        .await
        .unwrap();
    mem.symlink(&vp("mem:///lf"), b"f", SymlinkKind::Unknown)
        .await
        .unwrap();
    mem.symlink(&vp("mem:///lroto"), b"nada", SymlinkKind::Unknown)
        .await
        .unwrap();

    assert_eq!(
        mem.symlink_kind_of(&vp("mem:///ld")),
        Some(SymlinkKind::Dir)
    );
    assert_eq!(
        mem.symlink_kind_of(&vp("mem:///lf")),
        Some(SymlinkKind::File)
    );
    assert_eq!(
        mem.symlink_kind_of(&vp("mem:///lroto")),
        Some(SymlinkKind::File),
        "broken degrades to File, never an error"
    );
    // Explicit kind: honored as-is, nothing resolved.
    mem.symlink(&vp("mem:///lex"), b"nada", SymlinkKind::Dir)
        .await
        .unwrap();
    assert_eq!(
        mem.symlink_kind_of(&vp("mem:///lex")),
        Some(SymlinkKind::Dir)
    );
}

// ---------- ambiguous mutation (issue #17) ----------

/// The POST-effect failure: the mutation applies AND returns
/// `ProviderUnavailable` retryable — a remote's "timeout after commit". It
/// is the fixture for the engine's disambiguating retry.
#[tokio::test]
async fn ambiguous_mutation_aplica_el_efecto_y_falla_transitorio() {
    let mem = MemProvider::new();
    mem.faults().ambiguous_mutations(1);
    match mem.mkdir(&vp("mem:///d")).await {
        Err(Error::ProviderUnavailable { retryable: true }) => {}
        other => panic!("expected ProviderUnavailable retryable, was {other:?}"),
    }
    // The effect WAS applied (that is the ambiguity).
    let e = mem.stat(&vp("mem:///d")).await.unwrap();
    assert_eq!(e.kind, EntryKind::Dir);
    // Consumed: the next mutation is normal.
    mem.mkdir(&vp("mem:///d2")).await.unwrap();
}

/// The ambiguous failure covers the trait's 4 point mutations
/// (mkdir/remove/rename/symlink); reads do NOT consume it.
#[tokio::test]
async fn ambiguous_mutation_cubre_las_cuatro_mutaciones() {
    use norte_vfs::SymlinkKind;
    let mem = MemProvider::new();
    write_file(&mem, "mem:///a", b"x").await;

    // A read in between does not consume the armed fault.
    mem.faults().ambiguous_mutations(1);
    mem.stat(&vp("mem:///a")).await.unwrap();
    assert!(mem.rename(&vp("mem:///a"), &vp("mem:///b")).await.is_err());
    assert!(mem.stat(&vp("mem:///b")).await.is_ok(), "rename applied");

    mem.faults().ambiguous_mutations(1);
    assert!(mem.remove(&vp("mem:///b")).await.is_err());
    assert_eq!(
        mem.stat(&vp("mem:///b")).await.unwrap_err(),
        Error::NotFound,
        "remove applied"
    );

    mem.faults().ambiguous_mutations(1);
    assert!(
        mem.symlink(&vp("mem:///l"), b"t", SymlinkKind::File)
            .await
            .is_err()
    );
    assert_eq!(mem.read_link(&vp("mem:///l")).await.unwrap(), b"t");
}

/// A link→link chain with follow: `NotFound`, consistent with `read()`
/// (Mem's minimal resolution does not follow chains — documented limit).
#[tokio::test]
async fn node_id_follow_sobre_cadena_es_notfound() {
    use norte_vfs::{FollowLinks, SymlinkKind};
    let mem = MemProvider::new();
    write_file(&mem, "mem:///f", b"x").await;
    mem.symlink(&vp("mem:///l1"), b"f", SymlinkKind::File)
        .await
        .unwrap();
    mem.symlink(&vp("mem:///l2"), b"l1", SymlinkKind::File)
        .await
        .unwrap();
    assert_eq!(
        mem.node_id(&vp("mem:///l2"), FollowLinks::Yes)
            .await
            .unwrap_err(),
        Error::NotFound
    );
    // One level does resolve.
    assert!(
        mem.node_id(&vp("mem:///l1"), FollowLinks::Yes)
            .await
            .unwrap()
            .is_some()
    );
}

/// Symlink traversal composes with the normalization axis: a dir stored in
/// NFD, looked up in NFC through an intermediate link.
#[tokio::test]
async fn travesia_compone_con_normalizacion_insensible() {
    use norte_testkit::Normalization;
    let mem = MemProvider::new().with_normalization(Normalization::Insensitive);
    // Dir with an NFD name (e + combining mark).
    mem.mkdir(&vp("mem:///e%CC%81")).await.unwrap();
    write_file(&mem, "mem:///e%CC%81/f", b"x").await;
    // Link pointing at the dir by its NFC form (precomposed é).
    mem.symlink(&vp("mem:///ln"), &[0xC3, 0xA9], norte_vfs::SymlinkKind::Dir)
        .await
        .unwrap();
    // Read THROUGH the link (NFC target → NFD dirent).
    assert_eq!(read_all(&mem, "mem:///ln/f").await.unwrap(), b"x");
    let e = mem.stat(&vp("mem:///ln/f")).await.unwrap();
    assert_eq!(e.kind, EntryKind::File);
}

/// `SymlinkKind::Unknown` with hostile targets: absolute and `..` degrade
/// to File (minimal resolution → Unsupported); a non-UTF8 target pointing
/// at a dir with a non-UTF8 name resolves Dir.
#[tokio::test]
async fn symlink_unknown_con_targets_hostiles() {
    use norte_vfs::SymlinkKind;
    let mem = MemProvider::new();
    mem.symlink(&vp("mem:///labs"), b"/etc", SymlinkKind::Unknown)
        .await
        .unwrap();
    mem.symlink(&vp("mem:///ldot"), b"../fuera", SymlinkKind::Unknown)
        .await
        .unwrap();
    assert_eq!(
        mem.symlink_kind_of(&vp("mem:///labs")),
        Some(SymlinkKind::File)
    );
    assert_eq!(
        mem.symlink_kind_of(&vp("mem:///ldot")),
        Some(SymlinkKind::File)
    );

    let seg = norte_proto::Segment::new(vec![0xE9]).unwrap();
    let hostile_dir = MemProvider::root().join(seg.clone());
    mem.mkdir(&hostile_dir).await.unwrap();
    let link = MemProvider::root().join(norte_proto::Segment::new(b"lhost".to_vec()).unwrap());
    mem.symlink(&link, &[0xE9], SymlinkKind::Unknown)
        .await
        .unwrap();
    assert_eq!(mem.symlink_kind_of(&link), Some(SymlinkKind::Dir));
}

#[tokio::test]
async fn faults_cuenta_las_llamadas_a_read() {
    // Observability #61: the read counter lets cache tests assert "N
    // concurrent ops = a SINGLE build's reads".
    let mem = MemProvider::new();
    write_file(&mem, "mem:///f", b"data").await;
    let faults = mem.faults();
    assert_eq!(faults.read_calls(), 0);
    let _ = read_all(&mem, "mem:///f").await.expect("read");
    let _ = read_all(&mem, "mem:///f").await.expect("read");
    assert_eq!(faults.read_calls(), 2);
}

// ---------- synthetic attrs (#108 block 2) ----------

#[tokio::test]
async fn synthetic_attrs_hostiles_y_deterministas() {
    use norte_vfs::{AttrRequest, ListOptions};
    let mem = MemProvider::new().with_synthetic_attrs();
    write_file(&mem, "mem:///f.txt", b"x").await;
    let opt = ListOptions {
        attrs: AttrRequest::sanitized(
            ["mem.owner", "mem.note", "mem.mode", "mem.stamp"].map(str::to_owned),
        ),
    };
    let e = mem
        .stat_with(&vp("mem:///f.txt"), &opt)
        .await
        .expect("stat_with");
    // Non-UTF-8 owner: raw BYTES, never a String (rule 1).
    assert_eq!(
        e.attrs.get("mem.owner"),
        Some(&norte_proto::AttrValue::Bytes(
            b"due\xf1o-\xff\xfe".to_vec()
        ))
    );
    // Hostile text: RTL override + ZWJ, within the cap.
    let Some(norte_proto::AttrValue::Text(note)) = e.attrs.get("mem.note") else {
        panic!("mem.note must be Text");
    };
    assert!(note.contains('\u{202e}') && note.contains('\u{200d}'));
    assert_eq!(
        e.attrs.get("mem.mode"),
        Some(&norte_proto::AttrValue::Uint(0o100_644))
    );
    assert!(matches!(
        e.attrs.get("mem.stamp"),
        Some(norte_proto::AttrValue::TimeMs(_))
    ));

    // Partial request: ONLY what was asked for.
    let solo = ListOptions {
        attrs: AttrRequest::sanitized(["mem.mode".to_owned()]),
    };
    let e = mem
        .stat_with(&vp("mem:///f.txt"), &solo)
        .await
        .expect("stat_with");
    assert_eq!(e.attrs.len(), 1);

    // No request → no attrs, in list too.
    assert!(
        mem.stat(&vp("mem:///f.txt"))
            .await
            .expect("stat")
            .attrs
            .is_empty()
    );
    let mut s = mem.list(&vp("mem:///")).await.expect("list");
    while let Some(e) = s.next().await {
        assert!(e.expect("entry").attrs.is_empty());
    }
}

// ---------- per-location capabilities (ADR 0054) ----------

#[tokio::test]
async fn capabilities_at_defaults_to_the_declaration() {
    let mem = MemProvider::new();
    let root = MemProvider::root();
    assert_eq!(
        mem.capabilities_at(&root).await.expect("responds"),
        mem.capabilities(),
        "unscripted, the location answers what the backend declares"
    );
}

#[tokio::test]
async fn a_scripted_location_overrides_the_declaration() {
    let mem = MemProvider::new();
    let usb = vp("mem:///usb");
    mem.mkdir(&usb).await.expect("dir");
    mem.set_caps_at(
        &usb,
        norte_proto::Capabilities {
            flags: CapabilityFlags::CASE_PRESERVING | CapabilityFlags::FULL_FOLD,
            max_path: None,
        },
    );

    let at = mem.capabilities_at(&usb).await.expect("responds");
    assert!(at.flags.contains(CapabilityFlags::FULL_FOLD));
    assert!(
        !mem.capabilities()
            .flags
            .contains(CapabilityFlags::FULL_FOLD),
        "and the backend still declares its own"
    );

    // A location without its own script does not inherit its neighbor's.
    assert_eq!(
        mem.capabilities_at(&MemProvider::root())
            .await
            .expect("responds"),
        mem.capabilities()
    );
}
