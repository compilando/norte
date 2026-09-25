//! What a tasks board paints about a progress, shared.
//!
//! Here and not in a frontend for the same reason as persistent notices: it
//! is presentation arithmetic over wire data, and two copies end up
//! drifting. The drift already happened once: the TUI fell back to ENTRIES
//! when there were no total bytes and the graphical window did not, so a
//! deletion — which does not count bytes — painted a bar stuck at zero.

/// Whether a task of this class really stops when paused WHILE RUNNING
/// (ADR 0147): only the ones with checkpoints — copy, move and delete. The
/// others would accept the pause and keep going, and a control that does not
/// do what it says is worse than one that is missing: the frontends say no.
///
/// ```
/// use norte_frontend::tasks::pausable;
/// use norte_proto::TaskKind;
/// assert!(pausable(TaskKind::Copy));
/// assert!(!pausable(TaskKind::Search));
/// ```
#[must_use]
pub fn pausable(kind: norte_proto::TaskKind) -> bool {
    matches!(
        kind,
        norte_proto::TaskKind::Copy | norte_proto::TaskKind::Move | norte_proto::TaskKind::Delete
    )
}

/// A task's CLASS: the suffix of its `gui-task-kind-*` catalogue key, and
/// the word the window's bridge carries.
///
/// A `match` and not `format!("{:?}").to_lowercase()`: `Debug` gave
/// `renamebatch` and `dirsize` for keys spelled `rename-batch` and
/// `dir-size`. `TaskKind` is `#[non_exhaustive]`, so a variant from a newer
/// daemon falls into `unknown` — a key that exists — and reads «task».
///
/// ```
/// use norte_frontend::tasks::class;
/// use norte_proto::TaskKind;
/// assert_eq!(class(TaskKind::RenameBatch), "rename-batch");
/// assert_eq!(class(TaskKind::Unknown), "unknown");
/// ```
#[must_use]
pub fn class(kind: norte_proto::TaskKind) -> &'static str {
    use norte_proto::TaskKind as K;
    match kind {
        K::Copy => "copy",
        K::Move => "move",
        K::Delete => "delete",
        K::Undo => "undo",
        K::Search => "search",
        K::Mkdir => "mkdir",
        K::Create => "create",
        K::Index => "index",
        K::Embed => "embed",
        K::RenameBatch => "rename-batch",
        K::Compare => "compare",
        K::DirSize => "dir-size",
        K::DirUsage => "dir-usage",
        K::Pack => "pack",
        K::TestArchive => "test-archive",
        K::Split => "split",
        K::Combine => "combine",
        K::SyncPlan => "sync-plan",
        K::Sync => "sync",
        K::Checksum => "checksum",
        K::SetMode => "set-mode",
        K::Unknown | _ => "unknown",
    }
}

/// What a task board calls a task of this class, in `lang`.
///
/// The ONE place both frontends name a task (#375): the terminal used to
/// print the class itself, so a Spanish board said «copy» next to «Copiar».
///
/// ```
/// use norte_frontend::tasks::kind_label;
/// use norte_i18n::Lang;
/// use norte_proto::TaskKind;
/// assert_eq!(kind_label(Lang::En, TaskKind::Copy), "copy");
/// ```
#[must_use]
pub fn kind_label(lang: norte_i18n::Lang, kind: norte_proto::TaskKind) -> String {
    norte_i18n::t_in(lang, &format!("gui-task-kind-{}", class(kind)))
}

/// A task's percentage: by bytes if known, otherwise by entries.
///
/// `None` = not known yet (the walk has not finished and there are no
/// totals). It is `None` and not a faked `0` on purpose: "it is at 0%" and
/// "how much is left is not known" are two different things, and the wire
/// already distinguishes them — the totals are `Option` for exactly that
/// reason.
#[must_use]
pub fn progress_pct(p: &norte_proto::TaskProgress) -> Option<u8> {
    let pct = |done: u64, total: u64| -> Option<u8> {
        if total == 0 {
            return None;
        }
        u8::try_from((done.min(total).saturating_mul(100)) / total).ok()
    };
    match (p.bytes_total, p.entries_total) {
        (Some(total), _) if total > 0 => pct(p.bytes_done, total),
        (_, Some(total)) if total > 0 => pct(p.entries_done, total),
        _ => None,
    }
}

/// Weight of the new sample in the rate's average.
///
/// A third: with the plain instantaneous rate the number jumps on every
/// snapshot (the wire coalesces at 30 Hz and a small file arrives whole
/// between two), and with a long average the rate is slow to notice the
/// network dropped. A third settles within a few samples and keeps
/// reacting.
const WEIGHT: f64 = 1.0 / 3.0;

/// A task's rate, estimated from its snapshots.
///
/// **Does not come from the wire**: `TaskProgress` says how much is done and
/// not at what speed, so the rate is computed by whoever watches, with the
/// painting clock. Lives here because both surfaces show it and a different
/// average in each would be another silent drift (ADR 0077).
///
/// It is PER TASK: the board is what stores it, and a task that has not
/// published twice has no rate — `None`, which is not "zero".
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rate {
    last: Option<(u64, i64)>,
    bps: Option<f64>,
}

impl Rate {
    /// Records a snapshot and returns the rate in effect, in bytes per
    /// second.
    ///
    /// Discards what it cannot measure: two snapshots with the same clock
    /// (or one that goes backwards, which on an injected clock is a test or
    /// a time adjustment) and a counter that GOES BACK — a resumed task
    /// starts counting again, and carrying the old rate over would lie
    /// about the current network.
    ///
    /// ```
    /// use norte_frontend::tasks::Rate;
    /// # use norte_proto::{TaskId, TaskKind, TaskProgress, TaskState};
    /// # fn p(bytes: u64) -> TaskProgress {
    /// #     TaskProgress { task_id: TaskId::new(1), kind: TaskKind::Copy,
    /// #         state: TaskState::Running, bytes_done: bytes, bytes_total: Some(1000),
    /// #         entries_done: 0, entries_total: None, current: None,
    /// #         unreadable: None, unvisited: None }
    /// # }
    /// let mut r = Rate::default();
    /// assert_eq!(r.observe(&p(0), 0), None, "with a single snapshot there is no rate");
    /// assert_eq!(r.observe(&p(100), 1000), Some(100.0), "100 B in one second");
    /// ```
    pub fn observe(&mut self, p: &norte_proto::TaskProgress, now_ms: i64) -> Option<f64> {
        let done = p.bytes_done;
        // The first snapshot only leaves the baseline: without two there is
        // no speed.
        let (before, when) = self.last.replace((done, now_ms))?;
        let dt = now_ms - when;
        if dt <= 0 || done < before {
            // No time to divide by, or a counter that reset: this snapshot
            // is taken as the new baseline and the estimate is forgotten.
            self.bps = None;
            return None;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "bytes and milliseconds to f64 for an average: the loss is \
                      of digits nobody paints"
        )]
        let sample = (done - before) as f64 * 1000.0 / dt as f64;
        self.bps = Some(match self.bps {
            Some(previous) => previous.mul_add(1.0 - WEIGHT, sample * WEIGHT),
            None => sample,
        });
        self.bps
    }

    /// The rate in effect, in bytes per second. `None` = not known yet.
    #[must_use]
    pub fn bps(&self) -> Option<f64> {
        self.bps
    }

    /// How much is left, in seconds, or `None` if it cannot be said.
    ///
    /// Both things are needed: a total (a copy knows how much it weighs; a
    /// deletion does not) and a rate. Missing either, "a while longer" is
    /// the only certain thing and it is said by staying silent, not with a
    /// made-up number.
    ///
    /// ```
    /// use norte_frontend::tasks::Rate;
    /// # use norte_proto::{TaskId, TaskKind, TaskProgress, TaskState};
    /// # fn p(bytes: u64, total: Option<u64>) -> TaskProgress {
    /// #     TaskProgress { task_id: TaskId::new(1), kind: TaskKind::Copy,
    /// #         state: TaskState::Running, bytes_done: bytes, bytes_total: total,
    /// #         entries_done: 0, entries_total: None, current: None,
    /// #         unreadable: None, unvisited: None }
    /// # }
    /// let mut r = Rate::default();
    /// r.observe(&p(0, Some(1000)), 0);
    /// r.observe(&p(100, Some(1000)), 1000);
    /// assert_eq!(r.eta_secs(&p(100, Some(1000))), Some(9), "900 B at 100 B/s");
    /// assert_eq!(r.eta_secs(&p(100, None)), None, "with no total there is no countdown");
    /// ```
    #[must_use]
    pub fn eta_secs(&self, p: &norte_proto::TaskProgress) -> Option<u64> {
        let total = p.bytes_total?;
        let bps = self.bps?;
        if bps <= 0.0 || total <= p.bytes_done {
            return None;
        }
        #[expect(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "seconds are painted rounded up"
        )]
        let seconds = ((total - p.bytes_done) as f64 / bps).ceil() as u64;
        Some(seconds)
    }
}

/// The rate, written for a row: `1.2 MiB/s`. Empty if not known.
///
/// ```
/// use norte_frontend::tasks::human_rate;
/// assert_eq!(human_rate(Some(1024.0)), "1.0 KiB/s");
/// assert_eq!(human_rate(None), "");
/// ```
#[must_use]
pub fn human_rate(bps: Option<f64>) -> String {
    let Some(bps) = bps else {
        return String::new();
    };
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a negative rate does not exist, and neither does one bigger than u64"
    )]
    let bytes = bps.max(0.0) as u64;
    format!("{}/s", crate::human_bytes(bytes))
}

/// What is left, written for a row: `9s`, `1m 20s`, `2h 05m`. Empty if not
/// known.
///
/// No Fluent on purpose: these are unit symbols, like `human_bytes`'s, and a
/// countdown that changes language every second does not read any better.
///
/// ```
/// use norte_frontend::tasks::human_eta;
/// assert_eq!(human_eta(Some(9)), "9s");
/// assert_eq!(human_eta(Some(80)), "1m 20s");
/// assert_eq!(human_eta(Some(7500)), "2h 05m");
/// assert_eq!(human_eta(None), "");
/// ```
#[must_use]
pub fn human_eta(secs: Option<u64>) -> String {
    let Some(s) = secs else {
        return String::new();
    };
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// `true` if this task class is board WORK.
///
/// The processes panel is for what the reader set running and can stop:
/// copies, moves, deletions, packing, a sync. The *observational* classes —
/// search, compare, measure a directory, checksum, index, plan a sync — are
/// not: each has its own surface (the findings list, the checksum sheet,
/// the plan), and that is where they are seen and cancelled.
///
/// The distinction exists because the panel can OPEN ON ITS OWN (ADR 0115),
/// and opening it for a search would cover half the screen to say what the
/// findings list is already saying. The TUI never put those classes in its
/// board — they live in `work.search`, `work.checksum` — so without this
/// rule written down the two frontends counted different things: it is the
/// kind of drift ADR 0077 asks to kill in the RULE, not in each wiring.
///
/// ```
/// use norte_frontend::tasks::counts_as_work;
/// use norte_proto::TaskKind;
///
/// assert!(counts_as_work(TaskKind::Copy));
/// assert!(!counts_as_work(TaskKind::Search));
/// ```
#[must_use]
pub fn counts_as_work(kind: norte_proto::TaskKind) -> bool {
    !matches!(
        kind,
        norte_proto::TaskKind::Search
            | norte_proto::TaskKind::Compare
            | norte_proto::TaskKind::DirSize
            // Measuring what a directory is made of is the same as measuring
            // how much space it takes: a READ the reader asked for by
            // looking, not work the board should announce.
            | norte_proto::TaskKind::DirUsage
            | norte_proto::TaskKind::Checksum
            | norte_proto::TaskKind::Index
            | norte_proto::TaskKind::Embed
            | norte_proto::TaskKind::SyncPlan
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every class this binary knows has its own name, in both languages.
    /// `DirUsage` — the disk map's measurement — fell into `unknown` and
    /// the strip said «task ✓» when a map finished (#372).
    #[test]
    fn every_known_class_is_named_in_both_languages() {
        use norte_i18n::Lang;
        use norte_proto::TaskKind as K;
        for kind in [
            K::Copy,
            K::Move,
            K::Delete,
            K::Undo,
            K::Search,
            K::Mkdir,
            K::Create,
            K::Index,
            K::Embed,
            K::RenameBatch,
            K::Compare,
            K::DirSize,
            K::Checksum,
            K::DirUsage,
            K::SetMode,
            K::Pack,
            K::TestArchive,
            K::Split,
            K::Combine,
            K::SyncPlan,
            K::Sync,
        ] {
            assert_ne!(class(kind), "unknown", "{kind:?} has no class of its own");
            for lang in [Lang::En, Lang::Es] {
                let label = kind_label(lang, kind);
                assert!(
                    !label.starts_with("gui-task-kind-"),
                    "{kind:?}: no key in {lang:?}"
                );
            }
        }
    }

    /// A Spanish board names its tasks in Spanish (#375). The catalogue
    /// kept ten classes in English in `es.ftl`, and the terminal did not
    /// read it at all.
    #[test]
    fn a_spanish_board_names_its_tasks_in_spanish() {
        use norte_i18n::Lang;
        use norte_proto::TaskKind as K;
        for kind in [
            K::Copy,
            K::Move,
            K::Delete,
            K::Undo,
            K::Search,
            K::Mkdir,
            K::Index,
            K::RenameBatch,
            K::Unknown,
        ] {
            let (en, es) = (kind_label(Lang::En, kind), kind_label(Lang::Es, kind));
            assert!(
                !es.starts_with("gui-task-kind-"),
                "{kind:?}: no key in es.ftl"
            );
            assert_ne!(en, es, "{kind:?} reads «{es}» in Spanish too");
        }
    }

    /// Every wire class decides, BY HAND, whether it is board work.
    ///
    /// [`counts_as_work`]'s `match` is by exclusion, so a NEW class counts
    /// as work without anyone thinking about it — which is the right
    /// default (a new class usually mutates, and a panel that never opens
    /// is a feature that does not exist) — but it cannot be a silent
    /// decision.
    ///
    /// **And this test does not catch it alone.** The `match` below carries
    /// no wildcard — the `other => panic!` forces a choice — but what it
    /// iterates is the ARRAY right next to it, written by hand: a class
    /// missing from the array is not tested, and the `panic!` never fires.
    /// It happened with `DirUsage` (0.75.0), which arrived classified and
    /// unexercised. Adding a class touches BOTH places, and this sentence
    /// exists because the previous one promised a safety net that was not
    /// there.
    #[test]
    fn every_class_decides_by_hand_whether_it_is_work() {
        use norte_proto::TaskKind as K;
        for kind in [
            K::Copy,
            K::Move,
            K::Delete,
            K::Undo,
            K::Search,
            K::Mkdir,
            K::Create,
            K::Index,
            K::Embed,
            K::RenameBatch,
            K::Compare,
            K::DirSize,
            K::DirUsage,
            K::Checksum,
            K::SetMode,
            K::Pack,
            K::TestArchive,
            K::Split,
            K::Combine,
            K::SyncPlan,
            K::Sync,
        ] {
            let expected = match kind {
                // WORK: moves bytes or changes the disk, and stops from the
                // panel.
                K::Copy
                | K::Move
                | K::Delete
                | K::Undo
                | K::Mkdir
                | K::Create
                | K::RenameBatch
                | K::SetMode
                | K::Pack
                | K::TestArchive
                | K::Split
                | K::Combine
                | K::Sync => true,
                // OBSERVATION: each has its own surface — the findings list,
                // the checksum sheet, the plan — and covering it with the
                // panel would say the same thing twice.
                K::Search
                | K::Compare
                | K::DirSize
                | K::DirUsage
                | K::Checksum
                | K::Index
                | K::Embed
                | K::SyncPlan => false,
                // No wildcard ON PURPOSE: see this test's rustdoc.
                other => panic!(
                    "new class on the wire ({other:?}): decide here whether it opens \
                     the processes panel, and write it in `counts_as_work`"
                ),
            };
            assert_eq!(
                counts_as_work(kind),
                expected,
                "{kind:?} switched sides without anyone saying so"
            );
        }
    }

    fn progress(bytes: Option<u64>, entries: Option<u64>) -> norte_proto::TaskProgress {
        norte_proto::TaskProgress {
            task_id: norte_proto::TaskId::new(1),
            kind: norte_proto::TaskKind::Delete,
            state: norte_proto::TaskState::Running,
            bytes_done: 5,
            bytes_total: bytes,
            entries_done: 1,
            entries_total: entries,
            current: None,
            unreadable: None,
            unvisited: None,
        }
    }

    /// With bytes, the byte count wins.
    #[test]
    fn with_total_bytes_the_byte_count_wins() {
        assert_eq!(progress_pct(&progress(Some(10), Some(4))), Some(50));
    }

    /// With no total bytes, entries count: a deletion has no byte weight,
    /// and without this fallback it painted a bar stuck at zero start to
    /// finish.
    #[test]
    fn with_no_bytes_entries_count() {
        assert_eq!(progress_pct(&progress(None, Some(4))), Some(25));
    }

    /// With neither of the two, it is not known, and that is NOT zero.
    #[test]
    fn with_no_totals_it_is_not_known() {
        assert_eq!(progress_pct(&progress(None, None)), None);
        assert_eq!(progress_pct(&progress(Some(0), None)), None);
    }

    /// A `done` bigger than its total does not go past 100%.
    #[test]
    fn it_does_not_go_past_a_hundred() {
        let mut p = progress(Some(2), None);
        p.bytes_done = 9;
        assert_eq!(progress_pct(&p), Some(100));
    }
}
