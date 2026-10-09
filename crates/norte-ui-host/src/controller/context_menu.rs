//! The window's right-click menu (spec 2026-10-09, bridge 107).
//!
//! Part of `controller`: these are methods of `State`. The only writer is
//! still the actor. WHICH entries a surface offers is
//! `norte_frontend::context_menu`, shared; whether each can run is the shared
//! availability table. What lives here is measuring the surface, the marks
//! rule, and the projection.

// These modules are the same `impl State` split into pieces, so they use
// the same imports as the parent. Listing them here would be a forty-line
// list per file, across 32 files, that goes out of sync the moment the
// parent imports something — `super::*` keeps it in sync on its own.
#[allow(clippy::wildcard_imports)]
use super::*;

use std::borrow::Cow;

use norte_frontend::context_menu::{Action, Entry, Surface, Verb};

/// The open right-click menu.
///
/// Everything it acts on is decided ONCE, at opening: the target, the
/// facts the verdicts are read from, and what the pane looked like. The
/// verdicts do not change under the pointer, and the fingerprint is what
/// lets an activation notice the pane moved underneath.
pub(super) struct ContextMenu {
    /// What the pointer was over, reduced for the shared model.
    pub(super) surface: Surface,
    /// The entries, in display order.
    pub(super) entries: Vec<norte_frontend::context_menu::Entry>,
    /// Frozen at open, AFTER the marks rule ran, so they describe the
    /// target and not what was there before the click.
    pub(super) facts: norte_frontend::availability::Facts,
    /// The highlighted entry.
    pub(super) cursor: usize,
    /// The slot it was opened on.
    pub(super) slot: u32,
    /// What it says it acts on, already worded and clamped.
    pub(super) header: String,
    /// Row surfaces only: what the pane pointed at and how many marks it
    /// had, to refuse running against a pane that changed underneath.
    pub(super) fingerprint: Option<Fingerprint>,
    /// The row or column it was opened on, for the verbs.
    pub(super) subject: Subject,
    /// Pixel anchor from the renderer, or `None` from the keyboard.
    pub(super) anchor: Option<(i32, i32)>,
}

/// What a row menu saw at opening.
///
/// A name and a count, not the generation: the generation moves with any
/// filler batch, a refresh that changed nothing included, and a menu that
/// went stale on every refresh would refuse a copy that targets exactly
/// what it said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Fingerprint {
    /// The pointed entry's last path component, as bytes.
    pub(super) entry: Option<Vec<u8>>,
    /// How many marks the pane had.
    pub(super) marks: usize,
}

/// What a menu-local verb acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Subject {
    /// The listing it was opened on (rows and the empty area).
    Listing,
    /// A column header, by column id.
    Column(String),
    /// A places row, by index, with the generation it was opened against.
    ///
    /// An index alone would name another row once the volumes land IN THE
    /// MIDDLE; the generation is what lets a verb refuse instead.
    Place {
        /// Its index in the bar's rows.
        row: usize,
        /// `gen_places` when it opened.
        generation: u64,
        /// A favorite whose target did not parse: its error key, frozen at
        /// opening like every other verdict. It dims the Open verbs.
        broken: Option<String>,
    },
    /// A tree branch, by index among the visible ones, with the generation
    /// it was opened against (an expansion inserts rows in the middle).
    Branch {
        /// Its index among the visible branches.
        row: usize,
        /// `gen_branches` when it opened.
        generation: u64,
    },
}

impl State {
    /// A right click on a listing row: decides the target ONCE and opens
    /// the menu on it.
    ///
    /// The marks rule is GPUI's and `mouse.md`'s: a marked row acts on the
    /// marks; an unmarked one DROPS them and puts the cursor on itself.
    /// Keeping them would let the menu say "acts on c.txt" while the copy —
    /// every command prefers the marks — took eleven. Irrecoverable, Esc
    /// included: the menu said what it acts on, and closing it does not
    /// bring back what that cost.
    pub(super) fn open_row_menu(
        &mut self,
        slot_id: u32,
        key: RowKey,
        generation: u64,
        anchor: Option<(i32, i32)>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.something_keeps_the_keys() {
            return (self.applied(), Vec::new());
        }
        let Some(i) = self.row_of(slot_id, key, generation) else {
            return (Self::stale(StaleAction::Generation), Vec::new());
        };
        // `..` is not an operand (`PaneState::selected` refuses it): a right
        // click on it is a right click on the folder.
        if self.slot().pane.is_parent_row(i) {
            return self.open_empty_menu(slot_id, anchor);
        }
        let pane = &mut self.slot_mut().pane;
        let marked = pane.entries().get(i).is_some_and(|e| pane.is_marked(e));
        if !marked {
            pane.clear_marks();
        }
        pane.set_cursor(i);
        let pane = &self.slot().pane;
        let Some(entry) = pane.entries().get(i) else {
            return (Self::stale(StaleAction::Generation), Vec::new());
        };
        let (count, all_files) = if marked {
            let marks = pane.marked_entries();
            (
                pane.marks_len(),
                marks.iter().all(|e| e.kind == EntryKind::File),
            )
        } else {
            (1, entry.kind == EntryKind::File)
        };
        let single = count == 1;
        let target = norte_frontend::context_menu::RowTarget {
            count,
            kind: single.then_some(entry.kind),
            archive: single && norte_frontend::nav::archive_root_for(entry).is_some(),
            all_files,
        };
        // ONE frame, "acts on …", for both targets: the name or the count
        // goes inside it, so the header always says it is the operand.
        let target_text = if single {
            // The SAME masking as the row's `display_name`, so the header
            // never says a name the row does not.
            let bytes = entry
                .path
                .file_name()
                .map_or(&[][..], norte_proto::Segment::as_bytes);
            let (text, _) = norte_frontend::display_name_with(bytes, pane.name_encoding());
            norte_frontend::context_menu::elide(&text)
        } else {
            norte_i18n::ta_in(
                self.lang,
                "gui-menu-target-marks",
                &[("n", &count.to_string())],
            )
        };
        let header = norte_i18n::ta_in(self.lang, "gui-menu-acts-on", &[("target", &target_text)]);
        let fingerprint = self.fingerprint_of(slot_id);
        let surface = Surface::Row(target);
        let bar = self.install_context_menu(ContextMenu {
            surface,
            entries: norte_frontend::context_menu::entries(&surface),
            facts: self.facts(),
            cursor: 0,
            slot: slot_id,
            header: clamp_display(header),
            fingerprint,
            subject: Subject::Listing,
            anchor,
        });
        // The rows go WITH the menu, in the same patch: the marks may have
        // gone and the cursor moved, and a menu painted over the old marks
        // would contradict its own header.
        let mut changes = vec![
            self.row_change(),
            self.header_of(self.active(), self.slot()),
        ];
        changes.extend(bar);
        self.context_menu_patch(changes)
    }

    /// A right click on the empty area of a listing (or on `..`): the
    /// folder's menu. Touches no marks — nothing was pointed at.
    pub(super) fn open_empty_menu(
        &mut self,
        slot_id: u32,
        anchor: Option<(i32, i32)>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.something_keeps_the_keys() {
            return (self.applied(), Vec::new());
        }
        if slot_id != self.active() {
            return (Self::stale(StaleAction::Generation), Vec::new());
        }
        let pane = &self.slot().pane;
        // The BASE scheme, the part after the last `+`: `zip+sftp` is
        // remote, `zip+file` is not — the same test `disconnect` makes
        // before refusing with `msg-disconnect-local`.
        let scheme = pane.dir().scheme();
        let base = scheme.rsplit('+').next().unwrap_or(scheme);
        let remote = base != "file";
        let (dir, _) = norte_frontend::path_display_with(pane.dir(), pane.name_encoding());
        let header = norte_i18n::ta_in(
            self.lang,
            "ctx-acts-on-dir",
            &[("dir", &norte_frontend::context_menu::elide(&dir))],
        );
        let surface = Surface::Empty { remote };
        let bar = self.install_context_menu(ContextMenu {
            surface,
            entries: norte_frontend::context_menu::entries(&surface),
            facts: self.facts(),
            cursor: 0,
            slot: slot_id,
            header: clamp_display(header),
            fingerprint: None,
            subject: Subject::Listing,
            anchor,
        });
        self.context_menu_patch(bar.into_iter().collect())
    }

    /// A right click on a column header.
    ///
    /// A column the active listing does not show is a race with an earlier
    /// header (the reader hid it, the slot narrowed), not an order: stale.
    pub(super) fn open_header_menu(
        &mut self,
        slot_id: u32,
        column: &str,
        anchor: Option<(i32, i32)>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.something_keeps_the_keys() {
            return (self.applied(), Vec::new());
        }
        if slot_id != self.active() {
            return (Self::stale(StaleAction::Generation), Vec::new());
        }
        // The label the header PAINTS — configured, or a plugin's — not the
        // factory one: the menu names the column the reader clicked.
        let Some(label) = self
            .headers(slot_id, self.slot())
            .into_iter()
            .find(|h| h.id == column)
            .map(|h| h.label)
        else {
            return (Self::stale(StaleAction::Generation), Vec::new());
        };
        let header = norte_i18n::ta_in(
            self.lang,
            "ctx-acts-on-column",
            &[("column", &norte_frontend::context_menu::elide(&label))],
        );
        let surface = Surface::Header {
            hideable: column != "name",
        };
        let bar = self.install_context_menu(ContextMenu {
            surface,
            entries: norte_frontend::context_menu::entries(&surface),
            facts: self.facts(),
            cursor: 0,
            slot: slot_id,
            header: clamp_display(header),
            fingerprint: None,
            subject: Subject::Column(column.to_owned()),
            anchor,
        });
        self.context_menu_patch(bar.into_iter().collect())
    }

    /// A right click on a places row (or `pane.context-menu` with the bar
    /// focused: `generation: None`, the current one).
    ///
    /// The bar's cursor goes to the row, as a click puts it there: the menu
    /// is about the row the reader sees highlighted. The header is the
    /// row's label as the bar paints it — masked, then elided.
    pub(super) fn open_place_menu(
        &mut self,
        row: u32,
        generation: Option<u64>,
        anchor: Option<(i32, i32)>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        use norte_frontend::context_menu::PlaceTarget;
        use norte_frontend::places::PlaceRow;
        if self.something_keeps_the_keys() {
            return (self.applied(), Vec::new());
        }
        // The SAME rejection as `activate_place`: the volumes land in the
        // middle, and `set_cursor` would clamp a vanished index onto
        // another row.
        let generation = generation.unwrap_or(self.gen_places);
        if generation != self.gen_places {
            return (Self::stale(StaleAction::Generation), Vec::new());
        }
        let (Some(SlotId(slot)), Some(state)) = (self.places_slot(), self.places.as_mut()) else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let i = row as usize;
        let Some(place) = state.rows().get(i) else {
            return (Self::stale(StaleAction::Generation), Vec::new());
        };
        let (target, label, broken) = match place {
            PlaceRow::Header { section, .. } => (
                PlaceTarget::SectionHeader,
                norte_i18n::t_in(self.lang, section.label_key()),
                None,
            ),
            PlaceRow::Drive { label, mount, .. } => (
                PlaceTarget::Drive,
                norte_frontend::places::drive_name(label, mount).0,
                None,
            ),
            PlaceRow::Favorite { name, target } => (
                PlaceTarget::Favorite,
                norte_frontend::display_name(name.as_bytes()).0,
                target.as_ref().err().cloned(),
            ),
        };
        state.set_cursor(i);
        let surface = Surface::Place(target);
        let bar = self.install_context_menu(ContextMenu {
            surface,
            entries: norte_frontend::context_menu::entries(&surface),
            facts: self.facts(),
            cursor: 0,
            slot,
            header: clamp_display(norte_frontend::context_menu::elide(&label)),
            fingerprint: None,
            subject: Subject::Place {
                row: i,
                generation,
                broken,
            },
            anchor,
        });
        // The bar's cursor moved: it travels WITH the menu.
        let mut changes = vec![ViewChange::Slot {
            slot: Box::new(crate::dto::SlotView::Places(Box::new(
                self.places_bar(slot),
            ))),
        }];
        changes.extend(bar);
        self.context_menu_patch(changes)
    }

    /// A right click on a tree branch (or `pane.context-menu` with the tree
    /// focused: `generation: None`, the current one). The header is the
    /// branch's whole display path: a bare folder name does not say which.
    pub(super) fn open_branch_menu(
        &mut self,
        row: u32,
        generation: Option<u64>,
        anchor: Option<(i32, i32)>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.something_keeps_the_keys() {
            return (self.applied(), Vec::new());
        }
        // `touch_branch`'s rejection: an expansion inserts rows in the
        // middle, so an old index names another folder.
        let generation = generation.unwrap_or(self.gen_branches);
        if generation != self.gen_branches {
            return (Self::stale(StaleAction::Generation), Vec::new());
        }
        let (Some(SlotId(slot)), Some(tree)) = (self.branches_slot(), self.branches.as_mut())
        else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let i = row as usize;
        let Some(path) = tree.rows().get(i).map(|r| r.path.clone()) else {
            return (Self::stale(StaleAction::Generation), Vec::new());
        };
        tree.set_cursor(i);
        let (shown, _) = norte_frontend::path_display(&path);
        let surface = Surface::Branch;
        let bar = self.install_context_menu(ContextMenu {
            surface,
            entries: norte_frontend::context_menu::entries(&surface),
            facts: self.facts(),
            cursor: 0,
            slot,
            header: clamp_display(norte_frontend::context_menu::elide(&shown)),
            fingerprint: None,
            subject: Subject::Branch { row: i, generation },
            anchor,
        });
        let mut changes = vec![ViewChange::Slot {
            slot: Box::new(crate::dto::SlotView::Tree(Box::new(self.branch_tree(slot)))),
        }];
        changes.extend(bar);
        self.context_menu_patch(changes)
    }

    /// The mouse hovering over an entry: moves the cursor and nothing else.
    /// A row outside the menu is a race with an earlier one: stale.
    pub(super) fn point_in_context_menu(
        &mut self,
        row: u32,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(m) = self.context_menu.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let i = row as usize;
        if i >= m.entries.len() {
            return (Self::stale(StaleAction::Generation), Vec::new());
        }
        m.cursor = i;
        self.context_menu_patch(Vec::new())
    }

    /// Closes the menu without running anything. The marks it dropped on
    /// opening stay dropped.
    pub(super) fn close_context_menu(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.context_menu.is_none() {
            return (self.applied(), Vec::new());
        }
        self.forget_context_menu();
        self.context_menu_patch(Vec::new())
    }

    /// Drops it with no patch (for paths that already send one).
    pub(super) fn forget_context_menu(&mut self) {
        self.context_menu = None;
    }

    /// The menu's projection, or `None` when none is open.
    ///
    /// The list never changes with the verdicts: an entry that cannot run
    /// shows dimmed with its reason, so the core stays the same on every
    /// row and the hand learns it once.
    pub(super) fn vista_context_menu(&self) -> Option<crate::dto::ContextMenuView> {
        let m = self.context_menu.as_ref()?;
        let items = m
            .entries
            .iter()
            .map(|e| {
                let reason = self.reason_against(m, e);
                let (chord, role) = match e.action {
                    Action::Command(c) => (
                        norte_frontend::palette::first_chord(c, &self.effective)
                            .unwrap_or_default(),
                        norte_frontend::menu::role(c).as_str(),
                    ),
                    Action::Verb(_) => (String::new(), "normal"),
                };
                crate::dto::ContextItemView {
                    label: clamp_display(norte_i18n::t_in(self.lang, e.label_key)),
                    chord: clamp_display(chord),
                    enabled: reason.is_none(),
                    reason: reason.map_or_else(String::new, |k| {
                        clamp_display(norte_i18n::t_in(self.lang, &k))
                    }),
                    section: e.section.map(|k| {
                        if k.is_empty() {
                            String::new()
                        } else {
                            clamp_display(norte_i18n::t_in(self.lang, k))
                        }
                    }),
                    role: role.to_owned(),
                }
            })
            .collect();
        Some(crate::dto::ContextMenuView {
            header: m.header.clone(),
            items,
            cursor: m.cursor as u64,
            x: m.anchor.map(|(x, _)| x),
            y: m.anchor.map(|(_, y)| y),
        })
    }

    /// Why entry `e` of `m` cannot run, as a Fluent reason key, or `None`
    /// when it can.
    ///
    /// ONE function for the two questions — what the menu PAINTS dimmed and
    /// what a click REFUSES — so the two cannot disagree: an entry painted
    /// enabled that refused, or the reverse, would be a menu lying about
    /// itself. The criteria are the shared table's verdict on the facts
    /// frozen at opening, plus "this window does not run it"; there is no
    /// third.
    ///
    /// The verbs have exactly two vetoes, both decided at opening: hiding a
    /// column that cannot be hidden (`name`), and the Open verbs on a
    /// favorite whose target did not parse — reason, the config's own error
    /// key, the one the bar already paints as the row's "broken" note.
    fn reason_against(&self, m: &ContextMenu, e: &Entry) -> Option<Cow<'static, str>> {
        match e.action {
            Action::Command(c) => {
                if crate::commands::all_with(self.effects).contains(&c) {
                    norte_frontend::availability::verdict(c, &m.facts)
                        .reason()
                        .map(|r| Cow::Borrowed(norte_frontend::availability::reason_key(r)))
                } else {
                    Some(Cow::Borrowed("reason-unavailable"))
                }
            }
            Action::Verb(Verb::HideColumn) => (m.surface == (Surface::Header { hideable: false }))
                .then_some(Cow::Borrowed("reason-wrong-target")),
            Action::Verb(Verb::OpenHere | Verb::OpenInOther | Verb::OpenInNewTab) => {
                match &m.subject {
                    Subject::Place {
                        broken: Some(key), ..
                    } => Some(Cow::Owned(key.clone())),
                    _ => None,
                }
            }
            Action::Verb(_) => None,
        }
    }

    /// What a row menu on `slot` would remember if it opened NOW: the
    /// cursor entry's last component and the marks count. `None` for a slot
    /// that is not a listing.
    fn fingerprint_of(&self, slot: u32) -> Option<Fingerprint> {
        let pane = &self.slots.get(&slot)?.pane;
        Some(Fingerprint {
            entry: pane
                .cursor_entry()
                .and_then(|e| e.path.file_name())
                .map(|s| s.as_bytes().to_vec()),
            marks: pane.marks_len(),
        })
    }

    /// A click (or `Enter`) on entry `row`: runs it, through the SAME
    /// dispatch as its key.
    ///
    /// Resolved against the HOST's entries, never the renderer's: a row
    /// outside the menu is a race with an earlier one. A disabled entry
    /// answers applied, runs nothing and leaves the menu up — the reader
    /// pointed at a reason, and taking the menu away would take the reason
    /// with it.
    ///
    /// A row menu checks its fingerprint first. Every command acts on what
    /// the pane points at NOW, and the menu SAID what it acts on when it
    /// opened: if a copy finished and re-listed, or a mark went on under
    /// the pointer, running would act on something the header never named.
    /// So it closes, answers `Stale` and runs nothing; the reader opens it
    /// again and reads the new target.
    ///
    /// A places or tree menu has the same contract through its generation:
    /// a bar or a tree that moved underneath (volumes landed, a branch
    /// folded) means the index names another row.
    pub(super) fn activate_context_menu(
        &mut self,
        row: u32,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(m) = self.context_menu.as_ref() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let Some(&entry) = m.entries.get(row as usize) else {
            return (Self::stale(StaleAction::Generation), Vec::new());
        };
        if self.reason_against(m, &entry).is_some() {
            return (self.applied(), Vec::new());
        }
        // The listing it was opened on has to be the one commands act on:
        // `apply_effect` runs over the ACTIVE slot.
        let moved =
            matches!(m.subject, Subject::Listing | Subject::Column(_)) && m.slot != self.active();
        let changed = m
            .fingerprint
            .as_ref()
            .is_some_and(|f| self.fingerprint_of(m.slot).as_ref() != Some(f));
        let vanished = match m.subject {
            Subject::Place { generation, .. } => generation != self.gen_places,
            Subject::Branch { generation, .. } => generation != self.gen_branches,
            Subject::Listing | Subject::Column(_) => false,
        };
        if moved || changed || vanished {
            let (_, closing) = self.close_context_menu();
            return (Self::stale(StaleAction::Generation), closing);
        }
        self.run_context_entry(entry, backend, mailbox)
    }

    /// Closes the menu and runs `entry`.
    ///
    /// The close travels in its OWN patch and BEFORE the effect, in
    /// `run_from_menu`'s order and for its reason: the command may open a
    /// dialog, and a menu still up behind it would keep the keys the
    /// dialog needs. A catalogue command goes through `effect_of(c, 1)` →
    /// `apply_effect`, the one dispatcher; the menu is another door into
    /// the catalogue, not a second dispatcher.
    pub(super) fn run_context_entry(
        &mut self,
        entry: Entry,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let subject = self.context_menu.as_ref().map(|m| m.subject.clone());
        self.forget_context_menu();
        let closing = self.parche(vec![ViewChange::ContextMenu { context_menu: None }]);
        let (ack, mut rest) = match (entry.action, subject) {
            (Action::Command(c), _) => match effect_of(c, 1) {
                Some(effect) => self.apply_effect(effect, backend, mailbox),
                None => self.no_implemented(c),
            },
            (Action::Verb(v), Some(subject)) => self.run_verb(v, subject, backend, mailbox),
            (Action::Verb(_), None) => (Self::stale(StaleAction::Modal), Vec::new()),
        };
        let mut outgoing = vec![closing];
        outgoing.append(&mut rest);
        (ack, outgoing)
    }

    /// Runs a menu-local verb on what the menu was opened on.
    ///
    /// Every verb is a door into a path the window already has — the
    /// click on a place or a branch, the transfer's destination, the tab
    /// button, `pane.copy-path`'s clipboard, the favorite dialog and its
    /// removal, the twisty, the header's sort and the column selector — so
    /// a verb cannot do something its gesture does not.
    ///
    /// The subject's row is resolved NOW, against the generation it was
    /// opened with: a row that vanished is `Stale` and nothing runs (the
    /// caller already checked; the paths that re-check do so for their own
    /// callers).
    pub(super) fn run_verb(
        &mut self,
        verb: Verb,
        subject: Subject,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let index = |row: usize| u32::try_from(row).unwrap_or(u32::MAX);
        match (verb, subject) {
            (Verb::SortByColumn, Subject::Column(c)) => self.sort_by(self.active(), &c),
            (Verb::HideColumn, Subject::Column(c)) => self.hide_column(&c, backend, mailbox),
            // What a click on the row does: `activate_place` /
            // `TreeActivateRow`, generation check included.
            (
                Verb::OpenHere,
                Subject::Place {
                    row, generation, ..
                },
            ) => self.activate_place(index(row), generation, backend, mailbox),
            (Verb::OpenHere, Subject::Branch { row, generation }) => {
                self.touch_branch(index(row), generation, true, backend, mailbox)
            }
            // The twisty: `TreeToggleRow`, and a places section's fold.
            (Verb::ToggleFold, Subject::Branch { row, generation }) => {
                self.touch_branch(index(row), generation, false, backend, mailbox)
            }
            (
                Verb::ToggleFold,
                Subject::Place {
                    row, generation, ..
                },
            ) => match self.places.as_mut() {
                Some(state) if generation == self.gen_places && row < state.rows().len() => {
                    state.set_cursor(row);
                    self.fold_place_at_cursor(backend, mailbox)
                }
                _ => (Self::stale(StaleAction::Generation), Vec::new()),
            },
            (
                Verb::RemoveFavorite,
                Subject::Place {
                    row, generation, ..
                },
            ) => {
                // The RAW name, never the painted label: the label is
                // masked, and the file is keyed by what the user typed.
                let name = self
                    .places
                    .as_ref()
                    .filter(|_| generation == self.gen_places)
                    .and_then(|s| match s.rows().get(row) {
                        Some(norte_frontend::places::PlaceRow::Favorite { name, .. }) => {
                            Some(name.clone())
                        }
                        _ => None,
                    });
                match name {
                    Some(name) => self.remove_favorite_named(name, mailbox),
                    None => (Self::stale(StaleAction::Generation), Vec::new()),
                }
            }
            (
                Verb::OpenInOther | Verb::OpenInNewTab | Verb::CopyPath | Verb::AddFavorite,
                Subject::Place {
                    broken: Some(key), ..
                },
            ) => {
                // A favorite that leads nowhere has no path to give: said
                // with the bar's own reason, not a silent nothing.
                let said = self.say(&key);
                (ActionAck::Unavailable { reason_key: key }, said)
            }
            (
                v @ (Verb::OpenInOther | Verb::OpenInNewTab | Verb::CopyPath | Verb::AddFavorite),
                subject @ (Subject::Place { .. } | Subject::Branch { .. }),
            ) => {
                let Some(path) = self.subject_path(&subject) else {
                    return (Self::stale(StaleAction::Generation), Vec::new());
                };
                self.run_path_verb(v, &path, backend, mailbox)
            }
            // The shared model never offers these pairs.
            _ => self.no_implemented("context-menu verb"),
        }
    }

    /// The four verbs that act on a place's or a branch's PATH.
    fn run_path_verb(
        &mut self,
        verb: Verb,
        path: &VPath,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match verb {
            // The transfer's "other pane", same rule and same refusals: two
            // ways of deciding which one it is would be two to drift apart.
            Verb::OpenInOther => match self.slot_dest() {
                Ok(dest) => (
                    self.applied(),
                    self.navigate_slot(dest, path, Trail::Record, backend, mailbox),
                ),
                Err(key) => (
                    ActionAck::Unavailable {
                        reason_key: key.to_owned(),
                    },
                    self.say(key),
                ),
            },
            // The tab button's path, then the new tab — the active slot
            // once `tab_new` returns — goes there. From the bar or the tree
            // the keys go back to the listing first, as a layout button
            // does: a new tab is a LISTING's, never the dock's.
            Verb::OpenInNewTab => {
                let refocused = !self.slots.contains_key(&self.focused());
                if refocused {
                    let listing = self.active();
                    self.roles.set(RoleId::Active, SlotId(listing));
                    self.reconciles_roles();
                }
                let (ack, mut outgoing) = self.tab_new(backend, mailbox);
                if refocused {
                    let change = ViewChange::Layout(self.layout());
                    outgoing.insert(0, self.parche(vec![change]));
                }
                if matches!(ack, ActionAck::Applied { .. }) {
                    outgoing.extend(self.navigate(path, Trail::Record, backend, mailbox));
                }
                (ack, outgoing)
            }
            // `pane.copy-path`'s bytes and message, for one path.
            Verb::CopyPath => {
                let bytes = norte_frontend::shell::clipboard_bytes(std::slice::from_ref(path));
                if !self.native(crate::dto::NativeEffect::CopyBytes { bytes, count: 1 }) {
                    return Self::without_desktop();
                }
                let said = self.say_with("msg-paths-copied", &[("n", "1")]);
                (self.applied(), said)
            }
            Verb::AddFavorite => self.request_favorite_of(path.clone()),
            _ => self.no_implemented("context-menu verb"),
        }
    }

    /// Where a place or a branch subject leads NOW, or `None` when its row
    /// vanished (the generation moved) or leads nowhere (a header).
    fn subject_path(&self, subject: &Subject) -> Option<VPath> {
        use norte_frontend::places::PlaceRow;
        match subject {
            Subject::Place {
                row, generation, ..
            } if *generation == self.gen_places => match self.places.as_ref()?.rows().get(*row)? {
                PlaceRow::Drive { mount, .. } => Some(mount.clone()),
                PlaceRow::Favorite { target, .. } => target.as_ref().ok().cloned(),
                PlaceRow::Header { .. } => None,
            },
            Subject::Branch { row, generation } if *generation == self.gen_branches => self
                .branches
                .as_ref()?
                .rows()
                .get(*row)
                .map(|r| r.path.clone()),
            _ => None,
        }
    }

    /// The keys while the menu is open.
    ///
    /// FIXED, like the menu bar's and for the same reason: there are no
    /// catalogue verbs for "next menu entry". The arrows walk with wrap —
    /// disabled entries included, so their reason can be read — `Enter`
    /// runs and `Escape` closes; anything else is swallowed instead of
    /// falling through to the listing underneath, which would be acted on
    /// for a screen the reader is not looking at.
    pub(super) fn key_in_context_menu(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(m) = self.context_menu.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let len = m.entries.len();
        match k.key.as_str() {
            "Escape" | "esc" => self.close_context_menu(),
            "ArrowUp" | "up" if len > 0 => {
                m.cursor = (m.cursor + len - 1) % len;
                self.context_menu_patch(Vec::new())
            }
            "ArrowDown" | "down" if len > 0 => {
                m.cursor = (m.cursor + 1) % len;
                self.context_menu_patch(Vec::new())
            }
            "Enter" | "enter" => {
                let row = u32::try_from(m.cursor).unwrap_or(u32::MAX);
                self.activate_context_menu(row, backend, mailbox)
            }
            _ => (self.applied(), Vec::new()),
        }
    }

    /// `pane.context-menu` (Shift+F10, the Menu key): the menu on whatever
    /// has the keyboard, with no pixel anchor — the renderer places it at
    /// the focused row.
    ///
    /// On a listing it is EXACTLY a right click on the cursor row, marks
    /// rule included: an unmarked cursor row drops the marks. A key that
    /// opened a menu saying "acts on c.txt" over eleven marks would be the
    /// same lie the rule exists to prevent. An empty listing opens the
    /// folder's menu. With the places bar or the tree focused, it opens on
    /// their cursor row, exactly as a right click there would.
    pub(super) fn open_context_menu_on_focus(
        &mut self,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // The bar or the tree with the keys: their cursor row, at the
        // generation painted now.
        if self.places_have_focus() {
            let row = self
                .places
                .as_ref()
                .map_or(0, norte_frontend::places::PlacesState::cursor);
            return self.open_place_menu(u32::try_from(row).unwrap_or(u32::MAX), None, None);
        }
        if self.branches_have_focus() {
            let row = self
                .branches
                .as_ref()
                .map_or(0, norte_frontend::tree::Tree::cursor);
            return self.open_branch_menu(u32::try_from(row).unwrap_or(u32::MAX), None, None);
        }
        let slot = self.active();
        let pane = &self.slot().pane;
        if pane.entries().is_empty() {
            return self.open_empty_menu(slot, None);
        }
        let key = RowKey(pane.cursor() as u64);
        let generation = pane.listing_epoch();
        self.open_row_menu(slot, key, generation, None)
    }

    /// Whether the open menu has to go because something took the screen
    /// from it: a surface that keeps the keys (a dialog, help, the
    /// palette…), the menu bar's dropdown, or the listing it was opened on
    /// went away.
    ///
    /// Asked in ONE place, [`State::over`], on every patch and snapshot that
    /// crosses: the dialogs alone are pushed from two dozen places, some of
    /// them answers that land long after the gesture (a drop, a conflict,
    /// a plugin), and a close wired into each opener would be the one the
    /// next opener forgets — the same reasoning that derives the panel bar
    /// there.
    pub(super) fn context_menu_lost_the_screen(&self) -> bool {
        self.context_menu.as_ref().is_some_and(|m| {
            self.something_keeps_the_keys()
                || self.menu.is_some()
                || match m.subject {
                    Subject::Listing | Subject::Column(_) => !self.slots.contains_key(&m.slot),
                    Subject::Place { .. } => self.places_slot().is_none(),
                    Subject::Branch { .. } => self.branches_slot().is_none(),
                }
        })
    }

    /// Puts `menu` up, closing the menu bar's dropdown first: two menus
    /// open at once would fight over the keyboard. Answers the bar's change
    /// when it closed one, for the patch that carries the menu.
    fn install_context_menu(&mut self, menu: ContextMenu) -> Option<ViewChange> {
        let bar = self.menu.is_some().then(|| {
            self.forget_menu();
            ViewChange::Menu {
                menu: self.vista_menu(),
            }
        });
        self.context_menu = Some(menu);
        bar
    }

    /// The patch that carries the menu as it is now, behind whatever else
    /// went with it (the bar's dropdown that opening closed).
    fn context_menu_patch(
        &mut self,
        mut changes: Vec<ViewChange>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        changes.push(ViewChange::ContextMenu {
            context_menu: self.vista_context_menu(),
        });
        (self.applied(), vec![self.parche(changes)])
    }
}
