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

use norte_core::backend::Backend;
use norte_frontend::layout::BySlot;
use norte_i18n::{t, ta};
use norte_proto::Error;

use crate::app::{App, Modal, error_category, error_message};
use crate::console::Waited;
use crate::fill::{Fill, release_refreshed_fill};
use crate::jobs::SearchRun;
use crate::navigate::listing;
use crate::probes::Probed;
use norte_frontend::busy::{Busy, BusyKind};

/// Si el resultado de esta clase de task es un INFORME que alguien cosecha
/// aparte, y por tanto su final no se anuncia con el `done` genérico.
///
/// Solo las sumas (#311), y por dos razones que van juntas: no mutan nada —así
/// que no hay panes que re-listar— y su respuesta es el veredicto de la
/// cosecha, que un `done` posterior taparía. Una copia o un borrado son lo
/// contrario en las dos cosas.
///
/// ```
/// use norte_proto::TaskKind;
/// assert!(norte_tui::refresh::habla_por_su_informe(TaskKind::Checksum));
/// assert!(!norte_tui::refresh::habla_por_su_informe(TaskKind::Copy));
/// ```
#[must_use]
pub fn habla_por_su_informe(kind: norte_proto::TaskKind) -> bool {
    matches!(kind, norte_proto::TaskKind::Checksum)
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
pub async fn on_tick(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
) -> [bool; 2] {
    let finished = app.board.tick(app.now_ms());
    // Cada tick, no solo cuando algo acaba: la fila que caduca terminó en un
    // tick ANTERIOR, así que colgar la limpieza de `finished` la dejaría en
    // pantalla hasta que otra task cualquiera volviera a pasar por aquí.
    app.board.prune_terminal(app.now_ms());
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
            // #311: la que contesta con un INFORME ya dijo lo suyo, y no mutó
            // nada que haya que re-listar. Un `done` genérico aquí pisaría el
            // veredicto, que es la única respuesta que el gesto tenía que dar.
            TaskState::Completed if habla_por_su_informe(fin.progress.kind) => {}
            TaskState::Completed => {
                refresh = true;
                // #314: un lote de permisos que termina «bien» puede no haber
                // cambiado la mitad —un enlace, un fichero de otro dueño—, y
                // `done` a secas se lee como que sí. El número está en el
                // progreso; lo que faltaba era decirlo.
                let sin_hacer = fin.progress.unreadable.unwrap_or(0);
                app.message = Some(
                    if fin.progress.kind == norte_proto::TaskKind::SetMode && sin_hacer > 0 {
                        ta("msg-chmod-partial", &[("n", &sin_hacer.to_string())])
                    } else {
                        t("msg-done")
                    },
                );
                // #290: el fichero que `pane.edit-new` mandó crear YA existe;
                // el editor se abre ahora y sobre la ruta que se pidió, no
                // sobre lo que haya bajo el cursor.
                if let Some(pendiente) = tomar_creacion(app, fin.progress.task_id) {
                    // La comprobación de #303 NO va aquí: entre este punto y
                    // el lanzamiento corre `refresh_panes`, así que preguntar
                    // ahora dejaría detrás justo la ventana que se quería
                    // estrechar. La suspensión se lleva la ruta y el run loop
                    // pregunta pegado al `exec`.
                    match crate::gestures::edit_created(&pendiente) {
                        Ok(shell) => app.pending_shell = Some(shell),
                        // El fichero SE CREÓ y el editor no se puede abrir: se
                        // dice. Tragarse el `None` dejaba `msg-done` en la
                        // barra y media mitad del gesto perdida sin una
                        // palabra, que es la clase de silencio que este
                        // comando vino a quitar.
                        Err(msg) => app.message = Some(msg),
                    }
                }
                // #250: el archivo se escribió entero y aun así puede llevar
                // dentro dos entradas que en macOS o en Windows son una sola.
                // El `Completed` es verdad y no lo cubre, así que se pregunta.
                if fin.progress.kind == norte_proto::TaskKind::Pack
                    && let Some(aviso) = aviso_de_empaquetado(backend, fin.progress.task_id).await
                {
                    app.message = Some(aviso);
                }
            }
            TaskState::Cancelled => {
                // Una que no muta no tiene panes que re-listar, ni cancelada.
                refresh = refresh || !habla_por_su_informe(fin.progress.kind);
                app.message = Some(t("msg-cancelled"));
                // Sin fichero no hay nada que editar: la intención se suelta
                // para que el SIGUIENTE `edit-new` no abra el fichero de este.
                drop(tomar_creacion(app, fin.progress.task_id));
            }
            TaskState::Failed { error } => {
                // La creación que falló —política, journal, un nombre que el
                // provider rehúsa— NO abre nada: abrir el editor sobre un
                // fichero que no existe es dejar que lo cree él, que es
                // exactamente lo que #290 quitó de en medio.
                drop(tomar_creacion(app, fin.progress.task_id));
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
                    refresh = refresh || !habla_por_su_informe(fin.progress.kind);
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

/// La intención de `pane.edit-new` SI la task que acaba de terminar es la
/// suya, consumiéndola (#290).
///
/// El id se compara a propósito: entre el submit y este tick puede terminar
/// cualquier otra task —una copia, un borrado, otra creación—, y abrir el
/// editor con la primera que pase abriría el fichero equivocado.
///
/// **Solo el id, sin época de conexión, y eso descansa en una invariante del
/// SDK**: tras un relevo del daemon los ids vuelven a empezar (la ventana sí
/// lleva época por esto — `Controller::epoca_conexion`). Aquí es correcto
/// porque `norte-client` sintetiza un desenlace `Failed` para toda task
/// huérfana ANTES de que la conexión nueva reparta ids, conservando el
/// `task_id`: la intención se consume en la conexión vieja. Si esa síntesis
/// desapareciera, un id reciclado abriría el editor sobre un fichero que quizá
/// no se creó — y entonces lo crearía el editor, que es el bug entero de vuelta.
/// El aviso de un empaquetado que acaba de terminar, o `None` si no hay nada
/// que decir (#250).
///
/// Se pregunta al COMPLETAR un `archive.pack` —de uno cancelado no hay archivo
/// del que avisar— y la respuesta corriente es que no hay nada. Que un archivo
/// limpio no diga nada es lo que hace que decir algo signifique algo.
///
/// Un fallo de la llamada también es `None`: si no se pudo preguntar, no hay
/// hallazgo que contar sobre el archivo, y pintar «no se pudo comprobar» sobre
/// un empaquetado que salió bien es ruido.
///
/// La llamada se espera DENTRO del tick, como el `refresh_panes` que viene
/// detrás: contra un daemon atascado el tick ya se para ahí, así que spawnear
/// esta sola compraría poco y costaría un canal.
async fn aviso_de_empaquetado(backend: &Backend, task_id: norte_proto::TaskId) -> Option<String> {
    let informe = backend.archive_pack_report(task_id).await.ok()?;
    let riesgos = informe.risky.len();
    if riesgos == 0 {
        return None;
    }
    // Un informe RECORTADO dice «al menos», que es lo único honesto: la lista
    // se corta en `ARCHIVE_PACK_REPORT_MAX` y pintar «64» sobre un archivo con
    // cuatrocientos es exactamente la mentira que `truncated` existe para
    // impedir. Misma forma que el «al menos» de `fs.dir_size` (#251).
    let clave = if informe.truncated {
        "msg-pack-warnings-partial"
    } else {
        "msg-pack-warnings"
    };
    Some(ta(clave, &[("risky", &riesgos.to_string())]))
}

fn tomar_creacion(app: &mut App, terminada: norte_proto::TaskId) -> Option<norte_proto::VPath> {
    match &app.pending_edit_open {
        Some((id, _)) if *id == terminada => app.pending_edit_open.take().map(|(_, p)| p),
        _ => None,
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
    events: &mut crate::console::Console<'_>,
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
        // #323: esto también es una espera que se come el bucle, y también
        // congelaba la pantalla — peor que la navegación, porque el lector no
        // la pidió: salta al acabar una task y en cada aviso del watcher, y
        // aquí se espera al listado COMPLETO, no a la primera página. Un
        // directorio remoto con muchas entradas dejaba la TUI muda un buen
        // rato sin que nadie hubiera tocado una tecla.
        let started = std::time::Instant::now();
        app.busy = Some(Busy::new(BusyKind::Listing, Some(dir.clone()), Some(i)));
        let esperado =
            crate::console::wait_painting(events, app, started, listing(backend, &dir, &attrs))
                .await;
        app.busy = None;
        match esperado {
            // El listado es COMPLETO: si venía de un cd paginado a medio
            // rellenar, ya no está cargando (el run loop suelta el drenador
            // tras este refresh). Un quick search vivo se re-aplica dentro
            // (índices nuevos).
            Waited::Done(Ok((entries, skipped))) => {
                app.panes[i].refresh_listing(entries);
                // #96: el refresh trae las omitidas FRESCAS — sin esto, el
                // badge conservaba el valor del listado anterior (rancio) tras
                // una mutación.
                app.panes[i].set_skipped(skipped);
                refreshed[i] = true;
            }
            // La conexión pide su contraseña (#325): un refresco SÍ es un
            // gesto del lector, así que aquí se PREGUNTA en vez de contestar
            // con la categoría del error.
            //
            // Es el camino que se recorre de verdad al reabrir: la
            // restauración de sesión deja el panel sobre la ruta remota sin
            // preguntar nada —restaurar no es pedir conectarse—, y el primer
            // Ctrl+R es lo que convierte eso en la pregunta. Sin esto, el
            // único camino que preguntaba era navegar a mano, o sea salir del
            // sitio donde estabas para poder volver.
            Waited::Done(Err(Error::SecretNeeded { conn, endpoint })) => {
                app.modal = Some(Modal::AskSecret {
                    conn,
                    endpoint,
                    input: crate::app::TypedSecret::default(),
                    dir: dir.clone(),
                    pane: i,
                    // `Record` y no un paso del rastro: un refresco no salió
                    // del historial, así que no hay nada que rebobinar si la
                    // pregunta se abandona. El panel se queda donde está.
                    trail: crate::app::Trail::Record,
                });
                return refreshed;
            }
            // Sin silencio: el dir pudo desaparecer (issue #20).
            Waited::Done(Err(e)) => {
                app.message = Some(ta("msg-refresh-error", &[("error", &error_category(&e))]));
            }
            // Un Esc a medias abandona el RESTO de panes, igual que antes.
            Waited::Cancelled => return refreshed,
            Waited::Quit => {
                app.quit = true;
                return refreshed;
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
    // Un refresco es el momento en que el espacio libre puede haber
    // cambiado sin que nadie navegue: se vuelve a pedir con él.
    app.volumes_stale = true;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::app_with_entries;

    fn tid(n: u64) -> norte_proto::TaskId {
        norte_proto::TaskId::new(n)
    }

    /// #290: la intención de `edit-new` la consume SU task y solo la suya.
    /// Cualquier otra que termine mientras tanto —una copia, un borrado— la
    /// deja intacta; abrirla con la primera que pase abriría otro fichero.
    #[test]
    fn solo_la_task_de_la_creacion_se_lleva_la_intencion() {
        let mut app = app_with_entries(&["a"]);
        let destino = norte_proto::VPath::parse("mem:///notas.txt").expect("wire");
        app.pending_edit_open = Some((tid(7), destino.clone()));

        assert_eq!(tomar_creacion(&mut app, tid(9)), None, "otra task, no");
        assert!(app.pending_edit_open.is_some(), "y la deja donde estaba");

        assert_eq!(tomar_creacion(&mut app, tid(7)), Some(destino));
        assert!(
            app.pending_edit_open.is_none(),
            "consumida: un segundo desenlace no reabre nada"
        );
        assert_eq!(tomar_creacion(&mut app, tid(7)), None);
    }
}
