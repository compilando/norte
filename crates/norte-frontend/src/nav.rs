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
///
/// `pub` (H1 T4): la palette de comandos de la TUI pliega texto que NO es
/// un nombre de `Entry` (comando+descripción) con el MISMO criterio de
/// normalización que el quick search — un solo pipeline de fold para todo
/// filtro substring del frontend, jamás una copia divergente.
#[must_use]
pub fn fold(name: &[u8]) -> String {
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
///
/// `pub` para que el marcado por patrón (#103) pliegue EXACTAMENTE igual que
/// el quick search — un solo pipeline, jamás una copia divergente, y el
/// claim de diseño ("un solo fold compartido") queda enlazable desde fuera
/// del crate en vez de solo prometido en prosa.
#[must_use]
pub fn fold_with(name: &[u8], enc: Option<norte_encoding::NameEncoding>) -> String {
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
            attrs: std::collections::BTreeMap::new(),
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

/// El historial de directorios de UN pane, y el rastro que lo recorre.
///
/// Vivía en `norte-tui`. No tenía nada de terminal: es la misma pregunta
/// —¿de dónde vengo y a dónde vuelvo?— para cualquier superficie que
/// navegue, y una segunda copia en el host gráfico habría sido exactamente
/// la clase de divergencia que este crate existe para evitar (ADR 0066, D14).
use std::collections::VecDeque;

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

/// Si una navegación se REGISTRA en el rastro del pane, o es el rastro
/// reproduciéndose a sí mismo.
///
/// Sin esta distinción `nav.back` se alimenta de su propio rastro: volver de
/// B a A registraría "estuve en B", así que el siguiente back devuelve a B y
/// el lector oscila entre dos directorios — el defecto exacto que el rastro
/// existe para evitar, un nivel más arriba.
///
/// Vive aquí, y no junto al `cd` de un frontend, porque el modal TOFU de un
/// frontend lo TRANSPORTA: el reintento tras confiar en la host key debe reanudar la
/// MISMA navegación que el TOFU interrumpió, y la lib no puede referirse a
/// un tipo declarado en `main.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trail {
    /// El usuario pidió este movimiento: entra en la MRU y en el rastro, y
    /// poda la rama de forward.
    Record,
    /// `nav.back`/`nav.forward` están reproduciendo, y ESTE es el paso que
    /// están dando. El rastro ya lo sabe, así que la navegación no se
    /// registra; el paso viaja dentro porque un `Replay` sin saber en qué
    /// sentido va no se puede deshacer, y quien tenga que rebobinarlo puede
    /// no ser quien lo empezó: el TOFU suspende la navegación y la respuesta
    /// al modal la termina, minutos después y desde otro sitio del código.
    ///
    /// Va DENTRO de la variante, y no en un campo aparte junto a ella, para
    /// que «registrar» y «tener sentido» no puedan contradecirse: un
    /// `Record` con sentido, o un `Replay` sin él, serían estados que alguien
    /// tendría que acordarse de no construir.
    Replay(TrailStep),
}

impl Trail {
    /// El paso del rastro que esta navegación está dando, si es que está
    /// dando alguno. `None` para un [`Trail::Record`]: no salió del rastro,
    /// así que no hay nada que rebobinar si acaba mal.
    #[must_use]
    pub fn step(self) -> Option<TrailStep> {
        match self {
            Self::Record => None,
            Self::Replay(step) => Some(step),
        }
    }
}

/// Which way `nav.back`/`nav.forward` are walking the trail. The two are the
/// same operation mirrored, so they share one body rather than two arms that
/// must be kept in step by hand.
///
/// Vive aquí por el mismo motivo que [`Trail`], que lo transporta: el modal
/// TOFU (el modal TOFU de un frontend) suspende una navegación que puede ser un
/// paso del rastro, y quien responda al modal necesita saber en qué sentido
/// iba para deshacerlo si la respuesta acaba abandonándola.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrailStep {
    /// `nav.back`.
    Back,
    /// `nav.forward`.
    Forward,
}

impl TrailStep {
    /// Fluent id for "there is nothing this way". A key that goes silent is
    /// indistinguishable from a broken one, so the exhausted trail SAYS so.
    #[must_use]
    pub fn empty_message(self) -> &'static str {
        match self {
            Self::Back => "msg-nav-no-back",
            Self::Forward => "msg-nav-no-forward",
        }
    }
}

#[cfg(test)]
mod history_tests {
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
