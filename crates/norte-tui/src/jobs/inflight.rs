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
use crate::probes::{
    CompareStatProbe, DecorateFetch, LogLevelProbe, LogTailProbe, PanelRenderProbe, PanelsProbe,
    PreviewFetch, Probed, StatProbe,
};
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

/// La medida de un mapa de disco EN VUELO (fase 4).
///
/// Mismo molde que [`ChecksumRun`] —la espera del informe va spawneada y el
/// ESTADO viaja con él, porque un informe de una Task cancelada está a medias—
/// con dos datos que las sumas no necesitan.
pub struct DiskMapRun {
    /// La espera del informe, spawneada.
    pub handle: tokio::task::JoinHandle<(
        norte_proto::TaskState,
        Result<norte_proto::methods::FsDirUsageReportResult, Error>,
    )>,
    /// La Task, para CANCELARLA si otra medida la releva. Abortar solo la
    /// espera dejaría al core recorriendo un `$HOME` entero sin nadie que lo
    /// recoja — y medir es justo lo que más tarda de todo esto.
    pub task: norte_core::backend::TaskObserver,
    /// El hueco cuyo mapa se está midiendo.
    pub slot: norte_frontend::layout::SlotId,
    /// El directorio que se mandó medir.
    ///
    /// Viaja con la medida para poder DESCARTAR lo que llegue tarde: medir un
    /// árbol grande tarda, y en ese rato el panel puede estar apuntando ya a
    /// otro sitio. Un informe aterrizado sin comprobar esto pintaría los
    /// tamaños de un directorio bajo el título de otro, que es la clase de
    /// mentira que este panel existe para no contar.
    pub dir: norte_proto::VPath,
}

/// Un lote de sumas EN VUELO (#311).
///
/// La Task ya está lanzada y en el tablero; lo que se espera aquí es el
/// INFORME, que solo tiene sentido pedir cuando la Task termina — los digests
/// no caben en el progreso. `publicado` distingue las dos caras del mismo
/// lote: `None` es «calcula y enséñame», `Some` es «compara contra esto».
pub struct ChecksumRun {
    /// La espera del informe, spawneada. Devuelve el ESTADO final de la Task
    /// junto al informe: un informe de una Task cancelada está a medias, y
    /// pintarlo como definitivo acusaría a ficheros que nadie llegó a leer.
    pub handle: tokio::task::JoinHandle<(
        norte_proto::TaskState,
        Result<norte_proto::methods::FsChecksumReportResult, Error>,
    )>,
    /// La Task, para poder CANCELARLA si otro lote la releva. Abortar solo la
    /// espera dejaría al core hasheando gigabytes sin nadie que los recoja.
    pub task: norte_core::backend::TaskObserver,
    /// Lo que el fichero de sumas publicaba, si esto es una verificación.
    pub publicado: Option<Publicado>,
}

/// El fichero de sumas leído, tal como hace falta para juzgarlo (#311).
pub struct Publicado {
    /// Las líneas entendidas, en el orden del fichero.
    pub lines: Vec<norte_frontend::checksums::SumLine>,
    /// Para cada línea, en qué posición de la PETICIÓN quedó su ruta, o `None`
    /// si su nombre no se puede escribir en este sistema —y eso no es que
    /// falte: es que aquí no se puede nombrar, y se arregla de otra forma—.
    ///
    /// Por índice y no por nombre: el informe conserva el orden pedido, y
    /// emparejar por nombre base daba «falta» sobre un `sub/dentro.txt` que
    /// estaba ahí.
    pub asked: Vec<Option<usize>>,
    /// Cuántas líneas parecían sumas y no se entendieron. Con esto mayor que
    /// cero, «todos correctos» no se puede decir.
    pub refused: usize,
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
    /// Lote de sumas en vuelo (#311): a lo sumo uno — el modal de resultados
    /// es uno, y lanzar otro CANCELA la Task del anterior además de abortar
    /// su espera.
    pub checksum: Option<ChecksumRun>,
    /// Medida de un mapa de disco en vuelo (fase 4): a lo sumo una — el panel
    /// es uno, y lanzar otra CANCELA la Task de la anterior. Sin eso, navegar
    /// deprisa por un árbol grande dejaba al core midiendo tres directorios
    /// que ya nadie iba a mirar.
    pub disk_map: Option<DiskMapRun>,
    /// Sumas listas llegadas con OTRO modal abierto: se RETIENEN aquí y se
    /// abren en cuanto no haya modal (disciplina [`Self::pending_ai_plan`]).
    /// Antes se tiraban, y la barra prometía «cierra el diálogo para verlas»
    /// sobre unas filas que ya no existían.
    pub pending_checksums: Option<(&'static str, Vec<crate::app::ChecksumRow>)>,
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
    /// Una vuelta de `log.tail` en vuelo (#328): a lo sumo una — el panel de
    /// registro es uno, y con dos un daemon lento acumularía una petición por
    /// vuelta del bucle para siempre.
    pub log_tail: Option<LogTailProbe>,
    /// El catálogo de plugins en vuelo, para declarar los paneles que aportan
    /// (fase 3): a lo sumo uno, y se pide UNA vez por sesión — lo que trae es
    /// qué huecos existen, no el contenido de ninguno.
    pub panels: Option<PanelsProbe>,
    /// El repintado de un panel de plugin en vuelo (fase 3): a lo sumo uno —el
    /// `multi: false` de su kind garantiza que hay como mucho un panel de
    /// plugin visible—, y pedir otro SUSTITUYE al anterior, soltando su
    /// receptor.
    pub panel_render: Option<PanelRenderProbe>,
    /// Cuándo toca la siguiente (ver [`crate::probes::LOG_TAIL_PERIODO`]).
    /// `None` = ya, que es lo que hace que abrir el panel pregunte en el acto.
    pub log_next_at: Option<tokio::time::Instant>,
    /// Una petición `log.level` al daemon en vuelo (#328): a lo sumo una, y la
    /// última pulsación releva a la anterior — pedirle dos niveles seguidos a
    /// un anillo que solo sube es pedirle el mayor.
    pub log_level: Option<LogLevelProbe>,
    /// El subshell persistente (#142): UNO por sesión, arrancado perezosamente
    /// la primera vez que se pide `app.toggle-panels` y vivo hasta salir.
    ///
    /// Vive aquí y no en `App` por lo mismo que el resto de esta estructura:
    /// es un recurso del run loop —un proceso, un pty y un hilo lector—, y el
    /// despacho de teclas no debe poder tocarlo. Que sea perezoso importa:
    /// quien nunca pulsa la tecla no paga un `fork` ni un pty.
    ///
    /// POSIX: en Windows no hay subshell y `app.toggle-panels` declina.
    #[cfg(unix)]
    pub subshell: Option<crate::subshell::Subshell>,
}
