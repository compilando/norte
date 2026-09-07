//! El panel de procesos (fase A): el cursor, y nada más.
//!
//! El estado vive en `norte-frontend`, no aquí: acotar el cursor al LEER es la
//! respuesta a «¿qué fila se cancelaría?», y esa pregunta la contestan las dos
//! superficies (ADR 0077). Lo que queda en este crate es el `KIND`, que sí es
//! del terminal — es lo que su disposición escribe.

/// El kind que ocupa un hueco de procesos.
pub const KIND: &str = "processes";

pub use norte_frontend::processes::Processes;
