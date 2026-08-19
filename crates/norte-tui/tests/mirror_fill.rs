//! Un espejo al otro pane no puede estrangular el relleno del pane mirado:
//! son dos huecos distintos y cada uno tiene su propio canal de `Fill`.

use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, Pane};
use norte_tui::fill::{Fill, FillMsg, apply_fill_msg};
use norte_tui::jobs::SearchRun;
use norte_tui::navigate::{Cd, apply_cd};
use norte_tui::probes::{DecorateFetch, Probed};

fn vp(w: &str) -> VPath {
    VPath::parse(w).expect("wire de test")
}

fn file(dir: &VPath, name: &str) -> Entry {
    Entry {
        attrs: std::collections::BTreeMap::new(),
        path: dir.join(Segment::new(name.as_bytes().to_vec()).unwrap()),
        kind: EntryKind::File,
        size: Some(1),
        mtime_ms: None,
    }
}

/// Un relleno paginado por PANE, y no uno global: `pane.mirror` manda el
/// OTRO pane a un sitio SIN mover el foco, así que con un solo hueco basta
/// una tecla para que el pane que el lector está mirando —el suyo, el
/// enfocado, aún paginando un dir grande— se quede a medias.
///
/// Soltar su `rx` mata al drenador sin `finish_listing`, y `loading` solo
/// lo apaga `finish_listing`/`Failed`/un listado nuevo: el pane queda con
/// el listado truncado bajo un «cargando…» permanente.
#[test]
fn un_espejo_al_otro_pane_no_estrangula_el_relleno_del_pane_mirado() {
    let dir = vp("file:///d");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir.clone(), Vec::new()),
    );
    // El pane 0 —el enfocado, el que el lector mira— está paginando.
    app.panes[0].begin_listing(dir.clone(), vec![file(&dir, "a")], true, None);
    let (tx0, rx0) = tokio::sync::mpsc::channel::<FillMsg>(1);
    let mut fill: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
    fill.insert(norte_tui::panel::SLOT_LEFT, Fill { rx: rx0 });
    let mut df: norte_frontend::layout::BySlot<DecorateFetch> =
        norte_frontend::layout::BySlot::new();
    let mut lp = Probed::new();

    // `pane.mirror`: el pane 1 viaja, y su listado también viene paginado.
    let (_tx1, rx1) = tokio::sync::mpsc::channel::<FillMsg>(1);
    app.panes[1].begin_listing(dir.clone(), Vec::new(), true, None);
    let mut sr: Option<SearchRun> = None;
    apply_cd(
        &app.panes,
        &mut fill,
        &mut df,
        &mut lp,
        &mut sr,
        Cd::Filling {
            pane: 1,
            fill: Fill { rx: rx1 },
        },
    );

    // El drenador del pane 0 sigue teniendo a quién enviar: nadie le
    // soltó el `rx` por debajo.
    tx0.try_send(FillMsg::Batch(vec![file(&dir, "b")]))
        .expect("el drenador del pane 0 no fue abandonado");
    let msg = fill
        .get_mut(norte_tui::panel::SLOT_LEFT)
        .expect("el relleno del pane 0 sigue en su hueco")
        .rx
        .try_recv()
        .ok();
    apply_fill_msg(&mut app, &mut fill, norte_tui::panel::SLOT_LEFT, msg);

    assert_eq!(
        app.panes[0].entries().len(),
        2,
        "el lote posterior entra en el listado del pane 0"
    );
    assert!(
        app.panes[0].loading(),
        "y el «cargando…» sigue vivo: nadie terminó el listado por él"
    );
    assert!(
        fill.get(norte_tui::panel::SLOT_RIGHT).is_some(),
        "el espejo se quedó con SU hueco"
    );
}
