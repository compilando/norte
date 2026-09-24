//! The DOCKED viewer (L3): what it should be showing, and what it shows.
//!
//! The DECISION, and only the decision, lives here: `main.rs` is a binary, so
//! nothing written there can be proven by an integration test. What this
//! module answers — "what should be read right now?" — is a pure function of
//! the tree, the roles, and the cursor, and that is why the spec's rules can
//! be pinned with tests instead of prose:
//!
//! - a preview slot the layout did not place (closed, behind a tab, or
//!   collapsed for lack of room) produces no target, so there is no request
//!   to count: **suspension is not a separate check someone can forget to
//!   write**;
//! - a directory under the cursor produces no read;
//! - the target travels with its SLOT, never with its position (the lesson
//!   from P6's phase C: an in-flight response applied by position lands on
//!   whoever occupies that spot when it arrives).

use norte_frontend::layout::{Resolved, SlotId};
use norte_frontend::viewer::Viewer;
use norte_proto::{EntryKind, VPath};

use crate::app::App;

/// The kind that occupies a viewer slot. The same as the full-screen viewer:
/// what changes is the link, not what is inside.
pub const KIND: &str = "viewer";

/// What the preview should be showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Want {
    /// This file, which must be read.
    File(VPath),
    /// Nothing to read, and this Fluent key says why: a directory, or a
    /// listing with no cursor.
    Note(&'static str),
}

/// What a preview slot has painted RIGHT NOW.
///
/// Stores the path alongside what is painted on purpose: it is what avoids
/// re-reading what is already being shown, and lets a response that arrives
/// late for a cursor that already moved be discarded.
/// (`Viewer` is not `Debug` — the manual `Debug` implementation below covers
/// `TuiPanel`'s, saying what is being shown without dumping an entire file
/// into a log.)
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
            .field("has_viewer", &self.viewer.is_some())
            .field("note", &self.note)
            .finish()
    }
}

impl Preview {
    /// A freshly opened preview: nothing inside yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Which path is being shown, if any.
    #[must_use]
    pub const fn shown(&self) -> Option<&VPath> {
        self.shown.as_ref()
    }

    /// The viewer, if a file has been read.
    #[must_use]
    pub fn viewer(&self) -> Option<&Viewer> {
        self.viewer.as_deref()
    }

    /// The viewer, to move it: the `viewer.*` keys work here the same as at
    /// full screen, because it is the same viewer.
    pub fn viewer_mut(&mut self) -> Option<&mut Viewer> {
        self.viewer.as_deref_mut()
    }

    /// The text that replaces the file: a directory, an error, a denial.
    #[must_use]
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Shows the file that was read.
    pub fn show(&mut self, path: VPath, viewer: Viewer) {
        self.shown = Some(path);
        self.viewer = Some(Box::new(viewer));
        self.note = None;
    }

    /// Shows text instead of a file.
    ///
    /// `path` is what the text is about, so a note from an old cursor is not
    /// left up once the cursor is already somewhere else.
    pub fn say(&mut self, path: Option<VPath>, text: String) {
        self.shown = path;
        self.viewer = None;
        self.note = Some(text);
    }
}

/// The preview slot PLACED in this layout, if any.
///
/// From the layout, not the tree: a slot behind a tab exists but is not being
/// seen, and what is not seen is not read.
#[must_use]
pub fn slot(app: &App, res: &Resolved) -> Option<SlotId> {
    res.placements
        .iter()
        .map(|(id, _)| *id)
        .find(|id| app.layout.kind_of(*id).is_some_and(|k| k.as_str() == KIND))
}

/// What the preview should be showing, and in which slot.
///
/// `None` when no preview is placed. The link is resolved by the engine
/// ([`norte_frontend::layout::resolve_follow`]), so a followed slot that dies
/// degrades to the `active` role with its diagnostic, instead of leaving the
/// pane staring at emptiness in silence.
#[must_use]
pub fn want(app: &App, res: &Resolved) -> Option<(SlotId, Want)> {
    let slot_id = slot(app, res)?;
    let mut diags = Vec::new();
    let in_a_row =
        norte_frontend::layout::resolve_follow(&app.layout, slot_id, &app.roles, &mut diags)
            .or_else(|| app.roles.get(norte_frontend::layout::RoleId::Active))?;
    let pane = app.panes.browser(in_a_row)?;
    // `cursor_entry` and not `selected`, for the same reason as the
    // attributes sheet: the viewer DESCRIBES what is under the cursor, and
    // over the `..` row "what is targeted" is `None` on purpose. It used to
    // say "nothing selected" with a row leading to a folder right there.
    let Some(entry) = pane.cursor_entry() else {
        return Some((slot_id, Want::Note("preview-empty")));
    };
    match entry.kind {
        EntryKind::File => Some((slot_id, Want::File(entry.path.clone()))),
        EntryKind::Dir => Some((slot_id, Want::Note("preview-directory"))),
        // A symlink or something the provider does not classify: it is not
        // read blindly, because reading "whatever it is" is exactly how an
        // automatic preview turns into opening a block device by accident.
        _ => Some((slot_id, Want::Note("preview-not-a-file"))),
    }
}
