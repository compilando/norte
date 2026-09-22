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

pub use marks::MarksSummary;

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

/// Cubos del histograma de anchos de nombre: el último cuenta todo lo que
/// mide 64 celdas o más, que ya es más de lo que ningún panel le dará.
const CUBOS_DE_NOMBRE: usize = 65;

/// Suma a `cubos` el ancho (celdas) del nombre de cada una de `entries`.
///
/// Sin reservar nada por entrada: un nombre UTF-8 se mide en sitio, y solo
/// uno que no lo es pasa por la conversión con pérdida — la misma que lo
/// pinta. La reinterpretación de encoding por panel no se mira: cambia qué
/// glifos salen, no cuántos caben.
fn medir_nombres(cubos: &mut [u32; CUBOS_DE_NOMBRE], entries: &[Entry]) {
    use unicode_width::UnicodeWidthStr;
    for e in entries {
        let bytes = e.path.file_name().map_or(&[][..], |n| n.as_bytes());
        let ancho = match std::str::from_utf8(bytes) {
            Ok(s) => s.width(),
            Err(_) => String::from_utf8_lossy(bytes).width(),
        };
        let cubo = ancho.min(CUBOS_DE_NOMBRE - 1);
        cubos[cubo] = cubos[cubo].saturating_add(1);
    }
}

/// El histograma de anchos de nombre de `entries`, desde cero.
fn cubos_de(entries: &[Entry]) -> [u32; CUBOS_DE_NOMBRE] {
    let mut cubos = [0u32; CUBOS_DE_NOMBRE];
    medir_nombres(&mut cubos, entries);
    cubos
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
    /// La selección de ANTES del último gesto en bloque, para `mark.restore`
    /// (#313). UNA foto por panel, ni un historial ni algo que se persista:
    /// es la red del que pulsa «desmarcar todo» sin querer, y con dos fotos ya
    /// nadie sabría a cuál vuelve.
    ///
    /// `None` = no ha habido ningún gesto en bloque desde que este panel
    /// existe (o desde el último `cd`, que la tira: las rutas de otro
    /// directorio no nombran nada de este listado).
    marks_previous: Option<HashSet<VPath>>,
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
    /// Histograma de anchos de nombre del listado, del que sale
    /// [`Self::name_width_p80`]. Se mide cuando el listado cambia, no al
    /// pintar: es lo que [`crate::columns::fitted_columns`] intenta dar al
    /// nombre, y medirlo por frame sería recorrer el directorio entero en
    /// cada tecla. Un lote de un relleno paginado SUMA sus nombres en vez de
    /// remedir: `extend` es O(n+m) a propósito, y remedir lo haría
    /// cuadrático en un directorio grande.
    name_cubos: [u32; CUBOS_DE_NOMBRE],
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
            crate::sort::sort_with_keys_spec(entries, &crate::sort::SortSpec::default());
        let name_cubos = cubos_de(&entries);
        Self {
            name_cubos,
            dir,
            entries,
            sort_keys,
            cursor: 0,
            loading: false,
            quick: None,
            marks: HashSet::new(),
            marks_previous: None,
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
        self.sort.clone()
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
        pares.sort_by(|a, b| crate::sort::cmp_keyed_with((&a.1, &a.0), (&b.1, &b.0), &self.sort));
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
        let (entries, sort_keys) = crate::sort::sort_with_keys_spec(entries, &self.sort);
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
        // Y la foto de `mark.restore` (#313): sus rutas son del directorio
        // ANTERIOR, y restaurarlas aquí no marcaría nada o —peor— marcaría lo
        // que casualmente se llame igual.
        self.marks_previous = None;
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
        // Y la foto de `mark.restore` (#313): sus rutas son del directorio
        // ANTERIOR, y restaurarlas aquí no marcaría nada o —peor— marcaría lo
        // que casualmente se llame igual.
        self.marks_previous = None;
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

    /// Si ALGUNA entrada del listado tiene icono (ADR 0105): entonces la
    /// columna de iconos se pinta en TODAS las filas, con hueco en las que
    /// no lo tienen, para que los nombres sigan alineados. Sin ningún icono
    /// no hay columna, y el listado se ve como antes de que existiera.
    #[must_use]
    pub fn any_icon(&self) -> bool {
        self.decorations.values().any(|d| d.icon.is_some())
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
        // La fila `..` NO es un operando, y este es EL sitio donde se decide.
        //
        // Ochenta y siete llamadas preguntan por «lo señalado» para copiarlo,
        // borrarlo, renombrarlo o mirarlo dentro, y ninguna tiene por qué
        // saber que existe una fila que no es un fichero. Contestando `None`
        // —que todas ya saben tratar: es «no hay nada señalado»— la fila deja
        // de ser peligrosa por construcción, en vez de por acordarse.
        //
        // Subir con ella no pasa por aquí: eso es `parent_target`, y lo mira
        // quien navega; describirla tampoco, y eso es [`Self::cursor_entry`].
        //
        // La guarda es sobre el ÍNDICE SEÑALADO y no sobre `self.cursor`, y
        // eso arregla un agujero que llevaba aquí desde el principio: en
        // `Mode::Filter` manda la selección del quick search y el cursor real
        // no se mueve, así que abrir el filtro —cuya query vacía nace
        // señalando la fila 0— dejaba a `selected()` devolviendo el
        // directorio PADRE. F8 ahí borra el padre, que es justo lo que esta
        // fila existe para impedir.
        if self.is_parent_row(self.indice_senalado()?) {
            return None;
        }
        self.senalada()
    }

    /// La entrada bajo el cursor PARA DESCRIBIRLA, fila `..` incluida.
    ///
    /// [`Self::selected`] contesta «sobre qué se actúa» y por eso calla sobre
    /// la fila de subir. Esta contesta «qué se está señalando», que es otra
    /// pregunta y tiene otra respuesta: los paneles que siguen al cursor —la
    /// hoja de atributos, el visor acoplado— DESCRIBEN lo que hay debajo.
    ///
    /// Preguntando la primera decían «nada bajo el cursor» teniendo una fila
    /// delante, y como el cursor nace sobre `..`, el panel de detalles
    /// arrancaba vacío en cada apertura y después de cada `cd`.
    ///
    /// **No es un operando.** Quien copie, borre, renombre o mire dentro
    /// pregunta a [`Self::selected`]; esta solo vale para pintar.
    #[must_use]
    pub fn cursor_entry(&self) -> Option<&Entry> {
        self.senalada()
    }

    /// ¿Lo señalado AHORA es la fila `..`?
    ///
    /// La pregunta que acompaña a [`Self::cursor_entry`]: quien la describa
    /// necesita saber que lo es, porque la `Entry` sintética lleva la ruta
    /// del PADRE y describirla por su `file_name` afirmaría que el cursor
    /// está sobre el padre.
    ///
    /// Sale del MISMO índice que la entrada, y por eso existe: preguntando
    /// `is_parent_row(cursor())` por separado, un filtro de quick search
    /// —que elige por su cuenta y no mueve el cursor real— dejaba la bandera
    /// y la entrada hablando de filas distintas.
    #[must_use]
    pub fn cursor_is_parent_row(&self) -> bool {
        self.indice_senalado()
            .is_some_and(|i| self.is_parent_row(i))
    }

    /// La fila que el cursor —o el filtro del quick search— está señalando,
    /// sin la guarda de la fila `..`.
    fn senalada(&self) -> Option<&Entry> {
        self.entries.get(self.indice_senalado()?)
    }

    /// El ÍNDICE señalado: la selección del filtro cuando hay uno, y el
    /// cursor real cuando no.
    ///
    /// UNA respuesta, y de ella salen las tres preguntas —«qué se opera»,
    /// «qué se señala» y «¿es la fila de subir?»—. Tres cálculos
    /// independientes de lo mismo es cómo dos de ellos acaban hablando de
    /// filas distintas.
    fn indice_senalado(&self) -> Option<usize> {
        if let Some(q) = &self.quick
            && q.mode() == Mode::Filter
        {
            return q.selected_entry_index();
        }
        Some(self.cursor)
    }

    /// Directorio listado.
    #[must_use]
    pub fn dir(&self) -> &VPath {
        &self.dir
    }

    /// Entradas actuales (ordenadas por el caller), **la fila `..` incluida**.
    ///
    /// Es la lista que se PINTA, y por eso la lleva: los índices de aquí son
    /// los que responde [`Self::is_parent_row`] y los que usan el cursor, el
    /// ratón y el marcado. Lo que se copia a OTRO pane es
    /// [`Self::real_entries`].
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// SEÑALA la entrada `entrada`, sea quien sea el que manda en la selección.
    ///
    /// Es la gemela de escritura de «el índice señalado», y existe porque
    /// [`Self::set_cursor`] NO basta: con un quick search en [`Mode::Filter`]
    /// vivo, lo señalado es la selección del filtro y el cursor real no se
    /// mira, así que mover el cursor deja quieto todo lo que sigue a «lo
    /// señalado» —el visor acoplado, la hoja de atributos— mientras el gesto
    /// dice que funcionó.
    ///
    /// Con filtro se mueve la selección del quick, paso a paso por lo VISIBLE;
    /// sin filtro, el cursor. Una `entrada` que el filtro no enseña no se
    /// puede señalar: no se toca nada.
    pub fn senalar(&mut self, entrada: usize) {
        let Some(vis) = self.quick_visible() else {
            self.set_cursor(entrada);
            return;
        };
        let destino = vis.iter().position(|&real| real == entrada);
        let actual = self
            .quick()
            .and_then(QuickSearch::selected_entry_index)
            .and_then(|real| vis.iter().position(|&r| r == real));
        let (Some(actual), Some(destino)) = (actual, destino) else {
            return;
        };
        // Sin restas con signo: la dirección es un booleano y la distancia un
        // conteo, que es justo lo que `quick_down`/`quick_up` consumen.
        let (adelante, pasos) = if destino >= actual {
            (true, destino - actual)
        } else {
            (false, actual - destino)
        };
        for _ in 0..pasos {
            if adelante {
                self.quick_down();
            } else {
                self.quick_up();
            }
        }
    }

    /// A dónde apunta un gesto que adopta el OBJETIVO DEL CURSOR: la carpeta
    /// bajo el cursor si lo es, y si no el directorio de este pane.
    ///
    /// Es la regla de `Ctrl+←`/`Ctrl+→` de Krusader, literal: «on a folder:
    /// refreshes the other panel with the contents of the folder; on a file:
    /// the other panel gets the same path». Vive aquí, y no en cada frontend,
    /// porque una decisión duplicada entre los dos diverge en silencio
    /// (ADR 0077).
    ///
    /// Sobre la fila `..` devuelve el directorio de este pane, no el padre:
    /// [`Self::selected`] responde `None` ahí —es el embudo que impide que esa
    /// fila sea el operando de nada— y este gesto no es la excepción. Un
    /// enlace a un directorio tampoco cuenta: en M0 un symlink no se sigue, y
    /// mandar al otro panel a donde apunta sería seguirlo.
    #[must_use]
    pub fn target_dir(&self) -> &VPath {
        match self.selected() {
            Some(e) if e.kind == EntryKind::Dir => &e.path,
            _ => &self.dir,
        }
    }

    /// Las entradas de VERDAD: [`Self::entries`] sin la fila `..`.
    ///
    /// Lo que hay que copiar cuando un pane nace del listado de otro —partir
    /// un panel, abrir una pestaña—, porque el pane nuevo se pone la suya. Con
    /// `entries()` la heredada se quedaba como entrada normal en medio del
    /// listado, con el nombre del directorio padre y marcable: cada partición
    /// añadía una, y marcar todo metía al PADRE en lo que se copia o se borra.
    ///
    /// Lo decide el campo, no la ruta, por lo mismo que [`Self::is_parent_row`]:
    /// una entrada de verdad puede apuntar al mismo sitio que el padre.
    #[must_use]
    pub fn real_entries(&self) -> &[Entry] {
        if self.tiene_padre() {
            &self.entries[1..]
        } else {
            &self.entries
        }
    }

    /// Los nombres que puede ORGANIZAR un productor de planes (fase 8): los
    /// ficheros de este directorio, en texto.
    ///
    /// Tres filtros, y cada uno tapa un agujero que se vio pilotando:
    ///
    /// - **Sin la fila `..`** — sale de [`Self::real_entries`]. Leyendo
    ///   `entries()` a pelo, un organizer proponía mover el DIRECTORIO PADRE
    ///   dentro de una carpeta nueva; es la misma trampa que ya documenta
    ///   `real_entries`, y la razón de que esto viva aquí y no en cada
    ///   frontend.
    /// - **Sin directorios.** Organizar es archivar FICHEROS en carpetas.
    ///   Dejar entrar los directorios deja que el plan mueva una carpeta que
    ///   otro movimiento del mismo plan usa de destino: el árbol enseña
    ///   `pdf/a.pdf`, y al aplicarlo `pdf` se ha ido a otro sitio con `a.pdf`
    ///   dentro. Nada se pierde, pero lo aplicado no es lo revisado, que es
    ///   peor.
    /// - **Solo lo que es texto.** `proposed_rel` viaja UTF-8, así que un
    ///   nombre que no lo es no puede ser origen de un movimiento. Se queda
    ///   fuera en vez de viajar lossy y volver apuntando a otro fichero.
    #[must_use]
    pub fn organizable_names(&self) -> Vec<String> {
        self.real_entries()
            .iter()
            .filter(|e| e.kind != norte_proto::EntryKind::Dir)
            .filter_map(|e| e.path.file_name())
            .filter_map(|s| std::str::from_utf8(s.as_bytes()).ok().map(str::to_owned))
            .collect()
    }

    /// Los nombres que ya OCUPAN este directorio, en texto: lo que el árbol
    /// de organizar necesita para distinguir una carpeta nueva de una que ya
    /// estaba.
    ///
    /// Aquí los directorios SÍ entran —son justo los que importan— y la fila
    /// `..` sigue fuera: el padre no es una entrada de este directorio, y
    /// contarlo haría «existente» a una carpeta que se llame como él.
    #[must_use]
    pub fn existing_names(&self) -> Vec<String> {
        self.real_entries()
            .iter()
            .filter_map(|e| e.path.file_name())
            .filter_map(|s| std::str::from_utf8(s.as_bytes()).ok().map(str::to_owned))
            .collect()
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
        self.name_cubos = cubos_de(&self.entries);
    }

    /// Celdas que cubren al 80% de los nombres de este listado: lo que el
    /// nombre necesita para leerse, medido al cambiar el listado.
    #[must_use]
    pub fn name_width_p80(&self) -> u16 {
        crate::columns::name_width_p80(&self.name_cubos)
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
        // Los nombres del lote se SUMAN al histograma: remedir el listado
        // entero en cada página haría cuadrático el relleno que el merge
        // mantiene lineal.
        medir_nombres(&mut self.name_cubos, &batch);
        let (batch, batch_keys) = crate::sort::sort_with_keys_spec(batch, &self.sort);
        crate::sort::merge_keyed_spec(
            &mut self.entries,
            &mut self.sort_keys,
            batch,
            batch_keys,
            &self.sort,
        );
        self.poner_padre();
        // Una página de un relleno paginado también MUEVE índices: el
        // merge inserta en su sitio ordenado, no al final. Solo la época:
        // los nombres ya se sumaron arriba.
        self.listing_epoch = self.listing_epoch.saturating_add(1);
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
        let (entries, sort_keys) = crate::sort::sort_with_keys_spec(entries, &self.sort);
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
mod tests;
