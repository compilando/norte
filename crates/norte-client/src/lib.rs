//! El SDK de cliente del daemon de norte.
//!
//! Lo que hace falta para hablar con un daemon —y NADA de lo que hace falta
//! para ser uno—. Un frontend que solo habla por socket no tiene por qué
//! arrastrar el engine, los providers, el índice ni el host de plugins, que
//! es lo que pasaba cuando todo esto vivía dentro de `norte-core`
//! (ADR 0066).
//!
//! Tres capas, de abajo arriba:
//!
//! - `transport` (privado): cómo se llega al daemon y cómo se le autentica.
//!   Hoy, un socket UNIX con credenciales del peer.
//! - [`rpc`]: el JSON-RPC enmarcado, que no sabe por dónde viaja.
//! - `remote`: el backend tipado que los frontends usan de verdad.
//!
//! La frontera de este crate es su lista de dependencias, y hay un test que
//! la vigila: `tests/dependency_boundary.rs`.
#![forbid(unsafe_code)]

pub mod remote;
pub mod rpc;
pub mod socket;
pub mod task;
mod transport;
pub mod types;

pub use remote::RemoteBackend;
pub use remote::calls::to_taxonomy;
pub use rpc::{Client, ClientError, is_version_mismatch};
pub use socket::{
    SPAWNED_DAEMON_IDLE_SECS, daemon_run_argv, default_socket_path, process_uid_best_effort,
};
pub use task::{RemoteTask, RemoteTaskCanceller};
pub use types::{
    AI_CALL_TIMEOUT, ConnEvent, EntryStream, SyncPlanEvent, Transfer, TransferOptions,
};
