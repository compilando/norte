//! El panel de sincronización de la GUI (#161, spec 3 fase C2): el run
//! abierto y las reglas PURAS que deciden qué le entra, qué se descarta y
//! cómo acaba la Task del plan.
//!
//! Gemelo de [`crate::compare_view`], del que copia la forma entera —un
//! módulo con superficie pura, funciones libres dueñas de las decisiones de
//! cancelación, y un guard de `generation` hilado desde el comando hasta el
//! evento— porque C1 ya pagó por descubrirla. El guard va donde se juzga una
//! PETICIÓN (el arranque y la negativa, [`on_start`] y [`failed_banner`]: C1
//! se dejó el de la negativa y pintó un banner de algo ya reemplazado); lo
//! que se juzga por Task —pasos, cierre y final— se correlaciona por
//! `task_id`, y el porqué está en [`route_steps`]. Lo que ESTA fase añade es
//! lo que separa un plan de una comparación:
//!
//! # El plan cierra con su NOTIFICACIÓN, no con el canal
//! Un `sync.plan_done` es lo único que produce `plan_hash`, y sin hash no hay
//! nada que aprobar ni nada que aplicar (ADR 0049). Que el canal de eventos
//! se acabe NO significa que el plan esté completo: significa que la Task
//! terminó, bien o mal. Los dos hechos llegan por eventos distintos
//! ([`SessionEvent::SyncPlanDone`](crate::session::SessionEvent::SyncPlanDone)
//! y [`SessionEvent::SyncPlanEnded`](crate::session::SessionEvent::SyncPlanEnded))
//! y se aplican por métodos distintos, precisamente para que nadie pueda
//! confundirlos: es la distinción que el CLI (fase A) y la tool MCP (fase B)
//! fallaron, cada uno a su manera, dando por completa una respuesta a la que
//! le faltaban lotes.
//!
//! # Lo que este módulo NO reimplementa
//! El modelo (pasos, cuentas, integridad, undo, la segunda pregunta) y el
//! envoltorio del run son [`norte_frontend::sync`], los MISMOS que usa la
//! TUI desde que la tarea 1 los movió: [`norte_frontend::sync::SyncView`],
//! [`norte_frontend::sync::SyncRunState::from_task_state`] y
//! [`norte_frontend::sync::SyncView::can_approve`]. Aquí solo se añade lo
//! que es de esta GUI: a qué Task pertenece lo que llega, qué pane pidió el
//! plan, y cómo se traduce un [`SessionEvent`](crate::session::SessionEvent)
//! en una mutación de ese modelo.
//!
//! En particular **«¿esto se puede aprobar?» no se re-envuelve aquí**: se
//! pregunta a [`norte_frontend::sync::SyncView::can_approve`] y punto, así que
//! en toda la GUI hay UNA función que contesta esa pregunta y es la misma que
//! contesta en la TUI. Un envoltorio propio sería la segunda, y la segunda es
//! la que un día contesta que sí sobre un plan que no cerró.
//!
//! Rendering y aprobación NO están aquí todavía: son las tareas 3 y 4 del
//! plan, deliberadamente separadas porque son la mitad DESTRUCTIVA.

use norte_frontend::sync::{SyncRunState, SyncView as SyncRun};
use norte_proto::methods::{SyncMode, SyncPlanDone, SyncStepsBatch};
use norte_proto::{TaskId, TaskState, VPath};

/// El panel de sincronización abierto en la GUI: el run compartido con la
/// TUI, más lo que solo esta GUI necesita.
pub struct SyncView {
    /// La Task que ALIMENTA este panel: la del plan mientras se planifica.
    ///
    /// No es decoración, y no es redundante con el `task_id` que el modelo
    /// guarda en `SyncState::Planning`: ese se cae en cuanto el plan cierra
    /// (un `SyncState::Ready` no tiene Task), y esto es lo que sigue
    /// habiendo que cancelar cuando el panel se suelta —el daemon puede
    /// seguir recorriendo los dos árboles aunque el plan ya esté cerrado, y
    /// cancelar una Task terminada es un no-op—.
    ///
    /// También es el filtro de [`SyncView::on_plan_ended`]: cuando la tarea 4
    /// lance `sync.apply`, el final del canal del PLAN seguirá en vuelo, y
    /// sin este contraste pintaría «hecho» sobre una aplicación en curso.
    pub task_id: TaskId,
    /// El pane que pidió el plan, o sea el lado ORIGEN. Viaja congelado
    /// desde la petición (mismo motivo que
    /// [`crate::compare_view::CompareView`]): el foco puede haberse movido
    /// mientras el RPC iba y venía, y el aviso se retira donde se puso.
    pub source_pane: usize,
    /// El run: estado del plan, modo, las dos raíces, las dos
    /// reinterpretaciones, la segunda pregunta y el desenlace. Es
    /// [`norte_frontend::sync::SyncView`], el mismo tipo que la TUI.
    pub run: SyncRun,
}

impl SyncView {
    /// Un panel recién abierto sobre la Task que acaba de arrancar.
    #[must_use]
    pub fn new(started: Started) -> Self {
        Self {
            task_id: started.task_id,
            source_pane: started.source_pane & 1,
            run: SyncRun::new(
                started.task_id,
                started.mode,
                started.source_root,
                started.dest_root,
                started.encodings.source,
                started.encodings.dest,
            ),
        }
    }

    /// Aplica un lote de pasos. Devuelve `false` —y no toca nada— si el lote
    /// NO es de este plan.
    ///
    /// Quién decide eso es el modelo compartido
    /// ([`norte_frontend::sync::SyncState::on_steps`], que compara el
    /// `task_id` del propio lote), y no una segunda comparación escrita
    /// aquí: la TUI ya obedece esa regla, y dos reglas para la misma
    /// pregunta es como una de las dos acaba aceptando los pasos de otro
    /// plan.
    pub fn on_steps(&mut self, batch: SyncStepsBatch) -> bool {
        self.run.state.on_steps(batch)
    }

    /// Aplica el `sync.plan_done` que CIERRA el plan: es lo que le da su
    /// `plan_hash` y lo único que puede hacerlo aprobable. Devuelve `false`
    /// si el cierre era de otro plan (o si éste ya estaba cerrado).
    pub fn on_plan_done(&mut self, done: SyncPlanDone) -> bool {
        self.run.state.on_plan_done(done)
    }

    /// Se cerró el canal de eventos del plan: fija el desenlace del run a
    /// partir del snapshot de progreso que el hilo de sesión leyó en ese
    /// instante, y devuelve la CATEGORÍA localizada del error si falló (para
    /// el banner del pane que lanzó; el saneado es del banner).
    ///
    /// **Esto no cierra el plan**, y es la mitad importante: un canal que se
    /// acaba no es un plan completo. Sin `sync.plan_done` el modelo sigue en
    /// `Planning`, `can_approve()` sigue diciendo que no, y eso es la verdad
    /// —no hay `plan_hash`—.
    ///
    /// Devuelve `None` sin tocar nada si el final es de OTRA Task, y ese
    /// contraste es lo ÚNICO que protege este camino: el evento no lleva
    /// generación (ver [`route_steps`]), así que el final del plan al que un
    /// panel nuevo sustituyó llega igual, y sin esto pintaría su desenlace
    /// encima. Lo mismo valdrá cuando la tarea 4 lance `sync.apply`: el final
    /// del canal del PLAN seguirá en vuelo mientras la aplicación corre, y
    /// apagaría su `Running` con un «hecho» que habla de otra cosa.
    ///
    /// El mapeo `TaskState` → [`SyncRunState`] no se decide aquí: es
    /// [`SyncRunState::from_task_state`], el MISMO que llama la TUI en
    /// `drain_sync_plan` (#161). Lo único que se queda de este lado es la
    /// localización del error, que es la mitad que de verdad difiere entre
    /// frontends.
    ///
    /// Nótese que ese mapeo manda `_ => Done`, y `state` puede llegar NO
    /// terminal (la bomba de eventos y la de progreso son tasks distintas,
    /// ver `session::pump_sync_plan`): un plan cuyo canal se cierra un
    /// instante antes de que el watch publique su `Cancelled` se pinta como
    /// «hecho». Es la carrera que la TUI también tiene en este mismo camino
    /// —su `harvest_sync_apply` solo la corrige para la APLICACIÓN—, así que
    /// no es una regresión de esta GUI; queda anotado aquí porque éste es el
    /// sitio donde se heredó (revisión rust MINOR-6).
    pub fn on_plan_ended(&mut self, task_id: TaskId, state: &TaskState) -> Option<String> {
        if task_id != self.task_id {
            return None;
        }
        self.run.run = SyncRunState::from_task_state(state);
        let TaskState::Failed { error } = state else {
            return None;
        };
        let categoria = norte_frontend::error::error_category(error);
        self.run.error = Some(categoria.clone());
        Some(categoria)
    }
}

/// Lo que hace falta para abrir un panel: el evento `SyncPlanStarted` con las
/// dos reinterpretaciones ya resueltas. Un struct y no siete argumentos
/// sueltos, que es como se cruzan dos raíces del mismo tipo por error — y
/// aquí cruzarlas invertiría el SENTIDO de la sincronización, que es la mitad
/// de lo que hay que aprobar.
pub struct Started {
    /// La Task del plan recién creado.
    pub task_id: TaskId,
    /// El pane que la lanzó (el lado ORIGEN).
    pub source_pane: usize,
    /// El modo pedido: un `Mirror` borra y un `Update` no.
    pub mode: SyncMode,
    /// Raíz ORIGEN, congelada en la petición.
    pub source_root: VPath,
    /// Raíz DESTINO.
    pub dest_root: VPath,
    /// Las dos reinterpretaciones de nombres (#57), congeladas con las
    /// raíces y NOMBRADAS: ver [`SyncEncodings`].
    pub encodings: SyncEncodings,
}

/// Las reinterpretaciones de nombres (#57) de los dos lados de un plan.
///
/// Una struct con dos campos NOMBRADOS y no una tupla `(Option<_>,
/// Option<_>)`, por lo mismo que [`Started`] no son siete argumentos sueltos:
/// los dos valores son del mismo tipo, así que trasponerlos compila — y
/// trasponerlos ES el #152, `dest_rel` decodificado con el codepage del
/// ORIGEN, o sea nombrando otros bytes que el fichero sobre el que cae la
/// escritura. Aquí el compilador no ayuda; el nombre sí (revisión rust
/// MINOR-2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncEncodings {
    /// La del pane ORIGEN.
    pub source: Option<norte_encoding::NameEncoding>,
    /// La del pane DESTINO, que puede ser otra: los dos panes son dos
    /// ubicaciones y pueden llevar overrides distintos.
    pub dest: Option<norte_encoding::NameEncoding>,
}

/// Qué Task hay que cancelar cuando llega un `SyncPlanStarted`.
///
/// La decisión ENTERA de ese evento, en un valor: quién se queda el hueco y a
/// quién se le manda `task.cancel`. Existe para que la parte que se puede
/// equivocar se pueda afirmar sin ventana ni daemon (revisión rust MAJOR-3:
/// la regla 3 pide un test de cancelación limpia por cada Task nueva, y el
/// `main.rs` que manda el comando no es testeable) — allí solo queda
/// ejecutarla.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum Start {
    /// El arranque llega VENCIDO: no se abre panel y su Task se cancela —
    /// planificar recorre los dos árboles enteros, y nadie va a mirar el
    /// resultado.
    Superseded(TaskId),
    /// Se abrió el panel. Si había otro, ésta es su Task, que también se
    /// cancela: dos planes a la vez serían dos flujos alimentando un diálogo
    /// cuyo `plan_hash` es lo que se aprueba (regla 3), mismo criterio que
    /// `launch_sync_plan` en la TUI.
    Opened(Option<TaskId>),
}

/// Decide qué hacer con un `SyncPlanStarted` y lo aplica sobre el hueco.
///
/// El guard de generación vive AQUÍ y no en los eventos de pasos: lo que la
/// generación cuenta son PETICIONES, y ésta es la única que se juzga como
/// petición (junto con [`failed_banner`]). Ver [`route_steps`] para por qué
/// los demás eventos se correlacionan por Task.
pub fn on_start(
    slot: &mut Option<SyncView>,
    current_gen: u64,
    generation: u64,
    started: Started,
) -> Start {
    if !crate::generation_is_current(current_gen, generation) {
        return Start::Superseded(started.task_id);
    }
    Start::Opened(open(slot, started))
}

/// Abre el panel para el plan que acaba de ARRANCAR, y devuelve la Task a la
/// que hay que mandarle `task.cancel`: la del panel al que sustituye, si lo
/// había.
///
/// Es una función y no un método por lo mismo que en
/// [`crate::compare_view::open`]: la decisión es sobre el HUECO, y quien
/// elige a quién cancelar es precisamente el caso en el que todavía no hay
/// vista. El llamante de esta GUI es [`on_start`], que le pone delante el
/// guard de generación.
#[must_use]
pub fn open(slot: &mut Option<SyncView>, started: Started) -> Option<TaskId> {
    let superseded = slot.take().map(|old| old.task_id);
    *slot = Some(SyncView::new(started));
    superseded
}

/// Encamina un lote de pasos: entra si es del plan abierto, y se descarta si
/// no.
///
/// **Nunca cancela nada**, y es la misma regla —y el mismo razonamiento— que
/// [`crate::compare_view::route_rows`]: los `TaskId` son únicos por PROCESO
/// del daemon, así que tras un reinicio ese id puede pertenecer ya a otra
/// Task, y cancelarla desde aquí pararía algo que nadie pidió parar. Y no
/// hace falta: cancelar es de quien SUELTA el panel, y hoy los tres caminos
/// que lo sueltan ya lo hacen — [`on_start`] (cancela a la que sustituye, y
/// cancela también el arranque vencido que ni siquiera llega a abrir) y
/// `NorteGui::close_sync`. **Quien añada un cuarto tiene que cancelar allí**,
/// no aquí.
///
/// Un lote descartado con el panel abierto queda en el log, igual que en la
/// TUI: o es de otro plan, o llega DESPUÉS del cierre —y eso segundo es una
/// violación del protocolo, que esconderla no arregla.
///
/// # Y NO se filtra por generación
/// El llamante tampoco lo hace, y es lo que arregla la revisión rust
/// BLOCKER-1: `sync_gen` cuenta PETICIONES, y una petición posterior que el
/// daemon RECHAZA (raíces solapadas, sin spool, un daemon N-1) la adelanta
/// sin llegar a abrir panel. Con un guard de generación aquí, el panel que
/// seguía abierto —el anterior, perfectamente vivo— dejaba de recibir pasos,
/// no podía cerrar su plan (así que jamás sería aprobable, con el
/// `plan_hash` retenido en el spool del daemon) y se quedaba con una Task
/// que ya nadie cancelaba. La correlación de un lote es su Task, no la
/// generación de una petición ajena — que es justo lo que hace `CompareRows`
/// en esta misma GUI.
pub fn route_steps(slot: &mut Option<SyncView>, batch: SyncStepsBatch) {
    if let Some(view) = slot.as_mut()
        && !view.on_steps(batch)
    {
        tracing::warn!("lote de sync.steps descartado: no es de este plan");
    }
}

/// Encamina el `sync.plan_done`, con la misma regla que [`route_steps`]: el
/// modelo decide si es suyo, y de aquí no sale ninguna cancelación.
pub fn route_plan_done(slot: &mut Option<SyncView>, done: SyncPlanDone) {
    if let Some(view) = slot.as_mut()
        && !view.on_plan_done(done)
    {
        tracing::warn!("sync.plan_done descartado: no es de este plan");
    }
}

/// La frase de un `sync.plan` RECHAZADO antes de existir Task alguna, o
/// `None` si esa petición ya está SUPERADA.
///
/// El `None` no es «no hay nada que decir»: es lo que el llamante escribe en
/// `errors[pane]`, y escribir `None` retira el «planificando…» que esa misma
/// petición dejó puesto. Nadie más lo va a retirar — la petición que la
/// superó limpia el SUYO, en el pane que ella lanzó, que no tiene por qué
/// ser éste.
///
/// El guard va en la NEGATIVA y no solo en el arranque, que es exactamente lo
/// que C1 se dejó: su `CompareFailed` no llevaba `generation`, así que la
/// negativa de una petición ya reemplazada pintaba un banner describiendo
/// algo que el lector había sustituido. Cada `SyncPlan` es su propio
/// `tokio::spawn` y dos teclas seguidas pueden contestar en orden INVERSO.
///
/// La categoría va por `banner_safe`, como todo lo que entra en un banner.
#[must_use]
pub fn failed_banner(
    current_gen: u64,
    generation: u64,
    error: &norte_proto::Error,
) -> Option<String> {
    if !crate::generation_is_current(current_gen, generation) {
        return None;
    }
    Some(failure_banner(&norte_frontend::error::error_category(
        error,
    )))
}

/// La MISMA frase para el fallo que llega ya con Task —el canal del plan se
/// cerró con un `Failed`, ver [`SyncView::on_plan_ended`]—, a partir de la
/// categoría que aquél devuelve.
///
/// Una sola función y no dos redacciones (revisión rust MINOR-1): son el
/// mismo hecho contado en dos momentos, y la segunda copia vivía en el
/// `main.rs` de catorce mil líneas, donde ningún test la mira.
#[must_use]
pub fn failure_banner(category: &str) -> String {
    norte_i18n::ta(
        "sync-status-failed",
        &[("error", crate::banner_safe(category).as_str())],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::methods::{
        CompareConfidence, CompareCriterion, DestTrash, PlanHash, RelPath, StepReversal,
        SyncCounts, SyncStep, SyncStepKind,
    };

    fn task() -> TaskId {
        TaskId::new(7)
    }

    fn otra_task() -> TaskId {
        TaskId::new(8)
    }

    fn origen() -> VPath {
        VPath::parse("file:///origen").expect("vpath")
    }

    fn destino() -> VPath {
        VPath::parse("file:///destino").expect("vpath")
    }

    /// Un `Copy` con la forma que el transductor le da contra un destino con
    /// papelera: `shape_is_consistent`, para que la integridad del plan no
    /// salga `Malformed` por culpa del fixture.
    fn paso(id: u64) -> SyncStep {
        SyncStep {
            id,
            kind: SyncStepKind::Copy,
            rel: RelPath::parse_wire("sub/a.txt").expect("rel"),
            dest_rel: None,
            size: Some(10),
            criterion: CompareCriterion::Presence,
            confidence: CompareConfidence::Certain,
            reversal: Some(StepReversal::Delete),
            reason: None,
        }
    }

    fn pasos(ids: &[u64]) -> Vec<SyncStep> {
        ids.iter().copied().map(paso).collect()
    }

    fn lote(task_id: TaskId, steps: Vec<SyncStep>) -> SyncStepsBatch {
        SyncStepsBatch { task_id, steps }
    }

    /// El cierre que el daemon manda para ESOS pasos: las cuentas se suman
    /// con la misma función que usó él, así que el plan sale `Complete`.
    fn cierre(task_id: TaskId, steps: &[SyncStep]) -> SyncPlanDone {
        let mut counts = SyncCounts::default();
        for s in steps {
            counts.add(s);
        }
        SyncPlanDone {
            task_id,
            plan_hash: PlanHash::parse(&"a".repeat(64)).expect("hex"),
            counts,
            blockers: vec![],
            blockers_total: 0,
            executable: true,
            dest_trash: DestTrash::Restorable,
        }
    }

    fn arranque(task_id: TaskId, source_pane: usize) -> Started {
        Started {
            task_id,
            source_pane,
            mode: SyncMode::Update,
            source_root: origen(),
            dest_root: destino(),
            encodings: SyncEncodings::default(),
        }
    }

    fn vista_de_prueba() -> SyncView {
        SyncView::new(arranque(task(), 0))
    }

    /// Los lotes de pasos se acumulan y el plan NO se cierra hasta que llega
    /// `sync.plan_done`: sin él no hay `plan_hash`, y sin hash no se aplica
    /// nada. Que el canal se acabe no es que el plan esté completo.
    #[test]
    fn el_plan_se_cierra_con_su_notificacion_y_no_con_el_canal() {
        let mut v = vista_de_prueba();
        let steps = pasos(&[1, 2]);
        assert!(v.on_steps(lote(task(), steps.clone())));
        assert_eq!(v.run.steps().len(), 2);
        assert!(!v.run.can_approve(), "todavía llegando");

        // El canal se acaba con la Task COMPLETADA, que es el caso feliz —y
        // aun así no hay plan: `sync.plan_done` no ha llegado.
        assert!(v.on_plan_ended(task(), &TaskState::Completed).is_none());
        assert!(
            !v.run.can_approve(),
            "el canal se acabó, pero sin sync.plan_done no hay plan_hash que aprobar"
        );

        assert!(v.on_plan_done(cierre(task(), &steps)));
        assert!(
            v.run.can_approve(),
            "cerrado, íntegro y con pasos: aprobable"
        );
    }

    /// Un lote de OTRO plan no entra: el `task_id` es lo único que lo dice.
    #[test]
    fn un_lote_de_otro_plan_se_descarta() {
        let mut v = vista_de_prueba();
        let antes = v.run.steps().len();
        assert!(!v.on_steps(lote(otra_task(), pasos(&[9]))));
        assert_eq!(v.run.steps().len(), antes);
    }

    /// Y su CIERRE tampoco: un `sync.plan_done` de otro plan haría aprobable
    /// —con el `plan_hash` de otro— un diálogo que enseña estos pasos.
    #[test]
    fn un_cierre_de_otro_plan_no_cierra_este() {
        let mut v = vista_de_prueba();
        let steps = pasos(&[1]);
        assert!(v.on_steps(lote(task(), steps.clone())));
        assert!(!v.on_plan_done(cierre(otra_task(), &steps)));
        assert!(!v.run.can_approve(), "sigue sin cerrar");
    }

    /// El mismo lote, encaminado por [`route_steps`] contra el hueco: entra
    /// si el panel es de ese plan, y con el hueco VACÍO no pasa nada (y sobre
    /// todo, no se cancela nada — quien suelta el panel ya canceló).
    #[test]
    fn encaminar_sin_panel_no_hace_nada() {
        let mut hueco: Option<SyncView> = None;
        route_steps(&mut hueco, lote(task(), pasos(&[1])));
        route_plan_done(&mut hueco, cierre(task(), &pasos(&[1])));
        assert!(hueco.is_none());

        let mut hueco = Some(vista_de_prueba());
        route_steps(&mut hueco, lote(task(), pasos(&[1, 2])));
        assert_eq!(hueco.expect("abierto").run.steps().len(), 2);
    }

    /// El arranque de un plan trae la decisión de cancelación entera, y se
    /// afirma sin ventana ni daemon (regla 3): abrir sobre un panel vivo
    /// devuelve la Task del anterior —dos planes a la vez serían dos flujos
    /// alimentando un `plan_hash`—, y el llamante la cancela.
    #[test]
    fn abrir_devuelve_la_task_a_la_que_sustituye() {
        let mut hueco: Option<SyncView> = None;
        assert_eq!(
            on_start(&mut hueco, 1, 1, arranque(task(), 0)),
            Start::Opened(None),
            "el primero no sustituye a nadie"
        );
        assert_eq!(
            on_start(&mut hueco, 1, 1, arranque(otra_task(), 1)),
            Start::Opened(Some(task())),
            "el segundo devuelve la Task del primero para cancelarla"
        );
        let v = hueco.expect("abierto");
        assert_eq!(v.task_id, otra_task());
        assert_eq!(v.source_pane, 1);
    }

    /// Un arranque VENCIDO no abre panel —y su Task se cancela, que
    /// planificar recorre los dos árboles enteros—. El panel que ya estaba
    /// abierto se queda **intacto**: es de otra petición, sigue vivo, y
    /// tirarlo aquí sería matar al que sí tiene lector.
    #[test]
    fn un_arranque_vencido_se_cancela_y_no_toca_el_panel_abierto() {
        let mut hueco: Option<SyncView> = None;
        assert_eq!(
            on_start(&mut hueco, 2, 2, arranque(task(), 0)),
            Start::Opened(None)
        );
        assert_eq!(
            on_start(&mut hueco, 2, 1, arranque(otra_task(), 1)),
            Start::Superseded(otra_task()),
            "la generación 1 ya está superada por la 2"
        );
        let v = hueco.expect("el panel vigente sigue abierto");
        assert_eq!(v.task_id, task());
    }

    /// **La regresión que la revisión (BLOCKER-1) destapó**: una petición
    /// posterior puede adelantar `sync_gen` sin llegar a abrir panel —el
    /// daemon la rechaza: raíces solapadas, sin spool, un daemon N-1—. El
    /// panel anterior sigue vivo y sus pasos, su cierre y su final tienen que
    /// seguir entrando, porque su Task no se enteró de nada. Por eso la
    /// correlación de esos tres es el `task_id` y no la generación.
    #[test]
    fn una_peticion_rechazada_despues_no_congela_al_panel_vivo() {
        let mut hueco: Option<SyncView> = None;
        assert_eq!(
            on_start(&mut hueco, 1, 1, arranque(task(), 0)),
            Start::Opened(None)
        );
        // Segunda tecla: `sync_gen` = 2, y el daemon la rechaza. No hay
        // `SyncPlanStarted`; sí un `SyncPlanFailed` cuyo banner sale por
        // `failed_banner(2, 2, …)` y no toca el hueco.
        assert!(
            failed_banner(
                2,
                2,
                &norte_proto::Error::OverlappingRoots {
                    relation: norte_proto::RootOverlap::DestInsideSource,
                }
            )
            .is_some()
        );

        let steps = pasos(&[1]);
        route_steps(&mut hueco, lote(task(), steps.clone()));
        route_plan_done(&mut hueco, cierre(task(), &steps));
        let v = hueco.expect("sigue abierto");
        assert_eq!(v.run.steps().len(), 1, "sus pasos siguen entrando");
        assert!(
            v.run.can_approve(),
            "y su plan cierra: una petición ajena rechazada no lo invalida"
        );
    }

    /// El final del canal dice cómo acabó la Task del plan, y el mapeo es el
    /// COMPARTIDO (`SyncRunState::from_task_state`). Un fallo devuelve además
    /// su categoría, que es lo que el pane que lanzó pinta en el banner.
    #[test]
    fn el_final_del_canal_fija_el_desenlace() {
        let mut v = vista_de_prueba();
        assert_eq!(v.run.run, SyncRunState::Running);
        let categoria = v.on_plan_ended(
            task(),
            &TaskState::Failed {
                error: norte_proto::Error::NotFound,
            },
        );
        assert_eq!(v.run.run, SyncRunState::Failed);
        assert_eq!(categoria.as_deref(), v.run.error.as_deref());
        assert!(categoria.is_some(), "un fallo trae su categoría");

        let mut v = vista_de_prueba();
        assert!(v.on_plan_ended(task(), &TaskState::Cancelled).is_none());
        assert_eq!(v.run.run, SyncRunState::Cancelled);
    }

    /// Un final de OTRA Task no toca el run. Hoy el guard de generación ya lo
    /// impide; en cuanto la tarea 4 lance `sync.apply`, el final del canal del
    /// PLAN llegará con la aplicación ya corriendo, y sin esto apagaría su
    /// `Running` con un «hecho» que habla de otra cosa.
    #[test]
    fn un_final_de_otra_task_no_toca_el_run() {
        let mut v = vista_de_prueba();
        assert!(
            v.on_plan_ended(otra_task(), &TaskState::Completed)
                .is_none()
        );
        assert_eq!(v.run.run, SyncRunState::Running);
    }

    /// Una negativa SUPERADA no pinta banner —y su `None` retira el
    /// «planificando…» que ella misma puso—. Es el defecto que C1 shipeó en
    /// `CompareFailed` y que aquí no se repite.
    #[test]
    fn una_negativa_superada_no_pinta_banner() {
        let e = norte_proto::Error::Unsupported;
        assert!(
            failed_banner(4, 4, &e).is_some(),
            "la vigente sí se pinta, con su categoría"
        );
        assert!(
            failed_banner(5, 4, &e).is_none(),
            "la superada no describe una petición que el lector ya reemplazó"
        );
    }
}
