//! Lo que el bucle de eventos tiene EN VUELO: los trabajos de fondo, las
//! sondas y los rellenos paginados que sus brazos del `select!` cosechan.
//!
//! Eran diecisiete variables locales de `run`, y por eso toda función que
//! quisiera salir de ese bucle nacía con quince parámetros. Juntas tienen un
//! nombre: son el trabajo que ESTE proceso dejó pedido y aún no ha llegado.
//! De cada clase hay a lo sumo UNO —el panel que lo enseña es uno— salvo los
//! que van por hueco ([`norte_frontend::layout::BySlot`]), donde el pane que
//! pagina no puede estrangular al otro.

use std::collections::VecDeque;

use norte_frontend::layout::BySlot;
use tokio_util::sync::CancellationToken;

use crate::fill::Fill;
use crate::jobs::{CompareRun, SearchRun, SyncRun};
use crate::lua::CommandRun;
use crate::probes::{CompareStatProbe, DecorateFetch, PreviewFetch, Probed, StatProbe};
use norte_proto::{Error, VPath};

/// Petición `ai.rename_plan` EN VUELO (M4-IA). Abortar el `JoinHandle`
/// cancela (regla 3): el abort dropea el future del backend en el runtime →
/// `CancelOnAbandon` envía `rpc.cancel` (remoto) / el timeout+drop aborta el
/// stream (embebido). OJO: DROPEAR el handle solo DESVINCULA la task de
/// tokio — cancelar exige `abort()` explícito.
pub struct AiRenameRun {
    /// La llamada al modelo, spawneada (es la única llamada larga del loop).
    pub handle: tokio::task::JoinHandle<Result<norte_proto::methods::AiRenamePlanResult, Error>>,
    /// Dir del pane al LANZAR; el plan se aplica AQUÍ aunque el usuario
    /// navegue mientras el modelo piensa.
    pub dir: VPath,
    /// Los nombres que había en ese dir al lanzar.
    ///
    /// El cinturón exige que cada `from` del plan EXISTA donde se va a
    /// aplicar (#275), y para cuando el modelo conteste el lector puede estar
    /// en otro sitio: preguntarle al pane entonces validaría el plan contra
    /// un directorio que no es el suyo.
    pub names: Vec<Vec<u8>>,
}

/// Un plan IA YA cosechado que espera a que se cierre el modal de turno
/// (M4-IA). Lleva el estado del plan del LOTE (§17), que se pide en cuanto
/// llega el plan IA: sin él, el modal abriría sin hash aprobado y confirmar
/// quedaría mudo hasta un segundo viaje que nadie dispara.
pub struct PendingAiPlan {
    /// Dir del pane al LANZAR (donde aterriza el lote).
    pub dir: VPath,
    /// Los nombres de ese dir al lanzar, por el mismo motivo que en
    /// [`AiRenameRun::names`].
    pub names: Vec<Vec<u8>>,
    /// Parejas from→to del modelo.
    pub entries: Vec<norte_proto::methods::AiRenameEntry>,
    /// Veredicto del lote: en vuelo, resuelto, o fallido.
    pub plan: norte_frontend::BatchPlan,
}

/// Petición `fs.rename_batch_plan` EN VUELO (§17). Spawneada por el mismo
/// motivo que [`AiRenameRun`]: es un `fs.list` del dir entero contra el
/// provider que toque, y esperarla dentro del `select!` dejaría el loop sin
/// dibujar, sin leer teclas y sin poder cancelar. A lo sumo una — el prompt
/// del rename IA no abre sobre otro modal, así que no hay dos planes IA
/// vivos a la vez que pudieran pisarse.
pub struct RenameBatchRun {
    /// La llamada al core, spawneada.
    pub handle:
        tokio::task::JoinHandle<Result<norte_proto::methods::FsRenameBatchPlanResult, Error>>,
}

/// Petición `index.search_semantic` EN VUELO (M4-IA-2). Mismo contrato de
/// cancelación que [`AiRenameRun`] (regla 3): `abort()` dropea el future del
/// backend → `rpc.cancel` (remoto) / drop (embebido); DROPEAR el handle solo
/// desvincula. Sin dir capturado: la consulta va contra TODOS los roots del
/// índice (`root = None`), navegar mientras piensa no la invalida.
pub struct SemanticRun {
    /// La llamada al índice+modelo, spawneada.
    pub handle: tokio::task::JoinHandle<Result<Vec<norte_proto::methods::SemanticHit>, Error>>,
}

/// Todo lo que el bucle pidió y aún no ha cosechado.
#[derive(Default)]
pub struct InFlight {
    /// Listados paginados rellenándose en background (ADR 0017): un hueco POR
    /// PANE — los dos panes pueden estar paginando a la vez, y con un hueco
    /// global el cd de uno mataba el drenador del otro.
    pub fill: BySlot<Fill>,
    /// Por dónde sigue el barrido de [`Self::fill`] (ver el brazo del
    /// `select!`).
    pub fill_cursor: usize,
    /// Búsqueda viva en curso (liveSearch T6): a lo sumo una, el pane virtual
    /// es uno. Se drena en el select y se suelta al salir.
    pub search: Option<SearchRun>,
    /// Comparación de directorios en curso (`Shift+F2`): a lo sumo una, el
    /// panel de diferencias es uno.
    pub compare: Option<CompareRun>,
    /// Sincronización en curso (`Ctrl+Y`): a lo sumo una — aprobar un plan
    /// mientras otro se aplica sería aprobar a ciegas.
    pub sync: Option<SyncRun>,
    /// Petición `ai.rename_plan` en vuelo (M4-IA): a lo sumo una — relanzar
    /// aborta la anterior; Esc (BROWSE) la cancela.
    pub ai_rename: Option<AiRenameRun>,
    /// Plan IA listo llegado con OTRO modal abierto: se RETIENE aquí (la cola
    /// de `App` es específica de aprobaciones) y se abre en cuanto no haya
    /// modal — jamás pisar (disciplina `open_next_pending`).
    pub pending_ai_plan: Option<PendingAiPlan>,
    /// Petición `fs.rename_batch_plan` en vuelo (§17): a lo sumo una,
    /// cosechada en el select como [`Self::ai_rename`].
    pub rename_batch: Option<RenameBatchRun>,
    /// Búsqueda semántica en vuelo (M4-IA-2): mismo molde que
    /// [`Self::ai_rename`].
    pub semantic: Option<SemanticRun>,
    /// Hits listos llegados con OTRO modal abierto: se RETIENEN aquí y se
    /// abren en cuanto no haya modal (disciplina [`Self::pending_ai_plan`]).
    pub pending_semantic: Option<Vec<norte_proto::methods::SemanticHit>>,
    /// Comando Lua en vuelo (M4, ADR 0026): a lo sumo UNO —el estado Lua es
    /// uno— y se pollea inline en el select, porque `CommandRun` es !Send y
    /// su future corre en `block_on`, jamás en un spawn.
    pub lua: Option<(CommandRun, CancellationToken)>,
    /// Comandos Lua encolados mientras otro corría.
    pub lua_queue: VecDeque<String>,
    /// Sonda de stat on-focus (#52, listado lazy): a lo sumo una en vuelo.
    pub stat: Option<StatProbe>,
    /// Dedup de la sonda de arriba, por (pane, path): dos panes sobre el
    /// MISMO dir hidratan cada uno la suya, y un stat fallido no se reintenta
    /// hasta cambiar de selección.
    pub probed: Probed,
    /// Sonda de stat de la fila SELECCIONADA del panel de diferencias (#157).
    /// Su dedup vive en `App::compare_size_probed` y no aquí, porque
    /// `App::compare_size_probe_targets` ya lo consulta para decidir qué
    /// falta por pedir.
    pub compare_stat: Option<CompareStatProbe>,
    /// Fetch de decoraciones de plugin en vuelo (G3b, ADR 0037), por hueco.
    pub decorate: BySlot<DecorateFetch>,
    /// L3: una lectura de preview en vuelo por hueco, superseded al moverse.
    pub preview: BySlot<PreviewFetch>,
}
