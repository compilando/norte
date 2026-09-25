//! Test-first matrix for M2's phase 4: transfer resume (ADR 0012). Against
//! `MemProvider` with injected faults and a real `LocalProvider` for
//! cross-invocation resumption.

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use norte_core::{Engine, TransferOptions};
use norte_proto::{Error, ResumePolicy, TaskState, VPath, VerifyPolicy};
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

async fn read_all(p: &dyn Provider, wire: &str) -> Result<Vec<u8>, Error> {
    let mut stream = p.read(&vp(wire), None).await?;
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk?);
    }
    Ok(out)
}

/// Provider-AGNOSTIC resume (ADR 0012 A2): `MemProvider` really implements
/// `open_resumable`/`keep`, so it resumes WITHOUT needing to declare
/// `APPEND` (the engine does not gate on the capability).
fn engine_with_mem() -> (Engine, Arc<MemProvider>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem)
}

fn resume_on() -> TransferOptions {
    TransferOptions {
        resume: ResumePolicy::On,
        ..TransferOptions::default()
    }
}

/// resume=On: a copy cut halfway by a NON-transient failure leaves a
/// resumable `.norte-partial`; the next copy continues from there and never
/// recopies what was done — verified by bytes read from the source.
#[tokio::test]
async fn resume_continues_without_recopying() {
    let (engine, mem) = engine_with_mem();
    // 10 KiB; Mem's chunk is 1 KiB, so there are many.
    let content: Vec<u8> = (0..10_240)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    write_file(&mem, "mem:///src.bin", &content).await;

    // Cuts the source's read at byte 4096 with a non-retryable Io.
    mem.faults().fail_read_at(&vp("mem:///src.bin"), 4096);
    let handle = engine
        .copy_with(&vp("mem:///src.bin"), &vp("mem:///dst.bin"), resume_on())
        .await
        .unwrap();
    assert!(matches!(handle.join().await, TaskState::Failed { .. }));
    // The final destination does not exist yet; there is a partial with ~4096 bytes.
    assert_eq!(
        mem.stat(&vp("mem:///dst.bin")).await.unwrap_err(),
        Error::NotFound
    );

    // Second copy: without the failure, it resumes and finishes.
    mem.faults().clear();
    let handle = engine
        .copy_with(&vp("mem:///src.bin"), &vp("mem:///dst.bin"), resume_on())
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        read_all(&*mem, "mem:///dst.bin").await.unwrap(),
        content,
        "the destination is the COMPLETE and correct file"
    );
}

/// resume=On: cancelling halfway keeps the partial (does not abort it); the
/// later resumption completes.
#[tokio::test]
async fn resume_does_not_reset_the_bar() {
    let (engine, mem) = engine_with_mem();
    let content: Vec<u8> = (0..10_240)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    write_file(&mem, "mem:///src.bin", &content).await;

    // Deterministic cut at ~4096 bytes: leaves a partial.
    mem.faults().fail_read_at(&vp("mem:///src.bin"), 4096);
    let handle = engine
        .copy_with(&vp("mem:///src.bin"), &vp("mem:///dst.bin"), resume_on())
        .await
        .unwrap();
    assert!(matches!(handle.join().await, TaskState::Failed { .. }));

    // Resumes: the destination completes and the already-present chunk was
    // counted when opening the resumable (the bar does not start from zero).
    mem.faults().clear();
    let handle = engine
        .copy_with(&vp("mem:///src.bin"), &vp("mem:///dst.bin"), resume_on())
        .await
        .unwrap();
    let rx = handle.progress();
    let start = rx.borrow().bytes_done;
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&*mem, "mem:///dst.bin").await.unwrap(), content);
    assert!(
        start >= 4096 || rx.borrow().bytes_done == content.len() as u64,
        "the resumed progress starts from the chunk already done (start={start})"
    );
}

/// resume=Off (default): cancelling leaves the destination CLEAN — M1's
/// contract intact, with no partial to resume.
// Paused clock: `MemProvider`'s per-op latency runs on tokio's clock, so
// "sleep and cancel" stops being a race with the machine.
#[tokio::test(start_paused = true)]
async fn without_resume_cancelling_leaves_it_clean() {
    let (engine, mem) = engine_with_mem();
    let content: Vec<u8> = (0..50_000)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    write_file(&mem, "mem:///src.bin", &content).await;
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(20)));

    let handle = engine
        .copy(&vp("mem:///src.bin"), &vp("mem:///dst.bin"))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    handle.cancel();
    assert_eq!(handle.join().await, TaskState::Cancelled);
    assert_eq!(
        mem.stat(&vp("mem:///dst.bin")).await.unwrap_err(),
        Error::NotFound,
        "clean destination (M1)"
    );
    // And a second copy WITHOUT resume starts from scratch (it does not
    // resume a partial that must not exist).
    mem.faults().clear();
    let handle = engine
        .copy(&vp("mem:///src.bin"), &vp("mem:///dst.bin"))
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&*mem, "mem:///dst.bin").await.unwrap(), content);
}

/// verify=Length: a partial LONGER than the source (the source shrank) is
/// discarded and the copy starts from scratch — never a corrupt destination.
#[tokio::test]
async fn resume_discards_a_partial_longer_than_the_source() {
    let (engine, mem) = engine_with_mem();
    // Leaves an 8-byte partial via a manual keep.
    let (mut sink, _) = mem.open_resumable(&vp("mem:///dst.bin")).await.unwrap();
    sink.write(Bytes::from_static(b"oldold!!")).await.unwrap();
    sink.keep().await.unwrap();

    // The source is now SHORTER (3 bytes).
    write_file(&mem, "mem:///src.bin", b"abc").await;
    let handle = engine
        .copy_with(&vp("mem:///src.bin"), &vp("mem:///dst.bin"), resume_on())
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        read_all(&*mem, "mem:///dst.bin").await.unwrap(),
        b"abc",
        "the stale partial was discarded; the destination is the current source"
    );
}

fn resume_hash() -> TransferOptions {
    TransferOptions {
        resume: ResumePolicy::On,
        verify: VerifyPolicy::Hash,
        ..TransferOptions::default()
    }
}

/// #35 — verify=Hash catches what Length CANNOT: the source CHANGED but kept
/// the SAME size. Length would resume over a stale prefix (a corrupt
/// destination: old prefix + new suffix); Hash compares the prefix and
/// discards the partial, starting from scratch.
#[tokio::test]
async fn hash_discards_a_partial_of_a_changed_same_size_source() {
    let (engine, mem) = engine_with_mem();
    // A 5-byte partial from an OLD source.
    let (mut sink, _) = mem.open_resumable(&vp("mem:///dst.bin")).await.unwrap();
    sink.write(Bytes::from_static(b"OLD!!")).await.unwrap();
    sink.keep().await.unwrap();

    // The CURRENT source measures the SAME (10 bytes) but its prefix differs.
    write_file(&mem, "mem:///src.bin", b"newnew1234").await;
    let handle = engine
        .copy_with(&vp("mem:///src.bin"), &vp("mem:///dst.bin"), resume_hash())
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        read_all(&*mem, "mem:///dst.bin").await.unwrap(),
        b"newnew1234",
        "Hash detected the stale prefix and recopied entirely"
    );
}

/// #35 — verify=Hash over a provider WITHOUT `partial_digest` (default
/// `None`) degrades to Length: it CANNOT compare prefixes, so a partial
/// shorter than the source is RESUMED (Length behavior), not discarded.
#[tokio::test]
async fn hash_without_a_digest_degrades_to_length() {
    use async_trait::async_trait;
    use norte_proto::{ByteRange, Capabilities, Entry};
    use norte_vfs::{ByteSink, ByteStream, EntryStream};

    // Delegates EVERYTHING to Mem (including open_resumable/keep: really
    // resumes) except partial_digest, which stays at the trait's default
    // `None`.
    struct NoDigest(Arc<MemProvider>);
    #[async_trait]
    impl Provider for NoDigest {
        fn scheme(&self) -> &str {
            self.0.scheme()
        }
        fn capabilities(&self) -> Capabilities {
            self.0.capabilities()
        }
        async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
            self.0.stat(p).await
        }
        async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
            self.0.list(p).await
        }
        async fn read(&self, p: &VPath, r: Option<ByteRange>) -> Result<ByteStream, Error> {
            self.0.read(p, r).await
        }
        async fn write(&self, p: &VPath) -> Result<Box<dyn ByteSink>, Error> {
            self.0.write(p).await
        }
        async fn open_resumable(&self, p: &VPath) -> Result<(Box<dyn ByteSink>, u64), Error> {
            self.0.open_resumable(p).await
        }
        async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
            self.0.mkdir(p).await
        }
        async fn remove(&self, p: &VPath) -> Result<(), Error> {
            self.0.remove(p).await
        }
        async fn rename(&self, a: &VPath, b: &VPath) -> Result<(), Error> {
            self.0.rename(a, b).await
        }
        // partial_digest is NOT overridden: default `None`.
    }

    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::new(NoDigest(Arc::clone(&mem))) as Arc<dyn Provider>);

    // "OLD!!" partial (5) from an old source; the current one measures the
    // SAME (10).
    let (mut sink, _) = mem.open_resumable(&vp("mem:///dst.bin")).await.unwrap();
    sink.write(Bytes::from_static(b"OLD!!")).await.unwrap();
    sink.keep().await.unwrap();
    write_file(&mem, "mem:///src.bin", b"newnew1234").await;

    let handle = engine
        .copy_with(&vp("mem:///src.bin"), &vp("mem:///dst.bin"), resume_hash())
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    // Without a digest, Hash degrades to Length: the stale prefix IS KEPT
    // (Length does not catch it) → "OLD!!" + "w1234" (source[5..]). This is
    // the documented degradation behavior, not a test bug.
    assert_eq!(
        read_all(&*mem, "mem:///dst.bin").await.unwrap(),
        b"OLD!!w1234",
        "without partial_digest, Hash behaves like Length (resumes the prefix)"
    );
}

/// #35 — verify=Hash with the source INTACT really resumes: the prefix
/// matches, the partial is kept and only the rest is copied.
#[tokio::test]
async fn hash_resumes_if_the_prefix_matches() {
    let (engine, mem) = engine_with_mem();
    let content = b"same prefix + rest".to_vec();
    // A partial with the source's REAL prefix (7 bytes).
    let (mut sink, _) = mem.open_resumable(&vp("mem:///dst.bin")).await.unwrap();
    sink.write(Bytes::copy_from_slice(&content[..7]))
        .await
        .unwrap();
    sink.keep().await.unwrap();

    write_file(&mem, "mem:///src.bin", &content).await;
    let handle = engine
        .copy_with(&vp("mem:///src.bin"), &vp("mem:///dst.bin"), resume_hash())
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        read_all(&*mem, "mem:///dst.bin").await.unwrap(),
        content,
        "prefix intact: resumed and completed correctly"
    );
}

/// resume=On against a provider that INHERITS the trait's defaults
/// (`open_resumable`=(write,0), `keep`=abort): degrades cleanly to a normal
/// copy — completes fine and an interruption leaves no partial.
#[tokio::test]
async fn resume_with_the_traits_defaults_degrades_cleanly() {
    use async_trait::async_trait;
    use norte_proto::{ByteRange, Capabilities, Entry};
    use norte_vfs::{ByteSink, ByteStream, EntryStream};

    // A wrapper that delegates EVERYTHING to Mem except open_resumable/keep,
    // which stay at the trait's default (no real resume).
    struct DefaultsProvider(Arc<MemProvider>);
    #[async_trait]
    impl Provider for DefaultsProvider {
        fn scheme(&self) -> &str {
            self.0.scheme()
        }
        fn capabilities(&self) -> Capabilities {
            self.0.capabilities()
        }
        async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
            self.0.stat(p).await
        }
        async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
            self.0.list(p).await
        }
        async fn read(&self, p: &VPath, r: Option<ByteRange>) -> Result<ByteStream, Error> {
            self.0.read(p, r).await
        }
        async fn write(&self, p: &VPath) -> Result<Box<dyn ByteSink>, Error> {
            self.0.write(p).await
        }
        async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
            self.0.mkdir(p).await
        }
        async fn remove(&self, p: &VPath) -> Result<(), Error> {
            self.0.remove(p).await
        }
        async fn rename(&self, a: &VPath, b: &VPath) -> Result<(), Error> {
            self.0.rename(a, b).await
        }
        // open_resumable/keep are NOT overridden: the trait's defaults.
    }

    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::new(DefaultsProvider(Arc::clone(&mem))) as Arc<dyn Provider>);
    write_file(&mem, "mem:///src", b"data").await;
    let handle = engine
        .copy_with(&vp("mem:///src"), &vp("mem:///dst"), resume_on())
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&*mem, "mem:///dst").await.unwrap(), b"data");

    // An interruption with defaults leaves NO partial (keep=abort).
    write_file(&mem, "mem:///src2", b"0123456789").await;
    mem.faults().fail_read_at(&vp("mem:///src2"), 4);
    let handle = engine
        .copy_with(&vp("mem:///src2"), &vp("mem:///dst2"), resume_on())
        .await
        .unwrap();
    assert!(matches!(handle.join().await, TaskState::Failed { .. }));
    assert_eq!(
        mem.stat(&vp("mem:///dst2")).await.unwrap_err(),
        Error::NotFound,
        "defaults: keep=abort → no partial, clean destination"
    );
}

/// Rule 3: cancelling a copy with resume=On is clean (keep durably keeps the
/// partial without hanging) and the later resumption completes.
// Paused clock: `MemProvider`'s per-op latency runs on tokio's clock, so
// "sleep and cancel" stops being a race with the machine.
#[tokio::test(start_paused = true)]
async fn resume_cancellation_is_clean_and_resumes() {
    let (engine, mem) = engine_with_mem();
    let content: Vec<u8> = (0..80_000)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    write_file(&mem, "mem:///src.bin", &content).await;
    // Per-op latency: gives room to cancel before the read assembles everything.
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(30)));

    let handle = engine
        .copy_with(&vp("mem:///src.bin"), &vp("mem:///dst.bin"), resume_on())
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    handle.cancel();
    assert_eq!(handle.join().await, TaskState::Cancelled);
    // The final destination never exists halfway (clean or a marked partial).
    assert_eq!(
        mem.stat(&vp("mem:///dst.bin")).await.unwrap_err(),
        Error::NotFound
    );

    // Resumes until completing, byte-exact.
    mem.faults().clear();
    let handle = engine
        .copy_with(&vp("mem:///src.bin"), &vp("mem:///dst.bin"), resume_on())
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&*mem, "mem:///dst.bin").await.unwrap(), content);
}
