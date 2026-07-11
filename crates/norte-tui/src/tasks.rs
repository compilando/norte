//! Panel de tasks del TUI (fase 5): snapshots vivos leídos del canal
//! `watch` de cada [`TaskHandle`] — el TUI jamás bloquea esperando a una
//! task; el tick copia el último snapshot publicado.

use norte_core::{TaskHandle, TransferOptions};
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
}

/// Una fila del panel.
pub struct TaskRow {
    handle: TaskHandle,
    rx: tokio::sync::watch::Receiver<TaskProgress>,
    /// Último snapshot copiado (lo que se pinta).
    pub last: TaskProgress,
    /// Contexto de reintento (None en deletes).
    pub retry: Option<RetrySpec>,
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
    /// Añade una task recién encolada.
    pub fn push(&mut self, handle: TaskHandle, retry: Option<RetrySpec>) {
        let rx = handle.progress();
        let last = rx.borrow().clone();
        self.rows.push(TaskRow {
            handle,
            rx,
            last,
            retry,
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
                row.handle.cancel();
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
}
