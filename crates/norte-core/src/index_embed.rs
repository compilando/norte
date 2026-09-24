//! `index.embed` Task (M4-IA-2, ADR 0031 A3): embeddings for files already
//! indexed. Filters (`denied_prefixes`, text heuristic, size) BEFORE reading
//! anything; reads prefixes capped by the provider (rule 2); the prefix's
//! sha256 decides whether to re-embed; batches to the provider with capped
//! retry on rate-limit. Cooperative cancellation per file (rule 3).

use std::sync::Arc;

use futures::StreamExt;
use norte_proto::{Error, VPath};
use norte_vfs::Provider;
use sha2::{Digest, Sha256};

use crate::scheduler::TaskCtx;

/// Prefix bytes embedded per file (v1, hard-coded — spec §IA-2).
pub(crate) const EMBED_PREFIX_BYTES: u64 = 32 * 1024;
/// Batch size toward the provider (v1, hard-coded).
pub(crate) const EMBED_BATCH: usize = 16;
/// Larger files are skipped (a giant binary's prefix isn't useful text).
pub(crate) const EMBED_MAX_FILE_SIZE: u64 = 8 * 1024 * 1024;
/// Retries on `RateLimited` before failing the task (spec: never hangs).
pub(crate) const EMBED_RETRY_MAX: u32 = 3;

/// Extensions considered text (v1 heuristic). Bytes, not strings: filenames
/// are not UTF-8 (rule 1).
const EMBED_TEXT_EXTS: &[&[u8]] = &[
    b"txt", b"md", b"rst", b"org", b"tex", b"rs", b"py", b"js", b"ts", b"tsx", b"jsx", b"go",
    b"java", b"kt", b"rb", b"php", b"pl", b"lua", b"c", b"h", b"cpp", b"hpp", b"cc", b"hh", b"cs",
    b"sh", b"bash", b"zsh", b"fish", b"toml", b"json", b"yaml", b"yml", b"xml", b"html", b"htm",
    b"css", b"sql", b"csv", b"ini", b"cfg", b"conf", b"log",
];

/// An embedding candidate? Decided WITHOUT reading the content (v1
/// extension heuristic; content sniffing is declared debt in the spec).
/// Unknown size (`None`) passes: the later read is capped anyway.
pub(crate) fn is_text_candidate(path: &VPath, size: Option<u64>) -> bool {
    if size.is_some_and(|s| s > EMBED_MAX_FILE_SIZE) {
        return false;
    }
    let Some(name) = path.file_name() else {
        return false;
    };
    let bytes = name.as_bytes();
    let Some(dot) = bytes.iter().rposition(|b| *b == b'.') else {
        return false;
    };
    let ext = &bytes[dot + 1..];
    if ext.is_empty() || ext.len() > 4 {
        return false;
    }
    EMBED_TEXT_EXTS.contains(&ext.to_ascii_lowercase().as_slice())
}

/// Cosine similarity. `None` if the dimensions differ, a vector is null, or
/// the result isn't finite (a belt: a `NaN` serialized by `serde_json`
/// becomes `null` and poisons the whole response on the client — the
/// wire's score is ALWAYS finite).
///
/// No callers outside the tests since #122 —search passes the norm
/// already computed— and it stays because it's the DEFINITION the shortcut
/// is checked against to confirm no score changed.
#[cfg(test)]
pub(crate) fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    let na: f32 = a.iter().map(|x| x * x).sum();
    cosine_prenormed(a, na.sqrt(), b)
}

/// [`cosine`] with `a`'s norm ALREADY computed (#122).
///
/// Semantic search scores the SAME query against every stored vector, and
/// recomputing its norm per row was a third of the whole sweep's
/// multiplications. The caller already has it: it computes it beforehand,
/// to reject a zero-norm query vector.
///
/// `norm_a` is passed as the root, not squared, because that's what goes
/// into the denominator — having one of the two roots done and not the
/// other would be half the savings and all the confusion.
pub(crate) fn cosine_prenormed(a: &[f32], norm_a: f32, b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let (mut dot, mut nb) = (0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        nb += y * y;
    }
    let denom = norm_a * nb.sqrt();
    // `>` is false for 0.0 and for NaN: both cases ⇒ None.
    if denom > 0.0 {
        let s = dot / denom;
        s.is_finite().then_some(s)
    } else {
        None
    }
}

/// A hit with its score, orderable, for semantic search's capped heap
/// (#122).
///
/// `Ord` IS the RESULT order —descending score and, on ties, ascending
/// path—, i.e. "better" is `Less`. Exactly because of that, `BinaryHeap`,
/// which is a max-heap and pops the LARGEST, pops the WORST of the `k`
/// stored: the only one every new candidate needs to be compared against.
/// Nothing needs inverting, and doing so —as it used to— puts the best on
/// top and drops the good ones one by one.
///
/// The path tiebreak isn't cosmetic: without it, two files with the same
/// score came out in whatever order `SQLite` returned them, and a repeated
/// search could answer with two different lists.
#[derive(Debug, Clone)]
pub(crate) struct Puntuado {
    pub score: f64,
    pub path: norte_proto::VPath,
}

impl Puntuado {
    /// The RESULT order: best first.
    fn mejor_first(&self, other: &Self) -> std::cmp::Ordering {
        other
            .score
            .total_cmp(&self.score)
            .then_with(|| self.path.cmp(&other.path))
    }
}

impl PartialEq for Puntuado {
    fn eq(&self, other: &Self) -> bool {
        self.mejor_first(other) == std::cmp::Ordering::Equal
    }
}

impl Eq for Puntuado {}

impl Ord for Puntuado {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Not inverted: see the type's note.
        self.mejor_first(other)
    }
}

impl PartialOrd for Puntuado {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// The `k` best of `candidates`, in result order, in O(k) memory.
///
/// It used to score everything into a `Vec` as long as the index, sort the
/// whole thing, and truncate to `k <= 100`. The heap does the same math
/// while storing at most `k`, which is what's going to be returned.
pub(crate) fn mejores_k(
    candidates: impl Iterator<Item = Puntuado>,
    k: usize,
) -> Vec<(norte_proto::VPath, f64)> {
    let mut heap: std::collections::BinaryHeap<Puntuado> =
        std::collections::BinaryHeap::with_capacity(k);
    for c in candidates {
        if heap.len() < k {
            heap.push(c);
        } else if let Some(worst) = heap.peek()
            && c.mejor_first(worst) == std::cmp::Ordering::Less
        {
            // `Less` in result order = BETTER than the worst one stored.
            heap.pop();
            heap.push(c);
        }
    }
    let mut out = heap.into_vec();
    out.sort_unstable_by(Puntuado::mejor_first);
    out.into_iter().map(|p| (p.path, p.score)).collect()
}

/// Clamps `k` to the wire range `[1, INDEX_SEMANTIC_MAX_K]`
/// (`index.search_semantic`).
pub(crate) fn clamp_k(k: u32) -> usize {
    // Invariant: MAX_K=100 fits in usize on any platform.
    usize::try_from(k.clamp(1, norte_proto::methods::INDEX_SEMANTIC_MAX_K))
        .expect("MAX_K fits in usize")
}

/// Maps a [`norte_index::IndexError`] to the wire's taxonomy (same
/// criterion as `index_build_as`: `SQLite`'s `BUSY`/`LOCKED` ⇒ retryable).
pub(crate) fn index_to_proto(e: &norte_index::IndexError) -> Error {
    tracing::warn!(error = %e, "index.embed: index error");
    Error::Io {
        retryable: e.is_retryable(),
    }
}

/// Reads the first [`EMBED_PREFIX_BYTES`] of `path` via the provider. `len`
/// caps it at the provider; the break + truncate are the belt in case one
/// delivers more (mirrors `handle_plugin_preview`).
///
/// **What it is gets checked again before reading it (#122).** The
/// candidate comes from the row `index.build` left behind, and between that
/// build and this embed there's room for a substitution: whoever can write
/// into the indexed tree swaps a `.txt` for a link to a DENIED file, and its
/// 32 KiB would go to the embeddings provider — which would make "not a
/// byte of a denied prefix is ever read" stop being true. `Provider::stat`
/// describes the LINK and never its target, so requiring `File` here closes
/// the door; what's left is the window between this `stat` and the `read`,
/// which is the standard mitigation, not the absence of one.
///
/// What this does NOT cover, and it has to be said: a HARD LINK to the
/// denied file. It has `kind = File` and a path the filter doesn't
/// recognize, so it passes with no race at all. Closing that requires
/// comparing inodes against the denied set, which is a different matter.
async fn read_prefix(provider: &dyn Provider, path: &VPath) -> Result<Vec<u8>, Error> {
    if provider.stat(path).await?.kind != norte_proto::EntryKind::File {
        tracing::debug!(
            path = %crate::engine::span_path(path),
            "index.embed: the candidate is no longer a regular file; not reading it"
        );
        return Err(Error::Conflict {
            conflict: norte_proto::ConflictKind::TypeMismatch,
        });
    }
    let range = norte_proto::ByteRange {
        offset: 0,
        len: Some(EMBED_PREFIX_BYTES),
    };
    let mut stream = provider.read(path, Some(range)).await?;
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        bytes.extend_from_slice(&chunk);
        if bytes.len() as u64 >= EMBED_PREFIX_BYTES {
            break;
        }
    }
    let cap = usize::try_from(EMBED_PREFIX_BYTES).unwrap_or(usize::MAX);
    bytes.truncate(cap.min(bytes.len()));
    Ok(bytes)
}

/// Sends `batch` to the provider (with capped retry on rate-limit) and
/// persists the vectors. Leaves `batch` empty on completion. The retry wait
/// is cancel-aware (rule 3: the retry loop is an inner loop — a cancel must
/// not wait up to 30s for the sleep to expire).
async fn flush_batch(
    embedder: &dyn norte_ai::AiProvider,
    index: &norte_index::Index,
    model: &str,
    batch: &mut Vec<(i64, String, [u8; 32])>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if batch.is_empty() {
        return Ok(());
    }
    let texts: Vec<String> = batch.iter().map(|(_, t, _)| t.clone()).collect();
    let mut attempt: u32 = 0;
    let vectors = loop {
        match embedder.embed(&texts).await {
            Ok(v) => break v,
            Err(norte_ai::AiError::RateLimited { retry_after })
                if attempt + 1 < EMBED_RETRY_MAX =>
            {
                // Capped wait: whatever the server asks for (clamped to
                // 30s) or 1s.
                let secs = retry_after.unwrap_or(1).min(30);
                tracing::debug!(attempt, secs, "provider rate-limited; retrying");
                tokio::select! {
                    () = ctx.cancel.cancelled() => return Err(Error::Cancelled),
                    () = tokio::time::sleep(std::time::Duration::from_secs(secs)) => {}
                }
                attempt += 1;
            }
            Err(e) => return Err(crate::engine::ai_to_proto_error(&e)),
        }
    };
    if vectors.len() != batch.len() {
        // Lying provider: it said N entries and returned something else.
        // Zipping blindly would associate vectors with the wrong files —
        // better to fail. This is PROVIDER misbehavior (not our bug):
        // ProviderUnavailable taxonomy, not retryable (repeating won't fix it).
        tracing::warn!(
            expected = batch.len(),
            got = vectors.len(),
            "index.embed: the provider returned an unexpected number of vectors"
        );
        return Err(Error::ProviderUnavailable { retryable: false });
    }
    // Same class of lie as above, and hence the same taxonomy (#122): a
    // zero-dimension vector scores against nothing, but stored it leaves the
    // file MARKED as embedded —its hash matches— and no later `index.embed`
    // retries it. `upsert_embedding` rejects it too, in case someone
    // someday writes through another path.
    if let Some(i) = vectors.iter().position(Vec::is_empty) {
        tracing::warn!(
            file_id = batch.get(i).map(|(id, _, _)| *id),
            "index.embed: the provider returned a zero-dimension vector"
        );
        return Err(Error::ProviderUnavailable { retryable: false });
    }
    for ((file_id, _, hash), vec) in batch.iter().zip(vectors.iter()) {
        index
            .upsert_embedding(*file_id, model, vec, hash)
            .await
            .map_err(|e| index_to_proto(&e))?;
    }
    batch.clear();
    Ok(())
}

/// Body of the `index.embed` task: embeds the files already indexed under
/// `root`. Filtering (denied → text heuristic) happens BEFORE reading any
/// byte; the prefix hash decides whether re-embedding is needed. Cancelable
/// between files with [`Error::Cancelled`] — batches already persisted stay
/// (the index is coherent at all times; the next run skips them by hash).
pub(crate) async fn embed_for_index(
    provider: Arc<dyn Provider>,
    embedder: norte_ai::SharedAiProvider,
    index: Arc<norte_index::Index>,
    root: VPath,
    model: String,
    denied: Vec<VPath>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let candidates = index
        .files_for_embed(&root)
        .await
        .map_err(|e| index_to_proto(&e))?;
    if candidates.is_empty() {
        // Defense in depth: the engine already pre-checks this in the
        // response (NotFound with no prior build); this covers the race
        // with a concurrent build that emptied the root.
        return Err(Error::NotFound);
    }
    // **Whatever is already stored and is NOW denied gets deleted** (#122).
    //
    // The filter below decides what gets read, i.e. it protects whatever
    // hasn't been embedded yet. A file embedded BEFORE the user added it to
    // `denied_prefixes` leaves its vector there forever, and a vector is
    // invertible to an approximation of the text: the new denial couldn't
    // be honored without deleting the whole `index.db`.
    //
    // This goes here, at the start of the task, because the predicate is
    // already computed and because this is the only moment anyone looks at
    // this list. It's not a garbage collector: it's the denial applying
    // backward.
    if !denied.is_empty() {
        let stored = index
            .embedded_files(&root)
            .await
            .map_err(|e| index_to_proto(&e))?;
        let to_forget: Vec<i64> = stored
            .into_iter()
            .filter(|(_, p)| denied.iter().any(|d| crate::policy::is_under(d, p)))
            .map(|(id, _)| id)
            .collect();
        if !to_forget.is_empty() {
            let forgotten = index
                .forget_embeddings(&to_forget)
                .await
                .map_err(|e| index_to_proto(&e))?;
            tracing::info!(
                forgotten,
                "index.embed: vectors for now-denied files, forgotten (#122)"
            );
        }
    }
    // Filter BEFORE reading anything: denied_prefixes first (not a byte of
    // a denied prefix is ever read or comes out — spec §9), then the
    // text/size heuristic.
    let work: Vec<norte_index::EmbedCandidate> = candidates
        .into_iter()
        .filter(|c| !denied.iter().any(|d| crate::policy::is_under(d, &c.path)))
        .filter(|c| is_text_candidate(&c.path, c.size))
        .collect();
    let known = index
        .embedding_hashes(&root, &model)
        .await
        .map_err(|e| index_to_proto(&e))?;
    ctx.progress
        .update(|p| p.entries_total = Some(work.len() as u64));

    let mut batch: Vec<(i64, String, [u8; 32])> = Vec::new();
    for cand in work {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        ctx.progress.update(|p| {
            p.entries_done += 1;
            p.current = Some(cand.path.clone());
        });
        // Unreadable file: it may have died between the build and the
        // embed — it's skipped (counts as examined), it doesn't bring down
        // the task. To the log (path redacted like the spans), never
        // silently.
        let bytes = match read_prefix(provider.as_ref(), &cand.path).await {
            Ok(b) => b,
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    path = %crate::engine::span_path(&cand.path),
                    "index.embed: unreadable prefix; skipping"
                );
                continue;
            }
        };
        let hash: [u8; 32] = Sha256::digest(&bytes).into();
        if known
            .get(&cand.file_id)
            .is_some_and(|h| h.as_slice() == hash)
        {
            continue; // no change for this model: nothing to re-embed.
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        batch.push((cand.file_id, text, hash));
        if batch.len() >= EMBED_BATCH {
            flush_batch(embedder.as_ref(), &index, &model, &mut batch, ctx).await?;
        }
    }
    // Also checked before the final flush: a cancel arriving on the last
    // round must not trigger one more batch to the provider.
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    flush_batch(embedder.as_ref(), &index, &model, &mut batch, ctx).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("valid wire")
    }

    #[test]
    fn text_heuristic_by_extension() {
        // Known text extension.
        assert!(is_text_candidate(&vp("mem:///a.txt"), Some(10)));
        // Case-insensitive.
        assert!(is_text_candidate(&vp("mem:///B.RS"), Some(10)));
        // Known binary → no.
        assert!(!is_text_candidate(&vp("mem:///c.bin"), Some(10)));
        // No dot → no.
        assert!(!is_text_candidate(&vp("mem:///noext"), Some(10)));
        // Over the size cap → no, even if the extension is text.
        assert!(!is_text_candidate(
            &vp("mem:///big.txt"),
            Some(EMBED_MAX_FILE_SIZE + 1)
        ));
        // Unknown size passes (the read is capped anyway).
        assert!(is_text_candidate(&vp("mem:///a.md"), None));
        // Trailing dot (empty extension) → no.
        assert!(!is_text_candidate(&vp("mem:///weird."), Some(10)));
    }

    #[test]
    fn cosine_golden_order() {
        let q = [1.0f32, 0.0];
        assert!((cosine(&q, &[1.0, 0.0]).unwrap() - 1.0).abs() < 1e-6);
        assert!(cosine(&q, &[0.0, 1.0]).unwrap().abs() < 1e-6);
        assert!((cosine(&q, &[-1.0, 0.0]).unwrap() + 1.0).abs() < 1e-6);
        let mid = cosine(&q, &[1.0, 1.0]).unwrap();
        assert!(mid > 0.0 && mid < 1.0);
        // dim mismatch and null vector ⇒ None (ignored, doesn't break)
        assert!(cosine(&q, &[1.0]).is_none());
        assert!(cosine(&q, &[0.0, 0.0]).is_none());
        // not finite ⇒ None (belt: never a NaN score on the wire)
        assert!(cosine(&q, &[f32::NAN, 0.0]).is_none());
    }

    /// The prenormed norm gives EXACTLY the same as computing it inline: if
    /// not, the shortcut would have changed the wire's scores.
    #[test]
    fn the_prenormed_norm_does_not_change_the_score() {
        let q = [0.3f32, -1.7, 2.0];
        let n = q.iter().map(|x| x * x).sum::<f32>().sqrt();
        for v in [
            &[1.0f32, 0.0, 0.0][..],
            &[0.0, 1.0, 0.0][..],
            &[-2.0, 4.0, 0.5][..],
        ] {
            assert_eq!(cosine(&q, v), cosine_prenormed(&q, n, v), "{v:?}");
        }
        // And rejections are still rejections.
        assert!(cosine_prenormed(&q, n, &[1.0, 0.0]).is_none());
        assert!(cosine_prenormed(&q, n, &[0.0, 0.0, 0.0]).is_none());
        assert!(cosine_prenormed(&q, n, &[f32::NAN, 0.0, 0.0]).is_none());
    }

    fn puntuado(path: &str, score: f64) -> Puntuado {
        Puntuado {
            score,
            path: vp(path),
        }
    }

    /// The heap returns the same thing as sorting the whole thing and
    /// truncating, which is what it used to do: memory goes down, the
    /// result doesn't move.
    #[test]
    fn the_heap_gives_the_same_k_as_sorting_everything() {
        let all = vec![
            puntuado("mem:///c.txt", 0.10),
            puntuado("mem:///a.txt", 0.90),
            puntuado("mem:///d.txt", 0.50),
            puntuado("mem:///b.txt", 0.99),
            puntuado("mem:///e.txt", -0.20),
        ];
        let out = mejores_k(all.iter().cloned(), 3);
        assert_eq!(
            out.iter().map(|(p, _)| p.to_wire()).collect::<Vec<_>>(),
            ["mem:///b.txt", "mem:///a.txt", "mem:///d.txt"]
        );
        // Asking for more than there are returns all of them, in the same order.
        assert_eq!(mejores_k(all.into_iter(), 100).len(), 5);
    }

    /// At equal score, the PATH decides, and that's why the answer doesn't
    /// depend on the order `SQLite` returns the rows in.
    #[test]
    fn at_equal_score_the_order_is_stable() {
        let a = vec![
            puntuado("mem:///z.txt", 0.5),
            puntuado("mem:///a.txt", 0.5),
            puntuado("mem:///m.txt", 0.5),
        ];
        let mut reversed = a.clone();
        reversed.reverse();
        let one = mejores_k(a.into_iter(), 2);
        let other = mejores_k(reversed.into_iter(), 2);
        assert_eq!(one, other);
        assert_eq!(one[0].0.to_wire(), "mem:///a.txt");
    }

    /// `k = 0` cannot reach here (`clamp_k` bumps it to 1), but the heap
    /// must not explode if it ever does.
    #[test]
    fn zero_k_returns_nothing() {
        assert!(mejores_k([puntuado("mem:///a.txt", 1.0)].into_iter(), 0).is_empty());
    }

    #[test]
    fn clamp_k_pins_wire_bounds() {
        assert_eq!(clamp_k(0), 1, "k=0 is clamped to 1");
        assert_eq!(clamp_k(1000), 100, "upper bound = INDEX_SEMANTIC_MAX_K");
        assert_eq!(clamp_k(50), 50, "within range passes through unchanged");
    }
}
