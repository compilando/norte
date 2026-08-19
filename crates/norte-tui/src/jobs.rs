//! Las tres tareas largas que un panel enseña mientras corren: buscar,
//! comparar y sincronizar.
//!
//! Las tres tienen la misma forma —se lanzan, van llegando por un canal que se
//! drena, y mientras viven su panel se come el teclado con una tabla de teclas
//! propia— y las tres vivían en el root del binario `ntc`, un crate DISTINTO de
//! esta lib.
//!
//! De momento aquí solo está [`SearchRun`], y es a propósito: hay un ciclo
//! entre este módulo y [`crate::navigate`] —el `cd` tiene que soltar una
//! búsqueda viva al salir del pane virtual, y lanzar una búsqueda necesita el
//! `cd`—. Un ciclo entre módulos del MISMO crate es legal en Rust, así que el
//! orden de salida no importa; lo que no se puede es dejar el tipo en el
//! binario, que sí es otro crate. El resto de la banda entra detrás.

use norte_core::backend::TaskRef;
use norte_proto::VPath;
use norte_proto::methods::SearchHits;

use crate::app::SearchState;

/// Una búsqueda viva EN CURSO (`Alt+F7`, liveSearch T6): la Task cancelable,
/// el canal de lotes de hits y el pane virtual que los muestra. Molde `Fill`:
/// vive en el run loop, se drena en el `select!` y se suelta al salir del modo
/// virtual (un `cd`) cancelando la Task (regla 3).
pub struct SearchRun {
    /// Task de `fs.search` (cancelable con `TaskRef::cancel`).
    pub task: TaskRef,
    /// Canal de lotes de hits (embebido: lo cierra el walker; remoto: la
    /// bomba del `RemoteBackend` lo cierra al terminal).
    pub rx: tokio::sync::mpsc::Receiver<SearchHits>,
    /// Pane que muestra los hits (índice en `App::panes`).
    pub pane: usize,
    /// Directorio ANTERIOR del pane, para restaurarlo al salir del modo
    /// virtual (Esc tras terminar).
    pub prev_dir: VPath,
    /// Hits acumulados (== `panes[pane].entries().len()`, contador propio para
    /// no depender del re-sort del pane).
    pub hits: usize,
    /// Estado del run: `Running` mientras el walker emite; terminal tras
    /// cerrarse el canal (se lee del `TaskProgress`).
    pub state: SearchState,
}
