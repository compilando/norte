//! What `apply_cd` does to each slot's `Fill` in flight, event by event:
//! `Replaced` and `Refreshed` drop the affected pane's and only that one;
//! `Failed`, `Cancelled` and `Filling` keep it.

use norte_frontend::layout::BySlot;
use norte_tui::fill::{Fill, FillMsg};
use norte_tui::jobs::SearchRun;
use norte_tui::navigate::{Cd, apply_cd};
use norte_tui::panel::{PaneSlots, SLOT_LEFT, SLOT_RIGHT};
use norte_tui::probes::{DecorateFetch, Probed};

/// Two fake panes: `apply_cd` only asks them which slot each position occupies.
fn panes() -> PaneSlots {
    let d = norte_proto::VPath::parse("mem:///x").expect("wire");
    PaneSlots::new(
        norte_tui::app::Pane::new(d.clone(), Vec::new()),
        norte_tui::app::Pane::new(d, Vec::new()),
    )
}

fn slot(i: usize) -> norte_frontend::layout::SlotId {
    if i == 0 { SLOT_LEFT } else { SLOT_RIGHT }
}
use norte_proto::Error;

fn fill() -> Fill {
    let (_tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
    Fill { rx }
}

/// Pane 0's slot occupied and pane 1's free: the starting layout for almost
/// all of these cases.
fn en_el_pane_0() -> BySlot<Fill> {
    let mut f = BySlot::new();
    f.insert(SLOT_LEFT, fill());
    f
}

/// **A mirrored navigation files away BOTH drains.**
///
/// It is the entire reason `Cd::Mirrored` exists. A `cd` returns ONE
/// outcome and the twelve places that file it away do not know about
/// mirrors; if the mirrored panel's got left behind, its `Fill` — which IS
/// that listing's drain — would be dropped with no `finish_listing`, and
/// that panel would be left with the listing truncated under a "loading…"
/// nobody ever turns off again (#78).
#[test]
fn a_mirror_archives_both_fills() {
    let mut f = BySlot::new();
    let mut lp = Probed::new();
    let mut df: BySlot<DecorateFetch> = BySlot::new();
    let mut sr: Option<SearchRun> = None;
    apply_cd(
        &panes(),
        &mut f,
        &mut df,
        &mut lp,
        &mut sr,
        Cd::Mirrored {
            reader: Box::new(Cd::Filling {
                pane: 0,
                fill: fill(),
            }),
            mirror: Box::new(Cd::Filling {
                pane: 1,
                fill: fill(),
            }),
        },
    );
    assert!(
        f.get(slot(0)).is_some(),
        "the reading panel's, the one the reader moved"
    );
    assert!(
        f.get(slot(1)).is_some(),
        "and the one that repeated it: dropping it leaves it half-filled"
    );
}

/// And each half is filed away with ITS OWN semantics: a replace drops its
/// pane's fill even if the other half is still paginating.
#[test]
fn in_a_mirror_each_half_archives_on_its_own() {
    let mut f = en_el_pane_0();
    let mut lp = Probed::new();
    let mut df: BySlot<DecorateFetch> = BySlot::new();
    let mut sr: Option<SearchRun> = None;
    apply_cd(
        &panes(),
        &mut f,
        &mut df,
        &mut lp,
        &mut sr,
        Cd::Mirrored {
            // The reading pane replaced: its old fill is no longer needed.
            reader: Box::new(Cd::Replaced(0)),
            // The mirror is paginating: its own stays.
            mirror: Box::new(Cd::Filling {
                pane: 1,
                fill: fill(),
            }),
        },
    );
    assert!(f.get(slot(0)).is_none(), "the replace dropped its own");
    assert!(f.get(slot(1)).is_some(), "and the paginating one keeps it");
}

/// A REPLACE of the same pane drops its obsolete fill.
#[test]
fn replaced_releases_the_fill_of_the_pane() {
    let mut f = en_el_pane_0();
    let mut lp = Probed::new();
    let mut df: BySlot<DecorateFetch> = BySlot::new();
    let mut sr: Option<SearchRun> = None;
    apply_cd(&panes(), &mut f, &mut df, &mut lp, &mut sr, Cd::Replaced(0));
    assert!(
        f.get(slot(0)).is_none(),
        "the old listing's fill is dropped"
    );
}

/// A replace of ANOTHER pane does not touch a live fill.
#[test]
fn a_replaced_from_another_pane_does_not_touch_it() {
    let mut f = en_el_pane_0();
    let mut lp = Probed::new();
    let mut df: BySlot<DecorateFetch> = BySlot::new();
    let mut sr: Option<SearchRun> = None;
    apply_cd(&panes(), &mut f, &mut df, &mut lp, &mut sr, Cd::Replaced(1));
    assert!(f.get(slot(0)).is_some(), "pane 0's fill survives");
}

/// #78: a FAILED cd does NOT drop the fill — the pane stays on its
/// previous listing, which keeps filling (dropping it hung it in loading).
#[test]
fn failed_keeps_the_fill() {
    let mut f = en_el_pane_0();
    let mut lp = Probed::new();
    let mut df: BySlot<DecorateFetch> = BySlot::new();
    let mut sr: Option<SearchRun> = None;
    apply_cd(
        &panes(),
        &mut f,
        &mut df,
        &mut lp,
        &mut sr,
        Cd::Failed(Error::NotFound),
    );
    assert!(
        f.get(slot(0)).is_some(),
        "the previous listing's fill is still alive after a failed cd"
    );
}

/// An abandoned cd touches nothing.
#[test]
fn cancelled_keeps_the_fill() {
    let mut f = en_el_pane_0();
    let mut lp = Probed::new();
    let mut df: BySlot<DecorateFetch> = BySlot::new();
    let mut sr: Option<SearchRun> = None;
    apply_cd(&panes(), &mut f, &mut df, &mut lp, &mut sr, Cd::Cancelled);
    assert!(f.get(slot(0)).is_some());
}

/// A new listing occupies ITS OWN pane's slot and only that: the other
/// pane's fill keeps draining. It is what lets `pane.mirror` — which sends
/// the OTHER pane somewhere without moving focus — not leave the pane the
/// reader is looking at half-done.
#[test]
fn filling_of_one_pane_does_not_touch_the_others_slot() {
    let mut f = en_el_pane_0();
    let mut lp = Probed::new();
    let mut df: BySlot<DecorateFetch> = BySlot::new();
    let mut sr: Option<SearchRun> = None;
    apply_cd(
        &panes(),
        &mut f,
        &mut df,
        &mut lp,
        &mut sr,
        Cd::Filling {
            pane: 1,
            fill: fill(),
        },
    );
    assert!(f.get(slot(0)).is_some(), "pane 0's fill stays in its slot");
    assert!(f.get(slot(1)).is_some(), "and the new one occupies its own");
}

/// #118: Ctrl+R re-listed pane 0 (a whole NEW listing) — its old drain
/// would duplicate rows if it stayed alive. Probe #52's dedup also expires:
/// the new listing re-lazifies the entries.
#[test]
fn refreshed_releases_the_fill_of_the_relisted_pane() {
    let mut f = en_el_pane_0();
    let mut lp = Probed::from([(0, norte_proto::VPath::parse("file:///d/x").unwrap())]);
    let mut df: BySlot<DecorateFetch> = BySlot::new();
    let mut sr: Option<SearchRun> = None;
    apply_cd(
        &panes(),
        &mut f,
        &mut df,
        &mut lp,
        &mut sr,
        Cd::Refreshed([true, false]),
    );
    assert!(
        f.get(slot(0)).is_none(),
        "the old listing's drain is dropped"
    );
    assert!(lp.is_empty(), "probe #52's dedup expires");
}

/// #118: Esc halfway through — pane 1 was NEVER re-listed, its paginated
/// fill is still valid (#78: dropping it hung it in loading).
#[test]
fn a_partial_refresh_preserves_the_unrelisted_panes_fill() {
    let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
    f.insert(norte_tui::panel::SLOT_RIGHT, fill());
    let mut lp = Probed::new();
    let mut df: BySlot<DecorateFetch> = BySlot::new();
    let mut sr: Option<SearchRun> = None;
    apply_cd(
        &panes(),
        &mut f,
        &mut df,
        &mut lp,
        &mut sr,
        Cd::Refreshed([true, false]),
    );
    assert!(
        f.get(slot(1)).is_some(),
        "the NOT re-listed pane's fill survives the halfway Esc"
    );
}

/// And the symmetric case: a refresh of BOTH panes drops both slots.
#[test]
fn a_refresh_of_both_panes_releases_both_slots() {
    let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
    f.insert(SLOT_LEFT, fill());
    f.insert(SLOT_RIGHT, fill());
    let mut lp = Probed::new();
    let mut df: BySlot<DecorateFetch> = BySlot::new();
    let mut sr: Option<SearchRun> = None;
    apply_cd(
        &panes(),
        &mut f,
        &mut df,
        &mut lp,
        &mut sr,
        Cd::Refreshed([true, true]),
    );
    assert!(f.get(slot(0)).is_none() && f.get(slot(1)).is_none());
}

/// #118: a fully abandoned refresh (Esc before the first pane) or both
/// panes in virtual mode: nothing changed, nothing is touched.
#[test]
fn an_empty_refresh_touches_nothing() {
    let mut f = en_el_pane_0();
    let mut lp = Probed::from([(0, norte_proto::VPath::parse("file:///d/x").unwrap())]);
    let mut df: BySlot<DecorateFetch> = BySlot::new();
    let mut sr: Option<SearchRun> = None;
    apply_cd(
        &panes(),
        &mut f,
        &mut df,
        &mut lp,
        &mut sr,
        Cd::Refreshed([false, false]),
    );
    assert!(
        f.get(slot(0)).is_some(),
        "with no pane re-listed, the fill stays"
    );
    assert!(!lp.is_empty(), "with no pane re-listed, the dedup stays");
}
