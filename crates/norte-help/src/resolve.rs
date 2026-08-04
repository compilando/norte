//! Resolving the corpus' live command marks: the seam through which a
//! frontend supplies what only IT knows.
//!
//! The corpus stores command ids and never a key, which is the whole reason
//! its prose cannot lie: a user who rebinds copy to `Ctrl+C` gets a page that
//! says `Ctrl+C`, because the chord is looked up as the page is drawn. That
//! lookup needs the effective keymap, the command catalogue and the state of
//! the current pane — three things this crate must not know about (rule 7),
//! so they arrive through a trait.
//!
//! The dependency therefore points from the frontend INTO here, never the
//! other way: `norte-tui` implements [`ChordResolver`] over its
//! `Effective`/Fluent pair exactly as `norte_frontend::palette::first_chord`
//! already joins a command to its chord for the palette. A `norte-help` that
//! depended on a frontend could only ever serve one of the three.

use crate::model::{Availability, CommandRow, Span, Topic};
use crate::parse::is_blank_id;

/// What a frontend must be able to answer for the help to be drawn.
///
/// Three questions, and each is asked of the FRONTEND because each has a
/// different answer in each one: the TUI's chord comes from a crossterm
/// keymap, the GUI's from its own, and a plain-text dump has none at all.
///
/// ```
/// use norte_help::{Availability, ChordResolver, Span, render_span};
///
/// struct Orthodox;
///
/// impl ChordResolver for Orthodox {
///     fn chord(&self, command: &str) -> Option<String> {
///         (command == "pane.copy").then(|| "F5".to_owned())
///     }
///
///     fn label(&self, command: &str) -> String {
///         format!("the {command} command")
///     }
///
///     fn availability(&self, _command: &str) -> Availability {
///         Availability::Available
///     }
/// }
///
/// let mark = Span::CommandRef("pane.copy".to_owned());
/// assert_eq!(render_span(&mark, &Orthodox), "F5");
/// ```
pub trait ChordResolver {
    /// The user's EFFECTIVE chord for a command, or `None` when nothing is
    /// bound to it — which is a normal answer, not an error: a command
    /// reachable only from the palette has no key, and so does one the user
    /// unbound.
    ///
    /// The string is PAINTED, so the implementor masks it: a chord can come
    /// from an untrusted project keymap layer, and
    /// `norte_frontend::palette::first_chord` is the join that already does
    /// this (`norte_encoding::mask_terminal_hazards`). Masking here instead
    /// would put the terminal's rules inside a render-agnostic crate, and
    /// the GUI does not share them.
    fn chord(&self, command: &str) -> Option<String>;

    /// A SHORT label for the command — what the reader sees when there is no
    /// key to show, and the second column of a runnable row. In the TUI it is
    /// the Fluent catalogue (`help-cmd-*`), so it arrives translated.
    ///
    /// Returning something blank is not fatal: the helpers below fall through
    /// to the command id rather than paint a gap.
    fn label(&self, command: &str) -> String;

    /// Whether the command can run RIGHT NOW, in the context the page is
    /// being drawn in — inside a read-only archive, over a degraded
    /// connection, for an agent the policy denies.
    fn availability(&self, command: &str) -> Availability;
}

/// What a `{{cmd:…}}` mark resolved to, and WHICH of the two it is.
///
/// A plain `String` would lose the distinction, and the distinction is the
/// one thing a renderer needs from it: a chord is painted as a key (reversed,
/// boxed, whatever that frontend's key style is) and a name is painted as
/// prose. Collapsing them makes every mark look like a key, including the
/// ones that are not — which is the exact lie this module exists to prevent.
///
/// Both variants are guaranteed NON-BLANK; see [`render_command`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandText {
    /// The user's effective key for the command.
    Chord(String),
    /// No key to show, so the command names itself: its short label, or the
    /// id when the label is blank too.
    Name(String),
}

impl CommandText {
    /// The text to paint, whichever it turned out to be. Never blank.
    ///
    /// ```
    /// use norte_help::CommandText;
    ///
    /// assert_eq!(CommandText::Chord("F5".to_owned()).text(), "F5");
    /// assert_eq!(CommandText::Name("copy".to_owned()).text(), "copy");
    /// ```
    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            Self::Chord(t) | Self::Name(t) => t,
        }
    }

    /// `true` when this is a real key the reader can press.
    ///
    /// ```
    /// use norte_help::CommandText;
    ///
    /// assert!(CommandText::Chord("F5".to_owned()).is_chord());
    /// assert!(!CommandText::Name("copy".to_owned()).is_chord());
    /// ```
    #[must_use]
    pub fn is_chord(&self) -> bool {
        matches!(self, Self::Chord(_))
    }

    /// Consumes it into its text.
    ///
    /// ```
    /// use norte_help::CommandText;
    ///
    /// assert_eq!(CommandText::Chord("F5".to_owned()).into_text(), "F5");
    /// ```
    #[must_use]
    pub fn into_text(self) -> String {
        match self {
            Self::Chord(t) | Self::Name(t) => t,
        }
    }
}

/// Resolves one command reference for display: the chord if there is one,
/// the label if there is not, and the id if the label is blank as well.
///
/// The chain never produces something that paints nothing, and each step is
/// a deliberate refusal to do the easy wrong thing. Inventing a key would be
/// a lie the reader acts on. Rendering blank would be a hole in a sentence —
/// "press  to copy" — which a reader cannot even report, because there is
/// nothing there to report.
///
/// "Blank" is the PARSER's definition (`is_blank_id`), not `trim`: a string of
/// `U+FFFD`s is what a masked hazard becomes, and a Hangul filler is a
/// character that paints nothing while being neither empty nor whitespace.
/// One definition, so a chord the parser would have refused as an id is not
/// silently accepted here as a key.
///
/// ```
/// use norte_help::{Availability, ChordResolver, CommandText, render_command};
///
/// struct Sparse;
///
/// impl ChordResolver for Sparse {
///     fn chord(&self, command: &str) -> Option<String> {
///         (command == "pane.copy").then(|| "F5".to_owned())
///     }
///     fn label(&self, _command: &str) -> String {
///         String::new() // a catalogue with no entry for it
///     }
///     fn availability(&self, _command: &str) -> Availability {
///         Availability::Available
///     }
/// }
///
/// assert_eq!(render_command("pane.copy", &Sparse), CommandText::Chord("F5".to_owned()));
/// assert_eq!(
///     render_command("pane.rename", &Sparse),
///     CommandText::Name("pane.rename".to_owned()),
///     "no key and no label still names the command"
/// );
/// ```
#[must_use]
pub fn render_command(command: &str, r: &(impl ChordResolver + ?Sized)) -> CommandText {
    if let Some(chord) = r.chord(command).filter(|c| !is_blank_id(c)) {
        return CommandText::Chord(chord);
    }
    let label = r.label(command);
    CommandText::Name(if is_blank_id(&label) {
        command.to_owned()
    } else {
        label
    })
}

/// The display text of one [`Span`].
///
/// Everything that carries its own payload renders as that payload: this
/// function decides TEXT, never style. Whether `Strong` is bold, `Code`
/// boxed or a `TopicLink` underlined is the renderer's business, and it still
/// has the `Span` in hand to decide it.
///
/// A [`Span::TopicLink`] renders as the id of the page it opens — the same
/// string the reader types into the help index and the same one `see_also`
/// shows, so a link is followable even in a rendering with no links, such as
/// piped plain text.
///
/// ```
/// use norte_help::{Availability, ChordResolver, Span, TopicId, render_span};
///
/// struct None_;
///
/// impl ChordResolver for None_ {
///     fn chord(&self, _command: &str) -> Option<String> {
///         None
///     }
///     fn label(&self, command: &str) -> String {
///         command.to_owned()
///     }
///     fn availability(&self, _command: &str) -> Availability {
///         Availability::Available
///     }
/// }
///
/// assert_eq!(render_span(&Span::Code("~/.norte".to_owned()), &None_), "~/.norte");
/// assert_eq!(
///     render_span(&Span::TopicLink(TopicId::new("copying")), &None_),
///     "copying"
/// );
/// ```
#[must_use]
pub fn render_span(span: &Span, r: &(impl ChordResolver + ?Sized)) -> String {
    match span {
        Span::Text(t) | Span::Strong(t) | Span::Emph(t) | Span::Code(t) => t.clone(),
        Span::CommandRef(c) => render_command(c, r).into_text(),
        Span::TopicLink(id) => id.to_string(),
    }
}

/// One runnable row, ready to paint: the model's [`CommandRow`] plus the two
/// things only the frontend could supply.
///
/// Composed rather than flattened, so the model type stays the model type: a
/// row IS a `CommandRow` that has been through a resolver, and a renderer
/// that only wants to dispatch it reaches for `row.command` — the same
/// dispatch key the palette sends, never painted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedRow {
    /// The command and its availability in this context.
    pub row: CommandRow,
    /// Short label. Never blank: it falls back to the command id, for the
    /// reason [`render_command`] gives.
    pub label: String,
    /// The effective chord, or `None` when nothing is bound. `Some` is always
    /// something that paints, so a renderer choosing a placeholder (the
    /// palette's `—`) only has to handle `None`.
    pub chord: Option<String>,
}

/// The runnable rows of a topic, one per entry of [`Topic::commands`], in the
/// order the front matter declares them.
///
/// Declared order and not sorted: the list is the author's running order —
/// copy, then move, then delete — and an alphabetical table would teach the
/// reader a different sequence in each language.
///
/// ```
/// use norte_help::{Availability, ChordResolver, Lang, rows_of, topic};
///
/// struct Orthodox;
///
/// impl ChordResolver for Orthodox {
///     fn chord(&self, command: &str) -> Option<String> {
///         (command == "pane.copy").then(|| "F5".to_owned())
///     }
///     fn label(&self, command: &str) -> String {
///         command.to_owned()
///     }
///     fn availability(&self, _command: &str) -> Availability {
///         Availability::Available
///     }
/// }
///
/// let copying = topic(Lang::En, "copying").expect("the `copying` topic");
/// let rows = rows_of(copying, &Orthodox);
/// assert_eq!(rows[0].row.command, "pane.copy");
/// assert_eq!(rows[0].chord.as_deref(), Some("F5"));
/// assert!(rows.iter().all(|r| r.row.avail.is_available()));
/// ```
#[must_use]
pub fn rows_of(topic: &Topic, r: &(impl ChordResolver + ?Sized)) -> Vec<ResolvedRow> {
    topic
        .commands
        .iter()
        .map(|command| {
            // The same fallback chain as a `{{cmd:…}}` mark, so a command
            // reads the same in the prose and in the table below it.
            let (chord, label) = match render_command(command, r) {
                CommandText::Chord(chord) => (Some(chord), r.label(command)),
                CommandText::Name(name) => (None, name),
            };
            ResolvedRow {
                row: CommandRow {
                    command: command.clone(),
                    avail: r.availability(command),
                },
                label: if is_blank_id(&label) {
                    command.clone()
                } else {
                    label
                },
                chord,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Availability, Reason, Span, TopicId};

    /// A frontend, faked: one command bound, one unavailable, and a label
    /// built from the id so a wrong lookup is visible in the assertion.
    struct Fake;

    impl ChordResolver for Fake {
        fn chord(&self, command: &str) -> Option<String> {
            match command {
                "pane.copy" => Some("F5".to_owned()),
                // A resolver that answers with something that paints
                // nothing: the render must not put it on screen.
                "pane.move" => Some("  ".to_owned()),
                _ => None,
            }
        }

        fn label(&self, command: &str) -> String {
            match command {
                // A label that paints nothing is the same defect one step
                // further down the fallback chain.
                "task.cancel" => String::new(),
                _ => format!("label of {command}"),
            }
        }

        fn availability(&self, command: &str) -> Availability {
            if command == "pane.delete-permanent" {
                Availability::Unavailable {
                    reason: Reason::ReadOnlyBackend,
                }
            } else {
                Availability::Available
            }
        }
    }

    #[test]
    fn a_bound_command_renders_the_users_own_chord() {
        assert_eq!(
            render_span(&Span::CommandRef("pane.copy".to_owned()), &Fake),
            "F5"
        );
        assert_eq!(
            render_command("pane.copy", &Fake),
            CommandText::Chord("F5".to_owned()),
            "a renderer must be able to tell a key from a name to style it"
        );
    }

    #[test]
    fn an_unbound_command_names_itself_instead_of_inventing_a_key() {
        assert_eq!(
            render_span(&Span::CommandRef("pane.rename".to_owned()), &Fake),
            "label of pane.rename",
            "with no binding the prose names the command rather than \
             claiming a key the user does not have"
        );
        assert_eq!(
            render_command("pane.rename", &Fake),
            CommandText::Name("label of pane.rename".to_owned())
        );
    }

    #[test]
    fn a_mark_never_renders_as_nothing() {
        // A gap in a sentence is worse than either honest answer: the
        // reader cannot tell a missing key from a missing word. Each step
        // of the chain falls through to the next, and the last one is the
        // command id itself, which always paints something.
        assert_eq!(
            render_span(&Span::CommandRef("pane.move".to_owned()), &Fake),
            "label of pane.move",
            "a blank chord is not a chord"
        );
        assert_eq!(
            render_span(&Span::CommandRef("task.cancel".to_owned()), &Fake),
            "task.cancel",
            "a blank label leaves the id, which is at least true"
        );
    }

    #[test]
    fn every_other_span_carries_its_own_payload() {
        for span in [
            Span::Text("plain".to_owned()),
            Span::Strong("plain".to_owned()),
            Span::Emph("plain".to_owned()),
            Span::Code("plain".to_owned()),
        ] {
            assert_eq!(render_span(&span, &Fake), "plain", "{span:?}");
        }
        assert_eq!(
            render_span(&Span::TopicLink(TopicId::new("copying")), &Fake),
            "copying",
            "a link renders as the page it opens, so the reader can find it"
        );
    }

    #[test]
    fn rows_keep_the_order_the_topic_declares_them_in() {
        let mut topic = crate::corpus::topic(crate::Lang::En, "copying")
            .expect("the `copying` topic exists")
            .clone();
        topic.commands = vec![
            "pane.move".to_owned(),
            "pane.copy".to_owned(),
            "pane.move".to_owned(),
        ];
        let rows = rows_of(&topic, &Fake);
        assert_eq!(
            rows.iter()
                .map(|r| r.row.command.as_str())
                .collect::<Vec<_>>(),
            ["pane.move", "pane.copy", "pane.move"],
            "the front matter's order is the author's running order, and a \
             repeat is a repeat"
        );
    }

    #[test]
    fn an_unavailable_row_keeps_the_reason_it_cannot_run_for() {
        let topic = crate::corpus::topic(crate::Lang::En, "copying").expect("`copying`");
        let rows = rows_of(topic, &Fake);
        let row = rows
            .iter()
            .find(|r| r.row.command == "pane.delete-permanent")
            .expect("`copying` documents `pane.delete-permanent`");
        assert_eq!(
            row.row.avail.reason(),
            Some(Reason::ReadOnlyBackend),
            "a dimmed row without its reason is a row nobody can act on"
        );
        // …and it is still a complete row: a command that cannot run now is
        // still a command the reader is learning about.
        assert_eq!(row.label, "label of pane.delete-permanent");
    }

    #[test]
    fn a_real_topic_resolves_the_rows_its_front_matter_declares() {
        // Driven by the shipped corpus rather than a synthetic topic: the
        // join is only worth anything if it works on the pages we ship.
        let topic = crate::corpus::topic(crate::Lang::En, "copying").expect("`copying`");
        let rows = rows_of(topic, &Fake);
        assert_eq!(
            rows.iter()
                .map(|r| r.row.command.clone())
                .collect::<Vec<_>>(),
            topic.commands,
            "one row per declared command, in the declared order"
        );
        let copy = rows
            .iter()
            .find(|r| r.row.command == "pane.copy")
            .expect("`copying` documents `pane.copy`");
        assert_eq!(copy.chord.as_deref(), Some("F5"));
        assert_eq!(copy.label, "label of pane.copy");
        assert!(copy.row.avail.is_available());

        let unbound = rows
            .iter()
            .find(|r| r.row.command == "task.cancel")
            .expect("`copying` documents `task.cancel`");
        assert_eq!(unbound.chord, None, "no key bound, and none invented");
        assert_eq!(
            unbound.label, "task.cancel",
            "a blank label falls back to the id here too, so no row paints \
             an empty cell"
        );
    }

    #[test]
    fn a_resolver_reaches_the_helpers_through_a_reference_of_any_kind() {
        // `&dyn` and `&T` alike: a frontend that keeps its resolver behind a
        // trait object (a viewer that swaps context, say) must not have to
        // clone it or reimplement the join.
        let boxed: Box<dyn ChordResolver> = Box::new(Fake);
        let as_dyn: &dyn ChordResolver = boxed.as_ref();
        assert_eq!(
            render_span(&Span::CommandRef("pane.copy".to_owned()), as_dyn),
            "F5"
        );
        let topic = crate::corpus::topic(crate::Lang::En, "copying").expect("`copying`");
        assert_eq!(rows_of(topic, as_dyn).len(), topic.commands.len());
    }
}
