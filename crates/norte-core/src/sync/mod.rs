//! Sincronización de directorios: el plan retenido y su ejecución (spec
//! `docs/superpowers/specs/2026-08-11-directory-sync-design.md`, ADR 0049).
//!
//! El PLANIFICADOR no vive aquí: es `norte-sync`, un transductor puro sobre las
//! filas de `norte-compare` que no toca un provider. Lo que vive aquí es todo
//! lo que necesita un daemon para que ese plan se pueda **aprobar** y
//! **ejecutar**:
//!
//! - [`spool`] — el plan aprobado, retenido en un fichero atado a la conexión
//!   que lo produjo. Es lo que hace que `sync.apply` no lleve más que un hash y
//!   que lo que se ejecuta sea, por la FORMA del wire, lo que un humano vio.
//!
//! La Task de `sync.plan`, el ejecutor y el lote del journal llegan en las
//! tareas 8 y 9 del plan.

pub mod spool;

pub use spool::{
    SPOOL_DIR_NAME, SPOOL_FORMAT, Spool, SpoolError, SpoolHeader, SpoolReader, SpoolSummary,
    SpoolWriter,
};
