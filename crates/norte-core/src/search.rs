//! Matchers PUROS de fs.search (spec 2026-07-18 live search): sin I/O, sin
//! Tasks. Los consume el walker de T3.
//!
//! - **Nombre**: `lossy → NFC (+ lowercase si case-insensitive) → NFC` contra
//!   glob o regex — la MISMA disciplina que el quick search del TUI
//!   (`norte-tui/src/nav.rs::fold`). La 2ª NFC es crítica: `to_lowercase`
//!   puede reintroducir formas descompuestas (p.ej. `J̌`→`ǰ`). La identidad
//!   del fichero JAMÁS se normaliza; esto es matching de DISPLAY.
//! - **Contenido**: la AGUJA se transcodifica a los encodings candidatos de
//!   `norte-encoding::reload_cycle()` que la representen SIN pérdida; el pajar
//!   se busca bytes-contra-bytes con `memmem` por chunks con SOLAPE — jamás se
//!   decodifica el fichero entero (spec §17.1a).

use norte_encoding::{Encoding, reload_cycle};
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

/// Los encodings a los que se transcodifica la AGUJA. `reload_cycle()` menos
/// UTF-16: un literal de texto en UTF-16 mete bytes NUL (que además el
/// detector clasifica como binario y el walker salta), así que sus bytes solo
/// añadirían ruido/falsos positivos sin cubrir ningún caso real (decisión v1,
/// spec §17.1a).
fn needle_encodings() -> impl Iterator<Item = &'static Encoding> {
    // Filtrado por nombre: norte-encoding aísla `encoding_rs` a propósito (no
    // re-exporta las constantes UTF_16*); `Encoding::name()` es API pública.
    reload_cycle()
        .iter()
        .copied()
        .filter(|e| e.name() != "UTF-16LE" && e.name() != "UTF-16BE")
}

/// Aguja LITERAL de contenido, ya transcodificada a los bytes de cada encoding
/// candidato que la representa sin pérdida. La búsqueda es bytes-contra-bytes
/// (jamás decodifica el pajar).
#[derive(Debug, Clone)]
pub struct ContentNeedle {
    /// Cada variante = la aguja codificada en un encoding (dedup).
    needles: Vec<Vec<u8>>,
    /// Longitud del needle más largo (dimensiona el solape entre chunks).
    max_len: usize,
}

impl ContentNeedle {
    /// Construye la aguja. Con `!case_sensitive` añade las variantes en
    /// minúsculas y mayúsculas del texto ANTES de codificar (fold simple: solo
    /// cubre agujas de una caja homogénea; una aguja en caja MIXTA en el pajar
    /// es best-effort — v1 documentada). Cada variante se codifica a cada
    /// encoding candidato (`needle_encodings`) que la mapee SIN pérdida; los
    /// encodings con caracteres no mapeables se descartan.
    #[must_use]
    pub fn literal(text: &str, case_sensitive: bool) -> Self {
        let mut variants: Vec<String> = vec![text.to_string()];
        if !case_sensitive {
            variants.push(text.to_lowercase());
            variants.push(text.to_uppercase());
        }
        let mut needles: Vec<Vec<u8>> = Vec::new();
        for variant in variants {
            for enc in needle_encodings() {
                let (bytes, _enc, had_unmappable) = enc.encode(&variant);
                if had_unmappable || bytes.is_empty() {
                    continue;
                }
                let bytes = bytes.into_owned();
                if !needles.contains(&bytes) {
                    needles.push(bytes);
                }
            }
        }
        let max_len = needles.iter().map(Vec::len).max().unwrap_or(0);
        Self { needles, max_len }
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
        for needle in &self.needles {
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
}
