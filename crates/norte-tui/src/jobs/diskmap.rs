//! La medida del mapa de disco: lanzarla y cosechar su informe (fase 4).
//!
//! Mismo reparto que el lote de sumas (#311) y por el mismo motivo (regla 3):
//! medir un `$HOME` tarda minutos, así que la espera va SPAWNEADA y se cosecha
//! en el bucle. Esperarla aquí dejaría la TUI sin dibujar, sin teclas y sin
//! poder cancelar — que es justo cuando alguien cancela.

use norte_core::backend::Backend;
use norte_i18n::t;
use norte_proto::Error;

use crate::app::App;
use crate::jobs::{DiskMapRun, InFlight};

/// Lanza `fs.dir_usage` sobre el directorio del listado enfocado.
///
/// El mapa describe LO QUE SE ESTÁ MIRANDO: se apunta al listado activo, y si
/// ya apuntaba a otro sitio se olvida lo medido antes de pedir nada — el mapa
/// del directorio anterior bajo el título del nuevo es la respuesta equivocada
/// durante justo el rato que dura la medida.
pub async fn lanzar(app: &mut App, backend: &Backend, work: &mut InFlight) {
    let Some(slot) = app.disk_map_slot() else {
        return; // el panel no está abierto: nada que medir
    };
    let dir = app.focused().dir().clone();
    if let Some(m) = app.panes.disk_map_mut(slot)
        && m.dir() != Some(&dir)
    {
        m.apuntar(dir.clone());
    }
    let params = norte_proto::methods::FsDirUsageParams {
        path: dir.clone(),
        // Un nivel: es lo que pinta un mapa, y es lo único que el servidor
        // sirve hoy. Pedir más se RECHAZA, no se recorta (ADR 0117).
        depth: 1,
    };
    match backend.dir_usage(params).await {
        Ok(task) => {
            app.message = Some(t("msg-disk-map-started"));
            app.board.push(&task, None);
            let id = task.id();
            let observador = task.observer();
            let mut prog = task.progress();
            if let Some(m) = app.panes.disk_map_mut(slot) {
                m.midiendo(id);
            }
            let b = backend.clone();
            let handle = tokio::spawn(async move {
                // El informe solo es DEFINITIVO cuando la Task es terminal.
                // Pedirlo antes daría medio mapa sin decir que lo es, y medio
                // mapa se lee como un directorio pequeño.
                while !prog.borrow().state.is_terminal() {
                    if prog.changed().await.is_err() {
                        break;
                    }
                }
                // El estado viaja CON el informe, como en las sumas: cancelada
                // o fallida significa que lo que hay está a medias, y un
                // `changed()` que muere sin llegar a terminal —el daemon se
                // cayó— no es ninguna de las dos.
                let estado = prog.borrow().state.clone();
                if !estado.is_terminal() {
                    return (estado, Err(Error::ProviderUnavailable { retryable: true }));
                }
                (estado, b.dir_usage_report(id).await)
            });
            if let Some(old) = work.disk_map.replace(DiskMapRun {
                handle,
                task: observador,
                slot,
                dir,
            }) {
                // Cancelar la TASK, no solo la espera: abortar el `JoinHandle`
                // dejaba al core recorriendo un árbol entero sin nadie que
                // recogiera el resultado. Navegar deprisa dejaba tres.
                old.task.cancel();
                old.handle.abort();
            }
        }
        Err(e) => app.message = Some(crate::app::error_message(&e)),
    }
}

/// Aterriza el informe de una medida ya terminada.
///
/// **Se DESCARTA lo que llegue tarde.** Medir tarda, y en ese rato el panel
/// puede estar apuntando a otro directorio: un informe aterrizado sin
/// comprobarlo pintaría los tamaños de un sitio bajo el título de otro.
pub fn harvest(
    app: &mut App,
    work: &mut InFlight,
    res: Result<
        (
            norte_proto::TaskState,
            Result<norte_proto::methods::FsDirUsageReportResult, Error>,
        ),
        tokio::task::JoinError,
    >,
) {
    let Some(run) = work.disk_map.take() else {
        return;
    };
    let (estado, informe) = match res {
        Ok(par) => par,
        // Un relevo aborta el handle viejo y el brazo del `select!` ya solo
        // poll-ea el nuevo, así que eso NO llega aquí. Lo que sí llega es un
        // pánico del future, y ahí nadie ha puesto mensaje.
        Err(join) => {
            if join.is_panic() {
                app.message = Some(t("msg-disk-map-partial"));
            }
            return;
        }
    };
    let informe = match informe {
        Ok(r) => r,
        Err(e) => {
            if let Some(m) = app.panes.disk_map_mut(run.slot) {
                m.fallo(crate::app::error_message(&e));
            }
            app.message = Some(crate::app::error_message(&e));
            return;
        }
    };
    // ¿Sigue el panel donde estaba? Si no, esto es de otro directorio.
    let Some(mapa) = app.panes.disk_map_mut(run.slot) else {
        return;
    };
    if mapa.dir() != Some(&run.dir) {
        return;
    }
    let completa = estado == norte_proto::TaskState::Completed;
    mapa.aterrizar(informe, completa);
    if !completa {
        // Lo que hay es un fragmento correcto para mirar AHORA, con su aviso
        // delante. No se guarda en la caché: `DiskMap::aterrizar` solo declara
        // hecha la medida completa, y la caché solo acepta lo terminado.
        app.message = Some(t("msg-disk-map-partial"));
    }
}
