//! `Engine::checksum_as` integration (#311): the content digest of a batch of
//! files, as a cancelable Task, with the digests in a REPORT — because N sums
//! do not fit in a Task's outcome nor in its progress.
//!
//! In-memory `MemProvider` → deterministic, without touching disk.

use std::sync::Arc;

use bytes::Bytes;
use norte_core::{Actor, Engine};
use norte_proto::methods::{ChecksumAlgo, ChecksumMiss, FsChecksumParams};
use norte_proto::{Error as ProtoError, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write opens");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

fn setup() -> (Engine, Arc<MemProvider>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn norte_vfs::Provider>);
    (engine, mem)
}

fn params(paths: &[&str]) -> FsChecksumParams {
    FsChecksumParams {
        paths: paths.iter().map(|p| vp(p)).collect(),
        algo: ChecksumAlgo::Sha256,
    }
}

/// The digest of an empty file and of one with content, against the values any
/// `sha256sum` publishes: if this drifted, checking against an outside sum
/// would stop being useful for anything — which is exactly what it exists for.
#[tokio::test]
async fn the_digests_are_sha256sums() {
    let (engine, mem) = setup();
    write_file(&mem, "mem:///empty", b"").await;
    write_file(&mem, "mem:///abc", b"abc").await;

    let handle = engine
        .checksum_as(params(&["mem:///empty", "mem:///abc"]), Actor::User)
        .await
        .expect("launches");
    let id = handle.id();
    assert_eq!(handle.join().await, TaskState::Completed);

    let (_actor, report) = engine.checksum_report(id).expect("there is a report");
    assert_eq!(report.pending, 0, "finished: nothing left to resolve");
    assert_eq!(report.entries.len(), 2);
    assert_eq!(
        report.entries[0].digest.as_deref(),
        Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
        "the sha256 of the empty file"
    );
    assert_eq!(
        report.entries[1].digest.as_deref(),
        Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
        "the sha256 of `abc`"
    );
}

/// The report's order is the REQUEST's. A report that reordered itself could
/// not be compared against the list one sent.
#[tokio::test]
async fn the_report_keeps_the_requested_order() {
    let (engine, mem) = setup();
    write_file(&mem, "mem:///a", b"a").await;
    write_file(&mem, "mem:///b", b"b").await;
    write_file(&mem, "mem:///c", b"c").await;

    let handle = engine
        .checksum_as(params(&["mem:///c", "mem:///a", "mem:///b"]), Actor::User)
        .await
        .expect("launches");
    let id = handle.id();
    assert_eq!(handle.join().await, TaskState::Completed);
    let (_actor, report) = engine.checksum_report(id).expect("report");
    let paths: Vec<String> = report.entries.iter().map(|e| e.path.to_wire()).collect();
    assert_eq!(paths, vec!["mem:///c", "mem:///a", "mem:///b"]);
}

/// What could not be read and what was not a file come out with their REASON,
/// and do not bring down the batch: checking a hundred files cannot die on the
/// one someone just moved.
#[tokio::test]
async fn the_unreadable_and_non_file_entries_come_out_with_their_reason() {
    let (engine, mem) = setup();
    write_file(&mem, "mem:///good", b"hello").await;
    mem.mkdir(&vp("mem:///folder")).await.expect("mkdir");

    let handle = engine
        .checksum_as(
            params(&["mem:///good", "mem:///folder", "mem:///does-not-exist"]),
            Actor::User,
        )
        .await
        .expect("launches");
    let id = handle.id();
    assert_eq!(
        handle.join().await,
        TaskState::Completed,
        "an unreadable entry does NOT fail the Task"
    );

    let (_actor, report) = engine.checksum_report(id).expect("report");
    assert_eq!(report.entries.len(), 3, "all three paths come out");
    assert!(
        report.entries[0].digest.is_some(),
        "the good one does have a sum"
    );
    assert_eq!(report.entries[1].miss, Some(ChecksumMiss::NotAFile));
    assert_eq!(report.entries[2].miss, Some(ChecksumMiss::Unreadable));
    assert!(
        report.entries[1].digest.is_none() && report.entries[2].digest.is_none(),
        "no sum when there is a reason: the two fields are mutually exclusive"
    );
}

/// The empty list is rejected BEFORE any Task is created: summing nothing is
/// not a request, and a REQUEST rejection is not a launched Task's failure.
#[tokio::test]
async fn no_paths_means_no_task() {
    let (engine, _mem) = setup();
    let Err(err) = engine.checksum_as(params(&[]), Actor::User).await else {
        panic!("an empty list has to be rejected");
    };
    assert!(matches!(err, ProtoError::InvalidPath), "{err:?}");
}

/// Above the cap it is REJECTED, not trimmed: a silently trimmed report reads
/// as "everything checked" for files nobody looked at.
#[tokio::test]
async fn above_the_cap_it_is_rejected() {
    let (engine, _mem) = setup();
    let many: Vec<VPath> = (0..=norte_proto::methods::FS_CHECKSUM_MAX_PATHS)
        .map(|i| vp(&format!("mem:///f{i}")))
        .collect();
    let Err(err) = engine
        .checksum_as(
            FsChecksumParams {
                paths: many,
                algo: ChecksumAlgo::Sha256,
            },
            Actor::User,
        )
        .await
    else {
        panic!("above the cap has to be rejected");
    };
    assert!(matches!(err, ProtoError::InvalidPath), "{err:?}");
}

/// Hard rule 3: the batch cancels cleanly, the state says so, and the report
/// **is left half done saying so too**.
///
/// `pending > 0` with the Task already terminal is the signal that what is
/// there is not everything. Without it, a frontend comparing against a sums
/// file would accuse — "does not match or is missing" — files nobody ever got
/// to read, which is the worst possible error in the one tool whose job is to
/// verify.
#[tokio::test]
async fn cancelling_leaves_the_report_marked_incomplete() {
    let (engine, mem) = setup();
    let mut paths = Vec::new();
    for i in 0..400 {
        let wire = format!("mem:///f{i}");
        write_file(&mem, &wire, b"content").await;
        paths.push(vp(&wire));
    }
    let handle = engine
        .checksum_as(
            FsChecksumParams {
                paths,
                algo: ChecksumAlgo::Sha256,
            },
            Actor::User,
        )
        .await
        .expect("launches");
    let id = handle.id();
    handle.cancel();
    assert_eq!(handle.join().await, TaskState::Cancelled);

    let (_actor, report) = engine.checksum_report(id).expect("there is a report");
    assert!(
        report.pending > 0,
        "cancelled halfway: what is missing has to keep counting, \
         not drop to zero as if the batch had finished"
    );
    assert!(
        report.entries.len() < 400,
        "if all 400 were there, nothing was cancelled and the test proves nothing"
    );
}

/// The report says WHAT it was computed with. It can be requested without
/// having sent the request — `task.list` shows other actors' tasks — so
/// defaulting to sha256 would paint digests of something else the day there is
/// a second algorithm.
#[tokio::test]
async fn the_report_names_its_algorithm() {
    let (engine, mem) = setup();
    write_file(&mem, "mem:///x", b"abc").await;
    let handle = engine
        .checksum_as(params(&["mem:///x"]), Actor::User)
        .await
        .expect("launches");
    let id = handle.id();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        engine.checksum_report(id).expect("report").1.algo,
        ChecksumAlgo::Sha256
    );
}

/// An id that was never a checksum batch has no report — and that is what the
/// daemon turns into `NotFound` for whoever asks about someone else's.
#[tokio::test]
async fn a_foreign_id_has_no_report() {
    let (engine, _mem) = setup();
    assert!(
        engine
            .checksum_report(norte_proto::TaskId::new(4242))
            .is_none()
    );
}
