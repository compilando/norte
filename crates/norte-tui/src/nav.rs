//! Navegación TC (spec 2026-07-18): lógica PURA del quick search — sin
//! terminal, sin `App`. El match es UX de tipeo sobre el nombre lossy
//! normalizado a NFC y case-plegado (trampa macOS NFD, CLAUDE.md); la
//! IDENTIDAD de las entradas sigue siendo el `VPath` en bytes — operar usa
//! siempre `entries[índice_real]`.

use norte_proto::{Entry, VPath};
use unicode_normalization::UnicodeNormalization;

/// Modo del quick search (`[ui] quick_search`, default filtro).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// El listado se reduce a los matches.
    #[default]
    Filter,
    /// El cursor salta entre matches; el listado no cambia.
    Jump,
}

/// Nombre → clave de comparación: lossy del último segmento, NFC, lowercase.
///
/// Coste: una `String` nueva por entrada y por keystroke (recompute
/// completo en cada char); cacheo diferido a #77 (misma zona que la sort
/// key de `extend_listing`, app.rs). `to_lowercase` es case-folding simple
/// de Rust, NO full Unicode case-folding — consciente, suficiente para
/// substring UX de tipeo.
fn fold(name: &[u8]) -> String {
    String::from_utf8_lossy(name)
        .nfc()
        .flat_map(char::to_lowercase)
        .collect()
}

/// Índices de `entries` cuyo nombre contiene `query` (misma normalización
/// en ambos lados, ver [`fold`]). `query` en bytes crudos (viene del input
/// tal cual).
#[must_use]
pub fn matches(query: &[u8], entries: &[Entry]) -> Vec<usize> {
    let q = fold(query);
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            let name = e.path.file_name().map_or(&b""[..], |s| s.as_bytes());
            fold(name).contains(&q)
        })
        .map(|(i, _)| i)
        .collect()
}

/// Estado vivo del quick search de UN pane.
#[derive(Debug)]
pub struct QuickSearch {
    query: Vec<u8>,
    mode: Mode,
    /// Índices REALES en `entries` que casan (query vacía = todos).
    visible: Vec<usize>,
    /// Posición de la selección DENTRO de `visible`.
    pos: usize,
}

impl QuickSearch {
    /// Arranca un quick search vacío en el modo dado sobre `entries`: query
    /// vacía, `visible` se calcula ya mismo (query vacía = todo visible).
    #[must_use]
    pub fn new(mode: Mode, entries: &[Entry]) -> Self {
        let mut q = Self {
            query: Vec::new(),
            mode,
            visible: Vec::new(),
            pos: 0,
        };
        q.recompute(entries);
        q
    }

    /// Recalcula `visible` a partir de la query actual sobre `entries`.
    fn recompute(&mut self, entries: &[Entry]) {
        self.visible = if self.query.is_empty() {
            (0..entries.len()).collect()
        } else {
            matches(&self.query, entries)
        };
    }

    /// Añade un carácter tecleado a la query y recalcula.
    pub fn push_char(&mut self, c: char, entries: &[Entry]) {
        let mut buf = [0u8; 4];
        self.query
            .extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        self.recompute(entries);
        self.pos = 0;
    }

    /// Retira el último byte tecleado (borra por char UTF-8 completo) y
    /// recalcula. Query vacía tras el borrado = todo visible.
    pub fn backspace(&mut self, entries: &[Entry]) {
        if self.query.is_empty() {
            return;
        }
        // Retrocede hasta el inicio del último char UTF-8 (o hasta el
        // último byte si la query no es UTF-8 válida — no debería pasar
        // porque solo se alimenta vía `push_char`, pero no panica igual).
        let mut cut = self.query.len() - 1;
        while cut > 0 && (self.query[cut] & 0b1100_0000) == 0b1000_0000 {
            cut -= 1;
        }
        self.query.truncate(cut);
        self.recompute(entries);
        self.pos = 0;
    }

    /// Recalcula `visible` sobre el `entries` YA mutado (lote nuevo del
    /// fill, o un re-sort completo — `Pane::extend_listing` re-sortea el
    /// listado entero en cada lote) conservando la selección por
    /// IDENTIDAD, no por índice: un índice recordado de ANTES del sort
    /// puede apuntar a otra entrada tras él.
    ///
    /// `prev_selected` es el `VPath` de la entrada seleccionada ANTES de
    /// la mutación — el caller lo captura vía
    /// `entries[selected_entry_index()?].path.clone()` antes de mutar
    /// `entries`. Se re-busca ese path dentro del nuevo `visible`; si
    /// murió (ya no casa / fue removido) o no había selección previa,
    /// clampa dentro del nuevo rango.
    pub fn refresh(&mut self, entries: &[Entry], prev_selected: Option<&VPath>) {
        self.recompute(entries);
        if let Some(prev) = prev_selected
            && let Some(new_pos) = self.visible.iter().position(|&i| entries[i].path == *prev)
        {
            self.pos = new_pos;
            return;
        }
        self.clamp_pos();
    }

    /// Clampa `pos` dentro de `[0, visible.len())`, sin panicar si está vacío.
    fn clamp_pos(&mut self) {
        if self.visible.is_empty() {
            self.pos = 0;
        } else if self.pos >= self.visible.len() {
            self.pos = self.visible.len() - 1;
        }
    }

    /// Mueve la selección una posición hacia abajo (clamp al final).
    pub fn down(&mut self) {
        if self.pos + 1 < self.visible.len() {
            self.pos += 1;
        }
    }

    /// Mueve la selección una posición hacia arriba (clamp al inicio).
    pub fn up(&mut self) {
        self.pos = self.pos.saturating_sub(1);
    }

    /// Modo salto: avanza al siguiente match con wrap; noop si no hay matches.
    pub fn next_match(&mut self) {
        if self.visible.is_empty() {
            return;
        }
        self.pos = (self.pos + 1) % self.visible.len();
    }

    /// Índices reales visibles (todos si la query está vacía).
    #[must_use]
    pub fn visible(&self) -> &[usize] {
        &self.visible
    }

    /// Índice real de la entrada seleccionada, si hay alguna visible.
    #[must_use]
    pub fn selected_entry_index(&self) -> Option<usize> {
        self.visible.get(self.pos).copied()
    }

    /// Query para pintar en pantalla (lossy — el usuario la tecleó, no
    /// forma parte de la identidad de ninguna entrada).
    #[must_use]
    pub fn query_display(&self) -> String {
        String::from_utf8_lossy(&self.query).into_owned()
    }

    /// Modo activo (Filter o Jump).
    #[must_use]
    pub fn mode(&self) -> Mode {
        self.mode
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::{Entry, EntryKind, VPath};

    // Entry NO deriva Default y `mtime_ms` es el nombre real del campo
    // (no `mtime`) — ver crates/norte-proto/src/entry.rs.
    fn e(wire: &str) -> Entry {
        Entry {
            path: VPath::parse(wire).expect("wire"),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
        }
    }

    #[test]
    fn filtro_substring_case_insensitive() {
        let entries = vec![
            e("mem:///Proyectos"),
            e("mem:///readme.md"),
            e("mem:///PROBE"),
        ];
        let m = matches(b"pro", &entries);
        assert_eq!(m, vec![0, 2], "Proyectos y PROBE casan; readme no");
    }

    #[test]
    fn filtro_nfc_casa_con_nfd() {
        // "año" en NFC como aguja; entrada con nombre en NFD (a + n + ̃ + o).
        let nfd = "an\u{0303}o.txt";
        let entries = vec![e(&format!("mem:///{nfd}"))];
        assert_eq!(
            matches("año".as_bytes(), &entries),
            vec![0],
            "NFD casa con aguja NFC"
        );
    }

    #[test]
    fn bytes_no_utf8_no_rompen_y_no_casan_en_falso() {
        let entries = vec![e("mem:///%FF%FE"), e("mem:///normal.txt")];
        assert_eq!(matches(b"norm", &entries), vec![1]);
        // La entrada hostil sigue filtrable por lo que su lossy muestra (�)
        // — contrato testeado, no solo "no panica".
        assert_eq!(matches("\u{FFFD}".as_bytes(), &entries), vec![0]);
    }

    #[test]
    fn estado_filtro_navega_y_confirma() {
        let entries = vec![e("mem:///a1"), e("mem:///b"), e("mem:///a2")];
        let mut q = QuickSearch::new(Mode::Filter, &entries);
        q.push_char('a', &entries);
        assert_eq!(q.visible(), &[0, 2]);
        q.down();
        assert_eq!(q.selected_entry_index(), Some(2), "segundo match");
        q.backspace(&entries);
        assert_eq!(q.visible(), &[0, 1, 2], "query vacía = todo visible");
    }

    #[test]
    fn modo_salto_tab_con_wrap() {
        let entries = vec![e("mem:///ab"), e("mem:///zz"), e("mem:///ac")];
        let mut q = QuickSearch::new(Mode::Jump, &entries);
        q.push_char('a', &entries);
        assert_eq!(q.selected_entry_index(), Some(0));
        q.next_match();
        assert_eq!(q.selected_entry_index(), Some(2));
        q.next_match();
        assert_eq!(q.selected_entry_index(), Some(0), "wrap");
    }

    #[test]
    fn reaplicar_tras_lote_nuevo_conserva_seleccion_si_sobrevive() {
        let mut entries = vec![e("mem:///a1")];
        let mut q = QuickSearch::new(Mode::Filter, &entries);
        q.push_char('a', &entries);
        let prev = q.selected_entry_index().map(|i| entries[i].path.clone());
        entries.push(e("mem:///a2")); // llega un lote del fill
        q.refresh(&entries, prev.as_ref());
        assert_eq!(q.visible(), &[0, 1]);
        assert_eq!(q.selected_entry_index(), Some(0), "la selección no salta");
    }

    #[test]
    fn refresh_sobrevive_a_un_resort() {
        // review MAJOR: la selección se conserva por IDENTIDAD (VPath), no
        // por índice — `Pane::extend_listing` re-sortea el listado completo
        // en cada lote (app.rs), así que un índice recordado apunta a OTRA
        // entrada tras el sort.
        let mut entries = vec![e("mem:///a1"), e("mem:///a2")];
        let mut q = QuickSearch::new(Mode::Filter, &entries);
        q.push_char('a', &entries);
        q.down(); // selecciona a2 (índice real 1)
        assert_eq!(q.selected_entry_index(), Some(1));
        let prev = q.selected_entry_index().map(|i| entries[i].path.clone());

        // El lote re-sortea: a2 pasa a índice real 0, a1 a índice real 1.
        entries.swap(0, 1);
        q.refresh(&entries, prev.as_ref());

        assert_eq!(
            q.selected_entry_index().map(|i| entries[i].path.clone()),
            prev,
            "la selección sigue en el MISMO path tras el resort, no en el mismo índice"
        );
    }
}
