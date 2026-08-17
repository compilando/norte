//! El recorrido del foco, que solo pasa por lo que está en pantalla.

use super::{Resolved, SlotId};

/// El siguiente hueco enfocable, ciclando.
///
/// Si `actual` ya no está en `focus_order` —se ocultó su pestaña, o su `Split`
/// colapsó— devuelve el PRIMERO: el foco no se pierde nunca mientras haya algo
/// que enfocar, porque un foco apuntando a algo que no se ve es un teclado que
/// no hace nada y un usuario que no sabe por qué.
#[must_use]
pub fn focus_next(resolved: &Resolved, actual: SlotId) -> Option<SlotId> {
    let orden = &resolved.focus_order;
    match orden.iter().position(|id| *id == actual) {
        Some(i) => orden.get((i + 1) % orden.len()).copied(),
        None => orden.first().copied(),
    }
}

/// Como [`focus_next`], hacia atrás.
#[must_use]
pub fn focus_prev(resolved: &Resolved, actual: SlotId) -> Option<SlotId> {
    let orden = &resolved.focus_order;
    match orden.iter().position(|id| *id == actual) {
        Some(i) => orden.get((i + orden.len() - 1) % orden.len()).copied(),
        None => orden.first().copied(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Rect;

    fn resuelto(orden: &[u32]) -> Resolved {
        Resolved {
            placements: orden
                .iter()
                .map(|i| (SlotId(*i), Rect::new(0, 0, 10, 10)))
                .collect(),
            hidden: vec![],
            focus_order: orden.iter().map(|i| SlotId(*i)).collect(),
            diagnostics: vec![],
        }
    }

    #[test]
    fn el_foco_cicla_en_los_dos_sentidos() {
        let r = resuelto(&[1, 2, 3]);
        assert_eq!(focus_next(&r, SlotId(3)), Some(SlotId(1)));
        assert_eq!(focus_prev(&r, SlotId(1)), Some(SlotId(3)));
        assert_eq!(focus_next(&r, SlotId(1)), Some(SlotId(2)));
        assert_eq!(focus_prev(&r, SlotId(3)), Some(SlotId(2)));
    }

    /// El foco estaba en un hueco que acaba de ocultarse (cambio de pestaña, o
    /// la ventana encogió y colapsó): NO se pierde, cae al primero visible.
    #[test]
    fn un_foco_que_ya_no_se_ve_cae_al_primer_visible() {
        let r = resuelto(&[2, 3]);
        assert_eq!(focus_next(&r, SlotId(1)), Some(SlotId(2)));
        assert_eq!(focus_prev(&r, SlotId(1)), Some(SlotId(2)));
    }

    #[test]
    fn sin_nada_enfocable_no_hay_foco() {
        let r = resuelto(&[]);
        assert_eq!(focus_next(&r, SlotId(1)), None);
        assert_eq!(focus_prev(&r, SlotId(1)), None);
    }

    /// Con UN solo hueco, ciclar se queda en él. Parece obvio y no lo es: un
    /// `(i + 1) % 1` mal escrito devuelve `None` y la tecla de cambiar de
    /// panel deja de responder cuando solo hay uno.
    #[test]
    fn con_un_solo_hueco_ciclar_se_queda_en_el() {
        let r = resuelto(&[7]);
        assert_eq!(focus_next(&r, SlotId(7)), Some(SlotId(7)));
        assert_eq!(focus_prev(&r, SlotId(7)), Some(SlotId(7)));
    }
}
