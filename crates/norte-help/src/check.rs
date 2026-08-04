//! Corpus integrity checks: they return DATA, never `panic!`.
//!
//! Two consumers, and the shape follows from having both. The test suite
//! treats a non-empty list as a build failure, which is what keeps a broken
//! link or an undocumented command out of a release. `norte doctor` (phase
//! H3g) shows the SAME findings to a user, who is holding a binary that has
//! already shipped and for whom a panic would be the least useful possible
//! report. One set of checks, therefore, returning values.
//!
//! Every check comes in two forms: a `check_*_in` that takes the topics as an
//! argument and holds all the logic, and a `check_*` wrapper that feeds it the
//! embedded corpus. The split is not decoration — it is what lets the tests
//! drive a check with a DELIBERATELY broken corpus (the only honest way to
//! prove a check fires) instead of corrupting the one we ship, and it is what
//! will let `norte doctor` run the same checks over a plugin's topics.

use std::collections::BTreeSet;

use norte_i18n::Lang;

use crate::model::{Block, Span, Topic};

/// The locales the embedded corpus ships. [`check_corpus`] derives its input
/// from this, so adding a locale extends the parity check by construction
/// rather than by remembering to.
const LOCALES: [Lang; 2] = [Lang::En, Lang::Es];

/// The locale [`check_commands`] and [`check_contexts`] read.
///
/// Reading ONE locale is deliberate, and it is only sound because parity is
/// machine-enforced elsewhere: `tests/corpus.rs` pins that `commands` and
/// `context` are identical across locales, and the unit tests at the bottom of
/// this file pin the same for the `{{cmd:…}}` marks in the bodies — which the
/// front-matter comparison does not see. With that held, checking `es` as well
/// could only ever report the same finding twice, in a list a human reads.
///
/// The one thing this must NOT become is a silent assumption. If those parity
/// tests are ever removed, this constant is where the loss lands: the check
/// would keep passing while the Spanish reader gets rows the English one does
/// not. Hence a named constant and not a hard-coded `Lang::En` in two bodies.
const VOCABULARY_LOCALE: Lang = Lang::En;

/// A problem found in a corpus.
///
/// Deliberately NOT `#[non_exhaustive]`, against the convention the rest of
/// this crate follows for enums expected to grow. The reason is the second
/// consumer: `norte doctor` has to RENDER every variant, and an exhaustive
/// `match` is what makes adding a new kind of problem break the renderer
/// instead of silently printing nothing for it. A diagnostic nobody prints is
/// worse than no diagnostic at all.
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
/// for a reader to press. The unit tests pin that.
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

/// The commands a topic mentions: its `commands` front matter plus every
/// `{{cmd:…}}` mark in its body, wherever the mark sits.
///
/// Both doors count, and they are not the same door. `commands` is the
/// topic's runnable rows; a `{{cmd:…}}` is the prose naming a key. A command
/// referenced only in prose is still documented, and a command listed only in
/// the front matter is still a claim that the command exists.
///
/// Sorted and deduplicated: a command named three times in one page is one
/// mention, and the order a `Vec` walk happens to produce is not something a
/// diff should be sensitive to.
fn mentions(t: &Topic) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = t.commands.iter().cloned().collect();
    out.extend(spans_of(t).into_iter().filter_map(|s| match s {
        Span::CommandRef(c) => Some(c.clone()),
        _ => None,
    }));
    out
}

/// Every topic id a topic points at: `see_also` plus the `[[links]]` of its
/// body. Sorted and deduplicated, for the reason [`mentions`] is.
fn links(t: &Topic) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = t.see_also.iter().map(ToString::to_string).collect();
    out.extend(spans_of(t).into_iter().filter_map(|s| match s {
        Span::TopicLink(id) => Some(id.to_string()),
        _ => None,
    }));
    out
}

/// Checks a set of locales against each other: topic parity, unique ids, and
/// links that resolve. The logic behind [`check_corpus`].
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
        }
    }
    issues
}

/// Checks the EMBEDDED corpus: locale parity, unique ids, links that resolve.
/// Needs no external vocabulary, so it is the one check that can run anywhere.
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
/// // …and the allowlist silences it.
/// let issues = check_commands(&["no.such-command"], &["no.such-command"]);
/// assert!(!issues.iter().any(|i| matches!(i, Issue::UndocumentedCommand { .. })));
///
/// // The other direction fires too, and PER TOPIC: three of the commands the
/// // corpus names are named by two topics each, so nineteen distinct
/// // commands produce twenty-two mentions, each of them a page to fix.
/// assert_eq!(
///     issues.iter().filter(|i| matches!(i, Issue::UnknownCommand { .. })).count(),
///     22
/// );
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
            vec![Issue::UndocumentedCommand {
                command: "app.quit".to_owned(),
                lang: Lang::En,
            }],
            "allowing a command that IS documented silences nothing else"
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
            check_commands_in(Lang::En, &[t], &["pane.copy"], &[]),
            vec![Issue::UndocumentedCommand {
                command: "pane.copy".to_owned(),
                lang: Lang::En,
            }],
            "inert text documents nothing"
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

    // --- What makes reading ONE locale safe. See `VOCABULARY_LOCALE`.

    #[test]
    fn both_locales_mention_exactly_the_same_commands() {
        // `tests/corpus.rs` pins the `commands` FRONT MATTER across locales.
        // This pins the other half — the `{{cmd:…}}` marks in the prose,
        // which the front-matter comparison cannot see, and which a
        // translator is far more likely to drop than a header list.
        for t_en in crate::corpus::topics(Lang::En) {
            let t_es = crate::corpus::topic(Lang::Es, t_en.id.as_str())
                .unwrap_or_else(|| panic!("`{}` exists in both locales", t_en.id));
            assert_eq!(
                mentions(t_en),
                mentions(t_es),
                "`{}`: the locales document different commands, so checking \
                 only `{VOCABULARY_LOCALE:?}` would miss one",
                t_en.id
            );
        }
    }

    #[test]
    fn both_locales_declare_exactly_the_same_contexts() {
        for t_en in crate::corpus::topics(Lang::En) {
            let t_es = crate::corpus::topic(Lang::Es, t_en.id.as_str())
                .unwrap_or_else(|| panic!("`{}` exists in both locales", t_en.id));
            assert_eq!(t_en.context, t_es.context, "`{}`", t_en.id);
        }
    }

    #[test]
    fn checking_the_other_locale_finds_exactly_the_same_issues() {
        // The parity above is the argument; this is the conclusion, asserted
        // end to end. The two runs differ only in the `lang` each issue
        // carries, so `VOCABULARY_LOCALE` costs no coverage.
        let known = ["pane.copy", "app.quit"];
        let es: Vec<Issue> = check_commands_in(
            Lang::Es,
            crate::corpus::topics(Lang::Es),
            &known,
            &["app.quit"],
        )
        .into_iter()
        .map(|issue| match issue {
            Issue::UnknownCommand { topic, command, .. } => Issue::UnknownCommand {
                topic,
                command,
                lang: Lang::En,
            },
            other => other,
        })
        .collect();
        assert_eq!(check_commands(&known, &["app.quit"]), es);
    }
}
