//! El estado de la superficie de sincronización, y lo que dice la barra.
//!
//! La máquina de estados que un frontend pinta: qué fase es, qué teclas
//! tienen sentido en ella y qué frase la resume. Sin nada de terminal ni de
//! ventana: las dos superficies la comparten entera.

use norte_i18n::{Lang, t_in, ta_in};
use norte_proto::methods::{
    DestTrash, PlanHash, SyncCounts, SyncMode, SyncPlanDone, SyncReportResult, SyncStep,
    SyncStepsBatch,
};
use norte_proto::{TaskId, TaskState, VPath};

use super::{
    Applied, Applying, Confirmation, PlanIntegrity, Planning, SyncEncodings, SyncPlan, total_steps,
};

/// The dialog's whole state, and the only legal way to move through it.
///
/// **It never goes backwards.** Every transition is a method that only fires
/// from the one state it belongs to, so a notification that arrives late — a
/// `sync.plan_done` from a plan the user already approved, a batch after the
/// close — is dropped rather than rewinding a dialog the human is looking at.
/// Starting over means building a new [`SyncState`], not walking back through
/// this one.
///
/// **And it only ever listens to ONE plan.** One connection can have two plans
/// in flight — which is why `sync.steps` and `sync.plan_done` both carry a
/// `task_id` — so every transition checks it against the task this dialog was
/// opened for and DROPS what belongs to another. Without that check, a user who
/// re-plans with a narrower selection keeps looking at the first plan's steps
/// and approves the first plan's `plan_hash`: the daemon then executes exactly
/// what was approved, which is not what is on screen. The transitions return
/// `false` when they drop something, so a frontend can log it — a batch that
/// arrives after the close is a protocol violation, and silence is how that
/// hides.
///
/// A dialog opened with [`Planning::default`] (no task) accepts whatever
/// arrives: that is the constructed-plan door, for tests and for a caller that
/// correlated the stream itself.
///
/// **There is no failure state, deliberately.** A `sync.plan` or `sync.apply`
/// that fails or is cancelled is a TASK outcome, which a frontend already
/// paints from `task.progress`; this model would only be able to repeat it.
/// The dialog is dropped, not walked back. What is worth knowing after the
/// fact lives on [`Applied`]: a report with no `batch_id` means nothing was
/// journalled, so [`Applied::is_undoable`] — not the plan's outlook — is what
/// a frontend must read once the plan has run.
///
/// ```
/// use norte_frontend::sync::{Planning, SyncState};
/// let s = SyncState::Planning(Planning::default());
/// assert!(!s.can_approve(), "a plan that has not closed cannot be approved");
/// ```
#[derive(Debug, Clone)]
pub enum SyncState {
    /// `sync.plan` is running and steps are arriving.
    Planning(Planning),
    /// The plan closed and is waiting for a human.
    Ready(SyncPlan),
    /// `sync.apply` is running.
    Applying(Applying),
    /// It finished, and the report is in.
    Applied(Applied),
}

impl Default for SyncState {
    fn default() -> Self {
        Self::Planning(Planning::default())
    }
}

impl SyncState {
    /// A closed plan built from the steps that arrived and the notification
    /// that closed it.
    ///
    /// Routes through [`SyncState::on_plan_done`] rather than assembling a
    /// [`SyncPlan`] directly: there is ONE place where the steps are checked
    /// against the counts, and a second constructor is a second chance to
    /// forget it.
    #[must_use]
    pub fn ready(steps: Vec<SyncStep>, done: SyncPlanDone) -> Self {
        let mut planning = Planning::default();
        planning.extend(steps);
        let mut state = Self::Planning(planning);
        state.on_plan_done(done);
        state
    }

    /// A `sync.steps` batch.
    ///
    /// Takes the whole [`SyncStepsBatch`] and not a list of steps, because the
    /// `task_id` is the only thing that says the batch belongs to THIS plan.
    /// Returns `false` when the batch was dropped: it named another task, or
    /// the plan had already closed (which is a protocol violation, since
    /// `sync.plan_done` is last).
    pub fn on_steps(&mut self, batch: SyncStepsBatch) -> bool {
        let Self::Planning(p) = self else {
            return false;
        };
        if !p.owns(batch.task_id) {
            return false;
        }
        p.extend(batch.steps);
        true
    }

    /// `sync.plan_done`. Only ever fires from [`SyncState::Planning`], and only
    /// for the task this dialog is following; a late one — or one belonging to
    /// a plan the user launched afterwards — is dropped, and `false` says so.
    pub fn on_plan_done(&mut self, done: SyncPlanDone) -> bool {
        let Self::Planning(planning) = self else {
            return false;
        };
        if !planning.owns(done.task_id) {
            return false;
        }
        let planning = std::mem::take(planning);
        let integrity = integrity_of(
            &planning.counts,
            &done.counts,
            planning.malformed,
            planning.duplicate_ids,
        );
        let selected = planning.steps.first().map(|s| s.id);
        *self = Self::Ready(SyncPlan {
            done,
            steps: planning.steps,
            integrity,
            unreadable: planning.unreadable,
            selected,
            dropped: planning.dropped,
            viewport_offset: 0,
        });
        true
    }

    /// The human approved and `sync.apply` answered with a task. Only fires
    /// from [`SyncState::Ready`], and only for a plan that
    /// [`SyncPlan::can_approve`] — a frontend cannot talk this model into
    /// showing a blocked plan as running.
    pub fn on_apply_started(&mut self, task_id: TaskId) {
        let Self::Ready(plan) = self else {
            return;
        };
        if !plan.can_approve() {
            return;
        }
        *self = Self::Applying(Applying {
            plan: plan.clone(),
            task_id,
        });
    }

    /// `sync.report` came back. Only fires from [`SyncState::Applying`].
    pub fn on_report(&mut self, report: SyncReportResult) {
        let Self::Applying(applying) = self else {
            return;
        };
        *self = Self::Applied(Applied {
            plan: applying.plan.clone(),
            report,
        });
    }

    /// The closed plan, in whichever state still holds one.
    #[must_use]
    pub fn plan(&self) -> Option<&SyncPlan> {
        match self {
            Self::Planning(_) => None,
            Self::Ready(p) => Some(p),
            Self::Applying(a) => Some(&a.plan),
            Self::Applied(a) => Some(&a.plan),
        }
    }

    /// The closed plan, mutably — for the cursor, and only for the cursor.
    ///
    /// [`SyncPlan::select`] and [`SyncPlan::move_by`] are the whole reason this
    /// exists: a pane moves a cursor, and everything else on [`SyncPlan`] is a
    /// question. It is deliberately not a door back into the state machine —
    /// there is nothing mutable on [`SyncPlan`] that could rewind it.
    pub fn plan_mut(&mut self) -> Option<&mut SyncPlan> {
        match self {
            Self::Planning(_) => None,
            Self::Ready(p) => Some(p),
            Self::Applying(a) => Some(&mut a.plan),
            Self::Applied(a) => Some(&mut a.plan),
        }
    }

    /// Whether the confirm key does anything right now.
    #[must_use]
    pub fn can_approve(&self) -> bool {
        matches!(self, Self::Ready(p) if p.can_approve())
    }
}

/// Do the steps that arrived account for the plan the daemon closed?
///
/// The classes are compared one by one rather than by their total: two errors
/// that cancel out — a lost `delete_tree` and an extra `skip` — would pass a
/// sum, and those are not the same plan.
///
/// A step this build cannot name is reported FIRST, because it is the stronger
/// statement: the totals may match perfectly and the plan still contain
/// something that cannot be painted. Duplicate ids follow the same per-step
/// class as malformed shapes, and for the same reason (#194): each is a
/// defect in ONE step's identity, not in the totals, so it is checked before
/// anything that sums.
fn integrity_of(
    local: &SyncCounts,
    remote: &SyncCounts,
    malformed: u64,
    duplicate_ids: u64,
) -> PlanIntegrity {
    // The daemon's own `unknown_kind` counts too, and it is not redundant: a
    // daemon that reports one while every step we decoded had a name is
    // telling us the plan holds something neither of us can show.
    let unnameable = local.unknown_kind.max(remote.unknown_kind);
    if unnameable > 0 {
        return PlanIntegrity::Unnameable { steps: unnameable };
    }
    if malformed > 0 {
        return PlanIntegrity::Malformed { steps: malformed };
    }
    if duplicate_ids > 0 {
        return PlanIntegrity::DuplicateIds {
            steps: duplicate_ids,
        };
    }
    let classes_agree = local.create_dir == remote.create_dir
        && local.copy == remote.copy
        && local.overwrite == remote.overwrite
        && local.delete_tree == remote.delete_tree
        && local.skip == remote.skip;
    if !classes_agree {
        return PlanIntegrity::Mismatch {
            received: total_steps(local),
            counted: total_steps(remote),
        };
    }
    // Both sides ran `SyncCounts::add` over the same steps, so EVERY field must
    // match, not only the classes. `irreversible` is the one that matters most
    // — the headline is built on it — and `bytes`/`unmeasured_steps` come free
    // in the same comparison.
    if local == remote {
        PlanIntegrity::Complete
    } else {
        PlanIntegrity::Contradictory
    }
}

/// Cómo va la Task de un panel de sincronización, para la barra de estado.
///
/// Deliberadamente MÁS CORTO que [`crate::compare::CompareState`]: aquí el
/// «llegaron todas las filas» no se deduce de un conteo, lo DICE el
/// `sync.plan_done` — sin él no hay `plan_hash` y no hay nada que aprobar, así
/// que un plan incompleto no es un estado que pintar sino un plan que no
/// existe (`SyncPlanEvent`, ADR 0049).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncRunState {
    /// Una Task viva: se está planificando, o se está aplicando.
    #[default]
    Running,
    /// La Task terminó bien.
    Done,
    /// El usuario canceló.
    Cancelled,
    /// La Task falló (el error va por la barra).
    Failed,
}

impl SyncRunState {
    /// El desenlace de la Task que corre detrás del panel —la del plan
    /// primero, la de la aplicación después— leído de su [`TaskState`]
    /// terminal.
    ///
    /// Deliberadamente NO toca el error localizado que cada frontend pinta
    /// (una barra truncada en la TUI, algo distinto en la GUI): eso es la
    /// única mitad que legítimamente difiere entre las dos, y mezclarla aquí
    /// ataría este mapeo puro a un [`Lang`] sin necesidad. Lo que SÍ era una
    /// sola decisión repetida a mano —`Cancelled`/`Failed`/lo demás→`Done`—
    /// es lo que vive aquí, para que un `_ => Done` no se transcriba dos
    /// veces y un día se le olvide un brazo a una de las dos copias.
    ///
    /// ```
    /// use norte_frontend::sync::SyncRunState;
    /// use norte_proto::TaskState;
    ///
    /// assert_eq!(
    ///     SyncRunState::from_task_state(&TaskState::Cancelled),
    ///     SyncRunState::Cancelled
    /// );
    /// ```
    #[must_use]
    pub fn from_task_state(state: &TaskState) -> Self {
        match state {
            TaskState::Cancelled => Self::Cancelled,
            TaskState::Failed { .. } => Self::Failed,
            _ => Self::Done,
        }
    }
}

/// El panel de sincronización abierto: el modelo puro de [`SyncState`] más lo
/// que un frontend necesita para pintarlo y para hablar con el backend.
///
/// El reparto es el mismo que el de [`crate::compare::CompareView`] (regla
/// dura 7): el estado del diálogo —qué pasos llegaron, si cuadran con lo que
/// el daemon cerró, qué devuelve el undo y cuál es la segunda pregunta— vive
/// aquí, donde se prueba sin terminal. Lo que cada frontend añade son las dos
/// raíces que la cabecera pinta, el estado del run y la pregunta de
/// confirmación EN CURSO — y esas también viven aquí (#161): la TUI y la GUI
/// necesitan el MISMO envoltorio, no dos reimplementados por separado.
#[derive(Debug)]
pub struct SyncView {
    /// El modelo del diálogo (Task 12).
    pub state: SyncState,
    /// Cómo va la Task que está corriendo ahora mismo (la del plan primero, la
    /// de la aplicación después).
    pub run: SyncRunState,
    /// Modo pedido, que la cabecera pinta: un `Mirror` borra y un `Update` no,
    /// y el lector tiene que verlo antes de aprobar.
    pub mode: SyncMode,
    /// Raíz ORIGEN. De ella cuelgan las `rel` de casi todos los pasos.
    pub source_root: VPath,
    /// Raíz DESTINO. De ella cuelgan las de un `DeleteTree` y las de un `Skip`
    /// ilegible ([`crate::sync::anchor_of`]).
    pub dest_root: VPath,
    /// Reinterpretación de nombres (#57) del pane ORIGEN, congelada al abrir.
    pub source_encoding: Option<norte_encoding::NameEncoding>,
    /// La del pane DESTINO, que puede ser otra.
    ///
    /// Dos y no una, por lo mismo que el panel de diferencias lleva dos: los
    /// dos panes son dos ubicaciones y pueden llevar overrides distintos. Aquí
    /// además importa más, porque `SyncStep::dest_rel` existe precisamente
    /// para enseñar la ortografía del DESTINO (#152) — decodificarla con el
    /// codepage del ORIGEN nombraría con otros bytes el fichero sobre el que
    /// va a caer la escritura.
    pub dest_encoding: Option<norte_encoding::NameEncoding>,
    /// La segunda pregunta, ya formulada y esperando un `y`.
    ///
    /// `None` = todavía no se ha pulsado aprobar, o el plan no la necesitaba.
    /// Vive aquí y no en el modelo porque es estado de INTERACCIÓN —a medio
    /// contestar— y el modelo de Task 12 no retrocede: preguntar es de la
    /// pantalla, decidir es suyo.
    pub confirming: Option<Confirmation>,
    /// Ya se pidió cancelar (el primer `Esc`), igual que en el panel de
    /// diferencias y por el mismo motivo: el segundo `Esc` cierra pase lo que
    /// pase con la Task.
    pub cancel_requested: bool,
    /// Categoría del error de una Task que FALLÓ, ya localizada y saneada.
    pub error: Option<String>,
    /// El `sync.apply` ya SALIÓ y el daemon todavía no ha contestado.
    ///
    /// Privado a propósito: la única forma de echarlo es [`SyncView::submit`]
    /// y la única de leerlo, [`SyncView::is_submitted`]. Lo que lo hace
    /// necesario es que `Applying` NO llega con la tecla sino una vuelta
    /// entera después, cuando el daemon devuelve la Task — en una GUI que lee
    /// eventos entre teclas esa ventana admite un segundo `a`, y también un
    /// `Esc` (revisión de seguridad MAJOR-1).
    ///
    /// Vivió en `norte-gui` hasta la revisión de rama de C2, y ahí estaba mal:
    /// `can_approve`, [`hint_id`] y [`status_line`] viven en ESTE crate y no
    /// podían verlo, así que el pie seguía ofreciendo `a aprobar` sobre un
    /// plan que `approve` ya rechazaba — justo la pantalla rota que `hint_id`
    /// existe para no pintar. Lo limpian las TRANSICIONES DE ESTADO
    /// ([`SyncView::on_apply_started`], [`SyncView::on_apply_ended`]), nunca
    /// la generación de la petición: atarlo a la generación lo dejaba echado
    /// para siempre cuando un evento superado se descartaba.
    submitted: bool,
}

impl SyncView {
    /// Un panel recién abierto sobre estas dos raíces, sin pasos todavía.
    #[must_use]
    pub fn new(
        task_id: TaskId,
        mode: SyncMode,
        source_root: VPath,
        dest_root: VPath,
        source_encoding: Option<norte_encoding::NameEncoding>,
        dest_encoding: Option<norte_encoding::NameEncoding>,
    ) -> Self {
        Self {
            // Con el `task_id` desde el principio: es lo que hace que un lote
            // de OTRO plan —el lector replanifica con menos marcas— se caiga
            // en vez de mezclarse con éste (Task 12, nota 3).
            state: SyncState::Planning(Planning::new(task_id)),
            run: SyncRunState::Running,
            mode,
            source_root,
            dest_root,
            source_encoding,
            dest_encoding,
            confirming: None,
            cancel_requested: false,
            error: None,
            submitted: false,
        }
    }

    /// Qué papelera tiene el DESTINO, según el plan.
    ///
    /// [`DestTrash::Unknown`] mientras el plan no ha cerrado, que es la
    /// respuesta honesta: sin `sync.plan_done` no se sabe, y el modelo pinta
    /// cada paso como «esta versión no puede decirlo» en vez de prometer que
    /// vuelve. Nunca se lee [`SyncStep::reversal`] a pelo — esa es la mitad de
    /// la respuesta y la que miente cuando el destino no tiene papelera.
    #[must_use]
    pub fn dest_trash(&self) -> DestTrash {
        self.state
            .plan()
            .map_or(DestTrash::Unknown, SyncPlan::dest_trash)
    }

    /// Las dos reinterpretaciones, juntas y nombradas, para pasárselas a
    /// [`crate::sync::render_step`] de una pieza — que es lo que evita cruzarlas (#152).
    #[must_use]
    pub fn encodings(&self) -> SyncEncodings {
        SyncEncodings {
            source: self.source_encoding,
            dest: self.dest_encoding,
        }
    }

    /// Los pasos que hay AHORA MISMO, esté cerrado el plan o no.
    ///
    /// Mientras el plan llega, [`SyncState::plan`] contesta `None` —no hay
    /// plan hasta el `sync.plan_done`, que es lo que le da su `plan_hash`— y
    /// aun así los pasos ya recibidos existen y se pintan. Sin esto el panel
    /// enseñaba un hueco vacío mientras el pie contaba «planificando… 6
    /// pasos», que es la pantalla diciéndose la contraria a sí misma. La
    /// columna del undo de esos pasos sale «esta versión no puede decirlo»,
    /// que es la verdad hasta que se sepa la papelera del destino.
    #[must_use]
    pub fn steps(&self) -> &[SyncStep] {
        match &self.state {
            SyncState::Planning(p) => p.steps(),
            _ => self.state.plan().map_or(&[], |p| p.steps()),
        }
    }

    /// ¿Sigue habiendo algo que aprobar?
    ///
    /// `false` en cuanto el plan se manda: la línea de teclas no puede seguir
    /// ofreciendo `a aprobar` sobre un plan que ya se gastó —aplicarlo lo
    /// consume, y un segundo `sync.apply` del mismo hash es `PlanStale`—.
    #[must_use]
    pub fn awaiting_approval(&self) -> bool {
        matches!(self.state, SyncState::Ready(_))
    }

    /// ¿Se puede aprobar este panel AHORA MISMO?
    ///
    /// Envuelve [`SyncState::can_approve`] y NUNCA
    /// [`SyncPlan::can_approve`] — el segundo, alcanzable por
    /// [`SyncState::plan`], sigue contestando que sí sobre un plan que ya se
    /// aprobó, porque sus tres factores no cambian al gastarse. Este método
    /// es la forma de que un llamante no tenga ocasión de coger el atajo
    /// equivocado (#161, la trampa que la fase A del CLI no vio: no preguntó
    /// nada, y un plan `Malformed` se aplicó entero desde el spool).
    ///
    /// # Y el desenlace de la Task cuenta
    /// Un run `Cancelled` o `Failed` no se aprueba, aunque el plan HAYA
    /// cerrado. Los dos hechos son compatibles —`sync.plan_done` llega antes
    /// de que el canal se cierre, así que un `Esc` (o una caída del daemon)
    /// en esa ventana deja `Ready` + `Cancelled`—, y sin esta cláusula la
    /// pantalla decía las dos cosas a la vez: el pie pintaba «cancelado — no
    /// hay plan que aprobar» ([`status_line`]) mientras la línea de teclas
    /// seguía ofreciendo aprobar, y la tecla FUNCIONABA (revisión rust
    /// MAJOR-1). Se resuelve del lado conservador: quien pulsó `Esc` pidió
    /// parar, y esta pantalla escribe en el disco de alguien.
    #[must_use]
    pub fn can_approve(&self) -> bool {
        if self.submitted || matches!(self.run, SyncRunState::Cancelled | SyncRunState::Failed) {
            return false;
        }
        self.state.can_approve()
    }

    /// ¿Hay un `sync.apply` en vuelo sin contestar?
    ///
    /// Lo pregunta quien pinta la línea de teclas y quien interpreta un `Esc`:
    /// en esta ventana el daemon YA está escribiendo, así que un `Esc` tiene
    /// que pedir cancelación y no cerrar el panel. Cerrarlo pierde el informe
    /// —y con él el recuento, los fallos y el asa del undo— sobre un destino
    /// que se reescribió a medias (revisión de seguridad MAJOR-1).
    #[must_use]
    pub fn is_submitted(&self) -> bool {
        self.submitted
    }

    /// La petición se resolvió SIN Task: el daemon la rechazó, o llegó una
    /// Task que este panel no adopta.
    ///
    /// Suelta el pestillo, porque si no la `a` queda muerta para siempre y el
    /// pie sigue ofreciéndola. Se llama también en los caminos donde el evento
    /// se descarta por generación superada: atar la suelta a la generación es
    /// justo lo que dejaba el panel encallado cuando el segundo plan se
    /// rechazaba y ningún panel nuevo sustituía al primero (revisión de rama
    /// de C2, MINOR de las dos revisiones).
    pub fn on_apply_abandoned(&mut self) {
        self.submitted = false;
    }

    /// Echa el pestillo y devuelve el hash que se manda, o `None` si este
    /// panel no se puede aprobar.
    ///
    /// Una sola puerta para los dos frontends: quien quiera aplicar pasa por
    /// aquí, y lo que impide el segundo `sync.apply` es esta función, no que
    /// el estado sea `Applying` —no lo es todavía—. La TUI lo espera en línea
    /// y no puede leer una tecla en medio, así que para ella es un no-op; la
    /// GUI sí puede, y es la que lo necesita.
    pub fn submit(&mut self) -> Option<PlanHash> {
        if !self.can_approve() {
            return None;
        }
        let hash = self.state.plan()?.done().plan_hash.clone();
        self.submitted = true;
        Some(hash)
    }

    /// Se lanzó `sync.apply` y el daemon contestó con una Task: junta las
    /// CUATRO actualizaciones que ese instante exige — el modelo avanza a
    /// `Applying` ([`SyncState::on_apply_started`]), el run vuelve a
    /// `Running`, la segunda pregunta se cae (ya se contestó) y la
    /// cancelación pedida por un run anterior deja de aplicar al nuevo.
    ///
    /// Antes de que esto viviera aquí, `norte-tui` hacía las cuatro a mano en
    /// el sitio que lanza la Task; la GUI habría necesitado exactamente las
    /// mismas cuatro, y una reimplementación por su cuenta es justo la
    /// oportunidad de olvidar una — la trampa que este movimiento existe para
    /// no repetir (#161, revisión de C1).
    /// # Y puede NEGARSE
    /// Devuelve `false` sin tocar nada si ya se pidió cancelar. El `Esc` que
    /// pidió parar llegó ANTES que la Task, así que adoptarla aquí resucitaría
    /// un run que el lector dio por cortado y, peor, borraría la petición de
    /// cancelación con el `cancel_requested = false` de abajo — que existe
    /// para que una cancelación vieja no manche el run nuevo, no para
    /// descartar la que acaba de pedirse.
    ///
    /// El guard estaba en el envoltorio de la GUI y no aquí, así que la TUI se
    /// quedaba con el agujero: hoy no lo alcanza porque espera el `sync.apply`
    /// en línea, o sea por casualidad del flujo de control y no por diseño
    /// (revisión de rama de C2, rust MAJOR-2). Quien lo niegue tiene que
    /// cancelar la Task que le devolvieron: nadie más la conoce.
    pub fn on_apply_started(&mut self, task_id: TaskId) -> bool {
        if self.cancel_requested {
            return false;
        }
        self.state.on_apply_started(task_id);
        self.run = SyncRunState::Running;
        self.confirming = None;
        self.cancel_requested = false;
        // El daemon contestó: la ventana que el pestillo cubre se acabó, y a
        // partir de aquí quien impide el segundo `sync.apply` es el estado
        // `Applying`.
        self.submitted = false;
        true
    }

    /// Terminó la Task de `sync.apply`, con lo que `sync.report` contestó:
    /// mete el informe y fija el desenlace. Devuelve la categoría del error que
    /// hay que decir, SIN sanear — cada frontend la mete donde y como pinta.
    ///
    /// Compartida (#161) porque las tres reglas de aquí son de las que un
    /// frontend arregla y el otro se queda:
    ///
    /// 1. **El error de la TASK manda sobre el del informe**: es el que dice
    ///    por qué se paró.
    /// 2. **Sin informe no se dice que terminó bien.** `sync.report` es lo
    ///    ÚNICO que dice cuánto se llegó a escribir; si no se pudo pedir, el
    ///    desenlace es `Failed` con la categoría de ESE error aunque la Task
    ///    dijera `Completed`. `norte-tui` se quedaba aquí en `Applying` con una
    ///    barra transitoria, y el pie decía «aplicando…» para siempre.
    /// 3. **Un estado NO terminal también es fallo.** Solo se llega a él con
    ///    los emisores del progreso caídos: la conexión murió sin decir qué
    ///    pasó, y una sincronización a medias no es un éxito.
    ///
    /// Con UNA excepción a las dos últimas: una Task **cancelada** se dice
    /// cancelada aunque el informe falte. El lector pidió parar y eso ya lo
    /// sabe; convertirlo en «falló» le quita el único dato firme que tiene, y
    /// que el informe no llegara lo cuenta la categoría que esto devuelve.
    ///
    /// La segunda pregunta se cae con la petición que la motivó: dejarla puesta
    /// bajo un pie que ya dice «falló» es cómo un `y` posterior contesta a otra
    /// cosa.
    ///
    /// El informe se mete TAMBIÉN cuando la Task se canceló: lo aplicado hasta
    /// el corte se queda journalizado, y media sincronización es un estado real
    /// que el lector tiene que poder ver.
    pub fn on_apply_ended(
        &mut self,
        state: &TaskState,
        report: Result<SyncReportResult, norte_proto::Error>,
        lang: Lang,
    ) -> Option<String> {
        // El idioma va como PARÁMETRO y no se lee del global: la ventana
        // gráfica tiene uno por instancia, y el desenlace de una escritura en
        // el idioma de otra ventana es un desenlace que no se lee.
        let categoria = match (state, &report) {
            (TaskState::Failed { error }, _) => Some(crate::error::error_category_in(lang, error)),
            (_, Err(e)) => Some(crate::error::error_category_in(lang, e)),
            _ => None,
        };
        if let Ok(informe) = report {
            self.state.on_report(informe);
        }
        self.run = if matches!(state, TaskState::Cancelled) {
            // Una cancelación se dice CANCELADA aunque el informe no llegue:
            // el lector pidió parar y eso ya lo sabe, así que llamarlo «falló»
            // le quita el único dato firme que tiene. Que no se pueda decir
            // cuánto se escribió lo dice el banner, con la categoría que esto
            // devuelve.
            SyncRunState::Cancelled
        } else if categoria.is_some() || !state.is_terminal() {
            SyncRunState::Failed
        } else {
            SyncRunState::from_task_state(state)
        };
        self.confirming = None;
        // Terminó: el pestillo se suelta pase lo que pase, incluso si esto
        // llega sin que `on_apply_started` haya pasado nunca (una Task que
        // falla antes de adoptarse). Si no, el panel se queda sin poder
        // aprobar y con el pie ofreciéndolo.
        self.submitted = false;
        categoria
    }
}

/// Qué línea de TECLAS toca ahora mismo, como id de Fluent.
///
/// Tres, y la diferencia entre las dos últimas es la única tecla de esta
/// pantalla que escribe en el disco de alguien:
///
/// * `sync-hint-confirm` con la segunda pregunta puesta — el teclado se ha
///   reducido a `y` y «cualquier otra», y decir «↑↓ mover» ahí es ofrecer algo
///   que ya no funciona;
/// * `sync-hint`, que NOMBRA la tecla de aprobar, solo cuando aprobar hace
///   algo;
/// * `sync-hint-done` en todo lo demás.
///
/// # Por qué es compartida
/// El segundo brazo pregunta por [`SyncView::can_approve`] y no solo por
/// [`SyncView::awaiting_approval`], y ésa es la corrección: un plan que cerró
/// pero que el daemon marcó no ejecutable —o cuya Task se canceló— está en
/// `Ready` y NO se puede aprobar, y la línea de teclas seguía ofreciendo `a
/// aprobar` encima de un pie que ya decía «este plan no se puede aprobar»
/// ([`status_line`]). Es el mismo desacuerdo que la revisión rust MAJOR-1
/// arregló entre el pie y la tecla, una capa más arriba; vive aquí para que
/// haya UNA respuesta para los dos frontends y no una arreglada y otra no
/// —que es exactamente lo que C1 shipeó (#161)—.
///
/// Aplicar GASTA el plan, así que en `Applying`/`Applied` la `a` desaparece:
/// un segundo `sync.apply` del mismo hash contesta `PlanStale`.
///
/// ```
/// use norte_frontend::sync::{SyncView, hint_id};
/// use norte_proto::{TaskId, VPath};
/// use norte_proto::methods::SyncMode;
/// let v = SyncView::new(
///     TaskId::new(1),
///     SyncMode::Update,
///     VPath::parse("file:///a").expect("vpath"),
///     VPath::parse("file:///b").expect("vpath"),
///     None,
///     None,
/// );
/// // Todavía planificando: no hay nada que aprobar, así que no se ofrece.
/// assert_eq!(hint_id(&v), "sync-hint-done");
/// ```
#[must_use]
pub fn hint_id(view: &SyncView) -> &'static str {
    if view.confirming.is_some() {
        "sync-hint-confirm"
    } else if view.awaiting_approval() && view.can_approve() {
        "sync-hint"
    } else {
        "sync-hint-done"
    }
}

/// Where the synchronisation dialog IS: planning, waiting for a human, running
/// or finished — one sentence, for whatever a frontend uses as a footer.
///
/// Shared by both frontends (#161) for the same reason
/// [`crate::compare::status_line`] is, and this one carries more weight: its
/// `Ready` arm is where "this plan can be approved" reaches a human as words,
/// and a second copy of that decision is a second answer to the only question
/// on this screen that writes to a disk. It lived in `norte-tui`'s renderer
/// until the GUI needed the same footer.
///
/// Three things it deliberately does NOT re-derive:
///
/// * approvability is [`SyncPlan::can_approve`] over the plan the state still
///   holds — and the state is what chose this arm, so a plan that has already
///   been spent is `Applying`/`Applied` here and never `Ready`;
/// * whether the applied plan can be undone is [`Applied::is_undoable`], which
///   reads the report's `batch_id`, and NEVER the plan's outlook: a report with
///   no batch means nothing was journalled, whatever the plan promised before
///   it ran;
/// * the localised error of a failed task is [`SyncView::error`], which each
///   frontend fills the way it sanitises text.
///
/// The frontend adds its own padding; this returns the sentence alone.
///
/// ```
/// use norte_frontend::sync::{SyncView, status_line};
/// use norte_i18n::Lang;
/// use norte_proto::{TaskId, VPath};
/// use norte_proto::methods::SyncMode;
/// let v = SyncView::new(
///     TaskId::new(1),
///     SyncMode::Update,
///     VPath::parse("file:///a").expect("vpath"),
///     VPath::parse("file:///b").expect("vpath"),
///     None,
///     None,
/// );
/// // Recién abierto: planificando, con cero pasos.
/// assert!(!status_line(&v, Lang::En).is_empty());
/// ```
#[must_use]
pub fn status_line(view: &SyncView, lang: Lang) -> String {
    let plan = view.state.plan();
    let n = plan.map_or_else(
        || match &view.state {
            SyncState::Planning(p) => p.len(),
            _ => 0,
        },
        |p| {
            p.steps()
                .len()
                .saturating_add(usize::try_from(p.dropped()).unwrap_or(usize::MAX))
        },
    );
    let n = n.to_string();
    match (&view.state, view.run) {
        // **El informe manda, y va PRIMERO** — pero SIN perder cómo acabó.
        //
        // Una aplicación cortada a medias TIENE informe (lo aplicado hasta el
        // corte se queda, journalizado) y es justo el estado en el que el
        // lector más necesita saber cuánto llegó a escribirse. Con este brazo
        // detrás del de `Cancelled`, la pantalla decía «cancelado — habían
        // llegado N pasos, y no hay plan que aprobar» —una frase sobre el PLAN,
        // que ya se aprobó— encima de la lista de fallos de la APLICACIÓN.
        //
        // Y el desenlace elige la FRASE en vez de perderse: «cancelado tras
        // aplicar N» y «falló tras aplicar N» dicen las dos mitades. Poner el
        // brazo de `Failed` delante escondía las cuentas de un `Mirror` que
        // borró cuarenta árboles y luego murió, que es el sitio donde menos se
        // pueden esconder; ponerlo detrás sin frases propias borraba la palabra
        // «cancelado», y el color habría sido la única señal — en la rama cuyo
        // commit anterior se titula «legible sin color» (#161, fase C2 tarea 4;
        // revisiones rust MAJOR-2 y de seguridad MAJOR-4).
        (SyncState::Applied(a), run) => {
            let done = a.report().done.to_string();
            let failed = a.report().failed.to_string();
            let undo = if a.is_undoable() {
                "undoable"
            } else {
                "not-undoable"
            };
            let id = match run {
                SyncRunState::Cancelled => format!("sync-status-applied-cut-{undo}"),
                SyncRunState::Failed => format!("sync-status-applied-failed-{undo}"),
                SyncRunState::Running | SyncRunState::Done => {
                    format!("sync-status-applied-{undo}")
                }
            };
            ta_in(
                lang,
                &id,
                &[
                    ("done", &done),
                    ("failed", &failed),
                    ("error", view.error.as_deref().unwrap_or_default()),
                ],
            )
        }
        // Sin informe, el fallo manda: un error tiene que llegar entero, y no
        // hay recuento que lo pueda sustituir.
        (_, SyncRunState::Failed) => ta_in(
            lang,
            "sync-status-failed",
            &[("error", view.error.as_deref().unwrap_or_default())],
        ),
        (_, SyncRunState::Cancelled) => ta_in(lang, "sync-status-cancelled", &[("n", &n)]),
        (SyncState::Planning(_), _) => ta_in(lang, "sync-planning", &[("n", &n)]),
        // `view.can_approve()` y no `p.can_approve()`: UNA sola función
        // contesta esa pregunta, y es la misma que la línea de teclas
        // consulta. Con la del plan a secas, este brazo y aquella podían
        // discrepar en cuanto el desenlace de la Task entraba en juego.
        (SyncState::Ready(_), _) => {
            let id = if view.can_approve() {
                "sync-status-ready"
            } else {
                "sync-status-not-approvable"
            };
            ta_in(lang, id, &[("n", &n)])
        }
        (SyncState::Applying(_), _) => t_in(lang, "sync-status-applying"),
    }
}
