//! Swapping panes crosses EVERYTHING in flight that points at a slot: the
//! `Fill`s, the decorations, the live search and the watch targets. What it
//! does not do is harvest anything along the way.

use norte_core::backend::TaskRef;
use norte_frontend::search_status::Outcome;
use norte_proto::VPath;
use norte_proto::methods::SearchHits;
use norte_tui::app::{App, Pane};
use norte_tui::event_loop::watch_targets;
use norte_tui::fill::{Fill, FillMsg};
use norte_tui::jobs::SearchRun;
use norte_tui::navigate::{Cd, apply_cd, reconcile_swap};
use norte_tui::probes::{DecorateFetch, Probed};
use norte_tui::refresh::reap_search_runs;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("test wire")
}

/// A results run still RUNNING. The Task is synthetic (the core's test
/// `TaskRef`): what is exercised here is not the walker, but the slot the
/// run loop files it under.
fn search_run() -> SearchRun {
    let (_tx, rx) = tokio::sync::mpsc::channel::<SearchHits>(1);
    let id = norte_proto::TaskId::new(1);
    let (_progress, prx) = tokio::sync::watch::channel(norte_proto::TaskProgress {
        task_id: id,
        kind: norte_proto::TaskKind::Search,
        state: norte_proto::TaskState::Running,
        bytes_done: 0,
        bytes_total: None,
        entries_done: 0,
        entries_total: None,
        current: None,
        unreadable: None,
        unvisited: None,
    });
    SearchRun {
        task: TaskRef::synthetic_for_tests(id, prx),
        rx,
        outcome: Outcome::Running,
        panel_epoch: None,
    }
}

fn decorate(slot: norte_frontend::layout::SlotId) -> DecorateFetch {
    let (_tx, rx) = tokio::sync::oneshot::channel();
    DecorateFetch {
        slot,
        dir: vp("mem:///d"),
        rx,
    }
}

/// A fill IN FLIGHT is filed PER PANE: if the swap does not cross the
/// slots, the listing's batches keep arriving at the neighboring pane and
/// the reader sees the wrong list grow. It is the bug a green suite does
/// not see, because the listing keeps arriving: it just arrives at the
/// wrong place.
///
/// Checks that THAT drain moved and not just any slot: the batch sent by
/// pane 0's `tx` is picked up from pane 1's slot.
#[test]
fn the_swap_crosses_the_in_flight_filler_slots() {
    let (tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
    let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
    f.insert(norte_tui::panel::SLOT_LEFT, Fill { rx });
    let mut df: norte_frontend::layout::BySlot<DecorateFetch> =
        norte_frontend::layout::BySlot::new();
    df.insert(
        norte_tui::panel::SLOT_LEFT,
        decorate(norte_tui::panel::SLOT_LEFT),
    );
    let mut lp = Probed::from([(0, vp("mem:///d/x"))]);
    let mut sr: norte_frontend::layout::BySlot<SearchRun> = norte_frontend::layout::BySlot::new();

    reconcile_swap(
        norte_tui::panel::SLOT_LEFT,
        norte_tui::panel::SLOT_RIGHT,
        &mut f,
        &mut df,
        &mut lp,
        &mut sr,
    );

    assert!(
        f.get(norte_tui::panel::SLOT_LEFT).is_none(),
        "pane 0's slot is left free"
    );
    tx.try_send(FillMsg::Failed)
        .expect("the drain is still alive");
    assert!(
        f.get_mut(norte_tui::panel::SLOT_RIGHT)
            .expect("crossed to pane 1's slot")
            .rx
            .try_recv()
            .is_ok(),
        "and it is the SAME drain now feeding pane 1"
    );
    assert!(
        df.get(norte_tui::panel::SLOT_RIGHT).is_some()
            && df.get(norte_tui::panel::SLOT_LEFT).is_none(),
        "crossed"
    );
    assert!(lp.is_empty(), "the stat cache is dropped, not translated");
}

/// With nothing in flight the reconciliation is harmless: a swap cannot
/// invent a fill or a fetch where there were none.
#[test]
fn the_swap_with_nothing_in_flight_invents_nothing() {
    let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
    let mut df: norte_frontend::layout::BySlot<DecorateFetch> =
        norte_frontend::layout::BySlot::new();
    let mut lp = Probed::new();
    let mut sr: norte_frontend::layout::BySlot<SearchRun> = norte_frontend::layout::BySlot::new();
    reconcile_swap(
        norte_tui::panel::SLOT_LEFT,
        norte_tui::panel::SLOT_RIGHT,
        &mut f,
        &mut df,
        &mut lp,
        &mut sr,
    );
    assert!(
        f.get(norte_tui::panel::SLOT_LEFT).is_none()
            && f.get(norte_tui::panel::SLOT_RIGHT).is_none()
    );
    assert!(
        df.get(norte_tui::panel::SLOT_LEFT).is_none()
            && df.get(norte_tui::panel::SLOT_RIGHT).is_none()
    );
}

/// The `Cd::Swapped` outcome has to REACH the reconciliation: the half of
/// the swap `dispatch` cannot do travels through `apply_cd`, and an arm
/// that forgot to call it would leave the fill pointing at the wrong pane
/// with no `App` test noticing.
#[test]
fn apply_cd_swapped_reconciles_run_loop_state() {
    let (_tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
    let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
    f.insert(norte_tui::panel::SLOT_RIGHT, Fill { rx });
    let mut df: norte_frontend::layout::BySlot<DecorateFetch> =
        norte_frontend::layout::BySlot::new();
    df.insert(
        norte_tui::panel::SLOT_RIGHT,
        decorate(norte_tui::panel::SLOT_RIGHT),
    );
    let mut lp = Probed::from([(1, vp("mem:///d/x"))]);
    let mut sr: norte_frontend::layout::BySlot<SearchRun> = norte_frontend::layout::BySlot::new();
    let panes = norte_tui::panel::PaneSlots::new(
        Pane::new(vp("mem:///d"), Vec::new()),
        Pane::new(vp("mem:///d"), Vec::new()),
    );
    apply_cd(&panes, &mut f, &mut df, &mut lp, &mut sr, Cd::Swapped);
    assert!(
        f.get(norte_tui::panel::SLOT_LEFT).is_some()
            && f.get(norte_tui::panel::SLOT_RIGHT).is_none()
    );
    assert!(
        df.get(norte_tui::panel::SLOT_LEFT).is_some()
            && df.get(norte_tui::panel::SLOT_RIGHT).is_none()
    );
    assert!(lp.is_empty());
}

/// A results tab's run is filed by SLOT, and a swap moves the tab's content
/// to the other slot: the run has to travel with it, or the harvest that
/// the same call site does right after (`reap_search_runs`) finds a folder
/// under the run's slot and cancels a search the reader is still watching —
/// and the hits that were still coming would have nowhere to land.
///
/// After `swap_panes` + `Cd::Swapped` + the harvest, the run is still alive,
/// filed under the slot that now shows the results tab, and nothing is filed
/// under the slot that now shows a folder.
#[test]
fn the_run_follows_its_results_tab_through_a_swap() {
    use norte_tui::panel::{SLOT_LEFT, SLOT_RIGHT};
    let mut app = App::new(
        Pane::new(vp("file:///izq"), Vec::new()),
        Pane::new(vp("file:///der"), Vec::new()),
    );
    let saved = norte_frontend::results::SavedSearch::from_form(
        &norte_frontend::search::SearchForm::new(),
        vp("file:///izq"),
    );
    app.panes[0].begin_results(norte_frontend::results::ResultSet::new(
        saved,
        norte_frontend::results::MAX_HITS,
    ));
    assert_eq!(app.panes.slot_of(0), SLOT_LEFT);
    let mut sr: norte_frontend::layout::BySlot<SearchRun> = norte_frontend::layout::BySlot::new();
    sr.insert(SLOT_LEFT, search_run());
    let task_id = sr.get(SLOT_LEFT).expect("run").task.id();
    let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
    let mut df: norte_frontend::layout::BySlot<DecorateFetch> =
        norte_frontend::layout::BySlot::new();
    let mut lp = Probed::new();

    // `Ctrl+U`: `dispatch` swaps the panes and the run loop reconciles…
    app.swap_panes();
    apply_cd(&app.panes, &mut f, &mut df, &mut lp, &mut sr, Cd::Swapped);
    // …and the SAME call site harvests right after.
    reap_search_runs(&app, &mut sr);

    let tab = app.panes.browser(SLOT_RIGHT).expect("tab");
    assert!(tab.is_results(), "the results tab moved to the right slot");
    let s = sr
        .get(SLOT_RIGHT)
        .expect("the run is filed under the slot that shows the results");
    assert_eq!(s.task.id(), task_id, "the same run");
    assert!(sr.get(SLOT_LEFT).is_none(), "the folder has no run");
}

/// The watcher does NOT need its own reconciliation, and this is what makes
/// that claim true: the watched set is derived from `app.panes` on every
/// run loop turn (`rewatch(&watch_targets(app))` is the loop's first
/// statement), so it is enough that `watch_targets` caches nothing. If
/// someone introduced a per-side copy, the swap would leave each pane
/// watching the other's dir.
#[test]
fn watch_targets_follows_the_panes_after_the_swap() {
    let mut app = App::new(
        Pane::new(vp("file:///izq"), Vec::new()),
        Pane::new(vp("file:///der"), Vec::new()),
    );
    let before = watch_targets(&app);
    app.swap_panes();
    let after = watch_targets(&app);
    assert_eq!(
        before[0], after[1],
        "the left dir becomes watched on the right"
    );
    assert_eq!(before[1], after[0]);
    assert_ne!(
        before[0], before[1],
        "the two dirs were different to begin with"
    );
}
