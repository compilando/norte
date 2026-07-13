//! Provider VFS sobre object storage vía opendal (ADR 0016): S3 primero,
//! GCS/Azure después activando features del workspace sin tocar este código.
//!
//! El [`opendal::Operator`] llega INYECTADO ya configurado (bucket, region,
//! endpoint, credenciales) desde `norte-connect` (fase 7d): este crate jamás
//! ve un secreto (reglas 7/10 de CLAUDE.md).

#![forbid(unsafe_code)]

mod provider;

pub use provider::ObjectProvider;

// El único tipo de opendal que cruza la frontera pública es el del
// constructor (patrón `FtpStream` de norte-vfs-ftp): norte-connect lo
// construye y el core no depende de opendal directamente.
pub use opendal::Operator;
