//! Matchers PUROS de fs.search (spec 2026-07-18 live search): sin I/O, sin
//! Tasks. Los consume el walker de T3.
//!
//! - **Nombre (eje GLOB)**: `lossy → NFC (+ lowercase si case-insensitive) →
//!   NFC` — la MISMA disciplina que el quick search del TUI
//!   (`norte-tui/src/nav.rs::fold`). La 2ª NFC es crítica: `to_lowercase`
//!   puede reintroducir formas descompuestas (p.ej. `J̌`→`ǰ`). La identidad
//!   del fichero JAMÁS se normaliza; esto es matching de DISPLAY.
//! - **Nombre (eje REGEX)**: NFC sobre input y patrón, pero la insensibilidad
//!   de caja la resuelve el MOTOR de `regex` (case folding simple ASCII/
//!   Unicode del propio motor) — NO el mismo fold que el glob. Difiere en
//!   casos como `İ`/`i` o `ß`/`ss` (el motor no los pliega igual que
//!   `to_lowercase`). Consciente y suficiente.
//! - **Contenido**: la AGUJA se transcodifica a los encodings candidatos de
//!   `norte_encoding::needle_cycle()` (a-ciegas) que la representen SIN
//!   pérdida (`encode_lossless`); el pajar se busca bytes-contra-bytes con
//!   `memmem` por chunks con SOLAPE — jamás se decodifica el fichero entero
//!   (spec §17.1a). Modo ENCODING-AWARE ([`ContentNeedle::for_encoding`])
//!   para cuando T3 ya detectó el encoding del fichero.
//!
//! **Límite NFC/NFD del contenido**: la búsqueda de contenido es
//! bytes-contra-bytes, sensible a la FORMA de normalización — un fichero en
//! NFD (típico de macOS) puede no casar una aguja tecleada en NFC (y
//! viceversa). Inherente a la búsqueda literal sin decodificar; no se corrige
//! en v1.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use norte_encoding::{Detection, Encoding, encode_lossless, needle_cycle};
use norte_proto::methods::{FsSearchParams, MatchInfo, SEARCH_HITS_MAX_BATCH, SearchHits};
use norte_proto::{Entry, EntryKind, Error, Segment, TaskId, VPath};
use norte_vfs::{ByteStream, Provider};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use unicode_normalization::UnicodeNormalization;

/// Tope del programa regex compilado (anti-ReDoS): una regex cuya máquina
/// exceda 1 MiB se rechaza al COMPILAR (no cuelga en runtime).
const REGEX_SIZE_LIMIT: usize = 1 << 20;

/// Fallo de compilación de un patrón de nombre. El mensaje lleva el
/// diagnóstico del compilador (glob/regex): es el propio input del requester,
/// así que exponerlo es su diagnóstico, no una fuga. El daemon (T4) lo mapea a
/// `INVALID_PARAMS`.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    /// El glob de nombre no compila.
    #[error("glob inválido: {0}")]
    BadGlob(String),
    /// La regex (de nombre o contenido) no compila o excede el `size_limit`.
    #[error("regex inválida: {0}")]
    BadRegex(String),
    /// Ningún criterio de búsqueda (ni nombre ni contenido).
    #[error("sin criterios de búsqueda")]
    NoCriteria,
    /// Dos criterios excluyentes del MISMO eje (`name_glob`+`name_regex`, o
    /// `content`+`content_regex`).
    #[error("criterios excluyentes: {0}")]
    Conflicting(&'static str),
    /// Más de [`norte_proto::methods::SEARCH_EXCLUDES_MAX`] exclusiones
    /// (0.81.0).
    ///
    /// Se comprueba ANTES de compilar ninguna, que es donde está el gasto:
    /// cada nombre es un glob y con él una regex con su presupuesto, en la
    /// tarea que atiende la conexión y sin Task que lo acote.
    #[error("demasiadas exclusiones: {0} (el tope es {1})")]
    TooManyExcludes(usize, usize),
    /// Dos filtros que no pueden cumplirse a la vez (0.81.0).
    ///
    /// Es un error y no una búsqueda de cero resultados por la misma razón
    /// que el glob y la regex del mismo eje: cero resultados se lee como «no
    /// hay nada», y aquí lo que no hay es la pregunta.
    #[error("filtros contradictorios: {0}")]
    ImpossibleFilter(&'static str),
    /// El nombre de codificación de `encoding` no es ninguno conocido
    /// (0.81.0).
    ///
    /// Es un error de la PETICIÓN y no una búsqueda que no encuentra nada: un
    /// nombre mal escrito que cayera a la detección automática devolvería
    /// resultados perfectamente creíbles leídos con otro alfabeto, y quien
    /// forzó la codificación lo hizo justamente porque el automático no le
    /// valía.
    #[error("codificación desconocida: {0}")]
    BadEncoding(String),
}

/// Envuelve un patrón entre fronteras de palabra (0.81.0).
///
/// El grupo `(?:…)` no es decorativo: sin él, un patrón con alternancia de
/// primer nivel —`gato|perro`— se leería como `\bgato` o `perro\b`, que es
/// otra búsqueda y además una que casa lo que el lector pidió excluir.
fn palabra_entera(patron: &str) -> String {
    format!(r"\b(?:{patron})\b")
}

/// Fold de nombre para el eje GLOB: `lossy → NFC → (lowercase → NFC)`. La 2ª
/// NFC re-canoniza lo que `to_lowercase` pudo descomponer. Con
/// `case_sensitive` se omite el lowercase (pero se conserva la NFC, para que
/// NFD y NFC del mismo nombre casen).
fn fold_name(name: &[u8], case_sensitive: bool) -> String {
    let nfc: String = String::from_utf8_lossy(name).nfc().collect();
    if case_sensitive {
        nfc
    } else {
        nfc.chars().flat_map(char::to_lowercase).nfc().collect()
    }
}

/// NFC del nombre en bytes para el eje REGEX (la insensibilidad de caja la
/// resuelve la propia regex con `case_insensitive`; aquí solo canonizamos).
fn nfc_name(name: &[u8]) -> String {
    String::from_utf8_lossy(name).nfc().collect()
}

/// Recompila el regex byte-mode de un [`globset::Glob`] en modo Unicode
/// (#110): globset compila con `(?-u)`, donde `?` consume UN BYTE y una
/// clase casa byte a byte — `a?o` no casaba `año` (ñ = 2 bytes). globset
/// sigue siendo la única autoridad de sintaxis; esto solo traduce su
/// salida: pela el prefijo `(?-u)` y decodifica los runs de escapes `\xNN`
/// con NN ≥ 0x80 — la única forma en que globset emite los bytes no-ASCII
/// del patrón (`&str`, así que los runs son siempre UTF-8 completo) — de
/// vuelta a sus caracteres, literales dentro y fuera de una clase.
///
/// COPIA deliberada del traductor de `norte-frontend::pane` (mismo
/// criterio que el fold, duplicado core/frontend): no hay crate común por
/// debajo de ambos donde quepa sin arrastrar `globset`+`regex` a un crate
/// ajeno. Cada copia pinea la forma de globset con su propio test guardia.
///
/// # Errors
/// [`SearchError::BadGlob`] si un run decodificado no es UTF-8 válido — no
/// debería ocurrir con la globset pineada; fail-loud antes que casar bytes
/// que el usuario no escribió.
fn unicode_glob_regex(glob: &globset::Glob) -> Result<String, SearchError> {
    let src = glob.regex();
    let stripped = src.strip_prefix("(?-u)").unwrap_or(src);
    let mut out = String::with_capacity(stripped.len());
    let mut run: Vec<u8> = Vec::new();
    let flush = |run: &mut Vec<u8>, out: &mut String| -> Result<(), SearchError> {
        if run.is_empty() {
            return Ok(());
        }
        let decoded = std::str::from_utf8(run).map_err(|_| {
            SearchError::BadGlob(
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

/// Matcher del NOMBRE (último segmento del path): glob O regex, EXCLUYENTES
/// por eje. Ambos aplican el fold NFC descrito en el módulo antes de comparar.
#[derive(Debug)]
pub enum NameMatcher {
    /// Glob recompilado en modo Unicode (#110, `unicode_glob_regex`): `?`
    /// y las clases cuentan caracteres. El patrón ya viene foldeado igual
    /// que el input.
    Glob {
        /// Regex Unicode traducida del glob foldeado.
        matcher: regex::Regex,
        /// Si `false`, input y patrón se foldean a minúsculas.
        case_sensitive: bool,
    },
    /// Regex compilada con `size_limit` y `case_insensitive`.
    Regex(regex::Regex),
}

impl NameMatcher {
    /// Compila un glob de nombre. El patrón se foldea (NFC + lowercase si
    /// `!case_sensitive`) igual que el input, de modo que la insensibilidad de
    /// caja se resuelve por el fold y no por `globset`.
    ///
    /// # Errors
    /// [`SearchError::BadGlob`] si el patrón no es un glob válido.
    pub fn glob(pattern: &str, case_sensitive: bool) -> Result<Self, SearchError> {
        let folded = fold_name(pattern.as_bytes(), case_sensitive);
        let glob = globset::GlobBuilder::new(&folded)
            .build()
            .map_err(|e| SearchError::BadGlob(e.to_string()))?;
        // Modo Unicode (#110): `?`/clases cuentan CARACTERES, no bytes.
        // Mismo `size_limit` que el eje regex — el patrón es input del
        // usuario y esta es una API pública sin tope propio de longitud.
        // `dot_matches_new_line`: globset compila su matcher con ese flag,
        // y `*`/`?` traducen a `.`-derivados — sin él, un nombre con `\n`
        // (byte legal en unix; corpus `control_newline`) dejaría de casar
        // `*` EN SILENCIO, el inverso del bug que esto arregla.
        let matcher = regex::RegexBuilder::new(&unicode_glob_regex(&glob)?)
            .size_limit(REGEX_SIZE_LIMIT)
            .dot_matches_new_line(true)
            .build()
            .map_err(|e| SearchError::BadGlob(e.to_string()))?;
        Ok(Self::Glob {
            matcher,
            case_sensitive,
        })
    }

    /// Compila una regex de nombre con `size_limit` anti-ReDoS y
    /// `case_insensitive(!case_sensitive)`. El patrón se pasa por NFC.
    ///
    /// # Errors
    /// [`SearchError::BadRegex`] si no compila o excede el `size_limit`.
    pub fn regex(pattern: &str, case_sensitive: bool) -> Result<Self, SearchError> {
        let pat = nfc_name(pattern.as_bytes());
        let re = regex::RegexBuilder::new(&pat)
            .size_limit(REGEX_SIZE_LIMIT)
            .case_insensitive(!case_sensitive)
            .build()
            .map_err(|e| SearchError::BadRegex(e.to_string()))?;
        Ok(Self::Regex(re))
    }

    /// `true` si `name_bytes` (último segmento, bytes crudos) casa. Nunca hace
    /// panic con bytes no-UTF8 (matchea sobre el lossy).
    #[must_use]
    pub fn matches(&self, name_bytes: &[u8]) -> bool {
        match self {
            Self::Glob {
                matcher,
                case_sensitive,
            } => matcher.is_match(&fold_name(name_bytes, *case_sensitive)),
            Self::Regex(re) => re.is_match(&nfc_name(name_bytes)),
        }
    }
}

/// Aguja LITERAL de contenido, ya transcodificada a los bytes de uno o más
/// encodings que la representan sin pérdida (pares `(encoding, bytes)`). La
/// búsqueda es bytes-contra-bytes (jamás decodifica el pajar).
///
/// Dos modos:
/// - [`ContentNeedle::literal`]: A-CIEGAS — la aguja en TODOS los encodings de
///   `needle_cycle()`. Fallback cuando la detección del fichero es incierta.
///   **Trade-off**: una aguja legacy corta (p.ej. `ñ`→1 byte `0xF1` en
///   windows-1252) casa POR AZAR bytes que en otro encoding forman parte de
///   una secuencia distinta (0xF1 es byte líder de un char de 4 bytes en
///   UTF-8) — falsos positivos. Ver el test que fija ese límite.
/// - [`ContentNeedle::for_encoding`]: ENCODING-AWARE — la aguja SOLO en el
///   encoding detectado (+ UTF-8). T3 lo usa tras `norte_encoding::detect`
///   para eliminar el falso positivo del modo a-ciegas.
#[derive(Debug, Clone)]
pub struct ContentNeedle {
    /// Pares `(encoding de origen, bytes)`: el encoding acompaña a la aguja
    /// para que el consumidor (T3) sea encoding-aware; `find_in` solo usa los
    /// bytes. Dedup por bytes.
    needles: Vec<(&'static Encoding, Vec<u8>)>,
    /// Longitud del needle más largo (dimensiona el solape entre chunks).
    max_len: usize,
}

impl ContentNeedle {
    /// Variantes de caja del texto: el propio texto, y con `!case_sensitive`
    /// también minúsculas y mayúsculas ANTES de codificar (fold simple: cubre
    /// agujas de caja HOMOGÉNEA; una aguja en caja mixta en el pajar es
    /// best-effort — v1 documentada).
    fn case_variants(text: &str, case_sensitive: bool) -> Vec<String> {
        let mut variants = vec![text.to_string()];
        if !case_sensitive {
            variants.push(text.to_lowercase());
            variants.push(text.to_uppercase());
        }
        variants
    }

    /// Construye la aguja codificando cada variante de caja a cada encoding de
    /// `encs` que la mapee SIN pérdida ([`encode_lossless`]); dedup por bytes.
    fn build(text: &str, case_sensitive: bool, encs: &[&'static Encoding]) -> Self {
        let variants = Self::case_variants(text, case_sensitive);
        let mut needles: Vec<(&'static Encoding, Vec<u8>)> = Vec::new();
        for variant in &variants {
            for &enc in encs {
                if let Some(bytes) = encode_lossless(enc, variant)
                    && !needles.iter().any(|(_, b)| *b == bytes)
                {
                    needles.push((enc, bytes));
                }
            }
        }
        let max_len = needles.iter().map(|(_, b)| b.len()).max().unwrap_or(0);
        Self { needles, max_len }
    }

    /// Aguja A-CIEGAS: transcodificada a TODOS los encodings de
    /// `norte_encoding::needle_cycle()` que la representen sin pérdida.
    /// Fallback cuando el encoding del fichero es incierto; asume el
    /// trade-off de falsos positivos por agujas legacy cortas (ver doc del
    /// tipo).
    #[must_use]
    pub fn literal(text: &str, case_sensitive: bool) -> Self {
        Self::build(text, case_sensitive, needle_cycle())
    }

    /// Aguja ENCODING-AWARE: solo en `enc` (el encoding YA detectado del
    /// fichero) MÁS siempre UTF-8 (red de seguridad si el detector se
    /// equivocó hacia un legacy con ASCII común). Devuelve `None` si el texto
    /// es vacío (ninguna aguja útil). Para un fichero detectado como UTF-16
    /// pásale `UTF_16LE`/`BE`: `encode_lossless` cae a UTF-8 (gotcha WHATWG),
    /// así que T3 debe buscar en el texto DECODIFICADO, no aquí — la aguja
    /// UTF-8 es lo mejor disponible en ese caso.
    #[must_use]
    pub fn for_encoding(text: &str, case_sensitive: bool, enc: &'static Encoding) -> Option<Self> {
        let n = Self::build(text, case_sensitive, &[enc, norte_encoding::UTF_8]);
        if n.needles.is_empty() { None } else { Some(n) }
    }

    /// Los needles transcodificados `(encoding, bytes)`. El walker (T3) los
    /// consume en un scan con rastreo de línea/offset que [`Self::find_in`] no
    /// expone (necesita `pos` y `\n` acumulados para el `line`/`preview`).
    #[must_use]
    pub fn needles(&self) -> &[(&'static Encoding, Vec<u8>)] {
        &self.needles
    }

    /// Longitud del needle más largo (dimensiona el solape entre chunks).
    #[must_use]
    pub fn max_len(&self) -> usize {
        self.max_len
    }

    /// Busca la aguja en `tail_anterior + chunk` con `memmem`; devuelve el
    /// offset del primer match dentro de ese buffer combinado (o `None`).
    /// Actualiza `ov` reteniendo los últimos `max_len - 1` bytes para que una
    /// aguja partida por el borde del chunk siguiente aún se encuentre.
    ///
    /// El walker (T3) para en el primer hit por fichero, así que no hay
    /// re-conteo de la cola retenida entre chunks.
    pub fn find_in(&self, ov: &mut Overlap, chunk: &[u8]) -> Option<usize> {
        let mut buf = std::mem::take(&mut ov.tail);
        buf.extend_from_slice(chunk);
        let mut hit: Option<usize> = None;
        for (_enc, needle) in &self.needles {
            if let Some(pos) = memchr::memmem::find(&buf, needle) {
                hit = Some(hit.map_or(pos, |h: usize| h.min(pos)));
            }
        }
        let keep = self.max_len.saturating_sub(1).min(buf.len());
        ov.tail = buf[buf.len() - keep..].to_vec();
        hit
    }
}

/// Estado de solape entre chunks de un fichero: retiene la cola del chunk
/// anterior para que una aguja partida por el borde se encuentre.
#[derive(Debug, Default)]
pub struct Overlap {
    /// Últimos `max_len - 1` bytes ya vistos (prefijo del siguiente buffer).
    tail: Vec<u8>,
}

/// Regex de CONTENIDO (`content_regex`): solo compila aquí con `size_limit` y
/// `case_insensitive`; se aplica sobre el texto ya decodificado por líneas en
/// el walker (T3), donde vive la decodificación por chunks.
#[derive(Debug, Clone)]
pub struct ContentRegex {
    /// Regex compilada; se corre por líneas en el walker.
    re: regex::Regex,
}

impl ContentRegex {
    /// Compila la regex de contenido con `size_limit` anti-ReDoS y
    /// `case_insensitive(!case_sensitive)`.
    ///
    /// # Errors
    /// [`SearchError::BadRegex`] si no compila o excede el `size_limit`.
    pub fn new(pattern: &str, case_sensitive: bool) -> Result<Self, SearchError> {
        let re = regex::RegexBuilder::new(pattern)
            .size_limit(REGEX_SIZE_LIMIT)
            .case_insensitive(!case_sensitive)
            .build()
            .map_err(|e| SearchError::BadRegex(e.to_string()))?;
        Ok(Self { re })
    }

    /// `true` si `line` (texto ya decodificado) casa.
    #[must_use]
    pub fn is_match(&self, line: &str) -> bool {
        self.re.is_match(line)
    }
}

// ── Walker de fs.search (T3): recorre un subtree y emite hits en lotes. ────

/// Flush por tiempo del lote de hits (calca `FILL_INTERVAL` del TUI): un lote
/// no vacío se envía al pasar este intervalo aunque no llene el batch, para
/// que el pane virtual "gotee" resultados en vivo.
const FLUSH_INTERVAL: Duration = Duration::from_millis(100);
/// Tope de caracteres del `preview` de un match (recorte server-side).
const PREVIEW_MAX_CHARS: usize = 160;
/// Cota de RAM por línea en la ruta decode (`scan_decode_lines`): una línea
/// sin `'\n'` que rebasa este tamaño se evalúa TRUNCADA a este prefijo y el
/// resto se descarta hasta el próximo `'\n'` (evita volcar un fichero de una
/// sola línea gigante en memoria).
const LINE_MATCH_CAP: usize = 1 << 20;

/// Criterio de contenido ya compilado.
enum ContentSpec {
    /// Sin búsqueda de contenido (solo nombre).
    None,
    /// Literal multi-encoding (la aguja se transcodifica por fichero según el
    /// encoding detectado; el texto crudo se retiene para el modo decode de
    /// UTF-16, ver [`run_walk`]).
    Literal(String),
    /// Regex sobre el contenido decodificado por líneas.
    Regex(ContentRegex),
}

/// Criterios de [`FsSearchParams`] ya validados y COMPILADOS (matchers de
/// nombre/contenido). Se construye ANTES de la Task: un glob/regex inválido o
/// una combinación ilegal es un error del REQUEST, no un fallo de la Task.
pub struct SearchMatchers {
    name: Option<NameMatcher>,
    content: ContentSpec,
    case_sensitive: bool,
    /// Tope de hits (ya convertido a `usize`); `None` = sin tope.
    max_hits: Option<usize>,
    /// Los filtros de 0.81.0: lo que decide si una entrada que ya casó por
    /// nombre y contenido cuenta además como resultado.
    filtros: Filtros,
    /// Nombres de carpeta que no se bajan, ya compilados a glob. Vacío = se
    /// baja a todas.
    excluir_nombres: Vec<NameMatcher>,
    /// `false` = solo el directorio raíz.
    recursivo: bool,
    /// La codificación con la que leer el contenido, si el lector forzó una
    /// (0.81.0). `None` = la que detecte cada fichero.
    encoding: Option<&'static Encoding>,
}

/// Lo que se le pide a una entrada ADEMÁS de casar por nombre o contenido
/// (protocolo 0.81.0).
///
/// Separado de los matchers porque son de otra naturaleza: aquéllos
/// compilan patrones y pueden fallar, y éstos son comparaciones sobre lo que
/// la entrada ya trae. Juntarlos haría que un rango de fechas tuviera que
/// pasar por `Result`.
#[derive(Debug, Clone, Default)]
struct Filtros {
    kinds: Vec<EntryKind>,
    min_size: Option<u64>,
    max_size: Option<u64>,
    mtime_after: Option<i64>,
    mtime_before: Option<i64>,
}

impl Filtros {
    /// ¿Cuenta esta entrada como resultado?
    ///
    /// Un dato que el provider no sabe decir NO pasa un filtro sobre él.
    /// Filtrar es afirmar, y «no lo sé» no es «sí»: un bucket de objetos que
    /// no reporta fecha devolvería su contenido entero bajo «modificado esta
    /// semana», que es peor que no devolver nada, porque se lee igual que un
    /// resultado.
    fn pasa(&self, e: &Entry) -> bool {
        if !self.kinds.is_empty() && !self.kinds.contains(&e.kind) {
            return false;
        }
        if self.min_size.is_some() || self.max_size.is_some() {
            let Some(size) = e.size else { return false };
            if self.min_size.is_some_and(|m| size < m) || self.max_size.is_some_and(|m| size > m) {
                return false;
            }
        }
        if self.mtime_after.is_some() || self.mtime_before.is_some() {
            let Some(t) = e.mtime_ms else { return false };
            if self.mtime_after.is_some_and(|m| t < m) || self.mtime_before.is_some_and(|m| t > m) {
                return false;
            }
        }
        true
    }

    /// ¿Hay algún filtro puesto? Sin ninguno no se toca nada, que es el
    /// camino de 0.80.
    fn vacios(&self) -> bool {
        self.kinds.is_empty()
            && self.min_size.is_none()
            && self.max_size.is_none()
            && self.mtime_after.is_none()
            && self.mtime_before.is_none()
    }
}

impl SearchMatchers {
    /// Valida y compila los criterios de `params`.
    ///
    /// # Errors
    /// - [`SearchError::NoCriteria`] si no hay ningún criterio.
    /// - [`SearchError::Conflicting`] si se dan `name_glob`+`name_regex`, o
    ///   `content`+`content_regex` (excluyentes por eje).
    /// - [`SearchError::BadGlob`]/[`SearchError::BadRegex`] si un patrón no
    ///   compila (o excede el `size_limit` anti-ReDoS).
    /// - [`SearchError::TooManyExcludes`] por encima de
    ///   [`norte_proto::methods::SEARCH_EXCLUDES_MAX`] (0.81.0).
    /// - [`SearchError::ImpossibleFilter`] si dos filtros no pueden cumplirse
    ///   a la vez (0.81.0).
    /// - [`SearchError::BadEncoding`] si `encoding` no nombra ninguna
    ///   codificación conocida (0.81.0).
    pub fn compile(params: &FsSearchParams) -> Result<Self, SearchError> {
        if params.name_glob.is_some() && params.name_regex.is_some() {
            return Err(SearchError::Conflicting(
                "name_glob y name_regex son excluyentes",
            ));
        }
        if params.content.is_some() && params.content_regex.is_some() {
            return Err(SearchError::Conflicting(
                "content y content_regex son excluyentes",
            ));
        }
        let cs = params.case_sensitive;
        let name = match (&params.name_glob, &params.name_regex) {
            (Some(g), _) => Some(NameMatcher::glob(g, cs)?),
            (_, Some(r)) => Some(NameMatcher::regex(r, cs)?),
            _ => None,
        };
        // Una aguja de contenido VACÍA no es un criterio (casaría con todo):
        // se trata como ausente para el cómputo de "al menos un criterio".
        // «Palabra entera» (0.81.0) se implementa SIEMPRE como regex, incluso
        // para una aguja literal, y conviene decir por qué: el camino literal
        // rápido busca BYTES, con la aguja transcodificada a varios
        // candidatos y el pajar sin decodificar. Una frontera de palabra no
        // es una propiedad de los bytes — depende de qué es letra, y eso
        // depende del alfabeto—, así que no se puede comprobar ahí sin
        // decodificar, que es justo lo que ese camino existe para no hacer.
        // Pedirla cuesta el camino rápido; no pedirla no cuesta nada.
        let content = if let Some(c) = &params.content {
            if c.is_empty() {
                ContentSpec::None
            } else if params.whole_word {
                ContentSpec::Regex(ContentRegex::new(&palabra_entera(&regex::escape(c)), cs)?)
            } else {
                ContentSpec::Literal(c.clone())
            }
        } else if let Some(r) = &params.content_regex {
            let patron = if params.whole_word {
                palabra_entera(r)
            } else {
                r.clone()
            };
            ContentSpec::Regex(ContentRegex::new(&patron, cs)?)
        } else {
            ContentSpec::None
        };
        // El tope de exclusiones, antes de compilar ninguna: ahí está el
        // gasto, y esto lo alcanza un agente.
        let tope = norte_proto::methods::SEARCH_EXCLUDES_MAX;
        for n in [params.exclude_names.len(), params.exclude_roots.len()] {
            if n > tope {
                return Err(SearchError::TooManyExcludes(n, tope));
            }
        }
        let filtros = Filtros {
            kinds: params.kinds.clone(),
            min_size: params.min_size,
            max_size: params.max_size,
            mtime_after: params.mtime_after,
            mtime_before: params.mtime_before,
        };
        // Dos filtros que no pueden cumplirse a la vez se DICEN. Cero
        // resultados se lee como «no hay nada que casara», y lo que no hay
        // es la pregunta.
        if let (Some(min), Some(max)) = (filtros.min_size, filtros.max_size)
            && min > max
        {
            return Err(SearchError::ImpossibleFilter(
                "el tamaño mínimo es mayor que el máximo",
            ));
        }
        if let (Some(desde), Some(hasta)) = (filtros.mtime_after, filtros.mtime_before)
            && desde > hasta
        {
            return Err(SearchError::ImpossibleFilter(
                "la fecha de inicio es posterior a la de fin",
            ));
        }
        // Buscar CONTENIDO solo en carpetas no puede casar nada: el
        // contenido se lee de ficheros regulares y nada más.
        let solo_carpetas = !filtros.kinds.is_empty() && !filtros.kinds.contains(&EntryKind::File);
        let pide_contenido = params.content.is_some() || params.content_regex.is_some();
        if solo_carpetas && pide_contenido {
            return Err(SearchError::ImpossibleFilter(
                "se pide contenido y se excluyen los ficheros",
            ));
        }
        // Un filtro SOLO —«todo lo que pese más de un giga»— es un criterio
        // legítimo y de los más útiles que hay. Antes «sin nombre y sin
        // contenido» era siempre «sin criterios»; ahora lo es solo cuando
        // tampoco hay filtros.
        if name.is_none() && matches!(content, ContentSpec::None) && filtros.vacios() {
            return Err(SearchError::NoCriteria);
        }
        // Los nombres a no bajar son globs sobre el último segmento, con la
        // misma disciplina que `name_glob` — incluido el fold, que es lo que
        // hace que `Target` excluya `target` en un macOS.
        let excluir_nombres = params
            .exclude_names
            .iter()
            .map(|g| NameMatcher::glob(g, cs))
            .collect::<Result<Vec<_>, _>>()?;
        // `for_label_no_replacement` y NO `for_label`: el segundo acepta las
        // etiquetas de reemplazo del estándar —`utf-7`, `hz-gb-2312`,
        // `iso-2022-cn`— y devuelve el encoding REPLACEMENT, que decodifica
        // el fichero entero a un solo U+FFFD. La búsqueda no fallaría: no
        // encontraría nada, en silencio, que es exactamente lo que el
        // rustdoc de este campo promete no hacer.
        let encoding = match &params.encoding {
            None => None,
            Some(nombre) => Some(
                Encoding::for_label_no_replacement(nombre.as_bytes())
                    .ok_or_else(|| SearchError::BadEncoding(nombre.clone()))?,
            ),
        };
        Ok(Self {
            name,
            content,
            case_sensitive: cs,
            max_hits: params.max_hits.map(|m| m as usize),
            filtros,
            excluir_nombres,
            recursivo: params.recursive,
            encoding,
        })
    }

    /// ¿Se baja a este directorio? (protocolo 0.81.0)
    ///
    /// Por NOMBRE y en cualquier nivel: la carpeta que sobra —`target`,
    /// `node_modules`, `.git`— aparece cien veces en sitios que no se saben
    /// de antemano, así que nombrarla por ruta no serviría de nada.
    fn se_baja_a(&self, e: &Entry) -> bool {
        if self.excluir_nombres.is_empty() {
            return true;
        }
        let Some(seg) = e.path.file_name() else {
            return true;
        };
        !self
            .excluir_nombres
            .iter()
            .any(|m| m.matches(seg.as_bytes()))
    }

    /// `true` si hay criterio de contenido (el walker debe leer ficheros).
    fn searches_content(&self) -> bool {
        !matches!(self.content, ContentSpec::None)
    }
}

/// Lote de hits en construcción; `matches` solo se puebla en búsquedas de
/// contenido (alineado 1:1 con `entries`).
struct Batch {
    task_id: TaskId,
    content: bool,
    entries: Vec<Entry>,
    matches: Vec<MatchInfo>,
}

impl Batch {
    fn new(task_id: TaskId, content: bool) -> Self {
        Self {
            task_id,
            content,
            entries: Vec::new(),
            matches: Vec::new(),
        }
    }

    fn push(&mut self, entry: Entry, info: Option<MatchInfo>) {
        self.entries.push(entry);
        if self.content {
            self.matches.push(info.unwrap_or(MatchInfo {
                line: None,
                preview: None,
            }));
        }
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Extrae el lote acumulado como [`SearchHits`], dejando el buffer vacío.
    fn take(&mut self) -> SearchHits {
        SearchHits {
            task_id: self.task_id,
            entries: std::mem::take(&mut self.entries),
            matches: if self.content {
                Some(std::mem::take(&mut self.matches))
            } else {
                None
            },
        }
    }
}

/// Desenlace de un [`flush`].
enum FlushOutcome {
    /// Enviado (o nada que enviar): sigue el walk.
    Continue,
    /// El receptor murió (dueño de la búsqueda se fue): termina limpio.
    ReceiverGone,
    /// Cancelado mientras el `send` estaba bloqueado (backpressure): termina
    /// como `Cancelled` (regla 3).
    Cancelled,
}

/// Envía el lote pendiente (si lo hay). Un `send` bloqueado por backpressure
/// (canal lleno + receptor lento) NO ignora la cancelación: se hace `select`
/// contra `ctx.cancel`.
async fn flush(
    tx: &mpsc::Sender<SearchHits>,
    batch: &mut Batch,
    cancel: &CancellationToken,
) -> FlushOutcome {
    if batch.is_empty() {
        return FlushOutcome::Continue;
    }
    let hits = batch.take();
    tokio::select! {
        biased;
        () = cancel.cancelled() => FlushOutcome::Cancelled,
        r = tx.send(hits) => match r {
            Ok(()) => FlushOutcome::Continue,
            Err(_) => FlushOutcome::ReceiverGone,
        },
    }
}

/// Recorre el subtree bajo `root` en BFS (iterativo, sin recursión — una
/// jerarquía hostilmente profunda no revienta la pila) emitiendo lotes de
/// hits por `tx`. Lectura pura: sin journal, sin mutaciones.
///
/// - **Cancelación** (regla 3): `ctx.cancel` se chequea por directorio, por
///   entrada y por chunk de contenido; al cancelar devuelve
///   [`Error::Cancelled`] (→ `TaskState::Cancelled`) y `tx` se dropea (el
///   canal se cierra).
/// - **Symlinks**: NO se siguen para descender (evita ciclos); un symlink SÍ
///   cuenta como candidato de NOMBRE, pero nunca se lee su contenido.
/// - **`excluded`**: subárboles que el walk NO mira — ni desciende, ni lee, ni
///   emite hit de nombre, ni los pone en `current` (que se difunde). Cuentan
///   como entrada examinada y nada más. Es lo que impide que una búsqueda
///   sobre `$HOME` de un AGENTE baje al directorio de estado del daemon
///   (#165): el gate de lectura mira la RAÍZ de la búsqueda, así que sin esto
///   una raíz legítima arrastraría el subárbol protegido con ella.
/// - **Errores por entrada**: un `list`/`read` que falla se SALTA (cuenta como
///   entrada examinada) y la búsqueda continúa — un subdir ilegible no aborta.
/// - **`max_hits`**: alcanzado el tope, envía lo pendiente y termina
///   `Completed` (no `Failed`); el cliente infiere "truncada" comparando el
///   total recibido con `max_hits`.
/// - **Progreso**: `entries_done` = entradas examinadas (incluidas las
///   saltadas por error); `bytes_done` = nº de hits acumulados (reutiliza el
///   campo, no hay bytes reales en una búsqueda); `current` = última entrada
///   vista.
/// - **Coalescing**: los hits se acumulan hasta [`SEARCH_HITS_MAX_BATCH`] o se
///   drenan cada `FLUSH_INTERVAL` (lo que ocurra antes).
///
/// # Errors
/// [`Error::Cancelled`] si se canceló; jamás propaga errores por-entrada (se
/// saltan). El `root` ilegible cuenta como un salto más (Completed con 0 hits).
pub async fn run_walk(
    provider: Arc<dyn Provider>,
    root: VPath,
    matchers: SearchMatchers,
    excluded: Vec<VPath>,
    tx: mpsc::Sender<SearchHits>,
    ctx: &crate::scheduler::TaskCtx,
) -> Result<(), Error> {
    // Una raíz que YA cae en lo excluido no se recorre: sin esto la exclusión
    // por entrada dejaría pasar el listado del propio directorio protegido.
    if excluded.iter().any(|x| crate::policy::is_under(x, &root)) {
        return Ok(());
    }
    let task_id = ctx.progress.snapshot().task_id;
    let content_search = matchers.searches_content();
    let mut batch = Batch::new(task_id, content_search);
    let mut last_flush = Instant::now();
    let mut hits: usize = 0;

    let mut queue: VecDeque<VPath> = VecDeque::new();
    // Frontera DURA del walk: NO confiamos en que `provider.list` solo devuelva
    // descendientes byte-genuinos del dir. Toda entrada se re-verifica contra
    // `confine` con `is_under` (defensa en profundidad, security T4); un provider
    // con bug (o malicioso) que liste un path fuera del root jamás filtra.
    let confine = root.clone();
    queue.push_back(root);

    while let Some(dir) = queue.pop_front() {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        // Directorio ilegible: se salta (cuenta como examinado) y sigue.
        let Ok(mut stream) = provider.list(&dir).await else {
            ctx.progress.update(|p| p.entries_done += 1);
            continue;
        };
        while let Some(item) = stream.next().await {
            // Flush por tiempo/tamaño en CADA iteración (aunque la entrada no
            // sea hit): así el pane gotea en vivo incluso escaneando fallos.
            if !batch.is_empty()
                && (batch.len() >= SEARCH_HITS_MAX_BATCH || last_flush.elapsed() >= FLUSH_INTERVAL)
            {
                match flush(&tx, &mut batch, &ctx.cancel).await {
                    FlushOutcome::Continue => last_flush = Instant::now(),
                    FlushOutcome::ReceiverGone => return Ok(()), // termina limpio
                    FlushOutcome::Cancelled => return Err(Error::Cancelled),
                }
            }
            if ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let Ok(entry) = item else {
                ctx.progress.update(|p| p.entries_done += 1);
                continue;
            };
            // Cinturón-y-tirantes: una entrada cuyo path NO cae bajo el root del
            // walk se ignora POR COMPLETO — ni descenso, ni contenido, ni hit de
            // nombre, ni se filtra en `current` (que se difunde). El scope de la
            // búsqueda es invariante del core, no de la corrección del provider.
            if !crate::policy::is_under(&confine, &entry.path) {
                ctx.progress.update(|p| p.entries_done += 1);
                continue;
            }
            // Subárbol excluido (#165): se cuenta como examinado y se deja
            // caer ENTERO, antes de tocar `current` — un path protegido no se
            // difunde ni en el progreso.
            if excluded
                .iter()
                .any(|x| crate::policy::is_under(x, &entry.path))
            {
                ctx.progress.update(|p| p.entries_done += 1);
                continue;
            }
            ctx.progress.update(|p| {
                p.entries_done += 1;
                p.current = Some(entry.path.clone());
            });

            // Descenso: dirs sí; symlinks NO (candidato de nombre, no se sigue).
            //
            // Y desde 0.81.0, tampoco si el lector pidió no recorrer
            // subdirectorios o excluyó este NOMBRE. Las dos cosas frenan el
            // descenso y solo el descenso: la carpeta sigue pudiendo ser un
            // resultado por su nombre, que es lo que quiere quien busca
            // `node_modules` mientras excluye lo que hay dentro.
            if entry.kind == EntryKind::Dir && matchers.recursivo && matchers.se_baja_a(&entry) {
                queue.push_back(entry.path.clone());
            }

            // Filtro de nombre (barato) antes de tocar el contenido.
            let name_bytes = entry.path.file_name().map_or(&[][..], Segment::as_bytes);
            let name_ok = matchers.name.as_ref().is_none_or(|m| m.matches(name_bytes));
            if !name_ok {
                continue;
            }
            // Los filtros de 0.81.0: clase, tamaño y fecha. Antes del
            // contenido a propósito — son comparaciones sobre lo que la
            // entrada ya trae, y el contenido es una LECTURA por fichero,
            // que por SFTP es una petición por cabeza.
            if !matchers.filtros.pasa(&entry) {
                continue;
            }

            let info = if content_search {
                // El contenido solo tiene sentido en ficheros regulares.
                if entry.kind != EntryKind::File {
                    continue;
                }
                match search_content(&*provider, &entry.path, &matchers, &ctx.cancel).await {
                    Ok(Some(info)) => Some(info),
                    Err(Error::Cancelled) => return Err(Error::Cancelled),
                    // No casa (o binario), o ilegible: en ambos casos se salta.
                    Ok(None) | Err(_) => continue,
                }
            } else {
                None
            };

            batch.push(entry, info);
            hits += 1;
            ctx.progress.update(|p| p.bytes_done = hits as u64);

            if matchers.max_hits.is_some_and(|max| hits >= max) {
                // Truncado: drena lo pendiente y COMPLETA (el tope se alcanzó).
                let _ = flush(&tx, &mut batch, &ctx.cancel).await;
                return Ok(());
            }
        }
    }
    let _ = flush(&tx, &mut batch, &ctx.cancel).await;
    Ok(())
}

/// Busca el criterio de contenido en UN fichero, en streaming. Devuelve el
/// contexto del primer match (`line`/`preview`) o `None` (no casa / binario /
/// vacío).
async fn search_content(
    provider: &dyn Provider,
    path: &VPath,
    matchers: &SearchMatchers,
    cancel: &CancellationToken,
) -> Result<Option<MatchInfo>, Error> {
    let mut stream = provider.read(path, None).await?;
    // Primer chunk NO vacío para la detección de encoding.
    let first = loop {
        match stream.next().await {
            Some(Ok(c)) if c.is_empty() => {}
            Some(Ok(c)) => break c,
            Some(Err(e)) => return Err(e),
            None => return Ok(None), // fichero vacío: nada que casar.
        }
    };
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let enc = match norte_encoding::detect(first.as_ref()) {
        Detection::Text { encoding, .. } => encoding,
        // Binario (NUL sin BOM): NO se busca contenido (spec §17.1a).
        //
        // También con una codificación FORZADA (0.81.0): forzar dice con qué
        // alfabeto leer un texto, no que un ejecutable sea texto. Un fichero
        // con NUL leído como windows-1252 casaría por accidente contra
        // cualquier aguja corta, y eso es ruido con forma de resultado.
        Detection::Binary => return Ok(None),
    };
    // La codificación que el lector FORZÓ manda sobre la detectada
    // (0.81.0), que es el mismo trato que le da el visor: la detección
    // acierta casi siempre, y esto es para cuando no.
    let enc = matchers.encoding.unwrap_or(enc);
    let cs = matchers.case_sensitive;
    match &matchers.content {
        ContentSpec::None => Ok(None),
        ContentSpec::Literal(text) => {
            // UTF-16 (BOM): la aguja no se transcodifica a UTF-16 (needle_cycle
            // lo excluye), así que estos ficheros se enrutan por DECODE por
            // líneas — jamás casarían en el scan de bytes.
            if is_utf16(enc) {
                let needle = text.clone();
                scan_decode_lines(enc, first.to_vec(), stream, cancel, move |line| {
                    line_contains(line, &needle, cs)
                })
                .await
            } else {
                match ContentNeedle::for_encoding(text, cs, enc) {
                    Some(needle) => scan_bytes(&needle, enc, first.to_vec(), stream, cancel).await,
                    None => Ok(None), // aguja vacía tras encode: sin match útil
                }
            }
        }
        ContentSpec::Regex(re) => {
            let re = re.clone();
            scan_decode_lines(enc, first.to_vec(), stream, cancel, move |line| {
                re.is_match(line)
            })
            .await
        }
    }
}

/// Scan LITERAL byte-contra-byte con la aguja multi-encoding y solape entre
/// chunks; rastrea la línea (contando `\n`) para el contexto del match. Es la
/// misma mecánica que [`ContentNeedle::find_in`] pero inline, porque necesita
/// `pos` y los `\n` acumulados para computar `line`/`preview` (que `find_in`
/// no expone).
async fn scan_bytes(
    needle: &ContentNeedle,
    enc: &'static Encoding,
    first: Vec<u8>,
    mut stream: ByteStream,
    cancel: &CancellationToken,
) -> Result<Option<MatchInfo>, Error> {
    let mut tail: Vec<u8> = Vec::new();
    // `\n` en los bytes que YA dejaron el `tail` (front del fichero committeado).
    let mut committed_nl: u64 = 0;
    let mut chunk = Some(first);
    loop {
        let c = match chunk.take() {
            Some(c) => c,
            None => match stream.next().await {
                Some(Ok(c)) => c.to_vec(),
                Some(Err(e)) => return Err(e),
                None => break,
            },
        };
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        // buf = tail_anterior ++ chunk (el match, si cae, está aquí).
        let mut buf = std::mem::take(&mut tail);
        buf.extend_from_slice(&c);
        let mut hit: Option<usize> = None;
        for (_e, n) in needle.needles() {
            if let Some(pos) = memchr::memmem::find(&buf, n) {
                hit = Some(hit.map_or(pos, |h: usize| h.min(pos)));
            }
        }
        if let Some(pos) = hit {
            let line = committed_nl + count_nl(&buf[..pos]) + 1;
            let preview = extract_line_preview(&buf, pos, enc);
            return Ok(Some(MatchInfo {
                line: Some(line),
                preview: Some(preview),
            }));
        }
        // Retiene los últimos `max_len-1` bytes (solape); el resto se committea.
        let keep = needle.max_len().saturating_sub(1).min(buf.len());
        let split = buf.len() - keep;
        committed_nl += count_nl(&buf[..split]);
        tail = buf[split..].to_vec();
    }
    Ok(None)
}

/// Scan por líneas DECODIFICADAS con un decodificador de ESTADO: parte en
/// `'\n'` sobre el TEXTO decodificado, no sobre el byte crudo 0x0A. Crítico
/// para UTF-16 (el `LF` es `0A 00`/`00 0A`: cortar por el byte suelto
/// desalinea los pares y pierde el match de cualquier línea ≥2). Usado por
/// `content_regex` y por el literal en UTF-16.
///
/// **Cota de RAM** ([`LINE_MATCH_CAP`]): una línea sin `'\n'` que supera el
/// tope (minificados, CSV de una línea) se evalúa TRUNCADA a ese prefijo y el
/// resto se descarta hasta el próximo `'\n'` — un match más allá del tope en
/// una única línea gigante se pierde (límite documentado; el preview ya se
/// acota a [`PREVIEW_MAX_CHARS`]).
async fn scan_decode_lines(
    enc: &'static Encoding,
    first: Vec<u8>,
    mut stream: ByteStream,
    cancel: &CancellationToken,
    mut matches: impl FnMut(&str) -> bool,
) -> Result<Option<MatchInfo>, Error> {
    let mut decoder = norte_encoding::StreamDecoder::new(enc);
    let mut pending = String::new();
    let mut line_no: u64 = 0;
    // `true` mientras se descartan los bytes de una línea ya truncada/evaluada.
    let mut skipping = false;
    let mut chunk = Some(first);
    loop {
        let (bytes, last) = match chunk.take() {
            Some(c) => (c, false),
            None => match stream.next().await {
                Some(Ok(c)) => (c.to_vec(), false),
                Some(Err(e)) => return Err(e),
                None => (Vec::new(), true),
            },
        };
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        decoder.feed(&bytes, last, &mut pending);

        // Líneas completas (por el '\n' del texto decodificado).
        while let Some(nl) = pending.find('\n') {
            let line: String = pending.drain(..=nl).collect();
            line_no += 1;
            if skipping {
                // La línea gigante ya se evaluó truncada: solo se cuenta.
                skipping = false;
                continue;
            }
            let l = strip_eol(&line);
            if matches(l) {
                return Ok(Some(MatchInfo {
                    line: Some(line_no),
                    preview: Some(trim_preview(l)),
                }));
            }
        }

        // Línea sin '\n' que rebasa el tope: evalúala truncada y descarta el
        // resto hasta el próximo '\n' (cota de RAM).
        if !skipping && pending.len() > LINE_MATCH_CAP {
            let l = strip_eol(&pending);
            if matches(l) {
                return Ok(Some(MatchInfo {
                    line: Some(line_no + 1),
                    preview: Some(trim_preview(l)),
                }));
            }
            pending.clear();
            skipping = true;
        } else if skipping {
            pending.clear();
        }

        if last {
            // Última línea sin `\n` final.
            if !skipping && !pending.is_empty() {
                line_no += 1;
                let l = strip_eol(&pending);
                if matches(l) {
                    return Ok(Some(MatchInfo {
                        line: Some(line_no),
                        preview: Some(trim_preview(l)),
                    }));
                }
            }
            return Ok(None);
        }
    }
}

/// Recorta el `\n`/`\r` finales (CRLF) de una línea drenada.
fn strip_eol(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

/// `true` si `line` contiene `needle`. NOTA: el fold de caja de esta ruta
/// (decode-por-líneas, UTF-16/`content_regex`) es `to_lowercase` simple, NO el
/// mismo que el byte-scan literal, que transcodifica variantes de caja por
/// `needle_cycle`. Difieren en casos de caja no triviales (p. ej. `ß`/`SS`);
/// asimetría consciente entre las dos rutas de contenido.
fn line_contains(line: &str, needle: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        line.contains(needle)
    } else {
        line.to_lowercase().contains(&needle.to_lowercase())
    }
}

/// Sanea el preview EN ORIGEN y lo recorta a `PREVIEW_MAX_CHARS`. El orden
/// importa: enmascara PRIMERO (controles/bidi/invisibles → `U+FFFD` vía
/// [`norte_encoding::mask_terminal_hazards`]) y recorta DESPUÉS, así el corte
/// nunca parte dentro de un isolate bidi (ya es `U+FFFD`) ni deja un override
/// sin cerrar. El productor NO manda jamás hazards crudos por wire (spec §6):
/// un consumidor (p. ej. la tool MCP de `fs.search`) puede pintar el preview
/// directo sin ejecutar ANSI ni sufrir spoofing de orden visual.
///
/// El recorte es por char, no por byte (jamás parte un char multibyte). Sigue
/// sin ser consciente de grafema/celda de terminal (un cluster combinante o un
/// char de doble ancho puede quedar cortado por el borde): el saneo-primero
/// elimina el riesgo BIDI concreto; grafema/celda queda ligado a #79/#81.
fn trim_preview(line: &str) -> String {
    norte_encoding::mask_terminal_hazards(line)
        .chars()
        .take(PREVIEW_MAX_CHARS)
        .collect()
}

/// Extrae la línea que contiene el offset `pos` dentro de `buf` (entre los
/// `\n` que la rodean, o los bordes del buffer), decodificada y recortada.
/// Límite: si la línea empezó antes del `tail` retenido (línea más larga que
/// el solape), el preview queda recortado por la izquierda — best-effort.
fn extract_line_preview(buf: &[u8], pos: usize, enc: &'static Encoding) -> String {
    let start = buf[..pos]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |i| i + 1);
    let end = buf[pos..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(buf.len(), |i| pos + i);
    let decoded = norte_encoding::decode(&buf[start..end], enc, true).text;
    let line = decoded.strip_suffix('\r').unwrap_or(&decoded);
    trim_preview(line)
}

/// Nº de `\n` en `bytes`.
fn count_nl(bytes: &[u8]) -> u64 {
    memchr::memchr_iter(b'\n', bytes).count() as u64
}

/// `true` si `enc` es UTF-16 (LE o BE) — se enruta por decode, no por byte-scan.
fn is_utf16(enc: &'static Encoding) -> bool {
    enc.name().starts_with("UTF-16")
}

#[cfg(test)]
mod tests {
    use super::*;

    use norte_testkit::MemProvider;

    fn vp(w: &str) -> VPath {
        VPath::parse(w).expect("wire")
    }

    /// `TaskCtx` mínimo para llamar a [`run_walk`] sin scheduler.
    fn ctx_de_test() -> crate::scheduler::TaskCtx {
        let (reporter, _rx) =
            crate::progress::ProgressReporter::new(TaskId::new(1), norte_proto::TaskKind::Search);
        crate::scheduler::TaskCtx {
            pause: crate::scheduler::PauseGate::default(),
            cancel: CancellationToken::new(),
            progress: Arc::new(reporter),
            actor: crate::journal::Actor::User,
        }
    }

    fn params(root: &str) -> FsSearchParams {
        FsSearchParams {
            name_glob: Some("*".into()),
            ..FsSearchParams::new(vp(root))
        }
    }

    async fn arbol() -> Arc<MemProvider> {
        let mem = Arc::new(MemProvider::new());
        for d in [
            "mem:///home",
            "mem:///home/u",
            "mem:///home/u/docs",
            "mem:///home/u/.config",
            "mem:///home/u/.config/norte",
            "mem:///home/u/.config/norte-backup",
        ] {
            mem.mkdir(&vp(d)).await.expect("mkdir");
        }
        for f in [
            "mem:///home/u/docs/carta.txt",
            "mem:///home/u/.config/norte/journal.db",
            "mem:///home/u/.config/norte-backup/journal.db",
        ] {
            let mut sink = mem.write(&vp(f)).await.expect("write");
            sink.write(bytes::Bytes::from_static(b"x"))
                .await
                .expect("chunk");
            sink.commit().await.expect("commit");
        }
        mem
    }

    async fn walk(excluded: Vec<VPath>, root: &str) -> Vec<VPath> {
        let mem = arbol().await;
        let matchers = SearchMatchers::compile(&params(root)).expect("criterios");
        let (tx, mut rx) = mpsc::channel::<SearchHits>(8);
        let ctx = ctx_de_test();
        let provider: Arc<dyn Provider> = mem;
        let h = tokio::spawn(async move {
            let mut out = Vec::new();
            while let Some(lote) = rx.recv().await {
                out.extend(lote.entries.into_iter().map(|e| e.path));
            }
            out
        });
        run_walk(provider, vp(root), matchers, excluded, tx, &ctx)
            .await
            .expect("walk completo");
        drop(ctx);
        h.await.expect("colector")
    }

    /// Lo mismo que [`walk`] pero con los params que le den, para los
    /// filtros de 0.81.0.
    async fn walk_con(p: FsSearchParams) -> Vec<VPath> {
        let mem = arbol().await;
        let root = p.root.clone();
        let matchers = SearchMatchers::compile(&p).expect("criterios");
        let (tx, mut rx) = mpsc::channel::<SearchHits>(8);
        let ctx = ctx_de_test();
        let provider: Arc<dyn Provider> = mem;
        let h = tokio::spawn(async move {
            let mut out = Vec::new();
            while let Some(lote) = rx.recv().await {
                out.extend(lote.entries.into_iter().map(|e| e.path));
            }
            out
        });
        run_walk(provider, root, matchers, Vec::new(), tx, &ctx)
            .await
            .expect("walk completo");
        drop(ctx);
        h.await.expect("colector")
    }

    /// Excluir un NOMBRE de carpeta la salta en cualquier nivel, y solo frena
    /// el DESCENSO: la carpeta sigue pudiendo ser un resultado.
    #[tokio::test]
    async fn excluir_un_nombre_no_baja_pero_no_esconde_la_carpeta() {
        let hits = walk_con(FsSearchParams {
            name_glob: Some("*".into()),
            exclude_names: vec!["norte".into()],
            ..FsSearchParams::new(vp("mem:///home/u"))
        })
        .await;
        assert!(
            hits.contains(&vp("mem:///home/u/.config/norte")),
            "la carpeta excluida sigue siendo un resultado: {hits:?}"
        );
        assert!(
            !hits.contains(&vp("mem:///home/u/.config/norte/journal.db")),
            "pero no se baja a ella: {hits:?}"
        );
        assert!(
            hits.contains(&vp("mem:///home/u/.config/norte-backup/journal.db")),
            "y el vecino que solo comparte prefijo NO se excluye: {hits:?}"
        );
    }

    /// Sin recursión se lista el directorio de la raíz y nada más.
    #[tokio::test]
    async fn sin_recursion_solo_el_directorio_de_la_raiz() {
        let hits = walk_con(FsSearchParams {
            name_glob: Some("*".into()),
            recursive: false,
            ..FsSearchParams::new(vp("mem:///home/u"))
        })
        .await;
        assert!(hits.contains(&vp("mem:///home/u/docs")), "{hits:?}");
        assert!(
            !hits.contains(&vp("mem:///home/u/docs/carta.txt")),
            "nada de un nivel más abajo: {hits:?}"
        );
    }

    /// Filtrar por CLASE no frena el recorrido: lo que se busca puede estar
    /// dentro de una carpeta que no cuenta como resultado.
    #[tokio::test]
    async fn filtrar_por_clase_no_frena_el_recorrido() {
        let hits = walk_con(FsSearchParams {
            name_glob: Some("*".into()),
            kinds: vec![EntryKind::File],
            ..FsSearchParams::new(vp("mem:///home/u"))
        })
        .await;
        assert!(
            hits.contains(&vp("mem:///home/u/docs/carta.txt")),
            "el fichero de dos niveles abajo sale: {hits:?}"
        );
        assert!(
            !hits.contains(&vp("mem:///home/u/docs")),
            "y la carpeta que hubo que atravesar no: {hits:?}"
        );
    }

    /// Un filtro SOLO, sin nombre ni contenido, es un criterio legítimo — y
    /// de los más útiles que hay («todo lo que pese más de un giga»).
    #[test]
    fn un_filtro_solo_ya_es_un_criterio() {
        let p = FsSearchParams {
            min_size: Some(1),
            ..FsSearchParams::new(vp("mem:///"))
        };
        assert!(SearchMatchers::compile(&p).is_ok());
        // Y sin nada de nada sigue sin serlo.
        let vacio = FsSearchParams::new(vp("mem:///"));
        assert!(matches!(
            SearchMatchers::compile(&vacio),
            Err(SearchError::NoCriteria)
        ));
    }

    /// Un dato que el provider no sabe decir NO pasa un filtro sobre él.
    ///
    /// Es la mitad honesta del asunto: un bucket que no reporta fecha
    /// devolvería su contenido entero bajo «modificado esta semana», y eso se
    /// lee igual que un resultado.
    #[test]
    fn lo_que_no_se_sabe_no_pasa_el_filtro() {
        let sin_datos = Entry {
            path: vp("mem:///x"),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
            attrs: std::collections::BTreeMap::new(),
        };
        let por_tamano = Filtros {
            min_size: Some(0),
            ..Filtros::default()
        };
        assert!(!por_tamano.pasa(&sin_datos), "sin tamaño no pasa");
        let por_fecha = Filtros {
            mtime_after: Some(i64::MIN),
            ..Filtros::default()
        };
        assert!(!por_fecha.pasa(&sin_datos), "sin fecha no pasa");
        // Y sin ningún filtro pasa todo, que es el camino de 0.80.
        assert!(Filtros::default().pasa(&sin_datos));
    }

    /// Una codificación que no se reconoce es un error de la PETICIÓN.
    ///
    /// Caer a la automática devolvería resultados perfectamente creíbles
    /// leídos con otro alfabeto, y quien la forzó lo hizo porque la
    /// automática no le valía.
    #[test]
    fn una_codificacion_desconocida_no_cae_a_la_automatica() {
        let p = FsSearchParams {
            content: Some("hola".into()),
            encoding: Some("no-existe-2026".into()),
            ..FsSearchParams::new(vp("mem:///"))
        };
        assert!(matches!(
            SearchMatchers::compile(&p),
            Err(SearchError::BadEncoding(_))
        ));
    }

    /// Las exclusiones tienen tope, y se comprueba ANTES de compilarlas.
    ///
    /// `fs.search` la alcanza un agente, y cada nombre excluido compila un
    /// glob y una regex con su presupuesto en la tarea que atiende la
    /// conexión — sin Task todavía, así que fuera del tope de tareas vivas.
    #[test]
    fn hay_un_tope_de_exclusiones() {
        let tope = norte_proto::methods::SEARCH_EXCLUDES_MAX;
        let muchos = vec!["x".to_owned(); tope + 1];
        let p = FsSearchParams {
            name_glob: Some("*".into()),
            exclude_names: muchos,
            ..FsSearchParams::new(vp("mem:///"))
        };
        assert!(matches!(
            SearchMatchers::compile(&p),
            Err(SearchError::TooManyExcludes(_, _))
        ));
        // Y justo en el tope pasa: la cota es inclusiva.
        let justos = vec!["x".to_owned(); tope];
        let p = FsSearchParams {
            name_glob: Some("*".into()),
            exclude_names: justos,
            ..FsSearchParams::new(vp("mem:///"))
        };
        assert!(SearchMatchers::compile(&p).is_ok());
    }

    /// Dos filtros que no pueden cumplirse a la vez se DICEN.
    ///
    /// Cero resultados se lee como «no hay nada que casara»; aquí lo que no
    /// hay es la pregunta, y son dos cosas distintas.
    #[test]
    fn los_filtros_imposibles_se_dicen() {
        let imposible = |p: FsSearchParams| {
            assert!(
                matches!(
                    SearchMatchers::compile(&p),
                    Err(SearchError::ImpossibleFilter(_))
                ),
                "debería ser imposible"
            );
        };
        imposible(FsSearchParams {
            min_size: Some(10),
            max_size: Some(1),
            ..FsSearchParams::new(vp("mem:///"))
        });
        imposible(FsSearchParams {
            mtime_after: Some(100),
            mtime_before: Some(1),
            ..FsSearchParams::new(vp("mem:///"))
        });
        // Contenido solo en carpetas: el contenido se lee de ficheros.
        imposible(FsSearchParams {
            content: Some("hola".into()),
            kinds: vec![EntryKind::Dir],
            ..FsSearchParams::new(vp("mem:///"))
        });
        // Y los rangos que sí se pueden cumplir compilan, incluido el de un
        // solo valor.
        let p = FsSearchParams {
            min_size: Some(5),
            max_size: Some(5),
            ..FsSearchParams::new(vp("mem:///"))
        };
        assert!(SearchMatchers::compile(&p).is_ok());
    }

    /// Una etiqueta de REEMPLAZO no es una codificación utilizable.
    ///
    /// `utf-7` y compañía existen en el estándar solo para que un navegador
    /// las neutralice: decodifican el fichero entero a un U+FFFD. Aceptarlas
    /// haría que la búsqueda no fallara y no encontrara nada, que es justo
    /// lo que forzar una codificación viene a evitar.
    #[test]
    fn una_etiqueta_de_reemplazo_no_vale_como_codificacion() {
        for etiqueta in ["utf-7", "hz-gb-2312", "iso-2022-cn"] {
            let p = FsSearchParams {
                content: Some("hola".into()),
                encoding: Some(etiqueta.to_owned()),
                ..FsSearchParams::new(vp("mem:///"))
            };
            assert!(
                matches!(
                    SearchMatchers::compile(&p),
                    Err(SearchError::BadEncoding(_))
                ),
                "{etiqueta} coló"
            );
        }
        // Y una de verdad sí.
        let p = FsSearchParams {
            content: Some("hola".into()),
            encoding: Some("windows-1252".into()),
            ..FsSearchParams::new(vp("mem:///"))
        };
        assert!(SearchMatchers::compile(&p).is_ok());
    }

    /// «Palabra entera» envuelve el patrón en UN grupo.
    ///
    /// Sin el grupo, `gato|perro` se leería como `\bgato` o `perro\b`: otra
    /// búsqueda, y una que casa justo lo que se pidió excluir.
    #[test]
    fn palabra_entera_agrupa_la_alternancia() {
        assert_eq!(palabra_entera("gato|perro"), r"\b(?:gato|perro)\b");
    }

    /// #165: el gate de lectura mira la RAÍZ de la búsqueda, así que una raíz
    /// legítima (`$HOME`) arrastraría el directorio de estado del daemon con
    /// ella. El walk no baja ahí — ni al dir, ni a su contenido — y el vecino
    /// que solo comparte prefijo de bytes (`norte-backup`) sí sale.
    #[tokio::test]
    async fn el_walk_no_entra_en_un_subarbol_excluido() {
        let hits = walk(vec![vp("mem:///home/u/.config/norte")], "mem:///home/u").await;
        assert!(
            hits.contains(&vp("mem:///home/u/docs/carta.txt")),
            "lo de fuera sigue saliendo: {hits:?}"
        );
        assert!(
            hits.contains(&vp("mem:///home/u/.config/norte-backup/journal.db")),
            "el vecino con el mismo prefijo NO está protegido: {hits:?}"
        );
        assert!(
            !hits
                .iter()
                .any(|p| p.to_wire().starts_with("mem:///home/u/.config/norte/")),
            "nada de dentro del subárbol protegido: {hits:?}"
        );
        assert!(
            !hits.contains(&vp("mem:///home/u/.config/norte")),
            "ni el directorio protegido mismo: {hits:?}"
        );
    }

    /// Y una raíz que YA cae en lo excluido no se recorre en absoluto: sin
    /// esto el propio listado del directorio protegido se emitiría entero.
    #[tokio::test]
    async fn una_raiz_excluida_no_da_ni_una_fila() {
        let hits = walk(
            vec![vp("mem:///home/u/.config/norte")],
            "mem:///home/u/.config/norte",
        )
        .await;
        assert!(hits.is_empty(), "{hits:?}");
    }

    #[test]
    fn glob_e_insensibilidad_nfc() {
        let m = NameMatcher::glob("*.RS", false).expect("glob");
        assert!(m.matches(b"main.rs"));
        // NFD vs NFC: "año.rs" con la ñ descompuesta casa con el glob "año*".
        let m = NameMatcher::glob("año*", false).expect("glob");
        assert!(m.matches("an\u{0303}o.rs".as_bytes()));
        // Bytes no-UTF8: no panic, matchea sobre el lossy.
        let m = NameMatcher::glob("*", false).expect("glob");
        assert!(m.matches(b"\xFF\xFE"));
    }

    /// #110 (mismo fix que el marcado por patrón del frontend): `?` y las
    /// clases cuentan CARACTERES, no bytes UTF-8 — globset a solas compila
    /// `(?-u)` byte-mode, donde `a?o` no casaba `año` (ñ = 2 bytes) y
    /// `a[ñx]o` casaba `axo` pero jamás `año`. El patrón que el usuario
    /// aprende en la búsqueda vale en el marcado y viceversa.
    #[test]
    fn glob_cuenta_caracteres_no_bytes_utf8() {
        let m = NameMatcher::glob("a?o.txt", false).expect("glob");
        assert!(m.matches("a\u{f1}o.txt".as_bytes()), "? = un carácter");
        assert!(m.matches(b"axo.txt"));
        let m = NameMatcher::glob("a[\u{f1}x]o.txt", false).expect("glob");
        assert!(m.matches("a\u{f1}o.txt".as_bytes()), "clase con multibyte");
        assert!(m.matches(b"axo.txt"));
        let m = NameMatcher::glob("a[\u{f0}-\u{f2}]o.txt", false).expect("glob");
        assert!(m.matches("a\u{f1}o.txt".as_bytes()), "rango multibyte");
        assert!(!m.matches(b"axo.txt"));
        // Astral (4 bytes UTF-8): un carácter, no cuatro.
        let m = NameMatcher::glob("?.txt", false).expect("glob");
        assert!(m.matches("\u{1D11E}.txt".as_bytes()), "𝄞 = UN carácter");
    }

    /// La traducción #110 debe CONSERVAR `dot_matches_new_line` (globset
    /// compila su matcher con él): `\n` es un byte legal de nombre en unix
    /// (corpus `control_newline`) y `*`/`?` traducen a `.`-derivados —
    /// perder el flag haría que `*` dejara de casar esos nombres EN
    /// SILENCIO, el inverso del bug byte/carácter.
    #[test]
    fn glob_sigue_casando_nombres_con_newline() {
        let m = NameMatcher::glob("*", false).expect("glob");
        assert!(m.matches(b"a\nb"));
        let m = NameMatcher::glob("a?b", false).expect("glob");
        assert!(m.matches(b"a\nb"), "? tambien cruza \\n, como en globset");
        let m = NameMatcher::glob("*.txt", false).expect("glob");
        assert!(m.matches(b"a\nb.txt"));
    }

    /// Guardia de la forma del regex de globset que la traducción #110
    /// decodifica (`unicode_glob_regex`): prefijo `(?-u)` y bytes no-ASCII
    /// como runs de escapes `\xNN`. Un upgrade de globset que cambie
    /// cualquiera falla AQUÍ, ruidoso, en vez de dejar de casar nombres
    /// no-ASCII en silencio. (El frontend pinea la suya igual —
    /// `globset_regex_shape_is_the_one_this_translation_expects` en
    /// `norte-frontend::pane` — porque cada lado tiene su copia del
    /// traductor, mismo criterio que el fold duplicado.)
    #[test]
    fn la_forma_del_regex_de_globset_es_la_que_la_traduccion_espera() {
        let g = globset::GlobBuilder::new("a\u{f1}o").build().expect("glob");
        assert!(g.regex().starts_with("(?-u)"), "{}", g.regex());
        assert!(g.regex().contains(r"\xc3\xb1"), "{}", g.regex());
        assert_eq!(unicode_glob_regex(&g).expect("traducción"), "^a\u{f1}o$");
    }

    #[test]
    fn regex_de_nombre_con_size_limit() {
        assert!(
            NameMatcher::regex("^ma.n\\.rs$", false)
                .expect("re")
                .matches(b"main.rs")
        );
        // Regex bomba: el size_limit (1 MiB) la rechaza al COMPILAR, no
        // cuelga. El motor de `regex` es lineal (sin backtracking
        // catastrófico), así que el guard es de MEMORIA del programa
        // compilado: `(a|aa)"×8000` supera 1 MiB (medido; ×2000 no llegaba).
        assert!(NameMatcher::regex(&"(a|aa)".repeat(8000), false).is_err());
    }

    #[test]
    fn aguja_literal_multiencoding() {
        let n = ContentNeedle::literal("año", false);
        // UTF-8:
        assert!(
            n.find_in(&mut Overlap::default(), "hay un año aquí".as_bytes())
                .is_some()
        );
        // Latin-1 (0xF1 = ñ):
        assert!(
            n.find_in(&mut Overlap::default(), b"hay un a\xF1o aqu\xED")
                .is_some()
        );
        // Y en dos chunks partiendo la aguja por la mitad (solape):
        let mut ov = Overlap::default();
        assert!(n.find_in(&mut ov, "hay un a".as_bytes()).is_none());
        assert!(
            n.find_in(&mut ov, "\u{00F1}o aqu\u{00ED}".as_bytes())
                .is_some()
        );
    }

    #[test]
    fn case_insensitive_de_contenido_ascii() {
        let n = ContentNeedle::literal("AÑO", false);
        assert!(
            n.find_in(&mut Overlap::default(), "el año".as_bytes())
                .is_some()
        );
    }

    #[test]
    fn aguja_no_representable_en_un_encoding_se_omite() {
        let n = ContentNeedle::literal("π", false);
        // UTF-8 (0xCF 0x80) sí se encuentra.
        assert!(
            n.find_in(&mut Overlap::default(), "un π aquí".as_bytes())
                .is_some()
        );
        // El encoder de windows-1252 NO puede mapear π: emitiría la referencia
        // numérica "&#960;". Ese encoding se DESCARTA (unmappable), así que su
        // representación lossy JAMÁS se convierte en un needle → no casa.
        assert!(n.find_in(&mut Overlap::default(), b"&#960;").is_none());
    }

    // Los casos de corpus con ficheros reales YA existen sobre el walker de
    // contenido (`engine_search.rs`: `contenido_encoding_aware_tres_ficheros`),
    // y la fixture canónica del falso positivo CJK vive en el corpus de
    // norte-testkit (`cjk_utf8_lead_f1`). Aquí solo se fija el COMPORTAMIENTO
    // de los matchers con bytes sintéticos.

    #[test]
    fn falso_positivo_de_aguja_latina_corta_es_limite_de_literal() {
        // "ñ" en windows-1252/ISO-8859-15 = 1 byte 0xF1. En contenido UTF-8
        // CJK, 0xF1 aparece como byte LÍDER de una secuencia de 4 bytes
        // (U+40000..U+7FFFF) → el modo a-ciegas (`literal`) casa POR AZAR.
        // Límite conocido y documentado del modo multi-aguja.
        let cjk_utf8 = b"texto \xF1\x84\x80\x81 fin"; // char U+44001 (F1 84 80 81)
        let multi = ContentNeedle::literal("ñ", false);
        assert!(
            multi.find_in(&mut Overlap::default(), cjk_utf8).is_some(),
            "límite conocido: la aguja legacy de 1 byte casa por azar"
        );
        // ENCODING-AWARE con el encoding DETECTADO (UTF-8) NO tiene el falso
        // positivo: busca 0xC3 0xB1 / 0xC3 0x91, ausentes en ese contenido.
        let aware =
            ContentNeedle::for_encoding("ñ", false, norte_encoding::UTF_8).expect("aguja no vacía");
        assert!(aware.find_in(&mut Overlap::default(), cjk_utf8).is_none());
    }

    #[test]
    fn for_encoding_dirigida_encuentra_en_su_encoding() {
        let w1252 = norte_encoding::Encoding::for_label(b"windows-1252").unwrap();
        // Fichero detectado windows-1252: la aguja dirigida encuentra 0xF1.
        let aware = ContentNeedle::for_encoding("año", false, w1252).expect("aguja");
        assert!(
            aware
                .find_in(&mut Overlap::default(), b"un a\xF1o legacy")
                .is_some()
        );
        // Y también su forma UTF-8 (red de seguridad incluida siempre).
        assert!(
            aware
                .find_in(&mut Overlap::default(), "un año utf8".as_bytes())
                .is_some()
        );
    }

    #[test]
    fn utf16_con_bom_no_lo_cubre_la_aguja_literal_limite_documentado() {
        // Fichero genuinamente UTF-16LE con BOM: "año" = FF FE 61 00 F1 00
        // 6F 00. La aguja literal NO codifica a UTF-16 (needle_cycle lo
        // excluye; `encode_lossless` a UTF-16 cae a UTF-8 por la regla WHATWG),
        // así que NO casa — los bytes UTF-8/legacy no son contiguos entre los
        // NUL. Límite documentado: T3 enruta los ficheros UTF-16-con-BOM por
        // DECODIFICACIÓN (deuda corpus: `year_utf16bom`).
        let utf16_bom = b"\xFF\xFE\x61\x00\xF1\x00\x6F\x00";
        let n = ContentNeedle::literal("año", false);
        assert!(n.find_in(&mut Overlap::default(), utf16_bom).is_none());
    }
}
