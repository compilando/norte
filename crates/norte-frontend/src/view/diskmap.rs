//! The disk map's state (phase 4), shared by both surfaces.
//!
//! Nothing gets painted here: the layout into rectangles is
//! [`crate::treemap`], and who draws it is each frontend's job. This is what
//! BOTH need to know — which directory is being shown, what has been
//! measured, which child is chosen, and whether the measurement is still
//! running— and it lives together for the usual reason: a decision written
//! twice diverges silently (ADR 0077).
//!
//! # The chosen child is remembered by NAME
//! A map gets measured again: on refresh, on returning from an external
//! change, on entering and leaving. If the chosen one were an index, a
//! measurement that no longer brings back the child above it would move the
//! selection to a different file without anyone pressing a key — and in a map
//! the next key ENTERS whatever is chosen. The name identifies it; the
//! position only finds it.
//!
//! `pub` identifiers in this module (`State`/`Idle`/`Measuring`/`Done`/
//! `Failure`, and the `DiskMap` methods `report`/`chosen`/`state`/
//! `aim`/`measuring`/`failure`/`land`/`mover`/`choose`) are Spanish and
//! reported for a cross-file rename in phase 2: they are called from
//! `norte-tui` (`src/jobs/diskmap.rs`, `src/ui/panels.rs`,
//! `src/screens/side_nav.rs`) and `norte-ui-host`
//! (`src/controller/diskmap.rs`), both outside this task's file set.

use norte_proto::methods::{DirUsageChild, FsDirUsageReportResult};
use norte_proto::{Segment, TaskId, VPath};

/// What point this map's measurement is at.
///
/// Four states and not an `Option`, because "not requested", "measuring", and
/// "requested and failed" have to be shown differently: a panel that
/// collapsed the three would say "empty" both about a directory nobody has
/// looked at and about one whose permission was denied.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum State {
    /// Nobody has requested anything yet.
    #[default]
    Idle,
    /// A measurement is running; it can be cancelled.
    Measuring(TaskId),
    /// It finished and what was measured is in the report.
    Done,
    /// It failed, and this is the reason, already translated, to show it.
    Failure(String),
}

/// What a slot's disk map knows right now.
#[derive(Debug, Default)]
pub struct DiskMap {
    /// Which directory is being described. `None` = none yet.
    dir: Option<VPath>,
    /// What has been measured. Empty while nothing has landed.
    report: FsDirUsageReportResult,
    /// The name of the chosen child, if there is one.
    chosen: Option<Segment>,
    /// What point the measurement is at.
    state: State,
}

impl DiskMap {
    /// An empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The directory being described.
    #[must_use]
    pub fn dir(&self) -> Option<&VPath> {
        self.dir.as_ref()
    }

    /// What has been measured so far.
    #[must_use]
    pub fn report(&self) -> &FsDirUsageReportResult {
        &self.report
    }

    /// What point the measurement is at.
    #[must_use]
    pub fn state(&self) -> &State {
        &self.state
    }

    /// Points at another directory: forgets what was measured and the
    /// selection.
    ///
    /// What was measured is OF a directory, so keeping it on change would
    /// paint the previous one's map under the new one's title — for however
    /// long the measurement takes, which is exactly the moment someone is
    /// looking at it.
    pub fn aim(&mut self, dir: VPath) {
        self.dir = Some(dir);
        self.report = FsDirUsageReportResult::default();
        self.chosen = None;
        self.state = State::Idle;
    }

    /// Says that a measurement is running.
    pub fn measuring(&mut self, task: TaskId) {
        self.state = State::Measuring(task);
    }

    /// The running measurement's task, if there is one (to cancel it).
    #[must_use]
    pub fn task(&self) -> Option<TaskId> {
        match self.state {
            State::Measuring(id) => Some(id),
            _ => None,
        }
    }

    /// Says why the measurement could not be taken.
    pub fn failure(&mut self, motivo: String) {
        self.state = State::Failure(motivo);
    }

    /// Lands a report —partial or final— onto this map.
    ///
    /// **The selection is kept by name**, and dropped only if that child is
    /// no longer there. A partial report arrives several times while the
    /// measurement runs, and with the selection tied to a position the
    /// cursor would go jumping from file to file as the children kept
    /// arriving.
    ///
    /// `ready` distinguishes the last report from the ones in between: it is
    /// what decides whether this can be saved to the cache.
    pub fn land(&mut self, report: FsDirUsageReportResult, ready: bool) {
        let follows = self
            .chosen
            .as_ref()
            .is_some_and(|n| report.children.iter().any(|c| c.name == *n));
        if !follows {
            self.chosen = None;
        }
        self.report = report;
        if ready {
            self.state = State::Done;
        }
    }

    /// The chosen child, if there is one and it is still there.
    #[must_use]
    pub fn chosen(&self) -> Option<&DirUsageChild> {
        let n = self.chosen.as_ref()?;
        self.report.children.iter().find(|c| c.name == *n)
    }

    /// Moves the selection `delta` positions over the NAMED children.
    ///
    /// With nothing chosen, the first move chooses the first one —which is
    /// the largest, because the report arrives ordered by listing but the
    /// map is walked the way it is painted— instead of doing nothing: a key
    /// that does nothing the first time seems broken.
    ///
    /// It is CLAMPED at both ends and does not wrap: in a list of rectangles
    /// the edge is a legitimate position to stay at, and wrapping would make
    /// moving down from the last one jump to the other side of the screen.
    pub fn mover(&mut self, delta: isize) {
        if self.report.children.is_empty() {
            self.chosen = None;
            return;
        }
        let actual = self
            .chosen
            .as_ref()
            .and_then(|n| self.report.children.iter().position(|c| c.name == *n));
        let new = match actual {
            None => 0,
            Some(i) => {
                let max = self.report.children.len().saturating_sub(1);
                let cand = isize::try_from(i).unwrap_or(0).saturating_add(delta);
                usize::try_from(cand).unwrap_or(0).min(max)
            }
        };
        self.chosen = self.report.children.get(new).map(|c| c.name.clone());
    }

    /// Chooses a child by its name —what a click on its rectangle does— and
    /// says whether it existed.
    pub fn choose(&mut self, name: &Segment) -> bool {
        let exists = self.report.children.iter().any(|c| c.name == *name);
        if exists {
            self.chosen = Some(name.clone());
        }
        exists
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::EntryKind;

    fn seg(s: &str) -> Segment {
        Segment::new(s.as_bytes().to_vec()).expect("segment")
    }

    fn child(name: &str, bytes: u64) -> DirUsageChild {
        DirUsageChild {
            name: seg(name),
            kind: EntryKind::Dir,
            bytes,
            entries: 1,
            partial: false,
        }
    }

    fn report(names: &[&str]) -> FsDirUsageReportResult {
        FsDirUsageReportResult {
            children: names.iter().map(|n| child(n, 10)).collect(),
            listed: true,
            ..FsDirUsageReportResult::default()
        }
    }

    /// The chosen one is remembered by NAME: a report that no longer brings
    /// back the neighbour above it does not move the selection to a
    /// different file.
    ///
    /// It is the difference that matters, because the next key ENTERS the
    /// chosen one: with an index, measuring again could leave the cursor
    /// over a directory different from the one the reader was looking at.
    #[test]
    fn the_chosen_one_is_remembered_by_name_and_not_by_position() {
        let mut m = DiskMap::new();
        m.land(report(&["a", "b", "c"]), true);
        assert!(m.choose(&seg("c")));
        // Measures again and `a` is no longer there: `c` is still chosen even
        // though it is now one position higher.
        m.land(report(&["b", "c"]), true);
        assert_eq!(m.chosen().map(|c| c.name.clone()), Some(seg("c")));
    }

    /// If the chosen one disappears, it is dropped: showing something as
    /// chosen when it is no longer there is promising a key that cannot
    /// work.
    #[test]
    fn if_the_chosen_one_disappears_it_is_dropped() {
        let mut m = DiskMap::new();
        m.land(report(&["a", "b"]), true);
        assert!(m.choose(&seg("a")));
        m.land(report(&["b"]), true);
        assert!(m.chosen().is_none());
    }

    /// The first move chooses: a key that does nothing the first time seems
    /// broken.
    #[test]
    fn the_first_move_chooses_the_first_one() {
        let mut m = DiskMap::new();
        m.land(report(&["a", "b"]), true);
        m.mover(1);
        assert_eq!(m.chosen().map(|c| c.name.clone()), Some(seg("a")));
    }

    /// It is CLAMPED at the ends, it does not wrap.
    #[test]
    fn moving_clamps_at_the_edges() {
        let mut m = DiskMap::new();
        m.land(report(&["a", "b", "c"]), true);
        m.choose(&seg("c"));
        m.mover(1);
        assert_eq!(
            m.chosen().map(|c| c.name.clone()),
            Some(seg("c")),
            "all the way down stays down"
        );
        m.choose(&seg("a"));
        m.mover(-1);
        assert_eq!(m.chosen().map(|c| c.name.clone()), Some(seg("a")));
    }

    /// Pointing at another directory forgets what was measured: the previous
    /// one's map under the new one's title is the wrong answer for exactly
    /// the moment someone is looking at it.
    #[test]
    fn pointing_at_another_directory_forgets_what_was_measured() {
        let mut m = DiskMap::new();
        m.land(report(&["a"]), true);
        m.choose(&seg("a"));
        m.aim(VPath::parse("mem:///other").expect("wire"));
        assert!(m.report().children.is_empty());
        assert!(m.chosen().is_none());
        assert_eq!(m.state(), &State::Idle);
    }

    /// A half-done map is not declared finished: `land(_, false)` leaves
    /// the state where it was so the panel keeps saying it is measuring.
    #[test]
    fn a_partial_report_does_not_declare_the_measurement_finished() {
        let mut m = DiskMap::new();
        m.measuring(TaskId::new(7));
        m.land(report(&["a"]), false);
        assert_eq!(m.state(), &State::Measuring(TaskId::new(7)));
        assert_eq!(m.task(), Some(TaskId::new(7)));
        m.land(report(&["a", "b"]), true);
        assert_eq!(m.state(), &State::Done);
        assert!(
            m.task().is_none(),
            "once finished there is nothing left to cancel"
        );
    }
}
