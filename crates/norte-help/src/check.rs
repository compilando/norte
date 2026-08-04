//! Corpus integrity checks: they return DATA, never `panic!`.
//!
//! Two consumers, and the shape follows from having both. The test suite
//! treats a non-empty list as a build failure, which is what keeps a broken
//! link or an undocumented command out of a release. `norte doctor` (phase
//! H3g) shows the SAME findings to a user, who is holding a binary that has
//! already shipped and for whom a panic would be the least useful possible
//! report. One set of checks, therefore, returning values.
//!
//! The command and context checks come in two forms: a `check_*_in` that
//! takes the topics as an argument and holds all the logic, and a `check_*`
//! wrapper that feeds it the embedded corpus. The split is not decoration — it
//! is what lets the tests drive a check with a DELIBERATELY broken corpus (the
//! only honest way to prove a check fires) instead of corrupting the one we
//! ship, and it is what will let `norte doctor` run the same checks over a
//! plugin's topics.
//!
//! [`check_corpus`] does not follow that shape, and the asymmetry is real
//! rather than an oversight: its pure form is [`check_locales`], which takes
//! ALL the locales at once because parity is a claim about a set of corpora,
//! not about one. There is nothing sensible for a `check_corpus_in(lang,
//! topics)` to check.

use std::collections::BTreeSet;
use std::fmt;

use norte_i18n::Lang;

use crate::model::{Block, Span, Topic};
use crate::parse::{CMD_OPEN, LINK_OPEN};

/// The locales the embedded corpus ships, and the input [`check_corpus`]
/// builds from.
///
/// This is a hand-written list, so the anchor below is what makes a new locale
/// break the build HERE instead of being silently left out of every parity
/// check. Nothing else points at this file: `corpus::sources` is compile-forced
/// by its own `match`, and a passing parity suite over `{En, Es}` says nothing
/// about `Fr`.
const LOCALES: [Lang; 2] = [Lang::En, Lang::Es];

/// Compile-time anchor for [`LOCALES`] and for the parity tests below: a
/// wildcard-free `match` over every [`Lang`]. Adding a variant fails to compile
/// here, next to the list that has to grow with it.
const _: fn(Lang) = |lang| match lang {
    Lang::En | Lang::Es => (),
};

/// The locale [`check_commands`] and [`check_contexts`] read.
///
/// Reading ONE locale is deliberate, and it is only sound because parity is
/// machine-enforced. The tests at the bottom of THIS file are what enforce it,
/// deliberately placed next to the constant they justify: they compare the
/// `commands` front matter, the `{{cmd:…}}` marks of the prose, and the
/// `context` declarations, each as its OWN set. Separately and not merged,
/// because a command that moves from the front matter into the prose in one
/// locale keeps the union identical while changing what a reader can run.
///
/// With that held, checking the other locale could only ever report the same
/// finding twice, in a list a human reads.
///
/// The one thing this must NOT become is a silent assumption. If those parity
/// tests are ever removed, this constant is where the loss lands: the check
/// would keep passing while the Spanish reader gets rows the English one does
/// not. Hence a named constant and not a hard-coded `Lang::En` in two bodies.
const VOCABULARY_LOCALE: Lang = Lang::En;

/// Where an inert mark was found — a block whose text the parser never cuts
/// into spans, so a mark inside it can only ever render literally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inert {
    /// In a heading.
    Heading,
    /// In a table cell, header or body.
    TableCell,
}

impl Inert {
    /// The word [`Issue`]'s `Display` uses for this place.
    fn label(self) -> &'static str {
        match self {
            Self::Heading => "heading",
            Self::TableCell => "table cell",
        }
    }
}

/// Which of the corpus' two live marks was written somewhere inert.
///
/// The corpus has exactly two, and they fail identically: a `{{cmd:…}}` the
/// reader cannot press, a `[[…]]` the reader cannot follow. Reporting them
/// through one finding with a kind — rather than one finding each, or worse,
/// only the first one anybody thought of — is what keeps the two from drifting
/// apart again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// A command reference, `{{cmd:…}}`.
    Command,
    /// A topic link, `[[…]]`.
    TopicLink,
}

impl Mark {
    /// The opener this mark is recognised by, straight from the parser.
    fn opener(self) -> &'static str {
        match self {
            Self::Command => CMD_OPEN,
            Self::TopicLink => LINK_OPEN,
        }
    }

    /// The words [`Issue`]'s `Display` uses for this mark.
    fn label(self) -> &'static str {
        match self {
            Self::Command => "command mark",
            Self::TopicLink => "topic link",
        }
    }
}

/// Both marks, in the order findings are reported for one piece of text.
const MARKS: [Mark; 2] = [Mark::Command, Mark::TopicLink];

/// Why an allowlist entry is dead weight. Both causes mean the same repair —
/// delete the line — but not the same story, and a backlog whose entries
/// cannot be told apart stops being read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stale {
    /// The command is no longer in the vocabulary at all: it was renamed or
    /// removed, and the entry now silences nothing.
    LeftTheVocabulary,
    /// The command is documented now. The debt was paid; the entry outlived it.
    NowDocumented,
}

impl Stale {
    /// The clause [`Issue`]'s `Display` uses for this cause.
    fn label(self) -> &'static str {
        match self {
            Self::LeftTheVocabulary => "is not in the vocabulary",
            Self::NowDocumented => "is already documented",
        }
    }
}

/// A problem found in a corpus.
///
/// Deliberately NOT `#[non_exhaustive]`, against the convention the rest of
/// this crate follows for enums expected to grow. The reason is the second
/// consumer: `norte doctor` has to RENDER every variant, and a diagnostic
/// nobody prints is worse than no diagnostic at all.
///
/// That reasoning only holds if something FORCES the rendering, and a renderer
/// in another crate cannot be forced — it is free to write `_ => {}`. So the
/// forcing function lives here: the `Display` impl below has no wildcard arm,
/// which means a new variant fails to compile in THIS crate, at the moment it
/// is added, and every consumer inherits a rendering it did not have to write.
///
/// Every variant carries the locale it applies to. A corpus is per-locale from
/// the reader's point of view, so "a dangling link" without a language is a
/// report that cannot be acted on: the two `.md` files are edited separately.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Issue {
    /// A topic exists in one locale and is missing from another.
    MissingLocale {
        /// Id of the topic.
        id: String,
        /// The locale it is MISSING from.
        lang: Lang,
    },
    /// Two topics share an id within one locale.
    DuplicateTopic {
        /// The repeated id.
        id: String,
        /// Locale the collision is in.
        lang: Lang,
    },
    /// A `[[link]]` in a body, or a `see_also` entry, points at a topic that
    /// does not exist.
    DanglingLink {
        /// Topic holding the link.
        from: String,
        /// Id it points at.
        to: String,
        /// Locale the link is in.
        lang: Lang,
    },
    /// The corpus mentions a command that is not in the frontend's vocabulary
    /// — prose about a key that does nothing.
    UnknownCommand {
        /// Topic that mentions it.
        topic: String,
        /// The command as the corpus spells it.
        command: String,
        /// Locale the mention is in.
        lang: Lang,
    },
    /// A command of the vocabulary that no topic documents.
    UndocumentedCommand {
        /// The undocumented command.
        command: String,
        /// Locale whose corpus was searched.
        lang: Lang,
    },
    /// A topic declares a UI context the frontend does not have, so F1 in that
    /// context will never open it.
    UnknownContext {
        /// Topic that declares it.
        topic: String,
        /// The context as the corpus spells it.
        context: String,
        /// Locale the declaration is in.
        lang: Lang,
    },
    /// Two topics claim the same context. F1 there can only open one of them,
    /// and which one would depend on corpus order.
    DuplicateContext {
        /// The disputed context.
        context: String,
        /// Topic that claimed it first, in corpus order.
        first: String,
        /// Topic that claims it again.
        second: String,
        /// Locale the collision is in.
        lang: Lang,
    },
    /// A live mark written where the parser cannot read it: a heading, or a
    /// table cell. It renders as literal braces or brackets and does nothing.
    ///
    /// It is its own finding, and not a mention or a link, because it is
    /// invisible to every other check by construction. A `{{cmd:…}}` there is
    /// neither documented (there is nothing to press) nor mentioned (there is
    /// no `CommandRef`), so a bogus id typed into a table passes the vocabulary
    /// cross-check in SILENCE; a `[[…]]` there is not in `links`, so it never
    /// dangles however wrong its target. Both are the same defect, which is why
    /// they are one variant with a [`Mark`] rather than two that can drift.
    ///
    /// A keys-and-answers table is the single most natural place for an author
    /// to type a chord mark, and "the reader can see the brackets" is the
    /// mitigation this crate refuses to rely on anywhere else.
    InertMark {
        /// Topic holding it.
        topic: String,
        /// Which mark it is.
        mark: Mark,
        /// What kind of block it landed in.
        block: Inert,
        /// The offending text, whole, so it can be grepped for. Never a
        /// terminal hazard: a built-in topic is our own text, and a plugin's
        /// headings and cells are masked by `parse_untrusted`.
        text: String,
        /// Locale the mark is in.
        lang: Lang,
    },
    /// An allowlist entry that silences nothing.
    ///
    /// The documentation gate is built on a SHRINKING allowlist, so an entry
    /// that has stopped corresponding to real debt is how a backlog quietly
    /// stops being one.
    StaleAllowEntry {
        /// The command the allowlist names.
        command: String,
        /// Why the entry is dead weight.
        reason: Stale,
        /// Locale whose corpus was searched.
        lang: Lang,
    },
}

impl fmt::Display for Issue {
    /// One line per finding, for `norte doctor` and for a test failure alike.
    ///
    /// The `match` has NO wildcard arm, and that is the point: see the note on
    /// [`Issue`]. The text is hard-coded English rather than Fluent, like
    /// [`crate::ParseError`], because these are diagnostics about a corpus
    /// someone is AUTHORING, not strings on a user's screen.
    ///
    /// ```
    /// use norte_help::{Issue, Lang};
    ///
    /// let issue = Issue::UndocumentedCommand {
    ///     command: "app.quit".to_owned(),
    ///     lang: Lang::En,
    /// };
    /// assert_eq!(issue.to_string(), "[En] no topic documents `app.quit`");
    /// ```
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingLocale { id, lang } => {
                write!(f, "[{lang:?}] topic `{id}` is missing from this locale")
            }
            Self::DuplicateTopic { id, lang } => {
                write!(f, "[{lang:?}] topic `{id}` is declared twice")
            }
            Self::DanglingLink { from, to, lang } => {
                write!(
                    f,
                    "[{lang:?}] `{from}` links to `{to}`, which is not a topic"
                )
            }
            Self::UnknownCommand {
                topic,
                command,
                lang,
            } => write!(
                f,
                "[{lang:?}] `{topic}` mentions `{command}`, which is not a command"
            ),
            Self::UndocumentedCommand { command, lang } => {
                write!(f, "[{lang:?}] no topic documents `{command}`")
            }
            Self::UnknownContext {
                topic,
                context,
                lang,
            } => write!(
                f,
                "[{lang:?}] `{topic}` claims the context `{context}`, which the UI does not have"
            ),
            Self::DuplicateContext {
                context,
                first,
                second,
                lang,
            } => write!(
                f,
                "[{lang:?}] `{first}` and `{second}` both claim the context `{context}`"
            ),
            Self::InertMark {
                topic,
                mark,
                block,
                text,
                lang,
            } => write!(
                f,
                "[{lang:?}] `{topic}` has a {} in a {}, where it renders literally: {text}",
                mark.label(),
                block.label()
            ),
            Self::StaleAllowEntry {
                command,
                reason,
                lang,
            } => write!(
                f,
                "[{lang:?}] the allowlist names `{command}`, which {}",
                reason.label()
            ),
        }
    }
}

/// Every [`Span`] in a topic's body, in reading order.
///
/// The `match` is exhaustive on purpose: a block kind skipped by a `_` arm is
/// a place a live mark could hide, and this walk is what the command and link
/// checks are built on.
///
/// [`Block::Heading`], [`Block::Code`] and [`Block::Table`] carry `String`s
/// rather than spans, so they yield nothing here — and that is a fact about
/// the PARSER, not an omission: it never builds a `CommandRef` or a
/// `TopicLink` inside them. A `{{cmd:…}}` typed into a table cell is inert
/// text that renders literally; it is not a mention, because there is nothing
/// for a reader to press. It is not IGNORED either: [`inert_marks`] reports it
/// as its own finding, because otherwise it is invisible to every check here.
fn spans_of(t: &Topic) -> Vec<&Span> {
    let mut out = Vec::new();
    for block in &t.blocks {
        match block {
            Block::Paragraph(spans) | Block::Callout { spans, .. } => out.extend(spans),
            Block::Bullets(items) => {
                for item in items {
                    out.extend(item);
                }
            }
            Block::Heading { .. } | Block::Code { .. } | Block::Table { .. } => {}
        }
    }
    out
}

/// The commands a topic's `commands` front matter declares: its runnable rows.
///
/// Sorted and deduplicated, for the reason [`links`] is.
fn header_commands(t: &Topic) -> BTreeSet<String> {
    t.commands.iter().cloned().collect()
}

/// The commands a topic's PROSE names, through `{{cmd:…}}` marks.
fn prose_commands(t: &Topic) -> BTreeSet<String> {
    spans_of(t)
        .into_iter()
        .filter_map(|s| match s {
            Span::CommandRef(c) => Some(c.clone()),
            _ => None,
        })
        .collect()
}

/// The commands a topic mentions: both doors, unioned.
///
/// They are not the same door. `commands` is the topic's runnable rows; a
/// `{{cmd:…}}` is the prose naming a key. A command referenced only in prose is
/// still documented, and a command listed only in the front matter is still a
/// claim that the command exists — so for the vocabulary cross-check, the union
/// is what "documented" means.
///
/// The parity tests do NOT use this: a union hides a command moving from one
/// door to the other, which is a real change to what a reader can run.
fn mentions(t: &Topic) -> BTreeSet<String> {
    let mut out = header_commands(t);
    out.extend(prose_commands(t));
    out
}

/// Every inert mark of a topic — `{{cmd:…}}` and `[[…]]` alike, written where
/// the parser never looks for one. See [`Issue::InertMark`].
///
/// BOTH marks, from one walk over one list of openers. The alternative,
/// checking the mark somebody happened to think of first, ships a checker that
/// catches one of two identical defects — and a reader of this module would
/// reasonably assume otherwise.
///
/// [`Block::Code`] is deliberately EXCLUDED, for both. A fence is where one
/// documents the syntax itself, so a literal mark there is the author saying
/// exactly what they meant; flagging it would make the one legitimate use of
/// the text unwritable. The other two block kinds have no such use: nobody
/// writes a mark into a heading or a table cell on purpose.
///
/// Detection is by OPENER, sharing the parser's own constants rather than
/// spelling them again. A mark that is unclosed, blank or otherwise refused is
/// still inert text with braces or brackets in it, and still worth reporting:
/// the author meant a key, or a jump, either way.
fn inert_marks(t: &Topic) -> Vec<(Mark, Inert, String)> {
    /// One piece of text that the parser will never cut into spans.
    fn scan(out: &mut Vec<(Mark, Inert, String)>, block: Inert, text: &str) {
        for mark in MARKS {
            if text.contains(mark.opener()) {
                out.push((mark, block, text.to_owned()));
            }
        }
    }

    let mut out = Vec::new();
    for block in &t.blocks {
        match block {
            Block::Heading { text, .. } => scan(&mut out, Inert::Heading, text),
            Block::Table { header, rows } => {
                for cell in header.iter().chain(rows.iter().flatten()) {
                    scan(&mut out, Inert::TableCell, cell);
                }
            }
            Block::Code { .. }
            | Block::Paragraph(_)
            | Block::Bullets(_)
            | Block::Callout { .. } => {}
        }
    }
    out
}

/// Every topic id a topic points at: `see_also` plus the `[[links]]` of its
/// body. Sorted and deduplicated: a topic linked three times is one link, and
/// the order a `Vec` walk happens to produce is not something a diff should be
/// sensitive to.
///
/// Only LIVE links, so a `[[…]]` typed into a heading or a table cell is not
/// here — and cannot dangle however wrong its target. [`inert_marks`] is what
/// catches those.
fn links(t: &Topic) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = t.see_also.iter().map(ToString::to_string).collect();
    out.extend(spans_of(t).into_iter().filter_map(|s| match s {
        Span::TopicLink(id) => Some(id.to_string()),
        _ => None,
    }));
    out
}

/// Checks a set of locales against each other: topic parity, unique ids, links
/// that resolve, and marks written where they render literally. The logic
/// behind [`check_corpus`].
///
/// Everything here is checkable with no external vocabulary — which is why the
/// inert-mark check lives in this function and not in [`check_commands_in`],
/// even for `{{cmd:…}}`: whether a mark can be pressed does not depend on what
/// the frontend knows.
///
/// Ids are compared BYTE-EXACTLY, like everywhere else in this crate
/// ([`crate::TopicId`] never normalises). A check that folded case or trimmed
/// would resolve links the renderer will not.
///
/// ```
/// use norte_help::{Issue, Lang, Origin, Topic, TopicId, check_locales};
///
/// let en = Topic {
///     id: TopicId::new("index"),
///     title: "Index".to_owned(),
///     tags: Vec::new(),
///     see_also: vec![TopicId::new("nowhere")],
///     commands: Vec::new(),
///     context: Vec::new(),
///     blocks: Vec::new(),
///     origin: Origin::BuiltIn,
/// };
/// assert_eq!(
///     check_locales(&[(Lang::En, &[en])]),
///     vec![Issue::DanglingLink {
///         from: "index".to_owned(),
///         to: "nowhere".to_owned(),
///         lang: Lang::En,
///     }]
/// );
/// ```
#[must_use]
pub fn check_locales(locales: &[(Lang, &[Topic])]) -> Vec<Issue> {
    let mut issues = Vec::new();

    // Parity against the UNION rather than pairwise: with three locales, a
    // topic present in one and absent from two must be reported twice, once
    // per locale that has to gain the file.
    let mut union: BTreeSet<&str> = BTreeSet::new();
    for &(_, ts) in locales {
        union.extend(ts.iter().map(|t| t.id.as_str()));
    }
    for &(lang, ts) in locales {
        let ids: BTreeSet<&str> = ts.iter().map(|t| t.id.as_str()).collect();
        for id in union.difference(&ids) {
            issues.push(Issue::MissingLocale {
                id: (*id).to_owned(),
                lang,
            });
        }
    }

    for &(lang, ts) in locales {
        // The set built here is also the set the links resolve against, so a
        // duplicate id cannot make a link look resolvable in one pass and not
        // in another.
        let mut ids: BTreeSet<&str> = BTreeSet::new();
        for t in ts {
            if !ids.insert(t.id.as_str()) {
                issues.push(Issue::DuplicateTopic {
                    id: t.id.to_string(),
                    lang,
                });
            }
        }
        for t in ts {
            for to in links(t) {
                if !ids.contains(to.as_str()) {
                    issues.push(Issue::DanglingLink {
                        from: t.id.to_string(),
                        to,
                        lang,
                    });
                }
            }
            for (mark, block, text) in inert_marks(t) {
                issues.push(Issue::InertMark {
                    topic: t.id.to_string(),
                    mark,
                    block,
                    text,
                    lang,
                });
            }
        }
    }
    issues
}

/// Checks the EMBEDDED corpus: locale parity, unique ids, links that resolve,
/// no inert marks. Needs no external vocabulary, so it is the one check that
/// can run anywhere.
///
/// ```
/// use norte_help::check_corpus;
///
/// assert_eq!(check_corpus(), Vec::new(), "the shipped corpus is consistent");
/// ```
///
/// # Panics
/// If an embedded topic is malformed; see [`crate::topics`]. That cannot
/// happen in a binary whose test suite ran.
#[must_use]
pub fn check_corpus() -> Vec<Issue> {
    let locales: Vec<(Lang, &'static [Topic])> = LOCALES
        .iter()
        .map(|&lang| (lang, crate::corpus::topics(lang)))
        .collect();
    check_locales(&locales)
}

/// Crosses a set of topics with a frontend's command vocabulary, in both
/// directions. The logic behind [`check_commands`].
///
/// - Every command the topics MENTION must be in `known`, or the prose
///   promises a key that does nothing.
/// - Every command in `known` must be mentioned by some topic, unless `allow`
///   lists it. The allowlist is an explicit, enumerated debt — a command is
///   silenced by name, one line per name, so the list itself is the backlog.
/// - Every entry of `allow` must still be silencing something, or the backlog
///   is quietly no longer a backlog. See [`Issue::StaleAllowEntry`].
///
/// ```
/// use norte_help::{Issue, Lang, Origin, Topic, TopicId, check_commands_in};
///
/// let t = Topic {
///     id: TopicId::new("copying").clone(),
///     title: "Copying".to_owned(),
///     tags: Vec::new(),
///     see_also: Vec::new(),
///     commands: vec!["pane.copy".to_owned()],
///     context: Vec::new(),
///     blocks: Vec::new(),
///     origin: Origin::BuiltIn,
/// };
/// let topics = [t];
///
/// assert_eq!(check_commands_in(Lang::En, &topics, &["pane.copy"], &[]), Vec::new());
/// assert_eq!(
///     check_commands_in(Lang::En, &topics, &["pane.copy", "app.quit"], &[]),
///     vec![Issue::UndocumentedCommand {
///         command: "app.quit".to_owned(),
///         lang: Lang::En,
///     }]
/// );
/// // …and the allowlist silences exactly that one.
/// assert_eq!(
///     check_commands_in(Lang::En, &topics, &["pane.copy", "app.quit"], &["app.quit"]),
///     Vec::new()
/// );
/// ```
#[must_use]
pub fn check_commands_in(
    lang: Lang,
    topics: &[Topic],
    known: &[&str],
    allow: &[&str],
) -> Vec<Issue> {
    let known_set: BTreeSet<&str> = known.iter().copied().collect();
    let allow_set: BTreeSet<&str> = allow.iter().copied().collect();
    let mut issues = Vec::new();
    let mut documented: BTreeSet<String> = BTreeSet::new();

    for t in topics {
        for command in mentions(t) {
            if known_set.contains(command.as_str()) {
                documented.insert(command);
            } else {
                issues.push(Issue::UnknownCommand {
                    topic: t.id.to_string(),
                    command,
                    lang,
                });
            }
        }
    }
    // In the caller's order, not sorted: `known` comes from a frontend's
    // vocabulary, whose order is the order a human wrote the commands in, and
    // that is the order the missing pages should be written in.
    for command in known {
        if !documented.contains(*command) && !allow_set.contains(command) {
            issues.push(Issue::UndocumentedCommand {
                command: (*command).to_owned(),
                lang,
            });
        }
    }
    // The allowlist is checked in the same pass, in the caller's order, for
    // the same reason: the report is a to-do list about that file.
    for command in allow {
        let reason = if !known_set.contains(command) {
            Some(Stale::LeftTheVocabulary)
        } else if documented.contains(*command) {
            Some(Stale::NowDocumented)
        } else {
            None
        };
        if let Some(reason) = reason {
            issues.push(Issue::StaleAllowEntry {
                command: (*command).to_owned(),
                reason,
                lang,
            });
        }
    }
    issues
}

/// [`check_commands_in`] over the embedded corpus, in [`VOCABULARY_LOCALE`].
///
/// ```
/// use norte_help::{Issue, check_commands};
///
/// // A vocabulary of one command the corpus never names: it comes back
/// // undocumented, once…
/// let issues = check_commands(&["no.such-command"], &[]);
/// assert_eq!(
///     issues.iter().filter(|i| matches!(i, Issue::UndocumentedCommand { .. })).count(),
///     1
/// );
/// // …and naming it in the allowlist silences it.
/// let issues = check_commands(&["no.such-command"], &["no.such-command"]);
/// assert!(!issues.iter().any(|i| matches!(i, Issue::UndocumentedCommand { .. })));
///
/// // The other direction fires PER TOPIC, so a command named by two pages is
/// // two findings: each page is a page to fix. Illustrative — the census of
/// // what the corpus documents is pinned by `DOCUMENTED` in `tests/corpus.rs`,
/// // where a drift names the command instead of moving a number.
/// assert!(issues.iter().any(|i| matches!(i, Issue::UnknownCommand { .. })));
/// ```
///
/// # Panics
/// If an embedded topic is malformed; see [`crate::topics`].
#[must_use]
pub fn check_commands(known: &[&str], allow: &[&str]) -> Vec<Issue> {
    check_commands_in(
        VOCABULARY_LOCALE,
        crate::corpus::topics(VOCABULARY_LOCALE),
        known,
        allow,
    )
}

/// Crosses the `context` declarations of a set of topics with the contexts a
/// frontend has. The logic behind [`check_contexts`].
///
/// Two failures, and they are different failures. An UNKNOWN context is a
/// topic F1 will never reach. A DUPLICATE one is a topic F1 reaches only by
/// accident of corpus order, which is worse: it works, until someone reorders
/// the table.
///
/// There is a THIRD, and it is not checked here yet: a known context that NO
/// topic claims, where F1 opens nothing at all. The spec asks for exactly one
/// topic per context, and today only `browse` has one — the viewer and the
/// dialogs have none, and this function is silent about it. Adding it needs a
/// variant on [`Issue`] (and therefore a line in its `Display`), which is
/// phase H3c's job, when F1 actually performs the lookup; `norte-tui`'s
/// `tests/help_gate.rs` carries the same note at the call site.
///
/// ```
/// use norte_help::{Issue, Lang, Origin, Topic, TopicId, check_contexts_in};
///
/// let t = Topic {
///     id: TopicId::new("panes"),
///     title: "Panes".to_owned(),
///     tags: Vec::new(),
///     see_also: Vec::new(),
///     commands: Vec::new(),
///     context: vec!["nowhere".to_owned()],
///     blocks: Vec::new(),
///     origin: Origin::BuiltIn,
/// };
/// assert_eq!(
///     check_contexts_in(Lang::En, &[t], &["browse"]),
///     vec![Issue::UnknownContext {
///         topic: "panes".to_owned(),
///         context: "nowhere".to_owned(),
///         lang: Lang::En,
///     }]
/// );
/// ```
#[must_use]
pub fn check_contexts_in(lang: Lang, topics: &[Topic], known: &[&str]) -> Vec<Issue> {
    let known_set: BTreeSet<&str> = known.iter().copied().collect();
    let mut issues = Vec::new();
    // Who claimed each context first, so the report can name BOTH topics: "a
    // duplicate context" without the other claimant sends the reader grepping.
    let mut claimed: Vec<(&str, &str)> = Vec::new();
    for t in topics {
        for context in &t.context {
            if !known_set.contains(context.as_str()) {
                issues.push(Issue::UnknownContext {
                    topic: t.id.to_string(),
                    context: context.clone(),
                    lang,
                });
            }
            if let Some((_, first)) = claimed.iter().find(|(c, _)| *c == context.as_str()) {
                issues.push(Issue::DuplicateContext {
                    context: context.clone(),
                    first: (*first).to_owned(),
                    second: t.id.to_string(),
                    lang,
                });
            } else {
                claimed.push((context.as_str(), t.id.as_str()));
            }
        }
    }
    issues
}

/// [`check_contexts_in`] over the embedded corpus, in [`VOCABULARY_LOCALE`].
///
/// ```
/// use norte_help::check_contexts;
///
/// // The frontend's `Screen`, spelled out: this crate does not depend on a
/// // frontend, so the caller supplies the vocabulary.
/// assert_eq!(check_contexts(&["browse", "viewer", "dialog"]), Vec::new());
/// ```
///
/// # Panics
/// If an embedded topic is malformed; see [`crate::topics`].
#[must_use]
pub fn check_contexts(known: &[&str]) -> Vec<Issue> {
    check_contexts_in(
        VOCABULARY_LOCALE,
        crate::corpus::topics(VOCABULARY_LOCALE),
        known,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Callout, Origin, TopicId};

    /// A minimal well-formed topic. Every negative test below builds its
    /// broken corpus from this rather than reaching for the shipped one: the
    /// way to prove a check fires is to hand it something broken, and
    /// breaking the corpus we ship to feed a test is backwards.
    fn topic(id: &str) -> Topic {
        Topic {
            id: TopicId::new(id),
            title: id.to_owned(),
            tags: Vec::new(),
            see_also: Vec::new(),
            commands: Vec::new(),
            context: Vec::new(),
            blocks: Vec::new(),
            origin: Origin::BuiltIn,
        }
    }

    #[test]
    fn a_topic_missing_from_a_locale_names_the_locale_that_lacks_it() {
        let en = [topic("index"), topic("panes")];
        let es = [topic("index")];
        assert_eq!(
            check_locales(&[(Lang::En, &en), (Lang::Es, &es)]),
            vec![Issue::MissingLocale {
                id: "panes".to_owned(),
                lang: Lang::Es,
            }],
            "the locale reported is the one that has to gain the file"
        );
    }

    #[test]
    fn a_duplicate_id_within_a_locale_is_reported_once() {
        let en = [topic("panes"), topic("panes")];
        assert_eq!(
            check_locales(&[(Lang::En, &en)]),
            vec![Issue::DuplicateTopic {
                id: "panes".to_owned(),
                lang: Lang::En,
            }]
        );
    }

    #[test]
    fn a_dangling_link_is_reported_from_see_also_and_from_the_body_alike() {
        let mut from_header = topic("index");
        from_header.see_also = vec![TopicId::new("nowhere")];

        let mut from_body = topic("index");
        from_body.blocks = vec![Block::Paragraph(vec![Span::TopicLink(TopicId::new(
            "nowhere",
        ))])];

        for t in [from_header, from_body] {
            assert_eq!(
                check_locales(&[(Lang::En, std::slice::from_ref(&t))]),
                vec![Issue::DanglingLink {
                    from: "index".to_owned(),
                    to: "nowhere".to_owned(),
                    lang: Lang::En,
                }],
                "both doors into another topic are checked: {t:?}"
            );
        }
    }

    #[test]
    fn a_link_that_resolves_only_in_another_locale_is_still_dangling() {
        // The link is resolved within ITS OWN locale. A `[[panes]]` in the
        // Spanish corpus is not saved by an English `panes.md`: the reader
        // presses it and lands nowhere.
        let en = [topic("index"), topic("panes")];
        let mut es_index = topic("index");
        es_index.see_also = vec![TopicId::new("panes")];
        let es = [es_index];

        let issues = check_locales(&[(Lang::En, &en), (Lang::Es, &es)]);
        assert!(
            issues.contains(&Issue::DanglingLink {
                from: "index".to_owned(),
                to: "panes".to_owned(),
                lang: Lang::Es,
            }),
            "issues: {issues:?}"
        );
    }

    #[test]
    fn a_command_outside_the_vocabulary_names_the_topic_that_mentions_it() {
        let mut t = topic("copying");
        t.commands = vec!["fs.copy".to_owned()];
        assert_eq!(
            check_commands_in(Lang::En, &[t], &["pane.copy"], &[]),
            vec![
                Issue::UnknownCommand {
                    topic: "copying".to_owned(),
                    command: "fs.copy".to_owned(),
                    lang: Lang::En,
                },
                Issue::UndocumentedCommand {
                    command: "pane.copy".to_owned(),
                    lang: Lang::En,
                },
            ],
            "a command nobody can dispatch, AND a real one left undocumented"
        );
    }

    #[test]
    fn an_undocumented_command_is_silenced_only_by_naming_it() {
        let known = ["pane.copy", "app.quit"];
        let mut t = topic("copying");
        t.commands = vec!["pane.copy".to_owned()];
        let topics = [t];

        assert_eq!(
            check_commands_in(Lang::En, &topics, &known, &[]),
            vec![Issue::UndocumentedCommand {
                command: "app.quit".to_owned(),
                lang: Lang::En,
            }]
        );
        assert_eq!(
            check_commands_in(Lang::En, &topics, &known, &["app.quit"]),
            Vec::new()
        );
        assert_eq!(
            check_commands_in(Lang::En, &topics, &known, &["pane.copy"]),
            vec![
                Issue::UndocumentedCommand {
                    command: "app.quit".to_owned(),
                    lang: Lang::En,
                },
                Issue::StaleAllowEntry {
                    command: "pane.copy".to_owned(),
                    reason: Stale::NowDocumented,
                    lang: Lang::En,
                },
            ],
            "allowing a command that IS documented silences nothing else, and \
             says so"
        );
    }

    #[test]
    fn an_allowlist_entry_that_silences_nothing_is_reported() {
        // The gate of task 9 is a SHRINKING allowlist, so an entry that has
        // stopped corresponding to debt is how a backlog becomes decoration.
        // Both ways an entry can rot, each named separately: the repair is the
        // same line to delete, the story is not.
        let mut t = topic("copying");
        t.commands = vec!["pane.copy".to_owned()];
        let topics = [t];

        assert_eq!(
            check_commands_in(Lang::En, &topics, &["pane.copy"], &["pane.copy"]),
            vec![Issue::StaleAllowEntry {
                command: "pane.copy".to_owned(),
                reason: Stale::NowDocumented,
                lang: Lang::En,
            }],
            "the debt was paid and the entry outlived it"
        );
        assert_eq!(
            check_commands_in(Lang::En, &topics, &["pane.copy"], &["pane.rename"]),
            vec![Issue::StaleAllowEntry {
                command: "pane.rename".to_owned(),
                reason: Stale::LeftTheVocabulary,
                lang: Lang::En,
            }],
            "the command was renamed or removed: the entry silences nothing"
        );
    }

    #[test]
    fn a_mark_counts_as_a_mention_from_a_paragraph_a_bullet_or_a_callout() {
        // The three block kinds that carry spans. A walk that missed one
        // would quietly report a documented command as undocumented, and the
        // fix for that is to write the page again — for nothing.
        let mut t = topic("selection");
        t.blocks = vec![
            Block::Paragraph(vec![Span::CommandRef("mark.toggle".to_owned())]),
            Block::Bullets(vec![vec![Span::CommandRef("mark.all".to_owned())]]),
            Block::Callout {
                kind: Callout::Tip,
                spans: vec![Span::CommandRef("mark.clear".to_owned())],
            },
        ];
        assert_eq!(
            check_commands_in(
                Lang::En,
                &[t],
                &["mark.toggle", "mark.all", "mark.clear"],
                &[]
            ),
            Vec::new(),
            "prose documents a command just as `commands` does"
        );
    }

    #[test]
    fn a_mark_typed_into_a_table_cell_or_a_heading_is_not_a_mention() {
        // Pinned because it is surprising, and because it is the parser's
        // doing rather than this module's: headings, code fences and table
        // cells hold `String`s, and no `{{cmd:…}}` inside one ever becomes a
        // `CommandRef`. Such a mark renders as literal text — there is
        // nothing for the reader to press, so counting it as documentation
        // would be a lie in the more dangerous direction.
        let mut t = topic("remote");
        t.blocks = vec![
            Block::Heading {
                level: 1,
                text: "{{cmd:pane.copy}}".to_owned(),
            },
            Block::Code {
                lang: None,
                text: "{{cmd:pane.copy}}\n".to_owned(),
            },
            Block::Table {
                header: vec!["{{cmd:pane.copy}}".to_owned()],
                rows: vec![vec!["{{cmd:pane.copy}}".to_owned()]],
            },
        ];
        assert_eq!(
            check_commands_in(Lang::En, std::slice::from_ref(&t), &["pane.copy"], &[]),
            vec![Issue::UndocumentedCommand {
                command: "pane.copy".to_owned(),
                lang: Lang::En,
            }],
            "inert text documents nothing"
        );
        // …and it is not IGNORED. The vocabulary cross-check cannot see it —
        // this is the other half of the same page.
        let inert = |block, text: &str| Issue::InertMark {
            topic: "remote".to_owned(),
            mark: Mark::Command,
            block,
            text: text.to_owned(),
            lang: Lang::En,
        };
        assert_eq!(
            check_locales(&[(Lang::En, std::slice::from_ref(&t))]),
            vec![
                inert(Inert::Heading, "{{cmd:pane.copy}}"),
                inert(Inert::TableCell, "{{cmd:pane.copy}}"),
                inert(Inert::TableCell, "{{cmd:pane.copy}}"),
            ],
            "header cell and body cell alike; the code fence is exempt"
        );
    }

    #[test]
    fn a_topic_link_typed_into_a_table_cell_or_a_heading_is_inert_the_same_way() {
        // The sibling defect, and the reason [`Issue::InertMark`] carries a
        // kind. A `[[…]]` written here is not in `links`, so it never dangles
        // however wrong its target: catching the command mark and not this one
        // would be a checker that covers one of two identical failures.
        let mut t = topic("panes");
        t.blocks = vec![
            Block::Heading {
                level: 1,
                text: "[[nowhere]]".to_owned(),
            },
            Block::Table {
                header: vec!["See".to_owned()],
                rows: vec![vec!["[[nowhere]]".to_owned()]],
            },
        ];
        let inert = |block| Issue::InertMark {
            topic: "panes".to_owned(),
            mark: Mark::TopicLink,
            block,
            text: "[[nowhere]]".to_owned(),
            lang: Lang::En,
        };
        assert_eq!(
            check_locales(&[(Lang::En, &[t])]),
            vec![inert(Inert::Heading), inert(Inert::TableCell)],
            "a link the reader cannot follow, and no `DanglingLink` to find it"
        );
        // The two kinds must be distinguishable in the report as well as in
        // the data: a doctor line that said "mark" for both would send the
        // author looking for the wrong six characters.
        assert_eq!(
            inert(Inert::TableCell).to_string(),
            "[En] `panes` has a topic link in a table cell, \
             where it renders literally: [[nowhere]]"
        );
        assert_eq!(
            Issue::InertMark {
                topic: "panes".to_owned(),
                mark: Mark::Command,
                block: Inert::Heading,
                text: "{{cmd:pane.copy}}".to_owned(),
                lang: Lang::En,
            }
            .to_string(),
            "[En] `panes` has a command mark in a heading, \
             where it renders literally: {{cmd:pane.copy}}"
        );
    }

    #[test]
    fn one_cell_holding_both_marks_reports_both() {
        let mut t = topic("panes");
        t.blocks = vec![Block::Table {
            header: vec!["press {{cmd:pane.copy}}, see [[copying]]".to_owned()],
            rows: Vec::new(),
        }];
        let text = "press {{cmd:pane.copy}}, see [[copying]]".to_owned();
        assert_eq!(
            check_locales(&[(Lang::En, &[t])]),
            vec![
                Issue::InertMark {
                    topic: "panes".to_owned(),
                    mark: Mark::Command,
                    block: Inert::TableCell,
                    text: text.clone(),
                    lang: Lang::En,
                },
                Issue::InertMark {
                    topic: "panes".to_owned(),
                    mark: Mark::TopicLink,
                    block: Inert::TableCell,
                    text,
                    lang: Lang::En,
                },
            ],
            "one scan per mark, so neither hides behind the other"
        );
    }

    #[test]
    fn neither_mark_is_flagged_inside_a_code_fence() {
        // A fence is where one documents the syntax ITSELF. Flagging it would
        // make the single legitimate use of these characters unwritable, and
        // this crate's own `parse_trusted` doctest is an example of the kind
        // of page that needs it. The exemption covers BOTH marks: there is no
        // reason it would hold for one and not the other.
        let mut t = topic("index");
        t.blocks = vec![Block::Code {
            lang: Some("markdown".to_owned()),
            text: "press {{cmd:pane.copy}} to copy, then see [[copying]]\n".to_owned(),
        }];
        assert_eq!(check_locales(&[(Lang::En, &[t])]), Vec::new());
    }

    #[test]
    fn an_unclosed_inert_mark_is_reported_too() {
        // Detection is by OPENER: a mark that is malformed, blank or refused
        // is still braces or brackets on screen where the author meant a key
        // or a jump, and still something no other check can see.
        for (mark, text) in [
            (Mark::Command, "{{cmd:pane.copy"),
            (Mark::TopicLink, "[[copying"),
        ] {
            let mut t = topic("remote");
            t.blocks = vec![Block::Table {
                header: vec!["Key".to_owned()],
                rows: vec![vec![text.to_owned()]],
            }];
            assert_eq!(
                check_locales(&[(Lang::En, &[t])]),
                vec![Issue::InertMark {
                    topic: "remote".to_owned(),
                    mark,
                    block: Inert::TableCell,
                    text: text.to_owned(),
                    lang: Lang::En,
                }]
            );
        }
    }

    #[test]
    fn every_issue_renders_a_line_that_names_its_payload() {
        // The `Display` impl is the forcing function behind the decision NOT
        // to mark `Issue` as `#[non_exhaustive]` (see the type's rustdoc), so
        // it is worth one test that every variant actually says something.
        // Built through the checks rather than by hand: a rendering nobody
        // reaches is not a rendering.
        let mut t = topic("copying");
        t.commands = vec!["fs.copy".to_owned()];
        // One unknown context, and a KNOWN one claimed twice: one finding
        // each, rather than two unknowns and a duplicate.
        t.context = vec![
            "nowhere".to_owned(),
            "browse".to_owned(),
            "browse".to_owned(),
        ];
        t.see_also = vec![TopicId::new("gone")];
        t.blocks = vec![Block::Heading {
            level: 1,
            text: "{{cmd:pane.copy}}".to_owned(),
        }];
        let dupe = topic("copying");

        let mut issues = check_locales(&[(Lang::En, &[t.clone(), dupe]), (Lang::Es, &[])]);
        issues.extend(check_commands_in(
            Lang::En,
            std::slice::from_ref(&t),
            &["pane.copy"],
            &["pane.rename"],
        ));
        issues.extend(check_contexts_in(Lang::En, &[t], &["browse"]));

        // One of each of the nine variants, and nothing rendered empty or
        // without its payload.
        assert_eq!(issues.len(), 9, "{issues:?}");
        for issue in &issues {
            let line = issue.to_string();
            assert!(
                line.starts_with("[En]") || line.starts_with("[Es]"),
                "{line}"
            );
            assert!(line.len() > "[En] ".len(), "{line}");
        }
        assert!(
            issues
                .iter()
                .any(|i| i.to_string() == "[En] no topic documents `pane.copy`")
        );
    }

    #[test]
    fn an_unknown_context_names_the_topic_that_declares_it() {
        let mut t = topic("panes");
        t.context = vec!["dialog.collision".to_owned()];
        assert_eq!(
            check_contexts_in(Lang::En, &[t], &["browse", "viewer", "dialog"]),
            vec![Issue::UnknownContext {
                topic: "panes".to_owned(),
                context: "dialog.collision".to_owned(),
                lang: Lang::En,
            }]
        );
    }

    #[test]
    fn two_topics_claiming_one_context_name_both_claimants() {
        let mut first = topic("panes");
        first.context = vec!["browse".to_owned()];
        let mut second = topic("selection");
        second.context = vec!["browse".to_owned()];
        assert_eq!(
            check_contexts_in(Lang::En, &[first, second], &["browse"]),
            vec![Issue::DuplicateContext {
                context: "browse".to_owned(),
                first: "panes".to_owned(),
                second: "selection".to_owned(),
                lang: Lang::En,
            }],
            "F1 can only open one of them, and which one is corpus order"
        );
    }

    #[test]
    fn one_topic_claiming_a_context_twice_is_also_a_collision() {
        let mut t = topic("panes");
        t.context = vec!["browse".to_owned(), "browse".to_owned()];
        assert_eq!(
            check_contexts_in(Lang::En, &[t], &["browse"]),
            vec![Issue::DuplicateContext {
                context: "browse".to_owned(),
                first: "panes".to_owned(),
                second: "panes".to_owned(),
                lang: Lang::En,
            }]
        );
    }

    // --- What makes reading ONE locale safe. These live HERE, next to the
    // constant they justify, rather than in `tests/corpus.rs`: a decision
    // resting on an assertion in another file is a decision resting on
    // nothing the next reader will find.

    /// Every locale paired with `VOCABULARY_LOCALE`'s copy of the same topic.
    /// Driven from [`LOCALES`], so a new locale is covered the moment the
    /// anchor next to that constant forces it to be added.
    fn against_the_read_locale() -> Vec<(Lang, &'static Topic, &'static Topic)> {
        let mut out = Vec::new();
        for lang in LOCALES {
            if lang == VOCABULARY_LOCALE {
                continue;
            }
            for read in crate::corpus::topics(VOCABULARY_LOCALE) {
                let other = crate::corpus::topic(lang, read.id.as_str())
                    .unwrap_or_else(|| panic!("`{}` exists in {lang:?} too", read.id));
                out.push((lang, read, other));
            }
        }
        out
    }

    #[test]
    fn every_locale_declares_the_same_commands_through_the_same_door() {
        // The two doors compared SEPARATELY, not as a union. A union is what
        // makes the interesting drift invisible: move `pane.copy` out of the
        // Spanish front matter while its prose keeps the mark and the union is
        // unchanged, yet the Spanish reader has lost a runnable row.
        for (lang, read, other) in against_the_read_locale() {
            assert_eq!(
                header_commands(read),
                header_commands(other),
                "`{}` in {lang:?}: the `commands` front matter differs, so \
                 checking only {VOCABULARY_LOCALE:?} would miss it",
                read.id
            );
            assert_eq!(
                prose_commands(read),
                prose_commands(other),
                "`{}` in {lang:?}: the `{{{{cmd:…}}}}` marks of the prose \
                 differ — the half the front-matter comparison cannot see",
                read.id
            );
        }
    }

    #[test]
    fn every_locale_declares_the_same_contexts() {
        for (lang, read, other) in against_the_read_locale() {
            assert_eq!(
                read.context, other.context,
                "`{}` in {lang:?}: F1 would open a different page",
                read.id
            );
        }
    }

    /// The same issue, restated as if it had been found in `lang`.
    ///
    /// Exhaustive on purpose, wildcard-free like the `Display` impl: a variant
    /// that fell through an `other => other` arm would keep its own locale and
    /// the comparison below would read as a parity break that is really this
    /// helper's fault.
    fn as_lang(issue: Issue, lang: Lang) -> Issue {
        match issue {
            Issue::MissingLocale { id, .. } => Issue::MissingLocale { id, lang },
            Issue::DuplicateTopic { id, .. } => Issue::DuplicateTopic { id, lang },
            Issue::DanglingLink { from, to, .. } => Issue::DanglingLink { from, to, lang },
            Issue::UnknownCommand { topic, command, .. } => Issue::UnknownCommand {
                topic,
                command,
                lang,
            },
            Issue::UndocumentedCommand { command, .. } => {
                Issue::UndocumentedCommand { command, lang }
            }
            Issue::UnknownContext { topic, context, .. } => Issue::UnknownContext {
                topic,
                context,
                lang,
            },
            Issue::DuplicateContext {
                context,
                first,
                second,
                ..
            } => Issue::DuplicateContext {
                context,
                first,
                second,
                lang,
            },
            Issue::InertMark {
                topic,
                mark,
                block,
                text,
                ..
            } => Issue::InertMark {
                topic,
                mark,
                block,
                text,
                lang,
            },
            Issue::StaleAllowEntry {
                command, reason, ..
            } => Issue::StaleAllowEntry {
                command,
                reason,
                lang,
            },
        }
    }

    #[test]
    fn checking_the_other_locale_finds_exactly_the_same_issues() {
        // The parity above is the argument; this is the conclusion, asserted
        // end to end. The two runs differ only in the `lang` each issue
        // carries, so `VOCABULARY_LOCALE` costs no coverage.
        //
        // A vocabulary chosen to make every command variant fire at once: one
        // documented, one not, one bogus mention (the whole corpus is bogus
        // against a two-word vocabulary) and one stale allowlist entry.
        let known = ["pane.copy", "app.quit"];
        let allow = ["app.quit", "pane.rename"];
        let read = check_commands_in(
            VOCABULARY_LOCALE,
            crate::corpus::topics(VOCABULARY_LOCALE),
            &known,
            &allow,
        );
        assert!(
            read.iter()
                .any(|i| matches!(i, Issue::StaleAllowEntry { .. })),
            "the fixture must exercise every variant it claims to: {read:?}"
        );

        for lang in LOCALES {
            let other: Vec<Issue> =
                check_commands_in(lang, crate::corpus::topics(lang), &known, &allow)
                    .into_iter()
                    .map(|issue| as_lang(issue, VOCABULARY_LOCALE))
                    .collect();
            assert_eq!(read, other, "{lang:?}");

            let contexts: Vec<Issue> =
                check_contexts_in(lang, crate::corpus::topics(lang), &["browse"])
                    .into_iter()
                    .map(|issue| as_lang(issue, VOCABULARY_LOCALE))
                    .collect();
            assert_eq!(
                check_contexts_in(
                    VOCABULARY_LOCALE,
                    crate::corpus::topics(VOCABULARY_LOCALE),
                    &["browse"]
                ),
                contexts,
                "{lang:?}"
            );
        }
    }
}
