//! A terminal's grid: the bytes a pty writes, parsed into cells, cursor and
//! attributes.
//!
//! It is the PURE half of a terminal pane, and the split is the same one
//! `norte-frontend`/`norte-tui` already have for the subshell (ADR 0084):
//! there is no pty here, no threads, no toolkit, no theme. Bytes go in
//! through [`Pantalla::alimentar`] and a grid comes out that anyone can
//! paint — the terminal with `ratatui`, the window as rows of spans through
//! the bridge.
//!
//! # What this crate does NOT decide
//!
//! **An index's color.** A terminal says "color 1" and what red that means
//! is the THEME's business, not the terminal's: [`ColorTerm::Indexado`]
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
//! use norte_term::Pantalla;
//!
//! let mut p = Pantalla::nueva(10, 2);
//! p.alimentar(b"hola\r\nmundo");
//! assert_eq!(p.fila_texto(0).trim_end(), "hola");
//! assert_eq!(p.fila_texto(1).trim_end(), "mundo");
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
    PorDefecto,
    /// One of the palette's 256. 0..=15 are the "usual" ones.
    Indexado(u8),
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
pub struct Estilo {
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
    pub inverso: bool,
    /// `SGR 9`.
    pub tachado: bool,
}

/// A grid cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Celda {
    /// What is seen. A space if nothing has been written.
    pub c: char,
    /// How it looks.
    pub estilo: Estilo,
    /// The second half of a WIDE character: it is not painted, and it exists
    /// so the columns keep lining up.
    pub estela: bool,
}

impl Default for Celda {
    fn default() -> Self {
        Self {
            c: ' ',
            estilo: Estilo::default(),
            estela: false,
        }
    }
}

/// The grid and the cursor: the state, without the parser.
///
/// It is separate from [`Pantalla`] for a mechanical reason, not a design
/// one: `vte::Parser::advance` borrows the parser AND whoever receives the
/// events, and being the same object that is not possible. With two fields,
/// it is.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rejilla {
    /// Width in columns.
    ancho: u16,
    /// Height in rows.
    alto: u16,
    /// `alto * ancho` cells, row by row.
    celdas: Vec<Celda>,
    /// Cursor row.
    fila: u16,
    /// Cursor column. CAN equal `ancho`: it is the "pending wrap" state a
    /// real terminal has, and without it writing right at the right edge
    /// would wrap a cell too early.
    col: u16,
    /// Hidden by `CSI ?25l`.
    cursor_visible: bool,
    /// The style currently being written with.
    estilo: Estilo,
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
    guardado: Option<(u16, u16, Estilo)>,
    /// The last PRINTED character, for `REP` (`CSI b`).
    ultimo: Option<char>,
    /// Is the line-drawing character set (`ESC ( 0`) active?
    dibujo: bool,
    /// The screen `CSI ?1049h` set aside, if one was set aside.
    ///
    /// It is what keeps a `less` or a `vim` from leaving their last frame
    /// stuck on exit: they enter, paint over a clean screen, and on exit the
    /// previous one is given back EXACTLY AS IT WAS —cells, cursor and
    /// style—. Of the two modes, the modern one (`1049`) also saves the
    /// cursor; the old one (`47`) does not, and that difference is honored.
    alterna: Option<Box<Guardada>>,
}

/// A screen set aside by the alternate screen.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Guardada {
    celdas: Vec<Celda>,
    fila: u16,
    col: u16,
    estilo: Estilo,
    region: (u16, u16),
}

impl Rejilla {
    fn nueva(ancho: u16, alto: u16) -> Self {
        let (ancho, alto) = (ancho.max(1), alto.max(1));
        Self {
            ancho,
            alto,
            celdas: vec![Celda::default(); usize::from(ancho) * usize::from(alto)],
            fila: 0,
            col: 0,
            cursor_visible: true,
            estilo: Estilo::default(),
            region: (0, alto - 1),
            guardado: None,
            ultimo: None,
            dibujo: false,
            alterna: None,
        }
    }

    /// `DECSC`: the cursor's position AND the style it was writing with.
    fn guardar_cursor(&mut self) {
        self.guardado = Some((self.fila, self.col, self.estilo));
    }

    /// `DECRC`. Does nothing with nothing saved, which is what the spec
    /// says: making up the top-left corner would move the cursor of a
    /// program that only wanted to make sure.
    fn restaurar_cursor(&mut self) {
        if let Some((fila, col, estilo)) = self.guardado {
            self.fila = fila.min(self.alto - 1);
            self.col = col.min(self.ancho);
            self.estilo = estilo;
        }
    }

    /// Enters or leaves the alternate screen.
    ///
    /// `con_cursor` distinguishes the modern mode (`1049`, also saves where
    /// the cursor was) from the old one (`47`, screen only). Entering twice
    /// does not stack: the second time does nothing, or the real screen
    /// would be lost.
    fn pantalla_alterna(&mut self, entrar: bool, con_cursor: bool) {
        if entrar {
            if self.alterna.is_some() {
                return;
            }
            self.alterna = Some(Box::new(Guardada {
                celdas: self.celdas.clone(),
                fila: self.fila,
                col: self.col,
                estilo: self.estilo,
                region: self.region,
            }));
            // The alternate one starts CLEAN and without a region: it is a
            // new screen.
            self.celdas = vec![Celda::default(); self.celdas.len()];
            self.region = (0, self.alto - 1);
            if con_cursor {
                self.fila = 0;
                self.col = 0;
            }
            return;
        }
        let Some(g) = self.alterna.take() else {
            return;
        };
        self.celdas = g.celdas;
        self.region = g.region;
        if con_cursor {
            self.fila = g.fila.min(self.alto - 1);
            self.col = g.col.min(self.ancho);
            self.estilo = g.estilo;
        }
    }

    /// `ED`: clears the screen. `0` from here down, `1` from here up, and
    /// anything else, the whole thing.
    fn borrar_pantalla(&mut self, modo: u16) {
        let (fila, col, alto, ancho) = (self.fila, self.col, self.alto, self.ancho);
        match modo {
            0 => {
                self.borrar_fila(fila, col, ancho);
                for f in fila + 1..alto {
                    self.borrar_fila(f, 0, ancho);
                }
            }
            1 => {
                for f in 0..fila {
                    self.borrar_fila(f, 0, ancho);
                }
                self.borrar_fila(fila, 0, col.saturating_add(1));
            }
            _ => {
                for f in 0..alto {
                    self.borrar_fila(f, 0, ancho);
                }
            }
        }
    }

    /// `EL`: the same, within the cursor's row.
    fn borrar_linea(&mut self, modo: u16) {
        let (fila, col, ancho) = (self.fila, self.col, self.ancho);
        match modo {
            0 => self.borrar_fila(fila, col, ancho),
            1 => self.borrar_fila(fila, 0, col.saturating_add(1)),
            _ => self.borrar_fila(fila, 0, ancho),
        }
    }

    /// `ICH`: opens `n` gaps in the cursor's row, pushing to the right.
    fn insertar_celdas(&mut self, n: u16) {
        let (fila, col, ancho) = (self.fila, self.col, self.ancho);
        let n = n.min(ancho.saturating_sub(col));
        for c in (col..ancho).rev() {
            let origen = c.checked_sub(n).filter(|o| *o >= col);
            let celda = origen.and_then(|o| self.indice(fila, o).map(|i| self.celdas[i].clone()));
            if let Some(i) = self.indice(fila, c) {
                self.celdas[i] = celda.unwrap_or_default();
            }
        }
    }

    /// `DCH`: takes `n` cells away from the cursor's row, pulling the rest in.
    fn borrar_celdas(&mut self, n: u16) {
        let (fila, col, ancho) = (self.fila, self.col, self.ancho);
        let n = n.min(ancho.saturating_sub(col));
        for c in col..ancho {
            let origen = c.checked_add(n).filter(|o| *o < ancho);
            let celda = origen.and_then(|o| self.indice(fila, o).map(|i| self.celdas[i].clone()));
            if let Some(i) = self.indice(fila, c) {
                self.celdas[i] = celda.unwrap_or_default();
            }
        }
    }

    fn indice(&self, fila: u16, col: u16) -> Option<usize> {
        (fila < self.alto && col < self.ancho)
            .then(|| usize::from(fila) * usize::from(self.ancho) + usize::from(col))
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
    fn bajar(&mut self) {
        let (arriba, abajo) = self.region;
        if self.fila == abajo {
            self.subir_region(arriba, abajo, 1);
            return;
        }
        if self.fila + 1 < self.alto {
            self.fila += 1;
        }
    }

    /// Scrolls the `[arriba, abajo]` chunk up `n` rows, filling in from the
    /// bottom.
    fn subir_region(&mut self, arriba: u16, abajo: u16, n: u16) {
        if arriba > abajo {
            return;
        }
        let alto_region = abajo - arriba + 1;
        let n = n.min(alto_region);
        for f in arriba..=abajo {
            let origen = f + n;
            for c in 0..self.ancho {
                let celda = if origen <= abajo {
                    self.indice(origen, c).map(|i| self.celdas[i].clone())
                } else {
                    None
                };
                if let Some(i) = self.indice(f, c) {
                    self.celdas[i] = celda.unwrap_or_default();
                }
            }
        }
    }

    /// Scrolls the `[arriba, abajo]` chunk down `n` rows, filling in from
    /// the top. It is the inverse of [`Self::subir_region`], and `IL` uses
    /// it.
    fn bajar_region(&mut self, arriba: u16, abajo: u16, n: u16) {
        if arriba > abajo {
            return;
        }
        let alto_region = abajo - arriba + 1;
        let n = n.min(alto_region);
        for f in (arriba..=abajo).rev() {
            let origen = f.checked_sub(n).filter(|o| *o >= arriba);
            for c in 0..self.ancho {
                let celda = origen.and_then(|o| self.indice(o, c).map(|i| self.celdas[i].clone()));
                if let Some(i) = self.indice(f, c) {
                    self.celdas[i] = celda.unwrap_or_default();
                }
            }
        }
    }

    /// The character `ESC ( 0` paints instead of `c` (DEC Special Graphics).
    ///
    /// Only the `0x60..=0x7e` range, which is what the set redefines; the
    /// rest is left as is. Without this, a program drawing a box would
    /// print `lqqqk` where it wanted a corner and three lines.
    fn dibujar(c: char) -> char {
        const TABLA: &str = "◆▒␉␌␍␊°±␤␋┘┐┌└┼⎺⎻─⎼⎽├┤┴┬│≤≥π≠£·";
        let i = (c as u32)
            .checked_sub(0x60)
            .and_then(|i| usize::try_from(i).ok());
        i.and_then(|i| TABLA.chars().nth(i)).unwrap_or(c)
    }

    /// Writes a character wherever the cursor is and advances it.
    fn poner(&mut self, c: char) {
        let c = if self.dibujo { Self::dibujar(c) } else { c };
        // For `REP`, which repeats the last PRINTED one — the translated
        // one included: what gets repeated is what shows.
        self.ultimo = Some(c);
        // A zero-width character —a combining one— has no cell of its own.
        // It is DROPPED, and that is a known limitation: the correct thing
        // is to attach it to the previous character, and that requires a
        // cell to store a cluster and not a `char`. Until then, dropping is
        // what does not throw the columns off.
        let ancho_c = u16::try_from(c.width().unwrap_or(0)).unwrap_or(0);
        if ancho_c == 0 {
            return;
        }
        // Saturating like the cursor movements, and for the same reason:
        // the column comes from a foreign parameter. It would take a
        // 65528-column grid to overflow here —i.e. never—, but closing the
        // whole class at once beats leaving four spots that have to be
        // re-reasoned about every time someone reads them.
        if self.col.saturating_add(ancho_c) > self.ancho {
            self.col = 0;
            self.bajar();
        }
        let (fila, col, estilo) = (self.fila, self.col, self.estilo);
        if let Some(i) = self.indice(fila, col) {
            self.celdas[i] = Celda {
                c,
                estilo,
                estela: false,
            };
        }
        for d in 1..ancho_c {
            if let Some(i) = self.indice(fila, col + d) {
                self.celdas[i] = Celda {
                    c: ' ',
                    estilo,
                    estela: true,
                };
            }
        }
        // Clamped to `ancho`, not a loose `+=`: a WIDE character on a
        // ONE-column grid does not fit even after wrapping —the jump above
        // leaves `col` at 0 and it still does not fit— so adding 2 left the
        // cursor at column 2 of a grid that only reaches 1. proptest found
        // it, which is what it is for: a one-column grid occurs to nobody
        // and a pty produces one as soon as the window narrows.
        //
        // `ancho` and not `ancho - 1` because that is the pending-wrap
        // position, which is a legitimate cursor state here.
        self.col = self.col.saturating_add(ancho_c).min(self.ancho);
    }

    /// Leaves `rango` of row `fila`'s cells as freshly placed.
    fn borrar_fila(&mut self, fila: u16, desde: u16, hasta: u16) {
        for col in desde..hasta.min(self.ancho) {
            if let Some(i) = self.indice(fila, col) {
                self.celdas[i] = Celda::default();
            }
        }
    }

    /// `SGR`: the attributes, with the two that carry arguments inside.
    fn sgr(&mut self, codigos: &[u16]) {
        // A bare `CSI m` is a `CSI 0 m`: reset.
        if codigos.is_empty() {
            self.estilo = Estilo::default();
            return;
        }
        let mut i = 0;
        while i < codigos.len() {
            let n = codigos[i];
            match n {
                0 => self.estilo = Estilo::default(),
                1 => self.estilo.negrita = true,
                2 => self.estilo.tenue = true,
                3 => self.estilo.cursiva = true,
                4 => self.estilo.subrayado = true,
                7 => self.estilo.inverso = true,
                9 => self.estilo.tachado = true,
                // 21 is "double underline" on some terminals and "remove
                // bold" on others; it is treated like 22, which is what the
                // ones that matter do.
                21 | 22 => {
                    self.estilo.negrita = false;
                    self.estilo.tenue = false;
                }
                23 => self.estilo.cursiva = false,
                24 => self.estilo.subrayado = false,
                27 => self.estilo.inverso = false,
                29 => self.estilo.tachado = false,
                30..=37 => self.estilo.fg = ColorTerm::Indexado(u8_de(n - 30)),
                39 => self.estilo.fg = ColorTerm::PorDefecto,
                40..=47 => self.estilo.bg = ColorTerm::Indexado(u8_de(n - 40)),
                49 => self.estilo.bg = ColorTerm::PorDefecto,
                // The "bright" ones are indices 8..=15 of the SAME palette,
                // nothing else: resolving them here to an RGB would take
                // away the theme's ability to say what they look like.
                90..=97 => self.estilo.fg = ColorTerm::Indexado(u8_de(n - 90 + 8)),
                100..=107 => self.estilo.bg = ColorTerm::Indexado(u8_de(n - 100 + 8)),
                38 | 48 => {
                    let (color, leidos) = color_extendido(&codigos[i + 1..]);
                    if let Some(color) = color {
                        if n == 38 {
                            self.estilo.fg = color;
                        } else {
                            self.estilo.bg = color;
                        }
                    }
                    i += leidos;
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
fn color_extendido(resto: &[u16]) -> (Option<ColorTerm>, usize) {
    match resto.first() {
        Some(5) => (
            resto.get(1).map(|n| ColorTerm::Indexado(u8_de(*n))),
            resto.len().min(2),
        ),
        Some(2) => (
            match (resto.get(1), resto.get(2), resto.get(3)) {
                (Some(r), Some(g), Some(b)) => {
                    Some(ColorTerm::Rgb(u8_de(*r), u8_de(*g), u8_de(*b)))
                }
                _ => None,
            },
            resto.len().min(4),
        ),
        _ => (None, 0),
    }
}

impl vte::Perform for Rejilla {
    fn print(&mut self, c: char) {
        self.poner(c);
    }

    /// The C0s that move the cursor. **Everything else is dropped**, and
    /// that is the crate's guarantee: a control byte cannot end up in a
    /// cell.
    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' | 0x0b | 0x0c => self.bajar(),
            b'\r' => self.col = 0,
            0x08 => self.col = self.col.saturating_sub(1),
            b'\t' => self.col = (self.col / 8).saturating_add(1).saturating_mul(8),
            _ => return,
        }
        self.col = self.col.min(self.ancho);
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermedios: &[u8],
        _ignora: bool,
        accion: char,
    ) {
        // Sub-parameters (`38:2:…`) are flattened together with parameters
        // (`38;2;…`): they are two ways of writing the same thing and no
        // program should notice which one was used.
        let codigos: Vec<u16> = params.iter().flat_map(|p| p.iter().copied()).collect();
        // A missing parameter is worth 1 for movements and 0 for clears,
        // which is what ECMA-48 says and what a `tput` expects.
        let uno = |n: usize| codigos.get(n).copied().filter(|v| *v != 0).unwrap_or(1);
        let cero = |n: usize| codigos.get(n).copied().unwrap_or(0);
        // `?` arrives as a private marker, and without checking it a plain
        // `CSI 25 l` would hide the cursor without anyone asking for it.
        let privado = intermedios.first() == Some(&b'?');
        match accion {
            'm' if !privado => self.sgr(&codigos),
            // `CSI H` counts from ONE; the grid, from zero.
            'H' | 'f' if !privado => {
                self.fila = (uno(0) - 1).min(self.alto - 1);
                self.col = (uno(1) - 1).min(self.ancho - 1);
            }
            // All four SATURATE, and all four have to: `uno` comes from a
            // parameter another program wrote and it can be 65535. Adding
            // it overflowed the `u16` and, with checks on —the profile the
            // suite and the dev binary run with—, that is a panic any file
            // with `ESC [ 6 5 5 3 5 C` inside triggers. That `A` and `D`
            // were saturating and `B` and `C` were not was not the visible
            // symptom.
            'A' => self.fila = self.fila.saturating_sub(uno(0)),
            'B' => self.fila = self.fila.saturating_add(uno(0)).min(self.alto - 1),
            'C' => self.col = self.col.saturating_add(uno(0)).min(self.ancho - 1),
            'D' => self.col = self.col.saturating_sub(uno(0)),
            'J' if !privado => self.borrar_pantalla(cero(0)),
            'K' if !privado => self.borrar_linea(cero(0)),
            // Absolute position on ONE axis: `CHA` the column, `VPA` the
            // row. Cheap and constant — a prompt that repaints itself uses
            // them on every keystroke—, and without them it kept writing
            // wherever it was.
            'G' | '`' if !privado => self.col = (uno(0) - 1).min(self.ancho - 1),
            'd' if !privado => self.fila = (uno(0) - 1).min(self.alto - 1),
            // Insert and delete LINES, within the region and from the
            // cursor: what an editor uses to open a gap without repainting
            // the rest.
            'L' if !privado => {
                let (arriba, abajo) = self.region;
                if self.fila >= arriba && self.fila <= abajo {
                    self.bajar_region(self.fila, abajo, uno(0));
                }
            }
            'M' if !privado => {
                let (arriba, abajo) = self.region;
                if self.fila >= arriba && self.fila <= abajo {
                    self.subir_region(self.fila, abajo, uno(0));
                }
            }
            // Insert and delete CHARACTERS on the cursor's row, and clear
            // without moving: what a line editor uses so it does not
            // repaint the whole line on every key.
            '@' if !privado => self.insertar_celdas(uno(0)),
            'P' if !privado => self.borrar_celdas(uno(0)),
            'X' if !privado => {
                let (fila, col) = (self.fila, self.col);
                self.borrar_fila(fila, col, col.saturating_add(uno(0)));
            }
            // Scrolls the region up and down without moving the cursor
            // (`SU`/`SD`).
            'S' if !privado => {
                let (arriba, abajo) = self.region;
                self.subir_region(arriba, abajo, uno(0));
            }
            'T' if !privado => {
                let (arriba, abajo) = self.region;
                self.bajar_region(arriba, abajo, uno(0));
            }
            // `REP`: repeat the last printed character. A `tput rep` uses it
            // to paint a line of dashes with four bytes.
            'b' if !privado => {
                if let Some(c) = self.ultimo {
                    for _ in 0..uno(0) {
                        self.poner(c);
                    }
                }
            }
            // `DECSTBM`: the scroll region. Without parameters it goes back
            // to being the whole screen, and the cursor goes to its corner —
            // the spec says so and the programs that set it take it for
            // granted.
            'r' if !privado => {
                let arriba = codigos.first().copied().filter(|v| *v != 0).unwrap_or(1) - 1;
                let abajo = codigos
                    .get(1)
                    .copied()
                    .filter(|v| *v != 0)
                    .unwrap_or(self.alto)
                    - 1;
                let (arriba, abajo) = (arriba.min(self.alto - 1), abajo.min(self.alto - 1));
                // A backwards or single-row region is not accepted: there is
                // nothing to scroll and leaving it set breaks normal scrolling.
                if arriba < abajo {
                    self.region = (arriba, abajo);
                    self.fila = arriba;
                    self.col = 0;
                }
            }
            // `SCP`/`RCP`, the twins of `ESC 7`/`ESC 8` in CSI form.
            's' if !privado => self.guardar_cursor(),
            'u' if !privado => self.restaurar_cursor(),
            'h' | 'l' if privado => {
                let encender = accion == 'h';
                match codigos.first() {
                    Some(&25) => self.cursor_visible = encender,
                    // The ALTERNATE screen. `1049` is the modern one —sets
                    // aside the screen AND the cursor— and `47`/`1047` the
                    // old one, which only sets aside the screen. It is what
                    // keeps a `less` from leaving its last frame stuck on
                    // exit.
                    Some(&1049) => self.pantalla_alterna(encender, true),
                    Some(&47 | &1047) => self.pantalla_alterna(encender, false),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermedios: &[u8], _ignora: bool, byte: u8) {
        match (intermedios.first(), byte) {
            // `DECSC`/`DECRC`: save and restore the cursor.
            (None, b'7') => self.guardar_cursor(),
            (None, b'8') => self.restaurar_cursor(),
            // `RIS`: full reset. Sent by a `reset`, and also by a program
            // that found the screen in a state it does not understand —
            // which is exactly when it must be obeyed.
            (None, b'c') => {
                let (ancho, alto) = (self.ancho, self.alto);
                *self = Self::nueva(ancho, alto);
            }
            // `ESC ( 0` and `ESC ( B`: the line-drawing character set and
            // the return to ASCII. Ignoring it made a program that draws
            // boxes print `lqqqk` where it wanted corners.
            (Some(b'('), b'0') => self.dibujo = true,
            (Some(b'('), b'B') => self.dibujo = false,
            // `IND` and `NEL` move down one line (with region scroll); `RI`
            // moves up.
            (None, b'D') => self.bajar(),
            (None, b'E') => {
                self.bajar();
                self.col = 0;
            }
            (None, b'M') => {
                let (arriba, abajo) = self.region;
                if self.fila == arriba {
                    self.bajar_region(arriba, abajo, 1);
                } else {
                    self.fila = self.fila.saturating_sub(1);
                }
            }
            _ => {}
        }
    }
}

/// A terminal's screen: the grid and the escape parser.
pub struct Pantalla {
    /// The state.
    rejilla: Rejilla,
    /// The escape state machine.
    parser: vte::Parser,
}

impl Pantalla {
    /// An empty `ancho` by `alto` screen.
    ///
    /// A size of zero on either axis is bumped to one: a grid with no cells
    /// has nowhere to put the cursor, and whoever paints a zero-column pane
    /// does not want a `None` in every cell, they want it to not crash.
    #[must_use]
    pub fn nueva(ancho: u16, alto: u16) -> Self {
        Self {
            rejilla: Rejilla::nueva(ancho, alto),
            parser: vte::Parser::new(),
        }
    }

    /// The size, in columns and rows.
    #[must_use]
    pub fn tamano(&self) -> (u16, u16) {
        (self.rejilla.ancho, self.rejilla.alto)
    }

    /// Where the cursor is: row and column.
    ///
    /// The column can be as large as the width, and that is not a bug: it
    /// is the terminal waiting to see whether the next thing to arrive
    /// needs to wrap.
    #[must_use]
    pub fn cursor(&self) -> (u16, u16) {
        (self.rejilla.fila, self.rejilla.col)
    }

    /// Is the cursor painted? (`CSI ?25l` / `CSI ?25h`).
    #[must_use]
    pub fn cursor_visible(&self) -> bool {
        self.rejilla.cursor_visible
    }

    /// The cell at `fila`, `col`, or `None` if it falls outside.
    #[must_use]
    pub fn celda(&self, fila: u16, col: u16) -> Option<&Celda> {
        self.rejilla
            .indice(fila, col)
            .and_then(|i| self.rejilla.celdas.get(i))
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
    /// use norte_term::Pantalla;
    ///
    /// let mut p = Pantalla::nueva(8, 1);
    /// p.alimentar(b"ab\x1b[31mcd");
    /// let tramos = p.fila_tramos(0);
    /// // Three: the normal one, the red one, and the trailing padding —which
    /// // is NOT red, and folding it into the previous span would paint the
    /// // rest of the line red.
    /// assert_eq!(tramos.len(), 3);
    /// assert_eq!(tramos[0].0, "ab");
    /// assert_eq!(tramos[1].0, "cd");
    /// assert_eq!(tramos[2].0, "    ");
    /// ```
    #[must_use]
    pub fn fila_tramos(&self, fila: u16) -> Vec<(String, Estilo)> {
        let mut tramos: Vec<(String, Estilo)> = Vec::new();
        for c in (0..self.rejilla.ancho).filter_map(|c| self.celda(fila, c)) {
            if c.estela {
                continue;
            }
            match tramos.last_mut() {
                Some((texto, estilo)) if *estilo == c.estilo => texto.push(c.c),
                _ => tramos.push((c.c.to_string(), c.estilo)),
            }
        }
        tramos
    }

    /// A row's text, with the trailing cells removed.
    ///
    /// Exists for tests and for whoever wants a line in one go; whoever
    /// paints with styles iterates [`Self::celda`] or [`Self::fila_tramos`].
    #[must_use]
    pub fn fila_texto(&self, fila: u16) -> String {
        (0..self.rejilla.ancho)
            .filter_map(|c| self.celda(fila, c))
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
        self.parser.advance(&mut self.rejilla, bytes);
    }

    /// Changes the size, keeping what fits.
    ///
    /// Text is not re-wrapped to the width: what overflows to the right is
    /// lost, and so is what overflows at the bottom. Re-wrapping requires
    /// knowing which lines used to be a single one split into two, and this
    /// grid does not store that — neither do half the terminals people use.
    pub fn redimensionar(&mut self, ancho: u16, alto: u16) {
        let (ancho, alto) = (ancho.max(1), alto.max(1));
        let vieja = std::mem::replace(&mut self.rejilla, Rejilla::nueva(ancho, alto));
        for fila in 0..alto.min(vieja.alto) {
            for col in 0..ancho.min(vieja.ancho) {
                if let (Some(destino), Some(origen)) =
                    (self.rejilla.indice(fila, col), vieja.indice(fila, col))
                {
                    self.rejilla.celdas[destino] = vieja.celdas[origen].clone();
                }
            }
        }
        self.rejilla.estilo = vieja.estilo;
        self.rejilla.cursor_visible = vieja.cursor_visible;
        self.rejilla.fila = vieja.fila.min(alto - 1);
        self.rejilla.col = vieja.col.min(ancho);
        self.rejilla.dibujo = vieja.dibujo;
        self.rejilla.ultimo = vieja.ultimo;
        // The saved cursor and the region are CLAMPED to the new size
        // instead of dropped: a `vim` that resizes while its region is set
        // does not send it again, and losing it would undo its status line.
        // A region that no longer fits goes back to being the whole screen,
        // which is what a real terminal does.
        self.rejilla.guardado = vieja
            .guardado
            .map(|(f, c, e)| (f.min(alto - 1), c.min(ancho), e));
        let (arriba, abajo) = vieja.region;
        self.rejilla.region = if arriba < abajo.min(alto - 1) {
            (arriba.min(alto - 1), abajo.min(alto - 1))
        } else {
            (0, alto - 1)
        };
        // And the set-aside screen is kept CROPPED: losing it would leave a
        // `less` with nothing to return on exit, which is the bug the
        // alternate screen exists to not have.
        self.rejilla.alterna = vieja.alterna.map(|g| {
            let mut celdas = vec![Celda::default(); usize::from(ancho) * usize::from(alto)];
            for fila in 0..alto.min(vieja.alto) {
                for col in 0..ancho.min(vieja.ancho) {
                    let destino = usize::from(fila) * usize::from(ancho) + usize::from(col);
                    let origen = usize::from(fila) * usize::from(vieja.ancho) + usize::from(col);
                    if let Some(c) = g.celdas.get(origen) {
                        celdas[destino] = c.clone();
                    }
                }
            }
            Box::new(Guardada {
                celdas,
                fila: g.fila.min(alto - 1),
                col: g.col.min(ancho),
                estilo: g.estilo,
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
    fn pantalla(p: &Pantalla, alto: u16) -> Vec<String> {
        (0..alto)
            .map(|f| p.fila_texto(f).trim_end().to_owned())
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
    fn la_pantalla_alterna_devuelve_lo_que_habia() {
        let mut p = Pantalla::nueva(10, 3);
        p.alimentar(b"uno\r\ndos\r\ntres");
        let antes = pantalla(&p, 3);
        let cursor_antes = p.cursor();

        // A full-screen program enters and paints its own thing.
        p.alimentar(b"\x1b[?1049h");
        assert_eq!(
            pantalla(&p, 3),
            vec![String::new(), String::new(), String::new()],
            "the alternate one starts clean"
        );
        assert_eq!(p.cursor(), (0, 0), "and with the cursor in the corner");
        p.alimentar(b"visor");
        assert_eq!(p.fila_texto(0).trim_end(), "visor");

        // And it exits.
        p.alimentar(b"\x1b[?1049l");
        assert_eq!(pantalla(&p, 3), antes, "what was there did not come back");
        assert_eq!(p.cursor(), cursor_antes, "nor the cursor");
    }

    /// **Resizing with the alternate screen set does not lose the screen
    /// underneath.**
    ///
    /// Easy to forget because resizing builds a new grid: if what was set
    /// aside does not travel with it, a `less` whose window the reader
    /// resizes ends up with nothing to return on exit — exactly the bug the
    /// alternate screen exists to not have.
    #[test]
    fn redimensionar_con_la_alterna_puesta_no_pierde_lo_de_abajo() {
        let mut p = Pantalla::nueva(10, 3);
        p.alimentar(b"real");
        p.alimentar(b"\x1b[?1049h");
        p.alimentar(b"visor");
        p.redimensionar(12, 4);
        p.alimentar(b"\x1b[?1049l");
        assert_eq!(p.fila_texto(0).trim_end(), "real");
    }

    /// Entering the alternate screen twice does not stack: the second time
    /// does nothing. If it stacked, the REAL screen would be lost by
    /// exiting just once.
    #[test]
    fn entrar_dos_veces_en_la_alterna_no_pierde_la_de_verdad() {
        let mut p = Pantalla::nueva(10, 2);
        p.alimentar(b"real");
        p.alimentar(b"\x1b[?1049h");
        p.alimentar(b"primera");
        p.alimentar(b"\x1b[?1049h");
        p.alimentar(b"\x1b[?1049l");
        assert_eq!(p.fila_texto(0).trim_end(), "real");
    }

    /// **The scroll region leaves what is outside it untouched** (#366).
    ///
    /// Anything that pins a status line uses it. Without it, that line
    /// would drift upward on every scroll until it fell off the screen.
    #[test]
    fn la_region_de_scroll_no_mueve_lo_de_fuera() {
        let mut p = Pantalla::nueva(10, 4);
        // A header pinned at the top and the region in the bottom three.
        p.alimentar(b"cabecera\x1b[2;4r");
        // `DECSTBM` leaves the cursor in the region's corner.
        assert_eq!(p.cursor(), (1, 0));
        p.alimentar(b"a\r\nb\r\nc\r\nd");
        assert_eq!(
            pantalla(&p, 4),
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
    fn la_posicion_absoluta_de_un_solo_eje() {
        let mut p = Pantalla::nueva(10, 3);
        p.alimentar(b"abcdef\x1b[3Gx");
        assert_eq!(p.fila_texto(0).trim_end(), "abxdef");
        // `VPA` moves the ROW and leaves the column where it was: that is
        // the half that sets it apart from a `CUP`, and the one a prompt
        // takes advantage of.
        assert_eq!(p.cursor(), (0, 3));
        p.alimentar(b"\x1b[3dy");
        assert_eq!(p.cursor().0, 2, "VPA did not go to row 3");
        assert_eq!(p.celda(2, 3).map(|c| c.c), Some('y'));
    }

    /// Inserting and deleting characters on the row: what a line editor
    /// uses so it does not repaint the whole line on every key.
    #[test]
    fn insertar_y_borrar_caracteres_en_la_fila() {
        let mut p = Pantalla::nueva(10, 1);
        p.alimentar(b"abcdef\x1b[1G\x1b[2@");
        assert_eq!(p.fila_texto(0).trim_end(), "  abcdef");
        p.alimentar(b"\x1b[1G\x1b[3P");
        assert_eq!(p.fila_texto(0).trim_end(), "bcdef");
        // `ECH` clears WITHOUT moving or pulling in the rest.
        p.alimentar(b"\x1b[1G\x1b[2X");
        assert_eq!(p.fila_texto(0).trim_end(), "  def");
    }

    /// Inserting and deleting LINES, within the region.
    #[test]
    fn insertar_y_borrar_lineas() {
        let mut p = Pantalla::nueva(10, 4);
        p.alimentar(b"a\r\nb\r\nc\r\nd\x1b[2;1H\x1b[L");
        assert_eq!(
            pantalla(&p, 4),
            vec![
                "a".to_owned(),
                String::new(),
                "b".to_owned(),
                "c".to_owned()
            ],
        );
        p.alimentar(b"\x1b[2;1H\x1b[M");
        assert_eq!(
            pantalla(&p, 4),
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
    fn guardar_y_restaurar_el_cursor_lleva_el_estilo() {
        let mut p = Pantalla::nueva(10, 2);
        p.alimentar(b"\x1b[31m\x1b[1;3H\x1b7");
        p.alimentar(b"\x1b[0m\x1b[2;1Hxx\x1b8y");
        assert_eq!(
            p.cursor(),
            (0, 3),
            "the cursor did not go back to where it was saved"
        );
        let c = p.celda(0, 2).expect("inside");
        assert_eq!(c.c, 'y');
        assert_eq!(
            c.estilo.fg,
            ColorTerm::Indexado(1),
            "restored the position and not the style"
        );
    }

    /// `RIS` leaves the screen as freshly opened: it is what a `reset`
    /// sends, and also whoever found the screen in a state they do not
    /// understand.
    #[test]
    fn ris_reinicia_del_todo() {
        let mut p = Pantalla::nueva(10, 3);
        p.alimentar(b"\x1b[31mhola\x1b[2;3r\x1b[?25l");
        p.alimentar(b"\x1bc");
        assert_eq!(
            pantalla(&p, 3),
            vec![String::new(), String::new(), String::new()]
        );
        assert_eq!(p.cursor(), (0, 0));
        assert!(p.cursor_visible(), "the cursor stayed hidden");
    }

    /// `REP` repeats the last printed character: a `tput rep` paints a line
    /// of dashes with four bytes instead of eighty.
    #[test]
    fn rep_repite_el_ultimo() {
        let mut p = Pantalla::nueva(10, 1);
        p.alimentar(b"-\x1b[4b");
        assert_eq!(p.fila_texto(0).trim_end(), "-----");
    }

    /// The DRAWING set: without it, a program drawing boxes printed
    /// `lqqqk` where it wanted a corner and three lines.
    #[test]
    fn el_juego_de_dibujo_pinta_lineas_y_no_letras() {
        let mut p = Pantalla::nueva(10, 1);
        p.alimentar(b"\x1b(0lqqk\x1b(Bx");
        assert_eq!(p.fila_texto(0).trim_end(), "┌──┐x");
    }

    #[test]
    fn el_texto_llena_la_fila_y_el_salto_baja() {
        let mut p = Pantalla::nueva(8, 3);
        p.alimentar(b"uno\r\ndos");
        assert_eq!(p.fila_texto(0).trim_end(), "uno");
        assert_eq!(p.fila_texto(1).trim_end(), "dos");
        assert_eq!(p.cursor(), (1, 3));
    }

    /// The right edge does NOT wrap too early: the last column can be
    /// written, and the wrap happens with the next character. Without the
    /// "pending" state, an eight-letter word in eight columns left the last
    /// one on the row below.
    #[test]
    fn el_borde_derecho_salta_despues_y_no_antes() {
        let mut p = Pantalla::nueva(4, 2);
        p.alimentar(b"abcd");
        assert_eq!(p.fila_texto(0), "abcd");
        assert_eq!(p.cursor(), (0, 4), "pending wrap, not wrapped");
        p.alimentar(b"e");
        assert_eq!(p.fila_texto(1).trim_end(), "e");
        assert_eq!(p.cursor(), (1, 1));
    }

    /// On reaching the bottom, the screen SCROLLS UP: without this the last
    /// thing a shell writes is what does not show.
    #[test]
    fn el_fondo_desplaza_hacia_arriba() {
        let mut p = Pantalla::nueva(4, 2);
        p.alimentar(b"a\r\nb\r\nc");
        assert_eq!(p.fila_texto(0).trim_end(), "b");
        assert_eq!(p.fila_texto(1).trim_end(), "c");
        assert_eq!(p.cursor(), (1, 1));
    }

    #[test]
    fn los_atributos_se_ponen_y_se_quitan() {
        let mut p = Pantalla::nueva(4, 1);
        p.alimentar(b"\x1b[1;31ma\x1b[0mb");
        let a = p.celda(0, 0).expect("celda");
        assert!(a.estilo.negrita);
        assert_eq!(a.estilo.fg, ColorTerm::Indexado(1));
        let b = p.celda(0, 1).expect("celda");
        assert_eq!(b.estilo, Estilo::default(), "`SGR 0` limpia todo");
    }

    /// The two SGRs a program uses to ask for an exact color, and the high
    /// index that is NOT the same thing (`38;5;n` is the palette, `38;2;…`
    /// is RGB).
    #[test]
    fn el_color_exacto_y_el_indice_alto_no_se_confunden() {
        let mut p = Pantalla::nueva(4, 1);
        p.alimentar(b"\x1b[38;2;10;20;30ma\x1b[38;5;200mb\x1b[48;5;7mc");
        assert_eq!(
            p.celda(0, 0).expect("celda").estilo.fg,
            ColorTerm::Rgb(10, 20, 30)
        );
        assert_eq!(
            p.celda(0, 1).expect("celda").estilo.fg,
            ColorTerm::Indexado(200)
        );
        assert_eq!(
            p.celda(0, 2).expect("celda").estilo.bg,
            ColorTerm::Indexado(7)
        );
    }

    /// A wide character takes up TWO cells, and the second is a trail: if
    /// not, the rest of the line comes out shifted one column per CJK char.
    #[test]
    fn un_caracter_ancho_ocupa_dos_celdas() {
        let mut p = Pantalla::nueva(6, 1);
        p.alimentar("日本x".as_bytes());
        assert_eq!(p.celda(0, 0).expect("celda").c, '日');
        assert!(p.celda(0, 1).expect("celda").estela);
        assert_eq!(p.celda(0, 2).expect("celda").c, '本');
        assert!(p.celda(0, 3).expect("celda").estela);
        assert_eq!(p.celda(0, 4).expect("celda").c, 'x');
        assert_eq!(p.fila_texto(0).trim_end(), "日本x");
    }

    /// **A control byte cannot end up in a cell**, which is the crate's
    /// guarantee. It is fed the bytes the hostile corpus uses to trick a
    /// line editor, plus broken UTF-8.
    #[test]
    fn ningun_byte_de_control_llega_a_una_celda() {
        let mut p = Pantalla::nueva(20, 2);
        p.alimentar(b"a\x15b\x01\x7f\x00c\xff\xfe d\x1b[e");
        for fila in 0..2 {
            for col in 0..20 {
                let c = p.celda(fila, col).expect("cell").c;
                assert!(
                    !c.is_control(),
                    "control byte painted at {fila},{col}: {c:?}"
                );
            }
        }
        let texto = p.fila_texto(0);
        assert!(texto.contains('a') && texto.contains('b') && texto.contains('c'));
    }

    /// A whole OSC paints nothing. It is norte's case: the subshell's cwd
    /// marker is an OSC, and a pane that painted it would show the reader
    /// the plumbing on every prompt — the bug #142 already had once.
    #[test]
    fn un_osc_no_pinta_nada() {
        let mut p = Pantalla::nueva(20, 1);
        p.alimentar(b"a\x1b]777;norte-cwd;abc123;/tmp\x07b");
        assert_eq!(p.fila_texto(0).trim_end(), "ab");
    }

    #[test]
    fn el_cursor_se_mueve_se_esconde_y_la_pantalla_se_borra() {
        let mut p = Pantalla::nueva(5, 2);
        p.alimentar(b"hola\x1b[?25l\x1b[2J\x1b[H");
        assert!(!p.cursor_visible());
        assert_eq!(p.cursor(), (0, 0));
        assert_eq!(p.fila_texto(0).trim_end(), "");
        p.alimentar(b"\x1b[?25h\x1b[2;3H");
        assert!(p.cursor_visible());
        assert_eq!(
            p.cursor(),
            (1, 2),
            "`CSI H` counts from one; the grid, from zero"
        );
    }

    #[test]
    fn redimensionar_conserva_lo_que_cabe() {
        let mut p = Pantalla::nueva(6, 2);
        p.alimentar(b"abcdef\r\nghi");
        p.redimensionar(3, 2);
        assert_eq!(p.tamano(), (3, 2));
        assert_eq!(p.fila_texto(0), "abc");
        assert_eq!(p.fila_texto(1), "ghi");
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
    fn un_movimiento_enorme_no_desborda() {
        let mut p = Pantalla::nueva(10, 4);
        p.alimentar(b"a\x1b[65535C\x1b[65535B\x1b[65535A\x1b[65535D");
        let (fila, col) = p.cursor();
        assert!(
            fila < 4 && col < 10,
            "the cursor ended up outside: {fila},{col}"
        );
    }

    /// A WIDE character on a ONE-column grid does not fit even by wrapping.
    ///
    /// The minimal case proptest found. Not far-fetched: a one-column grid
    /// comes from narrowing the window, and a wide `ä` comes from any `ls`.
    /// The cursor ended up at column 2 of a grid that only reaches 1, which
    /// is the invariant everything else takes for granted.
    #[test]
    fn un_caracter_ancho_en_una_rejilla_de_una_columna_no_saca_el_cursor() {
        let mut p = Pantalla::nueva(1, 2);
        p.alimentar("世".as_bytes());
        let (fila, col) = p.cursor();
        assert!(fila < 2, "row {fila} outside 2");
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
    fn ninguna_secuencia_tumba_la_rejilla() {
        use proptest::prelude::*;
        proptest!(|(trozos in prop::collection::vec(
            prop::collection::vec(any::<u8>(), 0..64),
            1..8,
        ), ancho in 1u16..40, alto in 1u16..12)| {
            let mut p = Pantalla::nueva(ancho, alto);
            for t in &trozos {
                p.alimentar(t);
            }
            let (fila, col) = p.cursor();
            prop_assert!(fila < alto, "row {fila} outside {alto}");
            prop_assert!(col <= ancho, "column {col} outside {ancho}");
            // And the crate's invariant holds no matter what.
            for f in 0..alto {
                for c in 0..ancho {
                    let celda = p.celda(f, c).expect("inside the grid");
                    prop_assert!(!celda.c.is_control(), "control at {f},{c}");
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
    fn el_corpus_hostil_no_ensucia_ninguna_celda() {
        let mut p = Pantalla::nueva(20, 4);
        for n in norte_testkit::corpus::hostile_names() {
            p.alimentar(&n.bytes);
            p.alimentar(b"\r\n");
            for f in 0..4 {
                for c in 0..20 {
                    let celda = p.celda(f, c).expect("inside");
                    assert!(
                        !celda.c.is_control(),
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
    fn un_escape_partido_entre_dos_trozos_se_reconoce() {
        let mut entero = Pantalla::nueva(6, 1);
        entero.alimentar(b"\x1b[31mrojo");
        let mut partido = Pantalla::nueva(6, 1);
        partido.alimentar(b"\x1b[3");
        partido.alimentar(b"1mrojo");
        for col in 0..6 {
            assert_eq!(
                entero.celda(0, col).expect("celda"),
                partido.celda(0, col).expect("celda"),
                "columna {col}"
            );
        }
    }
}
