//! Fitting a picture to the viewer and encoding it as half-block cells.
//!
//! Pure functions over RGB pixels, so they are tested on the host without a
//! decoder in sight. The WIT glue in `lib.rs` decodes and calls these.

/// The width the guest picks when the host sends no hint (the CLI).
pub const DEFAULT_COLUMNS: u32 = 80;

/// The host rejects a styled line with more spans than this, and a photo is
/// one span per cell, so the picture is never wider than this many cells.
pub const MAX_COLUMNS: u32 = 256;

/// Rows of cells a preview may take. A tall picture at full width could
/// run to thousands of rows; the viewer scrolls, but a preview is a glance,
/// not a poster. And the host bounds the whole answer at 4 MiB counting
/// every span's colours: 256 columns × 256 rows of one-cell spans is about
/// 3 MiB, twice that would be refused entire.
pub const MAX_ROWS: u32 = 256;

/// One cell of output: a `▀` (or a run of them) with the upper pixel as
/// `fg` and the lower one as `bg`. `bg` is `None` on the last row of a
/// picture with an odd number of pixel rows: there is no lower pixel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub fg: Option<(u8, u8, u8)>,
    pub bg: Option<(u8, u8, u8)>,
}

/// The pixel size to resample a `width`×`height` picture to, given the
/// viewer's width in cells. Never enlarges; keeps the aspect ratio under the
/// 2:1 cell geometry (a half block is one pixel wide and two tall, so pixels
/// stay square); bounded by [`MAX_COLUMNS`] and by twice [`MAX_ROWS`].
///
/// Returns `None` for an empty picture.
#[must_use]
pub fn fit(width: u32, height: u32, columns: Option<u32>) -> Option<(u32, u32)> {
    if width == 0 || height == 0 {
        return None;
    }
    let cols = columns.unwrap_or(DEFAULT_COLUMNS).clamp(1, MAX_COLUMNS);
    let max_h = MAX_ROWS * 2;
    // Scale down by whichever bound bites hardest; 1.0 means "as is".
    let by_w = f64::from(cols) / f64::from(width);
    let by_h = f64::from(max_h) / f64::from(height);
    let scale = by_w.min(by_h).min(1.0);
    let w = (f64::from(width) * scale).round().max(1.0);
    let h = (f64::from(height) * scale).round().max(1.0);
    // `round` of a value in 1..=MAX_COLUMNS fits u32 by construction.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some((w as u32, h as u32))
}

/// Encodes `width`×`height` RGB pixels (row-major, `pixels.len() == width *
/// height`) as lines of half-block spans: row `2k` on top, `2k + 1` at the
/// bottom of line `k`. Neighbouring cells with the same two colours are
/// merged into one span (a flat background is one span, not eighty).
#[must_use]
pub fn encode(pixels: &[(u8, u8, u8)], width: u32, height: u32) -> Vec<Vec<Span>> {
    let w = width as usize;
    let h = height as usize;
    debug_assert_eq!(pixels.len(), w * h);
    let mut lines = Vec::with_capacity(h.div_ceil(2));
    for top in (0..h).step_by(2) {
        let mut line: Vec<Span> = Vec::new();
        for x in 0..w {
            let fg = Some(pixels[top * w + x]);
            let bg = (top + 1 < h).then(|| pixels[(top + 1) * w + x]);
            match line.last_mut() {
                Some(last) if last.fg == fg && last.bg == bg => last.text.push('▀'),
                _ => line.push(Span {
                    text: "▀".to_owned(),
                    fg,
                    bg,
                }),
            }
        }
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_never_enlarges_and_keeps_aspect() {
        assert_eq!(fit(40, 20, Some(80)), Some((40, 20)));
        assert_eq!(fit(160, 40, Some(80)), Some((80, 20)));
        assert_eq!(fit(160, 40, None), Some((80, 20)), "no hint: 80 columns");
    }

    #[test]
    fn fit_is_bounded_by_the_host_span_cap_and_by_rows() {
        assert_eq!(fit(1000, 10, Some(4000)), Some((256, 3)));
        // 100 wide, 4000 tall: the row cap (256 rows = 512 px) bites first.
        let (w, h) = fit(100, 4000, Some(100)).unwrap();
        assert_eq!(h, 512);
        assert_eq!(w, 13);
        assert_eq!(fit(0, 5, None), None);
        assert_eq!(
            fit(1, 1, Some(0)),
            Some((1, 1)),
            "a zero width is a hint of one"
        );
    }

    #[test]
    fn encode_pairs_rows_and_merges_equal_neighbours() {
        let r = (255, 0, 0);
        let b = (0, 0, 255);
        let g = (0, 255, 0);
        // 4×4: two red rows, then a blue row, then a row blue-blue-green-green.
        let px = [r, r, r, r, r, r, r, r, b, b, b, b, b, b, g, g];
        let lines = encode(&px, 4, 4);
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            vec![Span {
                text: "▀▀▀▀".into(),
                fg: Some(r),
                bg: Some(r)
            }],
            "four equal cells are one span"
        );
        assert_eq!(lines[1].len(), 2);
        assert_eq!(lines[1][0].fg, Some(b));
        assert_eq!(lines[1][0].bg, Some(b));
        assert_eq!(lines[1][0].text, "▀▀");
        assert_eq!(lines[1][1].fg, Some(b));
        assert_eq!(lines[1][1].bg, Some(g));
    }

    #[test]
    fn encode_odd_height_leaves_the_last_bg_empty() {
        let r = (255, 0, 0);
        let b = (0, 0, 255);
        let lines = encode(&[r, r, b, b, r, b], 2, 3);
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[1][0],
            Span {
                text: "▀".into(),
                fg: Some(r),
                bg: None
            }
        );
        assert_eq!(
            lines[1][1],
            Span {
                text: "▀".into(),
                fg: Some(b),
                bg: None
            }
        );
    }
}
