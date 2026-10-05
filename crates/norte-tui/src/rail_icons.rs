//! The panel column's big icons (spec 2026-10-05): which terminal can
//! paint them, and how.
//!
//! The column is either today's one-cell glyphs or, on a terminal that
//! can paint graphics, the window's own icons at 2×2 cells. Which one is
//! decided ONCE, by probing, never by `TERM` — a terminal that cannot answer
//! the probe (tmux without passthrough) keeps the glyphs.

/// What the panel column draws its icons with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RailBackend {
    /// Kitty's graphics protocol: the shared SVG rasterised, 2×2 cells.
    KittyGraphics,
    /// One cell per icon: Unicode, Nerd Font or the letter. Every terminal.
    #[default]
    Glyph,
}

/// The backend for this terminal: `[ui] images` first — `off` and `blocks`
/// are "no pixels here", and `kitty` trusts the reader over the probe, as
/// the viewer does — then the probe's answer.
#[must_use]
pub fn choose(images: norte_config::Images, kitty: bool) -> RailBackend {
    use norte_config::Images;
    match images {
        Images::Kitty => RailBackend::KittyGraphics,
        Images::Auto if kitty => RailBackend::KittyGraphics,
        Images::Auto | Images::Off | Images::Blocks => RailBackend::Glyph,
    }
}

/// This frame's backend: derived, not stored, so a config reload or the
/// startup probe needs nobody to remember to update it.
#[must_use]
pub fn backend(app: &crate::app::App) -> RailBackend {
    choose(app.chrome.images(), crate::kitty_graphics::supported())
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_config::Images;

    #[test]
    fn choose_follows_the_probe_and_the_images_switch() {
        assert_eq!(choose(Images::Auto, true), RailBackend::KittyGraphics);
        assert_eq!(choose(Images::Kitty, false), RailBackend::KittyGraphics);
        assert_eq!(choose(Images::Auto, false), RailBackend::Glyph);
        assert_eq!(choose(Images::Off, true), RailBackend::Glyph);
        assert_eq!(choose(Images::Blocks, true), RailBackend::Glyph);
    }
}
