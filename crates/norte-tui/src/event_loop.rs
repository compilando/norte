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

use crate::app::{App, CompareState, SearchState, detail_for_bar, error_category};
use crate::config::{self, Layers};
use crate::config_reload::reload_config;
use crate::console::Console;
use crate::dispatch::dispatch;
use crate::fill::{Fill, apply_fill_msg};
use crate::gestures::launch_opener;
use crate::jobs::{
    InFlight, SearchRun, SyncTick, drain_compare, drain_search, drain_sync_plan, harvest_ai_rename,
    harvest_checksum, harvest_rename_batch, harvest_semantic, harvest_sync_apply,
};
use crate::keymap::{Command, Resolver};
use crate::keys::on_key;
use crate::lua::{RunOutcome, load_lua, start_lua_run};
use crate::mouse;
use crate::nav;
use crate::navigate::{apply_cd, cd, request_decorations, settle_cd};
use crate::overlays::watch_refresh_allowed;
use crate::paste::route_paste;
use crate::probes::{DecorateFetch, Probed};
use crate::refresh::{after_panes_refresh, on_tick, reap_search_run, refresh_panes};
use crate::screens::pane_attr_ids;
use crate::session_push::{
    JOURNAL_IDLE, SessionPush, capture_session, drain_notices, push_session,
};
use crate::tty;
use crate::turn;
use crate::ui;
use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use norte_core::backend::{Backend, ConnEvent};
use norte_frontend::layout::BySlot;
use norte_i18n::{t, ta};

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
#[expect(clippy::too_many_arguments, reason = "wiring del bucle, no API")]
pub(crate) async fn run_command(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
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
/// Lanza lo que el despacho dejó pedido, con la terminal que tiene la consola
/// —que es quien la tiene desde que una espera larga necesitó repintarse.
///
/// Sin terminal (consola desligada) no se lanza nada Y SE DESCARTA lo
/// pendiente: dejarlo puesto lo dispararía en el siguiente sitio que sí tenga
/// terminal, mucho después de la tecla que lo pidió.
pub(crate) async fn launch_pending(
    app: &mut App,
    console: &mut crate::console::Console<'_>,
    capture: &mut mouse::Capture,
) {
    let Some(pending) = app.pending_open.take() else {
        return;
    };
    if let Some(terminal) = console.terminal() {
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
#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "wiring del bucle, no API"
)]
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
    // `mut` desde los perfiles (ADR 0079): cambiar de perfil rehace las capas
    // en caliente, así que esto ya no es constante durante la sesión.
    mut layers: Layers,
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
    mut failed: Option<
        tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::ConnectionFailed>,
    >,
    mut plugin_notices: Option<
        tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::PluginNotice>,
    >,
    mut journal_warnings: Option<
        tokio::sync::mpsc::UnboundedReceiver<norte_core::embedded::JournalStatus>,
    >,
) -> Result<(), RunError> {
    let mut events = EventStream::new();
    // Alt solo abre el menú (`[ui] alt_menu`). Solo llega a ver un Alt
    // suelto si el protocolo de kitty está pedido; sin él no le entra nada.
    let mut alt_solo = crate::alt_menu::AltSolo::default();
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
    // Los listados INICIALES también se decoran (ADR 0105): `main` los
    // construye sin pasar por `settle_cd`, y sin esto un `ntc` recién abierto
    // no tenía ni un icono ni una insignia hasta el primer `cd`.
    for pane in 0..app.panes.len() {
        request_decorations(app, backend, &mut work.decorate, pane);
    }
    let mut dir_watch = norte_frontend::watch::DirWatch::new();
    let mut dir_watch_alive = true;
    loop {
        dir_watch.rewatch(&watch_targets(app));
        if dir_watch.take_degraded_notice() {
            app.message = Some(t("status-watch-degraded"));
        }
        // El gestor cambió el gobierno o los ajustes de un plugin: lo que
        // los plugins dijeron de cada listado se olvida y se vuelve a pedir.
        // `set` sobre el hueco tira la tanda en vuelo, así que una respuesta
        // de antes del cambio no aterriza.
        if std::mem::take(&mut app.redecorate) {
            for pane in &mut app.panes {
                pane.set_decorations(std::collections::HashMap::new());
                pane.set_plugin_columns(std::collections::HashMap::new());
            }
            for pane in 0..app.panes.len() {
                request_decorations(app, backend, &mut work.decorate, pane);
            }
        }
        // El espacio libre del pie de cada panel (spec 2026-09-10): se pide
        // cuando un listado aterriza o se refresca, y solo si el pie está
        // encendido — con él apagado la tabla de montaje no le hace falta a
        // nadie. Un fallo deja la cache como estaba: el pie calla el espacio
        // antes que inventarlo.
        //
        // EN LÍNEA pero ACOTADO (revisión M5): `Backend` no es `Clone`, así
        // que no se puede lanzar a una tarea como hace la ventana, y la
        // enumeración hace un `statvfs` por montaje con 200 ms de plazo cada
        // uno — un montaje de red colgado paraba el bucle entero. El tope de
        // 250 ms es el precio máximo por listado; pasado, el pie se queda con
        // la tabla anterior, que sigue teniendo el montaje correcto.
        if app.chrome.pane_footer()
            && std::mem::take(&mut app.volumes_stale)
            && let Ok(Ok(vols)) = tokio::time::timeout(
                std::time::Duration::from_millis(250),
                backend.volumes(false),
            )
            .await
        {
            app.volumes = vols;
        }
        turn::drain_pending(
            app,
            backend,
            capture,
            &mut Console::new(&mut events, terminal),
            &mut work,
        )
        .await;
        turn::open_retained_modals(app, &mut work);
        turn::prepare_frame(app, backend, terminal, lua_host.as_ref()).await?;
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
            // El subshell muere CON norte (#142), y lo hace por el `Drop` de
            // `Subshell` y no aquí: el bucle también sale por `RunError`, y un
            // cierre que solo cubriera esta rama dejaría un shell huérfano
            // justo cuando la terminal se rompió.
            drain_notices(app, &mut session_push);
            let last = (!app.session.detached)
                .then(|| capture_session(app, &mut session_push))
                .flatten();
            session_push.close(last).await;
            return Ok(());
        }
        turn::after_frame(app, backend, &mut work, painted.area).await;
        // La pantalla de arranque `brief` caduca con el reloj del PINTADO, el
        // mismo que caduca los avisos: medirla con otro sería un plazo que los
        // tests no pueden fijar. Va después del frame porque lo que promete es
        // «se ve, y se quita sola», no «se quita antes de verse».
        crate::splash::tick(app);
        // La fila que el lector eligió por su número: se navega aquí, donde
        // está el backend. Hoy toda fila del splash lleva un directorio, así
        // que esto es un `cd` normal —con su ritual de vuelta— y no una
        // segunda puerta al despachador.
        if let Some((_, Some(arg))) = app.pending_splash_row.take() {
            match norte_proto::VPath::parse(&arg) {
                Ok(destino) => {
                    let outcome = cd(
                        app,
                        backend,
                        &mut Console::new(&mut events, terminal),
                        destino,
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
                }
                // Una fila con una ruta que no parsea no navega y lo dice: la
                // escribió esta sesión, así que si pasa es un fallo nuestro.
                Err(_) => app.message = Some(t("err-invalid-path")),
            }
        }
        // El panel de procesos que se abre y se cierra solo (`[ui]
        // processes_panel = "auto"`, spec 2026-09-15): un panel que ocupa un
        // tercio de la pantalla para decir «nada en marcha» no se gana el
        // sitio, y buscar la tecla justo cuando empieza una copia tampoco.
        //
        // Abre SIN llevarse el teclado —el lector está en su listado— y solo
        // cierra lo que abrió él: un panel que abrió una persona se queda.
        if app.chrome.processes_panel() == norte_config::load::ProcessesPanel::Auto {
            let hay_tareas = !app.board.rows().is_empty();
            if hay_tareas && app.processes_slot().is_none() {
                app.open_processes(false);
                app.processes_auto = true;
            } else if !hay_tareas && app.processes_auto {
                app.close_processes();
                app.processes_auto = false;
            }
        }
        turn::spawn_probes(app, backend, &mut work);
        tokio::select! {
            _ = session_tick.tick() => {
                // Un segundo más para el aviso de la barra (spec 2026-09-10).
                app.tick_notices();
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
                let refreshed = on_tick(app, backend, &mut Console::new(&mut events, terminal)).await;
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
                    let refreshed =
                        refresh_panes(app, backend, &mut Console::new(&mut events, terminal)).await;
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
                    ConnEvent::Restored => t("msg-daemon-restored"),
                    // Un relevo y una parada se ven igual en cuanto la
                    // conexión cae: este aviso llega antes y es lo único que
                    // los distingue.
                    ConnEvent::GoingAway { reconnect: true } => t("msg-daemon-handover"),
                    ConnEvent::GoingAway { reconnect: false } => t("msg-daemon-stopping"),
                    // `Lost` y el comodín juntos: `ConnEvent` es no
                    // exhaustivo, y un evento de un SDK más nuevo se lee como
                    // una pérdida, que es lo conservador.
                    ConnEvent::Lost | _ => t("msg-daemon-lost"),
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
            Some(f) = async {
                match &mut failed {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                // #322: una conexión NO se abrió, y con el motivo. Llega
                // DESPUÉS del error del listado que la disparó —el handler que
                // lo espera bloquea este select mientras tanto—, así que pisa
                // la categoría genérica con la frase concreta, que es el orden
                // que se quiere.
                app.note_connection_failed(&f);
            }
            Some(n) = async {
                match &mut plugin_notices {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                // ADR 0100: la frase de un hook, atribuida al plugin, como
                // mensaje transitorio de la barra — igual que un fallo de
                // conexión. No es un indicador persistente: habla de una
                // mutación que ya pasó.
                app.note_plugin_notice(&n);
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
            (epoca, res) = async {
                match &mut work.log_tail {
                    Some(pr) => (pr.epoca, (&mut pr.rx).await.ok()),
                    None => std::future::pending().await,
                }
            } => {
                // El registro del daemon (#328): el hueco se limpia SIEMPRE,
                // haya contestado, fallado o muerto la task — el freno de
                // `log_next_at` es lo que evita el reintento en bucle, y un
                // canal cerrado no puede dejar el sondeo apagado para siempre.
                work.log_tail = None;
                if let Some(res) = res {
                    crate::logview::aterrizar_tail(app, epoca, res);
                }
            }
            (epoca, res) = async {
                match &mut work.log_level {
                    Some(pr) => (pr.epoca, (&mut pr.rx).await.ok()),
                    None => std::future::pending().await,
                }
            } => {
                // El nivel que el daemon dejó puesto DE VERDAD, que puede no
                // ser el que se pidió: su anillo es global a sus clientes y
                // solo sube.
                work.log_level = None;
                if let Some(res) = res {
                    crate::logview::aterrizar_nivel(app, epoca, res);
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
                    && let Some((map, cols, headers)) = res
                {
                    // Los RÓTULOS no son del listado: dicen cómo se llama una
                    // columna, y eso vale aunque esta respuesta llegue tarde
                    // o para otro directorio. Van al modelo compartido, fuera
                    // del guard anti-stale de las celdas.
                    app.columns.apply_plugin_headers(headers);
                    if let Some(p) = app.panes.browser_mut(f.slot)
                        && p.dir() == &f.dir
                    {
                        p.set_decorations(map);
                        // #117-follow-up: los valores de columnas plugin:
                        // viajan en el mismo fetch y comparten el guard
                        // anti-stale.
                        p.set_plugin_columns(cols);
                    }
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
                harvest_ai_rename(app, backend, &mut work, res);
            }
            res = async {
                // fs.rename_batch_plan en vuelo (§17): cosecha sin bloquear,
                // molde del brazo de `work.ai_rename`.
                match &mut work.rename_batch {
                    Some(r) => (&mut r.handle).await,
                    None => std::future::pending().await,
                }
            } => {
                harvest_rename_batch(app, &mut work, res);
            }
            res = async {
                // index.search_semantic en vuelo (M4-IA-2): cosecha sin
                // bloquear — molde del brazo de `work.ai_rename`.
                match &mut work.semantic {
                    Some(r) => (&mut r.handle).await,
                    None => std::future::pending().await,
                }
            } => {
                harvest_semantic(app, &mut work, res);
            }
            res = async {
                // El informe de un lote de sumas (#311): mismo molde. La espera
                // del estado terminal vive DENTRO del spawn, así que aquí no
                // hay más que cosechar.
                match &mut work.checksum {
                    Some(r) => (&mut r.handle).await,
                    None => std::future::pending().await,
                }
            } => {
                harvest_checksum(app, &mut work, res);
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
                // `[ui] alt_menu` en caliente, con la misma exención. NO se
                // pregunta al terminal: se lee lo que contestó al arrancar
                // (`alt_menu::consultar_soporte`), porque con el lector de
                // eventos vivo la pregunta bloquea dos segundos y dice «no».
                if let Err(e) = crate::alt_menu::set(
                    cfg.common.ui_alt_menu.unwrap_or(false),
                    crate::alt_menu::soportado,
                    terminal.backend_mut(),
                ) {
                    tracing::warn!(error = %e, "no se pudo cambiar el protocolo de teclado");
                }
                if pane_attr_ids(app) != attrs_before {
                    let refreshed =
                        refresh_panes(app, backend, &mut Console::new(&mut events, terminal)).await;
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
                    alt_solo.soltar();
                    mouse::on_mouse(
                        app,
                        backend,
                        capture,
                        &mut Console::new(&mut events, terminal),
                        resolver,
                        help_lines,
                        lang,
                        quick_mode,
                        confirm_quit,
                        &cfg,
                        &mut work,
                        me,
                    )
                    .await;
                    // Un clic en la barra de teclas dejó una tecla que
                    // sintetizar (spec 2026-09-10): va por `on_key`, el
                    // MISMO camino que la tecla de verdad, con los tres
                    // resolvers. No hay segundo despacho que pueda divergir.
                    if let Some(key) = app.pending_key.take() {
                        on_key(
                            app,
                            backend,
                            capture,
                            &mut Console::new(&mut events, terminal),
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
                    }
                } else if let Event::Key(key) = event
                    && crate::alt_menu::es_modificador(&key)
                {
                    // Una modificadora sola no es una tecla para el keymap:
                    // solo alimenta el gesto. Con el menú abierto lo pliega;
                    // con otro overlay delante no hace nada, como F9 allí.
                    if alt_solo.tecla(&key) && (app.menu.is_some() || !mouse::overlay_open(app)) {
                        // Como F9 por `on_key`: una secuencia a medias (`g`…)
                        // se abandona, o la siguiente tecla tras cerrar el
                        // menú la completaría.
                        resolver.reset();
                        app.toggle_menu();
                    }
                } else if let Event::Key(key) = event
                    // `Repeat` cuenta como pulsación: con el protocolo de
                    // kitty pedido, una flecha mantenida llega así, y
                    // filtrando solo `Press` avanzaría una fila. Sin él,
                    // crossterm lo manda todo como `Press`.
                    && key.kind != crossterm::event::KeyEventKind::Release
                {
                    alt_solo.tecla(&key);
                    on_key(
                        app,
                        backend,
                        capture,
                        // La consola CON terminal: este es el camino por el que
                        // se llega a una navegación larga, y es el único que
                        // necesita poder repintarse mientras espera.
                        &mut Console::new(&mut events, terminal),
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
        // Un cambio de perfil pedido por `profile.pick`/`next`/`prev`, una vez
        // por vuelta y AQUÍ.
        //
        // No al lado de cada `run_command` como `launch_pending_open`: aquél
        // necesita la terminal, que `keys.rs` tiene; éste necesita las capas,
        // los tres resolvers y la config entera, que solo están aquí. Cinco
        // sitios pasándose doce parámetros para servir a tres comandos sería
        // el cableado peor.
        if let Some(nombre) = app.pending_profile.take() {
            cambia_de_perfil(
                &nombre,
                app,
                backend,
                &mut Console::new(&mut events, terminal),
                resolver,
                viewer_resolver,
                dialog_resolver,
                help_lines,
                lang,
                &mut layers,
                cli_preset.as_deref(),
                &mut quick_mode,
                &mut confirm_quit,
                &mut cfg,
            )
            .await;
        }
        // Las unidades del sidebar, una vez por vuelta y DESPUÉS del cambio de
        // perfil, que monta una pantalla nueva y por tanto también las pide.
        // Tres caminos —abrir el panel, desplegar su sección, montar una
        // disposición que ya lo trae— y un solo sitio donde se sirven, que es
        // lo que evita que falten justo por el que nadie recordó (ver
        // `App::places_wants_drives`).
        crate::screens::drain_places_drives(app, backend).await;
        // Resize/Focus/etc: el draw del inicio del loop repinta solo.
    }
}

/// Cambia de perfil en caliente (ADR 0079, D8).
///
/// El orden ES el diseño:
///
/// 1. Volcar el estado del perfil que se deja, ANTES de tocar nada. Si esto
///    fuese después de recargar, lo que se guardaría bajo el nombre viejo
///    sería el estado del perfil nuevo.
/// 2. Rehacer las capas con el directorio del perfil y recargar. Esa recarga
///    es `reload_config`, que ya era «todo o nada»: si falla, deja la config
///    vigente y avisa, que es justo lo que D7 pide para un cambio (`Switch`).
/// 3. Solo si aplicó: montar la disposición guardada del perfil nuevo, darlo
///    por activo, y decir lo que no se pudo aplicar sin reiniciar.
///
/// Lo que NO hace falta hacer a mano: tema, keymap, columnas, favoritos y
/// openers los aplica el paso 2, y la siembra de cada hueco sale de
/// `apply_session`, que ya sabe leer la disposición bajo la clave del perfil.
#[expect(clippy::too_many_arguments, reason = "wiring del bucle, no API")]
async fn cambia_de_perfil(
    nombre: &std::ffi::OsStr,
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
    resolver: &mut Resolver,
    viewer_resolver: &mut Resolver,
    dialog_resolver: &mut Resolver,
    help_lines: &mut Vec<ratatui::text::Line<'static>>,
    lang: norte_i18n::Lang,
    layers: &mut Layers,
    cli_preset: Option<&str>,
    quick_mode: &mut nav::Mode,
    confirm_quit: &mut config::ConfirmQuit,
    cfg: &mut config::LoadedConfig,
) {
    // 1 — el estado del perfil que se DEJA, capturado antes de nada.
    let saliente = app.session_body();
    let antes = cfg.common.clone();

    // 2 — las capas nuevas. Un nombre que el resolutor no puede colgar deja
    // las capas como estaban, y entonces la recarga no cambiaría nada: se
    // rehúsa el cambio en vez de fingir que pasó algo.
    let nuevas = norte_config::standard_layers_with_profile(Some(nombre));
    if !nuevas
        .dirs
        .iter()
        .any(|(_, k)| *k == config::Layer::Profile)
    {
        app.message = Some(ta(
            "msg-profile-not-applied",
            &[("profile", &nombre.to_string_lossy())],
        ));
        return;
    }
    let anteriores = std::mem::replace(layers, nuevas);
    let aplicado = reload_config(
        app,
        backend,
        resolver,
        viewer_resolver,
        dialog_resolver,
        help_lines,
        lang,
        layers,
        cli_preset,
        quick_mode,
        confirm_quit,
        cfg,
    )
    .await;
    if !aplicado {
        // `reload_config` ya dejó la config vigente y dijo por qué; aquí solo
        // se devuelven las capas, que son lo único que este nivel había
        // tocado. El lector se queda en el perfil que tenía, con el estado
        // que tenía.
        *layers = anteriores;
        return;
    }

    // 3 — el perfil nuevo es el activo, y su pantalla se monta desde el mismo
    // cuerpo: `apply_session` lee la disposición bajo la clave del perfil y
    // guarda las de los demás.
    app.active_profile = Some(nombre.to_os_string());
    // El vector que devuelve se descarta a propósito, igual que hace
    // `apply_session_value` en el arranque: quién necesita listado lo resuelve
    // `refresh_panes` justo después, que es el camino que ya usa la recarga
    // cuando los attrs de un pane cambian.
    let _ = app.apply_session(&saliente);
    // Y `[profile.start]`, que es lo que hace útil un perfil recién creado o
    // uno que llega de otra máquina: dónde abre cada hueco la primera vez. Va
    // DESPUÉS de la sesión porque la sesión gana — un perfil es un espacio de
    // trabajo, no un marcador que te devuelve al principio cada vez.
    //
    // El veto NO es `saliente`: ése es `session_body()`, la pantalla de AHORA,
    // que nombra todos los huecos vivos y por tanto no dejaría sembrar nunca.
    // Lo pone el propio método, con lo leído del disco y lo ya sembrado.
    let _ = app.seed_profile_start(&cfg.common.profile_start);
    let _ = crate::refresh::refresh_panes(app, backend, events).await;
    let fuera = crate::app::profile::no_aplicable_en_caliente(&antes, &cfg.common);
    // Lo que el FICHERO del perfil trae y no se entiende gana a los otros dos
    // mensajes: «no se pudo aplicar en caliente» describe un límite de este
    // proceso, y esto describe líneas que no van a hacer nada nunca. Callarlas
    // es lo que convertía `[profile.start]` en una trampa.
    for aviso in &cfg.common.profile_warnings {
        tracing::warn!(motivo = %aviso, "línea del perfil ignorada");
    }
    app.message = Some(if !cfg.common.profile_warnings.is_empty() {
        ta(
            "msg-profile-config-ignored",
            &[
                ("profile", &nombre.to_string_lossy()),
                ("n", &cfg.common.profile_warnings.len().to_string()),
            ],
        )
    } else if fuera.is_empty() {
        ta(
            "msg-profile-switched",
            &[("profile", &nombre.to_string_lossy())],
        )
    } else {
        ta(
            "msg-profile-switched-partial",
            &[
                ("profile", &nombre.to_string_lossy()),
                ("keys", &fuera.join(", ")),
            ],
        )
    });
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
