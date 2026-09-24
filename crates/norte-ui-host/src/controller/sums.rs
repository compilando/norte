//! Checksums: requesting them, queuing them, and reading their report.
//!
//! Part of `controller`: these are methods of `Estado`, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl Estado` split into pieces, so they use
// the same imports as the parent. Listing them here would be a forty-line
// list per file, across 32 files, that goes out of sync the moment the
// parent imports something — `super::*` keeps it in sync on its own.
#[allow(clippy::wildcard_imports)]
use super::*;

/// A QUEUED batch of checksums, still without an id (#311).
///
/// It exists between when `checksum` is sent and the mailbox returns the
/// Task. It is a type and not an `Option<Option<_>>` because "there is no
/// batch" and "there is one that compares against nothing" are two different
/// things, and nesting two options to say so reads badly where it matters.
pub(super) struct SumasEncoladas {
    /// What the sums file published, if this is a verification.
    pub(super) publicado: Option<Publicado>,
}

/// The sums file, read exactly as needed to judge it (#311).
///
/// Twin of the terminal's, and with the same three fields for the same
/// reason: the lines in their order, where each one landed in the request,
/// and how many were not understood — which is what forbids saying "all
/// correct".
pub(super) struct Publicado {
    lines: Vec<norte_frontend::checksums::SumLine>,
    asked: Vec<Option<usize>>,
    refused: usize,
}

impl Estado {
    /// The checksum Task finished: its report is requested (#311).
    ///
    /// The same guards as sync's, and for the same reason: the CLASS, the
    /// connection EPOCH, and idempotency — a reconnection re-announces the
    /// terminal, and this is an RPC.
    pub(super) fn pedir_informe_de_sumas(
        &mut self,
        p: &norte_proto::TaskProgress,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) {
        let Some(s) = self.sumas.as_ref() else {
            return;
        };
        if s.task != p.task_id
            || s.epoca_conexion != self.epoca_conexion
            || !matches!(p.kind, norte_proto::TaskKind::Checksum)
            || s.informe_pedido
        {
            return;
        }
        if let Some(s) = self.sumas.as_mut() {
            s.informe_pedido = true;
        }
        let state = p.state.clone();
        let id = p.task_id;
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            let report = backend.checksum_report(id).await;
            let _ = mailbox
                .send(Mensaje::Fondo(Box::new(Fondo::InformeDeSumas(
                    id,
                    state,
                    Box::new(report),
                ))))
                .await;
        });
    }

    /// The checksum report arrived: it is judged and the dialog opens (#311).
    ///
    /// **A PARTIAL report is not compared against anything.** Cancelling
    /// leaves `pending` above zero with the Task already terminal, and
    /// judging that would accuse — "does not match or is missing" — files
    /// nobody ever got to read, which is the worst possible error in the one
    /// tool whose job is to verify.
    pub(super) fn informe_de_sumas(
        &mut self,
        task: norte_proto::TaskId,
        state: &norte_proto::TaskState,
        report: Result<norte_proto::methods::FsChecksumReportResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        use norte_frontend::checksums;

        let Some(sumas) = self.sumas.take().filter(|s| s.task == task) else {
            return Vec::new();
        };
        let Ok(report) = report else {
            return self.decir("err-checksum-failed");
        };
        if *state != norte_proto::TaskState::Completed || report.pending > 0 {
            return self.decir("err-checksum-partial");
        }
        let computed: Vec<checksums::Computed> = report
            .entries
            .iter()
            .map(|e| (e.digest.clone(), e.miss))
            .collect();
        let (lines, copyable, message) = if let Some(published) = sumas.publicado {
            let verdicts = checksums::judge(&published.lines, &published.asked, &computed);
            let rows: Vec<crate::dto::DialogLine> = published
                .lines
                .iter()
                .zip(&verdicts)
                .map(|(line, v)| self.fila_de_suma(&line.name, None, Some(*v)))
                .collect();
            let message = match checksums::summarize(&verdicts, published.refused) {
                checksums::Summary::Unreadable { n, refused } => self.decir_con(
                    "msg-checksum-unreadable-lines",
                    &[("n", &n.to_string()), ("refused", &refused.to_string())],
                ),
                checksums::Summary::AllOk { n } => {
                    self.decir_con("msg-checksum-all-ok", &[("n", &n.to_string())])
                }
                checksums::Summary::Bad { n } => {
                    self.decir_con("msg-checksum-bad", &[("n", &n.to_string())])
                }
            };
            // A verification carries no digests: there is no list to copy.
            (rows, Vec::new(), message)
        } else {
            let rows: Vec<crate::dto::DialogLine> = report
                .entries
                .iter()
                .map(|e| {
                    let name = e
                        .path
                        .file_name()
                        .map(|s| s.as_bytes().to_vec())
                        .unwrap_or_default();
                    let verdict = e.miss.map(|m| match m {
                        norte_proto::methods::ChecksumMiss::NotAFile => {
                            checksums::Verdict::NotAFile
                        }
                        _ => checksums::Verdict::Missing,
                    });
                    self.fila_de_suma(&name, e.digest.as_deref(), verdict)
                })
                .collect();
            let copyable: Vec<checksums::Computed> = computed.clone();
            (rows, copyable, Vec::new())
        };
        // What would be copied, in BYTES and with coreutils' escaping: a name
        // does not have to be text (rule 1).
        let to_copy: Vec<(Vec<u8>, Option<String>)> = report
            .entries
            .iter()
            .zip(copyable)
            .map(|(e, (digest, _))| {
                (
                    e.path
                        .file_name()
                        .map(|s| s.as_bytes().to_vec())
                        .unwrap_or_default(),
                    digest,
                )
            })
            .collect();
        let bytes = checksums::to_sums_bytes(&to_copy);
        let mut outgoing = message;
        outgoing.extend(self.abrir_sumas(lines, bytes));
        outgoing
    }

    /// A row of the checksums dialog: the verdict — or the clamped digest —
    /// and the name, sanitized like anything else this window paints.
    pub(super) fn fila_de_suma(
        &self,
        name: &[u8],
        digest: Option<&str>,
        verdict: Option<norte_frontend::checksums::Verdict>,
    ) -> crate::dto::DialogLine {
        let (text, hostile) = norte_frontend::display::display_name(name);
        let state = match (verdict, digest) {
            (Some(v), _) => norte_i18n::t_in(self.lang, v.label_key()),
            (None, Some(d)) => d.chars().take(12).collect::<String>(),
            (None, None) => norte_i18n::t_in(self.lang, "checksum-unreadable"),
        };
        crate::dto::DialogLine {
            text: clamp_display(format!("{state}  {text}")),
            hostile,
        }
    }

    /// Opens the dialog with the checksums already judged (#311).
    ///
    /// Confirming COPIES the list to the clipboard when there are digests to
    /// copy, and when there are not — a verification does not carry them —
    /// the dialog just closes.
    pub(super) fn abrir_sumas(
        &mut self,
        body: Vec<crate::dto::DialogLine>,
        bytes: Vec<u8>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let copyable = !bytes.is_empty();
        let mut choices = Vec::new();
        if copyable {
            choices.push(DialogChoice {
                id: "confirm".to_owned(),
                label_key: "dialog-copy".to_owned(),
                destructive: false,
            });
        }
        choices.push(DialogChoice {
            id: "cancel".to_owned(),
            label_key: "dialog-close".to_owned(),
            destructive: false,
        });
        let view = DialogView {
            id,
            title_key: "modal-checksums-title".to_owned(),
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body,
            overflow_note: String::new(),
            overflow_hostile: false,
            choices,
            input: None,
            input_hostile: false,
            input_secret: false,
            fields: Vec::new(),
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogos.push(Dialogo {
            id,
            vista: view.clone(),
            tecleado: Tecleado::Texto(String::new()),
            reconocido: true,
            al_confirmar: copyable.then_some(Pendiente::CopiarSumas { bytes }),
        });
        let change = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        vec![self.parche(vec![change])]
    }

    /// Computes the checksums of what is marked, or verifies the sums file
    /// under the cursor (#311, ADR 0080).
    ///
    /// Verifying reads the file BEFORE launching anything: without its lines
    /// there are no paths to request. That `read` is spawned, like everything
    /// that talks to the backend from here, and it comes back through the
    /// mailbox.
    pub(super) fn lanzar_sumas(
        &mut self,
        verify: bool,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if verify {
            let Some(sums) = self.hueco().pane.selected().map(|e| e.path.clone()) else {
                return (
                    ActionAck::Unavailable {
                        reason_key: "host-nothing-selected".to_owned(),
                    },
                    self.decir("host-nothing-selected"),
                );
            };
            let backend2 = Arc::clone(backend);
            let mailbox2 = mailbox.clone();
            tokio::spawn(async move {
                // One byte MORE than the cap, to be able to tell "fits" from
                // "does not fit": a sums file silently truncated verifies
                // half the list and reads as "all correct".
                let bytes = backend2
                    .read(
                        sums.clone(),
                        Some(norte_proto::ByteRange {
                            offset: 0,
                            len: Some(SUMS_MAX_BYTES + 1),
                        }),
                    )
                    .await;
                let _ = mailbox2
                    .send(Mensaje::Fondo(Box::new(Fondo::FicheroDeSumas(
                        Box::new(sums),
                        Box::new(bytes),
                    ))))
                    .await;
            });
            return (self.aplicada(), Vec::new());
        }
        let paths = self.hueco().pane.marked_paths();
        if paths.is_empty() {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-nothing-selected".to_owned(),
                },
                self.decir("host-nothing-selected"),
            );
        }
        let outgoing = self.encolar_sumas(paths, None, backend, mailbox);
        (self.aplicada(), outgoing)
    }

    /// The sums file arrived: it is read and the Task is launched (#311).
    pub(super) fn fichero_de_sumas(
        &mut self,
        sums: &VPath,
        bytes: Result<Vec<u8>, Error>,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Ok(bytes) = bytes else {
            return self.decir("err-checksum-not-a-sums-file");
        };
        if bytes.len() as u64 > SUMS_MAX_BYTES {
            return self.decir("err-checksum-sums-too-big");
        }
        let parsed = norte_frontend::checksums::parse_sums(&bytes);
        if parsed.lines.is_empty() {
            // Say WHY when it is known: a PowerShell file is a perfectly
            // valid sums file in a different encoding.
            return self.decir(if norte_frontend::checksums::looks_utf16(&bytes) {
                "err-checksum-sums-utf16"
            } else {
                "err-checksum-not-a-sums-file"
            });
        }
        // Against the SUMS FILE's directory, not the panel's: a `SHA256SUMS`
        // talks about what sits next to it.
        let Some(base) = sums.parent() else {
            return self.decir("err-checksum-not-a-sums-file");
        };
        let (paths, asked) = norte_frontend::checksums::resolve_targets(&base, &parsed.lines);
        if paths.is_empty() {
            return self.decir("err-checksum-not-a-sums-file");
        }
        let published = Publicado {
            lines: parsed.lines,
            asked,
            refused: parsed.refused,
        };
        self.encolar_sumas(paths, Some(published), backend, mailbox)
    }

    /// Queues the checksum Task and notes which report needs waiting for.
    pub(super) fn encolar_sumas(
        &mut self,
        paths: Vec<VPath>,
        publicado: Option<Publicado>,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let params = norte_proto::methods::FsChecksumParams {
            paths,
            algo: norte_proto::methods::ChecksumAlgo::Sha256,
        };
        let backend2 = Arc::clone(backend);
        let mailbox2 = mailbox.clone();
        tokio::spawn(async move {
            match backend2.checksum(params).await {
                Ok(task) => {
                    let _ = mailbox2
                        .send(Mensaje::TaskNueva(Box::new((task, Vec::new(), None))))
                        .await;
                }
                Err(e) => {
                    let _ = mailbox2.send(Mensaje::TaskFallida(Box::new(e))).await;
                }
            }
        });
        // The Task does not have an id yet: what is noted here is the
        // INTENT, and `apuntar_sumas` matches it with the id once the task is
        // born.
        self.sumas_pendientes = Some(SumasEncoladas { publicado });
        self.decir("msg-checksum-started")
    }
}
