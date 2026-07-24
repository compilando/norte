//! Estado PURO de un pane (sin render) para los frontends de norte. La GUI
//! (GPUI) consume [`PaneState`] como su modelo de un panel navegable: cursor,
//! quick search y el listado actual, sin una sola dependencia de UI.
//!
//! El `Pane` de la TUI (`norte-tui::app`) embebe este `PaneState` y le delega
//! toda la mecánica pura de cursor + quick search (#82 cerrado): la
//! duplicación se eliminó; la TUI solo añade su estado de render, scroll de
//! ratatui y búsqueda viva ENCIMA de `PaneState`.
//!
//! Incluye el contrato de refresh-tras-lote para el fill paginado (ADR 0017):
//! [`PaneState::extend`] (añade un lote, re-ordena y re-ancla el cursor por
//! path), [`PaneState::refill`] (reemplaza el listado del mismo dir con clamp)
//! y [`PaneState::refresh_quick`] (re-aplica el filtro vivo). La GUI hoy lista
//! de una sola vez y no los usa; la TUI sí.

use crate::nav::{Mode, QuickSearch};
use crate::sort::SortKey;
use norte_proto::{Entry, VPath};
use std::collections::HashSet;

/// Estado no-render de un pane: directorio, entradas (normalizadas
/// internamente — ya no exige orden previo del caller, ver [`PaneState::new`]),
/// cursor y quick search.
///
/// El cursor es un índice en `entries` (0 incluso con lista vacía). Con un
/// quick search en modo [`Mode::Filter`] activo, la SELECCIÓN vive dentro del
/// quick search (el cursor real no se mueve hasta confirmar); en
/// [`Mode::Jump`] el cursor real salta directo al match.
#[derive(Debug)]
pub struct PaneState {
    dir: VPath,
    entries: Vec<Entry>,
    /// Claves de orden persistidas, índice-paralelas a `entries` (#54): el
    /// fill mergea lotes O(n+m) sin recomputar la clave NFC de lo ya listado.
    sort_keys: Vec<SortKey>,
    cursor: usize,
    loading: bool,
    quick: Option<QuickSearch>,
    marks: HashSet<VPath>,
    /// Reinterpretación de NOMBRES no-UTF8 para display (#57, spec §6.1):
    /// `Some(enc)` = «ver nombres como enc» — SOLO display, los bytes jamás
    /// se mutan (regla 1). Compartida por los frontends (#98/m2): el quick
    /// search pliega sobre el texto reinterpretado y los renders la leen
    /// vía [`PaneState::name_encoding`]. Persistente por pane.
    name_encoding: Option<norte_encoding::NameEncoding>,
    /// Índice del ciclo por el que ENTRÓ la reinterpretación activa (la
    /// sugerencia de chardetng, o 0): el ciclo da la VUELTA COMPLETA y se
    /// apaga al volver aquí — sin esto, los encodings anteriores a la
    /// sugerencia serían inalcanzables (M1 del review #57).
    name_encoding_entry: usize,
    /// Omitidas del CONTENEDOR del listado actual (#93/#96): entradas que el
    /// índice del provider archive descartó (nombres hostiles/límites) y que
    /// por tanto NO están en `entries` — un listado incompleto jamás es
    /// silencioso. `None` = no aplica/desconocido; los frontends solo pintan
    /// `Some(n)` con `n > 0`. Se resetea con cada listado nuevo
    /// ([`Self::set_listing`]); el caller lo fija con el valor FRESCO de su
    /// `list_with_skipped`/`list_stream`.
    skipped: Option<u64>,
    /// Memoria de cursor por directorio (spec 2026-07-24 §S1): sesión-solo,
    /// per-pane (no persiste entre reinicios — mismo precedente que
    /// [`crate::nav`]'s history), LRU por recencia con tope
    /// [`CURSOR_MEMORY_CAP`]. Identidad de dir por [`VPath`] BYTE-EXACTO
    /// (regla 1): jamás se normaliza, así que dos gemelos hostiles con la
    /// misma forma visual pero bytes distintos son entradas DISTINTAS. Se
    /// alimenta con [`Self::remember_cursor`] y se consulta desde
    /// [`Self::set_listing`].
    cursor_memory: Vec<(VPath, usize)>,
    /// Foco pendiente de un `nav.parent` (spec §S1): el hijo del que
    /// venimos, para seleccionarlo en el listado del padre. Gana sobre
    /// [`Self::cursor_memory`] y se CONSUME (una sola vez) en el próximo
    /// [`Self::set_listing`], case o no case con una entrada del listado.
    /// Identidad por `VPath` byte-exacto, igual que la memoria.
    pending_focus: Option<VPath>,
}

/// Tope de la memoria de cursor por pane (spec §S1): sesión larga sin fuga
/// de memoria sin depender de una nueva dependencia (LRU a mano sobre un
/// `Vec`, barato para decenas de dirs visitados).
const CURSOR_MEMORY_CAP: usize = 64;

impl PaneState {
    /// Pane sobre `dir` con `entries` (se normalizan internamente: ya no hace
    /// falta ordenarlas antes — el contrato "ordénalas antes" deja de ser
    /// footgun, ver [`sort_entries`](crate::sort_entries) para el criterio).
    /// Cursor en 0, sin quick search, sin carga pendiente.
    #[must_use]
    pub fn new(dir: VPath, entries: Vec<Entry>) -> Self {
        let (entries, sort_keys) = crate::sort::sort_with_keys(entries);
        Self {
            dir,
            entries,
            sort_keys,
            cursor: 0,
            loading: false,
            quick: None,
            marks: HashSet::new(),
            name_encoding: None,
            name_encoding_entry: 0,
            skipped: None,
            cursor_memory: Vec::new(),
            pending_focus: None,
        }
    }

    /// Reinterpretación de nombres activa (#57): los renders pintan con ella
    /// ([`crate::display_name_with`]) y el quick search pliega sobre el
    /// mismo texto.
    #[must_use]
    pub fn name_encoding(&self) -> Option<norte_encoding::NameEncoding> {
        self.name_encoding
    }

    /// Cicla la reinterpretación de nombres (#57): `None` → (sugerencia de
    /// chardetng sobre los nombres no-UTF8 del listado, si cae en el ciclo;
    /// si no, cp437) → VUELTA COMPLETA al ciclo — todos los encodings
    /// alcanzables desde cualquier sugerencia — → `None` al regresar al
    /// punto de entrada. Un quick search vivo se RE-PLIEGA sobre el texto
    /// nuevo (#98/F1: el filtro casa contra lo que se VE). Devuelve la
    /// etiqueta a anunciar (`None` = modo apagado).
    pub fn cycle_name_encoding(&mut self) -> Option<&'static str> {
        let cycle = norte_encoding::name_reinterpret_cycle();
        self.name_encoding = match self.name_encoding {
            None => {
                let raws: Vec<&[u8]> = self
                    .entries
                    .iter()
                    .filter_map(|e| e.path.file_name().map(norte_proto::Segment::as_bytes))
                    .filter(|b| std::str::from_utf8(b).is_err())
                    .collect();
                let sugerido = norte_encoding::suggest_name_encoding(&raws);
                let entry = sugerido
                    .and_then(|s| cycle.iter().position(|e| *e == s))
                    .unwrap_or(0);
                self.name_encoding_entry = entry;
                Some(cycle[entry])
            }
            Some(cur) => match cycle.iter().position(|e| *e == cur) {
                Some(i) => {
                    let next = (i + 1) % cycle.len();
                    // Vuelta completada: apagar (el ciclo siempre acaba en
                    // off, pase por donde pase la sugerencia de entrada).
                    (next != self.name_encoding_entry).then(|| cycle[next])
                }
                // Valor fuera del ciclo (imposible hoy): apagar.
                None => None,
            },
        };
        // #98/F1: el cache de folds del quick vivo quedó plegado con el
        // encoding anterior — re-plegar conservando la selección.
        let prev = self.quick_selected_path();
        let enc = self.name_encoding;
        if let Some(q) = &mut self.quick {
            q.set_name_encoding(enc, &self.entries, prev.as_ref());
        }
        self.quick_sync_jump();
        self.name_encoding.map(|e| e.label())
    }

    /// Reemplaza el contenido tras un cd/refresh: resetea el cursor a 0, apaga
    /// el `loading` y MATA cualquier quick search vivo (filtraba OTRO listado).
    /// Normaliza `entries` internamente (mismo contrato que [`PaneState::new`]).
    ///
    /// Tras el reset, RESTAURA el cursor (spec §S1) en este orden de
    /// precedencia: (1) [`Self::set_pending_focus`] si hay un hint pendiente
    /// Y una entrada de `entries` casa su path byte-exacto (se CONSUME aquí,
    /// haya o no match); (2) si no, la memoria por dir
    /// ([`Self::remember_cursor`]) para el `dir` nuevo, con clamp; (3) si
    /// ninguna aplica, 0 — el comportamiento de siempre.
    pub fn set_listing(&mut self, dir: VPath, entries: Vec<Entry>) {
        let (entries, sort_keys) = crate::sort::sort_with_keys(entries);
        self.dir = dir;
        self.entries = entries;
        self.sort_keys = sort_keys;
        self.cursor = 0;
        self.loading = false;
        self.quick = None;
        self.marks.clear();
        // #96: las omitidas eran del listado ANTERIOR; el caller fija las
        // frescas con `set_skipped` si su fuente las trae.
        self.skipped = None;

        let restored = self
            .pending_focus
            .take()
            .and_then(|child| self.entries.iter().position(|e| e.path == child))
            .or_else(|| {
                self.cursor_memory
                    .iter()
                    .find(|(d, _)| *d == self.dir)
                    .map(|&(_, c)| c)
            });
        if let Some(i) = restored {
            self.set_cursor(i);
        }
    }

    /// Marca el pane como cargando `dir`: entradas vacías, `loading=true`, sin
    /// quick search. La GUI lo usa para pintar el destino de un cd mientras la
    /// Task de listado corre; el listado real llega luego por [`set_listing`].
    ///
    /// Punto de captura de la memoria de cursor (spec §S1) para el flujo de
    /// la GUI: graba `(dir viejo, cursor viejo)` con [`Self::remember_cursor`]
    /// ANTES de pisar el estado con el destino nuevo — es el único momento en
    /// que el dir viejo sigue en `self.dir`. La TUI no llama a este método
    /// (su `cd` espera el fetch entero antes de tocar el pane, ver
    /// `norte-tui::app::Pane::begin_listing`), así que graba en su propio
    /// punto de captura equivalente, justo antes de llamar a
    /// [`Self::set_listing`].
    ///
    /// [`set_listing`]: PaneState::set_listing
    pub fn begin_loading(&mut self, dir: VPath) {
        self.remember_cursor();
        self.dir = dir;
        self.entries = Vec::new();
        self.sort_keys = Vec::new();
        self.cursor = 0;
        self.loading = true;
        self.quick = None;
        self.marks.clear();
        self.skipped = None;
    }

    /// Omitidas del contenedor del listado actual (#93/#96) — ver el campo.
    #[must_use]
    pub fn skipped(&self) -> Option<u64> {
        self.skipped
    }

    /// Fija las omitidas FRESCAS del listado actual (#96): llamar tras
    /// [`Self::set_listing`]/[`Self::refill`] con el valor de la MISMA
    /// respuesta de listado (`list_with_skipped`/`list_stream`) — nunca
    /// arrastrar el de un listado anterior.
    pub fn set_skipped(&mut self, skipped: Option<u64>) {
        self.skipped = skipped;
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

    /// Arranca el quick search en `mode` sobre las entries actuales, plegando
    /// con la reinterpretación de nombres vigente (#98/F1).
    pub fn quick_start(&mut self, mode: Mode) {
        self.quick = Some(QuickSearch::new(mode, &self.entries, self.name_encoding));
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
            q.push_char(c);
            self.quick_sync_jump();
        }
    }

    /// Backspace con el quick search activo.
    pub fn quick_backspace(&mut self) {
        if let Some(q) = &mut self.quick {
            q.backspace();
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

    /// Fija el cursor real a `i` con clamp (jamás fuera de rango). Para re-anclar
    /// tras localizar un índice concreto (p. ej. un hit de búsqueda). (#82)
    pub fn set_cursor(&mut self, i: usize) {
        let max = self.entries.len().saturating_sub(1);
        self.cursor = i.min(max);
    }

    /// Graba `(dir actual, cursor actual)` en la memoria de cursor (spec
    /// §S1): sesión-solo, por pane, LRU con tope `CURSOR_MEMORY_CAP`
    /// (constante privada del módulo, 64).
    /// Reemplaza cualquier entrada previa del mismo dir (identidad
    /// byte-exacta, sin normalizar — regla 1) para que cada dir tenga como
    /// mucho UNA entrada, siempre la más reciente.
    ///
    /// El caller debe invocarlo mientras `self.dir`/`self.cursor` TODAVÍA
    /// reflejan el dir que se está abandonando — antes de cualquier reset
    /// (ver [`Self::begin_loading`], que lo llama primero por eso).
    pub fn remember_cursor(&mut self) {
        let dir = self.dir.clone();
        self.cursor_memory.retain(|(d, _)| *d != dir);
        self.cursor_memory.push((dir, self.cursor));
        if self.cursor_memory.len() > CURSOR_MEMORY_CAP {
            self.cursor_memory.remove(0);
        }
    }

    /// Fija un foco pendiente (spec §S1, `nav.parent`): en el PRÓXIMO
    /// [`Self::set_listing`], si una entrada del listado nuevo tiene este
    /// path EXACTO (bytes, sin normalizar — regla 1), el cursor aterriza
    /// ahí — por delante de la memoria. Se consume una sola vez (match o
    /// no) para no filtrar a navegaciones futuras no relacionadas.
    pub fn set_pending_focus(&mut self, child: VPath) {
        self.pending_focus = Some(child);
    }

    /// Descarta un foco pendiente SIN consumirlo contra un listado (revisión
    /// S, M2): [`Self::set_listing`] es el ÚNICO sitio que hasta ahora
    /// consumía `pending_focus` — un `nav.parent` cuyo `cd` FALLA (permiso
    /// denegado, error del daemon…) nunca llega a `set_listing`, así que el
    /// hint quedaba vivo y podía aterrizar en un `cd` MUY posterior y sin
    /// relación, en el pane equivocado. El caller (`nav.parent`, ambos
    /// frontends) llama a esto en la rama de error del `cd`.
    pub fn clear_pending_focus(&mut self) {
        self.pending_focus = None;
    }

    /// Marca/desmarca el pane como cargando SIN tocar el resto del estado: un
    /// fill paginado (ADR 0017) pinta la primera página y sigue (`true`), baja
    /// el flag al terminar (`false`). (#82)
    pub fn set_loading(&mut self, loading: bool) {
        self.loading = loading;
    }

    /// Siguiente match con wrap (Tab en modo [`Mode::Jump`]): mueve la selección
    /// del quick al match siguiente y, en Jump, arrastra el cursor real. No-op
    /// sin quick search. (#82)
    pub fn quick_next(&mut self) {
        if let Some(q) = &mut self.quick {
            q.next_match();
            self.quick_sync_jump();
        }
    }

    /// El quick search vivo (para que el render pinte la query y su contador);
    /// `None` = navegación normal. Solo lectura. (#82)
    #[must_use]
    pub fn quick(&self) -> Option<&QuickSearch> {
        self.quick.as_ref()
    }

    /// El path de la entrada seleccionada DENTRO del quick search, capturado
    /// ANTES de mutar/re-ordenar `entries` (contrato de [`QuickSearch::refresh`]:
    /// los índices previos al sort no identifican nada). (#82)
    fn quick_selected_path(&self) -> Option<VPath> {
        let i = self.quick.as_ref()?.selected_entry_index()?;
        Some(self.entries.get(i)?.path.clone())
    }

    /// Añade `batch` a un listado paginado en curso (ADR 0017): #54 mergea
    /// O(n+m) con las claves NFC PERSISTIDAS (`sort_keys`) — el lote se ordena
    /// solo y se mergea de forma estable contra lo ya listado, mismo orden
    /// final que [`sort_entries`](crate::sort_entries) sobre el total, sin
    /// recomputar la clave de lo que ya estaba. Reconcilia: re-ancla el cursor
    /// al PATH seleccionado (clamp por índice si desapareció) y RE-APLICA el
    /// quick por path. Lote vacío = no-op. (#82)
    pub fn extend(&mut self, batch: Vec<Entry>) {
        if batch.is_empty() {
            return;
        }
        let quick_prev = self.quick_selected_path();
        let anchor = self.entries.get(self.cursor).map(|e| e.path.clone());
        let (batch, batch_keys) = crate::sort::sort_with_keys(batch);
        crate::sort::merge_keyed(&mut self.entries, &mut self.sort_keys, batch, batch_keys);
        self.cursor = anchor
            .and_then(|p| self.entries.iter().position(|e| e.path == p))
            .unwrap_or_else(|| self.cursor.min(self.entries.len().saturating_sub(1)));
        if let Some(q) = &mut self.quick {
            q.refresh(&self.entries, quick_prev.as_ref());
        }
        self.quick_sync_jump();
    }

    /// Reemplaza el listado COMPLETO del MISMO dir (refresh tras mutación):
    /// conserva el cursor por ÍNDICE con clamp (tras un delete queda en la
    /// siguiente entrada — ortodoxo) y RE-APLICA el quick por path. No toca el
    /// flag de carga. Normaliza `entries` internamente (#54: cierra el mismo
    /// footgun que `new`/`set_listing` — idempotente si el caller ya venía
    /// ordenado). (#82)
    pub fn refill(&mut self, entries: Vec<Entry>) {
        let quick_prev = self.quick_selected_path();
        let (entries, sort_keys) = crate::sort::sort_with_keys(entries);
        self.cursor = self.cursor.min(entries.len().saturating_sub(1));
        self.entries = entries;
        self.sort_keys = sort_keys;
        if let Some(q) = &mut self.quick {
            q.refresh(&self.entries, quick_prev.as_ref());
        }
        self.quick_sync_jump();
    }

    /// RE-APLICA el quick vivo sobre las entradas ACTUALES (sin cambiarlas ni
    /// mover el cursor real): cierra un fill cuyo cierre podría re-ordenar. (#82)
    pub fn refresh_quick(&mut self) {
        let quick_prev = self.quick_selected_path();
        if let Some(q) = &mut self.quick {
            q.refresh(&self.entries, quick_prev.as_ref());
        }
    }

    /// Hidrata size/mtime de la entrada `path` (stat on-demand, #52). No-op si
    /// la entrada ya no está (un refresh la pisó). No reordena: size/mtime no
    /// participan en el sort.
    pub fn hydrate(&mut self, path: &VPath, size: Option<u64>, mtime_ms: Option<i64>) {
        if let Some(e) = self.entries.iter_mut().find(|e| &e.path == path) {
            e.size = e.size.or(size);
            e.mtime_ms = e.mtime_ms.or(mtime_ms);
        }
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
        // #54: `new` normaliza — orden por bytes crudos pone "a" (0x61) antes
        // que 0xFF, así que la hostil queda en el índice 1, no en el cursor 0.
        p.cursor_down();
        p.toggle_mark(); // marca la hostil
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

    // --- S1: memoria de cursor por directorio + foco pendiente (spec
    // 2026-07-24 §S1) ---------------------------------------------------

    /// Round trip básico: dejar un dir con el cursor movido, navegar a otro,
    /// volver — el cursor se restaura donde quedó (no en 0).
    #[test]
    fn cursor_memory_round_trip_basico() {
        let mut p = pane(&["a", "b", "c"]);
        p.set_cursor(2); // "c"
        p.remember_cursor(); // simula el punto de captura de begin_loading
        p.set_listing(
            VPath::parse("mem:///otro").unwrap(),
            vec![e("mem:///otro/x", EntryKind::File)],
        );
        assert_eq!(p.cursor(), 0, "dir nuevo, sin memoria: 0 de siempre");

        // Volver al dir original: begin_loading (aquí simulado con
        // remember_cursor + set_listing, igual que la GUI real) debe
        // restaurar el cursor recordado.
        p.remember_cursor();
        p.set_listing(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///a", EntryKind::File),
                e("mem:///b", EntryKind::File),
                e("mem:///c", EntryKind::File),
            ],
        );
        assert_eq!(p.cursor(), 2, "restaura el cursor recordado de mem:///");
    }

    /// Si el listado del dir recordado encogió, la restauración clampa.
    #[test]
    fn cursor_memory_restaura_con_clamp_si_encogio() {
        let mut p = pane(&["a", "b", "c"]);
        p.set_cursor(2); // "c"
        p.remember_cursor();
        p.set_listing(VPath::parse("mem:///otro").unwrap(), vec![]);
        p.remember_cursor();
        // Volvemos a "mem:///" pero ahora con solo 1 entrada.
        p.set_listing(
            VPath::parse("mem:///").unwrap(),
            vec![e("mem:///a", EntryKind::File)],
        );
        assert_eq!(p.cursor(), 0, "clamp: solo hay índice 0 disponible");
    }

    /// LRU: al superar el cap (64), la entrada más antigua se descarta.
    #[test]
    fn cursor_memory_lru_evict_al_superar_cap() {
        let mut p = PaneState::new(VPath::parse("mem:///d0").unwrap(), vec![]);
        // 65 dirs distintos, cada uno con cursor=7 (arbitrario, no importa el
        // clamp aquí: cada listing tiene una sola entrada, pero lo que se
        // recuerda es el valor crudo antes del clamp del set_cursor).
        for i in 0..65 {
            p.set_cursor(7); // clamp interno no afecta: listado vacío -> 0
            p.remember_cursor();
            p.set_listing(VPath::parse(&format!("mem:///d{}", i + 1)).unwrap(), vec![]);
        }
        // La entrada para "mem:///d0" (la primerísima, antes del bucle) debe
        // haber sido expulsada: si no lo fue, volver a "mem:///d0" con un
        // listado de 65 entradas restauraría el cursor a un índice != 0.
        let mut entries = Vec::new();
        for i in 0..65 {
            entries.push(e(&format!("mem:///d0/x{i:02}"), EntryKind::File));
        }
        p.remember_cursor();
        p.set_listing(VPath::parse("mem:///d0").unwrap(), entries);
        assert_eq!(
            p.cursor(),
            0,
            "d0 fue expulsado de la memoria LRU (cap 64), no hay nada que restaurar"
        );
    }

    /// `set_pending_focus` gana sobre la memoria y se consume una sola vez.
    #[test]
    fn pending_focus_gana_sobre_memoria_y_se_consume_una_vez() {
        let mut p = pane(&["a", "b", "c"]);
        p.set_cursor(2); // "c" — esto quedará en memoria para "mem:///"
        p.remember_cursor();
        p.set_listing(
            VPath::parse("mem:///a").unwrap(),
            vec![e("mem:///a/x", EntryKind::File)],
        );
        // Foco pendiente hacia "b" al volver a "mem:///".
        p.set_pending_focus(VPath::parse("mem:///b").unwrap());
        p.remember_cursor();
        p.set_listing(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///a", EntryKind::File),
                e("mem:///b", EntryKind::File),
                e("mem:///c", EntryKind::File),
            ],
        );
        assert_eq!(
            p.selected().unwrap().path,
            VPath::parse("mem:///b").unwrap(),
            "pending_focus gana sobre la memoria (que apuntaba a \"c\")"
        );

        // Segunda vuelta, SIN volver a fijar pending_focus: si el hint no
        // se hubiese consumido, seguiría ganando y aterrizaríamos otra vez
        // en "b" pase lo que pase. Movemos el cursor a "a" (índice 0) antes
        // de salir para que la memoria prediga un resultado DISTINTO de
        // "b" — solo la memoria (no un pending_focus fantasma) explica el
        // resultado.
        p.set_cursor(0); // "a"
        p.remember_cursor(); // sobrescribe la memoria de "mem:///" a 0
        p.set_listing(
            VPath::parse("mem:///a").unwrap(),
            vec![e("mem:///a/x", EntryKind::File)],
        );
        p.remember_cursor();
        p.set_listing(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///a", EntryKind::File),
                e("mem:///b", EntryKind::File),
                e("mem:///c", EntryKind::File),
            ],
        );
        assert_eq!(
            p.selected().unwrap().path,
            VPath::parse("mem:///a").unwrap(),
            "consumido: la segunda vuelta usa memoria (a), no el pending_focus viejo (b)"
        );
    }

    /// Revisión S, M2: un `cd` que FALLA no debe dejar un `pending_focus`
    /// fantasma vivo para un `set_listing` futuro y sin relación —
    /// `clear_pending_focus` (llamado por el caller en la rama de error del
    /// `cd`) lo descarta SIN consumirlo contra ningún listado.
    #[test]
    fn clear_pending_focus_descarta_el_hint_sin_listado() {
        let mut p = pane(&["a", "b", "c"]);
        p.set_pending_focus(VPath::parse("mem:///b").unwrap());
        p.clear_pending_focus();
        // Un `set_listing` posterior (el reintento del `cd`, o uno
        // totalmente distinto) NO aterriza en "b": no hay memoria para
        // "mem:///" en este pane fresco, así que el cursor cae al 0 de
        // siempre — si el hint hubiera sobrevivido, "b" ganaría igual.
        p.set_listing(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///a", EntryKind::File),
                e("mem:///b", EntryKind::File),
                e("mem:///c", EntryKind::File),
            ],
        );
        assert_eq!(p.cursor(), 0, "el hint descartado no debe ganar");
    }

    /// Bytes hostiles (segmento 0xFF/0xFE, forma wire del corpus): la
    /// memoria y el `pending_focus` identifican por PATH EXACTO en bytes, sin
    /// normalizar (regla 1 — nunca se pliegan gemelos hostiles).
    #[test]
    fn cursor_memory_y_pending_focus_byte_exacto_con_path_hostil() {
        let root = VPath::parse("mem:///").unwrap();
        let hostile = root
            .clone()
            .join(norte_proto::Segment::new(vec![0xFF, 0xFE]).unwrap());
        let benign = root
            .clone()
            .join(norte_proto::Segment::new(b"a".to_vec()).unwrap());
        let mut p = PaneState::new(
            root.clone(),
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
        // "new" normaliza: 0xFF > 0x61 => hostile queda en el índice 1.
        p.set_cursor(1);
        assert_eq!(p.selected().unwrap().path, hostile);

        // Simula entrar al dir hostil (cd) y salir de nuevo (parent-nav): el
        // hint se fija DESPUÉS de entrar, justo antes de volver al padre —
        // igual que `nav.parent` real (spec §S1 punto 3).
        p.remember_cursor();
        p.set_listing(hostile.clone(), vec![]);
        p.set_pending_focus(hostile.clone());
        p.remember_cursor();
        p.set_listing(
            root,
            vec![
                Entry {
                    path: hostile.clone(),
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                },
                Entry {
                    path: benign,
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                },
            ],
        );
        assert_eq!(
            p.selected().unwrap().path,
            hostile,
            "pending_focus casa por bytes exactos, sin plegar la forma hostil"
        );
    }

    /// `begin_loading` captura el dir VIEJO (y su cursor) antes de pisar el
    /// estado con el destino nuevo — es el punto de captura real para la
    /// GUI (`PaneState::begin_loading` se llama ANTES del fetch async).
    #[test]
    fn begin_loading_graba_el_dir_viejo_antes_de_pisarlo() {
        let mut p = pane(&["a", "b", "c"]);
        p.set_cursor(2); // "c" en "mem:///"
        p.begin_loading(VPath::parse("mem:///nuevo").unwrap());
        assert_eq!(p.cursor(), 0, "el destino arranca en 0 mientras carga");
        // set_listing del MISMO dir nuevo no debe alterar lo grabado del
        // dir viejo: volver a "mem:///" restaura el cursor grabado por
        // begin_loading, no un valor corrupto.
        p.set_listing(
            VPath::parse("mem:///nuevo").unwrap(),
            vec![e("mem:///nuevo/x", EntryKind::File)],
        );
        p.remember_cursor();
        p.set_listing(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///a", EntryKind::File),
                e("mem:///b", EntryKind::File),
                e("mem:///c", EntryKind::File),
            ],
        );
        assert_eq!(
            p.cursor(),
            2,
            "begin_loading grabó (mem:///, 2) antes de pisar el dir"
        );
    }

    #[test]
    fn set_cursor_fija_con_clamp() {
        let mut p = pane(&["a", "b", "c"]);
        p.set_cursor(2);
        assert_eq!(p.cursor(), 2);
        p.set_cursor(99); // clamp en len-1
        assert_eq!(p.cursor(), 2);
        p.set_cursor(0);
        assert_eq!(p.cursor(), 0);

        let mut vacia = pane(&[]);
        vacia.set_cursor(5); // no-op, sin panic
        assert_eq!(vacia.cursor(), 0);
    }

    #[test]
    fn set_loading_togglea_el_flag() {
        let mut p = pane(&["a"]);
        assert!(!p.loading());
        p.set_loading(true);
        assert!(p.loading());
        p.set_loading(false);
        assert!(!p.loading());
    }

    #[test]
    fn quick_next_mueve_el_cursor_real_con_wrap() {
        // #54: `new` normaliza (dirs primero, alfabético dentro del grupo).
        // "aa"(dir) y "ac"(file) casan con 'a'; "bb"(dir) queda en medio (no
        // casa) para seguir probando que Tab SALTA el no-match intermedio.
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///aa", EntryKind::Dir),
                e("mem:///bb", EntryKind::Dir),
                e("mem:///ac", EntryKind::File),
            ],
        );
        p.quick_start(crate::nav::Mode::Jump);
        p.quick_char('a');
        assert_eq!(p.cursor(), 0, "salta al primer match");
        assert!(p.quick_visible().is_none(), "en salto el listado va entero");
        p.quick_next();
        assert_eq!(
            p.cursor(),
            2,
            "Tab: siguiente match, salta el no-match intermedio"
        );
        p.quick_next();
        assert_eq!(p.cursor(), 0, "wrap");
    }

    #[test]
    fn quick_getter_expone_la_query_viva() {
        let mut p = pane(&["a"]);
        assert!(p.quick().is_none());
        p.quick_start(crate::nav::Mode::Filter);
        p.quick_char('a');
        assert_eq!(p.quick().unwrap().mode(), crate::nav::Mode::Filter);
        p.quick_cancel();
        assert!(p.quick().is_none());
    }

    /// `extend` re-ordena TODO y re-ancla el cursor al PATH seleccionado.
    #[test]
    fn extend_reordena_y_reancla_por_path() {
        let mut first = vec![
            e("mem:///m", EntryKind::File),
            e("mem:///z", EntryKind::File),
        ];
        crate::sort_entries(&mut first);
        let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), first);
        p.set_cursor(1); // "z"
        p.extend(vec![
            e("mem:///a", EntryKind::File),
            e("mem:///b", EntryKind::File),
        ]);
        let orden: Vec<_> = p
            .entries()
            .iter()
            .map(|e| e.path.file_name().unwrap().as_bytes().to_vec())
            .collect();
        assert_eq!(
            orden,
            vec![b"a".to_vec(), b"b".to_vec(), b"m".to_vec(), b"z".to_vec()]
        );
        assert_eq!(
            p.selected().unwrap().path,
            VPath::parse("mem:///z").unwrap(),
            "la selección sigue el path pese al re-orden"
        );
    }

    /// #54: el merge incremental produce EXACTAMENTE el mismo orden que
    /// `sort_entries` sobre el total (dirs primero, NFC, empate por bytes) —
    /// incluidos NFD/NFC mezclados y no-UTF8.
    #[test]
    fn extend_merge_equivale_a_sort_completo() {
        let lotes: Vec<Vec<Entry>> = vec![
            vec![
                e("mem:///zeta", EntryKind::File),
                e("mem:///Adir", EntryKind::Dir),
            ],
            vec![e("mem:///an%CC%83o", EntryKind::File)], // NFD
            vec![
                e("mem:///a%C3%B1o2", EntryKind::File), // NFC
                e("mem:///%FF%FE", EntryKind::File),    // no-UTF8
            ],
            vec![e("mem:///Bdir", EntryKind::Dir)],
            // Sobrelargo (>255 bytes): el orden no tiene camino especial por
            // longitud, pero que quede pineado en la equivalencia.
            vec![e(&format!("mem:///{}", "x".repeat(300)), EntryKind::File)],
        ];
        let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), Vec::new());
        for lote in lotes.clone() {
            p.extend(lote);
        }
        let mut plano: Vec<Entry> = lotes.into_iter().flatten().collect();
        crate::sort_entries(&mut plano);
        assert_eq!(p.entries(), plano.as_slice(), "merge ≡ sort completo");
    }

    /// El contrato "ordénalas antes" deja de ser footgun: `set_listing`/`new`
    /// normalizan internamente (claves + orden) — un caller desordenado ya
    /// no rompe el invariante del merge.
    #[test]
    fn set_listing_normaliza_aunque_llegue_desordenado() {
        let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), Vec::new());
        p.set_listing(
            VPath::parse("mem:///d").unwrap(),
            vec![
                e("mem:///d/z", EntryKind::File),
                e("mem:///d/a", EntryKind::File),
            ],
        );
        assert_eq!(p.entries()[0].path, VPath::parse("mem:///d/a").unwrap());
        // Y el extend posterior sigue mergeando bien sobre esa base.
        p.extend(vec![e("mem:///d/m", EntryKind::File)]);
        let names: Vec<_> = p.entries().iter().map(|x| x.path.clone()).collect();
        assert_eq!(
            names,
            vec![
                VPath::parse("mem:///d/a").unwrap(),
                VPath::parse("mem:///d/m").unwrap(),
                VPath::parse("mem:///d/z").unwrap(),
            ]
        );
    }

    /// Empate de clave NFC entre lotes (misma forma normalizada, bytes
    /// crudos distintos: NFD en el lote 1 vs NFC en el lote 2) — el
    /// desempate lo decide `name_bytes` crudo, igual que `sort_entries`, NO
    /// el orden de llegada del merge (que solo desempata IZQUIERDA=empate
    /// exacto de clave, y aquí las claves NFC coinciden pero los bytes no).
    #[test]
    fn extend_desempata_por_bytes_crudos_igual_que_sort_completo() {
        let nfd = e("mem:///an%CC%83o", EntryKind::File); // "año" NFD
        let nfc = e("mem:///a%C3%B1o", EntryKind::File); // "año" NFC
        let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), Vec::new());
        p.extend(vec![nfd.clone()]);
        p.extend(vec![nfc.clone()]);
        let mut plano = vec![nfd, nfc];
        crate::sort_entries(&mut plano);
        assert_eq!(
            p.entries(),
            plano.as_slice(),
            "el empate de clave NFC entre lotes se resuelve igual que sort_entries"
        );
    }

    /// ADVERSARIAL A (mutación, review encoding #54): la NFC llega ANTES que
    /// la NFD — el orden de llegada CONTRADICE el desempate por bytes crudos
    /// (NFD `61 6E CC 83` < NFC `61 C3 B1`). Un merge sin `.then_with(bytes)`
    /// pasaría el test gemelo de arriba (allí llegada y bytes coinciden) pero
    /// muere aquí.
    #[test]
    fn adversarial_nfc_llega_antes_que_nfd() {
        let nfd = e("mem:///an%CC%83o", EntryKind::File);
        let nfc = e("mem:///a%C3%B1o", EntryKind::File);
        let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), Vec::new());
        p.extend(vec![nfc.clone()]);
        p.extend(vec![nfd.clone()]);
        let mut plano = vec![nfc, nfd];
        crate::sort_entries(&mut plano);
        assert_eq!(p.entries(), plano.as_slice());
        assert_eq!(
            p.entries()[0].path,
            VPath::parse("mem:///an%CC%83o").unwrap(),
            "NFD primero por bytes crudos, no por orden de llegada"
        );
    }

    /// ADVERSARIAL B (mutación, review encoding #54): inversión NFC↔bytes.
    /// NFD "ñu" = `6E CC 83 75`, "o" = `6F`: por clave NFC (`C3 B1 75`)
    /// ñu > o, pero por bytes crudos ñu < o. Un `cmp_keyed` que use los
    /// bytes como clave PRIMARIA (ignorando la NFC persistida) invierte el
    /// orden — spec §6.1 rota en macOS/NFD sin que el resto de la suite lo
    /// note. Cruza frontera de lote a propósito.
    #[test]
    fn adversarial_inversion_nfc_vs_bytes_entre_lotes() {
        let nfd_enye = e("mem:///n%CC%83u", EntryKind::File);
        let o = e("mem:///o", EntryKind::File);
        let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), Vec::new());
        p.extend(vec![nfd_enye.clone()]);
        p.extend(vec![o.clone()]);
        let mut plano = vec![nfd_enye, o];
        crate::sort_entries(&mut plano);
        assert_eq!(p.entries(), plano.as_slice());
        assert_eq!(
            p.entries()[0].path,
            VPath::parse("mem:///o").unwrap(),
            "'o' primero: la clave primaria es NFC, no los bytes crudos"
        );
    }

    #[test]
    fn extend_vacio_es_noop() {
        let mut p = pane(&["a"]);
        p.extend(vec![]);
        assert_eq!(p.entries().len(), 1);
        assert_eq!(p.cursor(), 0);
    }

    /// `extend` re-aplica el filtro vivo sobre el listado nuevo.
    #[test]
    fn extend_reaplica_el_filtro() {
        let mut p = pane(&["a1"]);
        p.quick_start(crate::nav::Mode::Filter);
        p.quick_char('a');
        p.extend(vec![
            e("mem:///a2", EntryKind::File),
            e("mem:///zz", EntryKind::File),
        ]);
        assert_eq!(p.quick_visible().unwrap().len(), 2, "a2 entra, zz no");
    }

    /// `refill` conserva el cursor por índice con clamp y re-aplica el filtro.
    #[test]
    fn refill_conserva_cursor_por_indice_con_clamp() {
        let mut p = pane(&["a", "b", "c"]);
        p.set_cursor(2); // "c"
        p.refill(vec![
            e("mem:///a", EntryKind::File),
            e("mem:///b", EntryKind::File),
        ]);
        assert_eq!(p.cursor(), 1, "clamp a la última entrada del listado nuevo");
        assert_eq!(p.entries().len(), 2);
    }

    /// #52: hydrate por path rellena size/mtime de la entrada viva; un path
    /// desconocido es no-op; el orden no cambia (size/mtime no ordenan).
    #[test]
    fn hydrate_rellena_sin_reordenar_y_es_noop_si_no_esta() {
        let mut entries = vec![
            e("mem:///a", EntryKind::File),
            e("mem:///b", EntryKind::File),
            e("mem:///c", EntryKind::File),
        ];
        entries[1].size = Some(999); // "b" ya venía hidratada
        crate::sort_entries(&mut entries);
        let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), entries);

        p.hydrate(&VPath::parse("mem:///a").unwrap(), Some(5), Some(1000));
        p.hydrate(&VPath::parse("mem:///b").unwrap(), Some(1), Some(2)); // or-semantics: no pisa
        p.hydrate(&VPath::parse("mem:///no-existe").unwrap(), Some(7), Some(7)); // no-op

        let orden: Vec<_> = p
            .entries()
            .iter()
            .map(|e| e.path.file_name().unwrap().as_bytes().to_vec())
            .collect();
        assert_eq!(
            orden,
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()],
            "hydrate no reordena"
        );

        let a = p
            .entries()
            .iter()
            .find(|e| e.path == VPath::parse("mem:///a").unwrap())
            .unwrap();
        assert_eq!(a.size, Some(5));
        assert_eq!(a.mtime_ms, Some(1000));

        let b = p
            .entries()
            .iter()
            .find(|e| e.path == VPath::parse("mem:///b").unwrap())
            .unwrap();
        assert_eq!(
            b.size,
            Some(999),
            "or-semantics: el valor previo se conserva"
        );

        let c = p
            .entries()
            .iter()
            .find(|e| e.path == VPath::parse("mem:///c").unwrap())
            .unwrap();
        assert_eq!(c.size, None, "sin hydrate para c, sigue None");
    }

    /// `refresh_quick` re-aplica el filtro sin tocar entries ni cursor real.
    #[test]
    fn refresh_quick_no_toca_entries_ni_cursor() {
        let mut p = pane(&["a1", "a2"]);
        p.quick_start(crate::nav::Mode::Filter);
        p.quick_char('a');
        let antes: Vec<_> = p.entries().to_vec();
        let cur = p.cursor();
        p.refresh_quick();
        assert_eq!(p.entries(), antes.as_slice());
        assert_eq!(p.cursor(), cur);
        assert_eq!(p.quick_visible().unwrap().len(), 2);
    }

    /// #98/F1 (fixture `cp866_papka` del corpus): el quick search casa
    /// contra el texto que el usuario VE. Con reinterpretación IBM866
    /// activa, teclear «п» encuentra la entrada pintada «Папка» — y el
    /// cache de folds se invalida en AMBOS caminos: quick vivo al ciclar
    /// (`set_name_encoding`) y quick arrancado después (`new` con enc).
    #[test]
    fn quick_search_casa_contra_el_texto_reinterpretado() {
        let papka = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "cp866_papka")
            .expect("fixture del corpus")
            .bytes;
        let dir = VPath::parse("mem:///").unwrap();
        let seg = norte_proto::Segment::new(papka).unwrap();
        let entries = vec![
            Entry {
                path: dir.join(seg),
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            },
            e("mem:///otro.txt", EntryKind::File),
        ];

        // Camino 1: quick VIVO, luego ciclar — el fold se re-pliega.
        let mut p = PaneState::new(dir.clone(), entries.clone());
        p.quick_start(Mode::Filter);
        p.quick_char('\u{043f}'); // п
        assert_eq!(
            p.quick_visible().map(<[usize]>::len),
            Some(0),
            "sin reinterpretar, п no casa contra el lossy"
        );
        // Cicla hasta IBM866 (la sugerencia con estas muestras).
        assert_eq!(p.cycle_name_encoding(), Some("IBM866"));
        assert_eq!(
            p.quick_visible().map(<[usize]>::len),
            Some(1),
            "con IBM866 el filtro casa contra «Папка»"
        );

        // Camino 2: ciclar primero, quick después (folds nacen con enc).
        let mut p = PaneState::new(dir, entries);
        assert_eq!(p.cycle_name_encoding(), Some("IBM866"));
        p.quick_start(Mode::Filter);
        p.quick_char('\u{043f}');
        assert_eq!(p.quick_visible().map(<[usize]>::len), Some(1));
    }
}
