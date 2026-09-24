//! Panel gestures: mirror, pull, swap, jump and back.
//!
//! Part of `controller`: these are methods of `State`, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl State` split into pieces, so they use
// the same imports as the parent. Listing them here would be a forty-line
// list per file, across 32 files, that goes out of sync the moment the
// parent imports something — `super::*` keeps it in sync on its own.
#[allow(clippy::wildcard_imports)]
use super::*;

impl State {
    /// The three panel gestures from ADR 0058: mirror, pull, and swap.
    ///
    /// All three need the OTHER slot, and the other slot is decided by the
    /// shared role — the same one a copy's destination comes from — never "the
    /// one next to it": with three listings, guessing is sending someone's
    /// panel somewhere they did not choose.
    // TODO(translation): review — this paragraph documents the three panel
    /// gestures, but the item right after it is
    /// `toggle_mirror_permanent`'s own doc, about the sync-navigation
    /// toggle; it looks like a stale fragment left by an earlier edit.
    /// Turns SYNCHRONIZED navigation on or off, and says so.
    ///
    /// It does not navigate: turning it on does not move the other slot to
    /// where you already are. What it does is make the NEXT navigation be
    /// repeated by both — aligning them right now already has its own
    /// gesture, which is `pane.mirror`.
    pub(super) fn toggle_mirror_permanent(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.mirror_permanent = !self.mirror_permanent;
        let key = if self.mirror_permanent {
            "msg-sync-nav-on"
        } else {
            "msg-sync-nav-off"
        };
        self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, key)));
        let change = ViewChange::Status(self.status.clone());
        (self.applied(), vec![self.parche(vec![change])])
    }

    pub(super) fn pane_gesture(
        &mut self,
        effect: Effect,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let other = match self.slot_dest() {
            Ok(id) => id,
            Err(key) => {
                return (
                    ActionAck::Unavailable {
                        reason_key: key.to_owned(),
                    },
                    Vec::new(),
                );
            }
        };
        let active = self.active();
        match effect {
            // What travels is WHERE the source panel is GOING, not what it is
            // showing: during a navigation `pane.dir()` still answers with
            // the directory being left, and mirroring that would send the
            // other panel to the place the reader just left.
            Effect::Mirror | Effect::MirrorObjetivo | Effect::Bring => {
                let (source, arrives) = if matches!(effect, Effect::Bring) {
                    (other, active)
                } else {
                    (active, other)
                };
                let Some(target) = self.gesture_destination(effect, source) else {
                    return (Self::stale(StaleAction::Generation), Vec::new());
                };
                if self.current_dir(arrives).as_ref() == Some(&target) {
                    // Both are already there. A redundant cd RE-LISTS the
                    // arriving panel: `set_listing` erases its marks and a
                    // navigation — unlike a refresh — does not restore them,
                    // on top of sliding its listing under the cursor. All of
                    // that for nothing, because it already shows what is
                    // being asked. The TUI refuses for the same reason
                    // (`gestures::mirror_plan`).
                    return (self.applied(), Vec::new());
                }
                (
                    self.applied(),
                    self.navigate_slot(arrives, &target, Trail::Record, backend, mailbox),
                )
            }
            Effect::Swap => self.swap_slots(active, other, backend, mailbox),
            // The `match` above sends nothing else here.
            _ => Self::no_mutates(),
        }
    }

    /// The location that TRAVELS in a panel gesture, read from slot `source`.
    ///
    /// For mirror and pull it is [`Self::current_dir`]. For
    /// [`Effect::MirrorObjetivo`] it is the folder under the cursor if it is
    /// one (`PaneState::target_dir`, the same answer the TUI gives) — except
    /// with a navigation IN FLIGHT, where the cursor is still the abandoned
    /// listing's and what matters is where the slot is going.
    pub(super) fn gesture_destination(&self, effect: Effect, source: u32) -> Option<VPath> {
        let slot = self.slots.get(&source)?;
        if matches!(effect, Effect::MirrorObjetivo) && slot.dir_requested.is_none() {
            return Some(slot.pane.target_dir().clone());
        }
        self.current_dir(source)
    }

    /// Where a slot is going: the requested directory if a navigation is in
    /// flight, and the one it shows if not.
    ///
    /// `None` only if the slot does not exist, which for the caller is a
    /// screen that changed underneath.
    pub(super) fn current_dir(&self, slot: u32) -> Option<VPath> {
        let h = self.slots.get(&slot)?;
        Some(
            h.dir_requested
                .clone()
                .unwrap_or_else(|| h.pane.dir().clone()),
        )
    }

    /// The two listings change places. It does NOT touch disk.
    ///
    /// What is swapped is the slot's CONTENT — listing, cursor, marks, trail
    /// and sort order —, because splitting it further would be inventing
    /// rules about what stays where. Focus does not move: whoever had it
    /// keeps it, and now it shows the other one, which is what the gesture
    /// means.
    ///
    /// The only thing that does NOT travel is the paint window
    /// (`first_visible` and `visible`): that is the SLOT's geometry, not
    /// the listing's.
    ///
    /// What was IN FLIGHT is the part that is not visible. A response travels
    /// tagged with its slot, so after the swap it would arrive at the wrong
    /// slot and get discarded by token: the panel would be left loading
    /// forever. It is requested again, pointing at where it was going. Same
    /// with probing and decoration, which match by PATH and so would not
    /// paint anything wrong, but would leave the "already requested" memory
    /// over a listing that is no longer there — i.e. blank size columns
    /// forever.
    pub(super) fn swap_slots(
        &mut self,
        a: u32,
        b: u32,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if !self.slots.contains_key(&a) || !self.slots.contains_key(&b) {
            // One of the two disappeared between the role and here. It is
            // checked BEFORE taking either one out: a tuple's two `remove`
            // calls both evaluate before the pattern is matched, so leaving
            // through the error path with one already taken out DROPS it —
            // "what was done is undone" undid nothing.
            return (Self::stale(StaleAction::Generation), Vec::new());
        }
        let (Some(mut slot_a), Some(mut slot_b)) = (self.slots.remove(&a), self.slots.remove(&b))
        else {
            return (Self::stale(StaleAction::Generation), Vec::new());
        };
        // The paint window stays in ITS slot. `first_visible` and
        // `visible` do not describe the listing: the renderer sets them with
        // `set_visible_range`, and its `scrollTop` is its own — a swap does
        // not move it nor fire a scroll event that would recompute it. If
        // they travelled with the slot, each panel would paint rows from a
        // band the reader does not have in front of them, and BOTH would look
        // empty, with nothing to fix it short of dragging the scrollbar by
        // hand.
        std::mem::swap(&mut slot_a.first_visible, &mut slot_b.first_visible);
        std::mem::swap(&mut slot_a.visible, &mut slot_b.visible);
        self.slots.insert(a, slot_b);
        self.slots.insert(b, slot_a);
        for slot in [a, b] {
            if let Some(h) = self.slots.get_mut(&slot) {
                h.cancel_probe
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                h.cancel_probe = std::sync::Arc::default();
                h.sondeando = false;
                h.sondeados.clear();
                h.olvidar_adornos();
                h.adornando = false;
            }
            self.reanudar_request(slot, backend, mailbox);
        }
        // Both halves of the screen change at once — rows, headers, path,
        // cursor and state — so a SNAPSHOT travels and not six patches.
        let snap = self.snapshot();
        (
            self.applied(),
            vec![self.over(UiUpdate::Snapshot(Box::new(snap)))],
        )
    }

    /// Requests again whatever this slot had in flight, with a new token.
    ///
    /// "In flight" is TWO things, and looking at only the first let the
    /// common case slip through. `in_flight` clears as soon as the first page
    /// lands, while `drenando` keeps bringing the rest of the stream: in a
    /// directory with more than `FIRST_PAGE` entries — i.e. almost any of
    /// them — there is a window where only the drain is alive. Batches that
    /// kept arriving would be discarded by token (they do not cross slots,
    /// that part is fine), and the listing would be left frozen at the first
    /// hundred entries, in `Ready`, saying nothing: marking everything would
    /// act on that chunk.
    ///
    /// The two cases are re-requested differently:
    ///
    /// - **Navigation**: keeps the old request's TARGET, not the directory it
    ///   was leaving.
    /// - **Drain only**: the first page is already on screen, so this is a
    ///   REFRESH of what the reader is looking at — cursor and marks come
    ///   back, with the same discipline as [`Self::refresh`].
    ///
    /// With neither of the two it does nothing, and spends no token.
    pub(super) fn reanudar_request(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        let Some(h) = self.slots.get(&slot) else {
            return;
        };
        let navigating = h.in_flight.is_some();
        if !navigating && h.drenando.is_none() {
            return;
        }
        self.token += 1;
        let token = RequestToken(self.token);
        let Some(h) = self.slots.get_mut(&slot) else {
            return;
        };
        let dir = if navigating {
            h.dir_requested
                .clone()
                .unwrap_or_else(|| h.pane.dir().clone())
        } else {
            // The slot is ALREADY in its directory: what was missing was the
            // rest.
            h.pane.dir().clone()
        };
        if !navigating {
            if let Some(sel) = h.pane.selected().map(|e| e.path.clone()) {
                h.pane.set_pending_focus(sel);
            }
            h.pane.remember_cursor();
            // `marked_paths` falls back to the cursor with no marks, and
            // restoring THAT would be a mark nobody made.
            h.marks_to_restore = if h.pane.marks_len() > 0 {
                h.pane.marked_paths()
            } else {
                Vec::new()
            };
        }
        h.in_flight = Some(token);
        h.drenando = Some(token);
        // WITH the target when there is one. This function documents three
        // lines up that it keeps the old request's target, and then it used
        // to throw it away: swapping two panels while one navigates degraded
        // "going to X" to "loading…" over a body that still shows the
        // PREVIOUS directory — the unreadable mix this exists to avoid.
        let encoding = h.pane.name_encoding();
        h.state = Self::loading_toward(navigating.then_some(&dir), encoding);
        self.request_listing(slot, &dir, token, backend, mailbox);
    }

    /// Opens the active slot's history.
    pub(super) fn open_history(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let slot = self.active();
        self.open_history_list(slot, "picker-history-title", false, None, None)
    }

    /// Opens a SIDE of the screen's history (spec 2026-09-15 D7): what is
    /// chosen navigates THAT slot even if focus is on the other one. What a
    /// side is, is decided by the layout's geometry, like with volumes.
    pub(super) fn open_side_history(
        &mut self,
        right: bool,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(slot) = self.side_listing(right) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-other-slot".to_owned(),
                },
                Vec::new(),
            );
        };
        let title = if right {
            "picker-history-title-right"
        } else {
            "picker-history-title-left"
        };
        self.open_history_list(slot, title, false, None, None)
    }

    /// Opens the session's popular list (D6), navigating the active slot.
    pub(super) fn open_popular(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let slot = self.active();
        self.open_history_list(slot, "picker-popular-title", true, None, None)
    }

    /// Opens — or rebuilds, with `cursor` — a history list over `slot`.
    ///
    /// The rows are the SHARED ones (`norte_frontend::history`): what a panel
    /// remembers, in what order and with what mark cannot depend on who
    /// paints it. The `[ui] history_size` cap is applied here and while
    /// navigating, which are the two places where history is read or grows.
    fn open_history_list(
        &mut self,
        slot: u32,
        title: &'static str,
        popular: bool,
        cursor: Option<usize>,
        filter: Option<String>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let cap = self.config.common.ui_chrome.history_size();
        let Some(target_slot) = self.slots.get_mut(&slot) else {
            // The slot left with the list open: it closes, like when the same
            // thing happens on choosing. Leaving it open offered rows from a
            // panel that no longer exists.
            let closed = self.selector.take().is_some();
            let outgoing = if closed {
                vec![self.parche(vec![ViewChange::Picker { picker: None }])]
            } else {
                Vec::new()
            };
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-other-slot".to_owned(),
                },
                outgoing,
            );
        };
        target_slot.history.set_capacity(cap);
        let current = target_slot.pane.dir().clone();
        // History is painted with ITS panel's reinterpretation, like the
        // terminal and the path bar (#98/F4: a list is a decision surface).
        // The popular list gets none: it belongs to the whole session, and
        // applying one panel's encoding to another's paths would be
        // inventing mojibake.
        let encoding = if popular {
            None
        } else {
            target_slot.pane.name_encoding()
        };
        let query_text = filter.as_deref().unwrap_or("");
        let rows = if popular {
            norte_frontend::history::popular_rows(&self.popular, &current, query_text)
        } else {
            norte_frontend::history::history_rows(
                &target_slot.history,
                &current,
                query_text,
                encoding,
            )
        };
        let mut selector = crate::pickers::Selector::history(
            slot,
            &rows,
            |p| norte_frontend::path_display_with(p, encoding),
            self.lang,
            title,
            popular,
            filter,
        );
        if let Some(c) = cursor {
            selector.point_at(c.min(rows.len().saturating_sub(1)));
        }
        self.selector = Some(selector);
        self.gen_selector += 1;
        let change = ViewChange::Picker {
            picker: self.vista_selector(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// `dialog.remove` over a history list (D2): the cursor's row leaves the
    /// slot's trail — or the popular list — and the list is rebuilt without
    /// losing its place.
    pub(super) fn remove_from_history(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(s) = self.selector.as_ref() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let (slot, title, popular, cursor) = (s.slot(), s.title(), s.es_popular(), s.cursor());
        let filter = s.filter().map(str::to_owned);
        let Some(target) = s.choose() else {
            return (self.applied(), Vec::new());
        };
        if popular {
            self.popular.remove(&target);
        } else if let Some(h) = self.slots.get_mut(&slot) {
            // The "here" row is not removed: the list always puts it there,
            // so removing it would not remove it from the screen, and
            // `History::remove` WOULD prune the current directory and its
            // jump point from the trail without it showing (rust-reviewer,
            // phase 1).
            if *h.pane.dir() == target {
                return (self.applied(), Vec::new());
            }
            h.history.remove(&target);
        }
        self.open_history_list(slot, title, popular, Some(cursor), filter)
    }

    /// `dialog.clear` over a history list (D2). No confirmation, like in the
    /// terminal: it is navigation memory, not files, and the notice says so.
    pub(super) fn clear_history(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(s) = self.selector.as_ref() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let (slot, title, popular) = (s.slot(), s.title(), s.es_popular());
        let key = if popular {
            self.popular.clear();
            "msg-popular-cleared"
        } else {
            if let Some(h) = self.slots.get_mut(&slot) {
                h.history.clear();
            }
            "msg-history-cleared"
        };
        let (ack, mut outgoing) = self.open_history_list(slot, title, popular, Some(0), None);
        outgoing.extend(self.say(key));
        (ack, outgoing)
    }

    /// `dialog.confirm-other` (D2): what is chosen goes to the OTHER slot and
    /// focus stays where it is. The other one for a list with focus is the
    /// target; for a list on a side that does not have focus, it is the
    /// focused one.
    pub(super) fn choose_from_selector_into_another(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(s) = self.selector.as_ref() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let (from, target, has_row) = (s.slot(), s.choose(), s.hay_row());
        let other = if from == self.active() {
            self.slot_dest()
        } else {
            Ok(self.active())
        };
        let other = match other {
            Ok(other) => other,
            Err(key) => {
                return (
                    ActionAck::Unavailable {
                        reason_key: key.to_owned(),
                    },
                    Vec::new(),
                );
            }
        };
        let Some(target) = target else {
            // There is a row and it leads nowhere: a favorite whose path does
            // not parse. It is SAID, same as when choosing it in its own
            // place.
            if has_row {
                return (
                    ActionAck::Unavailable {
                        reason_key: "hotlist-invalid".to_owned(),
                    },
                    Vec::new(),
                );
            }
            return (self.applied(), Vec::new());
        };
        self.selector = None;
        let closing = self.parche(vec![ViewChange::Picker { picker: None }]);
        let mut outgoing = vec![closing];
        outgoing.extend(self.navigate_slot(other, &target, Trail::Record, backend, mailbox));
        (self.applied(), outgoing)
    }

    /// `nav.jump-back` (D5): a NORMAL navigation to the active slot's jump
    /// point. It enters the trail, so `nav.back` undoes the jump.
    pub(super) fn jump_to_point(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match norte_frontend::history::jump_target(&self.slot().history) {
            Ok(target) => {
                let outgoing = self.navigate(&target, Trail::Record, backend, mailbox);
                (self.applied(), outgoing)
            }
            Err(key) => (
                ActionAck::Unavailable {
                    reason_key: key.to_owned(),
                },
                Vec::new(),
            ),
        }
    }

    /// `nav.set-jump-point` (D5): marks the active slot's directory.
    pub(super) fn set_jump_point(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let dir = self.slot().pane.dir().clone();
        self.slot_mut().history.set_jump(dir);
        let outgoing = self.say("msg-nav-jump-point-set");
        (self.applied(), outgoing)
    }

    /// TEXT keys while filtering a history list (spec 2026-09-15 D2), with the
    /// terminal's rule: printables and backspace type, `Esc` removes the
    /// filter, and everything else keeps going to the keymap. `None` if the
    /// key was not the filter's, or nothing is being filtered.
    pub(super) fn filter_key(
        &mut self,
        k: &crate::keys::KeyInput,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        let mut filter = self.selector.as_ref()?.filter()?.to_owned();
        match k.key.as_str() {
            "Escape" | "esc" => return Some(self.filter_history(None)),
            "Backspace" | "backspace" => {
                filter.pop();
            }
            other => {
                // A TEXT key is a code point, like in the palette.
                let mut chars = other.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if !k.ctrl && !k.alt && !k.meta => filter.push(c),
                    _ => return None,
                }
            }
        }
        Some(self.filter_history(Some(filter)))
    }

    /// Rebuilds the open history list with a different filter; `None` removes
    /// it. The cursor goes back to the start: it is a different list.
    pub(super) fn filter_history(
        &mut self,
        filter: Option<String>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(s) = self.selector.as_ref() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        if !s.es_history() {
            return (self.applied(), Vec::new());
        }
        let (slot, title, popular) = (s.slot(), s.title(), s.es_popular());
        self.open_history_list(slot, title, popular, None, filter)
    }

    /// Opens the favorites from the configuration the window started with.
    ///
    /// The same ones that feed the side bar, and from the same source: two
    /// favorites lists read differently would be two configurations.
    pub(super) fn open_hotlist(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let slot = self.active();
        let favorites: Vec<(String, Result<VPath, String>)> = self
            .config
            .common
            .hotlist
            .iter()
            .map(|h| (h.name.clone(), h.target.clone()))
            .collect();
        self.selector = Some(crate::pickers::Selector::hotlist(
            slot, &favorites, self.lang,
        ));
        self.gen_selector += 1;
        let change = ViewChange::Picker {
            picker: self.vista_selector(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }
}
