//! Navegación TC (spec 2026-07-18): el quick search PURO (`Mode`, `matches`,
//! `QuickSearch`) vive ahora en [`norte_frontend::nav`] — compartido con la
//! GUI — y se re-exporta aquí para no tocar los call-sites de la TUI. El
//! historial de directorios por pane ([`History`]) es específico de la TUI y
//! se queda.

use std::collections::VecDeque;

use norte_proto::VPath;

pub use norte_frontend::nav::{Mode, QuickSearch, fold, matches};

/// Tope de directorios retenidos en el historial de un pane (spec
/// 2026-07-18: sesión, no persistido — a diferencia de la hotlist).
const HISTORY_MAX: usize = 30;

/// Historial de directorios visitados por UN pane. Cada `cd` EXITOSO
/// empuja el dir ANTERIOR (main.rs, brazos `Cd::Filling`/`Cd::Replaced`);
/// `Alt+↓` lo recorre en un popup (T5). Vive en memoria del proceso, no en
/// `norte.toml` — a propósito, fuera de alcance de la spec (§Fuera de
/// alcance).
#[derive(Debug, Default)]
pub struct History {
    /// Más reciente al frente.
    deque: VecDeque<VPath>,
    /// The trail behind the reader: where `nav.back` goes, newest last.
    ///
    /// Separate from `deque` because they answer different questions. The
    /// deque is "where has this pane been", deduplicated and most-recent
    /// first, which is what the popup lists. The trail is "where was I just
    /// now", in order, with repeats — walking the deque as if it were a trail
    /// oscillates between the two most recent directories forever.
    back: Vec<VPath>,
    /// Where `nav.forward` goes: the branch a `nav.back` stepped off, newest
    /// last. Cleared by any navigation the user initiates.
    fwd: Vec<VPath>,
}

impl History {
    /// Empuja `path` al frente. Dedup CONSECUTIVO: si `path` ya es el más
    /// reciente, no-op — evita repetir el mismo dir en cd's redundantes
    /// (p.ej. refrescar el pane). Un mismo dir en posiciones NO
    /// consecutivas del historial sí puede repetirse (visitarlo, irse,
    /// volver): es historial de sesión, no un conjunto. El dedup compara
    /// `VPath` byte-exacto SIN normalizar (la identidad jamás se
    /// normaliza); twins NFC/NFD conviven como filas distintas — decisión
    /// consciente.
    pub fn push(&mut self, path: VPath) {
        if self.deque.front() == Some(&path) {
            return;
        }
        self.deque.push_front(path);
        self.deque.truncate(HISTORY_MAX);
    }

    /// Retira TODAS las ocurrencias de `path` (p.ej. tras un `cd` fallido
    /// con `NotFound` al navegar desde el popup — la spec dice "se
    /// RETIRA si el cd falla con `NotFound`").
    ///
    /// Prunes the TRAIL as well as the MRU. "This directory is gone" is one
    /// fact, not two: left on the trail, a path the popup just retired would
    /// still be where `nav.back` aims — a key that can only fail, and one the
    /// reader has no other way to steer around. Pruning both is also what
    /// keeps the two structures from ever disagreeing about which places
    /// still exist.
    pub fn remove(&mut self, path: &VPath) {
        self.deque.retain(|p| p != path);
        self.back.retain(|p| p != path);
        self.fwd.retain(|p| p != path);
    }

    /// Entradas, más reciente primero.
    #[must_use]
    pub fn entries(&self) -> &VecDeque<VPath> {
        &self.deque
    }

    /// Records a navigation the USER initiated, leaving `prev` behind.
    ///
    /// Feeds BOTH structures: [`History::push`] for the MRU the popup paints,
    /// and the back stack for the trail `nav.back` walks. They are fed from
    /// the same event but kept apart on purpose — see the `History::back`
    /// field docs for why one cannot serve as the other.
    ///
    /// Skips the trail push when `prev` is already its top, mirroring the
    /// MRU's consecutive dedup: a redundant `cd` onto the directory we are
    /// already tracking (a pane refresh, say) is not a step the reader took,
    /// and recording it would make `nav.back` do nothing visible once.
    ///
    /// Clears `fwd`: the reader chose a different path, so the branch they
    /// stepped off no longer exists. Offering a "forward" into a history the
    /// reader already abandoned is the browser bug everyone knows.
    pub fn record(&mut self, prev: VPath) {
        self.push(prev.clone());
        if self.back.last() != Some(&prev) {
            self.back.push(prev);
            if self.back.len() > HISTORY_MAX {
                // Newest last, so the cap drops from the front: the oldest
                // step of the trail is the one the reader is least likely to
                // still want.
                self.back.remove(0);
            }
        }
        self.fwd.clear();
    }

    /// Steps one directory BACK along the trail, leaving `current` behind.
    ///
    /// Pops the back stack, pushes `current` onto the forward stack so
    /// [`History::step_forward`] can undo this, and returns the target.
    /// `None` when the trail is exhausted — the caller should then leave the
    /// pane where it is rather than invent a destination.
    ///
    /// Deliberately does NOT feed the MRU: going back is not visiting
    /// somewhere new, and a popup that grew an entry per back-press would
    /// stop being a list of the places the reader went.
    pub fn step_back(&mut self, current: VPath) -> Option<VPath> {
        let target = self.back.pop()?;
        self.fwd.push(current);
        Some(target)
    }

    /// Steps one directory FORWARD along the branch a [`History::step_back`]
    /// stepped off — the mirror image of it, down to leaving the MRU alone.
    ///
    /// `None` when there is no such branch, either because the reader never
    /// went back or because a [`History::record`] pruned it.
    pub fn step_forward(&mut self, current: VPath) -> Option<VPath> {
        let target = self.fwd.pop()?;
        self.back.push(current);
        Some(target)
    }

    /// Length of the back trail. Zero means `nav.back` is a no-op, which is
    /// what a caller checks before painting the key as available.
    #[must_use]
    pub fn back_len(&self) -> usize {
        self.back.len()
    }

    /// Length of the forward branch. Zero means `nav.forward` is a no-op.
    #[must_use]
    pub fn fwd_len(&self) -> usize {
        self.fwd.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire")
    }

    #[test]
    fn historial_push_dedup_tope_y_retirada() {
        let mut h = History::default();
        for i in 0..40 {
            h.push(vp(&format!("mem:///d{i}")));
        }
        assert_eq!(h.entries().len(), 30, "tope");
        assert_eq!(h.entries()[0], vp("mem:///d39"), "más reciente primero");
        h.push(vp("mem:///d39"));
        assert_eq!(h.entries().len(), 30, "dedup consecutivo");
        h.remove(&vp("mem:///d39"));
        assert!(
            !h.entries().contains(&vp("mem:///d39")),
            "retirada tras NotFound"
        );
    }

    /// review MINOR-3: el rustdoc de `push` promete que un mismo dir en
    /// posiciones NO consecutivas SÍ puede repetirse, y `remove` retira
    /// TODAS las ocurrencias — pínchalo con un caso A→B→A explícito.
    #[test]
    fn historial_permite_repetidos_no_consecutivos_y_remove_retira_todas() {
        let mut h = History::default();
        h.push(vp("mem:///a"));
        h.push(vp("mem:///b"));
        h.push(vp("mem:///a")); // NO consecutivo con el primer "a" (hay "b" en medio)
        let contar_a = |h: &History| h.entries().iter().filter(|p| **p == vp("mem:///a")).count();
        assert_eq!(
            contar_a(&h),
            2,
            "repetido no consecutivo: dos apariciones de a"
        );
        h.remove(&vp("mem:///a"));
        assert_eq!(contar_a(&h), 0, "remove retira TODAS las ocurrencias");
    }

    #[test]
    fn el_rastro_no_oscila_entre_dos_directorios() {
        // El defecto que este rastro existe para no tener: recorrer la MRU
        // como si fuera un rastro lleva de A a B, de vuelta a A, y de vuelta
        // a B — el lector se queda atrapado entre dos dirs sin salida.
        let mut h = History::default();
        h.record(vp("mem:///a")); // salimos de A hacia B
        h.record(vp("mem:///b")); // salimos de B hacia C (estamos en C)
        assert_eq!(h.step_back(vp("mem:///c")), Some(vp("mem:///b")));
        assert_eq!(h.step_back(vp("mem:///b")), Some(vp("mem:///a")));
        assert_eq!(h.step_back(vp("mem:///a")), None, "el rastro se acaba");
    }

    #[test]
    fn adelante_deshace_atras_y_una_navegacion_nueva_lo_borra() {
        let mut h = History::default();
        h.record(vp("mem:///a"));
        h.record(vp("mem:///b"));
        assert_eq!(h.step_back(vp("mem:///c")), Some(vp("mem:///b")));
        assert_eq!(h.step_forward(vp("mem:///b")), Some(vp("mem:///c")));
        assert_eq!(h.step_forward(vp("mem:///c")), None);

        // Volver atrás y NAVEGAR a otro sitio corta la rama de delante: es
        // la semántica del navegador, y lo contrario ofrecería un «adelante»
        // hacia una historia que el lector ya abandonó.
        assert_eq!(h.step_back(vp("mem:///c")), Some(vp("mem:///b")));
        h.record(vp("mem:///b"));
        assert_eq!(h.step_forward(vp("mem:///z")), None, "rama podada");
    }

    #[test]
    fn el_rastro_no_toca_la_mru_del_popup() {
        // Son dos preguntas distintas: «¿dónde he estado?» (la MRU que pinta
        // el popup) y «¿dónde estaba hace un momento?» (el rastro). Ir atrás
        // no es visitar un sitio nuevo.
        let mut h = History::default();
        h.record(vp("mem:///a"));
        h.record(vp("mem:///b"));
        let antes: Vec<VPath> = h.entries().iter().cloned().collect();
        let _ = h.step_back(vp("mem:///c"));
        let _ = h.step_forward(vp("mem:///b"));
        let despues: Vec<VPath> = h.entries().iter().cloned().collect();
        assert_eq!(antes, despues, "la MRU es asunto aparte");
    }

    /// «Este directorio ya no está» es UN hecho: `remove` lo aplica a la MRU
    /// y al rastro a la vez. Sin esto el popup retiraba la entrada y
    /// `nav.back` seguía apuntando al mismo dir muerto.
    #[test]
    fn remove_poda_el_rastro_y_no_solo_la_mru() {
        let mut h = History::default();
        h.record(vp("mem:///a"));
        h.record(vp("mem:///b"));
        // Y también la rama de delante: el mismo dir puede estar en las dos.
        assert_eq!(h.step_back(vp("mem:///c")), Some(vp("mem:///b")));
        assert_eq!(h.fwd_len(), 1);

        h.remove(&vp("mem:///b"));
        assert_eq!(h.back_len(), 1, "b sale del rastro de atrás");
        assert!(!h.entries().contains(&vp("mem:///b")), "y de la MRU");
        assert_eq!(
            h.step_back(vp("mem:///c")),
            Some(vp("mem:///a")),
            "atrás salta al siguiente vivo, no al dir retirado"
        );

        h.remove(&vp("mem:///c"));
        assert_eq!(h.fwd_len(), 0, "y de la rama de delante");
    }

    #[test]
    fn el_rastro_esta_acotado_como_la_mru() {
        let mut h = History::default();
        for i in 0..(HISTORY_MAX + 20) {
            h.record(vp(&format!("mem:///d{i}")));
        }
        assert_eq!(h.back_len(), HISTORY_MAX, "el rastro no crece sin fin");
    }
}
