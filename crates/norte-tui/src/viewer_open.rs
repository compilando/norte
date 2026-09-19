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
pub fn aviso_de_imagen(
    modo: Modo,
    no_hace_falta_avisar: bool,
    formato_ajeno: bool,
) -> Option<String> {
    if no_hace_falta_avisar {
        return None;
    }
    match modo {
        Modo::Bloques => Some(t("viewer-image-needs-previewer")),
        // Los dos motivos por los que en Kitty no hay píxeles piden cosas
        // DISTINTAS del lector, y sólo uno se arregla desde F12. Mandar a
        // aprobar lo que ya está aprobado es peor que no decir nada: el
        // lector va, lo encuentra todo en orden, y se queda sin pista.
        Modo::Kitty if formato_ajeno => Some(t("viewer-image-thumbnail-format")),
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
    /// Los bytes codificados que devolvió el plugin — SIEMPRE
    /// `"image/png"` (ver [`imagen_desde_miniatura`]): es el único formato
    /// que kitty sabe colocar con `f=100`, así que nada que llegue hasta
    /// aquí es otra cosa.
    pub bytes: Vec<u8>,
    /// El mimetype que dijo el plugin — se guarda para que la invariante de
    /// arriba (siempre PNG) sea COMPROBABLE, no sólo documentada.
    pub mimetype: String,
    /// Ancho en píxeles, el que dice la cabecera del raster.
    pub width: u32,
    /// Alto en píxeles, el que dice la cabecera del raster.
    pub height: u32,
    /// El id con el que se coloca y se borra por el protocolo de kitty.
    pub id: u32,
    /// Dónde se colocó la última vez (T4 la pinta y la rellena); `None`
    /// hasta el primer frame que la coloca.
    ///
    /// Es la COLOCACIÓN entera y no sólo el rect (spec 2026-09-20): con
    /// zoom, dos frames pueden ocupar las mismas celdas y enseñar trozos
    /// distintos de la imagen, y comparar sólo el rect dejaría la pantalla
    /// quieta mientras el lector se mueve por dentro.
    pub puesta_en: Option<Colocacion>,
}

/// Dónde va la imagen y qué parte de ella se ve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Colocacion {
    /// Las celdas que ocupa.
    pub rect: ratatui::layout::Rect,
    /// El trozo del raster que se enseña, en píxeles. `None` = entero.
    pub recorte: Option<Recorte>,
}

/// Un trozo del raster, en píxeles de la propia imagen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recorte {
    /// Desplazamiento desde la izquierda.
    pub x: u32,
    /// Desplazamiento desde arriba.
    pub y: u32,
    /// Ancho del trozo.
    pub w: u32,
    /// Alto del trozo.
    pub h: u32,
}

/// Dónde colocar una imagen con el zoom que tiene el visor.
///
/// Tres regímenes, y la razón de que sean tres es que un terminal no puede
/// pintar fuera del hueco del visor:
///
/// - **Ajustado** (100 %): la imagen se estira al hueco entero, que es lo
///   que hacía siempre.
/// - **Alejado** (< 100 %): el hueco que se le da ENCOGE, y la imagen entera
///   sigue dentro. No se recorta nada.
/// - **Acercado** (> 100 %): el hueco es el mismo y lo que encoge es el
///   TROZO del raster que se enseña. Eso es magnificar, y es lo que deja que
///   las teclas de mover el visor sirvan para pasearse por dentro.
///
/// `pan_x`/`pan_y` son el desplazamiento pedido, en celdas; se traducen a
/// píxeles del raster y se acotan para que el trozo no se salga.
#[must_use]
pub fn colocacion(
    zoom_pct: u16,
    hueco: ratatui::layout::Rect,
    ancho: u32,
    alto: u32,
    pan_x: usize,
    pan_y: usize,
) -> Colocacion {
    use ratatui::layout::Rect;
    if hueco.is_empty() || ancho == 0 || alto == 0 {
        return Colocacion {
            rect: hueco,
            recorte: None,
        };
    }
    if zoom_pct < 100 {
        // Encoge el hueco. Nunca a cero: `c=0,r=0` significa para kitty
        // «tamaño natural de la imagen», que sobre la pantalla entera es
        // exactamente lo que el guardia de `imagen_a_colocar` evita.
        let escala = |v: u16| {
            u16::try_from(u32::from(v) * u32::from(zoom_pct) / 100)
                .unwrap_or(u16::MAX)
                .max(1)
        };
        return Colocacion {
            rect: Rect {
                width: escala(hueco.width),
                height: escala(hueco.height),
                ..hueco
            },
            recorte: None,
        };
    }
    if zoom_pct == 100 {
        return Colocacion {
            rect: hueco,
            recorte: None,
        };
    }
    // Acercar: el trozo visible es el inverso del zoom, y al menos un píxel
    // — un trozo de cero no es una imagen pequeña, es ninguna.
    let pct = u32::from(zoom_pct);
    let w = (ancho * 100 / pct).max(1).min(ancho);
    let h = (alto * 100 / pct).max(1).min(alto);
    // El paseo se pide en CELDAS y aquí se gasta en píxeles: una celda de
    // movimiento mueve la misma fracción de imagen que ocupa una celda de
    // hueco, que es lo que hace que moverse se sienta igual con cualquier
    // zoom.
    let paso_x = w / u32::from(hueco.width).max(1);
    let paso_y = h / u32::from(hueco.height).max(1);
    let x = u32::try_from(pan_x)
        .unwrap_or(u32::MAX)
        .saturating_mul(paso_x)
        .min(ancho - w);
    let y = u32::try_from(pan_y)
        .unwrap_or(u32::MAX)
        .saturating_mul(paso_y)
        .min(alto - h);
    Colocacion {
        rect: hueco,
        recorte: Some(Recorte { x, y, w, h }),
    }
}

/// Lo que salió de pedir la miniatura de un fichero, con el MOTIVO cuando
/// no hay ninguna que colocar.
///
/// Un `Option<ImagenColocada>` decía «no hay» y nada más, y los dos «no
/// hay» piden cosas distintas del lector: sin plugin `thumbnail` aprobado
/// hay que ir a F12 y aprobarlo; con uno aprobado que contestó en JPEG no
/// hay nada que aprobar, y ese mismo aviso manda a una pantalla donde todo
/// se ve correcto. Un visor que pide lo imposible es peor que uno callado.
#[derive(Debug, Clone, Default)]
pub enum Miniatura {
    /// No hubo ninguna: ningún plugin `thumbnail` aprobado y encendido, la
    /// llamada falló, o el modo no pedía miniatura.
    #[default]
    Ninguna,
    /// Un plugin contestó, pero en un formato que kitty no sabe colocar
    /// —sólo PNG— así que se descartó ([`imagen_desde_miniatura`]).
    FormatoAjeno,
    /// Lista para colocar.
    Colocable(ImagenColocada),
}

impl Miniatura {
    /// La imagen, si la hay; descarta el motivo.
    #[must_use]
    pub fn colocable(self) -> Option<ImagenColocada> {
        match self {
            Self::Colocable(imagen) => Some(imagen),
            Self::Ninguna | Self::FormatoAjeno => None,
        }
    }
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

/// Convierte lo que devolvió `plugin.thumbnail` en una [`ImagenColocada`], o
/// dice POR QUÉ no hay ninguna que colocar ([`Miniatura`]).
///
/// Revisión de rama, hallazgo 1: `escape_colocar` manda `f=100` FIJO —el
/// protocolo de kitty no tiene una clave `f=` para JPEG ni para WebP, sólo
/// PNG (100) o raster crudo (24/32)— pero
/// [`norte_proto::methods::PluginThumbnail::mimetype`] admite las tres
/// (`thumb::reencode` en `norte-plugin-host` escribe PNG y cae a JPEG
/// calidad 85 cuando el PNG no cabe en 4 MiB, algo fácil con el `max_edge`
/// de hasta 1920 px que pide este visor). Sin este filtro, un JPEG viaja con
/// una cabecera que dice PNG: kitty lo rechaza, `q=2` calla el error,
/// [`crate::kitty_graphics::marcar_colocada`] ya anotó el id así que nadie
/// reintenta, y [`no_hace_falta_avisar_de_miniatura`] ve una
/// [`ImagenColocada`] para este fichero y calla el aviso — un visor vacío
/// sin ningún rastro de por qué.
///
/// Descartarlo devuelve el aviso, pero el aviso de «falta aprobar una
/// extensión» es FALSO en este caso concreto: la extensión está aprobada y
/// encendida, contestó, y lo que no sirve es su formato. Mandar al lector a
/// F12 a aprobar lo que ya está aprobado es un callejón sin salida. Por eso
/// esto devuelve [`Miniatura::FormatoAjeno`] y no un `None` sin motivo: el
/// aviso que sale entonces es otro y dice lo que pasa.
#[must_use]
pub fn imagen_desde_miniatura(
    path: &VPath,
    thumb: norte_proto::methods::PluginThumbnail,
) -> Miniatura {
    if thumb.mimetype != "image/png" {
        return Miniatura::FormatoAjeno;
    }
    Miniatura::Colocable(ImagenColocada {
        path: path.clone(),
        bytes: thumb.bytes,
        mimetype: thumb.mimetype,
        width: thumb.width,
        height: thumb.height,
        id: mint_image_id(),
        puesta_en: None,
    })
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
        // Y el motivo por el que no había imagen: sin visor no hay a quién
        // avisar, y dejarlo puesto haría que el PRÓXIMO visor del mismo
        // fichero heredase un aviso que nadie ha vuelto a comprobar.
        self.viewer_miniatura_ajena = None;
        // `viewer_modo` sin visor no significa nada — se deja en `Nada`
        // como `App::new`, para que un `viewer_modo` viejo (Hallazgo 3) no
        // sobreviva a este visor y confunda al que abra el siguiente antes
        // de que `open_viewer` lo fije de nuevo.
        self.viewer_modo = Modo::Nada;
    }

    /// Suelta la miniatura colocada cuando el modo EFECTIVO (recién resuelto
    /// contra la config recargada) dejó de ser [`Modo::Kitty`] — llamado
    /// SÓLO desde [`crate::config_reload::reload_config`], después de
    /// reasignar `App::chrome`.
    ///
    /// Revisión de rama, hallazgo 3: sin esto, un `Kitty` que pasa a `off` o
    /// `blocks` en caliente deja los píxeles ya colocados en pantalla PARA
    /// SIEMPRE — nada vuelve a mirarlos una vez que
    /// [`App::viewer_modo`] quedó pineado al abrir, y la ayuda promete que
    /// `off` «deja el visor en hexview sin más», una promesa que sólo se
    /// cumple si algo suelta la miniatura vieja.
    ///
    /// A propósito NO hace nada en la dirección contraria
    /// (`blocks`/`off` → `kitty`, o cualquier cambio mientras ya está en
    /// `Bloques`/`Nada`): actualizar el modo pineado ahí resucitaría el otro
    /// agujero de la misma revisión — el aviso «falta aprobar la extensión
    /// de miniaturas» saldría para un fichero al que el modo nuevo JAMÁS le
    /// pidió una. Devuelve si soltó algo, sólo para que quien llama pueda
    /// registrarlo si quiere; hoy nadie lo usa.
    pub fn soltar_miniatura_si_deja_de_ser_kitty(&mut self, modo_efectivo: Modo) -> bool {
        if self.viewer.is_none() || self.viewer_modo != Modo::Kitty || modo_efectivo == Modo::Kitty
        {
            return false;
        }
        self.viewer_imagen = None;
        self.viewer_modo = modo_efectivo;
        true
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
) -> Result<(Viewer, Miniatura), Error> {
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
) -> Result<(Viewer, Miniatura), Error> {
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
    let miniatura = if es_imagen && modo == Modo::Kitty {
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
            .map_or(Miniatura::Ninguna, |thumb| {
                imagen_desde_miniatura(path, thumb)
            })
    } else {
        Miniatura::Ninguna
    };
    Ok((viewer, miniatura))
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
        Waited::Done(Ok((viewer, miniatura))) => {
            // El formato ajeno se anota ANTES de consumir la miniatura, y
            // contra ESTE path: es lo que distingue «no hay extensión de
            // miniaturas» de «la hay, contestó, y su formato no sirve».
            app.viewer_miniatura_ajena =
                matches!(miniatura, Miniatura::FormatoAjeno).then(|| path.clone());
            app.viewer = Some(viewer);
            app.viewer_imagen = miniatura.colocable();
            // Hallazgo 3: el modo con que se PIDIÓ la miniatura, fijado
            // aquí y no recalculado después — ver el rustdoc de
            // `App::viewer_modo`.
            app.viewer_modo = modo;
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
