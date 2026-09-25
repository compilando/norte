//! Pack, test, split and combine (#132), end to end through the engine.
//!
//! Packing's oracle is this same repository's READER: if what comes out lists
//! and reads through the archive provider, the archive is at least as good as
//! the ones norte accepts from outside.

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt as _;
use norte_core::Engine;
use norte_proto::{Segment, TaskState, VPath, methods};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(w: &str) -> VPath {
    VPath::parse(w).expect("test wire")
}

/// An engine over a seeded `MemProvider`, and the provider to look inside it.
async fn engine_with(files: &[(&str, &[u8])]) -> (Engine, Arc<MemProvider>) {
    let mem = Arc::new(MemProvider::new());
    for (wire, data) in files {
        let p = vp(wire);
        // The whole chain of parents, not just the immediate one:
        // `mem:///p/a/b` needs `p` and `p/a`.
        let mut chain = Vec::new();
        let mut current = p.parent();
        while let Some(d) = current {
            if d.is_root() {
                break;
            }
            current = d.parent();
            chain.push(d);
        }
        for d in chain.into_iter().rev() {
            let _ = mem.mkdir(&d).await;
        }
        let mut sink = mem.write(&p).await.expect("write");
        sink.write(Bytes::copy_from_slice(data))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }
    let engine = Engine::new();
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem)
}

/// Reads a whole file from the provider.
async fn read(mem: &MemProvider, wire: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut s = mem.read(&vp(wire), None).await.expect("read");
    while let Some(c) = s.next().await {
        out.extend_from_slice(&c.expect("chunk"));
    }
    out
}

/// An archive's interior tree, as `(relative name, content)` pairs.
async fn inside(engine: &Engine, container: &str, token: &str) -> Vec<(String, Vec<u8>)> {
    let root = VPath::archive_compose(token, &vp(container), &[]).expect("compose");
    let mut out = Vec::new();
    let mut pending = vec![root.clone()];
    while let Some(dir) = pending.pop() {
        let mut stream = engine.list(&dir).await.expect("list");
        while let Some(e) = stream.next().await {
            let e = e.expect("entry");
            match e.kind {
                norte_proto::EntryKind::Dir => pending.push(e.path),
                norte_proto::EntryKind::File => {
                    let name = String::from_utf8_lossy(
                        &e.path
                            .segments()
                            .skip(root.segments().count())
                            .collect::<Vec<_>>()
                            .join(&b'/'),
                    )
                    .into_owned();
                    let mut data = Vec::new();
                    let mut bs = engine.read(&e.path, None).await.expect("read");
                    while let Some(c) = bs.next().await {
                        data.extend_from_slice(&c.expect("chunk"));
                    }
                    out.push((name, data));
                }
                _ => {}
            }
        }
    }
    out.sort();
    out
}

fn pack_params(sources: &[&str], dest: &str, base: &str) -> methods::ArchivePackParams {
    methods::ArchivePackParams {
        sources: sources.iter().map(|s| vp(s)).collect(),
        dest: vp(dest),
        format: methods::ArchiveFormat::Zip,
        level: Some(6),
        base: vp(base),
    }
}

/// The whole case: a tree is packed and the archive provider itself reads it
/// back, with names relative to the base.
///
/// The base is the directory the sources HANG from, not the source itself:
/// with `base == source` the root would have no name inside the archive, and
/// that is exactly what the op refuses instead of making one up.
#[tokio::test]
async fn packing_and_reading_it_back() {
    let (engine, mem) = engine_with(&[
        ("mem:///proj/README", b"hello"),
        ("mem:///proj/src/main.rs", b"fn main() {}"),
    ])
    .await;
    let handle = engine
        .pack_as(
            pack_params(&["mem:///proj"], "mem:///proj.zip", "mem:///"),
            norte_core::journal::Actor::User,
        )
        .await
        .expect("starts");
    assert!(
        matches!(handle.join().await, TaskState::Completed),
        "the task finishes"
    );
    assert!(!read(&mem, "mem:///proj.zip").await.is_empty());

    assert_eq!(
        inside(&engine, "mem:///proj.zip", "zip").await,
        vec![
            ("proj/README".to_owned(), b"hello".to_vec()),
            ("proj/src/main.rs".to_owned(), b"fn main() {}".to_vec()),
        ],
        "the names hang from the base, and the content comes back whole"
    );
}

/// **Rule 1 end to end**: a name that is not UTF-8 goes into the archive and
/// comes out byte for byte, passing through the whole engine.
#[tokio::test]
async fn a_hostile_name_survives_the_engine() {
    let hostile = b"cafe\xff.txt";
    let mem = Arc::new(MemProvider::new());
    let dir = vp("mem:///d");
    mem.mkdir(&dir).await.expect("mkdir");
    let p = dir.join(Segment::new(hostile.to_vec()).expect("seg"));
    let mut sink = mem.write(&p).await.expect("write");
    sink.write(Bytes::from_static(b"inside"))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
    let engine = Engine::new();
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);

    let handle = engine
        .pack_as(
            pack_params(&["mem:///d"], "mem:///d.zip", "mem:///"),
            norte_core::journal::Actor::User,
        )
        .await
        .expect("starts");
    assert!(matches!(handle.join().await, TaskState::Completed));

    let root = VPath::archive_compose("zip", &vp("mem:///d.zip"), &[]).expect("compose");
    let mut names = Vec::new();
    let mut pending = vec![root];
    while let Some(d) = pending.pop() {
        let mut stream = engine.list(&d).await.expect("list");
        while let Some(e) = stream.next().await {
            let e = e.expect("entry");
            if e.kind == norte_proto::EntryKind::Dir {
                pending.push(e.path);
            } else if let Some(n) = e.path.file_name() {
                names.push(n.as_bytes().to_vec());
            }
        }
    }
    assert_eq!(names, vec![hostile.to_vec()], "byte for byte");
}

/// A source that does not hang from the base has no name inside the archive,
/// and making one up would put the entry where nobody expects it when
/// unpacking.
#[tokio::test]
async fn a_source_outside_the_base_is_refused() {
    let (engine, _mem) = engine_with(&[("mem:///a/x", b"1"), ("mem:///b/y", b"2")]).await;
    let handle = engine
        .pack_as(
            pack_params(&["mem:///b/y"], "mem:///out.zip", "mem:///a"),
            norte_core::journal::Actor::User,
        )
        .await
        .expect("the task starts; the refusal is its own");
    assert!(
        matches!(
            handle.join().await,
            TaskState::Failed {
                error: norte_proto::Error::InvalidPath,
                ..
            }
        ),
        "a source outside the base is InvalidPath"
    );
}

/// Two sources that give the SAME name inside the archive are refused.
///
/// This happens with overlapping roots, which the wire accepts even though
/// the TUI's marks never form them: the archive would carry the entry twice
/// with its content twice, our index would resolve it as "the last one wins",
/// and other tools would extract it twice.
#[tokio::test]
async fn two_sources_with_the_same_name_are_refused() {
    let (engine, _mem) = engine_with(&[("mem:///p/a/b", b"x")]).await;
    let h = engine
        .pack_as(
            methods::ArchivePackParams {
                sources: vec![vp("mem:///p/a"), vp("mem:///p/a/b")],
                dest: vp("mem:///out.zip"),
                format: methods::ArchiveFormat::Zip,
                level: None,
                base: vp("mem:///p"),
            },
            norte_core::journal::Actor::User,
        )
        .await
        .expect("starts");
    assert!(matches!(
        h.join().await,
        TaskState::Failed {
            error: norte_proto::Error::Conflict { .. },
            ..
        }
    ));
}

/// The destination is not overwritten: making an archive over a file that is
/// already there is silent data loss.
#[tokio::test]
async fn it_does_not_pack_over_something() {
    let (engine, _mem) =
        engine_with(&[("mem:///a/x", b"1"), ("mem:///already.zip", b"it's me")]).await;
    let handle = engine
        .pack_as(
            pack_params(&["mem:///a"], "mem:///already.zip", "mem:///"),
            norte_core::journal::Actor::User,
        )
        .await
        .expect("starts");
    assert!(matches!(
        handle.join().await,
        TaskState::Failed {
            error: norte_proto::Error::Conflict { .. },
            ..
        }
    ));
}

/// **Rule 3**: cancelling leaves the destination CLEAN. A half archive that
/// looks like an archive is worse than none.
#[tokio::test]
async fn cancelling_leaves_no_half_archive() {
    let files: Vec<(String, Vec<u8>)> = (0..64)
        .map(|i| (format!("mem:///big/f{i:03}"), vec![b'x'; 200_000]))
        .collect();
    let refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(n, d)| (n.as_str(), d.as_slice()))
        .collect();
    let (engine, mem) = engine_with(&refs).await;

    let handle = engine
        .pack_as(
            pack_params(&["mem:///big"], "mem:///big.zip", "mem:///"),
            norte_core::journal::Actor::User,
        )
        .await
        .expect("starts");
    handle.cancel();
    let state = handle.join().await;
    assert!(
        matches!(state, TaskState::Cancelled | TaskState::Completed),
        "either it was cancelled or it won the race: {state:?}"
    );
    if matches!(state, TaskState::Cancelled) {
        assert!(
            mem.stat(&vp("mem:///big.zip")).await.is_err(),
            "cancelled = clean destination, not even a half file"
        );
    }
}

/// `archive.test` over a healthy archive: no failures, and saying WHAT it checked.
#[tokio::test]
async fn testing_a_healthy_archive() {
    let (engine, _mem) = engine_with(&[("mem:///a/x", b"content"), ("mem:///a/y", b"other")]).await;
    let h = engine
        .pack_as(
            pack_params(&["mem:///a"], "mem:///a.zip", "mem:///"),
            norte_core::journal::Actor::User,
        )
        .await
        .expect("starts");
    assert!(matches!(h.join().await, TaskState::Completed));

    let (h, report) = engine
        .test_archive_as(
            methods::ArchiveTestParams {
                path: vp("mem:///a.zip"),
            },
            norte_core::journal::Actor::User,
        )
        .await
        .expect("starts");
    assert!(matches!(h.join().await, TaskState::Completed));
    let r = report.lock().expect("report").clone();
    assert_eq!(r.entries, 2, "the two entries");
    assert!(r.failed.is_empty(), "healthy: {:?}", r.failed);
    assert!(!r.truncated);
    assert_eq!(r.checked, vec!["crc".to_owned()], "a zip carries a CRC");
}

/// A file that is not a known format is not "tested": saying it passes would
/// mean nothing.
#[tokio::test]
async fn testing_something_that_is_not_an_archive_is_unsupported() {
    let (engine, _mem) = engine_with(&[("mem:///notes.txt", b"hello")]).await;
    let r = engine
        .test_archive_as(
            methods::ArchiveTestParams {
                path: vp("mem:///notes.txt"),
            },
            norte_core::journal::Actor::User,
        )
        .await;
    match r {
        Err(e) => assert_eq!(e, norte_proto::Error::Unsupported),
        Ok(_) => panic!("a .txt is not an archive to test"),
    }
}

/// Splitting and combining back gives the original file, byte for byte.
#[tokio::test]
async fn splitting_and_combining_gives_the_original() {
    let data: Vec<u8> = (0..30_000_u32).map(|i| (i % 251) as u8).collect();
    let (engine, mem) = engine_with(&[("mem:///g.bin", &data)]).await;
    mem.mkdir(&vp("mem:///parts")).await.expect("mkdir");

    let h = engine
        .split_as(
            methods::FileSplitParams {
                path: vp("mem:///g.bin"),
                part_bytes: 8192,
                dest_dir: vp("mem:///parts"),
            },
            norte_core::journal::Actor::User,
        )
        .await
        .expect("starts");
    assert!(matches!(h.join().await, TaskState::Completed));

    // 30 000 / 8192 = 3 parts and some → four, and the last one shorter.
    assert_eq!(read(&mem, "mem:///parts/g.bin.001").await.len(), 8192);
    assert_eq!(
        read(&mem, "mem:///parts/g.bin.004").await.len(),
        30_000 - 3 * 8192
    );

    let h = engine
        .combine_as(
            methods::FileCombineParams {
                first: vp("mem:///parts/g.bin.001"),
                dest: vp("mem:///back.bin"),
            },
            norte_core::journal::Actor::User,
        )
        .await
        .expect("starts");
    assert!(matches!(h.join().await, TaskState::Completed));
    assert_eq!(read(&mem, "mem:///back.bin").await, data, "byte for byte");
}

/// **Cancelling a split leaves no half set.**
///
/// And this is the opposite of what a cancelled copy does, on purpose: a
/// half-copied tree is visible at a glance, but half a set of parts is
/// indistinguishable from a whole one — all of them the requested size, no
/// gaps — and combining it gives a short file that passes every guard. So the
/// ones already published are retracted.
#[tokio::test]
async fn cancelling_a_split_retracts_the_parts_already_written() {
    let data = vec![b'x'; 4096 * 40];
    let (engine, mem) = engine_with(&[("mem:///big.bin", &data)]).await;
    mem.mkdir(&vp("mem:///pieces")).await.expect("mkdir");
    let h = engine
        .split_as(
            methods::FileSplitParams {
                path: vp("mem:///big.bin"),
                part_bytes: 4096,
                dest_dir: vp("mem:///pieces"),
            },
            norte_core::journal::Actor::User,
        )
        .await
        .expect("starts");
    h.cancel();
    let state = h.join().await;
    if matches!(state, TaskState::Cancelled) {
        assert!(
            mem.stat(&vp("mem:///pieces/big.bin.001")).await.is_err(),
            "cancelled = not even a lone part that looks like the start of a set"
        );
    }
}

/// A part of the set that ALREADY exists from a previous attempt is reported
/// before writing the first one: mixing new parts with stale ones produces a
/// set that combines without anything squeaking.
#[tokio::test]
async fn a_preexisting_part_for_the_split_is_caught_before_starting() {
    let data = vec![b'y'; 4096 * 3];
    let (engine, mem) = engine_with(&[
        ("mem:///d.bin", &data[..]),
        ("mem:///p/d.bin.002", &vec![b'z'; 10][..]),
    ])
    .await;
    let h = engine
        .split_as(
            methods::FileSplitParams {
                path: vp("mem:///d.bin"),
                part_bytes: 4096,
                dest_dir: vp("mem:///p"),
            },
            norte_core::journal::Actor::User,
        )
        .await
        .expect("starts");
    assert!(matches!(
        h.join().await,
        TaskState::Failed {
            error: norte_proto::Error::Conflict { .. },
            ..
        }
    ));
    assert!(
        mem.stat(&vp("mem:///p/d.bin.001")).await.is_err(),
        "not even the first one was written"
    );
}

/// An EXACT split leaves no empty part at the end: a zero-byte `.004` is a
/// file nobody can tell is extra or missing.
#[tokio::test]
async fn an_exact_split_leaves_no_empty_part() {
    let data = vec![b'z'; 16_384];
    let (engine, mem) = engine_with(&[("mem:///e.bin", &data)]).await;
    mem.mkdir(&vp("mem:///t")).await.expect("mkdir");
    let h = engine
        .split_as(
            methods::FileSplitParams {
                path: vp("mem:///e.bin"),
                part_bytes: 8192,
                dest_dir: vp("mem:///t"),
            },
            norte_core::journal::Actor::User,
        )
        .await
        .expect("starts");
    assert!(matches!(h.join().await, TaskState::Completed));
    assert!(mem.stat(&vp("mem:///t/e.bin.002")).await.is_ok());
    assert!(
        mem.stat(&vp("mem:///t/e.bin.003")).await.is_err(),
        "there is no empty third part"
    );
}

/// A gap in the numbering does NOT get bridged, **and neither does what comes
/// before it get combined**.
///
/// This test used to assert the opposite and pass: the walk stopped at the
/// missing number, saw a set of ONE part and combined it. The task said
/// `Completed`, the journal recorded a `Created`, and what was left on disk
/// was 20% of an ISO that mounts as a corrupt image — no error, no partial
/// mark, and against what ADR 0060 and four rustdocs promise. `rust-reviewer`
/// found it by reading the test's name against its assert.
#[tokio::test]
async fn combining_with_a_gap_is_refused() {
    let (engine, mem) = engine_with(&[
        ("mem:///t/x.bin.001", &vec![b'a'; 100][..]),
        ("mem:///t/x.bin.003", &vec![b'c'; 100][..]),
    ])
    .await;
    let h = engine
        .combine_as(
            methods::FileCombineParams {
                first: vp("mem:///t/x.bin.001"),
                dest: vp("mem:///x.bin"),
            },
            norte_core::journal::Actor::User,
        )
        .await
        .expect("starts");
    assert!(
        matches!(
            h.join().await,
            TaskState::Failed {
                error: norte_proto::Error::Conflict { .. },
                ..
            }
        ),
        "an incomplete set is an error, not a short file"
    );
    assert!(
        mem.stat(&vp("mem:///x.bin")).await.is_err(),
        "and the destination was not created"
    );
}

/// A COMPLETE set of three combines whole: the gap check must not reject what
/// is actually fine.
#[tokio::test]
async fn combining_a_complete_set_combines_all_of_them() {
    let (engine, mem) = engine_with(&[
        ("mem:///t/y.bin.001", &vec![b'a'; 100][..]),
        ("mem:///t/y.bin.002", &vec![b'b'; 100][..]),
        ("mem:///t/y.bin.003", &vec![b'c'; 40][..]),
        // A neighbor that is NOT a part does not get in the way.
        ("mem:///t/y.bin.001.bak", &vec![b'z'; 5][..]),
    ])
    .await;
    let h = engine
        .combine_as(
            methods::FileCombineParams {
                first: vp("mem:///t/y.bin.001"),
                dest: vp("mem:///y.bin"),
            },
            norte_core::journal::Actor::User,
        )
        .await
        .expect("starts");
    assert!(matches!(h.join().await, TaskState::Completed));
    assert_eq!(read(&mem, "mem:///y.bin").await.len(), 240);
}

/// A middle part shorter than the first is a part that was copied halfway: it
/// is rejected BEFORE creating the destination.
#[tokio::test]
async fn combining_with_a_short_part_in_the_middle_is_refused() {
    let (engine, mem) = engine_with(&[
        ("mem:///t/z.bin.001", &vec![b'a'; 100][..]),
        ("mem:///t/z.bin.002", &vec![b'b'; 40][..]),
        ("mem:///t/z.bin.003", &vec![b'c'; 100][..]),
    ])
    .await;
    let h = engine
        .combine_as(
            methods::FileCombineParams {
                first: vp("mem:///t/z.bin.001"),
                dest: vp("mem:///z.bin"),
            },
            norte_core::journal::Actor::User,
        )
        .await
        .expect("starts");
    assert!(matches!(
        h.join().await,
        TaskState::Failed {
            error: norte_proto::Error::Conflict { .. },
            ..
        }
    ));
    assert!(
        mem.stat(&vp("mem:///z.bin")).await.is_err(),
        "and the destination was not created"
    );
}

/// A part below the minimum would produce a million files: it is refused
/// before writing anything.
#[tokio::test]
async fn a_tiny_part_is_refused() {
    let (engine, _mem) = engine_with(&[("mem:///p.bin", b"12345678")]).await;
    // **At SUBMIT, not inside the Task.** With the refusal inside the body,
    // the RPC answered `{task_id}` and a scripted client read a success: it
    // printed "splitting…", exited, and nothing had happened.
    let r = engine
        .split_as(
            methods::FileSplitParams {
                path: vp("mem:///p.bin"),
                part_bytes: 2,
                dest_dir: vp("mem:///"),
            },
            norte_core::journal::Actor::User,
        )
        .await;
    match r {
        Err(e) => assert_eq!(e, norte_proto::Error::InvalidPath),
        Ok(_) => panic!("a two-byte part is not a request"),
    }
}

/// More than 999 parts does not fit the `.001` convention, and it is said
/// BEFORE writing: discovering it at part 1000 leaves a set nobody can
/// combine back.
#[tokio::test]
async fn too_many_parts_are_refused_before_writing() {
    let data = vec![b'x'; 4096 * 1001];
    let (engine, mem) = engine_with(&[("mem:///huge.bin", &data)]).await;
    mem.mkdir(&vp("mem:///td")).await.expect("mkdir");
    let r = engine
        .split_as(
            methods::FileSplitParams {
                path: vp("mem:///huge.bin"),
                part_bytes: 4096,
                dest_dir: vp("mem:///td"),
            },
            norte_core::journal::Actor::User,
        )
        .await;
    match r {
        Err(norte_proto::Error::LimitExceeded { .. }) => {}
        Err(other) => panic!("expected LimitExceeded, was {other:?}"),
        Ok(_) => panic!("more than 999 parts is said at SUBMIT, not afterward"),
    }
    assert!(
        mem.stat(&vp("mem:///td/huge.bin.001")).await.is_err(),
        "not even the first one got written"
    );
}

/// #250 — two entries that FOLD to the same name are not packed either.
///
/// Different bytes are not enough: what decides is whether they collide where
/// the archive gets extracted, and an archive cannot know that — it gets sent
/// there. It folds with the widest mode on purpose, so the question is not
/// "here?" but "anywhere?". Extracted there, one of the two disappears without
/// a word, and that is the direction ADR 0005 says not to take.
///
/// Four pairs from the canonical corpus, and they are four different folds:
/// NFD/NFC normalization, an NFC singleton, mu versus micro, and a real ext4
/// `+F`'s full fold. A fix that only looked at case would pass none of them.
///
/// The fifth pair the issue lists — `win_trailing_dot` against
/// `win_trailing_space` — does NOT go in, and that is deliberate: those two do
/// not fold to the same thing under any Unicode key. What makes them collide
/// is that Windows TRIMS the tail of a name without the `\\?\` prefix, which
/// is path mangling and not folding. Catching that needs a different check,
/// and it belongs in #250's point 2 — the "this name means something else
/// there" warning — alongside `a\b`, `f:ads` and `CON`.
#[tokio::test]
async fn two_entries_that_fold_to_the_same_name_are_not_packed() {
    let corpus = norte_testkit::corpus::hostile_names();
    let bytes_of = |id: &str| {
        corpus
            .iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("fixture {id} is there"))
            .bytes
            .clone()
    };
    let pairs = [
        ("nfd_e_acute", "nfc_e_acute"),
        ("singleton_kelvin_sign", "ascii_capital_k"),
        ("micro_sign_mu", "greek_mu_twin"),
        ("ext4_full_fold_es_zett", "ext4_full_fold_ss"),
        // And the fifth fold, from #214: an invisible one. The full fold
        // DISCARDS `Default_Ignorable` codepoints, as the kernel's table does,
        // so two names that only differ by a soft hyphen are one when
        // extracted on a `+F`. This packing was rejected by the four above and
        // not by this one, and it is the one a reader cannot see coming: both
        // names render the same.
        ("full_fold_soft_hyphen", "full_fold_soft_hyphen_plain"),
    ];
    for (a, b) in pairs {
        let (one, other) = (bytes_of(a), bytes_of(b));
        assert_ne!(one, other, "[{a}/{b}] the premise: different bytes");

        let mem = Arc::new(MemProvider::new());
        mem.mkdir(&vp("mem:///p")).await.expect("p");
        for name in [&one, &other] {
            let p = vp("mem:///p").join(Segment::new(name.clone()).expect("segment"));
            let mut sink = mem.write(&p).await.expect("write");
            sink.write(Bytes::from_static(b"x")).await.expect("chunk");
            sink.commit().await.expect("commit");
        }
        let engine = Engine::new();
        engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);

        let h = engine
            .pack_as(
                pack_params(&["mem:///p"], "mem:///out.zip", "mem:///"),
                norte_core::journal::Actor::User,
            )
            .await
            .expect("starts");
        assert!(
            matches!(
                h.join().await,
                TaskState::Failed {
                    error: norte_proto::Error::Conflict { .. },
                    ..
                }
            ),
            "[{a}/{b}] both got packed: one gets lost on extraction"
        );
    }
}

/// #250 point 2 — a name that means SOMETHING ELSE outside is packed, and it
/// is reported.
///
/// The difference from the test above is what holds up the whole design: two
/// entries that fold to the same name make a file DISAPPEAR on extraction, so
/// they are rejected. `a\b.txt` extracted on Linux is still `a\b.txt` and on
/// Windows is a `b.txt` inside an `a` folder: nothing is lost, it is placed
/// differently. Rejecting it would take down legitimate Unix trees to prevent
/// something that is not even a loss.
#[tokio::test]
async fn a_name_that_means_something_else_outside_is_packed_and_reported() {
    let (engine, mem) = engine_with(&[
        ("mem:///p/normal.txt", b"1"),
        ("mem:///p/a\\b.txt", b"2"),
        ("mem:///p/CON", b"3"),
    ])
    .await;
    let h = engine
        .pack_as(
            pack_params(&["mem:///p"], "mem:///out.zip", "mem:///"),
            norte_core::journal::Actor::User,
        )
        .await
        .expect("starts");
    let id = h.id();
    assert_eq!(
        h.join().await,
        TaskState::Completed,
        "the archive IS WRITTEN"
    );
    assert!(!read(&mem, "mem:///out.zip").await.is_empty());

    let report = engine.archive_pack_report(id).expect("the ring has it").1;
    assert_eq!(
        report.checked,
        vec!["separator", "stream", "reserved", "trailing"],
        "the report declares WHAT it looked at: without that, a clean one asserts nothing"
    );

    let risks: Vec<(&str, &str)> = report
        .risky
        .iter()
        .map(|r| (r.name.as_str(), r.risk.as_str()))
        .collect();
    assert!(
        risks.contains(&("p/a\\b.txt", "separator")),
        "backslash is a separator in 7-Zip and in Explorer: {risks:?}"
    );
    assert!(
        risks.contains(&("p/CON", "reserved")),
        "`CON` does not extract on Windows at all: {risks:?}"
    );
    assert!(
        !risks.iter().any(|(n, _)| *n == "p/normal.txt"),
        "and the ordinary one is not named: a report that warns about everything is read by nobody"
    );
    assert!(report.entries >= 3, "how many were checked");
    assert!(!report.truncated);
}

/// And an archive whose names all travel intact leaves an EMPTY report, which
/// is not the same as not having looked: `entries` says so.
#[tokio::test]
async fn a_clean_archive_leaves_a_report_that_asserts_instead_of_staying_silent() {
    let (engine, _mem) =
        engine_with(&[("mem:///p/one.txt", b"1"), ("mem:///p/two.txt", b"2")]).await;
    let h = engine
        .pack_as(
            pack_params(&["mem:///p"], "mem:///out.zip", "mem:///"),
            norte_core::journal::Actor::User,
        )
        .await
        .expect("starts");
    let id = h.id();
    assert_eq!(h.join().await, TaskState::Completed);
    let report = engine.archive_pack_report(id).expect("report").1;
    assert!(report.risky.is_empty());
    assert!(
        report.entries >= 2,
        "they were looked at, which is what makes it useful"
    );
}

/// And two names that truly do NOT fold to the same thing get packed, which is
/// the normal case. The check must not cost the legitimate operation.
#[tokio::test]
async fn two_genuinely_different_names_do_get_packed() {
    let (engine, mem) =
        engine_with(&[("mem:///p/one.txt", b"1"), ("mem:///p/two.txt", b"2")]).await;
    let h = engine
        .pack_as(
            pack_params(&["mem:///p"], "mem:///out.zip", "mem:///"),
            norte_core::journal::Actor::User,
        )
        .await
        .expect("starts");
    assert_eq!(h.join().await, TaskState::Completed);
    assert!(!read(&mem, "mem:///out.zip").await.is_empty());
}
