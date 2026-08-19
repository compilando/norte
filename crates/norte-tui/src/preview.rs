//! El visor ACOPLADO (L3): qué debería estar enseñando, y qué enseña.
//!
//! Aquí vive la DECISIÓN, y solo la decisión: `main.rs` es un binario, así que
//! nada de lo que se escriba allí lo puede probar un test de integración. Lo
//! que este módulo contesta —«¿qué toca leer ahora mismo?»— es una función
//! pura del árbol, los roles y el cursor, y por eso las reglas del spec se
//! pueden fijar con tests en vez de con prosa:
//!
//! - un hueco de preview que el reparto no colocó (cerrado, detrás de una
//!   pestaña, o colapsado por falta de sitio) no produce objetivo, así que no
//!   hay petición que contar: **la suspensión no es una comprobación aparte
//!   que alguien pueda olvidarse de escribir**;
//! - un directorio bajo el cursor no produce lectura;
//! - el objetivo viaja con su HUECO, nunca con su posición (la lección de la
//!   fase C de P6: una respuesta en vuelo aplicada por posición aterriza en
//!   quien ocupe ese sitio al llegar).

use norte_frontend::layout::{Resolved, SlotId};
use norte_frontend::viewer::Viewer;
use norte_proto::{EntryKind, VPath};

use crate::app::App;

/// El kind que ocupa un hueco de visor. El mismo que el visor a pantalla
/// completa: lo que cambia es el vínculo, no lo que hay dentro.
pub const KIND: &str = "viewer";

/// Qué debería estar enseñando el preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Want {
    /// Este fichero, que hay que leer.
    File(VPath),
    /// Nada que leer, y esta clave Fluent dice por qué: un directorio, o un
    /// listado sin cursor.
    Note(&'static str),
}

/// Lo que un hueco de preview tiene pintado AHORA.
///
/// Guarda la ruta junto a lo pintado a propósito: es lo que permite no volver
/// a leer lo que ya se está enseñando, y descartar una respuesta que llega
/// tarde para un cursor que ya se movió.
/// (`Viewer` no es `Debug` — el `Debug` de `TuiPanel` lo resuelve la
/// implementación manual de abajo, que dice qué se está enseñando sin volcar
/// un fichero entero en un log.)
#[derive(Default)]
pub struct Preview {
    shown: Option<VPath>,
    viewer: Option<Box<Viewer>>,
    note: Option<String>,
}

impl std::fmt::Debug for Preview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Preview")
            .field("shown", &self.shown)
            .field("con_visor", &self.viewer.is_some())
            .field("note", &self.note)
            .finish()
    }
}

impl Preview {
    /// Un preview recién abierto: sin nada dentro todavía.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Qué ruta está enseñando, si alguna.
    #[must_use]
    pub const fn shown(&self) -> Option<&VPath> {
        self.shown.as_ref()
    }

    /// El visor, si hay fichero leído.
    #[must_use]
    pub fn viewer(&self) -> Option<&Viewer> {
        self.viewer.as_deref()
    }

    /// El visor, para moverlo: las teclas `viewer.*` valen aquí igual que a
    /// pantalla completa, porque es el mismo visor.
    pub fn viewer_mut(&mut self) -> Option<&mut Viewer> {
        self.viewer.as_deref_mut()
    }

    /// El texto que sustituye al fichero: un directorio, un error, una
    /// denegación.
    #[must_use]
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Enseña el fichero leído.
    pub fn show(&mut self, path: VPath, viewer: Viewer) {
        self.shown = Some(path);
        self.viewer = Some(Box::new(viewer));
        self.note = None;
    }

    /// Enseña un texto en vez de un fichero.
    ///
    /// `path` es de qué va el texto, para que una nota de un cursor viejo no
    /// se quede puesta cuando el cursor ya está en otro sitio.
    pub fn say(&mut self, path: Option<VPath>, text: String) {
        self.shown = path;
        self.viewer = None;
        self.note = Some(text);
    }
}

/// El hueco de preview COLOCADO en este reparto, si lo hay.
///
/// Del reparto y no del árbol: un hueco detrás de una pestaña existe, pero no
/// se está viendo, y lo que no se ve no lee.
#[must_use]
pub fn slot(app: &App, res: &Resolved) -> Option<SlotId> {
    res.placements
        .iter()
        .map(|(id, _)| *id)
        .find(|id| app.layout.kind_of(*id).is_some_and(|k| k.as_str() == KIND))
}

/// Qué debería estar enseñando el preview, y en qué hueco.
///
/// `None` cuando no hay preview colocado. El vínculo se resuelve con el motor
/// ([`norte_frontend::layout::resolve_follow`]), así que un hueco seguido que
/// muere degrada al rol `active` con su diagnóstico, en vez de dejar el panel
/// mirando al vacío en silencio.
#[must_use]
pub fn want(app: &App, res: &Resolved) -> Option<(SlotId, Want)> {
    let hueco = slot(app, res)?;
    let mut diags = Vec::new();
    let seguido =
        norte_frontend::layout::resolve_follow(&app.layout, hueco, &app.roles, &mut diags)
            .or_else(|| app.roles.get(norte_frontend::layout::RoleId::Active))?;
    let pane = app.panes.browser(seguido)?;
    let Some(entry) = pane.selected() else {
        return Some((hueco, Want::Note("preview-empty")));
    };
    match entry.kind {
        EntryKind::File => Some((hueco, Want::File(entry.path.clone()))),
        EntryKind::Dir => Some((hueco, Want::Note("preview-directory"))),
        // Un enlace o algo que el provider no clasifica: no se lee a ciegas,
        // porque leer «lo que sea» es justo como un preview automático se
        // convierte en abrir un dispositivo de bloque sin querer.
        _ => Some((hueco, Want::Note("preview-not-a-file"))),
    }
}
