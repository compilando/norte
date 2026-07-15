//! [`Color`] RGB de 24 bits (autoría canónica en `#rrggbb`) y su degradación a
//! paletas de 256 y 16 colores (ADR 0020 D2). El crate NO habla con ningún
//! backend: entrega un [`ResolvedColor`] que el frontend traduce a su color
//! nativo.

use std::fmt;

use serde::de::{self, Deserialize, Deserializer, Visitor};
use serde::ser::{Serialize, Serializer};

/// Color RGB de 24 bits. Formato de autoría: `#rrggbb` (o `#rgb` abreviado).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color {
    /// Rojo.
    pub r: u8,
    /// Verde.
    pub g: u8,
    /// Azul.
    pub b: u8,
}

/// Profundidad de color efectiva del frontend (ADR 0020 D2). El truecolor es
/// el ideal; las otras dos son degradaciones para terminales pobres.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorDepth {
    /// 24 bits: el color viaja tal cual.
    Truecolor,
    /// 256 colores xterm (cubo 6×6×6 + grises + 16 base).
    Ansi256,
    /// 16 colores ANSI base.
    Ansi16,
}

/// Color ya PROYECTADO a la profundidad del frontend. `Rgb` para truecolor;
/// `Indexed` para paletas (0–255 en 256, 0–15 en 16) — el frontend lo mapea a
/// `Color::Rgb`/`Color::Indexed` de su librería.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResolvedColor {
    /// Truecolor RGB.
    Rgb(u8, u8, u8),
    /// Índice de paleta (xterm-256 o ANSI-16).
    Indexed(u8),
}

/// Error al parsear un color desde texto.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ColorParseError {
    /// No empieza por `#` o la longitud no es 3/6 dígitos hex.
    #[error("color inválido: se esperaba `#rgb` o `#rrggbb`")]
    Format,
    /// Un dígito no es hexadecimal.
    #[error("dígito hex inválido en el color")]
    Digit,
}

impl Color {
    /// Construye desde componentes.
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Parsea `#rrggbb` o `#rgb` (este último expande cada dígito, `#abc` =
    /// `#aabbcc`).
    ///
    /// # Errors
    /// [`ColorParseError`] si el formato o los dígitos no son válidos.
    pub fn parse(s: &str) -> Result<Self, ColorParseError> {
        let hex = s.strip_prefix('#').ok_or(ColorParseError::Format)?;
        let comp = |a: u8, b: u8| -> Result<u8, ColorParseError> {
            let hi = char::from(a).to_digit(16).ok_or(ColorParseError::Digit)?;
            let lo = char::from(b).to_digit(16).ok_or(ColorParseError::Digit)?;
            // hi,lo ∈ 0..=15 ⇒ hi*16+lo ∈ 0..=255: el try_from nunca falla.
            u8::try_from(hi * 16 + lo).map_err(|_| ColorParseError::Digit)
        };
        match hex.len() {
            6 => {
                let x = hex.as_bytes();
                Ok(Self {
                    r: comp(x[0], x[1])?,
                    g: comp(x[2], x[3])?,
                    b: comp(x[4], x[5])?,
                })
            }
            3 => {
                let x = hex.as_bytes();
                // `#abc` → `#aabbcc`: cada dígito se duplica.
                Ok(Self {
                    r: comp(x[0], x[0])?,
                    g: comp(x[1], x[1])?,
                    b: comp(x[2], x[2])?,
                })
            }
            _ => Err(ColorParseError::Format),
        }
    }

    /// Forma canónica `#rrggbb`.
    #[must_use]
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Proyecta a la profundidad `depth` (ADR 0020 D2). Truecolor = idéntico;
    /// las paletas eligen el índice de color más cercano en distancia RGB.
    #[must_use]
    pub fn resolve(self, depth: ColorDepth) -> ResolvedColor {
        match depth {
            ColorDepth::Truecolor => ResolvedColor::Rgb(self.r, self.g, self.b),
            ColorDepth::Ansi256 => ResolvedColor::Indexed(self.nearest_256()),
            ColorDepth::Ansi16 => ResolvedColor::Indexed(self.nearest_16()),
        }
    }

    /// Índice xterm-256 más cercano (rango 16–255: cubo 6×6×6 + rampa de
    /// grises). Se evitan los 16 primeros —configurables por el usuario del
    /// terminal, poco fiables— salvo que el gris/cubo caiga ahí naturalmente.
    fn nearest_256(self) -> u8 {
        // Niveles del cubo 6×6×6: 0,95,135,175,215,255.
        const LV: [u8; 6] = [0, 95, 135, 175, 215, 255];
        // Índice de cubo (0..6) más cercano a un componente.
        let cube_idx = |v: u8| -> u8 {
            let mut best = 0u8;
            let mut bd = u16::MAX;
            for i in 0u8..6 {
                let d = (i16::from(v) - i16::from(LV[usize::from(i)])).unsigned_abs();
                if d < bd {
                    bd = d;
                    best = i;
                }
            }
            best
        };
        let (ri, gi, bi) = (cube_idx(self.r), cube_idx(self.g), cube_idx(self.b));
        let cube_rgb = (
            LV[usize::from(ri)],
            LV[usize::from(gi)],
            LV[usize::from(bi)],
        );
        let cube_index = 16 + 36 * ri + 6 * gi + bi;

        // Candidato de la rampa de grises (232–255: niveles 8,18,…,238).
        let avg = (u16::from(self.r) + u16::from(self.g) + u16::from(self.b)) / 3;
        let gray_n = if avg < 8 {
            0
        } else {
            ((avg - 8 + 5) / 10).min(23)
        };
        let gray_n = u8::try_from(gray_n).unwrap_or(23); // gray_n ≤ 23 por construcción
        let gray_v = 8 + gray_n * 10;
        let gray_index = 232 + gray_n;

        // El más cercano de los dos candidatos (cubo vs gris).
        if self.dist2((gray_v, gray_v, gray_v)) < self.dist2(cube_rgb) {
            gray_index
        } else {
            cube_index
        }
    }

    /// Índice ANSI-16 más cercano (tabla xterm por defecto).
    fn nearest_16(self) -> u8 {
        let mut best = 0u8;
        let mut bd = u32::MAX;
        for (i, &c) in ANSI16.iter().enumerate() {
            let d = self.dist2(c);
            if d < bd {
                bd = d;
                best = u8::try_from(i).unwrap_or(0); // i < 16
            }
        }
        best
    }

    /// Distancia euclídea al cuadrado en RGB (evita `sqrt`, monótona igual).
    /// Es una suma de cuadrados: siempre ≥ 0, por eso `unsigned_abs`.
    fn dist2(self, o: (u8, u8, u8)) -> u32 {
        let dr = i32::from(self.r) - i32::from(o.0);
        let dg = i32::from(self.g) - i32::from(o.1);
        let db = i32::from(self.b) - i32::from(o.2);
        (dr * dr + dg * dg + db * db).unsigned_abs()
    }
}

/// Paleta ANSI-16 por defecto de xterm (los 16 colores base). Los índices
/// 8–15 son las variantes «brillantes».
const ANSI16: [(u8, u8, u8); 16] = [
    (0x00, 0x00, 0x00), // 0 black
    (0x80, 0x00, 0x00), // 1 red
    (0x00, 0x80, 0x00), // 2 green
    (0x80, 0x80, 0x00), // 3 yellow
    (0x00, 0x00, 0x80), // 4 blue
    (0x80, 0x00, 0x80), // 5 magenta
    (0x00, 0x80, 0x80), // 6 cyan
    (0xc0, 0xc0, 0xc0), // 7 white
    (0x80, 0x80, 0x80), // 8 bright black
    (0xff, 0x00, 0x00), // 9 bright red
    (0x00, 0xff, 0x00), // 10 bright green
    (0xff, 0xff, 0x00), // 11 bright yellow
    (0x00, 0x00, 0xff), // 12 bright blue
    (0xff, 0x00, 0xff), // 13 bright magenta
    (0x00, 0xff, 0xff), // 14 bright cyan
    (0xff, 0xff, 0xff), // 15 bright white
];

// --- serde: un color viaja como el string `#rrggbb` ---

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct HexVisitor;
        impl Visitor<'_> for HexVisitor {
            type Value = Color;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("un color `#rgb` o `#rrggbb`")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Color, E> {
                Color::parse(v).map_err(E::custom)
            }
        }
        d.deserialize_str(HexVisitor)
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for Color {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Color".into()
    }
    fn json_schema(_g: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // Un string hex `#rgb`/`#rrggbb`.
        schemars::json_schema!({
            "type": "string",
            "pattern": "^#([0-9a-fA-F]{3}|[0-9a-fA-F]{6})$"
        })
    }
}
