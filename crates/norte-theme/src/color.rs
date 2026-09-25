//! 24-bit RGB [`Color`] (canonical authoring as `#rrggbb`) and its degradation to
//! 256- and 16-color palettes (ADR 0020 D2). The crate does NOT talk to any
//! backend: it hands over a [`ResolvedColor`] that the frontend translates to its
//! native color.

use std::fmt;

use serde::de::{self, Deserialize, Deserializer, Visitor};
use serde::ser::{Serialize, Serializer};

/// 24-bit RGB color. Authoring format: `#rrggbb` (or the short `#rgb`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
}

/// Effective color depth of the frontend (ADR 0020 D2). Truecolor is the
/// ideal; the other two are degradations for poor terminals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorDepth {
    /// 24 bits: the color travels as is.
    Truecolor,
    /// 256 xterm colors (6×6×6 cube + grays + 16 base).
    Ansi256,
    /// 16 base ANSI colors.
    Ansi16,
}

/// Color already PROJECTED to the frontend's depth. `Rgb` for truecolor;
/// `Indexed` for palettes (0–255 in 256, 0–15 in 16) — the frontend maps it to
/// its library's `Color::Rgb`/`Color::Indexed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResolvedColor {
    /// Truecolor RGB.
    Rgb(u8, u8, u8),
    /// Palette index (xterm-256 or ANSI-16).
    Indexed(u8),
}

/// Error parsing a color from text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ColorParseError {
    /// Does not start with `#` or the length is not 3/6 hex digits.
    #[error("invalid color: expected `#rgb` or `#rrggbb`")]
    Format,
    /// A digit is not hexadecimal.
    #[error("invalid hex digit in color")]
    Digit,
}

impl Color {
    /// Builds from components.
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Parses `#rrggbb` or `#rgb` (the latter expands each digit, `#abc` =
    /// `#aabbcc`).
    ///
    /// # Errors
    /// [`ColorParseError`] if the format or the digits are not valid.
    pub fn parse(s: &str) -> Result<Self, ColorParseError> {
        let hex = s.strip_prefix('#').ok_or(ColorParseError::Format)?;
        let comp = |a: u8, b: u8| -> Result<u8, ColorParseError> {
            let hi = char::from(a).to_digit(16).ok_or(ColorParseError::Digit)?;
            let lo = char::from(b).to_digit(16).ok_or(ColorParseError::Digit)?;
            // hi,lo ∈ 0..=15 ⇒ hi*16+lo ∈ 0..=255: the try_from never fails.
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
                // `#abc` → `#aabbcc`: each digit is doubled.
                Ok(Self {
                    r: comp(x[0], x[0])?,
                    g: comp(x[1], x[1])?,
                    b: comp(x[2], x[2])?,
                })
            }
            _ => Err(ColorParseError::Format),
        }
    }

    /// Canonical `#rrggbb` form.
    #[must_use]
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Projects to depth `depth` (ADR 0020 D2). Truecolor = identical;
    /// the palettes pick the nearest color index by RGB distance.
    #[must_use]
    pub fn resolve(self, depth: ColorDepth) -> ResolvedColor {
        match depth {
            ColorDepth::Truecolor => ResolvedColor::Rgb(self.r, self.g, self.b),
            ColorDepth::Ansi256 => ResolvedColor::Indexed(self.nearest_256()),
            ColorDepth::Ansi16 => ResolvedColor::Indexed(self.nearest_16()),
        }
    }

    /// Nearest xterm-256 index (range 16–255: 6×6×6 cube + gray
    /// ramp). The first 16 —configurable by the terminal's user,
    /// unreliable— are avoided unless the gray/cube naturally lands there.
    fn nearest_256(self) -> u8 {
        // 6×6×6 cube levels: 0,95,135,175,215,255.
        const LV: [u8; 6] = [0, 95, 135, 175, 215, 255];
        // Nearest cube index (0..6) to a component.
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

        // Gray ramp candidate (232–255: levels 8,18,…,238).
        let avg = (u16::from(self.r) + u16::from(self.g) + u16::from(self.b)) / 3;
        let gray_n = if avg < 8 {
            0
        } else {
            ((avg - 8 + 5) / 10).min(23)
        };
        let gray_n = u8::try_from(gray_n).unwrap_or(23); // gray_n ≤ 23 by construction
        let gray_v = 8 + gray_n * 10;
        let gray_index = 232 + gray_n;

        // The nearer of the two candidates (cube vs gray).
        if self.dist2((gray_v, gray_v, gray_v)) < self.dist2(cube_rgb) {
            gray_index
        } else {
            cube_index
        }
    }

    /// Nearest ANSI-16 index (default xterm table).
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

    /// Squared Euclidean distance in RGB (avoids `sqrt`, equally monotonic).
    /// It is a sum of squares: always ≥ 0, hence `unsigned_abs`.
    fn dist2(self, o: (u8, u8, u8)) -> u32 {
        let dr = i32::from(self.r) - i32::from(o.0);
        let dg = i32::from(self.g) - i32::from(o.1);
        let db = i32::from(self.b) - i32::from(o.2);
        (dr * dr + dg * dg + db * db).unsigned_abs()
    }
}

/// xterm's default ANSI-16 palette (the 16 base colors). Indices
/// 8–15 are the "bright" variants.
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

// --- serde: a color travels as the string `#rrggbb` ---

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
                f.write_str("a `#rgb` or `#rrggbb` color")
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
        // A hex string `#rgb`/`#rrggbb`.
        schemars::json_schema!({
            "type": "string",
            "pattern": "^#([0-9a-fA-F]{3}|[0-9a-fA-F]{6})$"
        })
    }
}
