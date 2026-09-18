//! Las tres tareas largas que un panel enseña mientras corren: buscar,
//! comparar y sincronizar.
//!
//! Las tres tienen la misma forma —se lanzan, van llegando por un canal que se
//! drena, y mientras viven su panel se come el teclado con una tabla de teclas
//! propia— y las tres vivían en el root del binario `ntc`, un crate DISTINTO de
//! esta lib.
//!
//! Hay un ciclo entre este módulo y [`crate::navigate`] —el `cd` tiene que
//! soltar una búsqueda viva al salir del pane virtual, y lanzar una búsqueda
//! necesita el `cd`—. Un ciclo entre módulos del MISMO crate es legal en Rust,
//! así que el orden de salida no importa; lo que no se podía es dejar una mitad
//! en el binario, que sí es otro crate.
//!
//! Los tres `*Run` son el asa: la Task cancelable (regla 3), el canal, y la
//! generación con la que un lote que llega tarde se descarta en vez de mezclarse
//! con el plan siguiente.
//!
//! Un fichero por dominio y `mod.rs` de pura fachada, que es el patrón que
//! `norte-frontend/src/layout/` ya demuestra en este repo: ningún fichero de
//! producción por encima de las mil líneas.

mod ai;
mod compare;
mod diskmap;
// Público, al contrario que los demás: sus dos verbos —pedir y olvidar— los
// llama el manejador de teclas en cada letra, y leerlos como
// `jobs::goto::pedir_al_indice` dice de qué pantalla son.
pub mod goto;
mod inflight;
mod search;
mod sync;

pub use ai::{
    harvest_ai_rename, harvest_checksum, harvest_organize, harvest_rename_batch, harvest_semantic,
    spawn_organize_plan, spawn_renamer_plan,
};
pub use compare::{
    COMPARE_PAGE_STEP, CompareKey, CompareRun, compare_key, drain_compare, launch_compare,
    on_compare_enter, on_compare_key,
};
pub use diskmap::{harvest as harvest_disk_map, lanzar as lanzar_disk_map};
pub use goto::harvest_goto_index;
pub use inflight::{
    AiRenameRun, ChecksumRun, DiskMapRun, GotoIndexRun, InFlight, OrganizeRun, PendingAiPlan,
    Publicado, RenameBatchRun, SemanticRun,
};
pub use search::{
    SEARCH_MAX_HITS, SearchRun, drain_search, finalize_search_state, launch_search,
    on_search_dialog_key, on_search_enter, on_search_escape, search_params,
};
pub use sync::{
    SyncKey, SyncRun, SyncTick, approve_sync, drain_sync_plan, harvest_sync_apply,
    launch_sync_apply, launch_sync_plan, on_sync_key, submit_sync, sync_key,
};
