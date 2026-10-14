//! The `:` box in the terminal: the live `sel` preview while typing, Esc
//! putting the marks back, and running an order through the doors F5, F6,
//! F7 and F8 already use (ADR 0185).

use norte_frontend::goto::{Action, Mode};
use norte_frontend::orders::{self, Context, Step};
use norte_i18n::t;

use norte_core::backend::Backend;

use crate::app::{App, TransferKind, error_message};

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
pub async fn run(app: &mut App, backend: &Backend, order: &norte_orders::Order) -> Option<Action> {
    // The box closed on Enter: what the preview marked stays marked, which
    // is what the reader saw and what the step below acts on.
    app.order_marks_before = None;
    app.order_preview.clear();
    let here = app.focused().dir().clone();
    let other = app.target_index().map(|i| app.panes[i].dir().clone());
    let cx = Context {
        here: &here,
        other: other.as_ref(),
    };
    let steps = match orders::steps(order, &cx) {
        Ok(s) => s,
        Err(e) => {
            app.message = Some(orders::order_error_text(&e, norte_i18n::active()));
            return None;
        }
    };
    let now = app.now_ms();
    let mut matched = None;
    for step in &steps {
        if let Step::Select(q) = step {
            match app.focused_mut().state_mut().select_query(q, now) {
                Ok(n) => matched = Some(n),
                Err(e) => {
                    app.message = Some(e.to_string());
                    return None;
                }
            }
        }
    }
    let runnable = orders::runnable(&steps, matched);
    let Some(last) = runnable.last() else {
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
        // `log` and `undo` are wired with the journal (task 6).
        Step::Log(_) | Step::Undo(_) => None,
    }
}
