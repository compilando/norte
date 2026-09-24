//! The disk map in the TUI: the kind and its keys.
//!
//! The state lives in [`norte_frontend::diskmap`], not here, for the same
//! reason as the processes pane: "which child is chosen" is a question both
//! surfaces answer, and the same decision written twice drifts apart in
//! silence (ADR 0077). What stays in this crate is the `KIND` — what the
//! layout writes — and which key does what while the pane holds the keyboard.

pub use norte_frontend::diskmap::{DiskMap, State};

/// The kind that occupies a disk-map slot.
pub const KIND: &str = "disk-map";

/// What a key asks the map to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapAction {
    /// Move the selection `n` rectangles.
    Mover(isize),
    /// Enter the chosen child.
    Enter,
    /// Re-measure this directory.
    Remeasure,
    /// Return the keyboard without closing the pane.
    Leave,
}

/// Translates a disk-map key.
///
/// An explicit `match` and NOT the keymap, on the same grounds as the log
/// pane: these keys only exist while the map holds the keyboard, and putting
/// them in the keymap would force all seven presets to declare shortcuts that
/// mean nothing outside here.
///
/// `r` for "remedir" (re-measure) is the only loose letter, and it earns its
/// place: a map is a snapshot from a while ago, and re-measuring without
/// leaving the pane is exactly what one wants right after deleting something
/// big.
#[must_use]
pub fn key(
    code: crossterm::event::KeyCode,
    mods: crossterm::event::KeyModifiers,
) -> Option<MapAction> {
    use crossterm::event::{KeyCode, KeyModifiers};
    if !(mods.is_empty() || mods == KeyModifiers::SHIFT) {
        return None;
    }
    Some(match code {
        KeyCode::Down | KeyCode::Right => MapAction::Mover(1),
        KeyCode::Up | KeyCode::Left => MapAction::Mover(-1),
        // Pages move in blocks, like in any list: a map of 4096 rectangles is
        // not walked one at a time.
        KeyCode::PageDown => MapAction::Mover(PAGE),
        KeyCode::PageUp => MapAction::Mover(-PAGE),
        KeyCode::Home => MapAction::Mover(isize::MIN),
        KeyCode::End => MapAction::Mover(isize::MAX),
        KeyCode::Enter => MapAction::Enter,
        KeyCode::Char('r') => MapAction::Remeasure,
        KeyCode::Esc => MapAction::Leave,
        _ => return None,
    })
}

/// How many rectangles a page skips.
const PAGE: isize = 10;

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    /// The four arrows move: in a map there is no "up" and "sideways", there
    /// is a list of rectangles being walked.
    #[test]
    fn the_four_arrows_move() {
        for (code, expected) in [
            (KeyCode::Down, MapAction::Mover(1)),
            (KeyCode::Right, MapAction::Mover(1)),
            (KeyCode::Up, MapAction::Mover(-1)),
            (KeyCode::Left, MapAction::Mover(-1)),
        ] {
            assert_eq!(key(code, KeyModifiers::NONE), Some(expected));
        }
    }

    /// Enter enters, `r` re-measures, `Esc` releases the keyboard WITHOUT
    /// closing.
    #[test]
    fn the_maps_own_keys() {
        assert_eq!(
            key(KeyCode::Enter, KeyModifiers::NONE),
            Some(MapAction::Enter)
        );
        assert_eq!(
            key(KeyCode::Char('r'), KeyModifiers::NONE),
            Some(MapAction::Remeasure)
        );
        assert_eq!(
            key(KeyCode::Esc, KeyModifiers::NONE),
            Some(MapAction::Leave)
        );
    }

    /// With Ctrl or Alt it is not the map's: those go to the keymap, which is
    /// where `alt+z` lives to close it and `alt+l` to open the one beside it.
    #[test]
    fn with_ctrl_or_alt_the_key_is_not_the_maps() {
        assert!(key(KeyCode::Char('r'), KeyModifiers::CONTROL).is_none());
        assert!(key(KeyCode::Down, KeyModifiers::ALT).is_none());
    }

    /// Any other letter does nothing: the map does not swallow the alphabet.
    #[test]
    fn an_unrelated_letter_is_not_the_maps() {
        assert!(key(KeyCode::Char('q'), KeyModifiers::NONE).is_none());
    }
}
