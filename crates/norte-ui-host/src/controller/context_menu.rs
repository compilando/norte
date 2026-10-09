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
    /// A places row, by index (Task 5).
    #[expect(dead_code, reason = "opened in Task 5")]
    Place(usize),
    /// A tree branch, by index (Task 5).
    #[expect(dead_code, reason = "opened in Task 5")]
    Branch(usize),
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
                        clamp_display(norte_i18n::t_in(self.lang, k))
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
    fn reason_against(&self, m: &ContextMenu, e: &Entry) -> Option<&'static str> {
        match e.action {
            Action::Command(c) => {
                if crate::commands::all_with(self.effects).contains(&c) {
                    norte_frontend::availability::verdict(c, &m.facts)
                        .reason()
                        .map(norte_frontend::availability::reason_key)
                } else {
                    Some("reason-unavailable")
                }
            }
            Action::Verb(v) => (v == Verb::HideColumn
                && m.surface == (Surface::Header { hideable: false }))
            .then_some("reason-wrong-target"),
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
        if moved || changed {
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
        self.forget_context_menu();
        let closing = self.parche(vec![ViewChange::ContextMenu { context_menu: None }]);
        let (ack, mut rest) = match entry.action {
            Action::Command(c) => match effect_of(c, 1) {
                Some(effect) => self.apply_effect(effect, backend, mailbox),
                None => self.no_implemented(c),
            },
            // Task 5: the menu-local verbs (open here / in the other pane /
            // in a tab, copy path, favorites, fold, sort, hide column).
            Action::Verb(_) => self.no_implemented("context-menu verb"),
        };
        let mut outgoing = vec![closing];
        outgoing.append(&mut rest);
        (ack, outgoing)
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
    /// folder's menu.
    pub(super) fn open_context_menu_on_focus(
        &mut self,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Task 5: the Places cursor row (`places_have_focus()`) and the
        // tree's cursor branch open their own menus here. Until then they
        // fall through to the active listing.
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
                || (matches!(m.subject, Subject::Listing | Subject::Column(_))
                    && !self.slots.contains_key(&m.slot))
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
