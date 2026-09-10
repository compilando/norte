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
    LOG_TAIL_PERIODO, STAT_BATCH_MAX, STAT_WINDOW_RADIUS, spawn_compare_stat_probe,
    spawn_log_level, spawn_log_tail, spawn_preview_fetch, spawn_stat_probe,
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
    capture: &mut mouse::Capture,
    // Consola ATADA, y esto tuvo que corregirse: aquí abajo hay un `cd`
    // —el de `pending_disconnect_dest`— y desconectar de un remoto suele
    // volver por el rastro a OTRO remoto, o sea a otra conexión que tarda. Con
    // una consola desligada eso era exactamente el congelado de #323, y encima
    // con `app.busy` puesto: el estado decía «estoy enseñando un spinner» y no
    // había nada en pantalla.
    events: &mut crate::console::Console<'_>,
    work: &mut InFlight,
) {
    // Ninguna espera sobrevive a una vuelta del bucle: la que hubiera se
    // resolvió, se canceló o falló dentro de la vuelta anterior. Limpiar aquí
    // hace que un `Busy` colgado sea estructuralmente imposible, pase lo que
    // pase con los caminos de salida de quien espera — presentes y futuros.
    app.busy = None;
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
    // #311: el despacho resolvió QUÉ resumir; leer el fichero de sumas y
    // esperar el informe es I/O, y eso es del run loop. Mismo reparto.
    if let Some(req) = app.pending_checksum.take() {
        crate::mutations::checksum_start(app, backend, work, req).await;
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
        // Los DOS diálogos, que son los dos caminos de una transferencia: con
        // varios ítems se confirma y con uno se teclea el nombre. Repartir el
        // aviso por cuál de los dos salió es lo que dejaba a un fichero suelto
        // copiándose sin decir nada (#343).
        if let Some(
            Modal::ConfirmTransfer { space, confine, .. }
            | Modal::TransferName { space, confine, .. },
        ) = app.modal.as_mut()
        {
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
    // #140: el panel que acaba de desconectar se va por el mismo `cd` que
    // cualquier otra navegación, con su ritual de vuelta.
    if let Some(destino) = app.pending_disconnect_dest.take() {
        let outcome = cd(app, backend, events, destino).await;
        apply_cd(
            &app.panes,
            &mut work.fill,
            &mut work.decorate,
            &mut work.probed,
            &mut work.search,
            outcome,
        );
    }
    // La secuencia va al EMULADOR y no al programa: se escribe cruda en la
    // salida del terminal, que es de este bucle y no de `dispatch` (#286).
    if let Some(bytes) = app.pending_osc52.take() {
        use std::io::Write as _;
        let mut salida = std::io::stdout();
        if salida
            .write_all(&bytes)
            .and_then(|()| salida.flush())
            .is_err()
        {
            app.message = Some(t("msg-clipboard-failed"));
        }
    }
    if let Some(pending) = app.take_pending_shell() {
        atender_suspension(app, backend, capture, events, work, pending).await;
    }
    if std::mem::take(&mut app.pending_subshell) {
        atender_subshell(app, backend, capture, events, work).await;
    }
}

/// Suspende la TUI para el programa que el despacho dejó pedido (#135).
///
/// Sale de [`drain_pending`] por tamaño, no por concepto: sigue siendo un
/// drenaje de cabecera de vuelta y no debe llamarse desde ningún otro sitio.
async fn atender_suspension(
    app: &mut App,
    backend: &Backend,
    capture: &mut mouse::Capture,
    events: &mut crate::console::Console<'_>,
    work: &mut InFlight,
    pending: crate::app::PendingShell,
) {
    {
        let crate::app::PendingShell {
            argv,
            cwd,
            wait_for_key,
            check_regular,
        } = pending;
        // #303: la comprobación va PEGADA al lanzamiento, y por eso vive aquí
        // y no donde se resolvió el gesto. Hacerla en `on_tick` —donde se
        // decide abrir el editor— no compraba nada: entre aquello y esto corre
        // `refresh_panes`, o sea el re-listado ENTERO de los dos paneles, que
        // en un pane remoto son segundos. Es justo la ventana que este
        // chequeo existe para estrechar.
        //
        // Sigue sin cerrarla: entre este `stat` y el `exec` queda hueco.
        if let Some(motivo) = crate::gestures::motivo_para_no_lanzar(backend, check_regular).await {
            app.message = Some(motivo);
            return;
        }
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
        // La terminal sale de la consola, que es su dueña desde que una espera
        // larga necesitó repintarse. Sin terminal no hay nada que ceder: el
        // caso solo existe en tests, y ahí suspenderse no significa nada.
        let Some(terminal) = events.terminal() else {
            return;
        };
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

/// Le cede la terminal al subshell persistente (#142), arrancándolo si es la
/// primera vez.
///
/// El shell se crea PEREZOSAMENTE y muere con la sesión: quien nunca pulsa la
/// tecla no paga un `fork`, y quien la pulsa dos veces vuelve al mismo shell
/// —con su historial y sus variables— que es la diferencia entera entre esto y
/// el scrollback de antes.
///
/// POSIX (ADR 0084): en Windows no hay pty que ceder, así que la tecla
/// DECLINA con el mismo mensaje que `app.terminal` sobre un pane remoto — que
/// es la verdad, y era lo que la ADR prometía sin que nada lo cumpliera.
#[cfg(not(unix))]
#[allow(clippy::unused_async)] // misma firma que la de Unix: el llamante no bifurca.
async fn atender_subshell(
    app: &mut App,
    _backend: &Backend,
    _terminal: &mut tty::Tui,
    _capture: &mut mouse::Capture,
    _events: &mut crate::console::Console<'_>,
    _work: &mut InFlight,
) {
    app.message = Some(t("msg-subshell-not-here"));
}

#[cfg(unix)]
async fn atender_subshell(
    app: &mut App,
    backend: &Backend,
    capture: &mut mouse::Capture,
    events: &mut crate::console::Console<'_>,
    work: &mut InFlight,
) {
    // Sin acorde suelto no se cede la terminal: el lector no tendría con qué
    // volver. Ver `detach_chord`.
    let Some(acorde) = app.subshell_chord else {
        app.message = Some(t("msg-subshell-no-key"));
        return;
    };
    // Un pane REMOTO no tiene directorio local, y un shell local ahí sería un
    // shell en otro sitio del que el panel enseña. Mismo veredicto y mismo
    // mensaje que `app.terminal`.
    let dir = match crate::gestures::shell_cwd(app) {
        Ok(dir) => dir,
        Err(msg) => {
            app.message = Some(msg);
            return;
        }
    };
    // Un shell que MURIÓ (el lector escribió `exit`) se sustituye, no se
    // resucita: el pty de un hijo muerto no acepta escrituras y la tecla
    // habría dejado de funcionar para el resto de la sesión.
    if work
        .subshell
        .as_mut()
        .is_some_and(crate::subshell::Subshell::muerto)
    {
        work.subshell = None;
    }
    if work.subshell.is_none() {
        let size = events
            .terminal()
            .and_then(|t| t.size().ok())
            .map_or((80, 24), |s| (s.width, s.height));
        match crate::subshell::Subshell::arrancar(&dir, size) {
            Ok(sub) => work.subshell = Some(sub),
            Err(e) => {
                app.message = Some(ta(
                    "msg-shell-failed",
                    &[
                        ("program", "$SHELL"),
                        ("error", &crate::app::detail_for_bar(&e.to_string())),
                    ],
                ));
                return;
            }
        }
    }
    let Some(sub) = work.subshell.as_mut() else {
        return;
    };
    // Auditoría: mismo criterio que la suspensión de arriba (design §D del
    // #135). El journal no ve nada de esto a propósito, y la línea que el
    // lector teclee no acaba en ningún fichero.
    tracing::info!("TUI handed the terminal to its persistent subshell (not journalled)");
    // `block_in_place` y no un `await`: ceder la terminal es I/O bloqueante que
    // dura lo que dure la sesión de shell. Ver `attach_subshell`.
    // La terminal sale de la consola (su dueña desde #323). Sin ella no hay
    // nada que ceder: solo pasa en tests, y ahí el subshell no significa nada.
    let Some(terminal) = events.terminal() else {
        return;
    };
    let cedida = tokio::task::block_in_place(|| {
        crate::suspend::attach_subshell(terminal, capture, sub, &dir, acorde)
    });
    let destino = match cedida {
        Ok(destino) => destino,
        Err(e) => {
            app.message = Some(ta(
                "msg-shell-failed",
                &[
                    ("program", "$SHELL"),
                    ("error", &crate::app::detail_for_bar(&e.to_string())),
                ],
            ));
            None
        }
    };
    // El panel SIGUE al shell: si el lector hizo `cd` ahí dentro, volver deja
    // el panel donde él quedó. Es la otra mitad del seguimiento, y va por el
    // cd de siempre (cancelable, con rastro), jamás por un `set_listing`.
    if let Some(destino) = destino
        && watch_refresh_allowed(app)
    {
        // Un fallo aquí NO se traga: el shell dijo dónde está y norte no ha
        // podido ir, y un seguimiento que a veces no pasa sin decir nada es
        // indistinguible de uno roto.
        let Ok(vpath) = norte_vfs_local::vpath_from_native(&destino) else {
            app.message = Some(t("msg-subshell-bad-cwd"));
            return;
        };
        let outcome = cd(app, backend, events, vpath).await;
        apply_cd(
            &app.panes,
            &mut work.fill,
            &mut work.decorate,
            &mut work.probed,
            &mut work.search,
            outcome,
        );
        return;
    }
    // Y si no se movió, lo que el shell haya tocado en disco se ve igual: por
    // el mismo refresh cancelable que la suspensión, y con el mismo gate (un
    // modal delante se comería la respuesta del lector).
    if watch_refresh_allowed(app) {
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
            seen: norte_frontend::AI_RENAME_PAIR_LIMIT,
            plan: pendiente.plan,
        });
    }
    // Sumas retenidas (#311): misma disciplina. La barra ya prometió que se
    // verían al cerrar el diálogo de delante, y esto es lo que lo cumple.
    if app.modal.is_none()
        && let Some((title_key, rows)) = work.pending_checksums.take()
    {
        app.modal = Some(Modal::Checksums {
            title_key,
            rows,
            offset: 0,
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
        mouse::FrameZones {
            tabs: ui::tab_zones(app, painted),
            menus: ui::menu_zones(app, painted),
            panels: ui::panel_zones(app, painted),
            keys: ui::key_zones(app, painted),
            modal: ui::modal_zones(app, painted),
            places: ui::places_zones(app, painted),
            tree: ui::tree_zones(app, painted),
            extensions: ui::extension_zones(app, painted),
            session: ui::session_zone(app, painted),
            borders: ui::resize_borders(app, painted),
            slots: ui::panel_slots(app, painted),
        },
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
                    // El ancho del HUECO, no el de la pantalla (0.66.0): un
                    // previewer de imagen encoge a lo que le digan, y el
                    // acoplado es la mitad de la terminal. Sin los dos
                    // bordes del marco.
                    let columnas = ui::slot_rect(&res, slot)
                        .map(|r| u32::from(r.width.saturating_sub(2).max(1)));
                    // Empezar otra SUSTITUYE la que hubiera: el `Receiver`
                    // viejo se cae aquí y su respuesta no se aplica nunca.
                    work.preview
                        .set(slot, Some(spawn_preview_fetch(backend, path, columnas)));
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
            Some((slot, crate::metadata::Want::Entry(e, subir))) => {
                if let Some(hoja) = app.panes.metadata_mut(slot) {
                    *hoja = Some((*e, subir));
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
    spawn_log_probes(app, backend, work);
}

/// El registro del DAEMON (#328): tirar de sus líneas y subirle el nivel.
///
/// Dos condiciones antes de gastar una sola RPC, y cada una tapa una avería
/// distinta que ya se había colado:
///
/// - **Hay un daemon.** Con el core embebido —que es el arranque por defecto de
///   `ntc`— el anillo del core es el de este proceso, o sea el que el panel ya
///   está leyendo. Preguntarle a `Backend::log_tail` devuelve `Unsupported` con
///   toda la razón, pero el panel lo leía como un hecho sobre un daemon y
///   acababa poniendo «este daemon no sirve su registro» donde no hay ninguno.
///   La respuesta no es otra frase: es no preguntar.
/// - **El panel se VE.** No que exista: uno escondido detrás de una pestaña que
///   no es la activa sigue existiendo, y sondearlo son dos RPC por segundo, toda
///   la sesión, por algo que nadie tiene delante.
///
/// Con las dos cumplidas se pregunta incluso con la fuente puesta en «esta
/// terminal», porque es la única forma de saber si hay una segunda fuente que
/// ofrecer —y por tanto de decidir si la tecla `s` significa algo.
///
/// Sin temporizador propio: esta terminal ya repinta por frame y su bucle
/// despierta diez veces por segundo, así que lo único que hace falta es el
/// freno de [`crate::probes::LOG_TAIL_PERIODO`]. La ventana sí necesita reloj
/// porque solo repinta cuando alguien hace algo.
fn spawn_log_probes(app: &mut App, backend: &Backend, work: &mut InFlight) {
    // La del transporte se pregunta al `Backend` y no al estado copiado en
    // `App`: aquí se está a punto de hablar por el cable, y quien decide si hay
    // cable es quien lo tiene.
    if !backend.is_remote() || app.log_slot_visible().is_none() {
        return;
    }
    // Lo que la tecla dejó pedido: subirle el nivel al anillo del daemon. Se
    // toma SIEMPRE aunque haya otra en vuelo —la última pulsación manda— y la
    // respuesta dice de paso si ese daemon sabe de registro.
    //
    // Y se descarta sin pedir nada a un daemon que ya dijo que no tiene
    // registro: subirle el nivel a un anillo que no existe es una RPC por
    // pulsación cuya respuesta ya se sabe.
    if let Some(nivel) = app.log_remote.pide_nivel.take()
        && app.log_remote.debe_pedir()
    {
        work.log_level = Some(spawn_log_level(backend, nivel, app.log_remote.epoca));
    }
    if work.log_tail.is_some()
        || !app.log_remote.debe_pedir()
        || work
            .log_next_at
            .is_some_and(|t| t > tokio::time::Instant::now())
    {
        return;
    }
    work.log_next_at = Some(tokio::time::Instant::now() + LOG_TAIL_PERIODO);
    work.log_tail = Some(spawn_log_tail(
        backend,
        app.log_remote.cursor,
        app.log_remote.epoca,
    ));
}
