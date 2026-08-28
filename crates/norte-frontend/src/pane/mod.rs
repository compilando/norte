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

mod marks;
mod quick;
mod viewport;

// `PaneState` sigue siendo UNO: lo que se reparte son sus métodos, en
// bloques `impl` hermanos. Un módulo hijo ve lo privado de su padre, así que
// esto no abre nada — solo pone junto lo que se lee junto.

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

/// En qué estado está la fila `..` de un listado.
///
/// Un enum y no dos `bool` porque el cuarto estado que dos booleanos
/// permitirían —«no pedida pero puesta»— no existe, y porque la diferencia
/// entre los dos que sí existen es justo la que se olvida: en una RAÍZ está
/// pedida y no puesta, y confundirlas haría que la primera entrada de verdad
/// del listado se comportara como la fila de subir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilaDeSubir {
    /// La configuración no la quiere.
    Apagada,
    /// La quiere, pero aquí no la hay: este directorio no tiene padre.
    Pedida,
    /// Está en `entries[0]`.
    Puesta,
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
    /// En qué estado está la fila `..` (`[ui] parent_entry`).
    fila_de_subir: FilaDeSubir,
    /// Claves de orden persistidas, índice-paralelas a `entries` (#54): el
    /// fill mergea lotes O(n+m) sin recomputar la clave NFC de lo ya listado.
    sort_keys: Vec<SortKey>,
    cursor: usize,
    loading: bool,
    quick: Option<QuickSearch>,
    marks: HashSet<VPath>,
    /// Snapshot of `marks` from before the pointer sweep in progress, so
    /// that [`Self::apply_sweep`] can RESTORE it and re-mark, making a drag
    /// that retreats give back the rows it pulled off. `None` = no sweep, or
    /// a sweep armed but not yet applied (the clone is deliberately lazy:
    /// a plain click arms a sweep it usually never uses, and cloning the
    /// mark set on every click would be a cost nobody asked for).
    ///
    /// Dropped by every listing change ([`Self::set_listing`],
    /// [`Self::begin_loading`], [`Self::refill`]): a baseline is a claim
    /// about entries that were listed, and restoring it over a listing that
    /// moved underneath would resurrect marks the prune already dropped.
    sweep_baseline: Option<HashSet<VPath>>,
    /// Bumped by every change that can MOVE an index into
    /// [`Self::entries`]: a new listing, a cd, a refill, a page of an
    /// incremental fill, a re-sort, a hidden-entries toggle. Read through
    /// [`Self::listing_epoch`].
    ///
    /// It exists because an index is the only thing a pointer gesture
    /// carries. A frontend resolves a click against the frame it painted
    /// and then feeds that index back — to a sweep, to a double click —
    /// and between the two the listing can move underneath. This is the
    /// one signal that says so, so that a frontend can drop a gesture that
    /// has stopped meaning anything instead of applying it to whatever now
    /// occupies that row.
    ///
    /// Deliberately NOT the same thing as dropping `sweep_baseline`: that
    /// one is a claim about mark IDENTITY (paths), and it is kept where it
    /// already was.
    listing_epoch: u64,
    /// Extent (`lo..=hi`, clamped nowhere) the sweep in progress applied
    /// last, so the next [`Self::apply_sweep`] can give back exactly the
    /// rows that left the range instead of rebuilding the mark set.
    sweep_extent: Option<(usize, usize)>,
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
    /// Filas de listado que el frontend pintó de este pane en el ÚLTIMO
    /// frame (#124). El alto real lo decide el widget al pintar, así que el
    /// modelo no puede deducirlo: el frontend lo DEVUELVE con
    /// [`Self::set_viewport_rows`] y de ahí salen el salto de página
    /// ([`Self::page_step`]) y el radio de la sonda de stat
    /// ([`Self::needs_stat_window`]) — antes eran constantes que mentían en
    /// cualquier terminal que no midiera justo eso. `None` = todavía sin
    /// pintar (o pane tapado, p. ej. con el visor abierto): manda el
    /// fallback del caller.
    viewport_rows: Option<usize>,
    /// La primera fila VISIBLE del listado: la ventana, que es PEGAJOSA.
    ///
    /// Antes se deducía del cursor en cada frame (`selected - (alto-1)`), y
    /// eso ancla el cursor a la ÚLTIMA fila: pasada la primera pantalla, cada
    /// pulsación movía el contenido en vez del cursor, y al volver hacia
    /// arriba la lista bajaba con él sin que el cursor se despegara del borde.
    /// Un gestor ortodoxo hace lo contrario — el cursor se mueve DENTRO de la
    /// ventana y solo la arrastra al tocar un borde—, y para eso la ventana
    /// tiene que recordar dónde estaba.
    ///
    /// Se reconcilia una vez por frame ([`Self::reconcile_viewport`]), ANTES
    /// de pintar: el pintado y el hit test del ratón leen los dos este mismo
    /// número, que es lo que impide que un click caiga en otra fila.
    viewport_offset: usize,
}

/// Salto de página sin frame pintado todavía (#124): el valor histórico,
/// solo hasta el primer [`PaneState::set_viewport_rows`].
pub const DEFAULT_PAGE: usize = 10;

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
            sweep_baseline: None,
            sweep_extent: None,
            listing_epoch: 0,
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
            viewport_rows: None,
            viewport_offset: 0,
            fila_de_subir: FilaDeSubir::Apagada,
        }
    }

    /// Enciende o apaga la fila `..` de este listado (`[ui] parent_entry`).
    ///
    /// Se pide una vez, al montar el pane, y se conserva por listado: es
    /// configuración, no estado de navegación.
    pub fn set_parent_row(&mut self, on: bool) {
        if on != matches!(self.fila_de_subir, FilaDeSubir::Apagada) {
            return;
        }
        self.quitar_padre();
        self.fila_de_subir = if on {
            FilaDeSubir::Pedida
        } else {
            FilaDeSubir::Apagada
        };
        self.poner_padre();
    }

    /// ¿La fila `i` es la de `..`?
    ///
    /// Lo preguntan los dos renderers —para pintar `..` en vez del nombre del
    /// directorio padre— y la navegación. Nadie más debería necesitarlo: lo
    /// que evita que esa fila sea el OPERANDO de una operación es que
    /// [`Self::selected`] devuelve `None` sobre ella, no que cada sitio se
    /// acuerde de preguntar.
    #[must_use]
    pub fn is_parent_row(&self, i: usize) -> bool {
        self.tiene_padre() && i == 0
    }

    /// A dónde lleva la fila `..`, si la hay: el directorio padre.
    #[must_use]
    pub fn parent_target(&self) -> Option<&VPath> {
        self.tiene_padre().then(|| &self.entries[0].path)
    }

    /// ¿Hay fila de padre AHORA MISMO en `entries`?
    ///
    /// Es un campo y no una comparación de rutas: una entrada de verdad puede
    /// apuntar al mismo sitio que el padre —un enlace, un montaje— y
    /// preguntarlo por la ruta convertiría esa entrada en «la fila de subir».
    /// El campo dice lo que de verdad se metió.
    const fn tiene_padre(&self) -> bool {
        matches!(self.fila_de_subir, FilaDeSubir::Puesta)
    }

    /// Da por NO puesta la fila, sin tocar `entries`: para cuando el listado
    /// se reemplaza entero y lo que hubiera se fue con él.
    const fn olvidar_padre(&mut self) {
        if let FilaDeSubir::Puesta = self.fila_de_subir {
            self.fila_de_subir = FilaDeSubir::Pedida;
        }
    }

    /// Mete la fila `..` al principio, si toca y no está ya.
    ///
    /// Se llama al FINAL de todo lo que reconstruye `entries`. Su pareja
    /// [`Self::quitar_padre`] va al principio, y las dos juntas son lo que
    /// permite que ordenar, filtrar y rellenar sigan trabajando sobre un
    /// listado de entradas REALES — una fila sintética metida en un merge por
    /// clave de orden es una fila que se duplica o se pierde.
    ///
    /// En una raíz no aparece por mucho que la configuración la encienda: no
    /// hay a dónde subir, y una fila que no lleva a ningún sitio es peor que
    /// no tenerla.
    fn poner_padre(&mut self) {
        if !matches!(self.fila_de_subir, FilaDeSubir::Pedida) {
            return;
        }
        let Some(padre) = self.dir.parent() else {
            return;
        };
        let fila = Entry {
            attrs: std::collections::BTreeMap::new(),
            path: padre,
            kind: norte_proto::EntryKind::Dir,
            // Ni tamaño ni fecha: no son de este directorio, y ponerlos sería
            // contestar por el padre sin haberlo mirado.
            size: None,
            mtime_ms: None,
        };
        // La clave se computa de SU entrada, que es el invariante que el
        // orden exige: una clave ajena compara mal en cuanto haya un empate.
        self.sort_keys.insert(0, crate::sort::sort_key(&fila));
        self.entries.insert(0, fila);
        self.fila_de_subir = FilaDeSubir::Puesta;
    }

    /// Saca la fila `..` si está puesta.
    ///
    /// Al PRINCIPIO de lo que reconstruye el listado, para que lo que ordena
    /// y mergea solo vea entradas de verdad.
    fn quitar_padre(&mut self) {
        if !self.tiene_padre() {
            return;
        }
        self.entries.remove(0);
        self.sort_keys.remove(0);
        self.fila_de_subir = FilaDeSubir::Pedida;
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
        // La fila `..` sale ANTES de particionar y vuelve después: no es una
        // entrada del listado, así que ni se oculta ni se guarda en el stash.
        self.quitar_padre();
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
        self.poner_padre();
        self.listing_moved();
        self.cursor = anchor
            .and_then(|p| self.entries.iter().position(|e| e.path == p))
            .unwrap_or_else(|| self.cursor.min(self.entries.len().saturating_sub(1)));
        self.sweep_baseline = None;
        self.sweep_extent = None;
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
        // La fila `..` sale ANTES de reordenar y vuelve después: no participa
        // del orden, va siempre primera. Ordenarla con las demás la mandaría
        // al medio del listado en cuanto alguien ordene por tamaño.
        self.quitar_padre();
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
        self.poner_padre();
        self.listing_moved();
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
        // El dir cambió: la fila `..` de antes apuntaba a otro padre.
        self.olvidar_padre();
        self.poner_padre();
        self.listing_moved();
        self.cursor = 0;
        self.loading = false;
        self.quick = None;
        self.marks.clear();
        self.sweep_baseline = None;
        self.sweep_extent = None;
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
        self.olvidar_padre();
        self.poner_padre();
        self.listing_moved();
        self.cursor = 0;
        self.loading = true;
        self.quick = None;
        self.marks.clear();
        self.sweep_baseline = None;
        self.sweep_extent = None;
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
        // La fila `..` NO es un operando, y este es EL sitio donde se decide.
        //
        // Ochenta y siete llamadas preguntan por «lo señalado» para copiarlo,
        // borrarlo, renombrarlo o mirarlo dentro, y ninguna tiene por qué
        // saber que existe una fila que no es un fichero. Contestando `None`
        // —que todas ya saben tratar: es «no hay nada señalado»— la fila deja
        // de ser peligrosa por construcción, en vez de por acordarse.
        //
        // Subir con ella no pasa por aquí: eso es `parent_target`, y lo mira
        // quien navega.
        if self.is_parent_row(self.cursor) {
            return None;
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

    /// How many times this pane's listing has MOVED (see the
    /// `listing_epoch` field). Opaque and monotonic: compare two readings,
    /// never interpret the number.
    ///
    /// The frontends hold it next to the geometry they painted, so that an
    /// in-flight pointer gesture whose indices no longer name what the user
    /// saw is dropped rather than applied to the new listing.
    ///
    /// ```
    /// use norte_frontend::PaneState;
    /// use norte_proto::VPath;
    ///
    /// let dir = VPath::parse("mem:///d").unwrap();
    /// let mut p = PaneState::new(dir.clone(), Vec::new());
    /// let antes = p.listing_epoch();
    /// p.set_listing(dir, Vec::new());
    /// assert_ne!(p.listing_epoch(), antes, "otro listado, otros índices");
    /// ```
    #[must_use]
    pub fn listing_epoch(&self) -> u64 {
        self.listing_epoch
    }

    /// Records that the indices of [`Self::entries`] may have moved.
    ///
    /// Called from EVERY site that touches `entries` — one line each,
    /// rather than a guess derived from the length (a re-sort keeps the
    /// length and moves every index) or from the directory (a refill of the
    /// same directory moves them too).
    ///
    /// Saturating: a session that overflowed a `u64` of listing changes is
    /// not reachable, and wrapping back onto the epoch a frontend is
    /// holding is the one outcome worth ruling out.
    fn listing_moved(&mut self) {
        self.listing_epoch = self.listing_epoch.saturating_add(1);
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
        // La fila `..` sale antes del MERGE: el merge empareja por clave de
        // orden, y una fila sintética metida ahí se duplicaría o acabaría en
        // medio del listado.
        self.quitar_padre();
        let (batch, batch_keys) = crate::sort::sort_with_keys_spec(batch, self.sort);
        crate::sort::merge_keyed_spec(
            &mut self.entries,
            &mut self.sort_keys,
            batch,
            batch_keys,
            self.sort,
        );
        self.poner_padre();
        // Una página de un relleno paginado también MUEVE índices: el
        // merge inserta en su sitio ordenado, no al final.
        self.listing_moved();
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
    pub fn refill(&mut self, mut entries: Vec<Entry>) {
        let quick_prev = self.quick_selected_path();
        self.inherit_known_metadata(&mut entries);
        // #107: el refill trae el listado COMPLETO del dir — el stash se
        // reconstruye fresco de él, nunca se acumula con el anterior.
        self.hidden_stash.clear();
        let entries = self.stash_hidden(entries);
        let (entries, sort_keys) = crate::sort::sort_with_keys_spec(entries, self.sort);
        self.entries = entries;
        self.sort_keys = sort_keys;
        // El listado se rehízo entero: la fila vuelve, y el cursor se acota
        // DESPUÉS de ponerla —si no, con un listado que encoge se quedaría
        // una fila más arriba de lo que hay.
        self.olvidar_padre();
        self.poner_padre();
        self.cursor = self.cursor.min(self.entries.len().saturating_sub(1));
        self.listing_moved();
        self.sweep_baseline = None;
        self.sweep_extent = None;
        self.pruned_marks = self.prune_marks();
        if let Some(q) = &mut self.quick {
            q.refresh(&self.entries, quick_prev.as_ref());
        }
        self.quick_sync_jump();
    }

    /// Traslada a `entries` el size/mtime que este pane YA conocía para el
    /// mismo path, solo donde el listado nuevo no lo trae.
    ///
    /// El motivo es visual y concreto: un `fs.list` local no hace `stat` de
    /// cada entrada (#52), así que un refresco del MISMO dir llega pelado y,
    /// instalado tal cual, vacía las columnas de tamaño y fecha hasta que la
    /// sonda las rellena. Con el watcher (#106) refrescando en cada evento
    /// del directorio eso se ve como un parpadeo continuo. Heredar no
    /// congela nada: un listado que sí trae el dato gana, y
    /// [`Self::hydrate`] —la sonda— gana a ambos.
    fn inherit_known_metadata(&self, entries: &mut [Entry]) {
        // Los ocultos cuentan: el toggle de ocultación los devuelve al
        // listado y perderían el dato si solo mirásemos lo visible.
        let known: HashMap<&VPath, (Option<u64>, Option<i64>)> = self
            .entries
            .iter()
            .chain(self.hidden_stash.iter())
            .filter(|e| e.size.is_some() || e.mtime_ms.is_some())
            .map(|e| (&e.path, (e.size, e.mtime_ms)))
            .collect();
        if known.is_empty() {
            return;
        }
        for entry in entries {
            if let Some(&(size, mtime_ms)) = known.get(&entry.path) {
                entry.size = entry.size.or(size);
                entry.mtime_ms = entry.mtime_ms.or(mtime_ms);
            }
        }
    }

    /// Hidrata size/mtime de la entrada `path` (stat on-demand, #52). No-op si
    /// la entrada ya no está (un refresh la pisó). No reordena: size/mtime no
    /// participan en el sort.
    ///
    /// La sonda es AUTORITATIVA: acaba de mirar el fichero, así que su valor
    /// pisa el que hubiera (que puede venir heredado de antes del refresco,
    /// ver `inherit_known_metadata` — sin esto, un fichero que crece
    /// mostraría para siempre el tamaño con el que se listó la primera vez).
    /// Lo que NO pisa es con `None`: un stat que falla o un provider que no
    /// sabe el dato jamás borra uno que sí se conocía.
    /// (#107 review MINOR-5, aceptado: un stat que resuelve tras moverse su
    /// entrada al stash de ocultos se pierde — al re-mostrar, la fila pinta
    /// `None` hasta la siguiente sonda de foco. Autocurativo y barato.)
    pub fn hydrate(&mut self, path: &VPath, size: Option<u64>, mtime_ms: Option<i64>) {
        if let Some(e) = self.entries.iter_mut().find(|e| &e.path == path) {
            e.size = size.or(e.size);
            e.mtime_ms = mtime_ms.or(e.mtime_ms);
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

    /// Un pane sobre un subdirectorio, que es donde la fila `..` aparece.
    fn pane_hijo(names: &[&str]) -> PaneState {
        let es = names
            .iter()
            .map(|n| e(&format!("mem:///casa/{n}"), EntryKind::File))
            .collect();
        let mut p = PaneState::new(VPath::parse("mem:///casa").unwrap(), es);
        p.set_parent_row(true);
        p
    }

    /// La fila `..` es la PRIMERA, y en una raíz no aparece: no hay a dónde
    /// subir, y una fila que no lleva a ningún sitio es peor que no tenerla.
    #[test]
    fn la_fila_de_subir_va_primera_y_no_esta_en_la_raiz() {
        let p = pane_hijo(&["a", "b"]);
        assert_eq!(p.entries().len(), 3, "las dos entradas y la de subir");
        assert!(p.is_parent_row(0));
        assert!(!p.is_parent_row(1));
        assert_eq!(p.parent_target(), Some(&VPath::parse("mem:///").unwrap()));

        let mut raiz = pane(&["a"]);
        raiz.set_parent_row(true);
        assert_eq!(raiz.entries().len(), 1, "en la raíz no hay fila de subir");
        assert!(!raiz.is_parent_row(0));
        assert_eq!(raiz.parent_target(), None);
    }

    /// Y NO es un operando. Este es el invariante que hace segura la fila:
    /// ochenta y siete sitios preguntan «qué hay señalado» para copiarlo o
    /// borrarlo, y sobre ella la respuesta es «nada».
    #[test]
    fn la_fila_de_subir_no_es_un_operando() {
        let mut p = pane_hijo(&["a"]);
        assert_eq!(p.cursor(), 0, "el cursor nace encima de ella");
        assert!(
            p.selected().is_none(),
            "sobre `..` no hay nada señalado: si hubiera, F8 borraría el padre"
        );
        p.cursor_down();
        assert!(p.selected().is_some(), "y sobre una entrada de verdad, sí");
    }

    /// Ni se puede marcar, POR NINGUNO de los caminos que marcan.
    ///
    /// Marcarla metería el directorio PADRE en la lista de lo que se copia o
    /// se borra, que es la peor forma de este bug. El test recorre todas las
    /// puertas: la del cursor, la de bloque, la de rango, la de una fila
    /// suelta, el invertir y el patrón.
    #[test]
    fn la_fila_de_subir_no_se_marca_por_ningun_camino() {
        let padre = VPath::parse("mem:///").unwrap();
        let mut p = pane_hijo(&["a", "b"]);

        p.toggle_mark(); // el cursor está sobre `..`
        p.mark_all();
        p.mark_range(0, 2);
        p.set_mark(0, true);
        p.invert_marks();
        let _ = p.mark_glob("*", true);
        assert!(
            !p.marked_paths().contains(&padre),
            "el padre JAMÁS entra en lo marcado: {:?}",
            p.marked_paths()
        );
        // Y lo demás sí se marca: la guarda protege una fila, no rompe el
        // marcado.
        assert_eq!(p.marks_len(), 2, "las dos entradas de verdad");
    }

    /// Reordenar no la mueve del sitio: va primera, no se ordena con las
    /// demás. Ordenar por tamaño la mandaría al medio del listado.
    #[test]
    fn la_fila_de_subir_sigue_primera_tras_reordenar() {
        use crate::sort::{SortColumn, SortDir, SortSpec};
        let mut p = pane_hijo(&["a", "b", "c"]);
        p.set_sort(SortSpec {
            column: SortColumn::Size,
            dir: SortDir::Desc,
            dirs_first: false,
        });
        assert!(p.is_parent_row(0), "sigue la primera");
        assert_eq!(p.entries().len(), 4);
    }

    /// Y un relleno paginado no la duplica ni la pierde: sale del merge y
    /// vuelve después, porque el merge empareja por clave de orden.
    #[test]
    fn un_relleno_no_duplica_la_fila_de_subir() {
        let mut p = pane_hijo(&["b"]);
        p.extend(vec![e("mem:///casa/a", EntryKind::File)]);
        p.extend(vec![e("mem:///casa/c", EntryKind::File)]);
        let subir = p
            .entries()
            .iter()
            .filter(|x| x.path == VPath::parse("mem:///").unwrap())
            .count();
        assert_eq!(subir, 1, "una sola fila de subir: {:?}", p.entries());
        assert!(p.is_parent_row(0));
        assert_eq!(p.entries().len(), 4);
    }

    /// Apagarla la quita, y encenderla la trae, sin tocar el listado.
    #[test]
    fn se_puede_apagar_y_encender() {
        let mut p = pane_hijo(&["a"]);
        assert_eq!(p.entries().len(), 2);
        p.set_parent_row(false);
        assert_eq!(p.entries().len(), 1, "solo la entrada de verdad");
        assert!(!p.is_parent_row(0));
        p.set_parent_row(true);
        assert!(p.is_parent_row(0));
    }

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

    /// #124: el alto del viewport lo devuelve el frontend tras pintar, y de
    /// ahí salen el salto de página (una pantalla menos una fila de
    /// contexto) y el radio de la sonda de stat. Sin frame pintado mandan
    /// los fallbacks.
    #[test]
    fn el_viewport_pintado_manda_en_pagina_y_sonda() {
        let lazy = |n: &str| {
            let mut x = e(&format!("mem:///{n}"), EntryKind::File);
            x.size = None;
            x
        };
        let entries: Vec<Entry> = (0..100).map(|i| lazy(&format!("f{i:03}"))).collect();
        let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), entries);
        assert_eq!(p.viewport_rows(), None, "sin pintar todavía");
        assert_eq!(p.page_step(), DEFAULT_PAGE, "sin frame: fallback histórico");
        assert_eq!(
            p.needs_stat_window(3).len(),
            4,
            "sin frame manda el fallback del caller: cursor 0 ± 3"
        );

        p.set_viewport_rows(30);
        assert_eq!(p.viewport_rows(), Some(30));
        assert_eq!(p.page_step(), 29, "una pantalla menos una fila de contexto");
        assert_eq!(
            p.needs_stat_window(3).len(),
            31,
            "el radio pasa a ser el alto REAL, no el fallback"
        );

        // Un pane de una sola fila sigue avanzando (jamás un salto de 0).
        p.set_viewport_rows(1);
        assert_eq!(p.page_step(), 1);
        // Pane tapado (visor abierto): vuelve a mandar el fallback.
        p.set_viewport_rows(0);
        assert_eq!(p.viewport_rows(), None);
        assert_eq!(p.page_step(), DEFAULT_PAGE);
    }

    /// #123: `needs_stat_at` filtra un rango ABSOLUTO explícito (el que la
    /// GUI recibe de `uniform_list`), con el mismo criterio que la ventana
    /// por radio: solo `File` sin `size`, y los índices fuera del listado se
    /// ignoran en vez de reventar.
    #[test]
    fn needs_stat_at_filtra_el_rango_explicito() {
        let lazy = |n: &str| {
            let mut x = e(&format!("mem:///{n}"), EntryKind::File);
            x.size = None;
            x
        };
        let mut ya = lazy("b");
        ya.size = Some(7);
        let mut dir = lazy("c");
        dir.kind = EntryKind::Dir;
        // `PaneState::new` ordena (dirs primero): [c, a, b, d].
        let p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![lazy("a"), ya, dir, lazy("d")],
        );
        let paths = p.needs_stat_at(0..99);
        let nombres: Vec<String> = paths
            .iter()
            .map(norte_proto::VPath::display_lossy)
            .collect();
        assert!(
            nombres.iter().any(|n| n.ends_with("/a")) && nombres.iter().any(|n| n.ends_with("/d")),
            "los File lazy del rango: {nombres:?}"
        );
        assert_eq!(paths.len(), 2, "ni el Dir ni el ya hidratado: {nombres:?}");
        assert!(
            p.needs_stat_at(50..99).is_empty(),
            "un rango fuera del listado no aporta nada"
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

    /// El refresco NO puede vaciar las columnas. Un listado fresco del MISMO
    /// dir llega SIN size/mtime (stat perezoso, #52), así que instalarlo tal
    /// cual deja las celdas de tamaño y fecha en blanco hasta que la sonda
    /// las rellena: con el watcher (#106) refrescando en cada evento, eso es
    /// un parpadeo constante. `refill` hereda por path lo que ya se sabía.
    #[test]
    fn refill_hereda_size_y_mtime_ya_conocidos() {
        let mut entries = vec![
            e("mem:///a", EntryKind::File),
            e("mem:///b", EntryKind::File),
        ];
        entries[0].size = Some(42);
        entries[0].mtime_ms = Some(1000);
        crate::sort_entries(&mut entries);
        let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), entries);

        // Lo que devuelve un `fs.list` del mismo dir: pelado.
        p.refill(vec![
            e("mem:///a", EntryKind::File),
            e("mem:///b", EntryKind::File),
        ]);

        let a = p
            .entries()
            .iter()
            .find(|x| x.path == VPath::parse("mem:///a").unwrap())
            .expect("a sigue en el listado");
        assert_eq!(a.size, Some(42), "el tamaño conocido sobrevive al refresco");
        assert_eq!(a.mtime_ms, Some(1000), "y la fecha también");
        let b = p
            .entries()
            .iter()
            .find(|x| x.path == VPath::parse("mem:///b").unwrap())
            .expect("b sigue en el listado");
        assert_eq!(b.size, None, "lo que nunca se supo sigue sin saberse");
    }

    /// La herencia anterior no puede congelar un valor rancio: el listado
    /// fresco manda cuando SÍ trae el dato (un provider que lo conoce), y la
    /// sonda posterior manda siempre — si no, un fichero que crece mostraría
    /// para siempre el tamaño con el que se listó la primera vez.
    #[test]
    fn el_dato_fresco_gana_a_la_herencia_y_la_sonda_gana_a_ambos() {
        let mut entries = vec![e("mem:///a", EntryKind::File)];
        entries[0].size = Some(42);
        let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), entries);

        let mut fresco = e("mem:///a", EntryKind::File);
        fresco.size = Some(100);
        p.refill(vec![fresco]);
        assert_eq!(p.entries()[0].size, Some(100), "el listado fresco manda");

        p.hydrate(&VPath::parse("mem:///a").unwrap(), Some(7), Some(7));
        assert_eq!(p.entries()[0].size, Some(7), "la sonda es autoritativa");
        p.hydrate(&VPath::parse("mem:///a").unwrap(), None, None);
        assert_eq!(
            p.entries()[0].size,
            Some(7),
            "una sonda que no sabe nada jamás borra lo que sí se sabe"
        );
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
        // La sonda PISA lo que hubiera: acaba de mirar el fichero. Antes se
        // conservaba el valor previo, y eso congelaba un tamaño heredado de
        // antes del refresco (ver `inherit_known_metadata`).
        p.hydrate(&VPath::parse("mem:///b").unwrap(), Some(1), Some(2));
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
            Some(1),
            "el dato recién medido gana al que ya había"
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

    // --- ratón T1: rango y marca por índice -------------------------------

    /// El rango marca en LOS DOS SENTIDOS: el ancla de un shift+click o de
    /// un barrido puede quedar por encima o por debajo del puntero, y el
    /// frontend no debe tener que ordenarlos antes de llamar. Devuelve las
    /// marcas que CAMBIÓ (convención de `mark_glob`), no el total.
    #[test]
    fn mark_range_marca_en_los_dos_sentidos() {
        let mut p = pane(&["a", "b", "c", "d"]);
        assert_eq!(p.mark_range(1, 2), 2, "b y c");
        p.clear_marks();
        assert_eq!(p.mark_range(2, 1), 2, "al revés, el MISMO rango");
        assert_eq!(p.marked_paths().len(), 2);
        assert!(p.is_marked(&e("mem:///b", EntryKind::File)));
        assert!(p.is_marked(&e("mem:///c", EntryKind::File)));
        assert!(!p.is_marked(&e("mem:///a", EntryKind::File)));
        assert!(!p.is_marked(&e("mem:///d", EntryKind::File)));
        // Solo AÑADE: re-marcar lo ya marcado cambia 0 aunque la selección
        // siga llena — el caller lee el contador, no lo confunde con el total.
        assert_eq!(p.mark_range(1, 2), 0);
        assert_eq!(p.marks_len(), 2);
    }

    /// Bajo un filtro vivo el rango alcanza SOLO lo visible, igual que
    /// `mark_all`: lo que el usuario no ve no se marca. Sin esta regla, un
    /// rango cuyos extremos abrazan una entrada oculta por el filtro la
    /// marcaría a ciegas y la siguiente operación masiva (copiar, BORRAR)
    /// se ensancharía sobre un fichero que nadie eligió.
    #[test]
    fn mark_range_bajo_filtro_solo_marca_lo_visible() {
        let mut p = pane(&["alfa", "beta", "alga"]);
        p.quick_start(Mode::Filter);
        p.quick_char('a');
        p.quick_char('l'); // deja visibles "alfa" y "alga", oculta "beta"
        let visibles = p.quick_visible().expect("filtro activo").to_vec();
        assert_eq!(visibles.len(), 2, "el filtro deja dos");
        // Rango sobre TODO el listado: los extremos abrazan la oculta.
        assert_eq!(p.mark_range(0, p.entries().len() - 1), 2);
        assert!(p.is_marked(&e("mem:///alfa", EntryKind::File)));
        assert!(p.is_marked(&e("mem:///alga", EntryKind::File)));
        assert!(
            !p.is_marked(&e("mem:///beta", EntryKind::File)),
            "beta estaba oculta por el filtro: jamás se marca"
        );
        assert_eq!(p.marks_len(), 2, "marked_paths cae al cursor: clava el SET");
    }

    /// Listado vacío (o índices fuera de rango) = no-op, sin panic: el
    /// frontend resuelve el índice desde la posición del puntero y puede
    /// llegar tarde a un pane que acaba de vaciarse.
    #[test]
    fn mark_range_en_listado_vacio_no_hace_nada() {
        let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), Vec::new());
        assert_eq!(p.mark_range(0, 0), 0);
        assert_eq!(p.mark_range(3, 9), 0);
        assert_eq!(p.marks_len(), 0);
        let mut q = pane(&["a"]);
        assert_eq!(q.mark_range(5, 7), 0, "rango entero fuera del listado");
        assert_eq!(q.marks_len(), 0);
    }

    /// Un rango que se sale del listado se RECORTA, no se rechaza: es
    /// exactamente lo que produce un hit test en el hueco bajo la última
    /// fila, y rechazarlo entero convertiría un barrido hasta el final del
    /// pane en un no-op.
    #[test]
    fn mark_range_recorta_al_listado() {
        let mut p = pane(&["a", "b", "c"]);
        assert_eq!(p.mark_range(1, 999), 2, "marca hasta la última entrada");
        assert_eq!(p.marks_len(), 2);
        assert!(!p.is_marked(&e("mem:///a", EntryKind::File)));
    }

    /// `set_mark` nombra UNA fila por índice (lo que necesita el
    /// ctrl+click), a diferencia de `toggle_mark`, que solo alcanza el
    /// cursor. Fuera de rango: no-op.
    #[test]
    fn set_mark_pone_y_quita_una_sola_entrada() {
        let mut p = pane(&["a", "b"]);
        p.set_mark(1, true);
        assert_eq!(p.marked_paths(), vec![VPath::parse("mem:///b").unwrap()]);
        p.set_mark(1, false);
        assert_eq!(p.marks_len(), 0);
        p.set_mark(9, true);
        assert_eq!(p.marks_len(), 0, "índice fuera del listado: no-op");
    }

    /// Marcar respeta el filtro y DESMARCAR no. El índice viene de un frame
    /// ya pintado contra un listado que no es estable (un fill inserta, un
    /// refill poda, un re-orden mueve): puede nombrar otra entrada, y bajo
    /// filtro una que el usuario no ve. Marcar de más ENSANCHA la siguiente
    /// operación masiva sobre un fichero que nadie eligió; desmarcar de más
    /// solo la encoge. Solo lo primero destruye datos, así que solo lo
    /// primero se rechaza.
    #[test]
    fn set_mark_marca_solo_lo_visible_pero_desmarca_siempre() {
        // Ordenado: alfa(0), alga(1), beta(2), zeta(3).
        let mut p = pane(&["alfa", "beta", "alga", "zeta"]);
        p.set_mark(2, true); // "beta" marcada ANTES de filtrar
        p.quick_start(Mode::Filter);
        p.quick_char('a');
        p.quick_char('l'); // visibles: alfa(0) y alga(1); ocultas: beta, zeta
        // Estado de partida asimétrico (lección de los tests de #103): la
        // fila que se intenta marcar NO puede estar ya marcada, o marcarla
        // de más no cambiaría el total y el test no vería nada.
        p.set_mark(3, true);
        assert_eq!(
            p.marks_len(),
            1,
            "marcar una fila oculta ('zeta'): rechazado — solo queda 'beta'"
        );
        assert!(!p.is_marked(&e("mem:///zeta", EntryKind::File)));
        p.set_mark(2, false);
        assert_eq!(
            p.marks_len(),
            0,
            "desmarcar una fila oculta: siempre permitido (solo encoge)"
        );
        p.set_mark(0, true);
        assert_eq!(p.marks_len(), 1, "la visible sí se marca");
    }

    // --- ratón T1 (fix): el barrido rebota contra su baseline -------------

    /// El barrido DEVUELVE lo que el puntero se pasó. Un barrido aditivo
    /// deja marcado todo lo que llegó a tocar: pasarse veinte filas y
    /// volver dejaba diecisiete ficheros marcados, y como el exceso ocurre
    /// en el borde del viewport bajo autoscroll, esas filas son justo las
    /// que acaban de salir de la pantalla — la siguiente operación masiva
    /// actuaría sobre ficheros invisibles de los que el usuario se echó
    /// atrás, sin más reparación que un ctrl+click por fila.
    #[test]
    fn el_barrido_devuelve_las_filas_del_exceso_al_retroceder() {
        let nombres: Vec<String> = (0..10).map(|i| format!("f{i}")).collect();
        let refs: Vec<&str> = nombres.iter().map(String::as_str).collect();
        let mut p = pane(&refs);
        p.begin_sweep();
        p.apply_sweep(2, 8); // el puntero se pasa hasta la 8
        assert_eq!(p.marks_len(), 7);
        p.apply_sweep(2, 4); // y el usuario vuelve
        assert_eq!(p.marks_len(), 3, "solo 2, 3 y 4 siguen marcadas");
        assert!(!p.is_marked(&e("mem:///f5", EntryKind::File)));
        assert!(!p.is_marked(&e("mem:///f8", EntryKind::File)));
    }

    /// La baseline es una foto del CONJUNTO de marcas: lo marcado a mano
    /// antes del gesto sobrevive a cada retroceso. Sin esto, rebotar el
    /// barrido borraría una selección que el usuario construyó con
    /// ctrl+click.
    #[test]
    fn el_barrido_conserva_lo_marcado_antes_del_gesto() {
        let mut p = pane(&["a", "b", "c", "d", "e"]);
        p.set_mark(0, true); // marcado a mano, fuera del rango del barrido
        p.begin_sweep();
        p.apply_sweep(2, 4);
        assert_eq!(p.marks_len(), 4);
        p.apply_sweep(2, 2); // retrocede del todo
        assert_eq!(p.marks_len(), 2, "queda 'a' (a mano) y 'c' (el ancla)");
        assert!(p.is_marked(&e("mem:///a", EntryKind::File)));
        assert!(p.is_marked(&e("mem:///c", EntryKind::File)));
    }

    /// `begin_sweep` suelta la baseline del gesto anterior: sin eso, un
    /// barrido nuevo restauraría la foto del anterior y borraría en
    /// silencio todo lo marcado entre medias.
    #[test]
    fn begin_sweep_no_restaura_la_baseline_del_gesto_anterior() {
        let mut p = pane(&["a", "b", "c", "d"]);
        p.begin_sweep();
        p.apply_sweep(0, 1); // gesto 1: marca a, b
        p.set_mark(3, true); // ctrl+click entre gestos
        p.begin_sweep(); // gesto 2
        p.apply_sweep(2, 2);
        assert_eq!(p.marks_len(), 4, "a, b y d sobreviven; c es del barrido");
        assert!(p.is_marked(&e("mem:///d", EntryKind::File)));
    }

    /// Un cambio de listado suelta la baseline: restaurarla sobre entradas
    /// que se movieron resucitaría marcas que el prune ya había tirado.
    #[test]
    fn un_refill_suelta_la_baseline_del_barrido() {
        let mut p = pane(&["a", "b", "c"]);
        p.begin_sweep();
        p.apply_sweep(0, 2);
        assert_eq!(p.marks_len(), 3);
        // "c" desaparece del dir; el refill poda su marca.
        p.refill(vec![
            e("mem:///a", EntryKind::File),
            e("mem:///b", EntryKind::File),
        ]);
        assert_eq!(p.pruned_marks(), 1);
        p.apply_sweep(0, 0); // el barrido sigue vivo y re-fotografía
        assert_eq!(p.marks_len(), 2, "a y b: 'c' NO resucita");
        assert_eq!(p.entries().len(), 2);
    }

    /// El barrido sigue respetando el filtro (pasa por `mark_range`), y la
    /// baseline conserva las marcas ocultas que no puede tocar.
    #[test]
    fn el_barrido_bajo_filtro_solo_alcanza_lo_visible() {
        // El listado se ordena: alfa(0), alga(1), beta(2).
        let mut p = pane(&["alfa", "beta", "alga"]);
        p.set_mark(2, true); // "beta", marcada antes de filtrar
        p.quick_start(Mode::Filter);
        p.quick_char('a');
        p.quick_char('l'); // visibles: "alfa" (0) y "alga" (1)
        p.begin_sweep();
        p.apply_sweep(0, 2);
        assert_eq!(p.marks_len(), 3, "las dos visibles + la oculta de antes");
        p.apply_sweep(0, 0);
        assert_eq!(p.marks_len(), 2, "suelta 'alga'; 'beta' oculta sobrevive");
        assert!(p.is_marked(&e("mem:///beta", EntryKind::File)));
    }

    /// El barrido re-marca lo que VUELVE a entrar en el rango. Marcar y
    /// desmarcar por delta (solo lo que entra y lo que sale) es lo que hace
    /// que un motion cueste una fila y no un repaso del listado, pero un
    /// delta mal cerrado dejaría huecos sin marcar en mitad del rango —
    /// invisibles hasta que la operación masiva se saltara un fichero.
    #[test]
    fn el_barrido_re_marca_lo_que_vuelve_a_entrar_en_el_rango() {
        let nombres: Vec<String> = (0..10).map(|i| format!("f{i}")).collect();
        let refs: Vec<&str> = nombres.iter().map(String::as_str).collect();
        let mut p = pane(&refs);
        p.begin_sweep();
        p.apply_sweep(2, 8);
        p.apply_sweep(2, 4); // retrocede: suelta 5..8
        p.apply_sweep(2, 6); // y vuelve a avanzar
        assert_eq!(p.marks_len(), 5, "2..6 sin huecos");
        for i in 2..=6 {
            assert!(
                p.is_marked(&e(&format!("mem:///f{i}"), EntryKind::File)),
                "f{i} debe seguir marcada"
            );
        }
        assert!(!p.is_marked(&e("mem:///f7", EntryKind::File)));
    }

    /// Los índices de `quick_visible` vienen en orden ASCENDENTE. No es un
    /// detalle: `is_markable` los busca en BINARIO, así que un día en que
    /// `nav` devolviera otro orden el filtro dejaría de aplicarse a filas
    /// sueltas — marcando en silencio lo que el usuario no ve.
    #[test]
    fn quick_visible_viene_en_orden_ascendente() {
        let mut p = pane(&["alfa", "beta", "alga", "zeta", "algo"]);
        p.quick_start(Mode::Filter);
        p.quick_char('a');
        p.quick_char('l');
        let vis = p.quick_visible().expect("filtro activo");
        assert!(vis.len() > 1, "hacen falta varios para ver el orden");
        assert!(
            vis.windows(2).all(|w| w[0] < w[1]),
            "índices ascendentes y sin repetir: {vis:?}"
        );
    }

    /// `apply_sweep` sin `begin_sweep` se auto-fotografía en la primera
    /// llamada: un frontend que se salte el armado sigue rebotando bien en
    /// vez de acumular.
    #[test]
    fn apply_sweep_sin_begin_se_fotografia_en_la_primera_llamada() {
        let mut p = pane(&["a", "b", "c", "d"]);
        p.set_mark(3, true);
        p.apply_sweep(0, 2);
        p.apply_sweep(0, 0);
        assert_eq!(p.marks_len(), 2, "queda 'a' (barrido) y 'd' (previa)");
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
