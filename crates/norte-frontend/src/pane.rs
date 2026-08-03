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

use crate::decoration::Decoration;
use crate::nav::{Mode, QuickSearch};
use crate::sort::SortKey;
use globset::GlobBuilder;
use norte_proto::{Entry, EntryKind, VPath};
use std::collections::{HashMap, HashSet};

/// Why a mark-by-pattern was rejected (hard rule 6: typed library errors).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PatternError {
    /// The glob does not compile. Carries the `globset` diagnostic, which
    /// EMBEDS the user's pattern verbatim — a frontend MUST mask it before
    /// painting it (`display_name`), exactly as it masks a file name: a
    /// pattern arrives by paste as easily as by typing, and can carry bidi
    /// overrides or invisibles. It quotes the FOLDED pattern
    /// ([`PaneState::mark_glob`] lowercases and NFC-normalises via
    /// `nav::fold` before compiling), not what the user typed — `ABC[`
    /// reports `'abc['`, text the user never typed.
    #[error("{0}")]
    Glob(String),
}

/// Recompila el regex byte-mode de un [`globset::Glob`] en modo Unicode
/// (#110): globset compila con `(?-u)`, donde `?` consume UN BYTE y una
/// clase casa byte a byte — `a?o` no casaba `año` (ñ = 2 bytes) y `a[ñx]o`
/// casaba `axo` pero JAMÁS `año`, marcando en silencio otro fichero que el
/// nombrado. globset sigue siendo la ÚNICA autoridad de sintaxis (misma
/// lib que `fs.search`): esto solo traduce su salida.
///
/// La traducción decodifica los runs de escapes `\xNN` con NN ≥ 0x80 —
/// la ÚNICA forma en que globset emite los bytes no-ASCII del patrón
/// (`&str`, así que los runs son SIEMPRE UTF-8 completo) — de vuelta a sus
/// caracteres, que nunca son metacaracteres de regex y van literales tanto
/// dentro como fuera de una clase. Un rango de clase con extremos
/// multibyte (`[ñ-ü]` → `[\xc3\xb1-\xc3\xbc]`) también cae bien: el `-`
/// ASCII corta el run y cada extremo decodifica a su char. `(?-u)` se pela
/// del prefijo; el resto de flags pasa tal cual.
///
/// COPIA deliberada del traductor de `norte-core::search` (mismo criterio
/// que el fold, duplicado core/frontend): no hay crate común por debajo de
/// ambos donde quepa sin arrastrar `globset`+`regex` a un crate ajeno.
/// Cada copia pinea la forma de globset con su propio test guardia.
///
/// # Errors
/// [`PatternError::Glob`] si un run decodificado no es UTF-8 válido — no
/// debería ocurrir con la globset pineada (test de guardia
/// `globset_regex_shape_is_the_one_this_translation_expects`); fail-loud
/// antes que casar bytes que el usuario no escribió.
fn unicode_glob_regex(glob: &globset::Glob) -> Result<String, PatternError> {
    let src = glob.regex();
    let stripped = src.strip_prefix("(?-u)").unwrap_or(src);
    let mut out = String::with_capacity(stripped.len());
    let mut run: Vec<u8> = Vec::new();
    let flush = |run: &mut Vec<u8>, out: &mut String| -> Result<(), PatternError> {
        if run.is_empty() {
            return Ok(());
        }
        let decoded = std::str::from_utf8(run).map_err(|_| {
            PatternError::Glob(
                "internal: the glob compiled to byte escapes that do not \
                 form UTF-8 characters"
                    .to_owned(),
            )
        })?;
        out.push_str(decoded);
        run.clear();
        Ok(())
    };
    let bytes = stripped.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // `stripped.get(..)` y no un slice directo: si un `\x` precediera a
        // un char multibyte, el rango i+2..i+4 partiría el char y un slice
        // directo PANICARÍA — inalcanzable con la globset pineada, pero
        // esta función falla por Result, no por panic.
        if bytes[i] == b'\\'
            && bytes.get(i + 1) == Some(&b'x')
            && let Some(hex) = stripped.get(i + 2..i + 4)
            && let Ok(b) = u8::from_str_radix(hex, 16)
            && b >= 0x80
        {
            run.push(b);
            i += 4;
            continue;
        }
        flush(&mut run, &mut out)?;
        // Copia el resto tal cual — incluidos escapes ASCII (`\.`), cuyo
        // significado es idéntico en modo Unicode.
        let step = if bytes[i] == b'\\' && i + 1 < bytes.len() {
            1 + stripped[i + 1..].chars().next().map_or(0, char::len_utf8)
        } else {
            stripped[i..].chars().next().map_or(1, char::len_utf8)
        };
        out.push_str(&stripped[i..i + step]);
        i += step;
    }
    flush(&mut run, &mut out)?;
    Ok(out)
}

/// ¿Entrada oculta? (#107): decisión por BYTES del ÚLTIMO segmento — la
/// regla 1 manda — con el criterio unix del `.` (0x2E) inicial. `a.txt` no
/// lo es; un nombre no-UTF8 que empieza por 0x2E sí. El atributo hidden de
/// Windows llegará por los attrs del wire (proto 0.30) cuando algún
/// provider lo anuncie.
fn is_hidden_entry(e: &Entry) -> bool {
    e.path
        .file_name()
        .is_some_and(|n| n.as_bytes().first() == Some(&b'.'))
}

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
    /// Marks dropped by the last [`Self::refill`] because their entry was gone
    /// (#103). The frontends surface it: a selection that shrinks behind the
    /// user's back must never be silent, because [`Self::marked_paths`] falls
    /// back to the CURSOR entry once the set empties — a silent prune would
    /// retarget the next bulk operation onto something nobody marked.
    pruned_marks: usize,
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
    /// Orden elegido del listado (#108 L7). Default = name/asc/dirs-first
    /// (el orden histórico). Cambia por [`PaneState::set_sort`], que
    /// re-ordena en sitio re-anclando el cursor por path.
    sort: crate::sort::SortSpec,
    /// Mostrar entradas ocultas (#107). `true` por defecto (el constructor
    /// no sabe de config; el frontend fija el default de `[ui] show_hidden`
    /// con [`Self::set_show_hidden`] tras construir). SOLO presentación
    /// (regla 7 al revés: la decisión vive aquí, compartida, y el provider
    /// sigue listando todo).
    show_hidden: bool,
    /// Entradas apartadas por la ocultación (#107): las de último segmento
    /// con `.` inicial cuando `show_hidden == false`. Se devuelven al
    /// listado (merge ordenado) al volver a mostrar — apartar, no tirar,
    /// para que el toggle no necesite re-listar. Vacío con `show_hidden`.
    hidden_stash: Vec<Entry>,
    /// Decoraciones de plugin por entrada (G3b, ADR 0037), YA saneadas
    /// ([`crate::decoration::sanitize_decoration`]): badge/rol de la
    /// entrada, si algún decorator consentido decoró esta ruta. Se llena de
    /// forma ASÍNCRONA tras el listado (nunca bloquea `set_listing`, ver el
    /// caller en cada frontend) y por eso vive FUERA del reset de
    /// `set_listing`/`begin_loading` normal — [`Self::set_listing`] y
    /// [`Self::begin_loading`] SÍ la limpian (un listado nuevo invalida las
    /// decoraciones del anterior; llegan tarde, no en silencio hasta
    /// entonces) mediante [`Self::clear_decorations`].
    decorations: HashMap<VPath, Decoration>,
    /// Valores de columnas `plugin:` por entrada (#117-follow-up), espejo
    /// asíncrono de `decorations`: clave exterior = id Display de la
    /// columna (`plugin:<p>/<c>`), interior = `VPath` del listado ACTUAL →
    /// valor YA saneado en el ingest ([`crate::columns::sanitize_cell`]).
    /// [`Self::set_listing`]/[`Self::begin_loading`] lo limpian (un
    /// listado nuevo invalida los valores del anterior).
    plugin_columns: HashMap<String, HashMap<VPath, String>>,
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
        let (entries, sort_keys) =
            crate::sort::sort_with_keys_spec(entries, crate::sort::SortSpec::default());
        Self {
            dir,
            entries,
            sort_keys,
            cursor: 0,
            loading: false,
            quick: None,
            marks: HashSet::new(),
            pruned_marks: 0,
            name_encoding: None,
            name_encoding_entry: 0,
            skipped: None,
            cursor_memory: Vec::new(),
            pending_focus: None,
            show_hidden: true,
            hidden_stash: Vec::new(),
            sort: crate::sort::SortSpec::default(),
            decorations: HashMap::new(),
            plugin_columns: HashMap::new(),
        }
    }

    /// ¿Se muestran las entradas ocultas? (#107)
    #[must_use]
    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    /// Cuántas entradas del listado actual están APARTADAS por la
    /// ocultación (#107). 0 con [`Self::show_hidden`] activo. El pie del
    /// pane lo pinta con la misma disciplina que `skipped`: un listado que
    /// enseña menos de lo que hay jamás es silencioso.
    #[must_use]
    pub fn hidden_count(&self) -> usize {
        self.hidden_stash.len()
    }

    /// Fija la visibilidad de ocultos (#107). Mostrar devuelve el stash al
    /// listado por el MISMO camino que un lote paginado ([`Self::extend`]:
    /// merge ordenado, cursor re-anclado por path, quick re-aplicado).
    /// Ocultar aparta los dotfiles, re-ancla el cursor por path (clamp si
    /// estaba sobre uno) y PODA sus marcas con el contador de
    /// [`Self::pruned_marks`] — la misma regla que `refill`: una selección
    /// que alimenta un bulk op jamás encoge en silencio.
    pub fn set_show_hidden(&mut self, show: bool) {
        if show == self.show_hidden {
            return;
        }
        self.show_hidden = show;
        if show {
            let stash = std::mem::take(&mut self.hidden_stash);
            self.extend(stash);
            return;
        }
        let anchor = self.entries.get(self.cursor).map(|e| e.path.clone());
        let quick_prev = self.quick_selected_path();
        let mut kept = Vec::with_capacity(self.entries.len());
        let mut kept_keys = Vec::with_capacity(self.sort_keys.len());
        // Partición manteniendo `sort_keys` índice-paralela (#54): un
        // retain solo sobre `entries` las desalinearía.
        for (entry, key) in std::mem::take(&mut self.entries)
            .into_iter()
            .zip(std::mem::take(&mut self.sort_keys))
        {
            if is_hidden_entry(&entry) {
                self.hidden_stash.push(entry);
            } else {
                kept.push(entry);
                kept_keys.push(key);
            }
        }
        self.entries = kept;
        self.sort_keys = kept_keys;
        self.cursor = anchor
            .and_then(|p| self.entries.iter().position(|e| e.path == p))
            .unwrap_or_else(|| self.cursor.min(self.entries.len().saturating_sub(1)));
        self.pruned_marks = self.prune_marks();
        if let Some(q) = &mut self.quick {
            q.refresh(&self.entries, quick_prev.as_ref());
        }
        self.quick_sync_jump();
    }

    /// El orden activo del listado (#108).
    #[must_use]
    pub fn sort(&self) -> crate::sort::SortSpec {
        self.sort
    }

    /// Cambia el orden del listado (#108 L7): re-ordena EN SITIO (claves
    /// #54 conservadas — solo cambia el comparador), re-ancla el cursor al
    /// PATH seleccionado y re-aplica el quick vivo. Las marcas no se tocan
    /// (van por identidad). No-op si el spec no cambia.
    pub fn set_sort(&mut self, spec: crate::sort::SortSpec) {
        if spec == self.sort {
            return;
        }
        self.sort = spec;
        let anchor = self.entries.get(self.cursor).map(|e| e.path.clone());
        let quick_prev = self.quick_selected_path();
        // Mismo guard anti-truncado que merge_keyed_spec (review m1): un
        // zip de paralelas desincronizadas PERDERÍA entradas en silencio.
        debug_assert_eq!(
            self.entries.len(),
            self.sort_keys.len(),
            "entries↔sort_keys desincronizados"
        );
        let mut pares: Vec<(Entry, crate::sort::SortKey)> = std::mem::take(&mut self.entries)
            .into_iter()
            .zip(std::mem::take(&mut self.sort_keys))
            .collect();
        pares.sort_by(|a, b| crate::sort::cmp_keyed_with((&a.1, &a.0), (&b.1, &b.0), self.sort));
        (self.entries, self.sort_keys) = pares.into_iter().unzip();
        self.cursor = anchor
            .and_then(|p| self.entries.iter().position(|e| e.path == p))
            .unwrap_or_else(|| self.cursor.min(self.entries.len().saturating_sub(1)));
        if let Some(q) = &mut self.quick {
            q.refresh(&self.entries, quick_prev.as_ref());
        }
        self.quick_sync_jump();
    }

    /// Toggle de [`Self::set_show_hidden`]; devuelve el estado nuevo.
    pub fn toggle_hidden(&mut self) -> bool {
        self.set_show_hidden(!self.show_hidden);
        self.show_hidden
    }

    /// Aparta de `entries` las ocultas hacia el stash si la ocultación está
    /// activa (#107); passthrough si no. Para los puntos de INGESTIÓN
    /// ([`Self::set_listing`], [`Self::extend`], [`Self::refill`]).
    fn stash_hidden(&mut self, entries: Vec<Entry>) -> Vec<Entry> {
        if self.show_hidden {
            return entries;
        }
        let (hidden, visible): (Vec<Entry>, Vec<Entry>) =
            entries.into_iter().partition(is_hidden_entry);
        self.hidden_stash.extend(hidden);
        visible
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
        // #107: stash del listado ANTERIOR fuera; el nuevo se filtra al
        // entrar si la ocultación está activa.
        self.hidden_stash.clear();
        let entries = self.stash_hidden(entries);
        let (entries, sort_keys) = crate::sort::sort_with_keys_spec(entries, self.sort);
        self.dir = dir;
        self.entries = entries;
        self.sort_keys = sort_keys;
        self.cursor = 0;
        self.loading = false;
        self.quick = None;
        self.marks.clear();
        self.pruned_marks = 0;
        // #96: las omitidas eran del listado ANTERIOR; el caller fija las
        // frescas con `set_skipped` si su fuente las trae.
        self.skipped = None;
        // G3b: las decoraciones eran del listado ANTERIOR (claves por
        // `VPath` byte-exacto de OTRO dir) — un listado nuevo las invalida.
        self.decorations.clear();
        self.plugin_columns.clear();

        // #107 review MINOR-1 (aceptado): el hint se resuelve contra el
        // listado YA filtrado — volver del interior de un dir oculto con la
        // ocultación activa pierde el foco (cae a memoria/0). Corregirlo
        // exigiría buscar en el stash y elegir un vecino visible; coste no
        // pagado hasta que moleste de verdad.
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
        self.pruned_marks = 0;
        self.skipped = None;
        self.hidden_stash.clear(); // #107: era del listado anterior
        self.decorations.clear();
        self.plugin_columns.clear();
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

    /// Decoración de plugin de `path` (G3b), ya saneada — `None` si ningún
    /// decorator consentido decoró esa ruta, o si las decoraciones de esta
    /// página no han llegado todavía (fetch asíncrono en curso).
    #[must_use]
    pub fn decoration_for(&self, path: &VPath) -> Option<&Decoration> {
        self.decorations.get(path)
    }

    /// Instala el LOTE de decoraciones ya resuelto y saneado (G3b): el
    /// caller lo llama tras un `Backend::plugin_decorate` que responde para
    /// EL MISMO listado que sigue activo (ver [`crate::merge_decorations`]
    /// para construir el mapa desde el wire) — llamar con decoraciones de
    /// un `dir` que ya no es el actual es un no-op observable inofensivo
    /// (las claves por `VPath` de otro dir simplemente no casan ninguna
    /// entrada visible), pero el caller debería descartar una respuesta
    /// tardía cuyo `dir` no case el actual ANTES de llamar (ver el sitio de
    /// la llamada en cada frontend).
    pub fn set_decorations(&mut self, decorations: HashMap<VPath, Decoration>) {
        self.decorations = decorations;
    }

    /// Limpia las decoraciones (G3b): llamado por [`Self::set_listing`]/
    /// [`Self::begin_loading`] — expuesto también para que un caller pueda
    /// forzar el reset (p. ej. al desactivar todos los decoradores).
    pub fn clear_decorations(&mut self) {
        self.decorations.clear();
    }

    /// Instala el LOTE de valores de columnas `plugin:` (#117-follow-up):
    /// clave exterior = id Display (`plugin:<p>/<c>`), interior = `VPath`
    /// del listado activo → valor saneado
    /// ([`crate::columns::sanitize_column_values`] en el ingest). Mismo
    /// contrato anti-rancio que [`Self::set_decorations`]: el caller
    /// descarta una respuesta tardía cuyo `dir` no case el actual.
    pub fn set_plugin_columns(&mut self, columns: HashMap<String, HashMap<VPath, String>>) {
        self.plugin_columns = columns;
    }

    /// Celda de la columna `plugin:` `display_id` para `path`
    /// (#117-follow-up): `None` = sin valor (blanco, jamás fabricado). El
    /// valor se RE-enmascara defensivamente al servirlo (doctrina P1: bidi
    /// sin mascarar en ratatui DESAPARECE en silencio — los consumidores no
    /// confían en que el ingest ya saneara).
    #[must_use]
    pub fn plugin_cell(&self, display_id: &str, path: &VPath) -> Option<String> {
        let v = self.plugin_columns.get(display_id)?.get(path)?;
        crate::columns::sanitize_cell(Some(v))
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

    /// mc/Total Commander sweep (`insert`, #103): toggle-mark the VISIBLE
    /// selection, then advance to the next visible row — holding the key
    /// selects a range. With a [`Mode::Filter`] quick search active, "next"
    /// means the next VISIBLE row within the filter ([`Self::quick_down`],
    /// which does not wrap); the real cursor is left untouched, exactly as
    /// [`Self::toggle_mark`] itself only ever acts on the filtered
    /// selection. Without an active filter (or in [`Mode::Jump`], where
    /// [`Self::selected`] already reads the real cursor), it advances the
    /// real cursor ([`Self::page_down`], which clamps). Either way, at the
    /// last visible row this marks WITHOUT wrapping back to the top.
    pub fn toggle_mark_and_advance(&mut self) {
        self.toggle_mark();
        let filtering = self
            .quick
            .as_ref()
            .is_some_and(|q| q.mode() == Mode::Filter);
        if filtering {
            self.quick_down();
        } else {
            self.page_down(1);
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

    /// The indices a BULK mark acts on: the VISIBLE subset under an active
    /// quick filter, the whole listing otherwise — what you see is what you
    /// mark. While a fill is running ([`Self::loading`]) it reaches only what
    /// has been drained so far; the pane already marks an in-progress listing
    /// (the title in the TUI, a status line in the GUI), so the partial reach
    /// is never silent.
    fn markable_indices(&self) -> Vec<usize> {
        match self.quick_visible() {
            Some(vis) => vis.to_vec(),
            None => (0..self.entries.len()).collect(),
        }
    }

    /// Marks every entry of the visible set (see `markable_indices`).
    pub fn mark_all(&mut self) {
        for i in self.markable_indices() {
            let Some(path) = self.entries.get(i).map(|e| &e.path) else {
                continue;
            };
            if !self.marks.contains(path) {
                self.marks.insert(path.clone());
            }
        }
    }

    /// Flips the mark of every entry of the visible set (see
    /// `markable_indices`). Marks OUTSIDE that set SURVIVE untouched:
    /// invert is "flip what you see", not "replace the selection with its
    /// complement" — under a filter, [`Self::marked_paths`] can therefore
    /// still return entries the user is not looking at.
    pub fn invert_marks(&mut self) {
        for i in self.markable_indices() {
            let Some(path) = self.entries.get(i).map(|e| e.path.clone()) else {
                continue;
            };
            if !self.marks.remove(&path) {
                self.marks.insert(path);
            }
        }
    }

    /// Marks (`mark = true`) or unmarks (`false`) the visible entries whose
    /// name matches `pattern`, a glob. Returns how many marks it ADDED or
    /// REMOVED, never the resulting total — a pattern that only re-marks
    /// what was already marked returns 0 even though the selection is
    /// non-empty; read [`Self::marks_len`] for the total.
    ///
    /// Matching folds BOTH sides through the quick-search pipeline
    /// ([`nav::fold_with`](crate::nav::fold_with): lossy UTF-8 → NFC →
    /// lowercase → NFC, honouring the pane's name reinterpretation) before
    /// compiling the glob, so a pattern matches the FOLDED name, not the
    /// text the pane paints: [`crate::display_name_with`] additionally
    /// MASKS bidi overrides and invisibles to U+FFFD, which the fold does
    /// not — a name typed exactly as painted only matches if it is already
    /// NFC, lowercase, and free of masked characters. Folding the pattern
    /// is what makes NFD and uppercase input match: the fold is the ONE
    /// definition of name equality, shared with the quick search. The glob
    /// deliberately does NOT add `case_insensitive` on top — regex-crate
    /// case folding is wider than the fold (`s` would match `ſ` U+017F)
    /// and would mark files the quick search considers distinct.
    ///
    /// A non-UTF-8 name's invalid bytes fold to U+FFFD and cannot be named
    /// INDIVIDUALLY — but typing U+FFFD in the pattern names ALL of them at
    /// once, matching every hostile name whose lossy form collapses there.
    /// [`Self::toggle_mark`] always reaches an entry by hand regardless, and
    /// [`Self::marked_paths`] returns each mark's original bytes untouched
    /// (hard rule 1).
    ///
    /// `?` and a character class (`[...]`) count CHARACTERS (#110): the
    /// glob's byte-mode regex is recompiled in Unicode mode
    /// (`unicode_glob_regex`), so `a?o` matches `año` even though `ñ` is
    /// two bytes. Unmarking down to an empty set re-arms
    /// [`Self::marked_paths`]'s cursor fallback (it returns the entry under
    /// the cursor when no marks remain) — a caller must read the count this
    /// method returns rather than assume the mark set still reflects what
    /// the user last saw.
    ///
    /// # Errors
    /// [`PatternError::Glob`] if the pattern does not compile. Nothing is
    /// marked in that case.
    pub fn mark_glob(&mut self, pattern: &str, mark: bool) -> Result<usize, PatternError> {
        // El patrón se pliega con el MISMO pipeline que el nombre (#103): el
        // fold es Unicode, `case_insensitive` de globset es solo-ASCII
        // (emite `(?-u)`), así que sin plegar la aguja un patrón NFD o una
        // mayúscula no-ASCII no casarían NADA en silencio.
        let folded = crate::nav::fold(pattern.as_bytes());
        // SIN `case_insensitive`: el fold ya minusculiza AMBOS lados, y el
        // `(?i)` del regex Unicode es case-folding MÁS ANCHO que el fold
        // (`s` casaría `ſ` U+017F, `μ` casaría `µ` U+00B5) — marcaría
        // ficheros que el quick search considera distintos. UNA sola
        // definición de igualdad: la del fold (audit #110).
        let glob = GlobBuilder::new(&folded)
            .backslash_escape(true) // si no, la semántica de `\` depende del SO (globset la
            // hace depender de `is_separator('\\')`, true en unix, false en
            // windows) — `\` es un byte de nombre legal en Linux (corpus
            // `win_backslash`) y el patrón debe casarlo igual en las dos.
            .build()
            .map_err(|e| PatternError::Glob(e.to_string()))?;
        // Modo Unicode (#110): `?`/clases cuentan CARACTERES, no bytes.
        // `size_limit` porque esto es API pública sin tope propio (el modal
        // de la TUI acota a 256 chars, pero nada obliga a otros callers);
        // el motor de `regex` es lineal, así que el guard es de memoria del
        // programa compilado, no de backtracking. `dot_matches_new_line`:
        // globset compila su matcher con ese flag y `*`/`?` traducen a
        // `.`-derivados — sin él, un nombre con `\n` (byte legal en unix,
        // corpus `control_newline`) dejaría de casar `*` EN SILENCIO.
        let matcher = regex::RegexBuilder::new(&unicode_glob_regex(&glob)?)
            .size_limit(1 << 20)
            .dot_matches_new_line(true)
            .build()
            .map_err(|e| PatternError::Glob(e.to_string()))?;
        let enc = self.name_encoding;
        let mut changed = 0usize;
        for i in self.markable_indices() {
            let Some(entry) = self.entries.get(i) else {
                continue;
            };
            let name = entry.path.file_name().map_or(&b""[..], |n| n.as_bytes());
            if !matcher.is_match(crate::nav::fold_with(name, enc).as_str()) {
                continue;
            }
            let path = entry.path.clone();
            let hit = if mark {
                self.marks.insert(path)
            } else {
                self.marks.remove(&path)
            };
            if hit {
                changed += 1;
            }
        }
        Ok(changed)
    }

    /// Total size of every marked entry that is NOT a directory, saturating.
    /// A symlink contributes its own size, never its target's. Directories
    /// contribute 0: nothing here walks a tree, and a status bar that added
    /// a directory's own inode size would be claiming a total it never
    /// computed.
    #[must_use]
    pub fn marked_bytes(&self) -> u64 {
        self.entries
            .iter()
            .filter(|e| e.kind != EntryKind::Dir && self.marks.contains(&e.path))
            .fold(0u64, |acc, e| acc.saturating_add(e.size.unwrap_or(0)))
    }

    /// How many marked entries are directories. [`Self::marked_bytes`]
    /// deliberately excludes directories (nothing here walks a tree), so a
    /// status bar that renders `marked_bytes` alone would understate a
    /// selection that includes one: a marked 10-byte file plus a 40 GiB
    /// directory must not read as "2 marked, 10 B" — that reads like a
    /// transfer size and is not one. Callers name the directory count
    /// separately instead of folding it into a total nobody computed.
    #[must_use]
    pub fn marked_dirs(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.kind == EntryKind::Dir && self.marks.contains(&e.path))
            .count()
    }

    /// Marks dropped by the last [`Self::refill`] because their entry was
    /// gone (#103) — see the `pruned_marks` field. Zero after a `cd`
    /// ([`Self::set_listing`]/[`Self::begin_loading`]) or when nothing was
    /// pruned. A later task surfaces this in the status bar; this accessor
    /// alone adds no UI.
    #[must_use]
    pub fn pruned_marks(&self) -> usize {
        self.pruned_marks
    }

    /// Drops marks whose entry is no longer listed and returns how many were
    /// dropped. A mark is a claim about an entry that EXISTS: a stale path
    /// would silently widen the next bulk operation. Called from
    /// [`Self::refill`], the same-dir refresh: the only path that can drop an
    /// entry without a `cd`. A paginated fill ([`Self::extend`], ADR 0017)
    /// only ADDS entries, so a mark placed mid-fill always points at
    /// something present and needs no pruning there.
    ///
    /// Accepted TOCTOU: identity here is the byte-exact `VPath` alone (hard
    /// rule 1) — the entry's `kind` is not part of it. If an external actor
    /// deletes a marked file and recreates a directory at the same path
    /// between listings, the mark survives the prune and a bulk operation
    /// acts on whatever now lives at that path, file or directory.
    fn prune_marks(&mut self) -> usize {
        if self.marks.is_empty() {
            return 0;
        }
        let before = self.marks.len();
        let present: HashSet<&VPath> = self.entries.iter().map(|e| &e.path).collect();
        self.marks.retain(|p| present.contains(p));
        before - self.marks.len()
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
        // #107: las ocultas del lote se apartan ANTES del merge — un lote
        // que queda vacío tras el filtro sigue alimentando el stash.
        let batch = self.stash_hidden(batch);
        if batch.is_empty() {
            return;
        }
        let quick_prev = self.quick_selected_path();
        // El cursor EN EL TOPE se ancla a la POSICIÓN, no al path: la
        // primera página de un dir paginado llega en orden de `readdir`
        // (hash del FS), así que su primer elemento una vez ordenado es
        // arbitrario. Anclarlo por path clavaba el cursor en mitad del
        // listado final —un dir de 5000 entradas abría enseñando la COLA—
        // aunque el usuario no hubiera tocado nada. En cuanto mueve el
        // cursor, el anclaje por path vuelve a mandar (rellenar no debe
        // mover su selección bajo los pies).
        let anchor = (self.cursor > 0)
            .then(|| self.entries.get(self.cursor).map(|e| e.path.clone()))
            .flatten();
        let (batch, batch_keys) = crate::sort::sort_with_keys_spec(batch, self.sort);
        crate::sort::merge_keyed_spec(
            &mut self.entries,
            &mut self.sort_keys,
            batch,
            batch_keys,
            self.sort,
        );
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
    ///
    /// Marks are pruned to the paths present in `entries` (#103, see
    /// `prune_marks`/[`Self::pruned_marks`]) — the listing passed in
    /// must be COMPLETE, since a partial page would silently discard the
    /// marks it omits.
    pub fn refill(&mut self, entries: Vec<Entry>) {
        let quick_prev = self.quick_selected_path();
        // #107: el refill trae el listado COMPLETO del dir — el stash se
        // reconstruye fresco de él, nunca se acumula con el anterior.
        self.hidden_stash.clear();
        let entries = self.stash_hidden(entries);
        let (entries, sort_keys) = crate::sort::sort_with_keys_spec(entries, self.sort);
        self.cursor = self.cursor.min(entries.len().saturating_sub(1));
        self.entries = entries;
        self.sort_keys = sort_keys;
        self.pruned_marks = self.prune_marks();
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
    /// (#107 review MINOR-5, aceptado: un stat que resuelve tras moverse su
    /// entrada al stash de ocultos se pierde — al re-mostrar, la fila pinta
    /// `None` hasta la siguiente sonda de foco. Autocurativo y barato.)
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
            attrs: std::collections::BTreeMap::new(),
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

    /// #108 L7: `set_sort` re-ordena en sitio, re-ancla el cursor por PATH
    /// y no toca las marcas (van por identidad); `extend` bajo el spec
    /// activo mergea en el orden nuevo.
    #[test]
    fn set_sort_reordena_reancla_y_extiende_bajo_el_spec() {
        use crate::sort::{SortColumn, SortDir, SortSpec};
        let mk = |n: &str, size: Option<u64>| {
            let mut e = e(&format!("mem:///{n}"), EntryKind::File);
            e.size = size;
            e
        };
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![mk("a", Some(30)), mk("b", Some(10)), mk("c", Some(20))],
        );
        p.cursor_down(); // "b"
        p.toggle_mark(); // marca "b"
        let spec = SortSpec {
            column: SortColumn::Size,
            dir: SortDir::Asc,
            dirs_first: true,
        };
        p.set_sort(spec);
        let orden: Vec<_> = p.entries().iter().map(|e| e.path.clone()).collect();
        assert_eq!(
            orden,
            vec![
                VPath::parse("mem:///b").unwrap(),
                VPath::parse("mem:///c").unwrap(),
                VPath::parse("mem:///a").unwrap()
            ]
        );
        assert_eq!(
            p.selected().map(|e| e.path.clone()),
            Some(VPath::parse("mem:///b").unwrap()),
            "cursor re-anclado por path"
        );
        assert_eq!(p.marks_len(), 1, "las marcas van por identidad");

        // Un fill que llega DESPUÉS mergea bajo el spec activo.
        p.set_loading(true);
        p.extend(vec![mk("d", Some(15))]);
        let orden: Vec<_> = p.entries().iter().map(|e| e.path.clone()).collect();
        assert_eq!(
            orden,
            vec![
                VPath::parse("mem:///b").unwrap(),
                VPath::parse("mem:///d").unwrap(),
                VPath::parse("mem:///c").unwrap(),
                VPath::parse("mem:///a").unwrap()
            ],
            "el lote entra en su posición bajo size/asc"
        );
    }

    /// #107: ocultar es PRESENTACIÓN — el provider lista todo, el pane
    /// aparta las de punto inicial a un stash y las devuelve al mostrar,
    /// mezcladas en orden (reusa `extend`). Solo el ÚLTIMO segmento
    /// decide: `a.txt` no es oculto.
    #[test]
    fn ocultar_aparta_los_dotfiles_y_mostrar_los_devuelve_en_orden() {
        let mut p = pane(&[".git", "a.txt", ".hidden", "b"]);
        assert_eq!(p.entries().len(), 4);
        assert!(p.show_hidden(), "default: se muestra todo");
        p.set_show_hidden(false);
        let names: Vec<_> = p.entries().iter().map(|e| e.path.clone()).collect();
        assert_eq!(
            names,
            vec![
                VPath::parse("mem:///a.txt").unwrap(),
                VPath::parse("mem:///b").unwrap()
            ],
            "solo el último segmento con '.' inicial se oculta"
        );
        assert_eq!(p.hidden_count(), 2);
        p.set_show_hidden(true);
        assert_eq!(p.entries().len(), 4, "mostrar restaura TODAS");
        assert_eq!(p.hidden_count(), 0);
        // Y el orden vuelve a ser el canónico (merge, no append).
        let first = p.entries().first().map(|e| e.path.clone());
        assert_eq!(first, Some(VPath::parse("mem:///.git").unwrap()));
    }

    #[test]
    fn un_listado_nuevo_bajo_ocultacion_filtra_al_entrar() {
        let mut p = pane(&["x"]);
        p.set_show_hidden(false);
        p.set_listing(
            VPath::parse("mem:///sub").unwrap(),
            vec![
                e("mem:///sub/.env", EntryKind::File),
                e("mem:///sub/main.rs", EntryKind::File),
            ],
        );
        assert_eq!(p.entries().len(), 1);
        assert_eq!(p.hidden_count(), 1);
        p.set_show_hidden(true);
        assert_eq!(p.entries().len(), 2);
    }

    #[test]
    fn un_fill_paginado_bajo_ocultacion_aparta_el_lote() {
        let mut p = pane(&["a"]);
        p.set_show_hidden(false);
        p.set_loading(true);
        p.extend(vec![
            e("mem:///.b", EntryKind::File),
            e("mem:///c", EntryKind::File),
        ]);
        assert_eq!(p.entries().len(), 2, "a + c");
        assert_eq!(p.hidden_count(), 1);
        // Un lote SOLO de ocultas no rompe nada.
        p.extend(vec![e("mem:///.d", EntryKind::File)]);
        assert_eq!(p.entries().len(), 2);
        assert_eq!(p.hidden_count(), 2);
    }

    /// Ocultar PODA las marcas de las entradas que desaparecen de la vista
    /// (misma disciplina que `refill`, #103): una selección invisible
    /// alimentando el siguiente F8 es exactamente el hazard que el
    /// contador `pruned_marks` existe para hacer ruidoso.
    #[test]
    fn ocultar_poda_las_marcas_de_los_dotfiles_y_lo_reporta() {
        let mut p = pane(&[".secret", "a"]);
        p.mark_all();
        assert_eq!(p.marks_len(), 2);
        p.set_show_hidden(false);
        assert_eq!(p.marks_len(), 1, "la marca de .secret cae");
        assert_eq!(p.pruned_marks(), 1, "y JAMÁS en silencio");
        assert_eq!(p.marked_paths(), vec![VPath::parse("mem:///a").unwrap()]);
    }

    #[test]
    fn ocultar_reancla_el_cursor_por_path() {
        let mut p = pane(&[".a", ".b", "c"]);
        p.cursor_down();
        p.cursor_down(); // "c"
        p.set_show_hidden(false);
        assert_eq!(p.entries().len(), 1);
        assert_eq!(p.cursor(), 0);
        assert_eq!(
            p.selected().map(|e| e.path.clone()),
            Some(VPath::parse("mem:///c").unwrap()),
            "el cursor sigue sobre la MISMA entrada visible"
        );
    }

    #[test]
    fn refill_bajo_ocultacion_reemplaza_el_stash_sin_duplicar() {
        let mut p = pane(&[".a", "b"]);
        p.set_show_hidden(false);
        assert_eq!(p.hidden_count(), 1);
        p.refill(vec![
            e("mem:///.a", EntryKind::File),
            e("mem:///.z", EntryKind::File),
            e("mem:///b", EntryKind::File),
        ]);
        assert_eq!(p.entries().len(), 1);
        assert_eq!(p.hidden_count(), 2, "stash FRESCO del refill, sin dup");
        p.set_show_hidden(true);
        assert_eq!(p.entries().len(), 3, "sin duplicados tras mostrar");
    }

    /// Regla 1: la decisión es por BYTES del último segmento — un nombre
    /// no-UTF8 que empieza por `.` (0x2E) se oculta igual; uno hostil que
    /// no, sigue visible.
    #[test]
    fn ocultar_decide_por_bytes_no_por_texto() {
        let dir = VPath::parse("mem:///").unwrap();
        let dot_hostile = dir
            .clone()
            .join(norte_proto::Segment::new(b".\xff\xfe".to_vec()).unwrap());
        let plain_hostile = dir
            .clone()
            .join(norte_proto::Segment::new(b"\xff\xfe".to_vec()).unwrap());
        let mk = |p: &VPath| Entry {
            attrs: std::collections::BTreeMap::new(),
            path: p.clone(),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
        };
        let mut p = PaneState::new(dir, vec![mk(&dot_hostile), mk(&plain_hostile)]);
        p.set_show_hidden(false);
        assert_eq!(p.entries().len(), 1);
        assert_eq!(p.entries()[0].path, plain_hostile);
        p.set_show_hidden(true);
        assert_eq!(p.entries().len(), 2, "los bytes vuelven intactos");
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
                    attrs: std::collections::BTreeMap::new(),
                    path: hostile.clone(),
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                },
                Entry {
                    attrs: std::collections::BTreeMap::new(),
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
                    attrs: std::collections::BTreeMap::new(),
                    path: nfc.clone(),
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                },
                Entry {
                    attrs: std::collections::BTreeMap::new(),
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
                    attrs: std::collections::BTreeMap::new(),
                    path: hostile.clone(),
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                },
                Entry {
                    attrs: std::collections::BTreeMap::new(),
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
                    attrs: std::collections::BTreeMap::new(),
                    path: hostile.clone(),
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                },
                Entry {
                    attrs: std::collections::BTreeMap::new(),
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

    /// El cursor EN EL TOPE se queda en el tope mientras el listado se
    /// rellena: la primera página de un dir paginado llega en orden de
    /// `readdir` (hash del FS), así que su primer elemento ORDENADO es
    /// arbitrario — anclar por path ahí dejaba el cursor clavado en mitad
    /// del listado final (en un dir de 5000 ficheros, el pane abría
    /// mostrando la COLA en vez del principio). Anclar por path sigue
    /// valiendo en cuanto el usuario mueve el cursor.
    #[test]
    fn extend_con_el_cursor_en_el_tope_lo_deja_en_el_tope() {
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![e("mem:///m", EntryKind::File)],
        );
        assert_eq!(p.cursor(), 0);
        p.extend(vec![
            e("mem:///a", EntryKind::File),
            e("mem:///b", EntryKind::File),
        ]);
        assert_eq!(p.cursor(), 0, "el cursor sigue en la primera fila");
        assert_eq!(
            p.selected().unwrap().path,
            VPath::parse("mem:///a").unwrap(),
            "y esa fila es el principio REAL del listado ya mergeado"
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

    #[test]
    fn refill_keeps_marks_of_entries_that_survive() {
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///a", EntryKind::File),
                e("mem:///b", EntryKind::File),
            ],
        );
        p.cursor_down(); // "b"
        p.toggle_mark();
        p.refill(vec![
            e("mem:///a", EntryKind::File),
            e("mem:///b", EntryKind::File),
        ]);
        assert_eq!(p.marks_len(), 1);
        assert_eq!(p.marked_paths(), vec![VPath::parse("mem:///b").unwrap()]);
    }

    #[test]
    fn refill_prunes_a_mark_whose_entry_vanished() {
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///a", EntryKind::File),
                e("mem:///b", EntryKind::File),
            ],
        );
        p.cursor_down();
        p.toggle_mark();
        p.refill(vec![e("mem:///a", EntryKind::File)]);
        assert_eq!(
            p.marks_len(),
            0,
            "a mark is a claim about an entry that exists"
        );
    }

    #[test]
    fn refill_prunes_only_the_marks_whose_entry_vanished() {
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///a", EntryKind::File),
                e("mem:///b", EntryKind::File),
                e("mem:///c", EntryKind::File),
            ],
        );
        p.toggle_mark(); // "a"
        p.cursor_down();
        p.cursor_down();
        p.toggle_mark(); // "c"
        p.refill(vec![
            e("mem:///a", EntryKind::File),
            e("mem:///b", EntryKind::File),
        ]);
        assert_eq!(p.marks_len(), 1);
        assert_eq!(p.marked_paths(), vec![VPath::parse("mem:///a").unwrap()]);
    }

    #[test]
    fn refill_reports_how_many_marks_it_pruned() {
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///a", EntryKind::File),
                e("mem:///b", EntryKind::File),
            ],
        );
        p.toggle_mark(); // "a"
        p.cursor_down();
        p.toggle_mark(); // "b"
        p.refill(vec![e("mem:///a", EntryKind::File)]);
        assert_eq!(p.pruned_marks(), 1);
        assert_eq!(p.marks_len(), 1);
    }

    #[test]
    fn a_fully_pruned_selection_falls_back_to_the_cursor_entry() {
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///a", EntryKind::File),
                e("mem:///b", EntryKind::File),
            ],
        );
        p.cursor_down(); // "b"
        p.toggle_mark();
        p.refill(vec![e("mem:///a", EntryKind::File)]);
        assert_eq!(p.pruned_marks(), 1);
        // Documented consequence, NOT an endorsement: with the set empty the
        // fallback takes over, so the caller must check `pruned_marks()`
        // before treating `marked_paths()` as "what the user selected".
        assert_eq!(p.marked_paths(), vec![VPath::parse("mem:///a").unwrap()]);
    }

    #[test]
    fn a_mark_placed_mid_fill_survives_the_rest_of_the_fill() {
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![e("mem:///b", EntryKind::File)],
        );
        p.set_loading(true);
        p.toggle_mark();
        p.extend(vec![e("mem:///a", EntryKind::File)]);
        p.set_loading(false);
        assert_eq!(p.marked_paths(), vec![VPath::parse("mem:///b").unwrap()]);
    }

    #[test]
    fn set_listing_to_the_same_dir_still_clears_marks() {
        // Deliberate, not a bug: `set_listing` means "a listing arrived for a
        // directory I navigated to" — even a `cd` that lands back on the SAME
        // dir clears marks. Only `refill` means "refresh" and preserves what
        // survives; this is the case neither `set_listing_limpia_las_marcas`
        // (different dir) nor the `refill` tests (same dir, but via `refill`)
        // cover.
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![e("mem:///a", EntryKind::File)],
        );
        p.toggle_mark();
        assert_eq!(p.marks_len(), 1);
        p.set_listing(
            VPath::parse("mem:///").unwrap(),
            vec![e("mem:///a", EntryKind::File)],
        );
        assert_eq!(p.marks_len(), 0);
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
                attrs: std::collections::BTreeMap::new(),
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

    // --- #103 task 2: mark all, invert, and the marked byte total -------

    /// Asymmetric starting state (#103 T9 review debt): pre-mark "a" before
    /// calling `mark_all`. `mark_all` only ADDS, so "a" stays marked and "b"
    /// gets added — total 2. A body accidentally rewritten to call
    /// `invert_marks` instead would FLIP "a" back off (it was already
    /// marked) while still marking "b" — total 1 — and this assertion would
    /// catch it; starting from an empty set cannot tell the two apart.
    #[test]
    fn mark_all_marks_every_entry() {
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///a", EntryKind::File),
                e("mem:///b", EntryKind::File),
            ],
        );
        p.toggle_mark(); // pre-marks "a" (asymmetric start)
        p.mark_all();
        assert_eq!(p.marks_len(), 2);
    }

    #[test]
    fn invert_marks_flips_every_entry() {
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///a", EntryKind::File),
                e("mem:///b", EntryKind::File),
            ],
        );
        p.toggle_mark(); // marks "a"
        p.invert_marks();
        assert_eq!(p.marked_paths(), vec![VPath::parse("mem:///b").unwrap()]);
    }

    /// Asymmetric starting state (#103 T9 review debt): pre-mark "alfa"
    /// (the ONLY entry the filter leaves visible) before filtering and
    /// calling `mark_all`. `mark_all` is idempotent on an already-marked
    /// visible entry, so it stays marked — total 1. A body accidentally
    /// rewritten to call `invert_marks` instead would FLIP it back off —
    /// total 0 — and this assertion would catch it; starting from an empty
    /// set cannot tell the two apart (both give 1).
    #[test]
    fn mark_all_under_a_filter_only_marks_the_visible() {
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///alfa", EntryKind::File),
                e("mem:///beta", EntryKind::File),
            ],
        );
        p.toggle_mark(); // pre-marks "alfa" (asymmetric start)
        p.quick_start(Mode::Filter);
        p.quick_char('a');
        p.quick_char('l'); // matches "alfa" only
        p.mark_all();
        assert_eq!(
            p.marks_len(),
            1,
            "marked_paths falls back to the cursor: pin the SET"
        );
        assert_eq!(p.marked_paths(), vec![VPath::parse("mem:///alfa").unwrap()]);
    }

    /// `invert` under a filter only flips the visible entries — the
    /// counterpart of `mark_all_under_a_filter_only_marks_the_visible`
    /// (nothing else pinned this direction).
    #[test]
    fn invert_under_a_filter_only_flips_the_visible() {
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///alfa", EntryKind::File),
                e("mem:///beta", EntryKind::File),
            ],
        );
        p.quick_start(Mode::Filter);
        p.quick_char('a');
        p.quick_char('l'); // only "alfa" visible
        p.invert_marks();
        assert_eq!(p.marks_len(), 1);
        let entries: Vec<_> = p.entries().to_vec();
        assert!(p.is_marked(&entries[0]), "alfa was visible: flipped");
        assert!(!p.is_marked(&entries[1]), "beta was hidden: untouched");
    }

    /// Marks OUTSIDE the visible set survive an invert untouched: invert is
    /// "flip what you see", not "replace the selection with its complement".
    #[test]
    fn invert_under_a_filter_leaves_a_hidden_mark_alone() {
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///alfa", EntryKind::File),
                e("mem:///beta", EntryKind::File),
            ],
        );
        p.cursor_down(); // "beta"
        p.toggle_mark(); // marks "beta"
        p.quick_start(Mode::Filter);
        p.quick_char('a');
        p.quick_char('l'); // only "alfa" visible now
        p.invert_marks();
        assert_eq!(p.marks_len(), 2, "beta survives, alfa gets flipped on");
        assert!(p.is_marked(&e("mem:///alfa", EntryKind::File)));
        assert!(p.is_marked(&e("mem:///beta", EntryKind::File)));
    }

    /// `Mode::Jump` marks the WHOLE listing, not just the jump target: unlike
    /// `Mode::Filter`, `quick_visible()` returns `None` in Jump, so
    /// `markable_indices` falls through to the full range. This is intended
    /// (a narrower `markable_indices` under Jump would also pass every other
    /// test in this file), so it needs its own pin.
    ///
    /// Asymmetric starting state (#103 T9 review debt): pre-mark "alfa"
    /// before jumping and calling `mark_all`. Correct behavior keeps BOTH
    /// entries marked (the pre-mark stays, "beta" gets added) — total ==
    /// `entries().len()`. A body accidentally rewritten to call
    /// `invert_marks` instead would flip "alfa" back off while still
    /// marking "beta" — total 1, short of `entries().len()` — and this
    /// assertion would catch it; starting from an empty set cannot (both
    /// give the full length).
    #[test]
    fn mark_all_under_jump_marks_the_whole_listing() {
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///alfa", EntryKind::File),
                e("mem:///beta", EntryKind::File),
            ],
        );
        p.toggle_mark(); // pre-marks "alfa" (asymmetric start)
        p.quick_start(Mode::Jump);
        p.quick_char('a');
        p.mark_all();
        assert_eq!(p.marks_len(), p.entries().len());
    }

    /// #103 review BLOCKER/MAJOR fix: `toggle_mark_and_advance` (mc/Total
    /// Commander sweep, `insert`) marks the FILTERED selection and advances
    /// WITHIN the filter — the real cursor (invisible to the user) must
    /// never move under a `Mode::Filter` quick search. Two presses under the
    /// filter `a` (visible: `aa`, `ab`) mark exactly those two and never
    /// touch the hidden `zz`.
    #[test]
    fn toggle_mark_and_advance_stays_inside_an_active_filter() {
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///aa", EntryKind::File),
                e("mem:///ab", EntryKind::File),
                e("mem:///zz", EntryKind::File),
            ],
        );
        p.quick_start(Mode::Filter);
        p.quick_char('a'); // visible: aa, ab

        p.toggle_mark_and_advance();
        p.toggle_mark_and_advance();

        assert_eq!(p.marks_len(), 2);
        assert!(p.is_marked(&e("mem:///aa", EntryKind::File)));
        assert!(p.is_marked(&e("mem:///ab", EntryKind::File)));
        assert!(
            !p.is_marked(&e("mem:///zz", EntryKind::File)),
            "zz is hidden by the filter: never marked"
        );
        assert_eq!(
            p.quick_visible().map(<[usize]>::len),
            Some(2),
            "the filter itself is untouched"
        );

        // The filter only has 2 visible rows, so the second press already
        // clamped at the last one ("ab") without wrapping. A third press
        // toggles "ab" back OFF — it never wraps onto the hidden "zz".
        p.toggle_mark_and_advance();
        assert!(
            p.is_marked(&e("mem:///aa", EntryKind::File)),
            "aa stays marked"
        );
        assert!(
            !p.is_marked(&e("mem:///ab", EntryKind::File)),
            "ab toggled back off, clamped at the last visible row"
        );
        assert!(
            !p.is_marked(&e("mem:///zz", EntryKind::File)),
            "sweeping never wraps onto a hidden entry"
        );
    }

    #[test]
    fn marked_bytes_sums_files_and_ignores_dirs() {
        let mut a = e("mem:///a", EntryKind::File);
        a.size = Some(10);
        let mut d = e("mem:///d", EntryKind::Dir);
        d.size = Some(4096); // a provider may report a dir size; it must not count
        let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), vec![a, d]);
        p.mark_all();
        assert_eq!(p.marked_bytes(), 10);
    }

    /// A file that is NOT marked must not contribute to the total: both of
    /// the tests above mark every entry, so neither would catch
    /// `marked_bytes` silently dropping the `self.marks.contains(...)` guard
    /// and summing the whole directory.
    #[test]
    fn marked_bytes_counts_only_what_is_marked() {
        let mut a = e("mem:///a", EntryKind::File);
        a.size = Some(10);
        let mut b = e("mem:///b", EntryKind::File);
        b.size = Some(32);
        let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), vec![a, b]);
        p.toggle_mark(); // "a" only
        assert_eq!(
            p.marked_bytes(),
            10,
            "an unmarked file must not be in the total"
        );
    }

    #[test]
    fn marked_bytes_saturates_instead_of_overflowing() {
        let mut a = e("mem:///a", EntryKind::File);
        a.size = Some(u64::MAX);
        let mut b = e("mem:///b", EntryKind::File);
        b.size = Some(1);
        let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), vec![a, b]);
        p.mark_all();
        assert_eq!(
            p.marked_bytes(),
            u64::MAX,
            "a hostile listing must not panic in debug"
        );
    }

    #[test]
    fn marked_dirs_counts_only_marked_directories() {
        let mut a = e("mem:///a", EntryKind::File);
        a.size = Some(10);
        let d = e("mem:///d", EntryKind::Dir);
        let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), vec![a, d]);
        assert_eq!(p.marked_dirs(), 0, "nothing marked yet");
        p.mark_all();
        assert_eq!(p.marked_dirs(), 1, "one of the two marked entries is a dir");
    }

    /// A directory that is NOT marked must not contribute: mirrors
    /// `marked_bytes_counts_only_what_is_marked` for the dir counter.
    #[test]
    fn marked_dirs_ignores_unmarked_directories() {
        let f = e("mem:///a", EntryKind::File);
        let d = e("mem:///d", EntryKind::Dir);
        let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), vec![f, d]);
        // `sort_entries` puts directories first, so the file is at index 1
        // regardless of construction order.
        let file_idx = p
            .entries()
            .iter()
            .position(|entry| entry.kind == EntryKind::File)
            .expect("the file is in the listing");
        p.set_cursor(file_idx);
        p.toggle_mark(); // marks the file only, not the directory
        assert_eq!(p.marked_dirs(), 0);
    }

    // --- #103: mark/unmark by glob ---------------------------------------

    #[test]
    fn mark_glob_marks_the_matching_names() {
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///a.rs", EntryKind::File),
                e("mem:///b.rs", EntryKind::File),
                e("mem:///c.txt", EntryKind::File),
            ],
        );
        assert_eq!(p.mark_glob("*.rs", true).unwrap(), 2);
        assert_eq!(
            p.marked_paths(),
            vec![
                VPath::parse("mem:///a.rs").unwrap(),
                VPath::parse("mem:///b.rs").unwrap()
            ]
        );
    }

    /// review: the direction that actually needs a fold is an UPPERCASE
    /// pattern against a lowercase name — the reverse always worked because
    /// the fold already lowercases the haystack regardless of `globset`'s
    /// own `case_insensitive` knob. Also pins the non-ASCII case: `globset`'s
    /// knob is ASCII-only (`(?-u)` byte mode), so `É*` only reaches
    /// `étude.txt` because `mark_glob` folds the PATTERN too (BLOCKER C).
    #[test]
    fn mark_glob_is_case_insensitive() {
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![e("mem:///photo.jpg", EntryKind::File)],
        );
        assert_eq!(
            p.mark_glob("*.JPG", true).unwrap(),
            1,
            "uppercase ASCII pattern, lowercase name"
        );

        let mut p2 = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![e("mem:///étude.txt", EntryKind::File)],
        );
        assert_eq!(
            p2.mark_glob("É*", true).unwrap(),
            1,
            "uppercase non-ASCII pattern, lowercase name — globset's own \
             case_insensitive can't do this, only the pattern fold can"
        );
    }

    #[test]
    fn mark_glob_with_mark_false_unmarks_only_the_matches() {
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///a.rs", EntryKind::File),
                e("mem:///c.txt", EntryKind::File),
            ],
        );
        p.mark_all();
        assert_eq!(p.mark_glob("*.rs", false).unwrap(), 1);
        // An empty mark set makes `marked_paths` fall back to the cursor
        // entry (see its rustdoc) — a one-element vector could then be
        // satisfied by ZERO marks. Pin the count first.
        assert_eq!(p.marks_len(), 1);
        assert_eq!(
            p.marked_paths(),
            vec![VPath::parse("mem:///c.txt").unwrap()]
        );
    }

    #[test]
    fn mark_glob_counts_only_the_marks_it_changed() {
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///a.rs", EntryKind::File),
                e("mem:///b.rs", EntryKind::File),
            ],
        );
        p.mark_glob("a.rs", true).unwrap();
        assert_eq!(
            p.mark_glob("*.rs", true).unwrap(),
            1,
            "a.rs was already marked"
        );
    }

    #[test]
    fn an_invalid_glob_errors_and_marks_nothing() {
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![e("mem:///a.rs", EntryKind::File)],
        );
        assert!(p.mark_glob("[", true).is_err());
        assert_eq!(p.marks_len(), 0);
    }

    #[test]
    fn mark_glob_under_a_filter_only_reaches_the_visible() {
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///alfa.rs", EntryKind::File),
                e("mem:///beta.rs", EntryKind::File),
            ],
        );
        p.quick_start(crate::nav::Mode::Filter);
        p.quick_char('a');
        p.quick_char('l');
        assert_eq!(p.mark_glob("*.rs", true).unwrap(), 1);
        assert_eq!(p.marks_len(), 1);
        assert!(p.is_marked(&p.entries()[0].clone()));
    }

    /// Hostile corpus (hard rule 1): a pattern addresses the FOLDED text
    /// (lossy → NFC → lowercase → NFC), never the raw bytes — the invalid
    /// bytes of a non-UTF-8 name fold to U+FFFD and cannot be named
    /// INDIVIDUALLY, though typing U+FFFD names ALL of them at once (a name
    /// whose valid suffix satisfies the rest of the pattern still matches).
    /// `marked_paths` gives the ORIGINAL bytes back regardless.
    #[test]
    fn mark_glob_matches_the_lossy_form_and_returns_raw_bytes() {
        // mem:///<0xFF><0xFE>.rs — same hostile construction as the other
        // hostile tests in this file (see
        // `marca_identidad_por_bytes_del_path_nombre_hostil`).
        let hostile = VPath::parse("mem:///")
            .unwrap()
            .join(norte_proto::Segment::new(vec![0xFF, 0xFE, b'.', b'r', b's']).unwrap());
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![Entry {
                attrs: std::collections::BTreeMap::new(),
                path: hostile.clone(),
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            }],
        );
        assert_eq!(p.mark_glob("*.rs", true).unwrap(), 1);
        assert_eq!(
            p.marked_paths(),
            vec![hostile],
            "raw bytes, never the lossy form"
        );

        // The invalid bytes themselves are unaddressable INDIVIDUALLY: they
        // fold to U+FFFD, and typing U+FFFD names all of them at once.
        p.clear_marks();
        assert_eq!(p.mark_glob("\u{FFFD}*", true).unwrap(), 1);
    }

    /// Symmetry pin (review requirement, the one that guards the fold
    /// forever): for EVERY name in the canonical hostile corpus, a pattern
    /// built from that name's own RAW (unfolded) text — escaped only for
    /// the glob metacharacters it happens to contain — must still mark
    /// exactly that entry, and `marked_paths` must return its exact bytes.
    ///
    /// This is what actually exercises `mark_glob` folding the PATTERN
    /// (BLOCKER C): before that fix, `GlobBuilder` compiled the pattern
    /// AS-IS while the haystack was already folded (NFC + lowercase) — an
    /// NFD name's raw (decomposed) text then never matched its own
    /// (composed) haystack. Fails today for `nfd_e_acute`,
    /// `nfd_uppercase_composed_only_lowercase`, and `name_max_nfd_overflow`.
    #[test]
    fn mark_glob_pattern_from_each_names_own_text_marks_exactly_that_entry() {
        fn escape_glob(s: &str) -> String {
            let mut out = String::with_capacity(s.len());
            for c in s.chars() {
                if matches!(c, '*' | '?' | '[' | ']' | '{' | '}' | '\\') {
                    out.push('\\');
                }
                out.push(c);
            }
            out
        }

        for name in norte_testkit::corpus::hostile_names() {
            let dir = VPath::parse("mem:///").unwrap();
            let seg = norte_proto::Segment::new(name.bytes.clone()).unwrap();
            let path = dir.clone().join(seg);
            let mut p = PaneState::new(
                dir,
                vec![Entry {
                    attrs: std::collections::BTreeMap::new(),
                    path: path.clone(),
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                }],
            );
            let raw = String::from_utf8_lossy(&name.bytes).into_owned();
            let pattern = escape_glob(&raw);
            let changed = p.mark_glob(&pattern, true).unwrap_or_else(|err| {
                panic!("[{}] pattern {pattern:?} must compile: {err}", name.id)
            });
            assert_eq!(
                changed, 1,
                "[{}] a pattern from its own text must mark exactly this \
                 entry (pattern {pattern:?})",
                name.id
            );
            assert_eq!(p.marks_len(), 1, "[{}] exactly one mark", name.id);
            assert_eq!(
                p.marked_paths(),
                vec![path],
                "[{}] raw bytes back, never the lossy/folded form",
                name.id
            );
        }
    }

    /// A pattern typed exactly as an NFD name is painted (macOS trap,
    /// CLAUDE.md) matches its NFD twin only because `mark_glob` folds the
    /// pattern to NFC before compiling (BLOCKER C). Also pins the
    /// name-reinterpretation branch (#57): `П*` reaches `cp866_papka` only
    /// under an active IBM866 reinterpretation — a mutant that folds the
    /// haystack with `fold_with(name, None)` instead of
    /// `fold_with(name, self.name_encoding)` would make this fail, since
    /// the raw bytes aren't valid UTF-8 and their plain lossy fold is
    /// unrelated Unicode replacement text, not Cyrillic.
    #[test]
    fn mark_glob_matches_nfd_typed_pattern_and_reinterpreted_uppercase() {
        let nfd_name = "an\u{0303}o.txt";
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![e(&format!("mem:///{nfd_name}"), EntryKind::File)],
        );
        assert_eq!(
            p.mark_glob(nfd_name, true).unwrap(),
            1,
            "NFD-typed pattern must find its NFD twin"
        );

        let papka = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "cp866_papka")
            .expect("fixture del corpus")
            .bytes;
        let dir = VPath::parse("mem:///").unwrap();
        let seg = norte_proto::Segment::new(papka).unwrap();
        let mut p2 = PaneState::new(
            dir.clone(),
            vec![Entry {
                attrs: std::collections::BTreeMap::new(),
                path: dir.join(seg),
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            }],
        );
        assert_eq!(p2.cycle_name_encoding(), Some("IBM866"));
        assert_eq!(
            p2.mark_glob("П*", true).unwrap(),
            1,
            "uppercase Cyrillic pattern must reach the reinterpreted name"
        );
    }

    /// Collapse pin (review requirement): `lossy_collapse_ff`/
    /// `lossy_collapse_fe` are two DISTINCT byte sequences (`\xFF.rs` vs
    /// `\xFE.rs`) whose lossy fold collapses to the SAME `"\u{FFFD}.rs"` —
    /// without this pair the collapse can't be pinned at all: with a single
    /// hostile entry, "matches this one" and "matches every invalid name"
    /// are indistinguishable.
    #[test]
    fn mark_glob_collapse_pin() {
        let names = norte_testkit::corpus::hostile_names();
        let ff = names
            .iter()
            .find(|n| n.id == "lossy_collapse_ff")
            .expect("fixture del corpus")
            .bytes
            .clone();
        let fe = names
            .iter()
            .find(|n| n.id == "lossy_collapse_fe")
            .expect("fixture del corpus")
            .bytes
            .clone();
        let dir = VPath::parse("mem:///").unwrap();
        let path_ff = dir.clone().join(norte_proto::Segment::new(ff).unwrap());
        let path_fe = dir.clone().join(norte_proto::Segment::new(fe).unwrap());
        let clean = dir
            .clone()
            .join(norte_proto::Segment::new(b"clean.rs".to_vec()).unwrap());
        let mut p = PaneState::new(
            dir,
            vec![
                Entry {
                    attrs: std::collections::BTreeMap::new(),
                    path: path_ff.clone(),
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                },
                Entry {
                    attrs: std::collections::BTreeMap::new(),
                    path: path_fe.clone(),
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                },
                Entry {
                    attrs: std::collections::BTreeMap::new(),
                    path: clean,
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                },
            ],
        );
        // ONE pattern (U+FFFD, unaddressable individually) reaches BOTH
        // distinct byte sequences, never the clean name.
        assert_eq!(p.mark_glob("\u{FFFD}*", true).unwrap(), 2);
        assert_eq!(p.marks_len(), 2);
        let mut marked = p.marked_paths();
        marked.sort();
        let mut expected = vec![path_ff, path_fe];
        expected.sort();
        assert_eq!(marked, expected, "both original byte sequences, untouched");

        // Separately: a many-to-one selector over byte-exact identities —
        // the counterpart of `marcas_distinguen_gemelos_nfc_y_nfd_sin_plegar`
        // (which pins that a TOGGLE never folds gemelos). Here, deliberately,
        // ONE NFC pattern marks BOTH the NFC and NFD é twins: `mark_glob`
        // matches by folded TEXT, an intentional many-to-one selector, while
        // each mark's IDENTITY (its `VPath`) stays byte-exact — this is the
        // opposite property from toggle's byte-exact SELECTION, not a
        // regression of it.
        let nfc = names
            .iter()
            .find(|n| n.id == "nfc_e_acute")
            .expect("fixture del corpus")
            .bytes
            .clone();
        let nfd = names
            .iter()
            .find(|n| n.id == "nfd_e_acute")
            .expect("fixture del corpus")
            .bytes
            .clone();
        let dir2 = VPath::parse("mem:///").unwrap();
        let path_nfc = dir2.clone().join(norte_proto::Segment::new(nfc).unwrap());
        let path_nfd = dir2.clone().join(norte_proto::Segment::new(nfd).unwrap());
        let mut p2 = PaneState::new(
            dir2,
            vec![
                Entry {
                    attrs: std::collections::BTreeMap::new(),
                    path: path_nfc.clone(),
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                },
                Entry {
                    attrs: std::collections::BTreeMap::new(),
                    path: path_nfd.clone(),
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                },
            ],
        );
        assert_eq!(
            p2.mark_glob("é", true).unwrap(),
            2,
            "a many-to-one selector over byte-exact identities: ONE NFC \
             pattern deliberately marks BOTH the NFC and NFD é twins — the \
             counterpart of toggle's byte-exact selection, not a regression \
             of it"
        );
        assert_eq!(p2.marks_len(), 2);
    }

    /// Guard for the #110 translation ([`unicode_glob_regex`]): pins the
    /// SHAPE of globset's output that the byte-run decoding relies on —
    /// the `(?-u)` prefix, and non-ASCII pattern bytes emitted as
    /// consecutive `\xNN` escapes (also across a class range's `-`). A
    /// globset upgrade that changes either fails HERE, loudly, instead of
    /// letting patterns silently stop matching non-ASCII names.
    #[test]
    fn globset_regex_shape_is_the_one_this_translation_expects() {
        // Same builder config as `mark_glob` (no `case_insensitive` — the
        // fold owns case, see the rustdoc there).
        let build = |p: &str| GlobBuilder::new(p).backslash_escape(true).build().unwrap();
        assert!(build("a").regex().starts_with("(?-u)"));
        let lit = build("a\u{f1}o").regex().to_owned();
        assert!(lit.contains(r"\xc3\xb1"), "ñ as a byte-escape run: {lit}");
        let class = build("[\u{f1}x]").regex().to_owned();
        assert!(class.contains(r"[\xc3\xb1x]"), "class run: {class}");
        let range = build("[\u{f1}-\u{fc}]").regex().to_owned();
        assert!(
            range.contains(r"\xc3\xb1-\xc3\xbc"),
            "range endpoints as runs split by ASCII '-': {range}"
        );

        // And the translation of those shapes, end to end:
        assert_eq!(
            unicode_glob_regex(&build("a\u{f1}o")).unwrap(),
            "^a\u{f1}o$"
        );
        assert_eq!(
            unicode_glob_regex(&build("[\u{f1}-\u{fc}]")).unwrap(),
            "^[\u{f1}-\u{fc}]$"
        );
        // Astral endpoints (4-byte UTF-8 runs): 𝄞..𝄢 stays a CHAR range.
        assert_eq!(
            unicode_glob_regex(&build("[\u{1D11E}-\u{1D122}]")).unwrap(),
            "^[\u{1D11E}-\u{1D122}]$"
        );
    }

    /// The #110 recompile must PRESERVE globset's `dot_matches_new_line`
    /// (audit MAJOR-1): `\n` is a legal name byte on unix (corpus
    /// `control_newline`) and `*`/`?` translate to `.`-derived tokens —
    /// losing the flag makes `*` silently stop matching those names, the
    /// INVERSE of the byte/char bug. `mark_glob("*")` must equal mark-all
    /// over the whole hostile corpus.
    #[test]
    fn mark_glob_star_still_reaches_names_with_newlines() {
        let dir = VPath::parse("mem:///").unwrap();
        let entries: Vec<Entry> = norte_testkit::corpus::hostile_names()
            .into_iter()
            .map(|n| Entry {
                attrs: std::collections::BTreeMap::new(),
                path: dir
                    .clone()
                    .join(norte_proto::Segment::new(n.bytes.clone()).unwrap()),
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            })
            .collect();
        let total = entries.len();
        let mut p = PaneState::new(dir.clone(), entries);
        assert_eq!(
            p.mark_glob("*", true).unwrap(),
            total,
            "'*' IS mark-all — a name the wildcard cannot reach would feed \
             the next bulk op a survivor set the user never chose"
        );
        p.clear_marks();

        // Direct pin on the `?` token crossing `\n`, like globset's does.
        let nl = dir.join(norte_proto::Segment::new(b"a\nb".to_vec()).unwrap());
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![Entry {
                attrs: std::collections::BTreeMap::new(),
                path: nl,
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            }],
        );
        assert_eq!(p.mark_glob("a?b", true).unwrap(), 1, "? crosses \\n");
    }

    /// Case decision pin (audit MINOR-2): equality is the FOLD's, and only
    /// the fold's. Regex-crate `(?i)` would additionally fold `ſ` (U+017F)
    /// to `s` — wider than `nav::fold`'s `to_lowercase`, so a pattern `s.*`
    /// would mark a file the quick search filter treats as distinct. The
    /// glob therefore compiles WITHOUT `case_insensitive`; flipping it back
    /// on fails here.
    #[test]
    fn mark_glob_case_equality_is_the_folds_not_the_regex_crates() {
        let dir = VPath::parse("mem:///").unwrap();
        let long_s = dir
            .clone()
            .join(norte_proto::Segment::new("\u{17f}.txt".as_bytes().to_vec()).unwrap());
        let mut p = PaneState::new(
            dir,
            vec![Entry {
                attrs: std::collections::BTreeMap::new(),
                path: long_s,
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            }],
        );
        assert_eq!(
            p.mark_glob("s.txt", true).unwrap(),
            0,
            "ſ folds to itself; only regex-crate case folding equates it \
             with s, and that is NOT the pane's definition of equality"
        );
        assert_eq!(p.mark_glob("\u{17f}.txt", true).unwrap(), 1);
    }

    /// Escaped-backslash adjacency (audit hole 4): in `a\\xc3o` the `\\`
    /// pair is one token, so `xc3` is LITERAL text — the translation must
    /// not re-scan it as a byte escape. Kills any future
    /// scan-and-replace-`\xNN` rewrite of the tokenizer.
    #[test]
    fn mark_glob_escaped_backslash_before_hex_text_stays_literal() {
        let dir = VPath::parse("mem:///").unwrap();
        let lit = dir
            .clone()
            .join(norte_proto::Segment::new(b"a\\xc3o".to_vec()).unwrap());
        let anio = dir
            .clone()
            .join(norte_proto::Segment::new("a\u{c3}o".as_bytes().to_vec()).unwrap());
        let mk = |path: &VPath| Entry {
            attrs: std::collections::BTreeMap::new(),
            path: path.clone(),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
        };
        let mut p = PaneState::new(dir, vec![mk(&lit), mk(&anio)]);
        assert_eq!(
            p.mark_glob("a\\\\xc3o", true).unwrap(),
            1,
            "the pattern names the literal-backslash file, nothing else"
        );
        assert_eq!(p.marked_paths(), vec![lit]);
    }

    /// Character-granularity (#110, fixed): `?` consumes one CHARACTER and
    /// a class matches char-wise — `a?o.txt` covers `año.txt`, and
    /// `a[ñx]o.txt` covers both twins. globset alone compiles `(?-u)` byte
    /// mode, where `ñ` is 2 bytes and both patterns silently marked a
    /// DIFFERENT file than the one named. Astral chars (4-byte UTF-8) are
    /// one character too.
    #[test]
    fn mark_glob_matches_characters_not_utf8_bytes() {
        let dir = VPath::parse("mem:///").unwrap();
        let anio = dir
            .clone()
            .join(norte_proto::Segment::new("a\u{f1}o.txt".as_bytes().to_vec()).unwrap());
        let axo = dir
            .clone()
            .join(norte_proto::Segment::new(b"axo.txt".to_vec()).unwrap());
        let mut p = PaneState::new(
            dir,
            vec![
                Entry {
                    attrs: std::collections::BTreeMap::new(),
                    path: anio.clone(),
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                },
                Entry {
                    attrs: std::collections::BTreeMap::new(),
                    path: axo.clone(),
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                },
            ],
        );
        assert_eq!(
            p.mark_glob("a?o.txt", true).unwrap(),
            2,
            "one char-wildcard covers año.txt AND axo.txt"
        );
        assert_eq!(p.marks_len(), 2);
        p.clear_marks();

        assert_eq!(
            p.mark_glob("a??o.txt", true).unwrap(),
            0,
            "two wildcards are two CHARACTERS — neither 3-char name matches"
        );
        p.clear_marks();

        assert_eq!(
            p.mark_glob("a[\u{f1}x]o.txt", true).unwrap(),
            2,
            "a character class reaches a multi-byte char"
        );
        assert_eq!(p.marks_len(), 2);
        p.clear_marks();

        // A class RANGE spanning non-ASCII endpoints is char-wise too.
        assert_eq!(
            p.mark_glob("a[\u{f0}-\u{f2}]o.txt", true).unwrap(),
            1,
            "ñ (U+00F1) sits inside the U+00F0..U+00F2 range"
        );
        assert_eq!(p.marked_paths(), vec![anio]);
        p.clear_marks();

        // A NEGATED class over chars: `axo` has no ñ, `año` does.
        assert_eq!(
            p.mark_glob("a[!\u{f1}]o.txt", true).unwrap(),
            1,
            "negated char class excludes año.txt only"
        );
        assert_eq!(p.marked_paths(), vec![axo]);

        // Astral: 𝄞 is FOUR UTF-8 bytes and exactly ONE `?`.
        let dir = VPath::parse("mem:///").unwrap();
        let clef = dir
            .clone()
            .join(norte_proto::Segment::new("\u{1D11E}.txt".as_bytes().to_vec()).unwrap());
        let mut p = PaneState::new(
            dir,
            vec![Entry {
                attrs: std::collections::BTreeMap::new(),
                path: clef,
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            }],
        );
        assert_eq!(p.mark_glob("?.txt", true).unwrap(), 1, "𝄞 = ONE char");
    }

    /// Masking-divergence pin: rows are painted through
    /// [`crate::display_name_with`], which MASKS bidi overrides to U+FFFD;
    /// `mark_glob`'s fold does NOT mask them. Typing exactly what the pane
    /// PAINTED therefore does not name what the fold preserves.
    #[test]
    fn mark_glob_pattern_diverges_from_the_painted_text_for_bidi_hazards() {
        let rtl = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "rtl_override")
            .expect("fixture del corpus")
            .bytes;
        let (painted, hostile) = crate::display_name_with(&rtl, None);
        assert!(hostile, "rtl_override is flagged hostile");
        assert!(
            painted.contains('\u{FFFD}'),
            "display_name_with masks the RLO override to U+FFFD: {painted:?}"
        );

        let dir = VPath::parse("mem:///").unwrap();
        let seg = norte_proto::Segment::new(rtl.clone()).unwrap();
        let mut p = PaneState::new(
            dir.clone(),
            vec![Entry {
                attrs: std::collections::BTreeMap::new(),
                path: dir.join(seg),
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            }],
        );
        assert_eq!(
            p.mark_glob(&painted, true).unwrap(),
            0,
            "the painted (U+FFFD-masked) text is not what the fold matches \
             against — the fold preserves the raw RLO, unmasked"
        );
    }

    /// Backslash pin: `backslash_escape(true)` (BLOCKER C) makes `\`
    /// consistently an escape character regardless of host OS — `globset`'s
    /// own default depends on `is_separator('\\')` (true on unix, false on
    /// windows; it also rewrites `\` to `/` in the haystack there), so the
    /// SAME pattern would otherwise answer differently per platform. `\` is
    /// a legal Linux filename byte (corpus `win_backslash`).
    #[test]
    fn mark_glob_backslash_matches_only_when_escaped() {
        let name = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "win_backslash")
            .expect("fixture del corpus")
            .bytes; // a\b: 3 bytes, a literal backslash in the middle.
        let dir = VPath::parse("mem:///").unwrap();
        let seg = norte_proto::Segment::new(name).unwrap();
        let mut p = PaneState::new(
            dir.clone(),
            vec![Entry {
                attrs: std::collections::BTreeMap::new(),
                path: dir.join(seg),
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            }],
        );
        // An escaped backslash (`\\` in the pattern) matches the literal byte.
        assert_eq!(
            p.mark_glob("a\\\\b", true).unwrap(),
            1,
            "escaped backslash matches the literal byte"
        );
        p.clear_marks();
        // A lone backslash is the escape character itself: `\b` escapes `b`
        // to a literal `b`, so the compiled pattern means "ab", not "a\b".
        assert_eq!(
            p.mark_glob("a\\b", true).unwrap(),
            0,
            "unescaped backslash is consumed as an escape, not a literal match"
        );
    }

    /// #117-follow-up: los valores de columnas `plugin:` viven en un
    /// side-map del pane (espejo de `decorations` — claves por `VPath` del
    /// listado ACTUAL): `plugin_cell` los sirve RE-enmascarados
    /// defensivamente (doctrina P1: los consumidores no confían en el
    /// ingest), y un listado nuevo los invalida igual que las decoraciones.
    #[test]
    fn plugin_columns_side_map_re_enmascara_y_se_limpia() {
        let mut p = pane(&["a", "b"]);
        let path = VPath::parse("mem:///a").unwrap();
        let mut per_path = std::collections::HashMap::new();
        // Valor con RLO crudo: el render jamás lo pinta sin U+FFFD.
        per_path.insert(path.clone(), "main\u{202E}evil".to_owned());
        let mut cols = std::collections::HashMap::new();
        cols.insert("plugin:git/branch".to_owned(), per_path);
        p.set_plugin_columns(cols);
        let cell = p
            .plugin_cell("plugin:git/branch", &path)
            .expect("valor presente");
        assert!(
            !cell.contains('\u{202E}'),
            "hazard crudo en la celda: {cell:?}"
        );
        assert!(
            cell.contains('\u{FFFD}'),
            "el hazard se enmascara: {cell:?}"
        );
        assert!(cell.starts_with("main"));
        // Columna desconocida o path sin valor → None (blanco).
        assert_eq!(p.plugin_cell("plugin:git/otro", &path), None);
        assert_eq!(
            p.plugin_cell("plugin:git/branch", &VPath::parse("mem:///b").unwrap()),
            None
        );
        // Un listado nuevo invalida el side-map (claves de OTRO listado).
        p.set_listing(VPath::parse("mem:///d").unwrap(), Vec::new());
        assert_eq!(p.plugin_cell("plugin:git/branch", &path), None);
    }

    /// Audit F4 (#117-follow-up): las claves del side-map son `VPath`
    /// BYTE-exactas — dos nombres no-UTF8 distintos cuyo display lossy
    /// COLAPSA al mismo `�` (corpus `lossy_collapse_ff`/`_fe`) conservan
    /// celdas separadas. Si alguien "simplifica" mañana keyeando por
    /// display, los valores se mezclarían entre ficheros distintos y esto
    /// se pone rojo.
    #[test]
    fn plugin_columns_clava_por_bytes_no_por_display() {
        let mut p = pane(&[]);
        let ff = VPath::parse("mem:///%FF").unwrap();
        let fe = VPath::parse("mem:///%FE").unwrap();
        let mut per_path = std::collections::HashMap::new();
        per_path.insert(ff.clone(), "uno".to_owned());
        per_path.insert(fe.clone(), "dos".to_owned());
        let mut cols = std::collections::HashMap::new();
        cols.insert("plugin:git/branch".to_owned(), per_path);
        p.set_plugin_columns(cols);
        assert_eq!(
            p.plugin_cell("plugin:git/branch", &ff).as_deref(),
            Some("uno")
        );
        assert_eq!(
            p.plugin_cell("plugin:git/branch", &fe).as_deref(),
            Some("dos")
        );
    }
}
