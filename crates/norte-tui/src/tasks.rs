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
    /// Sobre QUÉ actúa la task: el último `current` que llegó a verse.
    ///
    /// PEGAJOSO a propósito. `TaskProgress::current` es «la entrada en curso»,
    /// así que una task terminada suele publicarlo vacío — y una fila que dice
    /// «copy ✓» sin decir qué se copió no informa de nada, que es la queja que
    /// trajo esto. Guardando el último visto, la fila sigue nombrando su
    /// operando después de acabar.
    pub operand: Option<VPath>,
    /// Cuándo se vio terminal por primera vez, en el reloj INYECTADO del
    /// pintado ([`crate::app::App::now_ms`]). `None` mientras siga viva.
    terminal_at_ms: Option<i64>,
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
    /// El ÚLTIMO snapshot, el mismo que publicó el estado terminal.
    ///
    /// Hay tasks cuyo resultado ES su progreso —`fs.dir_size` cuenta bytes y
    /// entradas, y el total es lo que lleva la última publicación (#139)— así
    /// que sin esto habría que ir a buscarlo por `task_id` a un tablero que ya
    /// lo tiene delante. Y trae el `kind`, que es lo que distingue «terminó una
    /// mutación, recarga los paneles» de «terminó una cuenta, no toques nada».
    pub progress: TaskProgress,
}

/// Filas máximas del panel. Política: al empujar una task nueva caen las
/// terminales YA reportadas más viejas; las VIVAS jamás se tiran (sus
/// handles siguen en marcha), así que con más de `MAX_ROWS` tasks vivas el panel
/// crece — deliberado: cortar sería mentir sobre trabajo en curso.
const MAX_ROWS: usize = 6;

/// Cuánto sigue en el panel una task ya terminada.
///
/// El panel se quedaba con el histórico entero hasta que otra task lo empujaba
/// fuera por [`MAX_ROWS`], así que lo que enseñaba de un vistazo era trabajo de
/// hace media hora. Diez segundos bastan para leer el `✓` o el error, y por
/// debajo el panel vuelve a decir lo que pasa AHORA.
///
/// El reloj es el que inyecta el pintado, no `SystemTime`: los tests fijan
/// `App::render_now_ms` y esto no les añade una espera.
const TERMINAL_TTL_MS: i64 = 10_000;

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
        let operand = last.current.clone();
        self.rows.push(TaskRow {
            task,
            rx,
            last,
            retry,
            trash_target,
            operand,
            terminal_at_ms: None,
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
            // El operando NO se borra cuando el snapshot deja de traerlo: ver
            // la nota de `TaskRow::operand`.
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

    /// Sella la hora de las filas que acaban de terminar y tira las que
    /// llevan terminadas más de diez segundos (`TERMINAL_TTL_MS`, privado).
    ///
    /// Se llama DESPUÉS de [`Self::tick`] y con el mismo reloj del pintado.
    /// Solo mira filas ya REPORTADAS: una terminal sin reportar todavía tiene
    /// que emitir su `Finished`, y tirarla antes se comería el refresco de
    /// panes que esa mutación pide.
    pub fn prune_terminal(&mut self, now_ms: i64) {
        for row in &mut self.rows {
            if row.reported && row.last.state.is_terminal() && row.terminal_at_ms.is_none() {
                row.terminal_at_ms = Some(now_ms);
            }
        }
        self.rows.retain(|row| match row.terminal_at_ms {
            // `saturating_sub` y no `-`: el reloj lo inyecta quien pinta y un
            // test puede fijarlo hacia atrás; desbordar aquí tiraría filas
            // vivas.
            Some(t) => now_ms.saturating_sub(t) < TERMINAL_TTL_MS,
            None => true,
        });
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

    /// Cancela la task de la fila `i`. `false` si no hay fila, o si ya
    /// terminó.
    ///
    /// Consulta el estado EN VIVO, igual que [`Self::cancel_last_running`]: el
    /// snapshot del tick puede tener hasta 100 ms, y «cancelando…» no se dice
    /// de algo que ya acabó.
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

    /// Las filas visibles (recientes al final).
    #[must_use]
    pub fn rows(&self) -> &[TaskRow] {
        &self.rows
    }

    /// Los ids de las filas visibles, en el orden en que se pintan.
    ///
    /// Lo pide el cursor del panel de procesos, que guarda la IDENTIDAD de la
    /// tarea elegida y no su posición: el tablero se mueve solo, y una fila
    /// que se va por encima haría que la misma posición nombrara otra tarea.
    #[must_use]
    pub fn task_ids(&self) -> Vec<u64> {
        self.rows.iter().map(|r| r.last.task_id.get()).collect()
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

        assert_eq!(board.tick().len(), 1, "la terminal emite su Finished");
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
        board.tick();
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
        board.tick();
        assert_eq!(
            board.rows()[0].operand.as_ref().map(VPath::to_wire),
            Some("mem:///a".to_owned())
        );

        tx.send(progress(TaskState::Completed, None)).unwrap();
        board.tick();
        assert_eq!(
            board.rows()[0].operand.as_ref().map(VPath::to_wire),
            Some("mem:///a".to_owned()),
            "el terminal llegó sin `current` y la fila sigue sabiendo sobre qué actuó"
        );
    }
}
