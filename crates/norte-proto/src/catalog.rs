//! El catálogo del protocolo: qué métodos existen y de qué forma son.
//!
//! # Por qué existe
//!
//! Un método nuevo se toca en muchos sitios: la constante y sus tipos aquí, el
//! reparto del daemon, el cliente remoto, las rutas de notificación, el
//! backend embebido y el remoto, el schema, los goldens, el MCP o los
//! frontends, y la ventana de compatibilidad N/N-1. Ninguno de esos sitios es
//! superfluo y el reparto plano del daemon es deliberado — el problema nunca
//! fue que hubiera muchas superficies, sino que **olvidar una no se notaba**.
//!
//! Esto es la fuente declarativa contra la que se puede comprobar. No genera
//! los handlers, ni los cuerpos del daemon, ni la policy: genera la LISTA, y
//! los tests la usan para preguntarle a cada superficie si está.
//!
//! # Lo que el catálogo NO dice
//!
//! No lleva el acceso (humano/agente) ni nada de policy. Es a propósito: un
//! campo de acceso aquí sería una segunda fuente de verdad sobre quién puede
//! llamar a qué, y una que nadie consulta miente en cuanto la primera cambie.
//! Quien decide eso es el daemon, en el mismo sitio donde siempre. Cuando haya
//! un test que verifique el acceso REAL contra lo declarado, entonces cabrá
//! declararlo.
//!
//! # Dónde vive el nombre
//!
//! La constante se queda donde está, con su documentación —que en este
//! protocolo es la explicación de por qué cada método es como es, y son miles
//! de líneas—. El catálogo la NOMBRA, no la redeclara: meterla dentro de una
//! macro escondería justo lo que hay que leer. Que las dos no se separen lo
//! garantiza un test: una constante de método que no esté aquí pone el gate en
//! rojo.

use crate::methods;

/// Si el método lo INICIA quien llama o lo manda el daemon por su cuenta.
///
/// `non_exhaustive` desde el principio: el catálogo va a crecer (el acceso
/// humano/agente entra en cuanto haya un test que lo verifique), y añadir una
/// variante o un campo después sería una rotura de API que `cargo-semver-checks`
/// marca. Ponerlo ahora no cuesta nada; ponerlo luego, un major.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Kind {
    /// Petición con respuesta: lleva id y se contesta.
    Request,
    /// Notificación: sin id, sin respuesta. La manda el daemon.
    Notification,
}

/// Cómo entrega el método lo que produce.
///
/// Sobre una NOTIFICACIÓN, `Direct` no quiere decir «contesta»: quiere decir
/// «de un disparo», frente a `Stream`, «por lotes». Una notificación no
/// contesta nada.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Shape {
    /// De un disparo: contesta en el propio resultado (o, si es notificación,
    /// llega una y ya).
    Direct,
    /// Devuelve un `task_id` y el trabajo sigue por `task.progress`.
    Task,
    /// El resultado va llegando por notificaciones dirigidas.
    Stream,
    /// El handshake, que no es ninguna de las tres.
    Handshake,
}

/// Una entrada del catálogo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct MethodInfo {
    /// El nombre de wire, tomado de su constante.
    pub name: &'static str,
    /// Petición o notificación.
    pub kind: Kind,
    /// Cómo entrega lo que produce.
    pub shape: Shape,
    /// Nombre del tipo de params, o `"()"` si no lleva.
    pub params_ty: &'static str,
    /// Nombre del tipo de result, o `"()"` si no lleva.
    pub result_ty: &'static str,
}

impl MethodInfo {
    /// El tipo de params, si lleva.
    #[must_use]
    pub fn params(&self) -> Option<&'static str> {
        (self.params_ty != "()").then_some(self.params_ty)
    }

    /// El tipo de result, si lleva.
    #[must_use]
    pub fn result(&self) -> Option<&'static str> {
        (self.result_ty != "()").then_some(self.result_ty)
    }
}

/// Declara el catálogo, y hace que el compilador verifique los tipos.
///
/// Los nombres de tipo se guardan como cadenas —un `&'static [MethodInfo]` no
/// puede llevar tipos— y eso los dejaría sin comprobar. La función de abajo lo
/// arregla: menciona cada uno, así que un nombre mal escrito no compila. Sin
/// ella el catálogo sería una lista de deseos.
macro_rules! rpc_catalogo {
    ($( $konst:ident, $kind:ident, $shape:ident, $params:ty, $result:ty; )*) => {
        /// Todos los métodos del protocolo, en el orden en que se declararon.
        pub const CATALOGO: &[MethodInfo] = &[
            $(
                MethodInfo {
                    name: methods::$konst,
                    kind: Kind::$kind,
                    shape: Shape::$shape,
                    params_ty: stringify!($params),
                    result_ty: stringify!($result),
                }
            ),*
        ];

        /// Los tipos que el catálogo nombra EXISTEN. No se llama nunca.
        #[allow(dead_code, clippy::used_underscore_items)]
        fn _los_tipos_existen() {
            $(
                let _: Option<$params> = None;
                let _: Option<$result> = None;
            )*
        }
    };
}

rpc_catalogo! {
    // El handshake, obligatorio antes que nada (ADR 0011).
    INITIALIZE, Request, Handshake, methods::InitializeParams, methods::InitializeResult;
    DAEMON_SHUTDOWN, Request, Direct, methods::DaemonShutdownParams, methods::DaemonShutdownResult;

    // Lecturas del sistema de ficheros.
    FS_LIST, Request, Direct, methods::FsListParams, methods::FsListResult;
    FS_STAT, Request, Direct, methods::FsStatParams, methods::FsStatResult;
    FS_READ, Request, Direct, methods::FsReadParams, methods::FsReadResult;
    FS_CAPABILITIES, Request, Direct, methods::FsCapabilitiesParams, methods::FsCapabilitiesResult;

    // Mutaciones: todas Task, todas por el journal.
    FS_COPY, Request, Task, methods::FsCopyParams, methods::FsTaskResult;
    FS_MOVE, Request, Task, methods::FsMoveParams, methods::FsTaskResult;
    FS_DELETE, Request, Task, methods::FsDeleteParams, methods::FsTaskResult;
    FS_MKDIR, Request, Task, methods::FsMkdirParams, methods::FsTaskResult;
    FS_CREATE, Request, Task, methods::FsCreateParams, methods::FsTaskResult;
    FS_SET_MODE, Request, Task, methods::FsSetModeParams, methods::FsTaskResult;

    // Buscar y comparar: Task que entrega por notificación dirigida.
    FS_SEARCH, Request, Stream, methods::FsSearchParams, methods::FsTaskResult;
    SEARCH_HITS, Notification, Stream, methods::SearchHits, ();
    FS_COMPARE, Request, Stream, methods::FsCompareParams, methods::FsTaskResult;
    COMPARE_ROWS, Notification, Stream, methods::CompareRowsBatch, ();

    // Recuento, sumas y sus informes.
    FS_DIR_SIZE, Request, Task, methods::FsDirSizeParams, methods::FsTaskResult;
    FS_CHECKSUM, Request, Task, methods::FsChecksumParams, methods::FsTaskResult;
    FS_CHECKSUM_REPORT, Request, Direct, methods::FsChecksumReportParams, methods::FsChecksumReportResult;

    // Índice y semántica.
    // Task, no Direct: el daemon contesta `FsTaskResult { task_id }` y el
    // `IndexBuildResult` es el DESENLACE, que además hoy no viaja por wire.
    INDEX_BUILD, Request, Task, methods::IndexBuildParams, methods::FsTaskResult;
    INDEX_QUERY, Request, Direct, methods::IndexQueryParams, methods::IndexQueryResult;
    INDEX_EMBED, Request, Task, methods::IndexEmbedParams, methods::FsTaskResult;
    INDEX_SEARCH_SEMANTIC, Request, Direct, methods::IndexSearchSemanticParams, methods::IndexSearchSemanticResult;

    // Renombrado: el plan lo propone un modelo, el lote lo ejecuta el core.
    AI_RENAME_PLAN, Request, Direct, methods::AiRenamePlanParams, methods::AiRenamePlanResult;
    FS_RENAME_BATCH_PLAN, Request, Direct, methods::FsRenameBatchPlanParams, methods::FsRenameBatchPlanResult;
    FS_RENAME_BATCH, Request, Task, methods::FsRenameBatchParams, methods::FsTaskResult;
    FS_RENAME_BATCH_REPORT, Request, Direct, methods::FsRenameBatchReportParams, methods::FsRenameBatchReportResult;

    // Archivos: empaquetar, comprobar, partir y juntar.
    ARCHIVE_PACK, Request, Task, methods::ArchivePackParams, methods::FsTaskResult;
    ARCHIVE_PACK_REPORT, Request, Direct, methods::ArchivePackReportParams, methods::ArchivePackReportResult;
    ARCHIVE_TEST, Request, Task, methods::ArchiveTestParams, methods::FsTaskResult;
    ARCHIVE_TEST_REPORT, Request, Direct, methods::ArchiveTestReportParams, methods::ArchiveTestResult;
    FILE_SPLIT, Request, Task, methods::FileSplitParams, methods::FsTaskResult;
    FILE_COMBINE, Request, Task, methods::FileCombineParams, methods::FsTaskResult;

    // Sincronizar: el plan queda RETENIDO a nombre de la conexión.
    SYNC_PLAN, Request, Stream, methods::SyncPlanParams, methods::FsTaskResult;
    SYNC_STEPS, Notification, Stream, methods::SyncStepsBatch, ();
    SYNC_PLAN_DONE, Notification, Stream, methods::SyncPlanDone, ();
    SYNC_APPLY, Request, Task, methods::SyncApplyParams, methods::FsTaskResult;
    SYNC_REPORT, Request, Direct, methods::SyncReportParams, methods::SyncReportResult;

    // Tasks.
    TASK_LIST, Request, Direct, methods::TaskListParams, methods::TaskListResult;
    TASK_CANCEL, Request, Direct, methods::TaskCancelParams, methods::TaskCancelResult;
    TASK_PROGRESS, Notification, Stream, crate::TaskProgress, ();
    // NOTIFICACIÓN, no petición: va sin id y sin respuesta, y un daemon N-1
    // que no la conozca la descarta en silencio (ADR 0004). Mandarla como
    // petición sería esperar una contestación que no llega nunca.
    RPC_CANCEL, Notification, Direct, methods::RpcCancelParams, ();

    // Conexiones.
    HOST_VOLUMES, Request, Direct, methods::HostVolumesParams, methods::HostVolumesResult;
    CONNECTION_LIST, Request, Direct, (), methods::ConnectionListResult;
    CONNECTION_CLOSE, Request, Direct, methods::ConnectionCloseParams, methods::ConnectionCloseResult;
    CONNECTION_TRUST_HOST_KEY, Request, Direct, methods::ConnectionTrustHostKeyParams, methods::ConnectionTrustHostKeyResult;
    CONNECTION_PROVIDE_SECRET, Request, Direct, methods::ConnectionProvideSecretParams, methods::ConnectionProvideSecretResult;
    CONNECTION_DEGRADED, Notification, Direct, methods::ConnectionDegraded, ();
    DAEMON_GOING_AWAY, Notification, Direct, methods::DaemonGoingAway, ();

    // Policy: gobierno humano de lo que pide un agente.
    POLICY_REQUEST_SCOPE, Request, Direct, methods::RequestScopeParams, methods::RequestScopeResult;
    POLICY_GRANT_SCOPE, Request, Direct, methods::GrantScopeParams, methods::GrantScopeResult;
    POLICY_DECIDE, Request, Direct, methods::PolicyDecideParams, methods::PolicyDecideResult;
    POLICY_PENDING, Request, Direct, (), methods::PolicyPendingResult;
    POLICY_APPROVAL_REQUIRED, Notification, Direct, methods::PolicyApprovalRequired, ();
    POLICY_UNDO_SESSION, Request, Task, methods::PolicyUndoSessionParams, methods::PolicyUndoSessionResult;
    POLICY_UNDO_REPORT, Request, Direct, methods::PolicyUndoReportParams, methods::PolicyUndoReportResult;

    // Extensiones.
    PLUGIN_LIST, Request, Direct, methods::PluginListParams, methods::PluginListResult;
    PLUGIN_SET_APPROVAL, Request, Direct, methods::PluginSetApprovalParams, methods::PluginSetApprovalResult;
    PLUGIN_SET_ENABLED, Request, Direct, methods::PluginSetEnabledParams, methods::PluginSetEnabledResult;
    PLUGIN_RUN_COMMAND, Request, Direct, methods::PluginRunCommandParams, methods::PluginRunCommandResult;
    PLUGIN_PREVIEW, Request, Direct, methods::PluginPreviewParams, methods::PluginPreviewResult;
    PLUGIN_PREVIEW_STYLED, Request, Direct, methods::PluginPreviewStyledParams, methods::PluginPreviewStyledResult;
    PLUGIN_DECORATE, Request, Direct, methods::PluginDecorateParams, methods::PluginDecorateResult;
    PLUGIN_COLUMN_VALUES, Request, Direct, methods::PluginColumnValuesParams, methods::PluginColumnValuesResult;
    PLUGIN_GET_CONFIG, Request, Direct, methods::PluginGetConfigParams, methods::PluginGetConfigResult;
    PLUGIN_SET_CONFIG, Request, Direct, methods::PluginSetConfigParams, methods::PluginSetConfigResult;
    PLUGIN_HELP, Request, Direct, methods::PluginHelpParams, methods::PluginHelpResult;

    // Sesión de la ventana.
    SESSION_GET, Request, Direct, (), methods::SessionGetResult;
    SESSION_PUT, Request, Direct, methods::SessionPutParams, methods::SessionPutResult;
}

/// La entrada de un método por su nombre de wire.
#[must_use]
pub fn buscar(name: &str) -> Option<&'static MethodInfo> {
    CATALOGO.iter().find(|m| m.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ningún nombre repetido: dos entradas con el mismo nombre harían que
    /// `buscar` contestara la primera y la otra no existiera para nadie.
    #[test]
    fn los_nombres_no_se_repiten() {
        let mut vistos = std::collections::BTreeSet::new();
        for m in CATALOGO {
            assert!(vistos.insert(m.name), "nombre repetido: {}", m.name);
        }
    }

    /// Una notificación no lleva resultado: no hay a quién contestárselo.
    #[test]
    fn una_notificacion_no_tiene_resultado() {
        for m in CATALOGO {
            if m.kind == Kind::Notification {
                assert_eq!(m.result(), None, "{} es notificación y trae result", m.name);
            }
        }
    }

    /// Y `buscar` encuentra lo que hay.
    #[test]
    fn buscar_encuentra_por_nombre_de_wire() {
        assert_eq!(
            buscar(methods::FS_STAT).map(|m| m.shape),
            Some(Shape::Direct)
        );
        assert_eq!(buscar(methods::FS_COPY).map(|m| m.shape), Some(Shape::Task));
        assert_eq!(buscar("fs.no_existe"), None);
    }
}
