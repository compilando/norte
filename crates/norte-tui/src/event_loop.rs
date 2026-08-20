//! El bucle de eventos del TUI y lo que puede tumbarlo.
//!
//! Por ahora solo el error. El bucle (`run`, ~2.500 líneas) sigue en el root
//! del binario `ntc` y viene en la ronda siguiente; el error va delante porque
//! es lo que hace posible que venga: la regla 6 prohíbe `anyhow` en una
//! biblioteca, y `run` devolvía `anyhow::Result<()>`.
//!
//! El muro resultó ser de CUATRO líneas —dos `terminal.size()`, un
//! `terminal.draw()` y un `.context("evento de terminal")`—, así que dos
//! variantes lo cubren. Las dos llevan un `std::io::Error` dentro; están
//! separadas porque la información que aportaba aquel `.context` era CUÁL de las
//! dos superficies falló, y un solo `Io(..)` la perdería: no se diagnostica igual
//! una terminal que no se deja medir que un flujo de eventos que se corta.
//!
//! Estos textos NO van por Fluent, y es deliberado: no son cadenas de interfaz
//! sino el diagnóstico que `anyhow` imprime al morir el proceso, que es
//! exactamente el uso que el `.context` anterior les daba.

use crate::app::{App, CompareState, Modal, SearchState, Trail, detail_for_bar, error_category};
use crate::config::{self, Layers};
use crate::config_reload::reload_config;
use crate::dispatch::dispatch;
use crate::fill::{Fill, apply_fill_msg};
use crate::gestures::launch_opener;
use crate::jobs::{
    InFlight, PendingAiPlan, RenameBatchRun, SearchRun, SyncTick, drain_compare, drain_search,
    drain_sync_plan, harvest_sync_apply, launch_compare, launch_sync_apply, launch_sync_plan,
};
use crate::keymap::{Command, Resolver};
use crate::keys::on_key;
use crate::lua::{RunOutcome, load_lua, refresh_lua_status, start_lua_run};
use crate::mouse;
use crate::nav;
use crate::navigate::{apply_cd, cd, cd_in, settle_cd};
use crate::overlays::{fetch_plugin_page, settle_help_over_modal, watch_refresh_allowed};
use crate::paste::route_paste;
use crate::probes::{
    DecorateFetch, Probed, STAT_BATCH_MAX, STAT_WINDOW_RADIUS, spawn_compare_stat_probe,
    spawn_preview_fetch, spawn_stat_probe,
};
use crate::refresh::{after_panes_refresh, on_tick, reap_search_run, refresh_panes};
use crate::screens::{pane_attr_ids, refresh_places_drives};
use crate::session_push::{
    JOURNAL_IDLE, SessionPush, capture_session, drain_notices, push_session,
};
use crate::suspend::run_suspended;
use crate::tty;
use crate::ui;
use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use norte_core::backend::{Backend, ConnEvent};
use norte_frontend::layout::BySlot;
use norte_i18n::{t, ta};
use norte_proto::VPath;

/// Lo que aborta el bucle de eventos.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// La terminal no se dejó medir (`size`) o pintar (`draw`).
    #[error("la terminal no respondió: {0}")]
    Terminal(#[source] std::io::Error),
    /// El flujo de eventos de la terminal se rompió. Un flujo que se AGOTA no
    /// es esto: cerrar la ventana sale por el mismo sitio que un `app.quit`,
    /// para no perder la última foto de la sesión.
    #[error("evento de terminal: {0}")]
    Event(#[source] std::io::Error),
}

/// La frase TRADUCIDA de «esta sesión no queda registrada» (#167/#177).
///
/// El texto de `NoJournal::text()` es para el log del operador y va en crudo;
/// esto es interfaz, y la interfaz de este binario pasa por Fluent.
///
/// Las dos ramas dicen cosas DISTINTAS desde #178: `Busy` es «esto pasó y no
/// quedó anotado» y `Failed` es «esto no ha pasado». Compartir frase era el
/// defecto.
fn journal_warning_i18n(why: &norte_core::embedded::NoJournal) -> String {
    use norte_core::embedded::NoJournal as N;
    match why {
        N::Busy => t("msg-journal-busy"),
        // `detail_for_bar` y no el `Display` crudo: el motivo es el error de
        // `sqlx`/`JournalError`, que trae párrafos enteros (los `Corrupt`) y
        // texto derivado de rutas del entorno. La barra de estado tiene un
        // saneador para exactamente esto y todo lo demás pasa por él.
        N::Failed(motivo) => ta(
            "msg-journal-refused",
            &[("motivo", &crate::app::detail_for_bar(motivo))],
        ),
        // `#[non_exhaustive]`: un motivo nuevo no puede quedarse mudo — si
        // alguna vez lo hay, que al menos salga el texto del core.
        otro => otro.text(),
    }
}

/// Corre un comando y asienta su desenlace: el `cd` que pueda traer
/// ([`settle_cd`]) y la cosecha de la búsqueda viva —regla 3: un cd apaga el
/// pane virtual y su Task tiene que morir con él, y no en la tecla siguiente.
///
/// Lo que NO hace, a propósito, es lanzar el comando externo que `pane.open`
/// pudiera dejar resuelto: eso solo lo hacen los sitios que son ENTRADA del
/// usuario sobre un listado, y se lee en cada uno ([`launch_pending_open`]).
/// El resolver de teclas tampoco pasa por aquí: necesita leer `nav_stalled`
/// ANTES de que el desenlace se consuma, y frena el contador con él.
#[allow(clippy::too_many_arguments)] // wiring del bucle, no API
pub(crate) async fn run_command(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    help_lines: &mut Vec<ratatui::text::Line<'static>>,
    lang: norte_i18n::Lang,
    quick_mode: nav::Mode,
    confirm_quit: config::ConfirmQuit,
    cfg: &config::LoadedConfig,
    fill: &mut BySlot<Fill>,
    decorate_fetch: &mut BySlot<DecorateFetch>,
    last_probed: &mut Probed,
    search_run: &mut Option<SearchRun>,
    cmd: Command,
) {
    let outcome = dispatch(
        app,
        backend,
        events,
        help_lines,
        lang,
        quick_mode,
        confirm_quit,
        cfg,
        cmd,
    )
    .await;
    settle_cd(
        app,
        backend,
        fill,
        decorate_fetch,
        last_probed,
        search_run,
        outcome,
    );
    reap_search_run(app, search_run);
}

/// Lanza el comando externo que `pane.open` (#28) dejara resuelto. Vive en el
/// run loop porque es quien tiene la terminal: abrir un opener la suspende.
pub(crate) async fn launch_pending_open(
    app: &mut App,
    terminal: &mut tty::Tui,
    capture: &mut mouse::Capture,
) {
    if let Some(pending) = app.pending_open.take() {
        app.message = Some(launch_opener(terminal, capture, pending).await);
    }
}

/// El bucle de eventos: dibuja, espera, enruta la tecla y drena lo que las
/// tareas de fondo hayan traído, hasta que `app.quit`.
///
/// Nunca tuvo rustdoc, y hasta este commit no lo necesitaba: era una función
/// privada del root de un binario. Al pasar a `pub` en una biblioteca,
/// `missing_docs` lo exige, y merece la pena decir aquí las tres cosas que su
/// cuerpo repite y ningún llamante puede adivinar.
///
/// **No es dueña de la terminal, la tiene prestada.** `main` la crea y la
/// restaura; aquí se ENCIENDE y se APAGA la captura de ratón en caliente
/// (`[ui] mouse`) y se suelta alrededor de cada suspensión. Si esta función
/// devuelve `Err`, la terminal sigue siendo responsabilidad de `main`, que
/// restaura ANTES de propagar — un bucle roto no es un `--pick` cancelado.
///
/// **La salida limpia pasa por un solo sitio.** `app.quit` es el único camino,
/// y por eso el EOF del terminal (te cierran la ventana) lo enciende en vez de
/// hacer `return`: saliendo antes se perdía la última foto de la sesión.
///
/// **Sus parámetros son estado que una recarga puede sustituir.** Los tres
/// resolvers, `help_lines`, el modo del quick search y `confirm_quit` los toma
/// por `&mut` porque [`crate::config_reload::reload_config`] los reemplaza en
/// caliente, y los reemplaza TODOS o NINGUNO.
///
/// # Errors
///
/// [`RunError`], que son las dos superficies de la terminal que pueden fallar
/// de verdad: medirla o pintarla, y su flujo de eventos. Todo lo demás que sale
/// mal —un listado que no se deja leer, una mutación rechazada, un plugin que
/// no responde— es un mensaje en la barra, no un error del bucle: la regla es
/// que el gestor de ficheros no se cae porque el sistema de ficheros diga no.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)] // wiring del bucle, no API
pub async fn run(
    terminal: &mut tty::Tui,
    // Captura de ratón: la crea `main` (dueño de la terminal) y la retira
    // al salir; aquí se ENCIENDE y se APAGA en caliente (`[ui] mouse`) y se
    // suelta alrededor de cada suspensión por opener externo.
    capture: &mut mouse::Capture,
    app: &mut App,
    backend: &Backend,
    resolver: &mut Resolver,
    viewer_resolver: &mut Resolver,
    dialog_resolver: &mut Resolver,
    help_lines: &mut Vec<ratatui::text::Line<'static>>,
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
    mut journal_warnings: Option<
        tokio::sync::mpsc::UnboundedReceiver<norte_core::embedded::JournalStatus>,
    >,
) -> Result<(), RunError> {
    let mut events = EventStream::new();
    // Tick del panel de tasks: copia snapshots del watch (jamás bloquea).
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // L2: la sesión se escribe UNA vez por segundo, no por tecla. Su propio
    // tick y no el de 100 ms porque son dos ritmos distintos: el panel de
    // tareas mira un `watch` en memoria y esto acaba en un fichero.
    let mut session_tick = tokio::time::interval(std::time::Duration::from_secs(1));
    session_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut session_push = SessionPush::start(backend, app.session.revision);
    // Debounce del hot-reload SIN bloquear el loop (revisión fase 6): cada
    // evento de config empuja el deadline; el reload corre cuando vence.
    let mut reload_at: Option<tokio::time::Instant> = None;
    // Listados paginados rellenándose en background (ADR 0017): un hueco POR
    // PANE — los dos panes pueden estar paginando a la vez, y con un hueco
    // global el cd de uno mataba el drenador del otro (ver [`Fill`]).
    // Por dónde sigue el barrido de `work.fill` (ver el brazo del `select!`).
    // Búsqueda viva en curso (liveSearch T6): a lo sumo una (el pane virtual
    // es uno). Molde `Fill`: se drena en el select y se suelta al salir.
    // Comparación de directorios en curso (`Shift+F2`): a lo sumo una — el
    // panel de diferencias es uno. Mismo molde que `work.search`.
    // Sincronización en curso (`Ctrl+Y`): a lo sumo una — el panel es uno, y
    // aprobar un plan mientras otro se aplica sería aprobar a ciegas.
    // Petición ai.rename_plan en vuelo (M4-IA): a lo sumo una — relanzar
    // aborta la anterior. Se cosecha en el select y Esc (BROWSE) la cancela.
    // Plan IA listo llegado con OTRO modal abierto: se RETIENE aquí (la cola
    // de `App` es específica de aprobaciones) y se abre en cuanto no haya
    // modal — jamás pisar (disciplina `open_next_pending`).
    // Petición fs.rename_batch_plan en vuelo (§17): a lo sumo una, cosechada
    // en el select como `work.ai_rename`.
    // Búsqueda semántica en vuelo (M4-IA-2): mismo molde que `work.ai_rename`
    // — a lo sumo una, relanzar aborta la anterior, Esc (BROWSE) cancela.
    // Hits listos llegados con OTRO modal abierto: se RETIENEN aquí y se
    // abren en cuanto no haya modal (disciplina `work.pending_ai_plan`).
    // Scripting Lua (M4, ADR 0026): host por capas con trust TOFU. Como
    // `work.fill`, el estado vive en el run loop. El run en vuelo (a lo sumo UNO:
    // el estado Lua es uno) se pollea inline en el select — `CommandRun` es
    // !Send y este future corre en block_on, jamás en spawn.
    let mut lua_host = load_lua(app, &layers).await;
    // Sonda de stat on-focus (#52, listado lazy): a lo sumo una en vuelo,
    // dedup por (pane, path) — dos panes sobre el MISMO dir deben hidratar
    // cada uno la suya (no reintenta un stat fallido hasta cambiar
    // selección).
    // Sonda de stat de la fila SELECCIONADA del panel de diferencias (#157):
    // mismo molde que `work.stat`, a lo sumo una en vuelo. El dedup vive en
    // `App::compare_size_probed` y no en una variable local del run loop
    // (a diferencia de `work.probed`) porque `App::compare_size_probe_targets`
    // ya lo consulta para decidir qué falta por pedir.
    // Fetch de decoraciones de plugin en vuelo (G3b, ADR 0037): a lo sumo
    // uno, molde de `work.stat`/`work.fill`.
    // L3: una lectura de preview en vuelo por hueco, superseded al moverse.
    // #106 (watching): vigilancia de los dirs visibles — notify con
    // fallback a sondeo (pitfall inotify). El conjunto vigilado se
    // re-sincroniza en CADA vuelta (diff barato, no-op sin cambios).
    // Regla 2, exención puntual (review MINOR-6): crear el watcher y los
    // watch()/unwatch() de rewatch son syscalls cortas inline (mismo
    // criterio documentado que el draw síncrono de ratatui más abajo);
    // solo corren al arrancar o al CAMBIAR de dir.
    // Todo lo que este bucle deja pedido y aún no ha cosechado (ver
    // [`InFlight`]): rellenos, sondas y los trabajos de fondo.
    let mut work = InFlight::default();
    let mut dir_watch = norte_frontend::watch::DirWatch::new();
    let mut dir_watch_alive = true;
    loop {
        dir_watch.rewatch(&watch_targets(app));
        if dir_watch.take_degraded_notice() {
            app.message = Some(t("status-watch-degraded"));
        }
        // #135: la suspensión se drena AQUÍ y en NINGÚN otro sitio. El opener
        // de #28 se lanza en tres puntos (el despacho de teclas, el de la
        // palette y el de la ayuda) porque cada uno tiene su propio
        // `continue`; una suspensión también la deja pendiente el Enter de
        // `Modal::CommandLine`, que vive en un cuarto brazo con su propio
        // `continue` — así que el sitio que los cubre a todos, presentes y
        // futuros, es la cabecera de la vuelta. Antes del draw: los paneles
        // que se repinten ya son los del listado refrescado.
        // `Shift+F2`: el despacho resolvió QUÉ comparar; el run loop es el
        // dueño del canal y de la Task, así que lanza. Mismo reparto que
        // `pending_shell`/`pending_open`, y en la misma cabecera de vuelta,
        // por la misma razón: los brazos que responden teclas tienen sus
        // propios `continue`.
        if let Some(params) = app.pending_compare.take() {
            launch_compare(app, backend, &mut work.compare, params).await;
        }
        // #149 y #164: ¿cabe en el destino, y sabe el destino sujetar lo que se
        // escriba en él? Las dos son I/O, así que el modal se abre SIN los
        // avisos y esta vuelta los rellena. El reparto es el de
        // `pending_compare`: el despacho decide QUÉ, el run loop lo pregunta.
        //
        // Las dos preguntas fallan de forma DISTINTA, y es deliberado.
        //
        // El espacio se traga el fallo: no poder enumerar volúmenes no puede
        // impedir una copia ni pintar una alarma, y «no lo sé» se dice callando
        // — ese es el contrato de `space::warning`.
        //
        // El confinamiento no. Ahí el silencio SIGNIFICA «este destino sujeta
        // sus escrituras», así que tragarse el fallo sería afirmarlo sin
        // saberlo: fail-open en una línea de seguridad. Si no se sabe, se
        // avisa (revisión de seguridad de W5 B).
        if let Some(check) = app.pending_dest_check.take() {
            let free = match check.total {
                // Sin total no hay pregunta de espacio que hacer, y enumerar
                // volúmenes para tirar la respuesta es I/O por nada.
                None => None,
                Some(_) => backend
                    .volumes(false)
                    .await
                    .ok()
                    .and_then(|vols| norte_frontend::space::free_for(&check.to, &vols)),
            };
            let space_notice =
                norte_frontend::space::warning(check.total, free, norte_i18n::active());
            let confinement_notice = match backend.capabilities(&check.to).await {
                Ok(caps) => norte_frontend::confine::warning(caps, norte_i18n::active()),
                Err(_) => norte_frontend::confine::warning(
                    norte_proto::Capabilities {
                        flags: norte_proto::CapabilityFlags::empty(),
                        max_path: None,
                    },
                    norte_i18n::active(),
                ),
            };
            if let Some(Modal::ConfirmTransfer { space, confine, .. }) = app.modal.as_mut() {
                *space = space_notice;
                *confine = confinement_notice;
            }
        }
        // `Ctrl+Y` / `s` / `m`: el despacho resolvió QUÉ sincronizar, y aquí
        // se lanza — mismo reparto que la comparación, en la misma cabecera de
        // vuelta y por la misma razón.
        if let Some(params) = app.pending_sync.take() {
            launch_sync_plan(app, backend, &mut work.sync, params).await;
        }
        // Y la aprobación, que es la SEGUNDA Task del mismo diálogo. Lo único
        // que viaja es el hash (ADR 0049).
        if let Some(hash) = app.pending_sync_apply.take() {
            launch_sync_apply(app, backend, &mut work.sync, &hash).await;
        }
        // #140: el panel que acaba de desconectar vuelve a casa por el mismo
        // `cd` que cualquier otra navegación, con su ritual de vuelta.
        if let Some(casa) = app.pending_disconnect_home.take() {
            let outcome = cd(app, backend, &mut events, casa).await;
            apply_cd(
                &app.panes,
                &mut work.fill,
                &mut work.decorate,
                &mut work.probed,
                &mut work.search,
                outcome,
            );
        }
        if let Some(pending) = app.pending_shell.take() {
            let crate::app::PendingShell {
                argv,
                cwd,
                wait_for_key,
            } = pending;
            // Auditoría (review de S4): el journal NO ve nada de esto a
            // propósito (design §D), así que el rastro de que aquí hubo un
            // shell vive en el log. Sin la línea de comandos —es del usuario
            // y no tiene por qué acabar en un fichero— y con el programa a
            // secas.
            let launched = argv.first().map(|a| a.to_string_lossy().into_owned());
            tracing::info!(
                program = launched.as_deref().unwrap_or("(none)"),
                wait_for_key,
                "TUI suspended for a user-started program (not journalled: no actor, no reversal)"
            );
            // Nada que ejecutar = `app.toggle-panels`: solo enseña la
            // terminal anfitriona. Refrescar tras él costaría un re-listado
            // completo (remoto incluido) por una tecla que no toca el disco.
            let launched_something = !argv.is_empty();
            if let Err(e) = run_suspended(terminal, capture, argv, cwd, wait_for_key).await {
                // `detail_for_bar`, jamás el `Display` crudo del OS (review
                // de S4, L1/m4): el sistema lo localiza por su cuenta, no
                // tiene tope y —si el error viene de un join roto— arrastra
                // el payload de un panic. Y se NOMBRA el programa, como hace
                // `msg-open-failed`: si no, un `$SHELL` borrado y una
                // pantalla alternativa que no cerró dan el mismo texto.
                app.message = Some(ta(
                    "msg-shell-failed",
                    &[
                        ("program", launched.as_deref().unwrap_or("-")),
                        ("error", &crate::app::detail_for_bar(&e.to_string())),
                    ],
                ));
            }
            // Lo que el shell haya hecho en disco se ve al volver, por el
            // MISMO camino que `pane.refresh` (#118): refresh cancelable +
            // el ritual completo, jamás un `set_listing` a mano.
            //
            // GATEADO igual que el refresh del watcher (review de S4, M2):
            // `refresh_panes` polea `events` y se come toda tecla que no sea
            // Esc/Ctrl+C, y reinterpreta Esc como «abandona el refresh». Con
            // un modal delante —una aprobación de agente puede haberse
            // plantado al cerrarse el prompt— eso se traga la respuesta del
            // usuario hasta que la aprobación caduca. Si no se puede
            // refrescar ahora, el watcher (canal de capacidad 1) o el tick
            // lo hacen al cerrarse el overlay.
            if launched_something && watch_refresh_allowed(app) {
                let refreshed = refresh_panes(app, backend, &mut events).await;
                after_panes_refresh(
                    app,
                    refreshed,
                    &mut work.fill,
                    &mut work.probed,
                    &mut work.search,
                );
            }
        }
        // Review MINOR-1: `over_modal` describe el modal que hay AHORA, no uno
        // que ya se contestó. Antes de plantar los modales retenidos de abajo,
        // que tienen que encontrar la bandera ya limpia.
        settle_help_over_modal(app);
        // Plan IA retenido (M4-IA): abre en cuanto el modal activo se cierra.
        // Las aprobaciones no compiten aquí: con la cola no vacía y sin modal,
        // `open_next_pending` ya habría abierto una al cerrarse el anterior.
        if app.modal.is_none()
            && let Some(pendiente) = work.pending_ai_plan.take()
        {
            app.modal = Some(Modal::AiRenamePlan {
                dir: pendiente.dir,
                entries: pendiente.entries,
                offset: 0,
                plan: pendiente.plan,
            });
        }
        // Hits semánticos retenidos (M4-IA-2): misma disciplina. Si el plan
        // IA de arriba acaba de abrir, el `is_none` los deja esperando.
        if app.modal.is_none()
            && let Some(hits) = work.pending_semantic.take()
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
            let size = terminal.size().map_err(RunError::Terminal)?;
            let (width, height) = ui::help_body_size(
                ratatui::layout::Rect::new(0, 0, size.width, size.height),
                lang,
            );
            app.refresh_help(width, height);
        }
        // La ventana de cada pane se reconcilia ANTES de pintar (#124 + el
        // scroll pegajoso): el cursor ya está donde lo dejó la tecla, así que
        // esto decide qué filas se ven y el draw las pinta. Hacerlo DESPUÉS
        // costaba un frame de retraso — el cursor podía caer fuera de la
        // ventana pintada, o sea desaparecer de la pantalla justo al llegar
        // al borde.
        {
            let s = terminal.size().map_err(RunError::Terminal)?;
            ui::before_frame(app, ratatui::layout::Rect::new(0, 0, s.width, s.height));
        }
        // Exención puntual de la regla 2: el draw escribe la terminal de
        // control síncronamente (patrón async oficial de ratatui; acotado,
        // runtime multi-thread).
        let painted = terminal
            .draw(|f| ui::draw(f, app))
            .map_err(RunError::Terminal)?;
        if app.quit {
            // La última foto, y esperarla. El tick de un segundo se pierde lo
            // que pasó dentro de ese segundo, y salir es cuando más duele:
            // hasta aquí, cerrar norte justo después de un `cd` guardaba el
            // directorio anterior.
            //
            // Sin el gate del modal, a propósito: un modal abierto significa
            // «no guardes lo que estoy decidiendo», y aquí ya no se está
            // decidiendo nada — se está saliendo, y lo que hay que guardar es
            // dónde se estaba.
            drain_notices(app, &mut session_push);
            let last = (!app.session.detached)
                .then(|| capture_session(app, &mut session_push))
                .flatten();
            session_push.close(last).await;
            return Ok(());
        }
        // #124: el alto REAL del viewport vuelve al modelo tras cada frame —
        // la paginación (`page_step`) y el radio de la sonda de stat salen de
        // ahí en vez de constantes que mienten en cualquier terminal que no
        // mida justo eso. Con el visor abierto son 0 filas (ningún pane
        // pintado) y el modelo vuelve a sus fallbacks.
        // El alto REAL del frame que se acaba de pintar: si la terminal cambió
        // de tamaño entre `before_frame` y el draw, este es el bueno, y de él
        // salen la paginación y el radio de la sonda de stat.
        ui::before_frame(app, painted.area);
        // MISMO trato para la geometría del ratón: el draw es quien sabe
        // dónde cayó cada pane y con qué scroll, así que la devuelve al
        // modelo y el hit test resuelve contra la pantalla que el usuario
        // está mirando. Sin esto habría que recalcular el layout en cada
        // click, y un click resuelto contra un layout que no es el pintado
        // no falla ruidosamente: marca el fichero de al lado.
        mouse::after_frame(
            app,
            ui::pane_geometry(app, painted.area),
            ui::tab_zones(app, painted.area),
            ui::menu_zones(app, painted.area),
            ui::places_zones(app, painted.area),
        );
        // L3: el visor acoplado sigue al cursor del listado activo. Lo que se
        // pide sale de `preview::want`, que devuelve `None` cuando el hueco no
        // se colocó — cerrado, detrás de una pestaña, o colapsado por falta de
        // sitio. Por eso la suspensión de un hueco oculto no es una
        // comprobación que alguien pueda olvidarse de escribir: sin objetivo
        // no hay nada que pedir.
        {
            let res = ui::resolved_for(app, painted.area);
            match crate::preview::want(app, &res) {
                Some((slot, crate::preview::Want::File(path))) => {
                    let ya = app
                        .panes
                        .preview(slot)
                        .and_then(|p| p.shown().cloned())
                        .is_some_and(|s| s == path);
                    let in_flight = work.preview.get(slot).is_some_and(|f| f.path == path);
                    if !ya && !in_flight {
                        // Empezar otra SUSTITUYE la que hubiera: el `Receiver`
                        // viejo se cae aquí y su respuesta no se aplica nunca.
                        work.preview
                            .set(slot, Some(spawn_preview_fetch(backend, path)));
                    }
                }
                Some((slot, crate::preview::Want::Note(clave))) => {
                    // Un directorio no se lee: se dice lo que es. Y lo que
                    // hubiera en vuelo deja de importar.
                    work.preview.remove(slot);
                    let text = t(clave);
                    if let Some(p) = app.panes.preview_mut(slot)
                        && (p.note().is_none_or(|n| n != text) || p.shown().is_some())
                    {
                        p.say(None, text);
                    }
                }
                None => {}
            }
            // #136: el árbol SÍ pide, y por eso pide UNA rama por vuelta: un
            // directorio de diez mil entradas o un remoto lento no pueden
            // trabar el bucle, y la siguiente vuelta pide la siguiente.
            if let Some(dir) = app.tree().and_then(crate::tree::Tree::wants) {
                let child_dirs = match backend.list(&dir).await {
                    Ok(mut entries) => {
                        // El MISMO orden que el listado de al lado, con el
                        // mismo comparador: dos columnas que enseñan lo mismo
                        // en distinto orden se leen como si dijeran cosas
                        // distintas.
                        norte_frontend::sort_entries(&mut entries);
                        entries
                            .into_iter()
                            .filter(|e| e.kind == norte_proto::EntryKind::Dir)
                            .map(|e| e.path)
                            .collect()
                    }
                    // Una rama que no se deja leer se marca como leída y VACÍA:
                    // sin esto se volvería a pedir en cada vuelta, que es un
                    // bucle de peticiones contra un directorio prohibido.
                    Err(_) => Vec::new(),
                };
                if let Some(t) = app.tree_mut() {
                    t.insert_children(dir, child_dirs);
                }
            }
            // La hoja de atributos NO pide nada: lo que enseña ya vino en el
            // listado, así que esto es una copia, no una petición. Un hueco
            // que el reparto no colocó no produce objetivo y no se toca.
            match crate::metadata::want(app, &res) {
                Some((slot, crate::metadata::Want::Entry(e))) => {
                    if let Some(hoja) = app.panes.metadata_mut(slot) {
                        *hoja = Some(*e);
                    }
                }
                Some((slot, crate::metadata::Want::Note(_))) => {
                    if let Some(hoja) = app.panes.metadata_mut(slot) {
                        *hoja = None;
                    }
                }
                None => {}
            }
        }
        // #52: listado lazy — las entradas VISIBLES sin size se hidratan por
        // tandas (máx. una en vuelo; dedup por (pane, path) en `work.probed`).
        if work.stat.is_none() {
            let tanda: Vec<(usize, VPath)> = app
                .needs_stat_window(STAT_WINDOW_RADIUS)
                .into_iter()
                .filter(|c| !work.probed.contains(c))
                .take(STAT_BATCH_MAX)
                .collect();
            if !tanda.is_empty() {
                work.probed.extend(tanda.iter().cloned());
                work.stat = Some(spawn_stat_probe(backend, tanda));
            }
        }
        // #157: la fila seleccionada del panel de diferencias, mismo trato.
        if work.compare_stat.is_none() {
            let targets = app.compare_size_probe_targets();
            if !targets.is_empty() {
                work.compare_stat = Some(spawn_compare_stat_probe(
                    backend,
                    targets,
                    app.compare_generation(),
                ));
            }
        }
        tokio::select! {
            _ = session_tick.tick() => {
                push_session(app, &mut session_push);
                // #179: soltar el journal cuando lleva un rato sin
                // usarse. Este proceso lo tomaba en la primera mutación
                // y no lo devolvía hasta salir, así que una copia a las
                // 09:00 dejaba a `norte daemon run` y a `norte audit`
                // sin poder abrirlo en todo el día. La ventana se
                // reabre sola en la siguiente mutación, y la reapertura
                // RELEE la cadena, que es lo que lo hace seguro.
                //
                // En el CUERPO de la rama y no en su condición: el
                // cierre NO es cancel-safe, y dropearlo a medias deja
                // el pool agonizando en el worker de sqlx y al
                // siguiente `resolve` chocando contra nuestro propio
                // lock — un aviso de «sesión sin registro» que nos
                // habríamos inventado nosotros.
                backend.release_journal_if_idle(JOURNAL_IDLE).await;
            }
            _ = tick.tick() => {
                // Mutación terminada → refresh de panes; el ritual completo
                // (drenador/sonda #52/búsqueda) vive en `after_panes_refresh`
                // — ÚNICO para los tres disparadores del refresh (#117).
                let refreshed = on_tick(app, backend, &mut events).await;
                after_panes_refresh(app, refreshed, &mut work.fill, &mut work.probed, &mut work.search);
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
                        &mut work.fill,
                        &mut work.probed,
                        &mut work.search,
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
                app.board.push_foreign(&task);
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
            Some(state) = async {
                match &mut journal_warnings {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                // #167/#177: esta sesión acaba de mutar sin quedar registrada.
                // Uno por EPISODIO (el core no repite mientras el motivo no
                // cambie), así que pisar `message` aquí no puede convertirse en
                // un goteo. Y como `message` lo borra la siguiente tecla, el
                // hecho se anota además en el indicador PERSISTENTE de la
                // barra: esto no es un aviso que se pueda perder por pulsar una
                // flecha.
                //
                // #179: y la recuperación APAGA ese indicador. Sin esto, un
                // ocupante de paso —otro `norte cp`, un daemon reiniciándose—
                // dejaría a una sesión de tres horas enseñando «no se registra»
                // sobre mutaciones que sí se registran.
                use norte_core::embedded::JournalStatus;
                match state {
                    JournalStatus::Lost(why) => {
                        app.message = Some(journal_warning_i18n(&why));
                        app.note_no_journal(why);
                    }
                    JournalStatus::Recovered => {
                        app.message = Some(t("msg-journal-recovered"));
                        app.note_journal_recovered();
                    }
                    // #203: mismo hecho, otra explicación — y la barra lo dice
                    // con otra frase, porque la de siempre sale también cuando
                    // hay un daemon vivo y por eso ya no se mira.
                    JournalStatus::Squatted => {
                        app.message = Some(t("msg-journal-squatted"));
                        app.note_journal_squatted();
                    }
                    // `#[non_exhaustive]`: una transición nueva no puede
                    // cambiar el indicador a ciegas — se ignora hasta que
                    // alguien la enseñe a propósito.
                    _ => {}
                }
            }
            res = async {
                match &mut work.stat {
                    Some(pr) => (&mut pr.rx).await.ok(),
                    None => std::future::pending().await,
                }
            } => {
                // Sonda de stat del viewport (#52): el slot se limpia SIEMPRE
                // (haya hidratado algo, fallara el stat o se cerrara el canal)
                // — la dedup por `work.probed` evita reintentar hasta que un
                // listado nuevo la vacíe.
                work.stat = None;
                for (pane, path, entry) in res.unwrap_or_default() {
                    app.panes[pane].hydrate(&path, entry.size, entry.mtime_ms);
                }
            }
            (generation, res) = async {
                match &mut work.compare_stat {
                    Some(pr) => (pr.generation, (&mut pr.rx).await.ok()),
                    None => std::future::pending().await,
                }
            } => {
                // Sonda de la fila seleccionada del panel de diferencias
                // (#157): el slot se limpia SIEMPRE, igual que la de arriba.
                // Un canal cerrado (`res` es `None`) no marca nada sondeado:
                // la próxima vez que la selección lo vuelva a pedir se
                // reintenta, en vez de dejar la fila huérfana para siempre
                // porque la task que la pedía murió a medio camino.
                work.compare_stat = None;
                for (path, entry) in res.unwrap_or_default() {
                    // La generación es la del PEDIDO, no la de ahora: si otra
                    // comparación empezó mientras volaba, `hydrate` la tira.
                    app.hydrate_compare_size(generation, path, entry.and_then(|e| e.size));
                }
            }
            (slot, res) = std::future::poll_fn(|cx| {
                // Uno por HUECO, y tantos como huecos haya. `oneshot::Receiver`
                // es `Unpin`, así que se sondea a mano; `select!` tiene aridad
                // fija y aquí la aridad la pone el layout.
                for (id, f) in work.decorate.iter_mut() {
                    if let std::task::Poll::Ready(r) =
                        std::pin::Pin::new(&mut f.rx).poll(cx)
                    {
                        return std::task::Poll::Ready((id, r.ok()));
                    }
                }
                std::task::Poll::Pending
            }) => {
                // Fetch de decoraciones (G3b): se limpia SIEMPRE. Una
                // respuesta tardía cuyo `dir` ya no case el del pane (el
                // usuario cd'eó de nuevo mientras estaba en vuelo) se
                // DESCARTA — nunca pinta badges de un listado que ya no se
                // ve (mismo criterio anti-stale que el drain-guard de
                // `apply_fill_msg` para búsqueda virtual).
                if let Some(f) = work.decorate.remove(slot)
                    && let Some((map, cols)) = res
                    && let Some(p) = app.panes.browser_mut(f.slot)
                    && p.dir() == &f.dir
                {
                    p.set_decorations(map);
                    // #117-follow-up: los valores de columnas plugin:
                    // viajan en el mismo fetch y comparten el guard
                    // anti-stale.
                    p.set_plugin_columns(cols);
                }
            }
            (slot, res) = std::future::poll_fn(|cx| {
                // Lecturas del preview, una por hueco. Mismo sondeo a mano
                // que las decoraciones y por el mismo motivo: la aridad la
                // pone el layout, no `select!`.
                for (id, f) in work.preview.iter_mut() {
                    if let std::task::Poll::Ready(r) =
                        std::pin::Pin::new(&mut f.rx).poll(cx)
                    {
                        return std::task::Poll::Ready((id, r.ok()));
                    }
                }
                std::task::Poll::Pending
            }) => {
                // La respuesta se aplica SOLO si el hueco sigue queriendo
                // esa misma ruta: mientras volaba, el cursor pudo moverse.
                // Y un error se PINTA, jamás se pregunta — el preview sigue
                // al cursor, así que un diálogo por pulsación convertiría
                // bajar por un directorio en una ráfaga de modales.
                if let Some(f) = work.preview.remove(slot) {
                    match res {
                        Some(Ok(viewer)) => {
                            if let Some(p) = app.panes.preview_mut(slot) {
                                p.show(f.path, viewer);
                            }
                        }
                        Some(Err(e)) => {
                            let clave = error_category(&e);
                            app.preview_failed(slot, &clave);
                        }
                        // La task murió sin contestar: no se pinta un error
                        // inventado, se deja lo que hubiera y el siguiente
                        // movimiento del cursor lo vuelve a intentar.
                        None => {}
                    }
                }
            }
            (pane, msg) = std::future::poll_fn(|cx| {
                // Un canal POR HUECO, no por posición, y tantos como huecos
                // haya. `tokio::select!` tiene aridad fija, así que se sondean
                // a mano: `poll_recv` registra el waker, o sea que esto es tan
                // cancel-safe como `recv` y perder la carrera no pierde el
                // lote del otro.
                //
                // El barrido ARRANCA donde acabó el anterior. Sondear siempre
                // desde el principio deja que un drenador rápido en el primer
                // hueco no deje hablar nunca a los demás — con dos paneles
                // `select!` lo evitaba solo, porque elige al azar.
                let n = work.fill.len();
                for k in 0..n {
                    let Some((id, f)) = work.fill.iter_mut().nth((work.fill_cursor + k) % n) else {
                        break;
                    };
                    if let std::task::Poll::Ready(m) = f.rx.poll_recv(cx) {
                        work.fill_cursor = (work.fill_cursor + k + 1) % n;
                        return std::task::Poll::Ready((id, m));
                    }
                }
                std::task::Poll::Pending
            }) => {
                // Lote del drenador del listado paginado (ADR 0017): al pane
                // de SU hueco. `None` = canal cerrado (fin del drenado).
                apply_fill_msg(app, &mut work.fill, pane, msg);
            }
            hits = async {
                // Solo se drena mientras el run sigue vivo (`Running`): un
                // canal cerrado devolvería `None` en bucle (spin) — al leer el
                // `None` se pasa a terminal y este brazo queda pendiente.
                match &mut work.search {
                    Some(s) if s.state == SearchState::Running => s.rx.recv().await,
                    _ => std::future::pending().await,
                }
            } => {
                drain_search(app, &mut work.search, hits);
            }
            batch = async {
                // Igual que el brazo de hits: solo se drena con el run VIVO,
                // porque un canal cerrado devolvería `None` en bucle (spin).
                match &mut work.compare {
                    Some(c) if c.state == CompareState::Running => c.rx.recv().await,
                    _ => std::future::pending().await,
                }
            } => {
                drain_compare(app, &mut work.compare, batch);
            }
            // UN solo brazo para las dos Tasks del diálogo: `select!` no deja
            // tomar prestado `work.sync` dos veces, y son fases sucesivas del
            // mismo run — nunca hay plan y aplicación a la vez.
            tick = async {
                let Some(s) = &mut work.sync else {
                    return std::future::pending().await;
                };
                if s.applying {
                    // La aplicación no tiene canal: se espera a que su Task
                    // cambie de estado. `changed()` con el emisor caído
                    // devuelve `Err`, y eso también es un final — se sale y el
                    // cosechado lee el snapshot que haya.
                    let alive = s.progress.changed().await.is_ok();
                    return SyncTick::Applied { alive };
                }
                match &mut s.rx {
                    // Un canal ya cerrado devolvería `None` en bucle (spin):
                    // `drain_sync_plan` pone `rx = None` al cerrarse.
                    Some(rx) => SyncTick::Plan(rx.recv().await),
                    None => std::future::pending().await,
                }
            } => {
                match tick {
                    SyncTick::Plan(event) => drain_sync_plan(app, &mut work.sync, event),
                    SyncTick::Applied { alive } => {
                        harvest_sync_apply(app, backend, &mut work.sync, alive).await;
                    }
                }
            }
            res = async {
                // ai.rename_plan en vuelo (M4-IA): cosecha sin bloquear —
                // el brazo solo se arma con un run vivo (molde work.stat).
                match &mut work.ai_rename {
                    Some(r) => (&mut r.handle).await,
                    None => std::future::pending().await,
                }
            } => {
                if let Some(run) = work.ai_rename.take() {
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
                            // §17: el plan del LOTE se pide AQUÍ, en el mismo
                            // viaje que el plan IA — el modal necesita el
                            // `plan_hash` para que confirmar haga algo, y un
                            // plan retenido tras otro modal no tendría quién
                            // se lo pidiera después.
                            //
                            // SPAWNEADO, como la llamada al modelo: contra un
                            // dir enorme o un daemon lento esto es un `fs.list`
                            // entero, y esperarlo aquí congelaría el loop —
                            // sin dibujo, sin teclas, sin Esc. El modal abre en
                            // `Pending` y se rellena solo.
                            //
                            // Cinturón fail-loud COMPARTIDO con la GUI (audit
                            // MAJOR-2): una pareja que no es un `Segment`
                            // delata un daemon hostil/roto — ni se le pide
                            // plan al core, y confirmar queda muerto.
                            let state = if let Some(pairs) =
                                norte_frontend::rename_pairs(&plan.entries)
                            {
                                let b = backend.clone();
                                let d = run.dir.clone();
                                let handle =
                                    tokio::spawn(
                                        async move { b.rename_batch_plan(&d, &pairs).await },
                                    );
                                if let Some(old) =
                                    work.rename_batch.replace(RenameBatchRun { handle })
                                {
                                    old.handle.abort();
                                }
                                app.message = None;
                                norte_frontend::BatchPlan::Pending
                            } else {
                                app.message = Some(t("msg-ai-rename-invalid-plan"));
                                norte_frontend::BatchPlan::Failed
                            };
                            let ready = PendingAiPlan {
                                dir: run.dir,
                                entries: plan.entries,
                                plan: state,
                            };
                            if app.modal.is_none() {
                                app.modal = Some(Modal::AiRenamePlan {
                                    dir: ready.dir,
                                    entries: ready.entries,
                                    offset: 0,
                                    plan: ready.plan,
                                });
                            } else {
                                // Otro modal abierto (aprobación, colisión…):
                                // el plan espera su turno, jamás lo pisa. A
                                // diferencia de la GUI (banner superseded), aquí
                                // el overwrite es inalcanzable: run único en
                                // vuelo y el prompt no abre sobre otro modal.
                                work.pending_ai_plan = Some(ready);
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
                // fs.rename_batch_plan en vuelo (§17): cosecha sin bloquear,
                // molde del brazo de `work.ai_rename`.
                match &mut work.rename_batch {
                    Some(r) => (&mut r.handle).await,
                    None => std::future::pending().await,
                }
            } => {
                if work.rename_batch.take().is_some() {
                    let state = match res {
                        Ok(Ok(plan)) => norte_frontend::BatchPlan::Ready(Box::new(plan)),
                        Ok(Err(e)) => {
                            app.message = Some(ta(
                                "msg-rename-batch-plan-failed",
                                &[("error", &detail_for_bar(&error_category(&e)))],
                            ));
                            norte_frontend::BatchPlan::Failed
                        }
                        // Abortado (otra petición lo relevó) o pánico del
                        // future: no hay plan y no hay nada más que decir —
                        // quien lo relevó ya puso SU mensaje.
                        Err(_join) => norte_frontend::BatchPlan::Failed,
                    };
                    // El modal puede estar abierto, RETENIDO tras otro, o ya
                    // cerrado por el humano. En los dos primeros casos se
                    // rellena; en el tercero la respuesta se tira.
                    if !app.settle_ai_batch_plan(&state)
                        && let Some(p) = &mut work.pending_ai_plan
                        && p.plan == norte_frontend::BatchPlan::Pending
                    {
                        p.plan = state;
                    }
                }
            }
            res = async {
                // index.search_semantic en vuelo (M4-IA-2): cosecha sin
                // bloquear — molde del brazo de `work.ai_rename`.
                match &mut work.semantic {
                    Some(r) => (&mut r.handle).await,
                    None => std::future::pending().await,
                }
            } => {
                work.semantic = None;
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
                                work.pending_semantic = Some(hits);
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
                match &mut work.lua {
                    Some((run, _)) => run.await,
                    None => std::future::pending().await,
                }
            } => {
                // DROP INMEDIATO del CommandRun resuelto (contrato del
                // driver): retenerlo mantendría `run_active` encendido y la
                // statusbar Lua congelada.
                work.lua = None;
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
                    while work.lua.is_none() {
                        let Some(next) = work.lua_queue.pop_front() else {
                            break;
                        };
                        work.lua = start_lua_run(app, host, backend, &next);
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
                // arriba: son unos pocos bytes de escape a la terminal de
                // control síncronos, acotados, y solo cuando la clave CAMBIA.
                if let Err(e) =
                    capture.set(cfg.common.ui_mouse.unwrap_or(true), terminal.backend_mut())
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
                        &mut work.fill,
                        &mut work.probed,
                        &mut work.search,
                    );
                }
                // Hot-reload del scripting Lua (ADR 0026): host NUEVO entero
                // (jamás estado a medias). Un `CommandRun` en vuelo retiene
                // el estado VIEJO vía sus handles clonados (documentado en
                // `lua::api`) y no se toca; statusbar/estado renacen. La
                // cola también: sus nombres apuntaban al registro viejo
                // (y si `load_lua` dio None, no quedaría quién drenarla).
                lua_host = load_lua(app, &layers).await;
                work.lua_queue.clear();
            }
            maybe = events.next() => {
                // EOF del terminal —te cierran la ventana—: se sale por
                // el MISMO sitio que un `app.quit`, que es donde se
                // guarda la última foto de la sesión. Saliendo aquí con
                // un `return` se perdía.
                let Some(event) = maybe else { app.quit = true; continue; };
                let event = event.map_err(RunError::Event)?;
                // Ratón (`[ui] mouse`): solo llega si la captura está
                // pedida — sin ella el emulador no reporta nada y este
                // brazo no corre. La semántica del gesto (marcar, barrer,
                // transferir) vive en `norte-frontend` (regla 7); aquí solo
                // se resuelve la celda y se aplica.
                if let Event::Mouse(me) = event {
                    match mouse::handle(app, me) {
                        mouse::After::Nothing => {}
                        // Pulsar un elemento del menú: el ratón ya
                        // dejó el cursor encima; ejecutarlo es
                        // asíncrono y necesita el backend, así que se
                        // remata aquí — el MISMO camino que `Enter`,
                        // que es lo que hace que un menú y una tecla no
                        // puedan divergir.
                        mouse::After::MenuAccept => {
                            let chosen = app
                                .menu
                                .as_ref()
                                .and_then(norte_frontend::menu::MenuState::selected);
                            app.menu = None;
                            if let Some(id) = chosen
                                && let Some(cmd) = Command::parse(id)
                            {
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
                                apply_cd(
                                    &app.panes,
                                    &mut work.fill,
                                    &mut work.decorate,
                                    &mut work.probed,
                                    &mut work.search,
                                    outcome,
                                );
                                reap_search_run(app, &mut work.search);
                                launch_pending_open(app, terminal, capture).await;
                            }
                        }
                        // Doble click = `nav.enter`, por el MISMO `dispatch`
                        // que la tecla: mismo cd, mismo relleno paginado,
                        // misma cosecha de la búsqueda viva. Un segundo
                        // camino para entrar en un directorio sería un
                        // segundo sitio donde arreglar cada bug de cd.
                        mouse::After::Enter => {
                            // K3a: un gesto es OTRA entrada. La secuencia que
                            // el lector estuviera tecleando se abandona con su
                            // panel — no la completa el ratón, y dejarla
                            // armada haría que la siguiente tecla disparase un
                            // comando pedido antes de cambiar de directorio.
                            app.abandon_pending(resolver);
                            // Paridad con el sitio del resolver: entrar en
                            // un hit apaga el modo virtual del pane, y hay
                            // que cosechar el run (regla 3).
                            run_command(
                                app,
                                backend,
                                &mut events,
                                help_lines,
                                lang,
                                quick_mode,
                                confirm_quit,
                                &cfg,
                                &mut work.fill,
                                &mut work.decorate,
                                &mut work.probed,
                                &mut work.search,
                                Command::NavEnter,
                            )
                            .await;
                        }
                        // #226: el sidebar con el ratón toma los MISMOS
                        // caminos que su teclado. Desplegar las unidades
                        // es el momento de volver a pedirlas —y plegarlas,
                        // el de no pedirlas—, así que el ratón no puede
                        // ser un cuarto disparador de refresco: es este.
                        mouse::After::PlacesFolded => {
                            if app.places_drives_visible() {
                                refresh_places_drives(app, backend).await;
                            }
                        }
                        // Y activar una fila lleva el listado por el
                        // flujo de `cd` de siempre, igual que `Enter`
                        // dentro del sidebar.
                        mouse::After::PlacesActivate => {
                            app.abandon_pending(resolver);
                            if let Some(path) = app.places_activate() {
                                let pane = app.focus();
                                let outcome =
                                    cd_in(app, backend, &mut events, pane, path, Trail::Record)
                                        .await;
                                apply_cd(
                                    &app.panes,
                                    &mut work.fill,
                                    &mut work.decorate,
                                    &mut work.probed,
                                    &mut work.search,
                                    outcome,
                                );
                            }
                        }
                    }
                } else if let Event::Key(key) = event
                    && key.kind == crossterm::event::KeyEventKind::Press
                {
                    on_key(
                        app,
                        backend,
                        terminal,
                        capture,
                        &mut events,
                        resolver,
                        viewer_resolver,
                        dialog_resolver,
                        help_lines,
                        lang,
                        quick_mode,
                        confirm_quit,
                        &cfg,
                        cli_preset.as_deref(),
                        lua_host.as_ref(),
                        &mut work,
                        key,
                    )
                    .await;
                } else if let Event::Paste(text) = event {
                    // Bracketed paste (#143): ONE router beside the key
                    // dispatch above, not a second one — see `route_paste`.
                    app.message = None;
                    route_paste(app, &text);
                }
            }
        }
        // Resize/Focus/etc: el draw del inicio del loop repinta solo.
    }
}

/// Dirs NATIVOS vigilables de los panes (#106): solo `file://` (un dir
/// sftp/S3/archive no tiene inotify — su refresh sigue siendo Ctrl+R) y
/// solo panes reales (el virtual de búsqueda no muestra un dir). Puro:
/// `vpath_to_native` no toca el FS.
#[must_use]
pub fn watch_targets(app: &App) -> [Option<std::path::PathBuf>; 2] {
    std::array::from_fn(|i| {
        let p = &app.panes[i];
        if p.virtual_search {
            return None;
        }
        norte_vfs_local::vpath_to_native(p.dir()).ok()
    })
}
