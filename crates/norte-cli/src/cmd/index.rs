//! `norte index build|query|embed|semantic` (M4, ADR 0034).

use std::process::ExitCode;

use anyhow::Context;
use norte_core::backend::Backend;
use norte_proto::EntryKind;

use crate::IndexCmd;
use crate::cmd::connect::vpath;
use crate::task::run_task;

/// `norte index build|query` (M4, ADR 0034).
pub(crate) async fn index_cmd(backend: &Backend, cmd: IndexCmd) -> anyhow::Result<ExitCode> {
    match cmd {
        IndexCmd::Build { path } => {
            let root = vpath(&path)?;
            let task = backend
                .index_build(&root)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
                .context("no se pudo lanzar el index build")?;
            Ok(run_task(task, false).await)
        }
        IndexCmd::Query { path, text, limit } => {
            let root = vpath(&path)?;
            let hits = backend
                .index_query(&root, &text, limit)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
                .context("query del índice")?;
            for h in &hits {
                let marker = match h.kind {
                    EntryKind::Dir => "d",
                    EntryKind::Symlink => "l",
                    EntryKind::Other => "?",
                    EntryKind::File => "-",
                };
                let size = h.size.map_or_else(|| "-".to_string(), |s| s.to_string());
                // `display_lossy` sanitizes hostile bytes (rule 1): never
                // raw controls/non-UTF-8 over stdout.
                println!("{marker}\t{size}\t{}", h.path.display_lossy());
            }
            if hits.is_empty() {
                eprintln!("{}", norte_i18n::t("cli-no-results"));
            }
            Ok(ExitCode::SUCCESS)
        }
        IndexCmd::Embed { path } => {
            let root = vpath(&path)?;
            let task = backend
                .index_embed(&root)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
                .context("no se pudo lanzar el index embed")?;
            Ok(run_task(task, false).await)
        }
        IndexCmd::Semantic { text, root, k } => {
            let root = root.as_deref().map(vpath).transpose()?;
            let hits = backend
                .index_search_semantic(root.as_ref(), &text, k)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
                .context("búsqueda semántica")?;
            for h in &hits {
                // Paths with arbitrary names toward a terminal: the SAME
                // marked masking as `norte ai rename`'s plan
                // (`display_name` per segment via `path_display`, hazards
                // → � and the `!` gives away the alteration).
                let (text, hostile) = norte_frontend::path_display(&h.path);
                println!("{:.2}\t{}{text}", h.score, if hostile { "!" } else { "" });
            }
            if hits.is_empty() {
                eprintln!("{}", norte_i18n::t("cli-no-results"));
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}
