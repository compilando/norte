//! Planificar una sincronización y aplicarla.
//!
//! Dos Tasks y no una: primero el PLAN, que se lee, y después la APLICACIÓN,
//! que escribe. Entre las dos hay una aprobación explícita —`a` y luego `y`—
//! porque sincronizar borra y sobrescribe.

use crossterm::event::KeyCode;
use norte_core::backend::{Backend, TaskRef};
use norte_i18n::{t, ta};

use super::compare::COMPARE_PAGE_STEP;
use crate::app::{App, detail_for_bar, error_category};

/// Qué acaba de pasarle a la Task viva de un [`SyncRun`].
///
/// Un solo brazo del `select!` cubre las dos fases del diálogo, así que hace
/// falta un tipo que diga cuál de ellas habló.
pub enum SyncTick {
    /// Un evento del plan, o `None` = fin del flujo.
    Plan(Option<norte_core::sync::SyncPlanEvent>),
    /// La Task de la aplicación cambió de estado.
    Applied {
        /// Alguien sigue publicando su progreso.
        ///
        /// `false` = se cayeron TODOS los emisores. Es un final, no un tick:
        /// tratarlo como un tick con un último estado no terminal rearma el
        /// brazo sobre un future ya listo y gira. Se decide igual que
        /// `TaskRef::join`, que ante lo mismo sintetiza un fallo en vez de
        /// heredar la invariante de otro crate.
        alive: bool,
    },
}

/// Una sincronización EN CURSO (`Ctrl+Y`, 2026-08-11-directory-sync.md).
///
/// Cubre las DOS Tasks del flujo, una detrás de otra, porque son un solo
/// diálogo para el lector: `sync.plan` (con su canal de eventos) y, si aprueba,
/// `sync.apply` (sin canal — lo que hizo se pide con `sync.report`).
pub struct SyncRun {
    /// La Task viva, cancelable (regla 3).
    pub task: TaskRef,
    /// Canal de eventos del plan. `None` mientras corre la APLICACIÓN, que no
    /// tiene stream.
    pub rx: Option<tokio::sync::mpsc::Receiver<norte_core::sync::SyncPlanEvent>>,
    /// Progreso de la Task, para saber cuándo la aplicación acabó y pedir su
    /// informe. Se mira también al cerrarse el canal del plan, igual que en la
    /// comparación.
    pub progress: tokio::sync::watch::Receiver<norte_proto::TaskProgress>,
    /// La aplicación ya está corriendo (`sync.apply`), no el plan.
    pub applying: bool,
}

/// Lanza `sync.plan` y abre el panel de sincronización.
///
/// Los params ya vienen resueltos y validados por
/// [`App::request_sync`](crate::app::App::request_sync). Un panel anterior
/// se reemplaza y su Task se cancela (regla 3): dos planes a la vez serían dos
/// flujos alimentando un diálogo cuyo `plan_hash` es lo que se aprueba.
pub async fn launch_sync_plan(
    app: &mut App,
    backend: &Backend,
    sync_run: &mut Option<SyncRun>,
    params: norte_proto::methods::SyncPlanParams,
) {
    let (source, dest, mode) = (params.source.clone(), params.dest.clone(), params.mode);
    match backend.sync_plan(params).await {
        Ok((task, rx)) => {
            app.message = None;
            let progress = task.progress();
            let (source_encoding, dest_encoding) = app.pending_sync_encoding;
            app.sync = Some(crate::app::SyncView::new(
                task.id(),
                mode,
                source,
                dest,
                source_encoding,
                dest_encoding,
            ));
            if let Some(old) = sync_run.replace(SyncRun {
                task,
                rx: Some(rx),
                progress,
                applying: false,
            }) {
                old.task.cancel();
            }
        }
        // El panel NO se abre, por lo mismo que el de diferencias: un panel
        // vacío que dice «fallo» es peor que la frase en la barra. La
        // categoría va saneada — y `OverlappingRoots` llega aquí con su
        // relación, que es justo el error que este camino produce de verdad.
        Err(e) => {
            app.message = Some(ta(
                "sync-status-failed",
                &[("error", &detail_for_bar(&error_category(&e)))],
            ));
        }
    }
}

/// Lanza `sync.apply` sobre el plan APROBADO.
///
/// El hash es lo único que viaja: no hay forma de pedir que se ejecute algo
/// distinto de lo que el panel enseñó (ADR 0049). La Task del plan ya terminó,
/// así que este `SyncRun` la SUSTITUYE sin cancelar nada.
pub async fn launch_sync_apply(
    app: &mut App,
    backend: &Backend,
    sync_run: &mut Option<SyncRun>,
    plan_hash: &norte_proto::methods::PlanHash,
) {
    match backend.sync_apply(plan_hash).await {
        Ok(task) => {
            let progress = task.progress();
            // Y puede NEGARSE: `on_apply_started` refuse una Task que llega
            // después de que ya se pidiera cancelar. La TUI no puede leer una
            // tecla entre el `sync_apply` y esta línea —lo espera en línea—,
            // así que hoy no se alcanza; el guard vive en `norte-frontend`
            // porque estaba en el envoltorio de la GUI y esta rama lo dejaba
            // dependiendo del flujo de control (revisión de rama, rust
            // MAJOR-2). Quien la niega la cancela: nadie más la conoce.
            let adopted = app
                .sync
                .as_mut()
                .is_some_and(|view| view.on_apply_started(task.id()));
            if !adopted {
                task.cancel();
                return;
            }
            // Y AL TABLERO (#173): el panel conserva la task —`Esc` sigue
            // siendo desde donde se para un plan aprobado— y el tablero se
            // queda un `TaskObserver`, que pinta y cancela sin poseer. Antes
            // no estaba porque el tablero se quedaba el `TaskRef` entero, así
            // que la operación más destructiva del programa era la única
            // invisible: cerrado el panel, un `Mirror` seguía reescribiendo un
            // subárbol sin fila, sin progreso y sin forma de pararlo.
            app.board.push_observed(task.observer(), None);
            // La del plan se cancela SIEMPRE al sustituirla: `Ready` se alcanza
            // al RECIBIR el `sync.plan_done`, y su flujo puede no haberse
            // cerrado todavía. `TaskRef` no tiene `Drop`, así que soltarla sin
            // más deja al daemon recorriendo dos árboles para un plan ya
            // aprobado. Cancelar una Task terminada no hace nada.
            if let Some(anterior) = sync_run.replace(SyncRun {
                task,
                rx: None,
                progress,
                applying: true,
            }) {
                anterior.task.cancel();
            }
        }
        Err(e) => {
            if let Some(view) = app.sync.as_mut() {
                view.run = crate::app::SyncRunState::Failed;
                view.error = Some(detail_for_bar(&error_category(&e)));
            }
            app.message = Some(ta(
                "sync-status-failed",
                &[("error", &detail_for_bar(&error_category(&e)))],
            ));
        }
    }
}

/// Aplica un evento del plan (o el cierre del canal) al panel.
///
/// `None` = fin del flujo. A diferencia de la comparación, aquí NO hay que
/// cuadrar un conteo contra `entries_done`: el `sync.plan_done` es la señal, y
/// su ausencia es la protección — sin él no hay `plan_hash` y no hay nada que
/// aprobar. Lo que sí se hace es leer el estado terminal, para distinguir un
/// plan cancelado de uno que falló.
pub fn drain_sync_plan(
    app: &mut App,
    sync_run: &mut Option<SyncRun>,
    event: Option<norte_core::sync::SyncPlanEvent>,
) {
    let Some(run) = sync_run else {
        return;
    };
    let Some(view) = app.sync.as_mut() else {
        // El panel se cerró bajo el flujo: cancelar en vez de seguir
        // recibiendo pasos que nadie va a mirar (mismo motivo que en la
        // comparación — en remoto el daemon seguiría recorriendo los árboles).
        run.task.cancel();
        *sync_run = None;
        return;
    };
    match event {
        Some(norte_core::sync::SyncPlanEvent::Steps(batch)) => {
            if !view.state.on_steps(batch) {
                // Un lote de otro plan, o uno que llega DESPUÉS del cierre —
                // que es una violación del protocolo. El modelo lo tira; que
                // quede dicho en el log es lo que impide que se esconda.
                tracing::warn!("lote de sync.steps descartado: no es de este plan");
            }
        }
        Some(norte_core::sync::SyncPlanEvent::Done(done)) => {
            if !view.state.on_plan_done(done) {
                tracing::warn!("sync.plan_done descartado: no es de este plan");
            }
        }
        None => {
            let snapshot = run.progress.borrow_and_update().clone();
            // El mapeo `TaskState` → `SyncRunState` es de
            // `crate::app::SyncRunState::from_task_state` (#161): la
            // localización del error que sigue es la única mitad que de
            // verdad difiere entre frontends, y por eso se queda aquí.
            view.run = crate::app::SyncRunState::from_task_state(&snapshot.state);
            if let norte_proto::TaskState::Failed { error } = snapshot.state {
                let category = detail_for_bar(&error_category(&error));
                view.error = Some(category.clone());
                app.message = Some(ta("sync-status-failed", &[("error", &category)]));
            }
            // El canal se acabó: el `SyncRun` ya no tiene nada que drenar,
            // pero se conserva para que `Esc` siga pudiendo cancelar si la
            // Task no era terminal todavía.
            run.rx = None;
        }
    }
}

/// Cosecha la Task de `sync.apply` cuando llega a un estado terminal y pide su
/// informe.
///
/// El informe se pide SIEMPRE que la Task acaba, incluida la cancelación: lo
/// aplicado hasta el corte se queda, journalizado, y media sincronización es un
/// estado real que el lector tiene que poder ver.
pub async fn harvest_sync_apply(
    app: &mut App,
    backend: &Backend,
    sync_run: &mut Option<SyncRun>,
    alive: bool,
) {
    let Some(run) = sync_run.as_mut() else {
        return;
    };
    let snapshot = run.progress.borrow_and_update().clone();
    // Sin emisores no va a llegar nada más, así que un estado no terminal aquí
    // es todo lo que se va a saber: se cosecha igual. Volver sin cosechar
    // rearmaría el brazo sobre un `changed()` que devuelve `Err` al instante.
    if alive && !snapshot.state.is_terminal() {
        return;
    }
    let task_id = run.task.id();
    *sync_run = None;
    let report = backend.sync_report(task_id).await;
    let Some(view) = app.sync.as_mut() else {
        return;
    };
    // Las TRES reglas de este instante —el error de la Task manda sobre el del
    // informe, un informe que no llega es un fallo, y un estado no terminal
    // también— son de `norte_frontend::sync::SyncView::on_apply_ended`, la
    // COMPARTIDA con la GUI (#161). Estaban aquí, escritas a mano, y con la
    // segunda SIN aplicar: un `sync.report` que fallaba dejaba el modelo en
    // `Applying` y el pie diciendo «aplicando…» para siempre, con la única
    // explicación en una barra transitoria. Lo que se queda de este lado es la
    // única mitad que de verdad difiere entre frontends: cómo se sanea la
    // categoría y dónde se pinta.
    let category = view
        .on_apply_ended(&snapshot.state, report, norte_i18n::active())
        .map(|c| detail_for_bar(&c));
    view.error.clone_from(&category);
    if let Some(c) = category {
        app.message = Some(ta("sync-status-failed", &[("error", &c)]));
    }
}

/// Lo que una tecla SIGNIFICA en el panel de sincronización.
///
/// Separado del despacho por lo mismo que [`super::CompareKey`]: lo que se puede
/// equivocar aquí es la DECISIÓN, y una de ellas —aprobar— escribe en el disco
/// de alguien.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncKey {
    /// Nada que hacer con esta tecla.
    Ignore,
    /// Salir de norte (`Ctrl+C`, como en todos los demás overlays).
    Quit,
    /// Pedir la cancelación de la Task y quedarse.
    CancelTask,
    /// Cerrar el panel, cancelando la Task si sigue viva.
    Close,
    /// Mover el cursor por los pasos.
    Move(isize),
    /// Aprobar: la PRIMERA respuesta. Puede abrir la segunda pregunta.
    Approve,
    /// Contestar `sí` a la segunda pregunta.
    ConfirmYes,
    /// Cualquier otra tecla con la segunda pregunta en pantalla: cancela.
    ///
    /// Existe como variante propia y no como `Ignore` porque una pregunta a
    /// medio contestar tiene que resolverse: dejarla puesta mientras el cursor
    /// se mueve por debajo es cómo un `y` posterior aprueba otra cosa.
    ConfirmNo,
}

/// Traduce una tecla del panel de sincronización.
///
/// * `Ctrl+C` sale de norte, como en los otros diez overlays.
/// * El primer `Esc` sobre una Task viva la cancela; cualquier `Esc` posterior
///   cierra, sin mirar el estado de la Task — la misma salida de emergencia
///   que el panel de diferencias, por la misma razón.
/// * Con la segunda pregunta en pantalla el teclado se reduce a `y` y «no»:
///   `Ctrl+C` y `Esc` siguen valiendo (salir y cerrar no son respuestas a la
///   pregunta), y TODO lo demás la cancela en vez de ignorarse.
/// * `Enter` NO aprueba. Sincronizar borra y sobrescribe, así que se pide una
///   tecla que nadie pulsa por inercia — el mismo criterio que los diálogos
///   TOFU y la aprobación de una op de agente.
#[must_use]
pub fn sync_key(
    mods: crossterm::event::KeyModifiers,
    code: KeyCode,
    running: bool,
    cancel_requested: bool,
    confirming: bool,
) -> SyncKey {
    use crossterm::event::KeyModifiers as M;
    // Las DOS excepciones, y solo ellas: salir de norte y cerrar el panel no
    // son respuestas a la pregunta, así que valen con la pregunta puesta.
    if mods.contains(M::CONTROL) && code == KeyCode::Char('c') {
        return SyncKey::Quit;
    }
    if code == KeyCode::Esc {
        return if running && !cancel_requested {
            SyncKey::CancelTask
        } else {
            SyncKey::Close
        };
    }
    // Y la pregunta se resuelve ANTES que los filtros de modificador. Con
    // ellos delante, un `Ctrl+r` o un `Alt+e` de costumbre caían en `Ignore` y
    // dejaban «se van a borrar 2 árboles… ¿Seguir?» armada en pantalla,
    // esperando un `y` que ya no sabría a qué contesta.
    if confirming {
        return if mods.is_empty() && code == KeyCode::Char('y') {
            SyncKey::ConfirmYes
        } else {
            SyncKey::ConfirmNo
        };
    }
    if mods.intersects(M::CONTROL | M::ALT) {
        return SyncKey::Ignore;
    }
    match code {
        KeyCode::Up => SyncKey::Move(-1),
        KeyCode::Down => SyncKey::Move(1),
        KeyCode::PageUp => SyncKey::Move(-COMPARE_PAGE_STEP),
        KeyCode::PageDown => SyncKey::Move(COMPARE_PAGE_STEP),
        KeyCode::Char('a') => SyncKey::Approve,
        _ => SyncKey::Ignore,
    }
}

/// Despacha una tecla del panel de sincronización. Fijas, como las del panel
/// de diferencias y por lo mismo: no hay vocabulario `dialog.*` para «aprueba
/// este plan» y no es una pantalla del keymap propia.
pub fn on_sync_key(
    app: &mut App,
    sync_run: &mut Option<SyncRun>,
    mods: crossterm::event::KeyModifiers,
    code: KeyCode,
) {
    let Some(view) = app.sync.as_ref() else {
        return;
    };
    let running = view.run == crate::app::SyncRunState::Running;
    let action = sync_key(
        mods,
        code,
        running,
        view.cancel_requested,
        view.confirming.is_some(),
    );
    match action {
        SyncKey::Ignore => {}
        SyncKey::Quit => {
            if let Some(s) = sync_run.take() {
                s.task.cancel();
            }
            app.quit = true;
        }
        SyncKey::CancelTask => {
            if let Some(s) = sync_run.as_ref() {
                s.task.cancel();
            }
            if let Some(view) = app.sync.as_mut() {
                view.cancel_requested = true;
                // La pregunta se cae con la Task que la motivó: dejarla puesta
                // es cómo un `y` posterior aprueba otra cosa.
                view.confirming = None;
            }
        }
        SyncKey::Close => {
            // Se cancela SIEMPRE al salir, igual que en el panel de
            // diferencias: en remoto el daemon seguiría planificando —o
            // APLICANDO— para un panel que ya no existe.
            if let Some(s) = sync_run.take() {
                s.task.cancel();
            }
            app.close_sync();
        }
        SyncKey::Move(delta) => {
            if let Some(plan) = app.sync.as_mut().and_then(|v| v.state.plan_mut()) {
                plan.move_by(delta);
            }
        }
        SyncKey::Approve => approve_sync(app),
        SyncKey::ConfirmYes => {
            if let Some(view) = app.sync.as_mut() {
                view.confirming = None;
            }
            submit_sync(app);
        }
        SyncKey::ConfirmNo => {
            if let Some(view) = app.sync.as_mut() {
                view.confirming = None;
            }
        }
    }
}

/// `a` sobre un plan cerrado: o abre la segunda pregunta, o lo manda ya.
///
/// La segunda pregunta la decide el MODELO
/// ([`norte_frontend::sync::SyncPlan::confirmation`]), que la devuelve solo
/// cuando el plan borra árboles o cuando el undo no lo cubre entero. Preguntar
/// dos veces por un `Update` que se deshace del todo enseña a saltarse las dos.
pub fn approve_sync(app: &mut App) {
    let lang = norte_i18n::active();
    let Some(view) = app.sync.as_mut() else {
        return;
    };
    // `SyncView::can_approve` — que envuelve `SyncState::can_approve` y NUNCA
    // `SyncPlan::can_approve` — porque el segundo sigue contestando que sí
    // sobre un plan que ya se aprobó: `SyncState::plan()` devuelve el mismo
    // plan en `Applying` y en `Applied`, y ninguno de sus tres factores
    // cambia al gastarse. Con el del plan a secas, un `a` de más durante una
    // aplicación larga lanzaba un segundo `sync.apply` que el spool contesta
    // `PlanStale`, y el brazo de error pintaba «el plan falló» encima de una
    // sincronización que seguía ESCRIBIENDO; el `Esc` siguiente la cancelaba
    // a medias creyendo cerrar un fallo.
    if !view.can_approve() {
        app.message = Some(t("msg-sync-cannot-approve"));
        return;
    }
    let Some(plan) = view.state.plan() else {
        return;
    };
    match plan.confirmation(lang) {
        Some(c) => view.confirming = Some(c),
        None => submit_sync(app),
    }
}

/// Deja el `plan_hash` aprobado listo para que el run loop lo aplique.
///
/// Vuelve a preguntar por `can_approve`: entre la primera respuesta y la
/// segunda no ha llegado nada que pueda cambiarla —el modelo no retrocede—,
/// pero el hash sale de aquí hacia una escritura y no hay una segunda puerta
/// después de ésta.
pub fn submit_sync(app: &mut App) {
    // Por `SyncView::submit`, la ÚNICA puerta: mira `can_approve` y echa el
    // pestillo del apply en vuelo en el mismo gesto. Separarlos es lo que
    // dejaba la ventana que la GUI sí alcanzaba (revisión de rama de C2).
    let Some(view) = app.sync.as_mut() else {
        return;
    };
    let Some(hash) = view.submit() else {
        app.message = Some(t("msg-sync-cannot-approve"));
        return;
    };
    app.pending_sync_apply = Some(Box::new(hash));
}

/// Las teclas y los params de la SINCRONIZACIÓN (2026-08-11-directory-sync.md).
///
/// Sin backend y sin terminal: lo que se puede equivocar aquí es la DECISIÓN
/// —qué se sincroniza, en qué sentido, y cuántas veces se pregunta antes de
/// escribir—, y todo eso se afirma sobre un `App` y una función pura.
#[cfg(test)]
mod sync_tests {
    use super::{
        SyncKey, SyncRun, SyncTick, approve_sync, drain_sync_plan, harvest_sync_apply, on_sync_key,
        sync_key,
    };
    use crate::app::{App, Pane};
    use crossterm::event::{KeyCode, KeyModifiers as M};
    use norte_core::backend::TaskRef;
    use norte_proto::VPath;
    use norte_proto::methods::{
        CompareConfidence, CompareCriterion, CompareRow, CompareVerdict, PlanHash, Side,
        SyncCounts, SyncMode, SyncPlanDone,
    };

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire de test")
    }

    fn entry(wire: &str) -> norte_proto::Entry {
        norte_proto::Entry {
            attrs: std::collections::BTreeMap::new(),
            path: vp(wire),
            kind: norte_proto::EntryKind::File,
            size: Some(10),
            mtime_ms: None,
        }
    }

    /// Una fila emparejada, con entrada en los dos lados.
    fn par(id: u64, nombre: &str) -> CompareRow {
        CompareRow {
            id,
            left: Some(entry(&format!("file:///casa/{nombre}"))),
            right: Some(entry(&format!("file:///otro/{nombre}"))),
            verdict: CompareVerdict::Different,
            criterion: CompareCriterion::Size,
            confidence: CompareConfidence::Certain,
            newer: None,
            reason: None,
            side: None,
            paired_under: None,
        }
    }

    /// Un `App` con daemon (si no, todo se niega antes de mirar nada) y un
    /// panel de diferencias con `n` filas emparejadas.
    fn app_con_filas(n: u64) -> App {
        let mut app = App::new(
            Pane::new(vp("file:///casa"), Vec::new()),
            Pane::new(vp("file:///otro"), Vec::new()),
        );
        app.backend_journalled = true;
        let mut view =
            crate::app::CompareView::new(vp("file:///casa"), vp("file:///otro"), 0, None, None);
        view.pane
            .extend((1..=n).map(|i| par(i, &format!("f{i}.txt"))));
        app.compare = Some(view);
        app
    }

    fn marcar(app: &mut App, id: u64) {
        app.compare.as_mut().expect("panel").pane.toggle_mark(id);
    }

    /// Lo marcado en el panel de diferencias es lo que se sincroniza, y viaja
    /// como `include` — rutas RELATIVAS a la raíz, que es lo que el filtro del
    /// core compara.
    #[test]
    fn el_panel_siembra_include_con_lo_marcado() {
        let mut app = app_con_filas(3);
        marcar(&mut app, 1);
        marcar(&mut app, 2);
        let params = app.request_sync(SyncMode::Update).expect("params");
        let include = params.include.as_ref().expect("include");
        assert_eq!(include.len(), 2);
        let wire: Vec<String> = include
            .iter()
            .map(norte_proto::methods::RelPath::to_wire)
            .collect();
        assert_eq!(wire, vec!["f1.txt".to_owned(), "f2.txt".to_owned()]);
    }

    /// Sin marcas el plan cubre el árbol ENTERO, y eso es la AUSENCIA del
    /// campo: una lista vacía significaría un plan de cero pasos.
    #[test]
    fn sin_marcas_el_plan_cubre_el_arbol_entero() {
        let mut app = app_con_filas(3);
        let params = app.request_sync(SyncMode::Update).expect("params");
        assert!(params.include.is_none());
    }

    /// El sentido lo decide el LADO ACTIVO, y no se infiere de nada: `Tab` lo
    /// cambia y las dos raíces se intercambian enteras. Es la mitad de lo que
    /// el lector aprueba.
    #[test]
    fn el_lado_activo_decide_el_sentido_y_nada_se_infiere() {
        let mut app = app_con_filas(1);
        let a = app.request_sync(SyncMode::Update).expect("params").clone();
        app.compare.as_mut().expect("panel").pane.swap_active_side();
        let b = app.request_sync(SyncMode::Update).expect("params").clone();
        assert_eq!(a.source, b.dest);
        assert_eq!(a.dest, b.source);
        assert_eq!(
            app.compare.as_ref().expect("panel").pane.active_side(),
            Side::Right
        );
    }

    /// El modo viaja tal cual: `m` planifica un espejo, que además BORRA.
    #[test]
    fn el_modo_viaja_tal_cual() {
        let mut app = app_con_filas(1);
        assert_eq!(
            app.request_sync(SyncMode::Mirror).expect("params").mode,
            SyncMode::Mirror
        );
    }

    /// La TUI embebida no sincroniza, y lo DICE: `sync.apply` exige journal y
    /// se niega en cerrado (regla dura 4), así que planificar contra ella sería
    /// enseñar un plan que nadie puede aprobar. La frase es accionable —dice
    /// que hay que arrancar con `--daemon`—, no un «no soportado».
    #[test]
    fn sin_journal_no_se_planifica_y_se_dice_como_arreglarlo() {
        let mut app = app_con_filas(1);
        app.backend_journalled = false;
        assert!(app.request_sync(SyncMode::Update).is_none());
        assert!(app.pending_sync.is_none(), "no se lanza nada");
        let msg = app.message.clone().expect("una frase");
        assert_eq!(msg, norte_i18n::t("msg-sync-needs-daemon"));
        assert_ne!(msg, norte_i18n::t("err-unsupported"));
    }

    /// Las dos raíces en el mismo sitio se niegan aquí, sin ir y volver al
    /// daemon — igual que al comparar.
    #[test]
    fn las_dos_raices_en_el_mismo_sitio_se_niegan_aqui() {
        let mut app = App::new(
            Pane::new(vp("file:///casa"), Vec::new()),
            Pane::new(vp("file:///casa"), Vec::new()),
        );
        app.backend_journalled = true;
        assert!(app.request_sync(SyncMode::Update).is_none());
        assert!(app.message.is_some());
    }

    /// `Enter` NO aprueba. Sincronizar borra y sobrescribe, así que se pide una
    /// tecla que nadie pulsa por inercia — el mismo criterio que los diálogos
    /// TOFU y la aprobación de una op de agente.
    #[test]
    fn enter_no_aprueba_una_sincronizacion() {
        assert_eq!(
            sync_key(M::NONE, KeyCode::Enter, false, false, false),
            SyncKey::Ignore
        );
        assert_eq!(
            sync_key(M::NONE, KeyCode::Char('a'), false, false, false),
            SyncKey::Approve
        );
    }

    /// La tecla por defecto NO es una de función con modificador (#159: bajo
    /// tmux ninguna llega). Dentro del panel de diferencias son letras peladas.
    #[test]
    fn las_teclas_del_panel_no_son_de_funcion_con_modificador() {
        use crate::jobs::{CompareKey, compare_key};
        for (code, modo) in [
            (KeyCode::Char('s'), SyncMode::Update),
            (KeyCode::Char('m'), SyncMode::Mirror),
        ] {
            assert_eq!(
                compare_key(M::NONE, code, false, false),
                CompareKey::Sync(modo)
            );
        }
        // Y con modificador NO son nada: `Alt+s` no puede sincronizar por
        // accidente desde un panel cuyo teclado es entero suyo.
        assert_eq!(
            compare_key(M::ALT, KeyCode::Char('s'), false, false),
            CompareKey::Ignore
        );
    }

    /// Con la segunda pregunta en pantalla el teclado se reduce: `y` contesta
    /// que sí, `Esc` y `Ctrl+C` siguen valiendo porque no son respuestas a la
    /// pregunta, y TODO lo demás la cancela. Dejarla puesta mientras el cursor
    /// se mueve por debajo es cómo un `y` posterior aprueba otra cosa.
    #[test]
    fn la_segunda_pregunta_reduce_el_teclado() {
        assert_eq!(
            sync_key(M::NONE, KeyCode::Char('y'), false, false, true),
            SyncKey::ConfirmYes
        );
        for code in [KeyCode::Down, KeyCode::Char('a'), KeyCode::Enter] {
            assert_eq!(
                sync_key(M::NONE, code, false, false, true),
                SyncKey::ConfirmNo,
                "{code:?} dejó la pregunta a medias"
            );
        }
        assert_eq!(
            sync_key(M::NONE, KeyCode::Esc, false, false, true),
            SyncKey::Close
        );
        assert_eq!(
            sync_key(M::CONTROL, KeyCode::Char('c'), false, false, true),
            SyncKey::Quit
        );
    }

    /// La salida de emergencia del panel de diferencias, aquí también: el
    /// primer `Esc` sobre una Task viva la cancela y CUALQUIER `Esc` posterior
    /// cierra, sin mirar el estado de la Task. Sin esto, un daemon caído deja
    /// al lector encerrado en la pantalla desde la que se aprueban escrituras.
    #[test]
    fn el_segundo_esc_cierra_pase_lo_que_pase() {
        assert_eq!(
            sync_key(M::NONE, KeyCode::Esc, true, false, false),
            SyncKey::CancelTask
        );
        assert_eq!(
            sync_key(M::NONE, KeyCode::Esc, true, true, false),
            SyncKey::Close
        );
    }

    /// Todas las claves Fluent que este panel pinta existen en los DOS
    /// locales. Una que falte llega a la pantalla como su propio id, y este
    /// panel es donde se lee lo que se va a borrar.
    #[test]
    fn cada_cadena_del_panel_existe_en_ambos_locales() {
        for clave in [
            "sync-title",
            "sync-mode-update",
            "sync-mode-mirror",
            "sync-planning",
            "sync-empty",
            "sync-status-cancelled",
            "sync-status-failed",
            "sync-status-ready",
            "sync-status-not-approvable",
            "sync-status-applying",
            "sync-status-applied-undoable",
            "sync-status-applied-not-undoable",
            // Las cinco que este panel pinta DESDE que `sync_status_line`
            // delega en el `status_line` compartido y `mode_label` en el
            // compartido: la lista dejó de cubrir lo que la pantalla dice
            // (revisión de rama de C2, rust MINOR-5). Las cuatro de «cortada»
            // son justo las que esta rama añadió para que un run cortado no
            // se leyera como uno limpio.
            "sync-status-applied-cut-undoable",
            "sync-status-applied-cut-not-undoable",
            "sync-status-applied-failed-undoable",
            "sync-status-applied-failed-not-undoable",
            "sync-mode-unknown",
            "sync-hint",
            "sync-hint-done",
            "sync-hint-confirm",
            "sync-anchor-dest",
            "sync-anchor-either",
            "compare-marked",
            "msg-sync-needs-daemon",
            "msg-sync-too-many-marks",
            "msg-sync-cannot-approve",
            "reason-needs-daemon",
        ] {
            for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
                assert_ne!(
                    norte_i18n::t_in(lang, clave),
                    clave,
                    "falta {clave} en {lang:?}"
                );
            }
        }
    }

    /// Un `SyncRun` sintético: la Task la respalda un `watch` del propio test,
    /// así que se puede afirmar la lógica del run loop sin engine ni daemon
    /// (mismo molde que `compare_tests::run_con`).
    fn run_sync(
        state: norte_proto::TaskState,
        applying: bool,
    ) -> (
        SyncRun,
        tokio::sync::mpsc::Sender<norte_core::sync::SyncPlanEvent>,
        tokio::sync::watch::Sender<norte_proto::TaskProgress>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let id = norte_proto::TaskId::new(1);
        let (progreso, prx) = tokio::sync::watch::channel(norte_proto::TaskProgress {
            task_id: id,
            kind: norte_proto::TaskKind::Sync,
            state,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        });
        (
            SyncRun {
                task: TaskRef::synthetic_for_tests(id, prx.clone()),
                rx: (!applying).then_some(rx),
                progress: prx,
                applying,
            },
            tx,
            progreso,
        )
    }

    fn vista(app: &mut App, mode: SyncMode) {
        app.sync = Some(crate::app::SyncView::new(
            norte_proto::TaskId::new(1),
            mode,
            vp("file:///casa"),
            vp("file:///otro"),
            None,
            None,
        ));
    }

    /// Un cierre de plan de un paso, aprobable.
    fn cierre() -> SyncPlanDone {
        SyncPlanDone {
            task_id: norte_proto::TaskId::new(1),
            plan_hash: PlanHash::from_digest(&[3u8; 32]),
            counts: SyncCounts {
                copy: 1,
                bytes: 10,
                ..SyncCounts::default()
            },
            blockers: Vec::new(),
            blockers_total: 0,
            executable: true,
            dest_trash: norte_proto::methods::DestTrash::Restorable,
        }
    }

    fn paso() -> norte_proto::methods::SyncStep {
        norte_proto::methods::SyncStep {
            id: 1,
            kind: norte_proto::methods::SyncStepKind::Copy,
            rel: norte_proto::methods::RelPath::parse_wire("a.txt").expect("rel"),
            dest_rel: None,
            size: Some(10),
            criterion: CompareCriterion::Presence,
            confidence: CompareConfidence::Certain,
            reversal: Some(norte_proto::methods::StepReversal::Delete),
            reason: None,
        }
    }

    /// Un plan LISTO en el panel.
    fn app_con_plan_listo() -> App {
        let mut app = app_con_filas(1);
        vista(&mut app, SyncMode::Update);
        let view = app.sync.as_mut().expect("panel");
        view.state = norte_frontend::sync::SyncState::ready(vec![paso()], cierre());
        view.run = crate::app::SyncRunState::Done;
        app
    }

    /// **La regresión del BLOCKER.** Un `a` de más sobre un plan que ya se
    /// aprobó no puede volver a mandarlo.
    ///
    /// `SyncPlan::can_approve` sigue contestando que sí para siempre —sus tres
    /// factores no cambian al gastarse el plan— y `SyncState::plan()` devuelve
    /// el mismo plan en `Applying` y en `Applied`. Con esa comprobación a
    /// secas, un `a` durante una aplicación larga lanzaba un segundo
    /// `sync.apply` que el spool contesta `PlanStale`, el brazo de error
    /// pintaba «el plan falló» encima de una sincronización que seguía
    /// ESCRIBIENDO, y como eso deja `run != Running` el `Esc` siguiente la
    /// cancelaba a medias creyendo cerrar un fallo.
    #[test]
    fn aprobar_dos_veces_no_manda_el_plan_dos_veces() {
        let mut app = app_con_plan_listo();
        approve_sync(&mut app);
        assert!(app.pending_sync_apply.is_some(), "la primera sí manda");
        // El run loop se lo lleva y la Task arranca.
        app.pending_sync_apply = None;
        app.sync
            .as_mut()
            .expect("panel")
            .state
            .on_apply_started(norte_proto::TaskId::new(9));

        approve_sync(&mut app);
        assert!(
            app.pending_sync_apply.is_none(),
            "un segundo `a` mandó el mismo hash otra vez"
        );
        assert_eq!(app.message, Some(norte_i18n::t("msg-sync-cannot-approve")));
    }

    /// Y tampoco después, con el informe ya en pantalla: pisar la línea que
    /// dice qué se puede deshacer con un «el plan falló» destruye lo único que
    /// queda escrito sobre el asunto.
    #[test]
    fn aprobar_un_plan_ya_aplicado_no_hace_nada() {
        let mut app = app_con_plan_listo();
        {
            let view = app.sync.as_mut().expect("panel");
            view.state.on_apply_started(norte_proto::TaskId::new(9));
            view.state
                .on_report(norte_proto::methods::SyncReportResult {
                    done: 1,
                    failed: 0,
                    skipped: 0,
                    bytes: 10,
                    failures: Vec::new(),
                    batch_id: Some(1),
                    dest_trash: norte_proto::methods::DestTrash::Restorable,
                });
        }
        approve_sync(&mut app);
        assert!(app.pending_sync_apply.is_none());
    }

    /// La pregunta a medio contestar se resuelve con CUALQUIER tecla, también
    /// con las que llevan modificador.
    ///
    /// Con el filtro de modificadores delante, un `Ctrl+r` o un `Alt+e` de
    /// costumbre caían en `Ignore` y dejaban «se van a borrar 2 árboles…»
    /// armada en pantalla, esperando un `y` que ya no sabría a qué contesta.
    #[test]
    fn un_modificador_tambien_cancela_la_segunda_pregunta() {
        for (mods, code) in [
            (M::CONTROL, KeyCode::Char('r')),
            (M::ALT, KeyCode::Char('e')),
            (M::SHIFT, KeyCode::Char('Y')),
        ] {
            assert_eq!(
                sync_key(mods, code, false, false, true),
                SyncKey::ConfirmNo,
                "{mods:?}+{code:?} dejó la pregunta armada"
            );
        }
        // Y `y` con modificador NO es un sí: el sí es la tecla pelada.
        assert_eq!(
            sync_key(M::CONTROL, KeyCode::Char('y'), false, false, true),
            SyncKey::ConfirmNo
        );
    }

    /// Cancelar la Task suelta también la pregunta: dejarla puesta mientras la
    /// sincronización que la motivó se para es cómo un `y` posterior aprueba
    /// otra cosa.
    #[test]
    fn cancelar_suelta_la_segunda_pregunta() {
        let mut app = app_con_plan_listo();
        {
            let view = app.sync.as_mut().expect("panel");
            view.run = crate::app::SyncRunState::Running;
            view.confirming = Some(norte_frontend::sync::Confirmation {
                id: "sync-confirm-delete",
                text: "¿?".to_owned(),
            });
        }
        let (run, _tx, _prog) = run_sync(norte_proto::TaskState::Running, true);
        let mut sync_run = Some(run);
        on_sync_key(&mut app, &mut sync_run, M::NONE, KeyCode::Esc);
        let view = app.sync.as_ref().expect("el panel sigue abierto");
        assert!(view.cancel_requested);
        assert!(view.confirming.is_none(), "la pregunta sobrevivió al Esc");
    }

    /// El segundo `Esc` cierra y CANCELA: en remoto el daemon seguiría
    /// aplicando para un panel que ya no existe.
    #[test]
    fn el_segundo_esc_cierra_y_cancela_la_task() {
        let mut app = app_con_plan_listo();
        app.sync.as_mut().expect("panel").cancel_requested = true;
        let (run, _tx, _prog) = run_sync(norte_proto::TaskState::Running, true);
        let canceller = run.task.canceller();
        let mut sync_run = Some(run);
        on_sync_key(&mut app, &mut sync_run, M::NONE, KeyCode::Esc);
        assert!(app.sync.is_none(), "el panel se cierra");
        assert!(sync_run.is_none(), "y el run se suelta");
        drop(canceller);
    }

    /// Un lote con el panel ya cerrado cancela la Task en vez de seguir
    /// recibiendo pasos que nadie va a mirar.
    #[test]
    fn un_lote_con_el_panel_cerrado_cosecha_el_run() {
        let mut app = app_con_filas(1);
        app.sync = None;
        let (run, _tx, _prog) = run_sync(norte_proto::TaskState::Running, false);
        let mut sync_run = Some(run);
        drain_sync_plan(&mut app, &mut sync_run, None);
        assert!(sync_run.is_none(), "el run se suelta con el panel cerrado");
    }

    /// Cerrarse el flujo del plan deja `rx` a `None`: un canal cerrado
    /// devolvería `None` en bucle y el brazo del `select!` giraría.
    #[test]
    fn el_fin_del_flujo_desarma_el_brazo_del_plan() {
        let mut app = app_con_filas(1);
        vista(&mut app, SyncMode::Update);
        let (run, _tx, prog) = run_sync(norte_proto::TaskState::Running, false);
        let mut sync_run = Some(run);
        prog.send_modify(|p| p.state = norte_proto::TaskState::Completed);
        drain_sync_plan(&mut app, &mut sync_run, None);
        let run = sync_run.as_ref().expect("el run se conserva para el Esc");
        assert!(run.rx.is_none(), "el brazo se desarma al cerrarse el canal");
        assert_eq!(
            app.sync.as_ref().expect("panel").run,
            crate::app::SyncRunState::Done
        );
    }

    /// Un plan CANCELADO se dice cancelado, y no «hecho»: sin
    /// `sync.plan_done` no hay `plan_hash`, así que no hay nada que aprobar.
    #[test]
    fn un_plan_cancelado_se_dice_cancelado() {
        let mut app = app_con_filas(1);
        vista(&mut app, SyncMode::Update);
        let (run, _tx, prog) = run_sync(norte_proto::TaskState::Running, false);
        let mut sync_run = Some(run);
        prog.send_modify(|p| p.state = norte_proto::TaskState::Cancelled);
        drain_sync_plan(&mut app, &mut sync_run, None);
        assert_eq!(
            app.sync.as_ref().expect("panel").run,
            crate::app::SyncRunState::Cancelled
        );
        assert!(!app.sync.as_ref().expect("panel").state.can_approve());
    }

    /// Con los emisores del progreso caídos y un estado NO terminal, la
    /// aplicación se cosecha igual y se cuenta como fallo.
    ///
    /// Volver sin cosechar rearmaría el brazo sobre un `changed()` que
    /// devuelve `Err` al instante — un giro—, y llamarlo «hecho» sería decir
    /// que una sincronización a medias terminó bien.
    #[tokio::test]
    async fn un_emisor_caido_cosecha_la_aplicacion_como_fallo() {
        let mut app = app_con_plan_listo();
        app.sync
            .as_mut()
            .expect("panel")
            .state
            .on_apply_started(norte_proto::TaskId::new(9));
        let engine = norte_core::Engine::new();
        let backend = norte_core::backend::Backend::Embedded(std::sync::Arc::new(engine));
        let (run, _tx, prog) = run_sync(norte_proto::TaskState::Running, true);
        let mut sync_run = Some(run);
        drop(prog);
        harvest_sync_apply(&mut app, &backend, &mut sync_run, false).await;
        assert!(sync_run.is_none(), "el run se suelta");
        assert_eq!(
            app.sync.as_ref().expect("panel").run,
            crate::app::SyncRunState::Failed
        );
    }

    /// Y una Task que TERMINÓ pide su informe pase lo que pase, cancelada
    /// incluida: lo aplicado hasta el corte se queda, journalizado, y media
    /// sincronización es un estado real que el lector tiene que poder ver.
    #[tokio::test]
    async fn una_aplicacion_cancelada_pide_su_informe() {
        let mut app = app_con_plan_listo();
        app.sync
            .as_mut()
            .expect("panel")
            .state
            .on_apply_started(norte_proto::TaskId::new(9));
        let engine = norte_core::Engine::new();
        let backend = norte_core::backend::Backend::Embedded(std::sync::Arc::new(engine));
        let (run, _tx, _prog) = run_sync(norte_proto::TaskState::Cancelled, true);
        let mut sync_run = Some(run);
        harvest_sync_apply(&mut app, &backend, &mut sync_run, true).await;
        assert!(sync_run.is_none());
        // El engine embebido no tiene ese informe, así que la barra lo dice —
        // lo que se afirma es que se PIDIÓ y que el estado terminal se pintó.
        assert_eq!(
            app.sync.as_ref().expect("panel").run,
            crate::app::SyncRunState::Cancelled
        );
        assert!(app.message.is_some());
    }

    /// El `SyncTick` existe porque `select!` no deja tomar prestado `sync_run`
    /// dos veces; que sus dos variantes sean fases SUCESIVAS —nunca hay plan y
    /// aplicación a la vez— es lo que hace correcto el brazo único.
    #[test]
    fn el_tick_distingue_las_dos_fases() {
        let (plan, _tx, _prog) = run_sync(norte_proto::TaskState::Running, false);
        assert!(plan.rx.is_some() && !plan.applying);
        let (aplicando, _tx2, _prog2) = run_sync(norte_proto::TaskState::Running, true);
        assert!(aplicando.rx.is_none() && aplicando.applying);
        assert_ne!(
            std::mem::discriminant(&SyncTick::Plan(None)),
            std::mem::discriminant(&SyncTick::Applied { alive: true })
        );
    }

    /// Una marca que es una de las dos RAÍCES se niega en vez de mandarse: la
    /// raíz en un `include` significa «todo», así que una sola convertiría una
    /// selección estrecha en un plan del árbol entero — bajo `Mirror`, en
    /// «borra del destino todo lo que el origen no tenga».
    #[test]
    fn una_marca_que_es_la_raiz_se_niega() {
        let mut app = app_con_filas(1);
        {
            let view = app.compare.as_mut().expect("panel");
            view.pane.extend(vec![CompareRow {
                id: 99,
                left: Some(entry("file:///casa")),
                right: None,
                verdict: CompareVerdict::OnlyLeft,
                criterion: CompareCriterion::Presence,
                confidence: CompareConfidence::Certain,
                newer: None,
                reason: None,
                side: None,
                paired_under: None,
            }]);
            view.pane.toggle_mark(99);
        }
        assert!(app.request_sync(SyncMode::Mirror).is_none());
        assert_eq!(
            app.message,
            Some(norte_i18n::t("msg-sync-mark-is-the-root"))
        );
    }

    /// Y una que no cuelga de ninguna de las dos se niega también, en vez de
    /// caerse: `include` con la lista encogida a cero es un plan de cero pasos,
    /// que el panel pinta como «los dos árboles ya coinciden» — una mentira en
    /// la pantalla que autoriza escrituras.
    #[test]
    fn una_marca_fuera_de_las_dos_raices_se_niega() {
        let mut app = app_con_filas(1);
        {
            let view = app.compare.as_mut().expect("panel");
            view.pane.extend(vec![CompareRow {
                id: 98,
                left: Some(entry("file:///otra-parte/x.txt")),
                right: None,
                verdict: CompareVerdict::OnlyLeft,
                criterion: CompareCriterion::Presence,
                confidence: CompareConfidence::Certain,
                newer: None,
                reason: None,
                side: None,
                paired_under: None,
            }]);
            view.pane.toggle_mark(98);
        }
        assert!(app.request_sync(SyncMode::Update).is_none());
        assert_eq!(
            app.message,
            Some(norte_i18n::t("msg-sync-mark-outside-roots"))
        );
    }
}
