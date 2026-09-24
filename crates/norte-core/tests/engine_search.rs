//! `Engine::search_as` integration (M4 live search, T3): the cancelable BFS
//! walker emits hits in batches over a channel, honors `max_hits`, skips
//! unreadable entries without aborting, and validates criteria BEFORE
//! creating the Task. In-memory `MemProvider` → deterministic, without
//! touching disk.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use norte_core::{Actor, Engine};
use norte_proto::methods::{FsSearchParams, MatchInfo, SearchHits};
use norte_proto::{
    ByteRange, Capabilities, Entry, EntryKind, Error as ProtoError, TaskState, VPath,
};
use norte_testkit::MemProvider;
use norte_vfs::{ByteSink, ByteStream, EntryStream, Provider};
use tokio::sync::mpsc::Receiver;

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

async fn mkdir(mem: &MemProvider, wire: &str) {
    mem.mkdir(&vp(wire)).await.expect("mkdir");
}

/// Engine + in-memory `MemProvider` registered under `mem`.
fn setup() -> (Engine, Arc<MemProvider>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem)
}

/// Base params: only the root, everything else empty/false.
fn params(root: &str) -> FsSearchParams {
    FsSearchParams::new(vp(root))
}

/// Drains the channel until it closes; returns `(entry, match)` flattened.
async fn drain(mut rx: Receiver<SearchHits>) -> Vec<(Entry, Option<MatchInfo>)> {
    let mut out = Vec::new();
    while let Some(hits) = rx.recv().await {
        let matches = hits.matches.unwrap_or_default();
        for (i, e) in hits.entries.into_iter().enumerate() {
            out.push((e, matches.get(i).cloned()));
        }
    }
    out
}

fn paths(hits: &[(Entry, Option<MatchInfo>)]) -> Vec<String> {
    let mut p: Vec<String> = hits.iter().map(|(e, _)| e.path.display_lossy()).collect();
    p.sort();
    p
}

/// A wire path's display form (to compare against [`paths`]).
fn disp(wire: &str) -> String {
    vp(wire).display_lossy()
}

// 1 ───────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn name_only_finds_recursively() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///a").await;
    mkdir(&mem, "mem:///a/sub").await;
    write_file(&mem, "mem:///a/x.rs", b"").await;
    write_file(&mem, "mem:///a/sub/y.rs", b"").await;
    write_file(&mem, "mem:///a/sub/z.txt", b"").await;

    let mut p = params("mem:///a");
    p.name_glob = Some("*.rs".to_owned());
    let (h, rx) = engine.search_as(p, Actor::User).await.expect("search");
    let hits = drain(rx).await;
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(
        paths(&hits),
        vec![disp("mem:///a/sub/y.rs"), disp("mem:///a/x.rs")]
    );
    // Pure name: no content context.
    assert!(hits.iter().all(|(_, m)| m.is_none()));
}

// 2 ───────────────────────────────────────────────────────────────────────
// NOTE: the accented Spanish content below ("año", "niño", etc.) is
// deliberate — the accented bytes are what these encoding-detection tests
// exercise, so they are kept as is (not translated), per the plan's rule for
// content whose non-ASCII bytes are the point.
#[tokio::test]
async fn multiencoding_content_skips_binaries() {
    let (engine, mem) = setup();
    write_file(&mem, "mem:///f1.txt", "hay un año aquí\n".as_bytes()).await;
    // Latin-1: a sentence with several high bytes so the detector locks onto it.
    write_file(
        &mem,
        "mem:///f2.txt",
        b"El ni\xF1o comi\xF3 en el jard\xEDn hace un a\xF1o entero\n",
    )
    .await;
    // Binary: NUL + legacy-needle bytes → detect=Binary, it is skipped.
    write_file(&mem, "mem:///f3.bin", b"\x00\x00a\xF1o binario").await;

    let mut p = params("mem:///");
    p.content = Some("año".to_owned());
    let (h, rx) = engine.search_as(p, Actor::User).await.expect("search");
    let hits = drain(rx).await;
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(
        paths(&hits),
        vec![disp("mem:///f1.txt"), disp("mem:///f2.txt")]
    );
    // Content context populated (line + preview) in both.
    for (_, m) in &hits {
        let m = m.as_ref().expect("content match info");
        assert_eq!(m.line, Some(1));
        assert!(m.preview.as_ref().is_some_and(|s| s.contains('a')));
    }
}

// 3 ───────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn clean_cancellation_closes_the_channel() {
    let (engine, mem) = setup();
    for i in 0..200 {
        write_file(
            &mem,
            &format!("mem:///f{i:03}.txt"),
            b"contains ano and more\n",
        )
        .await;
    }
    // Latency per op: each read of the walker takes time, there is time to cancel.
    mem.faults()
        .set_latency_per_op(Some(Duration::from_millis(3)));

    let mut p = params("mem:///");
    p.content = Some("ano".to_owned());
    let (h, mut rx) = engine.search_as(p, Actor::User).await.expect("search");

    // Wait for the first batch (the search has already started) and cancel.
    let first = rx.recv().await.expect("first batch");
    assert!(!first.entries.is_empty());
    h.cancel();

    // The channel closes (tx drop) and the Task ends Cancelled.
    let mut got = first.entries.len();
    while let Some(b) = rx.recv().await {
        got += b.entries.len();
    }
    assert!(got < 200, "cancelled halfway: {got} < 200");
    assert_eq!(h.join().await, TaskState::Cancelled);
}

// 4 ───────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn max_hits_truncates_and_completes() {
    let (engine, mem) = setup();
    for i in 0..10 {
        write_file(&mem, &format!("mem:///m{i}.rs"), b"").await;
    }
    let mut p = params("mem:///");
    p.name_glob = Some("*.rs".to_owned());
    p.max_hits = Some(3);
    let (h, rx) = engine.search_as(p, Actor::User).await.expect("search");
    let hits = drain(rx).await;
    assert_eq!(hits.len(), 3, "exactly max_hits");
    assert_eq!(h.join().await, TaskState::Completed);
}

// 5 ───────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn per_entry_errors_do_not_abort() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///good").await;
    mkdir(&mem, "mem:///bad").await;
    write_file(&mem, "mem:///good/hit.rs", b"").await;
    write_file(&mem, "mem:///bad/other.rs", b"").await;
    // Listing `mem:///bad` fails: the walker skips it and continues.
    mem.faults().fail_list_at(&vp("mem:///bad"));

    let mut p = params("mem:///");
    p.name_glob = Some("*.rs".to_owned());
    let (h, rx) = engine.search_as(p, Actor::User).await.expect("search");
    let hits = drain(rx).await;
    assert_eq!(h.join().await, TaskState::Completed);
    // Only the file from the readable subdir appears.
    assert_eq!(paths(&hits), vec![disp("mem:///good/hit.rs")]);
}

// 6 ───────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn invalid_criteria_fail_before_the_task() {
    let (engine, _mem) = setup();

    // Zero criteria.
    let r = engine.search_as(params("mem:///"), Actor::User).await;
    assert!(r.is_err(), "no criteria = Err before the Task");

    // Glob AND name regex at once (mutually exclusive per axis).
    let mut p = params("mem:///");
    p.name_glob = Some("*.rs".to_owned());
    p.name_regex = Some("^.*$".to_owned());
    assert!(engine.search_as(p, Actor::User).await.is_err());

    // content AND content_regex at once.
    let mut p = params("mem:///");
    p.content = Some("x".to_owned());
    p.content_regex = Some("x".to_owned());
    assert!(engine.search_as(p, Actor::User).await.is_err());

    // A glob that does not compile.
    let mut p = params("mem:///");
    p.name_glob = Some("a[".to_owned());
    assert!(engine.search_as(p, Actor::User).await.is_err());
}

/// Encodes `s` to UTF-16 with a BOM (LE or BE).
fn utf16_bom(s: &str, le: bool) -> Vec<u8> {
    let mut out = if le {
        vec![0xFF, 0xFE]
    } else {
        vec![0xFE, 0xFF]
    };
    for cu in s.encode_utf16() {
        out.extend_from_slice(&if le {
            cu.to_le_bytes()
        } else {
            cu.to_be_bytes()
        });
    }
    out
}

// review MAJOR ── a match on a line ≥2 of a UTF-16 file is NOT lost ─────────
#[tokio::test]
async fn utf16_match_on_a_later_line_le_and_be() {
    let (engine, mem) = setup();
    // "x\naño\n": the match is on LINE 2 — cutting on the raw 0x0A byte would
    // misalign the pairs and lose the match (the review's regression).
    write_file(&mem, "mem:///le.txt", &utf16_bom("x\naño\n", true)).await;
    write_file(&mem, "mem:///be.txt", &utf16_bom("x\naño\n", false)).await;

    let mut p = params("mem:///");
    p.content = Some("año".to_owned());
    let (h, rx) = engine.search_as(p, Actor::User).await.expect("search");
    let hits = drain(rx).await;
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(
        paths(&hits),
        vec![disp("mem:///be.txt"), disp("mem:///le.txt")]
    );
    // And the reported line is 2 (not 1).
    for (_, m) in &hits {
        assert_eq!(m.as_ref().and_then(|m| m.line), Some(2));
    }
}

// review MINOR-1 ── a giant line does not blow up RAM (it is truncated) ─────
#[tokio::test]
async fn a_giant_line_is_truncated_but_matches_at_the_start() {
    let (engine, mem) = setup();
    // Content regex → the decode-by-lines path. A single 4 MiB line with the
    // needle at the START: it must match even though the line gets truncated
    // for the match.
    let mut giant = b"MATCH_ME ".to_vec();
    giant.extend(std::iter::repeat_n(b'a', 4 * 1024 * 1024));
    write_file(&mem, "mem:///huge.txt", &giant).await;

    let mut p = params("mem:///");
    p.content_regex = Some("MATCH_ME".to_owned());
    let (h, rx) = engine.search_as(p, Actor::User).await.expect("search");
    let hits = drain(rx).await;
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(paths(&hits), vec![disp("mem:///huge.txt")]);
    // The preview is bounded (it does not dump 4 MiB).
    let preview = hits[0].1.as_ref().and_then(|m| m.preview.clone()).unwrap();
    assert!(preview.chars().count() <= 160);
}

// 7 ── encoding-aware case with three files (debt T2) ──────────────────────
#[tokio::test]
async fn encoding_aware_content_three_files() {
    let (engine, mem) = setup();
    // Latin-1: "año" = 0xF1; a long sentence for stable detection.
    write_file(
        &mem,
        "mem:///year_latin1.txt",
        b"Este documento cumple un a\xF1o; el ni\xF1o so\xF1\xF3 en espa\xF1ol.\n",
    )
    .await;
    // UTF-16LE with BOM: "un año\n" → routed through DECODE, not byte-scan.
    let mut u16 = vec![0xFF, 0xFE];
    for cu in "un año\n".encode_utf16() {
        u16.extend_from_slice(&cu.to_le_bytes());
    }
    write_file(&mem, "mem:///year_utf16bom.txt", &u16).await;
    // CJK in UTF-8: contains 0xF1 as a LEAD byte (U+44001) — must NOT match
    // the short Latin needle with encoding-aware search. Canonical corpus
    // fixture (`cjk_utf8_lead_f1`), not an ad-hoc literal.
    let cjk = norte_testkit::corpus::content_fixtures()
        .into_iter()
        .find(|f| f.id == "cjk_utf8_lead_f1")
        .expect("fixture in the corpus");
    assert!(cjk.bytes.contains(&0xF1), "lead 0xF1");
    write_file(&mem, "mem:///cjk_utf8.txt", &cjk.bytes).await;

    let mut p = params("mem:///");
    p.content = Some("año".to_owned());
    let (h, rx) = engine.search_as(p, Actor::User).await.expect("search");
    let hits = drain(rx).await;
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(
        paths(&hits),
        vec![
            disp("mem:///year_latin1.txt"),
            disp("mem:///year_utf16bom.txt"),
        ],
        "Latin-1 and UTF-16-BOM match; the CJK-UTF8 does not (false positive avoided)"
    );
}

// A1 (encoding, HIGH): sanitizing the preview AT THE SOURCE ────────────────
/// A match whose preview carries an RLO + an unclosed isolate + ESC+OSC + raw
/// C0 (canonical fixture `preview_bidi_ctrl_injection`) MUST come out over the
/// wire already sanitized: no `is_terminal_hazard` char survives (the
/// consumer — the fs.search MCP tool — would paint it directly). Rule §6:
/// never raw controls/bidi, and it applies to the PRODUCER.
#[tokio::test]
async fn a_hostile_content_preview_comes_out_sanitized_at_the_source() {
    // NOTE: "aguja" (needle) below is fixture data baked into
    // `norte-testkit`'s `PREVIEW_BIDI_CTRL_INJECTION` corpus fixture (owned by
    // another task) and kept verbatim — see the T05 report's cross-file
    // literals.
    let fixture = norte_testkit::corpus::content_fixtures()
        .into_iter()
        .find(|f| f.id == "preview_bidi_ctrl_injection")
        .expect("fixture in the corpus");
    let (engine, mem) = setup();
    write_file(&mem, "mem:///hostile.txt", &fixture.bytes).await;

    let mut p = params("mem:///");
    p.content = Some("aguja".to_owned());
    let (h, rx) = engine.search_as(p, Actor::User).await.expect("search");
    let hits = drain(rx).await;
    assert_eq!(h.join().await, TaskState::Completed);

    let preview = hits
        .iter()
        .find_map(|(_, m)| m.as_ref().and_then(|m| m.preview.clone()))
        .expect("there is a match preview");
    // The clean needle is still there, but no raw hazard.
    assert!(
        preview.contains("aguja"),
        "keeps the readable text: {preview:?}"
    );
    assert!(
        !preview.chars().any(norte_encoding::is_terminal_hazard),
        "the preview carries no raw controls/bidi/invisibles: {preview:?}"
    );
    // And what was masked came out as U+FFFD (marked, not silently dropped).
    assert!(
        preview.contains('\u{FFFD}'),
        "hazards come out as �: {preview:?}"
    );
}

// 10 (security T4, MEDIUM) ──────────────────────────────────────────────────
/// A provider that delegates to a real `MemProvider` but, when listing
/// `inject_under`, ADDS fabricated entries whose path is OUTSIDE the subtree —
/// simulates a buggy (or malicious) provider that returns NON-descendants.
/// `run_walk` must ignore them entirely (defense in depth: the search's scope
/// is a HARD invariant of the core, it does not trust the list's correctness).
struct RogueList {
    inner: Arc<MemProvider>,
    inject_under: VPath,
    inject: Vec<Entry>,
}

#[async_trait]
impl Provider for RogueList {
    fn scheme(&self) -> &str {
        self.inner.scheme()
    }
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
    async fn stat(&self, p: &VPath) -> Result<Entry, ProtoError> {
        self.inner.stat(p).await
    }
    async fn list(&self, p: &VPath) -> Result<EntryStream, ProtoError> {
        let mut inner = self.inner.list(p).await?;
        let mut items: Vec<Result<Entry, ProtoError>> = Vec::new();
        while let Some(e) = inner.next().await {
            items.push(e);
        }
        // Injects the non-descendants ONLY when listing the attack dir.
        if p == &self.inject_under {
            for e in &self.inject {
                items.push(Ok(e.clone()));
            }
        }
        Ok(Box::pin(futures::stream::iter(items)))
    }
    async fn read(&self, p: &VPath, range: Option<ByteRange>) -> Result<ByteStream, ProtoError> {
        self.inner.read(p, range).await
    }
    async fn write(&self, p: &VPath) -> Result<Box<dyn ByteSink>, ProtoError> {
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

#[tokio::test]
async fn walk_ignores_entries_outside_the_root_even_if_the_provider_lists_them() {
    let mem = Arc::new(MemProvider::new());
    // Inside the attack's root: a genuine file with the needle.
    mkdir(&mem, "mem:///proj").await;
    write_file(&mem, "mem:///proj/inside.txt", "año dentro".as_bytes()).await;
    // OUTSIDE the root: a file with the needle and a dir with a child carrying
    // the needle.
    write_file(&mem, "mem:///secret.txt", "año secreto".as_bytes()).await;
    mkdir(&mem, "mem:///other").await;
    write_file(&mem, "mem:///other/hidden.txt", "año oculto".as_bytes()).await;

    // The provider injects those non-descendants when listing mem:///proj.
    let rogue = Arc::new(RogueList {
        inner: Arc::clone(&mem),
        inject_under: vp("mem:///proj"),
        inject: vec![
            Entry {
                attrs: std::collections::BTreeMap::new(),
                path: vp("mem:///secret.txt"),
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            },
            Entry {
                attrs: std::collections::BTreeMap::new(),
                path: vp("mem:///other"),
                kind: EntryKind::Dir,
                size: None,
                mtime_ms: None,
            },
        ],
    });
    let engine = Engine::new();
    engine.register_provider(rogue as Arc<dyn Provider>);

    let mut p = params("mem:///proj");
    p.content = Some("año".into());
    let (h, rx) = engine.search_as(p, Actor::User).await.expect("search");
    let hits = drain(rx).await;
    assert_eq!(h.join().await, TaskState::Completed);
    // ONLY the genuine descendant: the non-descendants are never read
    // (secret) nor descended into (other/hidden). Confinement does not depend
    // on the provider.
    assert_eq!(paths(&hits), vec![disp("mem:///proj/inside.txt")]);
}
