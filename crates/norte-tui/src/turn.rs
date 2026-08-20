//! La vuelta del bucle de eventos, por partes.
//!
//! Cada vuelta hace siempre lo mismo y en este orden: drenar lo que el
//! despacho dejó PEDIDO ([`drain_pending`]), plantar los modales que
//! esperaban turno ([`open_retained_modals`]), preparar el frame
//! ([`prepare_frame`]), pintar —eso se queda en el bucle, que es quien tiene
//! la terminal—, devolver al modelo lo que solo el frame pintado sabe
//! ([`after_frame`]) y pedir lo que falte por hidratar ([`spawn_probes`]).
//!
//! Está aquí y no en `App` por una razón que se repite: son cosas que el
//! modelo no puede hacer solo —lanzan Tasks, suspenden la terminal, hablan
//! con el backend— y el bucle sí. Sacarlas del bucle es lo que deja `run`
//! legible: lo que queda ahí es el ORDEN, que es su única responsabilidad.

use crossterm::event::EventStream;
use norte_core::backend::Backend;
use norte_i18n::{t, ta};
use norte_proto::VPath;

use crate::app::{App, Modal};
use crate::event_loop::RunError;
use crate::jobs::{InFlight, launch_compare, launch_sync_apply, launch_sync_plan};
use crate::lua::refresh_lua_status;
use crate::mouse;
use crate::navigate::{apply_cd, cd};
use crate::overlays::{fetch_plugin_page, settle_help_over_modal, watch_refresh_allowed};
use crate::probes::{
    STAT_BATCH_MAX, STAT_WINDOW_RADIUS, spawn_compare_stat_probe, spawn_preview_fetch,
    spawn_stat_probe,
};
use crate::refresh::{after_panes_refresh, refresh_panes};
use crate::suspend::run_suspended;
use crate::tty;
use crate::ui;

/// Lanza lo que el despacho dejó PEDIDO: comparar, sincronizar, aprobar un
/// plan, volver a casa tras desconectar, preguntar por el destino de una
/// transferencia y suspender la TUI para un programa del usuario.
///
/// Todo eso se drena en la CABECERA de la vuelta y en ningún otro sitio. La
/// razón es siempre la misma: los brazos que responden a una tecla tienen
/// cada uno su propio final anticipado, así que el único punto que los cubre
/// a todos —presentes y futuros— es este.
pub async fn drain_pending(
    app: &mut App,
    backend: &Backend,
    terminal: &mut tty::Tui,
    capture: &mut mouse::Capture,
    events: &mut EventStream,
    work: &mut InFlight,
) {
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
        let space_notice = norte_frontend::space::warning(check.total, free, norte_i18n::active());
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
        let outcome = cd(app, backend, events, casa).await;
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
            let refreshed = refresh_panes(app, backend, events).await;
            after_panes_refresh(
                app,
                refreshed,
                &mut work.fill,
                &mut work.probed,
                &mut work.search,
            );
        }
    }
}

/// Planta los modales que llegaron con otro abierto y esperaban turno: el
/// plan de rename IA y los hits semánticos.
///
/// Uno por vuelta y en este orden: si el plan acaba de abrir, los hits siguen
/// esperando. Jamás se pisa un modal abierto —esa es la disciplina de
/// `open_next_pending`— y por eso lo retenido se guarda en [`InFlight`] en
/// vez de plantarse al cosecharlo.
pub fn open_retained_modals(app: &mut App, work: &mut InFlight) {
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
}

/// Lo que hay que dejar listo ANTES de pintar: la barra de Lua, la ayuda
/// maquetada para el terminal sobre el que va a pintarse y la ventana de cada
/// pane reconciliada con su cursor.
///
/// La ventana antes del draw y no después: hacerlo al revés costaba un frame
/// de retraso, y el cursor podía caer fuera de la ventana pintada justo al
/// llegar al borde — o sea, desaparecer de la pantalla.
///
/// # Errors
///
/// [`RunError::Terminal`] si no se puede medir el terminal.
pub async fn prepare_frame(
    app: &mut App,
    backend: &Backend,
    terminal: &mut tty::Tui,
    lua_host: Option<&crate::lua::LuaHost>,
) -> Result<(), RunError> {
    // Barra Lua en cada vuelta, ANTES del draw (cacheada en el host).
    refresh_lua_status(app, lua_host);
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
    Ok(())
}

/// Lo que solo el frame YA pintado sabe, de vuelta al modelo: su alto real
/// (de ahí salen la paginación y el radio de la sonda), la geometría con la
/// que el ratón resuelve sus clicks, y lo que los huecos acoplados —visor,
/// árbol, hoja de atributos— quieren enseñar a continuación.
///
/// Un click resuelto contra un layout que no es el pintado no falla
/// ruidosamente: marca el fichero de al lado.
pub async fn after_frame(
    app: &mut App,
    backend: &Backend,
    work: &mut InFlight,
    painted: ratatui::layout::Rect,
) {
    // #124: el alto REAL del viewport vuelve al modelo tras cada frame —
    // la paginación (`page_step`) y el radio de la sonda de stat salen de
    // ahí en vez de constantes que mienten en cualquier terminal que no
    // mida justo eso. Con el visor abierto son 0 filas (ningún pane
    // pintado) y el modelo vuelve a sus fallbacks.
    // El alto REAL del frame que se acaba de pintar: si la terminal cambió
    // de tamaño entre `before_frame` y el draw, este es el bueno, y de él
    // salen la paginación y el radio de la sonda de stat.
    ui::before_frame(app, painted);
    // MISMO trato para la geometría del ratón: el draw es quien sabe
    // dónde cayó cada pane y con qué scroll, así que la devuelve al
    // modelo y el hit test resuelve contra la pantalla que el usuario
    // está mirando. Sin esto habría que recalcular el layout en cada
    // click, y un click resuelto contra un layout que no es el pintado
    // no falla ruidosamente: marca el fichero de al lado.
    mouse::after_frame(
        app,
        ui::pane_geometry(app, painted),
        ui::tab_zones(app, painted),
        ui::menu_zones(app, painted),
        ui::places_zones(app, painted),
    );
    // L3: el visor acoplado sigue al cursor del listado activo. Lo que se
    // pide sale de `preview::want`, que devuelve `None` cuando el hueco no
    // se colocó — cerrado, detrás de una pestaña, o colapsado por falta de
    // sitio. Por eso la suspensión de un hueco oculto no es una
    // comprobación que alguien pueda olvidarse de escribir: sin objetivo
    // no hay nada que pedir.
    {
        let res = ui::resolved_for(app, painted);
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
}

/// Pide lo que falta por hidratar: los tamaños de las entradas VISIBLES de un
/// listado lazy (#52) y el de la fila seleccionada del panel de diferencias
/// (#157). Una sonda de cada en vuelo como mucho.
pub fn spawn_probes(app: &mut App, backend: &Backend, work: &mut InFlight) {
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
}
