//! The modals born from the listing's marks: the queue of collisions and
//! approvals, copy and move, delete, sorting by scheme or by key, and the
//! properties sheet with its hydration.

use super::App;
use super::modal::{Modal, PromptKind, TransferKind};
use norte_proto::VPath;

impl App {
    /// If no modal is open, opens the next queued collision's dialog. Call
    /// after closing a modal and on every tick.
    pub fn open_next_collision(&mut self) {
        if self.modal.is_none()
            && let Some(retry) = self.pending_collisions.pop_front()
        {
            self.modal = Some(Modal::Collision { retry });
            self.abandon_shortcut_capture();
            return;
        }
        // Reports, behind: a collision ASKS something with a copy waiting, a
        // report just recounts what already happened.
        if self.modal.is_none()
            && let Some((kind, lines)) = self.pending_reports.pop_front()
        {
            self.modal = Some(Modal::Report { kind, lines });
            self.abandon_shortcut_capture();
        }
    }

    /// A modal is taking the keyboard, so the shortcut editor stops ASKING for
    /// a blind keypress (K3c).
    ///
    /// Capture mode paints "press the new key" and the reader is primed to
    /// press anything at all. A modal that arrives on its own — a policy
    /// approval off the bus, a collision at the end of a copy — takes the keys
    /// and is painted on top, so that next key answers a question the reader
    /// did not know was being asked, and on `Modal::ApproveAgentOp` the letter
    /// `y` approves an agent operation. The key still reaches the modal (that
    /// part is the modal's right); what norte must not do is keep inviting it.
    ///
    /// The editor itself SURVIVES: the reader gets their list back after
    /// answering, unless the modal arm of the key chain retires it
    /// (`main::close_stale_overlays`).
    fn abandon_shortcut_capture(&mut self) {
        if let Some(sc) = &mut self.shortcuts {
            sc.cancel_capture();
        }
    }

    /// If no modal is open, opens the next pending dialog: policy approvals
    /// FIRST (they have a TTL on the daemon), collisions after. Call after
    /// closing a modal and on an approval arriving.
    pub fn open_next_pending(&mut self) {
        if self.modal.is_none()
            && let Some(req) = self.pending_approvals.pop_front()
        {
            self.modal = Some(Modal::ApproveAgentOp { req });
            self.abandon_shortcut_capture();
            return;
        }
        self.open_next_collision();
    }

    /// Cancels `Modal::MarkPattern` WITHOUT marking anything — the equivalent
    /// of a `DialogOutcome::Cancelled` for THIS free-text modal (#103 T9),
    /// which doesn't go through [`super::dialog_action`]'s ALLOWLIST and
    /// therefore has no Esc of its own in `on_dialog_key`. Opens the next
    /// queued pending one, same discipline as closing any other modal (never
    /// overwrite an approval/collision that arrived while this one was
    /// open).
    ///
    /// Deliberately NOT generic over `self.modal` (review rust MAJOR M1): for
    /// `Modal::ApproveAgentOp` closing it outright leaves the agent without
    /// an answer until the daemon's TTL — THAT modal's real close
    /// (`on_dialog_key`, `DialogOutcome::Cancelled`) pairs the close with an
    /// async `policy.decide(approve: false)`, something a synchronous method
    /// can't do. The structural guard (`debug_assert!`) makes the allowlist's
    /// "free-text modals only" something the test compiler enforces, not the
    /// caller's discipline.
    pub fn cancel_mark_pattern(&mut self) {
        self.cancel_prompt(PromptKind::MarkPattern);
    }

    /// Opens the confirmation for a copy or a move `from` → `to`.
    ///
    /// **The SINGLE source for what submits a transfer**, the key (F5/F6) and
    /// the drag alike. It's not a style choice: a drop is a mutation, and a
    /// second path — even if it were born identical today — would be left
    /// without the confirmation, without the collision modal, without the
    /// journal entry or without undo as soon as one of the two changed.
    /// That's why the drop builds no modal of its own: it requests the same
    /// one `pane.copy` would. Twin of `transfer_modal` in the GUI.
    ///
    /// `promoted` is the only difference between the two entry points, and it
    /// only says WHAT it acts on: `None` = the pane's marks (or the cursor if
    /// there are none — `marked_paths`, the usual single source); `Some(idx)`
    /// = that row alone, because the gesture was promoted from an UNMARKED
    /// row and the pane's marks — if any — are something else the user isn't
    /// dragging.
    ///
    /// With ONE single item the destination name is EDITABLE (#105); a
    /// multi-item batch stays on the list confirm (there's no single name to
    /// edit). No-op if there's nothing to transfer: never a dialog over an
    /// empty batch.
    pub fn open_transfer(
        &mut self,
        kind: TransferKind,
        from: usize,
        to: usize,
        promoted: Option<usize>,
    ) {
        let to_dir = self.panes[to].dir().clone();
        self.open_transfer_to_dir(kind, from, to_dir, promoted);
    }

    /// Like [`Self::open_transfer`] but against a DIRECTORY, not a panel.
    ///
    /// Exists because there isn't always "the other panel": with a single
    /// listing — `simple` — the reader types the destination
    /// ([`Self::open_transfer_dest`]), and that transfer has to go through
    /// the SAME door as F5, or it's left without confirmation, without
    /// collision handling and without undo.
    pub fn open_transfer_to_dir(
        &mut self,
        kind: TransferKind,
        from: usize,
        to_dir: VPath,
        promoted: Option<usize>,
    ) {
        let items: Vec<VPath> = match promoted {
            Some(idx) => self.panes[from]
                .entries()
                .get(idx)
                .map(|e| vec![e.path.clone()])
                .unwrap_or_default(),
            None => self.panes[from].marked_paths(),
        };
        match items.as_slice() {
            [] => {}
            [one] => {
                // `from_marks` decides whether the submit CONSUMES the
                // selection ([`Self::transfer_name_submitted`]). A promoted
                // drag never consumes it: the promotion changes what the
                // gesture DOES, not what's selected — and what's marked can
                // be something else the user hasn't let go of.
                let from_marks = promoted.is_none() && self.panes[from].marks_len() > 0;
                self.open_transfer_name_with(kind, from, one.clone(), to_dir, from_marks);
            }
            _ => {
                // The total ONLY if ALL items carry a size (#149): a
                // directory doesn't carry one in the listing, and adding up
                // what does would warn with a number smaller than the real
                // one — worse than saying nothing.
                self.pending_dest_check = Some(crate::app::DestCheck {
                    to: to_dir.clone(),
                    total: self.transfer_total(from, &items),
                });
                self.modal = Some(Modal::ConfirmTransfer {
                    kind,
                    items,
                    to: to_dir,
                    space: None,
                    confine: None,
                });
            }
        }
    }

    /// The bytes a transfer is going to write, or `None` if any of the items
    /// doesn't say (#149).
    ///
    /// All or nothing, on purpose: a directory doesn't carry a size in the
    /// listing and a lazy listing may not carry it even for a file. Adding
    /// up only what's known would give a total SMALLER than the real one,
    /// and warning with it is warning too little — which, over "doesn't
    /// fit", is exactly the mistake that cannot be made.
    fn transfer_total(&self, pane: usize, items: &[VPath]) -> Option<u64> {
        // The RULE — all or nothing — lives in the shared crate: the window
        // asks the same question in the same dialog, and a total computed
        // with a different criterion is an alarm that fires in one frontend
        // and not the other.
        norte_frontend::space::total_to_write(self.panes[pane].entries(), items)
    }

    /// Opens the delete modal (F8, #103 T10) over the focused pane's MARKS
    /// (or the cursor if there are none). `permanent` is decided by the
    /// caller: it's `shift+F8`, or the provider's lack of a trash — probed
    /// ONCE per batch, not once per item (that would be N network round
    /// trips to always get the same answer). No-op if there's nothing to
    /// delete.
    pub fn open_delete_modal(&mut self, permanent: bool) {
        let items = self.focused().marked_paths();
        if items.is_empty() {
            return;
        }
        self.modal = Some(Modal::ConfirmDelete { items, permanent });
    }

    /// The marks are CONSUMED by the operation (mc/Total Commander): they get
    /// cleared on SUBMITTING the batch, not on completing it, so there's
    /// never a half-consumed selection whose meaning depends on which task
    /// finished (#103).
    pub fn consume_marks(&mut self) {
        self.focused_mut().clear_marks();
    }

    /// Applies to `pane` its scheme's order per config (#108 b4): called on
    /// landing a `cd` (the scheme may have changed) and at startup.
    /// `set_sort` is a no-op if the spec doesn't change.
    pub fn apply_scheme_sort(&mut self, pane: usize) {
        let scheme = self.panes[pane].dir().scheme().to_owned();
        let spec = self.columns.sort_for(&scheme);
        self.panes[pane].set_sort(spec);
    }

    /// Sorts the FOCUSED pane by `col`, with header-click semantics (#138).
    ///
    /// The active column flips its direction; a new one sorts ascending.
    /// `dirs_first` isn't touched by any sort key: it's a user preference,
    /// not a column criterion — it's changed in the columns dialog, which is
    /// where it lives.
    ///
    /// Only the focused pane: the order belongs to ONE listing, same as the
    /// cursor.
    pub fn sort_focused_by(&mut self, col: norte_frontend::SortColumn) {
        let spec = self.focused().sort().after_click(col);
        self.focused_mut().set_sort(spec);
    }

    /// Opens the properties of the entry under the cursor (#139).
    ///
    /// Returns the path whose size needs counting, if it's a folder: the
    /// dialog doesn't talk to the backend — this is `App`, not the run loop —
    /// so it says what's needed and whoever can requests it.
    pub fn open_properties(&mut self) -> Option<VPath> {
        let entry = self.focused().selected()?.clone();
        let count = (entry.kind == norte_proto::EntryKind::Dir).then(|| entry.path.clone());
        self.modal = Some(Modal::Properties {
            entry: Box::new(entry),
            size_task: None,
            size: None,
        });
        count
    }

    /// Puts into the dialog the entry JUST requested from the backend.
    ///
    /// A lazy listing (#52) carries neither size nor date, and for a folder
    /// it NEVER carries them: without this, a directory's properties said
    /// "unknown to the backend" about something a `stat` knows perfectly
    /// well. Keeps the count — it's a different question — and doesn't
    /// overwrite the dialog if the human already closed it.
    pub fn properties_hydrate(&mut self, fresh: norte_proto::Entry) {
        if let Some(Modal::Properties { entry, .. }) = &mut self.modal
            && entry.path == fresh.path
        {
            **entry = fresh;
        }
    }

    /// Ties to the properties dialog the count that was just launched.
    pub fn properties_counting(&mut self, task: norte_proto::TaskId) {
        if let Some(Modal::Properties { size_task, .. }) = &mut self.modal {
            *size_task = Some(task);
        }
    }

    /// Puts into the dialog the result of ITS OWN count (#139).
    ///
    /// By `task_id` and not "whichever arrives last": between opening the
    /// dialog and the count finishing there's room for another count — one
    /// the human launched by hand over a selection — and showing that number
    /// here would be answering a different question.
    ///
    /// Returns `true` if it was its own.
    pub fn properties_sized(
        &mut self,
        task: norte_proto::TaskId,
        bytes: u64,
        entries: u64,
    ) -> bool {
        let Some(Modal::Properties {
            size_task, size, ..
        }) = &mut self.modal
        else {
            return false;
        };
        if *size_task != Some(task) {
            return false;
        }
        *size = Some((bytes, entries));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::app::dialog_action;
    use crate::app::pane::Pane;
    use crate::app::testutil::*;
    use norte_proto::{Entry, EntryKind, VPath};

    /// #139: properties come from the LISTING, and over a folder they
    /// request the one thing the listing doesn't know.
    #[test]
    fn a_folders_properties_ask_to_count_it() {
        let mut app = app_with_entries(&["a.txt"]);
        // Over a file there's nothing to count: its size is already there.
        assert!(app.open_properties().is_none());
        assert!(matches!(app.modal, Some(Modal::Properties { .. })));
    }

    /// A count's result goes to the dialog that requested it, and to NO
    /// other: between opening the dialog and the count finishing there's
    /// room for another count — one the human launched over a selection —
    /// and showing that number here would be answering a different
    /// question.
    #[test]
    fn a_foreign_count_does_not_enter_the_dialog() {
        use norte_proto::TaskId;

        let mut app = app_with_entries(&["a.txt"]);
        app.open_properties();
        let mine = TaskId::new(7);
        app.properties_counting(mine);
        assert!(
            !app.properties_sized(TaskId::new(8), 1, 1),
            "someone else's doesn't get in"
        );
        assert!(app.properties_sized(mine, 4096, 12), "mine does");
        let Some(Modal::Properties { size, .. }) = &app.modal else {
            panic!("still open")
        };
        assert_eq!(*size, Some((4096, 12)));
    }

    /// With no dialog open, a count has nowhere to go and says so: it's
    /// what makes the run loop send the number to the status bar.
    #[test]
    fn with_no_dialog_the_count_has_nowhere_to_go() {
        let mut app = app_with_entries(&["a.txt"]);
        assert!(!app.properties_sized(norte_proto::TaskId::new(1), 10, 1));
    }

    /// #138: the sort key does the same as a header click — flips if already
    /// active, sorts ascending if new — and ONLY over the focused panel: the
    /// order belongs to one listing, like the cursor.
    #[test]
    fn a_sort_key_only_touches_the_panel_with_focus() {
        use norte_frontend::{SortColumn, SortDir};

        let mut app = app_dos_panes();
        let other = app.panes[1].sort();
        app.sort_focused_by(SortColumn::Size);
        assert_eq!(app.focused().sort().column, SortColumn::Size);
        assert_eq!(
            app.focused().sort().dir,
            SortDir::Asc,
            "a new one, ascending"
        );
        assert_eq!(app.panes[1].sort(), other, "the other panel doesn't know");

        app.sort_focused_by(SortColumn::Size);
        assert_eq!(
            app.focused().sort().dir,
            SortDir::Desc,
            "the same one again flips it"
        );
        app.sort_focused_by(SortColumn::Extension);
        assert_eq!(app.focused().sort().column, SortColumn::Extension);
        assert_eq!(app.focused().sort().dir, SortDir::Asc);
    }

    /// And `dirs_first` isn't touched by any sort key: it's a preference,
    /// not a column criterion.
    #[test]
    fn a_sort_key_does_not_touch_dirs_first() {
        use norte_frontend::SortColumn;

        let mut app = app_dos_panes();
        let mut spec = app.focused().sort();
        spec.dirs_first = false;
        app.focused_mut().set_sort(spec);
        app.sort_focused_by(SortColumn::Mtime);
        assert!(!app.focused().sort().dirs_first);
    }

    /// #108 b4: `apply_scheme_sort` applies the config's order to the pane
    /// per its scheme — the cd hook and startup go through here.
    #[test]
    fn apply_scheme_sort_orders_by_the_config() {
        use norte_frontend::columns::ColumnsSettings;
        let dir = VPath::parse("mem:///").unwrap();
        let mk = |n: &str, size: Option<u64>| {
            let mut e = e(&format!("mem:///{n}"), EntryKind::File);
            e.size = size;
            e
        };
        let mut app = App::new(
            Pane::new(dir.clone(), vec![mk("a", Some(3)), mk("b", Some(1))]),
            Pane::new(dir, Vec::new()),
        );
        let cfg = norte_config::ColumnsConfig {
            default_columns: None,
            sort: Some(norte_config::SortChoice {
                column: norte_config::SortColumnKey::Size,
                descending: false,
                dirs_first: true,
            }),
            schemes: std::collections::BTreeMap::new(),
            ..Default::default()
        };
        app.columns = ColumnsSettings::resolve(&cfg);
        app.apply_scheme_sort(0);
        let order: Vec<_> = app.panes[0]
            .entries()
            .iter()
            .map(|e| e.path.clone())
            .collect();
        assert_eq!(
            order,
            vec![
                VPath::parse("mem:///b").unwrap(),
                VPath::parse("mem:///a").unwrap()
            ],
            "size asc from the config"
        );
    }

    /// #105 review MAJOR-1: submitting ONE item that came from a MARK
    /// CONSUMES it (mc/TC batch doctrine); a rename (cursor) never touches
    /// the marks, and neither does Esc.
    #[test]
    fn submitting_an_item_consumes_the_mark_and_rename_does_not() {
        let dir = VPath::parse("mem:///").unwrap();
        let mk = |n: &str| Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(norte_proto::Segment::new(n.as_bytes().to_vec()).unwrap()),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
        };
        let mut app = App::new(
            Pane::new(dir.clone(), vec![mk("a"), mk("b")]),
            Pane::new(VPath::parse("mem:///dst").unwrap(), Vec::new()),
        );
        app.focused_mut().toggle_mark(); // marks "a"
        app.focused_mut().move_down(1); // cursor on "b"
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        let (_, from, _) = app.transfer_name_confirm().expect("valid");
        assert_eq!(
            from,
            VPath::parse("mem:///a").unwrap(),
            "the MARK, not the cursor"
        );
        app.transfer_name_submitted();
        assert_eq!(app.focused().marks_len(), 0, "submitting consumes the mark");

        // Esc doesn't consume.
        app.focused_mut().toggle_mark(); // marks "b" (cursor stays there)
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        app.cancel_transfer_name();
        assert_eq!(app.focused().marks_len(), 1, "cancelling keeps the mark");

        // Rename (cursor) doesn't touch unrelated marks.
        app.open_rename();
        app.transfer_name_push('2');
        assert!(app.transfer_name_confirm().is_some());
        app.transfer_name_submitted();
        assert_eq!(app.focused().marks_len(), 1, "rename doesn't consume marks");
    }

    /// #103 T10: F5/F6 build the modal from ALL marks, and the destination is
    /// the OTHER pane's DIRECTORY (with several items there's no single name
    /// to edit — that's #105).
    #[test]
    fn copy_builds_the_modal_from_every_mark() {
        let mut app = app_with_two_panes(&["a", "b", "c"], "mem:///dst");
        app.focused_mut().mark_all();
        assert_eq!(app.focused().marks_len(), 3, "all three ended up marked");
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        let Some(Modal::ConfirmTransfer { items, to, .. }) = &app.modal else {
            panic!("no transfer modal");
        };
        assert_eq!(items.len(), 3);
        assert_eq!(to, &VPath::parse("mem:///dst").unwrap());
    }

    /// With no mark at all, F5 still operates on the CURSOR (the classic
    /// gesture isn't lost) — `marked_paths` falls back to the selected one.
    /// With ONE single item the gate opens the EDITABLE name (#105), not the
    /// list confirm: it's the same decision for the key and for a drop.
    #[test]
    fn copy_without_marks_still_uses_the_cursor_entry() {
        let mut app = app_with_two_panes(&["a", "b"], "mem:///dst");
        assert_eq!(app.focused().marks_len(), 0, "no marks to start with");
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        let Some(Modal::TransferName {
            from,
            to_dir,
            from_marks,
            ..
        }) = &app.modal
        else {
            panic!("no transfer modal");
        };
        assert_eq!(
            from,
            &VPath::parse("mem:///a").unwrap(),
            "marked_paths falls back to the cursor"
        );
        assert_eq!(to_dir, &VPath::parse("mem:///dst").unwrap());
        assert!(!from_marks, "there was no mark to consume");
    }

    /// A PROMOTED drag carries the pressed row and NOTHING else: not the
    /// pane's marks (which are something else the user hasn't let go of) nor
    /// their consumption on submit. The promotion changes what the gesture
    /// DOES, not what's selected.
    #[test]
    fn a_promoted_transfer_carries_one_row_and_does_not_consume_the_marks() {
        let mut app = app_with_two_panes(&["a", "b", "c"], "mem:///dst");
        app.focused_mut().mark_all();
        assert_eq!(app.focused().marks_len(), 3);
        app.open_transfer(TransferKind::Copy, 0, 1, Some(2));
        let Some(Modal::TransferName {
            from, from_marks, ..
        }) = &app.modal
        else {
            panic!("a single item: editable name");
        };
        assert_eq!(
            from,
            &VPath::parse("mem:///c").unwrap(),
            "the promoted row, not the three marks"
        );
        assert!(!from_marks, "submitting must NOT consume the marks");
        app.transfer_name_submitted();
        assert_eq!(app.focused().marks_len(), 3, "the marks are still there");
    }

    /// A promoted index that no longer names any row (the listing shrank
    /// between the gesture and the drop) opens nothing: never a dialog over
    /// an empty batch, and never falling back to the marks — which would be
    /// copying what nobody dragged.
    #[test]
    fn a_promoted_index_out_of_range_opens_nothing() {
        let mut app = app_with_two_panes(&["a", "b"], "mem:///dst");
        app.focused_mut().mark_all();
        app.open_transfer(TransferKind::Copy, 0, 1, Some(9));
        assert!(app.modal.is_none());
    }

    /// Marks are CONSUMED by SUBMITTING the batch (mc/Total Commander): after
    /// `consume_marks` there's no half-consumed selection left.
    #[test]
    fn submitting_a_bulk_operation_consumes_the_marks() {
        let mut app = app_with_two_panes(&["a", "b"], "mem:///dst");
        app.focused_mut().mark_all();
        assert_eq!(app.focused().marks_len(), 2, "marked before submitting");
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        app.consume_marks();
        assert_eq!(app.focused().marks_len(), 0);
    }

    /// F8 over the marks: the modal carries the whole batch and the mode
    /// (trash/permanent) the caller decided after probing the capability
    /// ONCE.
    #[test]
    fn delete_builds_the_modal_from_every_mark() {
        let mut app = app_with_two_panes(&["a", "b", "c"], "mem:///dst");
        app.focused_mut().mark_all();
        app.open_delete_modal(true);
        let Some(Modal::ConfirmDelete { items, permanent }) = &app.modal else {
            panic!("no delete modal");
        };
        assert_eq!(items.len(), 3);
        assert!(*permanent);
    }

    /// An EMPTY pane opens no modal: there's nothing to copy or delete
    /// (neither marks nor a cursor) — never a dialog over an empty batch.
    #[test]
    fn an_empty_pane_opens_no_bulk_modal() {
        let mut app = app_with_two_panes(&[], "mem:///dst");
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        assert!(app.modal.is_none(), "no items means no copy modal");
        app.open_delete_modal(false);
        assert!(app.modal.is_none(), "no items means no delete modal");
    }

    /// #103 T9 review MINOR: `Modal::MarkPattern` has no ALLOWLIST — it's
    /// free text, the run loop intercepts it BEFORE the `dialog` context
    /// (main.rs). This pins the security half of that claim: NO command from
    /// the `dialog.*` vocabulary, not even `dialog.confirm` (Enter), can
    /// confirm it through `dialog_action` — if this modal ever slipped into
    /// the `dialog` context through a routing bug, it would still be inert
    /// there.
    #[test]
    fn dialog_action_is_always_none_for_mark_pattern() {
        let m = Modal::MarkPattern {
            mark: true,
            pattern: String::new(),
            error: None,
        };
        for cmd in crate::keymap::DIALOG_COMMANDS {
            assert_eq!(
                dialog_action(&m, cmd),
                None,
                "{cmd} must not confirm/cancel MarkPattern via dialog_action"
            );
        }
    }

    /// Cancelling (`cancel_mark_pattern`, this free-text modal's Esc) marks
    /// nothing, even if the user had already typed a pattern — and closes
    /// the modal, the real property this test had to pin.
    #[test]
    fn the_pattern_modal_cancels_without_marking() {
        let mut app = app_with_entries(&["a.rs"]);
        app.open_mark_pattern(true);
        app.mark_pattern_push('*');
        app.cancel_mark_pattern();
        assert!(app.modal.is_none(), "cancel closes the modal");
        assert_eq!(app.focused().marks_len(), 0);
    }

    /// Review rust MAJOR M1: `cancel_mark_pattern` is NOT a generic close —
    /// with a `Modal::ApproveAgentOp` open (arrived, say, while the user was
    /// typing a pattern that later got replaced), it must leave it INTACT.
    /// Closing it without the async `policy.decide(approve: false)`
    /// `on_dialog_key` does would leave the agent without an answer until
    /// the daemon's TTL, and the human without ever seeing the question
    /// again.
    ///
    /// The guard is a `debug_assert!`: in THIS build (test = dev,
    /// `debug-assertions` on) it panics BEFORE touching `self.modal` — it's
    /// caught with `catch_unwind` so the state afterward can be checked in
    /// the same assertion; in release it would be a no-op and the function
    /// would return early anyway, same result on the modal.
    #[test]
    fn cancel_mark_pattern_leaves_an_approval_modal_untouched() {
        let mut app = app_with_entries(&["a.rs"]);
        let approval = Modal::ApproveAgentOp {
            req: norte_proto::methods::PolicyApprovalRequired {
                approval_id: 7,
                session: Some("s1".into()),
                op: "copy".into(),
                paths: vec!["mem:///proj/a".into(), "mem:///proj/b".into()],
                paths_total: 0,
                ttl_ms: 60_000,
                detail: norte_proto::methods::ApprovalDetail::default(),
            },
        };
        app.modal = Some(approval.clone());
        // Silences the default panic hook: the panic is caught and
        // expected, it shouldn't clutter this test's output with a
        // backtrace.
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            app.cancel_mark_pattern();
        }));
        std::panic::set_hook(prev_hook);
        assert!(result.is_err(), "the guard must panic in debug on misuse");
        assert_eq!(
            app.modal,
            Some(approval),
            "an approval modal must not be closeable without a decision"
        );
    }
}
