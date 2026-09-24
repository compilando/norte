//! Copy, move and rename.
//!
//! Part of `controller`: these are `State` methods, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl State` split into pieces, so they use
// the same imports as the parent. Enumerating them here would be a
// forty-line list per file, in 32 files, that goes stale the moment the
// parent imports something — `super::*` tracks it on its own.
use super::tasks::Batch;
#[allow(clippy::wildcard_imports)]
use super::*;

impl State {
    /// The DIRECTORY a transfer goes to, or why there is none.
    ///
    /// The declared destination has to still be serviceable: exist, be
    /// visible, and not be itself — copying onto itself is not an operation.
    ///
    /// With no destination there are two DIFFERENT situations, and saying
    /// the same phrase for both sends whoever has three panes off to look
    /// for another one. The shared layer leaves the role UNSET when there
    /// are several candidates and none chosen (ADR 0058 D7): that is not
    /// "there is no other one", it is "pick which one".
    pub(super) fn directory_dest(&self) -> Result<VPath, &'static str> {
        self.slot_dest()
            .map(|id| self.slots[&id].pane.dir().clone())
    }

    /// The destination SLOT, with the same rule as
    /// [`Self::directory_dest`].
    ///
    /// Both through the same path: a comparison needs the slot (to navigate
    /// the right side) and a transfer needs its directory, and two ways of
    /// deciding "the other pane" are two places to drift apart.
    pub(super) fn slot_dest(&self) -> Result<u32, &'static str> {
        let active = self.active();
        let dest_id = self
            .roles
            .get(RoleId::Target)
            .map(|SlotId(id)| id)
            .filter(|id| *id != active && self.slots.contains_key(id) && !self.oculto(*id));
        if let Some(id) = dest_id {
            return Ok(id);
        }
        let candidates = self
            .slots
            .keys()
            .filter(|id| **id != active && !self.oculto(**id))
            .count();
        Err(if candidates > 1 {
            "host-no-target-designated"
        } else {
            "host-no-other-slot"
        })
    }

    /// Opens the confirmation for a copy or a move. Does NOT transfer.
    ///
    /// The source is the active slot's marks (or the cursor if there are
    /// none) and the destination is the DIRECTORY of the slot holding the
    /// `Target` role. Neither one is named by the renderer: it sends
    /// `pane.copy` and that is it. Same rule that left an image's bytes
    /// command with no parameter (ADR 0069), and for the same reason — a
    /// name coming from the webview is a name the webview can choose.
    /// Are there two batch entries whose NAMES are just one in the
    /// destination?
    ///
    /// It folds with the shared key under the DESTINATION's mode — the usual
    /// domain trap: casing and normalization are decided by where things are
    /// going, not by where they come from. With no mode yet (the slot has
    /// just landed, or the daemon has not answered) it does not fold: this
    /// is a client courtesy and the authority is the core.
    pub(super) fn two_marks_fold_the_same(&self, paths: &[VPath]) -> bool {
        let Some(modo) = self.slot_dest().ok().and_then(|id| self.pliegue_de(id)) else {
            return false;
        };
        if modo == norte_encoding::FoldMode::None {
            return false;
        }
        let mut vistas = std::collections::HashSet::new();
        paths
            .iter()
            .filter_map(|p| p.file_name())
            .any(|n| !vistas.insert(norte_encoding::name_key(n.as_bytes(), modo)))
    }

    /// A batch's two caps (#271), or `None` if it fits.
    ///
    /// Asked before opening any dialog: asking about something that will not
    /// be possible is worse than saying so up front.
    pub(super) fn batch_no_fits(&self, how_many: usize) -> Option<&'static str> {
        if how_many > MAX_TRANSFER_BATCH {
            return Some("host-batch-too-large");
        }
        // And that it fits in what the host RETAINS: eviction can only drop
        // terminal tasks, so a batch over a board already full of live ones
        // would have nowhere to land.
        if self.tasks.len().saturating_add(how_many) > MAX_TASKS_RETAINED {
            return Some("host-task-board-full");
        }
        None
    }

    /// WHAT a transfer to `dest` operates on, or the reason it cannot even
    /// be asked about. Returns `(source_dir, paths)`.
    ///
    /// The destination arrives as a PARAMETER since #284: it almost always
    /// comes from the shared role, but with only one listing on screen the
    /// reader picks it in the desktop's selector, and both ways have to go
    /// through the same checks.
    pub(super) fn transfer_operands(
        &self,
        dest: &VPath,
    ) -> Result<(VPath, Vec<VPath>), &'static str> {
        let dest = dest.clone();
        let source_dir = self.slot().pane.dir().clone();
        if source_dir == dest {
            // Both listings in the same place. The daemon would reject it
            // just the same, but opening a dialog that promises something
            // impossible is worse than saying so beforehand.
            //
            // BYTE FOR BYTE on purpose (#269): on a folding volume,
            // `/home/docs` and `/home/DOCS` are the same place and this
            // shortcut does NOT see that. Knowing that costs an
            // `fs.capabilities`, and this is BEFORE opening anything: the
            // one that probes the destination runs behind the dialog, so it
            // is no use here. The error on this side can only lean
            // PERMISSIVE: the authority is `norte_core::ops`, which DOES
            // fold (#215) and returns `InvalidPath`. Being stricter here
            // would actually break something: it would deny a legitimate
            // operation on a case-sensitive volume.
            return Err("host-same-directory");
        }
        // `marked_paths` already falls back to the cursor when there are no
        // marks: it is the single source of "what this operates on", and
        // duplicating that fallback here would be a second place for them to
        // drift apart.
        let paths: Vec<VPath> = self.slot().pane.marked_paths();
        if paths.is_empty() {
            return Err("msg-nothing-selected");
        }
        if let Some(motivo) = self.batch_no_fits(paths.len()) {
            return Err(motivo);
        }
        // Two marks that FOLD to the same name in the destination (#268): on
        // ext4 `README.txt` and `readme.txt` are two files, and on NTFS or
        // APFS they are one. Enqueuing both lets one win — which one is not
        // deterministic — and the other fail with no explanation over an
        // arbitrary member of the pair. With `CollisionPolicy::Fail` the
        // result is at least a visible error; the day the window offers a
        // choice to overwrite, the same batch silently loses a file.
        if self.two_marks_fold_the_same(&paths) {
            return Err("host-batch-folds-to-one");
        }
        // An entry with no last segment is a ROOT, and a root has no name to
        // compose in the destination. The whole batch is rejected instead of
        // skipping it: silently transferring "almost everything you asked
        // for" is exactly what a mutation must not do.
        if paths.iter().any(|p| p.file_name().is_none()) {
            return Err("host-cannot-transfer-root");
        }
        let _ = dest;
        Ok((source_dir, paths))
    }

    /// Opens the copy or move confirmation, resolving the destination.
    ///
    /// With only one listing on screen there is no destination pane, and
    /// until #284 that was the end of the road: the operation was refused
    /// and whoever had not split the window could not copy. Now the desktop
    /// is asked.
    pub(super) fn request_transfer(
        &mut self,
        mover: bool,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match self.directory_dest() {
            Ok(dest) => self.confirm_transfer(&dest, mover, backend, buzon),
            // With no OTHER slot to point at: the reader picks it outside.
            Err("host-no-other-slot") => self.request_destination_from_the_desktop(mover),
            Err(reason_key) => (
                ActionAck::Unavailable {
                    reason_key: reason_key.to_owned(),
                },
                Vec::new(),
            ),
        }
    }

    /// How many rows fit in the viewer, as measured by the renderer.
    ///
    /// At least one: a viewer with zero rows paints nothing and its
    /// pagination would divide by zero.
    pub(super) fn pin_viewer_rows(
        &mut self,
        rows: u32,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.visor_rows = Some(usize::try_from(rows).unwrap_or(1).max(1));
        let change = ViewChange::Viewer {
            viewer: self.vista_visor(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Asks the DESKTOP for the reader to choose the destination (#284).
    ///
    /// Only the VERB is remembered — copy or move — not the operands: when
    /// the answer comes back, they are recomputed from the state at that
    /// point. Freezing the marks here would promise an operation over a
    /// listing the reader could have changed while the selector was open.
    pub(super) fn request_destination_from_the_desktop(
        &mut self,
        mover: bool,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // The pane's directory is only the SUGGESTION for where to open the
        // selector, and that is why it is not required to be local: what the
        // selector returns is always a folder on this machine, and copying
        // from an `sftp://` to a local folder is a legitimate operation the
        // core has always done. With a remote pane, whoever runs it opens
        // wherever it can — the suggestion is lost, the operation is not.
        let from = self.slot().pane.dir().clone();
        if !self.nativo(crate::dto::NativeEffect::PickDirectory { from }) {
            return Self::without_desktop();
        }
        self.dest_pending = Some(mover);
        (self.applied(), self.say("host-pick-destination"))
    }

    /// The desktop's selector came back (#284).
    ///
    /// `None` = it closed without choosing, and then nothing happens:
    /// canceling is an answer. With a path, it is confirmed like any other
    /// transfer — which means the destination gets SHOWN before a single
    /// byte moves, which is what bounds the risk of the path having gone
    /// through the renderer.
    pub(super) fn dest_chosen(
        &mut self,
        path: Option<String>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(mover) = self.dest_pending.take() else {
            // Nobody asked for a destination: an answer that answers no
            // question is not interpreted.
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        let Some(native) = path else {
            return (self.applied(), Vec::new());
        };
        let Some(dest) = norte_frontend::shell::vpath_from_native_path(&native) else {
            let outside = self.say("host-bad-destination");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-bad-destination".to_owned(),
                },
                outside,
            );
        };
        self.confirm_transfer(&dest, mover, backend, buzon)
    }

    /// Files dropped from the desktop arrived (#283).
    ///
    /// Does not copy: it opens the same confirmation as copy, with the
    /// destination in its field and the names masked. The list is built by
    /// ANOTHER process, so showing it before writing is not a courtesy — it
    /// is the reader's only chance to see that what arrived is not what was
    /// dragged.
    ///
    /// Whatever does not convert to a `VPath` is discarded, and the trim is
    /// STATED: nine of ten remain and copying without saying so would lie
    /// about the batch.
    pub(super) fn soltados(
        &mut self,
        paths: &[String],
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let arrived = paths.len();
        let usables: Vec<VPath> = paths
            .iter()
            .filter_map(|p| norte_frontend::shell::vpath_from_native_path(p))
            // A root has no name to give it in the destination, and the send
            // would silently skip it: it is dropped here, where it can still
            // be counted.
            .filter(|v| v.file_name().is_some())
            .collect();
        if usables.is_empty() {
            let key = if arrived == 0 {
                "host-drop-empty"
            } else {
                "host-drop-unusable"
            };
            let outside = self.say(key);
            return (
                ActionAck::Unavailable {
                    reason_key: key.to_owned(),
                },
                outside,
            );
        }
        if let Some(motivo) = self.batch_no_fits(usables.len()) {
            let outside = self.say(motivo);
            return (
                ActionAck::Unavailable {
                    reason_key: motivo.to_owned(),
                },
                outside,
            );
        }
        // Same reason as in a normal copy (#268): two that fold to the same
        // name let one win without saying which. And here the reader did
        // not choose the batch by marking, so finding it out afterward would
        // be even less explicable.
        if self.two_marks_fold_the_same(&usables) {
            let outside = self.say("host-batch-folds-to-one");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-batch-folds-to-one".to_owned(),
                },
                outside,
            );
        }
        // The ACTIVE pane, and without requiring it to be local: uploading
        // what is dragged from the desktop is the convenient case, and the
        // core has always copied between providers.
        let dest = self.slot().pane.dir().clone();
        let dest_line = Self::path_line(&dest);
        let body: Vec<crate::dto::DialogLine> = usables
            .iter()
            .take(Self::MAX_LINES_DIALOG)
            .map(Self::path_line)
            .collect();
        // The trim counts against what ARRIVED, not against what could be
        // converted: "16 of 40 shown" has to stay true when four of those
        // 40 fell out along the way.
        let note = self.truncation_note(body.len(), arrived.max(usables.len()));
        let hostile_outside = norte_frontend::overflow_hostile(&usables, body.len());
        let id = ModalId(self.next_modal);
        self.next_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-drop-title".to_owned(),
            destination: Some(dest_line),
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body,
            overflow_note: note,
            overflow_hostile: hostile_outside,
            choices: vec![
                DialogChoice {
                    id: "confirm".to_owned(),
                    label_key: "dialog-confirm".to_owned(),
                    destructive: false,
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
            dest_check: crate::dto::DestCheckView::Checking,
        };
        self.dialogs.push(Dialog {
            id,
            vista,
            typed: Typed::Text(String::new()),
            recognized: true,
            on_confirm: Some(Pending::Release {
                paths: usables,
                dest: dest.clone(),
            }),
        });
        // Here too, and it is the path that can LEAST afford to skip it: the
        // operand list is built by another process, and this box looks the
        // same as a probed copy's — so the line's absence would read the
        // same way. With no total: dropped files are in no listing, so only
        // the confinement one can come out, which is the one that matters
        // here.
        self.sondear_dest(id, dest, None, backend, buzon);
        let change = ViewChange::Dialogs {
            dialogs: self.dialog_views(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Asks the DESTINATION what needs to be known before saying yes:
    /// whether it fits (#149) and whether it knows to secure what gets
    /// written to it (#164).
    ///
    /// Both are I/O, so the dialog opens WITHOUT them and this fills them in
    /// when they come back. Waiting for them would leave F5 painting nothing
    /// against a slow SFTP, which is worse than a line that appears half a
    /// second late: what the human has in front of them meanwhile is the
    /// list of what is about to be copied, which is what they came to read.
    ///
    /// **The two fail differently, and it is deliberate.** Space swallows
    /// the failure: not being able to enumerate volumes cannot paint an
    /// alarm, and "I don't know" is said by staying quiet — `space::warning`'s
    /// contract. Confinement does not: there, silence MEANS "this destination
    /// secures its writes", so swallowing the failure would be asserting it
    /// without knowing, which is fail-open on a security line. If it is not
    /// known, it is flagged.
    ///
    /// Capabilities come from the slot's CACHE when the path matches, and
    /// from a round trip when it does not. Not to save the RPC: to shorten
    /// the window during which the dialog is painted without the answer. In
    /// the normal case — two panes, F5 — the destination is a slot that
    /// already has them, so the line comes out on the FIRST paint. The round
    /// trip is needed just the same because the destination is not always a
    /// slot: with a single listing the reader picks it on the desktop
    /// (#284).
    ///
    /// And it sends the `Background` ALWAYS, even with an empty list. Staying
    /// quiet when there is nothing to say would leave the dialog saying
    /// "checking" forever, and then "I asked and it's clean" would become
    /// indistinguishable from "I haven't asked yet" again — exactly what
    /// [`crate::dto::DestCheckView`] exists to keep apart.
    fn sondear_dest(
        &self,
        id: ModalId,
        dest: VPath,
        total: Option<u64>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) {
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        let lang = self.lang;
        let sabidas = self.path_caps(&dest);
        tokio::spawn(async move {
            let free = match total {
                // With no total there is no space question to ask, and
                // enumerating volumes only to throw away the answer is I/O
                // for nothing.
                None => None,
                Some(_) => backend
                    .volumes()
                    .await
                    .ok()
                    .and_then(|vols| norte_frontend::space::free_for(&dest, &vols)),
            };
            let caps =
                match sabidas {
                    Some(c) => c,
                    None => backend.capabilities(dest.clone()).await.unwrap_or(
                        norte_proto::Capabilities {
                            flags: norte_proto::CapabilityFlags::empty(),
                            max_path: None,
                        },
                    ),
                };
            let notices: Vec<String> = norte_frontend::space::warning(total, free, lang)
                .into_iter()
                .chain(norte_frontend::confine::warning(caps, lang))
                .collect();
            let _ = buzon
                .send(Message::Background(Box::new(
                    Background::DestinationNotices(id, notices),
                )))
                .await;
        });
    }

    /// Attaches the warnings to the dialog they belong to, if it is still
    /// open.
    ///
    /// By id and not "the topmost one": an `esc` and another dialog fit
    /// between asking and answering, and attaching one destination's warning
    /// to the question about something else is worse than not warning at
    /// all.
    pub(super) fn destination_notices(
        &mut self,
        id: ModalId,
        notices: Vec<String>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(d) = self.dialogs.iter_mut().find(|d| d.id == id) else {
            return Vec::new();
        };
        d.vista.dest_check = crate::dto::DestCheckView::Done { warnings: notices };
        let change = ViewChange::Dialogs {
            dialogs: self.dialog_views(),
        };
        vec![self.parche(vec![change])]
    }

    /// The confirmation itself, with the destination already resolved.
    pub(super) fn confirm_transfer(
        &mut self,
        dest: &VPath,
        mover: bool,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let active = self.active();
        let dest = dest.clone();
        let (source_dir, paths) = match self.transfer_operands(&dest) {
            Ok(t) => t,
            Err(reason_key) => {
                return (
                    ActionAck::Unavailable {
                        reason_key: reason_key.to_owned(),
                    },
                    Vec::new(),
                );
            }
        };
        // The destination goes in ITS OWN FIELD, not as a line with an
        // arrow: a directory can be named `docs → /home/DELETE` and that
        // arrow is legitimate, it is not masked and not flagged, so the line
        // would read as two paths and whoever confirms would believe they
        // are sending their files to the second one (`arrow_join_spoof`
        // fixture in the canonical corpus).
        let dest_line = Self::path_line(&dest);
        // And the sources, masked and clamped the same as the listing: these
        // names are controlled by whoever has written to the directory.
        let body: Vec<crate::dto::DialogLine> = paths
            .iter()
            .take(Self::MAX_LINES_DIALOG)
            .map(Self::path_line)
            .collect();
        let note = self.truncation_note(body.len(), paths.len());
        let hostile_outside = norte_frontend::overflow_hostile(&paths, body.len());
        let id = ModalId(self.next_modal);
        self.next_modal += 1;
        let vista = DialogView {
            id,
            title_key: if mover {
                "modal-move-title"
            } else {
                "modal-copy-title"
            }
            .to_owned(),
            destination: Some(dest_line),
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body,
            overflow_note: note,
            overflow_hostile: hostile_outside,
            choices: vec![
                DialogChoice {
                    id: "confirm".to_owned(),
                    label_key: "dialog-confirm".to_owned(),
                    // Neither copy nor move is marked destructive, and it is
                    // a deliberate choice: `destructive` is what makes
                    // `Enter` pick cancel, and F5/F6 are the two most-pressed
                    // keys of an orthodox manager. What destroys is
                    // deletion, and that one does carry it.
                    destructive: false,
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
            dest_check: crate::dto::DestCheckView::Checking,
        };
        // What is about to be written, with the SHARED rule: it is all or
        // nothing, because a directory carries no size in the listing and
        // adding up only what does would warn with a number smaller than
        // the real one.
        let total = norte_frontend::space::total_to_write(self.slot().pane.entries(), &paths);
        self.dialogs.push(Dialog {
            id,
            vista: vista.clone(),
            typed: Typed::Text(String::new()),
            recognized: true,
            on_confirm: Some(Pending::Transferir {
                source: active,
                source_dir,
                paths,
                dest: dest.clone(),
                mover,
            }),
        });
        self.sondear_dest(id, dest, total, backend, buzon);
        let change = ViewChange::Dialogs {
            dialogs: self.dialog_views(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Resolves the confirmed name and enqueues the rename, or says why not.
    pub(super) fn confirm_rename(
        &mut self,
        from: &VPath,
        siembra: &str,
        written: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        match Self::bytes_del_rename(from, siembra, written) {
            Ok(dest) => {
                Self::launch_rename(from.clone(), dest, backend, buzon);
                (None, Vec::new())
            }
            Err(key) => {
                // The reason comes back so the ACK can say it, not just the
                // status bar: a renderer receiving `Applied` believes the
                // operation succeeded, and the same surface answered
                // `Unavailable` when the rejection was for the marks.
                self.status.message = Some(clamp_display(norte_i18n::t_in(self.lang, key)));
                let change = ViewChange::Status(self.status.clone());
                (Some(key), vec![self.parche(vec![change])])
            }
        }
    }

    /// Enqueues ONE rename's `fs.move`, with the destination already
    /// composed.
    ///
    /// Separate from [`Self::launch_transfer`] because a rename's
    /// destination is a FULL PATH and a transfer's is a DIRECTORY the
    /// source's name is composed onto. Passing one for the other would
    /// rename to `new/old-name`, exactly the kind of bug a parameter with
    /// two meanings produces.
    ///
    /// Same wire verb, same journal entry and same undo path as move: what
    /// changes is the question, not the effect.
    pub(super) fn launch_rename(
        from: VPath,
        to: VPath,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) {
        // The directory it leaves and the one it arrives at are the SAME, so
        // one single entry: re-listing it twice would be asking for the same
        // listing twice.
        let afectados: Vec<VPath> = from.parent().into_iter().collect();
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let message = match backend
                // A rename is not enqueued: it is a single step and moves no
                // bytes from one place to another.
                .move_(from, to, norte_proto::CollisionPolicy::Fail, false)
                .await
            {
                Ok(task) => Message::TaskNew(Box::new((task, afectados, None))),
                Err(e) => Message::TaskFailed(Box::new(e)),
            };
            let _ = buzon.send(message).await;
        });
    }

    /// Enqueues the batch's Tasks and hooks their progress to the actor.
    ///
    /// The destination's name is composed HERE, with the source's last
    /// segment as is: bytes, unnormalized and never through the screen. A
    /// name that has gone to the webview and back is a different name (ADR
    /// 0061). What byte-for-byte composition CANNOT resolve is a name legal
    /// at the source and illegal at the destination (`CON`, a trailing dot,
    /// a `:` going from ext4 to NTFS): that is the destination provider's
    /// business, and it is tracked in issue #217.
    ///
    /// The collision policy is `Fail`, the wire's safe default: if the
    /// destination exists, the Task fails and the board says so. Overwriting
    /// or renaming are the reader's decisions, and this window has nowhere
    /// yet to make them — choosing for them would be the kind of silence
    /// that deletes files.
    /// Opens the batch and launches the send. The two always go together.
    ///
    /// The batch opens BEFORE launching, with the count it is about to ask
    /// for: the count has to exist before the first outcome arrives, which
    /// with a task born terminal can be before the send loop has even asked
    /// for the second one. A single one is not a batch: its outcome is
    /// already stated in its row and its rejection in the status bar, with
    /// the error's typed phrase.
    pub(super) fn send_batch(
        &mut self,
        paths: &[VPath],
        source_dir: &VPath,
        dest: &VPath,
        mover: bool,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) {
        self.batch = (paths.len() > 1).then(|| Batch {
            total: paths.len(),
            ..Batch::default()
        });
        // The reinterpretation is captured HERE, with the slot still in
        // front: a collision arrives asynchronously and on top of whatever
        // the reader is doing, so reading it on arrival can give the wrong
        // one's.
        let enc = self.slot().pane.name_encoding();
        Self::launch_transfer(
            paths,
            (source_dir, dest),
            mover,
            enc,
            self.enqueue,
            backend,
            buzon,
        );
    }

    pub(super) fn launch_transfer(
        paths: &[VPath],
        path_list: (&VPath, &VPath),
        mover: bool,
        enc: Option<norte_encoding::NameEncoding>,
        a_la_cola: bool,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Message>,
    ) {
        let (source_dir, dest) = path_list;
        // The directories the outcome leaves out of date. In a copy, only
        // the destination; in a move, also where it came from — and the
        // source one is taken from the SLOT, not from each entry's parent:
        // the parent is written by the provider and the slot can come from
        // the config or the session, so under NFD against NFC, or against a
        // server with no case distinction, they are two strings for the
        // same place and the refresh's byte-for-byte comparison would not
        // find the pane (ADR 0061). Both get noted: one of them matches.
        let mut afectados = vec![dest.clone()];
        if mover {
            afectados.push(source_dir.clone());
        }
        let mut jobs: Vec<(VPath, VPath)> = Vec::with_capacity(paths.len());
        for path in paths {
            let Some(name) = path.file_name() else {
                // Impossible here: `request_transfer` rejects the whole
                // batch if any entry is a root. It is checked anyway because
                // the alternative is an `unwrap` on a mutation's path.
                continue;
            };
            if mover
                && let Some(padre) = path.parent()
                && !afectados.contains(&padre)
            {
                afectados.push(padre);
            }
            jobs.push((path.clone(), dest.join(name.clone())));
        }
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        // ONE send task for the whole batch, and the calls IN SERIES. A
        // `spawn` per entry would open as many simultaneous RPCs as there
        // were marks: marking a few thousand files and pressing F5 is the
        // normal flow of an orthodox manager, and against SFTP that is not a
        // copy, it is a denial of service against the daemon itself. In
        // series the daemon can still do the work in parallel if it wants
        // to; what is capped is how many requests are in flight at once.
        tokio::spawn(async move {
            for (from, to) in jobs {
                // The ORIGINAL pair travels with the task (#274): if this
                // collides, it is the only thing that can be retried with a
                // different policy. Rebuilding it from progress does not
                // work — that says which file is currently in flight, not
                // what was requested.
                let retry = Retry {
                    from: from.clone(),
                    to: to.clone(),
                    mover,
                    enc,
                };
                let encolada = if mover {
                    backend
                        .move_(from, to, norte_proto::CollisionPolicy::Fail, a_la_cola)
                        .await
                } else {
                    backend
                        .copy(from, to, norte_proto::CollisionPolicy::Fail, a_la_cola)
                        .await
                };
                let message = match encolada {
                    Ok(task) => Message::TaskNew(Box::new((task, afectados.clone(), Some(retry)))),
                    // To the batch's COUNT, not to the status bar: N
                    // rejections used to be N messages of which only the
                    // last one survived (#271).
                    Err(e) => Message::BatchTaskRejected(Box::new(e)),
                };
                if buzon.send(message).await.is_err() {
                    // The actor is no longer there: whatever remains of the
                    // batch matters to nobody, and continuing to request it
                    // would matter.
                    return;
                }
            }
        });
    }
}
