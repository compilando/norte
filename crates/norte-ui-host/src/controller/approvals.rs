//! The policy approvals that arrive from the daemon.
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
    /// Requests this location's attribute catalogue, if needed.
    ///
    /// Only if there are `attr:` columns configured and its scheme's
    /// catalogue is not already held: asking for a catalogue nobody is going
    /// to read is one more trip on every `cd`.
    pub(super) fn request_catalog(
        &self,
        dir: &VPath,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        if self.attrs_de(dir).is_empty() || self.catalogos.contains_key(dir.scheme()) {
            return;
        }
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        let dir = dir.clone();
        let scheme = dir.scheme().to_owned();
        tokio::spawn(async move {
            // A catalogue that does not arrive breaks nothing: the cells are
            // painted opaque, which is exactly what is known about them.
            if let Ok(catalog) = backend.attr_catalog(dir).await {
                let _ = mailbox
                    .send(Message::Catalog(Box::new((scheme, catalog))))
                    .await;
            }
        });
    }

    /// What is being requested, in one line (#314).
    ///
    /// For every op but one it is the op's name: approving "copy these
    /// twelve" IS the decision. A `set-mode` is not, because two with the
    /// same paths and different modes mean opposite things, so the mode goes
    /// HERE, with the subject — between path lines, a path can impersonate
    /// any other line, and this is half the decision.
    pub(super) fn approval_subject(
        &self,
        req: &norte_proto::methods::PolicyApprovalRequired,
    ) -> String {
        let base = match req.detail.mode {
            Some(mode) => norte_i18n::ta_in(
                self.lang,
                "modal-approval-op-mode",
                &[
                    ("op", &req.op),
                    ("mode", &norte_frontend::chmod::format_mode(mode)),
                ],
            ),
            None => req.op.clone(),
        };
        // #315: and the SCOPE. A recursive one over a root arrives with
        // `paths_total = 1`, so without this the question said "set-mode over
        // 1 path" and what was approved was the whole tree.
        if !req.detail.recursive {
            return base;
        }
        let tail = match req.detail.dir_mode {
            Some(dir) => norte_i18n::ta_in(
                self.lang,
                "modal-approval-recursive-dirs",
                &[("mode", &norte_frontend::chmod::format_mode(dir))],
            ),
            None => norte_i18n::t_in(self.lang, "modal-approval-recursive"),
        };
        format!("{base} {tail}")
    }

    /// Opens the dialog for an agent op waiting on a decision.
    ///
    /// The paths arrive REDACTED from the server and are display only: they
    /// are never reparsed into an operation — the real op is tied to the
    /// `approval_id` —, and they are painted with canonical sanitizing
    /// because they are controlled by whoever requested the operation.
    // TODO(translation): review — this paragraph describes
    /// `open_approval` below, but it is attached, with no blank line in
    /// between, to the doc comment for `approve_or_deny` right after it; it
    /// looks like a stale fragment left by an earlier edit.
    /// An agent approval's two answers.
    ///
    /// Outside the constructor because the constructor no longer had room,
    /// and separate because these two labels are not a normal dialog's:
    /// `approve` and `deny` are named differently from `confirm`/`cancel` on
    /// purpose — on a security surface, "confirm" and "approve" should not be
    /// able to get confused in a renderer.
    fn approve_or_deny() -> Vec<DialogChoice> {
        vec![
            DialogChoice {
                id: "approve".to_owned(),
                label_key: "dialog-approve".to_owned(),
                // Approving an agent's mutation IS destructive: the renderer
                // paints it as such, and Enter does not trigger it alone
                // because there is no default answer.
                destructive: true,
            },
            DialogChoice {
                id: "deny".to_owned(),
                label_key: "dialog-deny".to_owned(),
                destructive: false,
            },
        ]
    }

    pub(super) fn open_approval(
        &mut self,
        req: &norte_proto::methods::PolicyApprovalRequired,
        mailbox: &mpsc::Sender<Message>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // The SAME approval can arrive twice: the SDK resyncs
        // `policy.pending` on every reconnection, and whatever is still alive
        // comes back through the channel. Two dialogs are two answers, and
        // the second one lands on an id the daemon already closed.
        if self.dialogs.iter().any(|d| {
            matches!(
                d.on_confirm,
                Some(Pending::Decide { approval_id, .. }) if approval_id == req.approval_id
            )
        }) {
            return Vec::new();
        }
        // The session that requested it is NOTED, even if the dialog never
        // ends up opening for whatever reason: it is the ONLY thing that
        // names an agent in the whole protocol, and without that note there
        // is no way to offer undoing what it did other than typing its id by
        // hand (#276).
        let mut agents_notice = Vec::new();
        if let Some(session) = req.session.as_deref() {
            self.agency.sessions.vista(session, &req.op);
            // And it is REPAINTED if the panel is open. The list changes with
            // NO gesture — this request reorders it — and a renderer that is
            // not told keeps painting the previous order: the row the reader
            // sees highlighted stops being the one the host has selected, and
            // `u` undoes another session's work.
            if self.agency.panel {
                agents_notice.push(self.parche(vec![ViewChange::Agents {
                    agents: self.vista_agents(),
                }]));
            }
        }
        // These paths arrive from the daemon as already-redacted TEXT, not as
        // `VPath`, so the masking is the string kind and the flag is
        // computed by comparing: if masking changed something, what is read
        // is not what is there, and whoever approves has to see it.
        let line = |text: &str| {
            let masked = norte_encoding::mask_terminal_hazards(text);
            // TWO reasons to flag, and the second is the one that was
            // missing: these paths arrive REDACTED from the daemon, which
            // already ran the bytes through `display_lossy` — controls, bidi
            // overrides and invalid bytes are already U+FFFD — so comparing
            // against the original detects none of that, and the flag did
            // not fire exactly on the most dangerous class. On top of that it
            // was inconsistent: a `zwsp` DID turn it on, because the
            // daemon's lossy pass does not touch it.
            //
            // The replacement character IS the signal that what is read is
            // not what is there. What was there cannot be recovered — that
            // is why the daemon sends text and not a `VPath` — but it can
            // still say it is not faithful. The RULE lives in the shared
            // crate ever since the clamp summary needed it too: the terminal
            // used to answer the same question with a different function,
            // which is how two surfaces end up flagging different things
            // over the same paths.
            let hostile = norte_frontend::redacted_hostile(text);
            crate::dto::DialogLine {
                text: clamp_display(masked),
                hostile,
            }
        };
        // The body is ONLY the paths: the renderer numbers them by position,
        // which is a label no file name can write. Everything else — what is
        // being asked, who asks it, when it expires — goes in its own
        // fields, for the same reason as a transfer's destination: between
        // path lines, a path impersonates any other line.
        let body: Vec<crate::dto::DialogLine> = req
            .paths
            .iter()
            .take(Self::MAX_LINES_DIALOG)
            .map(|p| line(p))
            .collect();
        let subject = line(&self.approval_subject(req));
        // Who is asking is the FIRST thing needed to decide, and it used to
        // be dropped: the title says "agent approval" and without this there
        // is no way to know which agent.
        let asker = req.session.as_deref().map(line);
        // If the list arrives TRUNCATED it has to be said: approving
        // believing there are three paths when there are a thousand is
        // approving something else (0.36.0). And there are TWO truncations:
        // the daemon's (`paths_total`) and our own. The honest count is the
        // larger of the two.
        //
        // The phrase goes in `overflow_note` and not as one more body line,
        // for the same reason a transfer's destination has its own field:
        // between path lines, a path can impersonate it. It used to be a
        // line, and on top of that it quoted `modal-approval-truncated`, a
        // Fluent key that does not exist in any language — so a truncated
        // batch painted the raw identifier.
        let total = std::cmp::max(req.paths_total, req.paths.len() as u64);
        let shown = req.paths.len().min(Self::MAX_LINES_DIALOG);
        let note = self.truncation_note(shown, usize::try_from(total).unwrap_or(usize::MAX));
        // How much time is left, SAID and in its own field. A decision with
        // an expiry that does not show it reads as one that waits forever,
        // and whoever comes back later presses approve on something the
        // daemon already denied.
        //
        // With `ttl_ms == 0` — UNKNOWN: a pending item rebuilt by
        // `policy.pending`'s resync does not carry the remaining TTL — it
        // says it is not known, instead of staying quiet: staying quiet
        // leaves the dialog in front inviting approval on an id the daemon
        // may have reaped a while ago. And without a deadline line, a file
        // named "expires in 3600 s" would be the only one that looked like
        // one.
        let deadline = Some(if req.ttl_ms > 0 {
            clamp_display(norte_i18n::ta_in(
                self.lang,
                "modal-approval-ttl",
                &[("s", &req.ttl_ms.div_ceil(1000).to_string())],
            ))
        } else {
            clamp_display(norte_i18n::t_in(self.lang, "modal-approval-ttl-unknown"))
        });
        // And WHEN it expires, so the renderer counts instead of repeating a
        // frozen phrase (#279). Only with a known TTL: counting down from a
        // made-up deadline would be worse than not counting.
        let expires_at = (req.ttl_ms > 0)
            .then(|| i64::try_from(req.ttl_ms).ok().map(|ms| now_ms() + ms))
            .flatten();
        let id = ModalId(self.next_modal);
        self.next_modal += 1;
        let view = DialogView {
            id,
            title_key: "modal-approval-title".to_owned(),
            destination: None,
            subject: Some(subject),
            asker,
            deadline,
            deadline_at_ms: expires_at,
            body,
            overflow_note: note,
            // And whether anything TRUNCATED would paint altered. The
            // terminal has always said so in its summary and this window did
            // not, over the same paths: the answer is now the same function.
            overflow_hostile: norte_frontend::overflow_hostile_redacted(&req.paths, shown),
            choices: Self::approve_or_deny(),
            input: None,
            input_hostile: false,
            input_secret: false,
            fields: Vec::new(),
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        let dropped = self.stack_dialog(Dialog {
            id,
            vista: view.clone(),
            typed: Typed::Text(String::new()),
            // It opens ON ITS OWN: an agent's op brings it, not a key.
            recognized: false,
            on_confirm: Some(Pending::Decide {
                approval_id: req.approval_id,
                session: req.session.clone(),
            }),
        });
        // And its expiry is scheduled. The daemon stops accepting the id once
        // the TTL runs out: a dialog that stayed in front would invite
        // approving into the void, and whoever did would be left believing
        // they authorized what actually ended up denied by silence.
        if req.ttl_ms > 0 {
            let mailbox = mailbox.clone();
            let approval_id = req.approval_id;
            let ttl = std::time::Duration::from_millis(req.ttl_ms);
            tokio::spawn(async move {
                tokio::time::sleep(ttl).await;
                let _ = mailbox.send(Message::ApprovalExpired(approval_id)).await;
            });
        }
        let change = ViewChange::Dialogs {
            dialogs: self.dialog_views(),
        };
        let mut outgoing = vec![self.parche(vec![change])];
        outgoing.extend(dropped);
        outgoing
    }

    /// An approval's TTL ran out: its dialog closes and it is said.
    ///
    /// `policy.decide` is not sent: the daemon already resolved it on its
    /// own — an expired TTL is a denial —, and answering about a closed id
    /// only produces an error that means nothing to whoever reads it.
    pub(super) fn expires_approval(&mut self, approval_id: u64) -> Vec<BridgeEnvelope<UiUpdate>> {
        let before = self.dialogs.len();
        self.dialogs.retain(|d| {
            !matches!(
                d.on_confirm,
                Some(Pending::Decide { approval_id: id, .. }) if id == approval_id
            )
        });
        if self.dialogs.len() == before {
            // It had already been answered: the expiry arrives and there is
            // nothing to close. It is not an error, and nothing is said.
            return Vec::new();
        }
        let change = ViewChange::Dialogs {
            dialogs: self.dialog_views(),
        };
        let mut outgoing = vec![self.parche(vec![change])];
        // NAMES the one that expired (#279). With two stacked, "the approval
        // expired" does not say which one closed on its own nor which is
        // still waiting.
        outgoing.extend(self.say_with("msg-approval-expired", &[("id", &approval_id.to_string())]));
        outgoing
    }
}
