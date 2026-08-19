//! Lo que se pide EN SEGUNDO PLANO para rellenar lo que el listado dejó a
//! medias.
//!
//! Cuatro sondas con la misma forma —lanzar, un `Receiver`, y la supersesión
//! como cancelación: soltar el receptor descarta la respuesta— y todas nacieron
//! en el root del binario `ntc`, que es un crate DISTINTO de esta lib. Son la
//! base de la que cuelgan las tareas largas de panel (`crate::jobs`, cuando salga) y el ritual de aterrizaje de un `cd`, así que salen antes que ellos.
//!
//! [`Probed`] es el dedup: un stat que falló no se reintenta hasta que el
//! listado se renueve, para no martillear un provider roto.

use norte_core::backend::Backend;
use norte_frontend::layout::SlotId;
use norte_proto::{Entry, Error, VPath};

use crate::viewer::Viewer;
use crate::viewer_open::viewer_for;

/// Sonda de stat del VIEWPORT (#52): hidrata size/mtime de las entradas
/// VISIBLES que el listado lazy dejó en None — no solo la enfocada, o las
/// columnas Tamaño/Fecha quedan en blanco en todas las demás filas. A lo
/// sumo UNA tanda en vuelo, acotada a [`STAT_BATCH_MAX`] paths y resuelta
/// con concurrencia [`STAT_BATCH_CONCURRENCY`] (una sesión remota no puede
/// pagar N RTT en serie). Dedup por `(pane, path)` en el conjunto `probed`
/// del run loop: un stat fallido no se reintenta hasta que el listado se
/// renueve (sin martillear un provider roto). Cada stat va acotado con
/// timeout (`STAT_PROBE_TIMEOUT`): un provider colgado no bloquea la tanda
/// para siempre.
pub struct StatProbe {
    /// Los stats resueltos: `(pane, path, entry)`. Soltar el receptor descarta
    /// la tanda, que es la cancelación de esta sonda.
    pub rx: tokio::sync::oneshot::Receiver<Vec<(usize, VPath, Entry)>>,
}

/// Dedup de la sonda #52: `(pane, path)` ya pedidos. Se vacía con cada
/// listado nuevo (cd/refresh) — las entries vuelven a nacer lazy.
pub type Probed = std::collections::HashSet<(usize, VPath)>;

/// Radio en filas de la ventana que la sonda #52 hidrata alrededor del
/// cursor de cada pane (aproximación del viewport: el alto real lo decide
/// el widget al pintar). Cubre un terminal alto con margen.
pub const STAT_WINDOW_RADIUS: usize = 64;

/// Tope de paths por tanda de la sonda #52: lo que no entre se pide en la
/// siguiente vuelta, ya sin los que la tanda anterior hidrató.
pub const STAT_BATCH_MAX: usize = 64;

/// Stats simultáneos dentro de una tanda (#52): acota las peticiones en
/// vuelo contra el daemon sin serializar la latencia de la pantalla entera.
pub const STAT_BATCH_CONCURRENCY: usize = 8;

/// Tope del stat de la sonda on-focus (#52, MINOR-1): un provider remoto
/// colgado no debe dejar la sonda en vuelo indefinidamente — vencido el
/// plazo se trata como fallo (entrada se queda en `None`, no se reintenta
/// hasta cambiar la selección).
pub const STAT_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Lanza la tanda de `StatProbe`: clona el `Backend` (barato, Arc interno) y
/// los paths para que la task no retenga el préstamo del run loop. Los
/// fallos (error del provider o timeout) simplemente no vuelven — la entrada
/// se queda lazy y la dedup del run loop evita el reintento en bucle.
pub fn spawn_stat_probe(backend: &Backend, paths: Vec<(usize, VPath)>) -> StatProbe {
    use futures::StreamExt as _;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let b = backend.clone();
    tokio::spawn(async move {
        let hidratadas: Vec<(usize, VPath, Entry)> = futures::stream::iter(paths)
            .map(|(pane, path)| {
                let b = b.clone();
                async move {
                    let entry = tokio::time::timeout(STAT_PROBE_TIMEOUT, b.stat(&path))
                        .await
                        .ok()
                        .and_then(Result::ok)?;
                    Some((pane, path, entry))
                }
            })
            .buffer_unordered(STAT_BATCH_CONCURRENCY)
            .filter_map(|r| async move { r })
            .collect()
            .await;
        let _ = tx.send(hidratadas);
    });
    StatProbe { rx }
}

/// Sonda de stat de la fila seleccionada del panel de diferencias (#157).
/// Molde de [`StatProbe`], reducido a lo que ese caso necesita: como mucho
/// dos paths (los dos lados de una fila), así que no hace falta
/// `STAT_BATCH_CONCURRENCY` ni un tope de tanda — la propia selección ya
/// acota cuántos hay que pedir.
pub struct CompareStatProbe {
    /// Los stats resueltos: `None` = ese lado no tiene el fichero.
    pub rx: tokio::sync::oneshot::Receiver<Vec<(VPath, Option<Entry>)>>,
    /// La comparación bajo la que se pidió (#198): el resultado solo vale
    /// para ella.
    pub generation: u64,
}

/// Lanza la sonda #157: un `stat` por path, con el mismo timeout que la del
/// pane normal para no dejarla en vuelo para siempre contra un provider
/// colgado. Un fallo (error o timeout) viaja como `(path, None)` en vez de
/// perderse — a diferencia de [`spawn_stat_probe`], aquí SÍ hace falta saber
/// qué se pidió y no llegó: es lo que `App::hydrate_compare_size` usa para
/// marcarlo sondeado y no reintentarlo cada frame.
pub fn spawn_compare_stat_probe(
    backend: &Backend,
    paths: Vec<VPath>,
    generation: u64,
) -> CompareStatProbe {
    use futures::StreamExt as _;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let b = backend.clone();
    tokio::spawn(async move {
        let resultado: Vec<(VPath, Option<Entry>)> = futures::stream::iter(paths)
            .map(|path| {
                let b = b.clone();
                async move {
                    let entry = tokio::time::timeout(STAT_PROBE_TIMEOUT, b.stat(&path))
                        .await
                        .ok()
                        .and_then(Result::ok);
                    (path, entry)
                }
            })
            .buffer_unordered(STAT_BATCH_CONCURRENCY)
            .collect()
            .await;
        let _ = tx.send(resultado);
    });
    CompareStatProbe { rx, generation }
}

/// Fetch de decoraciones de plugin EN VUELO (G3b, ADR 0037): el pane/dir
/// destino y el canal one-shot. Molde de [`StatProbe`] — UNO POR PANE
/// (#117-follow-up review MINOR-2: con un slot global, un cd en el pane B
/// pisaba el fetch en vuelo del A y sus columnas `plugin:` configuradas
/// quedaban en blanco hasta el próximo cd de A — con las columnas ahora
/// config-driven eso contradecía «jamás una columna permanentemente en
/// blanco»). `dir` se conserva para descartar una respuesta TARDÍA que ya
/// no corresponde al listado actual del pane. Límite heredado del diseño
/// de decoraciones (review MINOR-3): `paths` es la página YA listada al
/// asentar el cd — entradas drenadas DESPUÉS por el fill incremental
/// (#52/#54) no viajan en la petición y pintan blanco hasta el próximo
/// re-list (documentado, mismo alcance que las decoraciones).
/// Valores de columnas `plugin:` por id Display → `VPath` → celda saneada
/// (#117-follow-up) — el shape que consume `PaneState::set_plugin_columns`.
pub type PluginColumnValues =
    std::collections::HashMap<String, std::collections::HashMap<VPath, String>>;

/// Un fetch de decoraciones y columnas de plugin en vuelo, por HUECO.
pub struct DecorateFetch {
    /// El HUECO al que va, no la posición: una respuesta tardía tiene que
    /// aterrizar en el listado que la pidió, no en quien ocupe su sitio.
    pub slot: SlotId,
    /// El dir bajo el que se pidió: si el hueco ya está en otro, se tira.
    pub dir: VPath,
    /// Decoraciones por path, y valores de columna por id de plugin.
    pub rx: tokio::sync::oneshot::Receiver<(
        std::collections::HashMap<VPath, norte_frontend::Decoration>,
        PluginColumnValues,
    )>,
}

/// Una lectura de preview en vuelo, por HUECO.
///
/// Guarda la ruta que pidió: cuando llega, si el hueco ya quiere otra cosa
/// —el cursor se movió mientras volaba— la respuesta se TIRA. Es la regla 3
/// del spec y la lección de la fase C de P6, que es la misma cosa.
pub struct PreviewFetch {
    /// La ruta que se pidió: si el hueco ya quiere otra, la respuesta se tira.
    pub path: VPath,
    /// El visor construido, o el error de lectura sin traducir.
    pub rx: tokio::sync::oneshot::Receiver<Result<Viewer, Error>>,
}

/// Lee `path` en segundo plano para el hueco `slot`.
///
/// Sin `select!` sobre el teclado, a diferencia de [`crate::viewer_open::open_viewer`]: nadie está
/// esperando delante del preview, así que no hay nada que cancelar con `Esc`.
/// Lo que sí hay es supersesión: mover el cursor deja caer este `Receiver` y
/// la respuesta se pierde sin aplicarse.
pub fn spawn_preview_fetch(backend: &Backend, path: VPath) -> PreviewFetch {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let b = backend.clone();
    let p = path.clone();
    tokio::spawn(async move {
        let _ = tx.send(viewer_for(&b, &p).await);
    });
    PreviewFetch { path, rx }
}

/// Lanza el fetch de decoraciones (G3b) para TODAS las entradas actualmente
/// listadas de `pane` (la "página visible" — el listado YA cargado, sea la
/// primera página de un dir grande paginándose o el dir entero; el resto de
/// un dir aún rellenándose queda sin decorar hasta la próxima visita, mismo
/// alcance MVP documentado en el ADR/plan). Sin guardia especial de "algún
/// decorator activado": `Backend::plugin_decorate` resuelve el catálogo en
/// cada llamada (barato embebido, una RPC remota) — intentar cada listado y
/// descartar en silencio si no hay decoradores consentidos es más simple y
/// honesto que cachear un flag que podría quedar obsoleto tras un F12.
/// `None` si el pane no tiene entradas (nada que decorar).
/// #117-follow-up: el MISMO viaje trae también los valores de las columnas
/// `plugin:` CONFIGURADAS del scheme (`plugin_cols` = pares
/// (plugin, columna) de `ColumnsSettings::plugin_ids_for`) — un solo slot
/// en vuelo, un solo guard anti-stale.
pub fn spawn_decorate_fetch(
    backend: &Backend,
    slot: SlotId,
    dir: VPath,
    paths: Vec<VPath>,
    plugin_cols: Vec<(String, String)>,
) -> Option<DecorateFetch> {
    if paths.is_empty() {
        return None;
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    let b = backend.clone();
    tokio::spawn(async move {
        let plugins = b.plugin_decorate(&paths).await.unwrap_or_default();
        let merged = norte_frontend::merge_decorations(&paths, &plugins);
        // Review MINOR-1 (regla 3 en espíritu): un fetch SUPERADO (el run
        // loop pisó el slot → rx dropeado) corta antes de cada RPC restante
        // en vez de gastar hasta 8 llamadas cuyo send fallará igual.
        let cols = fetch_plugin_columns(&b, &plugin_cols, &paths, || tx.is_closed()).await;
        let _ = tx.send((merged, cols));
    });
    Some(DecorateFetch { slot, dir, rx })
}

/// Valores de las columnas `plugin:` configuradas (#117-follow-up): la
/// validación de pertenencia + dedupe de colisiones vive en el modelo
/// COMPARTIDO (`norte_frontend::columns::validated_plugin_requests` —
/// review MAJOR-1: una sola definición para ambos frontends; colisión de
/// id bare = blanco antes que atribución falsa, desambiguación real =
/// issue #120). Fail-soft por columna: catálogo caído o RPC fallida =
/// celdas en blanco, jamás un error de listado. `superseded` corta entre
/// RPCs cuando el fetch ya fue pisado (review MINOR-1).
async fn fetch_plugin_columns(
    backend: &Backend,
    requested: &[(String, String)],
    paths: &[VPath],
    superseded: impl Fn() -> bool,
) -> PluginColumnValues {
    let mut out = std::collections::HashMap::new();
    if requested.is_empty() {
        return out;
    }
    let Ok(list) = backend.plugins_list().await else {
        return out;
    };
    for (plugin, column) in
        norte_frontend::columns::validated_plugin_requests(requested, &list.plugins)
    {
        if superseded() {
            return out;
        }
        let raw = backend
            .plugin_column_values(&plugin, &column, paths)
            .await
            .unwrap_or_default();
        let sanitized = norte_frontend::columns::sanitize_column_values(paths, &raw);
        out.insert(
            norte_frontend::columns::plugin_display_id(&plugin, &column),
            sanitized,
        );
    }
    out
}
