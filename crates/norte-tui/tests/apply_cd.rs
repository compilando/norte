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

fn hueco(i: usize) -> norte_frontend::layout::SlotId {
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
/// It is the entire reason `Cd::Espejado` exists. A `cd` returns ONE
/// outcome and the twelve places that file it away do not know about
/// mirrors; if the mirrored panel's got left behind, its `Fill` — which IS
/// that listing's drain — would be dropped with no `finish_listing`, and
/// that panel would be left with the listing truncated under a "loading…"
/// nobody ever turns off again (#78).
#[test]
fn un_espejo_archiva_los_dos_rellenos() {
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
        Cd::Espejado {
            lector: Box::new(Cd::Filling {
                pane: 0,
                fill: fill(),
            }),
            espejo: Box::new(Cd::Filling {
                pane: 1,
                fill: fill(),
            }),
        },
    );
    assert!(
        f.get(hueco(0)).is_some(),
        "the reading panel's, the one the reader moved"
    );
    assert!(
        f.get(hueco(1)).is_some(),
        "and the one that repeated it: dropping it leaves it half-filled"
    );
}

/// And each half is filed away with ITS OWN semantics: a replace drops its
/// pane's fill even if the other half is still paginating.
#[test]
fn en_un_espejo_cada_mitad_se_archiva_por_su_cuenta() {
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
        Cd::Espejado {
            // The reading pane replaced: its old fill is no longer needed.
            lector: Box::new(Cd::Replaced(0)),
            // The mirror is paginating: its own stays.
            espejo: Box::new(Cd::Filling {
                pane: 1,
                fill: fill(),
            }),
        },
    );
    assert!(f.get(hueco(0)).is_none(), "the replace dropped its own");
    assert!(f.get(hueco(1)).is_some(), "and the paginating one keeps it");
}

/// A REPLACE of the same pane drops its obsolete fill.
#[test]
fn replaced_suelta_el_fill_del_pane() {
    let mut f = en_el_pane_0();
    let mut lp = Probed::new();
    let mut df: BySlot<DecorateFetch> = BySlot::new();
    let mut sr: Option<SearchRun> = None;
    apply_cd(&panes(), &mut f, &mut df, &mut lp, &mut sr, Cd::Replaced(0));
    assert!(
        f.get(hueco(0)).is_none(),
        "the old listing's fill is dropped"
    );
}

/// A replace of ANOTHER pane does not touch a live fill.
#[test]
fn replaced_de_otro_pane_no_toca() {
    let mut f = en_el_pane_0();
    let mut lp = Probed::new();
    let mut df: BySlot<DecorateFetch> = BySlot::new();
    let mut sr: Option<SearchRun> = None;
    apply_cd(&panes(), &mut f, &mut df, &mut lp, &mut sr, Cd::Replaced(1));
    assert!(f.get(hueco(0)).is_some(), "pane 0's fill survives");
}

/// #78: a FAILED cd does NOT drop the fill — the pane stays on its
/// previous listing, which keeps filling (dropping it hung it in loading).
#[test]
fn failed_conserva_el_fill() {
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
        f.get(hueco(0)).is_some(),
        "the previous listing's fill is still alive after a failed cd"
    );
}

/// An abandoned cd touches nothing.
#[test]
fn cancelled_conserva_el_fill() {
    let mut f = en_el_pane_0();
    let mut lp = Probed::new();
    let mut df: BySlot<DecorateFetch> = BySlot::new();
    let mut sr: Option<SearchRun> = None;
    apply_cd(&panes(), &mut f, &mut df, &mut lp, &mut sr, Cd::Cancelled);
    assert!(f.get(hueco(0)).is_some());
}

/// A new listing occupies ITS OWN pane's slot and only that: the other
/// pane's fill keeps draining. It is what lets `pane.mirror` — which sends
/// the OTHER pane somewhere without moving focus — not leave the pane the
/// reader is looking at half-done.
#[test]
fn filling_de_un_pane_no_toca_el_hueco_del_otro() {
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
    assert!(f.get(hueco(0)).is_some(), "pane 0's fill stays in its slot");
    assert!(
        f.get(hueco(1)).is_some(),
        "and the new one occupies its own"
    );
}

/// #118: Ctrl+R re-listed pane 0 (a whole NEW listing) — its old drain
/// would duplicate rows if it stayed alive. Probe #52's dedup also expires:
/// the new listing re-lazifies the entries.
#[test]
fn refreshed_suelta_el_fill_del_pane_relistado() {
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
        f.get(hueco(0)).is_none(),
        "the old listing's drain is dropped"
    );
    assert!(lp.is_empty(), "probe #52's dedup expires");
}

/// #118: Esc halfway through — pane 1 was NEVER re-listed, its paginated
/// fill is still valid (#78: dropping it hung it in loading).
#[test]
fn refreshed_a_medias_conserva_el_fill_del_pane_no_relistado() {
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
        f.get(hueco(1)).is_some(),
        "the NOT re-listed pane's fill survives the halfway Esc"
    );
}

/// And the symmetric case: a refresh of BOTH panes drops both slots.
#[test]
fn refreshed_de_ambos_paneles_suelta_los_dos_huecos() {
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
    assert!(f.get(hueco(0)).is_none() && f.get(hueco(1)).is_none());
}

/// #118: a fully abandoned refresh (Esc before the first pane) or both
/// panes in virtual mode: nothing changed, nothing is touched.
#[test]
fn refreshed_vacio_no_toca_nada() {
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
        f.get(hueco(0)).is_some(),
        "with no pane re-listed, the fill stays"
    );
    assert!(!lp.is_empty(), "with no pane re-listed, the dedup stays");
}
