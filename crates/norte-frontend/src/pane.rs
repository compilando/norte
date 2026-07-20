//! Estado PURO de un pane (sin render) para los frontends de norte. La GUI
//! (GPUI) consume [`PaneState`] como su modelo de un panel navegable: cursor,
//! quick search y el listado actual, sin una sola dependencia de UI.
//!
//! El `Pane` de la TUI (`norte-tui::app`) mantiene por ahora su PROPIA copia de
//! esta lógica (cursor + quick entrelazados con el estado de render y de
//! búsqueda viva): ya está testeada y funciona, así que GUI-a no la
//! refactoriza (YAGNI, spec §2). La duplicación es la mecánica de cursor
//! (clamps triviales), no lógica de negocio; unificar el `Pane` de la TUI
//! sobre `PaneState` es deuda anotada (issue #82).
//!
//! GUI-a lista el directorio de una sola vez (sin fill paginado): por eso aquí
//! NO hay contrato de refresh-tras-lote (el streaming incremental de la TUI
//! vía [`crate::nav::QuickSearch::refresh`] es optimización posterior).

use crate::nav::{Mode, QuickSearch};
use norte_proto::{Entry, VPath};
use std::collections::HashSet;

/// Estado no-render de un pane: directorio, entradas (ya ordenadas por el
/// caller con [`sort_entries`](crate::sort_entries)), cursor y quick search.
///
/// El cursor es un índice en `entries` (0 incluso con lista vacía). Con un
/// quick search en modo [`Mode::Filter`] activo, la SELECCIÓN vive dentro del
/// quick search (el cursor real no se mueve hasta confirmar); en
/// [`Mode::Jump`] el cursor real salta directo al match.
#[derive(Debug)]
pub struct PaneState {
    dir: VPath,
    entries: Vec<Entry>,
    cursor: usize,
    loading: bool,
    quick: Option<QuickSearch>,
    marks: HashSet<VPath>,
}

impl PaneState {
    /// Pane sobre `dir` con `entries` (ordénalas antes con
    /// [`sort_entries`](crate::sort_entries)). Cursor en 0, sin quick search,
    /// sin carga pendiente.
    #[must_use]
    pub fn new(dir: VPath, entries: Vec<Entry>) -> Self {
        Self {
            dir,
            entries,
            cursor: 0,
            loading: false,
            quick: None,
            marks: HashSet::new(),
        }
    }

    /// Reemplaza el contenido tras un cd/refresh: resetea el cursor a 0, apaga
    /// el `loading` y MATA cualquier quick search vivo (filtraba OTRO listado).
    pub fn set_listing(&mut self, dir: VPath, entries: Vec<Entry>) {
        self.dir = dir;
        self.entries = entries;
        self.cursor = 0;
        self.loading = false;
        self.quick = None;
        self.marks.clear();
    }

    /// Marca el pane como cargando `dir`: entradas vacías, `loading=true`, sin
    /// quick search. La GUI lo usa para pintar el destino de un cd mientras la
    /// Task de listado corre; el listado real llega luego por [`set_listing`].
    ///
    /// [`set_listing`]: PaneState::set_listing
    pub fn begin_loading(&mut self, dir: VPath) {
        self.dir = dir;
        self.entries = Vec::new();
        self.cursor = 0;
        self.loading = true;
        self.quick = None;
        self.marks.clear();
    }

    /// Sube el cursor una posición (tope en 0). No-op si la lista está vacía.
    pub fn cursor_up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Baja el cursor una posición (tope en la última entrada). No-op si vacía.
    pub fn cursor_down(&mut self) {
        let max = self.entries.len().saturating_sub(1);
        self.cursor = (self.cursor + 1).min(max);
    }

    /// Sube el cursor `n` posiciones (tope en 0).
    pub fn page_up(&mut self, n: usize) {
        self.cursor = self.cursor.saturating_sub(n);
    }

    /// Baja el cursor `n` posiciones (tope en la última entrada).
    pub fn page_down(&mut self, n: usize) {
        let max = self.entries.len().saturating_sub(1);
        self.cursor = (self.cursor + n).min(max);
    }

    /// Cursor a la primera entrada.
    pub fn home(&mut self) {
        self.cursor = 0;
    }

    /// Cursor a la última entrada (0 si la lista está vacía).
    pub fn end(&mut self) {
        self.cursor = self.entries.len().saturating_sub(1);
    }

    /// La entrada seleccionada: con quick search en modo [`Mode::Filter`], la
    /// selección DENTRO del filtro (así las ops operan sobre lo filtrado sin
    /// saber del quick search); si el filtro no tiene matches, `None` (jamás
    /// una entrada que el usuario no ve). Sin filtro (o en [`Mode::Jump`], que
    /// mueve el cursor real), la entrada bajo el cursor.
    #[must_use]
    pub fn selected(&self) -> Option<&Entry> {
        if let Some(q) = &self.quick
            && q.mode() == Mode::Filter
        {
            return self.entries.get(q.selected_entry_index()?);
        }
        self.entries.get(self.cursor)
    }

    /// Directorio listado.
    #[must_use]
    pub fn dir(&self) -> &VPath {
        &self.dir
    }

    /// Entradas actuales (ordenadas por el caller).
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Índice bajo el cursor real (0 incluso con lista vacía).
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// El listado se está cargando (destino de un cd en curso).
    #[must_use]
    pub fn loading(&self) -> bool {
        self.loading
    }

    /// Arranca el quick search en `mode` sobre las entries actuales.
    pub fn quick_start(&mut self, mode: Mode) {
        self.quick = Some(QuickSearch::new(mode, &self.entries));
    }

    /// En [`Mode::Jump`] el cursor REAL sigue a la selección del quick search
    /// (el listado no cambia; saltar ES mover el cursor). En Filter, no-op.
    fn quick_sync_jump(&mut self) {
        if let Some(q) = &self.quick
            && q.mode() == Mode::Jump
            && let Some(i) = q.selected_entry_index()
        {
            self.cursor = i;
        }
    }

    /// Un carácter tecleado con el quick search activo.
    pub fn quick_char(&mut self, c: char) {
        if let Some(q) = &mut self.quick {
            q.push_char(c, &self.entries);
            self.quick_sync_jump();
        }
    }

    /// Backspace con el quick search activo.
    pub fn quick_backspace(&mut self) {
        if let Some(q) = &mut self.quick {
            q.backspace(&self.entries);
            self.quick_sync_jump();
        }
    }

    /// Selección del quick search una posición abajo.
    pub fn quick_down(&mut self) {
        if let Some(q) = &mut self.quick {
            q.down();
            self.quick_sync_jump();
        }
    }

    /// Selección del quick search una posición arriba.
    pub fn quick_up(&mut self) {
        if let Some(q) = &mut self.quick {
            q.up();
            self.quick_sync_jump();
        }
    }

    /// Cierra el quick search fijando el cursor REAL a la selección (Enter: la
    /// op siguiente parte de ahí). Devuelve `true` si el cursor apunta a una
    /// entrada que el usuario VEÍA: en Filter sin matches devuelve `false` (la
    /// lista pintada estaba vacía); en Jump devuelve `true` si hay entradas (el
    /// listado se pinta entero, el cursor real es visible por definición).
    pub fn quick_confirm(&mut self) -> bool {
        let Some(q) = self.quick.take() else {
            return false;
        };
        if let Some(i) = q.selected_entry_index() {
            self.cursor = i;
            return true;
        }
        q.mode() == Mode::Jump && !self.entries.is_empty()
    }

    /// Cierra el quick search SIN tocar el cursor real: en Filter el listado
    /// completo vuelve con el cursor donde estaba; en Jump el cursor se queda
    /// donde saltó.
    pub fn quick_cancel(&mut self) {
        self.quick = None;
    }

    /// Índices REALES visibles bajo el filtro; `None` = sin filtro (quick
    /// inactivo, o modo Jump: el listado se pinta entero).
    #[must_use]
    pub fn quick_visible(&self) -> Option<&[usize]> {
        self.quick
            .as_ref()
            .filter(|q| q.mode() == Mode::Filter)
            .map(QuickSearch::visible)
    }

    /// Togglea la marca de la entrada seleccionada (respeta el filtro quick:
    /// marca la entrada VISIBLE bajo la selección). No-op si no hay selección.
    pub fn toggle_mark(&mut self) {
        let Some(path) = self.selected().map(|e| e.path.clone()) else {
            return;
        };
        if !self.marks.remove(&path) {
            self.marks.insert(path);
        }
    }

    /// ¿Está marcada esta entrada? (por su `VPath` absoluto).
    #[must_use]
    pub fn is_marked(&self, entry: &Entry) -> bool {
        self.marks.contains(&entry.path)
    }

    /// Cuántas entradas marcadas.
    #[must_use]
    pub fn marks_len(&self) -> usize {
        self.marks.len()
    }

    /// Los `VPath` sobre los que opera la acción: las marcas (en el ORDEN de
    /// `entries`, determinista), o la selección (respeta el filtro quick) si
    /// no hay marcas (vacío si tampoco hay selección). Fuente única de "sobre
    /// qué opera la op".
    #[must_use]
    pub fn marked_paths(&self) -> Vec<VPath> {
        if self.marks.is_empty() {
            return self
                .selected()
                .map(|e| e.path.clone())
                .into_iter()
                .collect();
        }
        self.entries
            .iter()
            .filter(|e| self.marks.contains(&e.path))
            .map(|e| e.path.clone())
            .collect()
    }

    /// Limpia todas las marcas.
    pub fn clear_marks(&mut self) {
        self.marks.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::{Entry, EntryKind, VPath};
    fn e(w: &str, k: EntryKind) -> Entry {
        Entry {
            path: VPath::parse(w).unwrap(),
            kind: k,
            size: None,
            mtime_ms: None,
        }
    }
    fn pane(names: &[&str]) -> PaneState {
        let es = names
            .iter()
            .map(|n| e(&format!("mem:///{n}"), EntryKind::File))
            .collect();
        PaneState::new(VPath::parse("mem:///").unwrap(), es)
    }

    #[test]
    fn cursor_se_mueve_con_clamp() {
        let mut p = pane(&["a", "b", "c"]);
        assert_eq!(p.cursor(), 0);
        p.cursor_up(); // clamp en 0
        assert_eq!(p.cursor(), 0);
        p.cursor_down();
        p.cursor_down();
        assert_eq!(p.cursor(), 2);
        p.cursor_down(); // clamp en len-1
        assert_eq!(p.cursor(), 2);
        p.home();
        assert_eq!(p.cursor(), 0);
        p.end();
        assert_eq!(p.cursor(), 2);
    }

    #[test]
    fn selected_respeta_el_filtro_quick() {
        let mut p = pane(&["alfa", "beta", "alto"]);
        p.quick_start(crate::nav::Mode::Filter);
        p.quick_char('a');
        // "alfa" y "alto" casan; el selected es el primero filtrado.
        assert_eq!(
            p.selected().unwrap().path,
            VPath::parse("mem:///alfa").unwrap()
        );
        p.quick_cancel();
        assert_eq!(
            p.selected().unwrap().path,
            VPath::parse("mem:///alfa").unwrap()
        );
    }

    #[test]
    fn set_listing_resetea_cursor_y_cierra_quick() {
        let mut p = pane(&["a", "b"]);
        p.cursor_down();
        p.quick_start(crate::nav::Mode::Filter);
        p.set_listing(
            VPath::parse("mem:///otro").unwrap(),
            vec![e("mem:///otro/x", EntryKind::File)],
        );
        assert_eq!(p.cursor(), 0);
        assert!(p.quick_visible().is_none());
        assert_eq!(p.dir(), &VPath::parse("mem:///otro").unwrap());
    }

    #[test]
    fn page_se_mueve_con_clamp() {
        let mut p = pane(&["a", "b", "c"]);
        p.page_down(100); // clamp en len-1
        assert_eq!(p.cursor(), 2);
        p.page_up(100); // clamp en 0
        assert_eq!(p.cursor(), 0);

        let mut vacia = pane(&[]);
        vacia.page_down(100); // no-op, sin panic
        assert_eq!(vacia.cursor(), 0);
        vacia.page_up(100);
        assert_eq!(vacia.cursor(), 0);
    }

    #[test]
    fn begin_loading_deja_estado_transitorio() {
        let mut p = pane(&["a", "b", "c"]);
        p.cursor_down();
        p.begin_loading(VPath::parse("mem:///nuevo").unwrap());
        assert!(p.entries().is_empty());
        assert!(p.loading());
        assert!(p.selected().is_none());
        assert_eq!(p.dir(), &VPath::parse("mem:///nuevo").unwrap());

        p.set_listing(
            VPath::parse("mem:///nuevo").unwrap(),
            vec![e("mem:///nuevo/x", EntryKind::File)],
        );
        assert!(!p.loading());
        assert_eq!(p.cursor(), 0);
        assert!(p.selected().is_some());
    }

    #[test]
    fn end_en_lista_vacia_no_panica() {
        let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), vec![]);
        p.home();
        p.end();
        p.cursor_down();
        p.cursor_up();
        assert_eq!(p.cursor(), 0);
        assert!(p.selected().is_none());
    }

    #[test]
    fn toggle_marca_y_desmarca_la_entrada_bajo_cursor() {
        let mut p = pane(&["a", "b", "c"]);
        p.cursor_down(); // cursor en "b"
        assert_eq!(p.marks_len(), 0);
        p.toggle_mark();
        assert_eq!(p.marks_len(), 1);
        assert!(p.is_marked(&e("mem:///b", EntryKind::File)));
        assert!(!p.is_marked(&e("mem:///a", EntryKind::File)));
        p.toggle_mark(); // desmarca
        assert_eq!(p.marks_len(), 0);
        assert!(!p.is_marked(&e("mem:///b", EntryKind::File)));
    }

    #[test]
    fn marked_paths_sin_marcas_devuelve_el_target_del_cursor() {
        let mut p = pane(&["a", "b", "c"]);
        p.cursor_down(); // "b"
        assert_eq!(p.marked_paths(), vec![VPath::parse("mem:///b").unwrap()]);
    }

    #[test]
    fn marked_paths_con_marcas_en_orden_de_entries() {
        let mut p = pane(&["a", "b", "c"]);
        p.cursor_down();
        p.cursor_down();
        p.toggle_mark(); // marca "c"
        p.home();
        p.toggle_mark(); // marca "a"
        // Orden = el de `entries` (determinista), no el de inserción.
        assert_eq!(
            p.marked_paths(),
            vec![
                VPath::parse("mem:///a").unwrap(),
                VPath::parse("mem:///c").unwrap(),
            ]
        );
    }

    #[test]
    fn set_listing_limpia_las_marcas() {
        let mut p = pane(&["a", "b"]);
        p.toggle_mark();
        assert_eq!(p.marks_len(), 1);
        p.set_listing(
            VPath::parse("mem:///otro").unwrap(),
            vec![e("mem:///otro/x", EntryKind::File)],
        );
        assert_eq!(p.marks_len(), 0);
    }

    #[test]
    fn begin_loading_limpia_las_marcas() {
        let mut p = pane(&["a", "b"]);
        p.toggle_mark();
        p.begin_loading(VPath::parse("mem:///nuevo").unwrap());
        assert_eq!(p.marks_len(), 0);
    }

    #[test]
    fn toggle_bajo_filtro_marca_la_seleccion_visible() {
        let mut p = pane(&["alfa", "beta", "alto"]);
        p.quick_start(crate::nav::Mode::Filter);
        p.quick_char('a'); // "alfa" y "alto" visibles; selección = "alfa"
        p.toggle_mark();
        assert!(p.is_marked(&e("mem:///alfa", EntryKind::File)));
        assert!(!p.is_marked(&e("mem:///beta", EntryKind::File)));
    }

    #[test]
    fn marca_identidad_por_bytes_del_path_nombre_hostil() {
        // Un nombre con bytes NO-UTF8 (0xFF): la marca lo distingue por su
        // VPath exacto, sin degradar a lossy (regla 1).
        let hostile = VPath::parse("mem:///")
            .unwrap()
            .join(norte_proto::Segment::new(vec![0xFF, 0xFE]).unwrap());
        let benign = VPath::parse("mem:///a").unwrap();
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                Entry {
                    path: hostile.clone(),
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                },
                Entry {
                    path: benign.clone(),
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                },
            ],
        );
        p.toggle_mark(); // marca la hostil (cursor 0)
        assert!(p.marks.contains(&hostile));
        assert!(!p.marks.contains(&benign));
        assert_eq!(p.marked_paths(), vec![hostile]);
    }

    #[test]
    fn clear_marks_vacia_el_set() {
        let mut p = pane(&["a", "b"]);
        p.toggle_mark();
        p.cursor_down();
        p.toggle_mark();
        assert_eq!(p.marks_len(), 2);
        p.clear_marks();
        assert_eq!(p.marks_len(), 0);
    }

    #[test]
    fn marked_paths_sin_entries_ni_marcas_es_vacio() {
        let p = PaneState::new(VPath::parse("mem:///").unwrap(), vec![]);
        assert!(p.marked_paths().is_empty());
    }

    #[test]
    fn marcas_distinguen_gemelos_nfc_y_nfd_sin_plegar() {
        // é en NFC (0xC3 0xA9) vs NFD (0x65 0xCC 0x81): bytes distintos, misma
        // forma visual. La marca NO debe plegarlos (trampa macOS: preserva bytes).
        let nfc = VPath::parse("mem:///")
            .unwrap()
            .join(norte_proto::Segment::new(vec![0xC3, 0xA9]).unwrap());
        let nfd = VPath::parse("mem:///")
            .unwrap()
            .join(norte_proto::Segment::new(vec![0x65, 0xCC, 0x81]).unwrap());
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                Entry {
                    path: nfc.clone(),
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                },
                Entry {
                    path: nfd.clone(),
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                },
            ],
        );
        p.toggle_mark();
        p.cursor_down();
        p.toggle_mark();
        assert_eq!(p.marks_len(), 2, "nfc y nfd son DOS marcas distintas");
        assert!(p.marks.contains(&nfc));
        assert!(p.marks.contains(&nfd));
    }
}
