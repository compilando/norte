//! Provider VFS sobre FTP (ADR 0014), con `suppaftp`.
//!
//! El [`FtpProvider`] envuelve una conexión FTP YA establecida (inyección de
//! conexión): el `connect` con auth (cleartext / FTPS) llega en la fase 6.
//! Aquí vive la lógica del provider y su contención de nombres hostiles.
//!
//! Los tipos de `suppaftp` NO cruzan la frontera pública: fuera del crate solo
//! se ve el trait [`norte_vfs::Provider`].
#![forbid(unsafe_code)]

mod provider;

pub use provider::FtpProvider;
