//! The refresh ritual: the tick that reacts to tasks that just finished, the
//! reload of both panes after a mutation, and what has to be released
//! afterward.
//!
//! It used to live in the `ntc` binary's root — a crate DISTINCT from this
//! lib — and it is named by four of the test modules still waiting there for
//! their dependencies to leave.
//!
//! The refresh's three triggers (a mutation finishing in [`on_tick`], the
//! column picker's confirm, and `[ui.columns]`'s hot reload) go through the
//! SAME funnel, [`after_panes_refresh`]: this is what guarantees a stale
//! paginated drainer does not duplicate entries from a re-listed pane.

use norte_core::backend::Backend;
use norte_frontend::layout::BySlot;
use norte_i18n::{t, ta};
use norte_proto::Error;

use crate::app::{App, Modal, error_category, error_message};
use crate::console::Waited;
use crate::fill::{Fill, release_refreshed_fill};
use crate::jobs::SearchRun;
use crate::navigate::listing;
use crate::probes::Probed;
use norte_frontend::busy::{Busy, BusyKind};

/// Whether this task kind's result is a REPORT harvested separately by
/// someone else, and therefore its end is not announced with the generic
/// `done`.
///
/// Only checksums (#311), for two reasons that go together: they mutate
/// nothing — so there are no panes to re-list — and their answer is the
/// harvest's verdict, which a later `done` would cover up. A copy or a
/// delete are the opposite on both counts.
///
/// ```
/// use norte_proto::TaskKind;
/// assert!(norte_tui::refresh::speaks_through_its_report(TaskKind::Checksum));
/// assert!(!norte_tui::refresh::speaks_through_its_report(TaskKind::Copy));
/// ```
#[must_use]
pub fn speaks_through_its_report(kind: norte_proto::TaskKind) -> bool {
    matches!(kind, norte_proto::TaskKind::Checksum)
}

/// Tick: refreshes the panel's snapshots and reacts to tasks that JUST
/// finished — collision with context → to the dialog QUEUE (an open modal is
/// never stepped on, finding B1); the rest → message by category + refresh
/// of both panes (a mutation may have changed them).
/// (Hardcoded message strings until Fluent — phase 9, issue #1.)
/// Returns which panes it REFRESHED (a mutation finished and `refresh_panes`
/// rewrote them with the complete listing): the run loop then applies
/// [`after_panes_refresh`]'s ritual — a stale drainer from a re-listed pane
/// would duplicate entries if it stayed alive.
pub async fn on_tick(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
) -> [bool; 2] {
    let finished = app.board.tick(app.now_ms());
    // Every tick, not just when something finishes: the row that expires
    // finished on a PREVIOUS tick, so hanging `finished`'s cleanup off it
    // would leave it on screen until some other task happened to pass
    // through here again.
    app.board.prune_terminal(app.now_ms());
    // The light bar looks at the board AFTER copying the snapshots: the same
    // instant the processes panel paints (ADR 0146).
    app.note_strip();
    if finished.is_empty() {
        app.open_next_pending();
        return [false; 2];
    }
    let mut refresh = false;
    for fin in finished {
        use norte_proto::TaskState;
        let report = report_to_request(&fin);
        match fin.state {
            // #139: counting mutates nothing, so it does not reload the
            // panels — and its result IS its progress: the last snapshot
            // carries the total.
            TaskState::Completed if fin.progress.kind == norte_proto::TaskKind::DirSize => {
                let (bytes, entries) = (fin.progress.bytes_done, fin.progress.entries_done);
                // If the properties dialog was waiting for THIS count, the
                // number goes there; if not, to the bar.
                if !app.properties_sized(fin.progress.task_id, bytes, entries) {
                    // "At least" when part of the tree could not be read
                    // (#251). The short number is the dangerous direction
                    // for this error — this count is used to decide whether
                    // something fits at the destination — so stating it flat
                    // without having been able to count it whole is a wrong
                    // answer, not an incomplete one.
                    //
                    // `Some(0)` and `None` are NOT the same: the first is "I
                    // counted them and there were none", the second "whoever
                    // emits this does not count it" (a 0.52 daemon). Only
                    // the first authorizes stating the total flat.
                    let skipped = fin.progress.unreadable.unwrap_or(0);
                    let size = norte_frontend::human_bytes(bytes);
                    let count = entries.to_string();
                    app.message = Some(if skipped > 0 {
                        ta(
                            "msg-dir-size-partial",
                            &[
                                ("size", &size),
                                ("count", &count),
                                ("skipped", &skipped.to_string()),
                            ],
                        )
                    } else {
                        ta("msg-dir-size", &[("size", &size), ("count", &count)])
                    });
                }
            }
            // #311: the one that answers with a REPORT already said its
            // piece, and did not mutate anything that needs re-listing. A
            // generic `done` here would step on the verdict, which is the
            // only answer the gesture had to give.
            TaskState::Completed if speaks_through_its_report(fin.progress.kind) => {}
            TaskState::Completed => {
                refresh = true;
                // #314: a permissions batch that finishes "fine" may not
                // have changed half of it — a symlink, a file owned by
                // someone else — and a plain `done` reads as if it did. The
                // number is in the progress; what was missing was saying it.
                let undone = fin.progress.unreadable.unwrap_or(0);
                app.message = Some(
                    if fin.progress.kind == norte_proto::TaskKind::SetMode && undone > 0 {
                        ta("msg-chmod-partial", &[("n", &undone.to_string())])
                    } else {
                        t("msg-done")
                    },
                );
                // #290: the file `pane.edit-new` sent to be created ALREADY
                // exists; the editor opens now and over the path that was
                // requested, not over whatever is under the cursor.
                if let Some(pending) = take_creation(app, fin.progress.task_id) {
                    // #303's check does NOT go here: between this point and
                    // launching, `refresh_panes` runs, so asking now would
                    // leave behind exactly the window that was meant to be
                    // narrowed. The suspension carries the path and the run
                    // loop asks right next to the `exec`.
                    match crate::gestures::edit_created(&pending) {
                        Ok(shell) => app.pending_shell = Some(shell),
                        // The file WAS CREATED and the editor cannot be
                        // opened: it says so. Swallowing the `None` left
                        // `msg-done` in the bar and half the gesture lost
                        // with no word, which is exactly the kind of silence
                        // this command came to remove.
                        Err(msg) => app.message = Some(msg),
                    }
                }
                // #250: the archive was written whole and can still carry
                // inside two entries that on macOS or Windows are one. The
                // `Completed` is true and does not cover this, so it asks.
                if fin.progress.kind == norte_proto::TaskKind::Pack
                    && let Some(notice) = pack_notice(backend, fin.progress.task_id).await
                {
                    app.message = Some(notice);
                }
            }
            TaskState::Cancelled => {
                // One that does not mutate has no panes to re-list, not even
                // cancelled.
                refresh = refresh || !speaks_through_its_report(fin.progress.kind);
                app.message = Some(t("msg-cancelled"));
                // With no file there is nothing to edit: the intent is
                // released so the NEXT `edit-new` does not open this one's
                // file.
                drop(take_creation(app, fin.progress.task_id));
            }
            TaskState::Failed { error } => {
                // A creation that failed — policy, journal, a name the
                // provider refuses — opens NOTHING: opening the editor over
                // a file that does not exist is letting the editor create
                // it, which is exactly what #290 removed from the picture.
                drop(take_creation(app, fin.progress.task_id));
                if let (Error::Unsupported, Some(target)) = (&error, &fin.trash_target) {
                    // The trash could not do it HERE (a mount with no
                    // topdir…): it is re-offered as PERMANENT with a notice
                    // — degradation with the user informed (ADR 0009), never
                    // stepping on a modal.
                    if app.modal.is_none() {
                        app.modal = Some(Modal::ConfirmDelete {
                            // Re-offer of THAT item, not the batch: the rest
                            // of the batch's tasks run their course.
                            // Confirming it goes back through
                            // `submit_deletes`, which CONSUMES the marks —
                            // the original batch's were already consumed
                            // when it was submitted, so it would only affect
                            // marks made in the window between the submit
                            // and this tick (with no modal open).
                            items: vec![target.clone()],
                            permanent: true,
                        });
                    } else {
                        app.message = Some(t("msg-no-trash-here"));
                    }
                } else if let (Error::Conflict { .. }, Some(retry)) = (&error, fin.retry) {
                    app.pending_collisions.push_back(retry);
                } else {
                    // Localized render by CATEGORY (spec §17.7, #20): never
                    // the English Display nor OS strings.
                    app.message = Some(error_message(&error));
                    refresh = refresh || !speaks_through_its_report(fin.progress.kind);
                }
            }
            _ => {}
        }
        if let Some((kind, id, failure)) = report
            && let Some(p) = request_report(backend, kind, id, failure).await
        {
            app.pending_reports.push_back(p);
        }
    }
    app.open_next_pending();
    // The open timeline is re-read when something finishes: what has just
    // been done — or undone — has to appear in a panel that follows what is
    // seen. The undo cap already stops going past what was counted; this is
    // so what was counted is what is current now.
    if app.timeline_slot().is_some() {
        crate::dispatch::load_timeline(app, backend, None).await;
    }
    if refresh {
        refresh_panes(app, backend, events).await
    } else {
        [false; 2]
    }
}

/// What has to be shown from a rename batch's report right after it
/// finishes, or `None` if there is nothing to look for.
///
/// The same criterion as the window (`Controller::batch_report`): a clean
/// batch opens nothing; one that left something half-done, does. A report
/// that could not be requested is only mentioned if the Task also failed or
/// was cancelled — if it finished fine, the board row is enough, and if not,
/// the directory would be left unexplained.
///
/// ```
/// use norte_proto::methods::FsRenameBatchReportResult;
/// let clean = FsRenameBatchReportResult {
///     applied: 2, rolled_back: 0, failed_pair: None, stuck: None,
///     uncertain: None, compensations_lost: 0,
/// };
/// assert!(norte_tui::refresh::batch_report(&Ok(clean), false).is_none());
/// let no_report = Err(norte_proto::Error::Unsupported);
/// assert!(norte_tui::refresh::batch_report(&no_report, false).is_none());
/// assert!(norte_tui::refresh::batch_report(&no_report, true).is_some());
/// ```
#[must_use]
pub fn batch_report(
    result: &Result<norte_proto::methods::FsRenameBatchReportResult, Error>,
    task_failed: bool,
) -> Option<Vec<norte_frontend::ReportLine>> {
    match result {
        Ok(r) if norte_frontend::batch_report_is_clean(r) => None,
        Ok(r) => Some(norte_frontend::batch_report_lines(r, norte_i18n::active())),
        Err(_) if !task_failed => None,
        Err(e) => Some(vec![norte_frontend::ReportLine::Phrase(t(
            if matches!(e, Error::Unsupported) {
                "modal-batch-unsupported"
            } else {
                "modal-batch-report-failed"
            },
        ))]),
    }
}

/// Whether the Task that just finished has a REPORT to request: a rename
/// batch or an undo. Both account in it for what the Task's outcome does not
/// cover — a stuck step, skipped irreversibles — so it is always requested,
/// the same as the window does. The `bool` is "the Task failed or was
/// cancelled".
fn report_to_request(
    fin: &crate::tasks::Finished,
) -> Option<(norte_proto::TaskKind, norte_proto::TaskId, bool)> {
    matches!(
        fin.progress.kind,
        norte_proto::TaskKind::RenameBatch | norte_proto::TaskKind::Undo
    )
    .then_some((
        fin.progress.kind,
        fin.progress.task_id,
        !matches!(fin.state, norte_proto::TaskState::Completed),
    ))
}

/// Requests the report for a batch or an undo that just finished, and
/// returns the dialog to open (its title and its lines), or `None` if there
/// is nothing to say.
///
/// Awaited INSIDE the tick, like `pack_notice`: against a hung daemon the
/// tick already stalls in `refresh_panes`, right behind it.
async fn request_report(
    backend: &Backend,
    kind: norte_proto::TaskKind,
    id: norte_proto::TaskId,
    failure: bool,
) -> Option<(crate::app::ReportKind, Vec<norte_frontend::ReportLine>)> {
    if kind == norte_proto::TaskKind::Undo {
        undo_report(&backend.undo_report(id).await, failure)
            .map(|l| (crate::app::ReportKind::Undo, l))
    } else {
        batch_report(&backend.rename_batch_report(id).await, failure)
            .map(|l| (crate::app::ReportKind::Batch, l))
    }
}

/// What has to be shown from an undo's report right after it finishes, or
/// `None` if it returned everything.
///
/// The same criterion as the window (`Controller::undo_report`) and as
/// [`batch_report`]: what was skipped — irreversible, or a creation left
/// in place because the destination has no trash — counts as not returned,
/// and is said.
///
/// ```
/// use norte_proto::methods::PolicyUndoReportResult;
/// let report = |skipped| PolicyUndoReportResult {
///     undone: 2, skipped_irreversible: skipped, skipped_created_no_trash: 0,
///     skipped_not_ours: 0,
///     blocked: None, batch_stuck: None, compensations_lost: 0,
///     denied: Vec::new(), denied_total: 0,
/// };
/// assert!(norte_tui::refresh::undo_report(&Ok(report(0)), false).is_none());
/// assert!(norte_tui::refresh::undo_report(&Ok(report(1)), false).is_some());
/// ```
#[must_use]
pub fn undo_report(
    result: &Result<norte_proto::methods::PolicyUndoReportResult, Error>,
    task_failed: bool,
) -> Option<Vec<norte_frontend::ReportLine>> {
    match result {
        Ok(r) if norte_frontend::undo_report_is_clean(r) => None,
        Ok(r) => Some(norte_frontend::undo_report_lines(r, norte_i18n::active())),
        Err(_) if !task_failed => None,
        Err(e) => Some(vec![norte_frontend::ReportLine::Phrase(t(
            if matches!(e, Error::Unsupported) {
                "modal-undo-unsupported"
            } else {
                "modal-undo-report-failed"
            },
        ))]),
    }
}

/// The notice for a pack that just finished, or `None` if there is nothing
/// to say (#250).
///
/// Asked upon COMPLETING an `archive.pack` — a cancelled one has no archive
/// to warn about — and the common answer is that there is nothing. That a
/// clean archive says nothing is what makes saying something mean something.
///
/// A failed call is also `None`: if it could not be asked, there is no
/// finding to report about the archive, and painting "could not check" over
/// a pack that went fine is noise.
///
/// The call is awaited INSIDE the tick, like the `refresh_panes` that comes
/// after it: against a hung daemon the tick already stalls there, so
/// spawning just this one would buy little and cost a channel.
async fn pack_notice(backend: &Backend, task_id: norte_proto::TaskId) -> Option<String> {
    let report = backend.archive_pack_report(task_id).await.ok()?;
    let risky = report.risky.len();
    if risky == 0 {
        return None;
    }
    // A TRUNCATED report says "at least", which is the only honest thing:
    // the list is cut at `ARCHIVE_PACK_REPORT_MAX` and painting "64" over an
    // archive with four hundred is exactly the lie `truncated` exists to
    // prevent. Same shape as `fs.dir_size`'s "at least" (#251).
    let key = if report.truncated {
        "msg-pack-warnings-partial"
    } else {
        "msg-pack-warnings"
    };
    Some(ta(key, &[("risky", &risky.to_string())]))
}

/// `pane.edit-new`'s intent IF the task that just finished is its own,
/// consuming it (#290).
///
/// The id is compared on purpose: between the submit and this tick any other
/// task can finish — a copy, a delete, another creation — and opening the
/// editor with the first one that comes along would open the wrong file.
///
/// **Only the id, with no connection epoch, and that rests on an SDK
/// invariant**: after a daemon handover ids start over again (the window
/// does carry an epoch for this reason — `Controller::epoch_connection`). Here
/// it is correct because `norte-client` synthesizes a `Failed` outcome for
/// every orphaned task BEFORE the new connection hands out ids, keeping the
/// `task_id`: the intent is consumed on the old connection. If that
/// synthesis went away, a recycled id would open the editor over a file that
/// may not have been created — and the editor would then create it, which is
/// the whole bug all over again.
fn take_creation(app: &mut App, finished: norte_proto::TaskId) -> Option<norte_proto::VPath> {
    match &app.pending_edit_open {
        Some((id, _)) if *id == finished => app.pending_edit_open.take().map(|(_, p)| p),
        _ => None,
    }
}

/// Reloads both panes after a mutation (they may show the same dir).
/// CANCELABLE like the cd (rule 3): Esc abandons the refresh (the panes stay
/// as they were), Ctrl-C quits. The cursor is kept by INDEX (after a delete
/// it lands on the next entry — orthodox semantics).
/// Returns which panes REALLY received the complete listing (#117 review):
/// a half-done Esc abandons the rest — the caller
/// ([`after_panes_refresh`]) uses this to decide whether to release the
/// paginated drainer (#78).
pub async fn refresh_panes(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
) -> [bool; 2] {
    let mut refreshed = [false; 2];
    for i in 0..app.panes.len() {
        // A pane in virtual search mode (liveSearch T6) does NOT
        // auto-refresh: `refresh_listing` would pull it out of virtual mode
        // and `reap` would cancel the Task without the user leaving (review
        // MINOR-1). Its hits live outside the FS: there is no real dir to
        // reload.
        if app.panes[i].virtual_search {
            continue;
        }
        let dir = app.panes[i].dir().clone();
        // #117: same attrs as a cd to this dir — the refresh must not leave
        // the attr cells blank (values only if requested).
        let attrs = app.columns.attr_ids_for(dir.scheme());
        // #323: this is also a wait that eats the loop, and it also froze
        // the screen — worse than navigation, because the reader did not ask
        // for it: it fires when a task finishes and on every watcher notice,
        // and here it waits for the COMPLETE listing, not the first page. A
        // remote directory with many entries left the TUI mute for a good
        // while with nobody having touched a key.
        let started = std::time::Instant::now();
        app.busy = Some(Busy::new(BusyKind::Listing, Some(dir.clone()), Some(i)));
        let waited =
            crate::console::wait_painting(events, app, started, listing(backend, &dir, &attrs))
                .await;
        app.busy = None;
        match waited {
            // The listing is COMPLETE: if it came from a cd paginated
            // halfway through filling in, it is no longer loading (the run
            // loop releases the drainer after this refresh). A live quick
            // search is re-applied inside (new indices).
            Waited::Done(Ok((entries, skipped))) => {
                app.panes[i].refresh_listing(entries);
                // #96: the refresh brings the skipped ones FRESH — without
                // this, the badge kept the previous listing's (stale) value
                // after a mutation.
                app.panes[i].set_skipped(skipped);
                refreshed[i] = true;
            }
            // The connection asks for its password (#325): a refresh IS a
            // reader's gesture, so here it ASKS instead of answering with
            // the error's category.
            //
            // This is the path really walked when reopening: session
            // restore leaves the panel over the remote path with no
            // question asked — restoring is not asking to connect — and the
            // first Ctrl+R is what turns that into the question. Without
            // this, the only path that asked was navigating by hand, i.e.
            // leaving the spot you were at to be able to come back.
            Waited::Done(Err(Error::SecretNeeded { conn, endpoint })) => {
                app.modal = Some(Modal::AskSecret {
                    conn,
                    endpoint,
                    input: crate::app::TypedSecret::default(),
                    dir: dir.clone(),
                    pane: i,
                    // `Record` and not a trail step: a refresh did not leave
                    // the history, so there is nothing to rewind if the
                    // question is abandoned. The panel stays where it is.
                    trail: crate::app::Trail::Record,
                });
                return refreshed;
            }
            // No silence: the dir may have disappeared (issue #20).
            Waited::Done(Err(e)) => {
                app.message = Some(ta("msg-refresh-error", &[("error", &error_category(&e))]));
            }
            // A half-done Esc abandons the REST of the panes, same as before.
            Waited::Cancelled => return refreshed,
            Waited::Quit => {
                app.quit = true;
                return refreshed;
            }
        }
    }
    refreshed
}

/// The ritual after a [`refresh_panes`], SINGLE for its three triggers (a
/// mutation finishing in `on_tick`, the picker's confirm, and
/// `[ui.columns]`'s hot reload — #117 review): the paginated drainer is only
/// released if its pane was really re-listed (releasing it blindly after a
/// half-done Esc would leave the pane hanging in `loading` forever, #78 —
/// its fill is still valid); probe #52's dedup is invalidated (a new listing
/// re-lazifies the entries and a re-probe of the SAME selection is
/// legitimate, MAJOR-1); and the search run is harvested ([`reap_search_run`]
/// is already a no-op if its pane is still in virtual mode).
///
/// And, if the help is open, its facts are RE-FROZEN (review MAJOR-2). The
/// freeze exists so a verdict does not change because the reader moves
/// around the page; not to survive the listing it describes ceasing to
/// exist. `enterable` and `viewable` talk about the entry under the cursor,
/// and this is the funnel all THREE refresh triggers go through — including
/// the `tick`'s, which has no overlay guard, so a copy or a delete end up
/// re-listing the panes with the help in front. Re-freezing here keeps "no
/// verdict changes because the reader scrolls" and drops "no verdict changes
/// because the world changes".
pub fn after_panes_refresh(
    app: &mut App,
    refreshed: [bool; 2],
    fill: &mut BySlot<Fill>,
    last_probed: &mut Probed,
    search_run: &mut Option<SearchRun>,
) {
    if refreshed == [false; 2] {
        return;
    }
    // A refresh is the moment free space may have changed with nobody
    // navigating: it is requested again along with it.
    app.volumes_stale = true;
    release_refreshed_fill(&app.panes, &refreshed, fill, last_probed);
    reap_search_run(app, search_run);
    if app.help.is_some() {
        app.freeze_help_facts();
    }
}

/// Releases the [`SearchRun`] if its pane LEFT virtual mode (a `cd`/refresh
/// turned it off): its drainer would feed a real listing. Cancels the Task
/// if it is still alive (rule 3).
pub fn reap_search_run(app: &App, search_run: &mut Option<SearchRun>) {
    if let Some(s) = search_run.as_ref()
        && !app.panes[s.pane].virtual_search
    {
        s.task.cancel();
        *search_run = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::app_with_entries;

    fn tid(n: u64) -> norte_proto::TaskId {
        norte_proto::TaskId::new(n)
    }

    fn stuck() -> norte_proto::methods::FsRenameBatchReportResult {
        norte_proto::methods::FsRenameBatchReportResult {
            applied: 1,
            rolled_back: 1,
            failed_pair: Some(1),
            stuck: Some(norte_proto::methods::RenameStuckStep {
                from: norte_proto::VPath::parse("mem:///d/a").expect("wire"),
                to: norte_proto::VPath::parse("mem:///d/b").expect("wire"),
                pair_index: 0,
                error: Error::Io { retryable: false },
                journalled: true,
                still_applied: 1,
            }),
            uncertain: None,
            compensations_lost: 0,
        }
    }

    /// A batch that left a stuck step opens its report, and with another
    /// modal open it WAITS in the queue instead of stepping on it or being
    /// lost: it opens as soon as the other one closes.
    #[test]
    fn a_stuck_batch_opens_its_report_without_stepping_on_another_modal() {
        let mut app = app_with_entries(&["a"]);
        let lines = batch_report(&Ok(stuck()), true).expect("it must be said");
        assert!(
            lines
                .iter()
                .any(|l| matches!(l, norte_frontend::ReportLine::Path(_))),
            "says where to look: {lines:?}"
        );
        app.modal = Some(Modal::ConfirmQuit);
        app.pending_reports
            .push_back((crate::app::ReportKind::Batch, lines));
        app.open_next_pending();
        assert!(
            matches!(app.modal, Some(Modal::ConfirmQuit)),
            "does not step on it"
        );
        app.modal = None;
        app.open_next_pending();
        assert!(
            matches!(app.modal, Some(Modal::Report { .. })),
            "opens once the screen is free"
        );
    }

    /// An undo that skipped an irreversible says so, with the same modal as
    /// the batch but its own title: before, the terminal stayed silent about
    /// it and the reader believed the whole tree had been returned.
    #[test]
    fn an_undo_that_skipped_something_opens_its_report() {
        let report = |skipped| norte_proto::methods::PolicyUndoReportResult {
            undone: 2,
            skipped_irreversible: skipped,
            skipped_created_no_trash: 0,
            skipped_not_ours: 0,
            blocked: None,
            batch_stuck: None,
            compensations_lost: 0,
            denied: Vec::new(),
            denied_total: 0,
        };
        let mut app = app_with_entries(&["a"]);
        let lines = undo_report(&Ok(report(1)), false).expect("it must be said");
        app.pending_reports
            .push_back((crate::app::ReportKind::Undo, lines));
        app.open_next_pending();
        assert!(matches!(
            app.modal,
            Some(Modal::Report {
                kind: crate::app::ReportKind::Undo,
                ..
            })
        ));
        assert!(
            undo_report(&Ok(report(0)), true).is_none(),
            "an undo that returned everything opens nothing"
        );
    }

    /// The report's text puts the path ALONE on its line (#273): a name
    /// cannot fake a report sentence it does not share.
    #[test]
    fn the_reports_path_stands_alone_on_its_line() {
        let lines = batch_report(&Ok(stuck()), false).expect("report");
        let text = crate::ui::report_text(&lines);
        assert!(
            text.lines().any(|l| l.trim() == "⟨mem⟩/d/b"),
            "the current path, alone: {text}"
        );
    }

    /// #290: `edit-new`'s intent is consumed by ITS OWN task and only that
    /// one. Any other one finishing meanwhile — a copy, a delete — leaves it
    /// intact; opening it with the first one that comes along would open a
    /// different file.
    #[test]
    fn only_the_creations_task_takes_the_intent() {
        let mut app = app_with_entries(&["a"]);
        let target = norte_proto::VPath::parse("mem:///notas.txt").expect("wire");
        app.pending_edit_open = Some((tid(7), target.clone()));

        assert_eq!(take_creation(&mut app, tid(9)), None, "another task, no");
        assert!(
            app.pending_edit_open.is_some(),
            "and leaves it where it was"
        );

        assert_eq!(take_creation(&mut app, tid(7)), Some(target));
        assert!(
            app.pending_edit_open.is_none(),
            "consumed: a second outcome reopens nothing"
        );
        assert_eq!(take_creation(&mut app, tid(7)), None);
    }
}
