//! The `:` box in the terminal: the live `sel` preview while typing, Esc
//! putting the marks back, and running an order through the doors F5, F6,
//! F7 and F8 already use (ADR 0185).

use norte_frontend::goto::{Action, Mode};
use norte_frontend::orders::{self, Context, Step};
use norte_i18n::t;

use norte_core::backend::Backend;
use norte_proto::VPath;

use crate::app::{App, TransferKind, error_message};
use crate::jobs::{InFlight, OrderMeasureRun, OrderMeasured};

/// After every edit of the box. In `:` mode, previews the line's `sel` over
/// the focused pane from the marks it had when `:` was typed; leaving `:`
/// mode puts those marks back.
pub fn after_edit(app: &mut App) {
    let Some(goto) = &app.goto else {
        return;
    };
    let in_order = goto.mode() == Mode::Order;
    if !in_order && app.order_marks_before.is_none() {
        return;
    }
    let query = goto.query().to_owned();
    let before = match app.order_marks_before.take() {
        Some(b) => b,
        None => orders::marks_before(app.focused().state()),
    };
    let now = app.now_ms();
    let lang = norte_i18n::active();
    app.order_preview = orders::preview(app.focused_mut().state_mut(), &query, &before, now, lang);
    if in_order {
        app.order_marks_before = Some(before);
    } else {
        app.order_preview.clear();
    }
}

/// Esc: the marks from before the box come back.
pub fn cancel(app: &mut App) {
    if let Some(before) = app.order_marks_before.take() {
        orders::restore(app.focused_mut().state_mut(), &before);
    }
    app.order_preview.clear();
}

/// Runs an order. Selection and mutations happen here; a final `cd` or
/// catalogue command comes back as the [`Action`] the box already knows how
/// to run, so it takes exactly the path its key or row would.
///
/// A `sel` that needs rows the lazy listing never measured starts a job
/// (`work.order_measure`) and returns; [`measured`] runs the order when the
/// stats land.
pub async fn run(
    app: &mut App,
    backend: &Backend,
    work: &mut InFlight,
    order: &norte_orders::Order,
) -> Option<Action> {
    cancel_measure(app, work);
    go(app, backend, Some(work), order).await
}

/// Esc, or the next edit of the box: the order still measuring is aborted
/// and runs nothing. Whether there was one.
pub fn cancel_measure(app: &mut App, work: &mut InFlight) -> bool {
    let Some(run) = work.order_measure.take() else {
        return false;
    };
    run.handle.abort();
    app.message = None;
    true
}

/// The measuring landed: into the listing, then the order runs — once, and
/// only if the pane is where it was asked and the box was not opened again;
/// a row whose stat failed stays unknown and does not match.
pub async fn measured(
    app: &mut App,
    backend: &Backend,
    work: &mut InFlight,
    res: OrderMeasured,
) -> Option<Action> {
    let run = work.order_measure.take()?;
    // Aborted: whoever aborted it already said so.
    let stats = res.ok()?;
    if app.goto.is_some() || app.focus() != run.pane || app.focused().dir() != &run.dir {
        app.message = Some(t("msg-order-stale"));
        return None;
    }
    for (path, size, mtime) in stats {
        app.focused_mut().state_mut().hydrate(&path, size, mtime);
    }
    app.order_marks_before = Some(run.before);
    go(app, backend, None, &run.order).await
}

/// The order's steps from the marks before the box. `work` is where a
/// measuring would go; `None` once it has measured.
async fn go(
    app: &mut App,
    backend: &Backend,
    work: Option<&mut InFlight>,
    order: &norte_orders::Order,
) -> Option<Action> {
    // The marks from before the box: every path that does not act gives
    // them back, and the selection below starts from them, so `mark.restore`
    // afterwards returns to them too.
    let before = app
        .order_marks_before
        .take()
        .unwrap_or_else(|| orders::marks_before(app.focused().state()));
    app.order_preview.clear();
    orders::restore(app.focused_mut().state_mut(), &before);
    let here = app.focused().dir().clone();
    let other = app.target_index().map(|i| app.panes[i].dir().clone());
    let cx = Context {
        here: &here,
        other: other.as_ref(),
        listing: app.focused().state().real_entries(),
        enc: app.focused().state().name_encoding(),
    };
    let steps = match orders::steps(order, &cx) {
        Ok(s) => s,
        Err(e) => {
            app.message = Some(orders::order_error_text(&e, norte_i18n::active()));
            return None;
        }
    };
    // F7's own refusal, per pane: the folder the order creates in.
    if let Some(Step::Mkdir { path }) = steps.last() {
        let parent = path.parent().unwrap_or_else(|| path.clone());
        let read_only = app
            .panes
            .iter()
            .position(|p| *p.dir() == parent)
            .map_or_else(
                || {
                    norte_frontend::availability::read_only(
                        app.caps(&parent).copied(),
                        parent.scheme(),
                    )
                },
                |i| app.pane_read_only(i),
            );
        if read_only {
            let reason = norte_help::Reason::ReadOnlyBackend;
            app.message = Some(t(norte_frontend::availability::reason_key(reason)));
            return None;
        }
    }
    if let Some(work) = work {
        let missing: Vec<VPath> = steps
            .iter()
            .filter_map(|s| match s {
                Step::Select(q) => Some(orders::unmeasured(app.focused().state(), q)),
                _ => None,
            })
            .flatten()
            .collect();
        if !missing.is_empty() {
            work.order_measure = Some(OrderMeasureRun {
                handle: tokio::spawn(measure(backend.clone(), missing)),
                order: order.clone(),
                pane: app.focus(),
                dir: here,
                before,
            });
            app.message = Some(t("msg-order-measuring"));
            return None;
        }
    }
    let now = app.now_ms();
    let mut matched = None;
    for step in &steps {
        if let Step::Select(q) = step {
            let Ok(n) = app.focused_mut().state_mut().select_query(q, now) else {
                // The glob error quotes the pattern verbatim: never painted
                // raw (`PatternError`'s own rule).
                orders::restore(app.focused_mut().state_mut(), &before);
                app.message = Some(t("err-bad-pattern"));
                return None;
            };
            matched = Some(n);
        }
    }
    let runnable = orders::runnable(&steps, matched);
    let Some(last) = runnable.last() else {
        orders::restore(app.focused_mut().state_mut(), &before);
        app.message = Some(t("msg-order-nothing-matched"));
        return None;
    };
    match last {
        Step::Select(_) => None,
        Step::Copy { to } | Step::Move { to } => {
            let kind = if matches!(last, Step::Copy { .. }) {
                TransferKind::Copy
            } else {
                TransferKind::Move
            };
            // The same refusal the typed-destination prompt gives: every mark
            // onto itself is N failed tasks, not a copy.
            if *to == here {
                app.message = Some(t("msg-transfer-dest-same"));
            } else {
                app.open_transfer_to_dir(kind, app.focus(), to.clone(), None);
            }
            None
        }
        Step::Trash => {
            // F8's own probe: trash if the provider has one, else the same
            // dialog warns it is permanent (ADR 0009).
            if let Some(first) = app.focused().marked_paths().first() {
                let has_trash = backend
                    .capabilities(first)
                    .await
                    .is_ok_and(|c| c.flags.contains(norte_proto::CapabilityFlags::TRASH));
                app.open_delete_modal(!has_trash);
            }
            None
        }
        Step::Mkdir { path } => {
            match backend.mkdir(path).await {
                Ok(task) => app.board.push(&task, None),
                Err(e) => app.message = Some(error_message(&e)),
            }
            None
        }
        Step::Cd { to } => Some(Action::Ir(to.clone())),
        Step::Command(id) => Some(Action::Command(id.clone())),
        // The timeline panel IS the log: it pages, groups batches and
        // undoes from a row.
        Step::Log(_) => Some(Action::Command("layout.timeline".to_owned())),
        Step::Undo(n) => {
            undo(app, backend, *n).await;
            None
        }
    }
}

/// Stats in flight at once while measuring.
const MEASURE_PARALLEL: usize = 32;

/// The lazy listing (#52) measured only the painted rows: stat the rest the
/// query needs, or `size>` silently skips them. A job, not the key: it is
/// cancelled by aborting it (rule 3), never by a clock.
async fn measure(backend: Backend, paths: Vec<VPath>) -> Vec<(VPath, Option<u64>, Option<i64>)> {
    use futures::StreamExt as _;
    let backend = &backend;
    futures::stream::iter(paths)
        .map(|path| async move {
            let st = backend.stat(&path).await;
            (path, st)
        })
        .buffer_unordered(MEASURE_PARALLEL)
        .filter_map(|(path, st)| async move { st.ok().map(|st| (path, st.size, st.mtime_ms)) })
        .collect()
        .await
}

/// `undo N`: the timeline's own confirmation, over the reader's newest N
/// operations, with the ceiling frozen at what was counted.
async fn undo(app: &mut App, backend: &Backend, n: u32) {
    let page = match backend
        .journal_list(
            None,
            norte_proto::methods::JOURNAL_LIST_MAX_PAGE,
            Some("user"),
        )
        .await
    {
        Ok(p) => p,
        Err(e) => {
            app.message = Some(error_message(&e));
            return;
        }
    };
    match orders::undo_cut(&page.rows, page.next_before_seq, n) {
        Ok(cut) => {
            app.modal = Some(crate::app::Modal::ConfirmUndoAfter {
                seq: cut.seq,
                to_undo: cut.summary.to_undo,
                irreversible: cut.summary.irreversible,
                foreign: cut.summary.foreign,
                techo: cut.techo,
            });
        }
        Err(orders::UndoRefusal::Nothing) => app.message = Some(t("timeline-undo-nothing")),
        Err(orders::UndoRefusal::TooFar) => app.message = Some(t("msg-order-undo-too-far")),
    }
}
