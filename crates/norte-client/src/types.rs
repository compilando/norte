//! Los valores que el SDK necesita nombrar por su cuenta.
//!
//! Ninguno es una copia por comodidad: son las cosas que un cliente remoto
//! usa y que vivían en `norte-core` solo porque el cliente vivía allí. Cada
//! una está aquí por un motivo distinto, y el motivo está escrito al lado.

use std::time::Duration;

use norte_proto::{CollisionPolicy, Entry, Error, ResumePolicy, SymlinkPolicy, VerifyPolicy};

/// Timeout de llamadas de IA: el proveedor (modelo remoto) tarda
/// legítimamente mucho más que un `fs.*`.
pub const AI_CALL_TIMEOUT: Duration = Duration::from_mins(2);

/// El stream de un listado paginado.
///
/// Alias PROPIO y no el de `norte-vfs` (que es idéntico) porque arrastrar el
/// crate del contrato de providers a un cliente que solo habla por socket
/// sería pagar un árbol entero por un alias (ADR 0066).
pub type EntryStream = futures::stream::BoxStream<'static, Result<Entry, Error>>;

/// Copiar o mover: los dos verbos de una transferencia (#270).
///
/// Un enum, y no el nombre del método como cadena, porque cuando el verbo era
/// una `&str` el despacho era `if method == FS_COPY { … } else { … }`: todo lo
/// que no fuera exactamente `fs.copy` se convertía en un MOVIMIENTO, que
/// además borra el origen. El fallo de un typo no era un error visible sino la
/// otra operación. `norte-ui-host` ya interponía un enum propio por su lado
/// para no poder equivocarse; el SDK no lo tenía.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transfer {
    /// `fs.copy`.
    Copy,
    /// `fs.move` — BORRA el origen.
    Move,
}

/// Las opciones de una transferencia, tal como viajan por el wire.
///
/// Gemela de `norte_core::engine::TransferOptions`, y a propósito: la del
/// core es la entrada del ENGINE y puede crecer con cosas que solo el motor
/// entiende; esta es lo que un cliente remoto pone en los params. El core
/// convierte entre las dos con un `From` exhaustivo, así que un campo nuevo
/// en cualquiera de ellas es un error de compilación y no una opción que se
/// pierde en silencio.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransferOptions {
    /// Qué hacer si el destino ya existe.
    pub on_collision: CollisionPolicy,
    /// Qué hacer con los symlinks del origen.
    pub symlinks: SymlinkPolicy,
    /// Reanudación de transferencias interrumpidas (ADR 0012).
    pub resume: ResumePolicy,
    /// Verificación del parcial al reanudar (solo con `resume=On`).
    pub verify: VerifyPolicy,
    /// A la COLA en vez de en paralelo (ADR 0149): de una en una.
    pub queued: bool,
}

/// Lo que un `sync.plan` va emitiendo.
///
/// Vive en el SDK y `norte_core::sync` lo re-exporta —en vez de tener cada
/// uno el suyo— porque sus dos variantes SON tipos del wire: el plan
/// embebido y el remoto emiten exactamente lo mismo, y dos definiciones
/// serían dos sitios donde añadir una variante.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncPlanEvent {
    /// Un lote de pasos, acotado por `SYNC_STEPS_MAX_BATCH`.
    Steps(norte_proto::methods::SyncStepsBatch),
    /// El cierre del plan. Como mucho UNO por Task, y siempre el último.
    Done(norte_proto::methods::SyncPlanDone),
}

/// Evento de conexión del backend remoto (para la barra de mensajes).
///
/// `#[non_exhaustive]`: este crate es la superficie publicable del SDK (ADR
/// 0066), y añadir `GoingAway` ya obligó a tocar todos los `match` de fuera.
/// El siguiente evento tiene que poder ser aditivo.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnEvent {
    /// La conexión con el daemon se perdió; reconectando en background.
    Lost,
    /// Reconectado (y resincronizado vía `task.list`).
    Restored,
    /// El daemon avisó de que se va (`daemon.going_away`, 0.46.0).
    ///
    /// Llega ANTES de que la conexión se cierre, y es lo único que distingue
    /// un relevo de una parada: desde el corte las dos se ven igual. El
    /// frontend lo necesita para decir cuál de las dos está pasando en vez de
    /// pintar «reconectando…» sobre un daemon que no va a volver.
    GoingAway {
        /// El daemon dice que vuelve (un relevo, p. ej. una actualización).
        reconnect: bool,
    },
}
