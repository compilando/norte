//! #394 (ADR 0166): a run of files is copied several at a time — and that
//! changes nothing a reader can observe except the time: every published
//! file is journaled, a failure or a cancel never leaves one that undo does
//! not know, and the bar ends exactly at its total.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::Bytes;
use futures::StreamExt as _;
use norte_core::{Engine, Journal, MutationObserver, SqliteJournal};
use norte_proto::{EntryKind, Error as ProtoError, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::{ByteSink, Provider};

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

/// Open sinks right now, and the most there ever were.
#[derive(Default)]
struct Gauge {
    open: AtomicUsize,
    peak: AtomicUsize,
}

/// `MemProvider` whose sinks take a moment to commit, are counted, and
/// whose write to `fails` (if any) is refused for good.
struct Slow {
    inner: Arc<MemProvider>,
    gauge: Arc<Gauge>,
    fails: Option<VPath>,
}

struct SlowSink {
    inner: Box<dyn ByteSink>,
    gauge: Arc<Gauge>,
}

#[async_trait::async_trait]
impl ByteSink for SlowSink {
    async fn write(&mut self, chunk: Bytes) -> Result<(), ProtoError> {
        self.inner.write(chunk).await
    }
    async fn commit(self: Box<Self>) -> Result<(), ProtoError> {
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let gauge = Arc::clone(&self.gauge);
        let r = self.inner.commit().await;
        gauge.open.fetch_sub(1, Ordering::SeqCst);
        r
    }
    async fn abort(self: Box<Self>) -> Result<(), ProtoError> {
        self.gauge.open.fetch_sub(1, Ordering::SeqCst);
        self.inner.abort().await
    }
}

#[async_trait::async_trait]
impl Provider for Slow {
    fn scheme(&self) -> &str {
        self.inner.scheme()
    }
    fn capabilities(&self) -> norte_proto::Capabilities {
        self.inner.capabilities()
    }
    async fn stat(&self, p: &VPath) -> Result<norte_proto::Entry, ProtoError> {
        self.inner.stat(p).await
    }
    async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, ProtoError> {
        self.inner.list(p).await
    }
    async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, ProtoError> {
        self.inner.read(p, range).await
    }
    async fn write(&self, p: &VPath) -> Result<Box<dyn ByteSink>, ProtoError> {
        if self.fails.as_ref() == Some(p) {
            return Err(ProtoError::Io { retryable: false });
        }
        let inner = self.inner.write(p).await?;
        let now = self.gauge.open.fetch_add(1, Ordering::SeqCst) + 1;
        self.gauge.peak.fetch_max(now, Ordering::SeqCst);
        Ok(Box::new(SlowSink {
            inner,
            gauge: Arc::clone(&self.gauge),
        }))
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

/// Engine with a journal over a `Slow` `MemProvider`, and a source of
/// `files` files in two folders.
async fn setup(
    files: usize,
    fails: Option<&str>,
) -> (Engine, Arc<MemProvider>, Arc<SqliteJournal>, Arc<Gauge>) {
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("journal"),
    ));
    let engine = Engine::with_observer(Arc::clone(&journal) as Arc<dyn MutationObserver>);
    let mem = Arc::new(MemProvider::new());
    let gauge = Arc::new(Gauge::default());
    engine.register_provider(Arc::new(Slow {
        inner: Arc::clone(&mem),
        gauge: Arc::clone(&gauge),
        fails: fails.map(vp),
    }) as Arc<dyn Provider>);
    mem.mkdir(&vp("mem:///src")).await.expect("src");
    mem.mkdir(&vp("mem:///src/sub")).await.expect("sub");
    for i in 0..files {
        let mut sink = mem.write(&vp(&source(i))).await.expect("write");
        sink.write(Bytes::from(vec![b'x'; 100 + i]))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }
    (engine, mem, journal, gauge)
}

fn source(i: usize) -> String {
    let dir = if i.is_multiple_of(2) {
        "src"
    } else {
        "src/sub"
    };
    format!("mem:///{dir}/f{i:02}")
}

/// Every FILE under `mem:///dst`.
async fn copied(mem: &MemProvider) -> Vec<VPath> {
    let mut out = Vec::new();
    let mut dirs = vec![vp("mem:///dst")];
    while let Some(d) = dirs.pop() {
        let Ok(mut list) = mem.list(&d).await else {
            continue;
        };
        while let Some(e) = list.next().await {
            let e = e.expect("entry");
            match e.kind {
                EntryKind::Dir => dirs.push(e.path),
                _ => out.push(e.path),
            }
        }
    }
    out
}

/// Every file that reached the destination has its `created` row: none of
/// them is left for undo not to know.
async fn every_copied_file_is_journaled(mem: &MemProvider, journal: &SqliteJournal) {
    let rows = journal.journal().entries().await.expect("entries");
    for f in copied(mem).await {
        let wire = f.to_wire();
        assert!(
            rows.iter()
                .any(|r| r.op == "created" && r.path == wire.as_bytes()),
            "{wire} was published with no journal entry"
        );
    }
}

/// Twenty files in two folders: written more than one at a time, every one
/// arrives whole, and the bar ends exactly at its total.
#[tokio::test(flavor = "multi_thread")]
async fn files_are_copied_several_at_a_time() {
    let (engine, mem, journal, gauge) = setup(20, None).await;
    let handle = engine
        .copy(&vp("mem:///src"), &vp("mem:///dst"))
        .await
        .expect("launches");
    let progress = handle.progress();
    assert_eq!(handle.join().await, TaskState::Completed);

    assert!(gauge.peak.load(Ordering::SeqCst) > 1, "one at a time");
    for i in 0..20 {
        let target = source(i).replace("/src", "/dst");
        let e = mem.stat(&vp(&target)).await.expect("copied");
        assert_eq!(e.size, Some(100 + i as u64), "{target} whole");
    }
    let last = progress.borrow().clone();
    assert_eq!(
        Some(last.bytes_done),
        last.bytes_total,
        "the bar ends at its total"
    );
    every_copied_file_is_journaled(&mem, &journal).await;
}

/// One file of a run fails while its siblings are committing: the task
/// fails, and the siblings that DID publish are journaled — they are not
/// dropped halfway with their rename applied and no `created`.
#[tokio::test(flavor = "multi_thread")]
async fn a_failing_file_does_not_orphan_its_siblings() {
    let (engine, mem, journal, _gauge) = setup(20, Some("mem:///dst/f04")).await;
    let handle = engine
        .copy(&vp("mem:///src"), &vp("mem:///dst"))
        .await
        .expect("launches");
    assert!(matches!(handle.join().await, TaskState::Failed { .. }));
    assert!(!copied(&mem).await.is_empty(), "some sibling got through");
    every_copied_file_is_journaled(&mem, &journal).await;
}

/// Cancelled with files in flight (hard rule 3): it ends `Cancelled`, and
/// what got published is journaled.
#[tokio::test(flavor = "multi_thread")]
async fn cancelling_with_files_in_flight_journals_what_landed() {
    let (engine, mem, journal, gauge) = setup(40, None).await;
    let handle = engine
        .copy(&vp("mem:///src"), &vp("mem:///dst"))
        .await
        .expect("launches");
    while gauge.peak.load(Ordering::SeqCst) < 2 {
        tokio::task::yield_now().await;
    }
    handle.cancel();
    assert_eq!(handle.join().await, TaskState::Cancelled);
    every_copied_file_is_journaled(&mem, &journal).await;
    assert_eq!(gauge.open.load(Ordering::SeqCst), 0, "no sink left open");
}

/// Paused with files in flight, some of them finishing DURING the pause,
/// and resumed: the copy completes. Awaiting the next entry's pause alone,
/// with the file that lent the slot unpolled in the set, it used to hang.
#[tokio::test(flavor = "multi_thread")]
async fn a_pause_with_files_in_flight_resumes_to_the_end() {
    let (engine, mem, journal, gauge) = setup(40, None).await;
    let handle = engine
        .copy(&vp("mem:///src"), &vp("mem:///dst"))
        .await
        .expect("launches");
    let mut progress = handle.progress();
    while gauge.peak.load(Ordering::SeqCst) < 2 {
        tokio::task::yield_now().await;
    }
    handle.pause_gate().pause();
    progress
        .wait_for(|p| p.state == TaskState::Paused)
        .await
        .expect("pauses");
    // The files that were committing finish meanwhile.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    handle.pause_gate().resume();
    let ended = tokio::time::timeout(std::time::Duration::from_secs(30), handle.join())
        .await
        .expect("the copy does not hang after a resume");
    assert_eq!(ended, TaskState::Completed);
    assert_eq!(copied(&mem).await.len(), 40);
    every_copied_file_is_journaled(&mem, &journal).await;
}
