//! Binario del TUI (fases 3–4 M1): loop de eventos async sobre el core
//! EMBEBIDO o contra el DAEMON (fase 3 M2, por `[daemon] mode` o
//! `--daemon`), con keymap engine (ADR 0006). Regla 7: solo cambia el
//! transporte.
//! `ratatui::init/restore` gestionan raw mode + pantalla alternativa con
//! hook de pánico incluido: la terminal del usuario JAMÁS queda rota.
#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::sync::Arc;

use anyhow::{Context, Result};
use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use futures::StreamExt;
use norte_core::backend::EntryStream;
use norte_core::backend::{Backend, ConnEvent, TaskRef};
use norte_core::{Engine, TransferOptions};
use norte_i18n::{t, ta};
use norte_proto::DeleteMode;
use norte_proto::methods::{FsSearchParams, SearchHits};
use norte_proto::{Entry, EntryKind, Error, VPath};
use norte_tui::app::{
    ALLOW_COLUMNS, ALLOW_EXTENSIONS, ALLOW_NAV_HOTLIST, ALLOW_PICKER, ALLOW_PLUGIN_CONFIG, App,
    DialogOutcome, ExtensionManager, HelpOutcome, HelpView, KeymapsError, Modal, NavPopupKind,
    Palette, Pane, PendingWrite, PickerAction, SearchDialog, SearchState, Settings,
    SettingsEditError, Trail, TrailStep, TransferKind, config_error_category, detail_for_bar,
    dialog_action, error_category, error_message, io_error_category, keymaps_error_category,
    theme_error_category, trust_lua_key,
};
use norte_tui::config::{self, Layers, WatchMode};
use norte_tui::help::TuiChords;
use norte_tui::hints::DialogHints;
use norte_tui::keymap::{
    COMMANDS, Command, DIALOG_COMMANDS, Effective, Resolution, Resolver, Screen,
    chord_from_crossterm, presets,
};
use norte_tui::lua::{
    CommandRun, Layer, LuaHost, PaneCtx, RunOutcome, StatusInput, TrustDecision, TrustStore,
};
use norte_tui::mouse;
use norte_tui::nav;
use norte_tui::tasks::RetrySpec;
use norte_tui::ui;
use norte_tui::viewer::Viewer;
use norte_vfs_local::LocalProvider;
use sha2::Digest as _;
use tokio_util::sync::CancellationToken;

/// Filas que salta `cursor.page-up/down` (fijo hasta que el alto real del
/// pane viaje con el comando).
const PAGE: usize = 10;

/// Entradas de la PRIMERA página que un cd pinta antes de rellenar en
/// background (ADR 0017): con esto el primer render no espera al listado
/// entero (spec §11: primeras 100 en <16 ms aunque el dir tenga 500k).
const FIRST_PAGE: usize = 100;
/// Lote que el drenador coalesce antes de enviar (evita un re-sort por
/// entrada; el re-sort completo lo hace [`Pane::extend_listing`]). Un dir de
/// 100k son ~24 lotes ⇒ ~24 re-sorts de tamaño creciente durante el fill; el
/// merge incremental (claves persistidas) es la optimización diferida a issue.
const FILL_BATCH: usize = 4096;
/// El drenador vacía un lote PARCIAL cada tanto (además de al llenarlo): en un
/// listado remoto lento (páginas por RTT) el usuario ve progreso y el
/// contador `cargando… (n)` avanza en vez de saltar de 4096 en 4096.
const FILL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// Tope por defecto de hits de una búsqueda viva (`Alt+F7`, liveSearch T6):
/// el diálogo v1 no expone el campo, así que se fija un tope razonable —
/// acota la memoria del pane virtual (los hits se acumulan en `entries`) y
/// hace alcanzable el estado `Truncated`. Al llegar, la Task completa y la
/// barra pinta «truncada».
const SEARCH_MAX_HITS: u32 = 10_000;

/// Una búsqueda viva EN CURSO (`Alt+F7`, liveSearch T6): la Task cancelable,
/// el canal de lotes de hits y el pane virtual que los muestra. Molde `Fill`:
/// vive en el run loop, se drena en el `select!` y se suelta al salir del modo
/// virtual (un `cd`) cancelando la Task (regla 3).
struct SearchRun {
    /// Task de `fs.search` (cancelable con `TaskRef::cancel`).
    task: TaskRef,
    /// Canal de lotes de hits (embebido: lo cierra el walker; remoto: la
    /// bomba del `RemoteBackend` lo cierra al terminal).
    rx: tokio::sync::mpsc::Receiver<SearchHits>,
    /// Pane que muestra los hits (índice en `App::panes`).
    pane: usize,
    /// Directorio ANTERIOR del pane, para restaurarlo al salir del modo
    /// virtual (Esc tras terminar).
    prev_dir: VPath,
    /// Hits acumulados (== `panes[pane].entries().len()`, contador propio para
    /// no depender del re-sort del pane).
    hits: usize,
    /// Estado del run: `Running` mientras el walker emite; terminal tras
    /// cerrarse el canal (se lee del `TaskProgress`).
    state: SearchState,
}

/// Petición `ai.rename_plan` EN VUELO (M4-IA). Abortar el `JoinHandle`
/// cancela (regla 3): el abort dropea el future del backend en el runtime →
/// `CancelOnAbandon` envía `rpc.cancel` (remoto) / el timeout+drop aborta el
/// stream (embebido). OJO: DROPEAR el handle solo DESVINCULA la task de
/// tokio — cancelar exige `abort()` explícito.
struct AiRenameRun {
    /// La llamada al modelo, spawneada (es la única llamada larga del loop).
    handle: tokio::task::JoinHandle<Result<norte_proto::methods::AiRenamePlanResult, Error>>,
    /// Dir del pane al LANZAR; el plan se aplica AQUÍ aunque el usuario
    /// navegue mientras el modelo piensa.
    dir: VPath,
}

/// Hits que pide la búsqueda semántica (M4-IA-2): compartido con la GUI
/// desde `norte-frontend` (la MISMA consulta debe devolver lo mismo en
/// ambos frontends); ver su doc para la relación con `SEMANTIC_HIT_LIMIT`
/// y el techo del server.
use norte_frontend::SEMANTIC_K;

/// Petición `index.search_semantic` EN VUELO (M4-IA-2). Mismo contrato de
/// cancelación que [`AiRenameRun`] (regla 3): `abort()` dropea el future del
/// backend → `rpc.cancel` (remoto) / drop (embebido); DROPEAR el handle solo
/// desvincula. Sin dir capturado: la consulta va contra TODOS los roots del
/// índice (`root = None`), navegar mientras piensa no la invalida.
struct SemanticRun {
    /// La llamada al índice+modelo, spawneada.
    handle: tokio::task::JoinHandle<Result<Vec<norte_proto::methods::SemanticHit>, Error>>,
}

/// Mensaje del drenador de un listado paginado al run loop.
enum FillMsg {
    /// Un lote más de entradas para el pane.
    Batch(Vec<Entry>),
    /// El listado se cortó a mitad (error del provider/daemon): no es
    /// silencioso (la UI avisa y limpia el `loading`).
    Failed,
}

/// A listing being FILLED in the background: the drainer's channel, nothing
/// else. Dropping it drops the `rx` → the drainer dies on its next send →
/// releases the stream → cooperative cancellation (rule 3).
///
/// It does NOT carry its pane. The run loop keeps `[Option<Fill>; 2]`, one
/// slot per pane, so the array index IS the pane — the same shape as
/// `decorate_fetch`. A `pane` field beside it would be a second source of the
/// same fact, and two sources of one fact drift; a `pane.swap` would then have
/// to keep them in step by hand instead of just swapping the two slots.
///
/// One slot per pane is also the whole point rather than a tidiness: with a
/// single global slot, any cd that starts a new paginated listing dropped
/// whoever was draining — and `pane.mirror` makes that ONE keystroke with no
/// change of focus, so the pane left half-listed under a permanent
/// «cargando…» is the one the reader is looking at.
struct Fill {
    rx: tokio::sync::mpsc::Receiver<FillMsg>,
}

/// Sonda de stat del VIEWPORT (#52): hidrata size/mtime de las entradas
/// VISIBLES que el listado lazy dejó en None — no solo la enfocada, o las
/// columnas Tamaño/Fecha quedan en blanco en todas las demás filas. A lo
/// sumo UNA tanda en vuelo, acotada a [`STAT_BATCH_MAX`] paths y resuelta
/// con concurrencia [`STAT_BATCH_CONCURRENCY`] (una sesión remota no puede
/// pagar N RTT en serie). Dedup por `(pane, path)` en el conjunto `probed`
/// del run loop: un stat fallido no se reintenta hasta que el listado se
/// renueve (sin martillear un provider roto). Cada stat va acotado con
/// timeout (`STAT_PROBE_TIMEOUT`): un provider colgado no bloquea la tanda
/// para siempre.
struct StatProbe {
    rx: tokio::sync::oneshot::Receiver<Vec<(usize, VPath, Entry)>>,
}

/// Dedup de la sonda #52: `(pane, path)` ya pedidos. Se vacía con cada
/// listado nuevo (cd/refresh) — las entries vuelven a nacer lazy.
type Probed = std::collections::HashSet<(usize, VPath)>;

/// Radio en filas de la ventana que la sonda #52 hidrata alrededor del
/// cursor de cada pane (aproximación del viewport: el alto real lo decide
/// el widget al pintar). Cubre un terminal alto con margen.
const STAT_WINDOW_RADIUS: usize = 64;

/// Tope de paths por tanda de la sonda #52: lo que no entre se pide en la
/// siguiente vuelta, ya sin los que la tanda anterior hidrató.
const STAT_BATCH_MAX: usize = 64;

/// Stats simultáneos dentro de una tanda (#52): acota las peticiones en
/// vuelo contra el daemon sin serializar la latencia de la pantalla entera.
const STAT_BATCH_CONCURRENCY: usize = 8;

/// Tope del stat de la sonda on-focus (#52, MINOR-1): un provider remoto
/// colgado no debe dejar la sonda en vuelo indefinidamente — vencido el
/// plazo se trata como fallo (entrada se queda en `None`, no se reintenta
/// hasta cambiar la selección).
const STAT_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Lanza la tanda de `StatProbe`: clona el `Backend` (barato, Arc interno) y
/// los paths para que la task no retenga el préstamo del run loop. Los
/// fallos (error del provider o timeout) simplemente no vuelven — la entrada
/// se queda lazy y la dedup del run loop evita el reintento en bucle.
fn spawn_stat_probe(backend: &Backend, paths: Vec<(usize, VPath)>) -> StatProbe {
    use futures::StreamExt as _;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let b = backend.clone();
    tokio::spawn(async move {
        let hidratadas: Vec<(usize, VPath, Entry)> = futures::stream::iter(paths)
            .map(|(pane, path)| {
                let b = b.clone();
                async move {
                    let entry = tokio::time::timeout(STAT_PROBE_TIMEOUT, b.stat(&path))
                        .await
                        .ok()
                        .and_then(Result::ok)?;
                    Some((pane, path, entry))
                }
            })
            .buffer_unordered(STAT_BATCH_CONCURRENCY)
            .filter_map(|r| async move { r })
            .collect()
            .await;
        let _ = tx.send(hidratadas);
    });
    StatProbe { rx }
}

/// Fetch de decoraciones de plugin EN VUELO (G3b, ADR 0037): el pane/dir
/// destino y el canal one-shot. Molde de [`StatProbe`] — UNO POR PANE
/// (#117-follow-up review MINOR-2: con un slot global, un cd en el pane B
/// pisaba el fetch en vuelo del A y sus columnas `plugin:` configuradas
/// quedaban en blanco hasta el próximo cd de A — con las columnas ahora
/// config-driven eso contradecía «jamás una columna permanentemente en
/// blanco»). `dir` se conserva para descartar una respuesta TARDÍA que ya
/// no corresponde al listado actual del pane. Límite heredado del diseño
/// de decoraciones (review MINOR-3): `paths` es la página YA listada al
/// asentar el cd — entradas drenadas DESPUÉS por el fill incremental
/// (#52/#54) no viajan en la petición y pintan blanco hasta el próximo
/// re-list (documentado, mismo alcance que las decoraciones).
/// Valores de columnas `plugin:` por id Display → `VPath` → celda saneada
/// (#117-follow-up) — el shape que consume `PaneState::set_plugin_columns`.
type PluginColumnValues =
    std::collections::HashMap<String, std::collections::HashMap<VPath, String>>;

struct DecorateFetch {
    pane: usize,
    dir: VPath,
    rx: tokio::sync::oneshot::Receiver<(
        std::collections::HashMap<VPath, norte_frontend::Decoration>,
        PluginColumnValues,
    )>,
}

/// Lanza el fetch de decoraciones (G3b) para TODAS las entradas actualmente
/// listadas de `pane` (la "página visible" — el listado YA cargado, sea la
/// primera página de un dir grande paginándose o el dir entero; el resto de
/// un dir aún rellenándose queda sin decorar hasta la próxima visita, mismo
/// alcance MVP documentado en el ADR/plan). Sin guardia especial de "algún
/// decorator activado": `Backend::plugin_decorate` resuelve el catálogo en
/// cada llamada (barato embebido, una RPC remota) — intentar cada listado y
/// descartar en silencio si no hay decoradores consentidos es más simple y
/// honesto que cachear un flag que podría quedar obsoleto tras un F12.
/// `None` si el pane no tiene entradas (nada que decorar).
/// #117-follow-up: el MISMO viaje trae también los valores de las columnas
/// `plugin:` CONFIGURADAS del scheme (`plugin_cols` = pares
/// (plugin, columna) de `ColumnsSettings::plugin_ids_for`) — un solo slot
/// en vuelo, un solo guard anti-stale.
fn spawn_decorate_fetch(
    backend: &Backend,
    pane: usize,
    dir: VPath,
    paths: Vec<VPath>,
    plugin_cols: Vec<(String, String)>,
) -> Option<DecorateFetch> {
    if paths.is_empty() {
        return None;
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    let b = backend.clone();
    tokio::spawn(async move {
        let plugins = b.plugin_decorate(&paths).await.unwrap_or_default();
        let merged = norte_frontend::merge_decorations(&paths, &plugins);
        // Review MINOR-1 (regla 3 en espíritu): un fetch SUPERADO (el run
        // loop pisó el slot → rx dropeado) corta antes de cada RPC restante
        // en vez de gastar hasta 8 llamadas cuyo send fallará igual.
        let cols = fetch_plugin_columns(&b, &plugin_cols, &paths, || tx.is_closed()).await;
        let _ = tx.send((merged, cols));
    });
    Some(DecorateFetch { pane, dir, rx })
}

/// Valores de las columnas `plugin:` configuradas (#117-follow-up): la
/// validación de pertenencia + dedupe de colisiones vive en el modelo
/// COMPARTIDO (`norte_frontend::columns::validated_plugin_requests` —
/// review MAJOR-1: una sola definición para ambos frontends; colisión de
/// id bare = blanco antes que atribución falsa, desambiguación real =
/// issue #120). Fail-soft por columna: catálogo caído o RPC fallida =
/// celdas en blanco, jamás un error de listado. `superseded` corta entre
/// RPCs cuando el fetch ya fue pisado (review MINOR-1).
async fn fetch_plugin_columns(
    backend: &Backend,
    requested: &[(String, String)],
    paths: &[VPath],
    superseded: impl Fn() -> bool,
) -> PluginColumnValues {
    let mut out = std::collections::HashMap::new();
    if requested.is_empty() {
        return out;
    }
    let Ok(list) = backend.plugins_list().await else {
        return out;
    };
    for (plugin, column) in
        norte_frontend::columns::validated_plugin_requests(requested, &list.plugins)
    {
        if superseded() {
            return out;
        }
        let raw = backend
            .plugin_column_values(&column, paths)
            .await
            .unwrap_or_default();
        let sanitized = norte_frontend::columns::sanitize_column_values(paths, &raw);
        out.insert(
            norte_frontend::columns::plugin_display_id(&plugin, &column),
            sanitized,
        );
    }
    out
}

/// Pane que un desenlace de `cd` acaba de ASENTAR (`Filling`/`Replaced`,
/// listado nuevo YA en `app.panes[pane]`), o `None` si el cd no tocó ningún
/// pane (`Failed`/`Cancelled`). NO consume `outcome` (préstamo): el llamante
/// aún necesita pasarlo a [`apply_cd`] justo después.
fn cd_landed_pane(outcome: &Cd) -> Option<usize> {
    match outcome {
        Cd::Filling { pane, .. } | Cd::Replaced(pane) => Some(*pane),
        // Un refresh re-lista IN SITU (mismo dir, orden ya aplicado): no hay
        // aterrizaje que ordenar ni decoración nueva que pedir — paridad con
        // el camino de `on_tick`, que tampoco lo hace. Un `Swapped` tampoco
        // lista nada: los dos listados ya existían, solo cambiaron de lado
        // (sus decoraciones viajan con ellos en [`reconcile_swap`]).
        Cd::Refreshed(..) | Cd::Swapped | Cd::Failed(..) | Cd::Cancelled | Cd::Suspended => None,
    }
}

/// Desenlace de un `cd`, para que el run loop actualice el relleno vivo.
enum Cd {
    /// The pane was replaced and the REST of its listing fills in the
    /// background. The pane index rides ALONGSIDE the [`Fill`] and not inside
    /// it: the run loop files the fill by pane, and the index is that filing
    /// key, not a property of the drainer.
    Filling {
        /// Pane whose listing is filling.
        pane: usize,
        /// The drainer, headed for `fill[pane]`.
        fill: Fill,
    },
    /// El pane `usize` se reemplazó y ya está completo: un relleno anterior
    /// de ESE pane queda obsoleto y hay que soltarlo.
    Replaced(usize),
    /// El cd del pane `usize` FALLÓ al listar: el pane se quedó donde
    /// estaba (el error ya salió por la barra) sobre su listado ANTERIOR, así
    /// que un relleno previo de ese pane SIGUE siendo válido y se conserva
    /// (#78: soltarlo dejaba el pane colgado en `loading=true` —con
    /// «(parcial)» en la quick-search— sin drenador que lo apagara). El error
    /// VIAJA para quien navega desde el popup de historial (spec
    /// 2026-07-18: `NotFound` retira la entrada). Sin el índice de pane: al no
    /// tocar ya el relleno (#78) nadie lo consulta.
    Failed(Error),
    /// El cd se ABANDONÓ y nada lo reanuda: `Esc` durante el listado, `Ctrl-C`,
    /// o el stream de eventos muriéndose. Nada cambió y el relleno sigue.
    ///
    /// Distinto de [`Cd::Suspended`] a propósito: los dos dejan el pane donde
    /// estaba, pero solo uno de ellos va a volver. Quien recorre el rastro
    /// necesita saber cuál, y probar `app.modal` para averiguarlo adivina.
    Cancelled,
    /// El cd se PARÓ a medias y algo va a reanudar ESTA MISMA navegación: el
    /// modal TOFU (`Modal::TrustHostKey`), que carga el pane y el modo de
    /// rastro para que el reintento continúe donde esta se quedó.
    ///
    /// Para el relleno y para los panes es idéntico a [`Cd::Cancelled`] (el
    /// pane no se tocó); la diferencia la lee [`rewind_for`], que NO rebobina
    /// un paso del rastro que el reintento va a terminar.
    Suspended,
    /// #118: `pane.refresh` (Ctrl+R) re-listó estos panes DESDE `dispatch`
    /// (que no ve `fill`/`last_probed`): el desenlace viaja al run loop para
    /// que [`apply_cd`] aplique el ritual post-refresh — mismo `[bool; 2]`
    /// que devuelve [`refresh_panes`] (`true` = listado completo asentado).
    Refreshed([bool; 2]),
    /// `pane.swap` cruzó los panes DESDE `dispatch`, que no ve el estado
    /// indexado por pane que vive en el run loop. El desenlace viaja para que
    /// [`reconcile_swap`] cruce también esa mitad — mismo patrón que
    /// `Refreshed`.
    Swapped,
}

/// MINOR-4 (H1 close): un modal puede llegar de forma ASÍNCRONA (p. ej.
/// `Modal::ApproveAgentOp`, vía `ConnEvent` — un agente pide aprobación en
/// cualquier momento) mientras la palette está abierta. Sin este guard, el
/// run loop resolvía la tecla contra la palette PRIMERO (`app.palette.is_some()`
/// se comprobaba antes que `app.modal.is_some()`): un Enter pulsado para
/// responder al modal en realidad despachaba la fila resaltada de la
/// palette EN SILENCIO, y el modal de seguridad seguía esperando una
/// respuesta que nunca llegó por esa tecla. El modal SIEMPRE gana: la rama
/// de la palette del run loop excluye este caso de su condición (deja de
/// consumir la tecla) y la rama del modal cierra la palette, ahora obsoleta,
/// nada más entrar — la MISMA tecla cae al modal en la misma iteración.
///
/// GENERALIZADO a TODOS los overlays: el guard valía solo para la palette y
/// los ajustes, pero el modal se pinta el ÚLTIMO —por encima de CUALQUIER
/// overlay ([`norte_tui::ui::draw`])— mientras la cadena de teclado del run
/// loop resolvía ANTES contra el selector de tema, el picker de columnas, el
/// gestor de extensiones, el popup de navegación, el diálogo de búsqueda y la
/// ayuda. Los píxeles decían «responde al modal» y la tecla se iba a otra
/// parte: en el diálogo de búsqueda y en el campo de nombre del popup se
/// colaba como TEXTO tecleado, y en el gestor de extensiones como un
/// `dialog.toggle-enabled`/`dialog.remove` sobre el plugin resaltado — la
/// misma edición silenciosa de MINOR-4, con peor desenlace.
#[must_use]
fn modal_wins(app: &App) -> bool {
    app.modal.is_some()
}

/// Does the open help overlay own this key press? (H3c)
///
/// The ONE hole in [`modal_wins`], and it is shaped by which of the two
/// arrived first — the flag `HelpView::over_modal` is what remembers:
///
/// * help opened FROM a modal keeps the keys. Otherwise `F1` over a dialog
///   would open a page whose cursor keys all belong to the dialog underneath:
///   an overlay the reader asked for and cannot use.
/// * a modal that ARRIVED over an already-open help does NOT lose the key. The
///   help is the stale one there, and [`close_stale_overlays`] is what retires
///   it — same treatment the palette and the settings overlay already get.
///
/// While the help owns the keys the modal's own verbs are unreachable, which is
/// the point: nothing gets approved through a page covering it. The modal is
/// still painted on top ([`norte_tui::ui::draw`] paints it last), so the
/// question is never HIDDEN — only unanswerable until the help closes, and its
/// TTL running out denies the agent.
///
/// The flag decides ONLY while a modal is live, and it cannot be stale by the
/// time it is read: [`settle_help_over_modal`] clears it at the top of every
/// turn of the run loop, so it always describes the modal that is on screen NOW
/// rather than one that has since been answered (review MINOR-1).
#[must_use]
fn help_owns_keys(app: &App) -> bool {
    match app.help.as_ref() {
        // Ownership expressed as the two cases rather than as one boolean: with
        // a modal live the flag decides; with none there is nobody to compete
        // with and the help owns the key anyway.
        Some(help) => {
            if app.modal.is_some() {
                help.over_modal
            } else {
                debug_assert!(
                    !help.over_modal,
                    "`over_modal` con `app.modal` vacío: \
                     `settle_help_over_modal` no corrió esta vuelta"
                );
                true
            }
        }
        None => false,
    }
}

/// Makes `HelpView::over_modal` a fact about the PRESENT (review MINOR-1).
///
/// The flag is set once, when the overlay opens, and [`help_owns_keys`] and
/// [`close_stale_overlays`] both trust it later. That trust is only sound while
/// it still describes the modal on screen: if the modal an `over_modal` help was
/// opened over went away and a DIFFERENT one arrived, the help would keep the
/// keys and `close_stale_overlays` would never retire it — the new prompt
/// unanswerable until the reader closes a page about a dialog that no longer
/// exists.
///
/// Unreachable today (every async writer refuses to touch a live modal, and the
/// paths that close one are the paths that answer it), but the argument for that
/// spanned four functions. Clearing the flag whenever no modal is live makes it
/// one line: the memory cannot outlive what it is a memory OF, so `over_modal`
/// being `true` means a modal was there on the previous turn AND is there now.
///
/// Called at the top of the run loop, BEFORE the retained modals (the AI plan,
/// the semantic hits) are planted: one of those arriving must find the flag
/// already cleared, so it is treated as a modal arriving over an open help.
fn settle_help_over_modal(app: &mut App) {
    if app.modal.is_none()
        && let Some(help) = app.help.as_mut()
    {
        help.over_modal = false;
    }
}

/// Retires the overlays a modal has made obsolete (MINOR-4, H1 close; extended
/// to the help in H3c).
///
/// Called from the modal arm of the run loop's key chain — i.e. exactly when a
/// modal has the key and some overlay is still on screen. The palette and the
/// settings overlay are dropped because their rows EXPIRE (they were built
/// against a state the modal is about to change) and because that same key must
/// reach the modal instead of vanishing into a filter.
///
/// The help is dropped too, but only when it did not open over this modal:
/// `over_modal` help is the reader's deliberate "explain this dialog to me",
/// and it owns the keys ([`help_owns_keys`]), so this function is never even
/// reached while one is open. The guard states that, rather than relying on the
/// caller to.
///
/// The other overlays (theme selector, column picker, extensions, nav popup,
/// search dialog) yield the key but SURVIVE: their rows do not expire and the
/// reader gets them back intact after answering.
fn close_stale_overlays(app: &mut App) {
    app.palette = None;
    app.settings = None;
    if app.help.as_ref().is_some_and(|help| !help.over_modal) {
        app.help = None;
    }
}

/// `Command::AppHelp`: opens the overlay on the page about where the reader IS.
///
/// The context comes from [`norte_tui::help_context::help_context`] (the TUI's
/// closed vocabulary, anchored on `Modal`) and the page from the CORPUS, so
/// moving an explanation between pages is an edit to prose.
///
/// A word on the overlays that are NOT in that vocabulary — the palette, the
/// settings overlay, the theme and column pickers, the extension manager, the
/// nav popup, the search dialog. `help_context` answers `browse` for all of
/// them, and that answer is UNREACHABLE: each of those arms sits ahead of this
/// dispatch in the run loop's key chain with its own fixed keys, so `F1` there
/// is inert and never gets here. The one exception proves it — the palette's
/// `Enter` can dispatch `app.help`, and it clears `app.palette` BEFORE
/// dispatching, so by the time this runs the palette is gone and `browse` (or
/// `viewer`) is the honest answer. Growing the vocabulary for those overlays
/// would be vocabulary for a state that cannot happen.
///
/// `lang` is the NEGOTIATED language (`NORTE_LANG` > `[ui] lang` >
/// environment — the value `main` handed to `norte_i18n::force`), never
/// `Lang::from_env()`: the corpus is per-locale and a page in another language
/// than the chrome around it is the same bug as a half-translated dialog.
/// `help_lines` is the body of the synthetic keyboard entry, snapshotted from
/// the VIGENTE keymap (rebuilt by the hot reload, which also closes an open
/// overlay so no snapshot survives a rebind).
///
/// OVER A MODAL, a context with no page opens NOTHING (review MAJOR-2). The
/// index is the least surprising landing from a pane or the viewer — nothing
/// there is waiting on a decision — but over a dialog it covers a live question
/// with "Welcome to norte — norte is an orthodox file manager. Two panes…",
/// freezes the dialog's verbs, replaces its footer, and lets the reader walk
/// from the index into another dialog's `y`/`n` prose while an agent approval
/// waits behind it. So the reader is TOLD and the prompt stays answerable —
/// the same decision [`palette_help`] already makes for an undocumented row,
/// applied where it matters more. The pages still missing are on the
/// documentation gate's shrinking allowlist, so this is temporary by
/// construction.
/// `plugins` is the catalogue as of the moment the reader pressed the key
/// (H3e), or `None` when it could not be asked for. It arrives as a PARAMETER
/// because this function is SYNC — its callers are async and its tests are not
/// — and it is taken ONCE, on the open path, never while painting. `None` and
/// an empty catalogue land in the same place: no plugin rows in the sidebar and
/// every `plugin:` command dimmed, which is the honest answer to "I could not
/// find out".
/// Whether `F1` must refuse to open here: over a modal, with no page for this
/// context.
///
/// Pulled out of [`open_contextual_help`] so the refusal can be tested for what
/// it IS rather than through whichever modal happens to be undocumented. Since
/// H3h no context is: the documentation gate has no allowlist left, so a new
/// context arrives with its page or fails the build. That makes this guard
/// unreachable through the UI today and worth keeping anyway — it is the
/// fail-safe for the one way a context could still lose its page, which is
/// somebody deleting the page.
fn refuses_over_modal(lang: norte_help::Lang, context: &str, over_modal: bool) -> bool {
    over_modal && norte_help::topic_for_context(lang, context).is_none()
}

fn open_contextual_help(
    app: &mut App,
    lang: norte_help::Lang,
    help_lines: &[String],
    plugins: Option<&norte_proto::methods::PluginListResult>,
) {
    let context = norte_tui::help_context::help_context(app);
    let over_modal = app.modal.is_some();
    if refuses_over_modal(lang, context, over_modal) {
        app.message = Some(t("msg-help-no-dialog-page"));
        return;
    }
    // H3d: los hechos del contexto se CONGELAN aquí, antes de la primera
    // maquetación — un veredicto no puede cambiar bajo el cursor del lector a
    // mitad de página (`App::freeze_help_facts`).
    app.freeze_help_facts();
    app.help = Some(HelpView::new_at(
        lang,
        help_lines.to_vec(),
        context,
        over_modal,
    ));
    // H3e: el estado de los plugins se CONGELA con el resto de los hechos, en
    // las dos mitades a la vez (barra lateral y resolver) —
    // `App::freeze_help_plugins`.
    //
    // SIEMPRE, incluso sin catálogo: el resolver vive en `App` y sobrevive al
    // cierre del overlay, así que no congelar aquí dejaría en pie la foto de la
    // ayuda ANTERIOR. Un catálogo vacío es la respuesta honesta a «no lo pude
    // averiguar» — ninguna fila de extensión, y todo comando `plugin:`
    // atenuado — y fail-closed es la dirección en la que equivocarse.
    app.freeze_help_plugins(plugins.map_or(&[], |l| l.plugins.as_slice()));
}

/// Fetches the page of the plugin node the reader just opened (H3e).
///
/// On demand and once: 64 KiB per plugin must not ride every `plugin.list`, and
/// a page already installed is never asked for again within one overlay. The
/// "once" is [`HelpView::claim_plugin_fetch`]'s job — `plugin_needs_fetch` is a
/// POLLING question and this runs on every turn of the run loop, so without the
/// claim a dead daemon would be re-asked at frame rate.
///
/// A failure is SILENT on purpose — an empty page with the plugin's name is a
/// better answer than an error toast over a help overlay, and a daemon N-1
/// without the handler lands here too (`plugin.help` is 0.34.0). The page stays
/// blank for the life of the overlay; closing and reopening the help is the
/// retry.
///
/// The overlay is re-borrowed AFTER the await: the reader may have closed it, or
/// moved to another page, while the answer was in flight.
async fn fetch_plugin_page(backend: &Backend, app: &mut App) {
    let Some(id) = app.help.as_mut().and_then(HelpView::claim_plugin_fetch) else {
        return;
    };
    let Ok(res) = backend.plugin_help(&id).await else {
        return;
    };
    let Some(help) = app.help.as_mut() else {
        return;
    };
    // The publisher comes from the SNAPSHOT, already masked and capped, never
    // from the page: a plugin does not get to say who published it.
    // `parse_untrusted` masks it again, which is harmless.
    let publisher = help.publisher_of(&id);
    // `fold_flags` is not optional: the text arrives already short and already
    // decoded, so this parse comes out clean and the badge — the whole
    // user-facing mitigation for a hostile `help.md` — would go dark.
    let parsed = norte_help::parse_untrusted(res.markdown.as_bytes(), &id, publisher)
        .fold_flags(res.truncated, res.lossy);
    help.state.install_plugin_topic(parsed.topic);
}

/// The page that documents the palette row under the cursor, if one does (H3c).
///
/// The other direction of the bridge H3b built: from the help, `Ctrl+P` carries
/// the filter into the palette; from the palette, `F1` opens the page about the
/// highlighted command. Two views of one model at two densities, so crossing
/// between them should not cost the reader a re-type.
///
/// A PLUGIN row is answered `None` explicitly. Its key is
/// `plugin:{id}:{command}` ([`parse_plugin_key`]), which no corpus page
/// documents and which is not a host command either — the `command_id` half
/// comes from a third-party manifest with no validated charset, so it must never
/// be handed to a lookup as if it were one of ours. The corpus lookup would also
/// answer `None` on its own; the guard is what makes that a decision instead of
/// a coincidence, and it is the same `key`/`text` split the palette already
/// makes between dispatch and paint.
fn palette_help_target(app: &App, lang: norte_help::Lang) -> Option<&'static norte_help::Topic> {
    let key = app.palette.as_ref().and_then(Palette::selected)?;
    if parse_plugin_key(&key).is_some() {
        return None;
    }
    norte_help::topic_for_command(lang, &key)
}

/// `F1` inside the command palette: open the page for the highlighted row, or
/// say that no page documents it (H3c).
///
/// On success the palette CLOSES — the help takes the screen and the next key
/// belongs to what the reader is looking at — and the page arrives as the root
/// of the trail ([`HelpView::new_at_topic`]), so one `Esc` leaves it.
///
/// On failure the palette STAYS and the status bar says so. Opening the index
/// instead would be worse than nothing: the reader asked about one command and
/// would land on a table of contents, with no way to tell whether their command
/// is in there somewhere or simply undocumented.
///
/// `over_modal` is `false` and not `app.modal.is_some()`: the palette's arm of
/// the key chain only runs when no modal is on screen (`modal_wins`), so there
/// is no modal for this help to have been opened over.
fn palette_help(app: &mut App, lang: norte_help::Lang, help_lines: &[String]) {
    match palette_help_target(app, lang).map(|topic| topic.id.clone()) {
        Some(id) => {
            app.palette = None;
            // H3d: mismo congelado que `open_contextual_help` — la ayuda que
            // se abre desde la palette es la misma ayuda.
            app.freeze_help_facts();
            app.help = Some(HelpView::new_at_topic(
                lang,
                help_lines.to_vec(),
                &id,
                false,
            ));
        }
        None => app.message = Some(t("msg-palette-no-help")),
    }
}

#[cfg(test)]
mod palette_modal_guard_tests {
    use super::*;

    fn app() -> App {
        let d = VPath::parse("file:///x").expect("wire de test");
        App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
    }

    fn approval_modal() -> Modal {
        Modal::ApproveAgentOp {
            req: norte_proto::methods::PolicyApprovalRequired {
                approval_id: 1,
                session: Some("s1".into()),
                op: "copy".into(),
                paths: vec!["mem:///a".into()],
                ttl_ms: 60_000,
            },
        }
    }

    /// MINOR-4 (H1 close): con SOLO la palette abierta, no hay nada que
    /// preceder — el guard no dispara. Con AMBOS abiertos (un modal llegó
    /// asíncronamente encima de la palette), el modal debe ganar.
    #[test]
    fn modal_preempts_palette_solo_cuando_ambos_estan_abiertos() {
        let mut a = app();
        assert!(!modal_wins(&a), "sin modal, nadie precede a nadie");
        a.palette = Some(Palette::new(Vec::new()));
        assert!(
            !modal_wins(&a),
            "solo la palette abierta: la palette maneja sus teclas normalmente"
        );
        a.modal = Some(approval_modal());
        assert!(
            modal_wins(&a),
            "un modal en vuelo con la palette abierta DEBE ganarle"
        );
    }

    /// El guard vale para CUALQUIER overlay, no solo palette/ajustes: el
    /// modal se pinta el último (por encima de todos), así que la tecla que
    /// el usuario dirige a lo que VE tiene que llegarle. Antes el selector
    /// de tema, el picker de columnas, el gestor de extensiones, el popup de
    /// navegación, el diálogo de búsqueda y la ayuda resolvían PRIMERO y se
    /// comían la respuesta al modal (en los dos con campo de texto, como
    /// texto tecleado; en extensiones, como toggle/borrado del plugin
    /// resaltado).
    #[test]
    fn el_modal_gana_a_todos_los_overlays() {
        let mut a = app();
        a.theme_picker = Some(norte_tui::app::ThemePicker {
            names: Vec::new(),
            cursor: 0,
            original: a.theme.clone(),
        });
        a.extensions = Some(norte_tui::app::ExtensionManager {
            plugins: Vec::new(),
            errors: Vec::new(),
            cursor: 0,
            config: None,
        });
        a.help = Some(norte_tui::app::HelpView::new(
            norte_i18n::Lang::En,
            Vec::new(),
        ));
        assert!(!modal_wins(&a), "sin modal, cada overlay manda en su tecla");
        a.modal = Some(approval_modal());
        assert!(
            modal_wins(&a),
            "con overlays abiertos, el modal sigue ganando la tecla"
        );
    }

    /// #106 (review MAJOR-2): un evento de vigilancia JAMÁS refresca con
    /// un overlay abierto o un quick search tecleándose — `refresh_panes`
    /// se comería las teclas y Esc cambiaría de significado. El evento
    /// queda encolado y dispara al despejarse.
    #[test]
    fn watch_refresh_gateado_por_overlays() {
        let mut a = app();
        assert!(watch_refresh_allowed(&a), "sin overlays: permitido");
        a.modal = Some(approval_modal());
        assert!(!watch_refresh_allowed(&a), "modal abierto: encolado");
        a.modal = None;
        a.help = Some(norte_tui::app::HelpView::new(
            norte_i18n::Lang::En,
            Vec::new(),
        ));
        assert!(!watch_refresh_allowed(&a), "ayuda abierta: encolado");
        a.help = None;
        a.panes[0].quick_start(nav::Mode::Filter);
        assert!(
            !watch_refresh_allowed(&a),
            "quick search tecleándose: encolado"
        );
    }

    /// S3: el mismo caso para `app.settings` — un modal en vuelo (p.ej. una
    /// aprobación de policy) gana sobre el overlay de ajustes abierto.
    #[test]
    fn modal_preempts_settings_solo_cuando_ambos_estan_abiertos() {
        let mut a = app();
        assert!(!modal_wins(&a));
        a.settings = Some(Settings::new(Vec::new()));
        assert!(
            !modal_wins(&a),
            "solo el overlay de ajustes abierto: maneja sus teclas normalmente"
        );
        a.modal = Some(approval_modal());
        assert!(
            modal_wins(&a),
            "un modal en vuelo con ajustes abierto DEBE ganarle"
        );
    }
}

/// `F1` sobre una fila de la command palette (H3c): el puente hacia la página
/// que documenta ese comando, la otra dirección del que H3b ya tendió
/// (`Ctrl+P` desde la ayuda se lleva el filtro).
#[cfg(test)]
mod palette_help_tests {
    use super::*;

    fn app() -> App {
        let d = VPath::parse("file:///x").expect("wire de test");
        App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
    }

    /// La palette abierta con UNA fila, la de `key`, bajo el cursor. Las filas
    /// se construyen a mano y no del keymap efectivo a propósito: lo que se
    /// prueba es qué hace `F1` con la clave de despacho de la fila resaltada, y
    /// una fila de plugin no sale de `COMMANDS`.
    fn app_with_palette_on(key: &str) -> App {
        let mut app = app();
        app.palette = Some(Palette::new(vec![norte_tui::palette::Row {
            key: key.to_owned(),
            text: key.to_owned(),
            desc: "descripción de prueba".to_owned(),
            chord: "—".to_owned(),
        }]));
        app
    }

    /// `F1` sobre una fila de la palette abre la página que documenta ese
    /// comando: los dos son vistas del mismo modelo a dos densidades, así que
    /// cruzar de la rápida a la que explica no debería costar re-teclear.
    #[test]
    fn f1_en_la_palette_abre_la_pagina_del_comando_bajo_el_cursor() {
        let app = app_with_palette_on("pane.copy");
        let abierto =
            palette_help_target(&app, norte_help::Lang::En).expect("pane.copy tiene página");
        assert_eq!(abierto.id.as_str(), "copying");
    }

    /// …y la abre de verdad: la palette se cierra (la tecla siguiente es de la
    /// ayuda, que es lo que se ve) y la página llega como RAÍZ del rastro —
    /// `Esc` cierra el overlay en vez de caminar a un índice que el lector no
    /// pidió, igual que la ayuda contextual de un modal.
    #[test]
    fn abrir_la_pagina_cierra_la_palette_y_llega_sin_historial() {
        let mut app = app_with_palette_on("pane.copy");
        palette_help(&mut app, norte_help::Lang::En, &[]);
        assert!(app.palette.is_none(), "la palette se cierra");
        let help = app.help.as_mut().expect("la ayuda se abrió");
        assert_eq!(help.state.current().as_str(), "copying");
        assert!(
            !help.over_modal,
            "la rama de la palette solo corre sin modal en pantalla"
        );
        assert!(!help.state.back(), "sin historial: Esc cierra");
        assert!(app.message.is_none(), "y nada que disculparse");
    }

    /// Una fila SIN página no abre nada y lo dice: mejor que abrir el índice y
    /// dejar al lector buscando qué tenía que ver con lo que pidió.
    #[test]
    fn una_fila_sin_pagina_lo_dice() {
        // Un id SINTÉTICO, y no un comando real de la allowlist: desde H3h no
        // queda ninguno sin página, así que un test que se apoyara en ese
        // hueco mediría el corpus y no la rama. Esta rama sigue existiendo —
        // `topic_for_command` puede contestar `None` — y lo que se pinta
        // entonces es lo que hay que fijar.
        let mut app = app_with_palette_on("app.no-such-command");
        assert!(palette_help_target(&app, norte_help::Lang::En).is_none());
        palette_help(&mut app, norte_help::Lang::En, &[]);
        assert!(app.help.is_none(), "no se abre el índice por consolar");
        assert!(app.palette.is_some(), "y la palette se queda donde estaba");
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("msg-palette-no-help").as_str())
        );
    }

    /// La `key` de una fila de PLUGIN es `plugin:{id}:{command}` (P1): ningún
    /// tema del corpus la documenta y no es un comando del host. Toma el camino
    /// de «sin página» — ni pánico, ni una página ajena, ni un `Command::parse`
    /// que no le corresponde.
    #[test]
    fn una_fila_de_plugin_toma_el_camino_de_sin_pagina() {
        let mut app = app_with_palette_on("plugin:dev.norte.demo:greet");
        assert!(palette_help_target(&app, norte_help::Lang::En).is_none());
        palette_help(&mut app, norte_help::Lang::En, &[]);
        assert!(app.help.is_none());
        assert!(app.palette.is_some());
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("msg-palette-no-help").as_str())
        );
    }

    /// Sin ninguna fila visible (un filtro que no casa nada) no hay comando que
    /// documentar: mismo camino, sin `unwrap` de por medio.
    #[test]
    fn sin_fila_visible_no_hay_pagina() {
        let mut app = app_with_palette_on("pane.copy");
        for c in "zzzz".chars() {
            app.palette.as_mut().expect("abierta").push_char(c);
        }
        assert!(app.palette.as_ref().expect("abierta").visible().is_empty());
        assert!(palette_help_target(&app, norte_help::Lang::En).is_none());
        palette_help(&mut app, norte_help::Lang::En, &[]);
        assert!(app.help.is_none());
        assert!(app.palette.is_some());
    }
}

/// Aplica el desenlace de un cd a los rellenos paginados en curso: uno nuevo
/// ocupa el hueco DE SU PANE (el rx anterior de ESE pane, dropeado, mata su
/// drenador → suelta el stream, regla 3); un REEMPLAZO del MISMO pane lo
/// suelta (su drenador drenaría el listado viejo sobre el nuevo); un FALLO o
/// un cd ABANDONADO no tocan el pane —sigue en su listado anterior, cuyo
/// relleno continúa siendo válido— así que no tocan el fill (#78). Un
/// `Refreshed` (#118) delega en [`release_refreshed_fill`]: el mismo ritual
/// que [`after_panes_refresh`].
///
/// El hueco es POR PANE ([`Fill`]): un cd de un pane jamás estrangula el
/// relleno del otro.
///
/// `search_run` viaja hasta aquí SOLO por el brazo `Swapped`
/// ([`reconcile_swap`]): también está indexado por pane, y su cruce tiene que
/// pasar antes del [`reap_search_run`] que estos mismos call sites hacen a
/// continuación.
fn apply_cd(
    fill: &mut [Option<Fill>; 2],
    decorate_fetch: &mut [Option<DecorateFetch>; 2],
    last_probed: &mut Probed,
    search_run: &mut Option<SearchRun>,
    outcome: Cd,
) {
    match outcome {
        Cd::Filling { pane, fill: f } => {
            // Listado nuevo (lazy): la dedup de la sonda #52 caduca — la
            // misma entrada re-enfocada debe poder re-hidratarse.
            last_probed.clear();
            fill[pane] = Some(f);
        }
        Cd::Replaced(pane) => {
            last_probed.clear();
            fill[pane] = None;
        }
        // El pane no cambió: su relleno (si lo había) sigue drenando el mismo
        // listado. Soltarlo aquí lo dejaba colgado en `loading=true` (#78).
        // `Suspended` (TOFU) va aquí por la misma razón que `Cancelled`: el
        // pane no se tocó, y encima el reintento lo va a re-listar entero.
        Cd::Failed(..) | Cd::Cancelled | Cd::Suspended => {}
        // #118: Ctrl+R desde `dispatch` — misma semántica que el ritual de
        // los otros disparadores (`after_panes_refresh`), un solo cuerpo.
        // `reap_search_run` no hace falta aquí: `refresh_panes` SALTA los
        // panes virtuales (jamás los saca del modo), así que no hay run de
        // búsqueda que cosechar por este camino.
        Cd::Refreshed(refreshed) => release_refreshed_fill(refreshed, fill, last_probed),
        // `pane.swap`: `App::swap_panes` ya cruzó panes e historiales; aquí
        // se cruza la mitad que vive en el run loop.
        Cd::Swapped => reconcile_swap(fill, decorate_fetch, last_probed, search_run),
    }
}

/// The other half of `pane.swap`: the per-pane state that lives in the run
/// loop rather than in `App`.
///
/// `App::swap_panes` moves the panes and their histories; these four are
/// indexed by pane too, and leaving any of them behind is a bug a green suite
/// does not catch — the listing keeps arriving, just into the wrong half of
/// the screen, the decorations land on somebody else's rows, and the live
/// search's hits pour into the pane the reader is not looking at.
///
/// The watcher needs nothing here: the run loop re-points it from
/// `watch_targets(app)` at the top of EVERY iteration, so the swapped
/// directories reach it on the next tick.
///
/// That rests on two facts, and only one of them is pinned.
/// `swap_tests::watch_targets_sigue_a_los_panes_tras_el_intercambio` proves
/// `watch_targets` is a pure function of `app.panes` — nobody caches a target
/// per side, which is the half that could rot silently. The other half, that
/// the `rewatch` call really is the first statement of the loop body, is
/// ordering no unit test in this file can observe: if someone moved it below
/// the key handling, each pane would watch the other's directory for one
/// tick after a swap. Read the call site before trusting this comment.
fn reconcile_swap(
    fill: &mut [Option<Fill>; 2],
    decorate_fetch: &mut [Option<DecorateFetch>; 2],
    last_probed: &mut Probed,
    search_run: &mut Option<SearchRun>,
) {
    // Un hueco por pane: cruzarlos ES el reconciliado (el índice del array es
    // el pane, y no hay copia del índice dentro del `Fill` que mantener a
    // mano — ver [`Fill`]).
    fill.swap(0, 1);
    // La búsqueda VIVA guarda su pane virtual como el relleno guardaba el suyo,
    // y aquí es donde TIENE que voltear: el mismo call site cosecha con
    // `reap_search_run` justo después de `apply_cd`, y esa cosecha mira
    // `panes[s.pane].virtual_search` — con el índice sin voltear ve el listado
    // ordinario que acaba de llegar del otro lado y cancela la Task en
    // silencio, dejando al otro pane con hits a medias en `Running` para
    // siempre. A lo sumo hay UN run (el pane virtual es uno), así que voltear
    // su índice es todo el cruce que necesita.
    if let Some(s) = search_run.as_mut() {
        s.pane ^= 1;
    }
    // Cada slot lleva su `dir` como guard anti-stale, así que cruzarlos basta:
    // el fetch sigue correspondiendo al listado que ahora está al otro lado.
    decorate_fetch.swap(0, 1);
    // Es una caché de dedup de `stat`, no estado: traducir sus claves cuesta
    // más que volver a sondear, y un sondeo de más es invisible.
    last_probed.clear();
}

/// Núcleo del ritual post-refresh (#117 review, #118): suelta el drenador
/// paginado SOLO si su pane fue re-listado de verdad (soltarlo a ciegas tras
/// un Esc a medias dejaría el pane colgado en `loading` para siempre, #78) e
/// invalida la dedup de la sonda #52 (un listado nuevo re-lazifica las
/// entries). Cuerpo ÚNICO para [`after_panes_refresh`] (run loop) y el brazo
/// `Cd::Refreshed` de [`apply_cd`] (Ctrl+R vía `dispatch`).
fn release_refreshed_fill(
    refreshed: [bool; 2],
    fill: &mut [Option<Fill>; 2],
    last_probed: &mut Probed,
) {
    if refreshed == [false; 2] {
        return;
    }
    for pane in 0..2 {
        if refreshed[pane] {
            fill[pane] = None;
        }
    }
    last_probed.clear();
}

/// Aplica un mensaje del drenador de paginación (ADR 0017) al pane. Si el pane
/// pasó a modo virtual de búsqueda (Alt+F7 sobre un dir aún paginándose,
/// review MAJOR T6), el fill quedó OBSOLETO —`begin_search` vació las
/// entries— y su drenador alimentaría el listado REAL como si fueran hits (el
/// propio root de la búsqueda colándose entre resultados): se suelta el fill y
/// se DESCARTA el lote. Cinturón simétrico al drain-guard de [`drain_search`];
/// el tirante es soltar el fill en `launch_search`.
fn apply_fill_msg(app: &mut App, fill: &mut [Option<Fill>; 2], pane: usize, msg: Option<FillMsg>) {
    if app.panes[pane].virtual_search {
        fill[pane] = None;
        return;
    }
    match msg {
        Some(FillMsg::Batch(batch)) => app.panes[pane].extend_listing(batch),
        Some(FillMsg::Failed) => {
            app.panes[pane].finish_listing();
            app.message = Some(t("msg-list-incomplete"));
            fill[pane] = None;
        }
        None => {
            app.panes[pane].finish_listing();
            fill[pane] = None;
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Args: DIR posicional + `--preset`/`--daemon`/`--socket`. `--help` y
    // `--version` salen ANTES de tocar el terminal (antes se ignoraban como
    // flag desconocido y el binario moría al no poder abrir la TTY).
    let parsed = norte_frontend::cli::parse(std::env::args_os().skip(1), BOOL_FLAGS, VALUE_FLAGS);
    let Some(args) = args_or_exit(parsed)? else {
        return Ok(()); // `--help`/`--version`: ya impreso.
    };
    let (cli_preset, cli_daemon, cli_socket) = (
        args.text("--preset"),
        args.has("--daemon"),
        args.path("--socket"),
    );
    let layers = config::standard_layers();
    let cfg = config::load_async(layers.clone())
        .await
        .context("config inválida")?;
    // Idioma: NORTE_LANG explícito > [ui] lang de la config > entorno.
    let lang = if std::env::var("NORTE_LANG").is_ok_and(|v| !v.is_empty()) {
        norte_i18n::Lang::from_env()
    } else if let Some(l) = &cfg.common.ui_lang {
        norte_i18n::Lang::negotiate(Some(l))
    } else {
        norte_i18n::Lang::from_env()
    };
    let _ = norte_i18n::force(lang);
    let (browse_eff, viewer_eff, dialog_eff) = build_keymaps(&cfg, cli_preset.as_deref())?;
    // Bindings `lua:` descartados del keymap.toml de PROYECTO (seguridad,
    // review M4 Lua): se avisa tras crear la App, jamás descarte mudo. El
    // contexto `global` se fusiona en las TRES pantallas (H1 T2 suma
    // dialog), así que el máximo es el recuento sin dobles (un binding
    // global cuenta en todas).
    let discarded_lua = browse_eff
        .discarded_lua_bindings()
        .max(viewer_eff.discarded_lua_bindings())
        .max(dialog_eff.discarded_lua_bindings());

    let mut backend = make_backend(&cfg, cli_daemon, cli_socket).await?;

    // El DIR posicional manda sobre el `cwd`; se valida aquí para dar un
    // error claro en vez de un listado fallido dentro del TUI ya arrancado.
    let start = start_dir(args.dir)?;
    // #108 b4: columnas y orden desde `[ui.columns]` — resuelto UNA vez;
    // los ids inválidos no rompen el arranque (doctor los reporta). ANTES de
    // los listados iniciales (#117): ellos también piden los attrs
    // configurados — sin esto, las celdas attr nacen en blanco hasta el
    // primer cd/refresh.
    let columns = norte_frontend::columns::ColumnsSettings::resolve(&cfg.common.ui_columns);
    let start_attrs = columns.attr_ids_for(start.scheme());
    let left = initial_pane(&backend, &start, &start_attrs).await?;
    let right = initial_pane(&backend, &start, &start_attrs).await?;
    let mut app = App::new(left, right);
    app.columns = columns;
    // #117: el catálogo del scheme de arranque — incondicional, como el cd
    // (una vez por scheme y sesión; el picker de la tarea 4 lo quiere
    // aunque no haya columnas attr configuradas); un fallo NO tumba el
    // arranque — sin catálogo se pinta con defaults Opaque.
    // H3d: la MISMA respuesta trae las caps (`fs.capabilities` devuelve las
    // dos mitades), así que se cachean juntas — sin ellas la ayuda del primer
    // F1 caería al criterio sintáctico teniendo el dato al alcance.
    if let Ok(both) = backend.capabilities_and_attrs(&start).await {
        cache_capabilities(&mut app, &start, both);
    }
    for i in 0..app.panes.len() {
        app.apply_scheme_sort(i);
    }
    // #107: `[ui] show_hidden` siembra el estado INICIAL de ambos panes;
    // Ctrl+H lo cambia por pane en runtime (el hot-reload no lo pisa — un
    // toggle del usuario no debe deshacerse porque otro campo cambió).
    if let Some(show) = cfg.common.ui_show_hidden {
        for pane in &mut app.panes {
            pane.set_show_hidden(show);
        }
    }
    apply_theme(&mut app, &cfg);
    // Copia de la hotlist en el App (spec 2026-07-18): la fuente del popup
    // `Ctrl+D`; se refresca en cada hot-reload OK (`reload_config`).
    app.hotlist = cfg.common.hotlist.clone();
    // Hints de pie de página de los overlays (H1 T3, #24): PRECOMPUTADOS del
    // efectivo `dialog` ANTES de que se mueva al `Resolver` de abajo — igual
    // que `help_lines`, se reconstruyen en cada hot-reload OK.
    app.dialog_hints = DialogHints::build(&dialog_eff);
    // Openers declarativos (#28): fuente de `pane.open` (F4).
    app.openers = cfg.openers.clone();
    // Canales del modo daemon (None en embebido): tasks de otros frontends
    // y avisos de (re)conexión — se drenan en el loop principal.
    let foreign_tasks = backend.take_foreign_tasks();
    let conn_events = backend.take_conn_events();
    let approvals = backend.take_approvals();
    // #44: avisos `connection.degraded` del daemon → indicador persistente.
    let degraded = backend.take_degraded();
    let mut help_lines = norte_tui::help::build(&browse_eff, &viewer_eff, &dialog_eff);
    // H3b: the chord resolver the help corpus is rendered through. Built from
    // the SAME three effectives as `help_lines` and BEFORE they move into the
    // `Resolver`s below (it borrows), and rebuilt alongside them on every hot
    // reload — the obligation `TuiChords`' own rustdoc states: a rebind that
    // does not reach this resolver is a page that teaches the OLD key.
    app.help_chords = Arc::new(TuiChords::new(&browse_eff, &viewer_eff, &dialog_eff, lang));
    // Filas de la command palette (H1 T4): PRECOMPUTADAS de los efectivos
    // browse/viewer ANTES de que se muevan al `Resolver` de abajo — mismo
    // criterio que `help_lines`/`dialog_hints`.
    app.palette_rows = norte_tui::palette::build_rows(&browse_eff, &viewer_eff);
    let mut resolver = Resolver::new(browse_eff);
    let mut viewer_resolver = Resolver::new(viewer_eff);
    // H1 T2: resolver compartido por TODOS los overlays (modal, theme
    // picker, extensions, nav popup) — mutuamente exclusivos en el run loop
    // (el `if`/`else if` de más abajo), así que un único estado de secuencia
    // basta. Los presets `[dialog]` son de UN chord; un `Resolution::Pending`
    // (solo posible con una secuencia multi-tecla de una capa de usuario) se
    // trata como ignorar-y-reiniciar en cada handler — sin semántica de
    // overlay definida para eso todavía.
    let mut dialog_resolver = Resolver::new(dialog_eff);

    // Hot-reload: vigilancia de las capas, con aviso si degrada a polling.
    let (cfg_tx, cfg_rx) = tokio::sync::mpsc::channel(8);
    let watch = config::watch(&layers, cfg_tx).await;
    if watch.mode == WatchMode::Polling {
        app.message = Some(t("msg-config-polling"));
    }
    // DESPUÉS del aviso de polling: el de seguridad no debe quedar pisado.
    if discarded_lua > 0 {
        app.message = Some(ta(
            "msg-lua-keymap-project",
            &[("n", &discarded_lua.to_string())],
        ));
    }

    let mut terminal = ratatui::init();
    let mut capture = arm_mouse(&cfg, &mut app);
    let res = run(
        &mut terminal,
        &mut capture,
        &mut app,
        &backend,
        &mut resolver,
        &mut viewer_resolver,
        &mut dialog_resolver,
        &mut help_lines,
        lang,
        layers,
        cli_preset,
        cfg.quick_search_mode,
        cfg.common.ui_confirm_quit,
        cfg,
        cfg_rx,
        foreign_tasks,
        conn_events,
        approvals,
        degraded,
    )
    .await;
    let _ = capture.set(false, &mut std::io::stdout());
    ratatui::restore();
    drop(watch);
    res
}

/// Pide la captura de ratón si `[ui] mouse` no la desactiva (default ON).
///
/// Se pide ANTES de entrar al run loop y `main` la retira SIEMPRE al salir,
/// pase lo que pase: una terminal devuelta en modo ratón escupe secuencias
/// de escape en cuanto el usuario mueve el puntero, y para entonces ya no
/// queda nadie escuchándolas.
///
/// Un emulador que no acepte la secuencia no es motivo para no arrancar: se
/// sigue sin ratón y se dice por la barra, jamás en silencio (el usuario
/// hará click y no pasará nada).
fn arm_mouse(cfg: &config::LoadedConfig, app: &mut App) -> mouse::Capture {
    // El hook de pánico de `ratatui::init` sale de la pantalla alternativa y
    // del raw mode, pero la captura de ratón es un DECSET de la terminal
    // entera: no se va con la pantalla. Sin esto, un panic dejaría al
    // usuario en su shell con el ratón todavía capturado, escupiendo
    // secuencias que ya no lee nadie. Se envuelve el hook vigente (el de
    // ratatui, ya instalado) en vez de sustituirlo: primero se suelta el
    // ratón, después él restaura lo suyo e imprime el panic.
    let previo = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
        previo(info);
    }));
    let mut capture = mouse::Capture::new();
    if let Err(e) = capture.set(cfg.common.ui_mouse.unwrap_or(true), &mut std::io::stdout()) {
        tracing::warn!(error = %e, "no se pudo activar la captura de ratón");
        app.message = Some(t("msg-mouse-capture-failed"));
    }
    capture
}

/// Flags booleanos del TUI.
const BOOL_FLAGS: &[&str] = &["--daemon"];
/// Flags con valor del TUI.
const VALUE_FLAGS: &[&str] = &["--preset", "--socket"];

/// Texto de `--help`. En INGLÉS y sin Fluent a propósito: se imprime ANTES
/// de negociar el idioma (que sale de la config, que aún no se ha leído).
const USAGE: &str = "\
norte-tui — orthodox file manager, terminal frontend

Usage: norte-tui [OPTIONS] [DIR]

Arguments:
  [DIR]  Directory to start in (default: the current directory)

Options:
      --preset <NAME>  Keymap preset (orthodox|vim|cua); overrides norte.toml
      --daemon         Talk to the daemon instead of the embedded core
      --socket <PATH>  Daemon socket (default: $XDG_RUNTIME_DIR/norte/daemon.sock)
  -h, --help           Print help
  -V, --version        Print version
";

/// Directorio de arranque como [`VPath`]: el `[DIR]` de la línea de
/// comandos si vino, si no el `cwd`. Se valida ANTES de tomar el terminal
/// para dar un error legible en vez de un listado fallido dentro del TUI ya
/// arrancado.
///
/// Un cwd UNC de Windows (`\\server\share`, `\\wsl$\…`) ya round-trip-ea:
/// `vpath_from_native` lo mete como primer segmento y `to_native` lo
/// restituye como base de la raíz del OS (#22). Uno irrepresentable da
/// error claro, jamás un panic.
fn start_dir(dir: Option<std::path::PathBuf>) -> Result<VPath> {
    let nativo = match dir {
        Some(d) => {
            let meta = std::fs::metadata(&d)
                .with_context(|| format!("no se puede abrir {}", d.display()))?;
            anyhow::ensure!(meta.is_dir(), "{} no es un directorio", d.display());
            std::path::absolute(&d).unwrap_or(d)
        }
        None => std::env::current_dir().context("cwd")?,
    };
    norte_vfs_local::vpath_from_native(&nativo)
        .map_err(|e| anyhow::anyhow!("{} no representable como VPath: {e}", nativo.display()))
}

/// Resuelve los argumentos "de salida inmediata": imprime `--help`/
/// `--version` (devolviendo `None`, el caller termina) y convierte un flag
/// desconocido en error. Antes `--help` caía en el brazo de "ignora" y el
/// binario seguía hasta intentar tomar la TTY, donde moría con un panic de
/// ratatui.
fn args_or_exit(args: norte_frontend::cli::Cli) -> Result<Option<norte_frontend::cli::Cli>> {
    if args.help {
        print!("{USAGE}");
        return Ok(None);
    }
    if args.version {
        println!("norte-tui {}", env!("CARGO_PKG_VERSION"));
        return Ok(None);
    }
    if let Some(flag) = &args.unknown {
        anyhow::bail!("unknown flag `{flag}` — try `norte-tui --help`");
    }
    Ok(Some(args))
}

/// Elige el transporte (regla 7): `--daemon` o `[daemon] mode = daemon`
/// conecta al socket (arrancando `norte daemon run` si hace falta);
/// cualquier otra cosa = embebido (arranque instantáneo, el default).
async fn make_backend(
    cfg: &config::LoadedConfig,
    cli_daemon: bool,
    cli_socket: Option<std::path::PathBuf>,
) -> Result<Backend> {
    let want_daemon = cli_daemon || cfg.common.daemon_mode == Some(config::DaemonMode::Daemon);
    if !want_daemon {
        let engine = Engine::new();
        // #95.2: límites anti-bomba de archives desde `[archive]` (capas de
        // usuario, jamás la de proyecto). Antes de cualquier navegación: los
        // providers compuestos se cachean con los límites de su primer uso.
        // rust review item 3 (C1): la conversión override+saturación vivía
        // duplicada aquí y en `norte_core::archive_config` — un único home
        // en el core (`limits_from_overrides`) para que TUI y daemon jamás
        // diverjan en los límites anti-bomba.
        if let Some(limits) = norte_core::archive_config::limits_from_overrides(
            cfg.common.archive_max_entries,
            cfg.common.archive_max_decompressed_bytes,
            cfg.common.archive_max_nesting,
        ) {
            engine.set_archive_limits(limits);
        }
        engine.register_provider(Arc::new(LocalProvider::os_root()));
        // Conexiones remotas (fase 6e): un path sftp://…/ftp://… navegable si
        // la host key ya es de confianza. La CONFIRMACIÓN TOFU interactiva
        // (modal con fingerprint) es UX pendiente — hoy un primer contacto
        // aparece como error con la huella; confírmalo con `norte connect`.
        engine.set_connector(Arc::new(norte_core::connect::ConnectionManager::new(
            norte_core::connect::config_dir(),
        )));
        // IA (M4-IA): opt-in; sin [ai] el backend degrada (Unsupported).
        // Diagnóstico por eprintln, no tracing (rust review MINOR-4): el TUI
        // no instala subscriber (`logging::init` es de cli/daemon; un fmt a
        // stderr pelearía con la pantalla alternativa) — un `tracing::warn!`
        // aquí se descartaría mudo, y este punto es PRE-ratatui, donde stderr
        // aún llega al terminal. Mismo patrón que el wiring del daemon-run.
        match tokio::task::spawn_blocking(norte_core::ai::AiConfig::load).await {
            Ok(Ok(ai_cfg)) => {
                if let Some(pcfg) = ai_cfg.rename_provider_config().cloned() {
                    match norte_core::ai::resolve_and_build(
                        &pcfg,
                        norte_core::connect::config_dir(),
                    )
                    .await
                    {
                        Ok(provider) => engine.set_ai_provider(provider),
                        Err(e) => eprintln!("aviso: proveedor de IA no disponible ({e})"),
                    }
                }
                // Embeddings (M4-IA-2): proveedor propio, opt-in igual —
                // future-proofing del plan: la TUI embebida aún no lleva
                // índice (with_index es del daemon), el wiring es por paridad
                // para cuando lo gane.
                if let Some(w) = norte_core::ai::install_embed_provider(&engine, &ai_cfg).await {
                    eprintln!("{w}");
                }
                engine.set_ai_config(ai_cfg);
            }
            Ok(Err(e)) => eprintln!("aviso: [ai] inválido ({e})"),
            Err(e) => eprintln!("aviso: carga de [ai] falló ({e})"),
        }
        return Ok(Backend::Embedded(Arc::new(engine)));
    }
    #[cfg(not(unix))]
    {
        let _ = (cli_socket, cfg);
        anyhow::bail!("el modo daemon no está disponible en Windows todavía (issue #33)");
    }
    #[cfg(unix)]
    {
        use norte_core::backend::remote::RemoteBackend;
        let socket = match cli_socket.or_else(|| cfg.common.daemon_socket.clone()) {
            Some(s) => s,
            None => tokio::task::spawn_blocking(|| norte_core::daemon::default_socket_path(None))
                .await
                .context("resolución del socket")?,
        };
        let exe = std::env::current_exe().context("current_exe")?;
        // El binario del daemon es `norte` (la CLI), no `norte-tui`: junto
        // al ejecutable actual dentro del mismo directorio de instalación.
        let daemon_bin = exe.with_file_name("norte");
        let mut spawn_cmd: Vec<std::ffi::OsString> =
            vec![daemon_bin.into(), "daemon".into(), "run".into()];
        spawn_cmd.push("--socket".into());
        spawn_cmd.push(socket.clone().into());
        let remote = RemoteBackend::connect(
            socket,
            Some(spawn_cmd),
            norte_proto::methods::ClientInfo {
                name: "norte-tui".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("no se pudo hablar con el daemon")?;
        Ok(Backend::Remote(remote))
    }
}

/// Resuelve el preset (flag > config > default) y pliega las capas de
/// keymap (ADR 0007) para las TRES pantallas (browse, viewer, dialog — H1
/// T2). El error tipado ([`KeymapsError`], #73) vive en `norte_tui::app`
/// junto a su categoría Fluent.
fn build_keymaps(
    cfg: &config::LoadedConfig,
    cli_preset: Option<&str>,
) -> Result<(Effective, Effective, Effective), KeymapsError> {
    let preset_name = cli_preset.unwrap_or(&cfg.common.preset);
    let presets = presets();
    let (_, preset) = presets
        .iter()
        .find(|(n, _)| *n == preset_name)
        .ok_or_else(|| KeymapsError::UnknownPreset {
            name: preset_name.to_owned(),
            available: presets
                .iter()
                .map(|(n, _)| *n)
                .collect::<Vec<_>>()
                .join(", "),
        })?;
    let invalid = |e: norte_tui::keymap::KeymapError| KeymapsError::Invalid {
        detail: e.to_string(),
    };
    let browse = Effective::build_for(preset, &cfg.keymap_layers, COMMANDS, Screen::Browse)
        .map_err(invalid)?;
    let viewer = Effective::build_for(preset, &cfg.keymap_layers, COMMANDS, Screen::Viewer)
        .map_err(invalid)?;
    // Screen::Dialog fusiona `[dialog] ∪ [global]` (ADR 0006/H1 T1):
    // `build_for_impl` valida TODO el efectivo fusionado contra
    // `known_commands`, así que un binding GLOBAL (p. ej. `ctrl+c →
    // app.quit`) se validaría como `UnknownCommand` si solo pasáramos
    // `DIALOG_COMMANDS`. La UNIÓN con `COMMANDS` es la opción simple (T1 lo
    // deja elegido): inofensiva porque cada overlay ALLOWLISTEA solo sus
    // `dialog.*` soportados (`app::dialog_action` y las resoluciones ad hoc
    // de este módulo) y descarta cualquier otro comando resuelto.
    let dialog_known: Vec<&str> = COMMANDS
        .iter()
        .copied()
        .chain(DIALOG_COMMANDS.iter().copied())
        .collect();
    let dialog = Effective::build_for(preset, &cfg.keymap_layers, &dialog_known, Screen::Dialog)
        .map_err(invalid)?;
    Ok((browse, viewer, dialog))
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)] // wiring del binario, no API
async fn run(
    terminal: &mut ratatui::DefaultTerminal,
    // Captura de ratón: la crea `main` (dueño de la terminal) y la retira
    // al salir; aquí se ENCIENDE y se APAGA en caliente (`[ui] mouse`) y se
    // suelta alrededor de cada suspensión por opener externo.
    capture: &mut mouse::Capture,
    app: &mut App,
    backend: &Backend,
    resolver: &mut Resolver,
    viewer_resolver: &mut Resolver,
    dialog_resolver: &mut Resolver,
    help_lines: &mut Vec<String>,
    // H3b: the NEGOTIATED language (`NORTE_LANG` > `[ui] lang` > environment,
    // the same value handed to `norte_i18n::force`). The help corpus is
    // per-locale, so the overlay must open on the locale the rest of the UI
    // already speaks — `Lang::from_env()` here would hand a reader whose
    // `[ui] lang` says `es` an English corpus inside a Spanish UI. Fixed for
    // the session: `force` is called once, so the hot reload keeps this value.
    lang: norte_i18n::Lang,
    layers: Layers,
    cli_preset: Option<String>,
    // Modo del quick search (`[ui] quick_search`): vive en el run loop como
    // el preset CLI y se actualiza en el hot-reload de config.
    mut quick_mode: nav::Mode,
    // `[ui] confirm_quit` (S2): mismo patrón que `quick_mode` — vive en el
    // run loop, `applies_live` (solo afecta a `app.quit` NUEVOS, uno en
    // curso ya decidió) y se actualiza en el hot-reload.
    mut confirm_quit: config::ConfirmQuit,
    // S3 (`app.settings`): la config COMPLETA vive aquí, no solo los campos
    // sueltos de arriba — el overlay de ajustes necesita leer CUALQUIER
    // entrada del catálogo (`crate::settings::build_rows`), no una lista
    // fija. Se actualiza ENTERA en cada hot-reload OK (`reload_config`, al
    // final, tras aplicar todo lo demás — mismo criterio que `quick_mode`/
    // `confirm_quit`: solo si TODO aplicó).
    mut cfg: config::LoadedConfig,
    mut cfg_rx: tokio::sync::mpsc::Receiver<()>,
    mut foreign_tasks: Option<tokio::sync::mpsc::UnboundedReceiver<norte_core::backend::TaskRef>>,
    mut conn_events: Option<tokio::sync::mpsc::UnboundedReceiver<ConnEvent>>,
    mut approvals: Option<
        tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::PolicyApprovalRequired>,
    >,
    mut degraded: Option<
        tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::ConnectionDegraded>,
    >,
) -> Result<()> {
    let mut events = EventStream::new();
    // Tick del panel de tasks: copia snapshots del watch (jamás bloquea).
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Debounce del hot-reload SIN bloquear el loop (revisión fase 6): cada
    // evento de config empuja el deadline; el reload corre cuando vence.
    let mut reload_at: Option<tokio::time::Instant> = None;
    // Listados paginados rellenándose en background (ADR 0017): un hueco POR
    // PANE — los dos panes pueden estar paginando a la vez, y con un hueco
    // global el cd de uno mataba el drenador del otro (ver [`Fill`]).
    let mut fill: [Option<Fill>; 2] = [None, None];
    // Búsqueda viva en curso (liveSearch T6): a lo sumo una (el pane virtual
    // es uno). Molde `Fill`: se drena en el select y se suelta al salir.
    let mut search_run: Option<SearchRun> = None;
    // Petición ai.rename_plan en vuelo (M4-IA): a lo sumo una — relanzar
    // aborta la anterior. Se cosecha en el select y Esc (BROWSE) la cancela.
    let mut ai_rename_run: Option<AiRenameRun> = None;
    // Plan IA listo llegado con OTRO modal abierto: se RETIENE aquí (la cola
    // de `App` es específica de aprobaciones) y se abre en cuanto no haya
    // modal — jamás pisar (disciplina `open_next_pending`).
    let mut pending_ai_plan: Option<(VPath, Vec<norte_proto::methods::AiRenameEntry>)> = None;
    // Búsqueda semántica en vuelo (M4-IA-2): mismo molde que `ai_rename_run`
    // — a lo sumo una, relanzar aborta la anterior, Esc (BROWSE) cancela.
    let mut semantic_run: Option<SemanticRun> = None;
    // Hits listos llegados con OTRO modal abierto: se RETIENEN aquí y se
    // abren en cuanto no haya modal (disciplina `pending_ai_plan`).
    let mut pending_semantic: Option<Vec<norte_proto::methods::SemanticHit>> = None;
    // Scripting Lua (M4, ADR 0026): host por capas con trust TOFU. Como
    // `fill`, el estado vive en el run loop. El run en vuelo (a lo sumo UNO:
    // el estado Lua es uno) se pollea inline en el select — `CommandRun` es
    // !Send y este future corre en block_on, jamás en spawn.
    let mut lua_host = load_lua(app, &layers).await;
    let mut lua_run: Option<(CommandRun, CancellationToken)> = None;
    let mut lua_queue: VecDeque<String> = VecDeque::new();
    // Sonda de stat on-focus (#52, listado lazy): a lo sumo una en vuelo,
    // dedup por (pane, path) — dos panes sobre el MISMO dir deben hidratar
    // cada uno la suya (no reintenta un stat fallido hasta cambiar
    // selección).
    let mut stat_probe: Option<StatProbe> = None;
    let mut last_probed: Probed = Probed::new();
    // Fetch de decoraciones de plugin en vuelo (G3b, ADR 0037): a lo sumo
    // uno, molde de `stat_probe`/`fill`.
    let mut decorate_fetch: [Option<DecorateFetch>; 2] = [None, None];
    // #106 (watching): vigilancia de los dirs visibles — notify con
    // fallback a sondeo (pitfall inotify). El conjunto vigilado se
    // re-sincroniza en CADA vuelta (diff barato, no-op sin cambios).
    // Regla 2, exención puntual (review MINOR-6): crear el watcher y los
    // watch()/unwatch() de rewatch son syscalls cortas inline (mismo
    // criterio documentado que el draw síncrono de ratatui más abajo);
    // solo corren al arrancar o al CAMBIAR de dir.
    let mut dir_watch = norte_tui::watch::DirWatch::new();
    let mut dir_watch_alive = true;
    loop {
        dir_watch.rewatch(&watch_targets(app));
        if dir_watch.take_degraded_notice() {
            app.message = Some(t("status-watch-degraded"));
        }
        // Review MINOR-1: `over_modal` describe el modal que hay AHORA, no uno
        // que ya se contestó. Antes de plantar los modales retenidos de abajo,
        // que tienen que encontrar la bandera ya limpia.
        settle_help_over_modal(app);
        // Plan IA retenido (M4-IA): abre en cuanto el modal activo se cierra.
        // Las aprobaciones no compiten aquí: con la cola no vacía y sin modal,
        // `open_next_pending` ya habría abierto una al cerrarse el anterior.
        if app.modal.is_none()
            && let Some((dir, entries)) = pending_ai_plan.take()
        {
            app.modal = Some(Modal::AiRenamePlan {
                dir,
                entries,
                offset: 0,
            });
        }
        // Hits semánticos retenidos (M4-IA-2): misma disciplina. Si el plan
        // IA de arriba acaba de abrir, el `is_none` los deja esperando.
        if app.modal.is_none()
            && let Some(hits) = pending_semantic.take()
        {
            app.modal = Some(Modal::SemanticHits {
                hits,
                offset: 0,
                cursor: 0,
            });
        }
        // Barra Lua en cada vuelta, ANTES del draw (cacheada en el host).
        refresh_lua_status(app, lua_host.as_ref());
        // H3b: la ayuda se MAQUETA para el terminal sobre el que va a
        // pintarse, justo antes del draw — el modelo acota su scroll contra
        // el número de líneas que salieron, y solo el render lo sabe (ver
        // `HelpView::refresh`). Cada vuelta, no solo al cambiar de tema: un
        // resize no pasa por ninguna tecla.
        if let Some(lang) = app.help.as_ref().map(|h| h.state.lang()) {
            // H3e: la página de un nodo de plugin se pide AQUÍ, bajo demanda y
            // una sola vez por overlay (`fetch_plugin_page`). Antes de
            // maquetar, para que la página recién llegada se pinte en ESTE
            // frame y no en el siguiente.
            fetch_plugin_page(backend, app).await;
            let size = terminal.size()?;
            let (ancho, alto) = ui::help_body_size(
                ratatui::layout::Rect::new(0, 0, size.width, size.height),
                lang,
            );
            app.refresh_help(ancho, alto);
        }
        // Exención puntual de la regla 2: el draw escribe stdout síncrono
        // (patrón async oficial de ratatui; acotado, runtime multi-thread).
        let pintado = terminal.draw(|f| ui::draw(f, app))?;
        if app.quit {
            return Ok(());
        }
        // #124: el alto REAL del viewport vuelve al modelo tras cada frame —
        // la paginación (`page_step`) y el radio de la sonda de stat salen de
        // ahí en vez de constantes que mienten en cualquier terminal que no
        // mida justo eso. Con el visor abierto son 0 filas (ningún pane
        // pintado) y el modelo vuelve a sus fallbacks.
        let filas = usize::from(ui::pane_list_rows(app, pintado.area.height));
        for pane in &mut app.panes {
            pane.set_viewport_rows(filas);
        }
        // MISMO trato para la geometría del ratón: el draw es quien sabe
        // dónde cayó cada pane y con qué scroll, así que la devuelve al
        // modelo y el hit test resuelve contra la pantalla que el usuario
        // está mirando. Sin esto habría que recalcular el layout en cada
        // click, y un click resuelto contra un layout que no es el pintado
        // no falla ruidosamente: marca el fichero de al lado.
        mouse::after_frame(app, ui::pane_geometry(app, pintado.area));
        // #52: listado lazy — las entradas VISIBLES sin size se hidratan por
        // tandas (máx. una en vuelo; dedup por (pane, path) en `last_probed`).
        if stat_probe.is_none() {
            let tanda: Vec<(usize, VPath)> = app
                .needs_stat_window(STAT_WINDOW_RADIUS)
                .into_iter()
                .filter(|c| !last_probed.contains(c))
                .take(STAT_BATCH_MAX)
                .collect();
            if !tanda.is_empty() {
                last_probed.extend(tanda.iter().cloned());
                stat_probe = Some(spawn_stat_probe(backend, tanda));
            }
        }
        tokio::select! {
            _ = tick.tick() => {
                // Mutación terminada → refresh de panes; el ritual completo
                // (drenador/sonda #52/búsqueda) vive en `after_panes_refresh`
                // — ÚNICO para los tres disparadores del refresh (#117).
                let refreshed = on_tick(app, backend, &mut events).await;
                after_panes_refresh(app, refreshed, &mut fill, &mut last_probed, &mut search_run);
            }
            ev = dir_watch.rx.recv(), if dir_watch_alive && watch_refresh_allowed(app) => {
                // #106: cambio EXTERNO en un dir vigilado (debounced) —
                // mismo camino que pane.refresh (Ctrl+R): refresh
                // cancelable + ritual #118. GATEADO (review MAJOR-2): con
                // un overlay/quick abierto, `refresh_panes` se comería las
                // teclas del usuario y Esc cambiaría de significado — la
                // precondición deja el evento ENCOLADO (canal de capacidad
                // 1) y dispara al cerrarse el overlay.
                if let Some(()) = ev {
                    let refreshed = refresh_panes(app, backend, &mut events).await;
                    after_panes_refresh(
                        app,
                        refreshed,
                        &mut fill,
                        &mut last_probed,
                        &mut search_run,
                    );
                } else {
                    // Inalcanzable con `dir_watch` vivo (retiene el emisor
                    // crudo): si pasara, DESARMAR el brazo — un canal
                    // cerrado devolvería None en bucle (spin al 100%,
                    // review MINOR-1).
                    tracing::warn!("dir watch pipeline murió; brazo desarmado");
                    dir_watch_alive = false;
                }
            }
            Some(task) = async {
                match &mut foreign_tasks {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                // Task de OTRO frontend de la misma sesión (fase 3): al panel.
                app.board.push_foreign(task);
            }
            Some(ev) = async {
                match &mut conn_events {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                app.message = Some(match ev {
                    ConnEvent::Lost => t("msg-daemon-lost"),
                    ConnEvent::Restored => t("msg-daemon-restored"),
                });
            }
            Some(req) = async {
                match &mut approvals {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                // Aprobación de policy pendiente (M3-3b T5): a la cola de
                // diálogos (jamás pisa un modal abierto) y se abre si procede.
                app.pending_approvals.push_back(req);
                app.open_next_pending();
            }
            Some(d) = async {
                match &mut degraded {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                // #44: sesión remota degradó a texto plano — indicador
                // PERSISTENTE en la status bar (no pisa `message` transitorio).
                // H3d: se retiene el valor ESTRUCTURADO, no la frase — la barra
                // la compone (`App::connection_banner`) y la ayuda puede
                // preguntar por scheme cuál se degradó.
                app.note_degraded(d);
            }
            res = async {
                match &mut stat_probe {
                    Some(pr) => (&mut pr.rx).await.ok(),
                    None => std::future::pending().await,
                }
            } => {
                // Sonda de stat del viewport (#52): el slot se limpia SIEMPRE
                // (haya hidratado algo, fallara el stat o se cerrara el canal)
                // — la dedup por `last_probed` evita reintentar hasta que un
                // listado nuevo la vacíe.
                stat_probe = None;
                for (pane, path, entry) in res.unwrap_or_default() {
                    app.panes[pane].hydrate(&path, entry.size, entry.mtime_ms);
                }
            }
            (slot, res) = async {
                // Un slot POR PANE (review MINOR-2): se espera al primero
                // que responda; con ambos vacíos, pendiente.
                let [slot_a, slot_b] = &mut decorate_fetch;
                match (slot_a, slot_b) {
                    (Some(a), Some(b)) => tokio::select! {
                        r = &mut a.rx => (0, r.ok()),
                        r = &mut b.rx => (1, r.ok()),
                    },
                    (Some(a), None) => (0, (&mut a.rx).await.ok()),
                    (None, Some(b)) => (1, (&mut b.rx).await.ok()),
                    (None, None) => std::future::pending().await,
                }
            } => {
                // Fetch de decoraciones (G3b): se limpia SIEMPRE. Una
                // respuesta tardía cuyo `dir` ya no case el del pane (el
                // usuario cd'eó de nuevo mientras estaba en vuelo) se
                // DESCARTA — nunca pinta badges de un listado que ya no se
                // ve (mismo criterio anti-stale que el drain-guard de
                // `apply_fill_msg` para búsqueda virtual).
                if let Some(f) = decorate_fetch[slot].take()
                    && let Some((map, cols)) = res
                    && app.panes[f.pane].dir() == &f.dir
                {
                    app.panes[f.pane].set_decorations(map);
                    // #117-follow-up: los valores de columnas plugin: viajan
                    // en el mismo fetch y comparten el guard anti-stale.
                    app.panes[f.pane].set_plugin_columns(cols);
                }
            }
            (pane, msg) = async {
                // Un hueco POR PANE (igual que `decorate_fetch`): se espera al
                // primero que hable; con los dos vacíos, pendiente. `recv` es
                // cancel-safe, así que perder la carrera no pierde el lote del
                // otro brazo.
                let [slot_a, slot_b] = &mut fill;
                match (slot_a, slot_b) {
                    (Some(a), Some(b)) => tokio::select! {
                        m = a.rx.recv() => (0, m),
                        m = b.rx.recv() => (1, m),
                    },
                    (Some(a), None) => (0, a.rx.recv().await),
                    (None, Some(b)) => (1, b.rx.recv().await),
                    (None, None) => std::future::pending().await,
                }
            } => {
                // Lote del drenador del listado paginado (ADR 0017): al pane
                // de SU hueco. `None` = canal cerrado (fin del drenado).
                apply_fill_msg(app, &mut fill, pane, msg);
            }
            hits = async {
                // Solo se drena mientras el run sigue vivo (`Running`): un
                // canal cerrado devolvería `None` en bucle (spin) — al leer el
                // `None` se pasa a terminal y este brazo queda pendiente.
                match &mut search_run {
                    Some(s) if s.state == SearchState::Running => s.rx.recv().await,
                    _ => std::future::pending().await,
                }
            } => {
                drain_search(app, &mut search_run, hits);
            }
            res = async {
                // ai.rename_plan en vuelo (M4-IA): cosecha sin bloquear —
                // el brazo solo se arma con un run vivo (molde stat_probe).
                match &mut ai_rename_run {
                    Some(r) => (&mut r.handle).await,
                    None => std::future::pending().await,
                }
            } => {
                if let Some(run) = ai_rename_run.take() {
                    match res {
                        Ok(Ok(plan)) if plan.entries.is_empty() => {
                            app.message = Some(t("msg-ai-rename-empty"));
                        }
                        // Cinturón de INGESTIÓN (quality review 78eb243
                        // MINOR-5): un plan legítimo del engine queda muy
                        // por debajo del tope; superarlo delata un daemon
                        // hostil/N+1 inflando la respuesta — rechazo en
                        // bloque, ni se abre el modal.
                        Ok(Ok(plan))
                            if plan.entries.len() > norte_frontend::MAX_AI_PLAN_ENTRIES =>
                        {
                            app.message = Some(t("msg-ai-rename-invalid-plan"));
                        }
                        Ok(Ok(plan)) => {
                            app.message = None;
                            let ready = (run.dir, plan.entries);
                            if app.modal.is_none() {
                                let (dir, entries) = ready;
                                app.modal = Some(Modal::AiRenamePlan {
                                    dir,
                                    entries,
                                    offset: 0,
                                });
                            } else {
                                // Otro modal abierto (aprobación, colisión…):
                                // el plan espera su turno, jamás lo pisa. A
                                // diferencia de la GUI (banner superseded), aquí
                                // el overwrite es inalcanzable: run único en
                                // vuelo y el prompt no abre sobre otro modal.
                                pending_ai_plan = Some(ready);
                            }
                        }
                        Ok(Err(e)) => {
                            app.message = Some(ta(
                                "msg-ai-rename-failed",
                                &[("error", &detail_for_bar(&error_category(&e)))],
                            ));
                        }
                        // Abortado por Esc: silencio, la barra ya se limpió.
                        // (Un pánico del future del backend cae aquí también:
                        // no hay plan que abrir, el run ya está cosechado.)
                        Err(_join) => {}
                    }
                }
            }
            res = async {
                // index.search_semantic en vuelo (M4-IA-2): cosecha sin
                // bloquear — molde del brazo de `ai_rename_run`.
                match &mut semantic_run {
                    Some(r) => (&mut r.handle).await,
                    None => std::future::pending().await,
                }
            } => {
                semantic_run = None;
                match res {
                    Ok(Ok(hits)) if hits.is_empty() => {
                        app.message = Some(t("msg-semantic-empty"));
                    }
                    // Cinturón de INGESTIÓN (paridad IA-1): una respuesta
                    // por encima del techo contractual del server o con un
                    // score no finito delata un daemon hostil/N+1 — rechazo
                    // en bloque, ni se abre el modal (el guard es
                    // `norte_frontend::validate_semantic_hits`, pura y
                    // compartida con la GUI).
                    Ok(Ok(hits)) => match norte_frontend::validate_semantic_hits(hits) {
                        None => {
                            app.message = Some(t("msg-semantic-invalid"));
                        }
                        Some(hits) => {
                            app.message = None;
                            if app.modal.is_none() {
                                app.modal = Some(Modal::SemanticHits {
                                    hits,
                                    offset: 0,
                                    cursor: 0,
                                });
                            } else {
                                // Otro modal abierto (aprobación, colisión…):
                                // los hits esperan su turno, jamás lo pisan.
                                pending_semantic = Some(hits);
                            }
                        }
                    },
                    Ok(Err(e)) => {
                        app.message = Some(ta(
                            "msg-semantic-failed",
                            &[("error", &detail_for_bar(&error_category(&e)))],
                        ));
                    }
                    // Abortado por Esc: silencio, la barra ya se limpió.
                    // (Un pánico del future del backend cae aquí también:
                    // no hay hits que abrir, el run ya está cosechado.)
                    Err(_join) => {}
                }
            }
            outcome = async {
                match &mut lua_run {
                    Some((run, _)) => run.await,
                    None => std::future::pending().await,
                }
            } => {
                // DROP INMEDIATO del CommandRun resuelto (contrato del
                // driver): retenerlo mantendría `run_active` encendido y la
                // statusbar Lua congelada.
                lua_run = None;
                match outcome {
                    RunOutcome::Ok { messages } => {
                        if !messages.is_empty() {
                            // Unidos con « · » y por detail_for_bar (tope +
                            // enmascarado): la barra es una línea.
                            app.message = Some(detail_for_bar(&messages.join(" · ")));
                        }
                    }
                    RunOutcome::Err { detail, .. } => {
                        app.message = Some(ta(
                            "err-lua-command",
                            &[("detail", &detail_for_bar(&detail))],
                        ));
                    }
                    RunOutcome::Cancelled => app.message = Some(t("err-lua-cancelled")),
                    RunOutcome::TimedOut => app.message = Some(t("err-lua-timeout")),
                }
                // FIFO: arranca el siguiente encolado. En bucle: si uno ya
                // no existe (hot-reload lo quitó → `err-lua-unknown`), el
                // resto de la cola no se queda atascado.
                if let Some(host) = lua_host.as_ref() {
                    while lua_run.is_none() {
                        let Some(next) = lua_queue.pop_front() else {
                            break;
                        };
                        lua_run = start_lua_run(app, host, backend, &next);
                    }
                }
            }
            Some(()) = cfg_rx.recv() => {
                // Ráfaga de guardados: empuja el deadline (ADR 0007).
                reload_at =
                    Some(tokio::time::Instant::now() + std::time::Duration::from_millis(300));
            }
            () = async {
                match reload_at {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending().await,
                }
            } => {
                reload_at = None;
                while cfg_rx.try_recv().is_ok() {}
                // #117: si el reload cambia los attrs configurados de un
                // pane visible, hay que re-listar (los valores solo llegan
                // pidiéndolos) — mismo camino que el confirm del picker.
                let attrs_before = pane_attr_ids(app);
                reload_config(
                    app,
                    backend,
                    resolver,
                    viewer_resolver,
                    dialog_resolver,
                    help_lines,
                    lang,
                    &layers,
                    cli_preset.as_deref(),
                    &mut quick_mode,
                    &mut confirm_quit,
                    &mut cfg,
                )
                .await;
                // `[ui] mouse` en caliente: encenderla o apagarla sin
                // reiniciar. `set` es idempotente, así que un reload que
                // no tocó la clave (o que falló entero, dejando la config
                // vigente) no manda nada a la terminal.
                //
                // Exención puntual de la regla 2, la MISMA que el draw de
                // arriba: son unos pocos bytes de escape a stdout síncrono,
                // acotados, y solo cuando la clave CAMBIA.
                if let Err(e) =
                    capture.set(cfg.common.ui_mouse.unwrap_or(true), &mut std::io::stdout())
                {
                    // Y se dice, como en el arranque: quien acaba de
                    // encender el ratón desde el overlay de ajustes y se
                    // encuentra con que hacer click no hace nada merece
                    // saber por qué (antes esto solo iba al log).
                    tracing::warn!(error = %e, "no se pudo cambiar la captura de ratón");
                    app.message = Some(t("msg-mouse-capture-failed"));
                }
                if pane_attr_ids(app) != attrs_before {
                    let refreshed = refresh_panes(app, backend, &mut events).await;
                    after_panes_refresh(
                        app,
                        refreshed,
                        &mut fill,
                        &mut last_probed,
                        &mut search_run,
                    );
                }
                // Hot-reload del scripting Lua (ADR 0026): host NUEVO entero
                // (jamás estado a medias). Un `CommandRun` en vuelo retiene
                // el estado VIEJO vía sus handles clonados (documentado en
                // `lua::api`) y no se toca; statusbar/estado renacen. La
                // cola también: sus nombres apuntaban al registro viejo
                // (y si `load_lua` dio None, no quedaría quién drenarla).
                lua_host = load_lua(app, &layers).await;
                lua_queue.clear();
            }
            maybe = events.next() => {
                let Some(event) = maybe else { return Ok(()); };
                let event = event.context("evento de terminal")?;
                // Ratón (`[ui] mouse`): solo llega si la captura está
                // pedida — sin ella el emulador no reporta nada y este
                // brazo no corre. La semántica del gesto (marcar, barrer,
                // transferir) vive en `norte-frontend` (regla 7); aquí solo
                // se resuelve la celda y se aplica.
                if let Event::Mouse(me) = event {
                    match mouse::handle(app, me) {
                        mouse::After::Nothing => {}
                        // Doble click = `nav.enter`, por el MISMO `dispatch`
                        // que la tecla: mismo cd, mismo relleno paginado,
                        // misma cosecha de la búsqueda viva. Un segundo
                        // camino para entrar en un directorio sería un
                        // segundo sitio donde arreglar cada bug de cd.
                        mouse::After::Enter => {
                            let outcome = dispatch(
                                app,
                                backend,
                                &mut events,
                                help_lines,
                                lang,
                                quick_mode,
                                confirm_quit,
                                &cfg,
                                Command::NavEnter,
                            )
                            .await;
                            if let Some(pane) = cd_landed_pane(&outcome) {
                                app.apply_scheme_sort(pane);
                                let dir = app.panes[pane].dir().clone();
                                let paths: Vec<VPath> = app.panes[pane]
                                    .entries()
                                    .iter()
                                    .map(|e| e.path.clone())
                                    .collect();
                                let plugin_cols = app.columns.plugin_ids_for(dir.scheme());
                                decorate_fetch[pane] =
                                    spawn_decorate_fetch(backend, pane, dir, paths, plugin_cols);
                            }
                            apply_cd(
                                &mut fill,
                                &mut decorate_fetch,
                                &mut last_probed,
                                &mut search_run,
                                outcome,
                            );
                            // Paridad con el sitio del resolver: entrar en
                            // un hit apaga el modo virtual del pane, y hay
                            // que cosechar el run (regla 3).
                            reap_search_run(app, &mut search_run);
                        }
                    }
                } else if let Event::Key(key) = event
                    && key.kind == crossterm::event::KeyEventKind::Press
                {
                    app.message = None;
                    if app.theme_picker.is_some() && !modal_wins(app) {
                        on_theme_picker_key(app, dialog_resolver, key.modifiers, key.code).await;
                    } else if app.columns_picker.is_some() && !modal_wins(app) {
                        // Picker de columnas (#108 7a): mismo puesto en la
                        // cadena que el selector de tema (overlay antes que
                        // el brazo del modal, precedencia existente).
                        if on_columns_key(app, dialog_resolver, key.modifiers, key.code).await {
                            // #117: el set de attrs pintado cambió — los
                            // valores solo llegan pidiéndolos, así que se
                            // re-lista por el MISMO camino que tras una
                            // mutación (ritual en `after_panes_refresh`).
                            let refreshed = refresh_panes(app, backend, &mut events).await;
                            after_panes_refresh(
                                app,
                                refreshed,
                                &mut fill,
                                &mut last_probed,
                                &mut search_run,
                            );
                        }
                    } else if app.extensions.is_some() && !modal_wins(app) {
                        on_extensions_key(
                            app,
                            backend,
                            dialog_resolver,
                            lang,
                            help_lines,
                            key.modifiers,
                            key.code,
                        )
                        .await;
                    } else if app.nav_popup.is_some() && !modal_wins(app) {
                        // Popup historial/hotlist (spec 2026-07-18): Enter
                        // sobre un item NAVEGA por el flujo de cd normal —
                        // su desenlace toca el relleno como cualquier cd.
                        let outcome = on_nav_popup_key(
                            app,
                            backend,
                            &mut events,
                            dialog_resolver,
                            key.modifiers,
                            key.code,
                        )
                        .await;
                        if let Some(pane) = cd_landed_pane(&outcome) {
                            app.apply_scheme_sort(pane);
                            let dir = app.panes[pane].dir().clone();
                            let paths: Vec<VPath> =
                                app.panes[pane].entries().iter().map(|e| e.path.clone()).collect();
                            let plugin_cols = app.columns.plugin_ids_for(dir.scheme());
                            decorate_fetch[pane] =
                                spawn_decorate_fetch(backend, pane, dir, paths, plugin_cols);
                        }
                        apply_cd(
                            &mut fill,
                            &mut decorate_fetch,
                            &mut last_probed,
                            &mut search_run,
                            outcome,
                        );
                    } else if app.search_dialog.is_some() && !modal_wins(app) {
                        // Diálogo Alt+F7 (liveSearch T6): captura imprimibles
                        // como los demás overlays; Enter con criterio lanza la
                        // búsqueda (abre el pane virtual) — el resto de teclas
                        // no navegan.
                        if let Some(params) =
                            on_search_dialog_key(app, key.modifiers, key.code)
                        {
                            launch_search(app, backend, &mut fill, &mut search_run, params)
                                .await;
                        }
                    } else if app.palette.is_some() && !modal_wins(app) {
                        // Command palette (H1 T4): editor de filtro libre,
                        // como el diálogo de búsqueda de arriba — sus
                        // teclas son FIJAS, no resuelven por el contexto
                        // `dialog` (decisión 8 del plan H1: no hay
                        // vocabulario `dialog.*` para "teclear un carácter"
                        // o "correr la selección"). ctrl+c conserva su
                        // significado global (salir), como TODOS los
                        // overlays.
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && key.code == KeyCode::Char('c')
                        {
                            app.quit = true;
                            continue;
                        }
                        let plain = key.modifiers.is_empty()
                            || key.modifiers == KeyModifiers::SHIFT;
                        match key.code {
                            KeyCode::Char(c) if plain => {
                                if let Some(p) = &mut app.palette {
                                    p.push_char(c);
                                }
                            }
                            KeyCode::Backspace if plain => {
                                if let Some(p) = &mut app.palette {
                                    p.backspace();
                                }
                            }
                            KeyCode::Esc if plain => app.palette = None,
                            // H3c: el puente hacia la página que documenta la
                            // fila resaltada. Va AQUÍ, explícito junto a
                            // `ctrl+c`/`ctrl+p`, porque las teclas de la
                            // palette son FIJAS (decisión 8, arriba): no hay
                            // verbo `dialog.*` para «explícame esta fila», así
                            // que tampoco puede resolverse por el keymap.
                            KeyCode::F(1) if plain => {
                                palette_help(app, lang, help_lines);
                            }
                            KeyCode::Up if plain => {
                                if let Some(p) = &mut app.palette {
                                    p.up();
                                }
                            }
                            KeyCode::Down if plain => {
                                if let Some(p) = &mut app.palette {
                                    p.down();
                                }
                            }
                            KeyCode::PageUp if plain => {
                                if let Some(p) = &mut app.palette {
                                    p.page_up(PAGE);
                                }
                            }
                            KeyCode::PageDown if plain => {
                                if let Some(p) = &mut app.palette {
                                    p.page_down(PAGE);
                                }
                            }
                            KeyCode::Enter if plain => {
                                let cmd = app.palette.as_ref().and_then(Palette::selected);
                                app.palette = None;
                                if let Some(cmd) = cmd {
                                    // (P1) Enter sobre una fila de PLUGIN: la
                                    // `key` es `plugin:{id}:{command}`
                                    // (`palette::plugin_rows`, jamás pintada)
                                    // — no vive en `COMMANDS`, así que se
                                    // enruta AQUÍ, antes del vocabulario
                                    // tipado (#112). El resultado del plugin
                                    // es texto NO confiable: `detail_for_bar`
                                    // (enmascarado + tope, patrón #73).
                                    if let Some((id, command)) = parse_plugin_key(&cmd) {
                                        let (id, command) =
                                            (id.to_owned(), command.to_owned());
                                        run_plugin_command(app, backend, &id, &command)
                                            .await;
                                        continue;
                                    }
                                    // MISMA función de despacho que el
                                    // resolver del keymap invoca (#dispatch):
                                    // un comando elegido en la palette corre
                                    // EXACTAMENTE como si su tecla se
                                    // hubiera pulsado — incluida la apertura
                                    // de otro overlay (p.ej. `app.help`).
                                    // Las filas de la palette nacen de
                                    // `COMMANDS`, así que el parse no puede
                                    // fallar; el guard es defensivo (#112).
                                    let Some(cmd) = Command::parse(&cmd) else {
                                        debug_assert!(false, "palette fuera de COMMANDS");
                                        continue;
                                    };
                                    let outcome = dispatch(
                                        app,
                                        backend,
                                        &mut events,
                                        help_lines,
                                        lang,
                                        quick_mode,
                                        confirm_quit,
                                        &cfg,
                                        cmd,
                                    )
                                    .await;
                                    if let Some(pane) = cd_landed_pane(&outcome) {
                            app.apply_scheme_sort(pane);
                            let dir = app.panes[pane].dir().clone();
                            let paths: Vec<VPath> =
                                app.panes[pane].entries().iter().map(|e| e.path.clone()).collect();
                            let plugin_cols = app.columns.plugin_ids_for(dir.scheme());
                            decorate_fetch[pane] =
                                spawn_decorate_fetch(backend, pane, dir, paths, plugin_cols);
                        }
                        apply_cd(
                            &mut fill,
                            &mut decorate_fetch,
                            &mut last_probed,
                            &mut search_run,
                            outcome,
                        );
                                    // Paridad con el sitio del resolver (#118
                                    // review): un cd elegido en la palette
                                    // (nav.parent…) también puede apagar el
                                    // modo virtual — cosecha del run (regla 3);
                                    // y un `pane.open` de la palette deja su
                                    // comando externo resuelto — lanzarlo YA,
                                    // no en la siguiente tecla.
                                    reap_search_run(app, &mut search_run);
                                    if let Some(pending) = app.pending_open.take() {
                                        app.message = Some(launch_opener(terminal, capture, pending).await);
                                    }
                                }
                            }
                            _ => {}
                        }
                    } else if app.settings.is_some() && !modal_wins(app) {
                        // Overlay de ajustes (S3): mismo criterio que la
                        // palette de arriba (decisión 8 del plan H1) — sus
                        // teclas son fijas, hardcodeadas en `on_settings_key`.
                        on_settings_key(app, key.modifiers, key.code).await;
                    } else if help_owns_keys(app) {
                        // H3b: overlay de ayuda. La ruta de teclas vive en
                        // `on_help_key` (testeable, como `on_columns_key`);
                        // aquí solo queda lo que necesita el run loop, que es
                        // DESPACHAR la fila activada. El overlay ya se cerró:
                        // el comando actúa sobre los panes de debajo y la
                        // ayuda taparía la confirmación que abra.
                        match on_help_key(app, dialog_resolver, key.modifiers, key.code) {
                            // (H3e) Una fila de PLUGIN sale por el MISMO
                            // despacho que el Enter de la palette, no por un
                            // camino paralelo: `plugin.run_command` es de
                            // donde sale la autorización y la ayuda no la
                            // rodea. No toca los panes, así que no arrastra la
                            // contabilidad de cd del brazo de abajo.
                            Some(HelpDispatch::Plugin(id, command)) => {
                                run_plugin_command(app, backend, &id, &command).await;
                            }
                            None => {}
                            Some(HelpDispatch::Command(cmd)) => {
                            // MISMO despacho y MISMA contabilidad posterior que
                            // el Enter de la palette: una fila de la ayuda es
                            // `nav.parent` tanto como lo es una de la palette,
                            // así que el camino del cd (relleno paginado,
                            // decoración, cosecha de la búsqueda viva, opener
                            // externo pendiente) tiene que ser el mismo.
                            let outcome = dispatch(
                                app,
                                backend,
                                &mut events,
                                help_lines,
                                lang,
                                quick_mode,
                                confirm_quit,
                                &cfg,
                                cmd,
                            )
                            .await;
                            if let Some(pane) = cd_landed_pane(&outcome) {
                                app.apply_scheme_sort(pane);
                                let dir = app.panes[pane].dir().clone();
                                let paths: Vec<VPath> = app.panes[pane]
                                    .entries()
                                    .iter()
                                    .map(|e| e.path.clone())
                                    .collect();
                                let plugin_cols = app.columns.plugin_ids_for(dir.scheme());
                                decorate_fetch[pane] =
                                    spawn_decorate_fetch(backend, pane, dir, paths, plugin_cols);
                            }
                            apply_cd(
                                &mut fill,
                                &mut decorate_fetch,
                                &mut last_probed,
                                &mut search_run,
                                outcome,
                            );
                            reap_search_run(app, &mut search_run);
                            if let Some(pending) = app.pending_open.take() {
                                app.message =
                                    Some(launch_opener(terminal, capture, pending).await);
                            }
                            }
                        }
                    } else if app.modal.is_some() {
                        // MINOR-4 (H1 close): un modal llegado mientras la
                        // palette estaba abierta la cierra AQUÍ — obsoleta,
                        // y esta MISMA tecla responde al modal en vez de
                        // desaparecer dentro del filtro de la palette. El
                        // overlay de ajustes (S3) es el MISMO caso: un modal
                        // asíncrono (p.ej. una aprobación de policy) gana.
                        // El resto de overlays (selector de tema, picker de
                        // columnas, extensiones, popup de navegación,
                        // diálogo de búsqueda) también ceden la tecla
                        // (`modal_wins`) pero NO se cierran: sus filas no
                        // caducan como las de la palette/ajustes, y el
                        // usuario los recupera intactos al responder. La
                        // ayuda es el caso mixto (H3c) y lo decide
                        // `close_stale_overlays`.
                        close_stale_overlays(app);
                        // El TOFU de Lua se resuelve AQUÍ (necesita el host,
                        // que vive en este loop): no navega ni toca `fill`.
                        if matches!(app.modal, Some(Modal::TrustLuaInit { .. })) {
                            resolve_lua_trust(app, lua_host.as_ref(), key.code).await;
                            continue;
                        }
                        // ctrl+c conserva su significado global (salir),
                        // como los demás overlays (H1 T2) — ANTES de
                        // resolver contra el contexto `dialog`, hardcodeado.
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && key.code == KeyCode::Char('c')
                        {
                            app.quit = true;
                            continue;
                        }
                        // `Modal::MarkPattern` (#103 T9) es TEXTO libre, como
                        // el diálogo de búsqueda de arriba: consume
                        // caracteres crudos ANTES del contexto `dialog` — no
                        // tiene ALLOWLIST de `dialog_action` (`ctrl+c` ya
                        // quedó resuelto arriba, igual que para el resto de
                        // modales).
                        if matches!(app.modal, Some(Modal::MarkPattern { .. })) {
                            let plain = key.modifiers.is_empty()
                                || key.modifiers == KeyModifiers::SHIFT;
                            match key.code {
                                KeyCode::Char(c) if plain => app.mark_pattern_push(c),
                                KeyCode::Backspace if plain => app.mark_pattern_pop(),
                                // Un `Err` deja el diagnóstico en el propio
                                // modal (`mark_pattern_confirm`, que lo deja
                                // abierto): nada más que hacer aquí.
                                KeyCode::Enter if plain => {
                                    if let Ok(n) = app.mark_pattern_confirm() {
                                        app.message = Some(ta(
                                            "msg-marked-by-pattern",
                                            &[("n", &n.to_string())],
                                        ));
                                    }
                                }
                                KeyCode::Esc if plain => app.cancel_mark_pattern(),
                                _ => {}
                            }
                            continue;
                        }
                        // `Modal::Mkdir` (#104): mismo molde de texto libre.
                        // El submit vive AQUÍ (async): el modal valida y
                        // devuelve el destino; la task se registra en el
                        // board como cualquier otra mutación.
                        if matches!(app.modal, Some(Modal::Mkdir { .. })) {
                            let plain = key.modifiers.is_empty()
                                || key.modifiers == KeyModifiers::SHIFT;
                            match key.code {
                                KeyCode::Char(c) if plain => app.mkdir_push(c),
                                KeyCode::Backspace if plain => app.mkdir_pop(),
                                KeyCode::Enter if plain => {
                                    if let Some(target) = app.mkdir_confirm() {
                                        match backend.mkdir(&target).await {
                                            Ok(task) => {
                                                app.board.push(task, None);
                                                app.mkdir_submitted();
                                            }
                                            // MINOR-1: el nombre sobrevive
                                            // al fallo del submit.
                                            Err(e) => app
                                                .mkdir_set_error(error_message(&e)),
                                        }
                                    }
                                }
                                KeyCode::Esc if plain => app.cancel_mkdir(),
                                _ => {}
                            }
                            continue;
                        }
                        // `Modal::AiRenameInstruction` (M4-IA): mismo molde de
                        // texto libre que Mkdir. Enter SPAWNEA la petición al
                        // modelo (la única llamada larga del loop) y cierra el
                        // prompt; la cosecha vive en el select.
                        if matches!(app.modal, Some(Modal::AiRenameInstruction { .. })) {
                            let plain = key.modifiers.is_empty()
                                || key.modifiers == KeyModifiers::SHIFT;
                            match key.code {
                                KeyCode::Char(c) if plain => app.ai_rename_push(c),
                                KeyCode::Backspace if plain => app.ai_rename_pop(),
                                KeyCode::Enter if plain => {
                                    if let Some(instruction) = app.ai_rename_confirm() {
                                        let dir = app.focused().dir().clone();
                                        let b = backend.clone();
                                        let d = dir.clone();
                                        let handle = tokio::spawn(async move {
                                            b.ai_rename_plan(&d, &instruction).await
                                        });
                                        // Relanzar con un run vivo lo ABORTA
                                        // (dropear el handle solo desvincula):
                                        // a lo sumo una petición en vuelo.
                                        if let Some(old) =
                                            ai_rename_run.replace(AiRenameRun { handle, dir })
                                        {
                                            old.handle.abort();
                                        }
                                        // Invariante: lanzar VACÍA el stash —
                                        // un plan retenido de una petición
                                        // ANTERIOR jamás debe abrirse como si
                                        // fuera de esta.
                                        pending_ai_plan = None;
                                        app.message = Some(t("msg-ai-rename-running"));
                                        app.ai_rename_submitted();
                                    }
                                }
                                KeyCode::Esc if plain => app.cancel_ai_rename(),
                                _ => {}
                            }
                            continue;
                        }
                        // `Modal::SemanticQuery` (M4-IA-2): mismo molde de
                        // texto libre. Enter SPAWNEA la consulta al índice
                        // (root = None: todos los roots) y cierra el prompt;
                        // la cosecha vive en el select.
                        if matches!(app.modal, Some(Modal::SemanticQuery { .. })) {
                            let plain = key.modifiers.is_empty()
                                || key.modifiers == KeyModifiers::SHIFT;
                            match key.code {
                                KeyCode::Char(c) if plain => app.semantic_push(c),
                                KeyCode::Backspace if plain => app.semantic_pop(),
                                KeyCode::Enter if plain => {
                                    if let Some(query) = app.semantic_confirm() {
                                        let b = backend.clone();
                                        let handle = tokio::spawn(async move {
                                            b.index_search_semantic(None, &query, SEMANTIC_K)
                                                .await
                                        });
                                        // Relanzar con un run vivo lo ABORTA
                                        // (dropear el handle solo desvincula):
                                        // a lo sumo una consulta en vuelo.
                                        if let Some(old) =
                                            semantic_run.replace(SemanticRun { handle })
                                        {
                                            old.handle.abort();
                                        }
                                        // Invariante: lanzar VACÍA el stash —
                                        // unos hits retenidos de una consulta
                                        // ANTERIOR jamás deben abrirse como si
                                        // fueran de esta.
                                        pending_semantic = None;
                                        app.message = Some(t("msg-semantic-running"));
                                        app.semantic_submitted();
                                    }
                                }
                                KeyCode::Esc if plain => app.cancel_semantic(),
                                _ => {}
                            }
                            continue;
                        }
                        // `Modal::TransferName` (#105): mismo molde. El
                        // submit reusa `submit_transfer` — colisiones por el
                        // camino existente (`Modal::Collision` + backlog).
                        if matches!(app.modal, Some(Modal::TransferName { .. })) {
                            let plain = key.modifiers.is_empty()
                                || key.modifiers == KeyModifiers::SHIFT;
                            match key.code {
                                KeyCode::Char(c) if plain => app.transfer_name_push(c),
                                KeyCode::Backspace if plain => app.transfer_name_pop(),
                                KeyCode::Enter if plain => {
                                    if let Some((kind, from, dest)) =
                                        app.transfer_name_confirm()
                                    {
                                        // Cierra SOLO si encoló (disciplina
                                        // MINOR-1 de #104): un submit
                                        // fallido conserva el nombre; el
                                        // detalle queda en la barra.
                                        if submit_transfer(
                                            app,
                                            backend,
                                            kind,
                                            from,
                                            dest,
                                            TransferOptions::default(),
                                        )
                                        .await
                                        {
                                            app.transfer_name_submitted();
                                        } else {
                                            app.transfer_name_set_error(t(
                                                "msg-transfer-name-failed",
                                            ));
                                        }
                                    }
                                }
                                KeyCode::Esc if plain => app.cancel_transfer_name(),
                                _ => {}
                            }
                            continue;
                        }
                        // El modal TOFU (#45) puede NAVEGAR al confiar: su Cd
                        // se aplica igual que el de un comando.
                        let outcome = on_dialog_key(
                            app,
                            backend,
                            &mut events,
                            dialog_resolver,
                            key.modifiers,
                            key.code,
                            lang,
                            help_lines,
                        )
                        .await;
                        if let Some(pane) = cd_landed_pane(&outcome) {
                            app.apply_scheme_sort(pane);
                            let dir = app.panes[pane].dir().clone();
                            let paths: Vec<VPath> =
                                app.panes[pane].entries().iter().map(|e| e.path.clone()).collect();
                            let plugin_cols = app.columns.plugin_ids_for(dir.scheme());
                            decorate_fetch[pane] =
                                spawn_decorate_fetch(backend, pane, dir, paths, plugin_cols);
                        }
                        apply_cd(
                            &mut fill,
                            &mut decorate_fetch,
                            &mut last_probed,
                            &mut search_run,
                            outcome,
                        );
                    } else {
                        // Esc con un comando Lua en vuelo (BROWSE: sin modal
                        // ni overlay, y NO en el viewer): pide cancelación
                        // (regla 3) y CONSUME la tecla — no cae al resolver.
                        if app.viewer.is_none()
                            && key.modifiers.is_empty()
                            && key.code == KeyCode::Esc
                            && let Some((_, token)) = &lua_run
                        {
                            token.cancel();
                            continue;
                        }
                        // Esc con ai.rename_plan en vuelo (BROWSE, M4-IA):
                        // cancelar (regla 3) y CONSUMIR la tecla. Abortar
                        // dropea el future del backend en el runtime →
                        // rpc.cancel (remoto) / drop del stream (embebido).
                        if app.viewer.is_none()
                            && key.modifiers.is_empty()
                            && key.code == KeyCode::Esc
                            && let Some(run) = ai_rename_run.take()
                        {
                            run.handle.abort();
                            app.message = None;
                            continue;
                        }
                        // Esc con una búsqueda semántica en vuelo (BROWSE,
                        // M4-IA-2): mismo contrato de cancelación (regla 3).
                        if app.viewer.is_none()
                            && key.modifiers.is_empty()
                            && key.code == KeyCode::Esc
                            && let Some(run) = semantic_run.take()
                        {
                            run.handle.abort();
                            app.message = None;
                            continue;
                        }
                        // Pane virtual de búsqueda (liveSearch T6): con un
                        // search_run en el pane con foco (y sin quick vivo),
                        // Esc y Enter tienen semántica propia ANTES del
                        // resolver. El RESTO de teclas (cursor, F5/F8/F3…) cae
                        // al resolver y opera sobre el hit bajo el cursor.
                        if app.viewer.is_none()
                            && key.modifiers.is_empty()
                            && app.focused().quick().is_none()
                            && app.focused().virtual_search
                            && search_run
                                .as_ref()
                                .is_some_and(|s| s.pane == app.focus())
                        {
                            match key.code {
                                KeyCode::Esc => {
                                    // Task viva → cancela (hits conservados,
                                    // pasará a Cancelled al cerrarse el canal).
                                    // Ya terminada → sale del modo virtual
                                    // restaurando el dir anterior.
                                    on_search_escape(
                                        app,
                                        backend,
                                        &mut events,
                                        &mut fill,
                                        &mut decorate_fetch,
                                        &mut last_probed,
                                        &mut search_run,
                                    )
                                    .await;
                                    continue;
                                }
                                KeyCode::Enter => {
                                    // Enter sobre un hit: cd al PADRE del hit y
                                    // cursor sobre él (sale del modo virtual).
                                    on_search_enter(
                                        app,
                                        backend,
                                        &mut events,
                                        &mut fill,
                                        &mut decorate_fetch,
                                        &mut last_probed,
                                        &mut search_run,
                                    )
                                    .await;
                                    continue;
                                }
                                _ => {}
                            }
                        }
                        // Quick search ACTIVO en el pane con foco (BROWSE):
                        // sus teclas se comen ANTES del resolver — un char
                        // (incluida otra `/`) alimenta la query y jamás
                        // re-entra al keymap (sin recursión). El RESTO de
                        // teclas (F5, F8, F3, Tab en Filter…) NO se consume:
                        // cae al resolver y opera sobre `selected()` ya
                        // filtrado — feed-to-listbox gratis. Decisión
                        // consciente: las teclas de navegación NO
                        // interceptadas (PageUp/PageDown/Home/End) también
                        // caen al resolver y mueven el CURSOR REAL, que con
                        // el filtro activo es invisible; al cancelar (Esc)
                        // reaparece donde lo dejaron. Conectarlas a la
                        // selección del filtro no compensa el estado extra.
                        if app.viewer.is_none() && app.focused().quick().is_some() {
                            let jump = app
                                .focused()
                                .quick()
                                .is_some_and(|q| q.mode() == nav::Mode::Jump);
                            // SHIFT pasa (una mayúscula llega como
                            // Char('A')+SHIFT y el char ya viene tal cual);
                            // ctrl/alt caen al resolver (ctrl+c sigue
                            // saliendo).
                            let plain = key.modifiers.is_empty()
                                || key.modifiers == KeyModifiers::SHIFT;
                            match key.code {
                                KeyCode::Char(c) if plain => {
                                    app.focused_mut().quick_char(c);
                                    continue;
                                }
                                KeyCode::Backspace if plain => {
                                    app.focused_mut().quick_backspace();
                                    continue;
                                }
                                KeyCode::Up if plain => {
                                    app.focused_mut().quick_up();
                                    continue;
                                }
                                KeyCode::Down if plain => {
                                    app.focused_mut().quick_down();
                                    continue;
                                }
                                KeyCode::Tab if plain && jump => {
                                    app.focused_mut().quick_next();
                                    continue;
                                }
                                KeyCode::Esc if plain => {
                                    app.focused_mut().quick_cancel();
                                    continue;
                                }
                                KeyCode::Enter if plain => {
                                    // Confirma (cursor real = seleccionado) y
                                    // REUSA el camino de nav.enter: un dir (o
                                    // contenedor) entra, un fichero se queda.
                                    // `false` = el filtro no tenía matches:
                                    // solo cierra — jamás despachar sobre una
                                    // entrada que el usuario no veía (review
                                    // MAJOR T4).
                                    if app.focused_mut().quick_confirm() {
                                        let outcome = dispatch(
                                            app,
                                            backend,
                                            &mut events,
                                            help_lines,
                                            lang,
                                            quick_mode,
                                            confirm_quit,
                                            &cfg,
                                            Command::NavEnter,
                                        )
                                        .await;
                                        if let Some(pane) = cd_landed_pane(&outcome) {
                            app.apply_scheme_sort(pane);
                            let dir = app.panes[pane].dir().clone();
                            let paths: Vec<VPath> =
                                app.panes[pane].entries().iter().map(|e| e.path.clone()).collect();
                            let plugin_cols = app.columns.plugin_ids_for(dir.scheme());
                            decorate_fetch[pane] =
                                spawn_decorate_fetch(backend, pane, dir, paths, plugin_cols);
                        }
                        apply_cd(
                            &mut fill,
                            &mut decorate_fetch,
                            &mut last_probed,
                            &mut search_run,
                            outcome,
                        );
                                        // Paridad con el sitio del resolver
                                        // (#118 review): el Enter del quick
                                        // search ES un nav.enter — entrar en
                                        // un hit apaga el modo virtual del
                                        // pane; sin cosecha, la Task de
                                        // búsqueda quedaba viva (regla 3).
                                        reap_search_run(app, &mut search_run);
                                    }
                                    continue;
                                }
                                _ => {}
                            }
                        }
                        // Pantalla activa: el viewer tiene su contexto.
                        let active = if app.viewer.is_some() {
                            &mut *viewer_resolver
                        } else {
                            &mut *resolver
                        };
                        // Teclas que el keymap no modela (Media, BackTab,
                        // CapsLock…) no llegan al resolver como chord, pero
                        // el trato SÍ es el mismo que un `Resolution::Reset`:
                        // `active.reset()` rompe cualquier secuencia
                        // pendiente EN EL RESOLVER (no solo el `app.pending`
                        // de pantalla) — antes `from_event` siempre empujaba
                        // un chord (aunque exótico) y el `Miss` resultante
                        // limpiaba el pending interno; `chord_from_crossterm`
                        // devuelve `None` en su lugar, así que el reset hay
                        // que pedirlo explícito, jamás dejar la secuencia a
                        // medias viva.
                        if let Some(chord) = chord_from_crossterm(key.modifiers, key.code) {
                            match active.push(chord) {
                                Resolution::Run(cmd) => {
                                    app.pending.clear();
                                    // `lua:<nombre>` (M4): al despachador Lua —
                                    // jamás a `dispatch` (no es comando fijo).
                                    if let Some(name) = cmd.strip_prefix("lua:") {
                                        run_lua_command(
                                            app,
                                            lua_host.as_ref(),
                                            backend,
                                            name,
                                            &mut lua_run,
                                            &mut lua_queue,
                                        );
                                        continue;
                                    }
                                    // #112: el keymap se validó contra
                                    // COMMANDS al cargar — el parse no puede
                                    // fallar; guard defensivo.
                                    let Some(cmd) = Command::parse(&cmd) else {
                                        debug_assert!(false, "keymap fuera de COMMANDS");
                                        continue;
                                    };
                                    let outcome = dispatch(
                                        app,
                                        backend,
                                        &mut events,
                                        help_lines,
                                        lang,
                                        quick_mode,
                                        confirm_quit,
                                        &cfg,
                                        cmd,
                                    )
                                    .await;
                                    if let Some(pane) = cd_landed_pane(&outcome) {
                            app.apply_scheme_sort(pane);
                            let dir = app.panes[pane].dir().clone();
                            let paths: Vec<VPath> =
                                app.panes[pane].entries().iter().map(|e| e.path.clone()).collect();
                            let plugin_cols = app.columns.plugin_ids_for(dir.scheme());
                            decorate_fetch[pane] =
                                spawn_decorate_fetch(backend, pane, dir, paths, plugin_cols);
                        }
                        apply_cd(
                            &mut fill,
                            &mut decorate_fetch,
                            &mut last_probed,
                            &mut search_run,
                            outcome,
                        );
                                    // Un cd (nav.parent…) apagó el modo virtual del
                                    // pane de búsqueda: suelta el run y cancela.
                                    reap_search_run(app, &mut search_run);
                                    // #28: `pane.open` dejó un comando externo
                                    // resuelto — el run loop (dueño de la
                                    // terminal) sondea el binario y lo lanza.
                                    if let Some(pending) = app.pending_open.take() {
                                        app.message = Some(launch_opener(terminal, capture, pending).await);
                                    }
                                }
                                Resolution::Pending(_) => {
                                    app.pending = active
                                        .pending()
                                        .iter()
                                        .map(ToString::to_string)
                                        .collect::<Vec<_>>()
                                        .join(" ");
                                }
                                Resolution::Reset => app.pending.clear(),
                            }
                        } else {
                            active.reset();
                            app.pending.clear();
                        }
                    }
                }
            }
        }
        // Resize/Focus/etc: el draw del inicio del loop repinta solo.
    }
}

/// Hot-reload (ADR 0007): relee TODAS las capas; ante CUALQUIER error se
/// conserva la config vigente y se avisa por la barra — jamás romper una
/// sesión en marcha por un TOML a medio guardar.
/// Resuelve `[ui].theme` (preset o ruta) y lo aplica al `App`; ante error
/// degrada al preset por defecto y avisa (ADR 0020). El frontend no revienta
/// por un tema malo.
fn apply_theme(app: &mut App, cfg: &config::LoadedConfig) {
    let depth = norte_tui::theme::detect_depth();
    match norte_tui::theme::resolve(cfg.common.ui_theme.as_deref(), depth) {
        Ok(theme) => app.theme = theme,
        Err(e) => {
            app.theme = norte_tui::theme::TuiTheme::default();
            // Por categoría Fluent (#73): jamás el Display del OS ni el
            // diagnóstico crudo (el spec puede venir de un `./.norte` ajeno).
            app.message = Some(theme_error_category(&e));
        }
    }
}

/// Traduce las teclas del popup de tema a una acción de dominio (la lógica
/// vive en `App`, testeable) resolviendo contra el contexto `dialog` del
/// keymap (H1 T2, issue #24 — rebindeable). `ctrl+c` conserva su salida
/// global, hardcodeado ANTES de resolver, como los demás overlays. `F9`
/// cierra el picker como atajo ESPECÍFICO de este overlay (no es un binding
/// `dialog.*` del preset): se mantiene hardcodeado. Al confirmar, PERSISTE
/// la elección en el `norte.toml` del usuario (ADR 0020), sin bloquear el
/// runtime.
async fn on_theme_picker_key(
    app: &mut App,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    if mods.is_empty() && code == KeyCode::F(9) {
        app.theme_picker_input(PickerAction::Cancel);
        return;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return; // tecla no modelada por el keymap: ignorar
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run(cmd) => cmd,
        // Sin semántica de secuencia definida para overlays (T2): ignorar y
        // reiniciar el estado de resolución.
        Resolution::Pending(_) => {
            resolver.reset();
            return;
        }
        Resolution::Reset => return,
    };
    // H1 T3: el MISMO allowlist que consume el hint generado
    // (`hints::DialogHints::build`) — una sola fuente para dispatch y
    // footer. El match sigue siendo exhaustivo por defensa en profundidad.
    if !ALLOW_PICKER.contains(&cmd.as_str()) {
        return; // fuera del allowlist de este overlay: inerte
    }
    let action = match cmd.as_str() {
        "dialog.up" => PickerAction::Up,
        "dialog.down" => PickerAction::Down,
        "dialog.confirm" => PickerAction::Confirm,
        "dialog.cancel" => PickerAction::Cancel,
        _ => return, // ya filtrado por ALLOW_PICKER; inalcanzable en la práctica
    };
    // El nombre a persistir se toma ANTES de que Confirm cierre el popup.
    let confirmed = (action == PickerAction::Confirm)
        .then(|| {
            app.theme_picker
                .as_ref()
                .and_then(|p| p.selected().map(String::from))
        })
        .flatten();
    app.theme_picker_input(action);
    if let Some(name) = confirmed {
        // I/O en spawn_blocking: el runtime jamás se bloquea (regla 2).
        let n = name.clone();
        match tokio::task::spawn_blocking(move || config::persist_ui_theme(&n)).await {
            Ok(Ok(path)) => {
                // El path deriva de XDG_CONFIG_HOME/APPDATA (entorno):
                // saneado como cualquier detalle (#73).
                app.message = Some(ta(
                    "msg-theme-saved",
                    &[
                        ("name", &name),
                        ("path", &detail_for_bar(&path.display().to_string())),
                    ],
                ));
            }
            Ok(Err(e)) => {
                // El tema YA se aplicó (sesión); solo no se pudo guardar. A
                // la barra va la CATEGORÍA, jamás el Display del OS (#73).
                app.message = Some(ta(
                    "msg-theme-save-failed",
                    &[("error", &io_error_category(&e))],
                ));
            }
            // Un panic en el write es un bug nuestro: que no tumbe la TUI.
            Err(_) => {}
        }
    }
}

/// Teclas del picker de columnas (#108 7a): resuelve por keymap (pantalla
/// `dialog`) y filtra por [`ALLOW_COLUMNS`] — misma disciplina única-fuente
/// que el resto de overlays (#24). `ctrl+c` conserva su salida global,
/// hardcodeado ANTES de resolver, como los demás overlays. Devuelve `true`
/// si un confirm cambió el set de attrs pintado (#117): el run loop
/// re-lista entonces (mismo camino que tras una mutación).
async fn on_columns_key(
    app: &mut App,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) -> bool {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return false;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return false; // tecla no modelada por el keymap: ignorar
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run(cmd) => cmd,
        // Sin semántica de secuencia definida para overlays (T2): ignorar y
        // reiniciar el estado de resolución.
        Resolution::Pending(_) => {
            resolver.reset();
            return false;
        }
        Resolution::Reset => return false,
    };
    if !ALLOW_COLUMNS.contains(&cmd.as_str()) {
        return false; // fuera del allowlist de este overlay: inerte
    }
    let Some(p) = app.columns_picker.as_mut() else {
        return false;
    };
    match cmd.as_str() {
        "dialog.up" => p.up(),
        "dialog.down" => p.down(),
        "dialog.toggle-enabled" => p.toggle(),
        "dialog.move-up" => p.move_up(),
        "dialog.move-down" => p.move_down(),
        "dialog.sort" => p.sort_current(),
        "dialog.cycle-format" => p.cycle_format(),
        "dialog.cancel" => app.columns_picker = None,
        "dialog.confirm" => {
            let picked = p.finish();
            app.columns_picker = None;
            return apply_picked_columns(app, picked).await;
        }
        _ => {} // ya filtrado por ALLOW_COLUMNS; inalcanzable en la práctica
    }
    false
}

/// Routes one key inside the help overlay (H3b), and answers with the command
/// the run loop must DISPATCH — `Some` only for `Enter` on a runnable body
/// row, and only after this function has already closed the overlay.
///
/// Extracted from the run loop for the same reason as [`on_columns_key`]:
/// everything here is decidable from `App` plus the `dialog` resolver, and the
/// dispatch it hands back is the one thing that is not.
///
/// TWO REGIMES, the same split the palette and the search dialog already have:
///
/// What [`on_help_key`] hands the run loop to execute.
///
/// Two variants because a help row can name two different KINDS of thing, and
/// only the run loop can run either: this function is sync (its tests are, and
/// its callers are async), while both destinations need an `await`.
///
/// Keeping them apart in the type rather than collapsing to a string is the
/// point — `Command` is the closed, parsed vocabulary of the app (#112), and a
/// plugin key is deliberately NOT in it: its `command_id` half comes from a
/// third-party manifest with no validated charset, so it must never be handed
/// to a lookup as though it were one of ours.
#[derive(Debug, PartialEq, Eq)]
enum HelpDispatch {
    /// A built-in command, already parsed against `COMMANDS`.
    Command(Command),
    /// A plugin-contributed command: `(plugin_id, command_id)`, split at the
    /// FIRST colon after the prefix ([`parse_plugin_key`]).
    Plugin(String, String),
}

/// Runs a plugin's command and announces the result (P1, H3e).
///
/// The ONE place either surface dispatches one. It was written inline in the
/// palette's `Enter` arm and the help overlay grew a second need for it in
/// H3e; a copy would have been a second path with its own answer to what a
/// failure looks like, and H3b's rule is that executing from the help goes
/// through the SAME dispatch as the palette, with nothing bypassed.
///
/// Authorisation is the SERVER's: `plugin.run_command` resolves the command
/// against the catalogue and enforces approved+enabled itself
/// (`resolve_runnable`), independently of any snapshot a client froze. What a
/// client-side check buys is agreement with what the reader is looking at, and
/// it is never what permits the call.
///
/// The plugin's output is UNTRUSTED text: it goes through `detail_for_bar`
/// (masked and capped, pattern #73) before it reaches the status bar.
async fn run_plugin_command(app: &mut App, backend: &Backend, id: &str, command: &str) {
    app.message = Some(match backend.plugin_run_command(id, command, "").await {
        Ok(output) => ta("msg-plugin-run-ok", &[("output", &detail_for_bar(&output))]),
        Err(e) => error_message(&e),
    });
}

/// * While the sidebar filter is open the keys are FIXED. There is no
///   `dialog.*` verb for "type a character", so resolving through the keymap
///   here would make every printable key mean whatever it is bound to instead
///   of itself. `Esc` LEAVES the box keeping the text — the model's contract:
///   leaving a search is not undoing it, `Backspace` is what empties it.
/// * Otherwise the key resolves through the shared `dialog` resolver like
///   every other overlay's, and the resulting command is filtered through
///   [`norte_tui::app::help_action`]/`ALLOW_HELP` — the SAME list the footer
///   hint is generated from. A verb outside it is inert.
///
/// Two keys keep their global meaning ahead of both regimes (H1 T2, as in
/// every other overlay): `ctrl+c` quits, and `ctrl+p` hands what the reader
/// has typed to the command palette — the two are the same model at different
/// speeds (the `help` topic says as much), so the filter should not have to be
/// retyped to cross between them.
///
/// That second bridge is REFUSED while the page covers a modal
/// (`HelpView::over_modal`), the same guard the `Action::Run` arm makes: the
/// palette would open behind a live dialog, painted but unable to receive a
/// key, and every keystroke meant for its filter would be answering the dialog
/// instead.
fn on_help_key(
    app: &mut App,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) -> Option<HelpDispatch> {
    // Salida de emergencia global, hardcodeada ANTES de resolver — como en
    // todos los overlays (la de este fichero, jamás `Command::AppQuit`: no
    // pregunta).
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return None;
    }
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('p') {
        let help = app.help.as_ref()?;
        // Review H3c MAJOR-1: el MISMO guard que el brazo `Action::Run` de
        // abajo, y por una razón peor. Cruzar a la palette dejaría el modal en
        // pie, y la rama de la palette del run loop está gateada por
        // `!modal_wins`: la palette quedaría PINTADA con aspecto de viva y sin
        // recibir una sola tecla — todas caen en la rama del modal y se
        // resuelven contra su allowlist. Teclear `copy` para filtrar sobre una
        // aprobación de agente descartaría `c`, `o`, `p` y la `y` APROBARÍA la
        // mutación. La ayuda se queda abierta y se dice por qué.
        if help.over_modal {
            app.message = Some(t("msg-help-modal-waiting"));
            return None;
        }
        let filter = help.state.filter_raw().to_owned();
        app.help = None;
        // Sin filas de plugin: `Command::AppPalette` las pide al backend y
        // esta función es SÍNCRONA a propósito (todo lo demás aquí lo es).
        // Degradación conocida y acotada — los built-ins, que es lo que la
        // ayuda documenta, están todos.
        let mut palette = Palette::new(norte_tui::palette::rows_for_context(
            &app.palette_rows,
            app.viewer.is_some(),
        ));
        // El filtro CRUDO (`filter_raw`, no el enmascarado para pintar): es
        // lo que se empareja, y la palette lo vuelve a enmascarar al pintarlo.
        for c in filter.chars() {
            palette.push_char(c);
        }
        app.palette = Some(palette);
        return None;
    }
    // Régimen 1: editor de filtro. Teclas FIJAS (ver la doc de arriba).
    if app.help.as_ref()?.state.filtering() {
        // `plain` como en la palette: SHIFT es parte de teclear una mayúscula,
        // no un modificador que cambie el significado de la tecla.
        let plain = mods.is_empty() || mods == KeyModifiers::SHIFT;
        let help = app.help.as_mut()?;
        match code {
            KeyCode::Char(c) if plain => help.state.push_char(c),
            KeyCode::Backspace if plain => help.state.backspace(),
            // Ambas SALEN de la caja conservando el texto: Esc porque el
            // modelo lo promete, Enter porque el filtro ya está aplicado (la
            // lateral se rehace en cada carácter) y lo único que queda por
            // hacer es devolverle las flechas a la navegación.
            KeyCode::Esc | KeyCode::Enter if plain => help.state.end_filter(),
            // Sin salir de la caja: elegir un acierto mientras se sigue
            // afinando la búsqueda es el gesto que hace útil un filtro.
            KeyCode::Up if plain => help.state.up(),
            KeyCode::Down if plain => help.state.down(),
            _ => {}
        }
        return None;
    }

    // Régimen 2: el keymap manda (contexto `dialog`, rebindeable).
    let chord = chord_from_crossterm(mods, code)?;
    let cmd = match resolver.push(chord) {
        Resolution::Run(cmd) => cmd,
        // Sin semántica de secuencia definida para overlays (T2): ignorar y
        // reiniciar el estado de resolución.
        Resolution::Pending(_) => {
            resolver.reset();
            return None;
        }
        Resolution::Reset => return None,
    };
    // La tecla que ABRE la ayuda la CIERRA. `app.help` es un comando de
    // `[global]`, no un verbo de diálogo, así que no vive en `ALLOW_HELP` y
    // sin esta rama F1 sería inerte dentro de la ayuda — la única tecla del
    // teclado que el lector tiene garantizada para este overlay, sin efecto.
    // Se resuelve por el keymap igual que todo lo demás (un rebind de
    // `app.help` mueve las DOS mitades del interruptor a la vez); lo
    // hardcodeado es el significado, no la tecla. Mismo criterio que F9 en
    // `on_theme_picker_key`.
    if cmd == "app.help" {
        app.help = None;
        return None;
    }
    // Fuera de `ALLOW_HELP` la tecla es INERTE (misma disciplina que el resto
    // de overlays: la semántica vive en código, el keymap solo asigna teclas).
    let outcome = norte_tui::app::help_action(&cmd)?;

    let help = app.help.as_mut()?;
    // Leído ANTES del `match`: el brazo que lo consulta ya no tiene `help` a
    // mano (asigna `app.message`, que reclama el préstamo de vuelta).
    let over_modal = help.over_modal;
    match outcome {
        HelpOutcome::Up => help.state.up(),
        HelpOutcome::Down => help.state.down(),
        HelpOutcome::PageUp => help.state.page_up(PAGE),
        HelpOutcome::PageDown => help.state.page_down(PAGE),
        HelpOutcome::TogglePane => help.state.toggle_focus(),
        HelpOutcome::StartFilter => help.state.start_filter(),
        // Con historial, vuelve; SIN historial, cierra. Es lo que convierte
        // `Backspace` en una tecla honesta en vez de una muerta en la raíz:
        // "atrás" desde donde no se puede ir más atrás es salir.
        HelpOutcome::Back => {
            if !help.state.back() {
                app.help = None;
            }
        }
        HelpOutcome::Close => app.help = None,
        HelpOutcome::Activate => match help.state.action().cloned() {
            // Un enlace se sigue y la ayuda SIGUE abierta: leer no es salir.
            Some(norte_frontend::help::Action::Open(id)) => help.state.open(&id),
            Some(norte_frontend::help::Action::Run(cmd)) => {
                // H3c: una ayuda abierta ENCIMA de un modal no despacha nada
                // sobre los panes. `dispatch` planta sus propios modales (una
                // confirmación de copia), así que el comando SUSTITUIRÍA al
                // que está esperando respuesta: una aprobación de agente
                // desaparecería de la pantalla sin que nadie la haya
                // contestado. Se dice y la ayuda se queda abierta — mismo
                // trato que la fila no despachable de abajo.
                if over_modal {
                    app.message = Some(t("msg-help-modal-waiting"));
                    return None;
                }
                // La lista `commands` de un tema puede nombrar un verbo
                // `dialog.*` — el tema `help` documenta tres — y ésos son
                // vocabulario de overlay, no algo que un pane pueda correr:
                // no están en `COMMANDS` y `Command::parse` los rechaza. Se
                // dice y la ayuda se queda abierta; comerse el Enter en
                // silencio se leería como que el comando corrió. (El resto
                // de ids del corpus SÍ parsean: la puerta de documentación
                // los cruza byte a byte contra `COMMANDS ∪ DIALOG_COMMANDS`.)
                // (H3e) Una fila de PLUGIN. Su clave es `plugin:{id}:{cmd}`,
                // que no vive en `COMMANDS` y que `Command::parse` rechaza —
                // así que sin este brazo el Enter caía en el `msg-help-not-
                // runnable` de abajo y la app se negaba a correr justo la fila
                // que ella misma acababa de pintar como disponible, con el pie
                // prometiendo `⏎ ejecutar`. La atenuación era decorativa.
                if let Some((id, command)) = parse_plugin_key(&cmd) {
                    // La foto congelada DIMEA; jamás AUTORIZA. Negarse aquí es
                    // coherencia con lo que el lector tiene delante — una fila
                    // atenuada que al pulsarla corriera sería peor que no
                    // atenuar nada — pero la autoridad sigue siendo
                    // `resolve_runnable` en el servidor, que comprueba
                    // aprobado+activo por su cuenta y no se fía de ningún
                    // cliente. Dos comprobaciones que dicen lo mismo, una
                    // cortés y otra vinculante.
                    if !norte_help::ChordResolver::availability(&*app.help_chords, &cmd)
                        .is_available()
                    {
                        app.message = Some(t("msg-help-not-runnable"));
                        return None;
                    }
                    let (id, command) = (id.to_owned(), command.to_owned());
                    // Cerrar ANTES de despachar, como abajo.
                    app.help = None;
                    return Some(HelpDispatch::Plugin(id, command));
                }
                let Some(parsed) = Command::parse(&cmd) else {
                    // La barra de estado se ve: el overlay ocupa el frame
                    // menos una fila arriba y otra abajo, y la barra es esa
                    // última fila (`ui::help_layout`).
                    app.message = Some(t("msg-help-not-runnable"));
                    return None;
                };
                // Cerrar ANTES de despachar es deliberado: el comando actúa
                // sobre los panes de debajo y la ayuda taparía la
                // confirmación que abra.
                app.help = None;
                return Some(HelpDispatch::Command(parsed));
            }
            // Foco en la lateral. Arrear el cursor ya PREVISUALIZA (abre lo
            // que pisa), así que el tema resaltado suele ser YA el abierto y
            // `open` no haría nada: un Enter mudo, indistinguible de un fallo.
            // Cuando coinciden, Enter entra AL CUERPO; cuando no —el único
            // caso que queda, seguir un `see_also` desde una lista filtrada,
            // donde el resalte se quedó en la fila visible más cercana— abre.
            // En las dos ramas Enter significa lo mismo: «ir a lo que estoy
            // mirando».
            None => {
                let selected = help.state.selected_topic().cloned();
                if selected.is_some_and(|id| id != *help.state.current()) {
                    help.state.open_selected();
                } else {
                    help.state.toggle_focus();
                }
            }
        },
    }
    None
}

/// The help overlay's key routing, driven through [`on_help_key`] — the same
/// seam the run loop uses, so these exercise the WIRING (allowlist, the two
/// regimes, what closes the overlay, what the run loop is asked to dispatch)
/// and not the model underneath, which has its own tests in
/// `norte_frontend::help`.
#[cfg(test)]
mod help_key_tests {
    use super::*;
    use norte_frontend::help::{Focus, SidebarRow};
    use norte_help::{Lang, TopicId};

    /// An effective of the orthodox preset over the WHOLE vocabulary: the
    /// `dialog` screen merges `[global]` too, so `DIALOG_COMMANDS` alone
    /// would make `build_for` reject the preset outright.
    fn eff(screen: Screen) -> Effective {
        let (_, preset) = presets()
            .into_iter()
            .find(|(n, _)| *n == "orthodox")
            .expect("preset orthodox");
        let known: Vec<&str> = COMMANDS
            .iter()
            .copied()
            .chain(DIALOG_COMMANDS.iter().copied())
            .collect();
        Effective::build_for(&preset, &[], &known, screen).expect("efectivo del preset")
    }

    fn dialog_resolver() -> Resolver {
        Resolver::new(eff(Screen::Dialog))
    }

    fn app_with_help() -> App {
        let d = VPath::parse("file:///x").expect("wire de test");
        let mut app = App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()));
        app.help = Some(HelpView::new(Lang::En, Vec::new()));
        app
    }

    /// The same app WITHOUT the overlay: the H3c tests open it through the
    /// production seam instead of planting a `HelpView` by hand, because what
    /// they are checking is what that seam decides.
    fn app_with_help_closed() -> App {
        let mut app = app_with_help();
        app.help = None;
        app
    }

    /// `Command::AppHelp`'s whole body ([`open_contextual_help`]), which is
    /// what `F1` runs.
    fn abrir_ayuda(app: &mut App) {
        open_contextual_help(app, Lang::En, &[], None);
    }

    /// The page `context` opens today, or the documented fallback. The corpus
    /// half of the map is data being written page by page (H3h): a test that
    /// hard-coded `copying` here would fail the day a context is claimed and
    /// pass for the wrong reason until then.
    fn pagina_de(context: &str) -> String {
        norte_help::topic_for_context(Lang::En, context)
            .map_or_else(|| "index".to_owned(), |t| t.id.as_str().to_owned())
    }

    fn collision_modal_de_test() -> Modal {
        Modal::Collision {
            retry: norte_tui::tasks::RetrySpec {
                kind: norte_tui::app::TransferKind::Move,
                from: VPath::parse("file:///a").expect("wire de test"),
                to: VPath::parse("file:///b").expect("wire de test"),
                opts: norte_core::TransferOptions::default(),
                name_encoding: None,
            },
        }
    }

    /// Una aprobación de agente: el modal cuyo secuestro por un overlay es el
    /// defecto que `modal_wins` existe para cerrar (H1 MINOR-4).
    fn approval_modal_de_test() -> Modal {
        Modal::ApproveAgentOp {
            req: norte_proto::methods::PolicyApprovalRequired {
                approval_id: 1,
                session: Some("s1".into()),
                op: "copy".into(),
                paths: vec!["mem:///a".into()],
                ttl_ms: 60_000,
            },
        }
    }

    /// El TOFU de una host key: la superficie de SEGURIDAD que una ayuda sí
    /// puede tapar, porque `dialog.trust-host` tiene página (`remote`) y la
    /// aprobación de agente no — sobre ésa `F1` ya no abre nada (MAJOR-2), así
    /// que los tests de «los verbos de debajo son inertes» viven aquí. La
    /// consecuencia es de la misma clase: `dialog.approve` (la `y` del preset)
    /// CONFÍA en una clave sin verificar.
    fn trust_host_modal_de_test() -> Modal {
        Modal::TrustHostKey {
            host: "h".into(),
            port: Some(22),
            algo: "ssh-ed25519".into(),
            fingerprint: "SHA256:AAAA".into(),
            dir: VPath::parse("sftp://h/").expect("wire de test"),
            pane: 0,
            trail: norte_tui::app::Trail::Record,
        }
    }

    /// `F1` desde un pane abre la página del PANE, no el índice, y llega con
    /// el historial vacío: `Esc` cierra el overlay, no camina hacia atrás a un
    /// sitio que el lector no pidió.
    #[test]
    fn f1_abre_la_pagina_del_contexto_y_sin_historial() {
        let mut app = app_with_help_closed();
        abrir_ayuda(&mut app);
        let help = app.help.as_ref().expect("la ayuda se abrió");
        assert_eq!(
            help.state.current().as_str(),
            "panes",
            "el corpus reclama `browse`"
        );
        assert!(!help.over_modal, "no había modal ninguno");

        let mut r = dialog_resolver();
        press(&mut app, &mut r, KeyCode::Backspace);
        assert!(
            app.help.is_none(),
            "«atrás» en la raíz cierra: la página contextual NO es un paso de navegación"
        );
    }

    /// Review H3c MAJOR-2: sobre un modal SIN página escrita, `F1` no abre
    /// NADA — lo dice y deja la pregunta contestable.
    ///
    /// El fallback al índice vale desde un pane o desde el visor (nadie espera
    /// una decisión), pero sobre un diálogo tapaba una pregunta viva con
    /// «Bienvenido a norte — norte es un gestor de ficheros ortodoxo. Dos
    /// paneles…», congelaba sus verbos, le sustituía el pie y dejaba al lector
    /// caminar del índice a `copying` para leer la prosa de `y`/`n` de OTRO
    /// diálogo mientras la aprobación esperaba detrás. Es la misma decisión que
    /// [`palette_help`] ya había tomado para una fila sin documentar, aplicada
    /// donde importa más.
    #[test]
    fn f1_sobre_un_modal_sin_pagina_no_tapa_la_pregunta() {
        // Se prueba la GUARDA, no el hueco: desde H3h todo contexto tiene
        // página (la puerta de documentación se quedó sin allowlist), así que
        // un test que necesitara un modal indocumentado se quedaría sin sujeto
        // y habría que reescribirlo con cada página nueva. El contexto es
        // sintético; lo que se fija es que sobre un modal la respuesta a «no
        // hay página» es no abrir nada.
        for lang in [Lang::En, Lang::Es] {
            assert!(
                refuses_over_modal(lang, "dialog.no-such-context", true),
                "sobre un modal, sin página, F1 no abre nada"
            );
            assert!(
                !refuses_over_modal(lang, "dialog.no-such-context", false),
                "desde un pane el índice SÍ es un aterrizaje razonable"
            );
            assert!(
                !refuses_over_modal(lang, "dialog.approval", true),
                "y con página escrita se abre esa página"
            );
        }
    }

    /// La otra mitad, y lo que de verdad cambió en H3h: ningún modal que la
    /// TUI sepa abrir se queda sin página. Es lo mismo que cruza la puerta de
    /// documentación (`tests/help_gate.rs`), comprobado aquí desde el lado del
    /// lector — `F1` sobre una pregunta viva abre prosa sobre ESA pregunta, y
    /// nunca el mensaje de arriba.
    #[test]
    fn todo_contexto_de_modal_tiene_pagina() {
        for lang in [Lang::En, Lang::Es] {
            for context in norte_tui::help_context::CONTEXTS {
                assert!(
                    !refuses_over_modal(lang, context, true),
                    "[{lang:?}] el contexto `{context}` no tiene página que abrir"
                );
            }
        }
    }

    /// Y sobre un modal con página, `F1` la abre sin tapar la pregunta.
    #[test]
    fn f1_sobre_una_aprobacion_abre_su_pagina() {
        let mut app = app_with_help_closed();
        app.modal = Some(approval_modal_de_test());
        abrir_ayuda(&mut app);
        let help = app.help.as_ref().expect("la ayuda se abrió");
        assert_eq!(help.state.current().as_str(), "agents");
        assert!(help.over_modal, "la ayuda sabe que hay una pregunta detrás");
        assert!(app.modal.is_some(), "y la pregunta sigue ahí");
    }

    /// `F1` sobre un modal abre la página de ESE modal (o el índice mientras
    /// nadie la haya escrito) y deja el modal donde estaba.
    #[test]
    fn f1_sobre_un_modal_abre_la_pagina_del_modal() {
        let mut app = app_with_help_closed();
        app.modal = Some(collision_modal_de_test());
        abrir_ayuda(&mut app);
        let help = app.help.as_ref().expect("la ayuda se abrió");
        assert_eq!(
            help.state.current().as_str(),
            pagina_de("dialog.collision"),
            "la página del contexto del modal, jamás la de otro modal"
        );
        assert!(help.over_modal, "se abrió ENCIMA de un modal");
        assert!(app.modal.is_some(), "y el modal sigue ahí");
        assert!(
            help_owns_keys(&app),
            "…con las teclas: sin esto la rama del modal se las quedaría y el \
             lector no podría ni mover el cursor de la ayuda que acaba de abrir"
        );
    }

    /// La ayuda abierta desde un modal se queda las teclas, y `Esc` cierra
    /// SOLO la ayuda: el modal no se responde por accidente.
    #[test]
    fn esc_cierra_la_ayuda_y_deja_el_modal_intacto() {
        let mut app = app_with_help_closed();
        app.modal = Some(trust_host_modal_de_test());
        abrir_ayuda(&mut app);
        assert!(help_owns_keys(&app), "la rama de la ayuda es la que corre");
        let mut r = dialog_resolver();
        press(&mut app, &mut r, KeyCode::Esc);
        assert!(app.help.is_none(), "la ayuda se cerró");
        assert!(
            app.modal.is_some(),
            "una host key desconocida NO se contesta cerrando una ayuda"
        );
        assert!(
            !help_owns_keys(&app),
            "y cerrada, la tecla siguiente vuelve al modal"
        );
    }

    /// Mientras la ayuda tapa el modal, los verbos del modal son inertes: se
    /// decide con la ayuda cerrada, mirándolo.
    #[test]
    fn los_verbos_del_modal_no_se_alcanzan_por_debajo_de_la_ayuda() {
        let mut app = app_with_help_closed();
        app.modal = Some(trust_host_modal_de_test());
        abrir_ayuda(&mut app);
        // La rama que corre es la de la ayuda (`help_owns_keys`), así que la
        // del modal —la única que llama a `dialog_action`— no ve esta tecla.
        assert!(help_owns_keys(&app));
        let mut r = dialog_resolver();
        press(&mut app, &mut r, KeyCode::Char('y')); // dialog.approve
        assert!(app.modal.is_some(), "no se confió en nada a ciegas");
        assert!(app.help.is_some(), "y `y` tampoco cierra la ayuda");
    }

    /// Un modal que LLEGA sobre una ayuda abierta la cierra: la tecla
    /// siguiente tiene que ir donde apuntan los píxeles (el modal se pinta
    /// ÚLTIMO, por encima de todo), y una aprobación no se contesta a través
    /// de una página.
    #[test]
    fn un_modal_que_llega_cierra_la_ayuda() {
        let mut app = app_with_help_closed();
        abrir_ayuda(&mut app); // sin modal: over_modal == false
        assert!(!app.help.as_ref().expect("abierta").over_modal);
        app.modal = Some(approval_modal_de_test());
        assert!(
            !help_owns_keys(&app),
            "la ayuda que ya estaba abierta NO se queda la tecla del modal"
        );
        close_stale_overlays(&mut app);
        assert!(app.help.is_none(), "la ayuda cede la pantalla");
    }

    /// Review MINOR-1: `over_modal` es un hecho del PRESENTE, no un recuerdo.
    ///
    /// Si el modal sobre el que se abrió la ayuda desaparece y llega OTRO, la
    /// bandera vieja haría que la ayuda se quedara las teclas y
    /// `close_stale_overlays` no la retirara nunca: el modal nuevo sería
    /// incontestable hasta cerrar una página sobre un diálogo que ya no existe.
    /// `settle_help_over_modal` la limpia en cuanto no hay modal, así que el
    /// segundo modal se trata como lo que es — uno que LLEGA sobre una ayuda
    /// abierta.
    #[test]
    fn over_modal_no_sobrevive_al_modal_que_lo_justificaba() {
        let mut app = app_with_help_closed();
        app.modal = Some(collision_modal_de_test());
        abrir_ayuda(&mut app);
        assert!(app.help.as_ref().expect("abierta").over_modal);

        // El modal se contesta; la ayuda sigue abierta (Esc cerraría solo la
        // ayuda, pero el modal puede irse por su propio camino: un retry).
        app.modal = None;
        settle_help_over_modal(&mut app);
        assert!(
            !app.help.as_ref().expect("sigue abierta").over_modal,
            "la bandera no sobrevive a lo que era un recuerdo DE"
        );

        // …y ahora llega otro modal, que NO hereda las teclas de la ayuda.
        app.modal = Some(approval_modal_de_test());
        assert!(
            !help_owns_keys(&app),
            "el modal nuevo se queda la tecla: nadie pidió una página sobre ÉL"
        );
        close_stale_overlays(&mut app);
        assert!(app.help.is_none(), "y la ayuda caduca como el resto");
    }

    /// Y la ayuda que tapa un modal tampoco DESPACHA: `dispatch` planta sus
    /// propios modales, así que correr `pane.copy` desde la página sustituiría
    /// la pregunta que espera respuesta — desaparecería de la pantalla sin que
    /// nadie la haya contestado. Se dice y la página se queda.
    #[test]
    fn una_fila_ejecutable_no_se_despacha_por_encima_de_un_modal() {
        let mut app = app_with_help_closed();
        app.modal = Some(trust_host_modal_de_test());
        abrir_ayuda(&mut app);
        let mut r = dialog_resolver();
        // A una página con filas ejecutables (la del contexto puede no
        // tenerlas todavía) y al cuerpo, que es donde vive el Enter.
        app.help
            .as_mut()
            .expect("abierta")
            .state
            .open(&TopicId::new("copying"));
        press(&mut app, &mut r, KeyCode::Tab);
        assert_eq!(state(&app).focus(), Focus::Body);
        assert!(matches!(
            state(&app).action(),
            Some(norte_frontend::help::Action::Run(_))
        ));

        let cmd = press(&mut app, &mut r, KeyCode::Enter);
        assert_eq!(cmd, None, "nada que el run loop pueda despachar");
        assert!(app.help.is_some(), "y la ayuda no se cierra sola");
        assert!(app.modal.is_some(), "la pregunta sigue en pie");
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("msg-help-modal-waiting").as_str()),
            "el Enter no puede desaparecer en silencio"
        );
    }

    /// Y el PUENTE a la palette tampoco cruza por encima de un modal (review
    /// H3c MAJOR-1), por la misma razón que el brazo de arriba y con una
    /// consecuencia peor.
    ///
    /// `Ctrl+P` cerraba la ayuda y abría la palette dejando el modal en pie.
    /// La rama de la palette del run loop está gateada por `!modal_wins`, así
    /// que la palette quedaba PINTADA y con aspecto de viva pero sin recibir
    /// una sola tecla: todas caían en la rama del modal y se resolvían contra
    /// su allowlist. Teclear `copy` para filtrar sobre este TOFU descarta `c`,
    /// `o`, `p`… y la `y` CONFÍA en la host key.
    #[test]
    fn el_puente_a_la_palette_no_cruza_por_encima_de_un_modal() {
        let mut app = app_with_help_closed();
        app.modal = Some(trust_host_modal_de_test());
        abrir_ayuda(&mut app);
        assert!(
            app.help.as_ref().expect("abierta").over_modal,
            "precondición: la ayuda se abrió ENCIMA del modal"
        );
        let mut r = dialog_resolver();

        let cmd = on_help_key(&mut app, &mut r, KeyModifiers::CONTROL, KeyCode::Char('p'));
        assert_eq!(cmd, None, "nada que despachar");
        assert!(
            app.palette.is_none(),
            "la palette NO se abre: sus teclas se las quedaría el modal"
        );
        assert!(app.help.is_some(), "la ayuda se queda donde estaba");
        assert!(app.modal.is_some(), "y el modal sigue esperando respuesta");
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("msg-help-modal-waiting").as_str()),
            "el Ctrl+P no puede desaparecer en silencio"
        );
    }

    /// La tecla de la ayuda tiene que LLEGAR con un modal abierto. `app.help`
    /// es un comando de `[global]`, no un verbo `dialog.*`, así que el
    /// allowlist del modal (`dialog_action`) lo deja caer: sin la rama de
    /// `modal_help_toggle` en `on_dialog_key`, `F1` sobre un diálogo es INERTE
    /// y toda esta tarea no se puede usar. (Pillado pilotando la TUI en tmux:
    /// la suite en verde no lo veía porque abría la ayuda por `dispatch`.)
    #[test]
    fn f1_resuelve_y_abre_la_ayuda_con_un_modal_abierto() {
        let mut app = app_with_help_closed();
        app.modal = Some(collision_modal_de_test());
        let mut r = dialog_resolver();

        // El MISMO camino que el run loop: el chord de F1 resuelto contra el
        // efectivo `dialog` — la tecla es del keymap (rebindeable), el
        // significado es de aquí.
        let chord =
            chord_from_crossterm(KeyModifiers::NONE, KeyCode::F(1)).expect("F1 es un chord");
        let cmd = match r.push(chord) {
            Resolution::Run(cmd) => cmd,
            otro => panic!("F1 resuelve a un comando en el contexto dialog: {otro:?}"),
        };
        assert_eq!(cmd, "app.help", "el preset orthodox ata F1 a `app.help`");

        assert!(
            modal_help_toggle(&mut app, &cmd, Lang::En, &[]),
            "la tecla se CONSUME: el modal no la ve como una decisión"
        );
        let help = app.help.as_ref().expect("F1 abrió la ayuda sobre el modal");
        assert_eq!(help.state.current().as_str(), pagina_de("dialog.collision"));
        assert!(help.over_modal);
        assert!(app.modal.is_some(), "y el modal sigue en pie");

        // Y con la ayuda ya abierta la MISMA tecla la cierra (el interruptor
        // vive en `on_help_key`), así que este hook no puede reabrirla: la
        // rama de la ayuda gana la tecla antes de llegar aquí.
        assert!(help_owns_keys(&app));
    }

    /// Cualquier otro comando `dialog.*` no lo toca el hook: quien decide
    /// sigue siendo el allowlist del modal.
    #[test]
    fn el_hook_de_la_ayuda_no_se_come_los_verbos_del_modal() {
        let mut app = app_with_help_closed();
        app.modal = Some(approval_modal_de_test());
        assert!(!modal_help_toggle(
            &mut app,
            "dialog.approve",
            Lang::En,
            &[]
        ));
        assert!(app.help.is_none(), "ni abre nada");
    }

    /// Review H3c MINOR-3: los SEIS modales que el run loop intercepta antes de
    /// `on_dialog_key` no admiten ayuda por encima, y ahora eso es una DECISIÓN
    /// (`help_context::help_over_modal_allowed`) en vez de la resaca del
    /// enrutado de teclas.
    ///
    /// Antes quedaban fuera solo porque cada uno hace `continue` 3000 líneas
    /// más arriba; mover uno al keymap `dialog` —una limpieza plausible— habría
    /// abierto el agujero en silencio sobre un editor de texto libre y sobre el
    /// TOFU de `init.lua`, que NO tiene TTL.
    #[test]
    fn los_modales_interceptados_no_admiten_ayuda_por_encima() {
        let interceptados = [
            Modal::TrustLuaInit {
                path: "repo/.norte/init.lua".into(),
                hash_abbrev: "ab12cd34ef56ab78ab12cd34ef56ab78".into(),
            },
            Modal::MarkPattern {
                mark: true,
                pattern: "*.rs".into(),
                error: None,
            },
            Modal::Mkdir {
                name: "nuevo".into(),
                error: None,
            },
            Modal::AiRenameInstruction {
                instruction: "en snake_case".into(),
                error: None,
            },
            Modal::SemanticQuery {
                query: "facturas".into(),
                error: None,
            },
            Modal::TransferName {
                kind: norte_tui::app::TransferKind::Copy,
                from: VPath::parse("file:///x/a").expect("wire de test"),
                to_dir: VPath::parse("file:///y").expect("wire de test"),
                name: "a".into(),
                original: b"a".to_vec(),
                touched: false,
                from_marks: false,
                enc: None,
                error: None,
            },
        ];
        for modal in interceptados {
            let etiqueta = format!("{modal:?}");
            let mut app = app_with_help_closed();
            app.modal = Some(modal);
            assert!(
                !modal_help_toggle(&mut app, "app.help", Lang::En, &[]),
                "{etiqueta}: el hook no puede CONSUMIR la tecla de un modal que \
                 no admite ayuda — quien decide vuelve a ser el allowlist"
            );
            assert!(
                app.help.is_none(),
                "{etiqueta}: F1 no abre una página sobre un editor de texto \
                 libre ni sobre el TOFU de Lua"
            );
            assert!(app.modal.is_some(), "{etiqueta}: y el modal sigue ahí");
        }
    }

    /// …y la dirección contraria NO: una ayuda que el lector abrió DESDE el
    /// modal sobrevive a la limpieza, o `F1` sobre un diálogo abriría una
    /// página que la siguiente tecla se lleva.
    #[test]
    fn la_ayuda_abierta_desde_el_modal_sobrevive_a_la_limpieza() {
        let mut app = app_with_help_closed();
        app.modal = Some(trust_host_modal_de_test());
        abrir_ayuda(&mut app);
        app.palette = Some(Palette::new(Vec::new()));
        close_stale_overlays(&mut app);
        assert!(app.palette.is_none(), "la palette sí caduca");
        assert!(
            app.help.is_some(),
            "la ayuda que el lector pidió sobre ESTE modal se queda"
        );
    }

    /// One unmodified key press.
    fn press(app: &mut App, resolver: &mut Resolver, code: KeyCode) -> Option<HelpDispatch> {
        on_help_key(app, resolver, KeyModifiers::NONE, code)
    }

    fn state(app: &App) -> &norte_frontend::help::HelpState {
        &app.help.as_ref().expect("overlay abierto").state
    }

    fn topic_ids(app: &App) -> Vec<String> {
        state(app)
            .rows()
            .iter()
            .filter_map(|r| match r {
                SidebarRow::Topic { id, .. } => Some(id.as_str().to_owned()),
                SidebarRow::Group { .. } => None,
            })
            .collect()
    }

    /// `/` opens the filter, the characters narrow the sidebar, and `Esc`
    /// leaves the box KEEPING what was typed — the model's contract, and the
    /// reason the filter is not a modal editor: leaving a search is not
    /// undoing it.
    #[test]
    fn the_filter_editor_types_narrows_and_keeps_its_text_on_esc() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        let todos = topic_ids(&app);
        assert!(todos.len() > 3, "el corpus trae varias páginas: {todos:?}");

        press(&mut app, &mut r, KeyCode::Char('/'));
        assert!(state(&app).filtering(), "`/` abre el filtro");

        for c in "copying".chars() {
            press(&mut app, &mut r, KeyCode::Char(c));
        }
        let filtrados = topic_ids(&app);
        assert_eq!(
            filtrados,
            vec!["copying".to_owned()],
            "la lateral se estrecha a lo tecleado"
        );
        assert!(
            filtrados.len() < todos.len(),
            "el filtro tiene que quitar algo o no filtra nada"
        );

        // Y las teclas son FIJAS: `/` es un carácter más dentro de la caja, no
        // el verbo `dialog.filter` otra vez.
        press(&mut app, &mut r, KeyCode::Char('/'));
        assert_eq!(state(&app).filter_raw(), "copying/");
        press(&mut app, &mut r, KeyCode::Backspace);
        assert_eq!(state(&app).filter_raw(), "copying");

        press(&mut app, &mut r, KeyCode::Esc);
        assert!(!state(&app).filtering(), "Esc sale de la caja");
        assert_eq!(
            state(&app).filter_raw(),
            "copying",
            "…CONSERVANDO el texto: salir de una búsqueda no es deshacerla"
        );
        assert!(app.help.is_some(), "y Esc en la caja NO cierra el overlay");
    }

    /// Enter sobre una fila `Action::Run` devuelve el comando que el run loop
    /// debe despachar — el MISMO id que mandaría la palette — y deja el
    /// overlay CERRADO: el comando actúa sobre los panes de debajo.
    #[test]
    fn enter_on_a_runnable_row_hands_the_command_over_and_closes() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        app.help
            .as_mut()
            .expect("abierto")
            .state
            .open(&TopicId::new("copying"));
        press(&mut app, &mut r, KeyCode::Tab);
        assert_eq!(state(&app).focus(), Focus::Body, "Tab pasa al cuerpo");

        let cmd = press(&mut app, &mut r, KeyCode::Enter);
        assert_eq!(
            cmd,
            Some(HelpDispatch::Command(Command::PaneCopy)),
            "la primera fila de `copying` es `pane.copy`"
        );
        assert!(
            app.help.is_none(),
            "el overlay se cierra ANTES de despachar"
        );
    }

    /// Deja la ayuda abierta sobre la página de `acme.ftp`, con una fila
    /// ejecutable y el foco ya en el cuerpo: lo que ve un lector que llegó por
    /// `F1` desde el gestor de extensiones.
    fn app_con_pagina_de_plugin(activo: bool) -> (App, Resolver) {
        let mut app = app_with_help();
        let mut plugin = norte_proto::methods::PluginInfo {
            id: "acme.ftp".into(),
            name: "FTP".into(),
            publisher: "ACME".into(),
            version: "1.0.0".into(),
            category: "command".into(),
            capabilities: Vec::new(),
            approved: activo,
            enabled: activo,
            description: None,
            commands: vec![norte_proto::methods::PluginCommandInfo {
                id: "sync".into(),
                title: "Sincronizar".into(),
            }],
            columns: Vec::new(),
            has_help: true,
        };
        plugin.has_help = true;
        app.freeze_help_plugins(std::slice::from_ref(&plugin));
        let help = app.help.as_mut().expect("abierto");
        help.state.open(&TopicId::new("acme.ftp"));
        let parsed = norte_help::parse_untrusted(
            b"+++\nid = \"acme.ftp\"\ntitle = \"FTP\"\n\
              commands = [\"plugin:acme.ftp:sync\"]\n+++\ncuerpo",
            "acme.ftp",
            None,
        );
        help.state.install_plugin_topic(parsed.topic);
        let mut r = dialog_resolver();
        press(&mut app, &mut r, KeyCode::Tab);
        assert_eq!(state(&app).focus(), Focus::Body, "Tab pasa al cuerpo");
        (app, r)
    }

    /// H3e: Enter sobre la fila de un plugin ACTIVO la despacha de verdad.
    ///
    /// No lo hacía. La clave es `plugin:{id}:{cmd}`, que no vive en `COMMANDS`
    /// y que `Command::parse` rechaza, así que el Enter caía en el brazo de
    /// «esta fila no es ejecutable» — sobre una fila que el propio resolver
    /// acababa de pintar como DISPONIBLE, con el pie prometiendo `⏎ ejecutar`.
    /// La atenuación de `verdict_with_plugins` era decorativa: la app se negaba
    /// tanto con la fila encendida como con la apagada.
    #[test]
    fn enter_sobre_la_fila_de_un_plugin_activo_la_despacha() {
        let (mut app, mut r) = app_con_pagina_de_plugin(true);
        assert!(
            norte_help::ChordResolver::availability(&*app.help_chords, "plugin:acme.ftp:sync")
                .is_available(),
            "la premisa: el resolver la pinta disponible"
        );
        let cmd = press(&mut app, &mut r, KeyCode::Enter);
        assert_eq!(
            cmd,
            Some(HelpDispatch::Plugin(
                "acme.ftp".to_owned(),
                "sync".to_owned()
            )),
            "el run loop recibe qué plugin y qué comando, ya separados"
        );
        assert!(
            app.help.is_none(),
            "y el overlay se cierra ANTES de despachar, como con un built-in"
        );
    }

    /// Y sobre la de un plugin APAGADO se niega. La foto congelada no autoriza
    /// nada —`plugin.run_command` comprueba aprobado+activo por su cuenta en el
    /// servidor— pero una fila atenuada que al pulsarla corriera sería peor que
    /// no atenuar nada: el lector aprendería que la atenuación no significa
    /// nada.
    #[test]
    fn enter_sobre_la_fila_de_un_plugin_apagado_se_niega() {
        let (mut app, mut r) = app_con_pagina_de_plugin(false);
        assert_eq!(
            norte_help::ChordResolver::availability(&*app.help_chords, "plugin:acme.ftp:sync")
                .reason(),
            Some(norte_help::Reason::PluginInactive),
            "la premisa: el resolver la pinta atenuada"
        );
        let cmd = press(&mut app, &mut r, KeyCode::Enter);
        assert_eq!(cmd, None, "no se despacha nada");
        assert!(app.help.is_some(), "y la ayuda se queda abierta");
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("msg-help-not-runnable").as_str()),
            "comerse el Enter en silencio se leería como que el comando corrió"
        );
    }

    /// La lista `commands` de un tema puede nombrar un verbo `dialog.*` (el
    /// tema `help` documenta tres): no son despachables desde un pane. No se
    /// despacha nada, el overlay SIGUE abierto y se dice — comerse el Enter
    /// en silencio se leería como que el comando corrió.
    #[test]
    fn enter_on_a_dialog_verb_row_dispatches_nothing_and_says_so() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        app.help
            .as_mut()
            .expect("abierto")
            .state
            .open(&TopicId::new("help"));
        press(&mut app, &mut r, KeyCode::Tab);
        // `commands` del tema `help`: app.help, app.palette, dialog.filter…
        press(&mut app, &mut r, KeyCode::Down);
        press(&mut app, &mut r, KeyCode::Down);
        assert_eq!(
            state(&app).action(),
            Some(&norte_frontend::help::Action::Run("dialog.filter".into())),
            "la tercera fila del tema `help` es un verbo de overlay"
        );

        let cmd = press(&mut app, &mut r, KeyCode::Enter);
        assert_eq!(cmd, None, "un `dialog.*` no se despacha desde un pane");
        assert!(app.help.is_some(), "y el overlay se queda donde estaba");
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("msg-help-not-runnable").as_str()),
            "el Enter no puede desaparecer en silencio"
        );
    }

    /// Enter sobre un enlace lo SIGUE y el overlay sigue abierto (leer no es
    /// salir); `dialog.back` vuelve a la página de la que venía.
    #[test]
    fn enter_on_a_link_follows_it_and_back_returns() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        assert_eq!(state(&app).current().as_str(), "index");
        // El índice no tiene `commands`: todas sus acciones son `see_also`.
        press(&mut app, &mut r, KeyCode::Tab);
        assert_eq!(state(&app).focus(), Focus::Body);
        let destino = match state(&app).action() {
            Some(norte_frontend::help::Action::Open(id)) => id.as_str().to_owned(),
            otro => panic!("la primera acción del índice es un enlace: {otro:?}"),
        };

        let cmd = press(&mut app, &mut r, KeyCode::Enter);
        assert_eq!(cmd, None, "un enlace no despacha nada");
        assert!(app.help.is_some(), "…y el overlay SIGUE abierto");
        assert_eq!(state(&app).current().as_str(), destino);

        press(&mut app, &mut r, KeyCode::Backspace);
        assert!(app.help.is_some(), "volver tampoco cierra");
        assert_eq!(state(&app).current().as_str(), "index");
    }

    /// Enter en la lateral SOBRE EL TEMA YA ABIERTO entra al cuerpo. Arrear la
    /// lateral previsualiza, así que ése es el caso normal y `open` sería un
    /// no-op: un Enter mudo que nadie puede distinguir de un fallo.
    #[test]
    fn enter_on_the_open_topic_moves_the_focus_into_the_body() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        assert_eq!(state(&app).focus(), Focus::Topics);
        assert_eq!(
            state(&app).selected_topic().map(TopicId::as_str),
            Some(state(&app).current().as_str()),
            "el cursor de la lateral se apoya en el tema abierto"
        );

        let cmd = press(&mut app, &mut r, KeyCode::Enter);
        assert_eq!(cmd, None, "entrar al cuerpo no despacha nada");
        assert!(app.help.is_some(), "…ni cierra el overlay");
        assert_eq!(
            state(&app).focus(),
            Focus::Body,
            "Enter significa «ir a lo que estoy mirando»"
        );
    }

    /// La otra rama: con el resalte sobre un tema DISTINTO del abierto —lo
    /// que pasa al seguir un `see_also` desde una lista filtrada, donde el
    /// resalte se queda en la fila visible más cercana— Enter lo abre.
    #[test]
    fn enter_on_a_topic_that_is_not_the_open_one_opens_it() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        // Filtrar a `copying` y seguir su primer enlace: el destino no está
        // en la lateral filtrada, así que el resalte se queda en `copying`.
        press(&mut app, &mut r, KeyCode::Char('/'));
        for c in "copying".chars() {
            press(&mut app, &mut r, KeyCode::Char(c));
        }
        press(&mut app, &mut r, KeyCode::Esc);
        assert_eq!(topic_ids(&app), vec!["copying".to_owned()]);
        press(&mut app, &mut r, KeyCode::Tab);
        assert_eq!(state(&app).focus(), Focus::Body);
        while !matches!(
            state(&app).action(),
            Some(norte_frontend::help::Action::Open(_))
        ) {
            press(&mut app, &mut r, KeyCode::Down);
        }
        press(&mut app, &mut r, KeyCode::Enter);
        let abierto = state(&app).current().as_str().to_owned();
        assert_ne!(abierto, "copying", "el enlace llevó a otra página");
        assert_eq!(
            state(&app).selected_topic().map(TopicId::as_str),
            Some("copying"),
            "…y el resalte se quedó donde el filtro lo dejó"
        );

        // Enter en la lateral abre lo resaltado, que NO es lo abierto.
        press(&mut app, &mut r, KeyCode::Tab);
        assert_eq!(state(&app).focus(), Focus::Topics);
        let cmd = press(&mut app, &mut r, KeyCode::Enter);
        assert_eq!(cmd, None);
        assert_eq!(
            state(&app).current().as_str(),
            "copying",
            "Enter abre el tema resaltado"
        );
    }

    /// `dialog.back` en la RAÍZ (sin historial) cierra el overlay. Es lo que
    /// convierte `Backspace` en una tecla honesta en vez de una muerta.
    #[test]
    fn back_at_the_root_closes_the_overlay() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        press(&mut app, &mut r, KeyCode::Backspace);
        assert!(
            app.help.is_none(),
            "sin historial, «atrás» solo puede significar salir"
        );
    }

    /// Un verbo `dialog.*` FUERA de `ALLOW_HELP` es INERTE aquí, aunque el
    /// keymap lo tenga bien atado: la semántica de cada overlay vive en
    /// código. `y` es `dialog.approve` en el preset orthodox.
    #[test]
    fn a_verb_outside_the_allowlist_is_inert() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        let antes = state(&app).current().clone();
        let cmd = press(&mut app, &mut r, KeyCode::Char('y'));
        assert_eq!(cmd, None);
        assert!(app.help.is_some(), "`dialog.approve` no cierra la ayuda");
        assert_eq!(state(&app).current(), &antes, "ni navega");
    }

    /// La tecla que abre la ayuda la cierra: F1 resuelve a `app.help`, que
    /// NO está en `ALLOW_HELP` (es de `[global]`), y sin su rama propia sería
    /// inerte justo dentro del overlay que abre.
    #[test]
    fn the_key_that_opens_the_help_closes_it() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        let cmd = press(&mut app, &mut r, KeyCode::F(1));
        assert_eq!(cmd, None, "cerrar no despacha nada");
        assert!(app.help.is_none(), "F1 dentro de la ayuda la cierra");
    }

    /// …pero no mientras se teclea en el filtro: ahí la caja consume la
    /// tecla, como en la palette y el diálogo de búsqueda.
    #[test]
    fn the_filter_box_keeps_the_toggle_key() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        press(&mut app, &mut r, KeyCode::Char('/'));
        assert!(state(&app).filtering());
        press(&mut app, &mut r, KeyCode::F(1));
        assert!(
            app.help.is_some(),
            "una tecla de función dentro del editor no cierra el overlay"
        );
    }

    /// `ctrl+c` conserva su salida global y `ctrl+p` cruza a la palette
    /// LLEVÁNDOSE el filtro — los dos son el mismo modelo a dos velocidades
    /// (lo dice el tema `help`), así que no hay que reteclearlo.
    #[test]
    fn ctrl_c_quits_and_ctrl_p_hands_the_filter_to_the_palette() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        on_help_key(&mut app, &mut r, KeyModifiers::CONTROL, KeyCode::Char('c'));
        assert!(app.quit, "la salida de emergencia va antes que todo");

        let mut app = app_with_help();
        app.palette_rows =
            norte_tui::palette::build_rows(&eff(Screen::Browse), &eff(Screen::Viewer));
        press(&mut app, &mut r, KeyCode::Char('/'));
        for c in "copy".chars() {
            press(&mut app, &mut r, KeyCode::Char(c));
        }
        on_help_key(&mut app, &mut r, KeyModifiers::CONTROL, KeyCode::Char('p'));
        assert!(app.help.is_none(), "la ayuda cede el sitio");
        let palette = app.palette.as_ref().expect("la palette abrió");
        assert!(
            !palette.visible().is_empty(),
            "el filtro llegó y sigue casando algo"
        );
        assert!(
            palette.visible().len() < palette.rows().len(),
            "…y de verdad filtró: {} de {}",
            palette.visible().len(),
            palette.rows().len()
        );
    }
}

/// ¿Puede un evento de vigilancia disparar un refresh AHORA? (#106,
/// review MAJOR-2): con cualquier overlay abierto o un quick search
/// tecleándose, `refresh_panes` consumiría las teclas del usuario (su loop
/// de cancelación descarta todo lo que no sea Esc/Ctrl-C) y Esc pasaría a
/// significar «abandona el refresh» — jamás pisar la interacción en curso.
/// El evento queda encolado (capacidad 1) y dispara al despejarse.
fn watch_refresh_allowed(app: &App) -> bool {
    app.modal.is_none()
        && app.palette.is_none()
        && app.settings.is_none()
        && app.help.is_none()
        && app.viewer.is_none()
        && app.theme_picker.is_none()
        && app.columns_picker.is_none()
        && app.extensions.is_none()
        && app.nav_popup.is_none()
        && app.search_dialog.is_none()
        && app.panes.iter().all(|p| p.quick().is_none())
}

/// Dirs NATIVOS vigilables de los panes (#106): solo `file://` (un dir
/// sftp/S3/archive no tiene inotify — su refresh sigue siendo Ctrl+R) y
/// solo panes reales (el virtual de búsqueda no muestra un dir). Puro:
/// `vpath_to_native` no toca el FS.
fn watch_targets(app: &App) -> [Option<std::path::PathBuf>; 2] {
    std::array::from_fn(|i| {
        let p = &app.panes[i];
        if p.virtual_search {
            return None;
        }
        norte_vfs_local::vpath_to_native(p.dir()).ok()
    })
}

/// Los ids attr CONFIGURADOS de cada pane visible (#117): la huella que
/// decide si un cambio de columnas exige re-listar — los valores attr solo
/// llegan pidiéndolos en `fs.list`, así que un id nuevo con el listado
/// viejo pintaría blanco (ausencia) hasta el próximo cd. La huella ordenada
/// vive en el modelo (una única definición para ambos frontends).
fn pane_attr_ids(app: &App) -> Vec<Vec<String>> {
    // #117-follow-up (review MAJOR-1): huella COMBINADA attr+plugin, única
    // definición en el modelo (`pane_fingerprint`) para ambos frontends —
    // un cambio SOLO de plugins también re-lista (el re-list respawnea el
    // fetch de valores; sin él la columna nueva quedaría en blanco).
    app.panes
        .iter()
        .map(|p| app.columns.pane_fingerprint(p.dir().scheme()))
        .collect()
}

/// Aplica el resultado del picker (#108 7a): sesión primero (settings en
/// memoria + re-sort de TODO pane, `apply_scheme_sort` es no-op donde el
/// spec no cambia), disco después (`config::persist_columns` en
/// `spawn_blocking` — regla 2). A la barra va la CATEGORÍA del error, jamás
/// el Display del SO (#73). Devuelve `true` si el set de attrs pintado de
/// algún pane visible cambió (#117): el caller re-lista entonces por el
/// mismo camino que tras una mutación.
async fn apply_picked_columns(
    app: &mut App,
    picked: norte_frontend::columns_picker::Picked,
) -> bool {
    let attrs_before = pane_attr_ids(app);
    app.columns
        .apply_picked(picked.scheme_target.as_deref(), &picked.ids, picked.sort);
    // #108 7b: los formatos ciclados también EN SESIÓN antes del disco —
    // mismo lockstep (`apply_format` toca el spec retenido que lee
    // `style_for`).
    for (id, fmt) in &picked.formats {
        app.columns.apply_format(id, fmt);
    }
    for i in 0..app.panes.len() {
        app.apply_scheme_sort(i);
    }
    let needs_refresh = pane_attr_ids(app) != attrs_before;
    let Some(dir) = config::user_config_dir() else {
        app.message = Some(t("msg-settings-no-config-dir"));
        return needs_refresh;
    };
    let ids = picked.ids.clone();
    let scheme = picked.scheme_target.clone();
    let sort = picked.sort;
    let formats = picked.formats.clone();
    let res = tokio::task::spawn_blocking(move || {
        // Todas las escrituras en UNA tarea de fondo, secuenciales sobre el
        // mismo fichero (#108 7b): la lista+sort y después cada formato
        // ciclado — un solo desenlace, un solo toast.
        config::persist_columns(
            &dir,
            scheme.as_deref(),
            &ids,
            config::PersistSort {
                column: match sort.column {
                    norte_frontend::SortColumn::Name => "name",
                    norte_frontend::SortColumn::Size => "size",
                    norte_frontend::SortColumn::Mtime => "mtime",
                },
                descending: sort.dir == norte_frontend::SortDir::Desc,
                dirs_first: sort.dirs_first,
            },
        )?;
        for (id, fmt) in &formats {
            config::persist_column_format(&dir, id, fmt)?;
        }
        Ok::<_, std::io::Error>(())
    })
    .await;
    match res {
        Ok(Ok(())) => app.message = Some(t("msg-columns-saved")),
        Ok(Err(e)) => {
            app.message = Some(ta(
                "msg-settings-save-failed",
                &[("error", &io_error_category(&e))],
            ));
        }
        // Un panic en el write es un bug nuestro: que no tumbe la TUI (misma
        // disciplina que `persist_setting`) — se anuncia y queda traza.
        Err(e) => {
            tracing::error!(error = %e, "tarea de fondo de persist_columns no terminó");
            app.message = Some(t("msg-settings-save-crashed"));
        }
    }
    needs_refresh
}

/// Qué hacer tras procesar una tecla del overlay de ajustes — separa el
/// cómputo PURO (dentro del borrow de `app.settings`, `on_settings_key`) del
/// I/O async (`persist_setting`, fuera de ese borrow): `Settings::activate`/
/// `edit_commit` no pueden devolver directamente y persistir en el mismo
/// paso porque ya toman `&mut app.settings` — separarlo en un enum evita
/// pedir prestado `app` dos veces a la vez.
enum SettingsKeyOutcome {
    /// La tecla se consumió sin nada que persistir (navegación/filtro/
    /// edición de buffer en curso).
    None,
    /// Esc fuera de edición: cierra el overlay.
    Close,
    /// Un ajuste cambió — persistir y anunciar. Boxed: `PendingWrite` lleva
    /// un `toml_edit::Value` propio y hace este brazo mucho más grande que
    /// el resto (clippy `large_enum_variant`) — indirección, no un tipo
    /// distinto.
    Write(Box<PendingWrite>),
    /// `Settings::edit_commit` rechazó el buffer — anunciar el error, sin
    /// tocar nada (el buffer se queda, `Settings` ya lo conserva).
    Invalid(SettingsEditError),
}

/// Teclas del overlay de ajustes (`app.settings`, S3): mismo criterio que la
/// palette (decisión 8 del plan H1) — editor de filtro libre, NO resuelve
/// por el contexto `dialog`; sus teclas quedan hardcodeadas aquí. `ctrl+c`
/// conserva su salida global. Mientras `Settings::is_editing()` las teclas
/// van al buffer de edición inline (mismo patrón que `name_input` del popup
/// de navegación: imprimibles/backspace crudos, Enter confirma, Esc
/// cancela); si no, navegan/filtran como la palette y Enter activa la fila
/// bajo el cursor (`Settings::activate` — cicla YA para `Bool`/`Enum`/
/// `ThemeName`/`PresetName`, o abre el buffer para `Text`/`Int`).
async fn on_settings_key(app: &mut App, mods: KeyModifiers, code: KeyCode) {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    let plain = mods.is_empty() || mods == KeyModifiers::SHIFT;
    let outcome = {
        let Some(settings) = &mut app.settings else {
            return;
        };
        if settings.is_editing() {
            match code {
                KeyCode::Char(c) if plain => {
                    settings.edit_push_char(c);
                    SettingsKeyOutcome::None
                }
                KeyCode::Backspace if plain => {
                    settings.edit_backspace();
                    SettingsKeyOutcome::None
                }
                KeyCode::Esc => {
                    settings.edit_cancel();
                    SettingsKeyOutcome::None
                }
                KeyCode::Enter => match settings.edit_commit() {
                    Ok(write) => SettingsKeyOutcome::Write(Box::new(write)),
                    Err(e) => SettingsKeyOutcome::Invalid(e),
                },
                _ => SettingsKeyOutcome::None,
            }
        } else {
            match code {
                KeyCode::Char(c) if plain => {
                    settings.push_char(c);
                    SettingsKeyOutcome::None
                }
                KeyCode::Backspace if plain => {
                    settings.backspace();
                    SettingsKeyOutcome::None
                }
                KeyCode::Esc if plain => SettingsKeyOutcome::Close,
                KeyCode::Up if plain => {
                    settings.up();
                    SettingsKeyOutcome::None
                }
                KeyCode::Down if plain => {
                    settings.down();
                    SettingsKeyOutcome::None
                }
                KeyCode::PageUp if plain => {
                    settings.page_up(PAGE);
                    SettingsKeyOutcome::None
                }
                KeyCode::PageDown if plain => {
                    settings.page_down(PAGE);
                    SettingsKeyOutcome::None
                }
                KeyCode::Enter if plain => {
                    // Listas VIVAS para `ThemeName`/`PresetName` (mismo
                    // criterio que `App::open_theme_picker`): resueltas aquí,
                    // no `&'static` — el tema/keymap efectivo puede cambiar
                    // en caliente.
                    let theme_names: Vec<String> = norte_theme::preset_names()
                        .into_iter()
                        .map(String::from)
                        .collect();
                    let all_presets = presets();
                    let preset_names: Vec<&str> = all_presets.iter().map(|(n, _)| *n).collect();
                    match settings.activate(&theme_names, &preset_names) {
                        Some(write) => SettingsKeyOutcome::Write(Box::new(write)),
                        None => SettingsKeyOutcome::None,
                    }
                }
                _ => SettingsKeyOutcome::None,
            }
        }
    };
    match outcome {
        SettingsKeyOutcome::None => {}
        SettingsKeyOutcome::Close => app.settings = None,
        SettingsKeyOutcome::Write(write) => persist_setting(app, *write).await,
        SettingsKeyOutcome::Invalid(e) => app.message = Some(settings_edit_error_message(&e)),
    }
}

/// Persiste un [`PendingWrite`] (S3) — `spawn_blocking` (regla 2), mismo
/// patrón que el persist del theme picker (`on_theme_picker_key` arriba):
/// resuelve `user_config_dir()` a mano en vez de reutilizar
/// `config::persist_ui_theme` (esa wrapper no toma `section`/`key` — S2 solo
/// dio el genérico `persist_set(dir, ...)` con `dir` explícito).
async fn persist_setting(app: &mut App, write: PendingWrite) {
    let Some(dir) = config::user_config_dir() else {
        app.message = Some(t("msg-settings-no-config-dir"));
        return;
    };
    let PendingWrite {
        section,
        key,
        value,
        name,
        display,
    } = write;
    match tokio::task::spawn_blocking(move || config::persist_set(&dir, section, &key, value)).await
    {
        Ok(Ok(_path)) => {
            app.message = Some(ta(
                "msg-settings-saved",
                &[("name", &name), ("value", &display)],
            ));
        }
        Ok(Err(e)) => {
            app.message = Some(ta(
                "msg-settings-save-failed",
                &[("error", &io_error_category(&e))],
            ));
        }
        // Revisión S I1: la tarea de `spawn_blocking` panicó o se canceló
        // (antes: silencio total — la fila optimista de `Settings::
        // commit_row` quedaba MINTIENDO "editado" aunque nada se escribió).
        // No debe tumbar la TUI: se anuncia en la barra (categoría genérica,
        // sin `{$error}` — un `JoinError` no trae una categoría limpia) y se
        // deja rastro con `tracing` para diagnóstico — jamás `eprintln!`
        // aquí, que corrompería la pantalla alterna de ratatui mientras la
        // TUI sigue viva.
        Err(e) => {
            tracing::error!(error = %e, "tarea de fondo de persist_setting no terminó");
            app.message = Some(t("msg-settings-save-crashed"));
        }
    }
}

/// Mensaje de barra para un [`SettingsEditError`] (S3) — por CATEGORÍA
/// Fluent, nunca texto ad hoc (#73 pattern). Envoltorio fino (revisión S,
/// M6): byte-idéntico al de la GUI (`settings_view::edit_error_message`) —
/// hoisteado a [`norte_frontend::settings::edit_error_message`].
fn settings_edit_error_message(e: &SettingsEditError) -> String {
    norte_frontend::settings::edit_error_message(e)
}

#[cfg(test)]
mod settings_message_tests {
    use norte_i18n::{Lang, t_in};

    /// Revisión S I1: `msg-settings-save-crashed` (el brazo `Err(_)` de
    /// `persist_setting`, ver su doc) resuelve a texto REAL en ambos
    /// locales — no al id crudo, que es lo que se vería en la barra si
    /// faltara la clave en algún `.ftl`. Mismo criterio de cobertura que
    /// `norte_frontend::settings`'s `fluent_keys_existen_en_ambos_locales_
    /// para_cada_entrada`.
    #[test]
    fn msg_settings_save_crashed_existe_en_ambos_locales() {
        for lang in [Lang::Es, Lang::En] {
            assert_ne!(
                t_in(lang, "msg-settings-save-crashed"),
                "msg-settings-save-crashed",
                "falta la clave en {lang:?}"
            );
        }
    }
}

/// `F1` in the extension manager: open the help on the highlighted plugin's OWN
/// page (H3e).
///
/// The manager is where a human decides whether to approve an extension, and
/// the page that argues for it is one keystroke away — from the list they are
/// already looking at, with no detour through the help's own sidebar. The
/// snapshot is the list the manager ALREADY holds, so this costs no round trip;
/// the page itself is fetched by the run loop, on demand, like any other plugin
/// node.
///
/// Order matters here and the sequence is not interchangeable:
/// `HelpState::open_as_root` refuses an id that names nothing it can show, so
/// the nodes have to be installed BEFORE the page is opened.
///
/// The manager CLOSES, as the palette does for its own `F1` bridge: its arm
/// sits ahead of the help in the run loop's key chain, so an overlay left open
/// underneath would eat every key meant for the page.
///
/// A plugin with no `help.md` gets a status line rather than silence — the row
/// looks exactly like one that does, and a key that appears to do nothing reads
/// as a broken app. `over_modal` is `false`: this arm only runs with no modal on
/// screen (`modal_wins`).
fn extensions_help(app: &mut App, lang: norte_help::Lang, help_lines: &[String]) {
    let Some(plugin) = app.extensions.as_ref().and_then(ExtensionManager::selected) else {
        return;
    };
    if !plugin.has_help {
        app.message = Some(t("msg-extensions-no-help"));
        return;
    }
    let id = norte_help::TopicId::new(&plugin.id);
    let Some(plugins) = app.extensions.take().map(|m| m.plugins) else {
        return;
    };
    // H3d: el mismo congelado que `open_contextual_help` — la ayuda que se abre
    // desde el gestor es la misma ayuda.
    app.freeze_help_facts();
    app.help = Some(HelpView::new(lang, help_lines.to_vec()));
    app.freeze_help_plugins(&plugins);
    if let Some(help) = app.help.as_mut() {
        help.state.open_as_root(&id);
    }
}

#[cfg(test)]
mod extensions_help_tests {
    use super::{App, ExtensionManager, Pane, extensions_help};
    use norte_vfs::VPath;

    fn app_con(plugins: Vec<norte_proto::methods::PluginInfo>) -> App {
        let d = VPath::parse("file:///x").expect("wire de test");
        let mut app = App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()));
        app.extensions = Some(ExtensionManager {
            plugins,
            errors: Vec::new(),
            cursor: 0,
            config: None,
        });
        app
    }

    fn plugin(id: &str, has_help: bool) -> norte_proto::methods::PluginInfo {
        norte_proto::methods::PluginInfo {
            id: id.to_owned(),
            name: id.to_owned(),
            publisher: "ACME".to_owned(),
            version: "1.0.0".to_owned(),
            category: "command".to_owned(),
            capabilities: Vec::new(),
            approved: true,
            enabled: true,
            description: None,
            commands: Vec::new(),
            columns: Vec::new(),
            has_help,
        }
    }

    /// H3e: `F1` sobre la fila de un plugin con `help.md` abre la ayuda EN SU
    /// página, con el catálogo que el gestor ya tenía — sin pasar por la
    /// lateral y sin una segunda ida al daemon.
    #[test]
    fn f1_sobre_un_plugin_con_ayuda_abre_su_pagina() {
        let mut app = app_con(vec![plugin("acme.ftp", true)]);
        extensions_help(&mut app, norte_help::Lang::En, &[]);
        let help = app.help.as_ref().expect("la ayuda se abrió");
        assert_eq!(help.state.current().as_str(), "acme.ftp");
        assert!(
            app.extensions.is_none(),
            "el gestor se cierra: su rama va ANTES en la cadena de teclas y se \
             comería las teclas de la página"
        );
        // La página llega como RAÍZ del rastro: al lector lo PUSIERON ahí, así
        // que un `Esc` tiene que salir, no volver a un índice que no visitó.
        assert!(!app.help.as_mut().expect("abierta").state.back());
    }

    /// Y sobre una fila sin `help.md` se DICE. La fila es idéntica a una que sí
    /// la tiene, y una tecla que calla no se distingue de una rota.
    #[test]
    fn f1_sobre_un_plugin_sin_ayuda_lo_dice_y_no_cierra_el_gestor() {
        let mut app = app_con(vec![plugin("acme.ftp", false)]);
        extensions_help(&mut app, norte_help::Lang::En, &[]);
        assert!(app.help.is_none(), "no hay página que abrir");
        assert!(app.extensions.is_some(), "el gestor se queda donde estaba");
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("msg-extensions-no-help").as_str())
        );
    }
}

/// Teclas del overlay de extensiones (M4-P3), resueltas contra el contexto
/// `dialog` del keymap (H1 T2, issue #24); `ctrl+c` conserva su salida
/// global, hardcodeado ANTES de resolver. Regla 7: aprobar/activar viaja al
/// core por el `Backend`; el bool LOCAL solo se togglea tras un OK (feedback
/// inmediato sin relistar). El id y el estado se toman ANTES del `.await`
/// (el borrow del `mgr` se suelta durante la llamada al backend y se
/// re-obtiene después para reflejar el resultado). Allowlist de este
/// overlay: `dialog.up/down/cancel/approve/toggle-enabled` — `approve`
/// togglea la APROBACIÓN del plugin (decisión 3 del plan H1: "aprobar un
/// plugin" reutiliza semánticamente `dialog.approve`, antes era la tecla
/// `a` hardcodeada; ahora `a` es `dialog.add`, que este overlay no soporta).
///
/// Fuera del allowlist, una sola tecla más: `app.help` (H3e) abre la página del
/// plugin resaltado — ver [`extensions_help`].
async fn on_extensions_key(
    app: &mut App,
    backend: &Backend,
    resolver: &mut Resolver,
    lang: norte_help::Lang,
    help_lines: &[String],
    mods: KeyModifiers,
    code: KeyCode,
) {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    if app.extensions.is_none() {
        return;
    }
    // G3c drill-down: while a `[config]` `string`/`int` edit buffer is
    // active, keys are captured RAW (same idiom as `on_nav_popup_key`'s
    // `name_input`) — bypassing the keymap resolver entirely, so typing
    // e.g. "y" edits the buffer instead of resolving to `dialog.approve`.
    let editing = app
        .extensions
        .as_ref()
        .and_then(|m| m.config.as_ref())
        .is_some_and(|p| p.state.is_editing());
    if editing {
        on_plugin_config_edit_key(app, backend, mods, code).await;
        return;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return; // tecla no modelada por el keymap: ignorar
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run(cmd) => cmd,
        Resolution::Pending(_) => {
            resolver.reset();
            return;
        }
        Resolution::Reset => return,
    };
    let panel_open = app.extensions.as_ref().is_some_and(|m| m.config.is_some());
    // H3e: `app.help` es un comando de `[global]`, no un verbo `dialog.*`, así
    // que no está en ningún allowlist de este overlay y sin esta rama F1 sería
    // inerte aquí. Se resuelve por el keymap como todo lo demás (un rebind de
    // `app.help` mueve también este puente); lo cableado es el significado, no
    // la tecla. Mismo criterio que la rama `app.help` de `on_help_key` y que F9
    // en `on_theme_picker_key`. NO cuando el panel de `[config]` está abierto:
    // ahí el lector está editando valores, y perder el panel para leer prosa no
    // es lo que pidió.
    if cmd == "app.help" && !panel_open {
        extensions_help(app, lang, help_lines);
        return;
    }
    // H1 T3: el MISMO allowlist que consume el hint generado
    // (`hints::DialogHints::build`) — una sola fuente para dispatch y footer.
    // G3c: qué allowlist aplica depende de si el panel de `[config]` está
    // abierto.
    let allow: &[&str] = if panel_open {
        ALLOW_PLUGIN_CONFIG
    } else {
        ALLOW_EXTENSIONS
    };
    if !allow.contains(&cmd.as_str()) {
        return; // fuera del allowlist de este contexto: inerte
    }
    if panel_open {
        on_plugin_config_panel_cmd(app, backend, &cmd).await;
    } else {
        on_extensions_list_cmd(app, backend, &cmd).await;
    }
}

/// G3c: teclas RAW mientras un `string`/`int` de `[config]` se edita
/// (`on_extensions_key`'s guard `editing`) — mismo idioma que
/// `on_nav_popup_key`'s `name_input`.
async fn on_plugin_config_edit_key(
    app: &mut App,
    backend: &Backend,
    mods: KeyModifiers,
    code: KeyCode,
) {
    let plain = mods.is_empty() || mods == KeyModifiers::SHIFT;
    match code {
        KeyCode::Char(c) if plain => {
            if let Some(panel) = app.extensions.as_mut().and_then(|m| m.config.as_mut()) {
                panel.state.edit_push_char(c);
            }
        }
        KeyCode::Backspace if plain => {
            if let Some(panel) = app.extensions.as_mut().and_then(|m| m.config.as_mut()) {
                panel.state.edit_backspace();
            }
        }
        KeyCode::Esc => {
            if let Some(panel) = app.extensions.as_mut().and_then(|m| m.config.as_mut()) {
                panel.state.edit_cancel();
            }
        }
        KeyCode::Enter => {
            let Some(panel) = app.extensions.as_mut().and_then(|m| m.config.as_mut()) else {
                return;
            };
            match panel.state.edit_commit() {
                Ok(write) => {
                    let id = panel.plugin_id.clone();
                    commit_plugin_config_write(app, backend, &id, write).await;
                }
                Err(err) => {
                    app.message = Some(norte_frontend::settings::edit_error_message(&err));
                }
            }
        }
        _ => {}
    }
}

/// G3c: comandos resueltos (`up`/`down`/`confirm`/`cancel`) mientras el
/// panel de `[config]` está abierto y NADA se edita (`on_extensions_key`,
/// `panel_open` branch — `allow == ALLOW_PLUGIN_CONFIG`).
async fn on_plugin_config_panel_cmd(app: &mut App, backend: &Backend, cmd: &str) {
    let Some(panel) = app.extensions.as_mut().and_then(|m| m.config.as_mut()) else {
        return;
    };
    match cmd {
        "dialog.up" => panel.state.up(),
        "dialog.down" => panel.state.down(),
        "dialog.cancel" => {
            if let Some(mgr) = &mut app.extensions {
                mgr.config = None;
            }
        }
        "dialog.confirm" => {
            if let Some(write) = panel.state.activate() {
                let id = panel.plugin_id.clone();
                commit_plugin_config_write(app, backend, &id, write).await;
            }
        }
        _ => {}
    }
}

/// El resto de `on_extensions_key`: comandos sobre la LISTA de plugins
/// (`panel_open == false`, `allow == ALLOW_EXTENSIONS`) — navegar,
/// aprobar/activar, y `dialog.confirm` (G3c) abre el panel de `[config]`
/// del plugin resaltado SI declara alguna clave. Enter NUNCA aprueba (pin
/// P1): solo entra en un submenú.
async fn on_extensions_list_cmd(app: &mut App, backend: &Backend, cmd: &str) {
    let Some(mgr) = &mut app.extensions else {
        return;
    };
    match cmd {
        "dialog.up" => mgr.up(),
        "dialog.down" => mgr.down(),
        "dialog.cancel" => app.extensions = None,
        "dialog.approve" => {
            // Id y estado ANTES del await (suelta el borrow de `mgr`).
            let Some((id, cur)) = mgr.selected().map(|p| (p.id.clone(), p.approved)) else {
                return;
            };
            match backend.plugins_set_approval(&id, !cur).await {
                Ok(()) => {
                    if let Some(mgr) = &mut app.extensions {
                        mgr.set_local_approved(!cur);
                    }
                }
                Err(e) => app.message = Some(error_message(&e)),
            }
        }
        "dialog.toggle-enabled" => {
            let Some((id, cur)) = mgr.selected().map(|p| (p.id.clone(), p.enabled)) else {
                return;
            };
            match backend.plugins_set_enabled(&id, !cur).await {
                Ok(()) => {
                    if let Some(mgr) = &mut app.extensions {
                        mgr.set_local_enabled(!cur);
                    }
                }
                Err(e) => app.message = Some(error_message(&e)),
            }
        }
        "dialog.confirm" => {
            let Some((id, name)) = mgr.selected().map(|p| (p.id.clone(), p.name.clone())) else {
                return;
            };
            match backend.plugin_get_config(&id).await {
                Ok(result) if !result.keys.is_empty() => {
                    let rows = norte_frontend::plugin_config::sanitize_config_keys(&result.keys);
                    let (plugin_name, _) = norte_tui::app::display_name(name.as_bytes());
                    if let Some(mgr) = &mut app.extensions {
                        mgr.config = Some(norte_tui::app::PluginConfigPanel {
                            plugin_id: id,
                            plugin_name,
                            state: norte_frontend::plugin_config::PluginConfigState::new(rows),
                        });
                    }
                }
                Ok(_) => app.message = Some(t("msg-plugin-config-empty")),
                Err(e) => app.message = Some(error_message(&e)),
            }
        }
        _ => {} // fuera del allowlist de este overlay: inerte
    }
}

/// Persiste UN [`norte_frontend::plugin_config::PendingConfigWrite`] vía
/// `Backend::plugin_set_config` y anuncia el resultado (G3c) — factorizado
/// fuera de [`on_extensions_key`] porque el mismo commit ocurre desde DOS
/// sitios (edición inline confirmada con Enter, y un `bool`/`enum` que
/// cicla de inmediato en `dialog.confirm`).
async fn commit_plugin_config_write(
    app: &mut App,
    backend: &Backend,
    plugin_id: &str,
    write: norte_frontend::plugin_config::PendingConfigWrite,
) {
    match backend
        .plugin_set_config(plugin_id, &write.key, &write.value)
        .await
    {
        Ok(()) => {
            app.message = Some(ta(
                "msg-plugin-config-saved",
                &[("key", &write.key), ("value", &write.display)],
            ));
        }
        Err(e) => app.message = Some(error_message(&e)),
    }
}

/// Teclas del popup de navegación (historial `Alt+↓` / hotlist `Ctrl+D`);
/// `ctrl+c` conserva su salida global, hardcodeado ANTES de nada. Con
/// `name_input` activo (el `a` de hotlist abre un campo para el nombre del
/// favorito) los imprimibles/backspace se capturan como editor de texto RAW
/// — H1 T2 decisión: NO es un comando `dialog.*`, es entrada libre, se
/// queda hardcodeado. Fuera de `name_input`, la tecla resuelve contra el
/// contexto `dialog` del keymap (H1 T2, issue #24); `add`/`remove` los
/// filtra el ALLOWLIST de este overlay a `kind == Hotlist` (el historial no
/// tiene nada que nombrar ni borrar — mismo criterio que antes de H1).
/// Enter sobre un item válido NAVEGA por el flujo de cd normal; si el cd
/// desde el HISTORIAL falla con `NotFound`, la entrada se retira (spec
/// 2026-07-18) — la de hotlist NO (es config del usuario: se avisa y queda).
async fn on_nav_popup_key(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) -> Cd {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return Cd::Cancelled;
    }
    let Some(popup) = &mut app.nav_popup else {
        return Cd::Cancelled;
    };
    let kind = popup.kind;
    // SHIFT pasa (mayúsculas llegan como Char+SHIFT); ctrl/alt no escriben.
    let plain = mods.is_empty() || mods == KeyModifiers::SHIFT;
    if popup.name_input.is_some() {
        match code {
            KeyCode::Char(c) if plain => {
                if let Some(input) = &mut popup.name_input {
                    input.push(c);
                }
            }
            KeyCode::Backspace if plain => {
                if let Some(input) = &mut popup.name_input {
                    input.pop();
                }
            }
            KeyCode::Esc => popup.name_input = None,
            KeyCode::Enter => {
                let name = popup.name_input.take().unwrap_or_default();
                // Input vacío = cancela (plan T5): no hay favorito sin nombre.
                if !name.is_empty() {
                    hotlist_add(app, &name).await;
                }
            }
            _ => {}
        }
        return Cd::Cancelled;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return Cd::Cancelled; // tecla no modelada por el keymap: ignorar
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run(cmd) => cmd,
        Resolution::Pending(_) => {
            resolver.reset();
            return Cd::Cancelled;
        }
        Resolution::Reset => return Cd::Cancelled,
    };
    // H1 T3: el MISMO allowlist que consume el hint generado
    // (`hints::DialogHints::build`, campo `nav_list`) — una sola fuente
    // para dispatch y footer. Cubre AMBOS kinds (History es un subconjunto:
    // `add`/`remove` los filtra el guard `kind == Hotlist` de más abajo).
    if !ALLOW_NAV_HOTLIST.contains(&cmd.as_str()) {
        return Cd::Cancelled; // fuera del allowlist de este overlay: inerte
    }
    match cmd.as_str() {
        "dialog.up" => {
            app.nav_popup_input(PickerAction::Up);
        }
        "dialog.down" => {
            app.nav_popup_input(PickerAction::Down);
        }
        "dialog.cancel" => {
            app.nav_popup_input(PickerAction::Cancel);
        }
        "dialog.add" if kind == NavPopupKind::Hotlist => {
            app.nav_popup_open_name_input();
        }
        "dialog.remove" if kind == NavPopupKind::Hotlist => {
            if let Some(name) = app.nav_popup_selected_hotlist_name() {
                hotlist_remove(app, &name).await;
            }
        }
        "dialog.confirm" => {
            // Confirm sobre un item inválido/vacío es no-op (el popup sigue).
            if let Some(path) = app.nav_popup_input(PickerAction::Confirm) {
                let pane = app.focus();
                let outcome = cd(app, backend, events, path.clone()).await;
                if kind == NavPopupKind::History && matches!(&outcome, Cd::Failed(Error::NotFound))
                {
                    // El dir ya no existe: fuera del historial. La barra ya
                    // muestra el error normal del cd fallido.
                    app.history[pane].remove(&path);
                }
                return outcome;
            }
        }
        _ => {} // fuera del allowlist de este overlay (o kind): inerte
    }
    Cd::Cancelled
}

/// `config::user_config_dir()` o el MISMO io `NotFound` que fabrica
/// `persist_ui_theme` sin entorno (CI pelada): la barra lo pinta como
/// `err-not-found` vía categoría (#73), clave existente y razonable.
fn user_config_dir_io() -> std::io::Result<std::path::PathBuf> {
    config::user_config_dir().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "sin directorio de config de usuario",
        )
    })
}

/// Persiste el favorito `name` = cwd del pane con foco en el `norte.toml`
/// del USUARIO (`spawn_blocking`, regla 2 — `persist_hotlist_add` es
/// bloqueante por contrato). Solo si el disco fue bien se refresca la copia
/// en `App` (consistencia con disco) y sale `msg-hotlist-saved`; un fallo
/// io sale por categoría y la copia NO se toca.
async fn hotlist_add(app: &mut App, name: &str) {
    let target = app.focused().dir().clone();
    let wire = target.to_wire();
    let n = name.to_owned();
    let res = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let dir = user_config_dir_io()?;
        config::persist_hotlist_add(&dir, &n, &wire)?;
        Ok(())
    })
    .await;
    match res {
        Ok(Ok(())) => {
            app.hotlist_apply_saved(name, target);
            // El name lo tecleó el usuario, pero un PASTE puede colar
            // bidi/controles: por `detail_for_bar` como todo detalle (#73).
            app.message = Some(ta("msg-hotlist-saved", &[("name", &detail_for_bar(name))]));
        }
        Ok(Err(e)) => {
            app.message = Some(ta(
                "msg-hotlist-persist-failed",
                &[("error", &io_error_category(&e))],
            ));
        }
        // Un panic al persistir es un bug NUESTRO: que reviente visible
        // (criterio del binario, mismo que `config::load_async`).
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

/// Retira el favorito `name` del `norte.toml` del USUARIO (`spawn_blocking`,
/// regla 2). Mismo contrato de consistencia que [`hotlist_add`].
async fn hotlist_remove(app: &mut App, name: &str) {
    let n = name.to_owned();
    let res = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let dir = user_config_dir_io()?;
        config::persist_hotlist_remove(&dir, &n)?;
        Ok(())
    })
    .await;
    match res {
        Ok(Ok(())) => {
            app.hotlist_apply_removed(name);
            app.message = Some(ta(
                "msg-hotlist-removed",
                &[("name", &detail_for_bar(name))],
            ));
        }
        Ok(Err(e)) => {
            app.message = Some(ta(
                "msg-hotlist-persist-failed",
                &[("error", &io_error_category(&e))],
            ));
        }
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

#[allow(clippy::too_many_arguments)] // wiring del hot-reload, no API
async fn reload_config(
    app: &mut App,
    backend: &Backend,
    resolver: &mut Resolver,
    viewer_resolver: &mut Resolver,
    dialog_resolver: &mut Resolver,
    help_lines: &mut Vec<String>,
    // H3b: the negotiated language, so the rebuilt `TuiChords` answers in the
    // same locale it did at startup. Session-fixed (`norte_i18n::force` runs
    // once), so a `[ui] lang` edited in the file does NOT take effect here —
    // the same restriction the rest of the i18n already has.
    lang: norte_i18n::Lang,
    layers: &Layers,
    cli_preset: Option<&str>,
    quick_mode: &mut nav::Mode,
    confirm_quit: &mut config::ConfirmQuit,
    // S3 (`app.settings`): la snapshot COMPLETA que `run()` retiene para
    // construir/refrescar el overlay de ajustes — reemplazada ENTERA solo
    // si TODO el reload aplicó (mismo criterio que el resto de esta
    // función); un reload fallido deja la config VIGENTE, jamás a medias.
    cfg_out: &mut config::LoadedConfig,
) {
    match config::load_async(layers.clone()).await {
        Ok(cfg) => match build_keymaps(&cfg, cli_preset) {
            Ok((browse, viewer, dialog)) => {
                // El modo del quick search sigue a la config vigente (solo
                // afecta a quick searches NUEVOS; uno abierto conserva el
                // suyo). Mismo criterio que el tema: solo si TODO aplicó.
                *quick_mode = cfg.quick_search_mode;
                // `[ui] confirm_quit` (S2): mismo criterio — solo afecta a
                // `app.quit` NUEVOS (uno ya abierto como `Modal::ConfirmQuit`
                // conserva su decisión hasta que el usuario responda).
                *confirm_quit = cfg.common.ui_confirm_quit;
                // La copia de hotlist también (un popup abierto conserva su
                // snapshot hasta reabrirse — items congelados a propósito).
                app.hotlist.clone_from(&cfg.common.hotlist);
                // Openers (#28): recargados con el resto de la config.
                app.openers = cfg.openers.clone();
                // #108 7a: `[ui.columns]` editado fuera también refresca la
                // sesión (antes solo arrancaba); el re-sort mantiene los
                // panes coherentes con el fichero — el persist del picker
                // dispara este mismo camino y es idempotente con lo ya
                // aplicado en memoria.
                app.columns =
                    norte_frontend::columns::ColumnsSettings::resolve(&cfg.common.ui_columns);
                for i in 0..app.panes.len() {
                    app.apply_scheme_sort(i);
                }
                // Bindings `lua:` descartados del keymap de PROYECTO
                // (seguridad — mismo aviso que en el arranque; máximo
                // porque `global` se fusiona en las tres pantallas, H1 T2
                // suma dialog).
                let discarded_lua = browse
                    .discarded_lua_bindings()
                    .max(viewer.discarded_lua_bindings())
                    .max(dialog.discarded_lua_bindings());
                // La ayuda refleja el keymap VIGENTE: se reconstruye aquí.
                *help_lines = norte_tui::help::build(&browse, &viewer, &dialog);
                // H3b: and so does the resolver the CORPUS is rendered
                // through — same effectives, same moment, before they move
                // into the resolvers below (`TuiChords` borrows). A rebind
                // that reached `help_lines` but not this one would leave the
                // generated keyboard page right and every `{{cmd:…}}` mark in
                // the prose teaching the OLD key.
                app.help_chords = Arc::new(TuiChords::new(&browse, &viewer, &dialog, lang));
                app.help = None;
                // Filas de la palette (H1 T4): reconstruidas del keymap
                // VIGENTE, ANTES de que se mueva al resolver de abajo —
                // mismo criterio que help_lines. La palette abierta se
                // cierra (como la ayuda): sus filas congeladas podrían
                // apuntar a descripciones/chords ya viejos.
                app.palette_rows = norte_tui::palette::build_rows(&browse, &viewer);
                app.palette = None;
                // Hints de los overlays (H1 T3, #24): reconstruidos del
                // efectivo `dialog` VIGENTE, ANTES de que se mueva al
                // resolver de abajo — mismo criterio que help_lines.
                app.dialog_hints = DialogHints::build(&dialog);
                *resolver = Resolver::new(browse);
                *viewer_resolver = Resolver::new(viewer);
                *dialog_resolver = Resolver::new(dialog);
                app.pending.clear();
                app.message = Some(t("msg-config-reloaded"));
                // El tema también es hot-reloadable (ADR 0020): si falla, el
                // mensaje de error del tema pisa el de "config recargada".
                apply_theme(app, &cfg);
                // ÚLTIMO: el aviso de seguridad no debe quedar pisado.
                if discarded_lua > 0 {
                    app.message = Some(ta(
                        "msg-lua-keymap-project",
                        &[("n", &discarded_lua.to_string())],
                    ));
                }
                // S3: el overlay de ajustes, si está abierto, se REFRESCA
                // (no se cierra como `help`/`palette` arriba) — sus filas son
                // solo `(nombre, descripción, valor)` leídas de `cfg`, seguras
                // de recomputar sin tirar el filtro/edición en curso del
                // usuario (`Settings::refresh`).
                if let Some(settings) = &mut app.settings {
                    let summaries = plugin_config_summaries(backend).await;
                    settings.refresh(norte_tui::settings::build_rows(&cfg, &summaries));
                }
                *cfg_out = cfg;
            }
            Err(e) => {
                app.message = Some(ta(
                    "msg-config-not-applied",
                    &[("error", &keymaps_error_category(&e))],
                ));
            }
        },
        Err(e) => {
            app.message = Some(ta(
                "msg-config-not-applied",
                &[("error", &config_error_category(&e))],
            ));
        }
    }
}

/// Tope de la cola FIFO de comandos Lua (M4): con un run en vuelo, los
/// siguientes se encolan hasta aquí; llena, solo queda el aviso.
const LUA_QUEUE_MAX: usize = 8;

/// Directorio de ESTADO del usuario: `$XDG_STATE_HOME/norte` o
/// `~/.local/state/norte` (Windows: `%LOCALAPPDATA%\norte\state`). Sin
/// precedente en el workspace (verificado 2026-07-18: ningún uso de
/// `XDG_STATE_HOME`; la config usa `XDG_CONFIG_HOME` —
/// `config::user_config_dir`): el trust store de Lua es ESTADO local de la
/// máquina, no config que deba viajar con los dotfiles. `None` si el
/// entorno no define nada (CI pelada): el caller degrada con aviso
/// (fail-closed para el TOFU — sin store no corre el script de proyecto).
fn state_dir() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    if cfg!(windows) {
        return std::env::var_os("LOCALAPPDATA")
            .map(|d| PathBuf::from(d).join("norte").join("state"));
    }
    if let Some(xdg) = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(xdg).join("norte"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state/norte"))
}

/// Etiqueta ESTABLE de una capa para `err-lua-load` (no localizada: es un
/// identificador de capa, no prosa).
fn lua_layer_label(layer: Layer) -> &'static str {
    match layer {
        Layer::System => "system",
        Layer::User => "user",
        Layer::Project => "project",
    }
}

/// Evalúa una capa en el host y enruta error/warnings a la barra
/// (`err-lua-load`; con varios, el último gana el hueco — ok v1). El
/// detalle es diagnóstico CRUDO del runtime Lua: SIEMPRE por
/// `detail_for_bar` (patrón #73).
fn eval_lua_layer(app: &mut App, host: &LuaHost, source: &[u8], layer: Layer) {
    let label = lua_layer_label(layer);
    match host.eval_layer(source, layer) {
        Ok(warnings) => {
            for w in warnings {
                app.message = Some(ta(
                    "err-lua-load",
                    &[("layer", label), ("detail", &detail_for_bar(&w.detail))],
                ));
            }
        }
        Err(e) => {
            app.message = Some(ta(
                "err-lua-load",
                &[
                    ("layer", label),
                    ("detail", &detail_for_bar(&e.to_string())),
                ],
            ));
        }
    }
}

/// Lee `path` si existe (`spawn_blocking`, regla 2): `None` = capa ausente.
async fn read_optional_bytes(path: std::path::PathBuf) -> std::io::Result<Option<Vec<u8>>> {
    match tokio::task::spawn_blocking(move || std::fs::read(&path)).await {
        Ok(Ok(bytes)) => Ok(Some(bytes)),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Ok(Err(e)) => Err(e),
        // Un panic leyendo es un bug NUESTRO: que reviente visible (mismo
        // criterio que `config::load_async`).
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

/// Carga los `init.lua` por capas (ADR 0007 + 0026): sistema y usuario se
/// evalúan directo (config PROPIA del usuario); el de PROYECTO (`./.norte`,
/// la ÚLTIMA capa, como en `config::standard_layers`) pasa por el trust
/// TOFU ([`load_lua_project`]). La carga NO toca el `Backend` (solo evalúa
/// código; el FS de los comandos llega en `invoke`). Errores/warnings van a
/// la barra por categoría; una capa rota no impide las demás.
///
/// Devuelve `None` si mlua no pudo ni arrancar: el scripting queda
/// deshabilitado con aviso — el TUI sigue.
///
/// En hot-reload se llama de nuevo y el host RENACE entero (un `CommandRun`
/// en vuelo retiene el estado viejo vía sus handles — documentado en
/// `lua::api`); un `TrustLuaInit` pendiente de la carga anterior queda
/// obsoleto y se cierra (sus bytes ya no son lo que se evaluaría).
async fn load_lua(app: &mut App, layers: &Layers) -> Option<LuaHost> {
    if matches!(app.modal, Some(Modal::TrustLuaInit { .. })) {
        app.modal = None;
        app.open_next_pending();
    }
    app.lua_pending_trust = None;

    let host = match LuaHost::new() {
        Ok(h) => h,
        Err(e) => {
            app.message = Some(ta(
                "err-lua-load",
                &[
                    ("layer", "host"),
                    ("detail", &detail_for_bar(&e.to_string())),
                ],
            ));
            return None;
        }
    };
    for &(ref dir, layer) in &layers.dirs {
        // El kind viaja POR DIR (deuda #75 cerrada): antes se infería por
        // posición y el LABEL fallaba en Windows sin ProgramData (APPDATA
        // quedaba "system").
        if layer == Layer::Project {
            load_lua_project(app, &host, dir.clone()).await;
        } else {
            match read_optional_bytes(dir.join("init.lua")).await {
                Ok(Some(bytes)) => eval_lua_layer(app, &host, &bytes, layer),
                Ok(None) => {}
                Err(e) => {
                    app.message = Some(ta(
                        "err-lua-load",
                        &[
                            ("layer", lua_layer_label(layer)),
                            ("detail", &io_error_category(&e)),
                        ],
                    ));
                }
            }
        }
    }
    Some(host)
}

/// Resultado de la lectura VERIFICADA del `init.lua` de proyecto.
enum ProjectLua {
    /// No hay `./.norte/init.lua` (o `.norte` no es un directorio): nada.
    Absent,
    /// `.norte` o `init.lua` son SYMLINKS (criterio de seguridad de la
    /// review de T6): un symlink a un proyecto ya trusted ejecutaría
    /// contenido aprobado para OTRO sitio en un contexto hostil. No se
    /// carga, con aviso.
    Symlink,
    /// io real (permisos, etc.).
    Io(std::io::Error),
    /// Path CANÓNICO + bytes leídos UNA sola vez.
    Ready(std::path::PathBuf, Vec<u8>),
}

/// Chequeos + lectura del script de proyecto, todo síncrono en un bloque
/// (se llama bajo `spawn_blocking`): `symlink_metadata` verifica que `.norte`
/// es directorio REAL y que `init.lua` es fichero REGULAR — jamás a través
/// de un symlink. La ventana entre check y `read` no es cero (no hay
/// `O_NOFOLLOW` portable aquí), pero el contenido LEÍDO es exactamente lo que
/// se aprueba y evalúa (anti-TOCTOU del contenido; el residual es del path).
/// El path se CANONICALIZA para que check y record usen siempre la misma
/// forma (limitación NFC/NFD del store, documentada en `lua::trust`).
fn project_lua_read(dir: &std::path::Path) -> ProjectLua {
    let dir_md = match std::fs::symlink_metadata(dir) {
        Ok(md) => md,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ProjectLua::Absent,
        Err(e) => return ProjectLua::Io(e),
    };
    if dir_md.file_type().is_symlink() {
        return ProjectLua::Symlink;
    }
    if !dir_md.is_dir() {
        return ProjectLua::Absent;
    }
    let file = dir.join("init.lua");
    let file_md = match std::fs::symlink_metadata(&file) {
        Ok(md) => md,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ProjectLua::Absent,
        Err(e) => return ProjectLua::Io(e),
    };
    if file_md.file_type().is_symlink() {
        return ProjectLua::Symlink;
    }
    if !file_md.is_file() {
        return ProjectLua::Absent;
    }
    let canon = match std::fs::canonicalize(&file) {
        Ok(c) => c,
        Err(e) => return ProjectLua::Io(e),
    };
    match std::fs::read(&file) {
        Ok(bytes) => ProjectLua::Ready(canon, bytes),
        Err(e) => ProjectLua::Io(e),
    }
}

/// Capa de PROYECTO (ADR 0026): verifica symlinks, consulta el
/// [`TrustStore`] (`state_dir()/lua-trust.toml`, `spawn_blocking`) y decide
/// — `Trusted` evalúa; `Denied` CALLA; `DeniedPathChanged` avisa (deny
/// silencioso, JAMÁS modal automático: reabrirlo en cada edición de un
/// script ya rechazado acabaría en aprobación por fatiga); Unknown abre el
/// modal TOFU dejando los bytes pendientes en `App::lua_pending_trust`.
async fn load_lua_project(app: &mut App, host: &LuaHost, dir: std::path::PathBuf) {
    let read = match tokio::task::spawn_blocking(move || project_lua_read(&dir)).await {
        Ok(r) => r,
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    };
    let (path, bytes) = match read {
        ProjectLua::Absent => return,
        ProjectLua::Symlink => {
            app.message = Some(t("msg-lua-symlink"));
            return;
        }
        ProjectLua::Io(e) => {
            app.message = Some(ta(
                "err-lua-load",
                &[("layer", "project"), ("detail", &io_error_category(&e))],
            ));
            return;
        }
        ProjectLua::Ready(path, bytes) => (path, bytes),
    };
    let Some(state) = state_dir() else {
        // Sin dir de estado no hay store; sin store no hay TOFU; sin TOFU el
        // script de proyecto NO corre (fail-closed) — con aviso.
        app.message = Some(t("err-lua-no-state-dir"));
        return;
    };
    let store_path = state.join("lua-trust.toml");
    let (check_path, check_bytes) = (path.clone(), bytes.clone());
    let decision = match tokio::task::spawn_blocking(move || {
        TrustStore::open(store_path).map(|s| s.check(&check_path, &check_bytes))
    })
    .await
    {
        Ok(Ok(d)) => d,
        Ok(Err(e)) => {
            // Store ilegible/corrupto: fail-closed (podría ser el rastro de
            // una manipulación, no una ausencia benigna — `lua::trust`).
            app.message = Some(ta(
                "err-lua-load",
                &[
                    ("layer", "project"),
                    ("detail", &detail_for_bar(&e.to_string())),
                ],
            ));
            return;
        }
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    };
    match decision {
        TrustDecision::Trusted => eval_lua_layer(app, host, &bytes, Layer::Project),
        TrustDecision::Denied => {}
        TrustDecision::DeniedPathChanged => app.message = Some(t("msg-lua-denied-changed")),
        TrustDecision::Unknown => {
            if app.modal.is_some() {
                // Otro modal abierto (solo alcanzable en hot-reload): ni se
                // pisa ni se encola (v1) — el próximo reload re-pregunta.
                return;
            }
            let hash = sha2::Sha256::digest(&bytes);
            // 16 bytes = 32 hex = 128 bits (security review M4 Lua): el
            // humano compara LO QUE VE — forjar una colisión de 32 bits
            // (8 hex) cuesta minutos; 128 bits es imposible en la práctica.
            let hash_abbrev = hash.iter().take(16).fold(String::new(), |mut s, b| {
                use std::fmt::Write as _;
                let _ = write!(s, "{b:02x}");
                s
            });
            app.modal = Some(Modal::TrustLuaInit {
                // Saneado AQUÍ (contrato del modal: `path` ya listo para
                // pintar) — un path de repo ajeno puede traer bidi/control.
                path: detail_for_bar(&path.display().to_string()),
                hash_abbrev,
            });
            app.lua_pending_trust = Some((path, bytes));
        }
    }
}

/// Resuelve el modal [`Modal::TrustLuaInit`] (interceptado en el run loop,
/// que es quien tiene el host — decisión 8 del plan H1: NO migrado al
/// contexto `dialog`): `y` confía, `n`/Esc deniegan, Enter NO decide
/// ([`trust_lua_key`]). La decisión se PERSISTE en el [`TrustStore`]
/// (`spawn_blocking`, regla 2) y, si aprueba, se evalúan los BYTES guardados
/// en `App::lua_pending_trust` — lo aprobado = lo evaluado (anti-TOCTOU),
/// jamás una relectura de disco. Un fallo al persistir no bloquea la
/// decisión de ESTA sesión (solo re-preguntará la próxima): aviso y sigue.
async fn resolve_lua_trust(app: &mut App, host: Option<&LuaHost>, code: KeyCode) {
    if app.modal.is_none() {
        return;
    }
    let allow = match trust_lua_key(code) {
        DialogOutcome::Confirmed => true,
        DialogOutcome::Cancelled => false,
        DialogOutcome::Open | DialogOutcome::Retry(_) => return,
    };
    app.modal = None;
    if let Some((path, bytes)) = app.lua_pending_trust.take() {
        let (rec_path, rec_bytes) = (path.clone(), bytes.clone());
        let record = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let dir = state_dir().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "sin directorio de estado")
            })?;
            let mut store = TrustStore::open(dir.join("lua-trust.toml"))?;
            store.record(&rec_path, &rec_bytes, allow)
        })
        .await;
        match record {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                app.message = Some(ta(
                    "err-lua-load",
                    &[("layer", "project"), ("detail", &io_error_category(&e))],
                ));
            }
            // Un panic al persistir es un bug NUESTRO: que reviente visible
            // (mismo criterio que los demás spawn_blocking de este binario).
            Err(e) => std::panic::resume_unwind(e.into_panic()),
        }
        if allow && let Some(host) = host {
            eval_lua_layer(app, host, &bytes, Layer::Project);
        }
    }
    app.open_next_pending();
}

/// Arranca el comando Lua `name` con el snapshot ACTUAL de panes como
/// `PaneCtx` (congelado: determinismo > frescura). El TUI no tiene
/// multi-selección todavía: `selection` = la entrada bajo el cursor (o
/// vacía) — documentado, mismo dato que `current`. Comando no registrado →
/// barra `err-lua-unknown` (no es error de keymap) y `None`.
fn start_lua_run(
    app: &mut App,
    host: &LuaHost,
    backend: &Backend,
    name: &str,
) -> Option<(CommandRun, CancellationToken)> {
    let pane = app.focused();
    let other = &app.panes[1 - app.focus()];
    let current = pane.selected().map(|e| e.path.clone());
    let ctx = PaneCtx {
        cwd: pane.dir().clone(),
        other_cwd: other.dir().clone(),
        selection: current.clone().into_iter().collect(),
        current,
    };
    let token = CancellationToken::new();
    let Some(run) = host.invoke(name, backend.clone(), ctx, token.clone()) else {
        app.message = Some(ta("err-lua-unknown", &[("name", &detail_for_bar(name))]));
        return None;
    };
    Some((run, token))
}

/// Despacha un binding `lua:<nombre>`: con un run en vuelo lo ENCOLA (FIFO,
/// tope [`LUA_QUEUE_MAX`]; llena = solo el aviso); libre, arranca. Sin host
/// (mlua no arrancó) el comando no puede existir → `err-lua-unknown`.
fn run_lua_command(
    app: &mut App,
    lua_host: Option<&LuaHost>,
    backend: &Backend,
    name: &str,
    lua_run: &mut Option<(CommandRun, CancellationToken)>,
    lua_queue: &mut VecDeque<String>,
) {
    let Some(host) = lua_host else {
        app.message = Some(ta("err-lua-unknown", &[("name", &detail_for_bar(name))]));
        return;
    };
    if lua_run.is_some() {
        if lua_queue.len() < LUA_QUEUE_MAX {
            lua_queue.push_back(name.to_owned());
            app.message = Some(t("msg-lua-busy"));
        } else {
            // Cola llena = DESCARTE: decirlo («encolado» mentiría).
            app.message = Some(t("msg-lua-queue-full"));
        }
        return;
    }
    *lua_run = start_lua_run(app, host, backend, name);
}

/// Recalcula la barra Lua (hook `norte.ui.statusbar`) con el snapshot del
/// pane con foco. El host cachea por `PartialEq` y CONGELA con un run en
/// vuelo (ver `lua::api`); su salida ya viene saneada. Un fallo del hook
/// (take-once) sale una vez por la barra y el hook queda deshabilitado
/// hasta el próximo hot-reload.
fn refresh_lua_status(app: &mut App, lua_host: Option<&LuaHost>) {
    let Some(host) = lua_host else {
        app.lua_status = None;
        return;
    };
    let pane = app.focused();
    let input = StatusInput {
        cwd: pane.dir().to_wire().into_bytes(),
        selected: pane.cursor(),
        // Sin multi-selección: los bytes de la entrada bajo el cursor.
        selected_bytes: pane.selected().and_then(|e| e.size).unwrap_or(0),
        entries: pane.entries().len(),
        tasks: app
            .board
            .rows()
            .iter()
            .filter(|r| !r.last.state.is_terminal())
            .count(),
    };
    app.lua_status = host.statusbar(&input);
    if let Some(detail) = host.statusbar_error() {
        app.message = Some(ta(
            "err-lua-statusbar",
            &[("detail", &detail_for_bar(&detail))],
        ));
    }
}

/// Tick: refresca snapshots del panel y reacciona a las tasks que ACABAN
/// de terminar — colisión con contexto → a la COLA de diálogos (jamás se
/// pisa un modal abierto, hallazgo B1); el resto → mensaje por categoría +
/// refresh de ambos panes (una mutación pudo cambiarlos).
/// (Strings de mensaje hardcodeados hasta Fluent — fase 9, issue #1.)
/// Devuelve qué panes REFRESCÓ (una mutación terminó y `refresh_panes` los
/// reescribió con el listado completo): el run loop aplica entonces el
/// ritual de [`after_panes_refresh`] — un drenador viejo de un pane
/// re-listado duplicaría entradas si siguiera vivo.
async fn on_tick(app: &mut App, backend: &Backend, events: &mut EventStream) -> [bool; 2] {
    let finished = app.board.tick();
    if finished.is_empty() {
        app.open_next_pending();
        return [false; 2];
    }
    let mut refresh = false;
    for fin in finished {
        use norte_proto::TaskState;
        match fin.state {
            TaskState::Completed => {
                refresh = true;
                app.message = Some(t("msg-done"));
            }
            TaskState::Cancelled => {
                refresh = true;
                app.message = Some(t("msg-cancelled"));
            }
            TaskState::Failed { error } => {
                if let (Error::Unsupported, Some(target)) = (&error, &fin.trash_target) {
                    // La papelera no pudo AQUÍ (mount sin topdir…): se
                    // reofrece PERMANENTE con aviso — degradación con
                    // usuario informado (ADR 0009), jamás pisando un modal.
                    if app.modal.is_none() {
                        app.modal = Some(Modal::ConfirmDelete {
                            // Reoferta de ESE ítem, no del lote: el resto
                            // de tasks del lote sigue su curso. Confirmarla
                            // vuelve a pasar por `submit_deletes`, que
                            // CONSUME las marcas — las del lote original ya
                            // se consumieron al enviarlo, así que solo
                            // afectaría a marcas hechas en la ventana entre
                            // el envío y este tick (sin modal abierto).
                            items: vec![target.clone()],
                            permanent: true,
                        });
                    } else {
                        app.message = Some(t("msg-no-trash-here"));
                    }
                } else if let (Error::Conflict { .. }, Some(retry)) = (&error, fin.retry) {
                    app.pending_collisions.push_back(retry);
                } else {
                    // Render por CATEGORÍA localizado (spec §17.7, #20):
                    // jamás el Display inglés ni strings del OS.
                    app.message = Some(error_message(&error));
                    refresh = true;
                }
            }
            _ => {}
        }
    }
    app.open_next_pending();
    if refresh {
        refresh_panes(app, backend, events).await
    } else {
        [false; 2]
    }
}

/// Recarga ambos panes tras una mutación (pueden mostrar el mismo dir).
/// CANCELABLE como el cd (regla 3): Esc abandona el refresh (los panes se
/// quedan como estaban), Ctrl-C sale. El cursor se conserva por ÍNDICE
/// (tras un delete queda en la siguiente entrada — semántica ortodoxa).
/// Devuelve qué panes recibieron DE VERDAD el listado completo (#117
/// review): un Esc a medias abandona el resto — con esto el caller
/// ([`after_panes_refresh`]) decide si suelta el drenador paginado (#78).
async fn refresh_panes(app: &mut App, backend: &Backend, events: &mut EventStream) -> [bool; 2] {
    let mut refreshed = [false; 2];
    for i in 0..app.panes.len() {
        // Un pane en modo virtual de búsqueda (liveSearch T6) NO se
        // auto-refresca: `refresh_listing` lo sacaría del modo virtual y el
        // `reap` cancelaría la Task sin que el usuario saliera (review
        // MINOR-1). Sus hits viven fuera del FS: no hay dir real que recargar.
        if app.panes[i].virtual_search {
            continue;
        }
        let dir = app.panes[i].dir().clone();
        // #117: mismos attrs que un cd a este dir — el refresh no puede
        // dejar las celdas attr en blanco (valores solo si se piden).
        let attrs = app.columns.attr_ids_for(dir.scheme());
        let fut = listing(backend, &dir, &attrs);
        tokio::pin!(fut);
        loop {
            tokio::select! {
                res = &mut fut => {
                    match res {
                        // El listado es COMPLETO: si venía de un cd paginado a
                        // medio rellenar, ya no está cargando (el run loop
                        // suelta el drenador tras este refresh). Un quick
                        // search vivo se re-aplica dentro (índices nuevos).
                        Ok((entries, skipped)) => {
                            app.panes[i].refresh_listing(entries);
                            // #96: el refresh trae las omitidas FRESCAS — sin
                            // esto, el badge conservaba el valor del listado
                            // anterior (rancio) tras una mutación.
                            app.panes[i].set_skipped(skipped);
                            refreshed[i] = true;
                        }
                        // Sin silencio: el dir pudo desaparecer (issue #20).
                        Err(e) => app.message = Some(ta("msg-refresh-error", &[("error", &error_category(&e))])),
                    }
                    break;
                }
                maybe = events.next() => {
                    match maybe {
                        Some(Ok(Event::Key(key)))
                            if key.kind == crossterm::event::KeyEventKind::Press =>
                        {
                            match (key.code, key.modifiers) {
                                (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                                    app.quit = true;
                                    return refreshed;
                                }
                                (KeyCode::Esc, _) => return refreshed,
                                _ => {}
                            }
                        }
                        Some(Ok(_)) => {}
                        Some(Err(_)) | None => return refreshed,
                    }
                }
            }
        }
    }
    refreshed
}

/// El ritual tras un [`refresh_panes`], ÚNICO para sus tres disparadores
/// (mutación terminada en `on_tick`, confirm del picker y hot-reload de
/// `[ui.columns]` — #117 review): el drenador paginado se suelta SOLO si su
/// pane fue re-listado de verdad (soltarlo a ciegas tras un Esc a medias
/// dejaría el pane colgado en `loading` para siempre, #78 — su relleno
/// sigue siendo válido); la dedup de la sonda #52 se invalida (un listado
/// nuevo re-lazifica las entries y un re-probe de la MISMA selección es
/// legítimo, MAJOR-1); y el run de búsqueda se cosecha ([`reap_search_run`]
/// ya es no-op si su pane sigue en modo virtual).
///
/// Y, si la ayuda está abierta, sus hechos se RECONGELAN (review MAJOR-2). El
/// congelado existe para que un veredicto no cambie porque el lector se mueva
/// por la página; no para sobrevivir a que el listado que describe deje de
/// existir. `enterable` y `viewable` hablan de la entrada bajo el cursor, y
/// este es el embudo por el que pasan los TRES disparadores del refresh — el
/// del `tick` incluido, que no tiene guarda de overlay, así que una copia o un
/// borrado terminan re-listando los panes con la ayuda delante. Recongelar
/// aquí conserva «ningún veredicto cambia porque el lector se desplace» y
/// tira «ningún veredicto cambia porque el mundo cambie».
fn after_panes_refresh(
    app: &mut App,
    refreshed: [bool; 2],
    fill: &mut [Option<Fill>; 2],
    last_probed: &mut Probed,
    search_run: &mut Option<SearchRun>,
) {
    if refreshed == [false; 2] {
        return;
    }
    release_refreshed_fill(refreshed, fill, last_probed);
    reap_search_run(app, search_run);
    if app.help.is_some() {
        app.freeze_help_facts();
    }
}

/// Confiar en la host key y REINTENTAR la navegación que el TOFU interrumpió
/// (#45). `Some(cd)` = el desenlace debe volver YA al caller (la pendiente
/// siguiente ya se gestionó aquí); `None` = confiar falló y el mensaje quedó
/// en la barra — el caller sigue por su camino común.
///
/// Vive fuera de [`on_dialog_key`] porque el brazo entero (destructurar el
/// modal + el `trust_host_key` + el reintento) no cabe en el presupuesto de
/// líneas de esa función.
async fn trust_host_retry(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    modal: Modal,
) -> Option<Cd> {
    let Modal::TrustHostKey {
        host,
        port,
        algo,
        fingerprint,
        dir,
        pane,
        trail,
    } = modal
    else {
        // El caller solo llama con este modal (brazo `Modal::TrustHostKey`).
        return None;
    };
    match backend
        .trust_host_key(&host, port, &algo, &fingerprint)
        .await
    {
        Ok(()) => {
            // El engine re-verifica el fingerprint contra la clave que el
            // host presenta AHORA (anti-TOCTOU, ADR 0015 D); si aún falla,
            // el retry lo mostrará.
            //
            // `cd_in` (no `cd`): se reanuda la navegación que el TOFU
            // interrumpió — su pane y su rastro —, que no tiene por qué ser
            // la del foco actual.
            let outcome = cd_in(app, backend, events, pane, dir.clone(), trail).await;
            // Y si era un paso del rastro, ESTE es el sitio donde se termina:
            // `walk_trail` lo dejó dado porque contaba con este reintento.
            settle_suspended_trail(app, pane, &dir, trail, &outcome);
            // Solo abrir la siguiente pendiente si el retry NO dejó un modal
            // (otro HostKeyUnknown): jamás pisar.
            if app.modal.is_none() {
                app.open_next_pending();
            }
            Some(outcome)
        }
        Err(e) => {
            app.message = Some(error_message(&e));
            // Confiar FALLÓ: no hay reintento, así que la navegación que el
            // TOFU suspendió muere aquí — para el rastro es idéntica a un cd
            // abandonado, y el paso tiene que volver.
            settle_suspended_trail(app, pane, &dir, trail, &Cd::Cancelled);
            None
        }
    }
}

/// Teclas de un modal abierto, resueltas contra el contexto `dialog` del
/// keymap (H1 T2, issue #24 CERRADO — rebindeable) y filtradas por el
/// ALLOWLIST del modal concreto ([`dialog_action`]): la semántica de
/// seguridad vive en código, solo la ASIGNACIÓN tecla→comando es keymap.
/// `Modal::TrustLuaInit` nunca llega aquí (interceptado antes en el run
/// loop, decisión 8). `events` es para el reintento de navegación del modal
/// TOFU (#45): confiar en la host key relanza el `cd`, que tiene su propio
/// loop de eventos.
#[allow(clippy::too_many_arguments)] // wiring del run loop, no API
async fn on_dialog_key(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
    lang: norte_help::Lang,
    help_lines: &[String],
) -> Cd {
    let Some(modal) = app.modal.clone() else {
        return Cd::Cancelled;
    };
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return Cd::Cancelled; // tecla no modelada por el keymap: ignorar
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run(cmd) => cmd,
        // Sin semántica de secuencia definida para overlays (T2): ignorar y
        // reiniciar el estado de resolución.
        Resolution::Pending(_) => {
            resolver.reset();
            return Cd::Cancelled;
        }
        Resolution::Reset => return Cd::Cancelled,
    };
    // H3c: ANTES del allowlist, que dejaría caer `app.help` — no es un verbo
    // `dialog.*`. La ayuda se abre sobre el modal y se queda las teclas
    // (`help_owns_keys`); el modal sigue intacto detrás.
    if modal_help_toggle(app, cmd.as_str(), lang, help_lines) {
        return Cd::Cancelled;
    }
    if modal_scroll(app, cmd.as_str()) {
        return Cd::Cancelled;
    }
    let Some(outcome) = dialog_action(&modal, &cmd) else {
        return Cd::Cancelled; // comando fuera del allowlist de ESTE modal
    };
    match outcome {
        DialogOutcome::Open => {} // dialog_action nunca lo devuelve: defensivo
        DialogOutcome::Cancelled => {
            app.modal = None;
            app.open_next_pending();
            match modal {
                // Cerrar el diálogo de aprobación ES denegar (fail-safe): el
                // agente recibe `not-approved`, jamás una espera colgada.
                Modal::ApproveAgentOp { req } => {
                    decide_approval(app, backend, req.approval_id, false).await;
                }
                // DENEGAR la host key abandona la navegación que el TOFU
                // suspendió: no hay reintento que la termine, así que el paso
                // del rastro que `walk_trail` dejó dado vuelve aquí. Es el
                // camino MÁS probable de los tres (decir que no a un host
                // desconocido es lo normal), y el único que no pasa por
                // `trust_host_retry`.
                Modal::TrustHostKey {
                    dir, pane, trail, ..
                } => settle_suspended_trail(app, pane, &dir, trail, &Cd::Cancelled),
                _ => {}
            }
        }
        DialogOutcome::Confirmed => {
            app.modal = None;
            // OJO (MAJOR del rust-reviewer): NO abrir la siguiente pendiente
            // ANTES del match — el retry TOFU (`return cd`) puede reabrir un
            // TrustHostKey y PISAR una aprobación de agente ya sacada de la
            // cola (quedaría huérfana hasta su TTL). Se difiere al final.
            match modal {
                Modal::ConfirmDelete { items, permanent } => {
                    submit_deletes(app, backend, &items, permanent).await;
                }
                Modal::ConfirmTransfer { kind, items, to } => {
                    submit_transfers(app, backend, kind, &items, &to, TransferOptions::default())
                        .await;
                }
                // TrustLuaInit se intercepta ANTES en el run loop (necesita
                // el LuaHost); MarkPattern (#103 T9) también, como texto
                // libre (mismo motivo que la búsqueda) — `dialog_action`
                // devuelve `None` para ambos, así que `on_dialog_key` ya
                // habría retornado antes de llegar a este match: inalcanzable
                // aquí, no-op defensivo.
                Modal::Collision { .. }
                | Modal::TrustLuaInit { .. }
                | Modal::MarkPattern { .. }
                | Modal::Mkdir { .. }
                | Modal::AiRenameInstruction { .. }
                | Modal::SemanticQuery { .. }
                | Modal::TransferName { .. } => {}
                // `AiRenamePlan` (M4-IA) SÍ es una superficie de decisión:
                // confirmar aplica el plan REVISADO — N fs.move gobernados
                // (journal + policy), en el orden del plan (molde CLI). Se
                // aplican TODAS las parejas, no solo la ventana visible: el
                // scroll (audit MAJOR-3) hace revisable el plan entero.
                Modal::AiRenamePlan { dir, entries, .. } => {
                    apply_ai_rename(app, backend, &dir, &entries).await;
                }
                // M4-IA-2: confirmar NAVEGA al hit bajo el cursor
                // (`semantic_hit_cd`). El `Cd` vuelve al caller (apply_cd +
                // decorate), como el retry TOFU; si el cd abrió un modal
                // (otro HostKeyUnknown), la siguiente pendiente espera —
                // jamás pisar.
                Modal::SemanticHits { hits, cursor, .. } => {
                    let outcome = semantic_hit_cd(app, backend, events, &hits, cursor).await;
                    if app.modal.is_none() {
                        app.open_next_pending();
                    }
                    return outcome;
                }
                // S2 (`[ui] confirm_quit`): confirmar cierra — el run loop
                // lo detecta en su chequeo de `app.quit` de cada vuelta
                // (main.rs, tope del `loop`).
                Modal::ConfirmQuit => app.quit = true,
                Modal::ApproveAgentOp { req } => {
                    decide_approval(app, backend, req.approval_id, true).await;
                }
                // TOFU (#45): confía en la host key y REINTENTA la navegación.
                m @ Modal::TrustHostKey { .. } => {
                    if let Some(outcome) = trust_host_retry(app, backend, events, m).await {
                        return outcome;
                    }
                }
            }
            // Todas las ramas salvo el retry TOFU (que ya volvió) abren aquí
            // la siguiente pendiente, con el modal ya cerrado.
            app.open_next_pending();
        }
        DialogOutcome::Retry(policy) => {
            app.modal = None;
            if let Modal::Collision { retry } = modal {
                // Conserva las opciones ORIGINALES; solo cambia la política.
                let opts = TransferOptions {
                    on_collision: policy,
                    ..retry.opts
                };
                submit_transfer(app, backend, retry.kind, retry.from, retry.to, opts).await;
            }
            app.open_next_pending();
        }
    }
    // Salvo el retry TOFU (que hace `return cd(...)`), un modal no navega.
    Cd::Cancelled
}

/// Resuelve una aprobación de policy (`policy.decide`, M3-3b T5). Un error
/// (id ya vencido/decidido por otro frontend, daemon caído) sale por la
/// barra: la pendiente, si sigue viva, vencerá por TTL — jamás se cuelga.
async fn decide_approval(app: &mut App, backend: &Backend, approval_id: u64, approve: bool) {
    if let Err(e) = backend.policy_decide(approval_id, approve).await {
        app.message = Some(error_message(&e));
    }
}

/// Los pares `(origen, destino)` de un lote: cada ítem aterriza en el
/// DIRECTORIO `to` con SU MISMO nombre — el nombre son BYTES (`Segment`,
/// regla 1), jamás texto, así que un `Папка` o un `\xff` viaja intacto. Un
/// ítem sin nombre (la raíz de un scheme) no es transferible y se descarta:
/// no hay nada que colgar del destino.
///
/// PURA a propósito: el lote entero se ve sin levantar backend.
fn transfer_dests(items: &[VPath], to: &VPath) -> Vec<(VPath, VPath)> {
    items
        .iter()
        .filter_map(|from| {
            let name = from.file_name()?.clone();
            Some((from.clone(), to.join(name)))
        })
        .collect()
}

/// Envía el lote de copia/movimiento: UNA task POR ÍTEM (#103 T10), cada una
/// con su progreso, su cancelación y sus entradas de journal propias —
/// cancelar una no toca a las demás.
///
/// Un fallo NO aborta el lote: los ítems restantes se envían igual y el
/// último error queda en la barra. Abandonar 4..n porque el 3 falló dejaría
/// media selección hecha sin decirlo; el panel de tasks muestra el resultado
/// de cada una por separado. Las colisiones no viajan por aquí: llegan
/// ASÍNCRONAS al terminar la task y `on_tick` las ENCOLA
/// (`pending_collisions`) para no pisar jamás un modal abierto.
///
/// Las marcas se consumen al ENVIAR el lote, no al completarse.
async fn submit_transfers(
    app: &mut App,
    backend: &Backend,
    kind: TransferKind,
    items: &[VPath],
    to: &VPath,
    opts: TransferOptions,
) {
    for (from, dest) in transfer_dests(items, to) {
        let _submitted = submit_transfer(app, backend, kind, from, dest, opts).await;
    }
    app.consume_marks();
}

/// Envía el lote de borrado: UNA task POR ÍTEM, mismo criterio que
/// [`submit_transfers`] (un fallo no abandona el resto). El objetivo de
/// papelera viaja con cada task para que un `Unsupported` reofrezca el
/// PERMANENTE de ESE ítem (ADR 0009), no del lote entero.
async fn submit_deletes(app: &mut App, backend: &Backend, items: &[VPath], permanent: bool) {
    let del_mode = if permanent {
        DeleteMode::Permanent
    } else {
        DeleteMode::Trash
    };
    for target in items {
        match backend.delete(target, del_mode).await {
            Ok(task) => {
                app.board
                    .push_full(task, None, (!permanent).then(|| target.clone()));
            }
            Err(e) => app.message = Some(error_message(&e)),
        }
    }
    app.consume_marks();
}

/// Encola una transferencia y la registra en el panel con su contexto de
/// reintento (para el diálogo de colisión).
/// Devuelve `true` si la task ENCOLÓ (#105: el modal de nombre editable
/// solo se cierra entonces); un fallo deja el error en la barra.
async fn submit_transfer(
    app: &mut App,
    backend: &Backend,
    kind: TransferKind,
    from: VPath,
    to: VPath,
    opts: TransferOptions,
) -> bool {
    let res = match kind {
        TransferKind::Copy => backend.copy(&from, &to, opts).await,
        TransferKind::Move => backend.move_(&from, &to, opts).await,
    };
    match res {
        Ok(task) => {
            // #98/M1: el enc del pane origen viaja con el retry — la
            // colisión llega async y el foco puede haber cambiado.
            let name_encoding = app.focused().name_encoding();
            app.board.push(
                task,
                Some(RetrySpec {
                    kind,
                    from,
                    to,
                    opts,
                    name_encoding,
                }),
            );
            true
        }
        Err(e) => {
            app.message = Some(error_message(&e));
            false
        }
    }
}

/// Aplica un plan de rename IA CONFIRMADO (M4-IA): un `fs.move` gobernado
/// (journal + policy + colisiones del engine) por pareja, en el ORDEN del
/// plan (molde CLI). PRE-valida el plan entero
/// ([`norte_frontend::validate_ai_plan`], cinturón COMPARTIDO con la GUI —
/// quality review 78eb243 MAJOR-1 — audit
/// MAJOR-2): una pareja inválida = plan adulterado → NADA se encola y la
/// barra lo dice. El primer fallo de SUBMIT para el lote — lo ya encolado
/// sigue en el board — y deja el detalle en la barra; si todo encoló, la
/// barra resume cuántos moves salieron.
async fn apply_ai_rename(
    app: &mut App,
    backend: &Backend,
    dir: &VPath,
    entries: &[norte_proto::methods::AiRenameEntry],
) {
    let Some(pairs) = norte_frontend::validate_ai_plan(entries) else {
        app.message = Some(t("msg-ai-rename-invalid-plan"));
        return;
    };
    let mut n = 0usize;
    for (from, to) in pairs {
        match backend
            .move_(&dir.join(from), &dir.join(to), TransferOptions::default())
            .await
        {
            Ok(task) => {
                app.board.push(task, None);
                n += 1;
            }
            Err(e) => {
                app.message = Some(ta(
                    "msg-ai-rename-failed",
                    &[("error", &detail_for_bar(&error_category(&e)))],
                ));
                // Primer fallo para; el detalle del fallo NO se pisa con el
                // resumen de éxito (desviación consciente del molde: la
                // barra es una línea y el fallo es lo accionable).
                return;
            }
        }
    }
    if n > 0 {
        app.message = Some(ta("msg-ai-rename-applied", &[("n", &n.to_string())]));
    }
}

#[cfg(test)]
mod bulk_tests {
    use super::transfer_dests;
    use norte_proto::VPath;

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire válido")
    }

    /// #103 T10: el lote se envía ENTERO — un par por ítem, cada uno con SU
    /// nombre colgado del directorio destino. (Mutación de control: hacer
    /// que el envío use solo el primer ítem rompe este test.)
    #[test]
    fn a_bulk_transfer_submits_every_item_not_just_the_first() {
        let items = vec![vp("mem:///src/a"), vp("mem:///src/b"), vp("mem:///src/c")];
        let pares = transfer_dests(&items, &vp("mem:///dst"));
        assert_eq!(pares.len(), 3, "una task POR ítem");
        assert_eq!(
            pares.iter().map(|(_, d)| d.clone()).collect::<Vec<_>>(),
            vec![vp("mem:///dst/a"), vp("mem:///dst/b"), vp("mem:///dst/c")],
        );
    }

    /// Regla 1: el nombre son BYTES. Un nombre no-UTF8 llega al destino
    /// byte a byte — el destino jamás se construye desde el texto pintado.
    #[test]
    fn a_bulk_transfer_keeps_non_utf8_names_byte_exact() {
        let raw = b"caf\xff\xfe.txt".to_vec();
        let seg = norte_proto::Segment::new(raw.clone()).expect("segmento");
        let from = vp("mem:///src").join(seg);
        let pares = transfer_dests(std::slice::from_ref(&from), &vp("mem:///dst"));
        assert_eq!(pares.len(), 1);
        assert_eq!(
            pares[0].1.file_name().map(|s| s.as_bytes().to_vec()),
            Some(raw),
            "los bytes del nombre viajan intactos al destino",
        );
    }

    /// El destino IGUAL que el origen (mismo dir en ambos panes) rinde un
    /// par `from == to`: la decisión de qué hacer con eso es del engine
    /// (colisión), no del frontend — que no debe inventarse un descarte.
    #[test]
    fn a_same_directory_transfer_maps_each_item_onto_itself() {
        let items = vec![vp("mem:///src/a")];
        let pares = transfer_dests(&items, &vp("mem:///src"));
        assert_eq!(pares[0].0, pares[0].1);
    }

    /// Una raíz de scheme no tiene nombre que colgar del destino: se
    /// descarta en vez de fabricar una ruta.
    #[test]
    fn a_rootless_item_is_dropped_from_the_batch() {
        let root = VPath::root(norte_proto::Scheme::new("mem").unwrap(), None);
        assert!(transfer_dests(&[root], &vp("mem:///dst")).is_empty());
    }
}

/// Traduce una tecla del diálogo de búsqueda (`Alt+F7`, liveSearch T6) a un
/// efecto sobre `App::search_dialog`. Teclas fijas como los demás overlays
/// (#24); `ctrl+c` conserva su salida global. Devuelve `Some(params)` SOLO
/// cuando Enter con algún criterio no vacío debe LANZAR la búsqueda (el caller
/// cierra el diálogo y abre el pane virtual); Enter sin criterio avisa y sigue.
fn on_search_dialog_key(
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
fn search_params(dialog: &SearchDialog, root: VPath) -> FsSearchParams {
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
async fn launch_search(
    app: &mut App,
    backend: &Backend,
    fill: &mut [Option<Fill>; 2],
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
            fill[pane] = None;
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
fn drain_search(app: &mut App, search_run: &mut Option<SearchRun>, hits: Option<SearchHits>) {
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
fn finalize_search_state(s: &SearchRun) -> SearchState {
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
async fn on_search_escape(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    fill: &mut [Option<Fill>; 2],
    decorate_fetch: &mut [Option<DecorateFetch>; 2],
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
    apply_cd(fill, decorate_fetch, last_probed, search_run, outcome);
}

/// Enter sobre un hit del pane virtual (liveSearch T6): cd al PADRE del hit y
/// deja el cursor sobre él (por path, si ya está en la primera página).
/// Cancela la Task si sigue viva y sale del modo virtual.
async fn on_search_enter(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    fill: &mut [Option<Fill>; 2],
    decorate_fetch: &mut [Option<DecorateFetch>; 2],
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
    apply_cd(fill, decorate_fetch, last_probed, search_run, outcome);
    // Re-ancla el cursor sobre el hit por path (el cd resetea a 0); si cayó
    // en una página aún no drenada, el cursor se queda arriba (v1).
    if let Some(i) = app.panes[pane].entries().iter().position(|e| e.path == hit) {
        app.panes[pane].set_cursor(i);
    }
}

/// up/down sobre los modales con ventana propia — el scroll del plan IA
/// (M4-IA, audit MAJOR-3) y el cursor de los hits semánticos (M4-IA-2).
/// Mueven la VENTANA o el CURSOR y JAMÁS confirman/cancelan: mismo par de
/// comandos que los pickers (`ALLOW_PICKER`); para `dialog_action` up/down
/// están FUERA del allowlist de decisión de estos modales (devuelve `None`,
/// pin en tests/modal.rs), así que el enrutado vive aquí, como el dispatch
/// de los pickers vive en su `on_*_key`. `true` = comando CONSUMIDO.
/// `F1` (o lo que el keymap ate a `app.help`) SOBRE un modal abierto: abre la
/// ayuda del contexto de ESE modal. `true` = comando CONSUMIDO.
///
/// Vive aquí por lo mismo que [`modal_scroll`]: `app.help` es un comando de
/// `[global]`, no un verbo `dialog.*`, así que el allowlist del modal concreto
/// ([`dialog_action`]) lo deja caer — y sin esta rama la única tecla que el
/// lector tiene garantizada sería inerte justo donde más falta hace, delante de
/// una pregunta que no entiende. Es el gemelo del interruptor de
/// [`on_help_key`]: la misma tecla que abre la ayuda la cierra, y lo
/// hardcodeado es el SIGNIFICADO, jamás la tecla.
///
/// Qué modales lo admiten es una DECISIÓN, no la resaca del enrutado: lo dice
/// [`norte_tui::help_context::help_over_modal_allowed`], exhaustivo sobre
/// `Modal` y sin comodín (review MINOR-3). Los cinco editores de TEXTO LIBRE
/// (`Mkdir`, `MarkPattern`, `AiRenameInstruction`, `SemanticQuery`,
/// `TransferName`) y el TOFU de Lua responden `false`: hoy tampoco llegan aquí
/// —el run loop los intercepta antes para leer teclas CRUDAS (decisión 8 del
/// plan H1: el keymap no puede reinterpretar lo que se está escribiendo)—, y
/// preguntarlo AQUÍ es lo que impide que mover uno de ellos al keymap `dialog`
/// abra el agujero en silencio. Sus contextos existen en el vocabulario y sus
/// páginas se alcanzan por el índice.
fn modal_help_toggle(
    app: &mut App,
    cmd: &str,
    lang: norte_help::Lang,
    help_lines: &[String],
) -> bool {
    if cmd != "app.help" {
        return false;
    }
    // No consumir la tecla cuando la ayuda no puede abrirse: quien decide
    // vuelve a ser el allowlist del modal (`dialog_action`), que deja caer
    // `app.help` — la tecla queda INERTE, que es lo que se quiere.
    if app
        .modal
        .as_ref()
        .is_some_and(|m| !norte_tui::help_context::help_over_modal_allowed(m))
    {
        return false;
    }
    // Sin foto de plugins (H3e): esta rama es SÍNCRONA — la cadena de teclas
    // del modal lo es — y pedirla cuesta una ida y vuelta al daemon. La
    // degradación es exactamente la documentada para `plugins: None`: la ayuda
    // que se abre sobre un diálogo no ofrece filas de extensión. Es la
    // superficie donde menos se echa en falta: el lector está contestando una
    // pregunta, no explorando el catálogo, y el grupo entero está a un `Esc` y
    // un F1 de distancia.
    open_contextual_help(app, lang, help_lines, None);
    true
}

fn modal_scroll(app: &mut App, cmd: &str) -> bool {
    if !matches!(cmd, "dialog.up" | "dialog.down") {
        return false;
    }
    let down = cmd == "dialog.down";
    match app.modal {
        Some(Modal::AiRenamePlan { .. }) => {
            app.ai_plan_scroll(down);
            true
        }
        Some(Modal::SemanticHits { .. }) => {
            app.semantic_cursor(down);
            true
        }
        _ => false,
    }
}

/// Enter sobre un hit del modal semántico (M4-IA-2): cd al PADRE del hit y
/// deja el cursor sobre él por path (molde [`on_search_enter`]; si cayó en
/// una página aún no drenada, el cursor se queda arriba, v1). Devuelve el
/// `Cd` para que el caller lo aplique (`apply_cd` + decorate);
/// `Cd::Cancelled` = nada que navegar (hits vacíos defensivo o hit raíz sin
/// padre).
async fn semantic_hit_cd(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    hits: &[norte_proto::methods::SemanticHit],
    cursor: usize,
) -> Cd {
    let Some(hit) = hits.get(cursor).map(|h| h.path.clone()) else {
        return Cd::Cancelled;
    };
    let Some(parent) = hit.parent() else {
        return Cd::Cancelled;
    };
    let pane = app.focus();
    let outcome = cd(app, backend, events, parent).await;
    if let Some(i) = app.panes[pane].entries().iter().position(|e| e.path == hit) {
        app.panes[pane].set_cursor(i);
    }
    outcome
}

/// Suelta el [`SearchRun`] si su pane SALIÓ del modo virtual (un `cd`/refresh
/// lo apagó): su drenador alimentaría un listado real. Cancela la Task si
/// sigue viva (regla 3).
fn reap_search_run(app: &App, search_run: &mut Option<SearchRun>) {
    if let Some(s) = search_run.as_ref()
        && !app.panes[s.pane].virtual_search
    {
        s.task.cancel();
        *search_run = None;
    }
}

/// Resuelve el opener (#28) del fichero seleccionado y deja en
/// `app.pending_open` lo que el run loop —dueño de la terminal— lanzará.
/// Primero manda `ns.toml`; sin regla para ese mimetype queda el lanzador
/// del escritorio, que es lo que hace que F4 funcione sin haber escrito
/// configuración. Cada fallo va a la barra —degradación limpia, jamás un
/// lanzamiento a ciegas—: sin fichero (no-op), remoto o dentro de un archivo
/// (`msg-open-remote`), o binario ausente (`msg-open-missing-program`, que
/// también cubre un Linux sin `xdg-utils`).
fn resolve_opener(app: &mut App) {
    use norte_frontend::openers;
    let Some(path) = app
        .focused()
        .selected()
        .filter(|e| matches!(e.kind, EntryKind::File | EntryKind::Symlink))
        .map(|e| e.path.clone())
    else {
        return;
    };
    // Ruta nativa: SOLO `file://` local; archive/sftp/s3 → sin path nativo.
    let Ok(native) = norte_vfs_local::vpath_to_native(&path) else {
        app.message = Some(t("msg-open-remote"));
        return;
    };
    let mime = openers::guess_mime(
        path.file_name()
            .map_or(&[][..], norte_proto::Segment::as_bytes),
    );
    let Some(opener) = app.openers.resolve(mime) else {
        // Sin regla en `ns.toml` para este mimetype queda el último recurso:
        // el lanzador del propio escritorio. Antes esto era un mensaje de
        // error, lo que obligaba a escribir configuración para abrir un PDF.
        // Va `detached` — entrega el fichero al programa asociado y vuelve,
        // así que suspender la TUI solo pintaría un parpadeo.
        let (program, argv) = openers::system_opener(&native);
        app.pending_open = Some(norte_tui::app::PendingOpen {
            program,
            argv,
            detached: true,
        });
        return;
    };
    let program = opener.program().to_owned();
    // La sonda del binario en el PATH (`program_available`) es I/O de disco:
    // NO se hace aquí (camino async) — el run loop la corre en spawn_blocking
    // junto al lanzamiento (regla 2).
    // `%d` = el directorio del pane (nativo); si por lo que sea no convierte,
    // el padre del propio fichero.
    let dir = norte_vfs_local::vpath_to_native(app.focused().dir()).unwrap_or_else(|_| {
        native
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default()
    });
    app.pending_open = Some(norte_tui::app::PendingOpen {
        program,
        argv: opener.argv(&[&native], &dir),
        detached: false,
    });
}

/// Sondea el binario del opener en el PATH (#28) — I/O de disco en
/// `spawn_blocking`, JAMÁS en el executor async (regla 2) — y, si existe,
/// suspende el TUI y lo lanza. Devuelve el mensaje de barra LOCALIZADO del
/// resultado (binario ausente / lanzado / fallo de spawn).
async fn launch_opener(
    terminal: &mut ratatui::DefaultTerminal,
    // Suspender la TUI cede la terminal ENTERA: la captura de ratón se
    // suelta antes y se restituye después ([`run_opener`]).
    capture: &mut mouse::Capture,
    pending: norte_tui::app::PendingOpen,
) -> String {
    let norte_tui::app::PendingOpen {
        program,
        argv,
        detached,
    } = pending;
    let prog = program.clone();
    let available =
        tokio::task::spawn_blocking(move || norte_frontend::openers::program_available(&prog))
            .await
            .unwrap_or(false);
    if !available {
        return ta("msg-open-missing-program", &[("program", &program)]);
    }
    if detached {
        return match spawn_detached(argv).await {
            Ok(()) => ta("msg-open-launched", &[("program", &program)]),
            Err(e) => ta(
                "msg-open-failed",
                &[("program", &program), ("error", &e.to_string())],
            ),
        };
    }
    match run_opener(terminal, capture, argv).await {
        Ok(_) => ta("msg-open-launched", &[("program", &program)]),
        Err(e) => ta(
            "msg-open-failed",
            &[("program", &program), ("error", &e.to_string())],
        ),
    }
}

/// Lanza el comando SIN tocar la terminal: el lanzador del escritorio
/// (`xdg-open`/`open`/`explorer.exe`) entrega el fichero al programa asociado
/// y termina, así que suspender la TUI para él sería un parpadeo gratis. El
/// stdio va a `null` — un lanzador hablador no puede escribir encima del
/// listado. El hijo se espera en segundo plano (regla 2: en `spawn_blocking`),
/// que es lo que lo entierra: sin ese `wait` quedaría zombi hasta que muriese
/// el propio norte.
async fn spawn_detached(argv: Vec<std::ffi::OsString>) -> std::io::Result<()> {
    debug_assert!(
        !argv.is_empty(),
        "el argv del lanzador del sistema siempre trae el binario"
    );
    let mut child = tokio::task::spawn_blocking(move || {
        std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
    })
    .await
    .map_err(std::io::Error::other)??;
    tokio::task::spawn_blocking(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Suspende el TUI (sale de la pantalla alternativa + raw mode), lanza el
/// comando externo con stdio HEREDADO (el usuario interactúa con `bat`/editor
/// directamente) y espera su fin en `spawn_blocking` (regla 2). La terminal se
/// restaura SIEMPRE una vez que se dejó el modo TUI — falle el hijo, se rompa
/// el join o falle una syscall intermedia — para no dejarla en raw-off. #28.
async fn run_opener(
    terminal: &mut ratatui::DefaultTerminal,
    capture: &mut mouse::Capture,
    argv: Vec<std::ffi::OsString>,
) -> std::io::Result<std::process::ExitStatus> {
    use crossterm::terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    };
    // Invariante: `Opener::argv` siempre empuja el binario (`command[0]`), y
    // `parse` rechaza `command` vacío — así `argv[0]` nunca panica.
    debug_assert!(
        !argv.is_empty(),
        "el argv de un opener siempre trae el binario"
    );
    // La captura de ratón se suelta ANTES de nada: el programa que viene
    // detrás no la pidió, y heredarla le mete cada movimiento del puntero
    // por stdin como si fueran teclas. Escritura síncrona a stdout, misma
    // exención puntual de la regla 2 que el resto de esta función (que ya
    // maneja la pantalla alternativa y el raw mode igual).
    let raton = mouse::release_for_suspend(capture, &mut std::io::stdout())?;
    disable_raw_mode()?;
    // A partir de aquí la terminal está fuera del modo TUI: restaurar pase lo
    // que pase antes de devolver.
    crossterm::execute!(std::io::stdout(), LeaveAlternateScreen)?;
    let child = tokio::task::spawn_blocking(move || {
        std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .status()
    })
    .await;
    let mut restore = || -> std::io::Result<()> {
        crossterm::execute!(std::io::stdout(), EnterAlternateScreen)?;
        enable_raw_mode()?;
        // Y se restituye exactamente como estaba: si el usuario la tenía
        // apagada (`[ui] mouse = false`), volver del opener no se la
        // enciende.
        mouse::restore_after_suspend(capture, raton, &mut std::io::stdout())?;
        terminal.clear()
    };
    let restored = restore();
    // El resultado del hijo prima; si además falló la restauración, se propaga.
    let status = child.map_err(std::io::Error::other)?;
    restored?;
    status
}

/// Where a pane gesture wants to send a pane.
///
/// The `*_plan` functions return this instead of navigating so the DECISION —
/// which pane travels, and where — is testable without a `Backend` or an
/// event stream. `None` from them means there is nothing to do, and WHY is
/// deliberately not this type's business: the dispatch arm decides whether
/// the reason deserves a message.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PaneMove {
    /// The pane that will navigate.
    pane: usize,
    /// Where it will go.
    dir: VPath,
}

/// `pane.mirror`: the UNFOCUSED pane goes where the focused one is, and the
/// focus stays put — the fastest way to line up a copy, because the
/// destination of `pane.copy` is whatever the other pane holds.
///
/// `None` when the focused pane is a virtual search listing (a list of hits
/// is not a location, so there is no origin to send) or when both panes are
/// already there — a redundant `cd` would re-list the other pane and slide
/// its listing out from under the reader's cursor for nothing.
///
/// "Already there" reads `virtual_search` as well as the directory: a results
/// pane's `dir()` is the ROOT the search walked, which is usually the very
/// directory the other pane is sitting in, and it is NOT what the reader is
/// looking at. Comparing the two alone refused the gesture in silence
/// precisely when it had the most to do — the real cd is what takes the pane
/// out of search mode.
fn mirror_plan(app: &App) -> Option<PaneMove> {
    let from = app.focus();
    let to = from ^ 1;
    if app.panes[from].virtual_search {
        return None;
    }
    let dir = app.panes[from].dir().clone();
    (app.panes[to].dir() != &dir || app.panes[to].virtual_search)
        .then_some(PaneMove { pane: to, dir })
}

/// `pane.pull`: the FOCUSED pane goes where the other one is — the same
/// gesture as [`mirror_plan`] the other way round, with the same two reasons
/// to decline and the same reading of a virtual DESTINATION.
fn pull_plan(app: &App) -> Option<PaneMove> {
    let to = app.focus();
    let from = to ^ 1;
    if app.panes[from].virtual_search {
        return None;
    }
    let dir = app.panes[from].dir().clone();
    (app.panes[to].dir() != &dir || app.panes[to].virtual_search)
        .then_some(PaneMove { pane: to, dir })
}

/// Carries out what a `*_plan` decided: navigate, or explain the refusal.
///
/// `pane.mirror` and `pane.pull` differ ONLY in which pane travels and which
/// one the location is read FROM, and both of those are already settled by
/// the time the plan exists — so they share this body rather than two arms
/// that must be kept in step by hand.
///
/// `origin` is the pane the location comes from. A virtual search listing
/// there is the one refusal that deserves a message: the reader asked for
/// something that cannot be done. Both panes already being in the same place
/// stays SILENT — nothing was asked for that failed.
async fn run_pane_gesture(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    plan: Option<PaneMove>,
    origin: usize,
) -> Cd {
    let Some(m) = plan else {
        if app.panes[origin].virtual_search {
            app.message = Some(t("msg-pane-not-a-location"));
        }
        return Cd::Cancelled;
    };
    // Navegación ORDINARIA, pero por `cd_in` y no por el envoltorio `cd`: el
    // pane que viaja lo dice el plan, y en el espejo NO es el del foco.
    cd_in(app, backend, events, m.pane, m.dir, Trail::Record).await
}

/// One step back for the focused pane, or `None` when the trail is empty.
///
/// Takes `&mut App` because asking IS the step: the trail hands the target
/// over and moves the current directory to the forward branch in one
/// operation, so a caller cannot peek and then forget to walk.
///
/// Reads `dir()` with NO `virtual_search` veto, unlike the mirror and pull
/// gestures, and on purpose. Those need a location to HAND OVER, and a list
/// of hits is not one. This one needs the directory to leave BEHIND on the
/// forward branch, and a results pane has a perfectly good one: its `dir()`
/// is the root the search walked, which is the pane's own directory at the
/// moment the reader pressed the search key — `launch_search` stores the very
/// same value as `SearchRun::prev_dir` to restore on `Esc`, and
/// `pane_gestures_tests::la_raiz_de_la_busqueda_es_el_dir_del_pane_que_la_lanza`
/// pins the equivalence at the seam that could break it. So a step back out
/// of a results pane goes where the reader really was, and the forward branch
/// keeps the directory they really searched from — as a listing, because the
/// hits died with the run this step reaps. Vetoing instead would strand the
/// reader in a results pane, taking away the one key that reads as "get me
/// out of here and back where I came from".
fn back_target(app: &mut App) -> Option<VPath> {
    let pane = app.focus();
    let current = app.panes[pane].dir().clone();
    app.history[pane].step_back(current)
}

/// One step forward, undoing a [`back_target`].
fn forward_target(app: &mut App) -> Option<VPath> {
    let pane = app.focus();
    let current = app.panes[pane].dir().clone();
    app.history[pane].step_forward(current)
}

/// Rewinds the step [`back_target`]/[`forward_target`] took, because the cd
/// it aimed at FAILED and the reader never actually left where they were.
///
/// The inverse of a step IS the step the other way: `step_forward(target)`
/// pops the `current` that `step_back` pushed onto the forward branch and
/// puts `target` back where it came from. Rewinding through the same two
/// methods is why the two stacks cannot drift — there is no second piece of
/// bookkeeping to get wrong.
fn untake_step(app: &mut App, pane: usize, step: TrailStep, target: VPath) {
    let _ = match step {
        TrailStep::Back => app.history[pane].step_forward(target),
        TrailStep::Forward => app.history[pane].step_back(target),
    };
}

/// What a finished trail step should do to the trail it was walking.
///
/// The WHOLE policy of [`walk_trail`], in one value the tests can ask for
/// directly. It used to live inline in `walk_trail`, where the only way to
/// pin it was to re-enact the effect in the test — which pins [`History`],
/// not the policy: `walk_trail` could stop rewinding altogether and every
/// test stayed green.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rewind {
    /// Leave the trail as the step left it: the pane really did move, or
    /// something is about to resume the very same navigation.
    No,
    /// Put the step back — the reader never left where they were.
    Step,
    /// Put the step back AND retire the destination from the whole history:
    /// it proved not to be there.
    StepAndRetire,
}

/// Decides, from how a trail step ENDED, what the trail owes the reader.
///
/// A `Failed` never moved the pane, so the step is put back; when the reason
/// is that the directory is GONE it also leaves the history entirely — the
/// same treatment the nav popup already gives a `NotFound`, so the reader is
/// never left with a key that can only aim at a directory that proved not to
/// be there.
///
/// A `Cancelled` rewinds TOO. It is the outcome of `Esc` during a slow
/// listing and of an event stream that died: nothing resumes those, and the
/// pane never moved, so a trail that kept the step would believe the reader
/// left a directory they are still looking at — and the next `nav.forward`
/// would "return" them to the listing already on screen while `back` grew a
/// phantom that eats the following `nav.back` as well. The one outcome that
/// must NOT rewind is `Suspended`: the TOFU modal resumes this very
/// navigation (it carries the pane and the trail mode), and a rewound trail
/// would count the successful retry twice.
///
/// Everything else — the two landings and the outcomes that reach `apply_cd`
/// from elsewhere — means the pane moved or the trail was never involved.
fn rewind_for(outcome: &Cd) -> Rewind {
    match outcome {
        Cd::Failed(Error::NotFound) => Rewind::StepAndRetire,
        Cd::Failed(_) | Cd::Cancelled => Rewind::Step,
        Cd::Suspended | Cd::Filling { .. } | Cd::Replaced(_) | Cd::Refreshed(..) | Cd::Swapped => {
            Rewind::No
        }
    }
}

/// Carries out what [`rewind_for`] decided, on the trail of `pane`.
///
/// Split from the decision so the decision can be read (and tested) without a
/// backend, and joined to it at the ONE call site in [`walk_trail`] — the
/// tests drive this pair, which is the pair the production path drives.
fn rewind_trail(app: &mut App, pane: usize, step: TrailStep, dir: &VPath, rewind: Rewind) {
    match rewind {
        Rewind::No => {}
        Rewind::Step => untake_step(app, pane, step, dir.clone()),
        Rewind::StepAndRetire => {
            untake_step(app, pane, step, dir.clone());
            app.history[pane].remove(dir);
        }
    }
}

/// Settles the trail of a navigation the TOFU prompt SUSPENDED, once that
/// prompt has been answered.
///
/// [`walk_trail`] deliberately leaves a `Cd::Suspended` step taken: the retry
/// was going to finish it. But the retry is not guaranteed to happen — the
/// reader can deny the key, trusting it can fail, and the retry itself can
/// fail or be abandoned — and when it does not, the step is left standing for
/// a move that never occurred. That is the same lie [`rewind_for`] exists to
/// stop, on the one path where the navigation OUTLIVES the function that
/// started it, which is why nobody was there to undo it.
///
/// Runs the answer's outcome through the very same [`rewind_for`] the trail
/// walker runs. `trail.step()` of `None` (a `Trail::Record` navigation: a
/// plain cd that happened to meet an unknown host) means there is no step to
/// rewind, so this is a no-op — and a second `Suspended` (another unknown
/// key, or the same one asked again) is a no-op TOO: the modal is open again
/// carrying the same trail, so the step is still going to be settled by
/// whoever answers THAT one.
///
/// Rewinding here cannot double up with [`walk_trail`]: the walker saw
/// `Suspended` and did nothing, so this is the FIRST and only rewind of that
/// step.
fn settle_suspended_trail(app: &mut App, pane: usize, dir: &VPath, trail: Trail, outcome: &Cd) {
    if let Some(step) = trail.step() {
        rewind_trail(app, pane, step, dir, rewind_for(outcome));
    }
}

/// `nav.back` / `nav.forward`: replays the focused pane's trail one step.
///
/// What a finished step owes the trail is [`rewind_for`]'s call, applied by
/// [`rewind_trail`]; this body only walks.
///
/// A `Cd::Suspended` leaves the step taken on purpose — see
/// [`settle_suspended_trail`], which is who finishes it.
async fn walk_trail(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    step: TrailStep,
) -> Cd {
    let pane = app.focus();
    let target = match step {
        TrailStep::Back => back_target(app),
        TrailStep::Forward => forward_target(app),
    };
    let Some(dir) = target else {
        app.message = Some(t(step.empty_message()));
        return Cd::Cancelled;
    };
    // `Trail::Replay`: el rastro se está recorriendo a sí mismo. Si esto
    // registrara, volver de B a A grabaría «estuve en B» y el siguiente atrás
    // devolvería a B — la misma oscilación que el rastro existe para evitar,
    // un nivel más arriba. LLEVA el paso: si la navegación se SUSPENDE (TOFU),
    // quien responda al modal es quien tendrá que rebobinarlo, y para eso
    // necesita saber en qué sentido iba.
    let outcome = cd_in(app, backend, events, pane, dir.clone(), Trail::Replay(step)).await;
    rewind_trail(app, pane, step, &dir, rewind_for(&outcome));
    outcome
}

/// Ejecuta un comando nombrado (ADR 0006: los mismos nombres que verán la
/// palette y el wire). Un error de listado en un cd NO tumba el TUI: el
/// pane se queda donde estaba (aviso visible: barra de mensajes, issue #20).
#[allow(clippy::too_many_lines, clippy::too_many_arguments)] // tabla de despacho comando→efecto, no API
async fn dispatch(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    help_lines: &[String],
    // H3b: the negotiated language `Command::AppHelp` opens the corpus in.
    // See the same parameter on `run`.
    lang: norte_i18n::Lang,
    quick_mode: nav::Mode,
    confirm_quit: config::ConfirmQuit,
    // S3 (`app.settings`): la config VIGENTE — solo leída, para construir
    // las filas del overlay al abrirlo (`crate::settings::build_rows`).
    cfg: &config::LoadedConfig,
    cmd: Command,
) -> Cd {
    // Solo los cd (nav.enter/nav.parent) tocan el relleno en background; el
    // resto de comandos lo dejan como está (`Cancelled`).
    let mut cd_outcome = Cd::Cancelled;
    match cmd {
        // S2 (`[ui] confirm_quit`): SOLO este brazo (el despacho nombrado de
        // `app.quit`, alcanzable por keymap Y por la palette) honra la
        // config y puede abrir `Modal::ConfirmQuit`. Los `app.quit = true`
        // hardcodeados de Ctrl+C repartidos por el resto de este fichero
        // (cada overlay tiene el suyo, documentado in situ) son la salida de
        // emergencia — se quedan INMEDIATOS a propósito, jamás preguntan.
        Command::AppQuit => {
            if norte_tui::app::quit_needs_confirm(confirm_quit, app.board.has_active()) {
                app.modal = Some(Modal::ConfirmQuit);
            } else {
                app.quit = true;
            }
        }
        Command::PaneSwitch => app.switch_focus(),
        // `pane.mirror`: la ubicación sale del pane con FOCO y viaja el otro.
        Command::PaneMirror => {
            let plan = mirror_plan(app);
            let origin = app.focus();
            cd_outcome = run_pane_gesture(app, backend, events, plan, origin).await;
        }
        // `pane.pull`: el mismo gesto al revés — la ubicación sale del OTRO
        // pane y viaja el del foco.
        Command::PanePull => {
            let plan = pull_plan(app);
            let origin = app.focus() ^ 1;
            cd_outcome = run_pane_gesture(app, backend, events, plan, origin).await;
        }
        // `pane.swap`: NO toca disco — los dos listados ya existían y solo
        // cambian de lado. La mitad que `dispatch` no ve (fill en vuelo,
        // fetches de decoración, dedup de la sonda) viaja al run loop.
        Command::PaneSwap => {
            app.swap_panes();
            cd_outcome = Cd::Swapped;
        }
        Command::NavBack => {
            cd_outcome = walk_trail(app, backend, events, TrailStep::Back).await;
        }
        Command::NavForward => {
            cd_outcome = walk_trail(app, backend, events, TrailStep::Forward).await;
        }
        // `/` (spec 2026-07-18): arranca el quick search en el modo de la
        // config. Con uno ya activo las teclas se comen antes del resolver,
        // así que este brazo solo corre para ABRIRLO — sin recursión.
        Command::PaneQuickSearch => app.focused_mut().quick_start(quick_mode),
        // `Alt+↓` / `Ctrl+D` (spec 2026-07-18): con el popup abierto sus
        // teclas se comen antes del resolver (patrón overlay) — estos
        // brazos solo corren para ABRIRLO.
        Command::PaneHistory => app.open_nav_popup(NavPopupKind::History),
        Command::PaneHotlist => app.open_nav_popup(NavPopupKind::Hotlist),
        // `Alt+F7` (liveSearch T6): abre el diálogo de búsqueda viva. Con él
        // abierto sus teclas se comen antes del resolver (patrón overlay) —
        // este brazo solo corre para ABRIRLO.
        Command::PaneSearch => app.open_search_dialog(),
        Command::CursorUp => app.focused_mut().move_up(1),
        Command::CursorDown => app.focused_mut().move_down(1),
        // #124: una PÁGINA es una pantalla del pane (menos una fila de
        // contexto), no una constante — el alto real llega del último frame.
        Command::CursorPageUp => {
            let paso = app.focused().page_step();
            app.focused_mut().move_up(paso);
        }
        Command::CursorPageDown => {
            let paso = app.focused().page_step();
            app.focused_mut().move_down(paso);
        }
        Command::CursorTop => app.focused_mut().move_to_start(),
        Command::CursorBottom => app.focused_mut().move_to_end(),
        Command::NavEnter => {
            // También symlinks: si apunta a un dir, el provider listará; si
            // no, el cd falla y se absorbe — qué es "entrable" lo decide el
            // core, no el TUI (regla 7). Un File .zip/.tar entra como
            // directorio virtual (ADR 0018): el TUI solo COMPONE el path
            // (azúcar de navegación); listar/validar sigue siendo del core.
            let target = app
                .focused()
                .selected()
                .filter(|e| matches!(e.kind, EntryKind::Dir | EntryKind::Symlink))
                .map(|e| e.path.clone())
                .or_else(|| app.focused().selected().and_then(nav::archive_root_for));
            if let Some(dir) = target {
                cd_outcome = cd(app, backend, events, dir).await;
            }
        }
        Command::NavParent => {
            // Salir de la raíz interior de un archivo = el dir que CONTIENE
            // al contenedor (el padre sintáctico sería un compuesto sin
            // marcador: malformado, ADR 0018).
            let dir = app.focused().dir().clone();
            // Foco pendiente (spec 2026-07-24 §S1): el hijo del que
            // venimos, para seleccionarlo en el listado del padre. Al salir
            // de la raíz interior de un archivo el hijo NO es `dir` (ese es
            // el path compuesto virtual, no una entrada real del listado
            // del padre) sino el archivo contenedor mismo (`aref.outer`).
            let (parent, child) = match dir.archive_split() {
                Ok(Some(aref)) if aref.inner.is_empty() => {
                    let outer = aref.outer.clone();
                    (outer.parent(), outer)
                }
                _ => (dir.parent(), dir.clone()),
            };
            if let Some(parent) = parent {
                app.focused_mut().set_pending_focus(child);
                cd_outcome = cd(app, backend, events, parent).await;
                // Revisión S, M2: un `cd` FALLIDO (permiso denegado, error
                // del daemon…) nunca llama a `set_listing` (`cd`'s doc, `Err`
                // arm), así que el hint recién fijado arriba nunca se
                // consume — descartarlo aquí evita que sobreviva a un `cd`
                // futuro sin relación. `Cd::Suspended` (el modal TOFU, que
                // REINTENTA esta misma navegación) lo CONSERVA a propósito: el
                // reintento debe seguir aterrizando en `child`. Un
                // `Cd::Cancelled` (Esc) también lo conserva — el lector sigue
                // en el mismo listado, y el hint muere con el siguiente cd que
                // sí aterrice.
                if matches!(cd_outcome, Cd::Failed(_)) {
                    app.focused_mut().clear_pending_focus();
                }
            } else {
                // Raíz `/` o raíz de unidad Windows (`parent()` = None): antes
                // era un no-op SILENCIOSO (#20). Ahora avisa por la barra.
                app.message = Some(t("msg-nav-at-top"));
            }
        }
        Command::PaneCopy | Command::PaneMove => {
            let kind = if cmd == Command::PaneCopy {
                TransferKind::Copy
            } else {
                TransferKind::Move
            };
            // Destino ortodoxo: el DIRECTORIO del otro pane. Los orígenes son
            // las marcas, o el cursor si no hay ninguna (#103). El resto —
            // nombre editable con un solo ítem (#105), confirm de lista con
            // varios— lo decide `open_transfer`, que es la MISMA puerta por
            // la que entra un drop del ratón: una segunda ruta para someter
            // una transferencia es una ruta que se queda sin confirmación,
            // sin colisiones o sin undo en cuanto una de las dos cambie.
            app.open_transfer(kind, app.focus(), app.focus() ^ 1, None);
        }
        // #105: shift+F6 — rename in situ (Move al PADRE de `from`, nombre
        // editable). Correcto también en el pane virtual: el destino sale
        // del propio path del hit, no del dir del pane.
        Command::PaneRename => app.open_rename(),
        // #106: Ctrl+R — recarga manual. Reusa el refresh post-mutación
        // (cancelable regla 3; marcas sobreviven vía refill con poda
        // VISIBLE, cursor por índice; el pane virtual de búsqueda se salta
        // — sus hits no viven en un dir). Ambos panes, como tras una task
        // propia: un cambio externo raramente respeta el foco.
        // #118: el desenlace VIAJA al run loop (`Cd::Refreshed`) — dispatch
        // no ve `fill`/`last_probed`, y sin el ritual un drenador paginado
        // vivo duplicaría filas sobre el listado recién completo.
        Command::PaneRefresh => {
            cd_outcome = Cd::Refreshed(refresh_panes(app, backend, events).await);
        }
        // Insert/Ctrl+A/Ctrl+Shift+A/`*` (#103): mc/Total Commander —
        // togglear la marca de esta entrada y avanzar (mantener Insert barre
        // un rango). Review MAJOR: bajo un quick search en Filter,
        // `toggle_mark` actúa sobre la selección FILTRADA mientras el cursor
        // real es otra cosa — avanzar el cursor real desincroniza el rango
        // barrido del filtro. La composición completa (marcar + a qué avanza
        // según haya o no filtro, clampado sin envolver) vive en el modelo
        // compartido.
        Command::MarkToggle => app.focused_mut().toggle_mark_and_advance(),
        Command::MarkAll => app.focused_mut().mark_all(),
        Command::MarkInvert => app.focused_mut().invert_marks(),
        Command::MarkClear => app.focused_mut().clear_marks(),
        // `+`/`-` (#103 T9): abren el modal de patrón (texto libre, ver el
        // brazo `app.modal.is_some()` de arriba) — marcar/desmarcar
        // corre al confirmar (`mark_pattern_confirm`), no aquí.
        Command::MarkPatternAdd => app.open_mark_pattern(true),
        Command::MarkPatternRemove => app.open_mark_pattern(false),
        // #104: F7 — crear directorio en el pane con foco. En el pane
        // VIRTUAL de búsqueda no hay directorio destino visible (review
        // MINOR-2: `dir()` es la raíz del walk, no lo que se pinta).
        Command::PaneMkdir => {
            if app.focused().virtual_search {
                app.message = Some(t("msg-mkdir-in-search"));
            } else {
                app.open_mkdir();
            }
        }
        // M4-IA: rename asistido del dir con foco. En el pane VIRTUAL de
        // búsqueda no hay un directorio único que renombrar (mismo criterio
        // que `PaneMkdir`). Las teclas del prompt y la petición viven en el
        // run loop (intercepción Tier-A + `AiRenameRun`).
        Command::PaneAiRename => {
            if app.focused().virtual_search {
                app.message = Some(t("msg-ai-rename-in-search"));
            } else {
                app.open_ai_rename();
            }
        }
        // M4-IA-2: búsqueda semántica sobre el índice (todos los roots). En
        // el pane VIRTUAL de búsqueda el prompt colisionaría con la
        // semántica Esc/Enter propia del modo (mismo criterio que
        // `PaneAiRename`). Las teclas del prompt y la petición viven en el
        // run loop (intercepción Tier-A + `SemanticRun`).
        Command::PaneSemanticSearch => {
            if app.focused().virtual_search {
                app.message = Some(t("msg-semantic-in-search"));
            } else {
                app.open_semantic_search();
            }
        }
        Command::PaneDelete | Command::PaneDeletePermanent => {
            // F8 = papelera si el provider la declara; sin ella, el MISMO
            // diálogo avisa de PERMANENTE (degradación con usuario
            // informado, ADR 0009). shift+f8 = permanente. La capability se
            // sondea UNA vez POR LOTE con el primer ítem (#103 T10): todas
            // las marcas viven en el mismo directorio del mismo provider,
            // así que N sondeos serían N round-trips de red para la misma
            // respuesta.
            if let Some(first) = app.focused().marked_paths().first() {
                let hay_papelera = backend
                    .capabilities(first)
                    .await
                    .is_ok_and(|c| c.flags.contains(norte_proto::CapabilityFlags::TRASH));
                let permanent = cmd == Command::PaneDeletePermanent || !hay_papelera;
                app.open_delete_modal(permanent);
            }
        }
        Command::PaneView => {
            // También symlinks (mismo criterio que nav.enter): si apunta a
            // un dir, el read fallará con mensaje visible.
            let target = app
                .focused()
                .selected()
                .filter(|e| matches!(e.kind, EntryKind::File | EntryKind::Symlink))
                .map(|e| e.path.clone());
            if let Some(path) = target {
                open_viewer(app, backend, events, path).await;
            }
        }
        Command::PaneOpen => resolve_opener(app),
        Command::ViewerClose => app.viewer = None,
        Command::ViewerUp => viewer_do(app, |v| v.scroll_up(1)),
        Command::ViewerDown => viewer_do(app, |v| v.scroll_down(1)),
        Command::ViewerPageUp => viewer_do(app, |v| v.scroll_up(norte_tui::viewer::PAGE)),
        Command::ViewerPageDown => viewer_do(app, |v| v.scroll_down(norte_tui::viewer::PAGE)),
        Command::ViewerTop => viewer_do(app, norte_tui::viewer::Viewer::scroll_top),
        Command::ViewerBottom => viewer_do(app, norte_tui::viewer::Viewer::scroll_bottom),
        Command::ViewerEncoding => viewer_do(app, norte_tui::viewer::Viewer::cycle_encoding),
        Command::ViewerEncodingAuto => viewer_do(app, norte_tui::viewer::Viewer::reset_encoding),
        Command::ViewerHex => viewer_do(app, norte_tui::viewer::Viewer::toggle_hex),
        // H3c: la página de DONDE ESTÁ el lector, no el índice. Todo el cuerpo
        // vive en `open_contextual_help` (documentado allí) para que los tests
        // abran la ayuda por el MISMO sitio que F1.
        // H3e: la foto del catálogo se toma AQUÍ, en el camino de apertura —
        // una sola llamada, jamás mientras se pinta. Un fallo del backend deja
        // la ayuda sin filas de extensión (y con todo comando `plugin:`
        // atenuado), que es exactamente lo que «no lo pude averiguar»
        // significa; nunca tumba la ayuda entera.
        Command::AppHelp => {
            let plugins = backend.plugins_list().await.ok();
            open_contextual_help(app, lang, help_lines, plugins.as_ref());
        }
        Command::PaneNamesEncoding => {
            // #57: cicla la reinterpretación de nombres no-UTF8 del pane con
            // foco (display-only, regla 1). El anuncio va por la barra.
            let label = app.focused_mut().cycle_name_encoding();
            app.message = Some(match label {
                Some(enc) => ta("msg-names-encoding", &[("enc", enc)]),
                None => t("msg-names-encoding-off"),
            });
        }
        Command::PaneToggleHidden => {
            // #107: presentación-solo — el pane aparta/devuelve dotfiles,
            // el provider no re-lista. El anuncio va por la barra.
            let showing = app.focused_mut().toggle_hidden();
            app.message = Some(if showing {
                t("msg-hidden-shown")
            } else {
                t("msg-hidden-hidden")
            });
        }
        Command::AppTheme => app.open_theme_picker(),
        Command::PaneColumns => app.open_columns_picker(),
        Command::AppExtensions => match backend.plugins_list().await {
            // El catálogo llega YA ordenado por categoría e id desde el core.
            Ok(list) => {
                // (P1 encoding audit F1) INGEST: clampa+enmascara `description`
                // UNA vez aquí, no en cada frame de `plugin_description_line`
                // — defensa contra un daemon hostil/comprometido que ignore
                // el tope del manifiesto.
                let mut plugins = list.plugins;
                norte_tui::app::clamp_plugin_descriptions(&mut plugins);
                app.extensions = Some(ExtensionManager {
                    plugins,
                    errors: list.errors,
                    cursor: 0,
                    config: None,
                });
            }
            Err(e) => app.message = Some(error_message(&e)),
        },
        // Ctrl+P / vim `:` (H1 T4, spec-promised): abre la palette sobre la
        // snapshot PRECOMPUTADA (`App::palette_rows`, `main::build_keymaps`
        // + hot-reload) — jamás recalcula el keymap efectivo aquí. Elegir
        // `app.palette` DESDE la palette (el run loop la cierra ANTES de
        // despachar, `enter`) es un no-op observable: cierra y reabre
        // vacía — inofensivo, sin recursión de estado.
        //
        // (P1) ahora es ASÍNCRONA, como `app.extensions` arriba: las filas
        // de plugin necesitan `backend.plugins_list().await` (aprobado +
        // activado, `palette::plugin_rows`). A diferencia de `app.extensions`
        // (que NO abre el gestor si el fetch falla), los built-ins SIEMPRE
        // deben poder despacharse — un daemon caído no debe tumbar la
        // palette entera, solo degradarla (sin filas de plugin + un aviso),
        // mismo principio "un error de listado no tumba el TUI" del resto
        // de `dispatch`.
        Command::AppPalette => {
            // MINOR-6 (H1 close): Ctrl+P/`:` viven en `[global]`, fundido en
            // AMBOS efectivos — la palette puede abrirse desde el viewer
            // también, no solo desde browse (`rows_for_context` doc).
            let mut rows =
                norte_tui::palette::rows_for_context(&app.palette_rows, app.viewer.is_some());
            match backend.plugins_list().await {
                Ok(list) => {
                    // (P1 encoding audit F1) INGEST: mismo clamp que el brazo
                    // `app.extensions` — un solo punto de entrada, mismo tope.
                    let mut plugins = list.plugins;
                    norte_tui::app::clamp_plugin_descriptions(&mut plugins);
                    rows.extend(norte_tui::palette::plugin_rows(&plugins));
                }
                Err(e) => app.message = Some(error_message(&e)),
            }
            app.palette = Some(Palette::new(rows));
        }
        // `F11` (S3): overlay de ajustes — las filas nacen del `cfg` VIGENTE
        // (mismo criterio que `help_lines`/`app.palette_rows`: reconstruidas
        // al abrir, jamás una copia arrastrada). Sección Plugins (G3c): un
        // resumen POR plugin con `[config]` (real ahora, ya no la nota
        // informativa de P2 — `plugin_config_summaries`).
        Command::AppSettings => {
            let summaries = plugin_config_summaries(backend).await;
            app.settings = Some(Settings::new(norte_tui::settings::build_rows(
                cfg, &summaries,
            )));
        }
        Command::TaskCancel => {
            app.message = Some(if app.board.cancel_last_running() {
                t("msg-cancelling")
            } else {
                t("msg-no-tasks")
            });
        } // Sin comodín (#112): `Command` es exhaustivo — un comando nuevo
          // sin brazo es un error de COMPILACIÓN, no un pánico de runtime.
    }
    cd_outcome
}

/// Parsea una `key` de fila de plugin de la palette
/// (`plugin:{plugin_id}:{command_id}`, [`norte_tui::palette::plugin_rows`])
/// de vuelta a `(plugin_id, command_id)`. El `plugin_id` es reverse-DNS
/// charset-validado por el core (`is_valid_plugin_id`, norte-plugin-host
/// manifest.rs — nunca lleva `:`); el `command_id` del manifiesto NO tiene
/// charset validado, así que puede llevar CUALQUIER byte, incluidos `:` o
/// saltos de línea. El PRIMER `:` que sigue al prefijo `plugin:` separa
/// ambos sin ambigüedad (el `plugin_id` no puede contenerlo) — el resto,
/// TODO lo que quede tras ese primer `:`, es el `command_id` crudo, tomado
/// ENTERO y jamás vuelto a partir.
fn parse_plugin_key(cmd: &str) -> Option<(&str, &str)> {
    let (id, command) = cmd.strip_prefix("plugin:")?.split_once(':')?;
    (!id.is_empty()).then_some((id, command))
}

/// Builds the Plugins-section summaries for the settings overlay (G3c):
/// `plugins_list` (approved+enabled only — same gate the palette's
/// `plugin_rows` and the extension manager's actionable rows use) then one
/// `plugin.get_config` PER surviving plugin, keeping only those with at
/// least one `[config.<key>]` (nothing to summarize/drill into otherwise).
/// Best-effort: a plugin whose `get_config` call fails (daemon hiccup, a
/// remote N-1 without the method) is simply DROPPED from the section — an
/// enrichment lost, never a hard error that would block opening settings
/// at all (same fallback contract as `plugin.decorate`/`column_values`).
/// `name` is masked here (plugin text, untrusted) — the ONLY point this
/// summary crosses into `norte_frontend::settings::Row`.
async fn plugin_config_summaries(
    backend: &Backend,
) -> Vec<norte_frontend::settings::PluginConfigSummary> {
    let Ok(list) = backend.plugins_list().await else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for p in list.plugins.iter().filter(|p| p.approved && p.enabled) {
        let Ok(cfg) = backend.plugin_get_config(&p.id).await else {
            continue;
        };
        if cfg.keys.is_empty() {
            continue;
        }
        let (name, _) = norte_tui::app::display_name(p.name.as_bytes());
        out.push(norte_frontend::settings::PluginConfigSummary {
            plugin_id: p.id.clone(),
            name,
            key_count: cfg.keys.len(),
        });
    }
    out
}

#[cfg(test)]
mod pane_gestures_tests {
    use super::{
        App, Cd, Pane, Rewind, Trail, TrailStep, back_target, forward_target, mirror_plan,
        pull_plan, record_step, rewind_for, rewind_trail, settle_suspended_trail,
    };
    use norte_proto::{Error, VPath};

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire de test")
    }

    /// `App` con cada pane sobre su dir. Se construyen los panes ENTEROS
    /// (`Pane::new`) en vez de mover un pane existente: es el mismo molde que
    /// usan los demás módulos de test de este fichero y no hace falta ningún
    /// setter `#[cfg(test)]` nuevo.
    fn app_en(izq: &str, der: &str) -> App {
        App::new(
            Pane::new(vp(izq), Vec::new()),
            Pane::new(vp(der), Vec::new()),
        )
    }

    /// Espejo: el pane SIN foco se va a donde está el que tiene el foco, y el
    /// foco no se mueve.
    #[test]
    fn el_espejo_manda_al_otro_pane_y_no_mueve_el_foco() {
        let mut app = app_en("mem:///a", "mem:///b");
        app.set_focus(0);
        let plan = mirror_plan(&app).expect("con dos panes normales hay plan");
        assert_eq!(plan.pane, 1, "viaja el OTRO pane");
        assert_eq!(plan.dir, vp("mem:///a"), "a donde está el del foco");
        assert_eq!(app.focus(), 0, "el foco no se ha movido");
    }

    /// El espejo mira al FOCO, no al pane 0: con el foco a la derecha viaja
    /// el izquierdo. (Mutación de control: fijar `from = 0` rompe aquí.)
    #[test]
    fn el_espejo_con_el_foco_a_la_derecha_manda_el_izquierdo() {
        let mut app = app_en("mem:///a", "mem:///b");
        app.set_focus(1);
        let plan = mirror_plan(&app).expect("plan");
        assert_eq!(plan.pane, 0);
        assert_eq!(plan.dir, vp("mem:///b"));
    }

    /// Traer: el pane CON foco se va a donde está el otro.
    #[test]
    fn traer_mueve_el_pane_con_foco() {
        let mut app = app_en("mem:///a", "mem:///b");
        app.set_focus(0);
        let plan = pull_plan(&app).expect("plan");
        assert_eq!(plan.pane, 0);
        assert_eq!(plan.dir, vp("mem:///b"));
    }

    /// Los dos ya en el mismo sitio: no-op SILENCIOSO, no un cd redundante
    /// que reordene el listado del otro pane bajo el cursor del lector.
    #[test]
    fn en_el_mismo_dir_no_hay_nada_que_hacer() {
        let mut app = app_en("mem:///a", "mem:///a");
        app.set_focus(0);
        assert!(mirror_plan(&app).is_none());
        assert!(pull_plan(&app).is_none());
    }

    /// Desde un pane VIRTUAL de resultados no hay ubicación que mandar ni de
    /// donde traer: `dir()` ahí es la RAÍZ del walk, no lo que el lector ve.
    #[test]
    fn un_pane_virtual_no_es_una_ubicacion() {
        let mut app = app_en("mem:///a", "mem:///b");
        app.panes[0].virtual_search = true;
        app.set_focus(0);
        assert!(mirror_plan(&app).is_none(), "no hay origen que mandar");
        app.set_focus(1);
        assert!(pull_plan(&app).is_none(), "ni de donde traer");
    }

    /// El pane virtual solo veta cuando es el ORIGEN. Mandarle una ubicación
    /// ENCIMA sí vale: el cd real lo saca del modo búsqueda, que es
    /// exactamente lo que el lector pidió.
    #[test]
    fn un_pane_virtual_si_puede_ser_destino() {
        let mut app = app_en("mem:///a", "mem:///b");
        app.panes[1].virtual_search = true;
        app.set_focus(0);
        let plan = mirror_plan(&app).expect("el destino virtual no veta");
        assert_eq!(plan.pane, 1);
        assert_eq!(plan.dir, vp("mem:///a"));
    }

    /// Y el atajo de «ya están los dos en el mismo sitio» no puede callarse
    /// ante un destino VIRTUAL: la raíz por la que anduvo la búsqueda suele
    /// ser justamente el dir del otro pane, y ahí `dir()` no es lo que el
    /// lector está viendo. Con el atajo comparando solo dirs, reflejar sobre
    /// un pane de resultados enraizado ahí mismo no hacía NADA — ni sacaba al
    /// pane del modo búsqueda, ni decía por qué.
    #[test]
    fn un_destino_virtual_en_el_mismo_dir_si_tiene_algo_que_hacer() {
        let mut app = app_en("mem:///a", "mem:///a");
        app.panes[1].virtual_search = true;

        app.set_focus(0);
        let plan = mirror_plan(&app).expect("el destino virtual no está «ya ahí»");
        assert_eq!(plan.pane, 1, "viaja el pane de resultados");
        assert_eq!(plan.dir, vp("mem:///a"), "y el cd real lo saca del modo");

        // Y el gesto simétrico: con el foco EN el pane virtual, `pane.pull`
        // lo trae a donde está el otro — el mismo dir, listado de verdad.
        app.set_focus(1);
        let plan = pull_plan(&app).expect("traer a un pane virtual tampoco es no-op");
        assert_eq!(plan.pane, 1);
        assert_eq!(plan.dir, vp("mem:///a"));
    }

    // --- nav.back / nav.forward ---

    /// Mueve el pane 0 a `dir` sin pasar por un `cd` (que necesita backend):
    /// un pane NUEVO sobre ese dir, el mismo molde que usan los demás
    /// módulos de test de este fichero.
    fn poner_en(app: &mut App, dir: &str) {
        app.panes[0] = Pane::new(vp(dir), Vec::new());
    }

    /// El rastro se recorre de verdad: A→B→C, dos veces atrás llega a A. La
    /// oscilación A→B→A→B que daría recorrer la MRU es lo que este test
    /// rechaza.
    #[test]
    fn atras_recorre_el_rastro_y_adelante_lo_deshace() {
        let mut app = app_en("mem:///c", "mem:///otro");
        app.set_focus(0);
        app.history[0].record(vp("mem:///a"));
        app.history[0].record(vp("mem:///b"));

        let a_donde = back_target(&mut app).expect("hay rastro");
        assert_eq!(a_donde, vp("mem:///b"));
        poner_en(&mut app, "mem:///b");
        assert_eq!(back_target(&mut app), Some(vp("mem:///a")));
        poner_en(&mut app, "mem:///a");
        assert_eq!(back_target(&mut app), None, "se acabó el rastro");

        assert_eq!(forward_target(&mut app), Some(vp("mem:///b")));
    }

    /// Con el rastro vacío la tecla lo DICE: una tecla que calla es
    /// indistinguible de una rota. (El mensaje lo pone `walk_trail`, que
    /// necesita backend; aquí se pinea la mitad que decide que NO hay
    /// destino.)
    #[test]
    fn atras_sin_rastro_no_da_destino() {
        let mut app = app_en("mem:///a", "mem:///otro");
        app.set_focus(0);
        assert_eq!(back_target(&mut app), None);
        assert_eq!(forward_target(&mut app), None);
    }

    /// La propiedad que impide el bucle: un `Replay` no registra. Se
    /// comprueba sobre el rastro, que es donde vive la decisión.
    #[test]
    fn el_rastro_no_se_alimenta_de_si_mismo() {
        let mut app = app_en("mem:///c", "mem:///otro");
        app.set_focus(0);
        app.history[0].record(vp("mem:///b"));
        let antes = app.history[0].back_len();
        let _ = back_target(&mut app);
        assert_eq!(
            app.history[0].back_len(),
            antes - 1,
            "un paso atrás CONSUME rastro; jamás lo produce"
        );
    }

    /// Un paso atrás desde un pane de RESULTADOS sí se da (es la tecla que
    /// más se parece a «sácame de aquí»), y lo que deja en la rama de delante
    /// es el directorio REAL desde el que se buscó — no una lista de hits, que
    /// no es un sitio. El `dir()` de un pane virtual ES ese directorio.
    #[test]
    fn atras_desde_un_pane_de_resultados_deja_el_dir_real_en_la_rama() {
        let mut app = app_en("mem:///b", "mem:///otro");
        app.set_focus(0);
        app.history[0].record(vp("mem:///a")); // el lector llegó a B desde A
        app.panes[0].begin_search(vp("mem:///b")); // Alt+F7 en B
        assert!(app.panes[0].virtual_search, "pane de resultados");

        assert_eq!(
            back_target(&mut app),
            Some(vp("mem:///a")),
            "el paso sale de la búsqueda hacia donde el lector estaba antes"
        );
        poner_en(&mut app, "mem:///a"); // el cd real aterriza (y cosecha el run)
        assert_eq!(
            forward_target(&mut app),
            Some(vp("mem:///b")),
            "y el adelante devuelve al dir desde el que se buscó, como listado"
        );
    }

    /// Lo que hace honesto el test de arriba, pinchado en la costura que
    /// podría romperlo: la raíz de la búsqueda ES el `dir()` del pane que la
    /// lanza. Si el diálogo dejara teclear otra raíz, `back_target` empezaría
    /// a apuntar a un sitio donde el lector no ha estado y tendría que leer
    /// `SearchRun::prev_dir` en su lugar.
    #[test]
    fn la_raiz_de_la_busqueda_es_el_dir_del_pane_que_la_lanza() {
        use crossterm::event::{KeyCode, KeyModifiers};

        let mut app = app_en("mem:///raiz", "mem:///otro");
        app.set_focus(0);
        let mut dialog = norte_tui::app::SearchDialog::new();
        dialog.push_char('x'); // sin criterio, Enter no lanza
        app.search_dialog = Some(dialog);

        let params = super::on_search_dialog_key(&mut app, KeyModifiers::NONE, KeyCode::Enter)
            .expect("Enter con criterio lanza la búsqueda");
        assert_eq!(
            params.root,
            *app.panes[0].dir(),
            "la raíz del walk es el dir del pane con foco"
        );
    }

    /// El rastro es POR PANE: `back_target` sigue al foco, no al pane 0.
    #[test]
    fn el_rastro_es_del_pane_con_foco() {
        let mut app = app_en("mem:///izq", "mem:///der");
        app.history[0].record(vp("mem:///solo-izq"));
        app.set_focus(1);
        assert_eq!(back_target(&mut app), None, "el pane 1 no tiene rastro");
        app.set_focus(0);
        assert_eq!(back_target(&mut app), Some(vp("mem:///solo-izq")));
    }

    // --- La POLÍTICA del rastro (`rewind_for`) y su efecto (`rewind_trail`).
    // Los tests llaman a las MISMAS funciones que llama `walk_trail`: antes
    // re-implementaban el efecto (`untake_step` + `history.remove` a mano),
    // así que `walk_trail` podía dejar de rebobinar y seguían verdes.

    /// Un `NotFound` no solo rebobina: RETIRA el destino de todo el
    /// historial. Es la única de las tres decisiones que toca la MRU.
    #[test]
    fn la_politica_ante_un_destino_que_no_existe_es_rebobinar_y_retirar() {
        assert_eq!(
            rewind_for(&Cd::Failed(Error::NotFound)),
            Rewind::StepAndRetire
        );
    }

    /// Cualquier OTRO fallo rebobina pero CONSERVA el destino: un host caído
    /// o un directorio que no puedes leer siguen siendo sitios, y pueden
    /// responder al siguiente intento.
    #[test]
    fn la_politica_ante_otro_fallo_es_rebobinar_conservando_el_destino() {
        assert_eq!(
            rewind_for(&Cd::Failed(Error::PermissionDenied)),
            Rewind::Step
        );
    }

    /// Un cd ABANDONADO (Esc durante un listado lento, o el stream de
    /// eventos muriéndose) rebobina IGUAL que un fallo: nadie lo reanuda y el
    /// pane no se movió. Sin esto el rastro cree que el lector se fue de un
    /// directorio que sigue en pantalla.
    #[test]
    fn la_politica_ante_un_cd_abandonado_es_rebobinar() {
        assert_eq!(rewind_for(&Cd::Cancelled), Rewind::Step);
    }

    /// Y la ÚNICA que no toca el rastro: el TOFU va a reanudar ESTA misma
    /// navegación (el modal carga el pane y el modo de rastro), así que
    /// rebobinar contaría dos veces el reintento que sí funcione.
    #[test]
    fn la_politica_ante_un_cd_suspendido_es_no_tocar_el_rastro() {
        assert_eq!(rewind_for(&Cd::Suspended), Rewind::No);
    }

    /// Un cd que SÍ movió el pane no debe rebobinar nada, ni los desenlaces
    /// que llegan de otros caminos (refresh, swap) y jamás salen de un paso
    /// del rastro.
    #[test]
    fn un_cd_que_aterriza_no_rebobina() {
        assert_eq!(rewind_for(&Cd::Replaced(0)), Rewind::No);
        assert_eq!(rewind_for(&Cd::Refreshed([true, true])), Rewind::No);
        assert_eq!(rewind_for(&Cd::Swapped), Rewind::No);
    }

    /// Un paso atrás que ATERRIZA en un cd fallido no puede dejar el rastro
    /// contando un movimiento que nunca ocurrió: el lector sigue donde
    /// estaba. Se rebobina ENTERO — el destino vuelve al rastro y el
    /// «adelante» no se queda con un fantasma que devolvería al lector al
    /// sitio del que no se ha movido.
    #[test]
    fn un_paso_atras_fallido_se_rebobina_entero() {
        let mut app = app_en("mem:///c", "mem:///otro");
        app.set_focus(0);
        app.history[0].record(vp("mem:///a"));
        app.history[0].record(vp("mem:///b"));
        let dir = back_target(&mut app).expect("hay rastro");
        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (1, 1)
        );

        let outcome = Cd::Failed(Error::PermissionDenied);
        rewind_trail(&mut app, 0, TrailStep::Back, &dir, rewind_for(&outcome));

        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (2, 0),
            "el rastro queda exactamente como estaba"
        );
        assert_eq!(
            back_target(&mut app),
            Some(dir),
            "y el mismo destino sigue disponible para reintentarlo"
        );
    }

    /// Simétrico: un paso ADELANTE fallido se rebobina igual.
    #[test]
    fn un_paso_adelante_fallido_se_rebobina_entero() {
        let mut app = app_en("mem:///c", "mem:///otro");
        app.set_focus(0);
        app.history[0].record(vp("mem:///b"));
        let _ = back_target(&mut app); // rastro: back=[], fwd=[c]
        poner_en(&mut app, "mem:///b");
        let dir = forward_target(&mut app).expect("hay rama de delante");
        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (1, 0)
        );

        let outcome = Cd::Failed(Error::PermissionDenied);
        rewind_trail(&mut app, 0, TrailStep::Forward, &dir, rewind_for(&outcome));

        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (0, 1)
        );
        assert_eq!(forward_target(&mut app), Some(dir));
    }

    /// El caso de esta revisión: `Esc` durante el listado del paso. El lector
    /// sigue en C, así que el rastro tiene que quedarse como estaba. Con el
    /// paso dado por bueno, el «adelante» siguiente cd-ea al directorio que
    /// ya está en pantalla (una tecla que no hace nada visible) y `back`
    /// hereda un fantasma que se come el siguiente `nav.back`.
    #[test]
    fn un_paso_abandonado_con_esc_no_deja_fantasma_en_el_rastro() {
        let mut app = app_en("mem:///c", "mem:///otro");
        app.set_focus(0);
        app.history[0].record(vp("mem:///a"));
        app.history[0].record(vp("mem:///b"));
        let dir = back_target(&mut app).expect("hay rastro");

        // El pane NO se movió: sigue en C (`poner_en` no se llama).
        rewind_trail(
            &mut app,
            0,
            TrailStep::Back,
            &dir,
            rewind_for(&Cd::Cancelled),
        );

        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (2, 0),
            "el rastro queda como estaba: nadie se fue de C"
        );
        assert_eq!(
            back_target(&mut app),
            Some(vp("mem:///b")),
            "y el mismo destino sigue ahí para reintentarlo"
        );
    }

    /// El TOFU es la excepción: el modal REANUDA esta misma navegación, así
    /// que el paso ya dado se queda dado — rebobinarlo haría que el reintento
    /// exitoso contara dos veces.
    #[test]
    fn un_paso_suspendido_por_el_tofu_conserva_el_paso() {
        let mut app = app_en("mem:///c", "mem:///otro");
        app.set_focus(0);
        app.history[0].record(vp("mem:///a"));
        app.history[0].record(vp("mem:///b"));
        let dir = back_target(&mut app).expect("hay rastro");

        rewind_trail(
            &mut app,
            0,
            TrailStep::Back,
            &dir,
            rewind_for(&Cd::Suspended),
        );

        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (1, 1),
            "el paso sigue dado: lo terminará el reintento"
        );
    }

    // --- El paso que sobrevive a quien lo empezó: TOFU (`Modal::TrustHostKey`).
    // `walk_trail` devuelve `Suspended` SIN rebobinar porque el reintento iba
    // a terminar el paso; estos tres pinchan quién lo termina de verdad, con
    // la misma pareja `rewind_for`/`rewind_trail` que corre en producción.

    /// El lector anduvo A→B→C→D, dio UN paso atrás (ya completo: está en C,
    /// con D en la rama de delante) y el SIGUIENTE paso atrás —hacia B— se
    /// queda suspendido en el modal TOFU. Devuelve la app y el destino del
    /// paso suspendido.
    ///
    /// La rama de delante previa no es decorado: es lo que distingue un
    /// rebobinado del rastro de dos. Con `fwd` vacío el segundo rebobinado
    /// sería un no-op y ningún test lo vería; con la rama del lector debajo,
    /// se la come.
    fn app_con_paso_suspendido() -> (App, VPath) {
        let mut app = app_en("mem:///d", "mem:///otro");
        app.set_focus(0);
        for dir in ["mem:///a", "mem:///b", "mem:///c"] {
            app.history[0].record(vp(dir));
        }
        // Un `nav.back` anterior YA completado: rastro back=[a,b], fwd=[d].
        let c = back_target(&mut app).expect("hay rastro");
        assert_eq!(c, vp("mem:///c"));
        poner_en(&mut app, "mem:///c");
        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (2, 1)
        );

        // Y AHORA el paso que el TOFU suspende: el pane NO se mueve.
        let dir = back_target(&mut app).expect("hay rastro");
        assert_eq!(dir, vp("mem:///b"));
        // Lo que hace `walk_trail` ante un `Suspended`: NADA, a propósito —
        // cuenta con que quien responda al modal termine el paso.
        rewind_trail(
            &mut app,
            0,
            TrailStep::Back,
            &dir,
            rewind_for(&Cd::Suspended),
        );
        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (1, 2),
            "el paso está dado y pendiente de terminar"
        );
        (app, dir)
    }

    /// El reintento ATERRIZA: el pane se movió de verdad, así que no se
    /// rebobina nada — el paso que `walk_trail` dio es el paso que ocurrió.
    #[test]
    fn un_reintento_que_aterriza_deja_el_paso_dado_y_no_lo_registra() {
        let (mut app, dir) = app_con_paso_suspendido();
        let antes = (app.history[0].back_len(), app.history[0].fwd_len());
        let mru_antes: Vec<VPath> = app.history[0].entries().iter().cloned().collect();
        // El modal TRANSPORTA el rastro de la navegación interrumpida, y el
        // reintento se lo pasa a `cd_in` tal cual: sigue siendo un `Replay`.
        let trail = Trail::Replay(TrailStep::Back);

        record_step(&mut app.history[0], &vp("mem:///c"), &dir, trail); // lo que hace el cd_in del reintento
        settle_suspended_trail(&mut app, 0, &dir, trail, &Cd::Replaced(0));

        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            antes,
            "el paso ya estaba contado: terminarlo no lo cuenta otra vez"
        );
        assert_eq!(
            app.history[0].entries().iter().cloned().collect::<Vec<_>>(),
            mru_antes,
            "y un reintento que aterriza sigue siendo un Replay: no entra en la MRU"
        );
    }

    /// El reintento FALLA (o el lector deniega la clave, o confiar falla): la
    /// navegación muere sin que el pane se moviera nunca. El paso vuelve
    /// EXACTAMENTE una vez — rebobinarlo dos veces se comería la rama de
    /// delante que el lector ya tenía.
    #[test]
    fn un_reintento_abandonado_rebobina_el_paso_exactamente_una_vez() {
        let (mut app, dir) = app_con_paso_suspendido();
        let (back_dado, fwd_dado) = (app.history[0].back_len(), app.history[0].fwd_len());

        settle_suspended_trail(
            &mut app,
            0,
            &dir,
            Trail::Replay(TrailStep::Back),
            &Cd::Cancelled,
        );

        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (back_dado + 1, fwd_dado - 1),
            "el paso vuelve UNA vez: uno de más detrás, uno de menos delante \
             (dos rebobinados darían (3, 0) y se comerían la rama del lector)"
        );
        assert_eq!(
            back_target(&mut app),
            Some(dir),
            "y el mismo destino sigue disponible para reintentarlo"
        );
        assert_eq!(
            app.history[0].fwd_len(),
            2,
            "con la rama de delante que el lector ya tenía intacta debajo"
        );
    }

    /// El reintento se topa con OTRA clave desconocida: vuelve a suspenderse.
    /// No se rebobina (el modal nuevo carga el mismo rastro y el mismo paso,
    /// así que sigue habiendo quien lo termine) y tampoco se registra nada —
    /// sigue siendo un `Replay`.
    #[test]
    fn un_reintento_que_vuelve_a_suspenderse_no_rebobina_ni_registra() {
        let (mut app, dir) = app_con_paso_suspendido();
        let antes = (app.history[0].back_len(), app.history[0].fwd_len());
        let mru_antes: Vec<VPath> = app.history[0].entries().iter().cloned().collect();

        settle_suspended_trail(
            &mut app,
            0,
            &dir,
            Trail::Replay(TrailStep::Back),
            &Cd::Suspended,
        );
        // Y lo que el `cd_in` del reintento hace con el rastro: nada.
        record_step(
            &mut app.history[0],
            &vp("mem:///c"),
            &dir,
            Trail::Replay(TrailStep::Back),
        );

        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            antes,
            "el paso sigue pendiente de terminar, ni rebobinado ni duplicado"
        );
        assert_eq!(
            app.history[0].entries().iter().cloned().collect::<Vec<_>>(),
            mru_antes,
            "y un Replay no entra en la MRU por reintentarse"
        );
    }

    /// Un cd NORMAL que se topa con el TOFU no tiene paso que rebobinar: no
    /// salió del rastro. `Trail::Record` lo dice, y `settle` no toca nada.
    #[test]
    fn un_cd_normal_suspendido_no_tiene_paso_que_rebobinar() {
        let (mut app, dir) = app_con_paso_suspendido();
        let antes = (app.history[0].back_len(), app.history[0].fwd_len());

        settle_suspended_trail(&mut app, 0, &dir, Trail::Record, &Cd::Cancelled);

        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            antes,
            "una navegación que no salió del rastro no le debe nada"
        );
    }

    /// Y si el destino resultó NO EXISTIR, además de rebobinar se RETIRA de
    /// todo el historial — el mismo trato que ya le da el popup a un
    /// `NotFound`. Sin esto `nav.back` seguiría apuntando a un directorio que
    /// acaba de demostrar que no está, y la tecla solo podría fallar.
    #[test]
    fn un_destino_notfound_desaparece_del_rastro_entero() {
        let mut app = app_en("mem:///c", "mem:///otro");
        app.set_focus(0);
        app.history[0].record(vp("mem:///a"));
        app.history[0].record(vp("mem:///b"));
        let dir = back_target(&mut app).expect("hay rastro");

        let outcome = Cd::Failed(Error::NotFound);
        rewind_trail(&mut app, 0, TrailStep::Back, &dir, rewind_for(&outcome));

        assert_eq!(
            back_target(&mut app),
            Some(vp("mem:///a")),
            "el atrás salta al siguiente vivo, no reintenta el dir muerto"
        );
        assert!(
            !app.history[0].entries().contains(&dir),
            "y tampoco sigue en la MRU que pinta el popup"
        );
    }
}

/// La OTRA decisión que `walk_trail` depende de y nadie pinchaba: qué
/// navegaciones entran en el rastro. El guard `trail == Trail::Record` es la
/// única línea que impide que `nav.back` se alimente de su propio rastro.
#[cfg(test)]
mod record_step_tests {
    use super::{Trail, TrailStep, nav, record_step};
    use norte_proto::VPath;

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire de test")
    }

    /// Una navegación del USUARIO deja huella en las dos estructuras: el
    /// rastro que recorre `nav.back` y la MRU que pinta el popup.
    #[test]
    fn una_navegacion_del_usuario_entra_en_el_rastro_y_en_la_mru() {
        let mut h = nav::History::default();
        record_step(&mut h, &vp("mem:///a"), &vp("mem:///b"), Trail::Record);
        assert_eq!(h.back_len(), 1, "un paso en el rastro");
        assert!(h.entries().contains(&vp("mem:///a")), "y en la MRU");
    }

    /// EL guard. Un `Replay` es el rastro recorriéndose a sí mismo: si
    /// registrara, volver de B a A grabaría «estuve en B», el siguiente atrás
    /// devolvería a B, y el lector oscilaría entre dos directorios para
    /// siempre. Borra `&& trail == Trail::Record` de `record_step` y este
    /// test se pone rojo — es su único guardián.
    #[test]
    fn un_replay_no_alimenta_el_rastro() {
        let mut h = nav::History::default();
        record_step(
            &mut h,
            &vp("mem:///b"),
            &vp("mem:///a"),
            Trail::Replay(TrailStep::Back),
        );
        assert_eq!(h.back_len(), 0, "un paso atrás jamás produce rastro");
        assert!(
            h.entries().is_empty(),
            "ni entra en la MRU: volver no es visitar un sitio nuevo"
        );
    }

    /// Un cd al MISMO dir (refresh-like) no es un paso que el lector diera:
    /// registrarlo haría que el siguiente `nav.back` no hiciera nada visible.
    #[test]
    fn un_cd_al_mismo_dir_no_es_un_paso() {
        let mut h = nav::History::default();
        record_step(&mut h, &vp("mem:///a"), &vp("mem:///a"), Trail::Record);
        assert_eq!(h.back_len(), 0);
        assert!(h.entries().is_empty());
    }
}

#[cfg(test)]
mod parse_plugin_key_tests {
    use super::parse_plugin_key;

    #[test]
    fn separa_plugin_id_y_command_id() {
        assert_eq!(
            parse_plugin_key("plugin:org.norte.demo:greet"),
            Some(("org.norte.demo", "greet"))
        );
    }

    /// El `command_id` NO tiene charset validado (a diferencia del
    /// `plugin_id`): puede llevar `:` o saltos de línea, y el split se
    /// queda con TODO lo que sigue al primero, sin volver a partir.
    #[test]
    fn command_id_hostil_se_toma_entero_sin_repartir() {
        assert_eq!(
            parse_plugin_key("plugin:org.norte.demo:a:b\nc"),
            Some(("org.norte.demo", "a:b\nc"))
        );
    }

    #[test]
    fn sin_prefijo_plugin_es_none() {
        assert_eq!(parse_plugin_key("app.quit"), None);
        assert_eq!(parse_plugin_key(""), None);
    }

    /// Sin el segundo `:` (formato mínimo `plugin:x` sin `command_id`): `None`
    /// — un despacho parcial jamás corre `plugin_run_command` con un id
    /// vacío o adivinado.
    #[test]
    fn sin_segundo_separador_es_none() {
        assert_eq!(parse_plugin_key("plugin:org.norte.demo"), None);
    }

    /// `plugin_id` vacío (`"plugin::greet"`) es `None` — nunca alcanzable
    /// desde una fila real (`PluginInfo.id` siempre no-vacío, validado por
    /// el core), pero el parser no debe entregar un id vacío a
    /// `plugin_run_command` si alguna vez lo fuera.
    #[test]
    fn plugin_id_vacio_es_none() {
        assert_eq!(parse_plugin_key("plugin::greet"), None);
    }
}

fn viewer_do(app: &mut App, f: impl FnOnce(&mut Viewer)) {
    if let Some(v) = &mut app.viewer {
        f(v);
    }
}

/// Presupuesto de lectura del viewer: cabecera de 256 KiB (el resto del
/// archivo NO se lee — rango de ADR 0005; «cargar más» = deuda de M2).
/// OJO si esto crece (>~1 MiB): `Viewer::recompute` y `rows()` corren en
/// el hilo del loop — harían falta `spawn_blocking` + índice de líneas.
const VIEW_CAP: u64 = 256 * 1024;

/// Abre el viewer leyendo la CABECERA vía el core (regla 7), cancelable
/// como el cd (Esc abandona, Ctrl-C sale).
async fn open_viewer(app: &mut App, backend: &Backend, events: &mut EventStream, path: VPath) {
    // Fase 1 leer la cabecera, fase 2 (M4-P5) intentar el preview de un plugin.
    // Ambas van dentro de la MISMA future para que Esc/Ctrl-C cancelen en
    // cualquiera de las dos. Un fallo del preview NUNCA impide ver el crudo.
    let fut = async {
        let (bytes, truncated) = read_head(backend, &path).await?;
        // G3a (ADR 0037): intenta el preview CON ESTILO primero; `Ok(None)`
        // (ningún previewer aplica, un guest cayó, o los topes del wire se
        // violaron — todos degradan igual, ver `Backend::
        // plugin_preview_styled`) cae al preview PLANO clásico, que a su
        // vez cae a la vista cruda si tampoco aplica. Un fallo de RED (no
        // `Ok`) en el intento estilizado tampoco bloquea: se trata igual
        // que `None` y se reintenta con el plano (mismo criterio que ya
        // regía para el plano frente a la vista cruda).
        let viewer = match backend.plugin_preview_styled(&path).await {
            Ok(Some(p)) => {
                Viewer::with_plugin_preview_styled(path.clone(), p.plugin_name, &p.lines, p.lossy)
            }
            Ok(None) | Err(_) => match backend.plugin_preview(&path).await {
                Ok(res) => match res.preview {
                    Some(p) => {
                        Viewer::with_plugin_preview(path.clone(), p.plugin_name, &p.output, p.lossy)
                    }
                    None => Viewer::new(path.clone(), bytes, truncated),
                },
                // Un plugin roto no bloquea el archivo: vista cruda de siempre.
                Err(_) => Viewer::new(path.clone(), bytes, truncated),
            },
        };
        Ok::<Viewer, Error>(viewer)
    };
    tokio::pin!(fut);
    loop {
        tokio::select! {
            res = &mut fut => {
                match res {
                    Ok(viewer) => app.viewer = Some(viewer),
                    Err(e) => app.message = Some(ta("msg-view-error", &[("error", &error_category(&e))])),
                }
                return;
            }
            maybe = events.next() => {
                match maybe {
                    Some(Ok(Event::Key(key)))
                        if key.kind == crossterm::event::KeyEventKind::Press =>
                    {
                        match (key.code, key.modifiers) {
                            (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                                app.quit = true;
                                return;
                            }
                            (KeyCode::Esc, _) => return,
                            _ => {}
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => return,
                }
            }
        }
    }
}

/// Lee hasta `VIEW_CAP + 1` bytes: el byte extra delata el truncado.
async fn read_head(backend: &Backend, path: &VPath) -> Result<(Vec<u8>, bool), Error> {
    let mut out = backend
        .read(
            path,
            Some(norte_proto::ByteRange {
                offset: 0,
                len: Some(VIEW_CAP + 1),
            }),
        )
        .await?;
    let truncated = out.len() as u64 > VIEW_CAP;
    if truncated {
        out.truncate(usize::try_from(VIEW_CAP).unwrap_or(usize::MAX));
    }
    Ok((out, truncated))
}

/// Pane inicial del arranque: listado COMPLETO de `start` pidiendo los
/// attrs configurados (#117) — sin ellos las celdas attr nacerían en
/// blanco hasta el primer cd/refresh. Regla 7: todo por el `Backend`.
async fn initial_pane(backend: &Backend, start: &VPath, attrs: &[String]) -> Result<Pane> {
    let (entries, skipped) = backend
        .list_with_skipped_attrs(start, attrs)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut pane = Pane::new(start.clone(), entries);
    // #93: las omitidas del contenedor también en el ARRANQUE — el badge no
    // debe nacer vacío teniendo el dato gratis (review #117 tarea 2).
    pane.set_skipped(skipped);
    Ok(pane)
}

/// Listado COMPLETO de `dir` (para `refresh_panes` tras una mutación:
/// conserva el cursor por índice). Una entrada con error corta el listado —
/// mejor un error honesto que un listado silenciosamente incompleto. #54: NO
/// ordena aquí — `refresh_listing`/`PaneState::refill` normalizan
/// internamente, un sort manual sería trabajo duplicado. `attrs` (#117):
/// los ids attr configurados del scheme — pedidos en el `fs.list`; un id
/// no anunciado viene ausente (celda en blanco), jamás es error.
async fn listing(
    backend: &Backend,
    dir: &VPath,
    attrs: &[String],
) -> Result<(Vec<Entry>, Option<u64>), Error> {
    backend.list_with_skipped_attrs(dir, attrs).await
}

/// Cachea en `App` las DOS mitades de una respuesta de `fs.capabilities`
/// (H3d): el catálogo de attrs (#117) y los flags de capacidad.
///
/// Una función y no dos líneas repetidas en el arranque y en el `cd` a
/// propósito: la respuesta trae ambas y guardarlas juntas es el punto entero
/// del cambio — quien tire una mitad aquí paga otra ronda de red por un dato
/// que ya estaba en el proceso, y ese descuido tiene ahora un test
/// (`caps_cache_tests`) en vez de vivir dentro del `select!` del run loop,
/// donde nada lo mira.
fn cache_capabilities(
    app: &mut App,
    dir: &VPath,
    (caps, catalog): (norte_proto::Capabilities, norte_proto::AttrCatalog),
) {
    app.insert_attr_catalog(dir.scheme().to_owned(), catalog);
    app.insert_caps(dir, caps);
}

/// Whether a cd to `dir` still has to ask `fs.capabilities`.
///
/// The two halves of that response are cached with DIFFERENT keys and the gate
/// has to ask about both, which is the whole reason this is a named function
/// and not an `is_none()` inline in [`cd_in`]. The attribute catalogue is per
/// scheme — a wrong column hint is cosmetic. The capability flags are per
/// scheme AND authority (`App::caps`), because a veto that answers for the
/// wrong server is not cosmetic: gating on the catalogue alone meant the first
/// `sftp` host to be visited answered "is this read-only?" for every other
/// `sftp` host of the session, since no second call was ever made.
///
/// So it fetches when EITHER half is missing, and the redundant fetch — a
/// second authority of a scheme whose catalogue is already cached — is one
/// call per connection, which is what asking the connection its own
/// capabilities costs.
fn needs_capabilities(app: &App, dir: &VPath) -> bool {
    app.attr_catalog(dir.scheme()).is_none() || app.caps(dir).is_none()
}

/// Primera página de `dir` (hasta [`FIRST_PAGE`]) más el stream con el RESTO
/// (o `None` si el dir cabía en la primera página) y las omitidas del
/// contenedor (#93). El primer render no espera al listado entero (ADR 0017).
/// Regla 7: el TUI no toca el FS. `attrs`/`fetch_caps` (#117): pide los
/// attrs configurados y, cuando el llamador dice que falta algo por cachear
/// ([`needs_capabilities`]), la respuesta de `fs.capabilities` (cuarto
/// elemento de la tupla).
///
/// H3d: ese cuarto elemento son las DOS mitades de `fs.capabilities` —
/// `Capabilities` y catálogo — porque el wire las trae juntas
/// (`Backend::capabilities_and_attrs`). La TUI cacheaba solo el catálogo y
/// luego preguntaba «¿es de solo lectura?» con otra ronda por un dato que ya
/// había llegado.
async fn first_page(
    backend: &Backend,
    dir: &VPath,
    attrs: &[String],
    fetch_caps: bool,
) -> Result<
    (
        Vec<Entry>,
        Option<EntryStream>,
        Option<u64>,
        Option<(norte_proto::Capabilities, norte_proto::AttrCatalog)>,
    ),
    Error,
> {
    // Las capacidades ANTES del stream (misma conexión, y solo cuando falta
    // algo por cachear — `needs_capabilities`); un fallo NO tumba el cd: sin
    // hints se pinta Opaque y el solo-lectura cae al criterio sintáctico.
    let catalog = if fetch_caps {
        backend.capabilities_and_attrs(dir).await.ok()
    } else {
        None
    };
    let (mut stream, skipped) = backend.list_stream_with(dir, attrs).await?;
    let mut first = Vec::with_capacity(FIRST_PAGE);
    while first.len() < FIRST_PAGE {
        match stream.next().await {
            Some(item) => first.push(item?),
            // El dir cabía en la primera página: no hay resto que drenar.
            None => return Ok((first, None, skipped, catalog)),
        }
    }
    Ok((first, Some(stream), skipped, catalog))
}

/// Arranca el drenador del RESTO del listado: envía lotes coalescidos al run
/// loop, que los aplica con [`Pane::extend_listing`]. Soltar el `rx` (un cd
/// nuevo DEL MISMO PANE) mata el drenador en su próximo envío → suelta el
/// stream (regla 3). Sin `pane`: quién lo recibe lo decide el hueco donde el
/// run loop lo archive (ver [`Fill`]).
fn spawn_fill(mut stream: EntryStream) -> Fill {
    // Bounded a 1: el drenador no corre por delante del run loop más de un
    // lote (backpressure); el pico de memoria es un lote, no todo el dir.
    let (tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
    tokio::spawn(async move {
        let mut batch = Vec::with_capacity(FILL_BATCH);
        let mut flush = tokio::time::interval(FILL_INTERVAL);
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        flush.tick().await; // consume el tick inmediato del interval
        loop {
            tokio::select! {
                item = stream.next() => match item {
                    Some(Ok(e)) => {
                        batch.push(e);
                        if batch.len() >= FILL_BATCH
                            && tx
                                .send(FillMsg::Batch(std::mem::take(&mut batch)))
                                .await
                                .is_err()
                        {
                            return; // el run loop soltó el rx (cd nuevo)
                        }
                    }
                    Some(Err(_)) => {
                        let _ = tx.send(FillMsg::Failed).await;
                        return;
                    }
                    None => {
                        if !batch.is_empty() {
                            let _ = tx.send(FillMsg::Batch(batch)).await;
                        }
                        return; // fin: drop(tx) cierra el canal → finish_listing
                    }
                },
                _ = flush.tick() => {
                    // Vacía un lote PARCIAL (progreso en streams lentos).
                    if !batch.is_empty()
                        && tx
                            .send(FillMsg::Batch(std::mem::take(&mut batch)))
                            .await
                            .is_err()
                    {
                        return;
                    }
                }
            }
        }
    });
    Fill { rx }
}

/// cd CANCELABLE (regla 3): el listado corre contra el stream de eventos —
/// Esc lo abandona (el pane se queda donde estaba) y Ctrl-C sale del TUI
/// (atajos FIJOS durante un cd: aquí no aplica el keymap — son la salida de
/// emergencia y no deben ser remapeables a algo que no exista). Soltar el
/// future del listado detiene al productor del provider (testeado en
/// vfs-local). El resto de teclas se descartan mientras dura el cd.
async fn cd(app: &mut App, backend: &Backend, events: &mut EventStream, dir: VPath) -> Cd {
    cd_in(app, backend, events, app.focus(), dir, Trail::Record).await
}

/// The ONE place that decides whether a navigation joins the pane's trail.
///
/// Two conditions, both load-bearing, both easy to lose in the middle of the
/// success arm of [`cd_in`] where they used to live:
///
/// - `prev != dir`: a cd onto the directory the pane is ALREADY showing (a
///   refresh-like navigation) is not a step the reader took. Recording it
///   would make the next `nav.back` do nothing visible. The MRU's consecutive
///   dedup covers the rest of the redundancies.
/// - `trail == Trail::Record`: a `Trail::Replay` is the trail walking ITSELF.
///   Recording there feeds the trail its own steps — going back from B to A
///   would log "I was at B", so the next `nav.back` returns to B and the
///   reader oscillates between two directories forever. This is the single
///   line that stops `nav.back` from doing that, and it is pinned by
///   `record_step_tests::un_replay_no_alimenta_el_rastro`.
fn record_step(h: &mut nav::History, prev: &VPath, dir: &VPath, trail: Trail) {
    if prev != dir && trail == Trail::Record {
        h.record(prev.clone());
    }
}

/// Navega `pane` — que NO tiene por qué ser el enfocado, porque
/// `pane.mirror` manda el OTRO pane a un sitio mientras el foco se queda
/// quieto. `trail` dice si el movimiento se REGISTRA en el rastro del pane o
/// es el rastro reproduciéndose ([`Trail`]).
///
/// Todo lo que aquí toca estado de pane va por el PARÁMETRO `pane`
/// (`app.panes[pane]`), jamás por `app.focused()`: son la misma cosa solo
/// mientras el llamante sea el envoltorio [`cd`].
async fn cd_in(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    pane: usize,
    dir: VPath,
    trail: Trail,
) -> Cd {
    // Historial (spec 2026-07-18): el dir ANTERIOR se captura AQUÍ y se
    // empuja solo en el brazo de ÉXITO (el pane se reemplazó de verdad).
    // Al vivir dentro de `cd_in` cubre TODOS los caminos que navegan —
    // nav.enter/nav.parent, quick-Enter (dispatch nav.enter), retry TOFU y
    // los popups de historial/hotlist — sin repetirlo por call-site.
    let prev = app.panes[pane].dir().clone();
    // #117: los attrs CONFIGURADOS del scheme de destino se piden en el
    // listado; el catálogo del provider se trae UNA vez por scheme y sesión
    // (cache en `App::attr_catalogs` — hints y cabeceras del render), y las
    // caps una vez por CONEXIÓN (ver `needs_capabilities`).
    let attrs = app.columns.attr_ids_for(dir.scheme());
    let fetch_caps = needs_capabilities(app, &dir);
    let fut = first_page(backend, &dir, &attrs, fetch_caps);
    tokio::pin!(fut);
    loop {
        tokio::select! {
            res = &mut fut => {
                match res {
                    Ok((first, stream, skipped, catalog)) => {
                        // #117: el catálogo recién llegado se cachea por
                        // scheme — los frames siguientes ya pintan con hints.
                        // H3d: y las caps de la MISMA respuesta, que es lo
                        // que responde «¿este pane es de solo lectura?» sin
                        // otra ronda (`App::pane_read_only`).
                        if let Some(both) = catalog {
                            cache_capabilities(app, &dir, both);
                        }
                        // #54: NO ordenamos aquí — `begin_listing` ->
                        // `PaneState::set_listing` normaliza internamente.
                        let more = stream.is_some();
                        app.panes[pane].begin_listing(dir.clone(), first, more, skipped);
                        record_step(&mut app.history[pane], &prev, &dir, trail);
                        // Si queda stream, un drenador lo rellena en background.
                        return match stream {
                            Some(s) => Cd::Filling {
                                pane,
                                fill: spawn_fill(s),
                            },
                            None => Cd::Replaced(pane),
                        };
                    }
                    // Primer contacto TOFU (#45): en vez de una línea de
                    // error con la huella, abre el modal de confianza — `y`
                    // confía y REINTENTA esta misma navegación.
                    Err(Error::HostKeyUnknown {
                        host,
                        port,
                        algo,
                        fingerprint,
                    }) => {
                        // El modal CARGA `pane` y `trail`: el reintento debe
                        // reanudar ESTA navegación (este pane, este modo de
                        // rastro), no una nueva contra el foco de entonces.
                        app.modal = Some(Modal::TrustHostKey {
                            host,
                            port,
                            algo,
                            fingerprint,
                            dir: dir.clone(),
                            pane,
                            trail,
                        });
                        // El pane NO se tocó (solo se abrió el modal): como
                        // `Cancelled`, conserva un relleno en vuelo del listado
                        // anterior, que sigue siendo válido (MINOR del
                        // rust-reviewer). Pero SUSPENDED y no `Cancelled`: esta
                        // navegación va a CONTINUAR en el retry del modal, y
                        // quien recorre el rastro tiene que distinguirla de un
                        // cd abandonado, que no vuelve.
                        return Cd::Suspended;
                    }
                    // Un error de listado NO tumba el TUI: el pane se queda,
                    // pero un relleno previo de ESTE pane ya no aplica. El
                    // error se PORTA en el desenlace (popup de historial).
                    Err(e) => {
                        app.message = Some(error_message(&e));
                        return Cd::Failed(e);
                    }
                }
            }
            maybe = events.next() => {
                match maybe {
                    Some(Ok(Event::Key(key)))
                        if key.kind == crossterm::event::KeyEventKind::Press =>
                    {
                        match (key.code, key.modifiers) {
                        (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                            app.quit = true;
                            return Cd::Cancelled;
                        }
                            (KeyCode::Esc, _) => return Cd::Cancelled,
                            _ => {}
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => return Cd::Cancelled,
                }
            }
        }
    }
}

#[cfg(test)]
mod open_tests {
    use super::{App, Pane, resolve_opener};
    use norte_proto::{Entry, EntryKind, Segment, VPath};

    fn pane_con(nombre: &str) -> Pane {
        let dir = VPath::parse("file:///d").expect("wire de test");
        let entry = Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(nombre.as_bytes().to_vec()).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        };
        Pane::new(dir, vec![entry])
    }

    /// Sin regla en `ns.toml` para el mimetype, F4 cae en el lanzador del
    /// escritorio en vez de rendirse con un mensaje. Antes esto obligaba a
    /// escribir configuración para abrir un PDF.
    #[test]
    fn sin_opener_declarado_cae_en_el_lanzador_del_sistema() {
        let mut app = App::new(pane_con("informe.pdf"), pane_con("otro.txt"));
        resolve_opener(&mut app);
        let pending = app.pending_open.expect("F4 resuelve algo que lanzar");
        assert!(
            pending.detached,
            "el lanzador del escritorio no suspende la TUI"
        );
        assert_eq!(pending.argv.len(), 2, "binario + fichero, sin shell");
        assert!(
            pending.argv[1].to_string_lossy().ends_with("informe.pdf"),
            "abre el fichero bajo el cursor: {:?}",
            pending.argv
        );
        assert!(app.message.is_none(), "y no deja un error en la barra");
    }

    /// Un opener declarado sigue mandando, y ese SÍ se queda con la terminal
    /// (puede ser `bat` o un editor).
    #[test]
    fn un_opener_declarado_gana_y_toma_la_terminal() {
        let mut app = App::new(pane_con("notas.txt"), pane_con("x.txt"));
        app.openers = norte_frontend::openers::OpenersConfig::parse(
            "[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"%f\"]\n",
        )
        .expect("config de test");
        resolve_opener(&mut app);
        let pending = app.pending_open.expect("F4 resuelve el opener declarado");
        assert_eq!(pending.program, "bat");
        assert!(!pending.detached);
    }

    /// Un fichero remoto no tiene ruta nativa: ni opener declarado ni
    /// lanzador del sistema pueden abrirlo, y el usuario debe enterarse.
    #[test]
    fn un_fichero_remoto_no_lanza_nada() {
        let dir = VPath::parse("sftp://host/d").expect("wire de test");
        let entry = Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(b"a.pdf".to_vec()).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        };
        let mut app = App::new(Pane::new(dir.clone(), vec![entry]), Pane::new(dir, vec![]));
        resolve_opener(&mut app);
        assert!(app.pending_open.is_none());
        assert!(app.message.is_some(), "lo dice en la barra");
    }
}

#[cfg(test)]
mod help_freeze_tests {
    use super::{App, Pane, open_contextual_help};
    use norte_help::ChordResolver as _;
    use norte_proto::VPath;

    /// Abrir la ayuda CONGELA los hechos del contexto (H3d).
    ///
    /// El resto de la cadena —la tabla compartida, el resolver, el pintor de la
    /// razón— tiene sus propios tests y seguiría VERDE con esta llamada
    /// borrada: el overlay se pintaría contra el resolver permisivo del
    /// arranque y ninguna fila se atenuaría jamás. Este test es el único que
    /// mira el eslabón.
    ///
    /// Se abre desde dentro de un zip (`READ_ONLY` por el scheme, ADR 0018) con
    /// los dos panes ahí: sin destino escribible, `pane.copy` no puede correr.
    #[test]
    fn abrir_la_ayuda_congela_los_hechos_del_contexto() {
        let dentro = VPath::parse("zip+file:///a.zip/!").expect("wire de test");
        let mut app = App::new(
            Pane::new(dentro.clone(), Vec::new()),
            Pane::new(dentro, Vec::new()),
        );
        assert!(
            app.help_chords.availability("pane.copy").is_available(),
            "antes de abrir, el resolver del arranque no atenúa nada"
        );

        open_contextual_help(&mut app, norte_help::Lang::En, &[], None);

        assert!(app.help.is_some(), "el overlay se abrió");
        assert_eq!(
            app.help_chords.availability("pane.copy").reason(),
            Some(norte_help::Reason::ReadOnlyBackend),
            "la ayuda tiene que saber que está dentro de un archivo"
        );
    }
}

#[cfg(test)]
mod caps_cache_tests {
    use super::{App, Pane, cache_capabilities, needs_capabilities};
    use norte_proto::VPath;

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire de test")
    }

    fn app_en(dir: &VPath) -> App {
        App::new(
            Pane::new(dir.clone(), Vec::new()),
            Pane::new(dir.clone(), Vec::new()),
        )
    }

    /// H3d: `fs.capabilities` devuelve catálogo Y flags en una respuesta, y
    /// las dos mitades se cachean. La que se tiraba era la de los flags, y
    /// tirarla costaba una ronda de red extra la próxima vez que alguien
    /// preguntase si el pane era de solo lectura.
    #[test]
    fn se_cachean_las_dos_mitades_de_una_respuesta() {
        let dir = vp("mem:///");
        let mut app = app_en(&dir);
        let caps = norte_proto::Capabilities {
            flags: norte_proto::CapabilityFlags::READ_ONLY,
            max_path: None,
        };
        let catalog = norte_proto::AttrCatalog::new(Vec::new());

        cache_capabilities(&mut app, &dir, (caps, catalog));

        assert_eq!(app.caps(&dir), Some(&caps), "los flags se quedaron");
        assert!(
            app.attr_catalog("mem").is_some(),
            "y el catálogo, que es la mitad que ya se guardaba"
        );
        // Y el efecto que la ayuda consume: con el flag puesto, el pane es de
        // solo lectura sin volver a preguntar a nadie.
        assert!(app.pane_read_only(0));
    }

    /// MAJOR-1, la otra mitad: la puerta que decide si se pregunta tiene que
    /// preguntar lo MISMO que responde el caché. Gateada solo por el catálogo
    /// —que es por scheme—, un `cd` a un segundo host de `sftp` no volvía a
    /// llamar jamás, así que las caps del primero contestaban por él durante
    /// toda la sesión.
    #[test]
    fn otra_authority_del_mismo_scheme_vuelve_a_preguntar() {
        let a = vp("sftp://a.org/");
        let b = vp("sftp://b.org/");
        let mut app = app_en(&a);
        assert!(needs_capabilities(&app, &a), "sin nada cacheado, se pide");

        cache_capabilities(
            &mut app,
            &a,
            (
                norte_proto::Capabilities {
                    flags: norte_proto::CapabilityFlags::READ_ONLY,
                    max_path: None,
                },
                norte_proto::AttrCatalog::new(Vec::new()),
            ),
        );

        assert!(
            !needs_capabilities(&app, &a),
            "al mismo host no se le pregunta dos veces"
        );
        assert!(
            needs_capabilities(&app, &b),
            "b.org no ha contestado nunca: hay que preguntarle a ÉL"
        );
    }

    /// La costura entera, contra un backend REAL: `first_page` con la puerta
    /// abierta trae las caps y el `cd` las guarda.
    ///
    /// Sin esto, el cableado podía revertirse en silencio y la suite quedaba
    /// verde: `App::pane_read_only` cae al criterio SINTÁCTICO del scheme
    /// cuando no hay caps, y hoy los dos coinciden en todo provider que
    /// existe. Ninguna otra prueba distingue «llegaron los flags» de «el
    /// scheme lo parecía».
    #[tokio::test]
    async fn la_primera_pagina_trae_las_caps_y_el_cd_las_guarda() {
        use norte_core::backend::Backend;
        use std::sync::Arc;

        let engine = norte_core::Engine::new();
        engine.register_provider(Arc::new(norte_testkit::MemProvider::new()));
        let backend = Backend::Embedded(Arc::new(engine));
        let dir = vp("mem:///");

        let (_first, _stream, _skipped, both) = super::first_page(&backend, &dir, &[], true)
            .await
            .expect("el listado del provider de memoria");
        let both = both.expect("con la puerta abierta llegan las DOS mitades");

        let mut app = app_en(&dir);
        assert!(app.caps(&dir).is_none());
        cache_capabilities(&mut app, &dir, both);
        assert!(
            app.caps(&dir).is_some(),
            "las caps de la respuesta tienen que quedarse en el caché"
        );

        // Y con la puerta CERRADA no se pregunta: el cuarto elemento es None.
        let (_f, _s, _k, ninguna) = super::first_page(&backend, &dir, &[], false)
            .await
            .expect("el listado igual");
        assert!(
            ninguna.is_none(),
            "con la puerta cerrada no hay ronda extra"
        );
    }
}

#[cfg(test)]
mod search_fill_tests {
    use super::{App, Fill, FillMsg, Pane, apply_fill_msg};
    use norte_proto::{Entry, EntryKind, Segment, VPath};

    fn vp(w: &str) -> VPath {
        VPath::parse(w).expect("wire de test")
    }

    fn file(dir: &VPath, name: &str) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(name.as_bytes().to_vec()).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        }
    }

    /// review MAJOR T6: un dir grande PAGINÁNDOSE (fill vivo) + `Alt+F7` sobre
    /// ese pane → `begin_search` lo marca virtual y lo vacía; un lote POSTERIOR
    /// del drenador del listado REAL jamás debe entrar en el pane virtual (se
    /// colaría como hit — el propio root de la búsqueda entre los resultados).
    #[test]
    fn fill_no_contamina_el_pane_virtual() {
        let root = vp("file:///d");
        let mut app = App::new(
            Pane::new(root.clone(), vec![]),
            Pane::new(root.clone(), vec![]),
        );
        // Relleno paginado vivo del pane 0 (dir aún cargándose).
        let (_tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
        let mut fill = [Some(Fill { rx }), None];
        // Alt+F7 sobre el pane 0: pasa a virtual y se vacía.
        app.panes[0].begin_search(root.clone());
        // Llega un lote del drenador del listado REAL.
        apply_fill_msg(
            &mut app,
            &mut fill,
            0,
            Some(FillMsg::Batch(vec![
                file(&root, "real1"),
                file(&root, "real2"),
            ])),
        );
        assert!(
            app.panes[0].entries().is_empty(),
            "el listado real NO entra en el pane virtual"
        );
        assert!(fill[0].is_none(), "el fill obsoleto se suelta");
        assert!(
            app.panes[0].virtual_search,
            "el pane sigue en modo búsqueda"
        );
    }
}

#[cfg(test)]
mod mirror_fill_tests {
    use super::{
        App, Cd, DecorateFetch, Fill, FillMsg, Pane, Probed, SearchRun, apply_cd, apply_fill_msg,
    };
    use norte_proto::{Entry, EntryKind, Segment, VPath};

    fn vp(w: &str) -> VPath {
        VPath::parse(w).expect("wire de test")
    }

    fn file(dir: &VPath, name: &str) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(name.as_bytes().to_vec()).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        }
    }

    /// Un relleno paginado por PANE, y no uno global: `pane.mirror` manda el
    /// OTRO pane a un sitio SIN mover el foco, así que con un solo hueco basta
    /// una tecla para que el pane que el lector está mirando —el suyo, el
    /// enfocado, aún paginando un dir grande— se quede a medias.
    ///
    /// Soltar su `rx` mata al drenador sin `finish_listing`, y `loading` solo
    /// lo apaga `finish_listing`/`Failed`/un listado nuevo: el pane queda con
    /// el listado truncado bajo un «cargando…» permanente.
    #[test]
    fn un_espejo_al_otro_pane_no_estrangula_el_relleno_del_pane_mirado() {
        let dir = vp("file:///d");
        let mut app = App::new(
            Pane::new(dir.clone(), Vec::new()),
            Pane::new(dir.clone(), Vec::new()),
        );
        // El pane 0 —el enfocado, el que el lector mira— está paginando.
        app.panes[0].begin_listing(dir.clone(), vec![file(&dir, "a")], true, None);
        let (tx0, rx0) = tokio::sync::mpsc::channel::<FillMsg>(1);
        let mut fill = [Some(Fill { rx: rx0 }), None];
        let mut df: [Option<DecorateFetch>; 2] = [None, None];
        let mut lp = Probed::new();

        // `pane.mirror`: el pane 1 viaja, y su listado también viene paginado.
        let (_tx1, rx1) = tokio::sync::mpsc::channel::<FillMsg>(1);
        app.panes[1].begin_listing(dir.clone(), Vec::new(), true, None);
        let mut sr: Option<SearchRun> = None;
        apply_cd(
            &mut fill,
            &mut df,
            &mut lp,
            &mut sr,
            Cd::Filling {
                pane: 1,
                fill: Fill { rx: rx1 },
            },
        );

        // El drenador del pane 0 sigue teniendo a quién enviar: nadie le
        // soltó el `rx` por debajo.
        tx0.try_send(FillMsg::Batch(vec![file(&dir, "b")]))
            .expect("el drenador del pane 0 no fue abandonado");
        let msg = fill[0]
            .as_mut()
            .expect("el relleno del pane 0 sigue en su hueco")
            .rx
            .try_recv()
            .ok();
        apply_fill_msg(&mut app, &mut fill, 0, msg);

        assert_eq!(
            app.panes[0].entries().len(),
            2,
            "el lote posterior entra en el listado del pane 0"
        );
        assert!(
            app.panes[0].loading(),
            "y el «cargando…» sigue vivo: nadie terminó el listado por él"
        );
        assert!(fill[1].is_some(), "el espejo se quedó con SU hueco");
    }
}

#[cfg(test)]
mod apply_cd_tests {
    use super::{Cd, DecorateFetch, Fill, FillMsg, Probed, SearchRun, apply_cd};
    use norte_proto::Error;

    fn fill() -> Fill {
        let (_tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
        Fill { rx }
    }

    /// El hueco del pane 0 ocupado y el del 1 libre: la disposición de
    /// partida de casi todos estos casos.
    fn en_el_pane_0() -> [Option<Fill>; 2] {
        [Some(fill()), None]
    }

    /// Un REEMPLAZO del mismo pane suelta su relleno obsoleto.
    #[test]
    fn replaced_suelta_el_fill_del_pane() {
        let mut f = en_el_pane_0();
        let mut lp = Probed::new();
        let mut df: [Option<DecorateFetch>; 2] = [None, None];
        let mut sr: Option<SearchRun> = None;
        apply_cd(&mut f, &mut df, &mut lp, &mut sr, Cd::Replaced(0));
        assert!(f[0].is_none(), "el fill del listado viejo se suelta");
    }

    /// Un reemplazo de OTRO pane no toca el relleno vivo.
    #[test]
    fn replaced_de_otro_pane_no_toca() {
        let mut f = en_el_pane_0();
        let mut lp = Probed::new();
        let mut df: [Option<DecorateFetch>; 2] = [None, None];
        let mut sr: Option<SearchRun> = None;
        apply_cd(&mut f, &mut df, &mut lp, &mut sr, Cd::Replaced(1));
        assert!(f[0].is_some(), "el fill del pane 0 sobrevive");
    }

    /// #78: un cd FALLIDO NO suelta el relleno — el pane sigue en su listado
    /// anterior, que se sigue rellenando (soltarlo lo colgaba en loading).
    #[test]
    fn failed_conserva_el_fill() {
        let mut f = en_el_pane_0();
        let mut lp = Probed::new();
        let mut df: [Option<DecorateFetch>; 2] = [None, None];
        let mut sr: Option<SearchRun> = None;
        apply_cd(
            &mut f,
            &mut df,
            &mut lp,
            &mut sr,
            Cd::Failed(Error::NotFound),
        );
        assert!(
            f[0].is_some(),
            "el fill del listado anterior sigue vivo tras un cd fallido"
        );
    }

    /// Un cd abandonado no toca nada.
    #[test]
    fn cancelled_conserva_el_fill() {
        let mut f = en_el_pane_0();
        let mut lp = Probed::new();
        let mut df: [Option<DecorateFetch>; 2] = [None, None];
        let mut sr: Option<SearchRun> = None;
        apply_cd(&mut f, &mut df, &mut lp, &mut sr, Cd::Cancelled);
        assert!(f[0].is_some());
    }

    /// Un listado nuevo ocupa el hueco DE SU PANE y solo ese: el relleno del
    /// otro pane sigue drenando. Es lo que hace que `pane.mirror` —que manda
    /// el OTRO pane a un sitio sin mover el foco— no pueda dejar a medias el
    /// pane que el lector está mirando.
    #[test]
    fn filling_de_un_pane_no_toca_el_hueco_del_otro() {
        let mut f = en_el_pane_0();
        let mut lp = Probed::new();
        let mut df: [Option<DecorateFetch>; 2] = [None, None];
        let mut sr: Option<SearchRun> = None;
        apply_cd(
            &mut f,
            &mut df,
            &mut lp,
            &mut sr,
            Cd::Filling {
                pane: 1,
                fill: fill(),
            },
        );
        assert!(f[0].is_some(), "el relleno del pane 0 sigue en su hueco");
        assert!(f[1].is_some(), "y el nuevo ocupa el suyo");
    }

    /// #118: Ctrl+R re-listó el pane 0 (listado COMPLETO nuevo) — su
    /// drenador viejo duplicaría filas si siguiera vivo. La dedup de la
    /// sonda #52 también caduca: el listado nuevo re-lazifica las entries.
    #[test]
    fn refreshed_suelta_el_fill_del_pane_relistado() {
        let mut f = en_el_pane_0();
        let mut lp = Probed::from([(0, norte_proto::VPath::parse("file:///d/x").unwrap())]);
        let mut df: [Option<DecorateFetch>; 2] = [None, None];
        let mut sr: Option<SearchRun> = None;
        apply_cd(
            &mut f,
            &mut df,
            &mut lp,
            &mut sr,
            Cd::Refreshed([true, false]),
        );
        assert!(f[0].is_none(), "el drenador del listado viejo se suelta");
        assert!(lp.is_empty(), "la dedup de la sonda #52 caduca");
    }

    /// #118: Esc a medias — el pane 1 NO llegó a re-listarse, su relleno
    /// paginado sigue siendo válido (#78: soltarlo lo colgaba en loading).
    #[test]
    fn refreshed_a_medias_conserva_el_fill_del_pane_no_relistado() {
        let mut f = [None, Some(fill())];
        let mut lp = Probed::new();
        let mut df: [Option<DecorateFetch>; 2] = [None, None];
        let mut sr: Option<SearchRun> = None;
        apply_cd(
            &mut f,
            &mut df,
            &mut lp,
            &mut sr,
            Cd::Refreshed([true, false]),
        );
        assert!(
            f[1].is_some(),
            "el fill del pane NO re-listado sobrevive al Esc a medias"
        );
    }

    /// Y el simétrico: un refresh de los DOS panes suelta los dos huecos.
    #[test]
    fn refreshed_de_ambos_panes_suelta_los_dos_huecos() {
        let mut f = [Some(fill()), Some(fill())];
        let mut lp = Probed::new();
        let mut df: [Option<DecorateFetch>; 2] = [None, None];
        let mut sr: Option<SearchRun> = None;
        apply_cd(
            &mut f,
            &mut df,
            &mut lp,
            &mut sr,
            Cd::Refreshed([true, true]),
        );
        assert!(f[0].is_none() && f[1].is_none());
    }

    /// #118: refresh totalmente abandonado (Esc antes del primer pane) o
    /// ambos panes en modo virtual: nada cambió, nada se toca.
    #[test]
    fn refreshed_vacio_no_toca_nada() {
        let mut f = en_el_pane_0();
        let mut lp = Probed::from([(0, norte_proto::VPath::parse("file:///d/x").unwrap())]);
        let mut df: [Option<DecorateFetch>; 2] = [None, None];
        let mut sr: Option<SearchRun> = None;
        apply_cd(
            &mut f,
            &mut df,
            &mut lp,
            &mut sr,
            Cd::Refreshed([false, false]),
        );
        assert!(f[0].is_some(), "sin pane re-listado, el fill sigue");
        assert!(!lp.is_empty(), "sin pane re-listado, la dedup sigue");
    }
}

#[cfg(test)]
mod swap_tests {
    use super::{
        App, Cd, DecorateFetch, Fill, FillMsg, Pane, Probed, SearchHits, SearchRun, SearchState,
        TaskRef, apply_cd, reap_search_run, reconcile_swap, watch_targets,
    };
    use norte_proto::VPath;

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire de test")
    }

    /// Una búsqueda viva CORRIENDO sobre `pane`. La Task es sintética (el
    /// `TaskRef` de test del core): aquí no se ejerce el walker, sino el
    /// índice de pane que el run loop guarda a su lado.
    fn search_run(pane: usize) -> SearchRun {
        let (_tx, rx) = tokio::sync::mpsc::channel::<SearchHits>(1);
        let id = norte_proto::TaskId::new(1);
        let (_progreso, prx) = tokio::sync::watch::channel(norte_proto::TaskProgress {
            task_id: id,
            kind: norte_proto::TaskKind::Search,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
        });
        SearchRun {
            task: TaskRef::synthetic_for_tests(id, prx),
            rx,
            pane,
            prev_dir: vp("file:///antes"),
            hits: 0,
            state: SearchState::Running,
        }
    }

    fn decorate(pane: usize) -> DecorateFetch {
        let (_tx, rx) = tokio::sync::oneshot::channel();
        DecorateFetch {
            pane,
            dir: vp("mem:///d"),
            rx,
        }
    }

    /// El relleno EN VUELO está archivado POR PANE: si el intercambio no cruza
    /// los huecos, los lotes del listado siguen llegando al pane de al lado y
    /// el lector ve crecer la lista equivocada. Es el bug que una suite verde
    /// no ve, porque el listado sigue llegando: solo llega al sitio que no es.
    ///
    /// Comprueba que se movió ESE drenador y no un hueco cualquiera: el lote
    /// enviado por el `tx` del pane 0 se recoge del hueco del pane 1.
    #[test]
    fn el_intercambio_cruza_los_huecos_del_relleno_en_vuelo() {
        let (tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
        let mut f = [Some(Fill { rx }), None];
        let mut df: [Option<DecorateFetch>; 2] = [Some(decorate(0)), None];
        let mut lp = Probed::from([(0, vp("mem:///d/x"))]);
        let mut sr: Option<SearchRun> = None;

        reconcile_swap(&mut f, &mut df, &mut lp, &mut sr);

        assert!(f[0].is_none(), "el hueco del pane 0 queda libre");
        tx.try_send(FillMsg::Failed)
            .expect("el drenador sigue vivo");
        assert!(
            f[1].as_mut()
                .expect("cruzado al hueco del pane 1")
                .rx
                .try_recv()
                .is_ok(),
            "y es EL MISMO drenador el que ahora alimenta al pane 1"
        );
        assert!(df[1].is_some() && df[0].is_none(), "cruzados");
        assert!(lp.is_empty(), "la caché de stat se tira, no se traduce");
    }

    /// Sin nada en vuelo el reconciliado es inofensivo: un intercambio no
    /// puede inventar un relleno ni un fetch donde no los había.
    #[test]
    fn el_intercambio_sin_nada_en_vuelo_no_inventa_nada() {
        let mut f: [Option<Fill>; 2] = [None, None];
        let mut df: [Option<DecorateFetch>; 2] = [None, None];
        let mut lp = Probed::new();
        let mut sr: Option<SearchRun> = None;
        reconcile_swap(&mut f, &mut df, &mut lp, &mut sr);
        assert!(f[0].is_none() && f[1].is_none());
        assert!(df[0].is_none() && df[1].is_none());
    }

    /// El desenlace `Cd::Swapped` tiene que LLEGAR al reconciliado: la mitad
    /// del intercambio que `dispatch` no puede hacer viaja por `apply_cd`, y
    /// un brazo que se olvidara de llamarlo dejaría el fill apuntando al pane
    /// que no es sin que ningún test de `App` se enterase.
    #[test]
    fn apply_cd_swapped_reconcilia_el_estado_del_run_loop() {
        let (_tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
        let mut f = [None, Some(Fill { rx })];
        let mut df: [Option<DecorateFetch>; 2] = [None, Some(decorate(1))];
        let mut lp = Probed::from([(1, vp("mem:///d/x"))]);
        let mut sr: Option<SearchRun> = None;
        apply_cd(&mut f, &mut df, &mut lp, &mut sr, Cd::Swapped);
        assert!(f[0].is_some() && f[1].is_none());
        assert!(df[0].is_some() && df[1].is_none());
        assert!(lp.is_empty());
    }

    /// La búsqueda VIVA también está indexada por pane: `SearchRun` guarda el
    /// pane virtual que muestra los hits, exactamente como el relleno guarda
    /// el suyo. Si el intercambio no lo voltea, los hits siguen entrando en el
    /// pane de al lado.
    #[test]
    fn el_intercambio_voltea_el_pane_de_la_busqueda_viva() {
        let mut f: [Option<Fill>; 2] = [None, None];
        let mut df: [Option<DecorateFetch>; 2] = [None, None];
        let mut lp = Probed::new();
        let mut sr = Some(search_run(0));

        reconcile_swap(&mut f, &mut df, &mut lp, &mut sr);

        assert_eq!(
            sr.as_ref().expect("el run sigue vivo").pane,
            1,
            "el pane virtual de la búsqueda cambió de lado con su pane"
        );
    }

    /// Y el intercambio no puede COSECHAR la búsqueda por el camino.
    ///
    /// El `Esc`/`Enter` del pane virtual son las ÚNICAS teclas que ese modo
    /// intercepta, así que un `Ctrl+U` cae al resolutor y cruza los panes con
    /// una búsqueda corriendo. Justo después, el mismo call site pasa por
    /// [`reap_search_run`], que suelta el run cuando su pane ya no es virtual:
    /// con el `pane` sin voltear mira el pane 0 —que ahora tiene el listado
    /// ordinario que vino del otro lado— y CANCELA la Task en silencio,
    /// dejando el pane 1 con hits a medias en `Running` para siempre y sin su
    /// manejador de `Esc` (que exige un run vivo PARA ESE pane).
    ///
    /// Por eso el volteo tiene que ocurrir DENTRO de `reconcile_swap`: pasada
    /// la cosecha ya no hay nada que salvar.
    #[test]
    fn un_intercambio_no_cosecha_la_busqueda_viva() {
        let mut app = App::new(
            Pane::new(vp("file:///izq"), Vec::new()),
            Pane::new(vp("file:///der"), Vec::new()),
        );
        // Búsqueda viva en el pane 0 (el `Alt+F7` lo dejó virtual).
        app.panes[0].begin_search(vp("file:///izq"));
        let mut sr = Some(search_run(0));
        let mut f: [Option<Fill>; 2] = [None, None];
        let mut df: [Option<DecorateFetch>; 2] = [None, None];
        let mut lp = Probed::new();

        // `Ctrl+U`: `dispatch` cruza los panes y el run loop reconcilia…
        app.swap_panes();
        apply_cd(&mut f, &mut df, &mut lp, &mut sr, Cd::Swapped);
        // …y el MISMO call site cosecha a continuación.
        reap_search_run(&app, &mut sr);

        let s = sr.as_ref().expect("la búsqueda en curso NO se cancela");
        assert_eq!(s.pane, 1, "sigue los hits a su nuevo lado");
        assert!(
            app.panes[s.pane].virtual_search,
            "y ese lado es el que está en modo búsqueda"
        );
    }

    /// El watcher NO necesita reconciliado propio, y esto es lo que hace
    /// cierta esa afirmación: el conjunto vigilado se deriva de `app.panes`
    /// en cada vuelta del run loop (`rewatch(&watch_targets(app))` es la
    /// primera sentencia del bucle), así que basta con que `watch_targets`
    /// no cachee nada. Si alguien introdujera una copia por lado, el
    /// intercambio dejaría cada pane vigilando el dir del otro.
    #[test]
    fn watch_targets_sigue_a_los_panes_tras_el_intercambio() {
        let mut app = App::new(
            Pane::new(vp("file:///izq"), Vec::new()),
            Pane::new(vp("file:///der"), Vec::new()),
        );
        let antes = watch_targets(&app);
        app.swap_panes();
        let despues = watch_targets(&app);
        assert_eq!(
            antes[0], despues[1],
            "el dir izquierdo pasa a vigilarse a la derecha"
        );
        assert_eq!(antes[1], despues[0]);
        assert_ne!(antes[0], antes[1], "los dos dirs eran distintos de partida");
    }
}

#[cfg(test)]
mod refresh_ritual_tests {
    use super::{App, Fill, FillMsg, Pane, Probed, SearchRun, after_panes_refresh};
    use norte_proto::VPath;

    fn fill() -> Fill {
        let (_tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
        Fill { rx }
    }

    fn app() -> App {
        let d = VPath::parse("file:///d").expect("wire de test");
        App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
    }

    /// #118 (regresión pedida en el issue): un Esc a medias del refresh
    /// re-listó el pane 0 pero ABANDONÓ el 1 — el ritual solo puede soltar
    /// el drenador del pane re-listado de verdad; el del otro sigue drenando
    /// un listado que sigue siendo el suyo (#78).
    #[test]
    fn esc_a_medias_conserva_el_fill_del_pane_no_refrescado() {
        let mut app = app();
        let mut f = [None, Some(fill())];
        let mut lp = Probed::from([(1, VPath::parse("file:///d/x").unwrap())]);
        let mut sr: Option<SearchRun> = None;
        after_panes_refresh(&mut app, [true, false], &mut f, &mut lp, &mut sr);
        assert!(
            f[1].is_some(),
            "el fill del pane 1 (no re-listado) sobrevive al Esc a medias"
        );
        assert!(lp.is_empty(), "la dedup de la sonda #52 caduca igualmente");
    }

    /// MAJOR-2: congelar impide que un veredicto cambie porque el lector se
    /// MUEVA, y eso está bien. Lo que no puede impedir es que cambie porque el
    /// MUNDO cambie: el brazo del `tick` no lleva guarda de overlay (a
    /// diferencia del de `dir_watch`, gateado por `watch_refresh_allowed`), así
    /// que una copia o un borrado que terminan con la ayuda abierta re-listan
    /// los dos panes y la entrada que los hechos describían puede haberse ido.
    /// La fila decía «no aplica a esta selección» de una selección que ya no
    /// existía.
    #[test]
    fn un_refresh_bajo_la_ayuda_abierta_recongela_los_hechos() {
        use norte_help::ChordResolver as _;

        let d = VPath::parse("file:///d").expect("wire de test");
        let fichero = norte_proto::Entry {
            attrs: std::collections::BTreeMap::new(),
            path: d.join(norte_proto::Segment::new(b"leeme.txt".to_vec()).expect("segmento")),
            kind: norte_proto::EntryKind::File,
            size: Some(3),
            mtime_ms: None,
        };
        let mut app = App::new(
            Pane::new(d.clone(), vec![fichero]),
            Pane::new(d, Vec::new()),
        );
        super::open_contextual_help(&mut app, norte_help::Lang::En, &[], None);
        assert!(
            app.help_chords.availability("pane.view").is_available(),
            "con un fichero bajo el cursor, F3 se puede pulsar"
        );

        // La tarea termina, el refresh entra por debajo del overlay y se lleva
        // por delante la entrada de la que hablaban los hechos.
        app.panes[0].refresh_listing(Vec::new());
        let mut f = [None, None];
        let mut lp = Probed::new();
        let mut sr: Option<SearchRun> = None;
        after_panes_refresh(&mut app, [true, false], &mut f, &mut lp, &mut sr);

        assert_eq!(
            app.help_chords.availability("pane.view").reason(),
            Some(norte_help::Reason::WrongTarget),
            "el listado cambió: los hechos tienen que volver a congelarse"
        );
    }
}
