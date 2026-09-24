//! What a PLUGIN paints in a slot: a text frame with style and clickable
//! zones (spec 2026-09-15, phase 3).
//!
//! The guest does not draw: it DESCRIBES. It sends lines of
//! [`crate::ansi::StyledSpan`] — the same span already used by styled
//! previews, with a role validated against the closed set of
//! `norte_theme::Role` — and a list of [`Hit`], zones that run a CATALOGUE
//! command when clicked. Never a free-form action: a plugin cannot do
//! anything with a click that the reader could not do with a key, so the
//! policy stays intact (hard rule 9).
//!
//! The limits are the PROTOCOL's (`norte_proto::methods::PANEL_MAX_*`), and
//! here they are only re-exported: the two surfaces — the terminal and the
//! window — have to clip the SAME way, and a frame one accepts and the
//! other rejects is exactly the divergence ADR 0077 is chasing. Writing the
//! numbers again in this crate would mean having two that must match and
//! nothing enforcing it.

use crate::ansi::StyledSpan;

/// Line limit of a frame.
///
/// This is the PROTOCOL's, re-exported: an eight-row panel that sends a
/// thousand lines describes something nobody is going to read, and the
/// limit caps that cost without turning it into an error (clipping is
/// fail-soft, like everything cosmetic). Declaring it again here would be
/// the same number written in two places, which is exactly what diverges
/// at the first change.
pub use norte_proto::methods::PANEL_MAX_LINES as MAX_LINES;

/// Span-per-line limit. The protocol's; see [`MAX_LINES`].
pub use norte_proto::methods::PANEL_MAX_SPANS_PER_LINE as MAX_SPANS_PER_LINE;

/// Clickable-zone limit of a frame. The protocol's; see [`MAX_LINES`].
pub use norte_proto::methods::PANEL_MAX_HITS as MAX_HITS;

/// A clickable zone of the frame: clicking it runs a catalogue command.
///
/// `row`/`col` are cells INSIDE the frame, not the screen: whoever paints
/// it knows where the slot landed and does the math. `width` is how many
/// cells it spans from `col`; a zone taller than one row is described with
/// one `Hit` per row, which is what avoids having to define overlaps in
/// two dimensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// Row of the frame where it starts, counting from zero.
    pub row: u16,
    /// Column where it starts, counting from zero.
    pub col: u16,
    /// How many cells it spans. Zero = cannot be clicked.
    pub width: u16,
    /// The catalogue command it runs. If it does not exist, nothing
    /// happens: normal dispatch resolves it, and it already knows how to
    /// say "not here".
    pub command: String,
    /// Its argument, if it carries one (a directory, a name).
    pub arg: Option<String>,
}

/// The active language's code, exactly as it travels to a guest.
///
/// Here and not in each frontend: the terminal and the window tell the
/// SAME thing to the same plugin. Two code tables start out equal and
/// diverge as soon as one more language appears — and the difference would
/// only show up in front of a translated panel.
///
/// ```
/// assert!(matches!(norte_frontend::frame::lang_code(), "es" | "en"));
/// ```
#[must_use]
pub fn lang_code() -> &'static str {
    match norte_i18n::active() {
        norte_i18n::Lang::Es => "es",
        norte_i18n::Lang::En => "en",
    }
}

/// The commands a panel ZONE is allowed to name.
///
/// The plugin picks the label AND the command, and nothing ties them
/// together: a zone that says "Refresh" can name `pane.unpack`, which
/// copies. The consent the reader gave was to PAINT — the manifest
/// capability is `panel` — not to drive the file manager, so the click has
/// to stay within the same scope as the keys a focused pane receives:
/// chrome and moving between panes.
///
/// Lives here, next to [`Hit`], and not in each frontend: the terminal and
/// the window have to filter the SAME way, and two lists diverge at the
/// first addition (ADR 0077). Whatever a plugin wants to offer beyond this
/// is requested with its own command, which goes through the catalogue,
/// through consent and through policy like any other.
pub const ZONA_PERMITIDA: &[&str] = &[
    "layout.grow",
    "layout.shrink",
    "layout.focus-next",
    "layout.focus-prev",
    "layout.places",
    "layout.preview",
    "layout.processes",
    "layout.metadata",
    "layout.log",
    "pane.tree",
];

/// Can a panel zone name this command?
///
/// ```
/// use norte_frontend::frame::zona_puede;
///
/// assert!(zona_puede("layout.focus-next"));
/// assert!(!zona_puede("pane.unpack"), "a plugin does not drive the file manager");
/// assert!(!zona_puede("app.quit"));
/// ```
#[must_use]
pub fn zona_puede(command: &str) -> bool {
    ZONA_PERMITIDA.contains(&command)
}

/// A paintable frame: styled lines and clickable zones.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StyledFrame {
    /// The lines, top to bottom. An empty line is a blank line.
    pub lines: Vec<Vec<StyledSpan>>,
    /// The clickable zones, in the order they arrived.
    pub hits: Vec<Hit>,
}

impl StyledFrame {
    /// Builds a CLIPPED frame from what a guest sent.
    ///
    /// Clips lines, spans and zones to this module's limits, and drops
    /// zones that point at a row the clipping took away: a `Hit` over a
    /// line that is not painted is an invisible zone that runs something,
    /// which is worse than not having it.
    ///
    /// ```
    /// use norte_frontend::frame::{Hit, StyledFrame};
    ///
    /// let f = StyledFrame::clamped(
    ///     Vec::new(),
    ///     vec![Hit { row: 3, col: 0, width: 4, command: "nav.enter".to_owned(), arg: None }],
    /// );
    /// assert!(f.hits.is_empty(), "with no lines there is nowhere to click");
    /// ```
    #[must_use]
    pub fn clamped(mut lines: Vec<Vec<StyledSpan>>, hits: Vec<Hit>) -> Self {
        lines.truncate(MAX_LINES);
        for line in &mut lines {
            line.truncate(MAX_SPANS_PER_LINE);
        }
        let height = lines.len();
        let hits: Vec<Hit> = hits
            .into_iter()
            .filter(|h| usize::from(h.row) < height && h.width > 0)
            .take(MAX_HITS)
            .collect();
        Self { lines, hits }
    }

    /// The frame a guest returned, CLIPPED and SANITIZED.
    ///
    /// The conversion lives here and not in each frontend for the same
    /// reason as [`crate::ansi::span_de_wire`], which is what sanitizes
    /// each span: the terminal and the window have to clip and mask the
    /// SAME way. A second, hand-written conversion was exactly what crept
    /// into the terminal — it copied the fields and skipped the masking —
    /// so the window does not write its own.
    ///
    /// ```
    /// use norte_frontend::frame::StyledFrame;
    /// use norte_proto::methods::{PanelFrame, PanelHit, SpanWire};
    ///
    /// let f = StyledFrame::de_wire(&PanelFrame {
    ///     plugin_id: "git".to_owned(),
    ///     lines: vec![vec![SpanWire {
    ///         text: "branch".to_owned(),
    ///         role: None,
    ///         fg: None,
    ///         bg: None,
    ///     }]],
    ///     hits: vec![PanelHit {
    ///         row: 9,
    ///         col: 0,
    ///         width: 4,
    ///         command: "layout.focus-next".to_owned(),
    ///         arg: None,
    ///     }],
    ///     state: None,
    /// });
    /// assert_eq!(f.lines.len(), 1);
    /// assert!(f.hits.is_empty(), "a zone over a row that does not exist is dropped");
    /// ```
    #[must_use]
    pub fn de_wire(frame: &norte_proto::methods::PanelFrame) -> Self {
        let lines = frame
            .lines
            .iter()
            .map(|line| line.iter().map(crate::ansi::span_de_wire).collect())
            .collect();
        let hits = frame
            .hits
            .iter()
            .map(|h| Hit {
                row: h.row,
                col: h.col,
                width: h.width,
                command: h.command.clone(),
                arg: h.arg.clone(),
            })
            .collect();
        Self::clamped(lines, hits)
    }

    /// The clickable zone at that cell of the frame, if there is one.
    ///
    /// The FIRST one that matches, which is the order the guest sent them
    /// in: two overlapping zones are a guest error, and picking the first
    /// is a rule that can be explained — picking "the smallest" or "the
    /// last" would ask the reader to guess which.
    ///
    /// ```
    /// use norte_frontend::frame::{Hit, StyledFrame};
    ///
    /// let f = StyledFrame::clamped(
    ///     vec![Vec::new()],
    ///     vec![Hit { row: 0, col: 2, width: 3, command: "pane.reload".to_owned(), arg: None }],
    /// );
    /// assert!(f.hit_at(0, 1).is_none());
    /// assert_eq!(f.hit_at(0, 4).map(|h| h.command.as_str()), Some("pane.reload"));
    /// assert!(f.hit_at(0, 5).is_none());
    /// ```
    #[must_use]
    pub fn hit_at(&self, row: u16, col: u16) -> Option<&Hit> {
        self.hits
            .iter()
            // The end is computed in `u32`: with `saturating_add`, a zone
            // flush against the top of the coordinate space lost its last
            // cell — the end saturated at `u16::MAX` and the comparison is
            // exclusive — so the frame's right edge could not be clicked.
            .find(|h| {
                h.row == row
                    && col >= h.col
                    && u32::from(col) < u32::from(h.col) + u32::from(h.width)
            })
    }

    /// How many rows it takes up.
    #[must_use]
    pub fn height(&self) -> usize {
        self.lines.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(row: u16, col: u16, width: u16) -> Hit {
        Hit {
            row,
            col,
            width,
            command: "nav.enter".to_owned(),
            arg: None,
        }
    }

    fn span(text: &str) -> StyledSpan {
        StyledSpan {
            text: text.to_owned(),
            role: None,
            fg: None,
            bg: None,
        }
    }

    #[test]
    fn the_limits_clip_lines_spans_and_zones() {
        let lines = vec![vec![span("x"); MAX_SPANS_PER_LINE + 10]; MAX_LINES + 10];
        let hits = (0..u16::try_from(MAX_HITS + 10).expect("fits"))
            .map(|i| hit(i, 0, 1))
            .collect();
        let f = StyledFrame::clamped(lines, hits);
        assert_eq!(f.lines.len(), MAX_LINES);
        assert_eq!(f.lines[0].len(), MAX_SPANS_PER_LINE);
        assert_eq!(f.hits.len(), MAX_HITS);
    }

    /// A zone pointing at a clipped row goes away with it.
    ///
    /// If it stayed, the frame would have a cell that runs something and
    /// shows nothing: an invisible button, which is worse than a missing
    /// one.
    #[test]
    fn a_zone_over_a_row_that_is_not_painted_is_dropped() {
        let f = StyledFrame::clamped(vec![vec![span("a")], vec![span("b")]], vec![hit(5, 0, 3)]);
        assert!(f.hits.is_empty());
    }

    /// Zero width is not a zone: it is a coordinate.
    #[test]
    fn a_zone_with_no_width_cannot_be_clicked() {
        let f = StyledFrame::clamped(vec![vec![span("a")]], vec![hit(0, 0, 0)]);
        assert!(f.hits.is_empty());
    }

    #[test]
    fn the_first_zone_that_matches_wins() {
        let mut a = hit(0, 0, 10);
        a.command = "first".to_owned();
        let mut b = hit(0, 2, 2);
        b.command = "second".to_owned();
        let f = StyledFrame::clamped(vec![vec![span("hello")]], vec![a, b]);
        assert_eq!(f.hit_at(0, 3).map(|h| h.command.as_str()), Some("first"));
    }

    /// The right edge is EXCLUSIVE: a three-cell zone from 2 covers 2, 3
    /// and 4, and not 5.
    #[test]
    fn a_zones_right_edge_does_not_count() {
        let f = StyledFrame::clamped(vec![vec![span("hello")]], vec![hit(0, 2, 3)]);
        assert!(f.hit_at(0, 1).is_none());
        assert!(f.hit_at(0, 2).is_some());
        assert!(f.hit_at(0, 4).is_some());
        assert!(f.hit_at(0, 5).is_none());
    }

    /// A zone at the end of the coordinate space does not overflow when
    /// summed.
    #[test]
    fn a_zone_flush_against_the_top_does_not_overflow() {
        let f = StyledFrame::clamped(vec![vec![span("a")]], vec![hit(0, u16::MAX - 1, 10)]);
        assert!(f.hit_at(0, u16::MAX).is_some());
    }
}
