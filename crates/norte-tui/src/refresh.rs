//! El ritual de refresco: el tick que reacciona a las tasks que acaban de
//! terminar, la recarga de ambos panes tras una mutación, y lo que hay que
//! soltar después.
//!
//! Vivía en el root del binario `ntc` —un crate DISTINTO de esta lib—, y lo
//! nombran cuatro de los módulos de test que siguen ahí esperando a que salgan
//! sus dependencias.
//!
//! Los tres disparadores del refresco (una mutación terminada en [`on_tick`],
//! el confirm del picker de columnas y el hot-reload de `[ui.columns]`) pasan
//! por el MISMO embudo, [`after_panes_refresh`]: es lo que garantiza que un
//! drenador paginado viejo no duplique entradas de un pane re-listado.

use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use futures::StreamExt as _;
use norte_core::backend::Backend;
use norte_frontend::layout::BySlot;
use norte_i18n::{t, ta};
use norte_proto::Error;

use crate::app::{App, Modal, error_category, error_message};
use crate::fill::{Fill, release_refreshed_fill};
use crate::jobs::SearchRun;
use crate::navigate::listing;
use crate::probes::Probed;

/// Tick: refresca snapshots del panel y reacciona a las tasks que ACABAN
/// de terminar — colisión con contexto → a la COLA de diálogos (jamás se
/// pisa un modal abierto, hallazgo B1); el resto → mensaje por categoría +
/// refresh de ambos panes (una mutación pudo cambiarlos).
/// (Strings de mensaje hardcodeados hasta Fluent — fase 9, issue #1.)
/// Devuelve qué panes REFRESCÓ (una mutación terminó y `refresh_panes` los
/// reescribió con el listado completo): el run loop aplica entonces el
/// ritual de [`after_panes_refresh`] — un drenador viejo de un pane
/// re-listado duplicaría entradas si siguiera vivo.
pub async fn on_tick(app: &mut App, backend: &Backend, events: &mut EventStream) -> [bool; 2] {
    let finished = app.board.tick();
    if finished.is_empty() {
        app.open_next_pending();
        return [false; 2];
    }
    let mut refresh = false;
    for fin in finished {
        use norte_proto::TaskState;
        match fin.state {
            // #139: contar no muta nada, así que no recarga los paneles — y su
            // resultado ES su progreso: el último snapshot trae el total.
            TaskState::Completed if fin.progress.kind == norte_proto::TaskKind::DirSize => {
                let (bytes, entradas) = (fin.progress.bytes_done, fin.progress.entries_done);
                // Si el diálogo de propiedades esperaba ESTE recuento, el
                // número va ahí; si no, a la barra.
                if !app.properties_sized(fin.progress.task_id, bytes, entradas) {
                    // «Al menos» cuando parte del árbol no se pudo leer
                    // (#251). El número corto es la dirección peligrosa del
                    // error —este recuento se usa para decidir si algo cabe
                    // en el destino—, así que decirlo redondo sin haberlo
                    // podido contar entero es una respuesta equivocada, no
                    // una respuesta incompleta.
                    //
                    // `Some(0)` y `None` NO son lo mismo: el primero es «los
                    // conté y no hubo», el segundo «quien lo emite no cuenta
                    // esto» (un daemon 0.52). Solo el primero autoriza a
                    // decir el total a secas.
                    let saltados = fin.progress.unreadable.unwrap_or(0);
                    let tamano = norte_frontend::human_bytes(bytes);
                    let cuantas = entradas.to_string();
                    app.message = Some(if saltados > 0 {
                        ta(
                            "msg-dir-size-partial",
                            &[
                                ("size", &tamano),
                                ("count", &cuantas),
                                ("skipped", &saltados.to_string()),
                            ],
                        )
                    } else {
                        ta("msg-dir-size", &[("size", &tamano), ("count", &cuantas)])
                    });
                }
            }
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
pub async fn refresh_panes(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
) -> [bool; 2] {
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
pub fn after_panes_refresh(
    app: &mut App,
    refreshed: [bool; 2],
    fill: &mut BySlot<Fill>,
    last_probed: &mut Probed,
    search_run: &mut Option<SearchRun>,
) {
    if refreshed == [false; 2] {
        return;
    }
    release_refreshed_fill(&app.panes, &refreshed, fill, last_probed);
    reap_search_run(app, search_run);
    if app.help.is_some() {
        app.freeze_help_facts();
    }
}

/// Suelta el [`SearchRun`] si su pane SALIÓ del modo virtual (un `cd`/refresh
/// lo apagó): su drenador alimentaría un listado real. Cancela la Task si
/// sigue viva (regla 3).
pub fn reap_search_run(app: &App, search_run: &mut Option<SearchRun>) {
    if let Some(s) = search_run.as_ref()
        && !app.panes[s.pane].virtual_search
    {
        s.task.cancel();
        *search_run = None;
    }
}
