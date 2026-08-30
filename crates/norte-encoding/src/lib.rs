//! Detección y decodificación de encodings de texto (spec §6): pipeline
//! BOM → heurística de binario (NUL) → chardetng, y decodificación con
//! `encoding_rs`. Aísla la superficie de esas deps (plan M1): el resto del
//! workspace consume ESTA API, jamás `chardetng`/`encoding_rs` directos.
#![forbid(unsafe_code)]

mod fold;

pub use encoding_rs::{Encoding, UTF_8};
pub use fold::{
    FoldMode, fold_delta, full_fold_expansion, has_canonical_singleton, is_canonical_singleton,
    is_default_ignorable, name_key,
};

/// Muestra de cabecera para chardetng: 64 KiB (spec §6.2).
const SNIFF_LEN: usize = 64 * 1024;

/// Resultado de la detección.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detection {
    /// Texto en el encoding detectado.
    Text {
        /// El encoding más probable.
        encoding: &'static Encoding,
        /// `true` si lo decidió un BOM (certeza, no estadística).
        bom: bool,
    },
    /// Binario (NUL sin BOM): al hexview, jamás decodificar a ciegas.
    Binary,
}

/// Detecta el encoding de `bytes` (spec §6): BOM primero (certeza), NUL
/// sin BOM = binario (sobre TODO el buffer: un binario con preámbulo
/// textual largo no cuela), y si no, chardetng sobre la cabecera
/// (64 KiB). UTF-16 SIN BOM cae a binario por diseño — recuperable
/// a mano con «recargar como…» ([`decode_forced`]).
///
/// ```
/// use norte_encoding::{Detection, detect};
/// assert!(matches!(detect("hola\n".as_bytes()), Detection::Text { .. }));
/// assert!(matches!(detect(b"PK\x03\x04\x00\x00"), Detection::Binary));
/// ```
#[must_use]
pub fn detect(bytes: &[u8]) -> Detection {
    if let Some((enc, _len)) = Encoding::for_bom(bytes) {
        return Detection::Text {
            encoding: enc,
            bom: true,
        };
    }
    if bytes.contains(&0) {
        return Detection::Binary;
    }
    let head = &bytes[..bytes.len().min(SNIFF_LEN)];
    // ISO-2022-JP fuera: su detección abre confusiones de escape en
    // contextos web; un archivo local que lo necesite usará «recargar
    // como…». UTF-8 permitido: esto no es un navegador con legado que
    // proteger — un archivo local en UTF-8 válido ES UTF-8.
    let mut det = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    det.feed(head, bytes.len() <= SNIFF_LEN);
    Detection::Text {
        encoding: det.guess(None, chardetng::Utf8Detection::Allow),
        bom: false,
    }
}

/// Texto decodificado.
#[derive(Debug, Clone)]
pub struct Decoded {
    /// El texto (con `�` donde hubo bytes inválidos).
    pub text: String,
    /// El encoding usado (informativo, para la status bar).
    pub encoding: &'static Encoding,
    /// `true` si algún byte no decodificó (el `�` es visible).
    pub had_errors: bool,
}

/// Decodifica `bytes` como `encoding`, quitando SU BOM si lo lleva.
/// `complete = false` cuando `bytes` es una CABECERA truncada: la
/// secuencia multibyte partida del final queda pendiente en vez de
/// marcarse como pérdida (un archivo válido cortado NO "tiene pérdidas").
///
/// ```
/// use norte_encoding::{UTF_8, decode};
/// assert_eq!(decode("año".as_bytes(), UTF_8, true).text, "año");
/// // ñ partida por un truncado: pendiente, no pérdida.
/// assert!(!decode(b"a\xC3", UTF_8, false).had_errors);
/// ```
#[must_use]
pub fn decode(bytes: &[u8], encoding: &'static Encoding, complete: bool) -> Decoded {
    run_decoder(encoding.new_decoder(), encoding, bytes, complete)
}

/// Como [`decode`] pero SIN honrar ningún BOM: lo que el usuario fuerza
/// con «recargar como…» MANDA (spec §6.2: siempre corregible a mano) —
/// unos bytes `FE FF` iniciales son DATO del encoding forzado.
///
/// ```
/// use norte_encoding::{Encoding, decode_forced};
/// let w1252 = Encoding::for_label(b"windows-1252").unwrap();
/// assert_eq!(decode_forced(b"\xFE\xFF!", w1252, true).text, "þÿ!");
/// ```
#[must_use]
pub fn decode_forced(bytes: &[u8], encoding: &'static Encoding, complete: bool) -> Decoded {
    run_decoder(
        encoding.new_decoder_without_bom_handling(),
        encoding,
        bytes,
        complete,
    )
}

fn run_decoder(
    mut decoder: encoding_rs::Decoder,
    encoding: &'static Encoding,
    bytes: &[u8],
    complete: bool,
) -> Decoded {
    let mut text = String::with_capacity(
        decoder
            .max_utf8_buffer_length(bytes.len())
            .unwrap_or(bytes.len()),
    );
    let (_result, _read, had_errors) = decoder.decode_to_string(bytes, &mut text, complete);
    Decoded {
        text,
        encoding,
        had_errors,
    }
}

/// Decodificador con ESTADO para consumir un fichero por chunks respetando
/// las secuencias multibyte partidas por el borde de un chunk (a diferencia de
/// [`decode`], que decodifica un buffer completo de una vez). Imprescindible
/// para partir en `'\n'` sobre el TEXTO decodificado: en UTF-16 el `LF` es
/// `0A 00`/`00 0A` y cortar por el byte crudo `0x0A` desalinea los pares.
///
/// El BOM inicial se consume (no aparece en la salida), igual que [`decode`].
///
/// ```
/// use norte_encoding::{Encoding, StreamDecoder};
/// let utf16le = Encoding::for_label(b"utf-16le").unwrap();
/// let mut dec = StreamDecoder::new(utf16le);
/// let mut out = String::new();
/// // "hi" en UTF-16LE con BOM, partido a mitad de un code unit.
/// dec.feed(&[0xFF, 0xFE, 0x68], false, &mut out); // BOM + 'h' incompleto
/// dec.feed(&[0x00, 0x69, 0x00], true, &mut out);  // resto de 'h' + 'i'
/// assert_eq!(out, "hi");
/// ```
pub struct StreamDecoder {
    decoder: encoding_rs::Decoder,
    encoding: &'static Encoding,
}

impl StreamDecoder {
    /// Crea un decodificador con estado para `encoding` (honra el BOM inicial).
    #[must_use]
    pub fn new(encoding: &'static Encoding) -> Self {
        Self {
            decoder: encoding.new_decoder(),
            encoding,
        }
    }

    /// El encoding de este decodificador.
    #[must_use]
    pub fn encoding(&self) -> &'static Encoding {
        self.encoding
    }

    /// Decodifica `bytes` y APPENDEA el texto a `out`, reteniendo internamente
    /// cualquier secuencia multibyte incompleta del final para el próximo
    /// `feed`. `last = true` en el chunk final vacía lo pendiente (los bytes
    /// colgando salen como `�`). Los bytes inválidos se sustituyen por `�`.
    pub fn feed(&mut self, bytes: &[u8], last: bool, out: &mut String) {
        out.reserve(
            self.decoder
                .max_utf8_buffer_length(bytes.len())
                .unwrap_or(bytes.len()),
        );
        let _ = self.decoder.decode_to_string(bytes, out, last);
    }
}

/// Fin de línea dominante de un texto (para la status bar del viewer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eol {
    /// Solo `\n`.
    Lf,
    /// Solo `\r\n`.
    CrLf,
    /// Solo `\r` (Mac clásico).
    Cr,
    /// Mezcla (sospechoso: merece verse).
    Mixed,
    /// Sin saltos de línea.
    None,
}

impl std::fmt::Display for Eol {
    /// Identificador TÉCNICO estable (una lib no localiza; el frontend
    /// mapea a texto de UI).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Lf => "LF",
            Self::CrLf => "CRLF",
            Self::Cr => "CR",
            Self::Mixed => "mixed",
            Self::None => "none",
        })
    }
}

/// Detecta el EOL de un texto ya decodificado.
///
/// ```
/// use norte_encoding::{Eol, detect_eol};
/// assert_eq!(detect_eol("a\r\nb\r\n"), Eol::CrLf);
/// ```
#[must_use]
pub fn detect_eol(text: &str) -> Eol {
    let bytes = text.as_bytes();
    let (mut lf, mut crlf, mut cr) = (0usize, 0usize, 0usize);
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' if bytes.get(i + 1) == Some(&b'\n') => {
                crlf += 1;
                i += 2;
            }
            b'\r' => {
                cr += 1;
                i += 1;
            }
            b'\n' => {
                lf += 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    match (lf > 0, crlf > 0, cr > 0) {
        (false, false, false) => Eol::None,
        (true, false, false) => Eol::Lf,
        (false, true, false) => Eol::CrLf,
        (false, false, true) => Eol::Cr,
        _ => Eol::Mixed,
    }
}

/// El ciclo de «recargar como…» del viewer: los encodings que un usuario
/// real necesita probar a mano (cubre el corpus del testkit; ampliable).
///
/// ```
/// assert!(norte_encoding::reload_cycle().len() >= 10);
/// ```
#[must_use]
pub fn reload_cycle() -> &'static [&'static Encoding] {
    const CYCLE: &[&Encoding] = &[
        encoding_rs::UTF_8,
        encoding_rs::WINDOWS_1252,
        encoding_rs::ISO_8859_15,
        // GB18030 (la etiqueta que nombra la spec; superset de GBK).
        encoding_rs::GB18030,
        encoding_rs::SHIFT_JIS,
        encoding_rs::EUC_JP,
        encoding_rs::BIG5,
        encoding_rs::KOI8_R,
        encoding_rs::UTF_16LE,
        encoding_rs::UTF_16BE,
    ];
    CYCLE
}

/// Los encodings a los que transcodificar una AGUJA para búsqueda literal
/// de contenido (fs.search): [`reload_cycle`] MENOS UTF-16LE/BE.
///
/// La decisión vive JUNTO a los datos (no en el consumidor por nombre).
/// Codificar a UTF-16 con `encoding_rs` NO produce bytes UTF-16: la regla
/// WHATWG «output encoding» mapea UTF-16LE/BE a UTF-8 (ver el gotcha de
/// [`encode_lossless`]), así que como aguja solo duplicarían la de UTF-8 —
/// inútiles. Un literal en un fichero GENUINAMENTE UTF-16 se busca
/// decodificando el fichero (el detector da UTF-16-con-BOM como texto;
/// sin-BOM cae a binario y el walker lo salta), no con una aguja cruda.
///
/// ```
/// assert!(norte_encoding::needle_cycle().len() == norte_encoding::reload_cycle().len() - 2);
/// ```
#[must_use]
pub fn needle_cycle() -> &'static [&'static Encoding] {
    const CYCLE: &[&Encoding] = &[
        encoding_rs::UTF_8,
        encoding_rs::WINDOWS_1252,
        encoding_rs::ISO_8859_15,
        encoding_rs::GB18030,
        encoding_rs::SHIFT_JIS,
        encoding_rs::EUC_JP,
        encoding_rs::BIG5,
        encoding_rs::KOI8_R,
    ];
    CYCLE
}

/// Codifica `text` a `enc` SIN pérdida: `None` si algún carácter no es
/// mapeable en ese encoding, o si el resultado es vacío. Encapsula el gotcha
/// de `encoding_rs::Encoding::encode` (que en un unmappable emite una
/// referencia numérica HTML `&#NNN;` en vez de fallar) exponiendo un
/// contrato honesto: bytes representables o nada.
///
/// **Gotcha UTF-16 → UTF-8**: `encoding_rs` sigue la regla WHATWG «get an
/// output encoding» — UTF-16LE/BE y `replacement` son encodings SOLO de
/// decodificación, y al codificar se sustituyen por UTF-8. Así,
/// `encode_lossless(UTF_16LE, "A")` devuelve los bytes UTF-8 `[0x41]`, NO los
/// UTF-16 `[0x41, 0x00]`. Consecuencia: esta función jamás produce bytes
/// UTF-16 reales; para buscar en un fichero genuinamente UTF-16 hay que
/// decodificarlo. Por eso [`needle_cycle`] excluye UTF-16 (solo duplicaría la
/// aguja UTF-8).
///
/// ```
/// use norte_encoding::{encode_lossless, UTF_8};
/// let w1252 = norte_encoding::Encoding::for_label(b"windows-1252").unwrap();
/// assert_eq!(encode_lossless(UTF_8, "año").unwrap(), "año".as_bytes());
/// assert_eq!(encode_lossless(w1252, "año").unwrap(), b"a\xF1o");
/// // π no es mapeable en windows-1252 → None (jamás el "&#960;" lossy):
/// assert!(encode_lossless(w1252, "π").is_none());
/// ```
#[must_use]
pub fn encode_lossless(enc: &'static Encoding, text: &str) -> Option<Vec<u8>> {
    let (bytes, _enc, had_unmappable) = enc.encode(text);
    if had_unmappable || bytes.is_empty() {
        return None;
    }
    Some(bytes.into_owned())
}

/// Rangos de `Default_Ignorable_Code_Point` (UCD `DerivedCoreProperties`,
/// Unicode 16.0) — la propiedad que dice «esto no se pinta».
///
/// Va como TABLA y no como llamada a una librería porque ninguna del árbol la
/// expone: `unicode-properties` da categoría general y emoji, y nada más. Una
/// tabla derivada de la UCD, con su versión escrita al lado y un test que la
/// recorre, es auditable; la lista de codepoints sueltos que había antes no lo
/// era — se escribió a mano, afirmaba en su rustdoc cubrir «los INVISIBLES
/// Cf/Zl/Zp», y se dejaba fuera ocho (#125), entre ellos los dos rellenos
/// Hangul, que son **Lo** y ninguna enumeración de Cf iba a coger nunca.
///
/// Ordenada: [`is_terminal_hazard`] la recorre con búsqueda binaria.
const DEFAULT_IGNORABLE: &[(char, char)] = &[
    ('\u{00AD}', '\u{00AD}'),   // SOFT HYPHEN
    ('\u{034F}', '\u{034F}'),   // COMBINING GRAPHEME JOINER
    ('\u{061C}', '\u{061C}'),   // ARABIC LETTER MARK
    ('\u{115F}', '\u{1160}'),   // rellenos HANGUL (Lo)
    ('\u{17B4}', '\u{17B5}'),   // vocales inherentes khmer
    ('\u{180B}', '\u{180F}'),   // selectores de variación mongoles + MVS
    ('\u{200B}', '\u{200F}'),   // ZWSP/ZWNJ/ZWJ/LRM/RLM
    ('\u{202A}', '\u{202E}'),   // embedding y overrides bidi
    ('\u{2060}', '\u{2064}'),   // WORD JOINER … INVISIBLE PLUS
    ('\u{2065}', '\u{2069}'),   // no asignado + isolates bidi
    ('\u{206A}', '\u{206F}'),   // deprecados de formato (NATIONAL DIGIT SHAPES…)
    ('\u{3164}', '\u{3164}'),   // HANGUL FILLER (Lo)
    ('\u{FE00}', '\u{FE0F}'),   // selectores de variación
    ('\u{FEFF}', '\u{FEFF}'),   // ZWNBSP / BOM
    ('\u{FFA0}', '\u{FFA0}'),   // HALFWIDTH HANGUL FILLER
    ('\u{FFF0}', '\u{FFF8}'),   // no asignados reservados
    ('\u{1BCA0}', '\u{1BCA3}'), // controles de formato Duployan
    ('\u{1D173}', '\u{1D17A}'), // controles de formato musical
    ('\u{E0000}', '\u{E0FFF}'), // TAG chars y selectores de variación suplementarios
];

/// Invisibles que `Default_Ignorable_Code_Point` NO cubre y que aun así se
/// pintan en blanco. Cada uno con su motivo, porque cada uno es una excepción
/// y una excepción sin motivo es una lista a la que se le añaden cosas.
/// Ordenada, como las demás: la recorre la misma búsqueda binaria.
const INVISIBLES_FUERA_DE_DI: &[(char, char)] = &[
    // Zl/Zp: separadores de línea y de párrafo, que `is_control` no coge.
    ('\u{2028}', '\u{2029}'),
    // BRAILLE PATTERN BLANK: categoría So, ni Cf ni ignorable para nadie —
    // simplemente es un braille sin puntos, o sea, un carácter en blanco de
    // ancho completo.
    ('\u{2800}', '\u{2800}'),
    // Cf, pero la UCD los EXCLUYE de DI (llevan semántica de anotación). Se
    // pintan en blanco igual, así que sirven para fabricar un gemelo.
    ('\u{FFF9}', '\u{FFFB}'),
];

/// Ignorables que se PERMITEN a sabiendas: componen emoji legítimos, y
/// enmascararlos rompería nombres reales a cambio del residual de un gemelo
/// que solo se diferencia en esto.
///
/// ZWJ une las partes de un emoji compuesto (familia, profesiones); los
/// selectores de variación eligen presentación emoji frente a texto. Ambos son
/// `Default_Ignorable`, así que sin esta excepción la propiedad los cogería.
const IGNORABLES_PERMITIDOS: &[(char, char)] = &[
    ('\u{200D}', '\u{200D}'),   // ZERO WIDTH JOINER
    ('\u{FE00}', '\u{FE0F}'),   // selectores de variación 1..16
    ('\u{E0100}', '\u{E01EF}'), // selectores de variación suplementarios
];

/// ¿Es `c` `Default_Ignorable_Code_Point`, según la MISMA tabla que usa el
/// pintado de invisibles?
///
/// Lo pregunta [`fold::is_default_ignorable`], que es la cara pública: el
/// pliegue completo los descarta antes de comparar (#214). La tabla es una y
/// las políticas son dos — `is_terminal_hazard` exime el ZWJ y los selectores
/// de variación por fidelidad de emoji, y el pliegue no puede eximir nada
/// porque el sistema de ficheros tampoco.
pub(crate) fn es_ignorable_por_defecto(c: char) -> bool {
    en_rangos(DEFAULT_IGNORABLE, c)
}

/// ¿Está `c` en alguno de los rangos ORDENADOS de `tabla`?
fn en_rangos(tabla: &[(char, char)], c: char) -> bool {
    tabla
        .binary_search_by(|(lo, hi)| {
            if c < *lo {
                std::cmp::Ordering::Greater
            } else if c > *hi {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// ¿Es `c` un peligro para un terminal? Tres familias:
///
/// - **Cc, los controles** (`\n`, ESC): un frontend directo los EJECUTARÍA —
///   inyección ANSI/OSC.
/// - **Los overrides bidi**: falsifican el ORDEN VISUAL del texto sin tocar
///   sus bytes (`202A..=202E`, `2066..=2069`).
/// - **Los invisibles**: dos textos visualmente idénticos que difieren en
///   bytes engañan a un humano, y con él a cualquier «aprueba lo que ya
///   viste». Se deciden por la propiedad Unicode
///   `Default_Ignorable_Code_Point` (`DEFAULT_IGNORABLE`) más los que se
///   pintan en blanco sin ser ignorables (`INVISIBLES_FUERA_DE_DI`).
///
/// ZWJ (`U+200D`) y los selectores de variación se PERMITEN a sabiendas
/// (`IGNORABLES_PERMITIDOS`): enmascararlos rompería los emoji compuestos —
/// fidelidad de emoji > el residual de un gemelo que solo se diferencia en
/// eso.
///
/// Fuente ÚNICA del set (spec §6: jamás controles/bidi crudos en superficies
/// de terminal). La consumen el saneo de preview de `fs.search` (productor,
/// en origen) y el `display_name`/`must_mask` de los frontends.
///
/// ```
/// use norte_encoding::is_terminal_hazard;
/// assert!(is_terminal_hazard('\u{202E}')); // RLO (bidi)
/// assert!(is_terminal_hazard('\u{001B}')); // ESC (control)
/// assert!(is_terminal_hazard('\u{FEFF}')); // BOM/ZWNBSP (invisible)
/// assert!(is_terminal_hazard('\u{3164}')); // HANGUL FILLER (Lo, #125)
/// assert!(is_terminal_hazard('\u{2800}')); // BRAILLE BLANK (So, #125)
/// assert!(!is_terminal_hazard('\u{200D}')); // ZWJ permitido (emoji)
/// assert!(!is_terminal_hazard('\u{FE0F}')); // VS16 permitido (emoji)
/// assert!(!is_terminal_hazard('a'));
/// ```
#[must_use]
pub fn is_terminal_hazard(c: char) -> bool {
    if en_rangos(IGNORABLES_PERMITIDOS, c) {
        return false;
    }
    c.is_control() || en_rangos(DEFAULT_IGNORABLE, c) || en_rangos(INVISIBLES_FUERA_DE_DI, c)
}

/// Reemplaza cada char de [`is_terminal_hazard`] por `U+FFFD` (`�`). Sanea
/// EN ORIGEN un texto destinado a pintarse: controles, overrides bidi e
/// invisibles jamás salen crudos. Mismo set y misma decisión ZWJ que
/// [`is_terminal_hazard`].
///
/// ```
/// use norte_encoding::mask_terminal_hazards;
/// // RLO + isolate sin cerrar + ESC+OSC + C0 → todos a U+FFFD:
/// let out = mask_terminal_hazards("ok \u{202E}\u{2066}\u{1B}]0;x\u{07}\u{01}");
/// assert_eq!(out, "ok \u{FFFD}\u{FFFD}\u{FFFD}]0;x\u{FFFD}\u{FFFD}");
/// // ZWJ (emoji) se preserva:
/// assert_eq!(mask_terminal_hazards("a\u{200D}b"), "a\u{200D}b");
/// ```
#[must_use]
pub fn mask_terminal_hazards(s: &str) -> String {
    s.chars()
        .map(|c| if is_terminal_hazard(c) { '\u{FFFD}' } else { c })
        .collect()
}

/// Encoding para REINTERPRETAR nombres de archivo como texto (#57, spec
/// §6.1, fila ZIP): SOLO display — los bytes del nombre jamás se mutan
/// (regla 1) y la elección es una acción explícita del usuario («ver
/// nombres como…»), nunca una decodificación a ciegas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameEncoding {
    /// IBM cp437 (DOS US), el legado clásico de los zip pre-Unicode.
    /// `encoding_rs` NO lo trae (WHATWG no lo incluye): tabla propia TOTAL
    /// (los 256 bytes mapean, la decodificación jamás falla).
    Cp437,
    /// Un encoding de `encoding_rs` (IBM866, `Shift_JIS`, GBK…).
    Rs(&'static Encoding),
}

impl NameEncoding {
    /// Etiqueta corta para UI (`cp437`, `IBM866`, `Shift_JIS`…).
    ///
    /// ```
    /// use norte_encoding::NameEncoding;
    /// assert_eq!(NameEncoding::Cp437.label(), "cp437");
    /// assert_eq!(NameEncoding::Rs(encoding_rs::GBK).label(), "GBK");
    /// ```
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Cp437 => "cp437",
            Self::Rs(e) => e.name(),
        }
    }
}

/// cp437, mitad alta (0x80–0xFF). La mitad baja es ASCII tal cual (los
/// controles 0x00–0x1F se dejan como controles: el enmascarado de display
/// los tapa aguas arriba, igual que en un nombre UTF-8 con controles).
const CP437_HIGH: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', 'É', 'æ', 'Æ',
    'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ', 'á', 'í', 'ó', 'ú', 'ñ', 'Ñ',
    'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»', '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕',
    '╣', '║', '╗', '╝', '╜', '╛', '┐', '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦',
    '╠', '═', '╬', '╧', '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐',
    '▀', 'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩', '≡', '±',
    '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', '\u{00A0}',
];

/// Decodifica un NOMBRE con `enc` para display. TOTAL: siempre produce
/// texto (cp437 mapea los 256 bytes; `encoding_rs` sustituye lo inválido
/// por `U+FFFD`). El saneado de hazards (controles/bidi/invisibles) es del
/// CALLER de display, igual que con nombres UTF-8.
///
/// ```
/// use norte_encoding::{NameEncoding, decode_name};
/// // "CAFÉ.TXT" en cp437 (É = 0x90):
/// assert_eq!(decode_name(b"CAF\x90.TXT", NameEncoding::Cp437), "CAFÉ.TXT");
/// // Cirílico en IBM866:
/// let ruso = decode_name(b"\x8f\xa0\xaf\xaa\xa0", NameEncoding::Rs(encoding_rs::IBM866));
/// assert_eq!(ruso, "Папка");
/// ```
#[must_use]
pub fn decode_name(bytes: &[u8], enc: NameEncoding) -> String {
    match enc {
        NameEncoding::Cp437 => bytes
            .iter()
            .map(|&b| {
                if b < 0x80 {
                    b as char
                } else {
                    CP437_HIGH[(b - 0x80) as usize]
                }
            })
            .collect(),
        NameEncoding::Rs(e) => e.decode_without_bom_handling(bytes).0.into_owned(),
    }
}

/// El ciclo de «ver nombres como…» (#57): los encodings de nombres que un
/// usuario real necesita probar sobre un zip/tar pre-Unicode. cp437 primero
/// (el default histórico del formato zip cuando el bit 11 está apagado).
///
/// ```
/// assert_eq!(norte_encoding::name_reinterpret_cycle().len(), 5);
/// ```
#[must_use]
pub fn name_reinterpret_cycle() -> &'static [NameEncoding] {
    const CYCLE: &[NameEncoding] = &[
        NameEncoding::Cp437,
        NameEncoding::Rs(encoding_rs::IBM866),
        NameEncoding::Rs(encoding_rs::SHIFT_JIS),
        NameEncoding::Rs(encoding_rs::GBK),
        NameEncoding::Rs(encoding_rs::WINDOWS_1252),
    ];
    CYCLE
}

/// Sugiere un encoding del ciclo para un conjunto de NOMBRES no-UTF8
/// (chardetng sobre las muestras). `None` = sin sugerencia útil (la
/// adivinanza cayó fuera del ciclo — p. ej. UTF-8 — o no hay muestras).
/// cp437 jamás se sugiere (chardetng no lo modela); el ciclo del frontend
/// da la vuelta completa, así que sigue siendo alcanzable a mano.
///
/// ```
/// use norte_encoding::{NameEncoding, suggest_name_encoding};
/// // "Папка" en cp866 → IBM866 (miembro del ciclo):
/// let s = suggest_name_encoding(&[b"\x8f\xa0\xaf\xaa\xa0"]);
/// assert_eq!(s, Some(NameEncoding::Rs(encoding_rs::IBM866)));
/// assert_eq!(suggest_name_encoding(&[]), None);
/// ```
#[must_use]
pub fn suggest_name_encoding(samples: &[&[u8]]) -> Option<NameEncoding> {
    if samples.is_empty() {
        return None;
    }
    let mut det = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    for (i, s) in samples.iter().enumerate() {
        det.feed(s, false);
        // Separador ASCII neutro entre muestras (F3 del audit): sin él, un
        // lead byte colgante al final de un nombre se emparejaría con el
        // primer byte del siguiente, fabricando secuencias multibyte
        // fantasma que sesgan la adivinanza.
        det.feed(b" ", i + 1 == samples.len());
    }
    let guess = det.guess(None, chardetng::Utf8Detection::Deny);
    name_reinterpret_cycle()
        .iter()
        .copied()
        .find(|e| matches!(e, NameEncoding::Rs(rs) if *rs == guess))
}
