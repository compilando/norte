//! The three LONG AI and batch requests, harvested without blocking.
//!
//! All three have the shape of the pane tasks next door: they get spawned,
//! harvested in a `select!` arm, and there is at most one alive — relaunching
//! aborts the previous one. What distinguishes them is what they do with the
//! answer, and that's what lives here: the ingestion belts (a plan above the
//! cap, or a pair that isn't a `Segment`, gives away a hostile daemon, and
//! gets rejected wholesale) and the discipline of never overwriting an open
//! modal.

use norte_core::backend::Backend;
use norte_i18n::{t, ta};
use norte_proto::Error;

use crate::app::{App, Modal, detail_for_bar, error_category};
use crate::jobs::{AiRenameRun, InFlight, PendingAiPlan, RenameBatchRun};

/// C3 (ADR 0095): asks a `renamer` plugin for its plan over what's marked —or
/// just the pointed-at row— and leaves it in the SAME run as the AI plan: the
/// harvest doesn't distinguish who proposed it, and that's why there is no
/// second review path. The operand is the batch-by-template one
/// (`rename_batch_names`): only names that are text, because a pair travels
/// UTF-8.
pub fn spawn_renamer_plan(
    app: &mut App,
    backend: &Backend,
    work: &mut InFlight,
    id: &str,
    renamer: &str,
) {
    let names = app.rename_batch_names();
    if names.is_empty() {
        app.message = Some(t("msg-rename-batch-nothing"));
        return;
    }
    let dir = app.focused().dir().clone();
    let b = backend.clone();
    let d = dir.clone();
    let (id, renamer) = (id.to_owned(), renamer.to_owned());
    let handle = tokio::spawn(async move { b.plugin_rename_plan(&id, &renamer, &d, &names).await });
    // The names of the directory being PLANNED (#275): the harvest belt
    // requires every `from` to exist where it's going to be applied.
    let dir_names: Vec<Vec<u8>> = app
        .focused()
        .entries()
        .iter()
        .filter_map(|e| e.path.file_name().map(|s| s.as_bytes().to_vec()))
        .collect();
    let run = AiRenameRun {
        handle,
        dir,
        names: dir_names,
    };
    if let Some(old) = work.ai_rename.replace(run) {
        old.handle.abort();
    }
    work.pending_ai_plan = None;
    app.message = Some(t("msg-ai-rename-running"));
}

/// Phase 8: requests an ORGANIZE plan over the focused directory, from the
/// model (`organizer = None`) or from an `organizer` plugin.
///
/// Both paths end in the SAME run and the same modal: what makes the
/// operation safe isn't where the names came from, so a plugin plan and a
/// model plan are reviewed the same way and applied through the same place
/// (ADR 0095, generalized).
///
/// The operand is the WHOLE directory, not what's marked: organizing is a
/// decision about the directory's shape, and doing it over half a dozen rows
/// would leave a half-shape nobody asked for.
pub fn spawn_organize_plan(
    app: &mut App,
    backend: &Backend,
    work: &mut InFlight,
    organizer: Option<(&str, &str)>,
) {
    use crate::jobs::OrganizeRun;

    if app.focused().virtual_search {
        app.message = Some(t("msg-ai-rename-in-search"));
        return;
    }
    let dir = app.focused().dir().clone();
    // The dir's names at LAUNCH time: the tree needs to know which folder
    // already existed, and by the time the producer answers the pane may
    // point elsewhere.
    let existing = app.focused().existing_names();
    // A plugin does NOT list the directory — it doesn't touch the disk, rule
    // 9 — so the caller has to hand it the names: with an empty list, an
    // organizer answers "I'm not moving anything" and the status bar says
    // so, which is what happened the first time this was piloted. The model
    // is the opposite case: the engine lists the dir for it, and there an
    // empty list DOES mean "everything", with no cap to respect.
    let operand = app.focused().organizable_names();
    if organizer.is_some() && operand.len() > norte_proto::methods::AI_RENAME_NAMES_MAX {
        app.message = Some(t("msg-organize-too-many"));
        return;
    }
    let b = backend.clone();
    let d = dir.clone();
    let handle = match organizer {
        Some((id, org)) => {
            let (id, org) = (id.to_owned(), org.to_owned());
            tokio::spawn(async move { b.plugin_organize_plan(&id, &org, &d, &operand).await })
        }
        None => tokio::spawn(async move { b.ai_organize_plan(&d, "", &[]).await }),
    };
    if let Some(old) = work.organize.replace(OrganizeRun {
        handle,
        dir,
        existentes: existing,
    }) {
        old.handle.abort();
    }
    app.message = Some(t("msg-organize-running"));
}

/// The organize plan (phase 8): opens the modal, or drops it if another is
/// already up front.
///
/// **No retention, unlike the AI plan**, and that's a decision: the rename
/// plan is retained because its `plan_hash` is requested on a second round
/// trip that nobody would trigger afterward. Here the token travels with the
/// plan, so a retained plan gains nothing — and a tree that opens by itself
/// minutes later, over a directory the reader is no longer looking at, is
/// worse than asking for it again.
pub fn harvest_organize(
    app: &mut App,
    work: &mut InFlight,
    res: Harvested<norte_proto::methods::AiOrganizePlanResult>,
) {
    let Some(run) = work.organize.take() else {
        return;
    };
    match res {
        // The producer said WHY it isn't proposing anything (#332): the
        // phrase already comes masked and bounded by the daemon.
        Ok(Ok(norte_proto::methods::AiOrganizePlanResult {
            refused: Some(why), ..
        })) => {
            app.message = Some(ta("msg-rename-plan-refused", &[("why", &why)]));
        }
        Ok(Ok(plan)) if plan.moves.is_empty() => {
            app.message = Some(t("msg-organize-empty"));
        }
        // Same INGESTION belt as the rename plan: a legitimate plan stays
        // well below the cap, and going over it gives away a hostile daemon
        // or N+1 inflating the response.
        Ok(Ok(plan)) if plan.moves.len() > norte_frontend::MAX_AI_PLAN_ENTRIES => {
            app.message = Some(t("msg-organize-invalid-plan"));
        }
        Ok(Ok(plan)) => {
            // No token, nothing to approve: confirming would be a button
            // that can't do anything, and opening the tree would promise it
            // could.
            let Some(plan_hash) = plan.plan_hash else {
                app.message = Some(t("msg-organize-invalid-plan"));
                return;
            };
            let lines = norte_frontend::organize::tree_lines(&plan.moves, &run.existentes);
            if app.modal.is_none() {
                app.message = None;
                app.modal = Some(Modal::OrganizePlan {
                    dir: run.dir,
                    moves: plan.moves,
                    lines,
                    plan_hash,
                    offset: 0,
                    seen: norte_frontend::organize::ORGANIZE_LINE_LIMIT,
                });
            } else {
                app.message = Some(t("msg-organize-hidden"));
            }
        }
        Ok(Err(e)) => {
            // ITS own message, not the rename one: with no AI provider
            // configured, asking to organize used to say "AI rename
            // failed" — a status bar naming an operation the reader never
            // asked for.
            app.message = Some(ta(
                "msg-organize-failed",
                &[("error", &detail_for_bar(&error_category(&e)))],
            ));
        }
        // Aborted by Esc, or a future panic: the status bar was already
        // cleared.
        Err(_join) => {}
    }
}

/// What a spawned request returns: the backend's error inside, the `join`'s
/// outside (abort by Esc, or a future panic).
type Harvested<T> = Result<Result<T, Error>, tokio::task::JoinError>;

/// The report of a checksum batch (#311): opens the modal with what was
/// computed, and with the VERDICT if this was a verification.
///
/// A report that doesn't arrive — the Task failed, or the ring already
/// evicted it — goes out through the status bar and opens nothing: an empty
/// modal would make it seem something was verified.
pub fn harvest_checksum(
    app: &mut App,
    work: &mut InFlight,
    res: Result<
        (
            norte_proto::TaskState,
            Result<norte_proto::methods::FsChecksumReportResult, Error>,
        ),
        tokio::task::JoinError,
    >,
) {
    use norte_frontend::checksums;

    let Some(run) = work.checksum.take() else {
        return;
    };
    let (state, report) = match res {
        Ok(pair) => pair,
        // A handoff's abort does NOT land here: on handoff, the old handle
        // gets aborted AND dropped, and the `select!` arm only polls the new
        // one from then on. What DOES land here is a future panic, and
        // nobody put a message there: without this the status bar stayed at
        // "computing checksums..." forever and nothing opened.
        Err(join) => {
            if join.is_panic() {
                app.message = Some(t("msg-checksum-failed"));
            }
            return;
        }
    };
    let report = match report {
        Ok(r) => r,
        Err(e) => {
            app.message = Some(crate::app::error_message(&e));
            return;
        }
    };
    // A report from a Task that was CANCELLED or that failed is partial, and
    // `pending > 0` says so. Painting verdicts over it would accuse — "wrong
    // or missing" — files nobody got to read, which is the worst possible
    // error in the one tool whose job is to verify. The Task's state was
    // already said via the status bar (cancelled / the error), so here it's
    // enough not to make up a conclusion.
    if state != norte_proto::TaskState::Completed || report.pending > 0 {
        app.message = Some(t("msg-checksum-partial"));
        return;
    }
    // Each path's digest and reason, IN THE REQUESTED ORDER, which is how
    // the report returns them and how they get paired up.
    let computed: Vec<checksums::Computed> = report
        .entries
        .iter()
        .map(|e| (e.digest.clone(), e.miss))
        .collect();
    // Verify: the list is the CHECKSUM FILE's — in its order and with all
    // its lines, including the ones that couldn't even be requested — and
    // the verdict comes out of the SHARED funnel, which is where the rule
    // for what each reason means lives. Compute: the list is what was
    // requested, with its digest.
    let (title_key, rows) = if let Some(published) = run.published {
        let verdicts = checksums::judge(&published.lines, &published.asked, &computed);
        app.message = Some(match checksums::summarize(&verdicts, published.refused) {
            // "All correct" can't be said about 37 of 40 lines: the three
            // that dropped out are exactly the ones with odd names.
            checksums::Summary::Unreadable { n, refused } => ta(
                "msg-checksum-unreadable-lines",
                &[("n", &n.to_string()), ("refused", &refused.to_string())],
            ),
            checksums::Summary::AllOk { n } => ta("msg-checksum-all-ok", &[("n", &n.to_string())]),
            checksums::Summary::Bad { n } => ta("msg-checksum-bad", &[("n", &n.to_string())]),
        });
        let rows: Vec<crate::app::ChecksumRow> = published
            .lines
            .into_iter()
            .zip(verdicts)
            .map(|(line, verdict)| crate::app::ChecksumRow {
                name: line.name,
                digest: None,
                verdict: Some(verdict),
            })
            .collect();
        ("modal-checksums-verify", rows)
    } else {
        // Compute: the requested path supplies the name, in bytes (rule 1).
        let rows: Vec<crate::app::ChecksumRow> = report
            .entries
            .iter()
            .map(|e| crate::app::ChecksumRow {
                name: e
                    .path
                    .file_name()
                    .map(|s| s.as_bytes().to_vec())
                    .unwrap_or_default(),
                digest: e.digest.clone(),
                verdict: e.miss.map(|m| match m {
                    norte_proto::methods::ChecksumMiss::NotAFile => checksums::Verdict::NotAFile,
                    _ => checksums::Verdict::Missing,
                }),
            })
            .collect();
        app.message = None;
        ("modal-checksums-create", rows)
    };
    if app.modal.is_none() {
        app.modal = Some(Modal::Checksums {
            title_key,
            rows,
            offset: 0,
        });
    } else {
        // With another modal open, nothing gets overwritten: the rows are
        // HELD and open by themselves once the one in front closes.
        // Dropping them was worse than saying nothing, because the status
        // bar promised to show them later.
        work.pending_checksums = Some((title_key, rows));
        app.message = Some(t("msg-checksum-done-hidden"));
    }
}

/// The model's plan (M4-IA): opens the modal, or HOLDS it if another is
/// open, and along the way requests the BATCH's plan (§17) on the same trip
/// — the modal needs its `plan_hash` for confirming to do anything, and a
/// plan held behind another modal wouldn't have anyone to ask for it
/// afterward.
pub fn harvest_ai_rename(
    app: &mut App,
    backend: &Backend,
    work: &mut InFlight,
    res: Harvested<norte_proto::methods::AiRenamePlanResult>,
) {
    if let Some(run) = work.ai_rename.take() {
        match res {
            // The producer said WHY it isn't proposing anything (#332): a
            // renamer that refused. The phrase already comes masked and
            // bounded by the daemon.
            Ok(Ok(norte_proto::methods::AiRenamePlanResult {
                refused: Some(why), ..
            })) => {
                app.message = Some(ta("msg-rename-plan-refused", &[("why", &why)]));
            }
            Ok(Ok(plan)) if plan.entries.is_empty() => {
                app.message = Some(t("msg-ai-rename-empty"));
            }
            // INGESTION belt (quality review 78eb243 MINOR-5): a legitimate
            // plan from the engine stays well below the cap; going over it
            // gives away a hostile/N+1 daemon inflating the response —
            // rejected wholesale, the modal doesn't even open.
            Ok(Ok(plan)) if plan.entries.len() > norte_frontend::MAX_AI_PLAN_ENTRIES => {
                app.message = Some(t("msg-ai-rename-invalid-plan"));
            }
            Ok(Ok(plan)) => {
                // §17: the BATCH's plan is requested HERE, on the same trip
                // as the AI plan — the modal needs the `plan_hash` for
                // confirming to do anything, and a plan held behind another
                // modal wouldn't have anyone to ask for it afterward.
                //
                // SPAWNED, like the call to the model: against a huge dir or
                // a slow daemon this is a whole `fs.list`, and waiting for
                // it here would freeze the loop — no drawing, no keys, no
                // Esc. The modal opens in `Pending` and fills itself in.
                //
                // fail-loud belt SHARED with the GUI (audit MAJOR-2): a pair
                // that isn't a `Segment` gives away a hostile/broken daemon
                // — the core isn't even asked for a plan, and confirming
                // stays dead.
                // Against the directory that was PLANNED, not the one the
                // pane shows now (#275).
                let state = if let Some(pairs) =
                    norte_frontend::rename_pairs_in(&plan.entries, Some(&run.names))
                {
                    let b = backend.clone();
                    let d = run.dir.clone();
                    let handle = tokio::spawn(async move { b.rename_batch_plan(&d, &pairs).await });
                    if let Some(old) = work.rename_batch.replace(RenameBatchRun { handle }) {
                        old.handle.abort();
                    }
                    app.message = None;
                    norte_frontend::BatchPlan::Pending
                } else {
                    app.message = Some(t("msg-ai-rename-invalid-plan"));
                    norte_frontend::BatchPlan::Failed
                };
                let ready = PendingAiPlan {
                    dir: run.dir,
                    names: run.names,
                    entries: plan.entries,
                    plan: state,
                };
                if app.modal.is_none() {
                    app.modal = Some(Modal::AiRenamePlan {
                        dir: ready.dir,
                        entries: ready.entries,
                        offset: 0,
                        seen: norte_frontend::AI_RENAME_PAIR_LIMIT,
                        plan: ready.plan,
                    });
                } else {
                    // Another modal open (approval, collision...): the plan
                    // waits its turn, never overwrites it. Unlike the GUI
                    // (superseded banner), here the overwrite is
                    // unreachable: a single run in flight and the prompt
                    // doesn't open over another modal.
                    work.pending_ai_plan = Some(ready);
                }
            }
            Ok(Err(e)) => {
                app.message = Some(ta(
                    "msg-ai-rename-failed",
                    &[("error", &detail_for_bar(&error_category(&e)))],
                ));
            }
            // Aborted by Esc: silence, the status bar was already cleared.
            // (A panic in the backend's future falls here too: there's no
            // plan to open, the run is already harvested.)
            Err(_join) => {}
        }
    }
}

/// The batch's verdict (§17). The modal may be open, HELD behind another, or
/// already closed by the human: in the first two cases it gets filled in; in
/// the third the answer is dropped.
pub fn harvest_rename_batch(
    app: &mut App,
    work: &mut InFlight,
    res: Harvested<norte_proto::methods::FsRenameBatchPlanResult>,
) {
    if work.rename_batch.take().is_some() {
        let state = match res {
            Ok(Ok(plan)) => norte_frontend::BatchPlan::Ready(Box::new(plan)),
            Ok(Err(e)) => {
                app.message = Some(ta(
                    "msg-rename-batch-plan-failed",
                    &[("error", &detail_for_bar(&error_category(&e)))],
                ));
                norte_frontend::BatchPlan::Failed
            }
            // Aborted (another request superseded it) or a future panic: no
            // plan and nothing more to say — whoever superseded it already
            // put ITS message.
            Err(_join) => norte_frontend::BatchPlan::Failed,
        };
        // The modal may be open, HELD behind another, or already closed by
        // the human. In the first two cases it gets filled in; in the third
        // the answer is dropped.
        if !app.settle_ai_batch_plan(&state)
            && let Some(p) = &mut work.pending_ai_plan
            && p.plan == norte_frontend::BatchPlan::Pending
        {
            p.plan = state;
        }
    }
}

/// The index's hits (M4-IA-2): same treatment as the AI plan — ingestion
/// belt and never overwriting an open modal.
pub fn harvest_semantic(
    app: &mut App,
    work: &mut InFlight,
    res: Harvested<Vec<norte_proto::methods::SemanticHit>>,
) {
    work.semantic = None;
    match res {
        Ok(Ok(hits)) if hits.is_empty() => {
            app.message = Some(t("msg-semantic-empty"));
        }
        // INGESTION belt (IA-1 parity): a response above the server's
        // contractual ceiling, or with a non-finite score, gives away a
        // hostile/N+1 daemon — rejected wholesale, the modal doesn't even
        // open (the guard is `norte_frontend::validate_semantic_hits`, pure
        // and shared with the GUI).
        Ok(Ok(hits)) => match norte_frontend::validate_semantic_hits(hits) {
            None => {
                app.message = Some(t("msg-semantic-invalid"));
            }
            Some(hits) => {
                app.message = None;
                if app.modal.is_none() {
                    app.modal = Some(Modal::SemanticHits {
                        hits,
                        offset: 0,
                        cursor: 0,
                    });
                } else {
                    // Another modal open (approval, collision...): the hits
                    // wait their turn, never overwrite it.
                    work.pending_semantic = Some(hits);
                }
            }
        },
        Ok(Err(e)) => {
            app.message = Some(ta(
                "msg-semantic-failed",
                &[("error", &detail_for_bar(&error_category(&e)))],
            ));
        }
        // Aborted by Esc: silence, the status bar was already cleared.
        // (A panic in the backend's future falls here too: there are no
        // hits to open, the run is already harvested.)
        Err(_join) => {}
    }
}
