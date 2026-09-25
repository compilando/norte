//! The attributes sheet (phase A): what it should be showing.
//!
//! Like [`crate::preview`], the DECISION and only the decision lives here, for
//! the same reason: it is a pure function of the tree, the roles, and the
//! cursor, so the rules are pinned with tests instead of prose.
//!
//! Unlike the docked viewer, this pane **reads nothing**: the `Entry` it shows
//! is already in the listing. A pane that follows the cursor and also
//! requests data per row is how walking down a directory turns into a storm
//! of requests.

use norte_frontend::layout::{Resolved, SlotId};
use norte_proto::Entry;

use crate::app::App;

/// The kind that occupies an attributes slot.
pub const KIND: &str = "metadata";

/// What the sheet should be showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Want {
    /// This entry, which the listing already has in front of it. The `bool`
    /// says whether it is the `..` row: the sheet describes it as `..` and
    /// not by the parent's name.
    Entry(Box<Entry>, bool),
    /// Nothing to show, and this Fluent key says why.
    Note(&'static str),
}

/// The attributes slot PLACED in this layout, if any.
///
/// From the layout, not the tree: a slot behind a tab exists but is not being
/// seen, and what is not seen shows nothing.
#[must_use]
pub fn slot(app: &App, res: &Resolved) -> Option<SlotId> {
    res.placements
        .iter()
        .map(|(id, _)| *id)
        .find(|id| app.layout.kind_of(*id).is_some_and(|k| k.as_str() == KIND))
}

/// The listing the sheet for slot `slot` follows.
///
/// The link is resolved by the shared engine, so a followed slot that dies
/// degrades to the `active` role with its diagnostic instead of being left
/// staring at emptiness in silence.
fn followed(app: &App, slot: SlotId) -> Option<&crate::app::Pane> {
    let mut diags = Vec::new();
    let in_a_row =
        norte_frontend::layout::resolve_follow(&app.layout, slot, &app.roles, &mut diags)
            .or_else(|| app.roles.get(norte_frontend::layout::RoleId::Active))?;
    app.panes.browser(in_a_row)
}

/// The path of the listing the placed sheet follows, already paintable, for
/// the pane's TITLE.
///
/// "Details" on its own does not say details of what: with two listings open,
/// the only way to tell which one was being described was to move the cursor
/// and see whether the sheet moved. The same answer the window gives in
/// `MetadataSlotView::follows_display` (ADR 0077).
#[must_use]
pub fn follows(app: &App, res: &Resolved) -> Option<(String, bool)> {
    let pane = followed(app, slot(app, res)?)?;
    Some(norte_frontend::path_display_with(
        pane.dir(),
        pane.name_encoding(),
    ))
}

/// What to show, and in which slot. `None` if no slot is placed.
///
/// The link is resolved by the engine, so a followed slot that dies degrades
/// to the `active` role with its diagnostic instead of being left staring at
/// emptiness in silence.
#[must_use]
pub fn want(app: &App, res: &Resolved) -> Option<(SlotId, Want)> {
    let slot_id = slot(app, res)?;
    let pane = followed(app, slot_id)?;
    // `cursor_entry` and not `selected`: the sheet DESCRIBES what is under
    // the cursor, and over the `..` row "what is targeted" is `None` on
    // purpose — that row is not an operand. Asking for the operand left the
    // pane empty right where the cursor is born.
    // The flag comes from the SAME index as the entry: asking `cursor()` by
    // hand, a quick-search filter — which chooses on its own and does not
    // move the real cursor — left the sheet describing `..` while the listing
    // highlighted another row.
    match pane.cursor_entry() {
        Some(e) => Some((
            slot_id,
            Want::Entry(Box::new(e.clone()), pane.cursor_is_parent_row()),
        )),
        None => Some((slot_id, Want::Note("metadata-empty"))),
    }
}
