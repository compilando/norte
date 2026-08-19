//! El ritual del refresco: un `Esc` a medias conserva el `Fill` del pane que
//! no se refrescó, y un refresco bajo la ayuda abierta recongela sus hechos.

use norte_proto::VPath;
use norte_tui::app::{App, Pane};
use norte_tui::fill::{Fill, FillMsg};
use norte_tui::jobs::SearchRun;
use norte_tui::probes::Probed;
use norte_tui::refresh::after_panes_refresh;

fn fill() -> Fill {
    let (_tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
    Fill { rx }
}

fn app() -> App {
    let d = VPath::parse("file:///d").expect("wire de test");
    App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
}

/// #118 (regresión pedida en el issue): un Esc a medias del refresh
/// re-listó el pane 0 pero ABANDONÓ el 1 — el ritual solo puede soltar
/// el drenador del pane re-listado de verdad; el del otro sigue drenando
/// un listado que sigue siendo el suyo (#78).
#[test]
fn esc_a_medias_conserva_el_fill_del_pane_no_refrescado() {
    let mut app = app();
    let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
    f.insert(norte_tui::panel::SLOT_RIGHT, fill());
    let mut lp = Probed::from([(1, VPath::parse("file:///d/x").unwrap())]);
    let mut sr: Option<SearchRun> = None;
    after_panes_refresh(&mut app, [true, false], &mut f, &mut lp, &mut sr);
    assert!(
        f.get(norte_tui::panel::SLOT_RIGHT).is_some(),
        "el fill del pane 1 (no re-listado) sobrevive al Esc a medias"
    );
    assert!(lp.is_empty(), "la dedup de la sonda #52 caduca igualmente");
}

/// MAJOR-2: congelar impide que un veredicto cambie porque el lector se
/// MUEVA, y eso está bien. Lo que no puede impedir es que cambie porque el
/// MUNDO cambie: el brazo del `tick` no lleva guarda de overlay (a
/// diferencia del de `dir_watch`, gateado por `watch_refresh_allowed`), así
/// que una copia o un borrado que terminan con la ayuda abierta re-listan
/// los dos panes y la entrada que los hechos describían puede haberse ido.
/// La fila decía «no aplica a esta selección» de una selección que ya no
/// existía.
#[test]
fn un_refresh_bajo_la_ayuda_abierta_recongela_los_hechos() {
    use norte_help::ChordResolver as _;

    let d = VPath::parse("file:///d").expect("wire de test");
    let fichero = norte_proto::Entry {
        attrs: std::collections::BTreeMap::new(),
        path: d.join(norte_proto::Segment::new(b"leeme.txt".to_vec()).expect("segmento")),
        kind: norte_proto::EntryKind::File,
        size: Some(3),
        mtime_ms: None,
    };
    let mut app = App::new(
        Pane::new(d.clone(), vec![fichero]),
        Pane::new(d, Vec::new()),
    );
    norte_tui::overlays::open_contextual_help(&mut app, norte_help::Lang::En, &[], None);
    assert!(
        app.help_chords.availability("pane.view").is_available(),
        "con un fichero bajo el cursor, F3 se puede pulsar"
    );

    // La tarea termina, el refresh entra por debajo del overlay y se lleva
    // por delante la entrada de la que hablaban los hechos.
    app.panes[0].refresh_listing(Vec::new());
    let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
    let mut lp = Probed::new();
    let mut sr: Option<SearchRun> = None;
    after_panes_refresh(&mut app, [true, false], &mut f, &mut lp, &mut sr);

    assert_eq!(
        app.help_chords.availability("pane.view").reason(),
        Some(norte_help::Reason::WrongTarget),
        "el listado cambió: los hechos tienen que volver a congelarse"
    );
}
