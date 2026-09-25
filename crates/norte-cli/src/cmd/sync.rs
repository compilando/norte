//! `norte sync`: plans, shows the plan, asks and applies (ADR 0049).

use std::process::ExitCode;

use anyhow::Context;
use norte_core::backend::Backend;
use norte_frontend::sync::Approval;
use norte_proto::TaskState;

use crate::SyncCliOpts;
use crate::cmd::compare::{parse_compare_criteria, rel_marked, write_error_code};
use crate::cmd::connect::vpath;
use crate::task::{SigintGate, drive_task};

/// `norte sync`: plans, shows, asks, applies — ALL in one connection.
///
/// # Why a single invocation
/// An approved plan is retained PER CONNECTION, in an in-memory registry
/// that starts out empty, and `sync.apply` carries nothing but the
/// `plan_hash`. A CLI that planned in one process and applied in another
/// could not work even if it wanted to: the second one's registry does not
/// know that hash. So the question is asked with the connection alive, and
/// `--dry-run` is this same function without the second half.
///
/// Plans, drains the stream through [`norte_frontend::sync::SyncState`] —
/// the ONLY place where the steps are squared against the closing
/// counters — shows the whole plan, and only THEN resolves the journal,
/// asks (unless `--yes`) and applies. `--dry-run` is this same function cut
/// right before that resolution: no path that goes through `--dry-run`
/// reaches `Backend::sync_apply`.
///
/// # The spool is unmounted on exit, whatever happens
/// This is just the wrapper that guarantees it. `sync.plan` leaves a file
/// in the state directory with the relative listing of the TWO trees
/// (ADR 0049), and the daemon collects it in two places this process does
/// not have: a sweep at startup and a `drop_connection` when each
/// connection closes. Without this, a `--dry-run` — which by definition
/// applies nothing — would leave the file there forever, and the same for
/// every question answered no.
///
/// Lives in a separate function and not at the end of the body because the
/// body has `?`: half a dozen exit paths, and the cleanup has to be in all
/// of them.
pub(crate) async fn sync_cmd(
    backend: &Backend,
    source: &std::path::Path,
    dest: &std::path::Path,
    opts: SyncCliOpts<'_>,
) -> anyhow::Result<ExitCode> {
    // ONCE per invocation, and before any phase: see `SigintGate`. Arming
    // it per phase leaves the `[y/N]` prompt with a `Ctrl+C` that tokio
    // swallows and that no longer kills the process (W2 branch review,
    // BLOCKER-1).
    let sigint = SigintGate::arm();
    let outcome = sync_plan_show_apply(backend, source, dest, opts, &sigint).await;
    // A Ctrl+C during planning no longer kills the process outright
    // (#180: `sync_plan_show_apply` arms its own `watch_ctrl_c` and
    // cancels via the token, so `run_sync_plan` closes the spool before
    // returning here). This call is still needed for the rest: a
    // `--dry-run` or a question answered no would also leave the plan
    // retained if nobody released it.
    backend.drop_retained_plans().await;
    outcome
}

/// The body of [`sync_cmd`], with its early exits. See there for why it is
/// split in two.
async fn sync_plan_show_apply(
    backend: &Backend,
    source: &std::path::Path,
    dest: &std::path::Path,
    opts: SyncCliOpts<'_>,
    sigint: &SigintGate,
) -> anyhow::Result<ExitCode> {
    let source = vpath(source)?;
    let dest = vpath(dest)?;

    // `SyncCompareOptions` DOES derive a real `Default` (unlike
    // `FsCompareParams` in `compare_cmd`), so there is no magic 2000 to
    // repeat here.
    let mut compare = norte_proto::methods::SyncCompareOptions {
        criteria: parse_compare_criteria(opts.criteria)?,
        ..norte_proto::methods::SyncCompareOptions::default()
    };
    if let Some(ms) = opts.mtime_tolerance_ms {
        compare.mtime_tolerance_ms = ms;
    }

    let params = norte_proto::methods::SyncPlanParams {
        source,
        dest,
        mode: opts.mode.into(),
        compare,
        // "absent = the caller doesn't have one" — left at its default
        // (Copy), as the task asks.
        on_unknown: norte_proto::methods::OnUnknown::default(),
        include: None,
    };

    // The Task is born INSIDE `sync_plan`, and the spool's `.part` with
    // it: a `Ctrl+C` arriving between the two has to wait for the handle,
    // not kill the process (which would skip the `Drop` that deletes the
    // `.part`).
    sigint.arming();
    let (task, mut rx) = backend
        .sync_plan(params)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context(norte_i18n::t("cli-sync-failed"))?;

    // A Ctrl+C during the drain cancels the Task via its
    // `CancellationToken` (hard rule 3), like the apply half — and not via
    // the OS's default SIGINT. Before this, this `while let` had no
    // handler at all: Ctrl+C killed the WHOLE process before
    // `run_sync_plan` could see the token and close the spool, so the
    // `.part` that `sync.plan` leaves in `<state>/sync-spools/` stayed
    // orphaned forever (#180) — the TTL only sweeps CLOSED plans and this
    // CLI has no daemon to collect it at startup. Cancelled CLEANLY,
    // instead, `run_sync_plan` sees the token, cuts the flow and calls
    // `writer.finish(PlanOutcome::Interrupted)`, which does delete the
    // `.part`.
    sigint.point_at(&task);

    // The ONLY place where the steps are squared against
    // `SyncPlanDone::counts` is `SyncState`; assembling a `SyncPlan` by
    // hand would be a second chance to forget that check (this task's
    // whole reason for existing).
    let mut state = norte_frontend::sync::SyncState::default();
    while let Some(event) = rx.recv().await {
        match event {
            norte_core::sync::SyncPlanEvent::Steps(batch) => {
                state.on_steps(batch);
            }
            norte_core::sync::SyncPlanEvent::Done(done) => {
                state.on_plan_done(done);
            }
        }
    }
    // The Task finished (the channel closed): the handler no longer has
    // anything to cancel. Without this `abort()`, the `ctrl_c()` inside
    // stays alive forever, waiting for a signal that is no longer useful
    // to anyone.
    sigint.release();

    // The channel closes when the Task finishes, so this `join` does not
    // wait extra. BOTH things are required: that the state has closed
    // (`sync.plan_done` arrived) AND that the Task finished `Completed`.
    // A channel that closes with the dialog still in `Planning` — the
    // Task died, cancelled, or this process's buffer filled up and the
    // routing closed the feed (see `Backend::sync_plan`'s rustdoc) — is
    // exactly the "could not tell" that must not be confused with "no
    // differences": without `sync.plan_done` there is no `plan_hash` and
    // nothing to approve.
    let task_state = task.join().await;
    let plan = match state {
        norte_frontend::sync::SyncState::Ready(plan) if task_state == TaskState::Completed => plan,
        _ => {
            eprintln!(
                "norte: {}",
                norte_i18n::ta(
                    "cli-sync-incomplete",
                    &[("state", &format!("{task_state:?}"))],
                )
            );
            return Ok(ExitCode::from(2));
        }
    };

    // Whether the plan can be approved, and if not why, is decided by
    // `SyncPlan::approval` — the SAME verdict that enables the approve
    // key in the TUI and in the window (rule 7). This CLI used to repeat
    // its three conditions by hand and in a different order, and in that
    // order an empty list read as "already matching" BEFORE looking at
    // integrity: a plan announcing two copies and bringing none would
    // exit with 0.
    //
    // A BLOCKED plan is not shown (there is no plan: `!executable` ⟹
    // empty `steps`, and reading it from the list would say "nothing to
    // sync"), and one already in sync has nothing to show.
    let approval = plan.approval();
    match approval {
        Approval::Blocked => return Ok(report_blockers(plan.done())),
        Approval::InSync => {
            println!("{}", norte_i18n::t("cli-sync-empty"));
            return Ok(ExitCode::SUCCESS);
        }
        Approval::Incomplete(_) | Approval::NothingActs | Approval::Approvable => {}
    }

    if let Err(e) = print_plan(&plan) {
        return Ok(write_error_code(&e));
    }

    match approval {
        // The steps that were just shown do not match what the plan
        // claims to be. They are shown anyway — they are the explanation
        // — but nothing is applied: the plan `sync.apply` would run is
        // the RETAINED one, whole, and approving a list that is not that
        // one is approving blind. Also applies to `--dry-run`: a plan
        // that cannot be shown whole has not been "shown" either.
        Approval::Incomplete(integrity) => {
            eprintln!(
                "norte: {}",
                norte_i18n::ta(
                    "cli-sync-integrity",
                    &[("detail", &format!("{integrity:?}"))],
                )
            );
            return Ok(ExitCode::from(2));
        }
        // Not even one step that writes: everything the plan brings is
        // omissions. There is nothing to approve and applying would not
        // change a byte, but the difference that caused them has not been
        // resolved either — so it is not a 0. With `--dry-run` it is a 1
        // like any other difference.
        Approval::NothingActs if !opts.dry_run => {
            eprintln!("norte: {}", norte_i18n::t("cli-sync-nothing-to-apply"));
            return Ok(ExitCode::from(2));
        }
        _ => {}
    }

    if opts.dry_run {
        return Ok(ExitCode::from(1));
    }
    sync_apply_and_report(backend, &plan, opts.yes, sigint).await
}

/// Shows why a plan cannot be run, and returns the code it exits with
/// (always 2: nothing happened).
///
/// To stderr because it is not the plan — the plan does not exist:
/// `!executable` ⟹ empty `steps` — but the explanation of why not.
///
/// Each blocker is THREE lines — path, anchor if there is one, and reason
/// — never a single one with `: ` in the middle: that was the same shape
/// removed from `cli-sync-failure`, and a name can fake it (corpus
/// `cause_join_spoof`, #189). The anchor matters because three of the four
/// named classes — `AmbiguousDest`, `DestReadOnly`, `DirTooLarge` — name
/// the DESTINATION by definition, and before `blocker_anchor` this list
/// read them with the source's reinterpretation (#152 reproduced against
/// three paths of the other tree).
fn report_blockers(done: &norte_proto::methods::SyncPlanDone) -> ExitCode {
    eprintln!("norte: {}", norte_i18n::t("cli-sync-blocked"));
    let lang = norte_i18n::active();
    let enc = norte_frontend::sync::SyncEncodings::default();
    for blocker in &done.blockers {
        let anchor = norte_frontend::sync::blocker_anchor(blocker);
        // And not plain `rel_display`: a blocker that is not about a
        // specific spot — a read-only destination — carries the ROOT
        // (empty `rel`), and `rel_display` alone paints that as nothing.
        // `rel_display_or_root` is the contract `RelDisplay::text`
        // documents and that no painter honored (#193): "the whole tree",
        // not a blank line.
        let rel =
            norte_frontend::sync::rel_display_or_root(&blocker.rel, enc.for_anchor(anchor), lang);
        eprintln!(
            "  {}",
            norte_i18n::ta("cli-sync-blocker", &[("rel", &rel_marked(&rel))])
        );
        if let Some(q) = norte_frontend::sync::anchor_label(anchor, lang) {
            eprintln!("    {q}");
        }
        eprintln!(
            "    {}",
            norte_i18n::ta(
                "cli-sync-blocker-why",
                &[(
                    "why",
                    &norte_frontend::sync::blocker_label(blocker.kind, lang),
                )],
            )
        );
    }
    // The list arrives CAPPED (`SYNC_MAX_BLOCKERS_REPORTED`) and the total
    // does not: staying silent about the difference would suggest all of
    // them were seen.
    let shown = u64::try_from(done.blockers.len()).unwrap_or(u64::MAX);
    if done.blockers_total > shown {
        eprintln!(
            "  {}",
            norte_i18n::ta(
                "cli-sync-blockers-more",
                &[("n", &done.blockers_total.saturating_sub(shown).to_string(),)],
            )
        );
    }
    ExitCode::from(2)
}

/// Writes the WHOLE plan — header, one step per line, and the summary —
/// to stdout.
///
/// Names this process does not fully control (the destination can spell
/// an entry differently from the source, #152): MARK the masked one, like
/// `ai_cmd` and `compare_cmd` — see [`rel_marked`].
///
/// Via a `BufWriter` released on return: buffered so as not to pay one
/// syscall per step, and returning the write error instead of panicking
/// the way `println!` would (`| head` on a ten-thousand-step plan is the
/// normal way to peek at it). The lock being released HERE matters: what
/// gets printed afterward — the question, the report — must not get ahead
/// of the plan.
///
/// # Errors
/// Whatever stdout's write says; the caller translates it with
/// [`write_error_code`].
/// The LINES of a plan step: one per field, never joined into one.
///
/// Pure and separate from [`print_plan`] so it can be pinned — the e2e
/// only reaches steps without `dest_rel`, which is exactly the branch
/// that does not fail.
///
/// **One field per line** (C2 branch review's encoding audit, MAJOR-2).
/// ` → ` and `  (…)` are ordinary printables that `display_name_with`
/// does not mask, so they arrive WITHOUT [`rel_marked`]'s `!`: a file
/// named `a → mem_b.txt` — corpus `arrow_join_spoof` — faked the whole
/// pair, and one named `backup  (unreadable)` faked the VERDICT, in the
/// list a human reviews looking for what gets deleted. And here it
/// weighs more than in the report: the report comes later, this is the
/// screen BEFORE the `y`. The newline IS a separator a name cannot fake —
/// `\n` is Cc and `is_terminal_hazard` masks it to `U+FFFD`.
///
/// ALL THREE glyphs go in the first line, the same as the TUI: the middle
/// one is the CONFIDENCE of the comparison that produced the step — i.e.
/// "this overwrite is decided by date alone" — and this is the screen
/// where a human says yes to deleting a subtree.
fn plan_step_lines(cells: &norte_frontend::sync::StepCells) -> Vec<String> {
    let lang = norte_i18n::active();
    let mut lines = vec![format!(
        "{}{}{} {}",
        cells.glyphs.kind,
        cells.glyphs.confidence,
        cells.glyphs.undo,
        rel_marked(&cells.rel)
    )];
    // The anchor, when the path does NOT hang off the source. In a list
    // where an unqualified path means "from the source", staying silent
    // AFFIRMS it — and a `DeleteTree`'s `rel` hangs off the destination
    // (MAJOR-1: the CLI was the only one of the three painters dropping
    // this field).
    if let Some(q) = norte_frontend::sync::anchor_label(cells.anchor, lang) {
        lines.push(format!("  {q}"));
    }
    if let Some(d) = &cells.dest_rel {
        lines.push(format!(
            "  {}",
            norte_i18n::ta("cli-sync-step-dest", &[("dest", &rel_marked(d))])
        ));
        // Both halves paint the same (an NFC/NFD pair, typically) without
        // either being hostile: without this the CLI repeats the same
        // string on two lines and nothing explains why (#192).
        if let Some(q) = norte_frontend::sync::dest_twin_label(cells.dest_rel_twin, lang) {
            lines.push(format!("  {q}"));
        }
    }
    // The reason for an omission, or an undo that would not restore the
    // file.
    if let Some(r) = cells.reason {
        lines.push(format!(
            "  {}",
            norte_i18n::ta(
                "cli-sync-step-reason",
                &[("reason", &norte_frontend::sync::reason_label(r, lang))]
            )
        ));
    }
    lines
}

fn print_plan(plan: &norte_frontend::sync::SyncPlan) -> std::io::Result<()> {
    use std::io::Write as _;

    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    writeln!(out, "{}", norte_i18n::t("cli-sync-plan"))?;
    for step in plan.steps() {
        // No per-side reinterpretation: the CLI has no panes, so names
        // are read as they come (`SyncEncodings::default()`).
        let cells = norte_frontend::sync::render_step(
            step,
            plan.dest_trash(),
            norte_frontend::sync::SyncEncodings::default(),
        );
        for line in plan_step_lines(&cells) {
            writeln!(out, "{line}")?;
        }
    }
    for line in plan.summary_lines(norte_i18n::active()) {
        writeln!(out, "{line}")?;
    }
    out.flush()
}

/// The second half of `norte sync` (task 3): resolve the journal, ask
/// unless `--yes`, apply and count. Separate from [`sync_cmd`] for length,
/// not independence — it is only called from there, with the plan that
/// JUST got printed, so there is no path that reaches it without the
/// whole plan already being on screen.
async fn sync_apply_and_report(
    backend: &Backend,
    plan: &norte_frontend::sync::SyncPlan,
    yes: bool,
    sigint: &SigintGate,
) -> anyhow::Result<ExitCode> {
    // The caller already discarded everything that is not `Approvable`,
    // and what got shown was THIS plan. It is checked anyway because what
    // follows writes to someone's disk: if another path ever reaches here,
    // it stops with the same code as everything that never got to write.
    if !plan.can_approve() {
        eprintln!("norte: {}", norte_i18n::t("cli-sync-nothing-to-apply"));
        return Ok(ExitCode::from(2));
    }

    // The journal is resolved HERE, before asking and before writing, and
    // not on the first mutation: what is being decided is whether to
    // rewrite a subtree, and "this cannot be undone" is part of the
    // question, not a footnote after the yes. OUTSIDE the `if !yes` because
    // with `--yes` there is no question to complete but there is still a
    // log someone reads, and that is exactly the path where nobody is
    // looking at the screen.
    //
    // And it STOPS, it does not just warn: `Engine::sync_apply_as` refuses
    // just the same a few lines below, so continuing only changes where
    // the "no" appears and who understands it.
    //
    // **Two reasons, two sentences** (#178). The common case is that
    // SOMEONE ELSE holds the journal: the embedded one is the SAME
    // `journal.db` the daemon opens exclusively, so anyone with an `ntc`
    // or a live daemon lands there, and the remedy — talk to that daemon
    // instead of fighting it for the file — is `--daemon`. But with an
    // UNREADABLE journal that remedy does not exist: `norte daemon run`
    // refuses to start with that same file, so sending the user to
    // `--daemon` would send them into another wall. Telling them which of
    // the two walls they are facing is the whole difference between an
    // actionable message and one that wastes half an hour.
    match backend.journal_obstacle().await {
        Some(norte_core::embedded::NoJournal::Failed(_)) => {
            eprintln!("norte: {}", norte_i18n::t("cli-sync-journal-unreadable"));
            return Ok(ExitCode::from(2));
        }
        Some(_) => {
            eprintln!("norte: {}", norte_i18n::t("cli-sync-unjournalled"));
            return Ok(ExitCode::from(2));
        }
        None => {}
    }

    if !yes {
        use std::io::{IsTerminal as _, Write as _};
        // Without a terminal there is nobody to ask, and a question
        // nobody is going to answer is not asked: it refuses BEFOREHAND,
        // like this same file's TOFU prompt. Reading a `< /dev/null`'s
        // EOF as a "no" would be just as correct as far as what gets
        // written (nothing), and much worse to explain, because the
        // human who set up the cron job is not here to read it; exiting
        // while naming `--yes` does get read tomorrow, in the log.
        if !std::io::stdin().is_terminal() {
            eprintln!("norte: {}", norte_i18n::t("cli-sync-noninteractive"));
            return Ok(ExitCode::from(2));
        }
        // The SECOND question, when the plan deserves it (it deletes
        // destination trees or the undo does not restore everything):
        // `SyncPlan::confirmation` already writes it from `dest_trash`
        // and the counters — there is no second sentence about deletion
        // to write here without risking it saying something different
        // from what the summary already said. Whether it shows up
        // depends on `can_approve`, and its three conditions are checked
        // before reaching here: if they weren't, the LEAST trustworthy
        // plan would be exactly the one asking with a bare `[y/N]`.
        if let Some(confirmation) = plan.confirmation(norte_i18n::active()) {
            eprintln!("{}", confirmation.text);
        }
        eprint!("{} ", norte_i18n::t("cli-sync-confirm"));
        std::io::stderr().flush().ok();
        // stdin is blocking: off the reactor (hard rule 2).
        let line = tokio::task::spawn_blocking(|| {
            let mut s = String::new();
            std::io::stdin().read_line(&mut s).map(|_| s)
        })
        .await
        .context(norte_i18n::t("cli-confirm-read"))??;
        let ans = line.trim().to_ascii_lowercase();
        if ans != "y" && ans != "s" {
            // A "no" is NOT "the trees are in sync". It exits with the
            // same code as everything else that never got to write, which
            // is what a `norte sync src dst && echo ok` needs so as not
            // to lie.
            println!("{}", norte_i18n::t("cli-sync-abort"));
            return Ok(ExitCode::from(2));
        }
    }

    // Same window as in planning: the apply Task is already WRITING
    // before `drive_task` points at it, and a `Ctrl+C` there used to kill
    // the process raw — without clean cancellation and without the
    // `.norte-partial` hard rule 3 promises.
    sigint.arming();
    let apply_task = backend
        .sync_apply(&plan.done().plan_hash)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context(norte_i18n::t("cli-sync-failed"))?;
    let task_id = apply_task.id();
    // `drive_task` and not `run_task`: Ctrl+C has to cancel CLEANLY via
    // the Task's `CancellationToken` (hard rule 3), not kill the process
    // halfway through writing a tree — the trap CLAUDE.md itself names
    // ("cancelling a copy must leave a clean destination or a
    // `.norte-partial`, never an unmarked partial file"). Up to there it
    // is the same as `cp`/`mv`/`rm`/`undo`.
    //
    // Where it diverges (#187): `run_task` would translate a `Cancelled`
    // to "clean destination" and return without asking for the report —
    // true for those four commands, false here. A cancelled `sync.apply`
    // leaves what was applied up to the JOURNALED cutoff (hard rule 4),
    // and `sync.report` is the only way to say how much: the TUI and the
    // GUI already ask for it whenever the Task ends, cancellation
    // included (`harvest_sync_apply`,
    // `norte_frontend::sync::SyncView::on_apply_ended`). This command was
    // the only one of the three frontends that could not say so.
    let final_state = drive_task(apply_task, true, Some(sigint)).await;
    match &final_state {
        TaskState::Completed => {}
        TaskState::Cancelled => {
            eprintln!("{}", norte_i18n::t("cli-sync-cancelled"));
        }
        TaskState::Failed { error } => {
            eprintln!(
                "{}",
                norte_i18n::ta("cli-final-error", &[("error", &error.to_string())])
            );
        }
        other => {
            // The progress channel closed without the Task reaching a
            // terminal outcome (the connection died halfway): there is
            // nothing reliable to ask for, same criterion as `run_task`'s
            // equivalent branch.
            eprintln!(
                "{}",
                norte_i18n::ta("cli-unexpected-state", &[("state", &format!("{other:?}"))])
            );
            return Ok(ExitCode::from(2));
        }
    }
    let report = backend
        .sync_report(task_id)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context(norte_i18n::t("cli-sync-failed"))?;

    print_sync_report(&report);

    // The ONLY 1 this command returns: an apply that FINISHED
    // (`Completed`) and left no failure behind. Everything else — what
    // could not be planned, what was not approved, what could not be
    // applied, what was cancelled and what was applied halfway — is 2.
    // Before #187 a `Task` that `Failed` exited with `run_task`'s
    // `ExitCode::FAILURE` of 1 — the SAME code as a clean apply with no
    // failures, a collision the `match` above already resolved by
    // returning 2 before reaching here for anything that is not
    // `Completed`.
    Ok(ExitCode::from(
        if matches!(final_state, TaskState::Completed) && report.failed == 0 {
            1
        } else {
            2
        },
    ))
}

/// The count line and the failure list of a
/// [`SyncReportResult`](norte_proto::methods::SyncReportResult), in the
/// same format no matter what (#187): a report cancelled halfway prints
/// THE SAME as a complete one, because what got applied up to the cutoff
/// is as real as the rest.
///
/// Separate from [`sync_apply_and_report`] only for length (clippy's
/// `too_many_lines`) — there is no second caller.
fn print_sync_report(report: &norte_proto::methods::SyncReportResult) {
    println!(
        "{}",
        norte_i18n::ta(
            "cli-sync-done",
            &[
                ("done", &report.done.to_string()),
                ("failed", &report.failed.to_string()),
                ("skipped", &report.skipped.to_string()),
            ],
        )
    );
    // And whether this can be undone or not (#208). The CLI is the reader
    // that NEVER had `sync.plan_done` in front of it — it prints a report
    // and exits — so until 0.42.0 this line could not be written: five
    // copies against a trash-less destination and five against one with
    // restorable trash were byte-for-byte the same report. Only when
    // something was applied: telling someone who did nothing "nothing can
    // be undone" is noise.
    if report.done > 0 {
        let outlook = norte_frontend::sync::UndoOutlook::of_report(report);
        println!(
            "{}",
            norte_i18n::t(&format!("sync-outlook-{}", outlook.id()))
        );
    }
    // A failure row is THREE fields and goes on THREE lines, not joined
    // into one with `: ` and ` → ` (encoding audit MAJOR-4). Both joiners
    // are ordinary printables `display_name_with` does not mask, so they
    // arrive WITHOUT `marked`'s `!`: `report :→ copy.txt: permission
    // denied` is a legal name on ext4 and APFS — it is in the corpus, as
    // `cause_join_spoof` — and used to print a whole fabricated row, after
    // a destructive `Mirror`.
    //
    // The newline IS a separator a name cannot fake: `\n` is Cc,
    // `is_terminal_hazard` masks it to `U+FFFD` and the name arrives
    // badged. It is what the GUI gets with sibling elements and a pipe
    // does not have.
    //
    // This list is now also shown after a cancellation (#187): the
    // `match` above only WARNS about how it ended, and the report —
    // this one, with its failures — is requested and printed the same
    // regardless of the terminal outcome.
    for failure in &report.failures {
        // Via `render_failure` and not two loose `rel_display` calls: the
        // destination spelling's folding when the BYTES match is the same
        // rule as a step's, and it lives once for the three frontends
        // (#161). Repeating the same path with an arrow in the middle
        // suggests a rename that did not happen.
        let cells = norte_frontend::sync::render_failure(
            failure,
            norte_frontend::sync::SyncEncodings::default(),
        );
        eprintln!(
            "{}",
            norte_i18n::ta("cli-sync-failure", &[("rel", &rel_marked(&cells.rel))])
        );
        // The anchor, for the same reason as in the plan: `render_failure`
        // computes it and this call exists for it, but the CLI dropped it
        // (encoding audit, MAJOR-1). A `DeleteTree` denied under `Mirror`
        // is a `Mirror`'s most common hostile row, and its `rel` hangs off
        // the DESTINATION: unqualified, the operator is going to fix the
        // wrong tree.
        if let Some(q) = norte_frontend::sync::anchor_label(cells.anchor, norte_i18n::active()) {
            eprintln!("  {q}");
        }
        if let Some(d) = &cells.dest_rel {
            eprintln!(
                "  {}",
                norte_i18n::ta("cli-sync-failure-dest", &[("dest", &rel_marked(d))])
            );
            // #192: with no badge on either half (both are valid UTF-8),
            // an NFC/NFD pair repeats on two lines with nothing explaining
            // it.
            if let Some(q) =
                norte_frontend::sync::dest_twin_label(cells.dest_rel_twin, norte_i18n::active())
            {
                eprintln!("  {q}");
            }
        }
        eprintln!(
            "  {}",
            norte_i18n::ta(
                "cli-sync-failure-cause",
                &[(
                    "cause",
                    &norte_frontend::sync::failure_cause_label(failure.cause, norte_i18n::active(),),
                )],
            )
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cli-sync-blocker` no longer joins the path and the reason with `: `
    /// (#189): the corpus's `cause_join_spoof` fixture carried exactly
    /// that joiner and would have faked a whole row.
    #[test]
    fn a_blocker_does_not_join_path_and_reason_on_one_line() {
        for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
            let rel_line = norte_i18n::ta_in(lang, "cli-sync-blocker", &[("rel", "sub/a.txt")]);
            assert_eq!(rel_line, "sub/a.txt", "{lang:?}: nothing stuck to the path");
            let why_line =
                norte_i18n::ta_in(lang, "cli-sync-blocker-why", &[("why", "dest read only")]);
            assert!(why_line.contains("dest read only"), "{lang:?}: {why_line}");
            assert!(!why_line.contains("sub/a.txt"), "{lang:?}: {why_line}");
        }
    }

    /// A whole-tree blocker (`DestReadOnly`, whose `rel` is the root) is
    /// not painted as an empty path (#193): `report_blockers` uses
    /// `rel_display_or_root`, not plain `rel_display`, precisely for this.
    #[test]
    fn a_whole_tree_blocker_does_not_print_an_empty_path() {
        let root = norte_proto::methods::RelPath::parse_wire("").expect("rel");
        assert!(root.is_root());
        let blocker = norte_proto::methods::SyncBlocker {
            rel: root.clone(),
            kind: norte_proto::methods::SyncBlockerKind::DestReadOnly,
            side: None,
        };
        let anchor = norte_frontend::sync::blocker_anchor(&blocker);
        let rel = norte_frontend::sync::rel_display_or_root(&root, None, norte_i18n::Lang::En);
        assert!(!rel.text.is_empty(), "the root is not painted as nothing");
        assert_eq!(anchor, norte_frontend::sync::RelAnchor::Dest);
    }

    /// #189, with an ADVERSARIAL name: `cause_join_spoof`
    /// (`report :→ copy.txt: permission denied`) carries the TWO joiners a
    /// blocker row would fabricate in-band (` → ` and `: `), and is an
    /// ordinary printable — `display_name_with` does not mask it, so
    /// `rel_marked` does not mark it. The lukewarm test above only covers
    /// the template with innocuous literals; this one runs the REAL name
    /// through the same path `report_blockers` uses
    /// (`rel_display_or_root` + `rel_marked`), which is where a fabricated
    /// row would have to show up if anyone reintroduced the joiner.
    #[test]
    fn a_blocker_with_an_adversarial_name_does_not_fabricate_a_row() {
        let fixture = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|f| f.id == "cause_join_spoof")
            .expect("corpus");
        let blocker = norte_proto::methods::SyncBlocker {
            rel: norte_proto::methods::RelPath::new(vec![
                norte_proto::Segment::new(fixture.bytes.clone()).expect("seg"),
            ]),
            kind: norte_proto::methods::SyncBlockerKind::DestReadOnly,
            side: None,
        };
        let lang = norte_i18n::Lang::En;
        let anchor = norte_frontend::sync::blocker_anchor(&blocker);
        let rel = norte_frontend::sync::rel_display_or_root(
            &blocker.rel,
            norte_frontend::sync::SyncEncodings::default().for_anchor(anchor),
            lang,
        );
        let rel_line = rel_marked(&rel);
        let why_line = norte_frontend::sync::blocker_label(blocker.kind, lang);
        assert!(
            !rel_line.contains(&why_line),
            "the path line does not carry the reason stuck to it: {rel_line:?}"
        );
        assert!(
            !why_line.contains("permission denied"),
            "the reason line does not carry the name's bytes stuck to it: {why_line:?}"
        );
        // And the joiner the fixture carries INSIDE the name is not
        // confused with a structural one: it stays part of the painted
        // text.
        assert!(rel_line.contains("permission denied"), "{rel_line:?}");
    }

    /// `report_blockers` does not panic for any combination of class and
    /// side, and always returns 2 — nothing was applied — whatever the
    /// blocker.
    #[test]
    fn report_blockers_does_not_panic_for_any_class_or_side() {
        use norte_proto::methods::{
            DestTrash, PlanHash, Side, SyncBlocker, SyncBlockerKind, SyncCounts, SyncPlanDone,
        };
        for kind in [
            SyncBlockerKind::AmbiguousDest,
            SyncBlockerKind::OverlapDetected,
            SyncBlockerKind::DestReadOnly,
            SyncBlockerKind::DirTooLarge,
            SyncBlockerKind::TypeMismatchDir,
            SyncBlockerKind::Unknown,
        ] {
            for side in [None, Some(Side::Left), Some(Side::Right)] {
                let done = SyncPlanDone {
                    task_id: norte_proto::TaskId::new(1),
                    plan_hash: PlanHash::parse(&"a".repeat(64)).expect("hex"),
                    counts: SyncCounts::default(),
                    blockers: vec![SyncBlocker {
                        rel: norte_proto::methods::RelPath::parse_wire("sub/a.txt").expect("rel"),
                        kind,
                        side,
                    }],
                    blockers_total: 1,
                    executable: false,
                    dest_trash: DestTrash::Restorable,
                };
                assert_eq!(
                    report_blockers(&done),
                    ExitCode::from(2),
                    "{kind:?}/{side:?}"
                );
            }
        }
    }
}

#[cfg(test)]
mod plan_step_lines_tests {
    use super::plan_step_lines;

    fn seg(b: &[u8]) -> norte_proto::methods::RelPath {
        norte_proto::methods::RelPath::new(vec![
            norte_proto::Segment::new(b.to_vec()).expect("segment"),
        ])
    }

    /// The plan row with BOTH spellings: each field on its own line.
    ///
    /// They used to be joined by ` → ` on the same line, and that
    /// character is an ordinary printable `display_name_with` does not
    /// mask — meaning a name carrying it inside (corpus
    /// `arrow_join_spoof`) faked the pair WITHOUT `rel_marked`'s `!`
    /// tripping. This is the screen where `y` is typed to delete.
    #[test]
    fn the_two_spellings_do_not_share_a_line() {
        let step = norte_proto::methods::SyncStep {
            id: 1,
            kind: norte_proto::methods::SyncStepKind::Overwrite,
            rel: seg(b"a \xe2\x86\x92 mem_b.txt"),
            dest_rel: Some(seg(b"other.txt")),
            size: Some(10),
            criterion: norte_proto::methods::CompareCriterion::Size,
            confidence: norte_proto::methods::CompareConfidence::Certain,
            reversal: Some(norte_proto::methods::StepReversal::Delete),
            reason: None,
        };
        let cells = norte_frontend::sync::render_step(
            &step,
            norte_proto::methods::DestTrash::Restorable,
            norte_frontend::sync::SyncEncodings::default(),
        );
        let lines = plan_step_lines(&cells);
        let first = &lines[0];
        assert!(
            first.contains("mem_b.txt"),
            "the source name goes whole: {first:?}"
        );
        assert!(
            !first.contains("other.txt"),
            "the DESTINATION spelling does not share a line with the name: {first:?}"
        );
        assert!(
            lines.iter().skip(1).any(|l| l.contains("other.txt")),
            "but it is said, on its own line: {lines:?}"
        );
    }

    /// And the anchor IS painted. `render_failure`/`render_step` compute
    /// it and the CLI was the only one of the three painters dropping it:
    /// in a list where an unqualified path means "from the source",
    /// staying silent about a `Dest` affirms it — and a `DeleteTree`'s
    /// `rel` hangs off the destination.
    #[test]
    fn a_delete_tree_says_its_path_is_from_the_destination() {
        let step = norte_proto::methods::SyncStep {
            id: 2,
            kind: norte_proto::methods::SyncStepKind::DeleteTree,
            rel: seg(b"old"),
            dest_rel: None,
            size: None,
            criterion: norte_proto::methods::CompareCriterion::Presence,
            confidence: norte_proto::methods::CompareConfidence::Certain,
            reversal: Some(norte_proto::methods::StepReversal::RestoreTrash),
            reason: None,
        };
        let cells = norte_frontend::sync::render_step(
            &step,
            norte_proto::methods::DestTrash::Restorable,
            norte_frontend::sync::SyncEncodings::default(),
        );
        assert_eq!(cells.anchor, norte_frontend::sync::RelAnchor::Dest);
        let lines = plan_step_lines(&cells);
        let expected = norte_frontend::sync::anchor_label(cells.anchor, norte_i18n::active())
            .expect("Dest has a qualifier");
        assert!(
            lines.iter().skip(1).any(|l| l.contains(&expected)),
            "the anchor's qualifier is painted: {lines:?}"
        );
    }

    /// An NFC/NFD pair (#192) paints the same string on both spelling
    /// lines, and without the note the reader has no way to tell that
    /// apart from a rename that did nothing.
    #[test]
    fn an_nfc_nfd_pair_carries_its_own_note() {
        let fixtures = norte_testkit::corpus::hostile_names();
        let nfc = fixtures
            .iter()
            .find(|f| f.id == "nfc_e_acute")
            .expect("corpus");
        let nfd = fixtures
            .iter()
            .find(|f| f.id == "nfd_e_acute")
            .expect("corpus");
        let step = norte_proto::methods::SyncStep {
            id: 3,
            kind: norte_proto::methods::SyncStepKind::Overwrite,
            rel: seg(&nfc.bytes),
            dest_rel: Some(seg(&nfd.bytes)),
            size: Some(1),
            criterion: norte_proto::methods::CompareCriterion::Size,
            confidence: norte_proto::methods::CompareConfidence::Certain,
            reversal: Some(norte_proto::methods::StepReversal::Delete),
            reason: None,
        };
        let cells = norte_frontend::sync::render_step(
            &step,
            norte_proto::methods::DestTrash::Restorable,
            norte_frontend::sync::SyncEncodings::default(),
        );
        assert!(cells.dest_rel_twin);
        let lines = plan_step_lines(&cells);
        let expected = norte_frontend::sync::dest_twin_label(true, norte_i18n::active())
            .expect("there is a note when twin is true");
        assert!(
            lines.iter().any(|l| l.contains(&expected)),
            "the note is painted: {lines:?}"
        );
    }
}
