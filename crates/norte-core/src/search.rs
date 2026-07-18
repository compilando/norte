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

/// Matcher del NOMBRE (último segmento del path): glob O regex, EXCLUYENTES
/// por eje. Ambos aplican el fold NFC descrito en el módulo antes de comparar.
#[derive(Debug)]
pub enum NameMatcher {
    /// Glob compilado (el patrón ya viene foldeado igual que el input).
    Glob {
        /// Matcher de `globset` sobre el patrón foldeado.
        matcher: globset::GlobMatcher,
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
        Ok(Self::Glob {
            matcher: glob.compile_matcher(),
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
            } => matcher.is_match(fold_name(name_bytes, *case_sensitive)),
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
        let content = if let Some(c) = &params.content {
            if c.is_empty() {
                ContentSpec::None
            } else {
                ContentSpec::Literal(c.clone())
            }
        } else if let Some(r) = &params.content_regex {
            ContentSpec::Regex(ContentRegex::new(r, cs)?)
        } else {
            ContentSpec::None
        };
        if name.is_none() && matches!(content, ContentSpec::None) {
            return Err(SearchError::NoCriteria);
        }
        Ok(Self {
            name,
            content,
            case_sensitive: cs,
            max_hits: params.max_hits.map(|m| m as usize),
        })
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

/// Envía el lote pendiente (si lo hay). `Err(())` = el receptor murió (el
/// dueño de la búsqueda se fue): el walker debe terminar limpio.
async fn flush(tx: &mpsc::Sender<SearchHits>, batch: &mut Batch) -> Result<(), ()> {
    if batch.is_empty() {
        return Ok(());
    }
    tx.send(batch.take()).await.map_err(|_| ())
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
///   drenan cada [`FLUSH_INTERVAL`] (lo que ocurra antes).
///
/// # Errors
/// [`Error::Cancelled`] si se canceló; jamás propaga errores por-entrada (se
/// saltan). El `root` ilegible cuenta como un salto más (Completed con 0 hits).
pub async fn run_walk(
    provider: Arc<dyn Provider>,
    root: VPath,
    matchers: SearchMatchers,
    tx: mpsc::Sender<SearchHits>,
    ctx: &crate::scheduler::TaskCtx,
) -> Result<(), Error> {
    let task_id = ctx.progress.snapshot().task_id;
    let content_search = matchers.searches_content();
    let mut batch = Batch::new(task_id, content_search);
    let mut last_flush = Instant::now();
    let mut hits: usize = 0;

    let mut queue: VecDeque<VPath> = VecDeque::new();
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
                if flush(&tx, &mut batch).await.is_err() {
                    return Ok(()); // receptor muerto: termina limpio.
                }
                last_flush = Instant::now();
            }
            if ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let Ok(entry) = item else {
                ctx.progress.update(|p| p.entries_done += 1);
                continue;
            };
            ctx.progress.update(|p| {
                p.entries_done += 1;
                p.current = Some(entry.path.clone());
            });

            // Descenso: dirs sí; symlinks NO (candidato de nombre, no se sigue).
            if entry.kind == EntryKind::Dir {
                queue.push_back(entry.path.clone());
            }

            // Filtro de nombre (barato) antes de tocar el contenido.
            let name_bytes = entry.path.file_name().map_or(&[][..], Segment::as_bytes);
            let name_ok = matchers.name.as_ref().is_none_or(|m| m.matches(name_bytes));
            if !name_ok {
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
                // Truncado: drena lo pendiente y COMPLETA.
                let _ = flush(&tx, &mut batch).await;
                return Ok(());
            }
        }
    }
    let _ = flush(&tx, &mut batch).await;
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
        Detection::Binary => return Ok(None),
    };
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

/// Scan por líneas DECODIFICADAS: acumula bytes, parte en `\n` (byte 0x0A,
/// inequívoco en ASCII/UTF-8/legacy) y decodifica cada línea con `enc` para
/// aplicar `matches`. Usado por `content_regex` y por el literal en UTF-16.
async fn scan_decode_lines(
    enc: &'static Encoding,
    first: Vec<u8>,
    mut stream: ByteStream,
    cancel: &CancellationToken,
    mut matches: impl FnMut(&str) -> bool,
) -> Result<Option<MatchInfo>, Error> {
    let mut raw: Vec<u8> = Vec::new();
    let mut line_no: u64 = 0;
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
        for &b in &c {
            if b == b'\n' {
                line_no += 1;
                if let Some(info) = check_line(&raw, enc, line_no, &mut matches) {
                    return Ok(Some(info));
                }
                raw.clear();
            } else {
                raw.push(b);
            }
        }
    }
    // Última línea sin `\n` final.
    if !raw.is_empty() {
        line_no += 1;
        if let Some(info) = check_line(&raw, enc, line_no, &mut matches) {
            return Ok(Some(info));
        }
    }
    Ok(None)
}

/// Decodifica `raw` con `enc`, recorta el `\r` de CRLF y aplica `matches`;
/// devuelve el contexto si casa.
fn check_line(
    raw: &[u8],
    enc: &'static Encoding,
    line_no: u64,
    matches: &mut impl FnMut(&str) -> bool,
) -> Option<MatchInfo> {
    let decoded = norte_encoding::decode(raw, enc, true).text;
    let line = decoded.strip_suffix('\r').unwrap_or(&decoded);
    if matches(line) {
        Some(MatchInfo {
            line: Some(line_no),
            preview: Some(trim_preview(line)),
        })
    } else {
        None
    }
}

/// `true` si `line` contiene `needle` (con fold de caja simple si
/// `!case_sensitive`).
fn line_contains(line: &str, needle: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        line.contains(needle)
    } else {
        line.to_lowercase().contains(&needle.to_lowercase())
    }
}

/// Recorta el preview a [`PREVIEW_MAX_CHARS`] caracteres (por char, no byte —
/// jamás parte un char multibyte).
fn trim_preview(line: &str) -> String {
    line.chars().take(PREVIEW_MAX_CHARS).collect()
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

    // NOTA deuda T3 (fixtures de corpus, ficheros reales): `year_latin1`,
    // `year_utf16bom`, `cjk_utf8_no_casa_aguja_latin_corta` entran al corpus de
    // norte-testkit cuando el walker de contenido exista. Aquí solo se fija el
    // COMPORTAMIENTO de los matchers con bytes sintéticos.

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
