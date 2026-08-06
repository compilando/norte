//! Corpus canónico de fixtures hostiles (spec §6.1/§12): 33 nombres de
//! archivo + 11 contenidos detectables + 3 solo-forzables. TODO crate que toque paths o
//! texto testea contra ESTE corpus — las fixtures nuevas entran aquí (regla
//! de CLAUDE.md: test-first en bugs de encoding).

use serde::Deserialize;

/// Un nombre de archivo hostil del corpus.
#[derive(Debug, Clone)]
pub struct HostileName {
    /// Identificador estable (para nombres de test y mensajes).
    pub id: String,
    /// Los bytes crudos del nombre, tal como los daría el OS.
    pub bytes: Vec<u8>,
    /// Por qué es hostil (documentación viva).
    pub why: String,
}

#[derive(Deserialize)]
struct RawName {
    id: String,
    hex: String,
    why: String,
}

/// Los 33 nombres hostiles canónicos.
///
/// ```
/// let names = norte_testkit::corpus::hostile_names();
/// assert_eq!(names.len(), 33);
/// // Todos son segmentos VPath válidos (sin NUL ni `/`).
/// for n in &names {
///     assert!(norte_proto::Segment::new(n.bytes.clone()).is_ok(), "{}", n.id);
/// }
/// ```
///
/// # Panics
/// Nunca con el corpus commiteado: la fixture embebida se valida en tests.
#[must_use]
pub fn hostile_names() -> Vec<HostileName> {
    let raw: Vec<RawName> =
        serde_json::from_str(include_str!("corpus/names.json")).expect("names.json válido");
    raw.into_iter()
        .map(|r| HostileName {
            bytes: hex_decode(&r.hex),
            id: r.id,
            why: r.why,
        })
        .collect()
}

/// Un contenido de archivo en un encoding no-UTF8.
#[derive(Debug, Clone)]
pub struct ContentFixture {
    /// Identificador estable.
    pub id: &'static str,
    /// Etiqueta WHATWG del encoding (la que entendería `encoding_rs`).
    pub encoding: &'static str,
    /// Los bytes crudos del archivo.
    pub bytes: Vec<u8>,
    /// El texto que un decoder correcto debe producir.
    pub decoded: &'static str,
}

/// Los 11 contenidos canónicos DETECTABLES. Texto base: `"año 2026\n"` (ñ fuera de ASCII),
/// `"テスト\n"` para Shift-JIS, o `"it’s\n"` para la zona divergente
/// 0x80–0x9F de windows-1252. Generados en código: deterministas,
/// autodocumentados, sin binarios opacos en el repo.
#[must_use]
pub fn content_fixtures() -> Vec<ContentFixture> {
    const TEXT: &str = "año 2026\n";
    let utf16 = |big_endian: bool| -> Vec<u8> {
        let bom: [u8; 2] = if big_endian {
            [0xFE, 0xFF]
        } else {
            [0xFF, 0xFE]
        };
        let mut out = bom.to_vec();
        for unit in TEXT.encode_utf16() {
            let b = if big_endian {
                unit.to_be_bytes()
            } else {
                unit.to_le_bytes()
            };
            out.extend_from_slice(&b);
        }
        out
    };
    vec![
        ContentFixture {
            id: "utf8_plain",
            encoding: "utf-8",
            bytes: TEXT.as_bytes().to_vec(),
            decoded: TEXT,
        },
        ContentFixture {
            // 0x95 0x32 0x82 0x36 = U+20000 (4 bytes, zona exclusiva de
            // GB18030): caza decoders que se queden en GBK "clásico".
            id: "gb18030",
            encoding: "gb18030",
            bytes: b"\x95\x32\x82\x36 2026\n".to_vec(),
            decoded: "\u{20000} 2026\n",
        },
        ContentFixture {
            // Cirílico PURO: KOI8-R y KOI8-U coinciden en letras (difieren
            // en box-drawing) — chardetng puede decir KOI8-U y el decode
            // sigue siendo exacto.
            id: "koi8_r",
            encoding: "koi8-r",
            bytes: b"\xf0\xd2\xc9\xd7\xc5\xd4 2026\n".to_vec(),
            decoded: "Привет 2026\n",
        },
        ContentFixture {
            id: "utf16le_bom",
            encoding: "utf-16le",
            bytes: utf16(false),
            decoded: TEXT,
        },
        ContentFixture {
            id: "utf16be_bom",
            encoding: "utf-16be",
            bytes: utf16(true),
            decoded: TEXT,
        },
        ContentFixture {
            id: "latin1",
            encoding: "windows-1252",
            bytes: b"a\xF1o 2026\n".to_vec(),
            decoded: TEXT,
        },
        ContentFixture {
            // 0x92 = ’ en windows-1252 pero control U+0092 en ISO-8859-1
            // estricto: caza decoders que confundan ambos (la ñ = 0xF1 no
            // distingue, es idéntica en los dos).
            id: "windows_1252_curly",
            encoding: "windows-1252",
            bytes: b"it\x92s\n".to_vec(),
            decoded: "it\u{2019}s\n",
        },
        ContentFixture {
            id: "shift_jis",
            // テスト en Shift-JIS + newline.
            encoding: "shift_jis",
            bytes: vec![0x83, 0x65, 0x83, 0x58, 0x83, 0x67, 0x0A],
            decoded: "テスト\n",
        },
        ContentFixture {
            id: "utf8_bom",
            encoding: "utf-8",
            bytes: {
                let mut v = vec![0xEF, 0xBB, 0xBF];
                v.extend_from_slice(TEXT.as_bytes());
                v
            },
            decoded: TEXT,
        },
        ContentFixture {
            // Falso positivo de la aguja LEGACY de 1 byte: "ñ" en
            // windows-1252/ISO-8859-15 = 0xF1, que en UTF-8 aparece como byte
            // LÍDER de una secuencia de 4 bytes. `F1 84 80 81` = U+44001: el
            // modo literal a-ciegas casa 0xF1 POR AZAR; la búsqueda
            // ENCODING-AWARE (aguja = 0xC3 0xB1) NO. Canoniza el límite que
            // hasta ahora solo vivía inline en engine_search.rs.
            id: "cjk_utf8_lead_f1",
            encoding: "utf-8",
            bytes: CJK_UTF8_LEAD_F1.as_bytes().to_vec(),
            decoded: CJK_UTF8_LEAD_F1,
        },
        ContentFixture {
            // Inyección por el PREVIEW de fs.search: la aguja + RLO (202E) +
            // isolate (2066) SIN cerrar + ESC+OSC (`\x1b]0;pwn\x07`, cambia el
            // título del terminal) + un C0 crudo (SOH). Un consumidor que
            // pinte el preview directo ejecutaría el ANSI y vería el orden
            // visual falsificado. El productor DEBE sanearlo en origen
            // (mask_terminal_hazards): ningún char de is_terminal_hazard
            // sobrevive. UTF-8 válido y sin NUL → detectable como texto.
            id: "preview_bidi_ctrl_injection",
            encoding: "utf-8",
            bytes: PREVIEW_BIDI_CTRL_INJECTION.as_bytes().to_vec(),
            decoded: PREVIEW_BIDI_CTRL_INJECTION,
        },
    ]
}

/// Contenidos que un decoder CORRECTO produce CON PÉRDIDA (`had_errors`): se
/// detectan como texto (con la certeza de un BOM) pero llevan un byte
/// inválido para ese encoding, así que el decode canónico inserta `U+FFFD`.
///
/// Separados de [`content_fixtures`] a propósito — el contrato de ese corpus es
/// «detectar como texto y decodificar EXACTO y sin pérdida», y sus tests lo
/// afirman en bucle. Estos son la aguja de la señal `lossy` de la preview de
/// plugin (#101) y de cualquier consumidor del honesto «esto vino de un decode
/// fallido, no del fichero». `decoded` es lo que produce el decoder correcto:
/// ya lleva el `U+FFFD`.
#[must_use]
pub fn lossy_content_fixtures() -> Vec<ContentFixture> {
    vec![ContentFixture {
        // BOM UTF-8 (EF BB BF) → detección de UTF-8 con CERTEZA (no
        // estadística: sin el BOM, chardetng elegiría windows-1252 donde 0xFF
        // es `ÿ` y el decode saldría limpio). El `0xFF` interior NUNCA es
        // válido en UTF-8 → `U+FFFD` con had_errors.
        id: "utf8_bom_invalid",
        encoding: "utf-8",
        bytes: {
            let mut v = vec![0xEF, 0xBB, 0xBF];
            v.extend_from_slice(b"a\xFFo 2026\n");
            v
        },
        decoded: "a\u{FFFD}o 2026\n",
    }]
}

/// Línea con `0xF1` como byte líder de un char de 4 bytes (`U+44001`): la
/// aguja latina corta `ñ` (0xF1 en legacy) casa por azar en modo a-ciegas.
pub(crate) const CJK_UTF8_LEAD_F1: &str = "汉字 \u{44001} texto\n";

/// Línea hostil para el preview de `fs.search`: aguja `aguja` + RLO + isolate
/// sin cerrar + ESC+OSC + C0 crudo. Ningún char de terminal-hazard debe
/// sobrevivir al saneo en origen.
pub(crate) const PREVIEW_BIDI_CTRL_INJECTION: &str =
    "aguja \u{202E}reovni\u{2066} \u{1B}]0;pwn\u{07}\u{01}fin\n";

/// Un chord hostil del corpus (encoding audit H1): un token de UN solo
/// codepoint, elegido de [`norte_encoding::is_terminal_hazard`], que
/// `norte_frontend::keymap::parse_chord` acepta sin más como
/// `KeyCode::Char` — CUALQUIER codepoint suelto parsea, el motor de keymap
/// no filtra hazards (esa no es su responsabilidad; ver el comentario en
/// `parse_chord`). Un `./.norte/keymap.toml` (capa de PROYECTO, sin trust)
/// puede ligar uno de estos a un comando soportado; `Chord`'s `Display`
/// lo escribe CRUDO a propósito (logs/debug quieren el chord real), así
/// que todo consumidor que pinte el chord FORMATEADO (ayuda generada,
/// palette) debe enmascararlo — este corpus ejercita esa obligación
/// render-side.
#[derive(Debug, Clone)]
pub struct HostileChord {
    /// Identificador estable (para nombres de test y mensajes).
    pub id: &'static str,
    /// El token, tal como iría en `on = [...]` de un keymap.toml (un solo
    /// codepoint).
    pub token: char,
    /// Por qué es hostil (documentación viva).
    pub why: &'static str,
}

/// Los 4 chords hostiles canónicos: un solo codepoint cada uno (dos o más
/// codepoints ya los rechaza `parse_chord`, ver
/// `parse_chord_rechaza_tokens_multi_codepoint_sin_partir` en
/// `norte-frontend`), cada uno un hazard de terminal distinto.
///
/// ```
/// let chords = norte_testkit::corpus::hostile_chords();
/// assert_eq!(chords.len(), 4);
/// // Todos son hazards de terminal detectados por la fuente única.
/// for c in &chords {
///     assert!(norte_encoding::is_terminal_hazard(c.token), "{}", c.id);
/// }
/// ```
#[must_use]
pub fn hostile_chords() -> Vec<HostileChord> {
    vec![
        HostileChord {
            id: "rlo",
            token: '\u{202E}',
            why: "RIGHT-TO-LEFT OVERRIDE: reordena visualmente TODO lo que \
                  sigue en la línea — en un footer `[chord] etiqueta` puede \
                  hacer que cancel/confirm se vean intercambiados",
        },
        HostileChord {
            id: "zwsp",
            token: '\u{200B}',
            why: "ZERO WIDTH SPACE: invisible, dos chords bindeados a \
                  comandos distintos pueden pintarse indistinguibles",
        },
        HostileChord {
            id: "lrm",
            token: '\u{200E}',
            why: "LEFT-TO-RIGHT MARK: override bidi invisible, altera el \
                  orden visual de texto RTL vecino sin dejar marca visible",
        },
        HostileChord {
            id: "bel",
            token: '\u{0007}',
            why: "BEL (control C0): un terminal sin sanear lo EJECUTA \
                  (campana/pitido) en vez de pintarlo como texto",
        },
    ]
}

/// A hostile DISPLAY TITLE: prose meant to be painted into a narrow column,
/// not a filename.
///
/// The other two families of this corpus cover the surfaces norte had until
/// now: [`hostile_names`] is BYTES off a filesystem, [`hostile_chords`] is a
/// single codepoint out of a keymap. A title is neither — it is a whole
/// string of editorial text, it is TRUNCATED to fit a sidebar or a column,
/// and from the help overlay (H3b) onwards it is also a surface a plugin
/// manifest can feed. The hazards that shape live on the CUT: two titles that
/// become the same string once truncated, a combining mark orphaned onto the
/// ellipsis, a grapheme cluster split down the middle.
///
/// Truncation of a title is by the RIGHT (`norte_tui::ui::right_ellipsis`):
/// head plus tail collides any two labels that agree on both ends, so a label
/// keeps its distinct prefix and loses its tail. [`HostileTitle::twin`] is
/// built for THAT rule — the pair shares everything up to the cut.
#[derive(Debug, Clone)]
pub struct HostileTitle {
    /// Stable identifier (for test names and messages).
    pub id: &'static str,
    /// The title itself, as an author or a plugin manifest would write it.
    pub text: &'static str,
    /// The other half of a COLLIDING pair, when the hazard needs two strings.
    ///
    /// A collision cannot be expressed by one string: it is a property of a
    /// PAIR that renders identically once cut. `None` for the fixtures whose
    /// hazard is internal to a single title.
    pub twin: Option<&'static str>,
    /// Why it is hostile (living documentation).
    pub why: &'static str,
}

/// The 4 canonical hostile titles.
///
/// ```
/// let titles = norte_testkit::corpus::hostile_titles();
/// assert_eq!(titles.len(), 4);
///
/// // The colliding pair shares a long prefix: cut short enough, both sides
/// // render the same string.
/// let pair = titles.iter().find(|t| t.id == "truncation_twins").unwrap();
/// let twin = pair.twin.expect("a collision needs two strings");
/// assert_ne!(pair.text, twin);
/// assert_eq!(&pair.text[..30], &twin[..30]);
///
/// // The bidi-isolate title is LEGITIMATE editorial text that is made of
/// // terminal hazards: it is what makes a hazard sweep over prose a live
/// // constraint and not a hypothetical.
/// let bidi = titles.iter().find(|t| t.id == "bidi_isolate_url").unwrap();
/// assert!(bidi.text.chars().any(norte_encoding::is_terminal_hazard));
/// ```
#[must_use]
pub fn hostile_titles() -> Vec<HostileTitle> {
    vec![
        HostileTitle {
            id: "truncation_twins",
            text: "Copiar al host remoto (SFTP, puerto 22)",
            twin: Some("Copiar al host remoto (SFTP, puerto 2222)"),
            why: "two DIFFERENT titles that share everything up to the cut: \
                  right-truncated into a sidebar column both read `Copiar al \
                  host remo…`, so a reader picking one of the two rows cannot \
                  tell which page they are opening. Nothing can prevent the \
                  collision in a narrow column — what a frontend owes is that \
                  the cut is MARKED (the `…`), never a silent equality",
        },
        HostileTitle {
            id: "nfd_accent_on_the_cut",
            // `cafe` + U+0301: the accent is its OWN codepoint, of width 0.
            text: "Copiar cafe\u{301}.txt al otro panel",
            twin: None,
            why: "an NFD combining acute is width 0, so a truncator that \
                  walks by CELLS never spends budget on it: the mark can \
                  survive its base character and end up composed onto the \
                  ellipsis (`caf…` painted as `caf´…`), moving an accent onto \
                  a glyph the author never wrote. macOS hands out NFD by \
                  default, so this is the ordinary case, not the exotic one",
        },
        HostileTitle {
            id: "zwj_cluster_on_the_cut",
            text: "Marcar 👨\u{200D}👩\u{200D}👧\u{200D}👦 y copiar",
            twin: None,
            why: "a ZWJ emoji cluster is several codepoints painted as ONE \
                  glyph. A cut that falls inside it turns one family into two \
                  or three unrelated people, and a cut that leaves the tail \
                  starting on the joiner composes the joiner onto the \
                  ellipsis. ZWJ is deliberately NOT masked (see `must_mask`), \
                  so a truncator cannot lean on masking to avoid it",
        },
        HostileTitle {
            id: "bidi_isolate_url",
            // U+2066 LRI … U+2069 PDI: the CORRECT way to put an LTR URL
            // inside RTL prose.
            text: "\u{2066}sftp://host/ruta\u{2069} en el panel derecho",
            twin: None,
            why: "the LEGITIMATE case: `U+2066`..`U+2069` are how an RTL \
                  locale keeps an LTR run (a URL, a path, a command id) from \
                  reordering the sentence around it — and every one of them \
                  is in `norte_encoding::is_terminal_hazard`. So a corpus \
                  hazard sweep is not a hypothetical the day an RTL \
                  translation lands: it is the gate that forces the choice \
                  between isolating the run and shipping raw bidi controls to \
                  a terminal to be a deliberate one",
        },
    ]
}

fn hex_decode(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "hex de longitud par: {s}");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("dígitos hex"))
        .collect()
}

/// Contenidos que la detección NO puede resolver (spec §6: recuperables
/// SOLO con «recargar como…» forzado): UTF-16 sin BOM (cae a binario por
/// la heurística NUL — contrato deliberado) y un BOM espurio que es DATO.
///
/// El contrato: `detect` no da su encoding, pero `decode_forced` con la
/// etiqueta debe ser EXACTO y sin pérdidas.
#[must_use]
pub fn content_fixtures_forced() -> Vec<ContentFixture> {
    const TEXT: &str = "año 2026\n";
    let utf16_nobom = |big_endian: bool| -> Vec<u8> {
        let mut out = Vec::new();
        for unit in TEXT.encode_utf16() {
            let b = if big_endian {
                unit.to_be_bytes()
            } else {
                unit.to_le_bytes()
            };
            out.extend_from_slice(&b);
        }
        out
    };
    vec![
        ContentFixture {
            id: "utf16le_nobom",
            encoding: "utf-16le",
            bytes: utf16_nobom(false),
            decoded: TEXT,
        },
        ContentFixture {
            id: "utf16be_nobom",
            encoding: "utf-16be",
            bytes: utf16_nobom(true),
            decoded: TEXT,
        },
        ContentFixture {
            // FE FF como DATOS windows-1252 (þÿ): un decode que sniffe el
            // BOM por encima del encoding FORZADO viola "siempre
            // corregible a mano" (spec §6.2).
            id: "w1252_fake_bom",
            encoding: "windows-1252",
            bytes: b"\xFE\xFF Fahr.\n".to_vec(),
            decoded: "þÿ Fahr.\n",
        },
    ]
}
