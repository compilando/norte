//! Panel de tasks del TUI (fase 5): snapshots vivos leídos del canal
//! `watch` de cada [`TaskRef`] — el TUI jamás bloquea esperando a una
//! task; el tick copia el último snapshot publicado.

use norte_core::TransferOptions;
use norte_core::backend::{TaskObserver, TaskRef};
use norte_proto::{TaskProgress, TaskState, VPath};

use crate::app::TransferKind;

/// Contexto para reintentar una transferencia tras una colisión.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrySpec {
    /// Copy o Move.
    pub kind: TransferKind,
    /// Origen.
    pub from: VPath,
    /// Destino.
    pub to: VPath,
    /// Opciones del intento que falló.
    pub opts: TransferOptions,
    /// Reinterpretación de nombres del pane ORIGEN, capturada al LANZAR la
    /// operación (#98/M1): el modal de colisión llega async — el usuario
    /// puede haber cambiado de pane o ciclado el encoding entre el submit y
    /// la notificación, y el modal debe pintar el mismo texto por el que se
    /// navegó, no el del pane que tenga el foco al llegar.
    pub name_encoding: Option<norte_encoding::NameEncoding>,
}

/// Una fila del panel.
pub struct TaskRow {
    task: TaskObserver,
    rx: tokio::sync::watch::Receiver<TaskProgress>,
    /// Último snapshot copiado (lo que se pinta).
    pub last: TaskProgress,
    /// Contexto de reintento (None en deletes).
    pub retry: Option<RetrySpec>,
    /// Objetivo de un delete a papelera (ver [`Finished::trash_target`]).
    pub trash_target: Option<VPath>,
    /// Ya se emitió su evento terminal.
    reported: bool,
}

/// Evento: una task alcanzó estado terminal (se emite UNA vez).
#[derive(Debug)]
pub struct Finished {
    /// Estado final.
    pub state: TaskState,
    /// Contexto de reintento de la transferencia, si lo había.
    pub retry: Option<RetrySpec>,
    /// Para un delete a PAPELERA: el objetivo (si falla Unsupported, el
    /// TUI reofrece el diálogo de permanente — ADR 0009).
    pub trash_target: Option<VPath>,
}

/// Filas máximas del panel. Política: al empujar una task nueva caen las
/// terminales YA reportadas más viejas; las VIVAS jamás se tiran (sus
/// handles siguen en marcha), así que con más de `MAX_ROWS` tasks vivas el panel
/// crece — deliberado: cortar sería mentir sobre trabajo en curso.
const MAX_ROWS: usize = 6;

/// Las tasks visibles en el panel.
#[derive(Default)]
pub struct TaskBoard {
    rows: Vec<TaskRow>,
}

impl TaskBoard {
    /// Añade una task recién encolada por ESTE frontend.
    pub fn push(&mut self, task: &TaskRef, retry: Option<RetrySpec>) {
        self.push_full(task, retry, None);
    }

    /// Añade una task de la que este frontend conserva el handle: el tablero
    /// se queda un [`TaskObserver`], no la task (#173). Es lo que permite que
    /// una sincronización APLICÁNDOSE salga en el tablero sin quitarle a su
    /// panel lo único con lo que se puede parar.
    pub fn push_observed(&mut self, task: TaskObserver, retry: Option<RetrySpec>) {
        self.push_observed_full(task, retry, None);
    }

    /// Añade una task FORÁNEA (otro frontend de la misma sesión, fase 3):
    /// sin contexto de reintento (no la lanzamos nosotros) — se ve
    /// progresar en el panel como una más. Duplicados por id se ignoran
    /// (la propia puede llegar también por broadcast).
    pub fn push_foreign(&mut self, task: &TaskRef) {
        self.push_full(task, None, None);
    }

    /// Como [`Self::push`], con objetivo de papelera (deletes Trash).
    pub fn push_full(
        &mut self,
        task: &TaskRef,
        retry: Option<RetrySpec>,
        trash_target: Option<VPath>,
    ) {
        self.push_observed_full(task.observer(), retry, trash_target);
    }

    /// Como [`Self::push_full`], desde un observador ya obtenido.
    pub fn push_observed_full(
        &mut self,
        task: TaskObserver,
        retry: Option<RetrySpec>,
        trash_target: Option<VPath>,
    ) {
        // Duplicados por id se ignoran: la propia puede llegar también por
        // broadcast, y una sincronización se empuja al lanzarla.
        if self.rows.iter().any(|r| r.task.id() == task.id()) {
            return;
        }
        let rx = task.progress();
        let last = rx.borrow().clone();
        self.rows.push(TaskRow {
            task,
            rx,
            last,
            retry,
            trash_target,
            reported: false,
        });
        // Hueco: caen primero las terminales más viejas — solo las YA
        // reportadas (una terminal sin reportar aún debe emitir su Finished).
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

    /// Copia los últimos snapshots y devuelve las tasks que ACABAN de
    /// terminar (el estado terminal siempre se publica — contrato del
    /// `ProgressReporter`).
    pub fn tick(&mut self) -> Vec<Finished> {
        let mut out = Vec::new();
        for row in &mut self.rows {
            row.last = row.rx.borrow().clone();
            if row.last.state.is_terminal() && !row.reported {
                row.reported = true;
                out.push(Finished {
                    state: row.last.state.clone(),
                    retry: row.retry.clone(),
                    trash_target: row.trash_target.clone(),
                });
            }
        }
        out
    }

    /// Cancela la task en marcha más RECIENTE. `false` si no hay ninguna.
    /// Consulta el estado EN VIVO (el snapshot del tick puede tener hasta
    /// 100 ms): "cancelando…" jamás se dice de algo ya terminado.
    pub fn cancel_last_running(&mut self) -> bool {
        for row in self.rows.iter().rev() {
            if !row.rx.borrow().state.is_terminal() {
                row.task.cancel();
                return true;
            }
        }
        false
    }

    /// Las filas visibles (recientes al final).
    #[must_use]
    pub fn rows(&self) -> &[TaskRow] {
        &self.rows
    }

    /// `true` si alguna fila del panel sigue EN VUELO (S2, `[ui]
    /// confirm_quit` modo `auto`): consulta el estado EN VIVO de cada task,
    /// mismo criterio que [`Self::cancel_last_running`] — el snapshot del
    /// tick puede tener hasta 100 ms de retraso, y "nada pendiente" no debe
    /// decirse de algo que en realidad sigue corriendo.
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
    use norte_proto::{TaskId, TaskKind, TaskProgress, TaskState};

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
}
