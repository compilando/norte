//! A terminal's grid: the bytes a pty writes, parsed into cells, cursor and
//! attributes.
//!
//! It is the PURE half of a terminal pane, and the split is the same one
//! `norte-frontend`/`norte-tui` already have for the subshell (ADR 0084):
//! there is no pty here, no threads, no toolkit, no theme. Bytes go in
//! through [`Screen::alimentar`] and a grid comes out that anyone can
//! paint — the terminal with `ratatui`, the window as rows of spans through
//! the bridge.
//!
//! # What this crate does NOT decide
//!
//! **An index's color.** A terminal says "color 1" and what red that means
//! is the THEME's business, not the terminal's: [`ColorTerm::Indexed`]
//! travels as-is and whoever paints resolves it, with whatever palette the
//! reader has set. That is why this does not depend on `norte-theme`: if the
//! grid translated to RGB, the pane would stop obeying the theme and nobody
//! could fix it from the theme.
//!
//! **What gets masked.** A program inside the pane is FOREIGN content, and
//! what it paints goes through the same masking as a filename. What this
//! grid guarantees is simpler and is the basis for that: **no control byte
//! can end up in a cell**. The parser eats the escapes and throws away what
//! it does not understand, so a half `\x1b[` or a whole OSC paint nothing.
//!
//! ```
//! use norte_term::Screen;
//!
//! let mut p = Screen::new(10, 2);
//! p.alimentar(b"hola\r\nmundo");
//! assert_eq!(p.row_text(0).trim_end(), "hola");
//! assert_eq!(p.row_text(1).trim_end(), "mundo");
//! assert_eq!(p.cursor(), (1, 5));
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

#[cfg(feature = "pty")]
pub mod pty;

use unicode_width::UnicodeWidthChar as _;

/// A color exactly as the terminal SAYS it, unresolved.
///
/// The three cases are the three that exist on a terminal's wire, and they
/// are kept distinct on purpose: an index is resolved by whoever paints's
/// theme (see the crate doc), and an RGB one already came decided by the
/// program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorTerm {
    /// None: whoever paints's normal background or text color.
    #[default]
    Default,
    /// One of the palette's 256. 0..=15 are the "usual" ones.
    Indexed(u8),
    /// An exact one, chosen by the program (`CSI 38;2;r;g;b m`).
    Rgb(u8, u8, u8),
}

/// A cell's attributes.
// The six are independent SGR flags, and the terminal sets and clears them
// one at a time (`SGR 1` / `SGR 22`). A struct of bools IS that
// representation; packing them into flags would invent a shape a terminal's
// wire does not have, and it is the same criterion `norte_theme::Style` uses
// for its own.
#[expect(
    clippy::struct_excessive_bools,
    reason = "six independent SGR attributes, not an enum or packed flags"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    /// Text color.
    pub fg: ColorTerm,
    /// Background color.
    pub bg: ColorTerm,
    /// `SGR 1`.
    pub negrita: bool,
    /// `SGR 2`.
    pub tenue: bool,
    /// `SGR 3`.
    pub cursiva: bool,
    /// `SGR 4`.
    pub subrayado: bool,
    /// `SGR 7`: the colors are swapped WHEN PAINTING, not here. Storing it
    /// already resolved would lose which was which, and `SGR 27` has to be
    /// able to undo it.
    pub inverse: bool,
    /// `SGR 9`.
    pub tachado: bool,
}

/// A grid cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// What is seen. A space if nothing has been written.
    pub c: char,
    /// How it looks.
    pub style: Style,
    /// The second half of a WIDE character: it is not painted, and it exists
    /// so the columns keep lining up.
    pub estela: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            c: ' ',
            style: Style::default(),
            estela: false,
        }
    }
}

/// The grid and the cursor: the state, without the parser.
///
/// It is separate from [`Screen`] for a mechanical reason, not a design
/// one: `vte::Parser::advance` borrows the parser AND whoever receives the
/// events, and being the same object that is not possible. With two fields,
/// it is.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Grid {
    /// Width in columns.
    width: u16,
    /// Height in rows.
    alto: u16,
    /// `alto * width` cells, row by row.
    cells: Vec<Cell>,
    /// Cursor row.
    row: u16,
    /// Cursor column. CAN equal `width`: it is the "pending wrap" state a
    /// real terminal has, and without it writing right at the right edge
    /// would wrap a cell too early.
    col: u16,
    /// Hidden by `CSI ?25l`.
    cursor_visible: bool,
    /// The style currently being written with.
    style: Style,
    /// The scroll REGION, `[top, bottom]` inclusive (`DECSTBM`).
    ///
    /// Without it the whole screen scrolls up when the cursor falls off the
    /// bottom. With it, ONLY that chunk scrolls, which is what any program
    /// that pins a status line uses: `tmux`, a `top`, an installer with a
    /// progress bar. Without implementing it, the pinned line would drift
    /// upward and eventually scroll off the screen.
    region: (u16, u16),
    /// The cursor saved by `ESC 7` / `CSI s`, and the style with it.
    ///
    /// The style goes INSIDE because `DECSC` saves it: restoring the
    /// position and leaving the color from somewhere else is what makes a
    /// prompt come out half one color.
    saved: Option<(u16, u16, Style)>,
    /// The last PRINTED character, for `REP` (`CSI b`).
    last: Option<char>,
    /// Is the line-drawing character set (`ESC ( 0`) active?
    drawing: bool,
    /// The screen `CSI ?1049h` set aside, if one was set aside.
    ///
    /// It is what keeps a `less` or a `vim` from leaving their last frame
    /// stuck on exit: they enter, paint over a clean screen, and on exit the
    /// previous one is given back EXACTLY AS IT WAS —cells, cursor and
    /// style—. Of the two modes, the modern one (`1049`) also saves the
    /// cursor; the old one (`47`) does not, and that difference is honored.
    toggles: Option<Box<Saved>>,
}

/// A screen set aside by the alternate screen.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Saved {
    cells: Vec<Cell>,
    row: u16,
    col: u16,
    style: Style,
    region: (u16, u16),
}

impl Grid {
    fn new(width: u16, alto: u16) -> Self {
        let (width, alto) = (width.max(1), alto.max(1));
        Self {
            width,
            alto,
            cells: vec![Cell::default(); usize::from(width) * usize::from(alto)],
            row: 0,
            col: 0,
            cursor_visible: true,
            style: Style::default(),
            region: (0, alto - 1),
            saved: None,
            last: None,
            drawing: false,
            toggles: None,
        }
    }

    /// `DECSC`: the cursor's position AND the style it was writing with.
    fn save_cursor(&mut self) {
        self.saved = Some((self.row, self.col, self.style));
    }

    /// `DECRC`. Does nothing with nothing saved, which is what the spec
    /// says: making up the top-left corner would move the cursor of a
    /// program that only wanted to make sure.
    fn restore_cursor(&mut self) {
        if let Some((row, col, style)) = self.saved {
            self.row = row.min(self.alto - 1);
            self.col = col.min(self.width);
            self.style = style;
        }
    }

    /// Enters or leaves the alternate screen.
    ///
    /// `con_cursor` distinguishes the modern mode (`1049`, also saves where
    /// the cursor was) from the old one (`47`, screen only). Entering twice
    /// does not stack: the second time does nothing, or the real screen
    /// would be lost.
    fn screen_toggles(&mut self, enter: bool, con_cursor: bool) {
        if enter {
            if self.toggles.is_some() {
                return;
            }
            self.toggles = Some(Box::new(Saved {
                cells: self.cells.clone(),
                row: self.row,
                col: self.col,
                style: self.style,
                region: self.region,
            }));
            // The alternate one starts CLEAN and without a region: it is a
            // new screen.
            self.cells = vec![Cell::default(); self.cells.len()];
            self.region = (0, self.alto - 1);
            if con_cursor {
                self.row = 0;
                self.col = 0;
            }
            return;
        }
        let Some(g) = self.toggles.take() else {
            return;
        };
        self.cells = g.cells;
        self.region = g.region;
        if con_cursor {
            self.row = g.row.min(self.alto - 1);
            self.col = g.col.min(self.width);
            self.style = g.style;
        }
    }

    /// `ED`: clears the screen. `0` from here down, `1` from here up, and
    /// anything else, the whole thing.
    fn delete_screen(&mut self, modo: u16) {
        let (row, col, alto, width) = (self.row, self.col, self.alto, self.width);
        match modo {
            0 => {
                self.delete_row(row, col, width);
                for f in row + 1..alto {
                    self.delete_row(f, 0, width);
                }
            }
            1 => {
                for f in 0..row {
                    self.delete_row(f, 0, width);
                }
                self.delete_row(row, 0, col.saturating_add(1));
            }
            _ => {
                for f in 0..alto {
                    self.delete_row(f, 0, width);
                }
            }
        }
    }

    /// `EL`: the same, within the cursor's row.
    fn delete_line(&mut self, modo: u16) {
        let (row, col, width) = (self.row, self.col, self.width);
        match modo {
            0 => self.delete_row(row, col, width),
            1 => self.delete_row(row, 0, col.saturating_add(1)),
            _ => self.delete_row(row, 0, width),
        }
    }

    /// `ICH`: opens `n` gaps in the cursor's row, pushing to the right.
    fn insertar_cells(&mut self, n: u16) {
        let (row, col, width) = (self.row, self.col, self.width);
        let n = n.min(width.saturating_sub(col));
        for c in (col..width).rev() {
            let source = c.checked_sub(n).filter(|o| *o >= col);
            let cell = source.and_then(|o| self.index(row, o).map(|i| self.cells[i].clone()));
            if let Some(i) = self.index(row, c) {
                self.cells[i] = cell.unwrap_or_default();
            }
        }
    }

    /// `DCH`: takes `n` cells away from the cursor's row, pulling the rest in.
    fn delete_cells(&mut self, n: u16) {
        let (row, col, width) = (self.row, self.col, self.width);
        let n = n.min(width.saturating_sub(col));
        for c in col..width {
            let source = c.checked_add(n).filter(|o| *o < width);
            let cell = source.and_then(|o| self.index(row, o).map(|i| self.cells[i].clone()));
            if let Some(i) = self.index(row, c) {
                self.cells[i] = cell.unwrap_or_default();
            }
        }
    }

    fn index(&self, row: u16, col: u16) -> Option<usize> {
        (row < self.alto && col < self.width)
            .then(|| usize::from(row) * usize::from(self.width) + usize::from(col))
    }

    /// Moves down one row, and if it was already at the REGION'S BOTTOM
    /// EDGE, scrolls its content up.
    ///
    /// Without this, the last thing a shell writes is exactly what does not
    /// show. And without looking at the region, a program with a pinned
    /// status line would see that line drift upward until it disappeared.
    ///
    /// Outside the region the cursor just moves down: there is no scroll
    /// there, which is exactly what the region promises.
    fn down(&mut self) {
        let (up, abajo) = self.region;
        if self.row == abajo {
            self.up_region(up, abajo, 1);
            return;
        }
        if self.row + 1 < self.alto {
            self.row += 1;
        }
    }

    /// Scrolls the `[up, down]` chunk up `n` rows, filling in from the
    /// bottom.
    fn up_region(&mut self, up: u16, abajo: u16, n: u16) {
        if up > abajo {
            return;
        }
        let alto_region = abajo - up + 1;
        let n = n.min(alto_region);
        for f in up..=abajo {
            let source = f + n;
            for c in 0..self.width {
                let cell = if source <= abajo {
                    self.index(source, c).map(|i| self.cells[i].clone())
                } else {
                    None
                };
                if let Some(i) = self.index(f, c) {
                    self.cells[i] = cell.unwrap_or_default();
                }
            }
        }
    }

    /// Scrolls the `[up, down]` chunk down `n` rows, filling in from
    /// the top. It is the inverse of [`Self::up_region`], and `IL` uses
    /// it.
    fn down_region(&mut self, up: u16, abajo: u16, n: u16) {
        if up > abajo {
            return;
        }
        let alto_region = abajo - up + 1;
        let n = n.min(alto_region);
        for f in (up..=abajo).rev() {
            let source = f.checked_sub(n).filter(|o| *o >= up);
            for c in 0..self.width {
                let cell = source.and_then(|o| self.index(o, c).map(|i| self.cells[i].clone()));
                if let Some(i) = self.index(f, c) {
                    self.cells[i] = cell.unwrap_or_default();
                }
            }
        }
    }

    /// The character `ESC ( 0` paints instead of `c` (DEC Special Graphics).
    ///
    /// Only the `0x60..=0x7e` range, which is what the set redefines; the
    /// rest is left as is. Without this, a program drawing a box would
    /// print `lqqqk` where it wanted a corner and three lines.
    fn draw(c: char) -> char {
        const TABLE: &str = "◆▒␉␌␍␊°±␤␋┘┐┌└┼⎺⎻─⎼⎽├┤┴┬│≤≥π≠£·";
        let i = (c as u32)
            .checked_sub(0x60)
            .and_then(|i| usize::try_from(i).ok());
        i.and_then(|i| TABLE.chars().nth(i)).unwrap_or(c)
    }

    /// Writes a character wherever the cursor is and advances it.
    fn set(&mut self, c: char) {
        let c = if self.drawing { Self::draw(c) } else { c };
        // For `REP`, which repeats the last PRINTED one — the translated
        // one included: what gets repeated is what shows.
        self.last = Some(c);
        // A zero-width character —a combining one— has no cell of its own.
        // It is DROPPED, and that is a known limitation: the correct thing
        // is to attach it to the previous character, and that requires a
        // cell to store a cluster and not a `char`. Until then, dropping is
        // what does not throw the columns off.
        let width_c = u16::try_from(c.width().unwrap_or(0)).unwrap_or(0);
        if width_c == 0 {
            return;
        }
        // Saturating like the cursor movements, and for the same reason:
        // the column comes from a foreign parameter. It would take a
        // 65528-column grid to overflow here —i.e. never—, but closing the
        // whole class at once beats leaving four spots that have to be
        // re-reasoned about every time someone reads them.
        if self.col.saturating_add(width_c) > self.width {
            self.col = 0;
            self.down();
        }
        let (row, col, style) = (self.row, self.col, self.style);
        if let Some(i) = self.index(row, col) {
            self.cells[i] = Cell {
                c,
                style,
                estela: false,
            };
        }
        for d in 1..width_c {
            if let Some(i) = self.index(row, col + d) {
                self.cells[i] = Cell {
                    c: ' ',
                    style,
                    estela: true,
                };
            }
        }
        // Clamped to `width`, not a loose `+=`: a WIDE character on a
        // ONE-column grid does not fit even after wrapping —the jump above
        // leaves `col` at 0 and it still does not fit— so adding 2 left the
        // cursor at column 2 of a grid that only reaches 1. proptest found
        // it, which is what it is for: a one-column grid occurs to nobody
        // and a pty produces one as soon as the window narrows.
        //
        // `width` and not `width - 1` because that is the pending-wrap
        // position, which is a legitimate cursor state here.
        self.col = self.col.saturating_add(width_c).min(self.width);
    }

    /// Leaves `rango` of row `row`'s cells as freshly placed.
    fn delete_row(&mut self, row: u16, from: u16, until: u16) {
        for col in from..until.min(self.width) {
            if let Some(i) = self.index(row, col) {
                self.cells[i] = Cell::default();
            }
        }
    }

    /// `SGR`: the attributes, with the two that carry arguments inside.
    fn sgr(&mut self, codes: &[u16]) {
        // A bare `CSI m` is a `CSI 0 m`: reset.
        if codes.is_empty() {
            self.style = Style::default();
            return;
        }
        let mut i = 0;
        while i < codes.len() {
            let n = codes[i];
            match n {
                0 => self.style = Style::default(),
                1 => self.style.negrita = true,
                2 => self.style.tenue = true,
                3 => self.style.cursiva = true,
                4 => self.style.subrayado = true,
                7 => self.style.inverse = true,
                9 => self.style.tachado = true,
                // 21 is "double underline" on some terminals and "remove
                // bold" on others; it is treated like 22, which is what the
                // ones that matter do.
                21 | 22 => {
                    self.style.negrita = false;
                    self.style.tenue = false;
                }
                23 => self.style.cursiva = false,
                24 => self.style.subrayado = false,
                27 => self.style.inverse = false,
                29 => self.style.tachado = false,
                30..=37 => self.style.fg = ColorTerm::Indexed(u8_de(n - 30)),
                39 => self.style.fg = ColorTerm::Default,
                40..=47 => self.style.bg = ColorTerm::Indexed(u8_de(n - 40)),
                49 => self.style.bg = ColorTerm::Default,
                // The "bright" ones are indices 8..=15 of the SAME palette,
                // nothing else: resolving them here to an RGB would take
                // away the theme's ability to say what they look like.
                90..=97 => self.style.fg = ColorTerm::Indexed(u8_de(n - 90 + 8)),
                100..=107 => self.style.bg = ColorTerm::Indexed(u8_de(n - 100 + 8)),
                38 | 48 => {
                    let (color, read) = color_extendido(&codes[i + 1..]);
                    if let Some(color) = color {
                        if n == 38 {
                            self.style.fg = color;
                        } else {
                            self.style.bg = color;
                        }
                    }
                    i += read;
                }
                _ => {}
            }
            i += 1;
        }
    }
}

/// A `u16` from an SGR code that is ALREADY known to be small, without `unwrap`.
fn u8_de(n: u16) -> u8 {
    u8::try_from(n).unwrap_or(0)
}

/// Reads what follows a `38`/`48`: `5;n` (palette) or `2;r;g;b` (exact).
///
/// Returns the color and HOW MANY codes it ate, so the caller can continue
/// from the right place. A malformed `38` still swallows its arguments:
/// letting them through would interpret them as loose attributes and paint
/// with whatever.
fn color_extendido(rest: &[u16]) -> (Option<ColorTerm>, usize) {
    match rest.first() {
        Some(5) => (
            rest.get(1).map(|n| ColorTerm::Indexed(u8_de(*n))),
            rest.len().min(2),
        ),
        Some(2) => (
            match (rest.get(1), rest.get(2), rest.get(3)) {
                (Some(r), Some(g), Some(b)) => {
                    Some(ColorTerm::Rgb(u8_de(*r), u8_de(*g), u8_de(*b)))
                }
                _ => None,
            },
            rest.len().min(4),
        ),
        _ => (None, 0),
    }
}

impl vte::Perform for Grid {
    fn print(&mut self, c: char) {
        self.set(c);
    }

    /// The C0s that move the cursor. **Everything else is dropped**, and
    /// that is the crate's guarantee: a control byte cannot end up in a
    /// cell.
    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' | 0x0b | 0x0c => self.down(),
            b'\r' => self.col = 0,
            0x08 => self.col = self.col.saturating_sub(1),
            b'\t' => self.col = (self.col / 8).saturating_add(1).saturating_mul(8),
            _ => return,
        }
        self.col = self.col.min(self.width);
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermedios: &[u8],
        _ignores: bool,
        action: char,
    ) {
        // Sub-parameters (`38:2:…`) are flattened together with parameters
        // (`38;2;…`): they are two ways of writing the same thing and no
        // program should notice which one was used.
        let codes: Vec<u16> = params.iter().flat_map(|p| p.iter().copied()).collect();
        // A missing parameter is worth 1 for movements and 0 for clears,
        // which is what ECMA-48 says and what a `tput` expects.
        let one = |n: usize| codes.get(n).copied().filter(|v| *v != 0).unwrap_or(1);
        let zero = |n: usize| codes.get(n).copied().unwrap_or(0);
        // `?` arrives as a private marker, and without checking it a plain
        // `CSI 25 l` would hide the cursor without anyone asking for it.
        let privado = intermedios.first() == Some(&b'?');
        match action {
            'm' if !privado => self.sgr(&codes),
            // `CSI H` counts from ONE; the grid, from zero.
            'H' | 'f' if !privado => {
                self.row = (one(0) - 1).min(self.alto - 1);
                self.col = (one(1) - 1).min(self.width - 1);
            }
            // All four SATURATE, and all four have to: `one` comes from a
            // parameter another program wrote and it can be 65535. Adding
            // it overflowed the `u16` and, with checks on —the profile the
            // suite and the dev binary run with—, that is a panic any file
            // with `ESC [ 6 5 5 3 5 C` inside triggers. That `A` and `D`
            // were saturating and `B` and `C` were not was not the visible
            // symptom.
            'A' => self.row = self.row.saturating_sub(one(0)),
            'B' => self.row = self.row.saturating_add(one(0)).min(self.alto - 1),
            'C' => self.col = self.col.saturating_add(one(0)).min(self.width - 1),
            'D' => self.col = self.col.saturating_sub(one(0)),
            'J' if !privado => self.delete_screen(zero(0)),
            'K' if !privado => self.delete_line(zero(0)),
            // Absolute position on ONE axis: `CHA` the column, `VPA` the
            // row. Cheap and constant — a prompt that repaints itself uses
            // them on every keystroke—, and without them it kept writing
            // wherever it was.
            'G' | '`' if !privado => self.col = (one(0) - 1).min(self.width - 1),
            'd' if !privado => self.row = (one(0) - 1).min(self.alto - 1),
            // Insert and delete LINES, within the region and from the
            // cursor: what an editor uses to open a gap without repainting
            // the rest.
            'L' if !privado => {
                let (up, abajo) = self.region;
                if self.row >= up && self.row <= abajo {
                    self.down_region(self.row, abajo, one(0));
                }
            }
            'M' if !privado => {
                let (up, abajo) = self.region;
                if self.row >= up && self.row <= abajo {
                    self.up_region(self.row, abajo, one(0));
                }
            }
            // Insert and delete CHARACTERS on the cursor's row, and clear
            // without moving: what a line editor uses so it does not
            // repaint the whole line on every key.
            '@' if !privado => self.insertar_cells(one(0)),
            'P' if !privado => self.delete_cells(one(0)),
            'X' if !privado => {
                let (row, col) = (self.row, self.col);
                self.delete_row(row, col, col.saturating_add(one(0)));
            }
            // Scrolls the region up and down without moving the cursor
            // (`SU`/`SD`).
            'S' if !privado => {
                let (up, abajo) = self.region;
                self.up_region(up, abajo, one(0));
            }
            'T' if !privado => {
                let (up, abajo) = self.region;
                self.down_region(up, abajo, one(0));
            }
            // `REP`: repeat the last printed character. A `tput rep` uses it
            // to paint a line of dashes with four bytes.
            'b' if !privado => {
                if let Some(c) = self.last {
                    for _ in 0..one(0) {
                        self.set(c);
                    }
                }
            }
            // `DECSTBM`: the scroll region. Without parameters it goes back
            // to being the whole screen, and the cursor goes to its corner —
            // the spec says so and the programs that set it take it for
            // granted.
            'r' if !privado => {
                let up = codes.first().copied().filter(|v| *v != 0).unwrap_or(1) - 1;
                let abajo = codes
                    .get(1)
                    .copied()
                    .filter(|v| *v != 0)
                    .unwrap_or(self.alto)
                    - 1;
                let (up, abajo) = (up.min(self.alto - 1), abajo.min(self.alto - 1));
                // A backwards or single-row region is not accepted: there is
                // nothing to scroll and leaving it set breaks normal scrolling.
                if up < abajo {
                    self.region = (up, abajo);
                    self.row = up;
                    self.col = 0;
                }
            }
            // `SCP`/`RCP`, the twins of `ESC 7`/`ESC 8` in CSI form.
            's' if !privado => self.save_cursor(),
            'u' if !privado => self.restore_cursor(),
            'h' | 'l' if privado => {
                let turn_on = action == 'h';
                match codes.first() {
                    Some(&25) => self.cursor_visible = turn_on,
                    // The ALTERNATE screen. `1049` is the modern one —sets
                    // aside the screen AND the cursor— and `47`/`1047` the
                    // old one, which only sets aside the screen. It is what
                    // keeps a `less` from leaving its last frame stuck on
                    // exit.
                    Some(&1049) => self.screen_toggles(turn_on, true),
                    Some(&47 | &1047) => self.screen_toggles(turn_on, false),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermedios: &[u8], _ignores: bool, byte: u8) {
        match (intermedios.first(), byte) {
            // `DECSC`/`DECRC`: save and restore the cursor.
            (None, b'7') => self.save_cursor(),
            (None, b'8') => self.restore_cursor(),
            // `RIS`: full reset. Sent by a `reset`, and also by a program
            // that found the screen in a state it does not understand —
            // which is exactly when it must be obeyed.
            (None, b'c') => {
                let (width, alto) = (self.width, self.alto);
                *self = Self::new(width, alto);
            }
            // `ESC ( 0` and `ESC ( B`: the line-drawing character set and
            // the return to ASCII. Ignoring it made a program that draws
            // boxes print `lqqqk` where it wanted corners.
            (Some(b'('), b'0') => self.drawing = true,
            (Some(b'('), b'B') => self.drawing = false,
            // `IND` and `NEL` move down one line (with region scroll); `RI`
            // moves up.
            (None, b'D') => self.down(),
            (None, b'E') => {
                self.down();
                self.col = 0;
            }
            (None, b'M') => {
                let (up, abajo) = self.region;
                if self.row == up {
                    self.down_region(up, abajo, 1);
                } else {
                    self.row = self.row.saturating_sub(1);
                }
            }
            _ => {}
        }
    }
}

/// A terminal's screen: the grid and the escape parser.
pub struct Screen {
    /// The state.
    grid: Grid,
    /// The escape state machine.
    parser: vte::Parser,
}

impl Screen {
    /// An empty `width` by `alto` screen.
    ///
    /// A size of zero on either axis is bumped to one: a grid with no cells
    /// has nowhere to put the cursor, and whoever paints a zero-column pane
    /// does not want a `None` in every cell, they want it to not crash.
    #[must_use]
    pub fn new(width: u16, alto: u16) -> Self {
        Self {
            grid: Grid::new(width, alto),
            parser: vte::Parser::new(),
        }
    }

    /// The size, in columns and rows.
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        (self.grid.width, self.grid.alto)
    }

    /// Where the cursor is: row and column.
    ///
    /// The column can be as large as the width, and that is not a bug: it
    /// is the terminal waiting to see whether the next thing to arrive
    /// needs to wrap.
    #[must_use]
    pub fn cursor(&self) -> (u16, u16) {
        (self.grid.row, self.grid.col)
    }

    /// Is the cursor painted? (`CSI ?25l` / `CSI ?25h`).
    #[must_use]
    pub fn cursor_visible(&self) -> bool {
        self.grid.cursor_visible
    }

    /// The cell at `row`, `col`, or `None` if it falls outside.
    #[must_use]
    pub fn cell(&self, row: u16, col: u16) -> Option<&Cell> {
        self.grid
            .index(row, col)
            .and_then(|i| self.grid.cells.get(i))
    }

    /// A row split into SPANS: consecutive text that shares a style.
    ///
    /// It is what anyone painting needs, and that is why it lives here and
    /// not in every frontend: the terminal makes a `ratatui` span per span
    /// and the window a `TerminalSpanView`, but *where* the cut falls is
    /// the same decision and it is made once.
    ///
    /// Grouping is not cosmetic. An eighty-cell row is eighty fragments if
    /// not grouped, and that gets paid on every repaint — through the
    /// bridge, moreover, where they are eighty JSON objects.
    ///
    /// A wide character's trailing cell does not come out on its own: it is
    /// already inside its character's span.
    ///
    /// ```
    /// use norte_term::Screen;
    ///
    /// let mut p = Screen::new(8, 1);
    /// p.alimentar(b"ab\x1b[31mcd");
    /// let tramos = p.row_tramos(0);
    /// // Three: the normal one, the red one, and the trailing padding —which
    /// // is NOT red, and folding it into the previous span would paint the
    /// // rest of the line red.
    /// assert_eq!(tramos.len(), 3);
    /// assert_eq!(tramos[0].0, "ab");
    /// assert_eq!(tramos[1].0, "cd");
    /// assert_eq!(tramos[2].0, "    ");
    /// ```
    #[must_use]
    pub fn row_tramos(&self, row: u16) -> Vec<(String, Style)> {
        let mut tramos: Vec<(String, Style)> = Vec::new();
        for c in (0..self.grid.width).filter_map(|c| self.cell(row, c)) {
            if c.estela {
                continue;
            }
            match tramos.last_mut() {
                Some((text, style)) if *style == c.style => text.push(c.c),
                _ => tramos.push((c.c.to_string(), c.style)),
            }
        }
        tramos
    }

    /// A row's text, with the trailing cells removed.
    ///
    /// Exists for tests and for whoever wants a line in one go; whoever
    /// paints with styles iterates [`Self::cell`] or [`Self::row_tramos`].
    #[must_use]
    pub fn row_text(&self, row: u16) -> String {
        (0..self.grid.width)
            .filter_map(|c| self.cell(row, c))
            .filter(|c| !c.estela)
            .map(|c| c.c)
            .collect()
    }

    /// Feeds it pty bytes.
    ///
    /// Can be called with whatever chunks a read returns, split wherever:
    /// the parser keeps what it has halfway through, which is normal when
    /// an escape falls on the boundary between two reads.
    pub fn alimentar(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.grid, bytes);
    }

    /// Changes the size, keeping what fits.
    ///
    /// Text is not re-wrapped to the width: what overflows to the right is
    /// lost, and so is what overflows at the bottom. Re-wrapping requires
    /// knowing which lines used to be a single one split into two, and this
    /// grid does not store that — neither do half the terminals people use.
    pub fn resize(&mut self, width: u16, alto: u16) {
        let (width, alto) = (width.max(1), alto.max(1));
        let vieja = std::mem::replace(&mut self.grid, Grid::new(width, alto));
        for row in 0..alto.min(vieja.alto) {
            for col in 0..width.min(vieja.width) {
                if let (Some(dest), Some(source)) =
                    (self.grid.index(row, col), vieja.index(row, col))
                {
                    self.grid.cells[dest] = vieja.cells[source].clone();
                }
            }
        }
        self.grid.style = vieja.style;
        self.grid.cursor_visible = vieja.cursor_visible;
        self.grid.row = vieja.row.min(alto - 1);
        self.grid.col = vieja.col.min(width);
        self.grid.drawing = vieja.drawing;
        self.grid.last = vieja.last;
        // The saved cursor and the region are CLAMPED to the new size
        // instead of dropped: a `vim` that resizes while its region is set
        // does not send it again, and losing it would undo its status line.
        // A region that no longer fits goes back to being the whole screen,
        // which is what a real terminal does.
        self.grid.saved = vieja
            .saved
            .map(|(f, c, e)| (f.min(alto - 1), c.min(width), e));
        let (up, abajo) = vieja.region;
        self.grid.region = if up < abajo.min(alto - 1) {
            (up.min(alto - 1), abajo.min(alto - 1))
        } else {
            (0, alto - 1)
        };
        // And the set-aside screen is kept CROPPED: losing it would leave a
        // `less` with nothing to return on exit, which is the bug the
        // alternate screen exists to not have.
        self.grid.toggles = vieja.toggles.map(|g| {
            let mut cells = vec![Cell::default(); usize::from(width) * usize::from(alto)];
            for row in 0..alto.min(vieja.alto) {
                for col in 0..width.min(vieja.width) {
                    let dest = usize::from(row) * usize::from(width) + usize::from(col);
                    let source = usize::from(row) * usize::from(vieja.width) + usize::from(col);
                    if let Some(c) = g.cells.get(source) {
                        cells[dest] = c.clone();
                    }
                }
            }
            Box::new(Saved {
                cells,
                row: g.row.min(alto - 1),
                col: g.col.min(width),
                style: g.style,
                region: (0, alto - 1),
            })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rows with text, without the right-hand padding: it is what a
    /// test wants to compare and writing it by hand in every one was half
    /// the noise.
    fn screen(p: &Screen, alto: u16) -> Vec<String> {
        (0..alto)
            .map(|f| p.row_text(f).trim_end().to_owned())
            .collect()
    }

    /// **The ALTERNATE screen returns exactly what was there** (#366).
    ///
    /// It is the whole point of `CSI ?1049h`: a `less` or a `vim` enters,
    /// paints over a clean screen and on exit the previous one comes back
    /// EXACTLY AS IT WAS. Without this, what stayed stuck in the pane was
    /// the last frame of the program that had just exited — the bug that
    /// looks broken rather than merely lacking.
    #[test]
    fn the_alternate_screen_returns_what_was_there() {
        let mut p = Screen::new(10, 3);
        p.alimentar(b"uno\r\ndos\r\ntres");
        let before = screen(&p, 3);
        let cursor_before = p.cursor();

        // A full-screen program enters and paints its own thing.
        p.alimentar(b"\x1b[?1049h");
        assert_eq!(
            screen(&p, 3),
            vec![String::new(), String::new(), String::new()],
            "the alternate one starts clean"
        );
        assert_eq!(p.cursor(), (0, 0), "and with the cursor in the corner");
        p.alimentar(b"visor");
        assert_eq!(p.row_text(0).trim_end(), "visor");

        // And it exits.
        p.alimentar(b"\x1b[?1049l");
        assert_eq!(screen(&p, 3), before, "what was there did not come back");
        assert_eq!(p.cursor(), cursor_before, "nor the cursor");
    }

    /// **Resizing with the alternate screen set does not lose the screen
    /// underneath.**
    ///
    /// Easy to forget because resizing builds a new grid: if what was set
    /// aside does not travel with it, a `less` whose window the reader
    /// resizes ends up with nothing to return on exit — exactly the bug the
    /// alternate screen exists to not have.
    #[test]
    fn resizing_with_the_alt_screen_on_does_not_lose_what_is_below() {
        let mut p = Screen::new(10, 3);
        p.alimentar(b"real");
        p.alimentar(b"\x1b[?1049h");
        p.alimentar(b"visor");
        p.resize(12, 4);
        p.alimentar(b"\x1b[?1049l");
        assert_eq!(p.row_text(0).trim_end(), "real");
    }

    /// Entering the alternate screen twice does not stack: the second time
    /// does nothing. If it stacked, the REAL screen would be lost by
    /// exiting just once.
    #[test]
    fn entering_the_alternate_screen_twice_does_not_lose_the_real_one() {
        let mut p = Screen::new(10, 2);
        p.alimentar(b"real");
        p.alimentar(b"\x1b[?1049h");
        p.alimentar(b"primera");
        p.alimentar(b"\x1b[?1049h");
        p.alimentar(b"\x1b[?1049l");
        assert_eq!(p.row_text(0).trim_end(), "real");
    }

    /// **The scroll region leaves what is outside it untouched** (#366).
    ///
    /// Anything that pins a status line uses it. Without it, that line
    /// would drift upward on every scroll until it fell off the screen.
    #[test]
    fn the_scroll_region_does_not_move_whats_outside() {
        let mut p = Screen::new(10, 4);
        // A header pinned at the top and the region in the bottom three.
        p.alimentar(b"cabecera\x1b[2;4r");
        // `DECSTBM` leaves the cursor in the region's corner.
        assert_eq!(p.cursor(), (1, 0));
        p.alimentar(b"a\r\nb\r\nc\r\nd");
        assert_eq!(
            screen(&p, 4),
            vec![
                "cabecera".to_owned(),
                "b".to_owned(),
                "c".to_owned(),
                "d".to_owned()
            ],
            "the header moved, or the region did not scroll"
        );
    }

    /// `CHA` and `VPA`: absolute position on a single axis. Any prompt that
    /// repaints itself uses them, and without them it wrote wherever it was.
    #[test]
    fn the_absolute_position_of_a_single_axis() {
        let mut p = Screen::new(10, 3);
        p.alimentar(b"abcdef\x1b[3Gx");
        assert_eq!(p.row_text(0).trim_end(), "abxdef");
        // `VPA` moves the ROW and leaves the column where it was: that is
        // the half that sets it apart from a `CUP`, and the one a prompt
        // takes advantage of.
        assert_eq!(p.cursor(), (0, 3));
        p.alimentar(b"\x1b[3dy");
        assert_eq!(p.cursor().0, 2, "VPA did not go to row 3");
        assert_eq!(p.cell(2, 3).map(|c| c.c), Some('y'));
    }

    /// Inserting and deleting characters on the row: what a line editor
    /// uses so it does not repaint the whole line on every key.
    #[test]
    fn inserting_and_deleting_characters_in_the_row() {
        let mut p = Screen::new(10, 1);
        p.alimentar(b"abcdef\x1b[1G\x1b[2@");
        assert_eq!(p.row_text(0).trim_end(), "  abcdef");
        p.alimentar(b"\x1b[1G\x1b[3P");
        assert_eq!(p.row_text(0).trim_end(), "bcdef");
        // `ECH` clears WITHOUT moving or pulling in the rest.
        p.alimentar(b"\x1b[1G\x1b[2X");
        assert_eq!(p.row_text(0).trim_end(), "  def");
    }

    /// Inserting and deleting LINES, within the region.
    #[test]
    fn inserting_and_deleting_lines() {
        let mut p = Screen::new(10, 4);
        p.alimentar(b"a\r\nb\r\nc\r\nd\x1b[2;1H\x1b[L");
        assert_eq!(
            screen(&p, 4),
            vec![
                "a".to_owned(),
                String::new(),
                "b".to_owned(),
                "c".to_owned()
            ],
        );
        p.alimentar(b"\x1b[2;1H\x1b[M");
        assert_eq!(
            screen(&p, 4),
            vec![
                "a".to_owned(),
                "b".to_owned(),
                "c".to_owned(),
                String::new()
            ],
        );
    }

    /// Saving and restoring the cursor, with its STYLE: restoring the
    /// position and leaving the color from somewhere else is what makes a
    /// prompt come out half-painted.
    #[test]
    fn saving_and_restoring_the_cursor_carries_the_style() {
        let mut p = Screen::new(10, 2);
        p.alimentar(b"\x1b[31m\x1b[1;3H\x1b7");
        p.alimentar(b"\x1b[0m\x1b[2;1Hxx\x1b8y");
        assert_eq!(
            p.cursor(),
            (0, 3),
            "the cursor did not go back to where it was saved"
        );
        let c = p.cell(0, 2).expect("inside");
        assert_eq!(c.c, 'y');
        assert_eq!(
            c.style.fg,
            ColorTerm::Indexed(1),
            "restored the position and not the style"
        );
    }

    /// `RIS` leaves the screen as freshly opened: it is what a `reset`
    /// sends, and also whoever found the screen in a state they do not
    /// understand.
    #[test]
    fn ris_resets_everything() {
        let mut p = Screen::new(10, 3);
        p.alimentar(b"\x1b[31mhola\x1b[2;3r\x1b[?25l");
        p.alimentar(b"\x1bc");
        assert_eq!(
            screen(&p, 3),
            vec![String::new(), String::new(), String::new()]
        );
        assert_eq!(p.cursor(), (0, 0));
        assert!(p.cursor_visible(), "the cursor stayed hidden");
    }

    /// `REP` repeats the last printed character: a `tput rep` paints a line
    /// of dashes with four bytes instead of eighty.
    #[test]
    fn rep_repeats_the_last_one() {
        let mut p = Screen::new(10, 1);
        p.alimentar(b"-\x1b[4b");
        assert_eq!(p.row_text(0).trim_end(), "-----");
    }

    /// The DRAWING set: without it, a program drawing boxes printed
    /// `lqqqk` where it wanted a corner and three lines.
    #[test]
    fn the_line_drawing_set_paints_lines_and_not_letters() {
        let mut p = Screen::new(10, 1);
        p.alimentar(b"\x1b(0lqqk\x1b(Bx");
        assert_eq!(p.row_text(0).trim_end(), "┌──┐x");
    }

    #[test]
    fn the_text_fills_the_row_and_the_line_break_moves_down() {
        let mut p = Screen::new(8, 3);
        p.alimentar(b"uno\r\ndos");
        assert_eq!(p.row_text(0).trim_end(), "uno");
        assert_eq!(p.row_text(1).trim_end(), "dos");
        assert_eq!(p.cursor(), (1, 3));
    }

    /// The right edge does NOT wrap too early: the last column can be
    /// written, and the wrap happens with the next character. Without the
    /// "pending" state, an eight-letter word in eight columns left the last
    /// one on the row below.
    #[test]
    fn the_right_edge_jumps_after_and_not_before() {
        let mut p = Screen::new(4, 2);
        p.alimentar(b"abcd");
        assert_eq!(p.row_text(0), "abcd");
        assert_eq!(p.cursor(), (0, 4), "pending wrap, not wrapped");
        p.alimentar(b"e");
        assert_eq!(p.row_text(1).trim_end(), "e");
        assert_eq!(p.cursor(), (1, 1));
    }

    /// On reaching the bottom, the screen SCROLLS UP: without this the last
    /// thing a shell writes is what does not show.
    #[test]
    fn the_background_scrolls_upward() {
        let mut p = Screen::new(4, 2);
        p.alimentar(b"a\r\nb\r\nc");
        assert_eq!(p.row_text(0).trim_end(), "b");
        assert_eq!(p.row_text(1).trim_end(), "c");
        assert_eq!(p.cursor(), (1, 1));
    }

    #[test]
    fn attributes_are_set_and_unset() {
        let mut p = Screen::new(4, 1);
        p.alimentar(b"\x1b[1;31ma\x1b[0mb");
        let a = p.cell(0, 0).expect("celda");
        assert!(a.style.negrita);
        assert_eq!(a.style.fg, ColorTerm::Indexed(1));
        let b = p.cell(0, 1).expect("celda");
        assert_eq!(b.style, Style::default(), "`SGR 0` limpia todo");
    }

    /// The two SGRs a program uses to ask for an exact color, and the high
    /// index that is NOT the same thing (`38;5;n` is the palette, `38;2;…`
    /// is RGB).
    #[test]
    fn the_exact_color_and_the_high_index_are_not_confused() {
        let mut p = Screen::new(4, 1);
        p.alimentar(b"\x1b[38;2;10;20;30ma\x1b[38;5;200mb\x1b[48;5;7mc");
        assert_eq!(
            p.cell(0, 0).expect("celda").style.fg,
            ColorTerm::Rgb(10, 20, 30)
        );
        assert_eq!(
            p.cell(0, 1).expect("celda").style.fg,
            ColorTerm::Indexed(200)
        );
        assert_eq!(p.cell(0, 2).expect("celda").style.bg, ColorTerm::Indexed(7));
    }

    /// A wide character takes up TWO cells, and the second is a trail: if
    /// not, the rest of the line comes out shifted one column per CJK char.
    #[test]
    fn a_wide_character_occupies_two_cells() {
        let mut p = Screen::new(6, 1);
        p.alimentar("日本x".as_bytes());
        assert_eq!(p.cell(0, 0).expect("celda").c, '日');
        assert!(p.cell(0, 1).expect("celda").estela);
        assert_eq!(p.cell(0, 2).expect("celda").c, '本');
        assert!(p.cell(0, 3).expect("celda").estela);
        assert_eq!(p.cell(0, 4).expect("celda").c, 'x');
        assert_eq!(p.row_text(0).trim_end(), "日本x");
    }

    /// **A control byte cannot end up in a cell**, which is the crate's
    /// guarantee. It is fed the bytes the hostile corpus uses to trick a
    /// line editor, plus broken UTF-8.
    #[test]
    fn no_control_byte_reaches_a_cell() {
        let mut p = Screen::new(20, 2);
        p.alimentar(b"a\x15b\x01\x7f\x00c\xff\xfe d\x1b[e");
        for row in 0..2 {
            for col in 0..20 {
                let c = p.cell(row, col).expect("cell").c;
                assert!(
                    !c.is_control(),
                    "control byte painted at {row},{col}: {c:?}"
                );
            }
        }
        let text = p.row_text(0);
        assert!(text.contains('a') && text.contains('b') && text.contains('c'));
    }

    /// A whole OSC paints nothing. It is norte's case: the subshell's cwd
    /// marker is an OSC, and a pane that painted it would show the reader
    /// the plumbing on every prompt — the bug #142 already had once.
    #[test]
    fn an_osc_paints_nothing() {
        let mut p = Screen::new(20, 1);
        p.alimentar(b"a\x1b]777;norte-cwd;abc123;/tmp\x07b");
        assert_eq!(p.row_text(0).trim_end(), "ab");
    }

    #[test]
    fn the_cursor_moves_hides_and_the_screen_clears() {
        let mut p = Screen::new(5, 2);
        p.alimentar(b"hola\x1b[?25l\x1b[2J\x1b[H");
        assert!(!p.cursor_visible());
        assert_eq!(p.cursor(), (0, 0));
        assert_eq!(p.row_text(0).trim_end(), "");
        p.alimentar(b"\x1b[?25h\x1b[2;3H");
        assert!(p.cursor_visible());
        assert_eq!(
            p.cursor(),
            (1, 2),
            "`CSI H` counts from one; the grid, from zero"
        );
    }

    #[test]
    fn resizing_preserves_what_fits() {
        let mut p = Screen::new(6, 2);
        p.alimentar(b"abcdef\r\nghi");
        p.resize(3, 2);
        assert_eq!(p.size(), (3, 2));
        assert_eq!(p.row_text(0), "abc");
        assert_eq!(p.row_text(1), "ghi");
        assert_eq!(
            p.cursor(),
            (1, 3),
            "the cursor does not fall outside the new grid"
        );
    }

    /// **A huge cursor movement crashes nothing.**
    ///
    /// `CSI 65535 C` is a parameter `vte` delivers as-is, and adding it to
    /// the column overflowed the `u16`: on a profile with checks on —i.e.
    /// the `dev` one the whole suite runs with and the binary `just link`
    /// leaves behind— that is a panic, and ANY file carrying those eight
    /// bytes triggers it. A `cat` of something downloaded, a commit
    /// message, a filename printed by `find`.
    ///
    /// What gave the bug away was in plain sight: `A` and `D` were
    /// saturating and `B` and `C` were not.
    #[test]
    fn a_huge_movement_does_not_overflow() {
        let mut p = Screen::new(10, 4);
        p.alimentar(b"a\x1b[65535C\x1b[65535B\x1b[65535A\x1b[65535D");
        let (row, col) = p.cursor();
        assert!(
            row < 4 && col < 10,
            "the cursor ended up outside: {row},{col}"
        );
    }

    /// A WIDE character on a ONE-column grid does not fit even by wrapping.
    ///
    /// The minimal case proptest found. Not far-fetched: a one-column grid
    /// comes from narrowing the window, and a wide `ä` comes from any `ls`.
    /// The cursor ended up at column 2 of a grid that only reaches 1, which
    /// is the invariant everything else takes for granted.
    #[test]
    fn a_wide_character_in_a_one_column_grid_does_not_push_the_cursor_out() {
        let mut p = Screen::new(1, 2);
        p.alimentar("世".as_bytes());
        let (row, col) = p.cursor();
        assert!(row < 2, "row {row} outside 2");
        assert!(col <= 1, "column {col} outside 1");
        // And repeating it does not push it out either: the state survives
        // between reads.
        p.alimentar("界".as_bytes());
        let (_, col) = p.cursor();
        assert!(col <= 1, "column {col} outside 1 after the second one");
    }

    /// **No sequence crashes the grid, whoever writes it.**
    ///
    /// It is the test the plan asked for this phase and that a hand-written
    /// byte list does not give: an emulator is fed whatever another program
    /// spits out, so what needs testing is not the fifteen cases we can
    /// think of, but that there is no sixteenth.
    ///
    /// Also split into arbitrary chunks, because that is how it arrives
    /// from a pty and because the state that survives between two reads is
    /// exactly where a parser breaks.
    #[test]
    fn no_sequence_brings_down_the_grid() {
        use proptest::prelude::*;
        proptest!(|(chunks in prop::collection::vec(
            prop::collection::vec(any::<u8>(), 0..64),
            1..8,
        ), width in 1u16..40, alto in 1u16..12)| {
            let mut p = Screen::new(width, alto);
            for t in &chunks {
                p.alimentar(t);
            }
            let (row, col) = p.cursor();
            prop_assert!(row < alto, "row {row} outside {alto}");
            prop_assert!(col <= width, "column {col} outside {width}");
            // And the crate's invariant holds no matter what.
            for f in 0..alto {
                for c in 0..width {
                    let cell = p.cell(f, c).expect("inside the grid");
                    prop_assert!(!cell.c.is_control(), "control at {f},{c}");
                }
            }
        });
    }

    /// The CANONICAL hostile corpus, fed to the grid.
    ///
    /// The corpus's names are what an `ls` or a `find` print inside the
    /// pane, so they are input to this crate just as much as to a listing.
    /// It runs against the corpus and not a local list for the reason
    /// `norte-frontend` states: a local list leaves the bug outside the
    /// place the rest of norte looks for it, and a new fixture would never
    /// reach here.
    #[test]
    fn the_hostile_corpus_does_not_dirty_any_cell() {
        let mut p = Screen::new(20, 4);
        for n in norte_testkit::corpus::hostile_names() {
            p.alimentar(&n.bytes);
            p.alimentar(b"\r\n");
            for f in 0..4 {
                for c in 0..20 {
                    let cell = p.cell(f, c).expect("inside");
                    assert!(
                        !cell.c.is_control(),
                        "\"{}\" ({}) left a control at {f},{c}",
                        n.id,
                        n.why
                    );
                }
            }
        }
    }

    /// Feeding in chunks has to give the SAME result as all at once: an
    /// escape split across two pty reads is normal, not rare.
    #[test]
    fn an_escape_split_across_two_chunks_is_recognized() {
        let mut whole = Screen::new(6, 1);
        whole.alimentar(b"\x1b[31mrojo");
        let mut partido = Screen::new(6, 1);
        partido.alimentar(b"\x1b[3");
        partido.alimentar(b"1mrojo");
        for col in 0..6 {
            assert_eq!(
                whole.cell(0, col).expect("celda"),
                partido.cell(0, col).expect("celda"),
                "columna {col}"
            );
        }
    }
}
