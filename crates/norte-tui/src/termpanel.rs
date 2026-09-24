//! The terminal pane (#362): a shell INSIDE a layout slot.
//!
//! This is what Krusader has and norte did not. What already existed is
//! different, and in one way better: `app.toggle-panels` hands the ENTIRE
//! terminal over to a live subshell (ADR 0084), and `app.terminal` launches a
//! separate shell. What was missing was seeing it AT THE SAME TIME as the
//! listings.
//!
//! # The split
//!
//! The emulation — bytes into a grid of cells — and the pty live in
//! `norte-term`: the first always, the second behind its feature. What stays
//! here is only the PAINTING with `ratatui`, the one thing it does not share
//! with the window. The day the window paints its own pane it will use the
//! same shell and the same grid, so the two frontends show the same thing by
//! construction and not because someone compared two emulators.
//!
//! # Who owns the keyboard
//!
//! This is the only pane that consumes BYTES and not catalogue commands, so
//! while it holds the keys it also keeps the chords that would otherwise
//! belong to norte. The way out is ONE loose chord — the same
//! `layout.terminal` that opened it — and [`crate::keys`] recognizes it
//! before forwarding anything. If the preset bound it to a sequence,
//! `Effective::lone_chord` returns `None` and the pane does NOT take the
//! keys: a pane you can only look at is better than one you cannot leave.
//!
//! # What this pane does NOT do yet
//!
//! It does not install the prompt hook, so the listing does not follow the
//! shell nor the shell the listing: that is what #142's subshell is for,
//! which does install it. A pane that typed `cd` into the reader's shell has
//! #363's problems — and would have them somewhere the reader watches it
//! happen — so that waits until that hole is closed.

use norte_term::{ColorTerm, Estilo, Pantalla};

/// The pane's shell, with its grid. It is `norte-term`'s.
pub use norte_term::pty::Shell as TermPanel;

/// The kind's id, which is also its command's suffix.
pub const KIND: &str = "terminal";

/// The command that opens the pane, gives it the keyboard, and takes it away.
///
/// It is the SAME one that exits, and that is why it lives here instead of
/// being hand-written in the two places that look it up in the keymap: the
/// chord that runs it is the only one the pane does not forward to the shell.
pub const COMANDO: &str = "layout.terminal";

/// How a pane's shell is started, with what norte decides.
///
/// The program and the environment are set HERE and not in `norte-term`:
/// resolving the reader's shell and the `NORTE_LEVEL` contract are norte's
/// rules, not an emulator's.
///
/// # Errors
/// Whatever fails opening the pty or launching the shell.
pub fn abrir(dir: &std::path::Path, size: (u16, u16)) -> std::io::Result<TermPanel> {
    norte_term::pty::Shell::abrir(
        &norte_term::pty::Arranque {
            // `login_shell` refuses to return a relative `$SHELL` and falls
            // back to `/bin/sh` (#302): without that, `portable_pty` would
            // look it up via `cwd`, which here is the directory the reader is
            // looking at.
            programa: &norte_frontend::shell::login_shell(),
            dir,
            tam: size,
            // The child knows it is INSIDE norte, just like the subshell and
            // a suspension do: the same `NORTE_LEVEL` contract, and the
            // reader's prompt reads it to say so.
            env: &[(
                norte_frontend::shell::LEVEL_VAR.into(),
                norte_frontend::shell::next_norte_level().into(),
            )],
        },
        // The same table the subshell answers with: a terminal query gets
        // the same answer no matter where it comes from.
        norte_frontend::subshell::terminal_reply,
    )
}

/// The grid's rows as `ratatui` spans.
///
/// The chunking — where a row is cut — is done by `norte-term`, because it is
/// the same decision for both frontends and is made once. Here each segment
/// is only translated into this toolkit's style.
///
/// The content is FOREIGN and even so nothing is masked: what comes out of
/// the grid no longer carries any control byte, and that is guaranteed by the
/// grid, not by this code.
#[must_use]
pub fn filas<'a>(p: &Pantalla) -> Vec<ratatui::text::Line<'a>> {
    use ratatui::text::{Line, Span};
    let (_, height) = p.tamano();
    (0..height)
        .map(|row| {
            Line::from(
                p.fila_tramos(row)
                    .into_iter()
                    .map(|(text, style)| Span::styled(text, style_of(style)))
                    .collect::<Vec<Span<'a>>>(),
            )
        })
        .collect()
}

/// A terminal [`Estilo`] translated into `ratatui`'s.
///
/// An INDEXED color passes through as-is (`Color::Indexed`): on a terminal it
/// is resolved by whatever palette the reader has set in their emulator,
/// exactly what would happen if the program ran outside norte.
///
/// **That is why norte's theme has no place here, not even as an argument.**
/// This is ANOTHER program's content, not our chrome, and a theme that
/// changed an `ls --color`'s colors would be lying about what that program
/// said. Ours is the frame, and the frame is painted by whoever draws it.
fn style_of(e: Estilo) -> ratatui::style::Style {
    use ratatui::style::{Modifier, Style};
    let mut s = Style::default();
    if let Some(c) = color_of(e.fg) {
        s = s.fg(c);
    }
    if let Some(c) = color_of(e.bg) {
        s = s.bg(c);
    }
    let mut m = Modifier::empty();
    if e.negrita {
        m |= Modifier::BOLD;
    }
    if e.tenue {
        m |= Modifier::DIM;
    }
    if e.cursiva {
        m |= Modifier::ITALIC;
    }
    if e.subrayado {
        m |= Modifier::UNDERLINED;
    }
    if e.inverso {
        m |= Modifier::REVERSED;
    }
    if e.tachado {
        m |= Modifier::CROSSED_OUT;
    }
    s.add_modifier(m)
}

fn color_of(c: ColorTerm) -> Option<ratatui::style::Color> {
    use ratatui::style::Color;
    match c {
        ColorTerm::PorDefecto => None,
        ColorTerm::Indexado(n) => Some(Color::Indexed(n)),
        ColorTerm::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

/// Where the cursor goes within the content area, if it must be painted.
///
/// Returns `None` when the shell hid it (`CSI ?25l`, which any full-screen
/// program does) or when the pane does not have the keys: a cursor blinking
/// in a pane that does not hold them says the keyboard is there, and it is
/// not.
#[must_use]
pub fn cursor_en(
    p: &Pantalla,
    area: ratatui::layout::Rect,
    has_keyboard: bool,
) -> Option<(u16, u16)> {
    if !has_keyboard || !p.cursor_visible() {
        return None;
    }
    let (row, col) = p.cursor();
    let (width, height) = p.tamano();
    // The column can equal the width — the "pending wrap" state — and there
    // the cursor is painted in the last cell: it is where a real terminal
    // leaves it.
    let col = col.min(width.saturating_sub(1));
    (row < height).then(|| (area.x + col, area.y + row))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A terminal style reaches `ratatui` with its attributes intact, and an
    /// index stays an index: resolving it here would take away the reader's
    /// emulator's own palette.
    #[test]
    fn a_terminal_style_crosses_over_whole() {
        let e = Estilo {
            fg: ColorTerm::Indexado(4),
            bg: ColorTerm::Rgb(1, 2, 3),
            negrita: true,
            subrayado: true,
            ..Estilo::default()
        };
        let s = style_of(e);
        assert_eq!(s.fg, Some(ratatui::style::Color::Indexed(4)));
        assert_eq!(s.bg, Some(ratatui::style::Color::Rgb(1, 2, 3)));
        assert!(s.add_modifier.contains(ratatui::style::Modifier::BOLD));
        assert!(
            s.add_modifier
                .contains(ratatui::style::Modifier::UNDERLINED)
        );
    }

    /// Cells in a row with the same style are ONE span: eighty spans per row
    /// is what makes a `make` in the pane felt across the rest.
    #[test]
    fn equal_cells_group_into_one_span() {
        let mut p = Pantalla::nueva(10, 1);
        p.alimentar(b"aaa\x1b[31mbbb");
        let rows = filas(&p);
        let spans = &rows[0].spans;
        // Three, not two: after `bbb` four cells are left unwritten, and
        // those carry the DEFAULT style, not red. Grouping them with the red
        // one would paint the rest of the line's background with the last
        // command's color, which is the classic bug of a hand-rolled
        // emulator.
        assert_eq!(spans.len(), 3, "two styles and the padding: {spans:?}");
        assert_eq!(spans[0].content, "aaa");
        assert_eq!(spans[1].content, "bbb");
        assert_eq!(spans[2].content, "    ");
        assert_eq!(spans[2].style, ratatui::style::Style::default());
    }

    /// With no keyboard, no cursor is painted: that would say the keyboard is
    /// here.
    #[test]
    fn the_cursor_is_only_painted_with_the_keyboard_inside() {
        let p = Pantalla::nueva(10, 3);
        let area = ratatui::layout::Rect::new(5, 2, 10, 3);
        assert_eq!(cursor_en(&p, area, false), None);
        assert_eq!(cursor_en(&p, area, true), Some((5, 2)));
    }

    /// And not either when the shell hides it, which any full-screen program
    /// does while it paints.
    #[test]
    fn a_hidden_cursor_is_not_painted() {
        let mut p = Pantalla::nueva(10, 3);
        p.alimentar(b"\x1b[?25l");
        let area = ratatui::layout::Rect::new(0, 0, 10, 3);
        assert_eq!(cursor_en(&p, area, true), None);
    }
}
