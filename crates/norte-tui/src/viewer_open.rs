//! Abrir el visor sobre un fichero, y mover el que ya está abierto.
//!
//! Vivía en el root del binario `ntc` —un crate DISTINTO de esta lib—, así que
//! ni los tests de integración ni el fetch de preview de fondo podían
//! alcanzarlo sin que el bucle de eventos hiciera de intermediario.

use norte_core::backend::Backend;
use norte_i18n::{t, ta};
use norte_proto::{Error, VPath};

use crate::app::{App, error_category};
use crate::console::Waited;
use crate::viewer::Viewer;
use norte_frontend::busy::{Busy, BusyKind};

/// Cómo se va a enseñar esta imagen, ya resueltos la clave y el terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modo {
    /// Píxeles por el protocolo del terminal.
    Kitty,
    /// Medios bloques, que los pone un previewer aprobado.
    Bloques,
    /// Nada: el visor se queda con los bytes.
    Nada,
}

/// Resuelve `[ui] images` contra lo que contestó la sonda.
///
/// `Bloques` NO es una rama que haga nada: es «no hagas nada especial», y
/// el previewer de imagen —si está aprobado y activado— ya pinta. Por eso
/// `Bloques` y `Nada` se parecen tanto aquí y se distinguen en la ayuda:
/// con `off` el lector pidió hexview; con `blocks` pidió medios bloques y
/// lo que falta es aprobar el plugin.
#[must_use]
pub fn modo_efectivo(cfg: norte_config::Images, soporta: bool) -> Modo {
    match cfg {
        norte_config::Images::Off => Modo::Nada,
        norte_config::Images::Kitty => Modo::Kitty,
        norte_config::Images::Auto if soporta => Modo::Kitty,
        // `Blocks` y el respaldo de `Auto` sin soporte son la MISMA rama
        // (clippy `match_same_arms`): las dos quieren «no pintes píxeles,
        // deja hacer al previewer» — la distinción vive en la ayuda, no en
        // el código.
        norte_config::Images::Blocks | norte_config::Images::Auto => Modo::Bloques,
    }
}

/// El aviso de la barra del visor cuando el hueco no tiene quién pinte la
/// imagen.
///
/// El piloto de verdad encontró este agujero: sin previewer aprobado, un PNG
/// en `Modo::Bloques` cae a hexview exactamente igual que un fichero que
/// nadie sabe interpretar, y nada en pantalla distingue los dos casos. Un
/// hexview silencioso es indistinguible de «norte no sabe hacerlo».
///
/// `no_hace_falta_avisar` es `true` cuando este hueco NO necesita el aviso.
/// El llamante decide QUÉ significa eso según `modo` — pasando
/// [`no_hace_falta_avisar_de_imagen`] en [`Modo::Bloques`] o
/// [`no_hace_falta_avisar_de_miniatura`] en [`Modo::Kitty`] — en vez de
/// repetir la expresión inline en el sitio de llamada (ronda de arreglo 1,
/// fase 5: un `hay_previewer` calculado ahí, con ese nombre, invitaba a
/// «simplificarlo» a `viewer.preview_plugin().is_some()`, que pierde el
/// primer motivo y avisaría para cualquier fichero no-imagen).
///
/// Task 5b (hallazgo de revisión de T6): el docstring original de esta
/// función decía que en [`Modo::Kitty`] «el terminal ya pinta píxeles por su
/// cuenta… no hay nada que aprobar». Es FALSO — los bytes que coloca Kitty
/// los da un plugin `thumbnail` (`plugins/image-thumb`), tan opcional y
/// aprobable como el `previewer` de [`Modo::Bloques`]; sin uno aprobado el
/// lector se queda en hexview igual de silenciosamente que en la otra rama,
/// que es exactamente el agujero que esta función existe para tapar. Los dos
/// modos avisan ahora, con textos DISTINTOS: piden aprobar EXTENSIONES
/// distintas, y mandar al lector a aprobar la equivocada es peor que no
/// avisar. En [`Modo::Nada`] el lector pidió hexview él mismo
/// (`images = "off"`): ahí no hay nada que aprobar y no se avisa.
#[must_use]
pub fn aviso_de_imagen(modo: Modo, no_hace_falta_avisar: bool) -> Option<String> {
    if no_hace_falta_avisar {
        return None;
    }
    match modo {
        Modo::Bloques => Some(t("viewer-image-needs-previewer")),
        Modo::Kitty => Some(t("viewer-image-needs-thumbnail")),
        Modo::Nada => None,
    }
}

/// Si `viewer` NO necesita el aviso de [`aviso_de_imagen`] en [`Modo::Bloques`]
/// — el segundo parámetro que ese sitio de llamada le pasa cuando el modo es
/// ese.
///
/// Es `!viewer.is_image()`, y [`Viewer::is_image`] ya hace el AND de las dos
/// condiciones que hacen falta: `plugin_preview.is_none() && image.is_some()`
/// — sólo `true` cuando NINGÚN previewer sustituyó la vista Y los bytes son
/// una imagen reconocida. Negarlo da «no es imagen, o SÍ lo es pero un
/// previewer ya pintó»: las dos razones para no avisar, juntas.
///
/// T3 (esta misma fase) ya avisó de que la TUI no debe usar `is_image()`
/// para decidir «es imagen» (deja de pintar píxeles en cuanto un previewer
/// sustituye la vista); aquí es al revés — se usa a propósito, PARA saber si
/// algo ya sustituyó la vista — pero la trampa hermana existe: no lo
/// "corrijas" a `viewer.preview_plugin().is_some()` pensando que es más
/// honesto. Eso pierde la mitad no-imagen y avisaría de un previewer de
/// IMAGEN que falta para cualquier fichero que no sea una imagen en
/// `Modo::Bloques` — justo la regresión que centralizar este cálculo aquí,
/// con este nombre, existe para prevenir.
///
/// Ver [`no_hace_falta_avisar_de_miniatura`] para la contraparte de
/// [`Modo::Kitty`], que pide un plugin `thumbnail`, no un `previewer`.
#[must_use]
pub fn no_hace_falta_avisar_de_imagen(viewer: &Viewer) -> bool {
    !viewer.is_image()
}

/// Si `viewer` NO necesita el aviso de [`aviso_de_imagen`] en [`Modo::Kitty`]
/// — la contraparte de [`no_hace_falta_avisar_de_imagen`] para el plugin
/// `thumbnail` en vez del `previewer`.
///
/// `true` cuando CUALQUIERA de dos cosas distintas ya hace innecesario el
/// aviso: `!viewer.is_image()` — el fichero no es una imagen, o SÍ lo es
/// pero un previewer de plugin ya sustituyó la vista y medios bloques ya se
/// están pintando («si la imagen se está viendo… no hay nada que avisar»,
/// igual que en [`Modo::Bloques`]) — O `imagen` trae una miniatura ya
/// COLOCADA para ESTE fichero. Comparar el `path` de `imagen` contra el de
/// `viewer` importa: el lector puede seguir viendo el hexview de un fichero
/// mientras la miniatura de OTRO (el que veía antes) sigue viva en
/// [`App::viewer_imagen`] a la espera de que el run loop la borre — esa
/// miniatura vieja no dice nada sobre si ÉSTE fichero tiene la suya.
#[must_use]
pub fn no_hace_falta_avisar_de_miniatura(viewer: &Viewer, imagen: Option<&ImagenColocada>) -> bool {
    !viewer.is_image() || imagen.is_some_and(|imagen| imagen.path == viewer.path)
}

/// Una miniatura ya pedida y lista para colocar (T4 la coloca/borra).
///
/// Vive en [`App`], no en [`Viewer`]: `Viewer` es de `norte-frontend` y lo
/// comparten los dos frontends, y la ventana ya tiene su propio camino a
/// las miniaturas — meter un campo de la TUI ahí ensuciaría una superficie
/// compartida.
#[derive(Debug, Clone)]
pub struct ImagenColocada {
    /// El fichero del que es esta miniatura — para saber si sigue siendo
    /// la que el visor enseña cuando el lector ya se movió a otro.
    pub path: VPath,
    /// Los bytes codificados (PNG/JPEG/WebP) que devolvió el plugin.
    pub bytes: Vec<u8>,
    /// Ancho en píxeles, el que dice la cabecera del raster.
    pub width: u32,
    /// Alto en píxeles, el que dice la cabecera del raster.
    pub height: u32,
    /// El id con el que se coloca y se borra por el protocolo de kitty.
    pub id: u32,
    /// Dónde se colocó la última vez (T4 la pinta y la rellena); `None`
    /// hasta el primer frame que la coloca.
    pub puesta_en: Option<ratatui::layout::Rect>,
}

/// El siguiente id de imagen que no se ha usado nunca en este proceso.
///
/// Propio de este módulo — no del contador de `SlotId` de [`App`], que es
/// privado a su propio módulo y no alcanza desde aquí — y nunca se
/// reutiliza por el mismo motivo que aquél: un id reciclado podría borrar o
/// reemplazar la imagen de otra colocación en vuelo.
fn mint_image_id() -> u32 {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl App {
    /// Cierra el visor a pantalla completa Y su miniatura A LA VEZ.
    ///
    /// La invariante es que no puede haber [`App::viewer_imagen`] sin el
    /// [`App::viewer`] al que corresponde — si no, T4 coloca o borra por un
    /// id que ya no tiene visor detrás. Un `app.viewer = None` suelto en el
    /// sitio que cierra el visor es exactamente el hallazgo de revisión que
    /// esto arregla: se quedaba la miniatura vieja colgando. Un único punto
    /// de cierre hace la invariante imposible de romper por accidente en un
    /// sitio nuevo, en vez de tener que acordarse de los dos campos cada vez.
    pub fn close_viewer(&mut self) {
        self.viewer = None;
        self.viewer_imagen = None;
    }
}

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
pub async fn viewer_for(
    backend: &Backend,
    path: &VPath,
    modo: Modo,
) -> Result<(Viewer, Option<ImagenColocada>), Error> {
    // El ancho del terminal es el del visor a pantalla completa, y es lo que
    // un previewer de imagen usa para encoger (proto 0.66.0). Sin terminal
    // —tests, un pipe— no hay pista y el guest elige su ancho.
    let columns = crossterm::terminal::size()
        .ok()
        .map(|(cols, _)| u32::from(cols));
    viewer_for_width(backend, path, columns, modo).await
}

/// [`viewer_for`] con el ancho dicho por el llamante (el visor acoplado de un
/// hueco es más estrecho que la pantalla).
///
/// `modo` decide si además se pide la miniatura ([`ImagenColocada`]): sólo
/// cuando los BYTES dicen que es una imagen ([`norte_frontend::viewer::image_format`])
/// Y el modo es [`Modo::Kitty`]. Con [`Modo::Bloques`] o [`Modo::Nada`] no se
/// pide nada aquí — `Bloques` lo pinta el previewer de plugin por su camino
/// normal (`plugin_preview_styled` abajo), no éste.
///
/// A propósito NO se usa `viewer.is_image()`: ese getter es `false` en
/// cuanto un previewer de plugin (estilizado o plano) sustituye la vista
/// cruda, así que decidir por él dejaría la miniatura sin pedirse nunca en
/// cuanto hubiera un previewer de imagen aprobado — que es precisamente el
/// caso que esta clave existe para resolver: el protocolo del terminal GANA
/// al previewer, no al revés (hallazgo de revisión: T3 fase 5).
///
/// # Errors
///
/// Los mismos que [`viewer_for`]: lo que devuelva el `Backend` al leer la
/// cabecera; un previewer roto degrada, no falla. Un fallo pidiendo la
/// miniatura TAMPOCO es un error: `None` y el visor se ve igual, sin
/// píxeles (ADR 0037).
pub async fn viewer_for_width(
    backend: &Backend,
    path: &VPath,
    columns: Option<u32>,
    modo: Modo,
) -> Result<(Viewer, Option<ImagenColocada>), Error> {
    let (bytes, truncated) = read_head(backend, path).await?;
    // Por BYTES, antes de que la cadena de preview de plugin —que puede
    // sustituir la vista cruda entera— tenga oportunidad de esconder el
    // formato. Ver el rustdoc de arriba.
    let es_imagen = norte_frontend::viewer::image_format(&bytes).is_some();
    let viewer = match backend.plugin_preview_styled(path, columns).await {
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
    let imagen = if es_imagen && modo == Modo::Kitty {
        // El lado mayor en PÍXELES que cabe en el hueco. Una celda de
        // terminal es aproximadamente 8x16 px y no hay forma portable de
        // preguntarlo, así que se estima: pasarse sólo cuesta que el
        // terminal la encoja, quedarse corto se ve borroso.
        let max_edge = columns.unwrap_or(80).saturating_mul(8).clamp(64, 1920);
        backend
            .plugin_thumbnail(path, max_edge)
            .await
            .ok()
            .flatten()
            .map(|thumb| ImagenColocada {
                path: path.clone(),
                bytes: thumb.bytes,
                width: thumb.width,
                height: thumb.height,
                id: mint_image_id(),
                puesta_en: None,
            })
    } else {
        None
    };
    Ok((viewer, imagen))
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
    let modo = modo_efectivo(app.chrome.images(), crate::kitty_graphics::soportado());
    let esperado =
        crate::console::wait_painting(events, app, started, viewer_for(backend, &path, modo)).await;
    app.busy = None;
    match esperado {
        Waited::Done(Ok((viewer, imagen))) => {
            app.viewer = Some(viewer);
            app.viewer_imagen = imagen;
        }
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
