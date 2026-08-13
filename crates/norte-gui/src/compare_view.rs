//! El panel de diferencias de la GUI (#158, spec 3 fase C1): el run abierto
//! y las reglas PURAS que deciden qué le entra y cómo acaba — testeables sin
//! GPUI, mismo reparto que [`crate::settings_view`] (el estado y `on_*` aquí,
//! el render y la orquestación en `main.rs`).
//!
//! # Lo que este módulo NO reimplementa
//! El modelo (filas, filtros, selección, lado activo) y el estado del run son
//! [`norte_frontend::compare`], los MISMOS que pinta la TUI. Aquí solo se
//! añade lo que es de esta GUI: a qué task pertenece lo que llega, y cómo se
//! traduce un [`SessionEvent`](crate::session::SessionEvent) en una mutación
//! de ese modelo. Reimplementar la cuenta de «¿llegaron todas las filas?» es
//! exactamente lo que el CLI (fase A) y la tool MCP (fase B) hicieron, y las
//! dos se equivocaron: ambas dieron por completa una respuesta a la que le
//! faltaban lotes.

use norte_frontend::compare::CompareState;
use norte_proto::methods::CompareRow;
use norte_proto::{TaskId, TaskState, VPath};

/// El panel de diferencias abierto en la GUI: el run compartido con la TUI,
/// más la task de la que es dueño.
pub struct CompareView {
    /// La task de ESTA comparación.
    ///
    /// No es decoración: es lo único que distingue un lote de esta
    /// comparación de uno de otra que el lector lanzó y canceló. Los eventos
    /// de la anterior siguen en vuelo por el puente GPUI↔tokio cuando la
    /// vista nueva ya está abierta —el hilo de sesión los emite desde otra
    /// task de tokio, sin ninguna barrera con esta—, así que un lote ajeno
    /// llega SIEMPRE que se comparan dos árboles seguidos. La TUI no necesita
    /// el filtro porque su run vive junto al panel en el mismo bucle; aquí no
    /// hay tal cosa.
    pub task_id: TaskId,
    /// El run: filas, filtros, selección, estado terminal y las dos raíces.
    /// Es [`norte_frontend::compare::CompareView`], el mismo tipo que la TUI.
    pub run: norte_frontend::compare::CompareView,
    /// Cuántas filas han LLEGADO por el canal, contadas aquí y no derivadas
    /// del pane: es la mitad de la comparación que decide `Done` contra
    /// `Incomplete`, y tiene que contar lo RECIBIDO aunque el modelo algún
    /// día deje de guardar todo lo que recibe.
    pub rows_received: u64,
}

impl CompareView {
    /// Un panel recién abierto sobre la task que acaba de arrancar.
    ///
    /// `left_pane` es el pane que la LANZÓ (el lado izquierdo, aunque sea el
    /// pane derecho de la pantalla) y viaja congelado desde que se pidió la
    /// comparación, no leído del foco al llegar la respuesta: el foco puede
    /// haberse movido mientras la petición estaba en vuelo.
    pub fn new(
        task_id: TaskId,
        left_root: VPath,
        right_root: VPath,
        left_pane: usize,
        left_encoding: Option<norte_encoding::NameEncoding>,
        right_encoding: Option<norte_encoding::NameEncoding>,
    ) -> Self {
        Self {
            task_id,
            run: norte_frontend::compare::CompareView::new(
                left_root,
                right_root,
                left_pane,
                left_encoding,
                right_encoding,
            ),
            rows_received: 0,
        }
    }

    /// Aplica un lote de filas. Devuelve `false` —y no toca nada— si el lote
    /// es de OTRA comparación (ver [`CompareView::task_id`]).
    pub fn on_rows(&mut self, task_id: TaskId, rows: Vec<CompareRow>) -> bool {
        if task_id != self.task_id {
            return false;
        }
        self.rows_received = self
            .rows_received
            .saturating_add(rows.len().try_into().unwrap_or(u64::MAX));
        self.run.pane.extend(rows);
        true
    }

    /// Se cerró el canal de filas: fija el estado terminal a partir del
    /// snapshot de progreso que el hilo de sesión leyó al cerrarse
    /// (`state`/`entries_done`). Devuelve `false` si el final es de otra
    /// comparación.
    ///
    /// Espejo EXACTO de `norte_tui::drain_compare`, incluidos sus dos casos
    /// que no son el normal:
    ///
    /// * `Cancelled` y `Failed` salen del [`TaskState`], **jamás** de contar
    ///   filas: una comparación cancelada no ha perdido nada, simplemente no
    ///   siguió, y sus filas siguen siendo ciertas.
    /// * un estado **no terminal** es la carrera benigna: el canal de filas
    ///   se cerró antes de que el snapshot terminal se publicara (las dos
    ///   bombas son tasks independientes), así que `entries_done` todavía no
    ///   es definitivo. Se pinta `Done` con lo que hay, SIN pasar por
    ///   [`norte_frontend::compare::CompareView::finish`] — acusar de pérdida
    ///   a esa carrera es el mismo error del CLI y la tool MCP, al revés.
    ///
    /// Solo `Completed` pasa por la cuenta, que es la ÚNICA situación en la
    /// que faltar filas significa que se perdieron.
    pub fn on_done(&mut self, task_id: TaskId, state: &TaskState, entries_done: u64) -> bool {
        if task_id != self.task_id {
            return false;
        }
        match state {
            TaskState::Cancelled => {
                self.run.state = CompareState::Cancelled;
                self.run.rows_expected = entries_done;
            }
            TaskState::Failed { error } => {
                self.run.state = CompareState::Failed;
                self.run.rows_expected = entries_done;
                // La CATEGORÍA localizada, jamás el `Display` inglés: el
                // campo se pinta de forma persistente (fase C1 tarea 3) y
                // varias variantes interpolan datos del peer. El mismo
                // vocabulario que usa la TUI (revisión MAJOR-3).
                self.run.error = Some(norte_frontend::error::error_category(error));
            }
            TaskState::Completed => self.run.finish(entries_done, self.rows_received),
            _ => {
                self.run.state = CompareState::Done;
                self.run.rows_expected = entries_done;
            }
        }
        true
    }
}

/// Abre el panel para la comparación que acaba de ARRANCAR, y devuelve la
/// Task a la que hay que mandarle `task.cancel`: la del panel al que
/// sustituye, si lo había.
///
/// Dos comparaciones a la vez serían dos flujos alimentando un panel (regla
/// 3), así que la anterior no se deja corriendo — mismo criterio que
/// `launch_compare` en la TUI, que cancela el run que reemplaza. Es una
/// función y no un método porque la decisión es sobre el HUECO (`Option`), no
/// sobre una vista: quien elige a quién cancelar es precisamente el caso en
/// el que todavía no hay vista abierta.
#[must_use]
pub fn open(slot: &mut Option<CompareView>, started: Started) -> Option<TaskId> {
    let superseded = slot.take().map(|old| old.task_id);
    *slot = Some(CompareView::new(
        started.task_id,
        started.left_root,
        started.right_root,
        started.left_pane,
        started.left_encoding,
        started.right_encoding,
    ));
    superseded
}

/// Lo que hace falta para abrir un panel: el evento `CompareStarted` con las
/// dos reinterpretaciones ya resueltas. Un struct y no seis argumentos
/// sueltos, que es como se cruzan dos raíces del mismo tipo por error.
pub struct Started {
    /// La Task recién creada.
    pub task_id: TaskId,
    /// Raíz izquierda: la del pane que lanzó.
    pub left_root: VPath,
    /// Raíz derecha.
    pub right_root: VPath,
    /// El pane que lanzó (índice ya acotado a 0|1 por el llamante).
    pub left_pane: usize,
    /// Reinterpretación de nombres (#57) del lado izquierdo.
    pub left_encoding: Option<norte_encoding::NameEncoding>,
    /// La del lado derecho.
    pub right_encoding: Option<norte_encoding::NameEncoding>,
}

/// Encamina un lote de filas y devuelve la Task a cancelar, si alguna.
///
/// Tres casos, y solo uno cancela:
///
/// * es del panel abierto → entra;
/// * **no hay panel** (se cerró bajo el bombeo) → nadie va a leer lo que esa
///   Task siga produciendo, así que se cancela (regla 3, igual que la TUI
///   suelta su run al encontrarse el panel cerrado);
/// * hay panel pero el lote es de una comparación ANTERIOR → se DESCARTA sin
///   cancelar nada. Esa Task ya recibió su `task.cancel` cuando [`open`] la
///   superó, y un segundo cancel sobre un id que puede haber muerto ya no
///   añade nada: los `TaskId` son únicos por PROCESO del daemon, así que si
///   el daemon reinició mientras el lote viajaba, ese id puede pertenecer ya
///   a otra task —una copia en curso— y cancelarla dejaría un
///   `.norte-partial` que nadie pidió (revisión MINOR-3).
#[must_use]
pub fn route_rows(
    slot: &mut Option<CompareView>,
    task_id: TaskId,
    rows: Vec<CompareRow>,
) -> Option<TaskId> {
    match slot.as_mut() {
        Some(view) => {
            view.on_rows(task_id, rows);
            None
        }
        None => Some(task_id),
    }
}

#[cfg(test)]
mod tests {
    use super::{CompareView, Started, open, route_rows};
    use norte_frontend::compare::CompareState;
    use norte_proto::methods::{
        CompareConfidence, CompareCriterion, CompareReason, CompareRow, CompareVerdict,
    };
    use norte_proto::{Error, TaskId, TaskState, VPath};

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire")
    }

    /// Una fila cualquiera, con el MISMO molde que la de la TUI
    /// (`norte-tui/src/main.rs::fila`): lo que se prueba aquí es la
    /// contabilidad del run, no el veredicto.
    fn fila(id: u64) -> CompareRow {
        CompareRow {
            id,
            left: None,
            right: None,
            verdict: CompareVerdict::Error,
            criterion: CompareCriterion::Presence,
            confidence: CompareConfidence::Unknown,
            newer: None,
            reason: Some(CompareReason::Unreadable),
            side: None,
        }
    }

    fn vista_de_prueba() -> CompareView {
        CompareView::new(
            TaskId::new(1),
            vp("file:///a"),
            vp("file:///b"),
            0,
            None,
            None,
        )
    }

    /// Los lotes que llegan se acumulan, y el veredicto terminal sale de
    /// contrastar lo recibido con lo que la task CONTÓ — no de que el canal se
    /// cerrara.
    #[test]
    fn los_lotes_se_acumulan_y_el_final_se_verifica() {
        let mut v = vista_de_prueba();
        assert!(v.on_rows(TaskId::new(1), vec![fila(1), fila(2)]));
        assert_eq!(v.run.pane.len(), 2);
        assert_eq!(v.run.state, CompareState::Running, "un lote no cierra nada");

        assert!(v.on_done(TaskId::new(1), &TaskState::Completed, 2));
        assert_eq!(v.run.state, CompareState::Done);
    }

    /// Un lote perdido se ve, y NO se pinta «hecho» encima. Es el error que
    /// el CLI (fase A) y la tool MCP (fase B) cometieron cada uno por su lado.
    #[test]
    fn un_lote_perdido_no_se_pinta_como_hecho() {
        let mut v = vista_de_prueba();
        v.on_rows(TaskId::new(1), vec![fila(1)]);
        v.on_done(TaskId::new(1), &TaskState::Completed, 2);
        assert_eq!(v.run.state, CompareState::Incomplete);
        assert_eq!(v.run.rows_expected, 2);
    }

    /// **La carrera benigna, y el error simétrico del anterior.** El canal de
    /// filas se cierra ANTES de que el estado terminal se publique (las dos
    /// bombas son tasks independientes), así que el snapshot que se lee sigue
    /// diciendo `Running` y su `entries_done` todavía no es definitivo:
    /// pasarlo por la cuenta acusaría de PÉRDIDA a una carrera que no lo es.
    /// La TUI lleva protegido este caso desde C6 (`drain_compare`), y la GUI
    /// tiene que decir lo mismo.
    #[test]
    fn la_carrera_benigna_se_pinta_hecha_y_no_perdida() {
        let mut v = vista_de_prueba();
        v.on_rows(TaskId::new(1), vec![fila(1)]);
        // Estado NO terminal + un contador que va por delante de las filas.
        v.on_done(TaskId::new(1), &TaskState::Running, 9);
        assert_eq!(
            v.run.state,
            CompareState::Done,
            "una carrera benigna no puede leerse como pérdida"
        );
    }

    /// Cancelar conserva lo que llegó (la comparación no escribe nada: las
    /// filas ya vistas siguen siendo ciertas) y NO pasa por la cuenta —
    /// `Cancelled` sale del `TaskState`, no de comparar contadores.
    #[test]
    fn cancelar_conserva_las_filas_y_no_pasa_por_la_cuenta() {
        let mut v = vista_de_prueba();
        v.on_rows(TaskId::new(1), vec![fila(1), fila(2)]);
        v.on_done(TaskId::new(1), &TaskState::Cancelled, 9);
        assert_eq!(v.run.state, CompareState::Cancelled);
        assert_eq!(v.run.pane.len(), 2, "las filas que llegaron se quedan");
    }

    /// Un fallo guarda su categoría para pintarla de forma PERSISTENTE, y
    /// tampoco pasa por la cuenta.
    #[test]
    fn un_fallo_guarda_su_categoria() {
        let mut v = vista_de_prueba();
        v.on_done(
            TaskId::new(1),
            &TaskState::Failed {
                error: Error::PermissionDenied,
            },
            0,
        );
        assert_eq!(v.run.state, CompareState::Failed);
        assert!(v.run.error.is_some(), "la barra necesita la categoría");
    }

    fn arranque(task_id: u64) -> Started {
        Started {
            task_id: TaskId::new(task_id),
            left_root: vp("file:///a"),
            right_root: vp("file:///b"),
            left_pane: 0,
            left_encoding: None,
            right_encoding: None,
        }
    }

    /// **Regla 3.** Una comparación que sustituye a otra tiene que decir a
    /// quién hay que cancelar: dos flujos alimentando un panel serían dos
    /// comparaciones a la vez, y la vieja seguiría recorriendo dos árboles
    /// que ya nadie mira.
    #[test]
    fn abrir_un_panel_encima_de_otro_cancela_el_anterior() {
        let mut slot = None;
        assert_eq!(
            open(&mut slot, arranque(1)),
            None,
            "el primero no supera a nadie"
        );
        assert_eq!(
            open(&mut slot, arranque(2)),
            Some(TaskId::new(1)),
            "el segundo se lleva por delante al primero, y lo dice"
        );
        assert_eq!(slot.expect("el panel nuevo").task_id, TaskId::new(2));
    }

    /// **Regla 3, la otra mitad.** Un lote que llega con el panel YA cerrado
    /// cancela su Task: sin esto el bombeo seguiría vivo alimentando un panel
    /// que no existe (paridad con `un_lote_con_el_panel_cerrado_cosecha_el_run`
    /// de la TUI).
    #[test]
    fn un_lote_sin_panel_cancela_su_task() {
        let mut slot = None;
        assert_eq!(
            route_rows(&mut slot, TaskId::new(7), vec![fila(1)]),
            Some(TaskId::new(7))
        );
    }

    /// Un lote de la comparación ABIERTA entra y no cancela nada.
    #[test]
    fn un_lote_del_panel_abierto_entra() {
        let mut slot = None;
        assert_eq!(open(&mut slot, arranque(1)), None);
        assert_eq!(route_rows(&mut slot, TaskId::new(1), vec![fila(1)]), None);
        assert_eq!(slot.expect("el panel").run.pane.len(), 1);
    }

    /// Un lote de una comparación ANTERIOR se descarta SIN cancelar: esa Task
    /// ya se canceló al superarla, y los `TaskId` son únicos por proceso del
    /// daemon — si reinició mientras el lote viajaba, un segundo cancel
    /// podría aterrizar sobre una copia en curso (revisión MINOR-3).
    #[test]
    fn un_lote_viejo_se_descarta_sin_cancelar_nada() {
        let mut slot = None;
        assert_eq!(open(&mut slot, arranque(2)), None);
        assert_eq!(
            route_rows(&mut slot, TaskId::new(1), vec![fila(1)]),
            None,
            "ni entra ni manda cancelar a nadie"
        );
        assert!(slot.expect("el panel").run.pane.is_empty());
    }

    /// **El filtro por `task_id` no es decoración.** Es lo único que dice que
    /// un lote pertenece a ESTA comparación y no a una que el usuario lanzó y
    /// canceló: los eventos de la anterior siguen en vuelo por el puente
    /// GPUI↔tokio cuando la nueva vista ya está abierta.
    #[test]
    fn un_lote_de_otra_comparacion_se_descarta() {
        let mut v = vista_de_prueba();
        v.on_rows(TaskId::new(1), vec![fila(1)]);

        assert!(
            !v.on_rows(TaskId::new(2), vec![fila(50), fila(51)]),
            "el lote es de otra comparación"
        );
        assert_eq!(v.run.pane.len(), 1, "no entró ni una fila ajena");

        assert!(
            !v.on_done(TaskId::new(2), &TaskState::Completed, 99),
            "y su final tampoco cierra esta vista"
        );
        assert_eq!(v.run.state, CompareState::Running);
        assert_eq!(v.run.rows_expected, 0, "ni le pega un contador ajeno");
    }
}
