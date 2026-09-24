//! The UI session as seen from `App` (L2, ADR 0059): composing the body of
//! NOW, applying the one read from disk, repositioning the cursor, adopting
//! orphaned slots and sealing their age.

use super::App;
use super::pane::Pane;
use norte_i18n::{t, ta};

impl App {
    /// Under which `layouts` key this process' screen goes.
    ///
    /// The active profile's name, or `default` if there is none — which is
    /// the key everyone used before there were profiles, so a reader who
    /// never picks one reads and writes exactly where they already wrote.
    ///
    /// The text conversion is D4's only one: `layouts` is a JSON object. A
    /// profile whose directory isn't UTF-8 falls back to `default`, which is
    /// the consequence the picker warns about upfront with its
    /// `carries_state`.
    fn session_key(&self) -> String {
        let active = self.session_key_active();
        if active.is_empty() {
            "default".to_owned()
        } else {
            active
        }
    }

    /// The active profile's name for the body's `active` field: empty when
    /// there is none, or when the one there is can't be a key.
    fn session_key_active(&self) -> String {
        self.active_profile
            .as_ref()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned()
    }

    /// The screen of NOW as a session body (L2).
    ///
    /// Carries the layout and, per listing slot, where it is, how it looks
    /// and where it's been. Does NOT carry the marks: they're the state of
    /// an operation half done, not of a session, and returning them on
    /// startup would be handing back an `F8` pointed at what you marked
    /// yesterday.
    ///
    /// Slots the session carried that this layout doesn't have travel back
    /// intact, in [`super::SessionUi`]'s orphan corner.
    #[must_use]
    pub fn session_body(&self) -> norte_frontend::session::SessionBody {
        self.session_body_with_marks(false)
    }

    /// The same screen, WITH what's marked (phase 9): what gets dumped for a
    /// handoff between frontends.
    ///
    /// **The marks travel here and not in the usual dump**, and that's the
    /// whole difference between the two methods. In a handoff, seconds pass
    /// between letting go and claiming, so returning what's marked is
    /// returning the work that was being done; in an ordinary startup, hours
    /// have passed, and returning it would be putting an `F8` on what you
    /// marked yesterday. `session_body`'s reasoning still holds: what
    /// changes isn't the doctrine, it's that a handoff isn't a startup.
    #[must_use]
    pub fn session_body_for_handoff(&self) -> norte_frontend::session::SessionBody {
        self.session_body_with_marks(true)
    }

    fn session_body_with_marks(&self, marks: bool) -> norte_frontend::session::SessionBody {
        use norte_frontend::session::{MARKS_CAP, SessionBody, SlotState};

        // THIS profile's layout goes under its name; the others' come back
        // as is. Writing only the active one's would erase from the
        // document the place where the other profiles left their panels.
        let mut layouts = self.session.other_layouts.clone();
        layouts.insert(self.session_key(), self.layout.clone());
        // In a HANDOFF to the window, also under the window's key
        // (ADR 0139): each frontend remembers its own, but handing over the
        // screen means the window opens with THIS one.
        if marks {
            layouts.insert(
                norte_frontend::session::window_layout_key(&self.session_key()),
                self.layout.clone(),
            );
        }
        let mut body = SessionBody {
            active: self.session_key_active(),
            layouts,
            slots: self.session.orphans.clone(),
            palette_recent: self.palette_recent.clone(),
            popular: self.popular.entries().to_vec(),
        };
        for id in self.layout.slot_ids() {
            let Some(pane) = self.panes.browser(id) else {
                continue;
            };
            let history = self.history.for_slot(id);
            body.slots.insert(
                id.0,
                SlotState {
                    path: pane.dir().clone(),
                    cursor: pane.cursor() as u64,
                    back: history.map(|h| h.trail().to_vec()).unwrap_or_default(),
                    forward: history
                        .map(|h| h.forward_trail().to_vec())
                        .unwrap_or_default(),
                    jump: history.and_then(|h| h.jump().cloned()),
                    sort: pane.sort(),
                    // Columns belong to the per-scheme CONFIGURATION, not
                    // per-slot state: capturing them here would invent a
                    // state this frontend doesn't have. The field exists for
                    // whoever does have one.
                    columns: Vec::new(),
                    show_hidden: pane.show_hidden(),
                    touched_ms: self.session.touched.get(&id.0).copied().unwrap_or_default(),
                    // By PATH, which is the row's identity: an index
                    // restored over a listing that changed points at another
                    // file, and what would get returned is a selection
                    // nobody made. The cap belongs to the model.
                    marks: if marks {
                        pane.marked_entries()
                            .iter()
                            .take(MARKS_CAP)
                            .map(|e| e.path.clone())
                            .collect()
                    } else {
                        Vec::new()
                    },
                },
            );
        }
        body
    }

    /// `ntc <DIR>` over an applied session: the ACTIVE panel switches to
    /// `dir` and nothing else changes. The saved session is more specific
    /// than the config, but a directory typed on the command line is more
    /// specific than both: whoever types `ntc ~/project` wants to see
    /// `~/project`, not wherever they closed yesterday.
    ///
    /// Keeps the panel's order and hidden setting (they're preferences, not
    /// location) and forgets the saved cursor, which was a row from ANOTHER
    /// directory. The slot stays on the list of ones that need listing.
    pub fn pin_start_dir(&mut self, dir: norte_proto::VPath) {
        let idx = self.focus();
        let slot = self.panes.slot_of(idx);
        let pane = &self.panes[idx];
        let (sort, hidden) = (pane.sort(), pane.show_hidden());
        // Through the adoption gate, like the other two listings born
        // outside the constructor. Set by hand, this one was left without
        // the `..` row — and `set_listing` doesn't fix it afterward, because
        // `poner_padre` does nothing from `Apagada`: the panel was left
        // without it until the config's next hot-reload.
        self.adoptar_pane(slot, Pane::new(dir, Vec::new()), Some(sort), Some(hidden));
        self.session.cursors.remove(&slot.0);
    }

    /// Applies a saved session and says which slots need listing.
    ///
    /// Sets the layout, seeds each listing with its directory, order, hidden
    /// setting and both trails, and SAVES the cursor for when the listing
    /// arrives ([`Self::restore_cursor`]): over an empty pane there's no row
    /// 12 to put it on.
    ///
    /// What the layout doesn't have is kept aside instead of dropped.
    pub fn apply_session(
        &mut self,
        body: &norte_frontend::session::SessionBody,
    ) -> Vec<norte_frontend::layout::SlotId> {
        let key = self.session_key();
        if let Some(tree) = body.layouts.get(&key) {
            self.set_layout(tree.clone());
        }
        self.palette_recent.clone_from(&body.palette_recent);
        self.popular = norte_frontend::history::Popular::from_entries(body.popular.clone());
        // The OTHER profiles' data is kept whole to be written back: this
        // process looks at one profile and the document belongs to all of
        // them.
        self.session.other_layouts = body
            .layouts
            .iter()
            .filter(|(k, _)| **k != key)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut ask = Vec::new();
        self.session.orphans.clear();
        for (raw, state) in &body.slots {
            let id = norte_frontend::layout::SlotId(*raw);
            self.session.touched.insert(*raw, state.touched_ms);
            // "Does THIS layout have the slot?" is asked of the LAYOUT, not
            // the pane store: orphans stay in the store, so
            // `browser(id).is_some()` used to answer yes for a slot the
            // layout doesn't place — and that slot came in through the
            // adoption gate instead of being kept as is, which is what the
            // paragraph below promises.
            if !self.layout.slot_ids().contains(&id) {
                // A slot this layout doesn't have does NOT get deleted: it's
                // kept as is and written back. Returning to yesterday's
                // layout gives the panel back where it was.
                self.session.orphans.insert(*raw, state.clone());
                continue;
            }
            // The pane comes up on the saved path and ADOPTS this session's
            // config: order and hidden setting belong to the session, the
            // `..` row belongs to the config. Setting it by hand here got it
            // lost on every restore.
            self.adoptar_pane(
                id,
                Pane::new(state.path.clone(), Vec::new()),
                Some(state.sort.clone()),
                Some(state.show_hidden),
            );
            self.session.cursors.insert(*raw, state.cursor);
            // A HANDOFF's marks, and only then: `attach` sets `--attach`.
            // Without that condition, a body that carried them — because the
            // handoff was left half done — would resurrect the next day a
            // selection nobody made, which is exactly what `session_body`
            // refuses to save.
            //
            // SAVED here and applied when the listing arrives, through the
            // same spot as the cursor. Seeding them now doesn't work, and
            // the pilot uncovered it: the pane is born empty, the listing
            // drains afterward, and `set_listing` clears the marks — which
            // is correct, a `cd` doesn't keep what's marked — so the early
            // seeding erased itself and the handoff returned the screen
            // with nothing marked.
            if self.session.attach && !state.marks.is_empty() {
                self.session.marks.insert(*raw, state.marks.clone());
            }
            let history = self.history.for_slot_mut(id);
            history.seed(state.back.clone(), state.forward.clone());
            history.seed_jump(state.jump.clone());
            ask.push(id);
        }
        ask
    }

    /// Seeds the slots `[profile.start]` names that the session doesn't
    /// know.
    ///
    /// Called AFTER [`Self::apply_session`]. Who wins is decided by
    /// [`norte_frontend::config::profile_start_seeds`], the function both
    /// frontends share, and the answer is that the session wins:
    /// `[profile.start]` is where a slot opens the first time, not a marker
    /// that sends you back to the start every time you enter the profile.
    ///
    /// **Both vetoes are set by this method, not by the caller.** They're
    /// what's read from disk and what this process has already seeded, and
    /// neither can be passed in as a parameter without getting it wrong:
    /// handing it `App::session_body()` — the screen of NOW — the filter
    /// names every live slot and nothing ever gets seeded.
    ///
    /// Only LISTING slots this layout PLACES get seeded. The pane store
    /// isn't asked: it keeps orphans and `insert` revives them, so an id the
    /// profile names that this layout doesn't place would overwrite the
    /// pane that slot has stored for when it returns to its layout. It's
    /// the same trap [`Self::apply_session`] documents thirty lines up.
    ///
    /// Returns the ones seeded. The caller re-lists them with
    /// `refresh_panes`, which only walks the VISIBLE ones: a slot seeded
    /// behind a hidden tab stays cold until it's looked at, same as one
    /// restored from the session through that same path.
    pub fn seed_profile_start(
        &mut self,
        start: &std::collections::BTreeMap<u32, norte_proto::VPath>,
    ) -> Vec<norte_frontend::layout::SlotId> {
        let placed: std::collections::BTreeSet<u32> =
            self.layout.slot_ids().into_iter().map(|s| s.0).collect();
        // An id the profile names that this layout doesn't place has
        // nowhere to open. It's SAID: staying quiet about it is the same
        // kind of silence the whole key had before ADR 0098 — you write
        // something in the file and nothing happens, with nothing to
        // explain why.
        let orphans = norte_frontend::config::profile_start_orphans(start, &placed);
        if !orphans.is_empty() {
            let ids: Vec<String> = orphans.iter().map(u32::to_string).collect();
            self.message = Some(ta(
                "msg-profile-start-orphans",
                &[("n", &orphans.len().to_string()), ("ids", &ids.join(", "))],
            ));
        }
        let mut ask = Vec::new();
        for (raw, path) in norte_frontend::config::profile_start_seeds(
            start,
            &self.session.read,
            &self.session.seeded,
        ) {
            let id = norte_frontend::layout::SlotId(raw);
            if !placed.contains(&raw) || self.panes.browser(id).is_none() {
                continue;
            }
            self.adoptar_pane(id, Pane::new(path, Vec::new()), None, None);
            self.session.seeded.insert(raw);
            ask.push(id);
        }
        ask
    }

    /// Places the cursor the session carried, now that the listing is there.
    ///
    /// Consumed: it's a ONE-TIME thing, startup's. Out of bounds it clamps —
    /// a directory with fewer entries than yesterday doesn't leave the
    /// cursor outside it — and [`Pane::set_cursor`] does that.
    pub fn restore_cursor(&mut self, id: norte_frontend::layout::SlotId) {
        // A handoff's marks go through the SAME gate as the cursor
        // (phase 9), and for the same reason: the pane is born empty and the
        // listing arrives afterward. Seeding them earlier got them erased by
        // `set_listing`, which clears what's marked on every `cd` — correct
        // for a `cd`, and fatal for seeding done too soon.
        if let Some(marks) = self.session.marks.remove(&id.0)
            && let Some(pane) = self.panes.browser_mut(id)
        {
            pane.seed_marks(marks);
        }
        let Some(row) = self.session.cursors.remove(&id.0) else {
            return;
        };
        if let Some(pane) = self.panes.browser_mut(id) {
            pane.set_cursor(usize::try_from(row).unwrap_or(usize::MAX));
        }
    }

    /// Adopts slots another window was keeping that this one didn't have
    /// (#231).
    ///
    /// The ones the LIVE layout has win over ours: this screen is the one
    /// that just moved. The rest get saved in the orphan corner and written
    /// back as is — the only path that brings this map is an ownership
    /// handoff, i.e. exactly when what's saved isn't ours, and overwriting
    /// it outright would throw away someone's history for a panel they were
    /// going to come back to.
    pub fn adopt_session_orphans(
        &mut self,
        foreign: std::collections::BTreeMap<u32, norte_frontend::session::SlotState>,
    ) {
        let alive: std::collections::BTreeSet<u32> =
            self.layout.slot_ids().into_iter().map(|s| s.0).collect();
        for (id, state) in foreign {
            if alive.contains(&id) {
                continue;
            }
            self.session.touched.insert(id, state.touched_ms);
            self.session.orphans.insert(id, state);
        }
    }

    /// Marks a slot as touched NOW, for the age sweep.
    pub fn touch_session_slot(&mut self, id: norte_frontend::layout::SlotId, now_ms: u64) {
        self.session.touched.insert(id.0, now_ms);
    }

    /// Applies the OPAQUE body that came from the core, or says why not.
    ///
    /// A body that can't be read does NOT leave a blank screen: the config's
    /// layout stays and a warning is given. It's the same decision the core
    /// makes with a corrupt file, one process removed.
    pub fn apply_session_value(&mut self, version: u32, v: &serde_json::Value) {
        match norte_frontend::session::SessionBody::from_value(version, v) {
            Ok(body) => {
                // The STICKY profile arrives HERE and not earlier: it lives
                // in the session, and the session belongs to the daemon,
                // which is reached with the config already loaded. So the
                // switch is requested and the loop does it through the same
                // path as any other one (ADR 0079, D8) — with the one gap
                // that path has: `[ui] lang` can't be reapplied, and it gets
                // announced.
                //
                // An explicit `--profile` already left `active_profile` set
                // before getting here, and then the sticky one does NOT win:
                // the reader named one for this time.
                if self.active_profile.is_none() && !body.active.is_empty() {
                    self.pending_profile = Some(std::ffi::OsString::from(&body.active));
                }
                // Which slots the saved data KNOWS ABOUT, before applying
                // it: it's `[profile.start]`'s veto, and it has to come out
                // of here because this is the only spot in the terminal
                // where the document is seen exactly as it came off disk.
                self.session.read = body.slots.keys().copied().collect();
                self.apply_session(&body);
            }
            // A body from a NEWER version doesn't get read and doesn't get
            // overwritten either: this window declares itself detached and
            // stops writing. Without this, the warning came out and a
            // second later the dump published the config's screen right on
            // top — "not read" ending in "gets lost", which is what
            // ADR 0059 promises doesn't happen.
            Err(e @ norte_frontend::session::SessionError::FromTheFuture { .. }) => {
                tracing::warn!(error = %e, "UI session from a newer version: not writing");
                self.session.detached = true;
                self.message = Some(t("msg-session-unreadable"));
            }
            Err(e) => {
                tracing::warn!(error = %e, "unreadable UI session");
                self.message = Some(t("msg-session-unreadable"));
            }
        }
    }

    /// Sets the `name` layout, and says whether it succeeded.
    ///
    /// First `<dir>/layouts/<name>.toml` and then the factory preset of the
    /// same name: the user's file wins, as in every other config layer, and
    /// a preset is recovered by deleting the file. If the file is broken it
    /// warns AND falls back to the preset — a layout that doesn't parse
    /// can't leave norte without a screen.
    ///
    /// The RULE — user's file, and if not the preset — lives in
    /// [`norte_frontend::layout::config::or_preset`], shared with the
    /// window: it used to be written by hand here and the window didn't
    /// have it, so `norte-gui --layout mine` couldn't open a user layout.
    /// What's left here is what really belongs to the TUI: setting the tree
    /// and painting the warning.
    ///
    /// Reads a small config file on the calling thread, like startup's
    /// `[ui] layout`.
    pub fn apply_loaded_layout(
        &mut self,
        name: &std::ffi::OsStr,
        loaded: Result<norte_frontend::layout::Node, norte_frontend::layout::LayoutError>,
    ) -> bool {
        // The name gets PAINTED, and comes from a file or the command line:
        // lossy and marked, and hazards masked, like any other name (#246
        // m3). The bytes aren't touched: the loader used them. And with its
        // mark if there were bytes that couldn't be painted: without it,
        // `$'\xff'` and `$'\xfe'` give the SAME message and the reader can't
        // tell which of the two was named.
        let (showable, lossy) = norte_frontend::display_os_name(name);
        let showable = norte_encoding::mask_terminal_hazards(&showable);
        let showable = if lossy {
            format!("{} {showable}", crate::ui::HOSTILE_BADGE)
        } else {
            showable
        };
        match norte_frontend::layout::config::or_preset(name, loaded) {
            Ok((tree, broken)) => {
                self.set_layout(tree);
                if let Some(e) = broken {
                    self.message = Some(ta(
                        "msg-layout-load-failed",
                        &[("name", &showable), ("err", &e.to_string())],
                    ));
                }
                true
            }
            Err(e) => {
                self.message = Some(ta(
                    "msg-layout-load-failed",
                    &[("name", &showable), ("err", &e.to_string())],
                ));
                false
            }
        }
    }
}
