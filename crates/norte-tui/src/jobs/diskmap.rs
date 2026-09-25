//! The disk map measurement: launch it and harvest its report (phase 4).
//!
//! Same split as the checksum batch (#311) and for the same reason (rule 3):
//! measuring a `$HOME` takes minutes, so the wait is SPAWNED and harvested in
//! the loop. Waiting for it here would leave the TUI without drawing,
//! without keys and without being able to cancel — which is exactly when
//! someone cancels.

use norte_core::backend::Backend;
use norte_i18n::t;
use norte_proto::Error;

use crate::app::App;
use crate::jobs::{DiskMapRun, InFlight};

/// Launches `fs.dir_usage` over the focused listing's directory.
///
/// The map describes WHAT IS BEING LOOKED AT: it points at the active
/// listing, and if it already pointed elsewhere, whatever was measured
/// before is forgotten before asking for anything — the previous
/// directory's map under the new title is the wrong answer for exactly as
/// long as the measurement lasts.
pub async fn launch(app: &mut App, backend: &Backend, work: &mut InFlight) {
    let Some(slot) = app.disk_map_slot() else {
        return; // the panel isn't open: nothing to measure
    };
    let dir = app.focused().dir().clone();
    if let Some(m) = app.panes.disk_map_mut(slot)
        && m.dir() != Some(&dir)
    {
        m.aim(dir.clone());
    }
    let params = norte_proto::methods::FsDirUsageParams {
        path: dir.clone(),
        // One level: it's what a map draws, and it's the only thing the
        // server serves today. Asking for more is REJECTED, not truncated
        // (ADR 0117).
        depth: 1,
    };
    match backend.dir_usage(params).await {
        Ok(task) => {
            app.message = Some(t("msg-disk-map-started"));
            app.board.push(&task, None);
            let id = task.id();
            let observer = task.observer();
            let mut progress = task.progress();
            if let Some(m) = app.panes.disk_map_mut(slot) {
                m.measuring(id);
            }
            let b = backend.clone();
            let handle = tokio::spawn(async move {
                // The report is only DEFINITIVE once the Task is terminal.
                // Asking for it earlier would give half a map without saying
                // it is one, and half a map reads as a small directory.
                while !progress.borrow().state.is_terminal() {
                    if progress.changed().await.is_err() {
                        break;
                    }
                }
                // The state travels WITH the report, as with the checksums:
                // cancelled or failed means what's there is partial, and a
                // `changed()` that dies without reaching terminal — the
                // daemon went down — is neither of those.
                let state = progress.borrow().state.clone();
                if !state.is_terminal() {
                    return (state, Err(Error::ProviderUnavailable { retryable: true }));
                }
                (state, b.dir_usage_report(id).await)
            });
            if let Some(old) = work.disk_map.replace(DiskMapRun {
                handle,
                task: observer,
                slot,
                dir,
            }) {
                // Cancel the TASK, not just the wait: aborting the
                // `JoinHandle` left the core walking a whole tree with
                // nobody to collect the result. Navigating fast left three.
                old.task.cancel();
                old.handle.abort();
            }
        }
        Err(e) => {
            // The map is aimed already and will not ask again: it has to
            // SAY it failed, or it reads as a map still measuring.
            if let Some(m) = app.panes.disk_map_mut(slot) {
                m.failure(crate::app::error_message(&e));
            }
            app.message = Some(crate::app::error_message(&e));
        }
    }
}

/// Lands the report of a measurement that already finished.
///
/// **Whatever arrives late is DISCARDED.** Measuring takes time, and in that
/// time the panel may be pointing at another directory: a report landed
/// without checking that would paint the sizes of one place under another's
/// title.
pub fn harvest(
    app: &mut App,
    work: &mut InFlight,
    res: Result<
        (
            norte_proto::TaskState,
            Result<norte_proto::methods::FsDirUsageReportResult, Error>,
        ),
        tokio::task::JoinError,
    >,
) {
    let Some(run) = work.disk_map.take() else {
        return;
    };
    let (state, report) = match res {
        Ok(pair) => pair,
        // A handoff aborts the old handle and the `select!`'s arm only
        // polls the new one from then on, so that does NOT land here. What
        // does land here is a future panic, and nobody has put a message
        // there.
        Err(join) => {
            if join.is_panic() {
                app.message = Some(t("msg-disk-map-partial"));
            }
            return;
        }
    };
    let report = match report {
        Ok(r) => r,
        Err(e) => {
            if let Some(m) = app.panes.disk_map_mut(run.slot) {
                m.failure(crate::app::error_message(&e));
            }
            app.message = Some(crate::app::error_message(&e));
            return;
        }
    };
    // Is the panel still where it was? If not, this is from another
    // directory.
    let Some(map) = app.panes.disk_map_mut(run.slot) else {
        return;
    };
    if map.dir() != Some(&run.dir) {
        return;
    }
    let complete = state == norte_proto::TaskState::Completed;
    map.land(report, complete);
    if !complete {
        // What's there is a fragment correct to look at NOW, with its
        // notice up front. It isn't saved to the cache: `DiskMap::land`
        // only declares the measurement done when complete, and the cache
        // only accepts what's finished.
        app.message = Some(t("msg-disk-map-partial"));
    }
}
