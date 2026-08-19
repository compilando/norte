//! La hoja de atributos (fase A): qué debería estar enseñando.
//!
//! Como [`crate::preview`], aquí vive la DECISIÓN y solo la decisión, por la
//! misma razón: es una función pura del árbol, los roles y el cursor, así que
//! las reglas se fijan con tests en vez de con prosa.
//!
//! A diferencia del visor acoplado, este panel **no lee nada**: la `Entry` que
//! enseña ya está en el listado. Un panel que sigue al cursor y además pide
//! datos por cada fila es como bajar por un directorio se convierte en una
//! tormenta de peticiones.

use norte_frontend::layout::{Resolved, SlotId};
use norte_proto::Entry;

use crate::app::App;

/// El kind que ocupa un hueco de atributos.
pub const KIND: &str = "metadata";

/// Qué debería estar enseñando la hoja.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Want {
    /// Esta entrada, que el listado ya tiene delante.
    Entry(Box<Entry>),
    /// Nada que enseñar, y esta clave Fluent dice por qué.
    Note(&'static str),
}

/// El hueco de atributos COLOCADO en este reparto, si lo hay.
///
/// Del reparto y no del árbol: un hueco detrás de una pestaña existe, pero no
/// se está viendo, y lo que no se ve no enseña nada.
#[must_use]
pub fn slot(app: &App, res: &Resolved) -> Option<SlotId> {
    res.placements
        .iter()
        .map(|(id, _)| *id)
        .find(|id| app.layout.kind_of(*id).is_some_and(|k| k.as_str() == KIND))
}

/// Qué toca enseñar, y en qué hueco. `None` si no hay hueco colocado.
///
/// El vínculo se resuelve con el motor, así que un hueco seguido que muere
/// degrada al rol `active` con su diagnóstico en vez de quedarse mirando al
/// vacío en silencio.
#[must_use]
pub fn want(app: &App, res: &Resolved) -> Option<(SlotId, Want)> {
    let hueco = slot(app, res)?;
    let mut diags = Vec::new();
    let in_a_row =
        norte_frontend::layout::resolve_follow(&app.layout, hueco, &app.roles, &mut diags)
            .or_else(|| app.roles.get(norte_frontend::layout::RoleId::Active))?;
    let pane = app.panes.browser(in_a_row)?;
    match pane.selected() {
        Some(e) => Some((hueco, Want::Entry(Box::new(e.clone())))),
        None => Some((hueco, Want::Note("metadata-empty"))),
    }
}
