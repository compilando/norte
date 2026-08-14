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
//! # Lo que este módulo SÍ escribe
//! El TEXTO de un paso y el árbol GPUI que lo pinta (tarea 3), aquí y no en
//! `main.rs`: ese fichero pasa de las quince mil líneas y su único `Render` ya
//! reparte diez pantallas, igual que con [`crate::compare_view`]. Lo que se
//! pinta —las tres marcas, qué devuelve el undo, el resumen y el pie— se
//! calcula en `norte_frontend::sync`; de este lado quedan el reparto del
//! sitio, los colores (que jamás son la única señal, spec §17) y la
//! separación ESTRUCTURAL de nombres y veredictos, sin la cual un fichero
//! bien nombrado puede hacerse pasar por una fila entera ante un lector de
//! pantalla.
//!
//! # Y la APROBACIÓN, que es la mitad destructiva
//! [`approve`], [`confirm_yes`] y [`on_apply_start`] (tarea 4). Aquí tampoco
//! se decide nada nuevo: «¿esto se puede aprobar?» la contesta
//! [`norte_frontend::sync::SyncView::can_approve`] y la frase de la segunda
//! pregunta la redacta
//! [`norte_frontend::sync::SyncPlan::confirmation`]. Lo que este módulo añade
//! es el ORDEN —el gate antes que el prompt, que es justo lo que el CLI de la
//! fase A hizo al revés— y a quién hay que cancelar cuando la Task que este
//! panel alimenta pasa a ser la que ESCRIBE.

use norte_frontend::sync::{SyncRunState, SyncState, SyncView as SyncRun};
use norte_proto::methods::{
    PlanHash, SyncFailure, SyncMode, SyncPlanDone, SyncReportResult, SyncStepsBatch,
};
use norte_proto::{TaskId, TaskState, VPath};

use crate::sp;

/// El panel de sincronización abierto en la GUI: el run compartido con la
/// TUI, más lo que solo esta GUI necesita.
pub struct SyncView {
    /// La Task del PLAN: la que arrancó este panel, y la que
    /// [`SyncView::on_plan_ended`] filtra contra.
    ///
    /// No es decoración, y no es redundante con el `task_id` que el modelo
    /// guarda en `SyncState::Planning`: ese se cae en cuanto el plan cierra
    /// (un `SyncState::Ready` no tiene Task), y esto es lo que sigue
    /// habiendo que cancelar cuando el panel se suelta —el daemon puede
    /// seguir recorriendo los dos árboles aunque el plan ya esté cerrado, y
    /// cancelar una Task terminada es un no-op—.
    ///
    /// **Ya NO se reasigna al arrancar `sync.apply`** (#191): antes este
    /// campo se sobrescribía en [`SyncView::on_apply_started`] con la Task de
    /// escritura, y eso perdía el ÚNICO sitio que sabía cuál era la Task del
    /// plan. `sync.plan_done` llega ANTES de que el canal del plan cierre
    /// (documentado en [`SyncView::on_plan_ended`]), así que aprobar
    /// enseguida deja ese canal todavía en vuelo cuando la reasignación ya
    /// había borrado el único id que lo nombraba — nada en la GUI podía
    /// cancelarlo. Ahora este campo se queda fijo mientras el panel vive, y
    /// [`Self::apply_task`] es quien nombra la Task de escritura una vez
    /// arranca.
    pub plan_task: TaskId,
    /// La Task de `sync.apply`, una vez arrancada — `None` mientras se
    /// planifica y antes de aprobar.
    ///
    /// Puesta por [`SyncView::on_apply_started`], que es también quien decide
    /// si el panel la ADOPTA. [`close`] y el `Esc` cancelan las DOS Tasks que
    /// este struct nombra, plan y aplicación, precisamente porque las dos
    /// pueden seguir corriendo a la vez (#191).
    pub apply_task: Option<TaskId>,
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
            plan_task: started.task_id,
            apply_task: None,
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
    /// contraste protege un camino: el evento no lleva generación (ver
    /// [`route_steps`]), así que el final del plan al que un panel nuevo
    /// sustituyó llega igual, y sin esto pintaría su desenlace encima.
    ///
    /// **Y también sin tocar nada si `sync.apply` ya arrancó** (#191): el
    /// final del canal del PLAN sigue en vuelo mientras la aplicación corre
    /// —`sync.plan_done` llega ANTES de que ese canal cierre—, y sin este
    /// SEGUNDO guard apagaría el `Running` de la aplicación con un «hecho»
    /// que habla de otra Task. Antes de #191 esto lo conseguía gratis la
    /// reasignación de un único campo (`task_id` pasaba a nombrar la Task de
    /// escritura, así que el contraste de arriba fallaba solo); separar
    /// `plan_task` de `apply_task` para poder cancelar los dos a la vez tiene
    /// este coste: la fase hay que preguntarla aparte.
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
        if task_id != self.plan_task || self.apply_task.is_some() {
            return None;
        }
        self.run.run = SyncRunState::from_task_state(state);
        // La segunda pregunta se cae con la petición que la motivó: dejarla
        // puesta bajo un pie que ya dice «falló» —con el marco en color de
        // aviso— es cómo un `y` posterior contesta a otra cosa (revisión de
        // seguridad MINOR-3, misma regla que `Key::CancelTask`).
        self.run.confirming = None;
        let TaskState::Failed { error } = state else {
            return None;
        };
        let categoria = norte_frontend::error::error_category(error);
        self.run.error = Some(categoria.clone());
        Some(categoria)
    }

    /// `sync.apply` contestó con una Task: el panel la ADOPTA — el modelo
    /// avanza a `Applying` y este panel pasa a cancelar la aplicación en vez
    /// del plan.
    ///
    /// Devuelve `false` **sin tocar nada** si este panel no está en
    /// condiciones de adoptarla, y entonces la Task es huérfana: nadie la mira
    /// y hay que cancelarla (ver [`on_apply_start`]).
    ///
    /// Dos negativas, y las dos se resuelven ANTES de mutar:
    ///
    /// * **Ya se pidió cancelar.** El `Esc` que cabe entre la tecla que aprobó
    ///   y esta respuesta cancela la Task del PLAN y deja `cancel_requested`
    ///   puesto; el `on_apply_started` compartido lo BORRA, así que sin este
    ///   guard la aplicación arrancaba igual y quien pulsó `Esc` necesitaba
    ///   otros dos para pararla. Se resuelve del lado conservador, que es lo
    ///   que [`norte_frontend::sync::SyncView::can_approve`] ya enuncia: quien
    ///   pulsó `Esc` pidió parar, y esta pantalla escribe en el disco de
    ///   alguien (revisión rust MAJOR-1).
    /// * **El panel no es aprobable**, que es exactamente la condición que el
    ///   modelo exige para pasar a `Applying` —y por la parte del run,
    ///   más estricta—. Se pregunta a la ÚNICA función que contesta eso en
    ///   toda la GUI, no a una segunda comprobación escrita aquí.
    ///
    /// # `apply_task` se RELLENA, no reasigna (#191)
    /// [`close`] y el `Esc` cancelan las DOS Tasks que el panel conoce, plan
    /// y aplicación — antes de #191 esto SOBRESCRIBÍA `plan_task`, así que
    /// cerrar cancelaba únicamente la que estaba ESCRIBIENDO y dejaba
    /// corriendo, sin nadie que la nombrara, la del plan (`sync.plan_done`
    /// llega antes de que su canal cierre — ver [`on_plan_ended`]). Se rellena
    /// solo si el modelo ACEPTÓ de verdad: la última palabra sobre si esto se
    /// está aplicando la tiene él.
    pub fn on_apply_started(&mut self, task_id: TaskId) -> bool {
        // El guard (`cancel_requested`) y el pestillo viven en
        // `norte_frontend::sync::SyncView` desde la revisión de rama de C2:
        // estaban aquí, y por eso la TUI se quedaba sin ellos.
        if !self.run.on_apply_started(task_id) {
            return false;
        }
        // Se pregunta por el ESTADO resultante y no se presupone: el modelo
        // es quien tiene la última palabra sobre si esto se está aplicando.
        let adoptada = matches!(&self.run.state, SyncState::Applying(a) if a.task_id() == task_id);
        if adoptada {
            self.apply_task = Some(task_id);
        }
        adoptada
    }

    /// Terminó la Task de `sync.apply`: fija el desenlace y mete el informe.
    /// Devuelve la CATEGORÍA localizada del error si hubo, para el banner.
    ///
    /// Devuelve `None` sin tocar nada si el final es de OTRA Task — el mismo
    /// contraste que [`SyncView::on_plan_ended`], y aquí protege el caso
    /// simétrico: el final del canal del PLAN sigue en vuelo mientras la
    /// aplicación corre.
    ///
    /// # Qué se hace con el informe NO se decide aquí
    /// Es [`norte_frontend::sync::SyncView::on_apply_ended`], la compartida:
    /// el error de la Task manda sobre el del informe, un informe que no llega
    /// es un FALLO (aunque la Task dijera `Completed`, porque sin él no se
    /// sabe cuánto se escribió) y un estado no terminal también. Vivía aquí,
    /// y la TUI tenía sus propios brazos en `harvest_sync_apply` — con el
    /// segundo de los tres SIN arreglar, así que su pie decía «aplicando…»
    /// para siempre. Arreglarlo en una copia y dejarlo en la otra es
    /// exactamente lo que C1 shipeó (revisión rust MAJOR-3).
    ///
    /// De este lado se queda lo que de verdad es de esta GUI: el contraste del
    /// `task_id` y dónde guardar la categoría.
    pub fn on_apply_ended(
        &mut self,
        task_id: TaskId,
        state: &TaskState,
        report: Result<SyncReportResult, norte_proto::Error>,
    ) -> Option<String> {
        if Some(task_id) != self.apply_task {
            return None;
        }
        let categoria = self.run.on_apply_ended(state, report);
        self.run.error.clone_from(&categoria);
        categoria
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
/// Vive en [`norte_frontend::sync`] desde la tarea 3, junto a
/// [`norte_frontend::sync::render_step`], que es quien decide con cuál se lee
/// cada ruta: el `rel` de un `DeleteTree` cuelga del DESTINO, así que leerlo
/// con la del origen nombraba el subárbol que se va a borrar con los bytes de
/// otro árbol. Esa decisión no puede vivir en un frontend, porque son tres los
/// que la necesitan (TUI, GUI y CLI).
pub use norte_frontend::sync::SyncEncodings;

/// Las Tasks vivas de un panel que se suelta: siempre la del PLAN, y la de
/// `sync.apply` si había arrancado.
///
/// #191: antes de esto, un panel guardaba una Task en un único campo que se
/// REASIGNABA a la de escritura en cuanto `sync.apply` arrancaba, así que
/// soltar el panel —por [`close`] o al sustituirlo en [`open`]— solo podía
/// cancelar UNA de las dos, nunca las dos a la vez. `sync.plan_done` llega
/// ANTES de que el canal del plan cierre (ver la rustdoc de
/// [`SyncView::on_plan_ended`]), así que una aprobación pronta deja la Task
/// del plan todavía viva en el instante en que la reasignación borraba el
/// único id que la nombraba — nada en la GUI podía cancelarla ya.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub struct ClosedTasks {
    /// La Task del plan. Cancelar una Task terminada es un no-op en el
    /// daemon, así que viaja siempre, sin mirar si su canal ya cerró.
    pub plan: TaskId,
    /// La Task de `sync.apply`, si había arrancado.
    pub apply: Option<TaskId>,
}

impl ClosedTasks {
    fn of(view: &SyncView) -> Self {
        Self {
            plan: view.plan_task,
            apply: view.apply_task,
        }
    }

    /// Los ids a los que mandarles `task.cancel`, plan primero.
    pub fn ids(self) -> impl Iterator<Item = TaskId> {
        std::iter::once(self.plan).chain(self.apply)
    }
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
    /// Se abrió el panel. Si había otro, éstas son SUS Tasks —plan y, si
    /// llegó a aplicar, aplicación (#191)—, que también se cancelan: dos
    /// planes a la vez serían dos flujos alimentando un diálogo cuyo
    /// `plan_hash` es lo que se aprueba (regla 3), mismo criterio que
    /// `launch_sync_plan` en la TUI.
    Opened(Option<ClosedTasks>),
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
pub fn open(slot: &mut Option<SyncView>, started: Started) -> Option<ClosedTasks> {
    let superseded = slot.take().as_ref().map(ClosedTasks::of);
    *slot = Some(SyncView::new(started));
    superseded
}

/// Suelta el panel y devuelve las Tasks a las que hay que mandarle
/// `task.cancel` ([`ClosedTasks`]).
///
/// La decisión de los dos caminos que lo sueltan desde la ventana —el `Esc`
/// del propio panel y la apertura del visor, que lo excluye— en un valor, por
/// lo mismo que [`on_start`]: `main.rs` no se puede testear, así que lo que se
/// puede equivocar no vive allí (revisión rust MINOR-4).
///
/// **Siempre devuelve la Task del plan, sin mirar si sigue viva**: un
/// `task.cancel` sobre una Task terminada es un no-op en el daemon, y mirar
/// antes sería una condición que un día se evalúa mal sobre algo que recorre
/// —o BORRA— dos árboles para un panel que ya no existe (regla dura 3).
///
/// # Qué significa cerrar A MEDIA APLICACIÓN
/// **Cancelar la aplicación, Y el plan (#191)**, y es una decisión, no un
/// accidente: desde [`SyncView::on_apply_started`] `apply_task` nombra la
/// Task que ESCRIBE y BORRA, así que soltar el panel la para — pero
/// `plan_task` sigue siendo una Task real (su canal puede seguir en vuelo, ver
/// [`SyncView::on_plan_ended`]) y dejarla corriendo es exactamente lo que la
/// regla 3 prohíbe igual que con la de escritura. Es también lo que hace la
/// TUI.
///
/// Llegar aquí a media aplicación pide DOS `Esc` (el primero pide la
/// cancelación y deja el panel abierto para leer el informe), así que el
/// camino corto no cierra nada por descuido.
///
/// # Lo que cancelar NO devuelve, hoy
/// La regla es que lo aplicado hasta el corte se queda journalizado (ADR
/// 0049), o sea media sincronización pero deshacible. **Tiene una excepción, y
/// es del core**: `sync::exec` no escribe la entrada del journal de un
/// `DeleteTree` que se corta a MEDIO borrar (`Err(Cancelled)`), y ese borrado
/// va entrada por entrada. Contra un destino sin papelera —un bucket, un
/// SFTP, un FAT— eso deja un subárbol parcialmente borrado, sin fila en el
/// journal, sin deshacer y sin fila en el informe. Es anterior a esta fase
/// (issue #186); se anota aquí porque éste es el sitio que la dispara y
/// porque el comentario que decía «no se pierde nada irrecuperable» era falso
/// para justo el plan que se lleva el aviso más largo (revisión de seguridad
/// MAJOR-2).
#[must_use]
pub fn close(slot: &mut Option<SyncView>) -> Option<ClosedTasks> {
    slot.take().as_ref().map(ClosedTasks::of)
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

/// Qué decir cuando el informe de un `sync.apply` llega y YA NO HAY PANEL.
///
/// Existe porque tirarlo era el desenlace: la tarea escribió, abrió un lote de
/// journal y el lector se quedaba sin el recuento, sin los fallos y sin saber
/// que hay algo que deshacer (revisión de seguridad MAJOR-1). Un banner corto
/// es poco, pero es la diferencia entre «no pasó nada» y «pasó esto».
///
/// El desenlace de la Task no entra: con informe, lo que importa es cuánto se
/// escribió, y eso lo dicen sus dos cuentas tanto si la Task terminó como si
/// la cortaron.
#[must_use]
pub fn orphan_report_banner(
    report: &Result<norte_proto::methods::SyncReportResult, norte_proto::Error>,
) -> String {
    match report {
        Ok(r) => norte_i18n::ta(
            "sync-orphan-report",
            &[
                ("done", &r.done.to_string()),
                ("failed", &r.failed.to_string()),
            ],
        ),
        Err(e) => failure_banner(&norte_frontend::error::error_category(e)),
    }
}

// ---------------------------------------------------------------------------
// Aprobar y aplicar (la mitad DESTRUCTIVA, tarea 4)
// ---------------------------------------------------------------------------

/// Qué hacer con la Task que `sync.apply` acaba de crear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum ApplyStart {
    /// **Nadie la mira: cancélala.** O la petición está vencida, o el panel
    /// que la pidió ya no está, o el modelo no la aceptó. Una Task de
    /// aplicación corriendo sin panel es exactamente lo que la regla 3
    /// prohíbe, y ésta ESCRIBE y BORRA: dejarla suelta es dejar corriendo un
    /// `Mirror` que nadie puede ni ver ni parar.
    Orphan(TaskId),
    /// El panel la adoptó: a partir de aquí es lo que su `Esc` y su cierre
    /// cancelan.
    Adopted,
}

/// Decide qué hacer con el `SyncApplyStarted` y lo aplica sobre el hueco.
///
/// El guard de generación va aquí porque esto juzga una PETICIÓN —la misma
/// regla que [`on_start`] y [`failed_banner`]— y porque una aplicación
/// superada es el peor de los casos: `sync_gen` avanza cuando el lector pide
/// otro plan, y ese plan ABRE otro panel; sin este guard la Task que está
/// borrando se quedaría corriendo detrás de él, sin nadie que la pinte ni la
/// pueda cancelar.
///
/// Lo que sigue correlacionándose por `task_id` es el FINAL
/// ([`SyncView::on_apply_ended`]), igual que el del plan.
pub fn on_apply_start(
    slot: &mut Option<SyncView>,
    current_gen: u64,
    generation: u64,
    task_id: TaskId,
) -> ApplyStart {
    if !crate::generation_is_current(current_gen, generation) {
        // La petición se resolvió aunque este evento se descarte: el panel que
        // sigue en pantalla no puede quedarse con el pestillo echado.
        if let Some(view) = slot.as_mut() {
            view.run.on_apply_abandoned();
        }
        return ApplyStart::Orphan(task_id);
    }
    match slot.as_mut() {
        Some(view) => {
            if view.on_apply_started(task_id) {
                ApplyStart::Adopted
            } else {
                // La petición se resolvió aunque el panel no la adopte: el
                // pestillo se suelta para no dejar la `a` muerta para siempre.
                view.run.on_apply_abandoned();
                ApplyStart::Orphan(task_id)
            }
        }
        None => ApplyStart::Orphan(task_id),
    }
}

/// `sync.apply` fue RECHAZADO antes de existir Task alguna (un plan caducado
/// en el spool, un `PlanStale`, un daemon que se cayó entre el plan y la
/// aprobación): marca el panel como fallido y devuelve `(pane, frase)` para
/// el banner, o `None` si la petición ya está SUPERADA.
///
/// El mismo guard que [`failed_banner`], y la MISMA frase: son el mismo hecho
/// —«esto no se va a aplicar»— y dos redacciones para él es lo que la revisión
/// de la tarea 2 ya corrigió una vez (MINOR-1).
///
/// A diferencia de un `sync.plan` rechazado, aquí SÍ hay panel: el lector está
/// mirando el plan que acaba de aprobar, y un banner que aparece mientras el
/// pie sigue diciendo «pulsa `a` para aprobar» es la pantalla contradiciéndose.
/// Por eso el desenlace se escribe en el run — y `can_approve()` pasa a decir
/// que no, que es la verdad: no se sabe si el plan sigue en el spool.
///
/// # Una negativa NO puede describir una aplicación que ya arrancó
/// El segundo guard —el estado— es la mitad de la corrección del BLOCKER que
/// las dos revisiones encontraron. Con dos `sync.apply` del mismo hash en
/// vuelo, el spool consume el derecho una vez y el perdedor vuelve como
/// `PlanStale`, con la MISMA generación que el ganador: sin este guard esa
/// negativa marcaba `Failed` un panel que estaba BORRANDO, apagaba
/// [`is_running`] y convertía el siguiente `Esc` en un cierre —que cancela la
/// aplicación viva— mientras la pantalla decía que ya había fallado. El
/// pestillo `norte_frontend::sync::SyncView::submit` impide que se llegue a mandar el segundo;
/// esto lo impide igual si llegara por otro camino, porque las dos mitades
/// pueden aterrizar en cualquier orden.
pub fn on_apply_failed(
    slot: &mut Option<SyncView>,
    current_gen: u64,
    generation: u64,
    error: &norte_proto::Error,
) -> Option<(usize, String)> {
    if !crate::generation_is_current(current_gen, generation) {
        // Mismo motivo que en `on_apply_start`: el descarte es del EVENTO, no
        // del hecho de que la petición ya se resolvió.
        if let Some(view) = slot.as_mut() {
            view.run.on_apply_abandoned();
        }
        return None;
    }
    let view = slot.as_mut()?;
    if !matches!(view.run.state, SyncState::Ready(_)) {
        tracing::warn!("sync.apply rechazado descartado: este panel ya no espera respuesta");
        return None;
    }
    // La petición se resolvió: el pestillo se suelta.
    view.run.on_apply_abandoned();
    let categoria = norte_frontend::error::error_category(error);
    view.run.run = SyncRunState::Failed;
    view.run.error = Some(categoria.clone());
    // La pregunta a medio contestar se cae con la petición que la motivó:
    // dejarla puesta es cómo un `y` posterior contesta a otra cosa.
    view.run.confirming = None;
    Some((view.source_pane, failure_banner(&categoria)))
}

/// Lo que la tecla de aprobar (o la `y` que contesta la segunda pregunta)
/// consigue.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub enum Approve {
    /// **No se puede aprobar, y se EXPLICA** — jamás se pregunta.
    ///
    /// Este brazo es la corrección del peor defecto de esta spec: el CLI de
    /// la fase A no llegó a preguntar «¿se puede?» y cayó directo al
    /// `confirmation()`, que devuelve `None` cuando `!can_approve()` — así
    /// que los planes MENOS fiables recibían el aviso MÁS CORTO (un `mirror`
    /// bloqueado salía con un sí/no pelado, sin la frase de «esto borra N
    /// árboles») y luego se aplicaban enteros desde el spool.
    ///
    /// La frase es corta a propósito: el PORQUÉ ya está en pantalla, en el
    /// resumen ([`summary_lines`], que dice si el plan está bloqueado, si no
    /// cuadra con sus cuentas o si trae pasos que esta build no sabe nombrar)
    /// y en el pie ([`status_line`]). Esto solo contesta a la tecla.
    Refused(String),
    /// El plan merece la SEGUNDA pregunta, que queda armada en
    /// `run.confirming`. Nada se ha mandado.
    Asked,
    /// No la merece (o ya se contestó): manda este `plan_hash`.
    ///
    /// El hash es lo ÚNICO que viaja (ADR 0049): no hay forma de pedir que se
    /// ejecute algo distinto de lo que el panel enseñó.
    Submit(Box<PlanHash>),
}

/// La tecla de aprobar sobre el panel.
///
/// Tres reglas, y cada una es un defecto que una fase anterior shipeó:
///
/// 1. **Se pregunta a [`norte_frontend::sync::SyncView::can_approve`]**, que
///    envuelve `SyncState::can_approve` y NUNCA `SyncPlan::can_approve` — el
///    segundo sigue siendo alcanzable por `SyncState::plan()` y contesta que
///    sí sobre un plan ya aprobado, porque ninguno de sus tres factores cambia
///    al gastarse. En toda esta GUI hay UNA función que contesta esta
///    pregunta, y es la misma que contesta en la TUI.
/// 2. **El gate va PRIMERO**: un plan que no se puede aprobar no llega al
///    prompt, se explica ([`Approve::Refused`]).
/// 3. **La frase de la segunda pregunta es
///    [`norte_frontend::sync::SyncPlan::confirmation`]**, que la calcula de
///    `dest_trash` y de las cuentas. Aquí no se redacta una segunda frase
///    sobre el borrado: una cabecera que diga «algo de esto se puede deshacer»
///    sobre una confirmación que diga «nada» enseña a saltarse las dos.
pub fn approve(view: &mut SyncView) -> Approve {
    if !view.run.can_approve() {
        return Approve::Refused(norte_i18n::t("msg-sync-cannot-approve"));
    }
    // `can_approve()` ya implica `Ready` con plan, así que este `else` no
    // ocurre; se contesta igual que la negativa en vez de con un `unwrap`
    // (regla dura 6).
    let Some(plan) = view.run.state.plan() else {
        return Approve::Refused(norte_i18n::t("msg-sync-cannot-approve"));
    };
    let pregunta = plan.confirmation(norte_i18n::active());
    match pregunta {
        Some(c) => {
            view.run.confirming = Some(c);
            Approve::Asked
        }
        // Por `submit`, que mira y echa el pestillo en un solo gesto y vive
        // con `can_approve`, `hint_id` y `status_line` (revisión de rama).
        None => match view.run.submit() {
            Some(hash) => Approve::Submit(Box::new(hash)),
            None => Approve::Refused(norte_i18n::t("msg-sync-cannot-approve")),
        },
    }
}

/// La `y` que contesta la segunda pregunta: la retira y manda el hash.
///
/// **Vuelve a preguntar por `can_approve`**, y no es redundante: entre la
/// primera respuesta y la segunda el modelo no retrocede, pero un
/// `SyncApplyStarted` o un `SyncApplyFailed` sí pueden haber aterrizado en
/// medio (esta GUI recibe eventos entre teclas), y el hash sale de aquí hacia
/// una escritura sin ninguna puerta después.
///
/// Y por el pestillo de `norte_frontend::sync::SyncView` antes que por nada: lo que hace imposible
/// aplicar dos veces NO es que el estado sea `Applying` —no lo es hasta que el
/// daemon contesta, una vuelta entera después de que la tecla mandara el
/// comando—, sino ese pestillo. La versión anterior de este comentario
/// afirmaba lo primero, que es cierto en la TUI (lanza el `sync.apply`
/// esperándolo, sin leer teclas) y era falso aquí.
pub fn confirm_yes(view: &mut SyncView) -> Approve {
    view.run.confirming = None;
    match view.run.submit() {
        Some(hash) => Approve::Submit(Box::new(hash)),
        None => Approve::Refused(norte_i18n::t("msg-sync-cannot-approve")),
    }
}

/// Cualquier otra tecla con la segunda pregunta puesta: la retira y no manda
/// nada.
///
/// Cualquiera y no solo una «n»: una pregunta a medio contestar tiene que
/// resolverse, porque dejarla puesta mientras el cursor se mueve por debajo es
/// cómo un `y` posterior aprueba otra cosa.
pub fn confirm_no(view: &mut SyncView) {
    view.run.confirming = None;
}

// ---------------------------------------------------------------------------
// El texto (puro: sin GPUI, testeable sin ventana ni GPU)
// ---------------------------------------------------------------------------

/// Una ruta de un paso, ya pintable.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PathText {
    /// Badge hostil + ruta enmascarada.
    pub label: String,
    /// El saneado ALTERÓ la ruta (spec §6). El badge visual ya va dentro de
    /// [`PathText::label`]; esto existe para la superficie AURAL, que no lo
    /// tiene: un lector de pantalla con la verbosidad de símbolos por defecto
    /// no pronuncia `⚠`, y bajo una reinterpretación activa (#57) el nombre
    /// enmascarado no lleva ni un `U+FFFD` — es texto limpio y legible que
    /// difiere de los bytes del disco, y el badge es su ÚNICA marca. Mismo
    /// razonamiento que [`crate::compare_view::FaceText::hostile`] (auditoría
    /// de encoding MAJOR-2 de C1).
    pub hostile: bool,
}

/// Un paso entero, ya pintable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepText {
    /// El id del paso, para anclar el elemento y el cursor.
    pub id: u64,
    /// Las TRES marcas —clase, confianza y qué devuelve el undo— separadas
    /// por espacios.
    ///
    /// Tres y no dos, y la del medio es la que la fase A perdió en el CLI y
    /// hubo que reponer: es exactamente la señal de «esto se sobrescribe
    /// fiándose sólo del mtime», en la pantalla donde se decide borrar un
    /// subárbol. Separadas porque el alfabeto NO es único entre las tres a
    /// propósito (`!` es `Certain` en la de confianza e `Irreversible` en la
    /// del undo): juntas se leerían como una palabra.
    pub marks: String,
    /// La ruta del paso, leída con la reinterpretación del lado del que
    /// CUELGA (#152, [`norte_frontend::sync::SyncEncodings::for_anchor`]).
    pub rel: PathText,
    /// La ortografía del DESTINO cuando el wire la manda (#152): los dos
    /// lados escriben la misma entrada de dos maneras y la escritura cae
    /// sobre ÉSTA.
    ///
    /// Va en su propio campo y en su propio elemento, jamás pegada a
    /// [`StepText::rel`] con un `→` dentro de la misma cadena: `→` es un
    /// carácter imprimible corriente que `display_name_with` no enmascara, o
    /// sea que un fichero llamado `a → b.txt` es legal en ext4, APFS y NTFS y
    /// llega SIN badge — unido en banda, fingiría la pareja. Es el mismo
    /// defecto que la auditoría de C1 encontró en el título del panel de
    /// diferencias (el `↔` entre las dos raíces), y el separador vuelve a ser
    /// ESTRUCTURAL por lo mismo.
    pub dest: Option<PathText>,
    /// De qué raíz cuelga [`StepText::rel`], en palabras, cuando no es la del
    /// origen. Vacío para lo normal.
    pub anchor: String,
    /// Tamaño legible, o vacío si el provider no lo dijo (jamás un cero
    /// fabricado).
    pub size: String,
    /// Nombre accesible de la FILA: las tres marcas **con palabras**, para
    /// quien no ve ni el glifo ni el color — y **nada más**.
    ///
    /// Las rutas NO están aquí, y esa ausencia es la corrección que la
    /// auditoría de encoding le hizo a C1 (MAJOR-2). El camino visual separa
    /// marcas, ruta de origen y ortografía de destino en elementos hermanos,
    /// así que un nombre no puede falsificar el veredicto: el separador es
    /// ESTRUCTURAL. Aplanarlos en una cadena unida por `·` le devolvería esa
    /// capacidad a la superficie aural — un fichero llamado
    /// `informe · copiar · el undo lo revierte.txt` se leería como una fila
    /// completa cuya clase la elige quien nombró el fichero, y aquí la clase
    /// es «borrar el subárbol». Cada ruta lleva su propio nombre accesible,
    /// en su propio elemento.
    pub a11y: String,
}

/// Ruta + badge, con el MISMO badge (`crate::HOSTILE_BADGE`) que el listado
/// de esta GUI — no un segundo marcador propio de este panel.
fn path_text(d: &norte_frontend::sync::RelDisplay) -> PathText {
    PathText {
        label: if d.hostile {
            format!("{} {}", crate::HOSTILE_BADGE, d.text)
        } else {
            d.text.clone()
        },
        hostile: d.hostile,
    }
}

/// Nombre accesible de UNA ruta: la ruta ya enmascarada, precedida de una
/// PALABRA localizada cuando el saneado la alteró, y seguida del
/// `qualifier` que diga de qué raíz cuelga.
///
/// La palabra y no el glifo: `⚠` no se pronuncia con la verbosidad de
/// símbolos por defecto de NVDA ni de Orca (auditoría de encoding MAJOR-2 de
/// C1, misma clave Fluent).
///
/// El `qualifier` es el ancla ([`StepText::anchor`]) y va PEGADO a la ruta que
/// califica, no suelto al final de la fila: para un `DeleteTree`,
/// «(destino)» es lo que dice que la ruta de la primera columna está en el
/// árbol del que se borra, y a la escucha eso solo se asocia por posición si
/// no se dice aquí (auditoría de encoding MINOR-3). Vacío = no hace falta
/// decir nada (una ruta de origen, que es lo normal).
///
/// # En banda, y por qué eso NO repite el defecto de C1 (#197)
/// El prefijo hostil y el `qualifier` se pegan los dos AL LADO del texto ya
/// resuelto (`out.push_str(qualifier)`, `format!("{prefix}: {label}")`) en
/// vez de vivir en elementos hermanos separados — a diferencia de
/// [`StepText::a11y`], cuya rustdoc explica por qué SU separador tiene que
/// ser estructural. La auditoría de C2 (MINOR-8) lo marcó por esa misma
/// forma, pero las dos direcciones aquí son fail-safe HOY: un nombre puede,
/// como mucho, AÑADIR un aviso que no le toca (un fichero literalmente
/// llamado `nombre alterado: x.txt` o `viejo (destino)` se queda con su
/// propio prefijo/calificador puesto APARTE, duplicado pero no sustituido,
/// porque ninguno de los dos `push`/`format!` de arriba lee el contenido de
/// `p.label` para decidir si emitirse) — nunca puede SUPRIMIR el que sí le
/// corresponde, porque las dos piezas se anteponen o se posponen siempre,
/// jamás se omiten por lo que diga el nombre. **Esa asimetría es la garantía
/// que un refactor futuro no puede invertir**: el día en que esto se vuelva
/// estructural (elementos hermanos, como `StepText::a11y`) está bien: el día
/// en que alguien decida el prefijo o el calificador MIRANDO `p.label` en vez
/// de mirar `p.hostile`/`qualifier`, deja de serlo.
#[must_use]
pub fn path_a11y(p: &PathText, qualifier: &str) -> String {
    if p.label.is_empty() {
        return String::new();
    }
    let mut out = if p.hostile {
        format!("{}: {}", norte_i18n::t("gui-a11y-hostile-name"), p.label)
    } else {
        p.label.clone()
    };
    if !qualifier.is_empty() {
        out.push(' ');
        out.push_str(qualifier);
    }
    out
}

/// Lo que un paso PINTA, resuelto de una vez.
///
/// Todo sale de [`norte_frontend::sync::render_step`], el mismo cálculo que
/// pinta la TUI y con el que el CLI imprime: aquí solo se compone el texto de
/// esta GUI. En particular **qué devuelve el undo no se deduce aquí** — se
/// deduce del PAR `(paso, papelera del destino)`, y un pintor que leyera
/// `SyncStep::reversal` a secas es justo el bug contra el que está escrito
/// `norte_frontend::sync`.
#[must_use]
pub fn step_text(
    step: &norte_proto::methods::SyncStep,
    dest_trash: norte_proto::methods::DestTrash,
    enc: norte_frontend::sync::SyncEncodings,
) -> StepText {
    use norte_frontend::sync::{render_step, step_label, undo_label};

    let cells = render_step(step, dest_trash, enc);
    let lang = norte_i18n::active();
    StepText {
        id: cells.id,
        marks: format!(
            "{} {} {}",
            cells.glyphs.kind, cells.glyphs.confidence, cells.glyphs.undo
        ),
        rel: path_text(&cells.rel),
        // Sin filtrar aquí: quién decide que las dos ortografías SON dos lo
        // decide `render_step`, por BYTES y una sola vez para los tres
        // frontends. Aquí se comparaba el texto ya enmascarado, que es lossy,
        // así que dos ficheros distintos con un byte inválido cada uno
        // plegaban a uno y el campo que dice sobre qué nombre cae la escritura
        // desaparecía de la fila —sin flecha, sin marca y sin nada— justo
        // cuando los nombres eran adversarios (auditoría de encoding MAJOR-1).
        dest: cells.dest_rel.as_ref().map(path_text),
        anchor: norte_frontend::sync::anchor_label(cells.anchor, norte_i18n::active())
            .unwrap_or_default(),
        size: cells
            .size
            .map_or_else(String::new, norte_frontend::human_bytes),
        a11y: format!(
            "{} · {} · {}",
            step_label(step.kind, lang),
            norte_frontend::compare::confidence_label(step.confidence, lang),
            undo_label(cells.undo, lang),
        ),
    }
}

/// El índice del paso bajo el cursor DENTRO de lo que la lista pinta, para
/// traerlo a la ventana virtualizada.
///
/// `None` mientras el plan no ha cerrado (no hay cursor: `SyncState::plan()`
/// contesta `None` sin `sync.plan_done`) o si el id seleccionado no está entre
/// los pasos que llegaron — que es la respuesta honesta, y no un 0 que
/// desplazaría la lista a un sitio que nadie pidió.
///
/// La posición se busca sobre [`norte_frontend::sync::SyncView::steps`], que
/// es exactamente lo que el `uniform_list` enumera: dos recorridos distintos
/// darían un índice que no señala a la fila del cursor.
#[must_use]
pub fn cursor_index(view: &SyncView) -> Option<usize> {
    let id = view.run.state.plan()?.selected_id()?;
    view.run.steps().iter().position(|s| s.id == id)
}

/// Tope de celdas por raíz en la cabecera, igual que en el panel de
/// diferencias: una raíz más larga se corta CON su marca (`middle_ellipsis`
/// recorta por el medio, #79: por celdas y no por chars) en vez de que el
/// `.truncate()` del div se la lleve por la derecha en silencio — «jamás
/// pérdida silenciosa» (spec §6).
const ROOT_MAX_CELLS: usize = 64;

/// La cabecera del panel: el modo y las dos raíces, cada una por su lado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleText {
    /// `sync-mode-update`, `sync-mode-mirror` o `sync-mode-unknown`. Que un
    /// `Mirror` BORRA es la mitad de lo que se aprueba, y tiene que verse
    /// antes de aprobar.
    pub mode: String,
    /// Raíz ORIGEN, badgeada y acotada.
    pub source: PathText,
    /// Raíz DESTINO, ídem.
    pub dest: PathText,
}

/// La cabecera, ya saneada, badgeada y ACOTADA — **y nunca unida en una sola
/// cadena**.
///
/// El sentido es la mitad de lo que se aprueba, así que el `→` que lo dice es
/// un elemento PROPIO y no un carácter dentro del texto: un directorio
/// llamado `docs → backup/home/victima` es legal en ext4, APFS y NTFS, no es
/// un hazard de terminal (así que llega sin badge) y en banda se leería como
/// OTRO par de raíces. Es el mismo defecto que la auditoría de encoding le
/// encontró al título del panel de diferencias con el `↔` (MAJOR-1), y la
/// misma corrección: separador estructural, y cada raíz con su propio
/// `flex_1` para que ninguna pueda expulsar a la otra.
#[must_use]
pub fn title_text(run: &SyncRun) -> TitleText {
    let one = |p: &VPath, enc: Option<norte_encoding::NameEncoding>| {
        let (texto, hostil) = norte_frontend::path_display_with(p, enc);
        let texto = norte_frontend::middle_ellipsis(&texto, ROOT_MAX_CELLS);
        PathText {
            label: if hostil {
                format!("{} {texto}", crate::HOSTILE_BADGE)
            } else {
                texto
            },
            // El bool SOBREVIVE al badge, y esa es la corrección: la cabecera
            // lo tiraba, así que ningún llamante podía construir la forma
            // AURAL y las dos raíces llegaban marcadas a la vista y desnudas
            // al oído —bajo una reinterpretación activa (#57) el badge es su
            // ÚNICA marca, porque el texto sale limpio y legible—. Y ésta es
            // la línea que nombra el árbol que se sobrescribe (auditoría de
            // encoding MAJOR-2).
            hostile: hostil,
        }
    };
    TitleText {
        // El brazo `_` NO cae en «actualizar»: `SyncMode` es
        // `#[non_exhaustive]`, y decir «esto no borra» de un modo que esta
        // build no sabe nombrar es afirmar la mitad SEGURA de lo que hay que
        // aprobar — al revés que `RelAnchor::Either` y `StepUndo::Unclear`,
        // que en el mismo modelo admiten que no lo saben.
        mode: norte_frontend::sync::mode_label(run.mode, norte_i18n::active()),
        source: one(&run.source_root, run.source_encoding),
        dest: one(&run.dest_root, run.dest_encoding),
    }
}

/// Las frases del resumen, en orden de lectura, o vacío mientras el plan no
/// haya cerrado.
///
/// La redacción entera es [`norte_frontend::sync::SyncPlan::summary_lines`],
/// COMPARTIDA con la TUI y con el CLI: es donde se dice qué devuelve el undo
/// y de qué papelera se habla, y una segunda redacción sería una segunda
/// respuesta a eso. **Una línea es una frase entera**: se pintan una por
/// elemento y no se concatenan.
#[must_use]
pub fn summary_lines(run: &SyncRun) -> Vec<String> {
    run.state
        .plan()
        .map(|p| p.summary_lines(norte_i18n::active()))
        .unwrap_or_default()
}

/// El informe de `sync.report`, si ya llegó. `None` en todo lo demás — y en
/// particular mientras la aplicación CORRE, que es cuando todavía no se sabe
/// qué se escribió.
#[must_use]
pub fn report(run: &SyncRun) -> Option<&SyncReportResult> {
    match &run.state {
        SyncState::Applied(a) => Some(a.report()),
        _ => None,
    }
}

/// UN paso que NO ocurrió, ya pintable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureText {
    /// La ruta del fallo, badgeada y enmascarada.
    pub rel: PathText,
    /// La ortografía del DESTINO cuando el informe la manda y difiere en
    /// bytes (#152) — en su PROPIO campo, por lo mismo que
    /// [`StepText::dest`]: `→` es un carácter imprimible corriente que el
    /// saneado no enmascara, así que unidas en banda un nombre fingiría la
    /// pareja.
    pub dest: Option<PathText>,
    /// Por qué no ocurrió, en palabras y en el idioma del lector.
    ///
    /// En su propio campo y en su propio elemento, jamás pegado a la ruta con
    /// un `:` dentro de la misma cadena: un fichero llamado
    /// `informe: permiso denegado.txt` es legal en ext4, APFS y NTFS y llega
    /// SIN badge, así que en banda fingiría el veredicto de la fila. Es la
    /// misma regla —y el mismo separador ESTRUCTURAL— que las tres marcas de
    /// un paso.
    pub cause: String,
    /// De qué raíz cuelga [`FailureText::rel`], en palabras, cuando no consta.
    /// Vacío cuando el informe lo prueba (manda `dest_rel`, así que `rel` es
    /// la mitad del origen).
    ///
    /// **Se pinta, y no se calla**: en este panel una ruta sin calificar
    /// significa «del origen» —así lo escribe [`StepText::anchor`] veinte
    /// líneas más arriba, y las dos listas van una encima de la otra—, así que
    /// el silencio sería la afirmación. Un `DeleteTree` que falla por permisos
    /// es la fila hostil más común de un `Mirror` y su ruta cuelga del
    /// DESTINO; el informe no trae la clase que lo diría, y decir «no consta»
    /// es lo único honesto (auditoría de encoding MAJOR-2).
    pub anchor: String,
    /// Nombre accesible de la FILA: la causa, con palabras, y **nada más**.
    ///
    /// Las rutas NO están aquí, por lo mismo que en [`StepText::a11y`]: si lo
    /// estuvieran, un fichero llamado como el veredicto podría hacerse pasar
    /// por una fila entera ante un lector de pantalla. Cada ruta lleva su
    /// propio nodo y su propio nombre.
    ///
    /// Es un CAMPO y no una cadena compuesta en el árbol de render para que se
    /// pueda afirmar sin ventana — que es lo que hace útil al test gemelo de
    /// los pasos (auditoría de encoding MINOR-5).
    pub a11y: String,
}

/// Un fallo del informe, ya pintable.
///
/// Las dos rutas y el plegado los resuelve
/// [`norte_frontend::sync::render_failure`], igual que un paso pasa por
/// `render_step`: el plegado es por BYTES y la ortografía del destino se lee
/// con la del destino. La causa la nombra
/// [`norte_frontend::sync::failure_cause_label`], la MISMA tabla que imprime
/// el CLI — un `_` que sobrevive a un daemon más nuevo no puede tener dos
/// copias, o una nombra lo que la otra llama «no reconocido».
#[must_use]
pub fn failure_text(
    failure: &SyncFailure,
    enc: norte_frontend::sync::SyncEncodings,
) -> FailureText {
    let cells = norte_frontend::sync::render_failure(failure, enc);
    let cause = norte_frontend::sync::failure_cause_label(failure.cause, norte_i18n::active());
    FailureText {
        rel: path_text(&cells.rel),
        dest: cells.dest_rel.as_ref().map(path_text),
        // `Dest` no lo produce `render_failure` hoy —haría falta la clase en
        // el wire—, pero el compartido lo nombra igual: el día que llegue, un
        // `_` lo habría pintado como «del origen» sin decir nada.
        anchor: norte_frontend::sync::anchor_label(cells.anchor, norte_i18n::active())
            .unwrap_or_default(),
        a11y: cause.clone(),
        cause,
    }
}

/// Cuántos fallos PINTA el panel como filas.
///
/// Un tope de PANTALLA, encima del tope del protocolo
/// ([`norte_proto::methods::SYNC_MAX_FAILURES_REPORTED`], 256): la lista no
/// está virtualizada —vive dentro de un `flex_col` con `overflow_hidden`, así
/// que 256 filas empujarían el pie y la línea de estado fuera de la ventana—
/// y lo que no cabe se pierde SIN decirlo, que es lo único que no se puede
/// hacer (spec §6). Con el tope, lo que no se pinta se CUENTA
/// ([`failures_hidden`]) — mismo trato que el CLI le da a los bloqueos de un
/// plan.
///
/// Ocho, que con el título y la línea del resto son diez filas: suficiente
/// para ver la FORMA del fallo (todo permisos, todo nombres ilegales) sin
/// comerse el panel. El informe completo se lee por el CLI o por el journal.
pub const FAILURES_SHOWN: usize = 8;

/// Los fallos del informe que se PINTAN, uno por línea y en el orden en que
/// llegaron: los primeros [`FAILURES_SHOWN`]. Vacío mientras no haya informe.
#[must_use]
pub fn failures(run: &SyncRun) -> Vec<FailureText> {
    let enc = run.encodings();
    report(run).map_or_else(Vec::new, |r| {
        r.failures
            .iter()
            .take(FAILURES_SHOWN)
            .map(|f| failure_text(f, enc))
            .collect()
    })
}

/// Cuántos fallos hay que el panel NO enseña.
///
/// Dos topes se acumulan y esta cifra los cuenta LOS DOS: `sync.report`
/// recorta la lista a [`norte_proto::methods::SYNC_MAX_FAILURES_REPORTED`]
/// mientras `failed` cuenta sin tope, y el panel pinta [`FAILURES_SHOWN`] de
/// los que llegaron. Sin esta cifra, ocho filas pasarían por el total de
/// cuarenta mil — la misma clase de mentira que un total de bytes que esconde
/// los ficheros que no se pudieron medir.
#[must_use]
pub fn failures_hidden(run: &SyncRun) -> u64 {
    report(run).map_or(0, |r| {
        let pintados = u64::try_from(r.failures.len().min(FAILURES_SHOWN)).unwrap_or(u64::MAX);
        r.failed.saturating_sub(pintados)
    })
}

/// El pie: en qué punto está el diálogo.
///
/// La frase la compone [`norte_frontend::sync::status_line`], la MISMA que
/// pinta la TUI (#161). Aquí no se re-decide nada — y sobre todo no se
/// re-decide «¿esto se puede aprobar?», que es el brazo `Ready` de esa
/// función y la única pregunta de esta pantalla que acaba escribiendo en el
/// disco de alguien.
#[must_use]
pub fn status_line(run: &SyncRun) -> String {
    norte_frontend::sync::status_line(run, norte_i18n::active())
}

/// Un estado que ya no espera nada del daemon: el `Esc` deja de tener nada
/// que cancelar.
#[must_use]
pub fn is_running(run: &SyncRun) -> bool {
    run.run == SyncRunState::Running
}

// ---------------------------------------------------------------------------
// El teclado del panel (puro)
// ---------------------------------------------------------------------------

/// Cuántos pasos mueve una tecla de página. El mismo 10 fijo que la TUI
/// (`COMPARE_PAGE_STEP`, que su panel de sincronización comparte) y que
/// [`crate::compare_view`]: el alto real del panel no llega hasta el momento
/// del layout de GPUI, y un salto que cambia con el tamaño de la ventana es
/// peor de aprender que uno constante.
const PAGE_STEP: isize = 10;

/// Lo que una tecla SIGNIFICA dentro del panel de sincronización.
///
/// Separado del despacho por lo mismo que [`crate::compare_view::Key`]: lo
/// que se puede equivocar aquí es la DECISIÓN, y dos de ellas —el `Esc`, que
/// es la única salida de una pantalla que se queda el teclado entero, y la
/// `y` que contesta a «esto borra N árboles»— no admiten un despacho que se
/// equivoque.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Ni la toca.
    Ignore,
    /// Pedir la cancelación de la Task, conservando los pasos que llegaron.
    CancelTask,
    /// Cerrar el panel (y cancelar lo que quede vivo).
    Close,
    /// Mover el cursor por los pasos.
    Move(isize),
    /// Aprobar: o abre la segunda pregunta, o manda el plan ([`approve`]).
    Approve,
    /// La `y` que contesta que sí a la segunda pregunta ([`confirm_yes`]).
    ConfirmYes,
    /// Cualquier otra cosa con la pregunta puesta: la retira ([`confirm_no`]).
    ConfirmNo,
}

/// Traduce una tecla de GPUI (`"escape"`, `"pagedown"`…) a lo que significa
/// en este panel.
///
/// * El primer `Esc` sobre una Task VIVA la cancela y conserva sus pasos;
///   **cualquier `Esc` posterior cierra**, sin mirar el estado de la Task.
///   Condicionar el cierre a un estado terminal deja encerrado al lector
///   cuando el canal no llega a cerrarse nunca —un daemon caído, un provider
///   colgado en una NFS muerta—, que es el BLOCKER-1 que la TUI ya pagó y que
///   el panel de diferencias de esta GUI heredó resuelto. Y con la aplicación
///   corriendo es TAMBIÉN la salida: `Esc` cancela lo que este panel esté
///   alimentando, que después de aprobar es la Task que escribe.
/// * **Con la segunda pregunta puesta el teclado se reduce a `y` y «no»**, y
///   eso se resuelve por ENCIMA del filtro de modificadores. Con el filtro
///   delante, un `Ctrl+r` o un `Alt+e` de costumbre caían en `Ignore` y
///   dejaban «se van a borrar 2 árboles… ¿Seguir?» armada en pantalla,
///   esperando un `y` que ya no sabe a qué contesta (la corrección que la TUI
///   documenta). El `Esc` es la única excepción: cerrar no es una respuesta a
///   la pregunta.
/// * **`Enter` NO aprueba.** Sincronizar borra y sobrescribe, así que se pide
///   una tecla que nadie pulsa por inercia — el mismo criterio que los
///   diálogos TOFU y la aprobación de una op de agente, y la misma tecla que
///   la TUI.
/// * Con `ctrl`/`alt`/`cmd` no significa nada.
/// * **No hay tecla de salir de norte**, y ahí diverge de la TUI: en modo raw
///   `ISIG` está apagado y su panel tuvo que añadir `Ctrl+C`; una ventana
///   tiene el botón de cerrar del gestor de ventanas, que no es algo que este
///   panel pueda comerse.
#[must_use]
pub fn key_meaning(
    key: &str,
    modified: bool,
    running: bool,
    cancel_requested: bool,
    confirming: bool,
    submitted: bool,
) -> Key {
    // El `Esc` va primero, y sigue valiendo con la pregunta puesta: salir no
    // es una respuesta a «¿seguro?». Solo sin modificadores, como el resto de
    // este panel.
    if key == "escape" && !modified {
        // `submitted` cuenta como corriendo, y ésa es la mitad que faltaba
        // (revisión de seguridad MAJOR-1). Entre la tecla y la respuesta del
        // daemon, `run` todavía es `Done`, así que este `Esc` resolvía a
        // `Close` — y para entonces `sync.apply` YA arrancó el ejecutor: el
        // destino se reescribía a medias, se abría un lote de journal, y el
        // informe llegaba a un hueco vacío y se tiraba. El lector cerró
        // creyendo que no había empezado nada.
        return if (running || submitted) && !cancel_requested {
            Key::CancelTask
        } else {
            Key::Close
        };
    }
    if confirming {
        return if !modified && key == "y" {
            Key::ConfirmYes
        } else {
            Key::ConfirmNo
        };
    }
    if modified {
        return Key::Ignore;
    }
    match key {
        "up" => Key::Move(-1),
        "down" => Key::Move(1),
        "pageup" => Key::Move(-PAGE_STEP),
        "pagedown" => Key::Move(PAGE_STEP),
        "a" => Key::Approve,
        _ => Key::Ignore,
    }
}

// ---------------------------------------------------------------------------
// El render
// ---------------------------------------------------------------------------

/// Ancho de la columna de las tres marcas. Fijo, y con sitio para los dos
/// espacios que las separan.
const MARKS_W: f32 = 44.0;

/// Los colores del panel, resueltos UNA vez por frame.
///
/// El coste de resolver un rol del tema es por FRAME y jamás por paso: un
/// plan puede tener cientos de miles, y resolver el tema dentro del bucle es
/// la trampa O(N)-por-frame del #87 — la misma que el listado y el panel de
/// diferencias evitan construyendo su paleta antes de entrar.
///
/// No resuelve nada nuevo del tema: se deriva de la [`crate::ChromeColors`]
/// que `render` ya construyó (con su glow aplicado), así que este panel no
/// puede desviarse del resto del chrome.
#[derive(Clone, Copy)]
struct Palette {
    /// Texto normal.
    fg: gpui::Rgba,
    /// Texto secundario (cabeceras, ancla, tamaño, teclas).
    dim: gpui::Rgba,
    /// Algo que mirar sin llegar a error.
    warn: gpui::Rgba,
    /// Algo que no vuelve, o que esta build no sabe juzgar.
    bad: gpui::Rgba,
    /// Fondo de la fila bajo el cursor.
    sel_bg: gpui::Rgba,
    /// Texto de la fila bajo el cursor, si el tema lo declara.
    sel_fg: Option<gpui::Rgba>,
}

impl Palette {
    fn from_chrome(c: &crate::ChromeColors) -> Self {
        Self {
            fg: c.fg,
            dim: c.info_fg,
            warn: c.warn_fg,
            bad: c.err_fg,
            sel_bg: c.sel_bg,
            sel_fg: c.sel_fg,
        }
    }

    /// El color de las marcas de un paso. El GLIFO ya lo dice sin color
    /// ninguno (spec §17); esto solo lo refuerza para quien sí lo ve. Brazo
    /// por brazo el mismo reparto que `sync_undo_style` en la TUI.
    fn for_undo(&self, undo: norte_frontend::sync::StepUndo) -> gpui::Rgba {
        use norte_frontend::sync::StepUndo as U;
        match undo {
            U::Reverts | U::Nothing => self.fg,
            U::LeftBehind => self.warn,
            U::Irreversible | U::Unclear => self.bad,
        }
    }
}

/// Pinta el panel de sincronización en el sitio de los dos panes, y ENCIMA
/// del de diferencias si lo hay: el plan es lo que hay que mirar mientras se
/// decide, y volver a las filas es cerrarlo (mismo orden que la TUI).
///
/// Nada de lo que decide QUÉ se ve está aquí (regla dura 7): los pasos, el
/// cursor, las tres marcas, qué devuelve el undo y el resumen salen de
/// [`norte_frontend::sync`], que se testea sin ventana. Este lado reparte el
/// sitio y elige colores, y el color nunca es lo único que distingue nada.
///
/// La lista está virtualizada (`uniform_list`, #124) por lo mismo que la del
/// panel de diferencias: el processor solo corre para el rango visible, así
/// que un plan de un millón de pasos no construye un millón de elementos por
/// frame.
///
/// # La segunda pregunta se pinta DENTRO, y con el marco cambiado
/// Cuando [`approve`] la arma, el marco pasa a color de aviso y la pregunta
/// —la de [`norte_frontend::sync::SyncPlan::confirmation`], nunca una segunda
/// redacción— ocupa el pie con la tecla que la contesta EN OTRA LÍNEA: a un
/// ancho estrecho la pregunta sola llena la fila, y la versión unida se cortaba
/// justo por donde decía qué tecla la contesta (lo cazó el snapshot de la TUI).
/// El color no es la única señal: el texto de la pregunta lo es.
///
/// # Y los fallos del informe, uno por línea
/// En cuanto `sync.report` llega, debajo de los pasos y con su propio título:
/// son los pasos que NO ocurrieron, y la lista de arriba sigue enseñando lo
/// que se planificó.
pub fn render(
    view: &SyncView,
    chrome: &crate::ChromeColors,
    fonts: &crate::FontSet,
    scroll: &gpui::UniformListScrollHandle,
    cx: &mut gpui::Context<crate::NorteGui>,
) -> impl gpui::IntoElement {
    use gpui::{ParentElement, Styled, prelude::*, px};

    let palette = Palette::from_chrome(chrome);
    let run = &view.run;
    let title = title_text(run);
    let row_h = fonts.row_h;
    let mono = fonts.mono.clone();
    let dest_trash = run.dest_trash();
    let enc = run.encodings();
    let total = run.steps().len();

    // Los pasos se pintan LLEGANDO y no solo cerrados: mientras el plan viaja
    // `SyncState::plan()` contesta `None` y el pie ya está contando «6 pasos»
    // — un hueco vacío debajo sería la pantalla contradiciéndose (la misma
    // corrección que la TUI).
    let body = if total == 0 {
        gpui::div()
            .flex_1()
            .px(px(sp::S))
            .py(px(sp::XS))
            .text_color(palette.dim)
            // Y «el plan no tiene pasos» solo se dice del plan CERRADO:
            // mientras planifica, quien cuenta es el pie («planificando… 0
            // pasos»), y anunciar que los dos árboles ya coinciden antes de
            // haberlos recorrido es afirmar el resultado de la comparación
            // que todavía está corriendo.
            .child(gpui::SharedString::from(if run.state.plan().is_some() {
                norte_i18n::t("sync-empty")
            } else {
                String::new()
            }))
            .into_any_element()
    } else {
        // El `Role::List` va en un ENVOLTORIO y no en el propio
        // `uniform_list`: su id alimenta el scroll y la medida virtualizados,
        // y pisarlo con `.id()` para colgarle el rol sería arriesgar esa
        // identidad — mismo motivo que en el panel de diferencias. Sin él las
        // filas quedan de `Role::ListItem` HUÉRFANAS y un lector de pantalla
        // no sabe de qué lista son ni cuántas hay.
        gpui::div()
            .id("sync-steps-list")
            .role(gpui::Role::List)
            // Nombre PROPIO y no `sync-title` otra vez: el marco ya se anuncia
            // «sincronizar», y repetirlo en la lista no dice en cuál de los dos
            // está el lector (mismo reparto que el panel de diferencias, que
            // tiene `gui-a11y-compare-rows`).
            .aria_label(norte_i18n::t("gui-a11y-sync-steps"))
            .flex_1()
            .flex()
            .flex_col()
            .child(
                gpui::uniform_list(
                    gpui::SharedString::from("sync-steps"),
                    total,
                    cx.processor(move |this, range: std::ops::Range<usize>, _window, _cx| {
                        let Some(view) = this.sync.as_ref() else {
                            return Vec::new();
                        };
                        let run = &view.run;
                        let selected = run
                            .state
                            .plan()
                            .and_then(norte_frontend::sync::SyncPlan::selected_id);
                        run.steps()
                            .iter()
                            .skip(range.start)
                            .take(range.len())
                            .map(|step| {
                                render_step_row(
                                    step,
                                    selected == Some(step.id),
                                    dest_trash,
                                    enc,
                                    &palette,
                                    row_h,
                                )
                            })
                            .collect()
                    }),
                )
                .track_scroll(scroll)
                .flex_1()
                .font(mono),
            )
            .into_any_element()
    };

    // El resumen: cada frase en su propio elemento, jamás concatenadas.
    let mut resumen = gpui::div().flex().flex_col().px(px(sp::S)).py(px(sp::XS));
    for linea in summary_lines(run) {
        resumen = resumen.child(gpui::div().child(gpui::SharedString::from(linea)));
    }

    let estado_malo = matches!(run.run, SyncRunState::Failed | SyncRunState::Cancelled);

    // Los fallos del informe, uno por elemento y con su título — que es
    // también el nombre de la lista para un lector de pantalla. Van DEBAJO de
    // los pasos y no encima de ellos: la lista de arriba sigue diciendo lo que
    // se planificó, y ésta lo que no ocurrió.
    let fallos = failures(run);
    let ocultos = failures_hidden(run);
    let informe = (!fallos.is_empty()).then(|| {
        // La lista lleva su NOMBRE accesible; el título va FUERA de ella, como
        // hermano. Dentro, un lector de pantalla anunciaba el nombre de la
        // lista y acto seguido las mismas palabras como primer elemento — y
        // además era un hijo que no es `ListItem` colgando de un `List`.
        let mut lista = gpui::div()
            .id("sync-failures-list")
            .role(gpui::Role::List)
            .aria_label(norte_i18n::t("sync-failures-title"))
            .flex()
            .flex_col();
        for (i, f) in fallos.iter().enumerate() {
            lista = lista.child(render_failure_row(f, i, &palette));
        }
        let mut bloque = gpui::div()
            .flex_none()
            .flex()
            .flex_col()
            .overflow_hidden()
            .px(px(sp::S))
            .py(px(sp::XS))
            .child(
                gpui::div()
                    .text_color(palette.dim)
                    .child(gpui::SharedString::from(norte_i18n::t(
                        "sync-failures-title",
                    ))),
            )
            .child(lista);
        if ocultos > 0 {
            // Los dos topes dichos en voz alta: sin esto, ocho filas pasarían
            // por el total de cuarenta mil.
            bloque = bloque.child(gpui::div().text_color(palette.dim).child(
                gpui::SharedString::from(norte_i18n::ta(
                    "sync-failures-more",
                    &[("n", &ocultos.to_string())],
                )),
            ));
        }
        bloque
    });

    gpui::div()
        .id("sync-view")
        .role(gpui::Role::Document)
        .aria_label(norte_i18n::t("sync-title"))
        .flex_1()
        .flex()
        .flex_col()
        .overflow_hidden()
        .border_2()
        // El marco AVISA mientras la segunda pregunta está puesta, igual que
        // en la TUI. Refuerzo y no señal: quien no vea el color sigue leyendo
        // la pregunta, que está escrita.
        .border_color(if run.confirming.is_some() {
            palette.warn
        } else {
            chrome.border_focus
        })
        .bg(chrome.pane_bg_focus)
        .child(
            gpui::div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(sp::S))
                .px(px(sp::S))
                .py(px(sp::XS))
                .bg(chrome.header_bg)
                .text_color(chrome.header_fg)
                .child(
                    gpui::div()
                        .flex_none()
                        .child(gpui::SharedString::from(format!(
                            "{} ({})",
                            norte_i18n::t("sync-title"),
                            title.mode
                        ))),
                )
                // Las dos raíces, cada una en su elemento y con el mismo
                // `flex_1`: ninguna puede expulsar a la otra, y la `→` de
                // enmedio —que ES el sentido— es un elemento propio que un
                // nombre no puede fingir.
                // Cada una con su PROPIO nodo accesible y su nombre por
                // `path_a11y`: el badge `⚠` no se pronuncia con la verbosidad
                // de símbolos por defecto de NVDA ni de Orca, así que sin esto
                // las dos raíces llegaban marcadas a la vista y desnudas al
                // oído — y ésta es la línea que dice qué árbol se sobrescribe.
                .child(
                    gpui::div()
                        .id("sync-root-source")
                        .aria_label(gpui::SharedString::from(path_a11y(&title.source, "")))
                        .flex_1()
                        .truncate()
                        .child(gpui::SharedString::from(title.source.label.clone())),
                )
                .child(gpui::div().flex_none().child("→"))
                .child(
                    gpui::div()
                        .id("sync-root-dest")
                        .aria_label(gpui::SharedString::from(path_a11y(&title.dest, "")))
                        .flex_1()
                        .truncate()
                        .child(gpui::SharedString::from(title.dest.label.clone())),
                ),
        )
        .child(resumen)
        .child(body)
        .children(informe)
        .child(
            gpui::div()
                .px(px(sp::S))
                .py(px(1.0)) // sub-XS: acento fino de una línea
                .truncate()
                .text_color(if estado_malo { palette.bad } else { palette.fg })
                .child(gpui::SharedString::from(status_line(run))),
        )
        .child(render_footer(run, &palette))
}

/// El pie de teclas, o la SEGUNDA PREGUNTA cuando la hay.
///
/// Tres estados y no dos, y el del medio es el que la TUI tuvo que separar en
/// dos líneas: la pregunta y la tecla que la contesta no caben juntas en una
/// fila estrecha, y lo que se cortaba era la mitad que dice cómo contestar.
///
/// Cuál de las tres líneas toca NO se decide aquí: es
/// [`norte_frontend::sync::hint_id`], la misma que consulta la TUI (#161). Un
/// pie que ofrece una tecla muerta —`a aprobar` sobre un plan bloqueado, o
/// sobre uno ya gastado— se lee como una pantalla rota, y ésa es exactamente
/// la clase de desacuerdo que se arregla en un frontend y se olvida en el
/// otro.
fn render_footer(run: &SyncRun, palette: &Palette) -> gpui::AnyElement {
    use gpui::{ParentElement, Styled, prelude::*, px};

    let pie = gpui::div().px(px(sp::S)).py(px(1.0)); // sub-XS: acento fino
    if let Some(c) = &run.confirming {
        return pie
            .id("sync-confirm")
            .role(gpui::Role::Dialog)
            .aria_label(norte_i18n::t("gui-a11y-sync-confirm"))
            // Nombre = título, DESCRIPCIÓN = la pregunta, que es la convención
            // de `Role::Dialog` en esta crate: un lector anuncia diálogo →
            // título → cuerpo. Sin ella, lo único que se anunciaba antes de
            // borrar subárboles era «confirmar el plan», y la frase que
            // distingue «van a la papelera» de «NO se van a poder restaurar»
            // quedaba en un hijo de texto que nada enfoca (auditoría de
            // encoding MAJOR-3).
            .aria_description(gpui::SharedString::from(c.text.clone()))
            .flex()
            .flex_col()
            .text_color(palette.warn)
            // La pregunta NO se trunca: es la frase que dice cuántos árboles
            // se borran y si vuelven, y recortarla es esconder exactamente
            // eso. Las dos líneas son dos elementos, no una cadena unida.
            .child(gpui::div().child(gpui::SharedString::from(c.text.clone())))
            .child(gpui::div().child(gpui::SharedString::from(norte_i18n::t(
                norte_frontend::sync::hint_id(run),
            ))))
            .into_any_element();
    }
    pie.truncate()
        .text_color(palette.dim)
        .child(gpui::SharedString::from(norte_i18n::t(
            norte_frontend::sync::hint_id(run),
        )))
        .into_any_element()
}

/// Una fila del informe: la ruta, la ortografía del destino si difiere, y la
/// causa.
///
/// Cada pieza en su PROPIO elemento accesible, por lo mismo que en una fila de
/// paso: un fichero llamado `informe: permiso denegado.txt` es legal y llega
/// sin badge, así que unido en banda fingiría el veredicto de la fila. El
/// nombre accesible de la FILA es la causa —lo único que no es un nombre de
/// fichero—, y cada ruta lleva el suyo, con el ancla PEGADA a la que califica.
fn render_failure_row(f: &FailureText, index: usize, palette: &Palette) -> gpui::AnyElement {
    use gpui::{ParentElement, Styled, prelude::*, px};

    // El id sale de la POSICIÓN en la lista y no de la ruta: un
    // `SyncFailure` no trae id, y dos fallos pueden nombrar el mismo fichero
    // (dos ortografías que se pintan igual) — un id derivado del nombre les
    // daría identidad de elemento compartida.
    // El `qualifier` va PEGADO a la ruta que califica, igual que en una fila de
    // paso: es lo que dice de qué árbol se habla, y a la escucha eso solo se
    // asocia por posición si no se dice aquí.
    let ruta = |p: &PathText, cual: &str, qualifier: &str| {
        gpui::div()
            .id(gpui::SharedString::from(format!(
                "sync-failure-{index}-{cual}"
            )))
            .aria_label(gpui::SharedString::from(path_a11y(p, qualifier)))
            .flex_1()
            .truncate()
            .child(gpui::SharedString::from(p.label.clone()))
    };
    let mut r = gpui::div()
        .id(gpui::SharedString::from(format!("sync-failure-{index}")))
        .role(gpui::Role::ListItem)
        .aria_label(gpui::SharedString::from(f.a11y.clone()))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(sp::S))
        .child(ruta(&f.rel, "rel", &f.anchor));
    if let Some(d) = &f.dest {
        r = r
            .child(gpui::div().flex_none().text_color(palette.dim).child("→"))
            .child(ruta(d, "dest", ""));
    }
    if !f.anchor.is_empty() {
        r = r.child(
            gpui::div()
                .flex_none()
                .text_color(palette.dim)
                .child(gpui::SharedString::from(f.anchor.clone())),
        );
    }
    // La causa en color de error y en `flex_none`: es el veredicto de la fila
    // y una ruta larga no puede expulsarlo.
    r.child(
        gpui::div()
            .flex_none()
            .text_color(palette.bad)
            .child(gpui::SharedString::from(f.cause.clone())),
    )
    .into_any_element()
}

/// Una fila: las tres marcas, la ruta, la ortografía del destino si difiere,
/// de qué raíz cuelga y el tamaño.
fn render_step_row(
    step: &norte_proto::methods::SyncStep,
    selected: bool,
    dest_trash: norte_proto::methods::DestTrash,
    enc: norte_frontend::sync::SyncEncodings,
    palette: &Palette,
    row_h: gpui::Pixels,
) -> gpui::AnyElement {
    use gpui::{ParentElement, Styled, prelude::*, px};

    let text = step_text(step, dest_trash, enc);
    let undo = norte_frontend::sync::step_undo(step, dest_trash);
    // Cada ruta es su propio nodo accesible, con su propio nombre: así el
    // nombre de un fichero no queda pegado a las palabras que dicen la clase
    // del paso, y no puede falsificarlas (auditoría de encoding MAJOR-2 de
    // C1). Y la del destino no queda pegada a la del origen, que es la otra
    // mitad: la escritura cae sobre la del destino.
    //
    // El recorte es `.truncate()` (por la DERECHA) y no `middle_ellipsis`,
    // igual que las caras del panel de diferencias y por lo mismo: el ancho
    // real de la celda no existe hasta el layout de GPUI, así que no hay
    // presupuesto de celdas que darle. Se acepta a sabiendas, y el riesgo es
    // el que la auditoría nombra (MINOR-2): la cola de una ruta es lo que la
    // identifica, así que dos rutas con prefijo común pueden verse iguales
    // —la cabecera SÍ acota, con `ROOT_MAX_CELLS`, porque allí el presupuesto
    // se puede fijar—. Cerrarlo pide medir en el layout, que pagaría también
    // en el panel de diferencias; queda como deuda, no como cosa hecha.
    let ruta = |p: &PathText, cual: &str, qualifier: &str| {
        gpui::div()
            .id(gpui::SharedString::from(format!(
                "sync-step-{}-{cual}",
                text.id
            )))
            .aria_label(gpui::SharedString::from(path_a11y(p, qualifier)))
            .flex_1()
            .truncate()
            .child(gpui::SharedString::from(p.label.clone()))
    };
    let mut r = gpui::div()
        // El id sale del `id` del PASO (u64, del motor) y no de su posición
        // en la ventana: un `as usize` truncaría en 32 bits y dos pasos
        // distintos compartirían identidad de elemento.
        .id(gpui::SharedString::from(format!("sync-step-{}", text.id)))
        .role(gpui::Role::ListItem)
        .aria_label(text.a11y.clone())
        .aria_selected(selected)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(sp::S))
        .h(row_h)
        .px(px(sp::S))
        .rounded(px(sp::RADIUS_ROW))
        .overflow_hidden()
        .child(
            gpui::div()
                .flex_none()
                .w(px(MARKS_W))
                .text_color(palette.for_undo(undo))
                .child(gpui::SharedString::from(text.marks.clone())),
        )
        .child(ruta(&text.rel, "rel", &text.anchor));
    if let Some(d) = &text.dest {
        // La flecha en su PROPIO elemento: un `→` dentro de un nombre no
        // puede fingir el límite entre las dos ortografías.
        r = r
            .child(gpui::div().flex_none().text_color(palette.dim).child("→"))
            .child(ruta(d, "dest", ""));
    }
    if !text.anchor.is_empty() {
        r = r.child(
            gpui::div()
                .flex_none()
                .text_color(palette.dim)
                .child(gpui::SharedString::from(text.anchor.clone())),
        );
    }
    // `flex_none`: el tamaño es un campo de COLA que no puede ser expulsado
    // por una ruta larga — invariante, no coincidencia de layout.
    if !text.size.is_empty() {
        r = r.child(
            gpui::div()
                .flex_none()
                .text_color(palette.dim)
                .child(gpui::SharedString::from(text.size.clone())),
        );
    }
    if selected {
        r = r.bg(palette.sel_bg);
        if let Some(fg) = palette.sel_fg {
            r = r.text_color(fg);
        }
    }
    r.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::methods::{
        CompareConfidence, CompareCriterion, DestTrash, PlanHash, RelPath, StepReversal,
        SyncCounts, SyncFailureCause, SyncStep, SyncStepKind,
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

    /// Un informe con `done`/`failed` y nada más.
    fn informe(done: u64, failed: u64) -> SyncReportResult {
        SyncReportResult {
            done,
            failed,
            skipped: 0,
            bytes: 0,
            failures: vec![],
            batch_id: Some(11),
        }
    }

    /// UN fallo del informe.
    fn fallo(rel: &str, dest: Option<&str>, cause: SyncFailureCause) -> SyncFailure {
        SyncFailure {
            rel: RelPath::parse_wire(rel).expect("rel"),
            dest_rel: dest.map(|d| RelPath::parse_wire(d).expect("rel")),
            cause,
        }
    }

    /// Los bytes de un fixture de la corpus canónica, por id.
    fn fixture(id: &str) -> Vec<u8> {
        norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|f| f.id == id)
            .unwrap_or_else(|| panic!("la corpus trae {id}"))
            .bytes
    }

    /// Un panel con el plan ya CERRADO por su `sync.plan_done`: es el único
    /// estado en el que hay `plan_hash`, resumen y algo que aprobar.
    fn vista_cerrada() -> SyncView {
        let mut v = vista_de_prueba();
        let steps = pasos(&[1, 2]);
        assert!(v.on_steps(lote(task(), steps.clone())));
        assert!(v.on_plan_done(cierre(task(), &steps)));
        v
    }

    /// Un `DeleteTree` contra un destino SIN papelera: lo que hace que el plan
    /// se gane la SEGUNDA pregunta, y con la frase larga.
    fn borrado() -> SyncStep {
        SyncStep {
            id: 3,
            kind: SyncStepKind::DeleteTree,
            rel: RelPath::parse_wire("viejo").expect("rel"),
            dest_rel: None,
            size: None,
            criterion: CompareCriterion::Presence,
            confidence: CompareConfidence::Certain,
            reversal: Some(StepReversal::Irreversible),
            reason: Some(norte_proto::methods::SyncReason::NoTrashOnTarget),
        }
    }

    /// Un cierre a medida: mismas cuentas que sus pasos, y con `executable` y
    /// `dest_trash` a elección — que son los dos campos que deciden si esto se
    /// aprueba y con qué frase.
    fn cierre_con(
        task_id: TaskId,
        steps: &[SyncStep],
        executable: bool,
        dest_trash: DestTrash,
    ) -> SyncPlanDone {
        let mut done = cierre(task_id, steps);
        done.executable = executable;
        done.dest_trash = dest_trash;
        if !executable {
            done.blockers_total = 3;
        }
        done
    }

    fn vista_con(steps: Vec<SyncStep>, executable: bool, dest_trash: DestTrash) -> SyncView {
        let mut v = vista_de_prueba();
        assert!(v.on_steps(lote(task(), steps.clone())));
        assert!(v.on_plan_done(cierre_con(task(), &steps, executable, dest_trash)));
        v
    }

    /// Un plan que el daemon marcó NO ejecutable: hay bloqueos, así que no se
    /// aprueba pase lo que pase.
    fn vista_bloqueada() -> SyncView {
        vista_con(pasos(&[1, 2]), false, DestTrash::Restorable)
    }

    /// Un plan que BORRA subárboles contra un destino sin papelera: aprobable,
    /// y con la segunda pregunta más larga que este modelo sabe redactar.
    fn vista_que_borra() -> SyncView {
        vista_con(vec![borrado()], true, DestTrash::Absent)
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
            Start::Opened(Some(ClosedTasks {
                plan: task(),
                apply: None
            })),
            "el segundo devuelve la Task del primero para cancelarla"
        );
        let v = hueco.expect("abierto");
        assert_eq!(v.plan_task, otra_task());
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
        assert_eq!(v.plan_task, task());
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

    /// Cada paso lleva sus TRES glifos —clase, confianza y qué devuelve el
    /// undo— porque el veredicto tiene que leerse SIN color. La fase A perdió
    /// el de confianza en el CLI y hubo que reponerlo: es justo la señal de
    /// «esto se sobrescribe fiándose sólo del mtime», en la pantalla donde se
    /// decide borrar un subárbol.
    #[test]
    fn cada_paso_lleva_sus_tres_glifos() {
        let cells = norte_frontend::sync::render_step(
            &paso(1),
            DestTrash::Absent,
            SyncEncodings::default(),
        );
        assert_ne!(cells.glyphs.kind, ' ');
        assert_ne!(cells.glyphs.confidence, ' ');
        assert_ne!(cells.glyphs.undo, ' ');

        // Y los tres llegan al texto de la fila, separados: el alfabeto no es
        // único entre las tres columnas a propósito.
        let t = step_text(&paso(1), DestTrash::Absent, SyncEncodings::default());
        assert_eq!(
            t.marks,
            format!(
                "{} {} {}",
                cells.glyphs.kind, cells.glyphs.confidence, cells.glyphs.undo
            )
        );
    }

    /// El resumen sale de `summary_lines`, no de una segunda redacción: es
    /// donde se dice qué devuelve el undo y de qué papelera se habla.
    #[test]
    fn el_resumen_es_el_compartido() {
        let v = vista_cerrada();
        assert_eq!(
            summary_lines(&v.run),
            v.run
                .state
                .plan()
                .expect("cerrado")
                .summary_lines(norte_i18n::active())
        );
        assert!(
            !summary_lines(&v.run).is_empty(),
            "un plan cerrado dice algo"
        );
    }

    /// Y el pie también: **una sola función en toda la GUI** contesta «¿esto
    /// se puede aprobar?», y es la que contesta en la TUI.
    #[test]
    fn el_pie_es_el_compartido() {
        let v = vista_cerrada();
        assert_eq!(
            status_line(&v.run),
            norte_frontend::sync::status_line(&v.run, norte_i18n::active())
        );
    }

    /// Mientras el plan LLEGA los pasos ya se pintan, y el resumen todavía no
    /// existe: `SyncState::plan()` contesta `None` sin `sync.plan_done`, y
    /// afirmar allí que los dos árboles coinciden sería dar por hecho el
    /// resultado del recorrido que sigue corriendo.
    #[test]
    fn sin_cerrar_hay_pasos_pero_no_resumen() {
        let mut v = vista_de_prueba();
        assert!(v.on_steps(lote(task(), pasos(&[1, 2]))));
        assert_eq!(v.run.steps().len(), 2);
        assert!(summary_lines(&v.run).is_empty());
        assert!(!v.run.can_approve());
    }

    /// El nombre accesible de la FILA no lleva rutas: si las llevara, un
    /// fichero llamado como el veredicto podría hacerse pasar por una fila
    /// entera ante un lector de pantalla (la corrección que la auditoría de
    /// encoding le hizo a C1). Cada ruta va en su propio nodo, con su propio
    /// nombre.
    #[test]
    fn el_nombre_accesible_de_la_fila_no_lleva_rutas() {
        // El nombre sale de la corpus (`score_spoof_inband`), que trae el
        // separador `·` exacto: es el mismo fixture con el que el panel de
        // diferencias afirma esto.
        let nombre = fixture("score_spoof_inband");
        let mut p = paso(1);
        p.rel = RelPath::new(vec![
            norte_proto::Segment::new(nombre.clone()).expect("seg"),
        ]);
        let pintado = String::from_utf8_lossy(&nombre).into_owned();
        let t = step_text(&p, DestTrash::Restorable, SyncEncodings::default());
        assert!(
            !t.a11y.contains(&pintado),
            "la ruta no entra en el nombre de la fila: {}",
            t.a11y
        );
        assert!(t.rel.label.contains('·'), "pero sí se pinta");
        assert_eq!(path_a11y(&t.rel, ""), t.rel.label, "y tiene el suyo propio");
        assert!(!t.a11y.is_empty(), "la fila dice su clase con palabras");
    }

    /// El ancla va PEGADA a la ruta que califica, no suelta al final de la
    /// fila: para un `DeleteTree`, «(destino)» es lo que dice que esa ruta
    /// está en el árbol del que se borra, y a la escucha eso solo se asocia
    /// por posición si no se dice aquí.
    #[test]
    fn el_ancla_viaja_con_la_ruta_que_califica() {
        let mut p = paso(1);
        p.kind = SyncStepKind::DeleteTree;
        p.reversal = Some(StepReversal::RestoreTrash);
        let t = step_text(&p, DestTrash::Restorable, SyncEncodings::default());
        assert_eq!(t.anchor, norte_i18n::t("sync-anchor-dest"));
        let aural = path_a11y(&t.rel, &t.anchor);
        assert!(aural.contains(&t.anchor), "{aural}");
        assert!(aural.contains(&t.rel.label), "{aural}");
    }

    /// Un nombre hostil llega MARCADO a las dos superficies: con el badge de
    /// esta GUI a la vista, y con una PALABRA a la aural — `⚠` no se
    /// pronuncia con la verbosidad de símbolos por defecto de NVDA ni de Orca.
    #[test]
    fn un_nombre_hostil_llega_marcado_a_las_dos_superficies() {
        let mut probados = 0_usize;
        // La corpus ENTERA, como hace el panel de diferencias: un fixture
        // escrito a mano prueba el que se te ocurrió.
        for f in norte_testkit::corpus::hostile_names() {
            let Ok(seg) = norte_proto::Segment::new(f.bytes.clone()) else {
                continue;
            };
            if !norte_frontend::display_name(&f.bytes).1 {
                continue;
            }
            probados += 1;
            // En LAS DOS ortografías: la del origen y la del destino, que es
            // sobre la que cae la escritura.
            let mut p = paso(1);
            p.kind = SyncStepKind::Overwrite;
            p.reversal = Some(StepReversal::RestoreTrash);
            p.rel = RelPath::new(vec![seg.clone()]);
            p.dest_rel = Some(RelPath::new(vec![
                norte_proto::Segment::new(b"otro".to_vec()).expect("seg"),
                seg,
            ]));
            let t = step_text(&p, DestTrash::Restorable, SyncEncodings::default());
            for (cual, ruta) in [("rel", &t.rel), ("dest", t.dest.as_ref().expect("dest"))] {
                assert!(
                    ruta.label.starts_with(crate::HOSTILE_BADGE),
                    "{}/{cual}: llegó sin badge → {:?}",
                    f.id,
                    ruta.label
                );
                let aural = path_a11y(ruta, "");
                assert!(
                    aural.starts_with(&norte_i18n::t("gui-a11y-hostile-name")),
                    "{}/{cual}: la superficie aural no lo dice con palabras → {aural}",
                    f.id
                );
            }
        }
        assert!(probados > 0, "la corpus tiene que traer nombres hostiles");
    }

    /// Y las dos RAÍCES de la cabecera igual: el badge sobrevive a
    /// `title_text` para que la superficie aural pueda decirlo con palabras.
    /// Bajo una reinterpretación activa (#57) el texto sale limpio y legible,
    /// así que el badge es su única marca — y ésta es la línea que nombra el
    /// árbol que se sobrescribe.
    #[test]
    fn las_raices_hostiles_de_la_cabecera_llegan_a_las_dos_superficies() {
        let hostil = VPath::parse("file:///a")
            .expect("vpath")
            .join(norte_proto::Segment::new(fixture("cp866_papka")).expect("seg"));
        let mut v = vista_de_prueba();
        v.run.dest_root = hostil;
        let t = title_text(&v.run);
        assert!(t.dest.hostile, "el bool sobrevive al badge");
        assert!(t.dest.label.starts_with(crate::HOSTILE_BADGE));
        assert!(
            path_a11y(&t.dest, "").starts_with(&norte_i18n::t("gui-a11y-hostile-name")),
            "{}",
            path_a11y(&t.dest, "")
        );
    }

    /// La ortografía del DESTINO (#152) va en su PROPIO campo y no pegada a
    /// la del origen con una flecha dentro de la cadena: `→` es un carácter
    /// imprimible corriente que el saneado no enmascara, así que en banda un
    /// nombre podría fingir la pareja.
    #[test]
    fn las_dos_ortografias_no_se_unen_en_banda() {
        // Con `arrow_join_spoof` (`a → mem_b.txt`), que es imprimible
        // corriente: `display_name_with` no lo enmascara, así que llega SIN
        // badge y en banda fingiría la pareja.
        let mut p = paso(1);
        p.kind = SyncStepKind::Overwrite;
        p.reversal = Some(StepReversal::RestoreTrash);
        p.dest_rel = Some(RelPath::new(vec![
            norte_proto::Segment::new(fixture("arrow_join_spoof")).expect("seg"),
        ]));
        let t = step_text(&p, DestTrash::Restorable, SyncEncodings::default());
        assert_eq!(t.rel.label, "sub/a.txt");
        let dest = t.dest.expect("la otra ortografía");
        assert!(dest.label.contains('→'), "el nombre lleva su flecha");
        assert!(
            !dest.hostile,
            "y llega sin badge, que es lo que lo hace peligroso en banda"
        );
        assert!(
            !t.rel.label.contains('→'),
            "la ruta de origen no se une a la del destino"
        );
    }

    /// Y cuando NO difiere no se pinta dos veces: repetir la misma ruta con
    /// una flecha en medio sugiere un renombrado que no hay.
    #[test]
    fn una_ortografia_identica_no_se_repite() {
        let mut p = paso(1);
        p.dest_rel = Some(RelPath::parse_wire("sub/a.txt").expect("rel"));
        let t = step_text(&p, DestTrash::Restorable, SyncEncodings::default());
        assert!(t.dest.is_none());
    }

    /// Y el pliegue lo decide el MODELO por bytes, no esta capa por el texto
    /// pintado: `lossy_collapse_ff`/`lossy_collapse_fe` son dos ficheros
    /// distintos que se pintan igual, y con el pliegue por texto la ortografía
    /// del destino desaparecía de la fila sin dejar marca.
    #[test]
    fn dos_ortografias_que_se_pintan_igual_siguen_siendo_dos() {
        let seg = |id: &str| norte_proto::Segment::new(fixture(id)).expect("seg");
        let mut p = paso(1);
        p.kind = SyncStepKind::Overwrite;
        p.reversal = Some(StepReversal::RestoreTrash);
        p.rel = RelPath::new(vec![seg("lossy_collapse_ff")]);
        p.dest_rel = Some(RelPath::new(vec![seg("lossy_collapse_fe")]));
        let t = step_text(&p, DestTrash::Restorable, SyncEncodings::default());
        let dest = t.dest.expect("dos ficheros distintos son dos ortografías");
        assert_eq!(dest.label, t.rel.label, "y se pintan igual");
    }

    /// Las dos raíces de la cabecera tampoco se unen: cada una en su cadena,
    /// y la `→` que dice el SENTIDO es un elemento aparte. Un directorio
    /// llamado `docs → backup/home/victima` es legal y llega sin badge.
    #[test]
    fn las_dos_raices_de_la_cabecera_no_se_unen_en_banda() {
        let v = vista_de_prueba();
        let t = title_text(&v.run);
        assert!(!t.source.label.contains('→'));
        assert!(!t.dest.label.contains('→'));
        assert!(t.source.label.contains("origen"));
        assert!(t.dest.label.contains("destino"));
        assert!(!t.mode.is_empty(), "el modo se ve antes de aprobar");
    }

    /// El primer `Esc` sobre una Task viva la cancela y conserva los pasos;
    /// cualquiera posterior cierra pase lo que pase con la Task — un canal
    /// que no llega a cerrarse nunca no puede dejar encerrado al lector. Y
    /// sigue valiendo con la segunda pregunta puesta: cerrar no es una
    /// respuesta a «¿seguro?».
    #[test]
    fn el_primer_esc_cancela_y_el_segundo_cierra_pase_lo_que_pase() {
        assert_eq!(
            key_meaning("escape", false, true, false, false, false),
            Key::CancelTask
        );
        assert_eq!(
            key_meaning("escape", false, true, true, false, false),
            Key::Close
        );
        assert_eq!(
            key_meaning("escape", false, false, false, false, false),
            Key::Close
        );
        assert_eq!(
            key_meaning("escape", false, false, false, true, false),
            Key::Close,
            "con la pregunta puesta también se sale"
        );
    }

    /// Un modificador no significa nada aquí — salvo con la pregunta puesta,
    /// que es justo el orden que la TUI documenta como corrección: con el
    /// filtro delante, un `Ctrl+r` de costumbre caía en `Ignore` y dejaba «se
    /// van a borrar 2 árboles… ¿Seguir?» armada, esperando un `y` que ya no
    /// sabe a qué contesta.
    #[test]
    fn un_modificador_no_significa_nada_salvo_con_la_pregunta_puesta() {
        assert_eq!(
            key_meaning("down", true, true, false, false, false),
            Key::Ignore
        );
        assert_eq!(
            key_meaning("down", false, false, false, false, false),
            Key::Move(1)
        );
        assert_eq!(
            key_meaning("pageup", false, false, false, false, false),
            Key::Move(-10)
        );
        assert_eq!(
            key_meaning("r", true, false, false, true, false),
            Key::ConfirmNo,
            "una tecla modificada RESUELVE la pregunta en vez de ignorarse"
        );
    }

    /// `a` aprueba y `y` confirma, y NADA más: `Enter` no aprueba, porque
    /// sincronizar borra y sobrescribe y se pide una tecla que nadie pulsa por
    /// inercia (mismo criterio que los diálogos TOFU).
    #[test]
    fn la_a_aprueba_la_y_confirma_y_enter_no() {
        assert_eq!(
            key_meaning("a", false, false, false, false, false),
            Key::Approve
        );
        assert_eq!(
            key_meaning("enter", false, false, false, false, false),
            Key::Ignore,
            "`Enter` no aprueba un borrado"
        );
        assert_eq!(
            key_meaning("y", false, false, false, true, false),
            Key::ConfirmYes
        );
        assert_eq!(
            key_meaning("enter", false, false, false, true, false),
            Key::ConfirmNo,
            "y tampoco lo confirma: cualquier otra tecla retira la pregunta"
        );
        assert_eq!(
            key_meaning("y", true, false, false, true, false),
            Key::ConfirmNo,
            "la `y` con modificador no es la respuesta"
        );
        assert_eq!(
            key_meaning("a", false, false, false, true, false),
            Key::ConfirmNo,
            "y con la pregunta puesta ni siquiera `a` significa aprobar"
        );
    }

    /// Soltar el panel devuelve SIEMPRE su Task, sin mirar si sigue viva: un
    /// `task.cancel` sobre una terminada es un no-op, y mirar antes es una
    /// condición que un día se evalúa mal sobre algo que recorre —o borra— dos
    /// árboles para un panel que ya no existe (regla dura 3). Es la decisión
    /// que comparten el `Esc` del panel y la apertura del visor, que lo
    /// excluye; vive aquí porque `main.rs` no se puede testear.
    #[test]
    fn soltar_el_panel_devuelve_siempre_su_task() {
        let mut hueco: Option<SyncView> = None;
        assert_eq!(
            close(&mut hueco),
            None,
            "sin panel no hay nada que cancelar"
        );

        let mut hueco = Some(vista_de_prueba());
        assert_eq!(
            close(&mut hueco),
            Some(ClosedTasks {
                plan: task(),
                apply: None
            })
        );
        assert!(hueco.is_none());

        // Con la Task ya terminada, IGUAL: el desenlace no cambia la decisión.
        let mut v = vista_de_prueba();
        assert!(v.on_plan_ended(task(), &TaskState::Completed).is_none());
        let mut hueco = Some(v);
        assert_eq!(
            close(&mut hueco),
            Some(ClosedTasks {
                plan: task(),
                apply: None
            })
        );
    }

    // -----------------------------------------------------------------
    // Aprobar y aplicar (tarea 4)
    // -----------------------------------------------------------------

    /// **Un plan que NO se puede aprobar no llega a preguntar.** La fase A
    /// shipeó lo contrario: `confirmation()` devuelve `None` cuando
    /// `!can_approve()`, así que los planes MENOS fiables recibían el aviso
    /// MÁS CORTO —un `mirror` bloqueado salía con un sí/no pelado y sin la
    /// frase de «esto borra N árboles»— y luego se aplicaban enteros desde el
    /// spool. Aquí el gate va primero y lo que sale es una EXPLICACIÓN.
    #[test]
    fn un_plan_no_aprobable_no_pregunta_y_se_explica() {
        let mut v = vista_bloqueada();
        assert!(!v.run.can_approve());
        assert_eq!(
            approve(&mut v),
            Approve::Refused(norte_i18n::t("msg-sync-cannot-approve"))
        );
        assert!(
            v.run.confirming.is_none(),
            "no queda ninguna pregunta armada"
        );

        // Y el PORQUÉ está en pantalla, que es la otra mitad de «se explica»:
        // el resumen dice cuántos bloqueos lo paran, y el pie que no se puede
        // aprobar.
        let resumen = summary_lines(&v.run);
        assert!(
            resumen.iter().any(|l| l.contains('3')),
            "el resumen nombra los bloqueos: {resumen:?}"
        );
        assert_eq!(
            status_line(&v.run),
            norte_i18n::ta("sync-status-not-approvable", &[("n", "2")])
        );
    }

    /// Un plan aprobable que BORRA pregunta con la frase COMPARTIDA, la que
    /// cuenta los borrados y dice si van a la papelera. Aquí no se redacta una
    /// segunda frase sobre el borrado.
    #[test]
    fn un_plan_que_borra_pregunta_con_la_frase_compartida() {
        let mut v = vista_que_borra();
        assert!(v.run.can_approve());
        let esperada = v
            .run
            .state
            .plan()
            .expect("cerrado")
            .confirmation(norte_i18n::active())
            .expect("un plan que borra sin papelera merece la segunda pregunta");

        assert_eq!(approve(&mut v), Approve::Asked, "pregunta, no manda");
        assert_eq!(v.run.confirming.as_ref(), Some(&esperada));
        assert!(
            esperada.text.contains('1'),
            "y la frase cuenta los árboles: {}",
            esperada.text
        );
    }

    /// Un `Update` que se deshace del todo NO pregunta dos veces: preguntar
    /// por lo rutinario enseña a saltarse las dos preguntas. Va directo al
    /// hash, y el hash es el del plan que se enseñó.
    #[test]
    fn un_plan_rutinario_no_pregunta_dos_veces() {
        let mut v = vista_cerrada();
        let hash = v
            .run
            .state
            .plan()
            .expect("cerrado")
            .done()
            .plan_hash
            .clone();
        assert_eq!(approve(&mut v), Approve::Submit(Box::new(hash)));
        assert!(v.run.confirming.is_none());
    }

    /// **Aplicar dos veces el mismo plan no puede pasar**, y lo sabe el estado
    /// compartido aunque el `SyncPlan` de dentro siga contestando que sí
    /// —`SyncState::plan()` lo sigue entregando en `Applying`, y ninguno de sus
    /// tres factores cambia al gastarse—.
    #[test]
    fn no_se_aplica_dos_veces() {
        let mut v = vista_cerrada();
        let Approve::Submit(hash) = approve(&mut v) else {
            panic!("la primera da el hash");
        };
        assert!(
            v.run
                .state
                .plan()
                .expect("sigue habiendo plan")
                .can_approve(),
            "el plan de dentro seguiría diciendo que sí: por eso no se le pregunta a él"
        );

        assert!(v.on_apply_started(otra_task()), "el modelo la adopta");
        assert_eq!(
            v.apply_task,
            Some(otra_task()),
            "y el panel pasa a cancelar ÉSA además del plan"
        );
        // #191: el plan sigue nombrado — antes esto se perdía al reasignar el
        // único campo que existía.
        assert_eq!(v.plan_task, task(), "y NO se pierde la Task del plan");
        assert!(!v.run.can_approve());
        assert_eq!(
            approve(&mut v),
            Approve::Refused(norte_i18n::t("msg-sync-cannot-approve")),
            "la segunda, nada"
        );
        // Y por la otra puerta tampoco: la `y` vuelve a preguntar.
        v.run.confirming = Some(norte_frontend::sync::Confirmation {
            id: "sync-confirm-delete",
            text: "¿seguro?".to_owned(),
        });
        assert_eq!(
            confirm_yes(&mut v),
            Approve::Refused(norte_i18n::t("msg-sync-cannot-approve"))
        );
        assert!(v.run.confirming.is_none(), "y la pregunta se retira igual");
        drop(hash);
    }

    /// **El BLOCKER que las dos revisiones encontraron**: `SyncState` no sale
    /// de `Ready` hasta que llega el EVENTO `SyncApplyStarted`, o sea una
    /// vuelta entera al daemon después de que la tecla mandara el comando.
    /// Entre las dos cosas el pie sigue ofreciendo `a` y `can_approve()` sigue
    /// diciendo que sí, así que dos pulsaciones mandaban DOS `sync.apply` del
    /// mismo hash — el segundo vuelve `PlanStale` y su negativa marcaba
    /// «falló» un panel que estaba BORRANDO.
    #[test]
    fn una_segunda_a_antes_de_la_respuesta_no_manda_nada() {
        let mut v = vista_cerrada();
        assert!(matches!(approve(&mut v), Approve::Submit(_)));
        assert!(v.run.is_submitted(), "queda el pestillo");
        assert!(
            matches!(v.run.state, norte_frontend::sync::SyncState::Ready(_)),
            "y el modelo SIGUE en Ready: el daemon no ha contestado"
        );
        // Antes de la revisión de rama de C2 este test asertaba aquí
        // `can_approve()`, porque el pestillo vivía en `norte-gui` y el modelo
        // compartido no podía verlo. Que ahora conteste que NO es el arreglo:
        // `hint_id` y `status_line` viven en ese crate y pintaban «a aprobar»
        // sobre un plan que `approve` ya rechazaba.
        assert!(
            !v.run.can_approve(),
            "y con el pestillo echado la respuesta compartida ya es NO"
        );
        assert_eq!(
            approve(&mut v),
            Approve::Refused(norte_i18n::t("msg-sync-cannot-approve")),
            "la segunda `a` no manda un segundo sync.apply"
        );

        // Y por la puerta de la segunda pregunta, igual.
        let mut v = vista_que_borra();
        assert_eq!(approve(&mut v), Approve::Asked);
        assert!(matches!(confirm_yes(&mut v), Approve::Submit(_)));
        v.run.confirming = Some(norte_frontend::sync::Confirmation {
            id: "sync-confirm-delete",
            text: "¿seguro?".to_owned(),
        });
        assert_eq!(
            confirm_yes(&mut v),
            Approve::Refused(norte_i18n::t("msg-sync-cannot-approve"))
        );
    }

    /// Revisión de seguridad MAJOR-1: el `Esc` entre la tecla y la respuesta
    /// del daemon. En esa ventana `run` todavía es `Done` —`Applying` no llega
    /// hasta que el daemon devuelve la Task—, así que `Esc` resolvía a `Close`:
    /// el panel se cerraba, el informe llegaba a un hueco vacío y se tiraba, y
    /// el lector se quedaba creyendo que no había empezado nada sobre un
    /// destino que ya se estaba reescribiendo.
    #[test]
    fn el_esc_con_un_apply_en_vuelo_cancela_en_vez_de_cerrar() {
        assert_eq!(
            key_meaning("escape", false, false, false, false, true),
            Key::CancelTask,
            "hay un sync.apply volando: Esc PIDE PARAR, no cierra"
        );
        assert_eq!(
            key_meaning("escape", false, false, true, false, true),
            Key::Close,
            "el segundo Esc cierra igual, como en el resto del panel"
        );
        assert_eq!(
            key_meaning("escape", false, false, false, false, false),
            Key::Close,
            "sin nada en vuelo se cierra, que es lo que siempre hizo"
        );
    }

    /// Y el informe de un apply que se quedó sin panel se DICE. Tirarlo era
    /// perder el único registro de lo que una tarea que escribe llegó a hacer,
    /// y de que hay un lote de journal que deshacerlo.
    #[test]
    fn un_informe_sin_panel_no_se_tira() {
        let frase = orphan_report_banner(&Ok(informe(7, 2)));
        assert!(frase.contains('7') && frase.contains('2'), "{frase}");
    }

    /// Y la otra mitad de la corrección: una negativa NO puede describir una
    /// aplicación que ya arrancó. Si lo hiciera, apagaría [`is_running`] y el
    /// siguiente `Esc` sería un CIERRE —que cancela la aplicación viva— en vez
    /// de una cancelación pedida.
    #[test]
    fn una_negativa_no_describe_una_aplicacion_ya_arrancada() {
        let mut v = vista_cerrada();
        assert!(matches!(approve(&mut v), Approve::Submit(_)));
        assert!(v.on_apply_started(otra_task()));
        let mut hueco = Some(v);
        assert!(
            on_apply_failed(&mut hueco, 1, 1, &norte_proto::Error::PlanStale).is_none(),
            "el PlanStale del duplicado no toca la aplicación viva"
        );
        let v = hueco.expect("abierto");
        assert_eq!(v.run.run, SyncRunState::Running, "sigue aplicándose");
        assert!(is_running(&v.run), "y el Esc sigue significando CANCELAR");
    }

    /// Un `Esc` entre la tecla que aprueba y la respuesta del daemon PARA la
    /// aplicación: quien pulsó `Esc` pidió parar, y esta pantalla escribe en el
    /// disco de alguien. El `on_apply_started` compartido borra
    /// `cancel_requested`, así que sin el guard la aplicación arrancaba igual.
    #[test]
    fn un_esc_entre_aprobar_y_la_respuesta_cancela_la_aplicacion() {
        let mut v = vista_cerrada();
        assert!(matches!(approve(&mut v), Approve::Submit(_)));
        v.run.cancel_requested = true; // lo que hace `Key::CancelTask`
        let mut hueco = Some(v);
        assert_eq!(
            on_apply_start(&mut hueco, 1, 1, otra_task()),
            ApplyStart::Orphan(otra_task()),
            "la Task se cancela en vez de adoptarse"
        );
        let v = hueco.expect("abierto");
        assert_eq!(v.plan_task, task(), "el plan sigue siendo el mismo");
        assert_eq!(v.apply_task, None, "y el panel no se queda la que borra");
        assert!(
            !v.run.is_submitted(),
            "el pestillo se suelta: la petición se resolvió"
        );
    }

    /// La `y` manda el MISMO hash que se aprobó, y retira la pregunta. Una
    /// cualquiera la retira sin mandar nada: dejarla puesta mientras el cursor
    /// se mueve por debajo es cómo un `y` posterior aprueba otra cosa.
    #[test]
    fn la_y_manda_el_hash_aprobado_y_cualquier_otra_lo_cancela() {
        let mut v = vista_que_borra();
        let hash = v
            .run
            .state
            .plan()
            .expect("cerrado")
            .done()
            .plan_hash
            .clone();
        assert_eq!(approve(&mut v), Approve::Asked);
        assert_eq!(confirm_yes(&mut v), Approve::Submit(Box::new(hash)));
        assert!(v.run.confirming.is_none());

        let mut v = vista_que_borra();
        assert_eq!(approve(&mut v), Approve::Asked);
        confirm_no(&mut v);
        assert!(v.run.confirming.is_none(), "y no se mandó nada");
    }

    /// Una Task de aplicación que NINGÚN panel adopta se cancela: es la regla
    /// 3, y aquí no es formalismo — una aplicación suelta es un `Mirror`
    /// borrando sin nadie que lo vea ni lo pueda parar.
    ///
    /// Tres formas de llegar: la petición vencida, el hueco vacío, y el modelo
    /// que la rechaza (el plan ya se gastó, o este panel es de otro plan).
    #[test]
    fn una_aplicacion_que_nadie_adopta_se_cancela() {
        let mut hueco = Some(vista_cerrada());
        assert_eq!(
            on_apply_start(&mut hueco, 2, 1, otra_task()),
            ApplyStart::Orphan(otra_task()),
            "la generación 1 ya está superada: el lector pidió otro plan"
        );
        assert_eq!(
            hueco.as_ref().expect("intacto").apply_task,
            None,
            "y el panel vencido no se queda la Task de la aplicación"
        );

        let mut vacio: Option<SyncView> = None;
        assert_eq!(
            on_apply_start(&mut vacio, 1, 1, otra_task()),
            ApplyStart::Orphan(otra_task())
        );

        // Un panel que TODAVÍA planifica: el modelo no está en `Ready`, así
        // que no la acepta.
        let mut planificando = Some(vista_de_prueba());
        assert_eq!(
            on_apply_start(&mut planificando, 1, 1, otra_task()),
            ApplyStart::Orphan(otra_task())
        );
        assert_eq!(
            planificando.expect("abierto").apply_task,
            None,
            "y no se le pone `apply_task` a un panel que no la adoptó"
        );

        // Y el caso feliz: adoptada, y el panel pasa a cancelar ESA además
        // del plan.
        let mut hueco = Some(vista_cerrada());
        assert_eq!(
            on_apply_start(&mut hueco, 1, 1, otra_task()),
            ApplyStart::Adopted
        );
        let v = hueco.expect("abierto");
        assert_eq!(v.apply_task, Some(otra_task()));
        assert_eq!(v.plan_task, task(), "y el plan sigue nombrado (#191)");
    }

    /// Cerrar a media aplicación CANCELA la aplicación Y el plan (#191): las
    /// dos Tasks pueden estar vivas a la vez, y antes de #191 solo una de las
    /// dos quedaba nombrada — la que se REASIGNABA a `apply_task` borraba el
    /// único campo que existía, y `close` solo podía devolver esa.
    #[test]
    fn cerrar_a_media_aplicacion_cancela_la_aplicacion() {
        let mut v = vista_cerrada();
        assert!(v.on_plan_ended(task(), &TaskState::Completed).is_none());
        assert!(v.on_apply_started(otra_task()));
        let mut hueco = Some(v);
        assert_eq!(
            close(&mut hueco),
            Some(ClosedTasks {
                plan: task(),
                apply: Some(otra_task()),
            }),
            "se cancela la Task que ESCRIBE Y la del plan, aunque su canal \
             siga en vuelo (#191)"
        );

        // Y el `Esc` no cierra a la primera: pide la cancelación y deja el
        // panel abierto, así que el camino corto no cierra nada por descuido.
        assert_eq!(
            key_meaning("escape", false, true, false, false, false),
            Key::CancelTask
        );
    }

    /// El final de la aplicación trae el informe, y el panel lo pinta. Con la
    /// Task cancelada TAMBIÉN: lo aplicado hasta el corte se queda
    /// journalizado, y media sincronización es un estado real.
    #[test]
    fn el_final_de_la_aplicacion_mete_el_informe() {
        let mut v = vista_cerrada();
        assert!(v.on_apply_started(otra_task()));
        assert!(
            v.on_apply_ended(otra_task(), &TaskState::Cancelled, Ok(informe(4, 1)))
                .is_none(),
            "una cancelación no es un error que pintar en el banner"
        );
        assert_eq!(v.run.run, SyncRunState::Cancelled);
        let r = report(&v.run).expect("el informe entró");
        assert_eq!((r.done, r.failed), (4, 1));
    }

    /// Un final de OTRA Task no toca la aplicación — y el que llega es el del
    /// canal del PLAN, todavía en vuelo: sin este contraste apagaría el
    /// `Running` de la aplicación con un «hecho» que habla de otra cosa.
    #[test]
    fn el_final_del_plan_no_apaga_la_aplicacion() {
        let mut v = vista_cerrada();
        assert!(v.on_apply_started(otra_task()));
        assert_eq!(v.run.run, SyncRunState::Running);
        assert!(
            v.on_plan_ended(task(), &TaskState::Completed).is_none(),
            "el final del PLAN ya no es de este panel"
        );
        assert_eq!(v.run.run, SyncRunState::Running, "sigue aplicándose");
        assert!(
            v.on_apply_ended(task(), &TaskState::Completed, Ok(informe(1, 0)))
                .is_none()
        );
        assert!(
            report(&v.run).is_none(),
            "y un informe de otra Task tampoco entra"
        );
    }

    /// **Sin informe no se dice que terminó bien.** `sync.report` es lo único
    /// que dice cuánto se llegó a escribir; si no se pudo pedir, el desenlace
    /// es fallo con la categoría de ESE error, aunque la Task dijera
    /// `Completed`. La TUI se quedaba aquí en «aplicando…» para siempre.
    #[test]
    fn sin_informe_no_se_dice_que_termino_bien() {
        let mut v = vista_cerrada();
        assert!(v.on_apply_started(otra_task()));
        let categoria = v
            .on_apply_ended(
                otra_task(),
                &TaskState::Completed,
                Err(norte_proto::Error::NotFound),
            )
            .expect("un informe que no llega es un fallo que decir");
        assert_eq!(v.run.run, SyncRunState::Failed);
        assert_eq!(v.run.error.as_deref(), Some(categoria.as_str()));
        assert!(report(&v.run).is_none());
        assert!(
            status_line(&v.run).contains(&categoria),
            "y el pie lo dice: {}",
            status_line(&v.run)
        );
    }

    /// El error de la TASK manda sobre el del informe: es el que dice por qué
    /// se paró.
    #[test]
    fn el_error_de_la_task_manda_sobre_el_del_informe() {
        let mut v = vista_cerrada();
        assert!(v.on_apply_started(otra_task()));
        let categoria = v
            .on_apply_ended(
                otra_task(),
                &TaskState::Failed {
                    error: norte_proto::Error::PermissionDenied,
                },
                Ok(informe(0, 0)),
            )
            .expect("un fallo trae su categoría");
        assert_eq!(
            categoria,
            norte_frontend::error::error_category(&norte_proto::Error::PermissionDenied)
        );
        assert_eq!(v.run.run, SyncRunState::Failed);
    }

    /// Un `sync.apply` RECHAZADO deja el panel abierto y lo marca: un banner
    /// mientras el pie sigue ofreciendo aprobar es la pantalla
    /// contradiciéndose. Y la negativa SUPERADA no toca nada, mismo guard que
    /// la del plan.
    #[test]
    fn una_aplicacion_rechazada_marca_el_panel_y_la_superada_no() {
        let e = norte_proto::Error::Unsupported;
        let mut hueco = Some(vista_cerrada());
        assert!(
            on_apply_failed(&mut hueco, 5, 4, &e).is_none(),
            "la superada no describe una petición que el lector ya reemplazó"
        );
        assert!(
            hueco.as_ref().expect("abierto").run.can_approve(),
            "y no le quita la aprobación a un panel que sigue vigente"
        );

        let (pane, frase) = on_apply_failed(&mut hueco, 4, 4, &e).expect("la vigente sí");
        assert_eq!(pane, 0);
        assert_eq!(
            frase,
            failure_banner(&norte_frontend::error::error_category(&e)),
            "la MISMA frase que la negativa del plan, no una segunda redacción"
        );
        let v = hueco.expect("el panel sigue abierto");
        assert_eq!(v.run.run, SyncRunState::Failed);
        assert!(
            !v.run.can_approve(),
            "no se sabe si el plan sigue en el spool: no se vuelve a ofrecer"
        );
    }

    /// Los fallos del informe se enseñan UNO POR LÍNEA, con su causa en
    /// palabras y en el idioma del lector — la MISMA tabla que imprime el CLI.
    #[test]
    fn los_fallos_del_informe_van_uno_por_linea() {
        let mut v = vista_cerrada();
        assert!(v.on_apply_started(otra_task()));
        let mut r = informe(1, 2);
        r.failures = vec![
            fallo("a.txt", None, SyncFailureCause::Denied),
            fallo("b.txt", Some("otro/b.txt"), SyncFailureCause::IllegalName),
        ];
        assert!(
            v.on_apply_ended(otra_task(), &TaskState::Completed, Ok(r))
                .is_none()
        );

        let filas = failures(&v.run);
        assert_eq!(filas.len(), 2, "una por fallo");
        assert_eq!(filas[0].rel.label, "a.txt");
        assert!(filas[0].dest.is_none());
        assert_eq!(
            filas[0].cause,
            norte_frontend::sync::failure_cause_label(
                SyncFailureCause::Denied,
                norte_i18n::active()
            )
        );
        assert_eq!(
            filas[1].dest.as_ref().expect("la otra ortografía").label,
            "otro/b.txt"
        );
        assert_ne!(filas[0].cause, filas[1].cause, "cada causa es la suya");
        assert_eq!(failures_hidden(&v.run), 0);
    }

    /// El panel pinta como mucho [`FAILURES_SHOWN`] filas —la lista no está
    /// virtualizada y vive en un `flex_col` recortado—, y lo que no pinta lo
    /// CUENTA: perder filas en silencio es lo único que no se puede hacer.
    #[test]
    fn los_fallos_que_no_caben_se_cuentan() {
        let mut v = vista_cerrada();
        assert!(v.on_apply_started(otra_task()));
        let n = FAILURES_SHOWN + 5;
        let mut r = informe(0, u64::try_from(n).expect("cabe"));
        r.failures = (0..n)
            .map(|i| fallo(&format!("f{i}.txt"), None, SyncFailureCause::Io))
            .collect();
        assert!(
            v.on_apply_ended(otra_task(), &TaskState::Completed, Ok(r))
                .is_none()
        );
        assert_eq!(failures(&v.run).len(), FAILURES_SHOWN);
        assert_eq!(failures_hidden(&v.run), 5);
    }

    /// `sync.report` recorta la lista a 256 y cuenta `failed` sin tope, así
    /// que lo que no se lista se DICE: si no, 256 filas pasarían por el total.
    #[test]
    fn los_fallos_que_no_se_listan_se_cuentan() {
        let mut v = vista_cerrada();
        assert!(v.on_apply_started(otra_task()));
        let mut r = informe(0, 40_000);
        r.failures = vec![fallo("a.txt", None, SyncFailureCause::Io)];
        assert!(
            v.on_apply_ended(otra_task(), &TaskState::Completed, Ok(r))
                .is_none()
        );
        assert_eq!(failures_hidden(&v.run), 39_999);
    }

    /// Un nombre hostil en un FALLO llega marcado a las dos superficies,
    /// exactamente igual que en un paso: el informe se lee sin el plan
    /// delante, así que es la única vez que ese nombre se ve.
    ///
    /// La corpus ENTERA, como en el test gemelo de los pasos: un fixture
    /// escrito a mano prueba el que se te ocurrió.
    #[test]
    fn un_fallo_con_nombre_hostil_llega_marcado() {
        let mut probados = 0_usize;
        for fx in norte_testkit::corpus::hostile_names() {
            let Ok(seg) = norte_proto::Segment::new(fx.bytes.clone()) else {
                continue;
            };
            if !norte_frontend::display_name(&fx.bytes).1 {
                continue;
            }
            probados += 1;
            let f = SyncFailure {
                rel: RelPath::new(vec![seg.clone()]),
                dest_rel: Some(RelPath::new(vec![
                    norte_proto::Segment::new(b"otro".to_vec()).expect("seg"),
                    seg,
                ])),
                cause: SyncFailureCause::Conflict,
            };
            let t = failure_text(&f, SyncEncodings::default());
            for (cual, ruta) in [("rel", &t.rel), ("dest", t.dest.as_ref().expect("dest"))] {
                assert!(
                    ruta.label.starts_with(crate::HOSTILE_BADGE),
                    "{}/{cual}: llegó sin badge → {:?}",
                    fx.id,
                    ruta.label
                );
                let aural = path_a11y(ruta, "");
                assert!(
                    aural.starts_with(&norte_i18n::t("gui-a11y-hostile-name")),
                    "{}/{cual}: la superficie aural no lo dice con palabras → {aural}",
                    fx.id
                );
            }
        }
        assert!(probados > 0, "la corpus tiene que traer nombres hostiles");
    }

    /// Y la CAUSA no se pega a la ruta, ni ante la vista ni al oído: el
    /// fixture `cause_join_spoof` (`informe :→ copia.txt: permission denied`)
    /// lleva LOS DOS joiners de una fila de fallo, es imprimible corriente —
    /// así que llega SIN badge— y en banda imprimiría una fila entera
    /// fabricada después de un `Mirror` destructivo. El separador es
    /// ESTRUCTURAL, como en las filas de paso.
    #[test]
    fn la_causa_de_un_fallo_no_se_pega_a_la_ruta() {
        let nombre = fixture("cause_join_spoof");
        let pintado = String::from_utf8_lossy(&nombre).into_owned();
        let f = SyncFailure {
            rel: RelPath::new(vec![norte_proto::Segment::new(nombre).expect("seg")]),
            dest_rel: None,
            cause: SyncFailureCause::Denied,
        };
        let t = failure_text(&f, SyncEncodings::default());
        assert!(!t.rel.hostile, "es imprimible corriente: llega sin badge");
        assert_eq!(t.rel.label, pintado, "y se pinta tal cual, con sus joiners");
        // El nombre accesible de la FILA es la causa y NADA más: con la ruta
        // dentro, este fichero se leería como una fila completa cuyo veredicto
        // lo elige quien lo nombró.
        assert_eq!(t.a11y, t.cause);
        assert!(!t.a11y.contains(&pintado), "{}", t.a11y);
        // Y la ruta tiene su propio nombre, con el ancla y NADA más pegado:
        // la igualdad exacta es lo que lo prueba, porque el `contains` no
        // sirve — el nombre del fichero LLEVA DENTRO las palabras de la causa,
        // que es justo lo que lo hace peligroso.
        assert_eq!(
            path_a11y(&t.rel, &t.anchor),
            format!("{pintado} {}", t.anchor),
            "a la ruta no se le pega nada que el lector pueda tomar por veredicto"
        );
    }

    /// El ancla de un fallo SE PINTA. En este panel una ruta sin calificar
    /// significa «del origen» —así lo dicen las filas de paso, veinte líneas
    /// más arriba y en la misma pantalla—, así que callar un ancla que no
    /// consta es afirmar el origen. Y el informe SÍ trae una prueba cuando
    /// manda `dest_rel`: entonces `rel` es la mitad del origen y no hay nada
    /// que calificar (auditoría de encoding MAJOR-2 y MINOR-1).
    #[test]
    fn el_ancla_de_un_fallo_se_dice_cuando_no_consta() {
        let sin_prueba = failure_text(
            &fallo("viejo", None, SyncFailureCause::Denied),
            SyncEncodings::default(),
        );
        assert_eq!(
            sin_prueba.anchor,
            norte_i18n::t("sync-anchor-either"),
            "un DeleteTree que falla por permisos habla del DESTINO y el informe no lo dice"
        );
        let aural = path_a11y(&sin_prueba.rel, &sin_prueba.anchor);
        assert!(aural.contains(&sin_prueba.anchor), "{aural}");

        let con_prueba = failure_text(
            &fallo("a.txt", Some("otro/a.txt"), SyncFailureCause::Io),
            SyncEncodings::default(),
        );
        assert!(
            con_prueba.anchor.is_empty(),
            "con `dest_rel`, `rel` es la mitad del origen y no hay nada que calificar"
        );
    }

    /// Cada ruta de un fallo se lee con la reinterpretación del lado que le
    /// toca (#152): la del DESTINO con la del destino, que es sobre la que
    /// cayó la escritura. Los dos campos son `Option<NameEncoding>`, así que
    /// trasponerlos COMPILA — el compilador no ayuda aquí y este test es lo
    /// único que lo guarda (auditoría de encoding MINOR-4).
    #[test]
    fn cada_ruta_de_un_fallo_se_lee_con_su_reinterpretacion() {
        let origen = norte_encoding::NameEncoding::Cp437;
        let destino = norte_encoding::name_reinterpret_cycle()
            .iter()
            .copied()
            .find(|e| e.label() != origen.label())
            .expect("el ciclo trae más de una");
        let bytes = fixture("cp866_papka");
        let seg = norte_proto::Segment::new(bytes.clone()).expect("seg");
        let f = SyncFailure {
            rel: RelPath::new(vec![seg.clone()]),
            dest_rel: Some(RelPath::new(vec![
                norte_proto::Segment::new(b"otro".to_vec()).expect("seg"),
                seg,
            ])),
            cause: SyncFailureCause::IllegalName,
        };
        let t = failure_text(
            &f,
            SyncEncodings {
                source: Some(origen),
                dest: Some(destino),
            },
        );
        let esperado_origen = norte_frontend::display_name_with(&bytes, Some(origen)).0;
        let esperado_destino = norte_frontend::display_name_with(&bytes, Some(destino)).0;
        assert_ne!(
            esperado_origen, esperado_destino,
            "el fixture tiene que distinguir los dos codepages"
        );
        assert!(t.rel.label.ends_with(&esperado_origen), "{:?}", t.rel);
        assert!(
            t.dest
                .expect("la otra ortografía")
                .label
                .ends_with(&esperado_destino),
            "la del DESTINO se lee con la del destino, que es donde cayó la escritura"
        );
    }

    /// Las causas se resuelven de verdad contra el catálogo: `norte_i18n::t`
    /// cae al id crudo sin panic, así que un id mal escrito se enviaría como
    /// la literal `sync-cause-denied` en pantalla, y el test de paridad entre
    /// locales solo prueba que en y es coinciden — no que la clave exista
    /// (auditoría de encoding MINOR-7).
    #[test]
    fn las_causas_estan_traducidas_de_verdad() {
        for c in [
            SyncFailureCause::Conflict,
            SyncFailureCause::Denied,
            SyncFailureCause::IllegalName,
            SyncFailureCause::Io,
            SyncFailureCause::Unknown,
        ] {
            let label = norte_frontend::sync::failure_cause_label(c, norte_i18n::active());
            assert!(!label.starts_with("sync-cause-"), "sin traducir: {label}");
            assert!(!label.is_empty());
        }
    }
    /// Y la ortografía del destino de un fallo se pliega por BYTES cuando
    /// coincide, igual que la de un paso: repetir la misma ruta con una flecha
    /// en medio sugiere un renombrado que no hay.
    #[test]
    fn una_ortografia_identica_de_un_fallo_no_se_repite() {
        let f = fallo("sub/a.txt", Some("sub/a.txt"), SyncFailureCause::Io);
        assert!(failure_text(&f, SyncEncodings::default()).dest.is_none());
    }

    /// Y el pliegue es por BYTES y no por el texto PINTADO:
    /// `lossy_collapse_ff`/`lossy_collapse_fe` son dos ficheros distintos que
    /// se pintan igual, y comparando textos la ortografía del destino
    /// desaparecía de la fila sin dejar marca — justo cuando los nombres son
    /// adversarios. Con bytes idénticos, ese fallo no se distingue del pliegue
    /// correcto (auditoría de encoding MINOR-3).
    #[test]
    fn dos_ortografias_de_un_fallo_que_se_pintan_igual_siguen_siendo_dos() {
        let seg = |id: &str| norte_proto::Segment::new(fixture(id)).expect("seg");
        let f = SyncFailure {
            rel: RelPath::new(vec![seg("lossy_collapse_ff")]),
            dest_rel: Some(RelPath::new(vec![seg("lossy_collapse_fe")])),
            cause: SyncFailureCause::Conflict,
        };
        let t = failure_text(&f, SyncEncodings::default());
        let dest = t.dest.expect("dos ficheros distintos son dos ortografías");
        assert_eq!(dest.label, t.rel.label, "y se pintan igual");
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
