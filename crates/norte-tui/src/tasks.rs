//! The TUI's task panel (phase 5): live snapshots read from each
//! [`TaskRef`]'s `watch` channel — the TUI never blocks waiting on a task;
//! the tick copies the last published snapshot.

use norte_core::TransferOptions;
use norte_core::backend::{TaskObserver, TaskRef};
use norte_proto::{TaskProgress, TaskState, VPath};

use crate::app::TransferKind;

/// Context to retry a transfer after a collision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrySpec {
    /// Copy or Move.
    pub kind: TransferKind,
    /// Source.
    pub from: VPath,
    /// Destination.
    pub to: VPath,
    /// The failed attempt's options.
    pub opts: TransferOptions,
    /// The SOURCE pane's name reinterpretation, captured when the operation
    /// was LAUNCHED (#98/M1): the collision modal arrives async — the user
    /// may have switched panes or cycled the encoding between the submit and
    /// the notification, and the modal must paint the same text it was
    /// navigated with, not the focused pane's when it arrives.
    pub name_encoding: Option<norte_encoding::NameEncoding>,
}

/// One row of the panel.
pub struct TaskRow {
    task: TaskObserver,
    rx: tokio::sync::watch::Receiver<TaskProgress>,
    /// Last snapshot copied (what gets painted).
    pub last: TaskProgress,
    /// Retry context (None on deletes).
    pub retry: Option<RetrySpec>,
    /// Target of a delete to trash (see [`Finished::trash_target`]).
    pub trash_target: Option<VPath>,
    /// WHAT the task acts on: the last `current` that was ever seen.
    ///
    /// STICKY on purpose. `TaskProgress::current` is "the entry in progress",
    /// so a finished task usually publishes it empty — and a row that says
    /// "copy ✓" without saying what was copied informs of nothing, which is
    /// the complaint that brought this in. Keeping the last one seen, the row
    /// keeps naming its operand after it is done.
    pub operand: Option<VPath>,
    /// This task's rate, estimated from its own snapshots (spec 2026-09-15,
    /// phase 2).
    ///
    /// PER ROW and not per board: two copies at once run at different
    /// speeds, and an average of the two describes neither.
    pub rate: norte_frontend::tasks::Rate,
    /// When it was first seen terminal, on the paint's INJECTED clock
    /// ([`crate::app::App::now_ms`]). `None` while it stays alive.
    terminal_at_ms: Option<i64>,
    /// Its terminal event has already been emitted.
    reported: bool,
}

/// Event: a task reached a terminal state (emitted ONCE).
#[derive(Debug)]
pub struct Finished {
    /// Final state.
    pub state: TaskState,
    /// The transfer's retry context, if there was one.
    pub retry: Option<RetrySpec>,
    /// For a delete to TRASH: the target (if it fails Unsupported, the TUI
    /// re-offers the permanent-delete dialog — ADR 0009).
    pub trash_target: Option<VPath>,
    /// The LAST snapshot, the same one that published the terminal state.
    ///
    /// Some tasks' result IS their progress — `fs.dir_size` counts bytes and
    /// entries, and the total is what the last publication carries (#139) —
    /// so without this it would have to be looked up by `task_id` on a board
    /// that already has it in front of it. And it carries the `kind`, which
    /// is what tells "a mutation finished, reload the panes" apart from "a
    /// count finished, touch nothing".
    pub progress: TaskProgress,
}

/// The panel's maximum rows. Policy: pushing a new task drops the OLDEST
/// already-reported terminal rows; LIVE ones are never dropped (their
/// handles are still running), so with more than `MAX_ROWS` live tasks the
/// panel grows — deliberate: cutting one off would lie about work in
/// progress.
const MAX_ROWS: usize = 6;

/// How long an already-finished task stays in the panel.
///
/// The panel used to keep the whole history until another task pushed it out
/// via [`MAX_ROWS`], so what it showed at a glance was work from half an hour
/// ago. Ten seconds is enough to read the `✓` or the error, and below that
/// the panel goes back to saying what is happening NOW.
///
/// The clock is the one the paint injects, not `SystemTime`: the tests pin
/// `App::render_now_ms` and this adds them no wait.
const TERMINAL_TTL_MS: i64 = 10_000;

/// The tasks visible in the panel.
#[derive(Default)]
pub struct TaskBoard {
    rows: Vec<TaskRow>,
}

impl TaskBoard {
    /// Adds a task THIS frontend just enqueued.
    pub fn push(&mut self, task: &TaskRef, retry: Option<RetrySpec>) {
        self.push_full(task, retry, None);
    }

    /// Adds a task this frontend keeps the handle for: the board keeps a
    /// [`TaskObserver`], not the task (#173). This is what lets a sync that
    /// is APPLYING show up on the board without taking away from its own
    /// panel the only thing it can be stopped with.
    pub fn push_observed(&mut self, task: TaskObserver, retry: Option<RetrySpec>) {
        self.push_observed_full(task, retry, None);
    }

    /// Adds a FOREIGN task (another frontend of the same session, phase 3):
    /// with no retry context (we did not launch it) — it is seen progressing
    /// on the panel like any other. Duplicates by id are ignored (our own can
    /// also arrive via broadcast).
    pub fn push_foreign(&mut self, task: &TaskRef) {
        self.push_full(task, None, None);
    }

    /// Like [`Self::push`], with a trash target (Trash deletes).
    pub fn push_full(
        &mut self,
        task: &TaskRef,
        retry: Option<RetrySpec>,
        trash_target: Option<VPath>,
    ) {
        self.push_observed_full(task.observer(), retry, trash_target);
    }

    /// Like [`Self::push_full`], from an already-obtained observer.
    pub fn push_observed_full(
        &mut self,
        task: TaskObserver,
        retry: Option<RetrySpec>,
        trash_target: Option<VPath>,
    ) {
        // Duplicates by id are ignored: our own can also arrive via
        // broadcast, and a sync is pushed when it is launched.
        if self.rows.iter().any(|r| r.task.id() == task.id()) {
            return;
        }
        let rx = task.progress();
        let last = rx.borrow().clone();
        let operand = last.current.clone();
        self.rows.push(TaskRow {
            task,
            rx,
            last,
            retry,
            trash_target,
            operand,
            rate: norte_frontend::tasks::Rate::default(),
            terminal_at_ms: None,
            reported: false,
        });
        // Room: the oldest terminals fall first — only the ALREADY reported
        // ones (an unreported terminal must still emit its Finished).
        while self.rows.len() > MAX_ROWS {
            let Some(pos) = self
                .rows
                .iter()
                .position(|r| r.last.state.is_terminal() && r.reported)
            else {
                break;
            };
            self.rows.remove(pos);
        }
    }

    /// Copies the latest snapshots and returns the tasks that JUST finished
    /// (the terminal state is always published — the `ProgressReporter`'s
    /// contract).
    pub fn tick(&mut self, now_ms: i64) -> Vec<Finished> {
        let mut out = Vec::new();
        for row in &mut self.rows {
            row.last = row.rx.borrow().clone();
            // The rate is measured with the PAINT's clock, the same one that
            // ages out the rows: a snapshot carries no time, and measuring
            // with another clock would be a number the tests cannot pin.
            row.rate.observe(&row.last, now_ms);
            // The operand is NOT cleared when the snapshot stops carrying it:
            // see `TaskRow::operand`'s note.
            if row.last.current.is_some() {
                row.operand = row.last.current.clone();
            }
            if row.last.state.is_terminal() && !row.reported {
                row.reported = true;
                out.push(Finished {
                    state: row.last.state.clone(),
                    retry: row.retry.clone(),
                    trash_target: row.trash_target.clone(),
                    progress: row.last.clone(),
                });
            }
        }
        out
    }

    /// Stamps the time of rows that just finished and drops the ones that
    /// have been finished for more than ten seconds (`TERMINAL_TTL_MS`,
    /// private).
    ///
    /// Called AFTER [`Self::tick`] and with the same paint clock. Only looks
    /// at already REPORTED rows: an unreported terminal still has to emit its
    /// `Finished`, and dropping it earlier would eat the pane refresh that
    /// mutation asks for.
    pub fn prune_terminal(&mut self, now_ms: i64) {
        for row in &mut self.rows {
            if row.reported && row.last.state.is_terminal() && row.terminal_at_ms.is_none() {
                row.terminal_at_ms = Some(now_ms);
            }
        }
        self.rows.retain(|row| match row.terminal_at_ms {
            // `saturating_sub` and not `-`: the clock is injected by whoever
            // paints and a test can set it backward; overflowing here would
            // drop live rows.
            Some(t) => now_ms.saturating_sub(t) < TERMINAL_TTL_MS,
            None => true,
        });
    }

    /// Cancels the most RECENT running task. `false` if there is none.
    /// Queries the LIVE state (the tick's snapshot can be up to 100 ms
    /// stale): "cancelling…" is never said of something already finished.
    pub fn cancel_last_running(&mut self) -> bool {
        for row in self.rows.iter().rev() {
            if !row.rx.borrow().state.is_terminal() {
                row.task.cancel();
                return true;
            }
        }
        false
    }

    /// The most RECENT running task — the one
    /// [`Self::cancel_last_running`] would cancel — and whether it is
    /// paused, queried LIVE.
    #[must_use]
    pub fn last_running(&self) -> Option<(TaskObserver, bool)> {
        self.rows.iter().rev().find_map(|row| {
            let state = row.rx.borrow().state.clone();
            (!state.is_terminal()).then(|| (row.task.clone(), state == TaskState::Paused))
        })
    }

    /// Row `i`'s task, to act on it without taking the row along.
    #[must_use]
    pub fn task_at(&self, i: usize) -> Option<TaskObserver> {
        self.rows.get(i).map(|r| r.task.clone())
    }

    /// The most recent failed transfer's retry context (ADR 0148): the one
    /// `task.retry` would repeat. `None` if none failed, or if the one that
    /// failed was not a transfer.
    #[must_use]
    pub fn last_failed_retry(&self) -> Option<RetrySpec> {
        self.rows.iter().rev().find_map(|row| {
            matches!(
                row.last.state,
                TaskState::Failed { .. } | TaskState::Cancelled
            )
            .then(|| row.retry.clone())
            .flatten()
        })
    }

    /// Cancels row `i`'s task. `false` if there is no such row, or it already
    /// finished.
    ///
    /// Queries the LIVE state, same as [`Self::cancel_last_running`]: the
    /// tick's snapshot can be up to 100 ms stale, and "cancelling…" is not
    /// said of something already done.
    pub fn cancel_at(&mut self, i: usize) -> bool {
        let Some(row) = self.rows.get(i) else {
            return false;
        };
        if row.rx.borrow().state.is_terminal() {
            return false;
        }
        row.task.cancel();
        true
    }

    /// The visible rows (most recent last).
    #[must_use]
    pub fn rows(&self) -> &[TaskRow] {
        &self.rows
    }

    /// The visible rows' ids, in the order they are painted.
    ///
    /// The processes pane's cursor asks for this: it keeps the IDENTITY of
    /// the chosen task, not its position — the board moves on its own, and a
    /// row leaving from above would make the same position name a different
    /// task.
    #[must_use]
    pub fn task_ids(&self) -> Vec<u64> {
        self.rows.iter().map(|r| r.last.task_id.get()).collect()
    }

    /// `true` if any panel row is still IN FLIGHT (S2, `[ui] confirm_quit`
    /// `auto` mode): queries each task's LIVE state, same criterion as
    /// [`Self::cancel_last_running`] — the tick's snapshot can lag up to
    /// 100 ms, and "nothing pending" must not be said of something that is
    /// actually still running.
    #[must_use]
    pub fn has_active(&self) -> bool {
        self.rows
            .iter()
            .any(|row| !row.rx.borrow().state.is_terminal())
    }
}

#[cfg(test)]
mod has_active_tests {
    use norte_core::backend::TaskRef;
    use norte_proto::{TaskId, TaskKind, TaskProgress, TaskState, VPath};

    use super::TaskBoard;

    fn task_ref(id: u64, state: TaskState) -> TaskRef {
        let progress = TaskProgress {
            task_id: TaskId::new(id),
            kind: TaskKind::Copy,
            state,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (_tx, rx) = tokio::sync::watch::channel(progress);
        TaskRef::synthetic_for_tests(TaskId::new(id), rx)
    }

    #[test]
    fn vacio_no_esta_activo() {
        let board = TaskBoard::default();
        assert!(!board.has_active());
    }

    #[test]
    fn una_fila_en_vuelo_es_activa() {
        let mut board = TaskBoard::default();
        board.push(&task_ref(1, TaskState::Running), None);
        assert!(board.has_active());
    }

    #[test]
    fn todas_las_filas_terminales_no_es_activa() {
        let mut board = TaskBoard::default();
        board.push(&task_ref(1, TaskState::Completed), None);
        board.push(&task_ref(2, TaskState::Cancelled), None);
        assert!(!board.has_active());
    }

    #[test]
    fn mezcla_una_en_vuelo_entre_terminales_es_activa() {
        let mut board = TaskBoard::default();
        board.push(&task_ref(1, TaskState::Completed), None);
        board.push(&task_ref(2, TaskState::Running), None);
        assert!(board.has_active());
    }

    /// ADR 0147: pausar elige la MISMA tarea que cancelar —la viva más
    /// reciente— y dice si ya está pausada, leído en vivo.
    #[test]
    fn pausar_elige_la_viva_mas_reciente_y_sabe_si_esta_pausada() {
        let mut board = TaskBoard::default();
        board.push(&task_ref(1, TaskState::Running), None);
        board.push(&task_ref(2, TaskState::Paused), None);
        board.push(&task_ref(3, TaskState::Completed), None);
        let (task, pausada) = board.last_running().expect("hay una viva");
        assert_eq!(task.id(), norte_proto::TaskId::new(2));
        assert!(pausada);
        let mut vacio = TaskBoard::default();
        vacio.push(&task_ref(9, TaskState::Completed), None);
        assert!(vacio.last_running().is_none());
    }

    /// #173: el tablero se queda un OBSERVADOR, así que quien lanzó la task
    /// conserva el `TaskRef` —y con él el `Esc` del panel de sincronización,
    /// que es el único sitio desde el que se para un plan aprobado—. Antes
    /// esto no se podía escribir: `push` se llevaba la task.
    #[test]
    fn el_tablero_observa_sin_quedarse_la_task() {
        let task = task_ref(4, TaskState::Running);
        let mut board = TaskBoard::default();
        board.push_observed(task.observer(), None);
        assert_eq!(board.rows().len(), 1);
        assert!(board.has_active());
        // La task sigue siendo de quien la lanzó: el tablero no se la llevó.
        assert_eq!(task.id(), norte_proto::TaskId::new(4));
        // Y el tablero puede pararla.
        assert!(board.cancel_last_running());
        // Un segundo empujón con el mismo id no duplica la fila (la propia
        // puede llegar además por broadcast).
        board.push_observed(task.observer(), None);
        assert_eq!(board.rows().len(), 1);
    }

    /// Una terminada se va sola a los diez segundos, y una viva NO se va por
    /// mucho que pase el tiempo: cortar trabajo en curso sería mentir, que es
    /// la misma razón por la que `MAX_ROWS` tampoco las tira.
    #[test]
    fn una_terminada_caduca_y_una_viva_no() {
        let mut board = TaskBoard::default();
        board.push(&task_ref(1, TaskState::Completed), None);
        board.push(&task_ref(2, TaskState::Running), None);
        // Sin `tick` no hay `reported`, así que el sello no se pone: una
        // terminal que todavía debe su `Finished` no se puede tirar.
        board.prune_terminal(0);
        assert_eq!(board.rows().len(), 2);

        assert_eq!(board.tick(0).len(), 1, "la terminal emite su Finished");
        board.prune_terminal(0);
        assert_eq!(board.rows().len(), 2, "recién terminada, todavía se ve");

        board.prune_terminal(9_999);
        assert_eq!(board.rows().len(), 2, "justo por debajo del TTL");

        board.prune_terminal(10_000);
        assert_eq!(board.rows().len(), 1, "la terminada se fue");
        assert!(board.has_active(), "la que quedó es la viva");
    }

    /// El sello es el del PRIMER pase que la ve terminal, no el del último:
    /// si se refrescara en cada pintada, una fila terminada no caducaría
    /// nunca mientras la pantalla siguiera pintándose.
    #[test]
    fn el_sello_no_se_refresca_en_cada_pase() {
        let mut board = TaskBoard::default();
        board.push(&task_ref(1, TaskState::Completed), None);
        board.tick(0);
        for t in 0..10 {
            board.prune_terminal(t * 1_000);
        }
        assert_eq!(board.rows().len(), 1);
        board.prune_terminal(10_000);
        assert!(board.rows().is_empty(), "caducó desde que se vio terminal");
    }

    /// El operando es PEGAJOSO: `current` viene vacío en el snapshot terminal
    /// de casi todas las tasks, y una fila que dice «copy ✓» sin decir sobre
    /// qué no informa de nada.
    #[test]
    fn el_operando_sobrevive_al_snapshot_terminal() {
        let progress = |state: TaskState, current: Option<VPath>| TaskProgress {
            task_id: TaskId::new(7),
            kind: TaskKind::Copy,
            state,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current,
            unreadable: None,
            unvisited: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progress(
            TaskState::Running,
            Some(VPath::parse("mem:///a").unwrap()),
        ));
        let mut board = TaskBoard::default();
        board.push(&TaskRef::synthetic_for_tests(TaskId::new(7), rx), None);
        board.tick(0);
        assert_eq!(
            board.rows()[0].operand.as_ref().map(VPath::to_wire),
            Some("mem:///a".to_owned())
        );

        tx.send(progress(TaskState::Completed, None)).unwrap();
        board.tick(0);
        assert_eq!(
            board.rows()[0].operand.as_ref().map(VPath::to_wire),
            Some("mem:///a".to_owned()),
            "el terminal llegó sin `current` y la fila sigue sabiendo sobre qué actuó"
        );
    }
}
