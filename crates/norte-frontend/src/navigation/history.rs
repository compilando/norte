//! Navigation history seen as LISTS: the rows both frontends paint, the
//! popular directories, and the one decision of "this counts as a visit"
//! (spec 2026-09-15, phase 1).
//!
//! [`crate::nav::History`] is ONE pane's structure — MRU, trail, jump
//! point. This is what is built on top to show it, and what belongs to no
//! pane: [`Popular`] is the whole session's, as in Krusader.
//!
//! Lives here and not in each frontend for the same reason as `History`
//! (ADR 0066 D14): which rows come out, in what order and with what mark
//! cannot depend on who paints them.

use crate::nav::{History, Trail};
use norte_proto::VPath;
use serde::{Deserialize, Serialize};

/// How many popular directories the session remembers.
pub const POPULAR_CAP: usize = 50;

/// Fluent key for "there is no jump point".
pub const NO_JUMP_POINT: &str = "msg-nav-no-jump-point";

/// A popular directory: how many times it was reached and when the last
/// time was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PopularEntry {
    /// The directory.
    pub path: VPath,
    /// How many of the reader's navigations ended up here.
    pub visits: u32,
    /// Order of the last visit: a counter of the list itself, not a clock —
    /// this way the tiebreak is deterministic and does not depend on the
    /// clock of the machine that wrote the session.
    #[serde(default)]
    pub last: u64,
}

/// The directories most often visited (Krusader's "Popular URLs",
/// `Ctrl+Z`).
///
/// ONE list for the whole session and not one per pane: the question is
/// "where do I usually go", and the answer does not change depending on
/// which side of the screen it is asked from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Popular {
    entries: Vec<PopularEntry>,
    clock: u64,
}

impl Popular {
    /// Rebuilds the list from a session. A repeated path (a hand-edited
    /// file) keeps its FIRST appearance, and whatever goes over
    /// [`POPULAR_CAP`] is evicted by the same rule as when visiting.
    #[must_use]
    pub fn from_entries(entries: Vec<PopularEntry>) -> Self {
        let mut p = Self::default();
        for e in entries {
            if p.entries.iter().any(|x| x.path == e.path) {
                continue;
            }
            p.clock = p.clock.max(e.last);
            p.entries.push(e);
        }
        while p.entries.len() > POPULAR_CAP {
            p.expulsa();
        }
        p
    }

    /// The entries in the order they are stored (not the paint order: see
    /// [`Self::ranked`]).
    #[must_use]
    pub fn entries(&self) -> &[PopularEntry] {
        &self.entries
    }

    /// Notes a visit to `path`. With the list full, a new path evicts the
    /// one with fewest visits and, on a tie, the one visited longest ago.
    pub fn visit(&mut self, path: &VPath) {
        self.clock += 1;
        if let Some(e) = self.entries.iter_mut().find(|e| e.path == *path) {
            e.visits = e.visits.saturating_add(1);
            e.last = self.clock;
            return;
        }
        if self.entries.len() >= POPULAR_CAP {
            self.expulsa();
        }
        self.entries.push(PopularEntry {
            path: path.clone(),
            visits: 1,
            last: self.clock,
        });
    }

    /// Removes `path` (`dialog.remove`, or a directory that no longer
    /// exists).
    pub fn remove(&mut self, path: &VPath) {
        self.entries.retain(|e| e.path != *path);
    }

    /// Empties the list (`dialog.clear`).
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// The entries from most to least visited; on a tie, the most recent
    /// first.
    #[must_use]
    pub fn ranked(&self) -> Vec<&PopularEntry> {
        let mut v: Vec<&PopularEntry> = self.entries.iter().collect();
        v.sort_by(|a, b| b.visits.cmp(&a.visits).then(b.last.cmp(&a.last)));
        v
    }

    fn expulsa(&mut self) {
        let victim = self
            .entries
            .iter()
            .enumerate()
            .min_by_key(|(_, e)| (e.visits, e.last))
            .map(|(i, _)| i);
        if let Some(i) = victim {
            self.entries.swap_remove(i);
        }
    }
}

/// Records a navigation in the pane's trail and in the popular ones, if it
/// counts. Returns whether it counted.
///
/// The ONE decision of "this is a reader step", shared by both frontends.
/// Two conditions, the same ones the trail already kept:
///
/// - `prev != dir`: navigating to the directory already shown is a
///   refresh, not a step.
/// - `trail == Trail::Record`: a `Replay` is the trail walking itself
///   (counting it would make it oscillate), and a `Seed` places the pane
///   with the reader never going anywhere — not a visit either.
///
/// The trail saves where you LEAVE FROM (`prev`); the popular ones, where
/// you ARRIVE (`dir`).
///
/// ```
/// use norte_frontend::history::{Popular, record_visit};
/// use norte_frontend::nav::{History, Trail, TrailStep};
/// use norte_proto::VPath;
/// let vp = |s: &str| VPath::parse(s).unwrap();
/// let (mut h, mut p) = (History::default(), Popular::default());
/// assert!(record_visit(&mut h, &mut p, &vp("mem:///a"), &vp("mem:///b"), Trail::Record));
/// assert!(!record_visit(&mut h, &mut p, &vp("mem:///b"), &vp("mem:///a"), Trail::Replay(TrailStep::Back)));
/// assert_eq!(h.back_len(), 1);
/// assert_eq!(p.entries().len(), 1);
/// ```
pub fn record_visit(
    history: &mut History,
    popular: &mut Popular,
    prev: &VPath,
    dir: &VPath,
    trail: Trail,
) -> bool {
    if !counts_as_step(prev, dir, trail) {
        return false;
    }
    history.record(prev.clone());
    popular.visit(dir);
    true
}

/// [`record_visit`]'s decision, alone: whether navigating from `prev` to
/// `dir` is a reader step.
///
/// Separate because the window has to split the event in two: the trail is
/// recorded when the listing is REQUESTED — the terminal, when it arrives —
/// and the visit to the popular ones waits until it arrives, because a
/// listing that fails is not a place that was gone to. Both halves ask
/// HERE, so they cannot disagree about what counts.
#[must_use]
pub fn counts_as_step(prev: &VPath, dir: &VPath, trail: Trail) -> bool {
    prev != dir && trail == Trail::Record
}

/// Where `nav.jump-back` leads, or the Fluent key for why it leads nowhere.
///
/// # Errors
///
/// [`NO_JUMP_POINT`] if the pane has no jump point.
pub fn jump_target(history: &History) -> Result<VPath, &'static str> {
    history.jump().cloned().ok_or(NO_JUMP_POINT)
}

/// What a history list's row is with respect to the reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryMark {
    /// The directory the pane is in right now (Krusader's check).
    Current,
    /// A place that was passed through.
    Visited,
    /// A place on the forward branch: the reader went back and can return.
    Forward,
}

/// A row of the history or popular list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRow {
    /// Where it navigates to.
    pub path: VPath,
    /// What it is with respect to the reader.
    pub mark: HistoryMark,
}

/// A row's mark's Fluent key, or `None` if the row carries none. A single
/// table for both frontends: the TUI paints it behind the path and the
/// window in the row's detail, but the WORD is the same.
#[must_use]
pub fn mark_key(mark: HistoryMark) -> Option<&'static str> {
    match mark {
        HistoryMark::Current => Some("history-mark-current"),
        HistoryMark::Forward => Some("history-mark-forward"),
        HistoryMark::Visited => None,
    }
}

/// Where a history list's cursor starts: on the SECOND row if the first is
/// the current directory, because you do not want to go where you already
/// are.
#[must_use]
pub fn start_cursor(rows: &[HistoryRow]) -> usize {
    usize::from(rows.len() > 1 && rows.first().is_some_and(|r| r.mark == HistoryMark::Current))
}

/// A pane's history list rows (`pane.history`).
///
/// First the CURRENT directory, marked; then the MRU, from most to least
/// recent, without listing the current one again. Those on the forward
/// branch carry [`HistoryMark::Forward`]. `filter` matches by subsequence
/// over the folded paintable path, same as the palette; empty matches
/// everything.
#[must_use]
pub fn history_rows(
    history: &History,
    current: &VPath,
    filter: &str,
    enc: Option<norte_encoding::NameEncoding>,
) -> Vec<HistoryRow> {
    let matches = matcher(filter, enc);
    let mut rows = Vec::with_capacity(history.entries().len() + 1);
    if matches(current) {
        rows.push(HistoryRow {
            path: current.clone(),
            mark: HistoryMark::Current,
        });
    }
    for p in history.entries().iter().filter(|p| *p != current) {
        if !matches(p) {
            continue;
        }
        let mark = if history.forward_trail().contains(p) {
            HistoryMark::Forward
        } else {
            HistoryMark::Visited
        };
        rows.push(HistoryRow {
            path: p.clone(),
            mark,
        });
    }
    rows
}

/// The popular list's rows (`pane.popular`), from most to least visited.
/// The current directory's carries [`HistoryMark::Current`] but does not
/// move from its spot: here the order IS the information.
#[must_use]
pub fn popular_rows(popular: &Popular, current: &VPath, filter: &str) -> Vec<HistoryRow> {
    // No reinterpretation: the popular ones belong to the whole session,
    // and applying a pane's encoding to them would be inventing what is
    // not written.
    let matches = matcher(filter, None);
    popular
        .ranked()
        .into_iter()
        .filter(|e| matches(&e.path))
        .map(|e| HistoryRow {
            path: e.path.clone(),
            mark: if e.path == *current {
                HistoryMark::Current
            } else {
                HistoryMark::Visited
            },
        })
        .collect()
}

/// Whether a path matches `filter`, read with reinterpretation `enc`.
///
/// Folded SEGMENT BY SEGMENT with [`crate::nav::fold_with`] — the decoded,
/// unmasked name, same as the quick search — and not the paintable path,
/// which already comes masked and knows nothing about encodings: with that
/// one, a `Папка` the pane correctly shows under CP866 would not match `п`
/// (encoding-auditor, phase 1). Filtering never changes a destination.
fn matcher(filter: &str, enc: Option<norte_encoding::NameEncoding>) -> impl Fn(&VPath) -> bool {
    let needle = crate::nav::fold(filter.as_bytes());
    move |p: &VPath| {
        needle.is_empty() || {
            let haystack: Vec<String> = p
                .segments()
                .map(|s| crate::nav::fold_with(s, enc))
                .collect();
            crate::palette_state::is_subsequence(&needle, &haystack.join("/"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nav::{HISTORY_DEFAULT, HISTORY_MAX, HISTORY_MIN, TrailStep};
    use proptest::prelude::*;

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire")
    }

    /// `norte-config` validates `[ui] history_size` with its own numbers
    /// because it cannot depend on this crate; this test is what stops the
    /// two ceilings from drifting apart.
    #[test]
    fn the_configs_caps_are_the_historys() {
        use norte_config::load::UiChrome;
        assert_eq!(UiChrome::MIN_HISTORY_SIZE as usize, HISTORY_MIN);
        assert_eq!(UiChrome::MAX_HISTORY_SIZE as usize, HISTORY_MAX);
        assert_eq!(UiChrome::DEFAULT_HISTORY_SIZE as usize, HISTORY_DEFAULT);
    }

    #[test]
    fn a_replay_or_a_seed_does_not_count_as_a_visit() {
        let (mut h, mut p) = (History::default(), Popular::default());
        let (a, b) = (vp("mem:///a"), vp("mem:///b"));
        assert!(!record_visit(
            &mut h,
            &mut p,
            &a,
            &b,
            Trail::Replay(TrailStep::Back)
        ));
        assert!(!record_visit(&mut h, &mut p, &a, &b, Trail::Seed));
        assert!(!record_visit(&mut h, &mut p, &a, &a, Trail::Record));
        assert_eq!((h.back_len(), p.entries().len()), (0, 0));
        assert!(record_visit(&mut h, &mut p, &a, &b, Trail::Record));
        assert_eq!(h.trail(), &[a]);
        assert_eq!(p.entries()[0].path, b, "counts where you ARRIVE");
    }

    #[test]
    fn popular_evicts_the_least_visited_and_on_a_tie_the_oldest() {
        let mut p = Popular::default();
        for i in 0..POPULAR_CAP {
            p.visit(&vp(&format!("mem:///d{i}")));
        }
        // d1 goes up to two visits: no longer a candidate.
        p.visit(&vp("mem:///d1"));
        p.visit(&vp("mem:///nueva"));
        assert_eq!(p.entries().len(), POPULAR_CAP);
        assert!(
            !p.entries().iter().any(|e| e.path == vp("mem:///d0")),
            "d0: one visit and the oldest"
        );
        assert!(p.entries().iter().any(|e| e.path == vp("mem:///d1")));
        assert_eq!(p.ranked()[0].path, vp("mem:///d1"));
    }

    #[test]
    fn popular_from_session_removes_duplicates_and_respects_the_cap() {
        let e = |s: &str, visits, last| PopularEntry {
            path: vp(s),
            visits,
            last,
        };
        let mut v = vec![e("mem:///a", 3, 7), e("mem:///a", 9, 9)];
        v.extend((0..POPULAR_CAP).map(|i| e(&format!("mem:///x{i}"), 1, i as u64)));
        let mut p = Popular::from_entries(v);
        assert_eq!(p.entries().len(), POPULAR_CAP);
        assert_eq!(
            p.entries()
                .iter()
                .filter(|x| x.path == vp("mem:///a"))
                .count(),
            1
        );
        p.visit(&vp("mem:///a"));
        let a = p
            .entries()
            .iter()
            .find(|x| x.path == vp("mem:///a"))
            .expect("a");
        assert_eq!(a.visits, 4, "the first appearance was kept");
        assert!(a.last > 9, "the clock follows the session's highest");
    }

    #[test]
    fn rows_mark_the_current_first_and_the_branch_ahead() {
        let mut h = History::default();
        h.record(vp("mem:///a"));
        h.record(vp("mem:///b"));
        // We are at C; back to B: C is left on the forward branch.
        assert_eq!(h.step_back(vp("mem:///c")), Some(vp("mem:///b")));
        let rows = history_rows(&h, &vp("mem:///b"), "", None);
        assert_eq!(rows[0].mark, HistoryMark::Current);
        assert_eq!(rows[0].path, vp("mem:///b"));
        assert!(
            rows[1..].iter().all(|r| r.path != vp("mem:///b")),
            "the current one is not repeated"
        );
        let a = rows.iter().find(|r| r.path == vp("mem:///a")).expect("a");
        assert_eq!(a.mark, HistoryMark::Visited);
        // C is not in the MRU (C was never left with a Record), so it is
        // not a row; what IS checked is the mark when it is one.
        h.push(vp("mem:///c"));
        let rows = history_rows(&h, &vp("mem:///b"), "", None);
        let c = rows.iter().find(|r| r.path == vp("mem:///c")).expect("c");
        assert_eq!(c.mark, HistoryMark::Forward);
    }

    #[test]
    fn the_filter_matches_by_subsequence_case_insensitively() {
        let mut h = History::default();
        h.record(vp("mem:///Documentos/facturas"));
        h.record(vp("mem:///tmp"));
        let rows = history_rows(&h, &vp("mem:///casa"), "dfac", None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, vp("mem:///Documentos/facturas"));
    }

    #[test]
    fn removing_a_path_erases_its_jump_point() {
        let mut h = History::default();
        h.set_jump(vp("mem:///a"));
        assert_eq!(jump_target(&h), Ok(vp("mem:///a")));
        h.remove(&vp("mem:///a"));
        assert_eq!(jump_target(&h), Err(NO_JUMP_POINT));
    }

    #[test]
    fn clearing_preserves_the_jump_point_and_the_cap() {
        let mut h = History::with_capacity(10);
        h.record(vp("mem:///a"));
        h.set_jump(vp("mem:///j"));
        h.clear();
        assert_eq!((h.back_len(), h.entries().len()), (0, 0));
        assert_eq!(h.jump(), Some(&vp("mem:///j")));
        assert_eq!(h.capacity(), 10);
    }

    #[test]
    fn the_cap_is_bounded_and_lowering_it_keeps_what_is_near() {
        assert_eq!(History::with_capacity(0).capacity(), HISTORY_MIN);
        assert_eq!(History::with_capacity(9999).capacity(), HISTORY_MAX);
        assert_eq!(History::default().capacity(), HISTORY_DEFAULT);
        let mut h = History::with_capacity(20);
        for i in 0..20 {
            h.record(vp(&format!("mem:///d{i}")));
        }
        h.set_capacity(5);
        assert_eq!(h.back_len(), 5);
        assert_eq!(
            h.trail().last(),
            Some(&vp("mem:///d19")),
            "the last one walked"
        );
        assert_eq!(h.entries()[0], vp("mem:///d19"));
        assert_eq!(h.entries().len(), 5);
    }

    #[test]
    fn lowering_the_cap_makes_the_forward_branch_lose_its_far_tip() {
        let mut h = History::with_capacity(10);
        for i in 0..6 {
            h.record(vp(&format!("mem:///d{i}")));
        }
        // At d6; three back: fwd = [d6, d5, d4], the next one forward is d4.
        let mut cur = vp("mem:///d6");
        for _ in 0..3 {
            cur = h.step_back(cur).expect("back");
        }
        h.set_capacity(5);
        assert_eq!(h.back_len() + h.fwd_len(), 5);
        assert_eq!(
            h.step_forward(cur),
            Some(vp("mem:///d4")),
            "the near one stays"
        );
    }

    #[derive(Debug, Clone)]
    enum Op {
        Record(u8),
        Back(u8),
        Forward(u8),
        Remove(u8),
        Cap(usize),
        // A session written with ANOTHER cap (rust-reviewer, phase 1):
        // `seed` does not go through `record`, so it has to restore the
        // invariant itself.
        Seed(Vec<u8>, Vec<u8>),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => (0u8..12).prop_map(Op::Record),
            2 => (0u8..12).prop_map(Op::Back),
            2 => (0u8..12).prop_map(Op::Forward),
            1 => (0u8..12).prop_map(Op::Remove),
            1 => (0usize..80).prop_map(Op::Cap),
            1 => (
                proptest::collection::vec(0u8..12, 0..80),
                proptest::collection::vec(0u8..12, 0..80),
            )
                .prop_map(|(b, f)| Op::Seed(b, f)),
        ]
    }

    proptest! {
        /// The invariant that bounds the trail's memory, under any
        /// sequence of operations — including changing the cap on the fly.
        #[test]
        fn the_trail_invariant_holds_up_under_any_sequence(
            ops in proptest::collection::vec(op(), 0..200),
            cap in 0usize..80,
        ) {
            let mut h = History::with_capacity(cap);
            for o in ops {
                let d = |i: u8| vp(&format!("mem:///d{i}"));
                match o {
                    Op::Record(i) => h.record(d(i)),
                    Op::Back(i) => { let _ = h.step_back(d(i)); }
                    Op::Forward(i) => { let _ = h.step_forward(d(i)); }
                    Op::Remove(i) => h.remove(&d(i)),
                    Op::Cap(c) => h.set_capacity(c),
                    Op::Seed(b, f) => h.seed(
                        b.into_iter().map(d).collect(),
                        f.into_iter().map(d).collect(),
                    ),
                }
                prop_assert!(h.back_len() + h.fwd_len() <= h.capacity());
                prop_assert!(h.entries().len() <= h.capacity());
                prop_assert!((HISTORY_MIN..=HISTORY_MAX).contains(&h.capacity()));
            }
        }
    }
}
