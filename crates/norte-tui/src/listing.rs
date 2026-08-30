//! Listar un directorio para PONERLO en un pane.
//!
//! Vivía en el root del binario `ntc`, que es un crate DISTINTO de esta lib, y
//! eso lo hacía inalcanzable para todo lo demás — incluida
//! [`crate::session_push`], que restaura la sesión listando los huecos que la
//! sesión colocó.
//!
//! El error es el del `Backend` y no un `anyhow`: la regla 6 reserva `anyhow`
//! para los binarios, y aquí el `anyhow` solo envolvía un
//! [`norte_proto::Error`] en su propio `Display` — el tipo se perdía para no
//! ganar nada. Los llamantes del binario siguen usando `?` dentro de una
//! función `anyhow`, que es exactamente lo que `From` ya sabe hacer.

use norte_core::backend::Backend;
use norte_proto::{Error, VPath};

use crate::app::Pane;

/// Pane inicial del arranque: listado COMPLETO de `start` pidiendo los
/// attrs configurados (#117) — sin ellos las celdas attr nacerían en
/// blanco hasta el primer cd/refresh. Regla 7: todo por el `Backend`.
///
/// # Errors
///
/// Lo que devuelva el `Backend` al listar `start`: no se traduce ni se
/// envuelve, porque quien lo pinta necesita el tipo (un `PermissionDenied` de
/// arranque no se dice igual que un `NotFound`).
pub async fn initial_pane(
    backend: &Backend,
    start: &VPath,
    attrs: &[String],
) -> Result<Pane, Error> {
    let (entries, skipped) = backend.list_with_skipped_attrs(start, attrs).await?;
    // El arranque de un panel es una pantalla: se retiene el ancla (#301).
    backend.remember_listing_anchor(start).await;
    let mut pane = Pane::new(start.clone(), entries);
    // #93: las omitidas del contenedor también en el ARRANQUE — el badge no
    // debe nacer vacío teniendo el dato gratis (review #117 tarea 2).
    pane.set_skipped(skipped);
    Ok(pane)
}
