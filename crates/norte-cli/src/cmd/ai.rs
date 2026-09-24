//! `norte ai rename` and `norte gc` (M4-A2 / #11, ADR 0031 / ADR 0012).

use std::process::ExitCode;
use std::sync::Arc;

use norte_core::backend::Backend;

use crate::cmd::compare::masked;
use crate::cmd::connect::vpath;
use crate::{AiCmd, JournalWarningStderr};

/// `norte ai rename`: suggests a REVIEWABLE batch rename (M4-A2, ADR
/// 0031). Embedded: builds an engine with `[ai]`'s provider, asks for the
/// plan (the opt-in/local-only/denied-paths gate cuts BEFORE any name
/// comes out), PRINTS it and confirms before applying. Applying = ONE
/// `fs.rename_batch` batch (one Task, one undo unit), as in the TUI and
/// the window — the plan is the product.
pub(crate) async fn ai_cmd(cmd: AiCmd) -> anyhow::Result<ExitCode> {
    let AiCmd::Rename {
        dir,
        instruction,
        yes,
    } = cmd;
    let dir = vpath(&dir)?;
    // From here everything goes through `Backend`, the same path as the
    // TUI and the window: the AI proposes, `fs.rename_batch_plan` decides
    // whether it can happen and `fs.rename_batch` applies (rule 7).
    let backend = backend_with_ia().await?;

    let plan = backend
        .ai_rename_plan(&dir, &instruction, &[])
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if plan.entries.is_empty() {
        println!("{}", norte_i18n::t("cli-ai-rename-empty"));
        return Ok(ExitCode::SUCCESS);
    }
    println!("{}", norte_i18n::t("cli-ai-rename-plan"));
    // Names controlled by the MODEL: mask terminal hazards (bidi/invisibles
    // → �) and MARK the masked one, like TUI/GUI. A valid UTF-8 reply can
    // carry an RLO and spoof the confirmation prompt.
    let mask = |bytes: &[u8]| {
        let (text, hostile) = norte_frontend::display_name(bytes);
        masked(&text, hostile)
    };
    // One name per line, with the same keys as the TUI's modal: `a → b`
    // on a single line let a file named `x → y` — an ordinary printable,
    // `display_name` does not mask it — fake the whole pair, right on the
    // screen read before answering "yes".
    for (i, e) in plan.entries.iter().enumerate() {
        println!(
            "  {}",
            norte_i18n::ta(
                "modal-ai-rename-pair-from",
                &[
                    ("n", &(i + 1).to_string()),
                    ("from", &mask(e.from.as_bytes()))
                ],
            )
        );
        println!(
            "     {}",
            norte_i18n::ta("modal-ai-rename-pair-to", &[("to", &mask(e.to.as_bytes()))])
        );
    }

    // The AI's plan is INTENT; whether it can run is decided by the
    // core's batch planner, which is the one that knows how to break a
    // cycle (`a↔b`) with a temp name and the one that sees collisions
    // with what already exists. Applying entry by entry used to fail on
    // every swap and leave any other plan with a clash half-done.
    //
    // Codes: 2 is "refused, nothing was touched" (invalid plan,
    // collisions, unreadable journal, stale plan), as in `norte sync`; 1
    // is "the batch ran and failed or was undone".
    let Some(pairs) = norte_frontend::rename_pairs(&plan.entries) else {
        eprintln!("norte: {}", norte_i18n::t("cli-ai-rename-invalid"));
        return Ok(ExitCode::from(2));
    };
    let batch = backend
        .rename_batch_plan(&dir, &pairs)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if !batch.executable {
        eprintln!("norte: {}", norte_i18n::t("cli-ai-rename-collisions"));
        let reviewed = norte_frontend::BatchPlan::Ready(Box::new(batch));
        for part in reviewed.detail_parts(pairs.len(), norte_i18n::active()) {
            for line in detail(part) {
                eprintln!("  {line}");
            }
        }
        return Ok(ExitCode::from(2));
    }
    let real = batch.steps.iter().filter(|s| !s.temp).count();

    // The journal is opened HERE, before asking and before renaming, and
    // not on the first rename: what is being decided is whether a model
    // renames a whole directory, and "this cannot be undone" is part of
    // the question, not a footnote after the yes. OUTSIDE the `if !yes`:
    // with `--yes` there is no question to complete, but there is still a
    // human (or a script whose log someone reads) who needs to find out,
    // and that is exactly the path where nobody is watching the screen.
    //
    // The stderr sink just said the reason; here comes the consequence —
    // and since #178 there are TWO different consequences, which a `bool`
    // used to confuse.
    //
    // With `Failed` the renames are not going to happen:
    // `Engine::gate` refuses them one by one. Asking "are you sure? they
    // cannot be undone" and renaming zero files while exiting
    // successfully is the worst of both worlds: a `norte ai rename --yes
    // && <next thing>` in a cron job would keep going over a silent
    // no-op. So it stops here, with the rejections' code.
    match backend.journal_obstacle().await {
        Some(norte_core::embedded::NoJournal::Failed(_)) => {
            eprintln!("norte: {}", norte_i18n::t("cli-ai-rename-refused"));
            return Ok(ExitCode::from(2));
        }
        // `Busy` (and any future reason) DOES mutate, without being
        // recorded: that is a warning, not a reason not to rename.
        Some(_) => eprintln!("norte: {}", norte_i18n::t("cli-ai-rename-unjournalled")),
        None => {}
    }

    if !yes {
        use std::io::Write as _;
        eprint!("{} ", norte_i18n::t("cli-ai-rename-confirm"));
        std::io::stderr().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok();
        let ans = line.trim().to_ascii_lowercase();
        if ans != "y" && ans != "s" {
            println!("{}", norte_i18n::t("cli-ai-rename-abort"));
            return Ok(ExitCode::SUCCESS);
        }
    }

    // ONE batch, ONE Task and ONE undoable journal unit (ADR 0042), with
    // the `plan_hash` of what was just shown: if the directory changed
    // while the human was reading, the core answers `PlanStale` and
    // touches nothing. Policy does not enter: this embedded engine gates
    // with `AllowAll` — who decides here is the human who just said yes
    // to the plan.
    let task = match backend.rename_batch(&dir, &pairs, &batch.plan_hash).await {
        Ok(task) => task,
        Err(norte_proto::Error::PlanStale) => {
            eprintln!("norte: {}", norte_i18n::t("cli-ai-rename-stale"));
            return Ok(ExitCode::from(2));
        }
        Err(e) => return Err(anyhow::anyhow!("{e}")),
    };
    let id = task.id();
    let outcome = crate::task::run_task(task, false).await;
    report_batch(&backend, id, outcome, real).await
}

/// `run_task` says how the Task ended; what actually happened on disk is
/// said by the REPORT, and it is always requested: a `Completed` batch
/// with one stuck step is exactly what the Task's outcome does not tell.
/// The lines are the same as the window's dialog (`batch_report_lines`),
/// and each path stands alone on its own.
async fn report_batch(
    backend: &Backend,
    id: norte_proto::TaskId,
    outcome: ExitCode,
    real: usize,
) -> anyhow::Result<ExitCode> {
    match backend.rename_batch_report(id).await {
        Ok(report) if norte_frontend::batch_report_is_clean(&report) => {
            if outcome == ExitCode::SUCCESS {
                println!(
                    "{}",
                    norte_i18n::ta("cli-ai-rename-done", &[("n", &real.to_string())])
                );
            }
        }
        Ok(report) => {
            for line in norte_frontend::batch_report_lines(&report, norte_i18n::active()) {
                match line {
                    norte_frontend::ReportLine::Phrase(text) => eprintln!("norte: {text}"),
                    norte_frontend::ReportLine::Path(p) => {
                        let (text, hostile) = norte_frontend::path_display(&p);
                        eprintln!("    {}", masked(&text, hostile));
                    }
                }
            }
            return Ok(ExitCode::FAILURE);
        }
        Err(_) if outcome != ExitCode::SUCCESS => {
            eprintln!("norte: {}", norte_i18n::t("modal-batch-report-failed"));
        }
        Err(_) => {}
    }
    Ok(outcome)
}

/// `norte ai rename`'s embedded engine, with `[ai]`'s provider.
///
/// #167: this subcommand does NOT go through `make_backend` — it builds
/// its own engine — and renames a whole directory with the names a MODEL
/// proposed. Of all the embedded paths, it is the one that most needs to
/// end up recorded, so it carries a journal like the others. Lazy like
/// the others too (#177): planning is reading, and reading takes nobody's
/// journal away; the lock is taken one step before renaming.
async fn backend_with_ia() -> anyhow::Result<Backend> {
    let dir = norte_core::connect::config_dir();
    let engine = norte_core::embedded::engine_in(&dir);
    // This branch does not go through `run`, so it installs its own — see
    // `JournalWarningStderr`.
    engine.set_journal_warning_sink(Arc::new(JournalWarningStderr));
    // What every engine carries, AI included (`norte_core::equipo`). The
    // core resolves the secret (env → keyring → age) and builds the
    // provider; the CLI never touches norte-connect nor sees the key
    // (rule 10). Only the renaming one: the embeddings one would resolve
    // another secret this command does not use.
    let ia = norte_core::equipo::Ia {
        renombrado: true,
        embeddings: false,
    };
    let done = norte_core::equipo::equipar(&engine, &dir, ia).await;
    if let Err(e) = norte_core::archive_config::aplicar(&engine).await {
        eprintln!(
            "{}",
            crate::cmd::daemon::warning_text(&norte_core::equipo::Aviso::ArchivoInvalido(
                e.to_string()
            ))
        );
    }
    if done.ia_renombrado {
        return Ok(Backend::Embedded(Arc::new(engine)));
    }
    // Here the AI is not optional: it is the command. What is a warning
    // elsewhere is, here, the reason nothing can be done — and if there
    // is a specific reason (`[ai]` broken, a secret that does not
    // resolve), THAT is the error: telling "define a provider" to someone
    // who already defined one sends them looking where it isn't.
    if let Some(reason) = done.avisos.iter().find(|a| {
        matches!(
            a,
            norte_core::equipo::Aviso::IaInvalida(_)
                | norte_core::equipo::Aviso::IaNoCargo(_)
                | norte_core::equipo::Aviso::IaNoDisponible(_)
        )
    }) {
        anyhow::bail!("{}", crate::cmd::daemon::warning_text(reason));
    }
    anyhow::bail!(
        "sin proveedor de IA para el rename: define [ai.providers.<n>] y \
         rename_provider en norte.toml (ADR 0031)"
    );
}

/// The lines of one part of an unexecutable plan's detail, with the same
/// sanitizing as the modal (`norte_frontend::BatchPlan::detail_parts`):
/// the offending name is a third party's and already arrives masked; here
/// it just gets the mark added.
///
/// The name goes on ITS OWN line, as in the TUI (#273): `display_name`
/// does not mask `✗`, digits or `:`, so a file named
/// `✗ 4. already exists: other.txt` stuck to its label would fake another
/// entry. The name comes out trimmed to the modal's width
/// (`middle_ellipsis`): enough to recognize it, and the whole plan was
/// already printed above without trimming.
fn detail(part: norte_frontend::DetailPart) -> Vec<String> {
    use norte_i18n::{t, ta};
    match part {
        norte_frontend::DetailPart::Temp { count } => {
            vec![ta("modal-rename-batch-temp", &[("n", &count.to_string())])]
        }
        norte_frontend::DetailPart::Collision {
            index,
            kind_key,
            name,
            hostile,
        } => {
            let kind = t(kind_key);
            let prefix = match index {
                Some(n) => ta(
                    "modal-rename-batch-collision-prefix",
                    &[("n", &n.to_string()), ("kind", &kind)],
                ),
                None => ta(
                    "modal-rename-batch-collision-prefix-unindexed",
                    &[("kind", &kind)],
                ),
            };
            vec![prefix, format!("  {}", masked(&name, hostile))]
        }
        norte_frontend::DetailPart::More {
            shown,
            total,
            hostile,
        } => vec![masked(
            &ta(
                "modal-rename-batch-collision-more",
                &[("shown", &shown.to_string()), ("total", &total.to_string())],
            ),
            hostile,
        )],
    }
}

/// `norte gc`: sweeps orphaned `.norte-partial` staging (#11, ADR 0012).
/// Embedded only: the wire does not (yet) expose GC — with `--daemon` the
/// error is actionable, not a bare `Unsupported`.
pub(crate) async fn gc_cmd(
    backend: &Backend,
    daemon: bool,
    path: &std::path::Path,
    older_than_hours: u64,
) -> anyhow::Result<ExitCode> {
    let dir = vpath(path)?;
    let older = std::time::Duration::from_secs(older_than_hours.saturating_mul(3600));
    match backend.gc_partials(&dir, older).await {
        Ok(n) => {
            println!(
                "{}",
                norte_i18n::ta(
                    "cli-gc-result",
                    &[("n", &n.to_string()), ("dir", &dir.display_lossy())],
                )
            );
            Ok(ExitCode::SUCCESS)
        }
        Err(norte_proto::Error::Unsupported) if daemon => {
            anyhow::bail!("{}", norte_i18n::t("cli-gc-remote-unsupported"))
        }
        Err(e) => Err(anyhow::anyhow!("{e}")),
    }
}
