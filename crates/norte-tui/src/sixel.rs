//! A sixel encoder for the panel column's icons (spec 2026-10-05, F2).
//!
//! An icon is one colour stroked over the background, so a palette of
//! eight levels between the two covers its antialiased edge: no general
//! colour quantizer, the reason ADR 0118 left sixel out.

use std::fmt::Write as _;

/// How many palette levels from the background (0) to the stroke (7).
const LEVELS: u16 = 8;

/// `alpha` (one coverage byte per pixel, row-major, `w`×`h`) as a DCS
/// sixel image: level 7 is `fg`, levels 1–6 the antialiased edge blended
/// toward `bg`, and level 0 — no coverage — is NOT painted (`P2=1`): the
/// cell's own background shows through, whatever the terminal's palette
/// makes of it. The caller repaints the cells before a new image, so an old
/// one never shows through either. `P1=9`: square pixels.
#[must_use]
pub fn encode(alpha: &[u8], w: u32, h: u32, fg: [u8; 3], bg: [u8; 3]) -> String {
    let level = |x: u32, y: u32| -> u16 {
        let i = usize::try_from(y * w + x).unwrap_or(usize::MAX);
        let a = u16::from(alpha.get(i).copied().unwrap_or(0));
        (a * (LEVELS - 1) + 127) / 255
    };
    let mut out = format!("\x1bP9;1;q\"1;1;{w};{h}");
    for l in 1..LEVELS {
        let pct = |c: usize| {
            let (f, b) = (u16::from(fg[c]), u16::from(bg[c]));
            // Mix in 0..=255, then to the percent sixel colours speak.
            let mixed = (b * (LEVELS - 1 - l) + f * l) / (LEVELS - 1);
            (mixed * 100 + 127) / 255
        };
        let _ = write!(out, "#{l};2;{};{};{}", pct(0), pct(1), pct(2));
    }
    for band in 0..h.div_ceil(6) {
        if band > 0 {
            out.push('-');
        }
        for l in 1..LEVELS {
            let column = |x: u32| -> u8 {
                (0..6)
                    .filter(|b| {
                        let y = band * 6 + b;
                        y < h && level(x, y) == l
                    })
                    .fold(0, |acc, b| acc | (1 << b))
            };
            if (0..w).all(|x| column(x) == 0) {
                continue;
            }
            let _ = write!(out, "#{l}");
            let mut x = 0;
            while x < w {
                let c = column(x);
                let run = (x..w).take_while(|&x2| column(x2) == c).count();
                let ch = char::from(63 + c);
                if run >= 4 {
                    let _ = write!(out, "!{run}{ch}");
                } else {
                    (0..run).for_each(|_| out.push(ch));
                }
                x += u32::try_from(run).unwrap_or(w);
            }
            out.push('$');
        }
    }
    out.push_str("\x1b\\");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// How many colours set each pixel's bit, from an encoded string.
    struct Bits(HashMap<(u32, u32), u32>);

    impl Bits {
        fn count(&self, x: u32, y: u32) -> u32 {
            self.0.get(&(x, y)).copied().unwrap_or(0)
        }
    }

    fn decode_bits(s: &str) -> Bits {
        let body = s.split_once('q').expect("DCS q").1;
        let mut chars = body.chars().peekable();
        let (mut x, mut band, mut out) = (0_u32, 0_u32, HashMap::new());
        let number = |chars: &mut std::iter::Peekable<std::str::Chars<'_>>| {
            let mut n = 0_u32;
            while let Some(d) = chars.peek().and_then(|c| c.to_digit(10)) {
                n = n * 10 + d;
                chars.next();
            }
            n
        };
        while let Some(c) = chars.next() {
            match c {
                '\x1b' => break,
                '"' => {
                    while chars
                        .peek()
                        .is_some_and(|c| c.is_ascii_digit() || *c == ';')
                    {
                        chars.next();
                    }
                }
                '#' => {
                    let _ = number(&mut chars);
                    while chars.peek() == Some(&';') {
                        chars.next();
                        let _ = number(&mut chars);
                    }
                }
                '$' => x = 0,
                '-' => {
                    x = 0;
                    band += 1;
                }
                '!' => {
                    let n = number(&mut chars);
                    let c = chars.next().expect("a sixel after a repeat");
                    for _ in 0..n {
                        mark(&mut out, x, band, c);
                        x += 1;
                    }
                }
                '?'..='~' => {
                    mark(&mut out, x, band, c);
                    x += 1;
                }
                other => panic!("unexpected {other:?} in {s:?}"),
            }
        }
        Bits(out)
    }

    fn mark(out: &mut HashMap<(u32, u32), u32>, x: u32, band: u32, c: char) {
        let bits = u32::from(c) - 63;
        for b in 0..6 {
            if bits & (1 << b) != 0 {
                *out.entry((x, band * 6 + b)).or_default() += 1;
            }
        }
    }

    #[test]
    fn header_raster_and_palette() {
        let s = encode(&[0; 4], 2, 2, [255, 0, 0], [0, 0, 0]);
        assert!(s.starts_with("\x1bP9;1;q\"1;1;2;2"), "{s:?}");
        assert!(s.contains("#7;2;100;0;0"), "{s:?}");
        assert!(!s.contains("#0;"), "level 0 is transparent: {s:?}");
        assert!(s.ends_with("\x1b\\"));
    }

    /// Empty pixels are LEFT ALONE (`P2=1`), so the cell's own background —
    /// whatever the terminal's palette makes of it — shows through: a
    /// painted background came out a different shade on a Solarized or
    /// gruvbox palette. Every covered pixel is set in exactly one colour.
    #[test]
    fn empty_pixels_are_transparent_and_covered_ones_set_once() {
        let alpha: Vec<u8> = (0..21_u8).map(|i| i * 12).collect();
        let bits = decode_bits(&encode(&alpha, 3, 7, [9, 9, 9], [0, 0, 0]));
        for y in 0..7 {
            for x in 0..3 {
                let a = alpha[usize::try_from(y * 3 + x).expect("small")];
                let want = u32::from((u16::from(a) * 7 + 127) / 255 > 0);
                assert_eq!(bits.count(x, y), want, "({x},{y}) alpha {a}");
            }
        }
    }

    /// `P1=9`: square pixels, for a terminal that ignores the raster
    /// attributes and would stretch the default 2:1.
    #[test]
    fn pixels_are_declared_square() {
        assert!(encode(&[0], 1, 1, [0; 3], [0; 3]).starts_with("\x1bP9;1;q"));
    }

    /// Height 7 is a band and one row: the rest of the second band stays
    /// untouched (`P2=1`), so nothing spills into the cell below.
    #[test]
    fn the_last_band_sets_no_row_past_the_height() {
        let bits = decode_bits(&encode(&[255; 3 * 7], 3, 7, [9, 9, 9], [0, 0, 0]));
        for y in 7..12 {
            for x in 0..3 {
                assert_eq!(bits.count(x, y), 0, "({x},{y})");
            }
        }
    }

    #[test]
    fn runs_are_compressed() {
        let s = encode(&[255; 40 * 6], 40, 6, [9, 9, 9], [0, 0, 0]);
        assert!(s.contains("!40~"), "{s:?}");
    }

    /// Full coverage is the stroke colour, none the background.
    #[test]
    fn coverage_picks_the_level() {
        let s = encode(&[0, 255], 2, 1, [255, 255, 255], [0, 0, 0]);
        // Colour 7 sets x=1 only (`@` = top bit, `?` = no bit); x=0 has no
        // coverage and is set by nobody.
        assert!(s.contains("#7?@$") && !s.contains("#0"), "{s:?}");
    }
}
