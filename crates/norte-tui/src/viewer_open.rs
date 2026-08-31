//! Abrir el visor sobre un fichero, y mover el que ya está abierto.
//!
//! Vivía en el root del binario `ntc` —un crate DISTINTO de esta lib—, así que
//! ni los tests de integración ni el fetch de preview de fondo podían
//! alcanzarlo sin que el bucle de eventos hiciera de intermediario.

use norte_core::backend::Backend;
use norte_i18n::ta;
use norte_proto::{Error, VPath};

use crate::app::{App, error_category};
use crate::console::Waited;
use crate::viewer::Viewer;
use norte_frontend::busy::{Busy, BusyKind};

/// Aplica `f` al visor que tiene el teclado.
///
/// El acoplado (preview enfocado) o el de pantalla completa, en ese orden: es
/// el criterio que hace que las teclas `viewer.*` no necesiten un segundo
/// vocabulario para el preview (L3).
pub fn viewer_do(app: &mut App, f: impl FnOnce(&mut Viewer)) {
    // Al visor que tenga el teclado. Con el preview acoplado enfocado las
    // teclas `viewer.*` mueven ESE, sin bindings nuevos y sin un segundo
    // vocabulario: es el mismo visor en otro sitio (L3).
    if app.key_owner() == crate::app::KeyOwner::Preview {
        if let Some(id) = app.preview_slot()
            && let Some(v) = app.panes.preview_mut(id).and_then(|p| p.viewer_mut())
        {
            f(v);
        }
        return;
    }
    if let Some(v) = &mut app.viewer {
        f(v);
    }
}

/// Presupuesto de lectura del viewer: cabecera de 256 KiB (el resto del
/// archivo NO se lee — rango de ADR 0005; «cargar más» = deuda de M2).
/// OJO si esto crece (>~1 MiB): `Viewer::recompute` y `rows()` corren en
/// el hilo del loop — harían falta `spawn_blocking` + índice de líneas.
const VIEW_CAP: u64 = 256 * 1024;

/// Lee la cabecera de `path` y construye su [`Viewer`], con la cadena de
/// preview de plugin y todas sus degradaciones.
///
/// NO es cancelable: quien la llama pone el `select!` si tiene a alguien
/// esperando delante ([`open_viewer`] lo hace, para que `Esc` abandone). El
/// preview acoplado no puede hacerlo —nadie está esperando: el lector sigue
/// moviéndose por el listado— y por eso el read y su envoltorio modal son dos
/// cosas separadas desde L3.
///
/// El orden de las degradaciones es el contrato (ADR 0037): preview de plugin
/// CON ESTILO, luego preview plano, luego la vista cruda. Un `Ok(None)` —
/// ningún previewer aplica, un guest se cayó, o se violaron los topes del
/// wire— y un fallo de RED degradan IGUAL: un plugin roto nunca impide ver el
/// fichero.
/// # Errors
///
/// Lo que devuelva el `Backend` al leer la cabecera de `path`, sin traducir:
/// el llamante distingue un `PermissionDenied` de un `NotFound` para decir
/// cosas distintas. Un fallo del previewer de plugin NO es un error — degrada
/// a la vista cruda, que es el contrato de arriba.
pub async fn viewer_for(backend: &Backend, path: &VPath) -> Result<Viewer, Error> {
    let (bytes, truncated) = read_head(backend, path).await?;
    let viewer = match backend.plugin_preview_styled(path).await {
        Ok(Some(p)) => {
            Viewer::with_plugin_preview_styled(path.clone(), p.plugin_name, &p.lines, p.lossy)
        }
        Ok(None) | Err(_) => match backend.plugin_preview(path).await {
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
    Ok(viewer)
}

/// Abre el viewer a pantalla completa leyendo la CABECERA vía el core (regla
/// 7), cancelable como el cd (Esc abandona, Ctrl-C sale).
pub async fn open_viewer(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
    path: VPath,
) {
    // #323: traer la cabecera de un fichero REMOTO es otra espera que se come
    // el bucle. El panel no cambia mientras dura, así que sin indicador F3
    // sobre un fichero de un bucket se veía exactamente igual que una tecla
    // que no hizo nada.
    let started = std::time::Instant::now();
    app.busy = Some(Busy::new(
        BusyKind::Opening,
        Some(path.clone()),
        Some(app.focus()),
    ));
    let esperado =
        crate::console::wait_painting(events, app, started, viewer_for(backend, &path)).await;
    app.busy = None;
    match esperado {
        Waited::Done(Ok(viewer)) => app.viewer = Some(viewer),
        Waited::Done(Err(e)) => {
            app.message = Some(ta("msg-view-error", &[("error", &error_category(&e))]));
        }
        Waited::Cancelled => {}
        Waited::Quit => app.quit = true,
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
