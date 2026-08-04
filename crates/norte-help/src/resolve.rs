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
    ///
    /// # Which screen the chord comes from
    ///
    /// There is no context parameter, and that is a decision rather than an
    /// omission. A frontend has several keymaps at once — the TUI builds an
    /// `Effective` per `Screen` (browse, viewer, dialog) — so an implementor
    /// must pick one. The rule is: resolve a command in the screen THAT
    /// COMMAND lives in, never in the screen the reader happens to be on.
    /// `{{cmd:dialog.approve}}` inside a page about copying must show the key
    /// that approves, which only exists in the dialog keymap; a context
    /// supplied by the caller would answer about the PAGE, which is the wrong
    /// question, and every frontend would pass the wrong value in good faith.
    ///
    /// A command bound in one screen resolves exactly. A `[global]` command is
    /// in all of them with the same chord, so it resolves exactly too. What
    /// remains is a command bound in more than one screen with DIFFERENT
    /// chords: take the first in the frontend's own precedence order — the
    /// order `first_chord` already walks — and know that the page then shows a
    /// key that works in one screen and not another. No command in the TUI is
    /// in that state today; if one ever is, it is the keymap that wants
    /// fixing, not this rule.
    fn chord(&self, command: &str) -> Option<String>;

    /// A SHORT label for the command — what the reader sees when there is no
    /// key to show, and the second column of a runnable row. In the TUI it is
    /// the Fluent catalogue (`help-cmd-*`), so it arrives translated.
    ///
    /// **If you have no label for the command, return an EMPTY string. Never
    /// return your lookup key.** The fallback chain below then names the
    /// command (`dialog.approve`), which is at least something the reader can
    /// search for. This is not a hypothetical: `norte_i18n::t` returns the
    /// requested id when the catalogue has no entry, so the obvious one-liner
    /// — `t(&help_id(command))` — silently paints `help-cmd-dialog-approve` at
    /// the reader and defeats the chain by never being blank. An implementor
    /// wanting that behaviour must check the catalogue for a miss and return
    /// `String::new()`.
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
///
/// Deliberately NOT `#[non_exhaustive]`, unlike [`crate::Reason`] and for the
/// same kind of reason [`crate::Issue`] is not: the question it answers —
/// "does the reader have a key for this, or not?" — has exactly two answers,
/// and a renderer that did not handle both could not paint the mark at all. A
/// third variant would be a change of meaning, not an extension, and should
/// break every `match` that exists. Renderers that do not care about the
/// distinction call [`CommandText::text`] and never match.
///
/// ```
/// use norte_help::CommandText;
///
/// // The two answers, and a renderer choosing a style from them.
/// let key = CommandText::Chord("F5".to_owned());
/// let name = CommandText::Name("copy files".to_owned());
/// let style = |c: &CommandText| if c.is_chord() { "key" } else { "prose" };
/// assert_eq!((style(&key), key.text()), ("key", "F5"));
/// assert_eq!((style(&name), name.text()), ("prose", "copy files"));
/// ```
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
/// # The id is painted, and this crate does not mask it
///
/// The last step of the chain puts a COMMAND ID on screen, and nothing here
/// masks it. Safe for the two ways an id can arrive, for two different
/// reasons:
///
/// - From a plugin topic, `parse_untrusted` has already refused any id it
///   could not paint (`is_own_command` rejects terminal hazards, `U+FFFD` and
///   invisibles outright, and never rewrites), so what reaches here is clean
///   by construction.
/// - From the BUILT-IN corpus, it is not: trusted mode does not charset-check
///   a `{{cmd:…}}` id, and `parse.rs` pins that `{{cmd:\u{202E}fs.copy}}`
///   survives with its bytes intact. What makes it safe is the documentation
///   gate (`norte-tui/tests/help_gate.rs`): every id the corpus names is
///   cross-checked BYTE-EXACTLY against the frontend's command vocabulary, so
///   a spoofed id fails the build as `Issue::UnknownCommand` — it cannot ship.
///
/// That second invariant lives in another crate's test suite, which is why it
/// is written down here, next to the code that relies on it. A frontend
/// rendering a corpus that has NOT been through that gate (a third-party topic
/// set, say) must mask the result itself.
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
    match chord_or_none(command, r) {
        Some(chord) => CommandText::Chord(chord),
        None => CommandText::Name(label_or_id(command, r)),
    }
}

/// Step one of the chain: the effective chord, unless it paints nothing.
///
/// Shared by [`render_command`] and [`rows_of`] rather than written twice, so
/// a mark and the row below it can never disagree about whether the user has
/// a key.
fn chord_or_none(command: &str, r: &(impl ChordResolver + ?Sized)) -> Option<String> {
    r.chord(command).filter(|c| !is_blank_id(c))
}

/// Steps two and three: the resolver's short label, or the command id when
/// there is none to paint. See [`ChordResolver::label`] — a resolver with no
/// entry must return blank, not its lookup key.
fn label_or_id(command: &str, r: &(impl ChordResolver + ?Sized)) -> String {
    let label = r.label(command);
    if is_blank_id(&label) {
        command.to_owned()
    } else {
        label
    }
}

/// The display text of one [`Span`] — the PLAIN-TEXT path.
///
/// Everything that carries its own payload renders as that payload: this
/// function decides TEXT, never style. Whether `Strong` is bold, `Code`
/// boxed or a `TopicLink` underlined is the renderer's business, and it still
/// has the `Span` in hand to decide it.
///
/// It does flatten away the one distinction a STYLING renderer needs, though:
/// what comes back for a `{{cmd:…}}` no longer says whether it is a key or a
/// name. A renderer that paints keys differently from prose — which is every
/// renderer with a key style — calls [`render_command`] for that span instead
/// and matches on the [`CommandText`]. This function is for the renderings
/// with no styles to choose between: `norte help` piped to a file, a
/// clipboard copy, an assertion in a test.
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
///
/// ```
/// use norte_help::{Availability, ChordResolver, Lang, Reason, rows_of, topic};
///
/// struct ReadOnly;
///
/// impl ChordResolver for ReadOnly {
///     fn chord(&self, command: &str) -> Option<String> {
///         (command == "pane.copy").then(|| "F5".to_owned())
///     }
///     fn label(&self, command: &str) -> String {
///         command.to_owned()
///     }
///     fn availability(&self, _command: &str) -> Availability {
///         Availability::Unavailable { reason: Reason::ReadOnlyBackend }
///     }
/// }
///
/// let copying = topic(Lang::En, "copying").expect("the `copying` topic");
/// let row = &rows_of(copying, &ReadOnly)[0];
/// // Everything a renderer needs for one line: what to dispatch, what to
/// // call it, which key to show, and why it is dimmed.
/// assert_eq!(row.row.command, "pane.copy");
/// assert_eq!(row.label, "pane.copy");
/// assert_eq!(row.chord.as_deref(), Some("F5"));
/// assert_eq!(row.row.avail.reason(), Some(Reason::ReadOnlyBackend));
/// ```
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
        .map(|command| ResolvedRow {
            row: CommandRow {
                command: command.clone(),
                avail: r.availability(command),
            },
            // The same two steps a `{{cmd:…}}` mark takes, through the same
            // helpers, so a command reads the same in the prose and in the
            // table below it. A row shows BOTH — the chord column and the
            // label column — where a mark shows whichever it has.
            label: label_or_id(command, r),
            chord: chord_or_none(command, r),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Availability, Block, Reason, Span, TopicId};

    /// A frontend, faked: one command bound, one unavailable, and a label
    /// built from the id so a wrong lookup is visible in the assertion.
    struct Fake;

    impl ChordResolver for Fake {
        fn chord(&self, command: &str) -> Option<String> {
            match command {
                "pane.copy" => Some("F5".to_owned()),
                "pane.delete" => Some("F8".to_owned()),
                // A resolver that answers with something that paints
                // nothing: the render must not put it on screen.
                "pane.move" => Some("  ".to_owned()),
                _ => None,
            }
        }

        fn label(&self, command: &str) -> String {
            match command {
                // A label that paints nothing is the same defect one step
                // further down the fallback chain. `pane.delete` has one
                // missing while HAVING a key: the two columns of a row fall
                // back independently.
                "task.cancel" | "pane.delete" => String::new(),
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

    /// A frontend that knows nothing: no keymap and no catalogue. Every
    /// question falls through to the last step of the chain.
    struct Bare;

    impl ChordResolver for Bare {
        fn chord(&self, _command: &str) -> Option<String> {
            None
        }

        fn label(&self, _command: &str) -> String {
            String::new()
        }

        fn availability(&self, _command: &str) -> Availability {
            Availability::Available
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
    fn a_row_with_a_key_but_no_label_still_names_its_command() {
        // The two columns fall back INDEPENDENTLY: the chord is there, and
        // the label column must not be an empty cell just because the
        // catalogue has a hole. (`render_command` cannot cover this on its
        // own — it stops at the chord and never looks at the label.)
        let topic = crate::corpus::topic(crate::Lang::En, "copying").expect("`copying`");
        let row = rows_of(topic, &Fake)
            .into_iter()
            .find(|r| r.row.command == "pane.delete")
            .expect("`copying` documents `pane.delete`");
        assert_eq!(row.chord.as_deref(), Some("F8"));
        assert_eq!(row.label, "pane.delete");
    }

    #[test]
    fn a_hazard_in_a_trusted_command_id_reaches_the_renderer_unmasked() {
        // The invariant `render_command`'s rustdoc states, made visible where
        // it is RELIED ON. Trusted mode does not charset-check a `{{cmd:…}}`
        // id, so a bidi override in the built-in corpus arrives here intact —
        // this module masks nothing. What keeps it off a screen is the
        // documentation gate in `norte-tui`, which compares the id against
        // the command vocabulary byte-exactly and fails the build.
        //
        // Driven through the real path (corpus text → parser → span → render)
        // rather than a hand-built `Span`: the claim is about what the
        // PARSER hands over, and the fixture is the canonical one.
        let (_, line) = norte_testkit::corpus::hostile_names()
            .into_iter()
            .map(|n| (n.id.clone(), String::from_utf8_lossy(&n.bytes).into_owned()))
            .find(|(id, _)| id == "cmd_mark_bidi_payload")
            .expect("the fixture lives in the canonical corpus");
        let doc = format!("+++\nid = \"t\"\ntitle = \"T\"\n+++\n{line}\n");
        let parsed = crate::parse::parse_trusted(&doc).expect("a valid topic");
        let Some(Block::Paragraph(spans)) = parsed.topic.blocks.first() else {
            panic!("one paragraph: {:?}", parsed.topic.blocks);
        };
        let [Span::CommandRef(id)] = spans.as_slice() else {
            panic!("one command reference: {spans:?}");
        };

        // `Bare` knows nothing about the spoofed id — which is what a real
        // resolver is, since the id matches no command: the chain runs all
        // the way to its last step, the id itself.
        let rendered = render_span(&spans[0], &Bare);
        assert_eq!(
            rendered.as_bytes(),
            id.as_bytes(),
            "no key is bound to a spoofed id, so the chain reaches the id \
             itself — and hands it over byte for byte"
        );
        assert!(
            rendered.contains('\u{202E}'),
            "this crate does not mask; the gate is what refuses to ship it"
        );
        assert_ne!(id.as_str(), "pane.copy", "it never passes for a command");
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
