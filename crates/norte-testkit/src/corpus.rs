//! Corpus canónico de fixtures hostiles (spec §6.1/§12): 19 nombres de
//! archivo + 9 contenidos detectables + 3 solo-forzables. TODO crate que toque paths o
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

/// Los 24 nombres hostiles canónicos.
///
/// ```
/// let names = norte_testkit::corpus::hostile_names();
/// assert_eq!(names.len(), 24);
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

/// Los 9 contenidos canónicos DETECTABLES. Texto base: `"año 2026\n"` (ñ fuera de ASCII),
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
