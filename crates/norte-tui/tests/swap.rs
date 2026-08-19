//! El intercambio de panes cruza TODO lo que está en vuelo apuntando a un
//! hueco: los `Fill`, las decoraciones, la búsqueda viva y los watch
//! targets. Lo que no hace es cosechar nada por el camino.

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
    VPath::parse(wire).expect("wire de test")
}

/// Una búsqueda viva CORRIENDO sobre `pane`. La Task es sintética (el
/// `TaskRef` de test del core): aquí no se ejerce el walker, sino el
/// índice de pane que el run loop guarda a su lado.
fn search_run(pane: usize) -> SearchRun {
    let (_tx, rx) = tokio::sync::mpsc::channel::<SearchHits>(1);
    let id = norte_proto::TaskId::new(1);
    let (_progreso, prx) = tokio::sync::watch::channel(norte_proto::TaskProgress {
        task_id: id,
        kind: norte_proto::TaskKind::Search,
        state: norte_proto::TaskState::Running,
        bytes_done: 0,
        bytes_total: None,
        entries_done: 0,
        entries_total: None,
        current: None,
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

/// El relleno EN VUELO está archivado POR PANE: si el intercambio no cruza
/// los huecos, los lotes del listado siguen llegando al pane de al lado y
/// el lector ve crecer la lista equivocada. Es el bug que una suite verde
/// no ve, porque el listado sigue llegando: solo llega al sitio que no es.
///
/// Comprueba que se movió ESE drenador y no un hueco cualquiera: el lote
/// enviado por el `tx` del pane 0 se recoge del hueco del pane 1.
#[test]
fn el_intercambio_cruza_los_huecos_del_relleno_en_vuelo() {
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
        "el hueco del pane 0 queda libre"
    );
    tx.try_send(FillMsg::Failed)
        .expect("el drenador sigue vivo");
    assert!(
        f.get_mut(norte_tui::panel::SLOT_RIGHT)
            .expect("cruzado al hueco del pane 1")
            .rx
            .try_recv()
            .is_ok(),
        "y es EL MISMO drenador el que ahora alimenta al pane 1"
    );
    assert!(
        df.get(norte_tui::panel::SLOT_RIGHT).is_some()
            && df.get(norte_tui::panel::SLOT_LEFT).is_none(),
        "cruzados"
    );
    assert!(lp.is_empty(), "la caché de stat se tira, no se traduce");
}

/// Sin nada en vuelo el reconciliado es inofensivo: un intercambio no
/// puede inventar un relleno ni un fetch donde no los había.
#[test]
fn el_intercambio_sin_nada_en_vuelo_no_inventa_nada() {
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

/// El desenlace `Cd::Swapped` tiene que LLEGAR al reconciliado: la mitad
/// del intercambio que `dispatch` no puede hacer viaja por `apply_cd`, y
/// un brazo que se olvidara de llamarlo dejaría el fill apuntando al pane
/// que no es sin que ningún test de `App` se enterase.
#[test]
fn apply_cd_swapped_reconcilia_el_estado_del_run_loop() {
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

/// La búsqueda VIVA también está indexada por pane: `SearchRun` guarda el
/// pane virtual que muestra los hits, exactamente como el relleno guarda
/// el suyo. Si el intercambio no lo voltea, los hits siguen entrando en el
/// pane de al lado.
#[test]
fn el_intercambio_voltea_el_pane_de_la_busqueda_viva() {
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
        sr.as_ref().expect("el run sigue vivo").pane,
        1,
        "el pane virtual de la búsqueda cambió de lado con su pane"
    );
}

/// Y el intercambio no puede COSECHAR la búsqueda por el camino.
///
/// El `Esc`/`Enter` del pane virtual son las ÚNICAS teclas que ese modo
/// intercepta, así que un `Ctrl+U` cae al resolutor y cruza los panes con
/// una búsqueda corriendo. Justo después, el mismo call site pasa por
/// [`reap_search_run`], que suelta el run cuando su pane ya no es virtual:
/// con el `pane` sin voltear mira el pane 0 —que ahora tiene el listado
/// ordinario que vino del otro lado— y CANCELA la Task en silencio,
/// dejando el pane 1 con hits a medias en `Running` para siempre y sin su
/// manejador de `Esc` (que exige un run vivo PARA ESE pane).
///
/// Por eso el volteo tiene que ocurrir DENTRO de `reconcile_swap`: pasada
/// la cosecha ya no hay nada que salvar.
#[test]
fn un_intercambio_no_cosecha_la_busqueda_viva() {
    let mut app = App::new(
        Pane::new(vp("file:///izq"), Vec::new()),
        Pane::new(vp("file:///der"), Vec::new()),
    );
    // Búsqueda viva en el pane 0 (el `Alt+F7` lo dejó virtual).
    app.panes[0].begin_search(vp("file:///izq"));
    let mut sr = Some(search_run(0));
    let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
    let mut df: norte_frontend::layout::BySlot<DecorateFetch> =
        norte_frontend::layout::BySlot::new();
    let mut lp = Probed::new();

    // `Ctrl+U`: `dispatch` cruza los panes y el run loop reconcilia…
    app.swap_panes();
    let panes = norte_tui::panel::PaneSlots::new(
        Pane::new(vp("mem:///d"), Vec::new()),
        Pane::new(vp("mem:///d"), Vec::new()),
    );
    apply_cd(&panes, &mut f, &mut df, &mut lp, &mut sr, Cd::Swapped);
    // …y el MISMO call site cosecha a continuación.
    reap_search_run(&app, &mut sr);

    let s = sr.as_ref().expect("la búsqueda en curso NO se cancela");
    assert_eq!(s.pane, 1, "sigue los hits a su nuevo lado");
    assert!(
        app.panes[s.pane].virtual_search,
        "y ese lado es el que está en modo búsqueda"
    );
}

/// El watcher NO necesita reconciliado propio, y esto es lo que hace
/// cierta esa afirmación: el conjunto vigilado se deriva de `app.panes`
/// en cada vuelta del run loop (`rewatch(&watch_targets(app))` es la
/// primera sentencia del bucle), así que basta con que `watch_targets`
/// no cachee nada. Si alguien introdujera una copia por lado, el
/// intercambio dejaría cada pane vigilando el dir del otro.
#[test]
fn watch_targets_sigue_a_los_panes_tras_el_intercambio() {
    let mut app = App::new(
        Pane::new(vp("file:///izq"), Vec::new()),
        Pane::new(vp("file:///der"), Vec::new()),
    );
    let before = watch_targets(&app);
    app.swap_panes();
    let after = watch_targets(&app);
    assert_eq!(
        before[0], after[1],
        "el dir izquierdo pasa a vigilarse a la derecha"
    );
    assert_eq!(before[1], after[0]);
    assert_ne!(before[0], before[1], "los dos dirs eran distintos de partida");
}
