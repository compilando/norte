//! Navegación TC (spec 2026-07-18): el quick search PURO (`Mode`, `matches`,
//! `QuickSearch`) vive ahora en [`norte_frontend::nav`] — compartido con la
//! GUI — y se re-exporta aquí para no tocar los call-sites de la TUI. El
//! historial de directorios por pane ([`History`]) es específico de la TUI y
//! se queda.

use std::collections::VecDeque;

use norte_proto::{EntryKind, VPath};

pub use norte_frontend::nav::{Mode, QuickSearch, fold, matches};

/// Tope de directorios retenidos en el historial de un pane (spec
/// 2026-07-18: sesión, no persistido — a diferencia de la hotlist).
const HISTORY_MAX: usize = 30;

/// Historial de directorios visitados por UN pane. Cada `cd` EXITOSO
/// empuja el dir ANTERIOR (main.rs, brazos `Cd::Filling`/`Cd::Replaced`);
/// `Alt+↓` lo recorre en un popup (T5). Vive en memoria del proceso, no en
/// `norte.toml` — a propósito, fuera de alcance de la spec (§Fuera de
/// alcance).
///
/// INVARIANTE del rastro: `back.len() + fwd.len() <= HISTORY_MAX`.
///
/// Es lo que acota la memoria del rastro, y no cada pila por su cuenta.
/// [`History::record`] es el único método que hace CRECER la suma, y la
/// acota: trunca `back` al tope y vacía `fwd`. Los dos pasos la conservan
/// exactamente — mueven un elemento de una pila a la otra — y
/// [`History::remove`] solo la reduce. Por eso [`History::step_forward`]
/// puede empujar a `back` SIN comprobar el tope: el hueco que deja el `pop`
/// de `fwd` es el que ocupa. Romper el invariante (p.ej. hacer que `record`
/// deje de vaciar `fwd`) haría crecer el rastro sin fin por el único camino
/// que no lo comprueba.
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

    /// El rastro de vuelta, del más viejo al más reciente: lo que la sesión
    /// guarda para que `nav.back` siga funcionando tras un reinicio.
    #[must_use]
    pub fn trail(&self) -> &[VPath] {
        &self.back
    }

    /// La rama de la que se salió con un `nav.back`, del más viejo al más
    /// reciente.
    #[must_use]
    pub fn forward_trail(&self) -> &[VPath] {
        &self.fwd
    }

    /// Siembra los dos rastros desde una sesión guardada.
    ///
    /// El MRU se reconstruye DEL rastro y no se guarda aparte: es lo que el
    /// popup lista, se deriva de por dónde se ha pasado, y guardarlo por
    /// separado sería una segunda copia de la misma historia que puede
    /// contradecir a la primera. Se empuja del más viejo al más reciente para
    /// que el orden del popup salga igual que si se hubiera andado.
    pub fn seed(&mut self, back: Vec<VPath>, fwd: Vec<VPath>) {
        for p in &back {
            self.push(p.clone());
        }
        self.back = back;
        self.fwd = fwd;
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
    ///
    /// Pushes onto `back` with no bound check because it cannot need one: it
    /// pops `fwd` first, and the type's invariant (`back.len() + fwd.len() <=
    /// HISTORY_MAX`, stated on [`History`]) makes that pop the room for this
    /// push.
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

// ── Azúcar de navegación por archivos comprimidos (ADR 0018) ──────────────
//
// `archive_root_for` vivía en `main.rs`, privada del binario. La necesita
// también `App::help_facts` (H3d): el hecho «esta entrada se ENTRA» es el
// predicado del brazo `nav.enter` del dispatch, y la ayuda tiene que
// contestarlo con la MISMA función o acabará atenuando `nav.enter` sobre un
// `.zip` que la app abre sin problemas.

/// Si la entrada es un contenedor navegable (`.<formato>` de la whitelist
/// de proto, extensión ASCII case-insensitive), la raíz de su interior
/// (ADR 0018). El mapa extensión→formato es azúcar de presentación; la
/// validación real es del core. Un SYMLINK a un archivo no entra como
/// contenedor en v1 (decisión consciente: exigiría resolver el target por
/// stat del core; issue de fase 8g).
#[must_use]
pub fn archive_root_for(e: &norte_proto::Entry) -> Option<VPath> {
    // Extensiones cuyo sufijo no coincide con el token del formato (#55):
    // `tar+gz` no tiene un `.tar+gz` real en el mundo, la gente escribe
    // `.tgz`/`.tar.gz`. Se comprueban ANTES del genérico `.{formato}` — un
    // `.tar.gz` no casaría de todos modos con `.tar` (termina en `.gz`), así
    // que el orden es defensivo, no estrictamente necesario hoy.
    const EXT_ALIASES: &[(&[u8], &str)] = &[(b".tar.gz", "tar+gz"), (b".tgz", "tar+gz")];
    fn ends_ci(name: &[u8], suffix: &[u8]) -> bool {
        name.len() >= suffix.len() && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
    }
    if e.kind != EntryKind::File {
        return None;
    }
    let name = e.path.file_name()?.as_bytes();
    let format = EXT_ALIASES
        .iter()
        .find(|(suffix, _)| ends_ci(name, suffix))
        .map(|(_, format)| *format)
        .or_else(|| {
            norte_proto::ARCHIVE_FORMATS
                .iter()
                .find(|f| ends_ci(name, format!(".{f}").as_bytes()))
                .copied()
        })?;
    // Falla (exterior con `!`, ya compuesto…): no es navegable — Enter no-op.
    VPath::archive_compose(format, &e.path, &[]).ok()
}

#[cfg(test)]
mod archive_nav_tests {
    use super::*;
    use norte_proto::{Entry, EntryKind};

    fn entry(wire: &str, kind: EntryKind) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse(wire).expect("wire de test"),
            kind,
            size: None,
            mtime_ms: None,
        }
    }

    #[test]
    fn archive_root_for_decide_por_extension_y_kind() {
        let e = entry("file:///d/A.ZIP", EntryKind::File);
        assert_eq!(
            archive_root_for(&e).expect("mayúsculas entran").to_wire(),
            "zip+file:///d/A.ZIP/!"
        );
        assert!(archive_root_for(&entry("file:///d/a.tar", EntryKind::File)).is_some());
        assert!(archive_root_for(&entry("file:///d/a.txt", EntryKind::File)).is_none());
        // Un dir llamado x.zip NO es contenedor; un symlink tampoco (v1).
        assert!(archive_root_for(&entry("file:///d/x.zip", EntryKind::Dir)).is_none());
        assert!(archive_root_for(&entry("file:///d/x.zip", EntryKind::Symlink)).is_none());
        // #56 (antes v1 = no-op): Enter sobre un zip DENTRO de un tar
        // compone una capa más — anidamiento navegable.
        assert_eq!(
            archive_root_for(&entry("tar+file:///a.tar/!/i.zip", EntryKind::File))
                .expect("anidado navegable")
                .to_wire(),
            "zip+tar+file:///a.tar/!/i.zip/!"
        );
    }

    /// #55: `.tgz`/`.tar.gz` no coinciden con el token `tar+gz` vía el
    /// genérico `.{formato}` (el `+` no está en la extensión de archivo) —
    /// `EXT_ALIASES` los mapea explícitamente, case-insensitive, antes del
    /// genérico. `.tar`/`.zip` planos siguen funcionando sin pasar por el
    /// alias (`.tar.gz` NO debe casar `.tar`: termina en `.gz`).
    #[test]
    fn archive_root_for_extensiones_targz() {
        for wire in [
            "file:///d/a.tgz",
            "file:///d/a.tar.gz",
            "file:///d/A.TAR.GZ",
        ] {
            let root = archive_root_for(&entry(wire, EntryKind::File))
                .unwrap_or_else(|| panic!("{wire} debería ser navegable"));
            assert_eq!(root.scheme(), "tar+gz+file", "wire={wire}");
        }
        // Extensiones planas siguen funcionando (no capturadas por el alias).
        assert_eq!(
            archive_root_for(&entry("file:///d/a.tar", EntryKind::File))
                .expect("tar plano sigue")
                .scheme(),
            "tar+file"
        );
        assert_eq!(
            archive_root_for(&entry("file:///d/a.zip", EntryKind::File))
                .expect("zip plano sigue")
                .scheme(),
            "zip+file"
        );
    }

    /// Candado de encoding (#55, review): `ends_ci` es de BYTES y el compose
    /// no pasa por String — un nombre NO-UTF8 terminado en `.tgz` compone
    /// bien y sus bytes crudos sobreviven el wire (regla 1). Si alguien
    /// "simplifica" mañana con `to_str()`/lossy, esto se pone rojo.
    #[test]
    fn archive_root_for_targz_nombre_no_utf8() {
        for wire in [
            "file:///d/%FF%FE.tgz",
            "file:///d/a%F1o.TGZ",
            "file:///d/%FF.tar.gz",
        ] {
            let root = archive_root_for(&entry(wire, EntryKind::File))
                .unwrap_or_else(|| panic!("{wire} debería ser navegable"));
            assert_eq!(root.scheme(), "tar+gz+file", "wire={wire}");
        }
        assert_eq!(
            archive_root_for(&entry("file:///d/%FF%FE.tgz", EntryKind::File))
                .expect("no-UTF8 navegable")
                .to_wire(),
            "tar+gz+file:///d/%FF%FE.tgz/!",
            "los bytes crudos sobreviven el compose"
        );
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
        let count_to = |h: &History| h.entries().iter().filter(|p| **p == vp("mem:///a")).count();
        assert_eq!(
            count_to(&h),
            2,
            "repetido no consecutivo: dos apariciones de a"
        );
        h.remove(&vp("mem:///a"));
        assert_eq!(count_to(&h), 0, "remove retira TODAS las ocurrencias");
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
        let before: Vec<VPath> = h.entries().iter().cloned().collect();
        let _ = h.step_back(vp("mem:///c"));
        let _ = h.step_forward(vp("mem:///b"));
        let after: Vec<VPath> = h.entries().iter().cloned().collect();
        assert_eq!(before, after, "la MRU es asunto aparte");
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

    /// El tope de arriba solo mueve `record`. El invariante que documenta el
    /// tipo —y del que depende `step_forward` para empujar a `back` sin
    /// comprobar nada— es sobre la SUMA de las dos pilas, así que hay que
    /// alternar las tres operaciones más allá del tope: ir hasta el fondo del
    /// rastro, volver hasta el final, y navegar de nuevo desde ahí.
    #[test]
    fn el_tope_aguanta_alternando_las_tres_operaciones() {
        let mut h = History::default();
        let total = |h: &History| h.back_len() + h.fwd_len();

        let mut cur = vp("mem:///start");
        for i in 0..(HISTORY_MAX * 2) {
            h.record(cur.clone());
            cur = vp(&format!("mem:///d{i}"));
            assert!(total(&h) <= HISTORY_MAX, "record no desborda la suma");
        }
        assert_eq!(h.back_len(), HISTORY_MAX, "el rastro está lleno");

        // Hasta el fondo: cada paso mueve un dir de una pila a la otra.
        let mut steps = 0;
        while let Some(target) = h.step_back(cur.clone()) {
            cur = target;
            steps += 1;
            assert!(total(&h) <= HISTORY_MAX, "atrás no desborda la suma");
        }
        assert_eq!(steps, HISTORY_MAX, "se recorrió el rastro entero");
        assert_eq!(h.fwd_len(), HISTORY_MAX, "toda la memoria está delante");

        // Y de vuelta: aquí es donde `step_forward` empuja a `back` sin
        // comprobar el tope. Sin el invariante, `back` acabaría por encima.
        while let Some(target) = h.step_forward(cur.clone()) {
            cur = target;
            assert!(total(&h) <= HISTORY_MAX, "adelante no desborda la suma");
        }
        assert_eq!(h.back_len(), HISTORY_MAX, "el rastro vuelve a estar lleno");

        // Una navegación nueva desde el tope tampoco lo desborda.
        h.record(cur);
        assert!(total(&h) <= HISTORY_MAX);
        assert_eq!(h.fwd_len(), 0, "y poda la rama de delante");
    }
}
