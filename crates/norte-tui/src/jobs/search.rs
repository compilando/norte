//! Buscar bajo un directorio, y el pane VIRTUAL que enseña los hits.
//!
//! Los hits llegan por lotes y se acumulan en un pane que no es un directorio:
//! salir de él (un `cd`) suelta la búsqueda y cancela la Task, que es por qué
//! [`super::super::navigate`] tiene que conocer [`SearchRun`].

use crossterm::event::{KeyCode, KeyModifiers};
use norte_core::backend::{Backend, TaskRef};
use norte_frontend::layout::BySlot;
use norte_i18n::{t, ta};
use norte_proto::VPath;
use norte_proto::methods::{FsSearchParams, SearchHits};

use crate::app::{App, SearchDialog, SearchState, detail_for_bar, error_category, error_message};
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
        KeyCode::F(4) if plain => dialog.toggle_whole_word(),
        KeyCode::F(5) if plain => dialog.toggle_recursive(),
        KeyCode::F(6) if plain => dialog.cycle_kinds(),
        KeyCode::Tab if plain => dialog.toggle_field(),
        KeyCode::Char(c) if plain => dialog.push_char(c),
        KeyCode::Backspace if plain => dialog.backspace(),
        KeyCode::Esc => app.search_dialog = None,
        KeyCode::Enter => {
            // Un campo que no se entiende para ANTES de lanzar y lleva el
            // foco a él (0.81.0). Lanzar ignorándolo devuelve el árbol
            // entero, y eso se lee igual que un resultado: es la misma
            // trampa que el aviso de versión evita contra un daemon viejo.
            if let Some(campo) = dialog.campo_ilegible() {
                dialog.field = campo;
                app.message = Some(t("search-bad-field"));
                return None;
            }
            if dialog.has_criteria() {
                let root = app.focused().dir().clone();
                return Some(search_params(app.search_dialog.as_ref()?, root));
            }
            // Sin ningún criterio: no-op con aviso (una búsqueda sin criterio
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
    // Los días se convierten a un instante AQUÍ, no en el core: «los últimos
    // siete» se cuenta desde cuando se pulsa Enter, y el core no tiene por
    // qué saber en qué momento se hizo la pregunta.
    let mtime_after = crate::app::parse_days(&dialog.days).map(|d| {
        let ahora = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0_i64, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        ahora.saturating_sub(i64::from(d).saturating_mul(86_400_000))
    });
    let encoding = {
        let e = dialog.encoding.trim();
        (!e.is_empty()).then(|| e.to_owned())
    };
    FsSearchParams {
        name_glob,
        name_regex,
        content,
        content_regex,
        case_sensitive: dialog.case,
        max_hits: Some(SEARCH_MAX_HITS),
        kinds: dialog.kinds.wire(),
        min_size: crate::app::parse_size(&dialog.min_size),
        max_size: crate::app::parse_size(&dialog.max_size),
        mtime_after,
        mtime_before: None,
        exclude_roots: Vec::new(),
        exclude_names: dialog.exclude_names(),
        whole_word: dialog.whole_word,
        recursive: dialog.recursive,
        encoding,
        ..FsSearchParams::new(root)
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
    events: &mut crate::console::Console<'_>,
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
    events: &mut crate::console::Console<'_>,
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
