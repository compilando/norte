//! Answering a modal, and sending the mutation the answer authorizes.
//!
//! Everything that WRITES to somebody's disk goes through here, which is why
//! this is the module with the strictest rule: an answer translates into a
//! Task and nothing more. The decision — what is asked, with what notice,
//! and how many times — belongs to the model (`crate::app`) and its
//! allowlist; this only executes it.
//!
//! It used to live in the `ntc` binary's root, a crate DISTINCT from this
//! lib, so the only test that could be written was the one for the pure
//! function that distributes destinations ([`transfer_dests`]).

use crossterm::event::{KeyCode, KeyModifiers};
use norte_core::TransferOptions;
use norte_core::backend::Backend;
use norte_i18n::{t, ta};
use norte_proto::{DeleteMode, VPath};

use crate::app::{
    App, DialogOutcome, Modal, TransferKind, detail_for_bar, dialog_action, error_category,
    error_message,
};
use crate::keymap::{Resolution, Resolver, chord_from_crossterm};
use crate::navigate::Cd;
use crate::navigate::{
    provide_secret_retry, semantic_hit_cd, settle_suspended_trail, trust_host_retry,
};
use crate::overlays::{modal_help_toggle, modal_scroll};
use crate::tasks::RetrySpec;

/// An open modal's keys, resolved against the keymap's `dialog` context (H1
/// T2, issue #24 CLOSED — rebindable) and filtered by the specific modal's
/// ALLOWLIST ([`crate::app::dialog_action`]): security semantics live in
/// code, only the key→command ASSIGNMENT is keymap. `Modal::TrustLuaInit`
/// never reaches here (intercepted earlier in the run loop, decision 8).
/// `events` is for the TOFU modal's navigation retry (#45): trusting the
/// host key relaunches the `cd`, which has its own event loop.
#[expect(clippy::too_many_arguments, reason = "run loop wiring, not API")]
pub async fn on_dialog_key(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
    lang: norte_help::Lang,
    help_lines: &[ratatui::text::Line<'static>],
) -> Cd {
    let Some(modal) = app.modal.clone() else {
        return Cd::Cancelled;
    };
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return Cd::Cancelled; // key not modeled by the keymap: ignore
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        // No defined sequence semantics for overlays (T2), and the same for
        // a key bound to something this build does not run (K1 T4): ignore
        // and reset the resolution state.
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return Cd::Cancelled;
        }
        Resolution::Reset => return Cd::Cancelled,
    };
    // H3c: BEFORE the allowlist, which would drop `app.help` — it is not a
    // `dialog.*` verb. The help opens over the modal and claims the keys
    // (`help_owns_keys`); the modal stays intact behind it.
    if modal_help_toggle(app, cmd.as_str(), lang, help_lines) {
        return Cd::Cancelled;
    }
    if modal_scroll(app, cmd.as_str()) {
        return Cd::Cancelled;
    }
    let Some(outcome) = dialog_action(&modal, &cmd) else {
        return Cd::Cancelled; // command outside THIS modal's allowlist
    };
    match outcome {
        DialogOutcome::Open => {} // dialog_action never returns it: defensive
        DialogOutcome::Cancelled => {
            app.modal = None;
            app.open_next_pending();
            match modal {
                // Closing the approval dialog IS denying (fail-safe): the
                // agent receives `not-approved`, never a hung wait.
                Modal::ApproveAgentOp { req } => {
                    decide_approval(app, backend, req.approval_id, false).await;
                }
                // DENYING the host key abandons the navigation TOFU
                // suspended: there is no retry to finish it, so the trail
                // step `walk_trail` had taken comes back here. This is the
                // MOST likely of the three paths (saying no to an unknown
                // host is the normal case), and the only one that does not
                // go through `trust_host_retry`.
                Modal::TrustHostKey {
                    dir, pane, trail, ..
                }
                // #325: and the same for not giving the secret. Closing it
                // abandons the navigation, and the half-typed secret dies
                // with the modal (`TypedSecret` is overwritten with zeros
                // when dropped).
                | Modal::AskSecret {
                    dir, pane, trail, ..
                } => settle_suspended_trail(app, pane, &dir, trail, &Cd::Cancelled),
                _ => {}
            }
        }
        DialogOutcome::Confirmed => {
            if let Some(cd) = confirm_modal(app, backend, events, modal).await {
                return cd;
            }
        }
        DialogOutcome::Retry(policy) => {
            app.modal = None;
            if let Modal::Collision { retry } = modal {
                // Keeps the ORIGINAL options; only the policy changes.
                let opts = TransferOptions {
                    on_collision: policy,
                    ..retry.opts
                };
                submit_transfer(app, backend, retry.kind, retry.from, retry.to, opts).await;
            }
            app.open_next_pending();
        }
    }
    // Except for the TOFU retry (which does `return cd(...)`), a modal does
    // not navigate.
    Cd::Cancelled
}

/// What CONFIRMING each modal does.
///
/// Pulled out of `on_dialog_key`'s `match` once it passed a hundred lines
/// (#132 added it two more arms). `Some(cd)` is the only path that
/// NAVIGATES — the TOFU retry and jumping to a semantic hit — and that is
/// why it goes back to the caller instead of being resolved here: it is the
/// caller who decides what to do with a `Cd`.
///
/// The `match` is kept EXHAUSTIVE on purpose: naming the modals that confirm
/// nothing is what makes adding a new one a compile error instead of an
/// Enter that does something behind everyone's back.
#[expect(
    clippy::too_many_lines,
    reason = "one arm per confirming modal: the list is literal on purpose"
)]
pub async fn confirm_modal(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
    modal: Modal,
) -> Option<Cd> {
    app.modal = None;
    // WATCH OUT (a MAJOR from rust-reviewer): do NOT open the next pending
    // one BEFORE the match — the TOFU retry (`return cd`) can reopen a
    // TrustHostKey and STOMP an agent approval already pulled off the queue
    // (it would be orphaned until its TTL). It is deferred to the end.
    match modal {
        Modal::ConfirmDelete { items, permanent } => {
            submit_deletes(app, backend, &items, permanent).await;
        }
        // A human already confirmed it after reading the enumerated
        // capabilities (#280). What follows is said by the CORE: it grants
        // and relists, instead of trusting a local `bool` the daemon never
        // confirmed.
        Modal::ConfirmPluginApproval { id, digest, .. } => {
            crate::screens::extensions::conceder_aprobacion(app, backend, &id, digest.as_deref())
                .await;
        }
        Modal::ConfirmPluginUninstall { id, .. } => {
            crate::screens::extensions::desinstalar_confirmada(app, backend, &id).await;
        }
        // A human read the count and said yes (phase 7). Runs as an undo
        // Task, with the usual progress and cancellation: what is said here
        // is that it STARTED, and what happened is told by its report.
        Modal::ConfirmUndoAfter { seq, techo, .. } => match backend.undo_after(seq, techo).await {
            Ok(_task) => app.message = Some(t("msg-timeline-undo-running")),
            Err(e) => app.message = Some(error_message(&e)),
        },
        // A human read the WHOLE tree and said yes (phase 8). It is applied
        // with the token from the plan they read: if the directory drifted,
        // the core answers `PlanStale` and nothing is touched.
        Modal::OrganizePlan {
            dir,
            moves,
            plan_hash,
            ..
        } => match backend.organize(&dir, &moves, &plan_hash).await {
            Ok(_task) => app.message = Some(t("msg-organize-running")),
            Err(e) => app.message = Some(error_message(&e)),
        },
        Modal::ConfirmTransfer {
            kind, items, to, ..
        } => {
            let o = TransferOptions::default();
            submit_transfers(app, backend, kind, &items, &to, o).await;
        }
        // TrustLuaInit is intercepted EARLIER in the run loop (it needs
        // the LuaHost); MarkPattern (#103 T9) too, as free text
        // (same reason as search) — `dialog_action` returns `None`
        // for both, so `on_dialog_key` would already have returned
        // before reaching this match: unreachable here, defensive
        // no-op.
        // And properties (#139) too: `dialog_action` only understands
        // cancel for them, so a "confirm" does not reach here —
        // naming them is what makes adding one a compile error
        // instead of an Enter that does something behind everyone's back.
        // #311: confirming COPIES the checksums to the clipboard, which is
        // the only thing that can be done with them — there is no file to
        // write while the protocol cannot write content (#132). What gets
        // copied is the format `sha256sum -c` reads.
        Modal::Checksums {
            title_key,
            rows,
            offset,
        } => {
            let entries: Vec<(Vec<u8>, Option<String>)> = rows
                .iter()
                .map(|r| (r.name.clone(), r.digest.clone()))
                .collect();
            // In BYTES: a name need not be text, and a list with a `U+FFFD`
            // inside does not check the file it names (rule 1).
            let bytes = norte_frontend::checksums::to_sums_bytes(&entries);
            if bytes.is_empty() {
                app.message = Some(t("msg-checksum-nothing-to-copy"));
            } else {
                // The helper writes through a pipe and the payload can be
                // hundreds of kilobytes: on the loop's thread that is the TUI
                // stalled mid-write.
                let outcome = tokio::task::spawn_blocking({
                    let bytes = bytes.clone();
                    move || norte_frontend::shell::copy_to_clipboard(&bytes)
                })
                .await
                .unwrap_or(norte_frontend::shell::ClipboardOutcome::Failed);
                app.message = Some(match outcome {
                    norte_frontend::shell::ClipboardOutcome::Done(_) => t("msg-checksum-copied"),
                    // Told apart from the previous one because the escape
                    // sequence does not answer: if the terminal does not
                    // honor it, nothing warns of it, and whoever reads it has
                    // to know which path it took.
                    norte_frontend::shell::ClipboardOutcome::NoHelper => {
                        app.pending_osc52 = Some(norte_frontend::shell::osc52(&bytes));
                        t("msg-checksum-copied-osc52")
                    }
                    // The copy failed: the modal COMES BACK. Closing it would
                    // take with it the only copy of digests that may have
                    // cost hours to read.
                    norte_frontend::shell::ClipboardOutcome::Failed => {
                        app.modal = Some(Modal::Checksums {
                            title_key,
                            rows,
                            offset,
                        });
                        t("msg-clipboard-failed")
                    }
                });
            }
        }
        Modal::Properties { .. }
        | Modal::Report { .. }
        | Modal::Collision { .. }
        | Modal::TrustLuaInit { .. }
        | Modal::MarkPattern { .. }
        | Modal::Mkdir { .. }
        | Modal::ProfileSaveAs { .. }
        | Modal::EditNew { .. }
        | Modal::CommandLine { .. }
        | Modal::AiRenameInstruction { .. }
        | Modal::RenameBatchPattern { .. }
        | Modal::SemanticQuery { .. }
        | Modal::TransferDest { .. }
        | Modal::Pack { .. }
        | Modal::Split { .. }
        // #314: free text just like the ones above — the Enter is handled by
        // the run loop via `PromptKind::Chmod`, not this funnel.
        | Modal::Chmod { .. }
        | Modal::TransferName { .. } => {}
        // `AiRenamePlan` (M4-IA) IS a decision surface: confirming
        // applies the plan REVIEWED by the transactional batch
        // executor (§17) — ONE governed task (journal + policy) for
        // the whole batch, in the order the core decided. ALL pairs
        // are applied, not just the visible window: scroll (audit
        // MAJOR-3) makes the whole plan reviewable.
        Modal::AiRenamePlan {
            dir, entries, plan, ..
        } => {
            apply_ai_rename(app, backend, &dir, &entries, &plan).await;
        }
        // M4-IA-2: confirming NAVIGATES to the hit under the cursor
        // (`semantic_hit_cd`). The `Cd` goes back to the caller (apply_cd +
        // decorate), like the TOFU retry; if the cd opened a modal
        // (another HostKeyUnknown), the next pending one waits —
        // never step on it.
        Modal::SemanticHits { hits, cursor, .. } => {
            let outcome = semantic_hit_cd(app, backend, events, &hits, cursor).await;
            if app.modal.is_none() {
                app.open_next_pending();
            }
            return Some(outcome);
        }
        // S2 (`[ui] confirm_quit`): confirming closes — the run loop
        // detects it in its `app.quit` check on every turn
        // (main.rs, top of the `loop`).
        Modal::ConfirmQuit => app.quit = true,
        Modal::ApproveAgentOp { req } => {
            decide_approval(app, backend, req.approval_id, true).await;
        }

        // TOFU (#45): trusts the host key and RETRIES the navigation.
        m @ Modal::TrustHostKey { .. } => {
            if let Some(outcome) = trust_host_retry(app, backend, events, m).await {
                return Some(outcome);
            }
        }

        // #325: hands over the typed secret and RETRIES the navigation.
        // Here there is already something typed: with the field empty,
        // `dialog_action` leaves confirming inert and this is not reached.
        m @ Modal::AskSecret { .. } => {
            if let Some(outcome) = provide_secret_retry(app, backend, events, m).await {
                return Some(outcome);
            }
        }
    }
    // Every arm except the two retries (which already returned) opens the
    // next pending one here, with the modal already closed.
    app.open_next_pending();
    None
}

/// Resolves a policy approval (`policy.decide`, M3-3b T5). An error (id
/// already expired/decided by another frontend, daemon down) goes out
/// through the bar: the pending one, if still alive, will expire by TTL —
/// never hangs.
pub async fn decide_approval(app: &mut App, backend: &Backend, approval_id: u64, approve: bool) {
    if let Err(e) = backend.policy_decide(approval_id, approve).await {
        app.message = Some(error_message(&e));
    }
}

/// A batch's `(source, destination)` pairs: each item lands in the `to`
/// DIRECTORY with ITS SAME name — the name is BYTES (`Segment`, rule 1),
/// never text, so a `Папка` or a `\xff` travels intact. An item with no name
/// (a scheme's root) is not transferable and is dropped: there is nothing to
/// hang off the destination.
///
/// PURE on purpose: the whole batch can be seen with no backend raised.
#[must_use]
pub fn transfer_dests(items: &[VPath], to: &VPath) -> Vec<(VPath, VPath)> {
    items
        .iter()
        .filter_map(|from| {
            let name = from.file_name()?.clone();
            Some((from.clone(), to.join(name)))
        })
        .collect()
}

/// Sends the copy/move batch: ONE task PER ITEM (#103 T10), each with its
/// own progress, cancellation and journal entries — cancelling one does not
/// touch the others.
///
/// A failure does NOT abort the batch: the remaining items are still sent
/// and the last error stays in the bar. Abandoning 4..n because item 3
/// failed would leave half the selection done without saying so; the tasks
/// panel shows each one's result separately. Collisions do not travel
/// through here: they arrive ASYNCHRONOUSLY when the task finishes and
/// `on_tick` QUEUES them (`pending_collisions`) so an open modal is never
/// stepped on.
///
/// Marks are consumed when the batch is SENT, not when it completes.
pub async fn submit_transfers(
    app: &mut App,
    backend: &Backend,
    kind: TransferKind,
    items: &[VPath],
    to: &VPath,
    opts: TransferOptions,
) {
    for (from, dest) in transfer_dests(items, to) {
        let _submitted = submit_transfer(app, backend, kind, from, dest, opts).await;
    }
    app.consume_marks();
}

/// Sends the delete batch: ONE task PER ITEM, same criterion as
/// [`submit_transfers`] (a failure does not abandon the rest). The trash
/// target travels with each task so an `Unsupported` re-offers PERMANENT for
/// THAT item (ADR 0009), not the whole batch.
pub async fn submit_deletes(app: &mut App, backend: &Backend, items: &[VPath], permanent: bool) {
    let del_mode = if permanent {
        DeleteMode::Permanent
    } else {
        DeleteMode::Trash
    };
    for target in items {
        match backend.delete(target, del_mode).await {
            Ok(task) => {
                app.board
                    .push_full(&task, None, (!permanent).then(|| target.clone()));
            }
            Err(e) => app.message = Some(error_message(&e)),
        }
    }
    app.consume_marks();
}

/// Launches a size count and registers it in the tasks panel (#139).
///
/// `for_dialog` ties the Task to the open properties modal, so its result
/// lands THERE and not just in the status bar.
///
/// The total does not come back through here: it arrives in the Task's
/// terminal progress, which is what `on_tick` is already watching for all
/// the others.
pub async fn launch_size_count(
    app: &mut App,
    backend: &Backend,
    paths: Vec<VPath>,
    for_dialog: bool,
) {
    if paths.is_empty() {
        return;
    }
    match backend
        .dir_size(norte_proto::methods::FsDirSizeParams { paths })
        .await
    {
        Ok(task) => {
            if for_dialog {
                app.properties_counting(task.id());
            } else {
                app.message = Some(t("msg-dir-size-counting"));
            }
            app.board.push(&task, None);
        }
        Err(e) => app.message = Some(error_message(&e)),
    }
}

/// `pane.unpack` (#132): copies the container under the cursor's INTERIOR to
/// the other panel.
///
/// It carries no method of its own and does not need one: the copy engine
/// already accepts an archive's interior as a source, so unpacking is the
/// copy the user could have done by hand, with the journal, the undo, the
/// collision policy and the cancellation the copy already has.
pub async fn unpack(app: &mut App, backend: &Backend) {
    let Some(entry) = app.focused().selected().cloned() else {
        return;
    };
    let Some(root) = crate::nav::archive_root_for(&entry) else {
        app.message = Some(t("msg-unpack-not-archive"));
        return;
    };
    // The destination is the OTHER panel, which is where an orthodox
    // manager unpacks. With only one, the same one — which is what F5 does
    // when there is no other place to point at.
    // The same notion of "the other" that splitting uses: by visible
    // POSITION, and with only one panel the same one. `focus() ^ 1` gave an
    // out-of-range index with three or four panels, and there
    // `pane_read_only` answers `false` without looking at anything — the
    // gate was left inert exactly where there are more spots to point at by
    // mistake.
    let other = app.split_dest_pane();
    if app.pane_read_only(other) {
        app.message = Some(t("msg-pack-read-only"));
        return;
    }
    let dest = app.panes[other].dir().clone();
    match backend.copy(&root, &dest, TransferOptions::default()).await {
        Ok(task) => {
            app.message = Some(t("msg-unpack-started"));
            app.board.push(&task, None);
        }
        Err(e) => app.message = Some(error_message(&e)),
    }
}

/// `pane.test-archive` (#132): checks the container under the cursor.
pub async fn test_archive(app: &mut App, backend: &Backend) {
    let Some(entry) = app.focused().selected().cloned() else {
        return;
    };
    if crate::nav::archive_root_for(&entry).is_none() {
        app.message = Some(t("msg-unpack-not-archive"));
        return;
    }
    match backend
        .test_archive(norte_proto::methods::ArchiveTestParams {
            path: entry.path.clone(),
        })
        .await
    {
        Ok(task) => {
            app.message = Some(t("msg-test-archive-started"));
            app.board.push(&task, None);
        }
        Err(e) => app.message = Some(error_message(&e)),
    }
}

/// Cap on the bytes read from a SUMS file (#311).
///
/// A large project's `SHA256SUMS` is a few hundred kilobytes; a megabyte
/// leaves plenty of margin and stops pointing this key at an ISO from
/// trying to fit it whole into memory just to find not one valid line.
const SUMS_MAX_BYTES: u64 = 1024 * 1024;

/// `pane.checksum` (#311): computes the sha256 of what is marked — or what
/// is under the cursor — and shows the list.
///
/// The operand is the usual one (`marked_paths`), so there is no new rule to
/// learn. The Task goes to the board like any other: what this gesture adds
/// is waiting for its REPORT, which is where the digests travel.
pub async fn checksum_start(
    app: &mut App,
    backend: &Backend,
    work: &mut crate::jobs::InFlight,
    req: crate::app::ChecksumRequest,
) {
    match req {
        crate::app::ChecksumRequest::Compute { paths } => {
            lanzar_sumas(app, backend, work, paths, None).await;
        }
        crate::app::ChecksumRequest::Verify { sums } => {
            checksum_verify(app, backend, work, &sums).await;
        }
    }
}

/// `pane.checksum-verify` (#311): checks the files listed by the sums file
/// under the cursor.
///
/// The file's names are resolved against ITS OWN directory — not the
/// pane's: a `SHA256SUMS` talks about what it has next to it, and resolving
/// it against another spot would check different files with the same
/// names.
async fn checksum_verify(
    app: &mut App,
    backend: &Backend,
    work: &mut crate::jobs::InFlight,
    sums: &norte_proto::VPath,
) {
    // ONE BYTE MORE than the cap is requested so "fits" can be told apart
    // from "does not fit". A sums file truncated in silence checks half the
    // list and the summary reads as "all correct" — and the cut lands on
    // some arbitrary byte, so the last line can end up with half a name and
    // accuse a file that IS there of being "missing". Same criterion as the
    // cap at the other end: REJECT, do not truncate.
    let bytes = match backend
        .read(
            sums,
            Some(norte_proto::ByteRange {
                offset: 0,
                len: Some(SUMS_MAX_BYTES + 1),
            }),
        )
        .await
    {
        Ok(b) => b,
        Err(e) => {
            app.message = Some(error_message(&e));
            return;
        }
    };
    if bytes.len() as u64 > SUMS_MAX_BYTES {
        app.message = Some(t("msg-checksum-sums-too-big"));
        return;
    }
    let published = norte_frontend::checksums::parse_sums(&bytes);
    if published.lines.is_empty() {
        // Say WHY when it is known: a PowerShell `SHA256SUMS` is a perfectly
        // valid sums file in a different encoding, and sending whoever reads
        // it to doubt the file is sending them to the wrong place.
        app.message = Some(t(if norte_frontend::checksums::looks_utf16(&bytes) {
            "msg-checksum-sums-utf16"
        } else {
            "msg-checksum-not-a-sums-file"
        }));
        return;
    }
    // The SUMS FILE's directory, not the pane's.
    let Some(base) = sums.parent() else {
        app.message = Some(t("msg-checksum-not-a-sums-file"));
        return;
    };
    // The resolution lives in the SHARED crate: the window checks the same
    // sums files, and two reads of `sub/dentro.txt` in two frontends would
    // be two different checks (ADR 0077).
    let (paths, asked) = norte_frontend::checksums::resolve_targets(&base, &published.lines);
    if paths.is_empty() {
        app.message = Some(t("msg-checksum-not-a-sums-file"));
        return;
    }
    let published = crate::jobs::Publicado {
        lines: published.lines,
        asked,
        refused: published.refused,
    };
    lanzar_sumas(app, backend, work, paths, Some(published)).await;
}

/// Launches the checksums Task and leaves it waiting for its report.
///
/// The wait is SPAWNED and harvested in the loop (rule 3): a batch of a
/// hundred large files takes a while, and awaiting it here would leave the
/// TUI not drawing, not taking keys and unable to cancel — which is exactly
/// when someone cancels.
async fn lanzar_sumas(
    app: &mut App,
    backend: &Backend,
    work: &mut crate::jobs::InFlight,
    paths: Vec<norte_proto::VPath>,
    publicado: Option<crate::jobs::Publicado>,
) {
    let params = norte_proto::methods::FsChecksumParams {
        paths,
        algo: norte_proto::methods::ChecksumAlgo::Sha256,
    };
    match backend.checksum(params).await {
        Ok(task) => {
            app.message = Some(t("msg-checksum-started"));
            app.board.push(&task, None);
            let id = task.id();
            let observer = task.observer();
            let mut prog = task.progress();
            let b = backend.clone();
            let handle = tokio::spawn(async move {
                // The report is only DEFINITIVE once the Task is terminal;
                // requesting it earlier would give half the list without
                // saying it is.
                while !prog.borrow().state.is_terminal() {
                    if prog.changed().await.is_err() {
                        break;
                    }
                }
                // The state travels WITH the report: `Cancelled` or `Failed`
                // mean what is there is half-done, and a `changed()` that
                // dies without reaching terminal — the daemon went down — is
                // neither of the two. Same criterion as `TaskRef::join`.
                let state = prog.borrow().state.clone();
                if !state.is_terminal() {
                    return (
                        state,
                        Err(norte_proto::Error::ProviderUnavailable { retryable: true }),
                    );
                }
                (state, b.checksum_report(id).await)
            });
            if let Some(old) = work.checksum.replace(crate::jobs::ChecksumRun {
                handle,
                task: observer,
                publicado,
            }) {
                // Cancel the TASK, not just the wait: aborting the
                // `JoinHandle` left the core hashing a whole ISO with
                // nobody to collect it and — since checksums do not say
                // "done" — without even saying that it finished.
                old.task.cancel();
                old.handle.abort();
            }
        }
        Err(e) => app.message = Some(error_message(&e)),
    }
}

/// `pane.combine-files` (#132): joins the pieces starting from the `.001`
/// under the cursor.
pub async fn combine_pieces(app: &mut App, backend: &Backend) {
    let Some(entry) = app.focused().selected().cloned() else {
        return;
    };
    let name = entry
        .path
        .file_name()
        .map(|s| s.as_bytes().to_vec())
        .unwrap_or_default();
    // Only from the FIRST piece: starting from `.007` would join half a
    // thing, and the core only knows how to search forward anyway. The rule
    // lives in the shared crate — the window asks the same (D14).
    let Some(seg) = norte_frontend::nav::base_de_trozos(&name) else {
        app.message = Some(t("msg-combine-needs-first"));
        return;
    };
    let dest = app.focused().dir().join(seg);
    match backend
        .combine_files(norte_proto::methods::FileCombineParams {
            first: entry.path,
            dest,
        })
        .await
    {
        Ok(task) => {
            app.message = Some(t("msg-combine-started"));
            app.board.push(&task, None);
        }
        Err(e) => app.message = Some(error_message(&e)),
    }
}

/// Queues a transfer and registers it in the panel with its retry context
/// (for the collision dialog).
/// Returns `true` if the task QUEUED (#105: the editable-name modal only
/// closes then); a failure leaves the error in the bar.
pub async fn submit_transfer(
    app: &mut App,
    backend: &Backend,
    kind: TransferKind,
    from: VPath,
    to: VPath,
    opts: TransferOptions,
) -> bool {
    // The session's switch decides which door it goes through (ADR 0149);
    // a collision's retry inherits what was requested the first time, so
    // whatever the options already carried is respected.
    let opts = TransferOptions {
        queued: opts.queued || app.encolar,
        ..opts
    };
    let res = match kind {
        TransferKind::Copy => backend.copy(&from, &to, opts).await,
        TransferKind::Move => backend.move_(&from, &to, opts).await,
    };
    match res {
        Ok(task) => {
            // #98/M1: the source pane's encoding travels with the retry —
            // the collision arrives async and focus may have changed.
            let name_encoding = app.focused().name_encoding();
            app.board.push(
                &task,
                Some(RetrySpec {
                    kind,
                    from,
                    to,
                    opts,
                    name_encoding,
                }),
            );
            true
        }
        Err(e) => {
            app.message = Some(error_message(&e));
            false
        }
    }
}

/// Applies an AI rename plan CONFIRMED (M4-IA) through the TRANSACTIONAL
/// batch executor (spec §17, ADR 0042): ONE task, ONE undoable journal unit,
/// rollback if a step fails.
///
/// Replaces the loop of one `fs.move` per pair, which was not a transaction
/// (the fifth failure left four applied), did not check the plan against
/// itself, and could not do a permutation — the NORMAL AI rename case
/// ("number these episodes correctly"), where `a→b, b→c` collided on the
/// first move.
///
/// Three refusals, in order, and none of them queues anything:
///
/// - a pair that is not a [`norte_proto::Segment`] = a tampered plan (belt
///   [`norte_frontend::rename_pairs`], SHARED with the GUI — quality review
///   78eb243 MAJOR-1, audit MAJOR-2);
/// - with no batch plan there is no approved `plan_hash` to send;
/// - with verdicts pending the core would not execute anything, so it is not
///   even requested.
///
/// All three are a belt: the confirm key is already mute with no applicable
/// plan (`dialog_action`). What reaches here is a single submit, and its
/// failure goes whole to the bar.
pub async fn apply_ai_rename(
    app: &mut App,
    backend: &Backend,
    dir: &VPath,
    entries: &[norte_proto::methods::AiRenameEntry],
    plan: &norte_frontend::BatchPlan,
) {
    // Only the SHAPE. The check that each `from` exists already ran when the
    // plan landed, against the directory that was PLANNED (#275); repeating
    // it here against the focused pane would run it against a different
    // directory, because the reader may have moved while reading the
    // review. And the core does it on its own before touching anything.
    let Some(pairs) = norte_frontend::rename_pairs(entries) else {
        app.message = Some(t("msg-ai-rename-invalid-plan"));
        return;
    };
    let Some(resolved) = plan.ready() else {
        app.message = Some(t("msg-rename-batch-no-plan"));
        return;
    };
    if !resolved.executable {
        app.message = Some(t("msg-rename-batch-collisions"));
        return;
    }
    // What is announced is the renames the core committed to doing, not the
    // pairs REQUESTED: the planner drops the null ones (`from == to`), and
    // promising more than what will happen is lying in the bar.
    let n = plan.real_steps();
    match backend.rename_batch(dir, &pairs, &resolved.plan_hash).await {
        Ok(task) => {
            app.board.push(&task, None);
            app.message = Some(ta("msg-rename-batch-applied", &[("n", &n.to_string())]));
        }
        Err(e) => {
            app.message = Some(ta(
                "msg-rename-batch-failed",
                &[("error", &detail_for_bar(&error_category(&e)))],
            ));
        }
    }
}

#[cfg(test)]
mod bulk_tests {
    use super::transfer_dests;
    use norte_proto::VPath;

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("valid wire")
    }

    /// #103 T10: the whole batch is sent — one pair per item, each with ITS
    /// OWN name hung off the destination directory. (Control mutation:
    /// making the submit use only the first item breaks this test.)
    #[test]
    fn a_bulk_transfer_submits_every_item_not_just_the_first() {
        let items = vec![vp("mem:///src/a"), vp("mem:///src/b"), vp("mem:///src/c")];
        let pairs = transfer_dests(&items, &vp("mem:///dst"));
        assert_eq!(pairs.len(), 3, "one task PER item");
        assert_eq!(
            pairs.iter().map(|(_, d)| d.clone()).collect::<Vec<_>>(),
            vec![vp("mem:///dst/a"), vp("mem:///dst/b"), vp("mem:///dst/c")],
        );
    }

    /// Rule 1: the name is BYTES. A non-UTF8 name arrives at the destination
    /// byte for byte — the destination is never built from the painted text.
    #[test]
    fn a_bulk_transfer_keeps_non_utf8_names_byte_exact() {
        let raw = b"caf\xff\xfe.txt".to_vec();
        let seg = norte_proto::Segment::new(raw.clone()).expect("segment");
        let from = vp("mem:///src").join(seg);
        let pairs = transfer_dests(std::slice::from_ref(&from), &vp("mem:///dst"));
        assert_eq!(pairs.len(), 1);
        assert_eq!(
            pairs[0].1.file_name().map(|s| s.as_bytes().to_vec()),
            Some(raw),
            "the name's bytes travel intact to the destination",
        );
    }

    /// A destination SAME as the source (same dir in both panes) yields a
    /// `from == to` pair: the decision of what to do with that belongs to
    /// the engine (collision), not the frontend — which must not invent a
    /// discard.
    #[test]
    fn a_same_directory_transfer_maps_each_item_onto_itself() {
        let items = vec![vp("mem:///src/a")];
        let pairs = transfer_dests(&items, &vp("mem:///src"));
        assert_eq!(pairs[0].0, pairs[0].1);
    }

    /// A scheme root has no name to hang off the destination: it is dropped
    /// instead of fabricating a path.
    #[test]
    fn a_rootless_item_is_dropped_from_the_batch() {
        let root = VPath::root(norte_proto::Scheme::new("mem").unwrap(), None);
        assert!(transfer_dests(&[root], &vp("mem:///dst")).is_empty());
    }
}
