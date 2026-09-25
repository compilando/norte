//! A log line as DATA, with nothing of `tracing` in it.
//!
//! Deliberately outside the `logging` feature. Who produces the lines is the
//! subscriber (and it pulls in `tracing-subscriber` and `tracing-appender`),
//! but who PAINTS them is a frontend, and a presentation frontend has no
//! reason to compile a subscriber to know how to draw a list. With the type
//! here, `norte-frontend` filters and lays out with no new dependencies, and
//! the graphical window inherits the same thing (ADR 0077: the presentation
//! decision is made once).

/// A line's level, from least to most verbose.
///
/// Its own type, not `tracing::Level`, for the reason above, and `Ord`
/// because the question always asked of it is "is this at most as verbose as
/// what is being shown?".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    /// Something failed.
    Error,
    /// Something is about to fail, or degraded.
    Warn,
    /// What happens, under normal conditions.
    Info,
    /// Detail for debugging.
    Debug,
    /// Everything.
    Trace,
}

impl LogLevel {
    /// The five-column label that gets painted, already aligned.
    ///
    /// FIXED width: a level column that shifts leaves the message starting in
    /// different places, and the list stops being scannable by eye.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN ",
            Self::Info => "INFO ",
            Self::Debug => "DEBUG",
            Self::Trace => "TRACE",
        }
    }

    /// The STABLE identifier, for the window's bridge (#326).
    ///
    /// Separate from [`Self::label`], which is what gets PAINTED: that one
    /// carries its five-column padding and could change shape the day the
    /// column changes width. This is a closed vocabulary a renderer compares
    /// by equality to color and to mark which one is set, and comparing
    /// against a screen label would tie the color to the width.
    ///
    /// ```
    /// use norte_config::logline::LogLevel;
    /// assert_eq!(LogLevel::Warn.wire(), "warn");
    /// assert_eq!(LogLevel::from_wire("warn"), Some(LogLevel::Warn));
    /// // One that does not exist does not fall onto another: it is reported
    /// // unknown.
    /// assert_eq!(LogLevel::from_wire("verbose"), None);
    /// ```
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }

    /// The level for a wire identifier, or `None` if it is not known.
    ///
    /// `None`, not a default value: falling back to `Info` for something
    /// unrecognized would leave the panel showing something other than what
    /// was asked for, silently.
    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        Self::all().into_iter().find(|l| l.wire() == s)
    }

    /// All of them, from least to most verbose.
    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::Error,
            Self::Warn,
            Self::Info,
            Self::Debug,
            Self::Trace,
        ]
    }
}

/// A line already ready to paint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// Milliseconds since the epoch (UTC), to format with the same rule as
    /// the date columns.
    pub epoch_ms: i64,
    /// Level of the event.
    pub level: LogLevel,
    /// Module that emitted it (`norte_core::connect`).
    pub target: String,
    /// The message and its fields, already flattened.
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The order is by VERBOSITY, which is the comparison the filter makes.
    #[test]
    fn order_goes_from_least_to_most_verbose() {
        assert!(LogLevel::Error < LogLevel::Warn);
        assert!(LogLevel::Warn < LogLevel::Info);
        assert!(LogLevel::Info < LogLevel::Debug);
        assert!(LogLevel::Debug < LogLevel::Trace);
    }

    /// The labels measure the same: otherwise the message starts in
    /// different columns and the list cannot be scanned by eye.
    #[test]
    fn labels_share_the_same_width() {
        for l in LogLevel::all() {
            assert_eq!(l.label().len(), 5, "{l:?} breaks the column");
        }
    }

    /// **The level vocabulary is THE SAME as the protocol's, in both
    /// directions** (0.65.0, #328).
    ///
    /// There are two copies because there have to be: `norte-proto` cannot
    /// depend on this crate — the arrow points the other way — so
    /// `methods::LOG_LEVELS` repeats the five strings as the wire's
    /// vocabulary. And this test is the only place in the tree from which
    /// both are visible, so this is where the equality lives. Same pattern
    /// as the hashing vocabulary, where `norte-core` keeps a frozen copy for
    /// the journal's format.
    ///
    /// Checked in BOTH directions on purpose. Only "every one of ours is in
    /// the protocol" would allow adding one to the wire that no frontend
    /// could paint; only the reverse would allow adding one here that could
    /// not be requested over the wire. And `from_wire` closes the return
    /// trip: the strings matching is worthless if the incoming one cannot be
    /// converted back.
    #[test]
    fn level_vocabulary_matches_the_protocols() {
        use std::collections::BTreeSet;

        let ours: BTreeSet<&str> = LogLevel::all().into_iter().map(LogLevel::wire).collect();
        let wires: BTreeSet<&str> = norte_proto::methods::LOG_LEVELS.iter().copied().collect();
        assert_eq!(
            ours, wires,
            "the two level vocabularies have drifted apart: renaming one is \
             a wire change (bump + golden), and adding one must happen in \
             both places"
        );
        for w in norte_proto::methods::LOG_LEVELS {
            assert_eq!(
                LogLevel::from_wire(w).map(LogLevel::wire),
                Some(*w),
                "`{w}` travels over the wire and cannot be converted back"
            );
        }
    }
}
