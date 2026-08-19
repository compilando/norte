//! La OTRA decisión que `walk_trail` depende de y nadie pinchaba: qué
//! navegaciones entran en el rastro. El guard `trail == Trail::Record` es la
//! única línea que impide que `nav.back` se alimente de su propio rastro.

use norte_proto::VPath;
use norte_tui::app::{Trail, TrailStep};
use norte_tui::nav;
use norte_tui::navigate::record_step;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire de test")
}

/// Una navegación del USUARIO deja huella en las dos estructuras: el
/// rastro que recorre `nav.back` y la MRU que pinta el popup.
#[test]
fn una_navegacion_del_usuario_entra_en_el_rastro_y_en_la_mru() {
    let mut h = nav::History::default();
    record_step(&mut h, &vp("mem:///a"), &vp("mem:///b"), Trail::Record);
    assert_eq!(h.back_len(), 1, "un paso en el rastro");
    assert!(h.entries().contains(&vp("mem:///a")), "y en la MRU");
}

/// EL guard. Un `Replay` es el rastro recorriéndose a sí mismo: si
/// registrara, volver de B a A grabaría «estuve en B», el siguiente atrás
/// devolvería a B, y el lector oscilaría entre dos directorios para
/// siempre. Borra `&& trail == Trail::Record` de `record_step` y este
/// test se pone rojo — es su único guardián.
#[test]
fn un_replay_no_alimenta_el_rastro() {
    let mut h = nav::History::default();
    record_step(
        &mut h,
        &vp("mem:///b"),
        &vp("mem:///a"),
        Trail::Replay(TrailStep::Back),
    );
    assert_eq!(h.back_len(), 0, "un paso atrás jamás produce rastro");
    assert!(
        h.entries().is_empty(),
        "ni entra en la MRU: volver no es visitar un sitio nuevo"
    );
}

/// Un cd al MISMO dir (refresh-like) no es un paso que el lector diera:
/// registrarlo haría que el siguiente `nav.back` no hiciera nada visible.
#[test]
fn un_cd_al_mismo_dir_no_es_un_paso() {
    let mut h = nav::History::default();
    record_step(&mut h, &vp("mem:///a"), &vp("mem:///a"), Trail::Record);
    assert_eq!(h.back_len(), 0);
    assert!(h.entries().is_empty());
}
