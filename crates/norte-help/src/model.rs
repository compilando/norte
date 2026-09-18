//! The corpus model: what the parser produces and what each frontend
//! renders. Deliberately free of anything UI — no colours, no widths, no
//! ratatui/GPUI types (rule 7).

use std::fmt;

/// Identifier of a topic (the `id` of the front matter), unique per corpus.
///
/// The id is NOT normalised, NOT validated and NOT masked: it keeps the
/// bytes exactly as they arrived. That is deliberate — the corpus checks
/// (`see_also`, `[[topic]]`) compare byte-exactly, and a silent
/// normalisation here would let two distinct ids collide without anyone
/// noticing. That is why `" Copying "` and `"copying"` are DIFFERENT ids.
///
/// Consequence for anyone building an id out of THIRD-PARTY text: mask
/// BEFORE constructing it, never after. `parse_untrusted` goes further and
/// does not build one out of third-party text at all — a plugin topic's id is
/// the HOST-ASSIGNED plugin id, so a `help.md` declaring `id = "copying"`
/// cannot shadow the built-in topic of that name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TopicId(String);

impl TopicId {
    /// Builds an id from anything that is text.
    ///
    /// ```
    /// use norte_help::TopicId;
    ///
    /// let id = TopicId::new("copying");
    /// assert_eq!(id.as_str(), "copying");
    ///
    /// // The bytes are kept: no trimming, no lowercasing.
    /// assert_ne!(TopicId::new(" Copying "), id);
    /// ```
    #[must_use]
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// The id as a `&str`.
    ///
    /// ```
    /// use norte_help::TopicId;
    ///
    /// assert_eq!(TopicId::new("selection").as_str(), "selection");
    /// ```
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TopicId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a topic comes from: the binary, or a third-party plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Topic from the embedded corpus: TRUSTED text, never masked.
    BuiltIn,
    /// Topic from a plugin `help.md`: THIRD-PARTY text, already masked and
    /// bounded by the parser (see `parse_untrusted`).
    ///
    /// Two fields of the topic are deliberately NOT masked, and both are
    /// identities rather than prose: this `id`, which the HOST assigns, and
    /// [`Topic::commands`] / [`Span::CommandRef`], which are dispatch keys.
    /// Masking an identity is not a safety measure — it is not injective, so
    /// it silently maps distinct keys onto one — and the parser refuses a key
    /// it could not paint instead of rewriting it. The same `key`/`text`
    /// split the command palette makes.
    ///
    /// "Masked" means free of `norte_encoding::is_terminal_hazard` characters,
    /// and that is the whole claim. A dispatch key may still carry zero-width
    /// combining marks, which are not hazards and are kept knowingly — so a
    /// renderer gets no promise about how many CELLS a string occupies. See
    /// `is_own_command` for why that line is drawn there.
    Plugin {
        /// Plugin id in the catalogue: a LOOKUP KEY, assigned by the host and
        /// kept byte-exact. It is what a registry or approval lookup matches
        /// on, so the caller must pass one it has validated.
        id: String,
        /// The manifest's `publisher`, if it declares one.
        publisher: Option<String>,
        /// The content exceeded some limit and was truncated.
        truncated: bool,
        /// The file was not valid UTF-8 and was decoded lossily.
        lossy: bool,
    },
}

/// Kind of notice of a [`Block::Callout`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Callout {
    /// Neutral note.
    Note,
    /// Warning (something may go wrong).
    Warn,
    /// Tip (something goes faster).
    Tip,
}

/// Inline fragment inside a block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Span {
    /// Plain text.
    Text(String),
    /// Strong emphasis (`**like this**`).
    Strong(String),
    /// Emphasis (`*like this*`).
    Emph(String),
    /// Inline code (`` `like this` ``).
    Code(String),
    /// Reference to a command (`{{cmd:fs.copy}}`), UNRESOLVED: the chord is
    /// filled in by the frontend with `ChordResolver` (task 9).
    ///
    /// A DISPATCH KEY, byte-exact and never masked. From a plugin topic it is
    /// always that plugin's own `plugin:{id}:{command}` — `parse_untrusted`
    /// refuses anything else, so a plugin cannot borrow the host's warning
    /// chrome and the host's real chord to make `{{cmd:fs.delete}}` look like
    /// the host asking.
    CommandRef(String),
    /// Jump to another topic (`[[selection]]`), UNRESOLVED.
    TopicLink(TopicId),
}

/// Content block. A CLOSED vocabulary (ADR 0040): the fact that a hostile
/// `help.md` cannot express anything beyond this is precisely what makes it
/// safe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// Heading of level 1..=3.
    Heading {
        /// Level, saturated to 1..=3.
        level: u8,
        /// Heading text.
        text: String,
    },
    /// Paragraph.
    Paragraph(Vec<Span>),
    /// Bullet list (one level, no nesting).
    Bullets(Vec<Vec<Span>>),
    /// Code block with an optional language.
    Code {
        /// Language label of the fence, if any.
        lang: Option<String>,
        /// Literal content, with no marks interpreted.
        text: String,
    },
    /// Simple table with a header.
    Table {
        /// Header cells.
        header: Vec<String>,
        /// Rows already NORMALISED to `header.len()` cells: the parser pads
        /// the missing ones with empty cells and drops the extra ones, so a
        /// renderer can index by column without checking the length.
        ///
        /// The normalisation lives in the parser (task 5), not here; this
        /// type is the contract the parser must honour. It matters because
        /// the rows come out of a `split` over a plugin `help.md` — hostile
        /// text — and a ragged row would panic while drawing.
        rows: Vec<Vec<String>>,
    },
    /// Highlighted notice.
    Callout {
        /// Kind of notice.
        kind: Callout,
        /// Content of the notice.
        spans: Vec<Span>,
    },
}

/// Why a command cannot run right now.
///
/// The variants carry no text: each one is translated into a Fluent key at
/// draw time, so the same reason is explained in the user's language and in
/// each frontend's own words.
///
/// `#[non_exhaustive]` on purpose: phase H3d wires up the real sources of
/// availability (backend capabilities, plugin state, the policy's
/// `DenyReason`) and the variants will need refining. Marking it today means
/// that adding them then does not break the `match`es of the three
/// frontends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Reason {
    /// The active pane's backend is read-only (e.g. inside a zip).
    ReadOnlyBackend,
    /// The backend does not offer that capability.
    Unsupported,
    /// The plugin owning the command is disabled or unapproved.
    PluginInactive,
    /// The policy denies it for the current actor.
    PolicyDenied,
    /// The connection is degraded.
    ConnectionDegraded,
    /// The key is answered by whatever overlay is open, not by the command
    /// table — so there is no "command" to be available or not.
    ///
    /// The dialog verbs are the case: every overlay in norte answers the same
    /// six, and it answers them with fixed keys rather than by dispatching an
    /// id. A frontend that checks membership in its own command table before
    /// dispatching (which it must, or a help row reaches a catch-all that
    /// `debug_assert`s) would otherwise have to explain the refusal with a
    /// reason about the BACKEND, on a page whose whole subject is that those
    /// keys work — and the reader is looking at an overlay answering them
    /// while it says they are unsupported.
    ///
    /// It is not an impediment at all, which is what separates it from every
    /// other variant here: nothing is unavailable, the question was simply
    /// asked of the wrong table.
    AnsweredByTheOverlay,
    /// The command does not apply to what is selected right now: a directory
    /// has nothing to show in the viewer, and "open" means nothing over
    /// eleven marked entries at once.
    ///
    /// Unlike the other variants this one is about the SELECTION, not about
    /// the backend or the actor — it is what a context menu needs to explain
    /// an entry it dims for the shape of what was clicked rather than for
    /// what the provider can do.
    WrongTarget,
    /// The command mutates through a journal, and this session has none — so
    /// it needs norte running against the daemon.
    ///
    /// It is an impediment of BACKEND, like [`Reason::ReadOnlyBackend`], and
    /// not one of state: it does not change with the next keystroke, the
    /// reader has to start norte differently. The one command that answers it
    /// today is `pane.sync-dirs` — synchronising deletes and overwrites, so
    /// hard rule 4 requires a journal entry and an undo path, and the
    /// in-process engine the TUI builds without `--daemon` has neither.
    NeedsDaemon,
    /// The command needs a desktop to open a window on, and this session has
    /// none — a terminal over SSH is the case.
    ///
    /// Distinct from [`Reason::NeedsDaemon`], and both can be true at once:
    /// the daemon is about who holds the screen, this is about whether there
    /// is anywhere to put the other frontend. `app.handoff` answers it, and
    /// saying which of the two is missing matters because they are fixed
    /// differently — one by starting norte against the daemon, the other by
    /// sitting at the machine.
    NeedsDesktop,
}

/// Availability of a command row in the CURRENT context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    /// It can run now.
    Available,
    /// It cannot, with a reason to explain it.
    Unavailable {
        /// Reason shown next to the dimmed row.
        reason: Reason,
    },
}

impl Availability {
    /// `true` if the row can run.
    #[must_use]
    pub fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }

    /// The reason, if the row is unavailable.
    ///
    /// ```
    /// use norte_help::{Availability, Reason};
    ///
    /// let ok = Availability::Available;
    /// assert!(ok.is_available());
    /// assert_eq!(ok.reason(), None);
    ///
    /// let ro = Availability::Unavailable {
    ///     reason: Reason::ReadOnlyBackend,
    /// };
    /// assert!(!ro.is_available());
    /// assert_eq!(ro.reason(), Some(Reason::ReadOnlyBackend));
    /// ```
    #[must_use]
    pub fn reason(self) -> Option<Reason> {
        match self {
            Self::Available => None,
            Self::Unavailable { reason } => Some(reason),
        }
    }
}

/// Runnable row of a topic: a command the user can launch from the help with
/// the SAME dispatch path as the palette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandRow {
    /// Command id (`fs.copy`, `plugin:<id>:<cmd>`).
    pub command: String,
    /// Availability in the current context (injected by the frontend).
    pub avail: Availability,
}

/// A parsed topic of the corpus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Topic {
    /// Unique id.
    pub id: TopicId,
    /// Displayed title.
    pub title: String,
    /// Grouping tags for the index.
    pub tags: Vec<String>,
    /// Related topics.
    pub see_also: Vec<TopicId>,
    /// Commands the topic documents, in the desired display order. Dispatch
    /// keys, never masked — see [`Origin::Plugin`] and [`Span::CommandRef`].
    pub commands: Vec<String>,
    /// UI contexts that open THIS topic with F1.
    pub context: Vec<String>,
    /// Body.
    pub blocks: Vec<Block>,
    /// Provenance.
    pub origin: Origin,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_topic_id_keeps_its_bytes_and_displays_them() {
        let id = TopicId::new("copying");
        assert_eq!(id.as_str(), "copying");
        assert_eq!(id.to_string(), "copying");

        // It does NOT normalise: no trimming, no lowercasing. If it ever
        // did, `see_also` and `[[topic]]` would start resolving to topics
        // the author never wrote.
        let odd = TopicId::new(" Copying ");
        assert_eq!(odd.as_str(), " Copying ");
        assert_eq!(odd.to_string(), " Copying ");
        assert_ne!(odd, id, "two distinct ids must never collide");
    }

    #[test]
    fn an_unavailable_row_carries_its_reason() {
        let row = CommandRow {
            command: "fs.copy".to_owned(),
            avail: Availability::Unavailable {
                reason: Reason::ReadOnlyBackend,
            },
        };
        assert!(!row.avail.is_available());
        assert_eq!(
            row.avail.reason(),
            Some(Reason::ReadOnlyBackend),
            "the UI needs the reason to explain it, not just the fact"
        );
    }
}
