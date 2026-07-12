//! Provider VFS sobre SFTP/SSH (ADR 0013), con `russh` + `russh-sftp`.
//!
//! El [`SftpProvider`] envuelve un `SftpSession` YA establecido (inyección
//! de sesión): la conexión SSH con auth y verificación de host key llega en
//! la fase 6 (`connections.toml` + keyring). Aquí vive la lógica del
//! provider y su contención de un servidor hostil (`../../`, symlinks
//! trampa) — ver [`SftpProvider`].
//!
//! Los tipos de `russh-sftp` NO cruzan la frontera pública: fuera del crate
//! solo se ve el trait [`norte_vfs::Provider`].
#![forbid(unsafe_code)]

mod provider;

pub use provider::SftpProvider;
