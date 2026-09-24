//! The journal's timeline (WOW program phase 7): what has been done on this
//! machine, in order, and how far back it can go.
//!
//! The model is shared by both frontends. What lives here is what cannot be
//! decided twice without the two answers drifting apart:
//!
//! - **What counts as a ROW.** A batch (`batch_id`) is one row, not `n`: it is
//!   undone whole or not touched at all, so offering a cut through the middle
//!   of one would be offering something that does not exist.
//! - **What marking one means.** "Go back to here" keeps the marked row
//!   whole, and undoes what came after it. That is why the cutoff is the
//!   NEWEST `seq` in the group and not the oldest: with the oldest, the
//!   marked batch itself would be undone halfway.
//! - **How many entries it is going to take with it.** It is counted BEFORE
//!   asking, because a confirmation that does not say how many is not a
//!   confirmation.
//!
//! What is NOT here: colors, keys and how a dot gets painted. That belongs to
//! each frontend, and it is the only thing that really differs between a
//! terminal and a window.

use norte_proto::methods::JournalRow;

/// How a row's path is PAINTED: with no `file://` in front (the local scheme
/// is not announced, per [`crate::path_display`]'s rule) and masked through
/// the same gate as a listing. A path that fails to parse is left as it came:
/// it is already text masked by whoever built the row.
///
/// In a narrow column, `file:///ho…` said nothing; `/home/oscar/Down…` did
/// (captured 2026-09-21).
///
/// ```
/// use norte_frontend::timeline::path_label;
/// assert_eq!(path_label("file:///home/ana/fotos"), "/home/ana/fotos");
/// assert_eq!(path_label("not a path"), "not a path");
/// ```
#[must_use]
pub fn path_label(path: &str) -> String {
    norte_proto::VPath::parse(path).map_or_else(|_| path.to_owned(), |v| crate::path_display(&v).0)
}

/// The human actor's class, exactly as the journal writes it.
///
/// It is the `actor_kind` value that [`crate::timeline::Timeline`] compares
/// to know which rows it can undo, and it lives here — and not as a loose
/// literal in every call site — because writing it wrong does not break
/// anything visibly: it just makes the count read zero forever.
pub const ACTOR_HUMANO: &str = "user";

/// One row of the timeline: a mutation, or a whole BATCH.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineRow {
    /// The NEWEST `seq` in the group. It is the cutoff that has to be sent to
    /// keep this row whole ([`Timeline::corte`]).
    pub seq: i64,
    /// When the newest thing in the group happened.
    pub ts_ms: i64,
    /// Who: `"user"`, `"agent"`, `"plugin"`…
    pub actor_kind: String,
    /// Which one, within that class. `None` for the human.
    pub actor_id: Option<String>,
    /// The operation. For a batch, the one from its newest entry.
    pub op: String,
    /// What it acted on. Already paintable; it is masked by whoever built the
    /// row, since that is who knows where those bytes came from.
    pub path: String,
    /// The destination, if the operation has two sides.
    pub path_to: Option<String>,
    /// Whether ALL entries in the group declared a way back. A batch with one
    /// irreversible entry inside is not reversible: it is undone whole or not
    /// at all.
    pub reversible: bool,
    /// Whether the group is ALREADY undone (its compensation is still alive),
    /// or IS a compensation.
    ///
    /// Both cases count the same for the one thing that matters here: undo is
    /// not going to touch them. A compensation is written with the actor of
    /// the HUMAN who ran the undo and with a real reversal, so without this a
    /// timeline would count it as undoable — and after undoing five things it
    /// would promise ten and do zero.
    pub ya_desecho: bool,
    /// [`Self::path`]'s text is painted differently from what the stored
    /// bytes say: the server had to mask something. It is flagged on the row,
    /// like on every decision surface.
    pub hostile: bool,
    /// How many journal entries are underneath this row. `1` except in a
    /// batch.
    pub members: usize,
    /// The batch, if it is one. It is there to paint it differently: a group
    /// does not read the same as a lone mutation.
    pub batch_id: Option<i64>,
}

impl TimelineRow {
    /// Whether the human did this row, i.e. whether `journal.undo_after` is
    /// even going to look at it.
    #[must_use]
    pub fn es_del_humano(&self) -> bool {
        self.actor_kind == ACTOR_HUMANO
    }

    /// Whether undo is actually going to try to bring this row back.
    ///
    /// These are the THREE conditions the core's query applies, not two: from
    /// the human, with a declared way back, and neither already undone nor
    /// itself a compensation. Counting only the first two is what made the
    /// confirmation promise twice what was actually going to happen.
    #[must_use]
    pub fn se_va_a_deshacer(&self) -> bool {
        self.es_del_humano() && self.reversible && !self.ya_desecho
    }
}

/// What a cutoff is going to take with it, counted BEFORE asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Corte {
    /// The HUMAN's entries, reversible, after the cutoff: the ones undo is
    /// going to try to bring back.
    pub a_deshacer: usize,
    /// The human's entries after the cutoff that undo will NOT touch because
    /// they have no way back, because they are already undone, or because
    /// they are themselves another one's compensation.
    ///
    /// These are counted separately and not added to [`Self::a_deshacer`] for
    /// the usual reason: a number that mixed them would promise something
    /// that is not going to happen, and this figure is the one shown right
    /// before asking.
    pub irreversibles: usize,
    /// Entries after the cutoff that are NOT the human's. Undo does not touch
    /// them — they belong to an agent or a plugin, and get undone through
    /// their own path — and that is why they are counted apart instead of
    /// added to the others: a number that mixed the three would promise
    /// something that is not going to happen.
    pub ajenas: usize,
}

impl Corte {
    /// Whether a cutoff here is not going to do anything.
    #[must_use]
    pub fn no_hace_nada(&self) -> bool {
        self.a_deshacer == 0
    }
}

/// The loaded timeline: the rows that have been brought in, newest to
/// oldest, and where the cursor stands.
#[derive(Debug, Clone, Default)]
pub struct Timeline {
    rows: Vec<TimelineRow>,
    cursor: usize,
    next_before_seq: Option<i64>,
    loaded: bool,
}

impl Timeline {
    /// A timeline with the first page already in it.
    #[must_use]
    pub fn new(rows: &[JournalRow], next_before_seq: Option<i64>) -> Self {
        Self {
            rows: group(rows),
            cursor: 0,
            next_before_seq,
            loaded: true,
        }
    }

    /// Whether anyone has gotten around to asking the journal.
    ///
    /// `false` on a freshly-born one — one that inherits a saved layout,
    /// before the loop fills it in — and it exists so as not to say "nothing
    /// has happened yet" about a journal that has not been looked at. On a
    /// history screen, that sentence is the worst possible mistake.
    #[must_use]
    pub fn cargada(&self) -> bool {
        self.loaded
    }

    /// Appends an OLDER page at the end.
    ///
    /// The whole page is grouped together with the last row that was already
    /// there, in case a batch ended up split across two pages: the journal
    /// paginates by entries and knows nothing about batches, so the cutoff
    /// can land inside one. Without this, half of a batch would be painted
    /// as its own group and would offer a cutoff through its middle, which is
    /// exactly what does not exist.
    pub fn extend(&mut self, rows: &[JournalRow], next_before_seq: Option<i64>) {
        let new_rows = group(rows);
        if let (Some(last), Some(first)) = (self.rows.last(), new_rows.first())
            && last.batch_id.is_some()
            && last.batch_id == first.batch_id
        {
            let tail = self.rows.pop().unwrap_or_else(|| unreachable!());
            let mut it = new_rows.into_iter();
            let first = it.next().unwrap_or_else(|| unreachable!());
            self.rows.push(fuse(tail, &first));
            self.rows.extend(it);
        } else {
            self.rows.extend(new_rows);
        }
        self.next_before_seq = next_before_seq;
        self.loaded = true;
    }

    /// The rows, newest to oldest.
    #[must_use]
    pub fn rows(&self) -> &[TimelineRow] {
        &self.rows
    }

    /// Where the cursor is.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Moves the cursor to `i`, clamped.
    pub fn set_cursor(&mut self, i: usize) {
        self.cursor = i.min(self.rows.len().saturating_sub(1));
    }

    /// Goes up one row (towards the newest).
    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Goes down one row (towards the oldest).
    pub fn down(&mut self) {
        self.cursor = (self.cursor + 1).min(self.rows.len().saturating_sub(1));
    }

    /// The row under the cursor.
    #[must_use]
    pub fn selected(&self) -> Option<&TimelineRow> {
        self.rows.get(self.cursor)
    }

    /// What `seq` has to be sent to `journal.undo_after` to go back to the
    /// state at the row under the cursor, keeping it.
    #[must_use]
    pub fn corte(&self) -> Option<i64> {
        self.selected().map(|r| r.seq)
    }

    /// What that cutoff is going to take with it, counting the rows NEWER
    /// than the marked one.
    ///
    /// This counts over what is loaded, and that is enough ONLY if undo does
    /// not go past what is loaded: whatever happened after the list was
    /// painted is not here. That is why whoever asks for the undo also sends
    /// [`Self::techo`] (`upto_seq`, 0.80.0), and the core does not undo
    /// anything newer than that. Whatever is below the cursor, which may not
    /// be loaded, a cutoff here does not touch.
    #[must_use]
    pub fn resumen(&self) -> Corte {
        let mut summary = Corte::default();
        for row in self.rows.iter().take(self.cursor) {
            if !row.es_del_humano() {
                summary.ajenas += row.members;
            } else if row.se_va_a_deshacer() {
                summary.a_deshacer += row.members;
            } else {
                summary.irreversibles += row.members;
            }
        }
        summary
    }

    /// The CEILING of an undo from this list: the newest `seq` that has been
    /// loaded, and therefore the newest thing [`Self::resumen`] could have
    /// counted. It goes as `upto_seq` in `journal.undo_after`: without it,
    /// whatever happened after the list was painted would enter the undo
    /// without having been counted.
    #[must_use]
    pub fn techo(&self) -> Option<i64> {
        self.rows.first().map(|r| r.seq)
    }

    /// The cursor to ask for the next (older) page, or `None` if there is
    /// nothing left behind.
    #[must_use]
    pub fn next_before_seq(&self) -> Option<i64> {
        self.next_before_seq
    }

    /// Whether there is no row at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// How many rows there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }
}

/// Joins the entries of the same batch into one row.
///
/// Entries arrive newest to oldest, and a batch's entries USUALLY are
/// contiguous in `seq` — but need not be: two concurrent batch tasks
/// interleave their entries, and `alloc_batch` in the core allows for that.
/// That is why the group is looked for in EVERYTHING already grouped so far
/// and not only in the previous row: a batch split across two rows would
/// offer two cutoffs inside a unit that is undone as one.
///
/// That this is not also DANGEROUS is guaranteed by the core, not by this:
/// the `undo_after` query leaves out the whole batch when the cutoff falls
/// inside it. This grouping is what keeps the screen from offering a cutoff
/// that the core is going to ignore.
fn group(rows: &[JournalRow]) -> Vec<TimelineRow> {
    let mut out: Vec<TimelineRow> = Vec::with_capacity(rows.len());
    for r in rows {
        let existing = r
            .batch_id
            .and_then(|b| out.iter_mut().find(|row| row.batch_id == Some(b)));
        if let Some(row) = existing {
            row.members += 1;
            // A batch is reversible only if ALL of its entries are: it is
            // undone whole or not touched. And it is enough for one of them
            // to already be undone for undo to skip it.
            row.reversible = row.reversible && r.reversible;
            row.ya_desecho = row.ya_desecho || r.undone || r.undoes_seq.is_some();
            continue;
        }
        out.push(TimelineRow {
            seq: r.seq,
            ts_ms: r.ts_ms,
            actor_kind: r.actor_kind.clone(),
            actor_id: r.actor_id.clone(),
            op: r.op.clone(),
            path: r.path.clone(),
            path_to: r.path_to.clone(),
            reversible: r.reversible,
            // A compensation is a mutation that happened and is shown, but
            // undo does not undo it again: it counts as already undone.
            ya_desecho: r.undone || r.undoes_seq.is_some(),
            hostile: r.hostile,
            members: 1,
            batch_id: r.batch_id,
        });
    }
    out
}

/// Fuses two pieces of the SAME batch split across two pages. The `seq` and
/// the operation are the NEWER piece's, since that is the one that governs
/// the cutoff.
fn fuse(new: TimelineRow, old: &TimelineRow) -> TimelineRow {
    TimelineRow {
        members: new.members + old.members,
        reversible: new.reversible && old.reversible,
        ya_desecho: new.ya_desecho || old.ya_desecho,
        hostile: new.hostile || old.hostile,
        ..new
    }
}

#[cfg(test)]
mod tests {
    use super::{Timeline, group};
    use norte_proto::methods::JournalRow;

    fn row(seq: i64, actor: &str, reversible: bool, batch: Option<i64>) -> JournalRow {
        JournalRow {
            undoes_seq: None,
            undone: false,
            hostile: false,
            seq,
            ts_ms: 1_000 + seq,
            actor_kind: actor.to_owned(),
            actor_id: None,
            op: "copied".to_owned(),
            path: format!("file:///a/{seq}"),
            path_to: None,
            reversible,
            batch_id: batch,
        }
    }

    /// A batch is ONE row: it is undone whole or not touched, so a list that
    /// split it would offer a cutoff that does not exist.
    #[test]
    fn a_batch_is_one_row() {
        let rows = [
            row(9, "user", true, None),
            row(8, "user", true, Some(3)),
            row(7, "user", true, Some(3)),
            row(6, "user", true, Some(3)),
            row(5, "user", true, None),
        ];
        let t = Timeline::new(&rows, None);
        assert_eq!(t.len(), 3, "lone, batch, lone");
        assert_eq!(t.rows()[1].members, 3);
        assert_eq!(t.rows()[1].seq, 8, "the newest in the batch governs");
    }

    /// A batch with one irreversible entry inside is NOT reversible: it is
    /// undone whole or not at all, so promising a way back would be promising
    /// half of one.
    #[test]
    fn a_batch_with_one_irreversible_entry_is_not_reversible() {
        let rows = [
            row(3, "user", true, Some(1)),
            row(2, "user", false, Some(1)),
        ];
        let t = Timeline::new(&rows, None);
        assert_eq!(t.len(), 1);
        assert!(!t.rows()[0].reversible);
    }

    /// The cutoff keeps the marked row WHOLE: it is the newest `seq` in the
    /// group. With the oldest, marking a batch would undo it halfway.
    #[test]
    fn the_cutoff_keeps_the_marked_batch_whole() {
        let rows = [
            row(9, "user", true, None),
            row(8, "user", true, Some(3)),
            row(7, "user", true, Some(3)),
        ];
        let mut t = Timeline::new(&rows, None);
        t.down();
        assert_eq!(t.corte(), Some(8), "the newest in the marked batch");
    }

    /// The count up front separates what is actually going to be undone from
    /// what is going to be SKIPPED and from what is not the human's. A number
    /// that mixed them would promise something undo is not going to do.
    #[test]
    fn the_count_separates_what_actually_gets_undone() {
        let rows = [
            row(10, "user", true, None),
            row(9, "agent", true, None),
            row(8, "user", false, None),
            row(7, "user", true, Some(2)),
            row(6, "user", true, Some(2)),
            row(5, "user", true, None),
        ];
        let mut t = Timeline::new(&rows, None);
        // Cursor on the last one (the oldest): everything above enters.
        t.set_cursor(99);
        let c = t.resumen();
        assert_eq!(
            c.a_deshacer, 3,
            "the lone one above and the two in the batch"
        );
        assert_eq!(c.irreversibles, 1);
        assert_eq!(c.ajenas, 1, "the agent's is not touched by this undo");
    }

    /// **Neither a compensation nor an already-undone entry counts as
    /// undoable.**
    ///
    /// This is the bug the protocol review found: a compensation is written
    /// with the HUMAN actor who ran the undo and with a real reversal, so
    /// looking only at `actor_kind` and `reversible` passes it off as
    /// undoable. Undo five things, reload, mark the same cutoff: the dialog
    /// promised ten and the undo did zero.
    #[test]
    fn neither_a_compensation_nor_something_already_undone_counts() {
        let mut undone = row(4, "user", true, None);
        undone.undone = true;
        let mut compensation = row(5, "user", true, None);
        compensation.undoes_seq = Some(4);

        let rows = [compensation, undone, row(3, "user", true, None)];
        let mut t = Timeline::new(&rows, None);
        t.set_cursor(99);
        let c = t.resumen();

        assert_eq!(c.a_deshacer, 0, "the two above are already settled");
        assert_eq!(
            c.irreversibles, 2,
            "and count as \"not going to be touched\""
        );
        assert!(c.no_hace_nada());
    }

    /// A batch groups together even when its entries are NOT contiguous: two
    /// concurrent batch tasks interleave their `seq`s, and a list that split
    /// it would offer two cutoffs inside a unit that is undone as one.
    #[test]
    fn a_batch_with_interleaved_seqs_is_still_one_row() {
        let rows = [
            row(9, "user", true, Some(1)),
            row(8, "user", true, Some(2)),
            row(7, "user", true, Some(1)),
        ];
        let t = Timeline::new(&rows, None);
        assert_eq!(t.len(), 2, "two batches, not three rows");
        assert_eq!(t.rows()[0].members, 2, "batch 1 joins 9 and 7");
    }

    /// With the cursor on the newest row there is nothing above it, so the
    /// cutoff does nothing — and the screen can say so before asking.
    #[test]
    fn a_cutoff_at_the_newest_entry_does_nothing() {
        let rows = [row(2, "user", true, None), row(1, "user", true, None)];
        let t = Timeline::new(&rows, None);
        assert!(t.resumen().no_hace_nada());
    }

    /// A batch split across two pages gets rejoined: the journal paginates by
    /// entries and knows nothing about batches, so the page cutoff can land
    /// inside one.
    #[test]
    fn a_batch_split_across_pages_gets_rejoined() {
        let first = [row(9, "user", true, None), row(8, "user", true, Some(3))];
        let second = [row(7, "user", true, Some(3)), row(6, "user", true, None)];
        let mut t = Timeline::new(&first, Some(8));
        t.extend(&second, None);

        assert_eq!(t.len(), 3, "lone, whole batch, lone");
        assert_eq!(t.rows()[1].members, 2);
        assert_eq!(t.rows()[1].seq, 8, "still governed by the newest");
    }

    /// And two DIFFERENT batches next to each other do not merge just for
    /// being adjacent.
    #[test]
    fn two_different_batches_do_not_merge() {
        let rows = [row(4, "user", true, Some(2)), row(3, "user", true, Some(1))];
        assert_eq!(group(&rows).len(), 2);
    }
}
