//! The status bar's `tasks` item's light progress bar (ADR 0146): WHEN it is
//! shown and WHAT it says, once for both frontends (ADR 0077).
//!
//! Work arrives in bursts: F5 is marked, pressed, and for a while there is
//! one or several tasks. The bar follows the burst, not each task:
//!
//! - it does not appear until the burst has been running for [`UMBRAL_MS`]:
//!   copying a small file finishes sooner, and painting a bar for 50 ms is a
//!   flicker, not information;
//! - with several tasks there is ONE bar, the total's;
//! - on finishing it leaves a "✓" for [`HECHO_MS`] — also if the burst was
//!   too short to show the bar: otherwise, a fast copy would give no sign of
//!   having happened — or a "✗" for [`FALLO_MS`] if something failed;
//! - the processes panel that opens on its own only waits [`PANEL_MS`]:
//!   whatever finishes before that is counted by this bar, without taking a
//!   third of the screen from the listing away from it.
//!
//! All with the clock the painter passes in, so it is tested with no
//! sleeping.

use std::collections::BTreeSet;

use norte_i18n::{Lang, t_in, ta_in};
use norte_proto::{TaskKind, TaskProgress, TaskState, VPath};

/// How long the bar takes to appear since a burst starts.
pub const UMBRAL_MS: i64 = 400;
/// How long the "✓" stays once it finishes well.
pub const HECHO_MS: i64 = 1_500;
/// How long the "✗" stays once it finishes with some failure: the same as a
/// row finished in the board, which is where you see which one and why.
pub const FALLO_MS: i64 = 10_000;
/// How long the automatic processes panel waits before opening.
pub const PANEL_MS: i64 = 2_000;
/// The bar's cells in the terminal (the window draws it its own way, with
/// the same width so the status bar's layout math is the same).
pub const BAR_CELLS: usize = 10;

/// A board task, as the bar sees it.
#[derive(Debug, Clone, Copy)]
pub struct StripTask<'a> {
    /// The latest progress.
    pub progress: &'a TaskProgress,
    /// What it acts on (the board's sticky operand).
    pub operand: Option<&'a VPath>,
    /// The estimated rate, in bytes per second.
    pub bps: Option<f64>,
}

/// What point of the burst the bar is at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripPhase {
    /// There is work running.
    Running,
    /// All live work is PAUSED (ADR 0147): the bar stays where it was, with
    /// ⏸ and no rate, since there is none right now.
    Paused,
    /// Everything finished well.
    Done,
    /// It finished, and something failed or was cancelled.
    Failed,
}

/// What the bar says right now, already worded for whatever depends on the
/// data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripView {
    /// The phase.
    pub phase: StripPhase,
    /// Tasks running (`Running`), done (`Done`) or failed (`Failed`).
    pub count: usize,
    /// The burst's TOTAL percentage, if known. `None` is not 0%.
    pub percent: Option<u8>,
    /// The class, if every one in the burst is the same.
    pub kind: Option<TaskKind>,
    /// The operand's name, already masked, if the burst is a SINGLE task.
    pub name: Option<String>,
    /// The summed rate, already written, or empty.
    pub rate: String,
    /// What is left, already written, or empty.
    pub eta: String,
}

/// A work burst in progress.
#[derive(Debug, Clone, Default)]
struct Burst {
    since_ms: i64,
    /// The tasks that have belonged to the burst.
    ids: BTreeSet<u64>,
    /// The ones already counted on finishing.
    counted: BTreeSet<u64>,
    done: usize,
    failed: usize,
    /// The common class, while it stays one.
    kind: Option<TaskKind>,
    /// More than one distinct class.
    mixed: bool,
    /// The last name seen, for the "✓ copied x" of a single task.
    name: Option<String>,
}

/// The last burst's outcome, while it is shown.
#[derive(Debug, Clone)]
struct Outcome {
    until_ms: i64,
    view: StripView,
}

/// The bar's state machine. One per frontend (per window session).
#[derive(Debug, Clone, Default)]
pub struct TaskStrip {
    burst: Option<Burst>,
    outcome: Option<Outcome>,
    /// The view of the burst in progress, computed on the last `update`.
    running: Option<StripView>,
    /// The board's tasks on the last `update`: one that shows up already
    /// FINISHED (it started and ended between two ticks) also belongs to a
    /// burst.
    seen: BTreeSet<u64>,
}

fn is_running(p: &TaskProgress) -> bool {
    !p.state.is_terminal()
}

fn succeeded(p: &TaskProgress) -> bool {
    matches!(p.state, TaskState::Completed)
}

/// An operand's visible name: its last segment, masked.
fn visible_name(v: &VPath) -> Option<String> {
    v.file_name().map(|s| crate::display_name(s.as_bytes()).0)
}

impl TaskStrip {
    /// Records this instant's board. Only looks at WORK
    /// ([`crate::tasks::counts_as_work`]): a search has its own list.
    pub fn update<'a>(&mut self, now_ms: i64, tasks: impl IntoIterator<Item = StripTask<'a>>) {
        let tasks: Vec<StripTask<'a>> = tasks
            .into_iter()
            .filter(|t| crate::tasks::counts_as_work(t.progress.kind))
            .collect();
        let any_running = tasks.iter().any(|t| is_running(t.progress));
        let new_ids: BTreeSet<u64> = tasks
            .iter()
            .map(|t| t.progress.task_id.get())
            .filter(|id| !self.seen.contains(id))
            .collect();
        self.seen = tasks.iter().map(|t| t.progress.task_id.get()).collect();
        if (any_running || !new_ids.is_empty()) && self.burst.is_none() {
            // New work: the previous outcome stops being news.
            self.outcome = None;
            self.burst = Some(Burst {
                since_ms: now_ms,
                ..Burst::default()
            });
        }
        let Some(r) = self.burst.as_mut() else {
            self.running = None;
            return;
        };
        for t in &tasks {
            let id = t.progress.task_id.get();
            if (is_running(t.progress) || new_ids.contains(&id)) && r.ids.insert(id) {
                match r.kind {
                    None if !r.mixed => r.kind = Some(t.progress.kind),
                    Some(k) if k != t.progress.kind => {
                        r.kind = None;
                        r.mixed = true;
                    }
                    _ => {}
                }
            }
            if !r.ids.contains(&id) {
                continue;
            }
            if let Some(n) = t.operand.and_then(visible_name) {
                r.name = Some(n);
            }
            if !is_running(t.progress) && r.counted.insert(id) {
                if succeeded(t.progress) {
                    r.done += 1;
                } else {
                    r.failed += 1;
                }
            }
        }
        let in_burst: Vec<&StripTask<'a>> = tasks
            .iter()
            .filter(|t| r.ids.contains(&t.progress.task_id.get()))
            .collect();
        if any_running {
            self.running = Some(running_view(r, &in_burst));
            return;
        }
        // The burst ended: its outcome. With none counted — its tasks left
        // the board without finishing, like a daemon's that is no longer
        // there — there is nothing to say, and a "✓ 0 done" would be a lie.
        if r.done + r.failed == 0 {
            self.burst = None;
            self.running = None;
            return;
        }
        let (phase, count, duration) = if r.failed > 0 {
            (StripPhase::Failed, r.failed, FALLO_MS)
        } else {
            (StripPhase::Done, r.done, HECHO_MS)
        };
        let view = StripView {
            phase,
            count,
            percent: None,
            kind: r.kind,
            name: if r.ids.len() == 1 {
                r.name.clone()
            } else {
                None
            },
            rate: String::new(),
            eta: String::new(),
        };
        self.outcome = Some(Outcome {
            until_ms: now_ms.saturating_add(duration),
            view,
        });
        self.burst = None;
        self.running = None;
    }

    /// What the bar shows at `now_ms`, or nothing.
    #[must_use]
    pub fn view(&self, now_ms: i64) -> Option<StripView> {
        if let Some(r) = &self.burst {
            return (now_ms.saturating_sub(r.since_ms) >= UMBRAL_MS)
                .then(|| self.running.clone())
                .flatten();
        }
        self.outcome
            .as_ref()
            .filter(|f| now_ms < f.until_ms)
            .map(|f| f.view.clone())
    }

    /// Whether the AUTOMATIC processes panel should be open: there is a
    /// burst that has already been running for [`PANEL_MS`].
    #[must_use]
    pub fn wants_panel(&self, now_ms: i64) -> bool {
        self.burst
            .as_ref()
            .is_some_and(|r| now_ms.saturating_sub(r.since_ms) >= PANEL_MS)
    }

    /// The next instant the bar changes WITHOUT new progress arriving: for
    /// whoever does not paint every tick (the window schedules a wake-up).
    #[must_use]
    pub fn next_change_ms(&self, now_ms: i64) -> Option<i64> {
        if let Some(r) = &self.burst {
            return [r.since_ms + UMBRAL_MS, r.since_ms + PANEL_MS]
                .into_iter()
                .find(|&t| t > now_ms);
        }
        self.outcome
            .as_ref()
            .map(|f| f.until_ms)
            .filter(|&t| t > now_ms)
    }
}

/// The view of a burst in progress, over the tasks still on the board
/// (finished ones included: their final total counts in the percentage, or
/// it would go backwards every time one finishes).
fn running_view(burst: &Burst, tasks: &[&StripTask<'_>]) -> StripView {
    let alive: Vec<&&StripTask<'_>> = tasks
        .iter()
        .filter(|task| is_running(task.progress))
        .collect();
    let sum = |field: fn(&TaskProgress) -> Option<(u64, u64)>| -> Option<(u64, u64)> {
        tasks
            .iter()
            .try_fold((0u64, 0u64), |(done_acc, total_acc), task| {
                let (done_part, total_part) = field(task.progress)?;
                Some((
                    done_acc.saturating_add(done_part),
                    total_acc.saturating_add(total_part),
                ))
            })
    };
    let bytes = sum(|p| p.bytes_total.map(|tot| (p.bytes_done.min(tot), tot)));
    let entries = sum(|p| p.entries_total.map(|tot| (p.entries_done.min(tot), tot)));
    let pct = |(done, total): (u64, u64)| -> Option<u8> {
        (total > 0).then(|| u8::try_from(done.saturating_mul(100) / total).unwrap_or(100))
    };
    let percent = bytes
        .filter(|&(_, t)| t > 0)
        .and_then(pct)
        .or_else(|| entries.and_then(pct));
    let rates: Vec<f64> = alive.iter().filter_map(|task| task.bps).collect();
    let bps = (!rates.is_empty()).then(|| rates.iter().sum::<f64>());
    let eta = match (bytes, bps) {
        (Some((done, total)), Some(rate)) if rate > 0.0 && total > done => {
            // Second-level precision: an f64 holding a whole disk's bytes is
            // still exact enough for an estimate.
            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss
            )]
            let s = ((total - done) as f64 / rate).ceil() as u64;
            Some(s)
        }
        _ => None,
    };
    let single = alive.len() == 1 && burst.ids.len() == 1;
    let paused = !alive.is_empty()
        && alive
            .iter()
            .all(|task| task.progress.state == TaskState::Paused);
    StripView {
        phase: if paused {
            StripPhase::Paused
        } else {
            StripPhase::Running
        },
        count: alive.len(),
        percent,
        kind: burst.kind,
        name: if single {
            alive
                .first()
                .and_then(|task| task.operand)
                .and_then(visible_name)
                .or_else(|| burst.name.clone())
        } else {
            None
        },
        // Paused has no rate nor a finish time: the last measured one would
        // lie about now.
        rate: if paused {
            String::new()
        } else {
            crate::tasks::human_rate(bps)
        },
        eta: if paused {
            String::new()
        } else {
            crate::tasks::human_eta(eta)
        },
    }
}

/// One form of the item: its text and whether the bar goes behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    /// The text.
    pub text: String,
    /// Whether the [`BAR_CELLS`]-cell bar goes behind it.
    pub bar: bool,
}

/// The item's forms, from longest to shortest. The status bar uses the
/// longest one that fits, and only when not even the shortest fits does it
/// drop the item (`statusbar::fit`).
#[must_use]
pub fn forms(v: &StripView, lang: Lang) -> Vec<Form> {
    let class = match v.kind {
        Some(TaskKind::Copy) => "copy",
        Some(TaskKind::Move) => "move",
        Some(TaskKind::Delete) => "delete",
        _ => "other",
    };
    let f = |text: String, bar: bool| Form { text, bar };
    match v.phase {
        StripPhase::Running | StripPhase::Paused => {
            let paused = v.phase == StripPhase::Paused;
            let icon = if paused { '⏸' } else { '⟳' };
            let verb = if paused {
                t_in(lang, "strip-paused")
            } else {
                t_in(lang, &format!("strip-running-{class}"))
            };
            let pct = v.percent.map(|p| format!(" {p} %")).unwrap_or_default();
            let head = match &v.name {
                Some(n) if v.count == 1 => format!("{icon} {verb} {n}"),
                _ => format!("{icon} {}", v.count),
            };
            let short = match &v.name {
                Some(n) if v.count == 1 => format!("{icon} {n}"),
                _ => format!("{icon} {}", v.count),
            };
            // What gets appended at the end: the rate with one task, what is
            // left with several (with several the rate is a sum, and what
            // the reader wants to know is when the batch finishes).
            let tail = if v.count == 1 { &v.rate } else { &v.eta };
            let mut out = Vec::new();
            if !tail.is_empty() {
                out.push(f(format!("{head}{pct} · {tail}"), true));
            }
            out.push(f(format!("{head}{pct}"), true));
            out.push(f(format!("{short}{pct}"), true));
            out.push(f(format!("{icon} {}{pct}", v.count), true));
            out.push(f(format!("{icon} {}{pct}", v.count), false));
            out.dedup();
            out
        }
        StripPhase::Done => {
            let full = match &v.name {
                Some(n) => format!("✓ {} {n}", t_in(lang, &format!("strip-done-{class}"))),
                None => format!(
                    "✓ {}",
                    ta_in(lang, "strip-done-many", &[("n", &v.count.to_string())])
                ),
            };
            vec![f(full, false), f("✓".to_owned(), false)]
        }
        StripPhase::Failed => vec![
            f(
                format!(
                    "✗ {}",
                    ta_in(lang, "strip-failed", &[("n", &v.count.to_string())])
                ),
                false,
            ),
            f(format!("✗ {}", v.count), false),
        ],
    }
}

/// The bar in terminal cells: `▕` + [`BAR_CELLS`] cells + `▏`, with block
/// eighths so a slow copy can be seen moving. With no percentage, a pulse
/// travels the bar with the clock: "not known how much" is not 0%.
#[must_use]
pub fn bar_glyphs(percent: Option<u8>, now_ms: i64) -> String {
    const EIGHTHS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
    let mut out = String::with_capacity(BAR_CELLS * 3 + 6);
    out.push('▕');
    if let Some(p) = percent {
        let eighths = usize::from(p.min(100)) * BAR_CELLS * 8 / 100;
        for i in 0..BAR_CELLS {
            let filled = eighths.saturating_sub(i * 8).min(8);
            out.push(if filled == 8 { '█' } else { EIGHTHS[filled] });
        }
    } else {
        // One step every 120 ms, back and forth.
        let step = usize::try_from(now_ms.max(0) / 120).unwrap_or(0) % (2 * (BAR_CELLS - 1));
        let pos = if step < BAR_CELLS {
            step
        } else {
            2 * (BAR_CELLS - 1) - step
        };
        for i in 0..BAR_CELLS {
            out.push(match i.abs_diff(pos) {
                0 => '▓',
                1 => '▒',
                _ => ' ',
            });
        }
    }
    out.push('▏');
    out
}

/// Cells a [`Form`] takes up, with its bar and the space separating it.
#[must_use]
pub fn form_cells(f: &Form) -> usize {
    crate::display::cells(&f.text) + if f.bar { BAR_CELLS + 3 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::TaskId;

    fn p(id: u64, kind: TaskKind, state: TaskState, done: u64, total: Option<u64>) -> TaskProgress {
        TaskProgress {
            task_id: TaskId::new(id),
            kind,
            state,
            bytes_done: done,
            bytes_total: total,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        }
    }

    fn t(p: &TaskProgress) -> StripTask<'_> {
        StripTask {
            progress: p,
            operand: None,
            bps: None,
        }
    }

    const R: TaskState = TaskState::Running;
    const OK: TaskState = TaskState::Completed;

    /// Before the threshold there is no bar; after, there is.
    #[test]
    fn the_bar_waits_for_the_threshold() {
        let mut s = TaskStrip::default();
        let a = p(1, TaskKind::Copy, R, 10, Some(100));
        s.update(0, [t(&a)]);
        assert_eq!(s.view(0), None);
        s.update(UMBRAL_MS - 1, [t(&a)]);
        assert_eq!(s.view(UMBRAL_MS - 1), None, "not yet");
        s.update(UMBRAL_MS, [t(&a)]);
        let v = s.view(UMBRAL_MS).expect("now");
        assert_eq!(
            (v.phase, v.count, v.percent),
            (StripPhase::Running, 1, Some(10))
        );
    }

    /// A copy shorter than the threshold paints no bar, but it DOES leave
    /// the ✓: otherwise it would give no sign of having happened.
    #[test]
    fn a_fast_copy_leaves_the_done_mark() {
        let mut s = TaskStrip::default();
        let a = p(1, TaskKind::Copy, R, 0, Some(100));
        s.update(0, [t(&a)]);
        let a = p(1, TaskKind::Copy, OK, 100, Some(100));
        s.update(100, [t(&a)]);
        let v = s.view(100).expect("the ✓");
        assert_eq!((v.phase, v.count), (StripPhase::Done, 1));
        assert!(s.view(100 + HECHO_MS - 1).is_some(), "it stays");
        assert_eq!(s.view(100 + HECHO_MS), None, "and it goes");
        assert_eq!(s.next_change_ms(100), Some(100 + HECHO_MS));
    }

    /// A failure stays longer than a success.
    #[test]
    fn a_failure_stays_longer() {
        let mut s = TaskStrip::default();
        let a = p(1, TaskKind::Copy, R, 0, Some(100));
        let b = p(2, TaskKind::Copy, R, 0, Some(100));
        s.update(0, [t(&a), t(&b)]);
        let a = p(1, TaskKind::Copy, OK, 100, Some(100));
        let b = p(2, TaskKind::Copy, TaskState::Cancelled, 5, Some(100));
        s.update(50, [t(&a), t(&b)]);
        let v = s.view(50).expect("the ✗");
        assert_eq!((v.phase, v.count), (StripPhase::Failed, 1));
        assert!(s.view(50 + HECHO_MS).is_some(), "longer than a ✓");
        assert_eq!(s.view(50 + FALLO_MS), None);
    }

    /// With several tasks, ONE bar: the byte total's, which does not go
    /// backwards when one finishes.
    #[test]
    fn several_tasks_one_bar_for_the_total() {
        let mut s = TaskStrip::default();
        let a = p(1, TaskKind::Copy, R, 50, Some(100));
        let b = p(2, TaskKind::Copy, R, 0, Some(300));
        s.update(0, [t(&a), t(&b)]);
        s.update(UMBRAL_MS, [t(&a), t(&b)]);
        let v = s.view(UMBRAL_MS).expect("bar");
        assert_eq!((v.count, v.percent), (2, Some(12)));
        let a = p(1, TaskKind::Copy, OK, 100, Some(100));
        s.update(UMBRAL_MS + 10, [t(&a), t(&b)]);
        let v = s.view(UMBRAL_MS + 10).expect("bar");
        assert_eq!(
            (v.count, v.percent),
            (1, Some(25)),
            "the finished one keeps counting"
        );
    }

    /// With no totals there is no percentage: `None`, not a faked 0.
    #[test]
    fn with_no_totals_there_is_no_percentage() {
        let mut s = TaskStrip::default();
        let a = p(1, TaskKind::Delete, R, 0, None);
        s.update(0, [t(&a)]);
        s.update(UMBRAL_MS, [t(&a)]);
        assert_eq!(s.view(UMBRAL_MS).expect("bar").percent, None);
    }

    /// A search is not bar work.
    #[test]
    fn a_search_does_not_count() {
        let mut s = TaskStrip::default();
        let a = p(1, TaskKind::Search, R, 0, None);
        s.update(0, [t(&a)]);
        s.update(UMBRAL_MS * 10, [t(&a)]);
        assert_eq!(s.view(UMBRAL_MS * 10), None);
        assert!(!s.wants_panel(UMBRAL_MS * 10));
    }

    /// The automatic panel waits for the burst to last.
    #[test]
    fn the_panel_waits() {
        let mut s = TaskStrip::default();
        let a = p(1, TaskKind::Copy, R, 0, None);
        s.update(0, [t(&a)]);
        assert!(!s.wants_panel(PANEL_MS - 1));
        assert!(s.wants_panel(PANEL_MS));
        assert_eq!(s.next_change_ms(0), Some(UMBRAL_MS));
        assert_eq!(s.next_change_ms(UMBRAL_MS), Some(PANEL_MS));
        assert_eq!(s.next_change_ms(PANEL_MS), None);
    }

    /// A copy that starts and ends between two ticks is never seen running,
    /// and still leaves its ✓.
    #[test]
    fn the_one_never_seen_running_leaves_the_done_mark() {
        let mut s = TaskStrip::default();
        let a = p(1, TaskKind::Copy, OK, 5, Some(5));
        s.update(0, [t(&a)]);
        assert_eq!(s.view(0).map(|v| v.phase), Some(StripPhase::Done));
        // And it is not counted again on the next tick.
        s.update(HECHO_MS, [t(&a)]);
        assert_eq!(s.view(HECHO_MS), None);
    }

    /// A burst whose tasks disappear without finishing (a daemon handover)
    /// closes quietly: no eternal bar nor "✓ 0".
    #[test]
    fn a_burst_left_with_no_tasks_closes_quietly() {
        let mut s = TaskStrip::default();
        let a = p(1, TaskKind::Copy, R, 0, Some(10));
        s.update(0, [t(&a)]);
        assert!(s.wants_panel(PANEL_MS));
        s.update(PANEL_MS, []);
        assert_eq!(s.view(PANEL_MS), None);
        assert!(!s.wants_panel(PANEL_MS));
        assert_eq!(s.next_change_ms(PANEL_MS), None);
    }

    /// New work covers the previous outcome.
    #[test]
    fn new_work_covers_the_done_mark() {
        let mut s = TaskStrip::default();
        let a = p(1, TaskKind::Copy, OK, 1, Some(1));
        let a0 = p(1, TaskKind::Copy, R, 0, Some(1));
        s.update(0, [t(&a0)]);
        s.update(10, [t(&a)]);
        assert!(s.view(10).is_some());
        let b = p(2, TaskKind::Copy, R, 0, Some(1));
        s.update(20, [t(&a), t(&b)]);
        assert_eq!(
            s.view(20),
            None,
            "the new burst has not reached the threshold yet"
        );
    }

    /// The operand's name arrives masked.
    #[test]
    fn the_name_is_masked() {
        let mut s = TaskStrip::default();
        let hostile = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "rtl_override")
            .expect("corpus fixture");
        let segment = norte_proto::Segment::new(hostile.bytes.clone()).expect("segment");
        let path = VPath::parse("file:///d").expect("vpath").join(segment);
        let a = p(1, TaskKind::Copy, R, 0, Some(10));
        let task = StripTask {
            progress: &a,
            operand: Some(&path),
            bps: None,
        };
        s.update(0, [task]);
        s.update(UMBRAL_MS, [task]);
        let n = s.view(UMBRAL_MS).and_then(|v| v.name).expect("name");
        assert!(!n.chars().any(norte_encoding::is_terminal_hazard), "{n:?}");
    }

    /// The forms go from longest to shortest, and the last one fits in
    /// little space.
    #[test]
    fn the_forms_get_shorter() {
        let v = StripView {
            phase: StripPhase::Running,
            count: 1,
            percent: Some(62),
            kind: Some(TaskKind::Copy),
            name: Some("foto.jpg".to_owned()),
            rate: "48 MiB/s".to_owned(),
            eta: String::new(),
        };
        let fs = forms(&v, Lang::Es);
        let widths: Vec<usize> = fs.iter().map(form_cells).collect();
        assert!(widths.windows(2).all(|w| w[0] >= w[1]), "{widths:?}");
        assert!(fs[0].text.contains("foto.jpg") && fs[0].text.contains("48 MiB/s"));
        assert_eq!(fs.last().map(|f| f.text.as_str()), Some("⟳ 1 62 %"));
    }

    /// ADR 0147: with everything alive paused the bar says so — ⏸, no
    /// rate — and stays at its percentage.
    #[test]
    fn everything_paused_is_said_and_has_no_rate() {
        let mut s = TaskStrip::default();
        let a = p(1, TaskKind::Copy, TaskState::Paused, 40, Some(100));
        let task = StripTask {
            progress: &a,
            operand: None,
            bps: Some(1_000_000.0),
        };
        let a0 = p(1, TaskKind::Copy, R, 40, Some(100));
        s.update(0, [t(&a0)]);
        s.update(UMBRAL_MS, [task]);
        let v = s.view(UMBRAL_MS).expect("bar");
        assert_eq!((v.phase, v.percent), (StripPhase::Paused, Some(40)));
        assert!(v.rate.is_empty() && v.eta.is_empty());
        assert!(forms(&v, Lang::Es)[0].text.starts_with('⏸'));
    }

    /// The bar always takes up the same space, full, empty, or unknown.
    #[test]
    fn the_bar_has_a_fixed_width() {
        for pct in [Some(0), Some(37), Some(100), None] {
            for now in [0, 1_000, 7_777] {
                let b = bar_glyphs(pct, now);
                assert_eq!(crate::display::cells(&b), BAR_CELLS + 2, "{pct:?} {now}");
            }
        }
        assert!(bar_glyphs(Some(100), 0).contains(&"█".repeat(BAR_CELLS)));
    }
}
