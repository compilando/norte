//! The layout buttons, to the right of the menu bar (ADR 0133).
//!
//! VS Code's, top right, say "the screen's shape changes here" without
//! opening a menu. In norte these are four commands that already exist —
//! split side by side, split top and bottom, equalize, pick a layout — and
//! this table is the ONE list: the TUI paints them as ASCII cells and the
//! window as icons, and both click through the same `id`.

use norte_i18n::{Lang, t_in};

/// A layout button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutButton {
    /// Stable id: the one that comes back with a click and the one that
    /// picks the icon.
    pub id: &'static str,
    /// The catalogue command that runs.
    pub command: &'static str,
    /// How it is painted in the terminal: ASCII in brackets, like `[+]` in
    /// the tab bar — a box-drawing glyph can measure two cells.
    pub glyph: &'static str,
}

/// The five, in the order they are painted.
pub const BUTTONS: [LayoutButton; 5] = [
    LayoutButton {
        id: "split-h",
        command: "layout.split-h",
        glyph: "[|]",
    },
    LayoutButton {
        id: "split-v",
        command: "layout.split-v",
        glyph: "[-]",
    },
    LayoutButton {
        id: "equalize",
        command: "layout.equalize",
        glyph: "[=]",
    },
    // Flip (ADR 0138): side by side <-> one above the other.
    LayoutButton {
        id: "flip",
        command: "layout.flip",
        glyph: "[/]",
    },
    LayoutButton {
        id: "pick",
        command: "layout.pick",
        glyph: "[#]",
    },
];

/// The button of an id, if it exists.
#[must_use]
pub fn by_id(id: &str) -> Option<&'static LayoutButton> {
    BUTTONS.iter().find(|b| b.id == id)
}

/// Its short name: the SAME as its menu entry.
#[must_use]
pub fn label(b: &LayoutButton, lang: Lang) -> String {
    t_in(lang, &format!("menu-item-{}", b.command.replace('.', "-")))
}

/// Cells all of them take up in the terminal, with a space between each two.
#[must_use]
pub fn width() -> usize {
    width_of(&BUTTONS.iter().collect::<Vec<_>>())
}

/// Cells `buttons` take up in the terminal, with a space between each two.
#[must_use]
pub fn width_of(buttons: &[&LayoutButton]) -> usize {
    buttons.iter().map(|b| b.glyph.len()).sum::<usize>() + buttons.len().saturating_sub(1)
}

/// The one that gives way first when they do not all fit: flip (ADR 0138)
/// is the newest and least used, and the four that have always been there
/// cannot disappear because of it in a hundred-column terminal.
const DISPENSABLE: &str = "flip";

/// The buttons that fit in `available` cells: all of them, or all but flip
/// (the one that gives way first), or none. Never a random half-set: a
/// button that is there sometimes and not others, depending on width, is
/// learned worse than one that always gives way the same.
#[must_use]
pub fn fitting(available: usize) -> Vec<&'static LayoutButton> {
    let all: Vec<&LayoutButton> = BUTTONS.iter().collect();
    if width_of(&all) <= available {
        return all;
    }
    let without: Vec<&LayoutButton> = BUTTONS.iter().filter(|b| b.id != DISPENSABLE).collect();
    if width_of(&without) <= available {
        return without;
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each button runs a command that exists and has a name in both
    /// languages: a mute button, or one that does nothing, is worse than
    /// not having it.
    #[test]
    fn every_button_exists_and_is_named() {
        for b in &BUTTONS {
            assert!(
                crate::keymap::catalogue::lookup(b.command).is_some(),
                "{}",
                b.command
            );
            for lang in [Lang::Es, Lang::En] {
                assert!(
                    !label(b, lang).starts_with("menu-item-"),
                    "{} {lang:?}",
                    b.id
                );
            }
            assert!(b.glyph.is_ascii());
            assert_eq!(by_id(b.id), Some(b));
        }
        assert_eq!(width(), 19);
    }

    /// With no room for all five, flip gives way; with no room for four,
    /// none.
    #[test]
    fn flip_gives_way_first() {
        assert_eq!(fitting(19).len(), 5);
        let four = fitting(18);
        assert_eq!(four.len(), 4);
        assert!(four.iter().all(|b| b.id != "flip"));
        assert_eq!(width_of(&four), 15);
        assert!(fitting(14).is_empty());
    }
}
