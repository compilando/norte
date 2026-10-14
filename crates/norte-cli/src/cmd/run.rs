//! `norte run '<order>'`: the box's order grammar from a script (ADR 0185).
//!
//! The grammar is `norte-orders`' and the matching `norte-frontend::orders`'
//! — the same a pane runs — so a line tried in the box does the same here.
//! There is no pane: `sel` reads `--cwd`, `>other` is refused, and a
//! catalogue command or `cd` has nothing to act on.

use std::ffi::OsStr;
use std::process::ExitCode;

use anyhow::Context as _;
use norte_core::backend::Backend;
use norte_frontend::orders::{self, CompiledQuery, Context, Step};
use norte_proto::{DeleteMode, Entry, VPath};

use crate::EXIT_CANCELLED;
use crate::cmd::connect::vpath;
use crate::task;

/// Exit code for an order that cannot run as written.
const BAD_ORDER: u8 = 2;

/// Says why and exits 2. Wrapped in `Result` because every caller returns it
/// as its own result.
#[allow(clippy::unnecessary_wraps)]
fn refuse(msg: &str) -> anyhow::Result<ExitCode> {
    eprintln!("{msg}");
    Ok(ExitCode::from(BAD_ORDER))
}

/// A matched name as the grammar writes it: what a script can paste back
/// into an order, and nothing a terminal would act on.
fn shown(p: &VPath) -> String {
    norte_orders::escape_name(p.file_name().map_or(&b""[..], |n| n.as_bytes()))
}

/// The order argument as the grammar reads it: bytes that are not UTF-8 (a
/// Latin-1 name a shell glob expanded) become `\xHH`, which the grammar reads
/// back as exactly those bytes — never a lossy `�`.
///
/// # Errors
/// The byte offset of an invalid byte right after an open `\`: escaped, it
/// would read as `\\` + `xHH`, another name.
fn order_line(arg: &OsStr) -> Result<String, usize> {
    use std::fmt::Write as _;
    let bytes = arg.as_encoded_bytes();
    let mut out = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        out.push_str(chunk.valid());
        for b in chunk.invalid() {
            let open = out.bytes().rev().take_while(|c| *c == b'\\').count() % 2 == 1;
            if open {
                return Err(out.len() - 1);
            }
            let _ = write!(out, "\\x{b:02x}");
        }
    }
    Ok(out)
}

/// Whether resolving the order needs the folder's listing: a `sel`, or a
/// relative name that must take the listed bytes (NFC, `norte-frontend`).
fn wants_listing(order: &norte_orders::Order) -> bool {
    use norte_orders::{Dest, Stage};
    order.stages.iter().any(|s| {
        matches!(
            s,
            Stage::Sel(_)
                | Stage::Mkdir(_)
                | Stage::Cd(_)
                | Stage::Cp(Dest::Path(_))
                | Stage::Mv(Dest::Path(_))
        )
    })
}

/// The switches of `norte run`: four independent command-line flags, which
/// is what they are — not a state machine in disguise.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct RunFlags {
    /// Print what would run; run nothing.
    pub dry_run: bool,
    /// Machine-readable output.
    pub json: bool,
    /// `undo` is allowed.
    pub yes: bool,
    /// `sel` may match dotfiles.
    pub hidden: bool,
}

pub(crate) async fn run(
    backend: &Backend,
    arg: &OsStr,
    cwd: &std::path::Path,
    flags: RunFlags,
) -> anyhow::Result<ExitCode> {
    let RunFlags {
        dry_run,
        json,
        hidden,
        ..
    } = flags;
    let lang = norte_i18n::active();
    let line = match order_line(arg) {
        Ok(line) => line,
        Err(offset) => {
            let why = norte_i18n::t(orders::parse_error_key(
                norte_orders::ParseErrorKind::BadEscape,
            ));
            return refuse(&format!("{why} (byte {offset})"));
        }
    };
    let order = match norte_orders::parse(&line) {
        Ok(o) => o,
        Err(e) => {
            let why = norte_i18n::t(orders::parse_error_key(e.kind));
            return refuse(&format!("{why} (byte {})", e.offset));
        }
    };
    let here = vpath(cwd)?;
    let listing = if wants_listing(&order) {
        Some(backend.list(&here).await)
    } else {
        None
    };
    let known: &[Entry] = match &listing {
        Some(Ok(entries)) => entries,
        _ => &[],
    };
    let steps = match orders::steps(
        &order,
        &Context {
            here: &here,
            other: None,
            listing: known,
            enc: None,
        },
    ) {
        Ok(s) => s,
        Err(e) => return refuse(&orders::order_error_text(&e, lang)),
    };
    let query = steps.iter().rev().find_map(|s| match s {
        Step::Select(q) => Some(q.clone()),
        _ => None,
    });
    let Some(last) = steps.last() else {
        return Ok(ExitCode::SUCCESS);
    };
    let matched = match (&query, listing) {
        (Some(q), Some(entries)) => select(backend, entries, q, hidden).await?,
        _ => Vec::new(),
    };
    let needs_operands = matches!(last, Step::Copy { .. } | Step::Move { .. } | Step::Trash);
    if needs_operands && query.is_none() {
        return refuse(&norte_i18n::t("cli-run-needs-selection"));
    }
    if query.is_some() && matched.is_empty() {
        // Nothing runs; a script reading `--json` still gets its answer.
        if json {
            return report(last, &matched, json);
        }
        eprintln!("{}", norte_i18n::t("msg-order-nothing-matched"));
        return Ok(ExitCode::SUCCESS);
    }
    if dry_run {
        if let Step::Undo(n) = last {
            return undo_preview(backend, *n, json).await;
        }
        return report(last, &matched, json);
    }
    act(backend, last, matched, flags).await
}

/// Runs the acting stage over what `sel` matched.
async fn act(
    backend: &Backend,
    last: &Step,
    matched: Vec<VPath>,
    flags: RunFlags,
) -> anyhow::Result<ExitCode> {
    let RunFlags { json, yes, .. } = flags;
    match last {
        Step::Select(_) => report(last, &matched, json),
        Step::Copy { to } | Step::Move { to } => {
            let moving = matches!(last, Step::Move { .. });
            let mut code = ExitCode::SUCCESS;
            for from in &matched {
                let Some(name) = from.file_name() else {
                    continue;
                };
                let dest = to.join(name.clone());
                let opts = norte_core::TransferOptions::default();
                let context = if moving {
                    "cli-enqueue-move"
                } else {
                    "cli-enqueue-copy"
                };
                let done = task::run_guarded(backend, true, context, || async {
                    if moving {
                        backend.move_(from, &dest, opts).await
                    } else {
                        backend.copy(from, &dest, opts).await
                    }
                })
                .await?;
                // Ctrl-C stops the batch, not just the item in flight.
                if done == ExitCode::from(EXIT_CANCELLED) {
                    return Ok(done);
                }
                if done != ExitCode::SUCCESS {
                    code = done;
                }
            }
            Ok(code)
        }
        Step::Trash => {
            let mut code = ExitCode::SUCCESS;
            for p in &matched {
                let done = task::run_guarded(backend, false, "cli-enqueue-delete", || {
                    backend.delete(p, DeleteMode::Trash)
                })
                .await?;
                if done == ExitCode::from(EXIT_CANCELLED) {
                    return Ok(done);
                }
                if done != ExitCode::SUCCESS {
                    code = done;
                }
            }
            Ok(code)
        }
        Step::Mkdir { path } => {
            task::run_guarded(backend, false, "cli-enqueue-mkdir", || backend.mkdir(path)).await
        }
        Step::Log(n) => log(backend, *n, json).await,
        Step::Undo(n) => {
            if !yes {
                return refuse(&norte_i18n::t("cli-run-undo-needs-yes"));
            }
            undo(backend, *n).await
        }
        Step::Cd { .. } | Step::Command(_) => refuse(&norte_i18n::t("cli-run-needs-pane")),
    }
}

/// `sel` over the folder's `listing` (without dotfiles unless `hidden`, as a
/// pane with hidden files off), measured where the query needs it (the
/// local listing is lazy, #52), then the panes' own matcher.
async fn select(
    backend: &Backend,
    listing: Result<Vec<Entry>, norte_proto::Error>,
    q: &norte_orders::Query,
    hidden: bool,
) -> anyhow::Result<Vec<VPath>> {
    let mut entries: Vec<Entry> = listing
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context(norte_i18n::t("cli-list-failed"))?;
    if !hidden {
        entries.retain(|e| !orders::is_hidden(e));
    }
    let measure = q.terms.iter().any(|t| {
        matches!(
            t.pred,
            norte_orders::Pred::Size(..) | norte_orders::Pred::Age(..)
        )
    });
    if measure {
        for e in &mut entries {
            if (e.size.is_none() || e.mtime_ms.is_none())
                && let Ok(st) = backend.stat(&e.path).await
            {
                e.size = e.size.or(st.size);
                e.mtime_ms = e.mtime_ms.or(st.mtime_ms);
            }
        }
    }
    // The glob error quotes the pattern verbatim: it never reaches stderr
    // (`PatternError`'s own rule); the key says what is wrong.
    let compiled = CompiledQuery::new(q, None)
        .map_err(|_| anyhow::anyhow!(norte_i18n::t("err-bad-pattern")))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    Ok(entries
        .iter()
        .filter(|e| orders::matches(e, &compiled, now))
        .map(|e| e.path.clone())
        .collect())
}

fn action_name(step: &Step) -> &'static str {
    match step {
        Step::Select(_) => "sel",
        Step::Copy { .. } => "cp",
        Step::Move { .. } => "mv",
        Step::Trash => "trash",
        Step::Mkdir { .. } => "mkdir",
        Step::Cd { .. } => "cd",
        Step::Log(_) => "log",
        Step::Undo(_) => "undo",
        Step::Command(_) => "command",
    }
}

/// What the order would do: the matched rows and the action.
fn report(last: &Step, matched: &[VPath], json: bool) -> anyhow::Result<ExitCode> {
    let dest = match last {
        Step::Copy { to } | Step::Move { to } => Some(to.to_wire()),
        Step::Mkdir { path } => Some(path.to_wire()),
        _ => None,
    };
    if json {
        let v = serde_json::json!({
            "action": action_name(last),
            "dest": dest,
            "matched": matched.iter().map(VPath::to_wire).collect::<Vec<_>>(),
        });
        serde_json::to_writer_pretty(std::io::stdout().lock(), &v)
            .context(norte_i18n::t("cli-serialize-failed"))?;
        println!();
    } else {
        for p in matched {
            println!("{}", shown(p));
        }
        println!(
            "{}",
            norte_i18n::ta(
                "cli-run-would",
                &[
                    ("action", action_name(last)),
                    ("n", &matched.len().to_string()),
                ],
            )
        );
    }
    Ok(ExitCode::SUCCESS)
}

async fn log(backend: &Backend, n: u32, json: bool) -> anyhow::Result<ExitCode> {
    let page = backend
        .journal_list(None, n, None)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if json {
        serde_json::to_writer_pretty(std::io::stdout().lock(), &page.rows)
            .context(norte_i18n::t("cli-serialize-failed"))?;
        println!();
    } else {
        for r in &page.rows {
            println!(
                "{:>6}  {}  {}",
                r.seq,
                r.op,
                norte_encoding::mask_terminal_hazards(&r.path)
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The reader's newest journal page, the one `undo N` cuts.
async fn user_page(backend: &Backend) -> anyhow::Result<norte_proto::methods::JournalListResult> {
    backend
        .journal_list(
            None,
            norte_proto::methods::JOURNAL_LIST_MAX_PAGE,
            Some("user"),
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// `undo N --dry-run`: the counts the confirmation would show, and nothing
/// undone.
async fn undo_preview(backend: &Backend, n: u32, json: bool) -> anyhow::Result<ExitCode> {
    let page = user_page(backend).await?;
    let cut = match orders::undo_cut(&page.rows, page.next_before_seq, n) {
        Ok(cut) => cut.summary,
        Err(orders::UndoRefusal::Nothing) => {
            return refuse(&norte_i18n::t("timeline-undo-nothing"));
        }
        Err(orders::UndoRefusal::TooFar) => {
            return refuse(&norte_i18n::t("msg-order-undo-too-far"));
        }
    };
    if json {
        let v = serde_json::json!({
            "action": "undo",
            "dest": null,
            "matched": [],
            "to_undo": cut.to_undo,
            "irreversible": cut.irreversible,
            "foreign": cut.foreign,
        });
        serde_json::to_writer_pretty(std::io::stdout().lock(), &v)
            .context(norte_i18n::t("cli-serialize-failed"))?;
        println!();
        return Ok(ExitCode::SUCCESS);
    }
    for (key, count, always) in [
        ("timeline-undo-count", cut.to_undo, true),
        ("timeline-undo-skipped", cut.irreversible, false),
        ("timeline-undo-foreign", cut.foreign, false),
    ] {
        if always || count > 0 {
            println!("{}", norte_i18n::ta(key, &[("n", &count.to_string())]));
        }
    }
    Ok(ExitCode::SUCCESS)
}

async fn undo(backend: &Backend, n: u32) -> anyhow::Result<ExitCode> {
    let page = user_page(backend).await?;
    match orders::undo_cut(&page.rows, page.next_before_seq, n) {
        Ok(cut) => {
            task::run_guarded(backend, false, "cli-enqueue-undo", || {
                backend.undo_after(cut.seq, cut.techo)
            })
            .await
        }
        Err(orders::UndoRefusal::Nothing) => refuse(&norte_i18n::t("timeline-undo-nothing")),
        Err(orders::UndoRefusal::TooFar) => refuse(&norte_i18n::t("msg-order-undo-too-far")),
    }
}
