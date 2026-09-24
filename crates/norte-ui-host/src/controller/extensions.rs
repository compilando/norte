//! The extensions catalog, its detail card and its governance.
//!
//! Part of `controller`: these are `State` methods, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl State` split into pieces, so they use
// the same imports as the parent. Enumerating them here would be a
// forty-line list per file, in 32 files, that goes stale the moment the
// parent imports something — `super::*` tracks it on its own.
#[allow(clippy::wildcard_imports)]
use super::*;

impl State {
    /// The directories on screen RIGHT NOW, without repeats.
    pub(super) fn dirs_visible(&self) -> Vec<VPath> {
        let mut v: Vec<VPath> = Vec::new();
        for h in self.slots.values() {
            let dir = h.pane.dir();
            if !v.contains(dir) {
                v.push(dir.clone());
            }
        }
        v
    }

    pub(super) fn open_extensions(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.extensions = Some(crate::extensions::Extensions::open());
        self.gen_extensions += 1;
        self.request_extensions_catalog(backend, mailbox);
        let change = ViewChange::Extensions {
            extensions: self.vista_extensions(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Dispatches a background answer to the surface that requested it.
    ///
    /// ONE spot for all of them: they all check the same thing — that their
    /// surface is still open — and they all answer the same thing: whatever
    /// patches need to be sent, or none.
    ///
    /// The `match` is a LIST: every arm delegates to its own method, so it
    /// grows one line per new answer and none of them carries logic here.
    /// That is why it has the `expect` instead of splitting into two nameless
    /// halves.
    #[expect(clippy::too_many_lines, reason = "a match that only dispatches")]
    pub(super) fn apply_in_background(
        &mut self,
        f: Background,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        match f {
            Background::Profiles(profiles, neighbor) => {
                self.with_the_profiles(profiles, neighbor, mailbox)
            }
            Background::ProfileLoaded(name, res) => {
                self.apply_profile(&name, *res, backend, mailbox)
            }
            Background::SettingWritten(done) => self.setting_written(*done, backend, mailbox),
            Background::SettingRestored(done) => self.setting_restored(*done, backend, mailbox),
            Background::PlanIa(epoch, res) => self.apply_plan_ia(epoch, *res, backend, mailbox),
            // Phase 8: the organize tree needs no second trip, so it carries
            // neither `backend` nor `mailbox`.
            Background::PlanOrganize(epoch, res) => self.apply_organize_plan(epoch, *res),
            // #311: the two halves of checking checksums — the file read
            // before launching anything, and the report that arrives
            // afterward.
            Background::ChecksumsFile(sums, bytes) => {
                self.checksums_file(&sums, *bytes, backend, mailbox)
            }
            Background::ChecksumsReport(task, state, report) => {
                self.checksums_report(task, &state, *report)
            }
            Background::BatchPlan(epoch, res) => self.apply_batch_plan(epoch, *res),
            Background::HelpPlugins(res) => self.apply_plugins_catalog(res, backend, mailbox),
            // The panes contributed by consented plugins become real kinds
            // (phase 3). With no surface to depend on: a plugin pane has to
            // be placeable even if nobody has opened help or the manager. The
            // approved/enabled filter lives in `insert_panels`, shared with
            // the terminal.
            //
            // A failure leaves the session with no plugin panes, which is
            // the usual screen: the cosmetic part degrades.
            Background::PluginPanes(res) => match res {
                Ok(list) => {
                    self.kinds.insert_panels(&list.plugins);
                    // Declaring a kind does NOT repaint on its own: the
                    // layout is rebuilt here — the newly declared pane's
                    // minimums change what fits — and the empty patch
                    // carries the status bar, which `parche` adds on its
                    // own. Without this, the screen stayed laid out as if
                    // the kind did not exist until the next unrelated
                    // change.
                    self.redo_split();
                    vec![self.parche(Vec::new())]
                }
                Err(_) => Vec::new(),
            },
            Background::PluginPage(id, res) => self
                .apply_plugin_page(&id, res.as_ref().ok())
                .into_iter()
                .collect(),
            Background::Catalog(opening, request, res) => {
                // The manager's catalog redeclares the panes (phase 3): it
                // is the same data, and it is the moment a plugin has just
                // been approved, enabled or uninstalled. Without this,
                // revoking a plugin's consent left its kind declared — and
                // its slot taking focus — until the next startup.
                if let Ok(list) = &res {
                    self.kinds.insert_panels(&list.plugins);
                    self.redo_split();
                }
                self.apply_extensions_catalog(opening, request, res, backend, mailbox)
            }
            Background::PluginTab(id, res) => self
                .apply_detail(&id, res.as_ref().ok())
                .into_iter()
                .collect(),
            Background::DestinationNotices(id, notices) => self.destination_notices(id, notices),
            Background::SessionUndo(task_id, session) => {
                self.agency.undos.insert(task_id, session);
                Vec::new()
            }
            Background::PalettePlugins(opening, res) => self.apply_plugin_rows(opening, res),
            Background::Governed(opening, res) => {
                self.apply_governance(opening, &res, backend, mailbox)
            }
            Background::ConfigWritten(opening, id, res) => {
                self.apply_write(opening, &id, res, backend, mailbox)
            }
            Background::CommandOutput(opening, data) => self.apply_output(opening, *data),
            Background::Volumes(opening, res) => {
                self.apply_volumes(opening, res).into_iter().collect()
            }
            Background::Connections(opening, res) => {
                self.apply_connections(opening, res).into_iter().collect()
            }
            Background::TimelinePage(slot, token, start, res) => self
                .land_page(slot, token, start, res)
                .into_iter()
                .collect(),
            Background::GoToConnections(opening, res) => {
                self.goto_connections(opening, res).into_iter().collect()
            }
            Background::GoToIndex(opening, query, res) => {
                self.goto_index(opening, &query, res).into_iter().collect()
            }
            Background::Disconnected(slot, res, dest) => {
                self.apply_disconnection(slot, res, &dest, backend, mailbox)
            }
            Background::PlacesVolumes(res) => self.apply_places(res).into_iter().collect(),
            Background::FooterVolumes(res) => self.apply_footer_volumes(res).into_iter().collect(),
            Background::TreeBranches(dir, children) => self
                .apply_branches(dir, children, backend, mailbox)
                .into_iter()
                .collect(),
            Background::Results(epoch, batch) => {
                self.apply_results(epoch, &batch).into_iter().collect()
            }
            Background::Semantic(epoch, hits) => self.apply_semantic(epoch, hits),
            Background::ComparisonViva(epoch, id) => {
                if let Some(c) = self.comparison.as_mut()
                    && c.epoch == epoch
                {
                    c.task = id;
                }
                Vec::new()
            }
            Background::RowsCompared(epoch, batch) => self.apply_rows_compared(epoch, *batch),
            Background::PlanDeSyncVivo(epoch, id) => self.open_sync_panel(epoch, id),
            Background::SyncApplying(epoch, id) => self.sync_applying(epoch, id, backend, mailbox),
            Background::SyncNoApplied(epoch, safe) => {
                let mut outside = Vec::new();
                if let Some(s) = self.sync.as_mut().filter(|s| s.epoch == epoch) {
                    if safe {
                        s.vista.on_apply_abandoned();
                    } else {
                        // Ambiguous: the latch STAYS thrown. The screen
                        // cannot say "did not apply" about something that
                        // might still be applying, nor offer to retry it.
                        outside.extend(self.say("msg-sync-apply-unknown"));
                    }
                }
                // With its patch: `on_apply_abandoned` changes what the
                // screen offers, and without repainting, the `a` that just
                // came back looks dead.
                outside.push(self.parche(vec![ViewChange::Sync {
                    sync: self.vista_sync(),
                }]));
                outside
            }
            Background::SyncReport(epoch, state, report) => {
                self.sync_report(epoch, &state, *report)
            }
            Background::FailedSyncPlan(epoch) => {
                if self
                    .sync_requested
                    .as_ref()
                    .is_some_and(|p| p.epoch == epoch)
                {
                    self.sync_requested = None;
                }
                Vec::new()
            }
            Background::SyncEvent(epoch, ev) => self.apply_sync_event(epoch, *ev),
            Background::Adornos(data) => self
                .apply_adornos(*data, backend, mailbox)
                .into_iter()
                .collect(),
            Background::Imagen(token, read) => self.apply_imagen(token, read).into_iter().collect(),
            Background::Style(token, preview) => {
                self.apply_style(token, preview).into_iter().collect()
            }
            Background::Thumbnail(token, thumb) => {
                self.apply_thumbnail(token, thumb).into_iter().collect()
            }
            Background::SearchViva(epoch, id) => {
                if let Some(b) = self.search.as_mut()
                    && b.epoch == epoch
                {
                    b.task = id;
                }
                Vec::new()
            }
            Background::SearchBroken(epoch, e) => self.search_broken(epoch, &e),
        }
    }

    /// Requests the catalog to declare which PANES the plugins contribute.
    ///
    /// On startup and only once: what it brings is which slots exist, not
    /// any of their contents. Through its own path — and not help's or the
    /// manager's — because those exit early if their surface is closed, and
    /// a plugin pane has to be placeable without either of the two having
    /// been opened (phase 3).
    ///
    /// Also in READ-ONLY, and it is deliberate. The palette's rule — "offer
    /// what is going to be refused is promising something that will not
    /// happen" — does not apply here: this offers nothing, it is a READ that
    /// brings the declaration of which slots exist, and without it a saved
    /// layout with a plugin pane leaves a slot of unknown kind, which is
    /// placed with a `(1, 1)` minimum, does not take focus, does not paint
    /// and cannot even be named: a blank box stealing space that the reader
    /// cannot identify. What IS gated by effects is the pane's INTERACTION —
    /// its clickable zones and its commands — where the promise is made.
    ///
    /// And with no gate also because the terminal always asks: a window and
    /// a TUI in read-only have to show the same screen.
    ///
    /// Fail-soft: if the RPC fails or times out, this session is left with
    /// no plugin panes, which is the usual screen.
    /// With no `self` on purpose: since there is no effects gate, it depends
    /// on nothing from the state.
    pub(super) fn request_panels(backend: &Arc<dyn HostBackend>, mailbox: &mpsc::Sender<Message>) {
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(DEADLINE_PLUGINS, backend.plugin_list()).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = mailbox
                .send(Message::Background(Box::new(Background::PluginPanes(res))))
                .await;
        });
    }

    /// The catalog arrived at the manager.
    ///
    /// A failure is also applied: it stops being "loading" and the list ends
    /// up empty, which with the warning off means "there are none". Staying
    /// "loading" forever would be the only worse answer.
    pub(super) fn apply_extensions_catalog(
        &mut self,
        opening: u64,
        request: u64,
        res: Result<norte_proto::methods::PluginListResult, Error>,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // From THIS opening. "Still open" is not "is the same one".
        if opening != self.gen_extensions {
            return Vec::new();
        }
        // And the NEWEST of whichever are in flight: an old catalog that
        // lands after the new one leaves the "approved" column saying the
        // old thing, about a change that has already happened.
        if request <= self.catalog_applied {
            return Vec::new();
        }
        self.catalog_applied = request;
        let Some(e) = self.extensions.as_mut() else {
            return Vec::new();
        };
        // A failure applies the same way: it stops being "loading" with an
        // empty list, which already knows how to say itself. Staying
        // "loading" forever is the only worse answer.
        e.set_catalog(&res.unwrap_or(norte_proto::methods::PluginListResult {
            plugins: Vec::new(),
            errors: Vec::new(),
        }));
        let _ = (backend, mailbox);
        let change = ViewChange::Extensions {
            extensions: self.vista_extensions(),
        };
        vec![self.parche(vec![change])]
    }

    /// Requests the chosen extension's detail card.
    pub(super) fn request_detail(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(e) = self.extensions.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let Some(id) = e.claim_detail() else {
            return (self.applied(), Vec::new());
        };
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            let res =
                match tokio::time::timeout(DEADLINE_PLUGINS, backend.plugin_config(id.clone()))
                    .await
                {
                    Ok(r) => r,
                    Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
                };
            let _ = mailbox
                .send(Message::Background(Box::new(Background::PluginTab(
                    id, res,
                ))))
                .await;
        });
        (self.applied(), Vec::new())
    }

    /// The detail card arrived.
    pub(super) fn apply_detail(
        &mut self,
        id: &str,
        res: Option<&norte_proto::methods::PluginGetConfigResult>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let lang = self.lang;
        let e = self.extensions.as_mut()?;
        match res {
            Some(r) => e.set_detail(id, r, lang),
            // A failure is also APPLIED: just returning left `requested` set,
            // so `claim_detail` returned `None` forever and that row could
            // never be reopened — `enter` did nothing and said nothing —
            // short of moving the cursor to another and back. It is the
            // same criterion this file already applies twice to the
            // catalog: staying "loading" forever is the only answer worse
            // than an error.
            None => e.close_detail(),
        }
        let change = ViewChange::Extensions {
            extensions: self.vista_extensions(),
        };
        Some(self.parche(vec![change]))
    }

    /// The manager's projection.
    pub(super) fn vista_extensions(&self) -> Option<crate::dto::ExtensionsView> {
        Some(self.extensions.as_ref()?.vista())
    }

    /// The keys while the manager is open.
    pub(super) fn key_in_extensions(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        /// How many rows a page moves.
        const PAGE: i64 = 10;
        let Some(e) = self.extensions.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        // THREE REGIMES, and the order matters. While a value is being
        // TYPED, letters are letters: resolving `a` as "approve" there would
        // turn typing the word "cat" into two capability grants.
        if e.editing() {
            return self.key_editing_config(k, backend, mailbox);
        }
        // `Home`/`End` stay fixed: the shared catalog has no verb for "to
        // the start" inside a dialog.
        let verb = match k.key.as_str() {
            "Home" | "home" | "End" | "end" => None,
            _ => self.dialog_verb(k),
        };
        let Some(e) = self.extensions.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        match (verb.as_deref(), k.key.as_str()) {
            (Some("dialog.cancel"), _) => {
                // The first `esc` closes the DETAIL CARD, not the manager:
                // leaving the list for closing a detail loses where the
                // reader was.
                if e.has_detail() {
                    e.close_detail();
                } else {
                    self.extensions = None;
                }
            }
            // With the detail card open, the arrows scroll THROUGH ITS keys:
            // moving the catalog underneath would drop the card being read.
            (Some("dialog.down"), _) => {
                if !e.move_in_card(1) {
                    e.mover(1);
                }
            }
            (Some("dialog.up"), _) => {
                if !e.move_in_card(-1) {
                    e.mover(-1);
                }
            }
            // The page and the extremes, through the same door as the
            // arrows: with the card open they scroll THROUGH ITS keys, and
            // only when there is nothing left to walk do they fall back to
            // the catalog.
            (Some("dialog.page-down"), _) => {
                if !e.move_in_card(PAGE) {
                    e.mover(PAGE);
                }
            }
            (Some("dialog.page-up"), _) => {
                if !e.move_in_card(-PAGE) {
                    e.mover(-PAGE);
                }
            }
            (_, "Home" | "home") => {
                if !e.move_in_card(i64::MIN / 2) {
                    e.mover(i64::MIN / 2);
                }
            }
            (_, "End" | "end") => {
                if !e.move_in_card(i64::MAX / 2) {
                    e.mover(i64::MAX / 2);
                }
            }
            (Some("dialog.confirm"), _) => {
                // A broken one has no settings to open: it is reported,
                // instead of a key that does nothing.
                if e.broken_chosen().is_some() {
                    return (
                        ActionAck::Unavailable {
                            reason_key: "ext-broken-only-uninstall".to_owned(),
                        },
                        self.say("ext-broken-only-uninstall"),
                    );
                }
                if e.has_detail() {
                    return self.activate_key(backend, mailbox);
                }
                return self.request_detail(backend, mailbox);
            }
            // Approving is `dialog.add` — granting — and enabling/disabling
            // is `dialog.toggle-enabled`: the two catalog verbs that mean
            // exactly that, instead of two letters only this window knew.
            (Some("dialog.add"), _) => {
                return self.govern_chosen(Change::Approval, backend, mailbox);
            }
            (Some("dialog.toggle-enabled"), _) => {
                return self.govern_chosen(Change::On, backend, mailbox);
            }
            // Uninstalling is `dialog.remove`, the verb that removes an
            // entry in the favorites list: here it removes the whole
            // extension, which is why it asks first.
            (Some("dialog.remove"), _) => {
                return self.govern_chosen(Change::Uninstallation, backend, mailbox);
            }
            _ => return (self.applied(), Vec::new()),
        }
        let change = ViewChange::Extensions {
            extensions: self.vista_extensions(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// The keys while a key's value is being TYPED.
    ///
    /// FIXED regime, like any other field on this host: here a letter is a
    /// letter. `Enter` confirms — and then it is written — `Escape` cancels
    /// without writing, and every other key means nothing.
    pub(super) fn key_editing_config(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(e) = self.extensions.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        match k.key.as_str() {
            "Escape" | "esc" => e.cancel_edit(),
            "Backspace" | "backspace" => e.delete(),
            "Enter" | "enter" => return self.confirm_config(backend, mailbox),
            other => {
                // A printable key is its character; any other one — and any
                // combination with a modifier — is not text.
                let mut cs = other.chars();
                match (cs.next(), cs.next()) {
                    (Some(c), None) if !k.ctrl && !k.alt && !k.meta => e.write(c),
                    _ => return (self.applied(), Vec::new()),
                }
            }
        }
        let change = ViewChange::Extensions {
            extensions: self.vista_extensions(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// `Enter` over a key: cycles, or opens the buffer to type it.
    pub(super) fn activate_key(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.effects == crate::commands::Effects::SoloRead {
            return Self::no_mutates();
        }
        let Some(e) = self.extensions.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let write = e.activate_key();
        let change = ViewChange::Extensions {
            extensions: self.vista_extensions(),
        };
        let mut outside = vec![self.parche(vec![change])];
        // A `bool` or an `enum` ALREADY changed value in the model: what is
        // left is telling the daemon. A `string`/`int` only opened the
        // buffer and there is nothing to write yet.
        if let Some((id, write)) = write {
            outside.extend(Self::write_config(
                self.gen_extensions,
                &id,
                write,
                backend,
                mailbox,
            ));
        }
        (self.applied(), outside)
    }

    /// `Enter` with the buffer open: validates and writes, or says why not.
    pub(super) fn confirm_config(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // The buffer is only opened by `activate_key`, which already checks
        // this, so today it is unreachable — same as
        // `rejects_for_read_only`, which exists anyway. A door that
        // writes is checked at the door.
        if self.effects == crate::commands::Effects::SoloRead {
            return Self::no_mutates();
        }
        let Some(e) = self.extensions.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let Some(result) = e.confirm_edit() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        match result {
            Ok((id, write)) => {
                let change = ViewChange::Extensions {
                    extensions: self.vista_extensions(),
                };
                let mut outside = vec![self.parche(vec![change])];
                outside.extend(Self::write_config(
                    self.gen_extensions,
                    &id,
                    write,
                    backend,
                    mailbox,
                ));
                (self.applied(), outside)
            }
            // This side's validation is not the one that authorizes — the
            // daemon validates again against the schema — but reporting it
            // here saves a trip and, above all, says WHAT the bound was.
            Err(norte_frontend::settings::SettingsEditError::NotAnInt) => (
                ActionAck::Unavailable {
                    reason_key: "host-not-an-int".to_owned(),
                },
                self.say("host-not-an-int"),
            ),
            Err(norte_frontend::settings::SettingsEditError::OutOfRange { min, max }) => {
                // The warning carries the bounds; the ACK cannot: nobody
                // substitutes variables into that key, so a `{ $min }` in
                // the ack gets logged literally. Two keys, and the one
                // carrying numbers is the one translated with them.
                let outside = self.say_with(
                    "host-out-of-range",
                    &[("min", &min.to_string()), ("max", &max.to_string())],
                );
                (
                    ActionAck::Unavailable {
                        reason_key: "host-value-rejected".to_owned(),
                    },
                    outside,
                )
            }
            // A plugin field has no closed vocabulary today (only norte's
            // settings editor returns this); it is reported like any
            // rejected value.
            Err(norte_frontend::settings::SettingsEditError::Invalid { .. }) => (
                ActionAck::Unavailable {
                    reason_key: "host-value-rejected".to_owned(),
                },
                self.say("host-value-rejected"),
            ),
        }
    }

    /// Sends ONE key to the daemon.
    ///
    /// The value is already set in the model (optimism): what fixes a
    /// failure is RE-REQUESTING the card, not guessing what there was
    /// before.
    pub(super) fn write_config(
        opening: u64,
        id: &str,
        write: norte_frontend::plugin_config::PendingConfigWrite,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let backend2 = Arc::clone(backend);
        let buzon2 = mailbox.clone();
        let (id2, key, value) = (id.to_owned(), write.key, write.value);
        tokio::spawn(async move {
            let res = match tokio::time::timeout(
                DEADLINE_PLUGINS,
                backend2.plugin_set_config(id2.clone(), key, value),
            )
            .await
            {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon2
                .send(Message::Background(Box::new(Background::ConfigWritten(
                    opening, id2, res,
                ))))
                .await;
        });
        Vec::new()
    }

    /// The write answered.
    pub(super) fn apply_write(
        &mut self,
        opening: u64,
        id: &str,
        res: Result<(), Error>,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Err(e) = res else {
            // A setting that changed can change what a decorator paints —
            // the icons' style, for one — listings are requested again.
            return self.redecorate_all(backend, mailbox);
        };
        let mut outside = self.say(norte_frontend::error::error_key(&e));
        if opening != self.gen_extensions {
            return outside;
        }
        // And the card is RE-REQUESTED: the screen's optimistic value is
        // right now a lie about what the plugin has configured, and
        // guessing the previous one is inventing a third state.
        //
        // Unless it is being TYPED into: re-requesting it drops the whole
        // `PluginConfigState`, and with it whatever the reader has typed for
        // another key. A stale value on screen is bad; eating what someone
        // just typed, worse — and the correction arrives just the same as
        // soon as the field closes.
        if let Some(ext) = self.extensions.as_mut()
            && ext.is_tab_of(id)
            && !ext.editing()
        {
            ext.close_detail();
            // The close ALWAYS travels in its own patch: `request_detail` sends
            // none along its happy path, so without this the renderer kept
            // painting a card the host no longer has — and the arrows,
            // unable to find it anymore, moved the catalog underneath it.
            let change = ViewChange::Extensions {
                extensions: self.vista_extensions(),
            };
            outside.push(self.parche(vec![change]));
            let (_, parts) = self.request_detail(backend, mailbox);
            outside.extend(parts);
        }
        outside
    }

    /// `a`/`e` over the chosen extension.
    pub(super) fn govern_chosen(
        &mut self,
        change: Change,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.effects == crate::commands::Effects::SoloRead {
            return Self::no_mutates();
        }
        let Some(e) = self.extensions.as_ref() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let Some(the_row) = e.row_chosen() else {
            // One that did NOT load: there are no capabilities to read and
            // nothing to turn on, and all it can be asked is to be removed —
            // if its directory is named like an id, which is what gets
            // deleted.
            let Some(broken) = e.broken_chosen() else {
                return (
                    ActionAck::Unavailable {
                        reason_key: "host-no-extension".to_owned(),
                    },
                    Vec::new(),
                );
            };
            let key = match (change, broken.id.clone()) {
                (Change::Uninstallation, Some(id)) => {
                    return self.ask_for_uninstall(&id);
                }
                (Change::Uninstallation, None) => "ext-broken-not-id",
                _ => "ext-broken-only-uninstall",
            };
            return (
                ActionAck::Unavailable {
                    reason_key: key.to_owned(),
                },
                self.say(key),
            );
        };
        let (id, approved, on) = (the_row.id.clone(), the_row.approved, the_row.enabled);
        match change {
            // Granting ASKS; revoking does not.
            Change::Approval if !approved => self.ask_for_approval(&id),
            Change::Approval => {
                let outside = self.govern(&id, Governance::Approve(false, None), backend, mailbox);
                (self.applied(), outside)
            }
            // TURNING ON a plugin with no approval is not a decision this
            // screen can make on its own: with no approved capabilities the
            // core is not going to load it, and saying "on" about something
            // that is not running is the screen lying. TURNING IT OFF, yes,
            // always: it goes in the safe direction, and denying it would
            // leave no way to turn off an enabled extension that just had
            // its capabilities revoked — i.e. it would forbid exactly what
            // must be possible.
            Change::On if !approved && !on => (
                ActionAck::Unavailable {
                    reason_key: "host-extension-not-approved".to_owned(),
                },
                self.say("host-extension-not-approved"),
            ),
            Change::On => {
                let outside = self.govern(&id, Governance::TurnOn(!on), backend, mailbox);
                (self.applied(), outside)
            }
            // Uninstalling ALWAYS asks: it deletes files and there is no
            // going back.
            Change::Uninstallation => self.ask_for_uninstall(&id),
        }
    }

    /// What a BUTTON does over a row (bridge 61): point at it and govern the
    /// pointed-at one, through the same path as the key. That it is the same
    /// path is the point: the questions — granting enumerates, uninstalling
    /// warns — are asked once, here, and no button dodges them.
    pub(super) fn govern_by_mouse(
        &mut self,
        row: u32,
        id: &str,
        change: Change,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // With a dialog in front, no: the manager is modal for the keyboard
        // (`input.rs` cuts it off before reaching here) and it has to be for
        // the mouse too, or a click behind the consent question would revoke
        // without asking, or stack a second question on the first.
        if !self.dialogs.is_empty() {
            return (Self::stale(StaleAction::Modal), Vec::new());
        }
        // Before moving anything: a read-only window does not repaint a
        // cursor moved by an action that is about to be refused.
        if self.effects == crate::commands::Effects::SoloRead {
            return Self::no_mutates();
        }
        let Some(e) = self.extensions.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let Some(moved) = Self::extension_row(e, row, id) else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let (ack, mut outside) = self.govern_chosen(change, backend, mailbox);
        if moved {
            // The cursor moved with the click, and that is painted even if
            // what follows is a question: the highlighted row is the one
            // the dialog describes.
            let change = ViewChange::Extensions {
                extensions: self.vista_extensions(),
            };
            outside.push(self.parche(vec![change]));
        }
        (ack, outside)
    }

    /// Points at the row a click names, if it is still the one the renderer
    /// saw. `None` if it is no longer there or no longer that one: the
    /// catalog is re-requested in the background and a row deleted above
    /// shifts the ones below. `Some(moved)` says whether the cursor changed
    /// place.
    fn extension_row(e: &mut crate::extensions::Extensions, row: u32, id: &str) -> Option<bool> {
        if e.row_id(row as usize)? != id {
            return None;
        }
        // By ROW, not by `chosen()`: that only looks at the loaded ones and
        // returns `None` for a broken one, so a click on an already-pointed-
        // -at broken one used to claim the cursor had moved and push a
        // whole patch that changed nothing.
        let moved = e.cursor() != row as usize;
        e.point_at(row as usize);
        Some(moved)
    }

    /// Opens the uninstall question, with the name and the id inside.
    ///
    /// The body says what is lost: the extension's files AND its consent —
    /// one installed later under the same id is born without it — because a
    /// plain "uninstall?" reads as "turn it fully off?", and that is not it.
    pub(super) fn ask_for_uninstall(
        &mut self,
        id: &str,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // One that did not load has no manifest name: its directory is
        // shown, which already comes sanitized and with its flag.
        let Some(name) = self.extensions.as_ref().and_then(|e| {
            e.grant(id)
                .map(|c| c.name)
                .or_else(|| e.broken(id).map(|r| (r.dir.clone(), r.hostile)))
        }) else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let modal = ModalId(self.next_modal);
        self.next_modal += 1;
        let vista = DialogView {
            id: modal,
            title_key: "modal-extension-uninstall-title".to_owned(),
            destination: None,
            subject: Some(crate::dto::DialogLine {
                text: clamp_display(id.to_owned()),
                hostile: false,
            }),
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![
                crate::dto::DialogLine {
                    text: name.0,
                    hostile: name.1,
                },
                crate::dto::DialogLine {
                    text: norte_i18n::t_in(self.lang, "modal-extension-uninstall-note"),
                    hostile: false,
                },
            ],
            overflow_note: String::new(),
            overflow_hostile: false,
            // `confirm`, like deleting files: it is a normal dialog's
            // affirmative answer, and the LABEL is what says what is being
            // confirmed. `approve` is reserved for granting capabilities.
            choices: vec![
                DialogChoice {
                    id: "confirm".to_owned(),
                    label_key: "dialog-uninstall".to_owned(),
                    destructive: true,
                },
                DialogChoice {
                    id: "cancel".to_owned(),
                    label_key: "dialog-cancel".to_owned(),
                    destructive: false,
                },
            ],
            input: None,
            input_hostile: false,
            input_secret: false,
            fields: Vec::new(),
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogs.push(Dialog {
            id: modal,
            vista: vista.clone(),
            typed: Typed::Text(String::new()),
            recognized: true,
            on_confirm: Some(Pending::UninstallExtension { id: id.to_owned() }),
        });
        let change = ViewChange::Dialogs {
            dialogs: self.dialog_views(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// That row's extension's help (bridge 61): what `app.help` does over
    /// the chosen row in the terminal, and cast in the same mold — the
    /// manager closes and help opens with that page as ROOT, with the
    /// catalog the manager already had so the side panel does not wait on
    /// the daemon. With no page it is reported and nothing opens: help
    /// opening at the index when it was asked for ONE extension's is the
    /// window answering a different question.
    pub(super) fn extension_help(
        &mut self,
        row: u32,
        id: &str,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Modal for the mouse just as for the keyboard: opening help would
        // close the manager under a pending question, and that question's
        // yes would find no catalog to compare what it grants against.
        if !self.dialogs.is_empty() {
            return (Self::stale(StaleAction::Modal), Vec::new());
        }
        let Some(e) = self.extensions.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        if Self::extension_row(e, row, id).is_none() {
            return (Self::stale(StaleAction::Modal), Vec::new());
        }
        let Some(the_row) = e.row_chosen() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        if !the_row.has_help {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-extensions-no-help".to_owned(),
                },
                self.say("msg-extensions-no-help"),
            );
        }
        let catalog = e.catalog().to_vec();
        let mut help = crate::help::Help::open(
            self.lang,
            self.help_context(),
            &self.effective,
            &self.effective_visor,
            self.facts(),
        );
        help.set_plugins(&catalog);
        let page = norte_help::TopicId::new(id);
        help.state.open_as_root(&page);
        if help.state.current() != &page {
            // The shared model does not open what it does not have, and it
            // does so silently: an id that never became a node would leave
            // the reader on the context page, which is not what they asked
            // for. It is reported, and the manager stays.
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-extensions-no-help".to_owned(),
                },
                self.say("msg-extensions-no-help"),
            );
        }
        self.extensions = None;
        self.help = Some(help);
        let mut outside = vec![self.parche(vec![ViewChange::Extensions { extensions: None }])];
        outside.extend(self.help_patch(backend, mailbox));
        (self.applied(), outside)
    }

    /// Opens the question to grant capabilities, with the capabilities
    /// inside.
    pub(super) fn ask_for_approval(
        &mut self,
        id: &str,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(grant) = self.extensions.as_ref().and_then(|e| e.grant(id)) else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let (name, capabilities, anchor) = (grant.name, grant.capabilities, grant.digest);
        // One capability per LINE, and the extension's name apart: they are
        // the decision's operands, and folding them into a sentence is what
        // lets a third party's name impersonate the window's text. Each with
        // ITS OWN flag: the one that paints differently from what it says is
        // exactly the one a hostile manifest writes to sneak through.
        // And NONE is trimmed. A dialog's line cap exists for a list of
        // paths where seeing part of it is enough; here the list IS the
        // grant, and showing sixteen of forty while the yes grants all forty
        // is exactly the gap the capability nobody read sneaks through. If
        // there are too many to fit, it is not asked about: it is refused.
        if capabilities.len() > MAX_CAPABILITIES {
            let outside = self.say("host-extension-too-many-caps");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-extension-too-many-caps".to_owned(),
                },
                outside,
            );
        }
        let mut body = vec![crate::dto::DialogLine {
            text: name.0,
            hostile: name.1,
        }];
        body.extend(
            capabilities
                .iter()
                .cloned()
                .map(|(text, hostile)| crate::dto::DialogLine { text, hostile }),
        );
        let note = String::new();
        let modal = ModalId(self.next_modal);
        self.next_modal += 1;
        let vista = DialogView {
            id: modal,
            title_key: "modal-extension-approve-title".to_owned(),
            destination: None,
            // The reverse-DNS id, which is the ONLY thing the core
            // validates: two extensions can share a name, and the name the
            // dialog shows is written by the manifest. Without this, the
            // screen where permissions are granted does not say to whom.
            subject: Some(crate::dto::DialogLine {
                text: clamp_display(id.to_owned()),
                hostile: false,
            }),
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body,
            overflow_note: note,
            // This dialog trims nothing: its body is the lines it is given
            // ready-made, not a list of paths to be capped.
            overflow_hostile: false,
            choices: vec![
                DialogChoice {
                    id: "approve".to_owned(),
                    label_key: "dialog-approve".to_owned(),
                    // Granting permissions deletes nothing, but it is not a
                    // plain dialog's harmless answer either: it is flagged
                    // so the renderer does not paint it like a notice's "OK".
                    destructive: true,
                },
                DialogChoice {
                    id: "deny".to_owned(),
                    label_key: "dialog-deny".to_owned(),
                    destructive: false,
                },
            ],
            input: None,
            input_hostile: false,
            input_secret: false,
            fields: Vec::new(),
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogs.push(Dialog {
            id: modal,
            vista: vista.clone(),
            typed: Typed::Text(String::new()),
            recognized: true,
            on_confirm: Some(Pending::ApproveExtension {
                id: id.to_owned(),
                capabilities: capabilities.into_iter().map(|(t, _)| t).collect(),
                digest: anchor,
            }),
        });
        let change = ViewChange::Dialogs {
            dialogs: self.dialog_views(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Grants the capabilities READ, or asks again if they have changed.
    ///
    /// The dialog keeps hold of KEYS, not of background messages: a catalog
    /// landing between the question and the yes can bring different
    /// capabilities for that extension, and then the yes would grant
    /// something nobody read.
    pub(super) fn grant(
        &mut self,
        id: &str,
        read: &[String],
        anchor_read: Option<String>,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        let now = self.extensions.as_ref().and_then(|e| e.grant(id)).map(|c| {
            c.capabilities
                .into_iter()
                .map(|(t, _)| t)
                .collect::<Vec<_>>()
        });
        if now.as_deref() == Some(read) {
            // The anchor that travels is THE QUESTION's, never the current
            // catalog's (#282): re-reading it here would certify to the core
            // "this is what the human read" about what the human did not
            // read, which is exactly the gap the field closes. And the
            // capability comparison above does not cover it: `category` and
            // `contributions` go into the anchor and not into the painted
            // list.
            return (
                None,
                self.govern(id, Governance::Approve(true, anchor_read), backend, mailbox),
            );
        }
        let mut outside = self.say("host-extension-changed");
        let (_, parts) = self.ask_for_approval(id);
        outside.extend(parts);
        (Some("host-extension-changed"), outside)
    }

    /// Sends the change to the daemon. The truth will come from the
    /// re-requested catalog.
    pub(super) fn govern(
        &mut self,
        id: &str,
        change: Governance,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let opening = self.gen_extensions;
        let backend2 = Arc::clone(backend);
        let buzon2 = mailbox.clone();
        let id2 = id.to_owned();
        tokio::spawn(async move {
            let call = match change {
                Governance::Approve(v, digest) => backend2.plugin_set_approval(id2, v, digest),
                Governance::TurnOn(v) => backend2.plugin_set_enabled(id2, v),
                // Whether it had consent does not change what follows: the
                // catalog is re-requested regardless, and the question
                // already said so before the yes.
                Governance::Uninstall => {
                    Box::pin(async move { backend2.plugin_uninstall(id2).await.map(|_| ()) })
                }
            };
            let res = match tokio::time::timeout(DEADLINE_PLUGINS, call).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon2
                .send(Message::Background(Box::new(Background::Governed(
                    opening, res,
                ))))
                .await;
        });
        Vec::new()
    }

    /// The governance change answered.
    ///
    /// With an OK the local `bool` is NOT touched: the catalog is
    /// RE-REQUESTED. An optimism the daemon did not confirm is, on this
    /// screen, an assertion about who can read your files.
    pub(super) fn apply_governance(
        &mut self,
        opening: u64,
        res: &Result<(), Error>,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if opening != self.gen_extensions {
            return Vec::new();
        }
        // The outcome IS STATED even if the manager is already closed: a
        // grant that failed and that nobody was told about is the window
        // keeping quiet about who can read your files.
        let mut outside = match res {
            Ok(()) => self.say("host-extension-updated"),
            Err(e) => self.say(norte_frontend::error::error_key(e)),
        };
        // And the catalog is re-requested IN BOTH CASES. The failure
        // includes THIS side's timeout, which is not "it didn't happen" but
        // "it isn't known": the daemon might have granted the capabilities
        // and been slow to answer, and then leaving the row saying
        // "unapproved" is the same lie as local optimism, in pessimistic
        // form. The only thing that resolves an unknown is going to ask.
        if self.extensions.is_some() {
            self.rerequest_catalog(backend, mailbox);
        }
        // And the LISTINGS, for the same reason: whatever a decorator or a
        // plugin column said about each row said it with the old catalog.
        outside.extend(self.redecorate_all(backend, mailbox));
        outside
    }

    /// Forgets what the plugins said about EVERY open listing and requests
    /// it again: it is what follows any change of governance or of a
    /// plugin's settings. Turning off the icon decorator left the icons on
    /// rows until the next `cd`, and the reader concluded that turning off
    /// does not turn off.
    ///
    /// A batch in flight is not awaited: the decoration generation goes up,
    /// and when it lands it is dropped and re-requested. The row patch goes
    /// out RIGHT AWAY, with bare rows, so the screen does not keep showing
    /// what the manager just said is not there.
    pub(super) fn redecorate_all(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let slots: Vec<u32> = self.slots.keys().copied().collect();
        let mut outside = Vec::new();
        for slot in slots {
            if let Some(target_slot) = self.slots.get_mut(&slot) {
                target_slot.forget_adornos();
                target_slot
                    .pane
                    .set_decorations(std::collections::HashMap::new());
                target_slot
                    .pane
                    .set_plugin_columns(std::collections::HashMap::new());
            }
            self.adornar(slot, backend, mailbox);
            outside.push(self.patch_rows_of(slot));
        }
        outside
    }

    /// Requests the catalog again for the LIVE opening.
    pub(super) fn rerequest_catalog(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        self.request_extensions_catalog(backend, mailbox);
    }

    /// Requests the catalog for the manager, numbering the REQUEST.
    ///
    /// Two numbers and not one: the OPENING says whether the manager is
    /// still the same one, and the REQUEST which of several in flight is the
    /// newest. Two governance changes in a row request two catalogs within
    /// the same opening, and they can answer in any order — without the
    /// second number, the old one used to overwrite the new one and the
    /// "approved" column stayed behind forever.
    pub(super) fn request_extensions_catalog(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        let opening = self.gen_extensions;
        self.gen_catalog += 1;
        let request = self.gen_catalog;
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(DEADLINE_PLUGINS, backend.plugin_list()).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = mailbox
                .send(Message::Background(Box::new(Background::Catalog(
                    opening, request, res,
                ))))
                .await;
        });
    }

    /// A command's output arrived.
    ///
    /// Only the LAST one launched's: two commands in flight with the slow
    /// one landing later would paint one's output under the other's title,
    /// which on a pane that says who printed what is lying.
    pub(super) fn apply_output(
        &mut self,
        opening: u64,
        data: OutputRequested,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let OutputRequested {
            id,
            plugin,
            command,
            res,
        } = data;
        if opening != self.gen_output {
            return Vec::new();
        }
        match res {
            Ok(text) => {
                // THIRD-PARTY text: it is CLAMPED first — masking a
                // megabyte only to keep four thousand characters is doing
                // the whole job for nothing — it is split into lines, and
                // each one is masked on its own. That it was cut is STATED:
                // the receiver cannot infer it, because what arrives is
                // already short.
                let cropped: String = text.chars().take(MAX_OUTPUT).collect();
                let mut truncado = text.chars().nth(MAX_OUTPUT).is_some();
                let mut lines = Vec::new();
                let mut hostile = false;
                for line in cropped.lines().take(MAX_OUTPUT_LINES) {
                    let (paintable, marked) = norte_frontend::display_name(line.as_bytes());
                    hostile |= marked;
                    lines.push(clamp_display(paintable));
                }
                truncado |= cropped.lines().nth(MAX_OUTPUT_LINES).is_some();
                self.desktop.output = Some(crate::dto::ExtensionOutputView {
                    plugin: crate::dto::MaskedTextView {
                        text: plugin.0,
                        hostile: plugin.1,
                    },
                    plugin_id: id,
                    command: crate::dto::MaskedTextView {
                        text: command.0,
                        hostile: command.1,
                    },
                    lines,
                    text_hostile: hostile,
                    truncated: truncado,
                });
                let change = ViewChange::PluginOutput {
                    output: self.desktop.output.clone(),
                };
                vec![self.parche(vec![change])]
            }
            Err(e) => self.say(norte_frontend::error::error_key(&e)),
        }
    }

    /// Closes the output panel.
    pub(super) fn close_output(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.desktop.output = None;
        (
            self.applied(),
            vec![self.parche(vec![ViewChange::PluginOutput { output: None }])],
        )
    }

    /// A click on a row of the manager: selects it.
    pub(super) fn choose_extension(
        &mut self,
        row: u32,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(e) = self.extensions.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        e.point_at(row as usize);
        let (_, mut sends) = self.request_detail(backend, mailbox);
        let change = ViewChange::Extensions {
            extensions: self.vista_extensions(),
        };
        sends.push(self.parche(vec![change]));
        (self.applied(), sends)
    }
}
