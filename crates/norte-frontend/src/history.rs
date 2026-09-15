//! La historia de navegación vista como LISTAS: las filas que pintan los dos
//! frontends, los directorios populares y la única decisión de «esto cuenta
//! como visita» (spec 2026-09-15, fase 1).
//!
//! [`crate::nav::History`] es la estructura de UN panel —MRU, rastro, punto de
//! salto—. Esto es lo que se construye encima para enseñarla y lo que no es de
//! ningún panel: [`Popular`] es de la sesión entera, como en Krusader.
//!
//! Vive aquí y no en cada frontend por lo mismo que `History` (ADR 0066 D14):
//! qué filas salen, en qué orden y con qué marca no puede depender de quién
//! las pinte.

use crate::nav::{History, Trail};
use norte_proto::VPath;
use serde::{Deserialize, Serialize};

/// Cuántos directorios populares recuerda la sesión.
pub const POPULAR_CAP: usize = 50;

/// Clave Fluent de «no hay punto de salto».
pub const NO_JUMP_POINT: &str = "msg-nav-no-jump-point";

/// Un directorio popular: cuántas veces se llegó a él y cuándo fue la última.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PopularEntry {
    /// El directorio.
    pub path: VPath,
    /// Cuántas navegaciones del lector acabaron aquí.
    pub visits: u32,
    /// Orden de la última visita: un contador de la propia lista, no un
    /// reloj — así el desempate es determinista y no depende de la hora de la
    /// máquina que escribió la sesión.
    #[serde(default)]
    pub last: u64,
}

/// Los directorios a los que más se va (Krusader «Popular URLs», `Ctrl+Z`).
///
/// UNA lista para toda la sesión y no una por panel: la pregunta es «a dónde
/// suelo ir», y la respuesta no cambia según el lado de la pantalla desde el
/// que se pregunte.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Popular {
    entries: Vec<PopularEntry>,
    clock: u64,
}

impl Popular {
    /// Reconstruye la lista desde una sesión. Una ruta repetida (un fichero
    /// editado a mano) se queda con su PRIMERA aparición, y lo que pase de
    /// [`POPULAR_CAP`] se expulsa por la misma regla que al visitar.
    #[must_use]
    pub fn from_entries(entries: Vec<PopularEntry>) -> Self {
        let mut p = Self::default();
        for e in entries {
            if p.entries.iter().any(|x| x.path == e.path) {
                continue;
            }
            p.clock = p.clock.max(e.last);
            p.entries.push(e);
        }
        while p.entries.len() > POPULAR_CAP {
            p.expulsa();
        }
        p
    }

    /// Las entradas en el orden en que se guardan (no el de pintar: ver
    /// [`Self::ranked`]).
    #[must_use]
    pub fn entries(&self) -> &[PopularEntry] {
        &self.entries
    }

    /// Anota una visita a `path`. Con la lista llena, una ruta nueva expulsa a
    /// la de menos visitas y, en empate, a la visitada hace más tiempo.
    pub fn visit(&mut self, path: &VPath) {
        self.clock += 1;
        if let Some(e) = self.entries.iter_mut().find(|e| e.path == *path) {
            e.visits = e.visits.saturating_add(1);
            e.last = self.clock;
            return;
        }
        if self.entries.len() >= POPULAR_CAP {
            self.expulsa();
        }
        self.entries.push(PopularEntry {
            path: path.clone(),
            visits: 1,
            last: self.clock,
        });
    }

    /// Quita `path` (`dialog.remove`, o un directorio que ya no existe).
    pub fn remove(&mut self, path: &VPath) {
        self.entries.retain(|e| e.path != *path);
    }

    /// Vacía la lista (`dialog.clear`).
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Las entradas de más a menos visitadas; en empate, la más reciente
    /// primero.
    #[must_use]
    pub fn ranked(&self) -> Vec<&PopularEntry> {
        let mut v: Vec<&PopularEntry> = self.entries.iter().collect();
        v.sort_by(|a, b| b.visits.cmp(&a.visits).then(b.last.cmp(&a.last)));
        v
    }

    fn expulsa(&mut self) {
        let victima = self
            .entries
            .iter()
            .enumerate()
            .min_by_key(|(_, e)| (e.visits, e.last))
            .map(|(i, _)| i);
        if let Some(i) = victima {
            self.entries.swap_remove(i);
        }
    }
}

/// Registra una navegación en el rastro del panel y en los populares, si es
/// que cuenta. Devuelve si contó.
///
/// La ÚNICA decisión de «esto es un paso del lector», compartida por los dos
/// frontends. Dos condiciones, las mismas que ya guardaba el rastro:
///
/// - `prev != dir`: navegar al directorio que ya se enseña es un refresco, no
///   un paso.
/// - `trail == Trail::Record`: un `Replay` es el rastro andándose a sí mismo
///   (contarlo lo haría oscilar), y un `Seed` coloca el panel sin que el
///   lector fuera a ninguna parte — tampoco es una visita.
///
/// El rastro guarda de dónde se SALE (`prev`); los populares, a dónde se
/// LLEGA (`dir`).
///
/// ```
/// use norte_frontend::history::{Popular, record_visit};
/// use norte_frontend::nav::{History, Trail, TrailStep};
/// use norte_proto::VPath;
/// let vp = |s: &str| VPath::parse(s).unwrap();
/// let (mut h, mut p) = (History::default(), Popular::default());
/// assert!(record_visit(&mut h, &mut p, &vp("mem:///a"), &vp("mem:///b"), Trail::Record));
/// assert!(!record_visit(&mut h, &mut p, &vp("mem:///b"), &vp("mem:///a"), Trail::Replay(TrailStep::Back)));
/// assert_eq!(h.back_len(), 1);
/// assert_eq!(p.entries().len(), 1);
/// ```
pub fn record_visit(
    history: &mut History,
    popular: &mut Popular,
    prev: &VPath,
    dir: &VPath,
    trail: Trail,
) -> bool {
    if prev == dir || trail != Trail::Record {
        return false;
    }
    history.record(prev.clone());
    popular.visit(dir);
    true
}

/// A dónde lleva `nav.jump-back`, o la clave Fluent de por qué no lleva a
/// ningún sitio.
///
/// # Errors
///
/// [`NO_JUMP_POINT`] si el panel no tiene punto de salto.
pub fn jump_target(history: &History) -> Result<VPath, &'static str> {
    history.jump().cloned().ok_or(NO_JUMP_POINT)
}

/// Qué es una fila de una lista de historia respecto al lector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryMark {
    /// El directorio en el que está el panel ahora (el check de Krusader).
    Current,
    /// Un sitio por el que se pasó.
    Visited,
    /// Un sitio de la rama de delante: el lector fue atrás y puede volver.
    Forward,
}

/// Una fila de la lista de historia o de populares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRow {
    /// A dónde navega.
    pub path: VPath,
    /// Qué es respecto al lector.
    pub mark: HistoryMark,
}

/// La clave Fluent de la marca de una fila, o `None` si la fila no lleva
/// ninguna. Una sola tabla para los dos frontends: la TUI la pinta detrás de la
/// ruta y la ventana en el detalle de la fila, pero la PALABRA es la misma.
#[must_use]
pub fn mark_key(mark: HistoryMark) -> Option<&'static str> {
    match mark {
        HistoryMark::Current => Some("history-mark-current"),
        HistoryMark::Forward => Some("history-mark-forward"),
        HistoryMark::Visited => None,
    }
}

/// Dónde empieza el cursor de una lista de historia: en la SEGUNDA fila si la
/// primera es el directorio actual, porque a donde ya estás no se quiere ir.
#[must_use]
pub fn start_cursor(rows: &[HistoryRow]) -> usize {
    usize::from(rows.len() > 1 && rows.first().is_some_and(|r| r.mark == HistoryMark::Current))
}

/// Las filas de la lista de historia de un panel (`pane.history`).
///
/// Primero el directorio ACTUAL, marcado; luego el MRU, del más reciente al
/// más viejo, sin volver a listar el actual. Las que están en la rama de
/// delante llevan [`HistoryMark::Forward`]. `filter` casa por subsecuencia
/// sobre la ruta pintable plegada, igual que la paleta; vacío casa todo.
#[must_use]
pub fn history_rows(history: &History, current: &VPath, filter: &str) -> Vec<HistoryRow> {
    let casa = matcher(filter);
    let mut rows = Vec::with_capacity(history.entries().len() + 1);
    if casa(current) {
        rows.push(HistoryRow {
            path: current.clone(),
            mark: HistoryMark::Current,
        });
    }
    for p in history.entries().iter().filter(|p| *p != current) {
        if !casa(p) {
            continue;
        }
        let mark = if history.forward_trail().contains(p) {
            HistoryMark::Forward
        } else {
            HistoryMark::Visited
        };
        rows.push(HistoryRow {
            path: p.clone(),
            mark,
        });
    }
    rows
}

/// Las filas de la lista de populares (`pane.popular`), de más a menos
/// visitada. La del directorio actual lleva [`HistoryMark::Current`] pero no
/// se mueve de su puesto: aquí el orden ES la información.
#[must_use]
pub fn popular_rows(popular: &Popular, current: &VPath, filter: &str) -> Vec<HistoryRow> {
    let casa = matcher(filter);
    popular
        .ranked()
        .into_iter()
        .filter(|e| casa(&e.path))
        .map(|e| HistoryRow {
            path: e.path.clone(),
            mark: if e.path == *current {
                HistoryMark::Current
            } else {
                HistoryMark::Visited
            },
        })
        .collect()
}

fn matcher(filter: &str) -> impl Fn(&VPath) -> bool {
    let needle = crate::nav::fold(filter.as_bytes());
    move |p: &VPath| {
        needle.is_empty() || {
            let (pintable, _) = crate::display::path_display(p);
            crate::palette_state::is_subsequence(&needle, &crate::nav::fold(pintable.as_bytes()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nav::{HISTORY_DEFAULT, HISTORY_MAX, HISTORY_MIN, TrailStep};
    use proptest::prelude::*;

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire")
    }

    /// `norte-config` valida `[ui] history_size` con sus propios números
    /// porque no puede depender de este crate; este test es lo que impide que
    /// los dos topes diverjan.
    #[test]
    fn los_topes_de_la_config_son_los_del_historial() {
        use norte_config::load::UiChrome;
        assert_eq!(UiChrome::MIN_HISTORY_SIZE as usize, HISTORY_MIN);
        assert_eq!(UiChrome::MAX_HISTORY_SIZE as usize, HISTORY_MAX);
        assert_eq!(UiChrome::DEFAULT_HISTORY_SIZE as usize, HISTORY_DEFAULT);
    }

    #[test]
    fn un_replay_o_un_seed_no_cuentan_como_visita() {
        let (mut h, mut p) = (History::default(), Popular::default());
        let (a, b) = (vp("mem:///a"), vp("mem:///b"));
        assert!(!record_visit(
            &mut h,
            &mut p,
            &a,
            &b,
            Trail::Replay(TrailStep::Back)
        ));
        assert!(!record_visit(&mut h, &mut p, &a, &b, Trail::Seed));
        assert!(!record_visit(&mut h, &mut p, &a, &a, Trail::Record));
        assert_eq!((h.back_len(), p.entries().len()), (0, 0));
        assert!(record_visit(&mut h, &mut p, &a, &b, Trail::Record));
        assert_eq!(h.trail(), &[a]);
        assert_eq!(p.entries()[0].path, b, "cuenta a dónde se LLEGA");
    }

    #[test]
    fn populares_expulsa_la_menos_visitada_y_en_empate_la_mas_vieja() {
        let mut p = Popular::default();
        for i in 0..POPULAR_CAP {
            p.visit(&vp(&format!("mem:///d{i}")));
        }
        // d1 sube a dos visitas: ya no es candidata.
        p.visit(&vp("mem:///d1"));
        p.visit(&vp("mem:///nueva"));
        assert_eq!(p.entries().len(), POPULAR_CAP);
        assert!(
            !p.entries().iter().any(|e| e.path == vp("mem:///d0")),
            "d0: una visita y la más vieja"
        );
        assert!(p.entries().iter().any(|e| e.path == vp("mem:///d1")));
        assert_eq!(p.ranked()[0].path, vp("mem:///d1"));
    }

    #[test]
    fn populares_desde_sesion_quita_repetidas_y_respeta_el_tope() {
        let e = |s: &str, visits, last| PopularEntry {
            path: vp(s),
            visits,
            last,
        };
        let mut v = vec![e("mem:///a", 3, 7), e("mem:///a", 9, 9)];
        v.extend((0..POPULAR_CAP).map(|i| e(&format!("mem:///x{i}"), 1, i as u64)));
        let mut p = Popular::from_entries(v);
        assert_eq!(p.entries().len(), POPULAR_CAP);
        assert_eq!(
            p.entries()
                .iter()
                .filter(|x| x.path == vp("mem:///a"))
                .count(),
            1
        );
        p.visit(&vp("mem:///a"));
        let a = p
            .entries()
            .iter()
            .find(|x| x.path == vp("mem:///a"))
            .expect("a");
        assert_eq!(a.visits, 4, "se quedó la primera aparición");
        assert!(a.last > 9, "el reloj sigue al más alto de la sesión");
    }

    #[test]
    fn las_filas_marcan_el_actual_primero_y_la_rama_de_delante() {
        let mut h = History::default();
        h.record(vp("mem:///a"));
        h.record(vp("mem:///b"));
        // Estamos en C; atrás a B: C queda en la rama de delante.
        assert_eq!(h.step_back(vp("mem:///c")), Some(vp("mem:///b")));
        let rows = history_rows(&h, &vp("mem:///b"), "");
        assert_eq!(rows[0].mark, HistoryMark::Current);
        assert_eq!(rows[0].path, vp("mem:///b"));
        assert!(
            rows[1..].iter().all(|r| r.path != vp("mem:///b")),
            "el actual no se repite"
        );
        let a = rows.iter().find(|r| r.path == vp("mem:///a")).expect("a");
        assert_eq!(a.mark, HistoryMark::Visited);
        // C no está en el MRU (nunca se salió de C con un Record), así que no
        // es fila; lo que sí se comprueba es la marca cuando lo está.
        h.push(vp("mem:///c"));
        let rows = history_rows(&h, &vp("mem:///b"), "");
        let c = rows.iter().find(|r| r.path == vp("mem:///c")).expect("c");
        assert_eq!(c.mark, HistoryMark::Forward);
    }

    #[test]
    fn el_filtro_casa_por_subsecuencia_sin_mayusculas() {
        let mut h = History::default();
        h.record(vp("mem:///Documentos/facturas"));
        h.record(vp("mem:///tmp"));
        let rows = history_rows(&h, &vp("mem:///casa"), "dfac");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, vp("mem:///Documentos/facturas"));
    }

    #[test]
    fn quitar_una_ruta_borra_su_punto_de_salto() {
        let mut h = History::default();
        h.set_jump(vp("mem:///a"));
        assert_eq!(jump_target(&h), Ok(vp("mem:///a")));
        h.remove(&vp("mem:///a"));
        assert_eq!(jump_target(&h), Err(NO_JUMP_POINT));
    }

    #[test]
    fn vaciar_conserva_el_punto_de_salto_y_el_tope() {
        let mut h = History::with_capacity(10);
        h.record(vp("mem:///a"));
        h.set_jump(vp("mem:///j"));
        h.clear();
        assert_eq!((h.back_len(), h.entries().len()), (0, 0));
        assert_eq!(h.jump(), Some(&vp("mem:///j")));
        assert_eq!(h.capacity(), 10);
    }

    #[test]
    fn el_tope_se_acota_y_al_bajar_se_queda_lo_cercano() {
        assert_eq!(History::with_capacity(0).capacity(), HISTORY_MIN);
        assert_eq!(History::with_capacity(9999).capacity(), HISTORY_MAX);
        assert_eq!(History::default().capacity(), HISTORY_DEFAULT);
        let mut h = History::with_capacity(20);
        for i in 0..20 {
            h.record(vp(&format!("mem:///d{i}")));
        }
        h.set_capacity(5);
        assert_eq!(h.back_len(), 5);
        assert_eq!(
            h.trail().last(),
            Some(&vp("mem:///d19")),
            "lo último andado"
        );
        assert_eq!(h.entries()[0], vp("mem:///d19"));
        assert_eq!(h.entries().len(), 5);
    }

    #[test]
    fn al_bajar_el_tope_la_rama_de_delante_pierde_su_punta_lejana() {
        let mut h = History::with_capacity(10);
        for i in 0..6 {
            h.record(vp(&format!("mem:///d{i}")));
        }
        // En d6; tres atrás: fwd = [d6, d5, d4], el siguiente adelante es d4.
        let mut cur = vp("mem:///d6");
        for _ in 0..3 {
            cur = h.step_back(cur).expect("atrás");
        }
        h.set_capacity(5);
        assert_eq!(h.back_len() + h.fwd_len(), 5);
        assert_eq!(
            h.step_forward(cur),
            Some(vp("mem:///d4")),
            "lo cercano se queda"
        );
    }

    #[derive(Debug, Clone)]
    enum Op {
        Record(u8),
        Back(u8),
        Forward(u8),
        Remove(u8),
        Cap(usize),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => (0u8..12).prop_map(Op::Record),
            2 => (0u8..12).prop_map(Op::Back),
            2 => (0u8..12).prop_map(Op::Forward),
            1 => (0u8..12).prop_map(Op::Remove),
            1 => (0usize..80).prop_map(Op::Cap),
        ]
    }

    proptest! {
        /// El invariante que acota la memoria del rastro, bajo cualquier
        /// secuencia de operaciones — incluido cambiar el tope en caliente.
        #[test]
        fn el_invariante_del_rastro_aguanta_cualquier_secuencia(
            ops in proptest::collection::vec(op(), 0..200),
            cap in 0usize..80,
        ) {
            let mut h = History::with_capacity(cap);
            for o in ops {
                let d = |i: u8| vp(&format!("mem:///d{i}"));
                match o {
                    Op::Record(i) => h.record(d(i)),
                    Op::Back(i) => { let _ = h.step_back(d(i)); }
                    Op::Forward(i) => { let _ = h.step_forward(d(i)); }
                    Op::Remove(i) => h.remove(&d(i)),
                    Op::Cap(c) => h.set_capacity(c),
                }
                prop_assert!(h.back_len() + h.fwd_len() <= h.capacity());
                prop_assert!(h.entries().len() <= h.capacity());
                prop_assert!((HISTORY_MIN..=HISTORY_MAX).contains(&h.capacity()));
            }
        }
    }
}
