//! Detección y decodificación de encodings de texto (spec §6): pipeline
//! BOM → heurística de binario (NUL) → chardetng, y decodificación con
//! `encoding_rs`. Aísla la superficie de esas deps (plan M1): el resto del
//! workspace consume ESTA API, jamás `chardetng`/`encoding_rs` directos.
#![forbid(unsafe_code)]

pub use encoding_rs::{Encoding, UTF_8};

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
