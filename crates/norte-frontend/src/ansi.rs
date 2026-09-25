//! MINIMAL ANSI-SGR parser and escape SANITIZER (#29): turns a plugin
//! previewer's output (e.g. syntect via `as_24_bit_terminal_escaped`) into
//! colored lines, interpreting ONLY foreground-color SGR sequences
//! (`ESC[…m`). Any OTHER escape sequence — cursor movement, screen clearing,
//! OSC (window title), unknown escapes — is DISCARDED, never forwarded to the
//! terminal.
//!
//! It is the plugin preview's trust boundary (rule 9 / hostile surface): a
//! plugin cannot inject dangerous escape sequences into the terminal through
//! its `output`; at most it paints colored text. LOOSE control bytes (BEL,
//! backspace…) are kept in the text and the frontend's display layer masks
//! them to `�` ([`crate::display_name`], which also handles bidi/invisibles)
//! — the parser only deals with the multi-character sequences that
//! `display_name` would not know how to recognize. The frontend translates
//! [`Rgb`] to its own color type (ratatui/GPUI).
//!
//! [`StyledSpan::role`] (G3a, ADR 0037) is the ONE field this parser NEVER
//! fills in — `parse_sgr` only understands color SGR (`fg`); a previewer with
//! ANSI-SGR output has no concept of a semantic role. It is filled by the
//! sibling conversion for an ALREADY-STRUCTURED preview (`SpanWire` →
//! `StyledSpan`, `crate::viewer::Viewer::with_plugin_preview_styled`), which
//! shares this same type so that `draw_viewer`/`render_viewer` paint BOTH
//! paths (ANSI-derived and WIT-structured) with the same code.

/// 24-bit RGB color (foreground).
pub type Rgb = (u8, u8, u8);

/// A text span with optional style: `role` (G3a, ADR 0037: a semantic name
/// ALREADY VALIDATED against `norte_theme::Role` — see the module's
/// rustdoc) or `fg` (raw foreground color). When BOTH are present, `role`
/// WINS when painting (the user's theme takes precedence over a plugin's
/// fixed color, ADR 0037 decision 3) — the frontend that consumes this type
/// (`ui.rs::draw_viewer`/`main.rs::render_viewer`) implements that
/// precedence; this type only CARRIES it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledSpan {
    /// The span's visible text (escape sequences already stripped).
    pub text: String,
    /// Semantic role ALREADY VALIDATED (G3a): `None` if the span carries no
    /// role, or if it carried one that is not in the closed set of
    /// `norte_theme::Role` (silent degradation, ADR 0037 — never a panic nor
    /// a free-form string). ALWAYS `None` on the ANSI-SGR path (`parse_sgr`,
    /// see the module's rustdoc).
    pub role: Option<norte_theme::Role>,
    /// Raw foreground color (fallback when `role` is `None`, or when the
    /// plugin itself did not declare a role), or `None` for the theme's.
    pub fg: Option<Rgb>,
    /// Raw BACKGROUND color (proto 0.66.0, D4): an image previewer paints
    /// half-blocks with the top pixel in `fg` and the bottom one here.
    /// `None` = the viewer's background. ALWAYS `None` on the ANSI-SGR path.
    pub bg: Option<Rgb>,
}

/// A line = a sequence of styled spans.
pub type StyledLine = Vec<StyledSpan>;

/// A WIRE span, sanitized and validated.
///
/// The two gates a span coming from a plugin crosses, in one place: `text`
/// is THIRD-PARTY text and gets masked the same way as the ANSI path's
/// (`crate::display_name`), and `role` is validated against what a plugin
/// CAN request ([`norte_theme::Role::from_kebab_requestable`]), which leaves
/// out the chrome and window-state roles (spec 2026-09-11, F2). An unknown
/// name degrades to `None`, never to an error: a guest from a newer norte
/// does not break the painting of an older one.
///
/// Shared because there are three consumers — the viewer with a styled
/// preview, the decorations and, since phase 3, a panel's frame — and the
/// third copy was written by copying the fields by hand, without masking the
/// text: a panel could smuggle terminal escapes through the one path meant
/// to prevent that.
///
/// ```
/// use norte_proto::methods::SpanWire;
///
/// let s = norte_frontend::ansi::span_de_wire(&SpanWire {
///     text: "branch".to_owned(),
///     role: Some("scrollbar-slider".to_owned()),
///     fg: None,
///     bg: None,
/// });
/// assert_eq!(s.text, "branch");
/// assert!(s.role.is_none(), "a plugin cannot request a chrome role");
/// ```
#[must_use]
pub fn span_de_wire(span: &norte_proto::methods::SpanWire) -> StyledSpan {
    StyledSpan {
        text: crate::display_name(span.text.as_bytes()).0,
        role: span
            .role
            .as_deref()
            .and_then(norte_theme::Role::from_kebab_requestable),
        fg: span.fg.map(|[r, g, b]| (r, g, b)),
        bg: span.bg.map(|[r, g, b]| (r, g, b)),
    }
}

/// Parses `input` (a previewer's output) into styled lines, interpreting
/// ONLY foreground-color SGR and DISCARDING any other escape sequence
/// (sanitized — see the module). Lines are split on `\n`; a trailing `\r`
/// at the end of a line is ignored (CRLF). Always returns at least one line.
///
/// ```
/// use norte_frontend::ansi::{parse_sgr, StyledSpan};
/// // A 24-bit red span + a hostile OSC (sets the title): the color is
/// // kept, the OSC is discarded whole.
/// let out = parse_sgr("\x1b[38;2;255;0;0mhi\x1b]0;PWNED\x07\x1b[0m fin");
/// assert_eq!(out.len(), 1);
/// assert_eq!(
///     out[0][0],
///     StyledSpan { text: "hi".into(), role: None, fg: Some((255, 0, 0)), bg: None }
/// );
/// assert_eq!(
///     out[0][1],
///     StyledSpan { text: " fin".into(), role: None, fg: None, bg: None }
/// );
/// ```
#[must_use]
pub fn parse_sgr(input: &str) -> Vec<StyledLine> {
    let mut lines: Vec<StyledLine> = Vec::new();
    let mut line: StyledLine = Vec::new();
    let mut cur = String::new();
    let mut fg: Option<Rgb> = None;
    let mut chars = input.chars().peekable();

    // Closes the current span (if it has text) with the color in effect.
    let flush = |cur: &mut String, fg: Option<Rgb>, line: &mut StyledLine| {
        if !cur.is_empty() {
            line.push(StyledSpan {
                text: std::mem::take(cur),
                role: None,
                fg,
                bg: None,
            });
        }
    };

    while let Some(c) = chars.next() {
        match c {
            '\x1b' => {
                // Escape sequence: ONLY CSI-SGR (`ESC[…m`) is interpreted;
                // the rest is consumed and discarded.
                if chars.peek() == Some(&'[') {
                    chars.next(); // '['
                    let mut params = String::new();
                    let mut final_byte = None;
                    for pc in chars.by_ref() {
                        // A CSI's final byte: 0x40..=0x7E.
                        if ('\u{40}'..='\u{7E}').contains(&pc) {
                            final_byte = Some(pc);
                            break;
                        }
                        params.push(pc);
                    }
                    if final_byte == Some('m') {
                        // SGR: apply to the color in effect after closing the span.
                        flush(&mut cur, fg, &mut line);
                        apply_sgr(&params, &mut fg);
                    }
                    // Any other CSI (cursor, clearing…) is discarded.
                } else if chars.peek() == Some(&']') {
                    // OSC (`ESC]…`): window title, hyperlinks… up to BEL
                    // or ST (`ESC\`). Consumed whole and discarded.
                    chars.next(); // ']'
                    while let Some(pc) = chars.next() {
                        if pc == '\x07' {
                            break;
                        }
                        if pc == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                } else {
                    // Unknown escape: discards the next character (if there
                    // is one) — it is never forwarded to the terminal.
                    chars.next();
                }
            }
            '\n' => {
                flush(&mut cur, fg, &mut line);
                lines.push(std::mem::take(&mut line));
            }
            '\r' => { /* CR: CRLF normalization — never moves the cursor */ }
            // Other control bytes (BEL, backspace, tab…) are KEPT in the
            // text: masking to `�` is done by the frontend's display layer
            // ([`crate::display_name`]), which already handles
            // bidi/invisibles. Here only ESCAPE SEQUENCES are sanitized
            // (above), which span several chars and display_name would not
            // know how to recognize.
            c => cur.push(c),
        }
    }
    flush(&mut cur, fg, &mut line);
    lines.push(line);
    lines
}

/// Applies a list of `;`-separated SGR parameters to the foreground color in
/// effect. Interprets: `0` (reset), `39` (default fg), `38;2;r;g;b`
/// (24-bit), `38;5;n` (256 → RGB). Correctly consumes `48;…` (background)
/// and ignores it; any other parameter (bold, underline…) is also ignored.
fn apply_sgr(params: &str, fg: &mut Option<Rgb>) {
    // An empty SGR (`ESC[m`) is equivalent to reset.
    if params.is_empty() {
        *fg = None;
        return;
    }
    let mut it = params.split(';').map(|p| p.parse::<u16>().unwrap_or(0));
    while let Some(code) = it.next() {
        match code {
            0 | 39 => *fg = None,
            38 => *fg = take_extended_color(&mut it).or(*fg),
            48 => {
                // Background: consume its sub-parameters (so as not to
                // misinterpret them) but do NOT apply it (no painting a
                // background from a plugin).
                let _ = take_extended_color(&mut it);
            }
            30..=37 => *fg = Some(ansi16_to_rgb(code - 30, false)),
            90..=97 => *fg = Some(ansi16_to_rgb(code - 90, true)),
            _ => {} // bold/italic/etc.: ignored
        }
    }
}

/// Consumes an extended color after `38`/`48`: `2;r;g;b` (24-bit) or `5;n`
/// (256). `None` if the shape does not fit (sub-parameters already consumed).
fn take_extended_color(it: &mut impl Iterator<Item = u16>) -> Option<Rgb> {
    match it.next()? {
        2 => {
            let r = it.next()?;
            let g = it.next()?;
            let b = it.next()?;
            Some((clamp8(r), clamp8(g), clamp8(b)))
        }
        5 => Some(xterm256_to_rgb(clamp8(it.next()?))),
        _ => None,
    }
}

fn clamp8(v: u16) -> u8 {
    u8::try_from(v).unwrap_or(255)
}

/// The 16 base ANSI colors → RGB (xterm's standard palette).
fn ansi16_to_rgb(idx: u16, bright: bool) -> Rgb {
    const NORMAL: [Rgb; 8] = [
        (0, 0, 0),
        (205, 0, 0),
        (0, 205, 0),
        (205, 205, 0),
        (0, 0, 238),
        (205, 0, 205),
        (0, 205, 205),
        (229, 229, 229),
    ];
    const BRIGHT: [Rgb; 8] = [
        (127, 127, 127),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (92, 92, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    let i = (idx as usize).min(7);
    if bright { BRIGHT[i] } else { NORMAL[i] }
}

/// xterm-256 index → RGB: 0..15 base, 16..231 6×6×6 cube, 232..255 grays.
fn xterm256_to_rgb(n: u8) -> Rgb {
    match n {
        0..=7 => ansi16_to_rgb(u16::from(n), false),
        8..=15 => ansi16_to_rgb(u16::from(n - 8), true),
        16..=231 => {
            let n = n - 16;
            let level = |v: u8| -> u8 { if v == 0 { 0 } else { 55 + v * 40 } };
            (level(n / 36), level((n / 6) % 6), level(n % 6))
        }
        _ => {
            let v = 8 + (n - 232) * 10;
            (v, v, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_one_line_no_color() {
        let out = parse_sgr("hola mundo");
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0],
            vec![StyledSpan {
                text: "hola mundo".into(),
                role: None,
                fg: None,
                bg: None,
            }]
        );
    }

    #[test]
    fn sgr_24_bit_colors_the_span() {
        // ESC[38;2;255;0;0m red ESC[0m
        let out = parse_sgr("\x1b[38;2;255;0;0mrojo\x1b[0m fin");
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0],
            vec![
                StyledSpan {
                    text: "rojo".into(),
                    role: None,
                    fg: Some((255, 0, 0)),
                    bg: None,
                },
                StyledSpan {
                    text: " fin".into(),
                    role: None,
                    fg: None,
                    bg: None,
                },
            ]
        );
    }

    #[test]
    fn colors_16_and_256() {
        let out = parse_sgr("\x1b[31mA\x1b[38;5;46mB");
        assert_eq!(
            out[0][0],
            StyledSpan {
                text: "A".into(),
                role: None,
                fg: Some((205, 0, 0)),
                bg: None,
            }
        );
        // 46 = cube: n=30 → (0,255,0)
        assert_eq!(out[0][1].text, "B");
        assert_eq!(out[0][1].fg, Some((0, 255, 0)));
    }

    #[test]
    fn line_breaks_and_crlf() {
        let out = parse_sgr("a\r\nb\nc");
        assert_eq!(out.len(), 3);
        assert_eq!(out[0][0].text, "a"); // the \r does not show up
        assert_eq!(out[1][0].text, "b");
        assert_eq!(out[2][0].text, "c");
    }

    /// SANITIZED: a hostile OSC (sets the window title) is discarded
    /// whole — it never reaches the terminal.
    #[test]
    fn a_hostile_osc_is_discarded() {
        let out = parse_sgr("antes\x1b]0;PWNED\x07despues");
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0],
            vec![StyledSpan {
                text: "antesdespues".into(),
                role: None,
                fg: None,
                bg: None,
            }]
        );
    }

    /// SANITIZED: a CSI that is NOT SGR (clear screen, move cursor) is discarded.
    #[test]
    fn a_non_sgr_csi_is_discarded() {
        let out = parse_sgr("x\x1b[2J\x1b[10;5Hy");
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0],
            vec![StyledSpan {
                text: "xy".into(),
                role: None,
                fg: None,
                bg: None,
            }]
        );
    }

    /// SANITIZED: a bare escape (`ESC Z`) is discarded (consumes the `Z`);
    /// LOOSE control bytes (BEL, backspace) are KEPT in the text so that
    /// `display_name` masks them to `�` afterwards.
    #[test]
    fn bare_escape_is_discarded_loose_controls_are_kept() {
        let out = parse_sgr("a\x07b\x08\x1bZc\td");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0][0].text, "a\x07b\x08c\td");
    }

    #[test]
    fn an_empty_sgr_is_a_reset() {
        let out = parse_sgr("\x1b[31mA\x1b[mB");
        assert_eq!(out[0][0].fg, Some((205, 0, 0)));
        assert_eq!(
            out[0][1],
            StyledSpan {
                text: "B".into(),
                role: None,
                fg: None,
                bg: None,
            }
        );
    }
}
