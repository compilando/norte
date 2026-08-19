//! Qué le hace `apply_cd` al `Fill` en vuelo de cada hueco, evento a evento:
//! `Replaced` y `Refreshed` sueltan el del pane afectado y solo ese;
//! `Failed`, `Cancelled` y `Filling` lo conservan.

use norte_frontend::layout::BySlot;
use norte_tui::fill::{Fill, FillMsg};
use norte_tui::jobs::SearchRun;
use norte_tui::navigate::{Cd, apply_cd};
use norte_tui::panel::{PaneSlots, SLOT_LEFT, SLOT_RIGHT};
use norte_tui::probes::{DecorateFetch, Probed};

/// Dos paneles de mentira: `apply_cd` solo les pregunta qué hueco ocupa
/// cada posición.
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

/// El hueco del pane 0 ocupado y el del 1 libre: la disposición de
/// partida de casi todos estos casos.
fn en_el_pane_0() -> BySlot<Fill> {
    let mut f = BySlot::new();
    f.insert(SLOT_LEFT, fill());
    f
}

/// Un REEMPLAZO del mismo pane suelta su relleno obsoleto.
#[test]
fn replaced_suelta_el_fill_del_pane() {
    let mut f = en_el_pane_0();
    let mut lp = Probed::new();
    let mut df: BySlot<DecorateFetch> = BySlot::new();
    let mut sr: Option<SearchRun> = None;
    apply_cd(&panes(), &mut f, &mut df, &mut lp, &mut sr, Cd::Replaced(0));
    assert!(
        f.get(hueco(0)).is_none(),
        "el fill del listado viejo se suelta"
    );
}

/// Un reemplazo de OTRO pane no toca el relleno vivo.
#[test]
fn replaced_de_otro_pane_no_toca() {
    let mut f = en_el_pane_0();
    let mut lp = Probed::new();
    let mut df: BySlot<DecorateFetch> = BySlot::new();
    let mut sr: Option<SearchRun> = None;
    apply_cd(&panes(), &mut f, &mut df, &mut lp, &mut sr, Cd::Replaced(1));
    assert!(f.get(hueco(0)).is_some(), "el fill del pane 0 sobrevive");
}

/// #78: un cd FALLIDO NO suelta el relleno — el pane sigue en su listado
/// anterior, que se sigue rellenando (soltarlo lo colgaba en loading).
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
        "el fill del listado anterior sigue vivo tras un cd fallido"
    );
}

/// Un cd abandonado no toca nada.
#[test]
fn cancelled_conserva_el_fill() {
    let mut f = en_el_pane_0();
    let mut lp = Probed::new();
    let mut df: BySlot<DecorateFetch> = BySlot::new();
    let mut sr: Option<SearchRun> = None;
    apply_cd(&panes(), &mut f, &mut df, &mut lp, &mut sr, Cd::Cancelled);
    assert!(f.get(hueco(0)).is_some());
}

/// Un listado nuevo ocupa el hueco DE SU PANE y solo ese: el relleno del
/// otro pane sigue drenando. Es lo que hace que `pane.mirror` —que manda
/// el OTRO pane a un sitio sin mover el foco— no pueda dejar a medias el
/// pane que el lector está mirando.
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
    assert!(
        f.get(hueco(0)).is_some(),
        "el relleno del pane 0 sigue en su hueco"
    );
    assert!(f.get(hueco(1)).is_some(), "y el nuevo ocupa el suyo");
}

/// #118: Ctrl+R re-listó el pane 0 (listado COMPLETO nuevo) — su
/// drenador viejo duplicaría filas si siguiera vivo. La dedup de la
/// sonda #52 también caduca: el listado nuevo re-lazifica las entries.
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
        "el drenador del listado viejo se suelta"
    );
    assert!(lp.is_empty(), "la dedup de la sonda #52 caduca");
}

/// #118: Esc a medias — el pane 1 NO llegó a re-listarse, su relleno
/// paginado sigue siendo válido (#78: soltarlo lo colgaba en loading).
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
        "el fill del pane NO re-listado sobrevive al Esc a medias"
    );
}

/// Y el simétrico: un refresh de los DOS panes suelta los dos huecos.
#[test]
fn refreshed_de_ambos_panes_suelta_los_dos_huecos() {
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

/// #118: refresh totalmente abandonado (Esc antes del primer pane) o
/// ambos panes en modo virtual: nada cambió, nada se toca.
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
        "sin pane re-listado, el fill sigue"
    );
    assert!(!lp.is_empty(), "sin pane re-listado, la dedup sigue");
}
