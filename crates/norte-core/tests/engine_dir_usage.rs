//! `Engine::dir_usage_as` integration (phase 4): what a directory is MADE OF,
//! as a cancelable Task, with the children in a REPORT — because a list does
//! not fit in a Task's outcome nor in its progress.
//!
//! It is `engine_dir_size`'s sibling, with the question reversed: that one
//! answers "how much does this occupy?" with a number, and that is why the
//! progress was enough. A map needs the list, hence the report.
//!
//! In-memory `MemProvider` → deterministic, without touching disk.

use std::sync::Arc;

use bytes::Bytes;
use norte_core::{Actor, Engine};
use norte_proto::methods::FsDirUsageParams;
use norte_proto::{EntryKind, Error as ProtoError, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

async fn write_file(mem: &MemProvider, wire: &str, bytes: usize) {
    let mut sink = mem.write(&vp(wire)).await.expect("write opens");
    sink.write(Bytes::from(vec![b'x'; bytes]))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

async fn mkdir(mem: &MemProvider, wire: &str) {
    mem.mkdir(&vp(wire)).await.expect("mkdir");
}

fn setup() -> (Engine, Arc<MemProvider>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem)
}

fn params(wire: &str) -> FsDirUsageParams {
    FsDirUsageParams {
        path: vp(wire),
        depth: 1,
    }
}

/// Finds a child by its name in bytes, which is how they are named (rule 1).
fn child<'a>(
    report: &'a norte_proto::methods::FsDirUsageReportResult,
    name: &[u8],
) -> &'a norte_proto::methods::DirUsageChild {
    report
        .children
        .iter()
        .find(|c| c.name.as_bytes() == name)
        .unwrap_or_else(|| panic!("child {} is not there", String::from_utf8_lossy(name)))
}

/// What the feature promises: each child with what it occupies IN FULL,
/// subtree included. This is the difference from a listing — which gives the
/// node's own size — and the only thing a map can be painted from.
#[tokio::test]
async fn the_map_says_what_the_directory_is_made_of() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///root").await;
    mkdir(&mem, "mem:///root/sub").await;
    write_file(&mem, "mem:///root/lone", 10).await;
    write_file(&mem, "mem:///root/sub/b", 32).await;
    write_file(&mem, "mem:///root/sub/c", 8).await;

    let handle = engine
        .dir_usage_as(params("mem:///root"), Actor::User)
        .await
        .expect("launches");
    let id = handle.id();
    assert_eq!(handle.join().await, TaskState::Completed);

    let (_actor, report) = engine.dir_usage_report(id).expect("there is a map");
    assert!(report.listed, "the root's listing finished");
    assert_eq!(report.pending, 0, "finished: no child left to measure");
    assert_eq!(report.omitted, 0);
    assert_eq!(
        report.children.len(),
        2,
        "one lone file and one subdirectory"
    );

    let lone = child(&report, b"lone");
    assert_eq!(lone.kind, EntryKind::File);
    assert_eq!(lone.bytes, 10);
    assert_eq!(lone.entries, 1);
    assert!(!lone.partial);

    // What makes this a MAP: the directory weighs what its content weighs,
    // not zero.
    let sub = child(&report, b"sub");
    assert_eq!(sub.kind, EntryKind::Dir);
    assert_eq!(sub.bytes, 40, "32 + 8, the whole subtree");
    assert_eq!(sub.entries, 3, "itself and its two files");
    assert!(!sub.partial);

    assert_eq!(report.total_bytes, 50);
    assert_eq!(report.total_entries, 4);
}

/// A child's name is its BYTES (rule 1), not a lossy `String`: a name that is
/// not UTF-8 exists on disk and the map has to be able to name it.
#[tokio::test]
async fn a_childs_name_is_the_bytes_that_were_on_disk() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///root").await;
    write_file(&mem, "mem:///root/caf%FF.txt", 3).await;

    let handle = engine
        .dir_usage_as(params("mem:///root"), Actor::User)
        .await
        .expect("launches");
    let id = handle.id();
    assert_eq!(handle.join().await, TaskState::Completed);

    let (_actor, report) = engine.dir_usage_report(id).expect("map");
    assert_eq!(report.children.len(), 1);
    assert_eq!(
        report.children[0].name.as_bytes(),
        b"caf\xFF.txt",
        "the bytes come back as is, without going through a lossy conversion"
    );
}

/// A child that cannot be fully measured comes out MARKED, and the others get
/// measured.
///
/// `partial` is per child, not per report, which is the difference between
/// being able to paint the map or not: the incomplete rectangle is marked and
/// the rest stays true. A global flag can only turn off the whole map.
#[tokio::test]
async fn an_unreadable_child_comes_out_marked_and_does_not_bring_down_the_map() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///root").await;
    mkdir(&mem, "mem:///root/forbidden").await;
    mkdir(&mem, "mem:///root/open").await;
    write_file(&mem, "mem:///root/forbidden/secret", 1000).await;
    write_file(&mem, "mem:///root/open/x", 7).await;
    mem.faults().fail_list_at(&vp("mem:///root/forbidden"));

    let handle = engine
        .dir_usage_as(params("mem:///root"), Actor::User)
        .await
        .expect("launches");
    let id = handle.id();
    assert_eq!(
        handle.join().await,
        TaskState::Completed,
        "an unreadable child does NOT fail the Task"
    );

    let (_actor, report) = engine.dir_usage_report(id).expect("map");
    let forbidden = child(&report, b"forbidden");
    assert!(
        forbidden.partial,
        "its number is a lower bound, and it is DECLARED"
    );
    assert_eq!(forbidden.bytes, 0, "the 1000 could not be read");

    let open = child(&report, b"open");
    assert!(!open.partial, "the healthy sibling does not catch it");
    assert_eq!(open.bytes, 7);
}

/// What a FILE is made of is not a question: it is made of itself.
///
/// It is rejected instead of answering with a one-rectangle map, which is the
/// answer that looks useful and is not.
#[tokio::test]
async fn describing_a_file_is_not_a_question() {
    let (engine, mem) = setup();
    write_file(&mem, "mem:///alone", 7).await;

    let handle = engine
        .dir_usage_as(params("mem:///alone"), Actor::User)
        .await
        .expect("launches");
    // And it fails FOR WHAT IT IS. A bare `Failed` would also come from a
    // caught panic (`Error::Internal` with `panic: true`), so without looking
    // at the cause this test would pass the day measuring a file blew up.
    let TaskState::Failed { error } = handle.join().await else {
        panic!("what a file is made of is not a question");
    };
    assert!(matches!(error, ProtoError::InvalidPath), "{error:?}");
}

/// Depth is checked BEFORE creating any Task, and anything over the cap is
/// REJECTED instead of trimmed.
///
/// Trimming silently leaves the client believing it has the two levels it
/// asked for: it would paint a one-level map claiming it is two. That is why
/// `2` is `Unsupported` — "the method exists, that depth is not served" — and
/// not a `1` in disguise.
#[tokio::test]
async fn a_depth_that_is_not_served_is_rejected_and_not_trimmed() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///root").await;

    let zero = FsDirUsageParams {
        path: vp("mem:///root"),
        depth: 0,
    };
    let Err(err) = engine.dir_usage_as(zero, Actor::User).await else {
        panic!("describing zero levels is not a request");
    };
    assert!(matches!(err, ProtoError::InvalidPath), "{err:?}");

    let over = FsDirUsageParams {
        path: vp("mem:///root"),
        depth: norte_proto::methods::DIR_USAGE_MAX_DEPTH + 1,
    };
    let Err(err) = engine.dir_usage_as(over, Actor::User).await else {
        panic!("above the cap has to be rejected");
    };
    assert!(matches!(err, ProtoError::InvalidPath), "{err:?}");

    let two = FsDirUsageParams {
        path: vp("mem:///root"),
        depth: 2,
    };
    let Err(err) = engine.dir_usage_as(two, Actor::User).await else {
        panic!("today only one level is served, and it says so");
    };
    assert!(
        matches!(err, ProtoError::Unsupported),
        "\"not served\", which is not \"invalid\": {err:?}"
    );
}

/// Rule 3: the map cancels cleanly WHILE IN PROGRESS, and the report is left
/// half done saying so.
///
/// The first version of this test cancelled right after launching, and proved
/// nothing: the body runs in another spawn, so the cancellation arrived before
/// the first entry, the report stayed at `default()`, and both asserts passed
/// on their own. It would have passed the same way with ALL the cancellation
/// checks removed — which is the definition of a green test that proves
/// nothing.
///
/// So the cancellation is armed WHERE time passes: at the third `list`, i.e.
/// with the root already listed and one child already measured, while the
/// next one is being measured. Deterministic and clockless — a `sleep` would
/// get it right by chance.
///
/// The state pinned here can only be produced by a cut halfway through
/// measuring: there is one measured child AND the listing did not finish.
/// That is what `listed` exists to say: without it, a map with one child reads
/// the same as a directory that only has one.
#[tokio::test]
async fn cancelling_mid_measurement_leaves_the_map_marked_incomplete() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///big").await;
    mkdir(&mem, "mem:///big/a").await;
    mkdir(&mem, "mem:///big/b").await;
    write_file(&mem, "mem:///big/a/x", 10).await;
    write_file(&mem, "mem:///big/b/y", 20).await;

    let handle = engine
        .dir_usage_as(params("mem:///big"), Actor::User)
        .await
        .expect("launches");
    // 1 = the root, 2 = `a`'s subtree, 3 = `b`'s: cut at the third one.
    mem.faults().cancel_after_lists(3, handle.cancel_token());
    let id = handle.id();
    assert_eq!(handle.join().await, TaskState::Cancelled);

    let (_actor, report) = engine.dir_usage_report(id).expect("there is a map");
    assert!(
        !report.listed,
        "the listing did NOT finish, and the report cannot imply it did"
    );
    assert_eq!(
        report.children.len(),
        1,
        "`a` was measured and it cut at `b`: if it were 0 the cancellation \
         arrived before starting and this test does not prove the inner loop stops"
    );
}

/// Above the cap the BIGGEST ones survive, and what is omitted is still
/// counted in the totals even though it loses its name.
///
/// The huge child is created LAST on purpose. With children all the same
/// size, this test used to pass under any policy — the first N and the
/// biggest N are the same set — which is exactly how keeping the first ones
/// slipped in: the protocol promises the biggest ones, and a map that sends
/// the 400 GB child to `omitted` to paint 4096 trifles is the function not
/// existing.
///
/// What the cap takes is the NAME, not the size: that is why the totals still
/// add up and a map can paint the rest as one more rectangle.
#[tokio::test]
async fn above_the_cap_the_biggest_ones_survive() {
    let (engine, mem) = setup();
    let cap = norte_proto::methods::DIR_USAGE_MAX_CHILDREN;
    mkdir(&mem, "mem:///many").await;
    for i in 0..cap {
        write_file(&mem, &format!("mem:///many/f{i}"), 2).await;
    }
    // The last one to arrive and the biggest of all.
    write_file(&mem, "mem:///many/huge", 100_000).await;

    let handle = engine
        .dir_usage_as(params("mem:///many"), Actor::User)
        .await
        .expect("launches");
    let id = handle.id();
    assert_eq!(handle.join().await, TaskState::Completed);

    let (_actor, report) = engine.dir_usage_report(id).expect("map");
    assert_eq!(report.children.len(), cap, "the list stops at the cap");
    assert_eq!(report.omitted, 1, "and says how many were left out");
    assert!(
        report.children.iter().any(|c| c.name.as_bytes() == b"huge"),
        "the biggest travels even arriving last: that is what a map paints"
    );
    assert_eq!(
        report.total_bytes,
        (cap as u64) * 2 + 100_000,
        "the totals count EVERYONE, including the one not named"
    );
    assert_eq!(report.total_entries, cap as u64 + 1);
}

/// A provider that ADMITS it left entries out does not produce a map that
/// claims "this is everything".
///
/// An archive's index leaves out what it cannot represent and counts it in
/// `list_skipped` (#93). Without looking at it, the report came out
/// `listed: true`, `omitted: 0` and every child `partial: false` — the three
/// signals of completeness at once, over a listing the provider itself said
/// was incomplete.
///
/// It goes to `unvisited` and not to `omitted` because `omitted` promises the
/// bytes of what was omitted ARE in the totals, and those of an entry nobody
/// listed are not.
#[tokio::test]
async fn a_listing_the_provider_trimmed_is_not_announced_as_complete() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new().with_list_skipped(3));
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    mkdir(&mem, "mem:///archive").await;
    write_file(&mem, "mem:///archive/visible", 5).await;

    let handle = engine
        .dir_usage_as(params("mem:///archive"), Actor::User)
        .await
        .expect("launches");
    let prog = handle.progress();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        prog.borrow().unvisited,
        Some(3),
        "the tree is bigger than what was walked, and it says so"
    );
}

/// Two children with DIFFERENT bytes that collapse to the same lossy name are
/// still two children, with their bytes intact.
///
/// `\xFF.rs` and `\xFE.rs` both turn into `�.rs` as soon as someone does a
/// lossy conversion. With a single hostile name the test cannot tell "passes
/// the bytes through" from "folds them": it needs BOTH, which is exactly why
/// the corpus carries them paired. If someone slips a `to_string_lossy` into
/// this path, the map paints one rectangle where there were two and
/// `nav.enter` opens the wrong one.
#[tokio::test]
async fn two_children_that_collapse_to_the_same_lossy_name_stay_two() {
    let (engine, mem) = setup();
    let root = vp("mem:///hostile");
    mem.mkdir(&root).await.expect("mkdir");

    let fixture = |id: &str| {
        norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("fixture {id} in the corpus"))
            .bytes
    };
    let ff = fixture("lossy_collapse_ff");
    let fe = fixture("lossy_collapse_fe");
    for bytes in [ff.clone(), fe.clone()] {
        let seg = norte_proto::Segment::new(bytes).expect("valid segment");
        let mut sink = mem.write(&root.join(seg)).await.expect("write opens");
        sink.write(Bytes::from_static(b"xy")).await.expect("chunk");
        sink.commit().await.expect("commit");
    }

    let handle = engine
        .dir_usage_as(params("mem:///hostile"), Actor::User)
        .await
        .expect("launches");
    let id = handle.id();
    assert_eq!(handle.join().await, TaskState::Completed);

    let (_actor, report) = engine.dir_usage_report(id).expect("map");
    assert_eq!(
        report.children.len(),
        2,
        "two different names, two children: a lossy fold would make them one"
    );
    assert!(
        report.children.iter().any(|c| c.name.as_bytes() == ff),
        "the 0xFF bytes come back as is"
    );
    assert!(
        report.children.iter().any(|c| c.name.as_bytes() == fe),
        "and so do the 0xFE ones, distinct from the others"
    );
}

/// An id that was never a map has no report — and that is what the daemon
/// turns into `NotFound` for whoever asks about someone else's.
#[tokio::test]
async fn a_foreign_id_has_no_map() {
    let (engine, _mem) = setup();
    assert!(
        engine
            .dir_usage_report(norte_proto::TaskId::new(4242))
            .is_none()
    );
}
