//! `Engine::dir_size_as` integration (#139): how much a tree occupies, counted
//! as a cancelable Task, with the TOTAL in the progress and not in a new type.
//!
//! In-memory `MemProvider` → deterministic, without touching disk.

use std::sync::Arc;

use bytes::Bytes;
use norte_core::{Actor, Engine};
use norte_proto::methods::FsDirSizeParams;
use norte_proto::{Error as ProtoError, TaskState, VPath};
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
    engine.register_provider(Arc::clone(&mem) as Arc<dyn norte_vfs::Provider>);
    (engine, mem)
}

fn params(paths: &[&str]) -> FsDirSizeParams {
    FsDirSizeParams {
        paths: paths.iter().map(|p| vp(p)).collect(),
    }
}

/// What the feature promises: the size of a whole tree, subdirectories
/// included, in the Task's progress. No new types — the last snapshot IS the
/// result.
#[tokio::test]
async fn the_total_of_a_tree_travels_in_the_progress() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///root").await;
    mkdir(&mem, "mem:///root/sub").await;
    write_file(&mem, "mem:///root/a", 10).await;
    write_file(&mem, "mem:///root/sub/b", 32).await;
    write_file(&mem, "mem:///root/sub/c", 8).await;

    let handle = engine
        .dir_size_as(params(&["mem:///root"]), Actor::User)
        .await
        .expect("launches");
    let prog = handle.progress();
    assert_eq!(handle.join().await, TaskState::Completed);
    let p = prog.borrow().clone();
    assert_eq!(p.bytes_done, 50, "10 + 32 + 8");
    assert_eq!(p.entries_done, 4, "three files and the subdirectory");
    assert_eq!(
        p.bytes_total,
        Some(50),
        "when finished, the total is what was counted"
    );
    assert_eq!(p.entries_total, Some(4));
}

/// Measuring a LONE file is a legitimate question: it counts its own size and
/// walks nothing.
#[tokio::test]
async fn a_lone_file_counts_its_own_size() {
    let (engine, mem) = setup();
    write_file(&mem, "mem:///alone", 7).await;
    let handle = engine
        .dir_size_as(params(&["mem:///alone"]), Actor::User)
        .await
        .expect("launches");
    let prog = handle.progress();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(prog.borrow().bytes_done, 7);
    assert_eq!(prog.borrow().entries_done, 1);
}

/// SEVERAL roots add up to ONE number: what the human has marked is a
/// selection, and the question being asked is "how much does ALL of this
/// occupy?" Two roots that overlap are REJECTED, as in `fs.compare` and
/// `sync.plan` (#247).
///
/// Without this, `["mem:///p", "mem:///p/sub"]` counted `sub` TWICE and
/// returned a number bigger than what the place occupies — the opposite of
/// what the method exists to answer ("does this fit at the destination?"). It
/// is rejected instead of deduplicated: a pane's selection is never nested
/// (they are siblings), so nested roots come from a script, and there an
/// error is an answer.
#[tokio::test]
async fn two_overlapping_roots_are_rejected() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    mkdir(&mem, "mem:///p").await;
    mkdir(&mem, "mem:///p/sub").await;
    write_file(&mem, "mem:///p/sub/b", 23).await;

    let Err(err) = engine
        .dir_size_as(params(&["mem:///p", "mem:///p/sub"]), Actor::User)
        .await
    else {
        panic!("nested roots cannot be launched")
    };
    assert!(
        matches!(err, ProtoError::OverlappingRoots { .. }),
        "and it says so by name: {err:?}"
    );
    // The same root twice is the same problem wearing a different face.
    let Err(err) = engine
        .dir_size_as(params(&["mem:///p", "mem:///p"]), Actor::User)
        .await
    else {
        panic!("the same root twice, neither")
    };
    assert!(
        matches!(err, ProtoError::OverlappingRoots { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn several_roots_give_a_single_total() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///a").await;
    mkdir(&mem, "mem:///b").await;
    write_file(&mem, "mem:///a/one", 5).await;
    write_file(&mem, "mem:///b/two", 6).await;
    let handle = engine
        .dir_size_as(params(&["mem:///a", "mem:///b"]), Actor::User)
        .await
        .expect("launches");
    let prog = handle.progress();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(prog.borrow().bytes_done, 11);
}

/// Without paths there is no request, and it is rejected BEFORE creating any
/// Task: it is a REQUEST error, not the failure of something already launched.
#[tokio::test]
async fn no_paths_creates_no_task() {
    let (engine, _mem) = setup();
    let Err(err) = engine.dir_size_as(params(&[]), Actor::User).await else {
        panic!("measuring nothing is not a request");
    };
    assert!(matches!(err, ProtoError::InvalidPath), "{err:?}");
}

/// Rule 3: the count cancels cleanly, and the state says so.
#[tokio::test]
async fn counting_cancels_and_says_so() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///big").await;
    for i in 0..400 {
        write_file(&mem, &format!("mem:///big/f{i}"), 4).await;
    }
    let handle = engine
        .dir_size_as(params(&["mem:///big"]), Actor::User)
        .await
        .expect("launches");
    handle.cancel();
    assert_eq!(handle.join().await, TaskState::Cancelled);
}

/// A root that does not exist does not bring down the count of the others: a
/// selection of twenty folders is not lost over one. What comes out is the
/// size of what could be read, which is the honest answer to a question that
/// can no longer be exact.
#[tokio::test]
async fn an_unreadable_root_does_not_bring_down_the_count() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///good").await;
    write_file(&mem, "mem:///good/x", 9).await;
    let handle = engine
        .dir_size_as(
            params(&["mem:///does-not-exist", "mem:///good"]),
            Actor::User,
        )
        .await
        .expect("launches");
    let prog = handle.progress();
    assert_eq!(
        handle.join().await,
        TaskState::Completed,
        "the missing one is not a failure of the Task"
    );
    assert_eq!(prog.borrow().bytes_done, 9);
}

/// A directory adds no bytes and IS counted: the bottom number says how many
/// things there are, and the top one how much the ones that occupy something
/// occupy.
#[tokio::test]
async fn an_entry_with_no_size_is_counted_and_adds_nothing() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///d").await;
    mkdir(&mem, "mem:///d/sub").await;
    write_file(&mem, "mem:///d/f", 3).await;
    let handle = engine
        .dir_size_as(params(&["mem:///d"]), Actor::User)
        .await
        .expect("launches");
    let prog = handle.progress();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(prog.borrow().bytes_done, 3, "the dir adds no bytes");
    assert_eq!(prog.borrow().entries_done, 2, "but it is counted");
}

/// A LAZY provider: its listing carries no sizes (`None`), like the real local
/// one (#52 — `readdir` gives the kind and nothing else), and only `stat`
/// knows them.
struct Lazy {
    inner: Arc<MemProvider>,
    /// How many `stat`s were asked for: what proves hydration exists.
    stats: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl Provider for Lazy {
    fn scheme(&self) -> &str {
        self.inner.scheme()
    }
    fn capabilities(&self) -> norte_proto::Capabilities {
        self.inner.capabilities()
    }
    async fn stat(&self, p: &VPath) -> Result<norte_proto::Entry, ProtoError> {
        self.stats
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.stat(p).await
    }
    async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, ProtoError> {
        use futures::StreamExt as _;
        let mut inner = self.inner.list(p).await?;
        let mut items: Vec<Result<norte_proto::Entry, ProtoError>> = Vec::new();
        while let Some(e) = inner.next().await {
            items.push(e.map(|mut e| {
                e.size = None;
                e.mtime_ms = None;
                e
            }));
        }
        Ok(Box::pin(futures::stream::iter(items)))
    }
    async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, ProtoError> {
        self.inner.read(p, range).await
    }
    async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, ProtoError> {
        self.inner.write(p).await
    }
    async fn mkdir(&self, p: &VPath) -> Result<(), ProtoError> {
        self.inner.mkdir(p).await
    }
    async fn remove(&self, p: &VPath) -> Result<(), ProtoError> {
        self.inner.remove(p).await
    }
    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), ProtoError> {
        self.inner.rename(from, to).await
    }
}

/// **The regression that piloting the real TUI found**: against the local
/// provider, a listing carries NO sizes (#52), so summing what came in the
/// listing gave "0 B" for a whole tree — the most wrong possible answer to the
/// one question being asked. Sizes are requested with `stat`, as `du` does and
/// as a copy's hydration does.
#[tokio::test]
async fn with_a_lazy_listing_the_sizes_are_requested() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    let stats = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    engine.register_provider(Arc::new(Lazy {
        inner: Arc::clone(&mem),
        stats: Arc::clone(&stats),
    }) as Arc<dyn Provider>);
    mkdir(&mem, "mem:///p").await;
    mkdir(&mem, "mem:///p/sub").await;
    write_file(&mem, "mem:///p/a", 100).await;
    write_file(&mem, "mem:///p/sub/b", 23).await;

    let handle = engine
        .dir_size_as(params(&["mem:///p"]), Actor::User)
        .await
        .expect("launches");
    let prog = handle.progress();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(prog.borrow().bytes_done, 123, "the sizes were requested");
    assert_eq!(prog.borrow().entries_done, 3);
    assert!(
        stats.load(std::sync::atomic::Ordering::Relaxed) >= 2,
        "one stat per file without a size"
    );
}

/// An unreadable subtree gets COUNTED and travels (#251).
///
/// It used to go to a local counter, come out through a `tracing::info!`, and
/// the terminal snapshot said `Completed` with a safe total that was too
/// small. And this is the dangerous direction for the error: the method
/// exists to answer "does this fit at the destination?", so a silently short
/// number says yes to a copy that runs out of room halfway.
///
/// With the field, whoever paints it says "at least X". It is the twin of the
/// `confidence` `fs.compare` gives each row, and for the same reason: a count
/// without it cannot say it is a lower bound.
#[tokio::test]
async fn an_unreadable_subtree_is_counted_and_travels_in_the_progress() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///root").await;
    mkdir(&mem, "mem:///root/forbidden").await;
    write_file(&mem, "mem:///root/a", 10).await;
    write_file(&mem, "mem:///root/forbidden/secret", 1000).await;
    mem.faults().fail_list_at(&vp("mem:///root/forbidden"));

    let handle = engine
        .dir_size_as(params(&["mem:///root"]), Actor::User)
        .await
        .expect("launches");
    let prog = handle.progress();
    // Does not fail: counting what can be read is the useful answer. What it
    // cannot do is stay quiet about what it did not read.
    assert_eq!(handle.join().await, TaskState::Completed);
    let p = prog.borrow().clone();
    assert_eq!(
        p.bytes_done, 10,
        "the 1000 from the forbidden subtree do not go in"
    );
    assert_eq!(
        p.unreadable,
        Some(1),
        "and the terminal snapshot SAYS there was one that could not be read"
    );
}

/// And the ordinary case still says zero, which is what makes the field
/// legible: if it defaulted to one, "at least X" would always be painted.
#[tokio::test]
async fn a_whole_readable_tree_declares_no_unreadables() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///everything").await;
    write_file(&mem, "mem:///everything/a", 4).await;
    let handle = engine
        .dir_size_as(params(&["mem:///everything"]), Actor::User)
        .await
        .expect("launches");
    let prog = handle.progress();
    assert_eq!(handle.join().await, TaskState::Completed);
    // `Some(0)`, not `None`: "I counted and there were none" is one answer,
    // and `None` — "whoever emits this does not count it" — would be another.
    // A client that confuses them paints "at least X" over a total that IS
    // exact.
    assert_eq!(prog.borrow().unreadable, Some(0));
}
