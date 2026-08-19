//! Las tres tareas largas que un panel enseña mientras corren: buscar,
//! comparar y sincronizar.
//!
//! Las tres tienen la misma forma —se lanzan, van llegando por un canal que se
//! drena, y mientras viven su panel se come el teclado con una tabla de teclas
//! propia— y las tres vivían en el root del binario `ntc`, un crate DISTINTO de
//! esta lib.
//!
//! Hay un ciclo entre este módulo y [`crate::navigate`] —el `cd` tiene que
//! soltar una búsqueda viva al salir del pane virtual, y lanzar una búsqueda
//! necesita el `cd`—. Un ciclo entre módulos del MISMO crate es legal en Rust,
//! así que el orden de salida no importa; lo que no se podía es dejar una mitad
//! en el binario, que sí es otro crate.
//!
//! Los tres `*Run` son el asa: la Task cancelable (regla 3), el canal, y la
//! generación con la que un lote que llega tarde se descarta en vez de mezclarse
//! con el plan siguiente.

use crossterm::event::{EventStream, KeyCode, KeyModifiers};
use norte_core::backend::{Backend, TaskRef};
use norte_frontend::layout::BySlot;
use norte_i18n::{t, ta};
use norte_proto::VPath;
use norte_proto::methods::{FsSearchParams, SearchHits};

use crate::app::{
    App, CompareState, SearchDialog, SearchState, detail_for_bar, error_category, error_message,
};
use crate::fill::Fill;
use crate::navigate::{apply_cd, cd};
use crate::probes::{DecorateFetch, Probed};

/// Una búsqueda viva EN CURSO (`Alt+F7`, liveSearch T6): la Task cancelable,
/// el canal de lotes de hits y el pane virtual que los muestra. Molde `Fill`:
/// vive en el run loop, se drena en el `select!` y se suelta al salir del modo
/// virtual (un `cd`) cancelando la Task (regla 3).
pub struct SearchRun {
    /// Task de `fs.search` (cancelable con `TaskRef::cancel`).
    pub task: TaskRef,
    /// Canal de lotes de hits (embebido: lo cierra el walker; remoto: la
    /// bomba del `RemoteBackend` lo cierra al terminal).
    pub rx: tokio::sync::mpsc::Receiver<SearchHits>,
    /// Pane que muestra los hits (índice en `App::panes`).
    pub pane: usize,
    /// Directorio ANTERIOR del pane, para restaurarlo al salir del modo
    /// virtual (Esc tras terminar).
    pub prev_dir: VPath,
    /// Hits acumulados (== `panes[pane].entries().len()`, contador propio para
    /// no depender del re-sort del pane).
    pub hits: usize,
    /// Estado del run: `Running` mientras el walker emite; terminal tras
    /// cerrarse el canal (se lee del `TaskProgress`).
    pub state: SearchState,
}

/// Tope por defecto de hits de una búsqueda viva (`Alt+F7`, liveSearch T6):
/// el diálogo v1 no expone el campo, así que se fija un tope razonable —
/// acota la memoria del pane virtual (los hits se acumulan en `entries`) y
/// hace alcanzable el estado `Truncated`. Al llegar, la Task completa y la
/// barra pinta «truncada».
pub const SEARCH_MAX_HITS: u32 = 10_000;

/// Una comparación de directorios EN CURSO (`Shift+F2`,
/// 2026-08-11-directory-comparison.md): la Task cancelable y el canal de lotes
/// de filas. Mismo molde que [`SearchRun`] — vive en el run loop, se drena en
/// el `select!` y se suelta al cerrarse el panel, cancelando la Task (regla 3).
pub struct CompareRun {
    /// Task de `fs.compare` (cancelable con `TaskRef::cancel`).
    pub task: TaskRef,
    /// Canal de lotes de filas (embebido: lo cierra el walk; remoto: la bomba
    /// del `RemoteBackend` lo cierra al terminal).
    pub rx: tokio::sync::mpsc::Receiver<norte_proto::methods::CompareRowsBatch>,
    /// Filas RECIBIDAS. Contador propio y no `pane.len()` por lo mismo que en
    /// la búsqueda: no depender de lo que el modelo haga con ellas.
    pub rows: usize,
    /// Estado del run; terminal tras cerrarse el canal.
    pub state: CompareState,
}

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
        vivo: bool,
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

/// Traduce una tecla del diálogo de búsqueda (`Alt+F7`, liveSearch T6) a un
/// efecto sobre `App::search_dialog`. Teclas fijas como los demás overlays
/// (#24); `ctrl+c` conserva su salida global. Devuelve `Some(params)` SOLO
/// cuando Enter con algún criterio no vacío debe LANZAR la búsqueda (el caller
/// cierra el diálogo y abre el pane virtual); Enter sin criterio avisa y sigue.
pub fn on_search_dialog_key(
    app: &mut App,
    mods: KeyModifiers,
    code: KeyCode,
) -> Option<FsSearchParams> {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return None;
    }
    let dialog = app.search_dialog.as_mut()?;
    // SHIFT pasa (mayúsculas/símbolos llegan como Char+SHIFT); ctrl/alt no
    // escriben en los campos.
    let plain = mods.is_empty() || mods == KeyModifiers::SHIFT;
    match code {
        // Toggles/Tab exigen `plain` (sin ctrl/alt): un Ctrl+F2 no togglea
        // (review MINOR-4), igual que el resto de la captura del diálogo.
        KeyCode::F(2) if plain => dialog.toggle_regex(),
        KeyCode::F(3) if plain => dialog.toggle_case(),
        KeyCode::Tab if plain => dialog.toggle_field(),
        KeyCode::Char(c) if plain => dialog.push_char(c),
        KeyCode::Backspace if plain => dialog.backspace(),
        KeyCode::Esc => app.search_dialog = None,
        KeyCode::Enter => {
            if dialog.has_criteria() {
                let root = app.focused().dir().clone();
                return Some(search_params(app.search_dialog.as_ref()?, root));
            }
            // Ambos campos vacíos: no-op con aviso (una búsqueda sin criterio
            // no tiene sentido). El diálogo sigue abierto.
            app.message = Some(t("search-empty"));
        }
        _ => {}
    }
    None
}

/// Construye los [`FsSearchParams`] del diálogo: el toggle `regex` decide, por
/// eje, `name_glob` vs `name_regex` y `content` vs `content_regex`; un campo
/// vacío no aporta criterio. `max_hits` se fija al tope por defecto
/// ([`SEARCH_MAX_HITS`]) — el diálogo v1 no lo expone.
#[must_use]
pub fn search_params(dialog: &SearchDialog, root: VPath) -> FsSearchParams {
    let (name_glob, name_regex) = match (dialog.name.is_empty(), dialog.regex) {
        (true, _) => (None, None),
        (false, false) => (Some(dialog.name.clone()), None),
        (false, true) => (None, Some(dialog.name.clone())),
    };
    let (content, content_regex) = match (dialog.content.is_empty(), dialog.regex) {
        (true, _) => (None, None),
        (false, false) => (Some(dialog.content.clone()), None),
        (false, true) => (None, Some(dialog.content.clone())),
    };
    FsSearchParams {
        root,
        name_glob,
        name_regex,
        content,
        content_regex,
        case_sensitive: dialog.case,
        max_hits: Some(SEARCH_MAX_HITS),
    }
}

/// Lanza la búsqueda: `backend.search` → Err deja el diálogo abierto y avisa
/// por la barra (`search-status-failed`); Ok cierra el diálogo, guarda el dir
/// anterior, arranca el pane virtual y registra el [`SearchRun`] (cancelando
/// uno previo — el pane virtual es uno).
pub async fn launch_search(
    app: &mut App,
    backend: &Backend,
    fill: &mut BySlot<Fill>,
    search_run: &mut Option<SearchRun>,
    params: FsSearchParams,
) {
    let pane = app.focus();
    let root = params.root.clone();
    match backend.search(params).await {
        Ok((task, rx)) => {
            // `prev_dir` y `root` son el MISMO directorio: la raíz sale de
            // `app.focused().dir()` en el Enter del diálogo. `back_target` se
            // apoya en esa igualdad — el `dir()` de un pane virtual es lo que
            // deja en la rama de delante, y solo es honesto porque es el sitio
            // donde el lector estaba. Si algún día la raíz se puede teclear,
            // el rastro necesita `prev_dir`, no `dir()`.
            let prev_dir = app.panes[pane].dir().clone();
            app.search_dialog = None;
            app.message = None;
            app.panes[pane].begin_search(root);
            // El pane pasa a virtual: un relleno paginado en vuelo de ESTE
            // pane (dir aún cargándose) alimentaría el listado real como hits
            // (review MAJOR T6) — se suelta ya (tirante; `apply_fill_msg` es
            // el cinturón por si llega un lote antes).
            fill.remove(app.panes.slot_of(pane));
            // Un run previo (raro: el diálogo se cierra al lanzar) se cancela.
            if let Some(old) = search_run.replace(SearchRun {
                task,
                rx,
                pane,
                prev_dir,
                hits: 0,
                state: SearchState::Running,
            }) {
                old.task.cancel();
            }
        }
        // Criterios inválidos u otro fallo del daemon/engine: el diálogo
        // SIGUE abierto (el usuario corrige) y el detalle va saneado a la
        // barra (categoría del error — jamás el patrón crudo).
        Err(e) => {
            app.message = Some(ta(
                "search-status-failed",
                &[("error", &detail_for_bar(&error_category(&e)))],
            ));
        }
    }
}

/// Aplica un lote de hits (o el cierre del canal) al pane virtual. Un lote
/// llega mientras el pane siga en modo virtual; si un `cd` lo apagó, se suelta
/// el run (su drenador alimentaría un listado real) cancelando la Task.
/// `None` = fin del stream: se lee el estado terminal y se refleja en el pane.
pub fn drain_search(app: &mut App, search_run: &mut Option<SearchRun>, hits: Option<SearchHits>) {
    let Some(s) = search_run else {
        return;
    };
    if let Some(batch) = hits {
        if app.panes[s.pane].virtual_search {
            // #81: el contexto del match (línea + preview, saneado EN ORIGEN
            // por el core) se guarda por path — la barra lo pinta para el
            // hit bajo el cursor. Vista del pane sigue plana (v1).
            if let Some(infos) = batch.matches {
                // Contrato del wire: alineado 1:1. Un server bug que mande
                // menos matches truncaría el zip EN SILENCIO — ruido en dev.
                debug_assert_eq!(batch.entries.len(), infos.len(), "matches desalineados");
                for (e, info) in batch.entries.iter().zip(infos) {
                    app.panes[s.pane]
                        .search_matches
                        .insert(e.path.clone(), info);
                }
            }
            let n = batch.entries.len();
            app.panes[s.pane].extend_listing(batch.entries);
            s.hits += n;
        } else {
            s.task.cancel();
            *search_run = None;
        }
    } else {
        // Canal cerrado: el walker terminó. Estado terminal no bloqueante.
        let state = finalize_search_state(s);
        s.state = state;
        app.panes[s.pane].search_state = state;
        // El detalle concreto va a la barra UNA vez (error_message); el pane
        // guarda la CATEGORÍA para pintar `search-status-failed` de forma
        // persistente tras limpiarse el mensaje (review MINOR-2).
        if state == SearchState::Failed {
            let mut rx = s.task.progress();
            if let norte_proto::TaskState::Failed { error } = rx.borrow_and_update().state.clone() {
                app.panes[s.pane].search_error = Some(error_category(&error));
                app.message = Some(error_message(&error));
            }
        }
    }
}

/// Lanza `fs.compare` sobre los dos panes y abre el panel de diferencias.
///
/// Los params ya vienen resueltos y validados por
/// [`App::request_compare`](crate::app::App::request_compare) — este lado
/// solo es dueño del canal y de la Task. Un panel anterior se reemplaza y su
/// Task se cancela (regla 3): dos comparaciones a la vez serían dos flujos
/// alimentando un solo panel.
pub async fn launch_compare(
    app: &mut App,
    backend: &Backend,
    compare_run: &mut Option<CompareRun>,
    params: norte_proto::methods::FsCompareParams,
) {
    let (left_root, right_root) = (params.left.clone(), params.right.clone());
    match backend.compare(params).await {
        Ok((task, rx)) => {
            app.message = None;
            // Las dos reinterpretaciones (#57) salen de los dos panes de
            // los que salieron las raíces, en ese mismo orden.
            let izq = app.focus();
            let (left_encoding, right_encoding) = (
                app.panes[izq].name_encoding(),
                app.panes[izq ^ 1].name_encoding(),
            );
            app.compare = Some(crate::app::CompareView::new(
                left_root,
                right_root,
                izq,
                left_encoding,
                right_encoding,
            ));
            // #157: la caché de tamaños hidratados y su dedup son de ESTA
            // comparación — una nueva empieza sin nada pedido, igual que
            // `last_probed` se vacía con cada listado nuevo. Y avanza la
            // generación (#198): una sonda de la comparación anterior sigue en
            // vuelo, y sin la marca aterrizaría en estas tablas recién
            // vaciadas.
            app.begin_compare_generation();
            if let Some(old) = compare_run.replace(CompareRun {
                task,
                rx,
                rows: 0,
                state: CompareState::Running,
            }) {
                old.task.cancel();
            }
        }
        // El panel NO se abre: un panel vacío que dice «fallo» es peor que la
        // frase en la barra, porque además hay que cerrarlo. La categoría va
        // saneada, jamás el error crudo del provider.
        Err(e) => {
            app.message = Some(ta(
                "compare-status-failed",
                &[("error", &detail_for_bar(&error_category(&e)))],
            ));
        }
    }
}

/// Aplica un lote de filas (o el cierre del canal) al panel de diferencias.
///
/// `None` = fin del flujo. Y ahí está la diferencia con la búsqueda, que es lo
/// que C6 descubrió y el plan no dice: **que el canal se cierre NO significa
/// que hayan llegado todas las filas**. La bomba de filas y la del snapshot
/// terminal son tasks independientes — pero la CUENTA que decide `Done` contra
/// [`CompareState::Incomplete`] ya no vive aquí (#158), y desde la revisión
/// de rama (MAJOR-1) tampoco vive aquí el MAPEO entero: los cuatro brazos son
/// [`norte_frontend::compare::CompareView::finish_from_task`], que es lo que
/// también llama la GUI. Lo que queda de este lado es el aviso PASAJERO de la
/// barra, que la GUI no tiene.
pub fn drain_compare(
    app: &mut App,
    compare_run: &mut Option<CompareRun>,
    batch: Option<norte_proto::methods::CompareRowsBatch>,
) {
    let Some(c) = compare_run else {
        return;
    };
    if let Some(b) = batch {
        // El panel se cerró bajo el drenador: se suelta el run cancelando la
        // Task, igual que hace la búsqueda al salir del modo virtual.
        let Some(view) = app.compare.as_mut() else {
            c.task.cancel();
            *compare_run = None;
            return;
        };
        c.rows += b.rows.len();
        view.pane.extend(b.rows);
    } else {
        let mut rx = c.task.progress();
        let snapshot = rx.borrow_and_update().clone();
        let expected = snapshot.entries_done;
        if let Some(view) = app.compare.as_mut() {
            // El mapeo entero —los cuatro brazos— es del modelo. Aquí solo
            // queda el aviso PASAJERO de la barra, que es lo único que esta
            // superficie tiene y la GUI no.
            let aviso = view
                .finish_from_task(
                    &snapshot.state,
                    expected,
                    c.rows as u64,
                    norte_i18n::active(),
                )
                .map(error_message);
            c.state = view.state;
            if let Some(m) = aviso {
                app.message = Some(m);
            }
        } else {
            // El panel ya se cerró: no hay nada que pintar, y el único uso
            // de `c.state` es una comprobación de `== Running` (aquí abajo
            // en `on_compare_key`, y en el `select!` del run loop) —
            // cualquier variante terminal le sirve, así que no se vuelve a
            // decidir cuál (era una CUARTA copia de la cuenta `Completed` vs
            // `Incomplete`, y la única que nadie podía ver equivocarse).
            c.state = CompareState::Done;
        }
    }
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
            let adoptada = app
                .sync
                .as_mut()
                .is_some_and(|view| view.on_apply_started(task.id()));
            if !adoptada {
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
                let categoria = detail_for_bar(&error_category(&error));
                view.error = Some(categoria.clone());
                app.message = Some(ta("sync-status-failed", &[("error", &categoria)]));
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
    vivo: bool,
) {
    let Some(run) = sync_run.as_mut() else {
        return;
    };
    let snapshot = run.progress.borrow_and_update().clone();
    // Sin emisores no va a llegar nada más, así que un estado no terminal aquí
    // es todo lo que se va a saber: se cosecha igual. Volver sin cosechar
    // rearmaría el brazo sobre un `changed()` que devuelve `Err` al instante.
    if vivo && !snapshot.state.is_terminal() {
        return;
    }
    let task_id = run.task.id();
    *sync_run = None;
    let informe = backend.sync_report(task_id).await;
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
    let categoria = view
        .on_apply_ended(&snapshot.state, informe)
        .map(|c| detail_for_bar(&c));
    view.error.clone_from(&categoria);
    if let Some(c) = categoria {
        app.message = Some(ta("sync-status-failed", &[("error", &c)]));
    }
}

/// Cuántas filas mueve una página en el panel de diferencias.
///
/// Una constante y no el alto del frame: el layout del panel lo decide
/// `ui::draw` y todavía no devuelve su geometría al run loop como hace
/// `pane_geometry` para los panes. Diez filas es lo que un `PageDown` recorre
/// en un pane de altura media, así que el error es de RECORRIDO y no de
/// corrección — el cursor nunca sale de la lista, porque `move_by` clampa.
pub const COMPARE_PAGE_STEP: isize = 10;

/// Lo que una tecla SIGNIFICA en el panel de diferencias.
///
/// Separado del despacho para poder afirmarlo sin un backend ni un terminal:
/// lo que se puede equivocar aquí es la DECISIÓN —y una de ellas, la salida,
/// es la diferencia entre un overlay y una trampa—, no el `cd` que viene
/// después.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareKey {
    /// Nada que hacer con esta tecla.
    Ignore,
    /// Salir de norte (`Ctrl+C`, como en todos los demás overlays).
    Quit,
    /// Pedir la cancelación de la Task y quedarse: las filas ya llegadas se
    /// conservan.
    CancelTask,
    /// Cerrar el panel, cancelando la Task si sigue viva.
    Close,
    /// Cambiar el lado activo.
    SwapSide,
    /// Mover el cursor.
    Move(isize),
    /// Al principio de lo visible.
    First,
    /// Al final de lo visible.
    Last,
    /// Alternar el filtro de la categoría `n` de `CATEGORIES`.
    Filter(usize),
    /// Ir a donde vive la fila.
    Open,
    /// Marcar o desmarcar la fila del cursor: lo que marque siembra el
    /// `include` del plan.
    Mark,
    /// Planificar una sincronización en este modo.
    Sync(norte_proto::methods::SyncMode),
}

/// Traduce una tecla del panel de diferencias.
///
/// * `Ctrl+C` sale de norte. Este panel se queda el teclado ENTERO y `ISIG`
///   está apagado en modo raw, así que sin esto era la única pantalla de
///   norte de la que no se sale (review BLOCKER-1); los otros nueve overlays
///   lo atienden igual.
/// * El primer `Esc` sobre una comparación viva la cancela y conserva sus
///   filas; **cualquier `Esc` posterior cierra**, sin mirar el estado de la
///   Task. Condicionar el cierre a un estado terminal dejaba encerrado al
///   lector cuando el canal de filas no llegaba a cerrarse nunca — un daemon
///   caído, un provider colgado en una NFS muerta.
/// * Cualquier otro modificador no pinta nada: sin ese filtro `Alt+1`
///   togglea un filtro y `Alt+Tab` cambia de lado.
#[must_use]
pub fn compare_key(
    mods: crossterm::event::KeyModifiers,
    code: KeyCode,
    running: bool,
    cancel_requested: bool,
) -> CompareKey {
    use crossterm::event::KeyModifiers as M;
    if mods.contains(M::CONTROL) {
        return if code == KeyCode::Char('c') {
            CompareKey::Quit
        } else {
            CompareKey::Ignore
        };
    }
    if mods.contains(M::ALT) {
        return CompareKey::Ignore;
    }
    match code {
        KeyCode::Esc => {
            if running && !cancel_requested {
                CompareKey::CancelTask
            } else {
                CompareKey::Close
            }
        }
        KeyCode::Tab => CompareKey::SwapSide,
        KeyCode::Up => CompareKey::Move(-1),
        KeyCode::Down => CompareKey::Move(1),
        KeyCode::PageUp => CompareKey::Move(-COMPARE_PAGE_STEP),
        KeyCode::PageDown => CompareKey::Move(COMPARE_PAGE_STEP),
        KeyCode::Home => CompareKey::First,
        KeyCode::End => CompareKey::Last,
        // El rango del patrón es exactamente el de `CATEGORIES`, así que el
        // índice no puede salirse.
        KeyCode::Char(c @ '1'..='5') => CompareKey::Filter(c as usize - '1' as usize),
        KeyCode::Enter => CompareKey::Open,
        KeyCode::Insert => CompareKey::Mark,
        // LETRAS PELADAS, y eso es la decisión (#159): bajo tmux ninguna tecla
        // de función con modificador llega, así que un `Shift+F5` aquí sería
        // un atajo documentado y muerto — este repo ya envió uno. Dentro de
        // este panel el teclado es entero suyo, así que no hay nada con lo que
        // chocar.
        KeyCode::Char('s') => CompareKey::Sync(norte_proto::methods::SyncMode::Update),
        KeyCode::Char('m') => CompareKey::Sync(norte_proto::methods::SyncMode::Mirror),
        _ => CompareKey::Ignore,
    }
}

/// Despacha una tecla del panel de diferencias (`Shift+F2`). Fijas, como las
/// del diálogo de búsqueda: no hay vocabulario `dialog.*` para «cambia de
/// lado» ni para «esconde los iguales». El significado lo decide
/// [`compare_key`]; esto solo lo ejecuta.
#[allow(clippy::too_many_arguments)]
pub async fn on_compare_key(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    fill: &mut BySlot<Fill>,
    decorate_fetch: &mut BySlot<DecorateFetch>,
    last_probed: &mut Probed,
    search_run: &mut Option<SearchRun>,
    compare_run: &mut Option<CompareRun>,
    mods: crossterm::event::KeyModifiers,
    code: KeyCode,
) {
    use norte_frontend::compare::CATEGORIES;

    let Some(cancel_requested) = app.compare.as_ref().map(|v| v.cancel_requested) else {
        return;
    };
    let running = compare_run
        .as_ref()
        .is_some_and(|c| c.state == CompareState::Running);
    match compare_key(mods, code, running, cancel_requested) {
        CompareKey::Ignore => {}
        CompareKey::Quit => {
            if let Some(c) = compare_run.take() {
                c.task.cancel();
            }
            app.quit = true;
        }
        CompareKey::CancelTask => {
            if let Some(c) = compare_run.as_ref() {
                c.task.cancel();
            }
            if let Some(view) = app.compare.as_mut() {
                view.cancel_requested = true;
            }
        }
        CompareKey::Close => {
            // Soltar el `CompareRun` no cancela nada (`TaskRef` no tiene
            // `Drop`): en remoto el daemon seguiría recorriendo los dos
            // árboles enteros para un panel que ya no existe (review
            // MAJOR-4). Se cancela SIEMPRE al salir.
            if let Some(c) = compare_run.take() {
                c.task.cancel();
            }
            app.close_compare();
        }
        CompareKey::Open => {
            on_compare_enter(
                app,
                backend,
                events,
                fill,
                decorate_fetch,
                last_probed,
                search_run,
                compare_run,
            )
            .await;
        }
        // Fuera del brazo que toma prestado el `view`: resolver los params
        // necesita el `App` entero (las dos raíces, el journal del backend y
        // la barra donde se dice que no). Solo RESUELVE; lanzar es del run
        // loop, igual que al abrir este mismo panel.
        CompareKey::Sync(sync_mode) => {
            app.request_sync(sync_mode);
        }
        otra => {
            let Some(view) = app.compare.as_mut() else {
                return;
            };
            match otra {
                CompareKey::SwapSide => view.pane.swap_active_side(),
                CompareKey::Move(delta) => view.pane.move_by(delta),
                CompareKey::First => view.pane.select_first(),
                CompareKey::Last => view.pane.select_last(),
                CompareKey::Filter(i) => {
                    if let Some(cat) = CATEGORIES.get(i) {
                        view.pane.toggle_filter(*cat);
                    }
                }
                CompareKey::Mark => {
                    if let Some(id) = view.pane.selected_id() {
                        view.pane.toggle_mark(id);
                    }
                }
                _ => {}
            }
        }
    }
}

/// Lo que una tecla SIGNIFICA en el panel de sincronización.
///
/// Separado del despacho por lo mismo que [`CompareKey`]: lo que se puede
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
    let accion = sync_key(
        mods,
        code,
        running,
        view.cancel_requested,
        view.confirming.is_some(),
    );
    match accion {
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

/// `Enter` sobre una fila del panel de diferencias: navega al directorio REAL
/// del lado ACTIVO y cierra el panel.
///
/// Un huérfano que el walk emitió como UNA fila sin enumerar su subárbol se
/// expande así, que es el motivo por el que la fila lleva el `Entry` entero y
/// no solo un nombre. Sin nada en el lado activo NO se cae al otro: se dice.
#[allow(clippy::too_many_arguments)]
pub async fn on_compare_enter(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    fill: &mut BySlot<Fill>,
    decorate_fetch: &mut BySlot<DecorateFetch>,
    last_probed: &mut Probed,
    search_run: &mut Option<SearchRun>,
    compare_run: &mut Option<CompareRun>,
) {
    let Some(view) = app.compare.as_ref() else {
        return;
    };
    if view.pane.target_entry().is_none() {
        let side = view.pane.active_side();
        // La lengua ambiente, la misma que resuelven `t`/`ta` a dos líneas
        // de aquí: pasar una distinta daría una frase medio traducida.
        let side_word = norte_frontend::compare::side_label(side, norte_i18n::active());
        app.message = Some(ta("compare-no-target", &[("side", &side_word)]));
        return;
    }
    // El directorio al que ir lo decide el MODELO (regla 7): el propio path
    // si la fila es un directorio, su padre si es un fichero — la misma regla
    // que necesitará la GUI.
    let Some(destino) = view.pane.navigation_target() else {
        // Hay entrada pero no hay a dónde ir: un fichero colgado de la raíz
        // de su scheme no tiene padre. Se DICE, igual que el caso de arriba —
        // un `Enter` que no hace nada y no explica por qué se lee como que la
        // tecla está rota. Lo arregló la GUI y aquí faltaba (revisión de
        // rama, MAJOR-4).
        let side_word =
            norte_frontend::compare::side_label(view.pane.active_side(), norte_i18n::active());
        app.message = Some(ta("compare-no-target", &[("side", &side_word)]));
        return;
    };
    // Y el cursor cae sobre la entrada de la que se salió, byte-exacto (lo
    // consume el listado al aterrizar; si ya no existe, cae al default). La
    // GUI lo hacía y esta rama no, mientras su comentario reclamaba paridad
    // (revisión de rama, MINOR-8).
    let foco = view.pane.target_path().cloned();
    // Al pane del lado ACTIVO, y el foco con él: mandar SIEMPRE al pane con
    // foco le costaba al lector el otro directorio para ir a ver este.
    let destino_pane = app.compare_active_pane().unwrap_or_else(|| app.focus());
    if let Some(c) = compare_run.take() {
        c.task.cancel();
    }
    app.close_compare();
    app.set_focus(destino_pane);
    if let Some(p) = foco {
        app.panes[destino_pane].set_pending_focus(p);
    }
    let outcome = cd(app, backend, events, destino).await;
    apply_cd(
        &app.panes,
        fill,
        decorate_fetch,
        last_probed,
        search_run,
        outcome,
    );
}

/// Lee el estado terminal de un [`SearchRun`] del `TaskProgress` (no
/// bloqueante) y lo mapea a [`SearchState`]. `Completed` con los hits al tope
/// = `Truncated`; sin tope = `Done`. Un canal cerrado sin estado terminal aún
/// publicado (carrera) se trata como `Done` (el walker ya no emite).
pub fn finalize_search_state(s: &SearchRun) -> SearchState {
    let mut rx = s.task.progress();
    match rx.borrow_and_update().state.clone() {
        norte_proto::TaskState::Cancelled => SearchState::Cancelled,
        norte_proto::TaskState::Failed { .. } => SearchState::Failed,
        norte_proto::TaskState::Completed if s.hits >= SEARCH_MAX_HITS as usize => {
            SearchState::Truncated
        }
        _ => SearchState::Done,
    }
}

/// Esc en el pane virtual de búsqueda (liveSearch T6): con la Task viva pide
/// cancelación (los hits ya llegados se conservan; el estado pasará a
/// `Cancelled` al cerrarse el canal); ya terminada, sale del modo virtual
/// restaurando el dir anterior con un `cd` normal.
pub async fn on_search_escape(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    fill: &mut BySlot<Fill>,
    decorate_fetch: &mut BySlot<DecorateFetch>,
    last_probed: &mut Probed,
    search_run: &mut Option<SearchRun>,
) {
    let Some(s) = search_run.as_ref() else {
        return;
    };
    if s.state == SearchState::Running {
        s.task.cancel();
        return;
    }
    let prev = s.prev_dir.clone();
    *search_run = None;
    let outcome = cd(app, backend, events, prev).await;
    apply_cd(
        &app.panes,
        fill,
        decorate_fetch,
        last_probed,
        search_run,
        outcome,
    );
}

/// Enter sobre un hit del pane virtual (liveSearch T6): cd al PADRE del hit y
/// deja el cursor sobre él (por path, si ya está en la primera página).
/// Cancela la Task si sigue viva y sale del modo virtual.
pub async fn on_search_enter(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    fill: &mut BySlot<Fill>,
    decorate_fetch: &mut BySlot<DecorateFetch>,
    last_probed: &mut Probed,
    search_run: &mut Option<SearchRun>,
) {
    let Some(hit) = app.focused().selected().map(|e| e.path.clone()) else {
        return;
    };
    let Some(parent) = hit.parent() else {
        return;
    };
    if let Some(s) = search_run.as_ref()
        && s.state == SearchState::Running
    {
        s.task.cancel();
    }
    *search_run = None;
    let pane = app.focus();
    let outcome = cd(app, backend, events, parent).await;
    apply_cd(
        &app.panes,
        fill,
        decorate_fetch,
        last_probed,
        search_run,
        outcome,
    );
    // Re-ancla el cursor sobre el hit por path (el cd resetea a 0); si cayó
    // en una página aún no drenada, el cursor se queda arriba (v1).
    if let Some(i) = app.panes[pane].entries().iter().position(|e| e.path == hit) {
        app.panes[pane].set_cursor(i);
    }
}

/// The diff pane's run-loop wiring (`Shift+F2`,
/// 2026-08-11-directory-comparison.md). The MODEL is tested in
/// `norte-frontend`, without a terminal; what is pinned here is the part only
/// this crate can get wrong.
#[cfg(test)]
mod compare_tests {
    use super::{CompareRun, drain_compare, launch_compare};
    use crate::app::{App, CompareState, Pane};
    use norte_core::backend::TaskRef;
    use norte_proto::VPath;
    use norte_proto::methods::{
        CompareConfidence, CompareCriterion, CompareRow, CompareRowsBatch, CompareVerdict,
    };

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire de test")
    }

    fn app_en(izq: &str, der: &str) -> App {
        App::new(
            Pane::new(vp(izq), Vec::new()),
            Pane::new(vp(der), Vec::new()),
        )
    }

    /// A synthetic run whose task reports `state` and `entries_done`, plus the
    /// sender that stays alive so the channel is only closed on purpose.
    fn run_con(
        state: norte_proto::TaskState,
        entries_done: u64,
        recibidas: usize,
    ) -> (
        CompareRun,
        tokio::sync::mpsc::Sender<CompareRowsBatch>,
        tokio::sync::watch::Sender<norte_proto::TaskProgress>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::channel::<CompareRowsBatch>(4);
        let id = norte_proto::TaskId::new(1);
        let (progreso, prx) = tokio::sync::watch::channel(norte_proto::TaskProgress {
            task_id: id,
            kind: norte_proto::TaskKind::Compare,
            state,
            bytes_done: 0,
            bytes_total: None,
            entries_done,
            entries_total: None,
            current: None,
        });
        (
            CompareRun {
                task: TaskRef::synthetic_for_tests(id, prx),
                rx,
                rows: recibidas,
                state: CompareState::Running,
            },
            tx,
            progreso,
        )
    }

    fn fila(id: u64) -> CompareRow {
        CompareRow {
            id,
            left: None,
            right: None,
            verdict: CompareVerdict::Error,
            criterion: CompareCriterion::Presence,
            confidence: CompareConfidence::Unknown,
            newer: None,
            reason: Some(norte_proto::methods::CompareReason::Unreadable),
            side: None,
            paired_under: None,
        }
    }

    /// El panel que LANZA es el izquierdo, aunque sea el pane derecho de la
    /// pantalla. La spec lo dice y no es cosmético: el lector que pulsa la
    /// tecla desde la derecha espera que su directorio sea «el suyo», y todas
    /// las marcas `<`/`>` del panel cuelgan de esa elección.
    #[test]
    fn el_pane_con_foco_es_el_lado_izquierdo() {
        let mut app = app_en("file:///a", "file:///b");
        app.switch_focus();
        app.request_compare();
        let params = app.pending_compare.expect("deja params pendientes");
        assert_eq!(params.left, vp("file:///b"));
        assert_eq!(params.right, vp("file:///a"));
    }

    /// C6, hallazgo 4: el daemon responde `InvalidPath` a dos raíces iguales,
    /// y tiene razón — pero la frase no depende de una vuelta por la red, y
    /// ninguna Task llega a existir.
    #[test]
    fn los_dos_panes_en_el_mismo_sitio_se_rechazan_aqui() {
        let mut app = app_en("file:///a", "file:///a");
        app.request_compare();
        assert!(app.pending_compare.is_none(), "no puede pedirse la Task");
        assert!(app.message.is_some(), "y el lector tiene que enterarse");
    }

    /// Una lista de hits no es un directorio: no hay raíz que mandar. Misma
    /// negativa que ya dan mirror y pull sobre un pane virtual.
    #[test]
    fn un_pane_virtual_no_puede_compararse() {
        let mut app = app_en("file:///a", "file:///b");
        app.panes[1].begin_search(vp("file:///b"));
        app.request_compare();
        assert!(app.pending_compare.is_none());
        assert_eq!(app.message, Some(norte_i18n::t("msg-pane-not-a-location")));
    }

    /// C6, hallazgo 3: no hay toggle de symlinks en la UI porque el engine
    /// acepta el campo y lo ignora, así que `Backend::compare` responde
    /// `Unsupported` a `true` antes de que exista Task. Pedir `true` desde
    /// aquí sería pedir una promesa que nadie cumple.
    #[test]
    fn jamas_se_pide_seguir_symlinks() {
        let mut app = app_en("file:///a", "file:///b");
        app.request_compare();
        assert!(!app.pending_compare.expect("params").follow_symlinks);
    }

    /// La copia local del default del wire no puede separarse del wire.
    #[test]
    fn la_tolerancia_por_defecto_sigue_al_wire() {
        let del_wire: norte_proto::methods::FsCompareParams =
            serde_json::from_str(r#"{"left":"file:///a","right":"file:///b"}"#)
                .expect("params mínimos");
        let mut app = app_en("file:///a", "file:///b");
        app.request_compare();
        assert_eq!(
            app.pending_compare.expect("params").mtime_tolerance_ms,
            del_wire.mtime_tolerance_ms
        );
    }

    /// **C6, hallazgo 2 — el que el plan no dice.** Que se cierre el canal de
    /// filas NO significa que hayan llegado todas: las dos bombas son tasks
    /// independientes. Con la Task `Completed` contando MÁS filas de las
    /// recibidas, el panel dice `Incomplete` — decir «hecho» sería mentir
    /// sobre lo completa que está la respuesta, que en una comparación es la
    /// respuesta entera.
    #[tokio::test]
    async fn un_lote_perdido_se_dice_en_vez_de_pasar_por_hecho() {
        let mut app = app_en("file:///a", "file:///b");
        app.compare = Some(crate::app::CompareView::new(
            vp("file:///a"),
            vp("file:///b"),
            0,
            None,
            None,
        ));
        // La task contó 9 filas; llegaron 7.
        let (run, _tx, _p) = run_con(norte_proto::TaskState::Completed, 9, 7);
        let mut run = Some(run);
        drain_compare(&mut app, &mut run, None);
        let view = app.compare.expect("el panel sigue abierto");
        assert_eq!(view.state, CompareState::Incomplete);
        assert_eq!(view.rows_expected, 9);
    }

    /// Y el caso contrario, que es el normal: todas las filas contadas
    /// llegaron, así que `Done` — sin acusar de pérdida a nadie.
    #[tokio::test]
    async fn cuando_llegan_todas_es_hecho_y_no_incompleto() {
        let mut app = app_en("file:///a", "file:///b");
        app.compare = Some(crate::app::CompareView::new(
            vp("file:///a"),
            vp("file:///b"),
            0,
            None,
            None,
        ));
        let (run, _tx, _p) = run_con(norte_proto::TaskState::Completed, 7, 7);
        let mut run = Some(run);
        drain_compare(&mut app, &mut run, None);
        assert_eq!(
            app.compare.expect("el panel").state,
            CompareState::Done,
            "una carrera benigna no puede leerse como pérdida"
        );
    }

    /// Cancelar conserva lo que llegó: la comparación no escribe nada, así
    /// que las filas ya vistas siguen siendo ciertas.
    #[tokio::test]
    async fn cancelar_conserva_las_filas_que_llegaron() {
        let mut app = app_en("file:///a", "file:///b");
        app.compare = Some(crate::app::CompareView::new(
            vp("file:///a"),
            vp("file:///b"),
            0,
            None,
            None,
        ));
        let (run, _tx, _p) = run_con(norte_proto::TaskState::Cancelled, 2, 2);
        let mut run = Some(run);
        drain_compare(
            &mut app,
            &mut run,
            Some(CompareRowsBatch {
                task_id: norte_proto::TaskId::new(1),
                rows: vec![fila(1), fila(2)],
            }),
        );
        drain_compare(&mut app, &mut run, None);
        let view = app.compare.expect("el panel");
        assert_eq!(view.state, CompareState::Cancelled);
        assert_eq!(view.pane.len(), 2);
    }

    /// Un lote que llega con el panel YA cerrado suelta el run y cancela la
    /// Task (regla 3): sin esto, el drenador seguiría vivo alimentando un
    /// panel que no existe.
    #[tokio::test]
    async fn un_lote_con_el_panel_cerrado_cosecha_el_run() {
        let mut app = app_en("file:///a", "file:///b");
        let (run, _tx, _p) = run_con(norte_proto::TaskState::Running, 0, 0);
        let mut run = Some(run);
        drain_compare(
            &mut app,
            &mut run,
            Some(CompareRowsBatch {
                task_id: norte_proto::TaskId::new(1),
                rows: vec![fila(1)],
            }),
        );
        assert!(run.is_none(), "el run tiene que soltarse");
    }

    /// **Lo que cazó el arnés de tmux y ninguna aserción del modelo podía
    /// ver.** `Enter` sobre una fila navega al pane del lado ACTIVO, no al
    /// pane con FOCO: mirando el lado derecho, el Enter mandaba el pane
    /// izquierdo —el que tenía el foco— a ver el directorio de la derecha, y
    /// el lector se quedaba con los dos panes en el mismo sitio y su
    /// izquierda perdida.
    ///
    /// Se pinea sobre `compare_active_pane` y no sobre el `cd`, que necesita
    /// un backend y un `EventStream`: lo que puede equivocarse es la ELECCIÓN
    /// del pane, y es lo que esto fija.
    #[test]
    fn el_enter_va_al_pane_del_lado_activo_y_no_al_del_foco() {
        let mut app = app_en("file:///a", "file:///b");
        // Lanzada desde el pane DERECHO: el izquierdo del panel es panes[1].
        app.switch_focus();
        app.compare = Some(crate::app::CompareView::new(
            vp("file:///b"),
            vp("file:///a"),
            app.focus(),
            None,
            None,
        ));
        assert_eq!(
            app.compare_active_pane(),
            Some(1),
            "lado izquierdo = panes[1]"
        );
        app.compare
            .as_mut()
            .expect("el panel")
            .pane
            .swap_active_side();
        assert_eq!(
            app.compare_active_pane(),
            Some(0),
            "el lado derecho del panel es el OTRO pane, sea cual sea el foco"
        );
        // Y con el panel cerrado no hay pane que elegir.
        app.close_compare();
        assert_eq!(app.compare_active_pane(), None);
    }

    /// **Review BLOCKER-1.** El panel se queda el teclado ENTERO y `ISIG`
    /// está apagado en modo raw, así que si ninguna tecla cierra
    /// incondicionalmente, el panel es una trampa: el estado solo pasa a
    /// terminal cuando se cierra el canal de filas, y hay formas de que no se
    /// cierre nunca —un daemon que se cae publica el fallo en el watch sin
    /// tocar la ruta, un provider colgado en una NFS muerta no mira su token
    /// hasta que vuelva el syscall—. Las DOS salidas, afirmadas:
    #[test]
    fn del_panel_de_diferencias_siempre_se_puede_salir() {
        use crossterm::event::KeyModifiers as M;

        // Ctrl+C sale de norte, con la Task viva o muerta, igual que en los
        // otros nueve overlays.
        for viva in [true, false] {
            assert_eq!(
                super::compare_key(
                    M::CONTROL,
                    crossterm::event::KeyCode::Char('c'),
                    viva,
                    false
                ),
                super::CompareKey::Quit,
                "viva={viva}"
            );
        }
        // Primer Esc con la Task viva: cancela y CONSERVA las filas.
        assert_eq!(
            super::compare_key(M::NONE, crossterm::event::KeyCode::Esc, true, false),
            super::CompareKey::CancelTask
        );
        // El segundo cierra AUNQUE la Task siga diciendo que corre — que es
        // justo el caso en el que el canal no se cierra nunca.
        assert_eq!(
            super::compare_key(M::NONE, crossterm::event::KeyCode::Esc, true, true),
            super::CompareKey::Close
        );
        // Y con la Task ya terminal, el primer Esc cierra.
        assert_eq!(
            super::compare_key(M::NONE, crossterm::event::KeyCode::Esc, false, false),
            super::CompareKey::Close
        );
    }

    /// Un modificador que el panel no usa no puede COLARSE como la tecla
    /// pelada: sin el filtro, `Alt+1` escondía una categoría y `Alt+Tab`
    /// cambiaba de lado (review MINOR).
    #[test]
    fn una_tecla_con_alt_no_hace_nada_en_el_panel() {
        use crossterm::event::{KeyCode as C, KeyModifiers as M};
        for code in [C::Tab, C::Char('1'), C::Enter, C::Down] {
            assert_eq!(
                super::compare_key(M::ALT, code, false, false),
                super::CompareKey::Ignore,
                "{code:?}"
            );
            // Y sin modificador la MISMA tecla sí significa algo: el filtro
            // no puede haberse comido el caso normal.
            assert_ne!(
                super::compare_key(M::NONE, code, false, false),
                super::CompareKey::Ignore,
                "{code:?}"
            );
        }
        // Un Ctrl que no es Ctrl+C tampoco: se traga la tecla, no sale.
        assert_eq!(
            super::compare_key(M::CONTROL, C::Down, false, false),
            super::CompareKey::Ignore
        );
    }

    /// Un fallo al pedir la Task NO abre el panel: un panel vacío que dice
    /// «fallo» es peor que la frase en la barra, porque además hay que
    /// cerrarlo. Y el detalle va por categoría, jamás crudo.
    #[tokio::test]
    async fn un_lanzamiento_fallido_no_abre_el_panel() {
        let engine = norte_core::Engine::new();
        let backend = norte_core::backend::Backend::Embedded(std::sync::Arc::new(engine));
        let mut app = app_en("file:///a", "file:///b");
        let mut run: Option<CompareRun> = None;
        // `follow_symlinks: true` es `Unsupported` antes de que exista Task
        // alguna (C6, hallazgo 3): el fallo más barato de provocar aquí.
        launch_compare(
            &mut app,
            &backend,
            &mut run,
            norte_proto::methods::FsCompareParams {
                left: vp("file:///a"),
                right: vp("file:///b"),
                criteria: norte_proto::methods::CompareCriteria::default(),
                max_depth: None,
                mtime_tolerance_ms: 2000,
                follow_symlinks: true,
                descend_orphans: None,
            },
        )
        .await;
        assert!(app.compare.is_none(), "el panel no puede abrirse vacío");
        assert!(run.is_none());
        assert!(app.message.is_some());
    }
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
        use super::{CompareKey, compare_key};
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
        let cancelador = run.task.canceller();
        let mut sync_run = Some(run);
        on_sync_key(&mut app, &mut sync_run, M::NONE, KeyCode::Esc);
        assert!(app.sync.is_none(), "el panel se cierra");
        assert!(sync_run.is_none(), "y el run se suelta");
        drop(cancelador);
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
            std::mem::discriminant(&SyncTick::Applied { vivo: true })
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
