//! Navegación TC (spec 2026-07-18): lógica PURA del quick search — sin
//! terminal, sin `App`. El match es UX de tipeo sobre el nombre lossy
//! normalizado a NFC y case-plegado (trampa macOS NFD, CLAUDE.md); la
//! IDENTIDAD de las entradas sigue siendo el `VPath` en bytes — operar usa
//! siempre `entries[índice_real]`.
//!
//! Compartido por los frontends (TUI y GUI): la mecánica del quick search no
//! cambia de naturaleza por el backend de render.

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

/// Nombre → clave de comparación: lossy del último segmento, NFC,
/// lowercase, y NFC OTRA VEZ.
///
/// La segunda NFC no es redundante: minusculizar puede sacar el resultado
/// de NFC cuando la precompuesta solo existe en minúscula (J+U+030C no
/// compone, pero su minúscula j+U+030C compone a ǰ U+01F0) — sin
/// re-normalizar, la aguja compuesta y el nombre descompuesto no casarían
/// (equivalencia canónica rota, review encoding MEDIA-1).
///
/// Solo equivalencia CANÓNICA (NFC): half-width katakana, ligaduras y demás
/// equivalencias de COMPATIBILIDAD (NFKC) quedan FUERA a sabiendas — «ﬁ» no
/// casa con «fi»; normalizarlas cambiaría de familia de equivalencia.
///
/// Coste: el fold por entrada se CACHEA en `QuickSearch::folds` (#77) —
/// un recompute completo por mutación del listado (`new` / `refresh`),
/// no por keystroke; los keystrokes (`push_char`/`backspace`)
/// solo pliegan la query. `to_lowercase` es case-folding simple de Rust,
/// NO full Unicode case-folding — consciente, suficiente para substring
/// UX de tipeo.
fn fold(name: &[u8]) -> String {
    String::from_utf8_lossy(name)
        .nfc()
        .flat_map(char::to_lowercase)
        .nfc()
        .collect()
}

/// [`fold`] con la reinterpretación de nombres del pane (#98/F1): un nombre
/// NO-UTF8 bajo `Some(enc)` se pliega sobre el texto DECODIFICADO
/// ([`decode_name`](norte_encoding::decode_name), la regla de
/// `display_name_with`) — teclear «п» encuentra la entrada que el pane pinta
/// «Папка». Sin reinterpretación (o nombre UTF-8 válido): el fold lossy de
/// siempre.
///
/// UN solo pipeline (F1 del audit): la rama enc DELEGA en [`fold`] — el
/// doble-NFC (caso J+U+030C) queda pineado para ambos caminos por la misma
/// fixture; duplicarlo aquí era un mutante irrematable hasta que el ciclo
/// gane un encoding con combinantes (windows-1258).
///
/// Divergencia CONSCIENTE con el texto pintado (F3 del audit): se pliega el
/// decodificado SIN enmascarar — un hazard enmascarado a `�` en pantalla no
/// casa tecleando `�` (misma asimetría pre-existente del camino lossy con
/// controles embebidos en UTF-8 válido). Los folds jamás se pintan.
fn fold_with(name: &[u8], enc: Option<norte_encoding::NameEncoding>) -> String {
    match (enc, std::str::from_utf8(name)) {
        (Some(e), Err(_)) => fold(norte_encoding::decode_name(name, e).as_bytes()),
        _ => fold(name),
    }
}

/// Folds precomputados de `entries` (índice-paralelo). Ver [`fold_with`].
fn fold_names(entries: &[Entry], enc: Option<norte_encoding::NameEncoding>) -> Vec<String> {
    entries
        .iter()
        .map(|e| fold_with(e.path.file_name().map_or(&b""[..], |s| s.as_bytes()), enc))
        .collect()
}

/// Matching sobre folds YA precomputados (camino caliente del keystroke).
fn matches_folded(query_folded: &str, folds: &[String]) -> Vec<usize> {
    folds
        .iter()
        .enumerate()
        .filter(|(_, f)| f.contains(query_folded))
        .map(|(i, _)| i)
        .collect()
}

/// Índices de `entries` cuyo nombre contiene `query` (misma normalización
/// en ambos lados, ver `fold`). `query` en bytes crudos (viene del input
/// tal cual).
///
/// Conveniencia SIN cache: pliega `entries` entero en cada llamada (en
/// streaming — pico de memoria de UN fold, no N). El estado cacheado
/// (camino caliente del keystroke) vive dentro de [`QuickSearch`] — esta
/// función es para el caller ocasional (tests, un solo cálculo puntual),
/// no para el bucle de tipeo. NO aplica la reinterpretación de nombres
/// (#57): para casar contra el texto reinterpretado usa [`QuickSearch`]
/// (que recibe el encoding del pane).
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
    /// Claves de comparación por entrada (índice-paralelas a `entries`),
    /// recomputadas UNA vez por mutación del listado (`new`/`refresh`), no
    /// por keystroke (#77).
    folds: Vec<String>,
    /// Índices REALES en `entries` que casan (query vacía = todos).
    visible: Vec<usize>,
    /// Posición de la selección DENTRO de `visible`.
    pos: usize,
    /// Reinterpretación de nombres vigente al plegar (#98/F1): los folds se
    /// computan sobre el texto que el usuario VE. Cambiarla exige re-plegar
    /// ([`QuickSearch::set_name_encoding`]).
    enc: Option<norte_encoding::NameEncoding>,
}

impl QuickSearch {
    /// Arranca un quick search vacío en el modo dado sobre `entries`: query
    /// vacía, `visible` se calcula ya mismo (query vacía = todo visible).
    /// `enc` = la reinterpretación de nombres del pane (#57), para que el
    /// filtro case contra el texto PINTADO.
    #[must_use]
    pub fn new(mode: Mode, entries: &[Entry], enc: Option<norte_encoding::NameEncoding>) -> Self {
        let mut q = Self {
            query: Vec::new(),
            mode,
            folds: fold_names(entries, enc),
            visible: Vec::new(),
            pos: 0,
            enc,
        };
        q.recompute();
        q
    }

    /// Cambia la reinterpretación de nombres y RE-PLIEGA el cache (#98/F1):
    /// el único otro punto de invalidación además de `new`/`refresh`.
    /// Mismo contrato de selección que [`QuickSearch::refresh`].
    pub fn set_name_encoding(
        &mut self,
        enc: Option<norte_encoding::NameEncoding>,
        entries: &[Entry],
        prev_selected: Option<&VPath>,
    ) {
        self.enc = enc;
        self.refresh(entries, prev_selected);
    }

    /// Recalcula `visible` a partir de la query actual sobre `self.folds`
    /// (el cache YA vigente — no toca `entries`).
    fn recompute(&mut self) {
        self.visible = if self.query.is_empty() {
            (0..self.folds.len()).collect()
        } else {
            matches_folded(&fold(&self.query), &self.folds)
        };
    }

    /// Añade un carácter tecleado a la query y recalcula. Solo pliega la
    /// query — el fold de las entradas ya está cacheado en `self.folds`.
    pub fn push_char(&mut self, c: char) {
        let mut buf = [0u8; 4];
        self.query
            .extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        self.recompute();
        self.pos = 0;
    }

    /// Retira el último byte tecleado (borra por char UTF-8 completo) y
    /// recalcula. Query vacía tras el borrado = todo visible.
    pub fn backspace(&mut self) {
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
        self.recompute();
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
    ///
    /// Además renueva el cache `folds` — junto a `new`, es el ÚNICO punto
    /// de invalidación (#77): los índices de `visible` refieren al
    /// `entries` del último `new`/`refresh`.
    pub fn refresh(&mut self, entries: &[Entry], prev_selected: Option<&VPath>) {
        self.folds = fold_names(entries, self.enc);
        self.recompute();
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
    /// forma parte de la identidad de ninguna entrada). Enmascarada con
    /// [`norte_encoding::is_terminal_hazard`] (review encoding BAJA): sin
    /// bracketed paste un IME/paste hostil llega como stream de `push_char`
    /// y pintaría bidi/invisibles crudos en el borde — el filtrado en sí
    /// (`matches`/`fold`) sigue operando sobre `self.query` SIN sanear, solo
    /// el texto que se pinta pasa por aquí.
    #[must_use]
    pub fn query_display(&self) -> String {
        String::from_utf8_lossy(&self.query)
            .chars()
            .map(|c| {
                if norte_encoding::is_terminal_hazard(c) {
                    '\u{FFFD}'
                } else {
                    c
                }
            })
            .collect()
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
    fn fold_re_normaliza_nfc_tras_el_lowercase() {
        // Encoding MEDIA-1: J + U+030C (combining caron) NO tiene forma
        // precompuesta MAYÚSCULA, pero su minúscula ǰ (U+01F0) SÍ existe.
        // Un fold que no re-normaliza NFC tras minusculizar deja
        // "j\u{030C}" (descompuesto) y la aguja "ǰ" (compuesta) no casa:
        // se pierde la equivalencia canónica.
        let entries = vec![e("mem:///J%CC%8C.txt")];
        assert_eq!(
            matches("ǰ".as_bytes(), &entries),
            vec![0],
            "la aguja precompuesta ǰ (U+01F0) casa con J+U+030C"
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
        let mut q = QuickSearch::new(Mode::Filter, &entries, None);
        q.push_char('a');
        assert_eq!(q.visible(), &[0, 2]);
        q.down();
        assert_eq!(q.selected_entry_index(), Some(2), "segundo match");
        q.backspace();
        assert_eq!(q.visible(), &[0, 1, 2], "query vacía = todo visible");
    }

    #[test]
    fn modo_salto_tab_con_wrap() {
        let entries = vec![e("mem:///ab"), e("mem:///zz"), e("mem:///ac")];
        let mut q = QuickSearch::new(Mode::Jump, &entries, None);
        q.push_char('a');
        assert_eq!(q.selected_entry_index(), Some(0));
        q.next_match();
        assert_eq!(q.selected_entry_index(), Some(2));
        q.next_match();
        assert_eq!(q.selected_entry_index(), Some(0), "wrap");
    }

    #[test]
    fn reaplicar_tras_lote_nuevo_conserva_seleccion_si_sobrevive() {
        let mut entries = vec![e("mem:///a1")];
        let mut q = QuickSearch::new(Mode::Filter, &entries, None);
        q.push_char('a');
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
        let mut q = QuickSearch::new(Mode::Filter, &entries, None);
        q.push_char('a');
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

    #[test]
    fn query_display_enmascara_hazards() {
        // review encoding BAJA: un RLO (U+202E) tecleado/pegado no debe salir
        // crudo en el eco `/{query}` — se empuja char a char, como llegaría de
        // un stream de input real (sin bracketed paste).
        let entries = vec![e("mem:///normal.txt")];
        let mut q = QuickSearch::new(Mode::Filter, &entries, None);
        for c in "a\u{202E}b".chars() {
            q.push_char(c);
        }
        let display = q.query_display();
        assert!(
            !display.chars().any(norte_encoding::is_terminal_hazard),
            "query_display dejó un hazard crudo: {display:?}"
        );
    }

    #[test]
    fn push_char_usa_los_folds_del_ultimo_refresh() {
        // El cache de folds (#77) debe renovarse en refresh: una entrada que
        // llega en un lote POSTERIOR tiene que casar con el siguiente keystroke.
        let mut entries = vec![e("mem:///zzz")];
        let mut q = QuickSearch::new(Mode::Filter, &entries, None);
        entries.push(e("mem:///nuevo.txt")); // lote del fill
        q.refresh(&entries, None);
        q.push_char('n');
        assert_eq!(
            q.visible(),
            &[1],
            "el fold de la entrada nueva está en el cache"
        );
    }

    #[test]
    fn el_cache_pliega_igual_que_matches_sobre_el_corpus_hostil() {
        // Pin anti-divergencia (review encoding #77): el camino cacheado
        // (new/refresh→push_char) y el sin cache (`matches`) deben dar
        // EXACTAMENTE lo mismo sobre el corpus hostil — un fast-path futuro
        // que optimice solo el cache pasaría el corpus (que entra por
        // `matches`) mientras rompe el bucle de tipeo real.
        let entries = vec![
            e("mem:///an%CC%83o.txt"), // NFD
            e("mem:///J%CC%8C.txt"),   // sin precompuesta mayúscula
            e("mem:///%FF%FE"),        // no-UTF8
        ];
        for needle in ["año", "ǰ", "\u{FFFD}"] {
            let mut q = QuickSearch::new(Mode::Filter, &entries, None);
            for c in needle.chars() {
                q.push_char(c);
            }
            assert_eq!(
                q.visible(),
                matches(needle.as_bytes(), &entries).as_slice(),
                "cache y camino directo divergen para {needle:?}"
            );
        }
    }
}
