//! Swapping panes crosses EVERYTHING in flight that points at a slot: the
//! `Fill`s, the decorations, the live search and the watch targets. What it
//! does not do is harvest anything along the way.

use norte_core::backend::TaskRef;
use norte_proto::VPath;
use norte_proto::methods::SearchHits;
use norte_tui::app::{App, Pane, SearchState};
use norte_tui::event_loop::watch_targets;
use norte_tui::fill::{Fill, FillMsg};
use norte_tui::jobs::SearchRun;
use norte_tui::navigate::{Cd, apply_cd, reconcile_swap};
use norte_tui::probes::{DecorateFetch, Probed};
use norte_tui::refresh::reap_search_run;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("test wire")
}

/// A live search RUNNING over `pane`. The Task is synthetic (the core's
/// test `TaskRef`): what is exercised here is not the walker, but the pane
/// index the run loop keeps beside it.
fn search_run(pane: usize) -> SearchRun {
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
        pane,
        prev_dir: vp("file:///antes"),
        hits: 0,
        state: SearchState::Running,
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
    let mut sr: Option<SearchRun> = None;

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
    let mut sr: Option<SearchRun> = None;
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
    let mut sr: Option<SearchRun> = None;
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

/// A LIVE search is also indexed per pane: `SearchRun` keeps the virtual
/// pane that shows the hits, exactly as a fill keeps its own. If the swap
/// does not flip it, the hits keep arriving at the neighboring pane.
#[test]
fn the_swap_flips_the_pane_with_the_live_search() {
    let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
    let mut df: norte_frontend::layout::BySlot<DecorateFetch> =
        norte_frontend::layout::BySlot::new();
    let mut lp = Probed::new();
    let mut sr = Some(search_run(0));

    reconcile_swap(
        norte_tui::panel::SLOT_LEFT,
        norte_tui::panel::SLOT_RIGHT,
        &mut f,
        &mut df,
        &mut lp,
        &mut sr,
    );

    assert_eq!(
        sr.as_ref().expect("the run is still alive").pane,
        1,
        "the search's virtual pane switched sides with its pane"
    );
}

/// And the swap cannot HARVEST the search along the way.
///
/// The virtual pane's `Esc`/`Enter` are the ONLY keys that mode
/// intercepts, so a `Ctrl+U` falls through to the resolver and swaps the
/// panes with a search running. Right after, the same call site goes
/// through [`reap_search_run`], which drops the run when its pane is no
/// longer virtual: with `pane` unflipped it looks at pane 0 — which now has
/// the ordinary listing that came from the other side — and CANCELS the
/// Task silently, leaving pane 1 with half-finished hits stuck at
/// `Running` forever and with no `Esc` handler (which requires a live run
/// FOR THAT pane).
///
/// That is why the flip has to happen INSIDE `reconcile_swap`: once the
/// harvest has passed there is nothing left to save.
#[test]
fn a_swap_does_not_harvest_the_live_search() {
    let mut app = App::new(
        Pane::new(vp("file:///izq"), Vec::new()),
        Pane::new(vp("file:///der"), Vec::new()),
    );
    // Live search on pane 0 (`Alt+F7` left it virtual).
    app.panes[0].begin_search(vp("file:///izq"));
    let mut sr = Some(search_run(0));
    let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
    let mut df: norte_frontend::layout::BySlot<DecorateFetch> =
        norte_frontend::layout::BySlot::new();
    let mut lp = Probed::new();

    // `Ctrl+U`: `dispatch` swaps the panes and the run loop reconciles…
    app.swap_panes();
    let panes = norte_tui::panel::PaneSlots::new(
        Pane::new(vp("mem:///d"), Vec::new()),
        Pane::new(vp("mem:///d"), Vec::new()),
    );
    apply_cd(&panes, &mut f, &mut df, &mut lp, &mut sr, Cd::Swapped);
    // …and the SAME call site harvests right after.
    reap_search_run(&app, &mut sr);

    let s = sr
        .as_ref()
        .expect("the search in progress is NOT cancelled");
    assert_eq!(s.pane, 1, "the hits follow to their new side");
    assert!(
        app.panes[s.pane].virtual_search,
        "and that side is the one in search mode"
    );
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
