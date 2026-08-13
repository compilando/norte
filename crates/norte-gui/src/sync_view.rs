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
//! La APROBACIÓN no está aquí: es la tarea 4 del plan, deliberadamente en
//! otro commit porque es la mitad DESTRUCTIVA.

use norte_frontend::sync::{SyncRunState, SyncView as SyncRun};
use norte_proto::methods::{SyncMode, SyncPlanDone, SyncStepsBatch};
use norte_proto::{TaskId, TaskState, VPath};

use crate::sp;

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
    ///
    /// **La tarea 4 tiene que REASIGNARLO** al arrancar `sync.apply`, y no es
    /// cosmético: [`close`] y el `Esc` cancelan lo que este campo diga, así
    /// que un `task_id` que se quedara en el del plan cancelaría una Task ya
    /// terminada (no-op) y dejaría corriendo la que está BORRANDO, para un
    /// panel que ya no existe (revisión rust MINOR-3).
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
/// Vive en [`norte_frontend::sync`] desde la tarea 3, junto a
/// [`norte_frontend::sync::render_step`], que es quien decide con cuál se lee
/// cada ruta: el `rel` de un `DeleteTree` cuelga del DESTINO, así que leerlo
/// con la del origen nombraba el subárbol que se va a borrar con los bytes de
/// otro árbol. Esa decisión no puede vivir en un frontend, porque son tres los
/// que la necesitan (TUI, GUI y CLI).
pub use norte_frontend::sync::SyncEncodings;

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

/// Suelta el panel y devuelve la Task a la que hay que mandarle `task.cancel`.
///
/// La decisión de los dos caminos que lo sueltan desde la ventana —el `Esc`
/// del propio panel y la apertura del visor, que lo excluye— en un valor, por
/// lo mismo que [`on_start`]: `main.rs` no se puede testear, así que lo que se
/// puede equivocar no vive allí (revisión rust MINOR-4).
///
/// **Siempre devuelve la Task, sin mirar si sigue viva**: un `task.cancel`
/// sobre una Task terminada es un no-op en el daemon, y mirar antes sería una
/// condición que un día se evalúa mal sobre algo que recorre —o BORRA— dos
/// árboles para un panel que ya no existe (regla dura 3).
#[must_use]
pub fn close(slot: &mut Option<SyncView>) -> Option<TaskId> {
    slot.take().map(|view| view.task_id)
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
    use norte_frontend::sync::{RelAnchor, render_step, step_label, undo_label};

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
        anchor: match cells.anchor {
            RelAnchor::Dest => norte_i18n::t("sync-anchor-dest"),
            RelAnchor::Either => norte_i18n::t("sync-anchor-either"),
            RelAnchor::Source => String::new(),
        },
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
        mode: norte_i18n::t(match run.mode {
            SyncMode::Mirror => "sync-mode-mirror",
            SyncMode::Update => "sync-mode-update",
            _ => "sync-mode-unknown",
        }),
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
/// que se puede equivocar aquí es la DECISIÓN, y una de ellas —el `Esc`— es
/// la única salida de una pantalla que se queda el teclado entero.
///
/// **Aprobar no está aquí**, y no es un olvido: es la tarea 4 del plan, la
/// mitad DESTRUCTIVA, deliberadamente en otro commit. Esta pantalla todavía
/// solo mira.
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
}

/// Traduce una tecla de GPUI (`"escape"`, `"pagedown"`…) a lo que significa
/// en este panel.
///
/// * El primer `Esc` sobre una Task VIVA la cancela y conserva sus pasos;
///   **cualquier `Esc` posterior cierra**, sin mirar el estado de la Task.
///   Condicionar el cierre a un estado terminal deja encerrado al lector
///   cuando el canal no llega a cerrarse nunca —un daemon caído, un provider
///   colgado en una NFS muerta—, que es el BLOCKER-1 que la TUI ya pagó y que
///   el panel de diferencias de esta GUI heredó resuelto.
/// * Con `ctrl`/`alt`/`cmd` no significa nada.
/// * **No hay tecla de salir de norte**, y ahí diverge de la TUI: en modo raw
///   `ISIG` está apagado y su panel tuvo que añadir `Ctrl+C`; una ventana
///   tiene el botón de cerrar del gestor de ventanas, que no es algo que este
///   panel pueda comerse.
#[must_use]
pub fn key_meaning(key: &str, modified: bool, running: bool, cancel_requested: bool) -> Key {
    // Tarea 4: la segunda pregunta se resuelve AQUÍ, por encima del filtro de
    // modificadores. Es el orden que la TUI documenta como corrección: con el
    // filtro delante, un `Ctrl+r` o un `Alt+e` de costumbre caen en `Ignore` y
    // dejan «se van a borrar 2 árboles… ¿Seguir?» armada en pantalla,
    // esperando un `y` que ya no sabe a qué contesta (revisión rust MINOR-2).
    if modified {
        return Key::Ignore;
    }
    match key {
        "escape" => {
            if running && !cancel_requested {
                Key::CancelTask
            } else {
                Key::Close
            }
        }
        "up" => Key::Move(-1),
        "down" => Key::Move(1),
        "pageup" => Key::Move(-PAGE_STEP),
        "pagedown" => Key::Move(PAGE_STEP),
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
/// # El pie nombra una tecla que todavía no existe
/// [`status_line`] es la frase COMPARTIDA, y su brazo `Ready` dice «pulsa `a`
/// para aprobar». En esta GUI `a` llega con la tarea 4 —la mitad
/// destructiva, deliberadamente en otro commit—, que es la que la hace
/// verdad. Escribir aquí una segunda redacción para evitarlo sería la segunda
/// respuesta a «¿esto se puede aprobar?», que es exactamente lo que este
/// módulo existe para no tener.
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

    gpui::div()
        .id("sync-view")
        .role(gpui::Role::Document)
        .aria_label(norte_i18n::t("sync-title"))
        .flex_1()
        .flex()
        .flex_col()
        .overflow_hidden()
        .border_2()
        .border_color(chrome.border_focus)
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
        .child(
            gpui::div()
                .px(px(sp::S))
                .py(px(1.0)) // sub-XS: acento fino de una línea
                .truncate()
                .text_color(if estado_malo { palette.bad } else { palette.fg })
                .child(gpui::SharedString::from(status_line(run))),
        )
        .child(
            gpui::div()
                .px(px(sp::S))
                .py(px(1.0)) // sub-XS: acento fino de una línea
                .truncate()
                .text_color(palette.dim)
                // `sync-hint-done` («↑↓ mover · Esc cerrar») y no `sync-hint`,
                // que además nombra la tecla de aprobar: esta pantalla
                // todavía no la tiene (tarea 4). Un pie que anuncia una tecla
                // muerta se lee como una pantalla rota.
                .child(gpui::SharedString::from(norte_i18n::t("sync-hint-done"))),
        )
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
    /// que no llega a cerrarse nunca no puede dejar encerrado al lector.
    #[test]
    fn el_primer_esc_cancela_y_el_segundo_cierra_pase_lo_que_pase() {
        assert_eq!(key_meaning("escape", false, true, false), Key::CancelTask);
        assert_eq!(key_meaning("escape", false, true, true), Key::Close);
        assert_eq!(key_meaning("escape", false, false, false), Key::Close);
    }

    /// Un modificador no significa nada aquí, y aprobar todavía tampoco: `a`
    /// es de la tarea 4, la mitad destructiva.
    #[test]
    fn un_modificador_no_significa_nada_y_la_a_todavia_no() {
        assert_eq!(key_meaning("down", true, true, false), Key::Ignore);
        assert_eq!(key_meaning("a", false, false, false), Key::Ignore);
        assert_eq!(key_meaning("down", false, false, false), Key::Move(1));
        assert_eq!(key_meaning("pageup", false, false, false), Key::Move(-10));
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
        assert_eq!(close(&mut hueco), Some(task()));
        assert!(hueco.is_none());

        // Con la Task ya terminada, IGUAL: el desenlace no cambia la decisión.
        let mut v = vista_de_prueba();
        assert!(v.on_plan_ended(task(), &TaskState::Completed).is_none());
        let mut hueco = Some(v);
        assert_eq!(close(&mut hueco), Some(task()));
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
