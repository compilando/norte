//! TC navigation (spec 2026-07-18): PURE quick-search logic — no terminal, no
//! `App`. The match is typing UX over the lossy name normalized to NFC and
//! case-folded (the macOS NFD trap, CLAUDE.md); the IDENTITY of the entries
//! is still the `VPath` in bytes — operating always uses
//! `entries[real_index]`.
//!
//! Shared by both frontends (TUI and GUI): the quick search's mechanics do
//! not change nature with the render backend.

use norte_proto::{Entry, VPath};
use unicode_normalization::UnicodeNormalization;

/// Quick-search mode (`[ui] quick_search`, default filter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// The listing shrinks to the matches.
    #[default]
    Filter,
    /// The cursor jumps between matches; the listing does not change.
    Jump,
}

/// Name → comparison key: lossy of the last segment, NFC, lowercase, and NFC
/// AGAIN.
///
/// The second NFC is not redundant: lowercasing can knock the result out of
/// NFC when the precomposed form only exists in lowercase (J+U+030C does not
/// compose, but its lowercase j+U+030C composes to ǰ U+01F0) — without
/// re-normalizing, the composed needle and the decomposed name would not
/// match (broken canonical equivalence, encoding review MEDIA-1).
///
/// Only CANONICAL equivalence (NFC): half-width katakana, ligatures and
/// other COMPATIBILITY equivalences (NFKC) are knowingly left OUT — "ﬁ"
/// does not match "fi"; normalizing them would switch equivalence families.
///
/// Cost: the per-entry fold is CACHED in `QuickSearch::folds` (#77) — a
/// full recompute per listing mutation (`new` / `refresh`), not per
/// keystroke; keystrokes (`push_char`/`backspace`) only fold the query.
/// `to_lowercase` is Rust's simple case-folding, NOT full Unicode
/// case-folding — deliberate, enough for typing-UX substring matching.
///
/// `pub` (H1 T4): the TUI's command palette folds text that is NOT an
/// `Entry` name (command+description) under the SAME normalization
/// criterion as the quick search — one single fold pipeline for every
/// substring filter in the frontend, never a diverging copy.
#[must_use]
pub fn fold(name: &[u8]) -> String {
    String::from_utf8_lossy(name)
        .nfc()
        .flat_map(char::to_lowercase)
        .nfc()
        .collect()
}

/// [`fold`] with the pane's name reinterpretation (#98/F1): a NON-UTF8 name
/// under `Some(enc)` is folded over the DECODED text
/// ([`decode_name`](norte_encoding::decode_name), `display_name_with`'s
/// rule) — typing "п" finds the entry the pane paints as "Папка". With no
/// reinterpretation (or a valid UTF-8 name): the usual lossy fold.
///
/// ONE single pipeline (audit F1): the `enc` branch DELEGATES to [`fold`] —
/// the double-NFC (the J+U+030C case) is pinned for both paths by the same
/// fixture; duplicating it here was an unkillable mutant until the cycle
/// gains an encoding with combining marks (windows-1258).
///
/// DELIBERATE divergence from the painted text (audit F3): the decoded
/// text is folded WITHOUT masking — a hazard masked to `�` on screen does
/// not match typing `�` (the same pre-existing asymmetry of the lossy path
/// with controls embedded in valid UTF-8). Folds are never painted.
///
/// `pub` so pattern-based marking (#103) folds EXACTLY like the quick
/// search — one single pipeline, never a diverging copy, and the design
/// claim ("one shared fold") becomes linkable from outside the crate
/// instead of only promised in prose.
#[must_use]
pub fn fold_with(name: &[u8], enc: Option<norte_encoding::NameEncoding>) -> String {
    match (enc, std::str::from_utf8(name)) {
        (Some(e), Err(_)) => fold(norte_encoding::decode_name(name, e).as_bytes()),
        _ => fold(name),
    }
}

/// Precomputed folds of `entries` (index-parallel). See [`fold_with`].
fn fold_names(entries: &[Entry], enc: Option<norte_encoding::NameEncoding>) -> Vec<String> {
    entries
        .iter()
        .map(|e| fold_with(e.path.file_name().map_or(&b""[..], |s| s.as_bytes()), enc))
        .collect()
}

/// Matching over ALREADY precomputed folds (the keystroke hot path).
fn matches_folded(query_folded: &str, folds: &[String]) -> Vec<usize> {
    folds
        .iter()
        .enumerate()
        .filter(|(_, f)| f.contains(query_folded))
        .map(|(i, _)| i)
        .collect()
}

/// Indices of `entries` whose name contains `query` (same normalization on
/// both sides, see `fold`). `query` in raw bytes (comes straight from the
/// input).
///
/// A convenience with NO cache: it folds the whole of `entries` on every
/// call (streaming — peak memory of ONE fold, not N). The cached state (the
/// keystroke hot path) lives inside [`QuickSearch`] — this function is for
/// the occasional caller (tests, a single one-off computation), not for the
/// typing loop. It does NOT apply name reinterpretation (#57): to match
/// against the reinterpreted text use [`QuickSearch`] (which receives the
/// pane's encoding).
#[must_use]
pub fn matches(query: &[u8], entries: &[Entry]) -> Vec<usize> {
    let q = fold(query);
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            let name = e.path.file_name().map_or(&b""[..], |s| s.as_bytes());
            fold(name).contains(&q)
        })
        .map(|(i, _)| i)
        .collect()
}

/// Live quick-search state for ONE pane.
#[derive(Debug)]
pub struct QuickSearch {
    query: Vec<u8>,
    mode: Mode,
    /// Comparison keys per entry (index-parallel to `entries`), recomputed
    /// ONCE per listing mutation (`new`/`refresh`), not per keystroke (#77).
    folds: Vec<String>,
    /// REAL indices into `entries` that match (empty query = all).
    visible: Vec<usize>,
    /// Selection position INSIDE `visible`.
    pos: usize,
    /// Name reinterpretation in effect when folding (#98/F1): folds are
    /// computed over the text the user SEES. Changing it requires
    /// re-folding ([`QuickSearch::set_name_encoding`]).
    enc: Option<norte_encoding::NameEncoding>,
}

impl QuickSearch {
    /// Starts an empty quick search in the given mode over `entries`: empty
    /// query, `visible` is computed right away (empty query = everything
    /// visible). `enc` = the pane's name reinterpretation (#57), so the
    /// filter matches against the PAINTED text.
    #[must_use]
    pub fn new(mode: Mode, entries: &[Entry], enc: Option<norte_encoding::NameEncoding>) -> Self {
        let mut q = Self {
            query: Vec::new(),
            mode,
            folds: fold_names(entries, enc),
            visible: Vec::new(),
            pos: 0,
            enc,
        };
        q.recompute();
        q
    }

    /// Changes the name reinterpretation and RE-FOLDS the cache (#98/F1):
    /// the only other invalidation point besides `new`/`refresh`. Same
    /// selection contract as [`QuickSearch::refresh`].
    pub fn set_name_encoding(
        &mut self,
        enc: Option<norte_encoding::NameEncoding>,
        entries: &[Entry],
        prev_selected: Option<&VPath>,
    ) {
        self.enc = enc;
        self.refresh(entries, prev_selected);
    }

    /// Recomputes `visible` from the current query over `self.folds` (the
    /// cache ALREADY in effect — does not touch `entries`).
    fn recompute(&mut self) {
        self.visible = if self.query.is_empty() {
            (0..self.folds.len()).collect()
        } else {
            matches_folded(&fold(&self.query), &self.folds)
        };
    }

    /// Adds a typed character to the query and recomputes. Only folds the
    /// query — the entries' fold is already cached in `self.folds`.
    pub fn push_char(&mut self, c: char) {
        let mut buf = [0u8; 4];
        self.query
            .extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        self.recompute();
        self.pos = 0;
    }

    /// Removes the last typed byte (deletes a whole UTF-8 char) and
    /// recomputes. An empty query after the delete = everything visible.
    pub fn backspace(&mut self) {
        if self.query.is_empty() {
            return;
        }
        // Backs up to the start of the last UTF-8 char (or to the last byte
        // if the query is not valid UTF-8 — should not happen since it is
        // only fed via `push_char`, but it does not panic either way).
        let mut cut = self.query.len() - 1;
        while cut > 0 && (self.query[cut] & 0b1100_0000) == 0b1000_0000 {
            cut -= 1;
        }
        self.query.truncate(cut);
        self.recompute();
        self.pos = 0;
    }

    /// Recomputes `visible` over the ALREADY mutated `entries` (a new batch
    /// from the fill, or a full re-sort — `Pane::extend_listing` re-sorts
    /// the whole listing on every batch), keeping the selection by
    /// IDENTITY, not by index: an index remembered from BEFORE the sort can
    /// point at a different entry after it.
    ///
    /// `prev_selected` is the `VPath` of the entry selected BEFORE the
    /// mutation — the caller captures it via
    /// `entries[selected_entry_index()?].path.clone()` before mutating
    /// `entries`. That path is looked up again inside the new `visible`;
    /// if it died (no longer matches / was removed) or there was no
    /// previous selection, it clamps within the new range.
    ///
    /// It also renews the `folds` cache — along with `new`, it is the ONLY
    /// invalidation point (#77): `visible`'s indices refer to the `entries`
    /// of the last `new`/`refresh`.
    pub fn refresh(&mut self, entries: &[Entry], prev_selected: Option<&VPath>) {
        self.folds = fold_names(entries, self.enc);
        self.recompute();
        if let Some(prev) = prev_selected
            && let Some(new_pos) = self.visible.iter().position(|&i| entries[i].path == *prev)
        {
            self.pos = new_pos;
            return;
        }
        self.clamp_pos();
    }

    /// Clamps `pos` within `[0, visible.len())`, without panicking if empty.
    fn clamp_pos(&mut self) {
        if self.visible.is_empty() {
            self.pos = 0;
        } else if self.pos >= self.visible.len() {
            self.pos = self.visible.len() - 1;
        }
    }

    /// Moves the selection one position down (clamped at the end).
    pub fn down(&mut self) {
        if self.pos + 1 < self.visible.len() {
            self.pos += 1;
        }
    }

    /// Moves the selection one position up (clamped at the start).
    pub fn up(&mut self) {
        self.pos = self.pos.saturating_sub(1);
    }

    /// Jump mode: advances to the next match with wrap; a no-op if there
    /// are no matches.
    pub fn next_match(&mut self) {
        if self.visible.is_empty() {
            return;
        }
        self.pos = (self.pos + 1) % self.visible.len();
    }

    /// Visible real indices (all of them if the query is empty).
    #[must_use]
    pub fn visible(&self) -> &[usize] {
        &self.visible
    }

    /// Real index of the selected entry, if there is one visible.
    #[must_use]
    pub fn selected_entry_index(&self) -> Option<usize> {
        self.visible.get(self.pos).copied()
    }

    /// Query to paint on screen (lossy — the user typed it, it is not part
    /// of any entry's identity). Masked with
    /// [`norte_encoding::is_terminal_hazard`] (encoding review LOW): with no
    /// bracketed paste, a hostile IME/paste arrives as a `push_char` stream
    /// and would paint raw bidi/invisibles at the edge — the filtering
    /// itself (`matches`/`fold`) still operates on `self.query` WITHOUT
    /// sanitizing, only the text that gets painted goes through here.
    #[must_use]
    pub fn query_display(&self) -> String {
        String::from_utf8_lossy(&self.query)
            .chars()
            .map(|c| {
                if norte_encoding::is_terminal_hazard(c) {
                    '\u{FFFD}'
                } else {
                    c
                }
            })
            .collect()
    }

    /// Active mode (Filter or Jump).
    #[must_use]
    pub fn mode(&self) -> Mode {
        self.mode
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::{Entry, EntryKind, VPath};

    // Entry does NOT derive Default and `mtime_ms` is the field's real name
    // (not `mtime`) — see crates/norte-proto/src/entry.rs.
    fn e(wire: &str) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse(wire).expect("wire"),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
        }
    }

    #[test]
    fn case_insensitive_substring_filter() {
        let entries = vec![
            e("mem:///Projects"),
            e("mem:///readme.md"),
            e("mem:///PROBE"),
        ];
        let m = matches(b"pro", &entries);
        assert_eq!(m, vec![0, 2], "Projects and PROBE match; readme does not");
    }

    #[test]
    fn nfc_filter_matches_nfd() {
        // "año" in NFC as the needle; entry with the name in NFD (a + n + ̃ + o).
        let nfd = "an\u{0303}o.txt";
        let entries = vec![e(&format!("mem:///{nfd}"))];
        assert_eq!(
            matches("año".as_bytes(), &entries),
            vec![0],
            "NFD matches the NFC needle"
        );
    }

    #[test]
    fn fold_renormalizes_nfc_after_lowercasing() {
        // Encoding MEDIA-1: J + U+030C (combining caron) has NO precomposed
        // UPPERCASE form, but its lowercase ǰ (U+01F0) DOES exist. A fold
        // that does not re-normalize to NFC after lowercasing leaves
        // "j\u{030C}" (decomposed) and the "ǰ" needle (composed) does not
        // match: canonical equivalence is lost.
        let entries = vec![e("mem:///J%CC%8C.txt")];
        assert_eq!(
            matches("ǰ".as_bytes(), &entries),
            vec![0],
            "the precomposed needle ǰ (U+01F0) matches J+U+030C"
        );
    }

    #[test]
    fn non_utf8_bytes_do_not_break_or_false_match() {
        let entries = vec![e("mem:///%FF%FE"), e("mem:///normal.txt")];
        assert_eq!(matches(b"norm", &entries), vec![1]);
        // The hostile entry stays filterable by what its lossy shows (�) —
        // a tested contract, not just "does not panic".
        assert_eq!(matches("\u{FFFD}".as_bytes(), &entries), vec![0]);
    }

    #[test]
    fn filter_state_navigates_and_confirms() {
        let entries = vec![e("mem:///a1"), e("mem:///b"), e("mem:///a2")];
        let mut q = QuickSearch::new(Mode::Filter, &entries, None);
        q.push_char('a');
        assert_eq!(q.visible(), &[0, 2]);
        q.down();
        assert_eq!(q.selected_entry_index(), Some(2), "second match");
        q.backspace();
        assert_eq!(q.visible(), &[0, 1, 2], "empty query = everything visible");
    }

    #[test]
    fn jump_mode_tab_wraps() {
        let entries = vec![e("mem:///ab"), e("mem:///zz"), e("mem:///ac")];
        let mut q = QuickSearch::new(Mode::Jump, &entries, None);
        q.push_char('a');
        assert_eq!(q.selected_entry_index(), Some(0));
        q.next_match();
        assert_eq!(q.selected_entry_index(), Some(2));
        q.next_match();
        assert_eq!(q.selected_entry_index(), Some(0), "wrap");
    }

    #[test]
    fn refresh_after_new_batch_keeps_selection_if_it_survives() {
        let mut entries = vec![e("mem:///a1")];
        let mut q = QuickSearch::new(Mode::Filter, &entries, None);
        q.push_char('a');
        let prev = q.selected_entry_index().map(|i| entries[i].path.clone());
        entries.push(e("mem:///a2")); // a batch from the fill arrives
        q.refresh(&entries, prev.as_ref());
        assert_eq!(q.visible(), &[0, 1]);
        assert_eq!(
            q.selected_entry_index(),
            Some(0),
            "the selection does not jump"
        );
    }

    #[test]
    fn refresh_survives_a_resort() {
        // review MAJOR: the selection is kept by IDENTITY (VPath), not by
        // index — `Pane::extend_listing` re-sorts the whole listing on
        // every batch (app.rs), so a remembered index points at a
        // DIFFERENT entry after the sort.
        let mut entries = vec![e("mem:///a1"), e("mem:///a2")];
        let mut q = QuickSearch::new(Mode::Filter, &entries, None);
        q.push_char('a');
        q.down(); // selects a2 (real index 1)
        assert_eq!(q.selected_entry_index(), Some(1));
        let prev = q.selected_entry_index().map(|i| entries[i].path.clone());

        // The batch re-sorts: a2 moves to real index 0, a1 to real index 1.
        entries.swap(0, 1);
        q.refresh(&entries, prev.as_ref());

        assert_eq!(
            q.selected_entry_index().map(|i| entries[i].path.clone()),
            prev,
            "the selection stays on the SAME path after the resort, not the same index"
        );
    }

    #[test]
    fn query_display_masks_hazards() {
        // encoding review LOW: an RLO (U+202E) typed/pasted must not come
        // out raw in the `/{query}` echo — it is pushed char by char, as it
        // would arrive from a real input stream (no bracketed paste).
        let entries = vec![e("mem:///normal.txt")];
        let mut q = QuickSearch::new(Mode::Filter, &entries, None);
        for c in "a\u{202E}b".chars() {
            q.push_char(c);
        }
        let display = q.query_display();
        assert!(
            !display.chars().any(norte_encoding::is_terminal_hazard),
            "query_display left a raw hazard: {display:?}"
        );
    }

    #[test]
    fn push_char_uses_the_last_refreshs_folds() {
        // The folds cache (#77) has to be renewed on refresh: an entry that
        // arrives in a LATER batch has to match the next keystroke.
        let mut entries = vec![e("mem:///zzz")];
        let mut q = QuickSearch::new(Mode::Filter, &entries, None);
        entries.push(e("mem:///new.txt")); // a batch from the fill
        q.refresh(&entries, None);
        q.push_char('n');
        assert_eq!(q.visible(), &[1], "the new entry's fold is in the cache");
    }

    #[test]
    fn the_cache_folds_the_same_as_matches_over_the_hostile_corpus() {
        // Anti-divergence pin (encoding review #77): the cached path
        // (new/refresh→push_char) and the uncached one (`matches`) have to
        // give EXACTLY the same result over the hostile corpus — a future
        // fast-path that optimizes only the cache would pass the corpus
        // (which goes through `matches`) while breaking the real typing
        // loop.
        let entries = vec![
            e("mem:///an%CC%83o.txt"), // NFD
            e("mem:///J%CC%8C.txt"),   // no uppercase precomposed form
            e("mem:///%FF%FE"),        // non-UTF8
        ];
        for needle in ["año", "ǰ", "\u{FFFD}"] {
            let mut q = QuickSearch::new(Mode::Filter, &entries, None);
            for c in needle.chars() {
                q.push_char(c);
            }
            assert_eq!(
                q.visible(),
                matches(needle.as_bytes(), &entries).as_slice(),
                "the cache and the direct path diverge for {needle:?}"
            );
        }
    }
}

/// ONE pane's directory history, and the trail that walks it.
///
/// It used to live in `norte-tui`. There was nothing terminal-specific about
/// it: it is the same question — where do I come from and where do I go
/// back to? — for any surface that navigates, and a second copy in the
/// graphical host would have been exactly the kind of divergence this crate
/// exists to prevent (ADR 0066, D14).
use std::collections::VecDeque;

/// Cap on directories retained in a pane's history when the configuration
/// does not say otherwise (`[ui] history_size`, spec 2026-09-15 D4).
pub const HISTORY_DEFAULT: usize = 30;

/// The lowest cap `[ui] history_size` accepts: below it, `nav.back` stops
/// being a trail and becomes "the previous one".
pub const HISTORY_MIN: usize = 5;

/// The highest cap: the one the session already stores per visible slot
/// ([`crate::session::HISTORY_CAP`]). A larger one would be lost on restart
/// with nothing saying so.
pub const HISTORY_MAX: usize = crate::session::HISTORY_CAP;

/// History of directories ONE pane has visited. Every SUCCESSFUL `cd`
/// pushes the PREVIOUS dir (main.rs, the `Cd::Filling`/`Cd::Replaced` arms);
/// `Alt+↓` walks it in a popup (T5). It lives in the process's memory, not
/// in `norte.toml` — deliberately, out of the spec's scope (§Out of scope).
///
/// Trail INVARIANT: `back.len() + fwd.len() <= cap`, with `cap` between
/// [`HISTORY_MIN`] and [`HISTORY_MAX`] ([`History::set_capacity`]).
///
/// This is what bounds the trail's memory, and not each stack on its own.
/// [`History::record`] is the only method that GROWS the sum, and it bounds
/// it: it truncates `back` to the cap and empties `fwd`. Both steps
/// preserve it exactly — they move one element from one stack to the
/// other — and [`History::remove`] only shrinks it. That is why
/// [`History::step_forward`] can push onto `back` WITHOUT checking the
/// cap: the room `fwd`'s `pop` leaves is the room it takes up. Breaking the
/// invariant (e.g. making `record` stop emptying `fwd`) would grow the
/// trail without end through the one path that does not check it.
#[derive(Debug)]
pub struct History {
    /// Most recent at the front.
    deque: VecDeque<VPath>,
    /// The trail behind the reader: where `nav.back` goes, newest last.
    ///
    /// Separate from `deque` because they answer different questions. The
    /// deque is "where has this pane been", deduplicated and most-recent
    /// first, which is what the popup lists. The trail is "where was I just
    /// now", in order, with repeats — walking the deque as if it were a trail
    /// oscillates between the two most recent directories forever.
    back: Vec<VPath>,
    /// Where `nav.forward` goes: the branch a `nav.back` stepped off, newest
    /// last. Cleared by any navigation the user initiates.
    fwd: Vec<VPath>,
    /// How many directories the MRU keeps, and the trail's sum
    /// (`back + fwd`). Between [`HISTORY_MIN`] and [`HISTORY_MAX`].
    cap: usize,
    /// The jump point (Krusader `Ctrl+J`, spec 2026-09-15 D5): a site
    /// marked ON PURPOSE, that `nav.jump-back` returns to. It is not part
    /// of the trail — a step back does not move it, a new navigation does
    /// not clear it — and that is why it is kept apart.
    jump: Option<VPath>,
}

impl Default for History {
    fn default() -> Self {
        Self::with_capacity(HISTORY_DEFAULT)
    }
}

impl History {
    /// An empty history that keeps up to `cap` directories, bounded to
    /// [`HISTORY_MIN`]`..=`[`HISTORY_MAX`].
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            deque: VecDeque::new(),
            back: Vec::new(),
            fwd: Vec::new(),
            cap: cap.clamp(HISTORY_MIN, HISTORY_MAX),
            jump: None,
        }
    }

    /// The cap currently in effect.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Changes the cap (a `[ui] history_size` reload).
    ///
    /// LOWERING it drops what is farthest from the reader, never what was
    /// just walked: the old end of the MRU and of the back trail, and the
    /// far tip of the forward branch. Raising it does not invent anything.
    pub fn set_capacity(&mut self, cap: usize) {
        self.cap = cap.clamp(HISTORY_MIN, HISTORY_MAX);
        self.clamp();
    }

    /// Restores the invariant after a cap change or a seed.
    fn clamp(&mut self) {
        self.deque.truncate(self.cap);
        if self.back.len() > self.cap {
            let overflow = self.back.len() - self.cap;
            self.back.drain(..overflow);
        }
        // `fwd` goes from oldest to most recent and `step_forward` pops the
        // LAST one, so the start is the farthest forward: what used to be
        // the first out of reach. The back trail has priority: if it fills
        // the cap on its own, the forward branch goes away entirely, which
        // is the same thing the next navigation would do.
        let excess = (self.back.len() + self.fwd.len()).saturating_sub(self.cap);
        self.fwd.drain(..excess.min(self.fwd.len()));
    }

    /// Sets the jump point to `path` (`nav.set-jump-point`).
    pub fn set_jump(&mut self, path: VPath) {
        self.jump = Some(path);
    }

    /// Seeds the jump point from a saved session.
    pub fn seed_jump(&mut self, path: Option<VPath>) {
        self.jump = path;
    }

    /// The jump point, if there is one.
    #[must_use]
    pub fn jump(&self) -> Option<&VPath> {
        self.jump.as_ref()
    }

    /// Empties the MRU and the trail (`dialog.clear`). The jump point and
    /// the cap stay: they were marked or configured, not walked.
    pub fn clear(&mut self) {
        self.deque.clear();
        self.back.clear();
        self.fwd.clear();
    }

    /// Pushes `path` to the front. CONSECUTIVE dedup: if `path` is already
    /// the most recent one, a no-op — this avoids repeating the same dir on
    /// redundant cd's (e.g. refreshing the pane). The same dir in
    /// NON-consecutive positions of the history CAN repeat (visit it,
    /// leave, come back): it is session history, not a set. The dedup
    /// compares `VPath` byte-exact WITHOUT normalizing (identity is never
    /// normalized); NFC/NFD twins coexist as distinct rows — a deliberate
    /// decision.
    pub fn push(&mut self, path: VPath) {
        if self.deque.front() == Some(&path) {
            return;
        }
        self.deque.push_front(path);
        self.deque.truncate(self.cap);
    }

    /// The back trail, from oldest to most recent: what the session saves
    /// so `nav.back` keeps working after a restart.
    #[must_use]
    pub fn trail(&self) -> &[VPath] {
        &self.back
    }

    /// The branch stepped off with a `nav.back`, from oldest to most
    /// recent.
    #[must_use]
    pub fn forward_trail(&self) -> &[VPath] {
        &self.fwd
    }

    /// Seeds both trails from a saved session.
    ///
    /// The MRU is rebuilt FROM the trail and not saved separately: it is
    /// what the popup lists, it is derived from where things have been, and
    /// saving it separately would be a second copy of the same history that
    /// can contradict the first. It is pushed from oldest to most recent so
    /// the popup's order comes out the same as if it had been walked.
    pub fn seed(&mut self, back: Vec<VPath>, fwd: Vec<VPath>) {
        for p in &back {
            self.push(p.clone());
        }
        self.back = back;
        self.fwd = fwd;
        // A session written with a cap higher than the current one brings
        // more than fits: it is trimmed just like lowering the cap live
        // would.
        self.clamp();
    }

    /// Removes ALL occurrences of `path` (e.g. after a `cd` that failed
    /// with `NotFound` while navigating from the popup — the spec says "it
    /// is REMOVED if the cd fails with `NotFound`").
    ///
    /// Prunes the TRAIL as well as the MRU. "This directory is gone" is one
    /// fact, not two: left on the trail, a path the popup just retired would
    /// still be where `nav.back` aims — a key that can only fail, and one the
    /// reader has no other way to steer around. Pruning both is also what
    /// keeps the two structures from ever disagreeing about which places
    /// still exist.
    pub fn remove(&mut self, path: &VPath) {
        self.deque.retain(|p| p != path);
        self.back.retain(|p| p != path);
        self.fwd.retain(|p| p != path);
        // A jump point to a place that is no longer there is the same key
        // that can only fail.
        if self.jump.as_ref() == Some(path) {
            self.jump = None;
        }
    }

    /// Entries, most recent first.
    #[must_use]
    pub fn entries(&self) -> &VecDeque<VPath> {
        &self.deque
    }

    /// Records a navigation the USER initiated, leaving `prev` behind.
    ///
    /// Feeds BOTH structures: [`History::push`] for the MRU the popup paints,
    /// and the back stack for the trail `nav.back` walks. They are fed from
    /// the same event but kept apart on purpose — see the `History::back`
    /// field docs for why one cannot serve as the other.
    ///
    /// Skips the trail push when `prev` is already its top, mirroring the
    /// MRU's consecutive dedup: a redundant `cd` onto the directory we are
    /// already tracking (a pane refresh, say) is not a step the reader took,
    /// and recording it would make `nav.back` do nothing visible once.
    ///
    /// Clears `fwd`: the reader chose a different path, so the branch they
    /// stepped off no longer exists. Offering a "forward" into a history the
    /// reader already abandoned is the browser bug everyone knows.
    pub fn record(&mut self, prev: VPath) {
        self.push(prev.clone());
        if self.back.last() != Some(&prev) {
            self.back.push(prev);
            if self.back.len() > self.cap {
                // Newest last, so the cap drops from the front: the oldest
                // step of the trail is the one the reader is least likely to
                // still want.
                self.back.remove(0);
            }
        }
        self.fwd.clear();
    }

    /// Steps one directory BACK along the trail, leaving `current` behind.
    ///
    /// Pops the back stack, pushes `current` onto the forward stack so
    /// [`History::step_forward`] can undo this, and returns the target.
    /// `None` when the trail is exhausted — the caller should then leave the
    /// pane where it is rather than invent a destination.
    ///
    /// Deliberately does NOT feed the MRU: going back is not visiting
    /// somewhere new, and a popup that grew an entry per back-press would
    /// stop being a list of the places the reader went.
    pub fn step_back(&mut self, current: VPath) -> Option<VPath> {
        let target = self.back.pop()?;
        self.fwd.push(current);
        Some(target)
    }

    /// Steps one directory FORWARD along the branch a [`History::step_back`]
    /// stepped off — the mirror image of it, down to leaving the MRU alone.
    ///
    /// `None` when there is no such branch, either because the reader never
    /// went back or because a [`History::record`] pruned it.
    ///
    /// Pushes onto `back` with no bound check because it cannot need one: it
    /// pops `fwd` first, and the type's invariant (`back.len() + fwd.len() <=
    /// cap`, stated on [`History`]) makes that pop the room for this
    /// push.
    pub fn step_forward(&mut self, current: VPath) -> Option<VPath> {
        let target = self.fwd.pop()?;
        self.back.push(current);
        Some(target)
    }

    /// Length of the back trail. Zero means `nav.back` is a no-op, which is
    /// what a caller checks before painting the key as available.
    #[must_use]
    pub fn back_len(&self) -> usize {
        self.back.len()
    }

    /// Length of the forward branch. Zero means `nav.forward` is a no-op.
    #[must_use]
    pub fn fwd_len(&self) -> usize {
        self.fwd.len()
    }
}

/// Where a pane goes whose session just closed (`pane.disconnect`).
///
/// The TRAIL walked backward, from most recent to oldest, skipping
/// everything that belongs to the SAME session: going back to
/// `sftp://server/another-folder` would reopen the connection that just
/// closed, which is exactly what the gesture asked not to have. The same
/// session is scheme AND authority — another server on the same scheme is
/// another connection, and going back there is legitimate.
///
/// **The scheme is compared WITHOUT its format prefix** (ADR 0028): a
/// `zip+sftp://server/x.zip!/…` from the trail is served by the same
/// connection as `sftp://server/…`, and the core evicts both keys at once
/// when closing it (`sessions`, the `…+{key}` sweep). Comparing the raw
/// scheme, `"zip+sftp" != "sftp"` and the pane would land right back inside
/// the machine that had just been let go — opening a NEW connection, with
/// its reauthentication, which is literally what this function exists to
/// prevent.
///
/// What this does NOT do is canonicalize aliases: the core deduplicates
/// authorities against `connections.toml` (#47) and a frontend does not
/// have that table, so `sftp://work/a` and `sftp://user@host/a` look like
/// two machines even if they are one. The cost of getting that wrong is
/// reconnecting, not losing anything.
///
/// `None` when nothing foreign is left — the pane was born remote, or its
/// whole trail belongs to that machine —: the caller then falls back to
/// [`crate::shell::home_vpath`]. What cannot happen is the pane being left
/// staring at something that can no longer be read.
///
/// Shared by both frontends ON PURPOSE: the decision is the same no matter
/// who looks at it, and back when it lived twice the TUI would go home
/// while the window walked back its trail.
///
/// ```
/// use norte_frontend::nav::regreso_after_disconnect;
/// use norte_proto::VPath;
/// let vp = |s: &str| VPath::parse(s).expect("wire");
/// let trail = [vp("file:///home/o"), vp("sftp://srv/a")];
/// assert_eq!(
///     regreso_after_disconnect(&vp("sftp://srv/a"), &trail),
///     Some(vp("file:///home/o")),
/// );
/// ```
#[must_use]
pub fn regreso_after_disconnect(closed: &VPath, trail: &[VPath]) -> Option<VPath> {
    let is_same_session = |p: &VPath| {
        session_scheme(p.scheme()) == session_scheme(closed.scheme())
            && p.authority() == closed.authority()
    };
    trail.iter().rev().find(|p| !is_same_session(p)).cloned()
}

/// The scheme that serves a path, without the archive format prefix: the
/// `sftp` of `zip+sftp`, the `file` of `tar+gz+file`.
///
/// It is half of the core's session key, the half a frontend can compute
/// without its connections table.
fn session_scheme(scheme: &str) -> &str {
    match norte_proto::scheme_archive_format(scheme) {
        // The format and the inner scheme are glued by a `+`, which is also
        // skipped: `scheme_archive_format` returns the prefix without it.
        Some(format) => &scheme[format.len() + 1..],
        None => scheme,
    }
}

// ── Navigation sugar for compressed archives (ADR 0018) ───────────────────
//
// `archive_root_for` used to live in `main.rs`, private to the binary.
// `App::help_facts` (H3d) needs it too: the fact "this entry gets ENTERED"
// is the predicate of the dispatch's `nav.enter` arm, and the help has to
// answer it with the SAME function or it will end up dimming `nav.enter`
// over a `.zip` the app opens without a problem.

/// Whether a navigation gets RECORDED in the pane's trail, or is the trail
/// replaying itself.
///
/// Without this distinction `nav.back` would feed off its own trail: going
/// back from B to A would record "was at B", so the next back returns to B
/// and the reader oscillates between two directories — the exact defect the
/// trail exists to prevent, one level up.
///
/// Lives here, and not next to a frontend's `cd`, because a frontend's TOFU
/// modal TRANSPORTS it: the retry after trusting the host key has to resume
/// the SAME navigation the TOFU interrupted, and the lib cannot refer to a
/// type declared in `main.rs`.
// TODO(translation): review — the paragraph above ends abruptly and the doc
// comment for `mirrored_destination` appears to have been merged into this one
// in the original Spanish (a stale merge, same class of issue as modal.rs);
// translated as-is, without restructuring.
/// Where the OTHER pane has to go when the navigation is mirrored, or `None`
/// if there is nothing to do.
///
/// Lives here, and not in each frontend, for the same reason as
/// [`Trail::Seed`]: a mirrored shot (`pane.mirror`) was already written
/// twice — once in the terminal and once in the window — and the two
/// versions did not say the same thing. A mode that repeats EVERY
/// navigation cannot afford that difference, because it does not show in
/// one gesture: it shows on the third `cd`, when the two panes are no
/// longer where the reader thinks.
///
/// What gets mirrored is the DESTINATION of the navigation that just
/// happened, not what the source pane shows: while a `cd` is in flight,
/// `dir()` still answers with the place being left, and mirroring that
/// would send the other pane right to where the reader just came from.
/// That is the rule the window had written down and the terminal did not.
///
/// `None` in the two cases where moving the other pane would be worse than
/// not doing it: it is already there — a redundant `cd` re-lists it and
/// slides its listing out from under the cursor for nothing — or the
/// destination is the same place. A pane showing SEARCH RESULTS is the
/// exception: its `dir()` is the root that was searched from, not what is
/// being looked at, so there it does navigate.
///
/// ```
/// use norte_frontend::nav::mirrored_destination;
/// use norte_proto::VPath;
///
/// let home = VPath::parse("mem:///home").unwrap();
/// let docs = VPath::parse("mem:///home/docs").unwrap();
/// // The other pane is somewhere else: it is sent to the destination.
/// assert_eq!(mirrored_destination(&docs, &home, false), Some(docs.clone()));
/// // Already there: it does not get re-listed for nothing.
/// assert_eq!(mirrored_destination(&docs, &docs, false), None);
/// // Unless what it shows is search results, which are not a location.
/// assert_eq!(mirrored_destination(&docs, &docs, true), Some(docs));
/// ```
#[must_use]
pub fn mirrored_destination(
    dest: &norte_proto::VPath,
    other_dir: &norte_proto::VPath,
    other_is_virtual: bool,
) -> Option<norte_proto::VPath> {
    (other_dir != dest || other_is_virtual).then(|| dest.clone())
}

/// How a navigation enters the pane's trail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trail {
    /// The user requested this move: it enters the MRU and the trail, and
    /// prunes the forward branch.
    Record,
    /// `nav.back`/`nav.forward` are replaying, and THIS is the step they are
    /// taking. The trail already knows it, so the navigation is not
    /// recorded; the step travels inside because a `Replay` that does not
    /// know which direction it is going cannot be undone, and whoever has
    /// to rewind it may not be who started it: the TOFU suspends the
    /// navigation and the modal's answer finishes it, minutes later and
    /// from another spot in the code.
    ///
    /// It goes INSIDE the variant, not in a separate field next to it, so
    /// that "recording" and "making sense" cannot contradict each other: a
    /// `Record` that makes sense, or a `Replay` that does not, would be
    /// states someone would have to remember not to construct.
    Replay(TrailStep),
    /// Someone PLACES the slot where it belongs, and it is not a step the
    /// reader took.
    ///
    /// Today, seeding `[profile.start]` when entering a profile. It does not
    /// enter the trail — a "back" leading to the previous profile's
    /// directory offers a return to a place never come from — and there is
    /// nothing to rewind if the listing fails, because no place the reader
    /// should be returned to was ever abandoned.
    ///
    /// It exists as a variant and not as a `Record` that does not matter
    /// because both frontends have to do the SAME thing: the terminal seeds
    /// by building the pane from scratch, with no trail; the window goes
    /// through its `navigate_slot`, which records. With no way to say "this
    /// is not a step", the two surfaces ended up with different histories
    /// (ADR 0077).
    Seed,
}

impl Trail {
    /// The trail step this navigation is taking, if it is taking one at
    /// all. `None` for a [`Trail::Record`]: it did not leave the trail, so
    /// there is nothing to rewind if it ends badly. `None` also for
    /// [`Trail::Seed`], for the same reason.
    #[must_use]
    pub fn step(self) -> Option<TrailStep> {
        match self {
            Self::Record | Self::Seed => None,
            Self::Replay(step) => Some(step),
        }
    }
}

/// Which way `nav.back`/`nav.forward` are walking the trail. The two are the
/// same operation mirrored, so they share one body rather than two arms that
/// must be kept in step by hand.
///
/// Lives here for the same reason as [`Trail`], which carries it: a
/// frontend's TOFU modal suspends a navigation that can be a trail step, and
/// whoever answers the modal needs to know which direction it was going in
/// order to undo it if the answer ends up abandoning it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrailStep {
    /// `nav.back`.
    Back,
    /// `nav.forward`.
    Forward,
}

impl TrailStep {
    /// Fluent id for "there is nothing this way". A key that goes silent is
    /// indistinguishable from a broken one, so the exhausted trail SAYS so.
    #[must_use]
    pub fn empty_message(self) -> &'static str {
        match self {
            Self::Back => "msg-nav-no-back",
            Self::Forward => "msg-nav-no-forward",
        }
    }
}

/// Where `nav.enter` NAVIGATES to over this entry, if it navigates at all.
///
/// Three things can be opened by entering: a directory, a symlink — M0 does
/// not follow it to decide a copy's destination, but Enter does try, which
/// is what an orthodox file manager does — and a CONTAINER, which is
/// navigated from inside ([`archive_root_for`]). Anything else is a file,
/// and with a file Enter does something else: opens it.
///
/// Lives here because both frontends used to answer it on their own and
/// with DIFFERENT answers: the terminal entered a `.zip` and followed a
/// symlink, and the window looked at `kind != Dir` and handed it to the
/// desktop — with a comment claiming to be making "the same decision as the
/// TUI" (ADR 0077). The `..` row does not go through here: going up is not
/// a property of the entry, and it is asked by whoever knows the cursor is
/// on that row.
///
/// ```
/// use norte_proto::{Entry, EntryKind, VPath};
/// let zip = Entry {
///     path: VPath::parse("file:///home/cosas.zip").unwrap(),
///     kind: EntryKind::File,
///     size: None,
///     mtime_ms: None,
///     attrs: std::collections::BTreeMap::new(),
/// };
/// // A container is navigated from inside…
/// assert!(norte_frontend::nav::enter_target(&zip).is_some());
/// // …and a normal file is not navigated: Enter opens it.
/// let txt = Entry { path: VPath::parse("file:///home/a.txt").unwrap(), ..zip };
/// assert!(norte_frontend::nav::enter_target(&txt).is_none());
/// ```
#[must_use]
pub fn enter_target(e: &norte_proto::Entry) -> Option<norte_proto::VPath> {
    use norte_proto::EntryKind;
    if matches!(e.kind, EntryKind::Dir | EntryKind::Symlink) {
        return Some(e.path.clone());
    }
    archive_root_for(e)
}

/// If the entry is a navigable container (`.<format>` from proto's
/// whitelist, ASCII case-insensitive extension), the root of its interior
/// (ADR 0018). The extension→format map is presentation sugar; the real
/// validation belongs to the core. A SYMLINK to an archive does not count
/// as a container in v1 (a deliberate decision: it would require resolving
/// the target via the core's stat).
///
/// Lives here and not in a frontend because TWO of them answer it: the TUI
/// to decide whether `Enter` enters, and the window to decide whether
/// `pane.unpack` and `pane.test-archive` are available. Two extension
/// tables are two places for one to be forgotten, and then the same entry
/// navigates on one surface and not on the other (ADR 0066, decision D14).
///
/// ```
/// use norte_proto::{Entry, EntryKind, VPath};
/// let e = Entry {
///     attrs: std::collections::BTreeMap::new(),
///     path: VPath::parse("file:///x/cosas.ZIP").unwrap(),
///     kind: EntryKind::File,
///     size: None,
///     mtime_ms: None,
/// };
/// // The extension is case-insensitive…
/// assert!(norte_frontend::nav::archive_root_for(&e).is_some());
/// // …and a directory is not a container no matter what it is called.
/// let d = Entry { kind: EntryKind::Dir, ..e };
/// assert!(norte_frontend::nav::archive_root_for(&d).is_none());
/// ```
#[must_use]
pub fn archive_root_for(e: &norte_proto::Entry) -> Option<norte_proto::VPath> {
    use norte_proto::{EntryKind, VPath};
    // Extensions whose suffix does not match the format's token (#55):
    // `tar+gz` has no real `.tar+gz` in the world, people write
    // `.tgz`/`.tar.gz`. They are checked BEFORE the generic `.{format}` — a
    // `.tar.gz` would not match `.tar` anyway (it ends in `.gz`), so the
    // order is defensive, not strictly necessary today.
    const EXT_ALIASES: &[(&[u8], &str)] = &[(b".tar.gz", "tar+gz"), (b".tgz", "tar+gz")];
    fn ends_ci(name: &[u8], suffix: &[u8]) -> bool {
        name.len() >= suffix.len() && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
    }
    if e.kind != EntryKind::File {
        return None;
    }
    let name = e.path.file_name()?.as_bytes();
    let format = EXT_ALIASES
        .iter()
        .find(|(suffix, _)| ends_ci(name, suffix))
        .map(|(_, format)| *format)
        .or_else(|| {
            norte_proto::ARCHIVE_FORMATS
                .iter()
                .find(|f| ends_ci(name, format!(".{f}").as_bytes()))
                .copied()
        })?;
    // Fails (already composed with an outer `!`, etc.): not navigable —
    // Enter is a no-op.
    VPath::archive_compose(format, &e.path, &[]).ok()
}

/// The archive format a NAME suggests, among the ones norte knows how to
/// WRITE (#132).
///
/// Presentation sugar, same as [`archive_root_for`]: what decides is the
/// wire's explicit field, and this only translates what the reader just
/// typed. `rar` is not here — it is delegated and read-only (ADR 0056) —
/// so a `.rar` falls to `None` and the dialog says so instead of packing a
/// zip with a rar-shaped name.
///
/// Shared for the same reason as its neighbor: the TUI and the window offer
/// the same dialog, and two extension tables would end up packing into
/// different formats for the same name.
///
/// ```
/// use norte_proto::methods::ArchiveFormat;
/// use norte_frontend::nav::format_by_name;
/// assert_eq!(format_by_name(b"cosas.TGZ"), Some(ArchiveFormat::TarGz));
/// assert_eq!(format_by_name(b"cosas.zip"), Some(ArchiveFormat::Zip));
/// // What norte cannot write is not made up.
/// assert_eq!(format_by_name(b"cosas.rar"), None);
/// ```
#[must_use]
pub fn format_by_name(name: &[u8]) -> Option<norte_proto::methods::ArchiveFormat> {
    use norte_proto::methods::ArchiveFormat as F;
    let ends = |suf: &[u8]| {
        name.len() >= suf.len() && name[name.len() - suf.len()..].eq_ignore_ascii_case(suf)
    };
    if ends(b".tar.gz") || ends(b".tgz") {
        return Some(F::TarGz);
    }
    if ends(b".tar") {
        return Some(F::Tar);
    }
    if ends(b".zip") {
        return Some(F::Zip);
    }
    None
}

/// A size with a suffix (`4096`, `10M`, `1G`) in bytes, or `None` if it
/// cannot be understood (#132).
///
/// BINARY suffixes, which is what they mean in a file manager: `M` is 1 MiB,
/// not a million. With no suffix they are bytes. Zero is not valid:
/// splitting into zero-byte chunks never finishes.
///
/// Shared for the same reason as its neighbors: the TUI and the window ask
/// for the same size in the same dialog, and two ways of reading `10M`
/// would split the same typed value into differently-sized files.
///
/// ```
/// use norte_frontend::nav::parse_size;
/// assert_eq!(parse_size("4096"), Some(4096));
/// assert_eq!(parse_size("10M"), Some(10 * 1024 * 1024), "binary, not decimal");
/// assert_eq!(parse_size("0"), None, "a zero-byte chunk never finishes");
/// assert_eq!(parse_size("ten"), None);
/// ```
#[must_use]
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num, mult) = match s.as_bytes()[s.len() - 1].to_ascii_uppercase() {
        b'K' => (&s[..s.len() - 1], 1024_u64),
        b'M' => (&s[..s.len() - 1], 1024 * 1024),
        b'G' => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1),
    };
    let n: u64 = num.trim().parse().ok()?;
    n.checked_mul(mult).filter(|v| *v > 0)
}

/// The BASE name of a split file, given its FIRST chunk (#132).
///
/// Only from `.001`: starting from `.007` would join half a thing, and the
/// core only knows how to search forward. `None` if the name does not end
/// in `.001` or if what is left is not a legal name.
///
/// ```
/// use norte_frontend::nav::chunk_base;
/// assert_eq!(
///     chunk_base(b"pelicula.mkv.001").map(|s| s.as_bytes().to_vec()),
///     Some(b"pelicula.mkv".to_vec())
/// );
/// // From another chunk, no: it would join half a thing.
/// assert!(chunk_base(b"pelicula.mkv.007").is_none());
/// ```
#[must_use]
pub fn chunk_base(name: &[u8]) -> Option<norte_proto::Segment> {
    let base = name
        .len()
        .checked_sub(4)
        .filter(|n| name[*n] == b'.' && &name[n + 1..] == b"001")
        .map(|n| name[..n].to_vec())?;
    norte_proto::Segment::new(base).ok()
}

#[cfg(test)]
mod history_tests {
    use super::*;

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire")
    }

    #[test]
    fn history_push_dedup_cap_and_removal() {
        let mut h = History::default();
        for i in 0..40 {
            h.push(vp(&format!("mem:///d{i}")));
        }
        assert_eq!(h.entries().len(), 30, "cap");
        assert_eq!(h.entries()[0], vp("mem:///d39"), "most recent first");
        h.push(vp("mem:///d39"));
        assert_eq!(h.entries().len(), 30, "consecutive dedup");
        h.remove(&vp("mem:///d39"));
        assert!(
            !h.entries().contains(&vp("mem:///d39")),
            "removed after NotFound"
        );
    }

    /// review MINOR-3: `push`'s rustdoc promises that the same dir in
    /// NON-consecutive positions CAN repeat, and `remove` removes ALL
    /// occurrences — pin it with an explicit A→B→A case.
    #[test]
    fn history_allows_nonconsecutive_repeats_and_remove_removes_all() {
        let mut h = History::default();
        h.push(vp("mem:///a"));
        h.push(vp("mem:///b"));
        h.push(vp("mem:///a")); // NOT consecutive with the first "a" ("b" is in between)
        let count_to = |h: &History| h.entries().iter().filter(|p| **p == vp("mem:///a")).count();
        assert_eq!(
            count_to(&h),
            2,
            "non-consecutive repeat: two occurrences of a"
        );
        h.remove(&vp("mem:///a"));
        assert_eq!(count_to(&h), 0, "remove removes ALL occurrences");
    }

    #[test]
    fn the_trail_does_not_oscillate_between_two_directories() {
        // The defect this trail exists not to have: walking the MRU as if
        // it were a trail goes from A to B, back to A, and back to B — the
        // reader gets stuck between two dirs with no way out.
        let mut h = History::default();
        h.record(vp("mem:///a")); // leaving A for B
        h.record(vp("mem:///b")); // leaving B for C (now at C)
        assert_eq!(h.step_back(vp("mem:///c")), Some(vp("mem:///b")));
        assert_eq!(h.step_back(vp("mem:///b")), Some(vp("mem:///a")));
        assert_eq!(h.step_back(vp("mem:///a")), None, "the trail runs out");
    }

    #[test]
    fn forward_undoes_back_and_a_new_navigation_clears_it() {
        let mut h = History::default();
        h.record(vp("mem:///a"));
        h.record(vp("mem:///b"));
        assert_eq!(h.step_back(vp("mem:///c")), Some(vp("mem:///b")));
        assert_eq!(h.step_forward(vp("mem:///b")), Some(vp("mem:///c")));
        assert_eq!(h.step_forward(vp("mem:///c")), None);

        // Going back and NAVIGATING somewhere else cuts the forward branch:
        // it is the browser's semantics, and the opposite would offer a
        // "forward" into a history the reader has already abandoned.
        assert_eq!(h.step_back(vp("mem:///c")), Some(vp("mem:///b")));
        h.record(vp("mem:///b"));
        assert_eq!(h.step_forward(vp("mem:///z")), None, "branch pruned");
    }

    #[test]
    fn the_trail_does_not_touch_the_popups_mru() {
        // Two different questions: "where have I been" (the MRU the popup
        // paints) and "where was I a moment ago" (the trail). Going back is
        // not visiting somewhere new.
        let mut h = History::default();
        h.record(vp("mem:///a"));
        h.record(vp("mem:///b"));
        let before: Vec<VPath> = h.entries().iter().cloned().collect();
        let _ = h.step_back(vp("mem:///c"));
        let _ = h.step_forward(vp("mem:///b"));
        let after: Vec<VPath> = h.entries().iter().cloned().collect();
        assert_eq!(before, after, "the MRU is a separate matter");
    }

    /// "This directory is gone" is ONE fact: `remove` applies it to the MRU
    /// and the trail at once. Without this the popup would drop the entry
    /// and `nav.back` would keep pointing at the same dead dir.
    #[test]
    fn remove_prunes_the_trail_and_not_just_the_mru() {
        let mut h = History::default();
        h.record(vp("mem:///a"));
        h.record(vp("mem:///b"));
        // And also the forward branch: the same dir can be in both.
        assert_eq!(h.step_back(vp("mem:///c")), Some(vp("mem:///b")));
        assert_eq!(h.fwd_len(), 1);

        h.remove(&vp("mem:///b"));
        assert_eq!(h.back_len(), 1, "b leaves the back trail");
        assert!(!h.entries().contains(&vp("mem:///b")), "and the MRU");
        assert_eq!(
            h.step_back(vp("mem:///c")),
            Some(vp("mem:///a")),
            "back jumps to the next living one, not the removed dir"
        );

        h.remove(&vp("mem:///c"));
        assert_eq!(h.fwd_len(), 0, "and the forward branch");
    }

    #[test]
    fn the_trail_is_bounded_like_the_mru() {
        let mut h = History::with_capacity(HISTORY_MAX);
        for i in 0..(HISTORY_MAX + 20) {
            h.record(vp(&format!("mem:///d{i}")));
        }
        assert_eq!(
            h.back_len(),
            HISTORY_MAX,
            "the trail does not grow without end"
        );
    }

    /// The cap above only moves `record`. The invariant the type documents
    /// — the one `step_forward` relies on to push onto `back` without
    /// checking anything — is about the SUM of the two stacks, so the three
    /// operations have to be alternated past the cap: walk to the bottom of
    /// the trail, come all the way back, and navigate again from there.
    #[test]
    fn the_cap_holds_while_alternating_the_three_operations() {
        let mut h = History::with_capacity(HISTORY_MAX);
        let total = |h: &History| h.back_len() + h.fwd_len();

        let mut cur = vp("mem:///start");
        for i in 0..(HISTORY_MAX * 2) {
            h.record(cur.clone());
            cur = vp(&format!("mem:///d{i}"));
            assert!(total(&h) <= HISTORY_MAX, "record does not overflow the sum");
        }
        assert_eq!(h.back_len(), HISTORY_MAX, "the trail is full");

        // To the bottom: each step moves one dir from one stack to the other.
        let mut steps = 0;
        while let Some(target) = h.step_back(cur.clone()) {
            cur = target;
            steps += 1;
            assert!(total(&h) <= HISTORY_MAX, "back does not overflow the sum");
        }
        assert_eq!(steps, HISTORY_MAX, "the whole trail was walked");
        assert_eq!(h.fwd_len(), HISTORY_MAX, "all the memory is ahead");

        // And back: this is where `step_forward` pushes onto `back` without
        // checking the cap. Without the invariant, `back` would end up over
        // it.
        while let Some(target) = h.step_forward(cur.clone()) {
            cur = target;
            assert!(
                total(&h) <= HISTORY_MAX,
                "forward does not overflow the sum"
            );
        }
        assert_eq!(h.back_len(), HISTORY_MAX, "the trail is full again");

        // A new navigation from the cap does not overflow it either.
        h.record(cur);
        assert!(total(&h) <= HISTORY_MAX);
        assert_eq!(h.fwd_len(), 0, "and it prunes the forward branch");
    }

    /// The trail is walked from most recent to oldest and the first one that
    /// is NOT from the session being closed is returned.
    #[test]
    fn the_return_skips_everything_from_the_closed_machine() {
        let trail = [vp("file:///home/o"), vp("sftp://srv/a"), vp("sftp://srv/b")];
        assert_eq!(
            regreso_after_disconnect(&vp("sftp://srv/b"), &trail),
            Some(vp("file:///home/o")),
        );
    }

    /// An archive ON the machine being closed is that same machine: it is
    /// served by the same connection (the core evicts both keys at once),
    /// so landing there would open a new connection with its
    /// reauthentication — exactly what the gesture asked not to have.
    /// Comparing the raw scheme, `zip+sftp` did not match `sftp` and the
    /// pane fell right inside.
    #[test]
    fn an_archive_on_that_machine_is_still_that_machine() {
        let trail = [
            vp("file:///home/o"),
            vp("zip+sftp://srv/x.zip%21/dentro"),
            vp("sftp://srv/a"),
        ];
        assert_eq!(
            regreso_after_disconnect(&vp("sftp://srv/b"), &trail),
            Some(vp("file:///home/o")),
        );
        // And the other way round: closing from INSIDE the archive does not
        // return to the outside of the same machine either.
        assert_eq!(
            regreso_after_disconnect(&vp("zip+sftp://srv/x.zip%21/dentro"), &trail),
            Some(vp("file:///home/o")),
        );
    }

    /// The same session is scheme AND authority: another server over sftp is
    /// another connection, and going back there does not reopen the one
    /// that closed.
    #[test]
    fn another_server_of_the_same_scheme_is_valid() {
        let trail = [vp("sftp://otro/x"), vp("sftp://srv/a")];
        assert_eq!(
            regreso_after_disconnect(&vp("sftp://srv/a"), &trail),
            Some(vp("sftp://otro/x")),
        );
    }

    /// A pane born remote — or whose whole trail belongs to that machine —
    /// has nowhere to go back to: the caller decides, and falls back home.
    #[test]
    fn with_nothing_foreign_in_the_trail_there_is_no_return() {
        assert_eq!(regreso_after_disconnect(&vp("sftp://srv/a"), &[]), None);
        let all_its_own = [vp("sftp://srv/a"), vp("sftp://srv/b")];
        assert_eq!(
            regreso_after_disconnect(&vp("sftp://srv/b"), &all_its_own),
            None,
        );
    }
}
