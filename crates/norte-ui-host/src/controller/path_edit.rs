//! The path line (`pane.edit-path`, ADR 0190): a slot's path, editable in
//! place of its breadcrumbs. The `<input>` owns the caret; the host decides
//! what text opens it, where Enter leads and what Tab completes, with the
//! terminal's functions (`norte_frontend::path_edit`).

#[allow(clippy::wildcard_imports)]
use super::*;

use norte_frontend::goto::Action;

/// The line while it is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Open {
    /// The slot whose path is edited.
    pub(super) slot: u32,
    /// The text, as the host last saw it.
    pub(super) text: String,
    /// All of it selected (just opened).
    pub(super) select_all: bool,
    /// The caret, in UTF-16 code units.
    pub(super) cursor: u32,
    /// `PathEditView::revision`.
    pub(super) revision: u32,
}

/// `s`'s length in UTF-16 code units, the unit an `<input>` counts in.
fn utf16_len(s: &str) -> u32 {
    u32::try_from(s.encode_utf16().count()).unwrap_or(u32::MAX)
}

/// The byte offset of UTF-16 offset `units` in `s`, rounded down to a char.
fn byte_of_utf16(s: &str, units: u32) -> usize {
    let mut seen = 0u32;
    for (i, c) in s.char_indices() {
        let next = seen.saturating_add(u32::try_from(c.len_utf16()).unwrap_or(2));
        if next > units {
            return i;
        }
        seen = next;
    }
    s.len()
}

impl State {
    /// What the renderer paints for slot `id`.
    pub(super) fn path_edit_view(&self, id: u32) -> Option<crate::dto::PathEditView> {
        let open = self.path_edit.as_ref().filter(|o| o.slot == id)?;
        Some(crate::dto::PathEditView {
            text: open.text.clone(),
            select_all: open.select_all,
            cursor: open.cursor,
            revision: open.revision,
        })
    }

    /// Puts `text` in the line on `slot`, the host's change: a new
    /// revision, so the renderer replaces what its `<input>` holds.
    fn set_path_edit(&mut self, slot: u32, text: String, select_all: bool, cursor: u32) {
        self.path_edit_revision = self.path_edit_revision.wrapping_add(1);
        self.path_edit = Some(Open {
            slot,
            text,
            select_all,
            cursor,
            revision: self.path_edit_revision,
        });
    }

    /// `pane.edit-path`: opens the line on the active slot's path, or says
    /// why it cannot.
    pub(super) fn open_path_edit(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let slot = self.active();
        let refused = |key: &str| ActionAck::Unavailable {
            reason_key: key.to_owned(),
        };
        let Some(h) = self.slots.get(&slot) else {
            let said = self.say("msg-path-edit-no-listing");
            return (refused("msg-path-edit-no-listing"), said);
        };
        let Some(text) = norte_frontend::path_edit::initial_text(h.pane.dir()) else {
            let said = self.say("msg-path-edit-refused");
            return (refused("msg-path-edit-refused"), said);
        };
        // Never clamped: a cut path is another path.
        if text.len() > crate::bridge::MAX_STRING_BYTES {
            let said = self.say("msg-path-edit-too-long");
            return (refused("msg-path-edit-too-long"), said);
        }
        // A navigation the line sent earlier no longer gives text back.
        self.path_edit_retry = None;
        let cursor = utf16_len(&text);
        self.set_path_edit(slot, text, true, cursor);
        self.responds_with_snapshot()
    }

    /// Whether `line` (slot, revision) is the line open now. An older
    /// revision is an `<input>` already replaced, whose late blur or key
    /// must not touch the current one.
    fn is_open(&self, (slot, revision): (u32, u32)) -> bool {
        self.path_edit
            .as_ref()
            .is_some_and(|o| o.slot == slot && o.revision == revision)
    }

    /// Keeps `text` as typed with no new revision: the `<input>` stays.
    fn keep_typed(&mut self, text: &str) {
        if let Some(open) = self.path_edit.as_mut() {
            text.clone_into(&mut open.text);
            open.select_all = false;
        }
    }

    /// Enter: goes to what the line says. A text that names no folder
    /// keeps the line open and says why; a folder that cannot be listed
    /// gives the text back when its listing fails (`land_listing`).
    pub(super) fn commit_path_edit(
        &mut self,
        line: (u32, u32),
        text: &str,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if !self.is_open(line) {
            return (Self::stale(StaleAction::Generation), Vec::new());
        }
        // The `<input>` has no filter of its own: what the terminal's line
        // would never let in is refused here.
        if !norte_frontend::path_edit::acceptable(text) {
            self.keep_typed(text);
            return (self.applied(), self.say("msg-path-edit-refused"));
        }
        let dir = match norte_frontend::path_edit::target(text) {
            Action::Ir(dir) => dir,
            other => {
                let reason = match other {
                    Action::Nothing(reason) => reason,
                    _ => "msg-goto-bad-path",
                };
                self.keep_typed(text);
                // As go-anywhere answers the same text: applied, and said.
                return (self.applied(), self.say(reason));
            }
        };
        let slot = line.0;
        self.path_edit = None;
        let mut out = self.navigate_slot(slot, &dir, Trail::Record, backend, mailbox);
        if let Some(token) = self.slots.get(&slot).and_then(|h| h.in_flight) {
            self.path_edit_retry = Some((slot, token, text.to_owned()));
        }
        let snap = self.snapshot();
        out.push(self.over(UiUpdate::Snapshot(Box::new(snap))));
        (self.applied(), out)
    }

    /// Escape or a click elsewhere: the line goes, nothing moves.
    pub(super) fn cancel_path_edit(
        &mut self,
        line: (u32, u32),
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if !self.is_open(line) {
            return (self.applied(), Vec::new());
        }
        self.path_edit = None;
        self.responds_with_snapshot()
    }

    /// Tab: completes the folder name before `cursor` against the slot's
    /// listing. Nothing to complete leaves the `<input>` as it is.
    pub(super) fn complete_path_edit(
        &mut self,
        line: (u32, u32),
        text: &str,
        cursor: u32,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let slot = line.0;
        if !self.is_open(line) {
            return (Self::stale(StaleAction::Generation), Vec::new());
        }
        let Some(h) = self.slots.get(&slot) else {
            return (Self::stale(StaleAction::Generation), Vec::new());
        };
        let done = norte_frontend::path_edit::acceptable(text)
            .then(|| {
                let dirs = norte_frontend::path_edit::folders_in(h.pane.dir(), h.pane.entries());
                let at = byte_of_utf16(text, cursor);
                norte_frontend::path_edit::complete(text, at, h.pane.dir(), &dirs)
            })
            .flatten();
        if let Some((done, caret)) = done {
            let cursor = utf16_len(&done[..caret]);
            self.set_path_edit(slot, done, false, cursor);
            return self.responds_with_snapshot();
        }
        self.keep_typed(text);
        (self.applied(), Vec::new())
    }

    /// A listing landed on `slot` with `token`: if it was the path line's
    /// and it failed, the line comes back with what was typed — unless the
    /// reader has moved on (another line open, another panel in front).
    pub(super) fn path_edit_landed(&mut self, slot: u32, token: RequestToken, failed: bool) {
        let Some((s, t, text)) = self.path_edit_retry.take() else {
            return;
        };
        if (s, t) != (slot, token) {
            // Another navigation's: still waiting for its own.
            self.path_edit_retry = Some((s, t, text));
            return;
        }
        if failed && self.path_edit.is_none() && slot == self.active() {
            let cursor = utf16_len(&text);
            self.set_path_edit(slot, text, false, cursor);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{byte_of_utf16, utf16_len};

    /// The `<input>` counts UTF-16; the line is UTF-8. An astral char is
    /// two units and four bytes.
    #[test]
    fn utf16_offsets_map_to_char_boundaries() {
        let s = "/a/𝄞b";
        assert_eq!(utf16_len(s), 6);
        assert_eq!(byte_of_utf16(s, 3), 3);
        assert_eq!(byte_of_utf16(s, 5), 7);
        // Inside the pair: rounded down to the char.
        assert_eq!(byte_of_utf16(s, 4), 3);
        assert_eq!(byte_of_utf16(s, 99), s.len());
    }
}
