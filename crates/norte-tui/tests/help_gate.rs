//! Documentation gate (ADR 0040): every command the TUI knows how to
//! dispatch lives in some topic of the `norte-help` corpus.
//!
//! It is DELIBERATE, permanent friction, the same idea as the i18n suite
//! that requires EN+ES for every `help-cmd-*`.
//!
//! # What exactly this gate measures
//!
//! MENTION, not explanation. A command is covered as soon as some topic
//! names it, and naming it is as cheap as adding its id to the front
//! matter's `commands` list: zero prose. What the gate guarantees, and it
//! is not little, is that no command can exist without SOMEONE having
//! looked at it while writing help, and that the corpus never promises a
//! command that does not exist. Whether the mention is also a useful
//! paragraph is decided by the page's review, which is where it can be
//! decided — no assertion knows whether a paragraph explains anything.
//!
//! Until H3h this carried a shrinking allowlist: the commands no topic
//! documented yet, hand-written, with a ceiling that could only go down.
//! H3h left it at zero and the list was deleted with it, which was the plan
//! from the start. What is left is the bare gate: a new command with no
//! page breaks the suite and there is NOWHERE to note it down — the fix is
//! writing the paragraph.
//!
//! # The other half: CONTEXTS
//!
//! The same, in both directions, for the places the reader can be in
//! (H3c): a topic cannot claim a screen the TUI does not have, and a screen
//! the TUI knows how to open cannot be left with no page — F1 there would
//! open the index and nobody would complain. The vocabulary comes from a
//! single source ([`contextos`]), and its allowlist ran out in H3h the same
//! as the commands' one: today every context the TUI knows how to open has
//! a page.
//!
//! Here the gate measures something MORE than a mention: claiming a
//! context tells the reader "this is what explains what you have in front
//! of you." Whether the page truly explains it is decided by whoever writes
//! it — an agent approval is not explained by the copy page — and that is
//! why the pending list has written, line by line, why each context is
//! still there.

use norte_help::{Issue, check_commands, check_contexts, check_corpus};
use norte_tui::keymap::{COMMANDS, DIALOG_COMMANDS};

/// The vocabulary the corpus is cross-checked against: EVERYTHING the TUI
/// dispatches, `COMMANDS` ∪ `DIALOG_COMMANDS`.
///
/// The union and not just `COMMANDS`, for the cross-check's other
/// direction. A `{{cmd:dialog.approve}}` in the approval modal's page is
/// legitimate prose — the verb exists, the TUI resolves it and F1 already
/// lists it (#113) — but with a vocabulary trimmed to `COMMANDS` it would
/// come out as `UnknownCommand`, i.e. "that command does not exist," which
/// is false. The author would only have two ways out: not document it, or
/// widen the vocabulary here. It was widened, and H3h paid the bill: the 19
/// `dialog.*` verbs have a page.
///
/// It is computed (both lists are already hand-written in `keymap.rs`, and
/// duplicating them here would be a third copy that goes out of sync).
fn vocabulary() -> Vec<&'static str> {
    COMMANDS
        .iter()
        .copied()
        .chain(DIALOG_COMMANDS.iter().copied())
        .collect()
}

/// The contexts the TUI knows how to open. ONE source: the closed
/// vocabulary of [`norte_tui::help_context::CONTEXTS`], anchored to `Modal`
/// there — `modal_context`'s wildcard-free `match` is what stops a new
/// modal from arriving without someone deciding which page explains it.
///
/// Duplicating the list here would be the third copy that goes out of sync
/// (and the second already did: until H3c this gate asked for a `dialog`
/// context no modal produces). It is computed, like [`vocabulary`], and
/// for the same reason.
fn contextos() -> Vec<&'static str> {
    norte_tui::help_context::CONTEXTS.to_vec()
}

#[test]
fn the_corpus_we_send_is_intact() {
    // Locale parity, unique ids, links that resolve, and no live markup
    // written where it paints literal. Needs no vocabulary: it is the only
    // thing `norte-help` itself already checks on its own.
    let issues = check_corpus();
    assert!(
        issues.is_empty(),
        "the corpus has integrity problems:\n{}",
        lines(&issues)
    );
}

#[test]
fn the_corpus_does_not_name_commands_that_do_not_exist() {
    // WITHOUT an allowlist (`&[]`, literally), and it is not an omission: a
    // topic that names a nonexistent command is always a bug — prose that
    // promises a key that does nothing, or a misspelled id. There is no
    // debt to paper over here, only typos to fix — and since H3h there is
    // no allowlist left to pass in the other direction either.
    let desconocidos: Vec<Issue> = check_commands(&vocabulary(), &[])
        .into_iter()
        .filter(|i| matches!(i, Issue::UnknownCommand { .. }))
        .collect();
    assert!(
        desconocidos.is_empty(),
        "the corpus names commands outside the TUI's vocabulary:\n{}",
        lines(&desconocidos)
    );
}

#[test]
fn every_command_in_the_vocabulary_is_documented() {
    // `&[]` and not an allowlist: since H3h there is no debt to paper over.
    // A command with no page is a failure with a single fix — write the
    // paragraph — and there is no line left to postpone it with.
    let issues = check_commands(&vocabulary(), &[]);

    let sin_documentar: Vec<&Issue> = issues
        .iter()
        .filter(|i| matches!(i, Issue::UndocumentedCommand { .. }))
        .collect();
    assert!(
        sin_documentar.is_empty(),
        "commands no topic documents. Write the paragraph: the allowlist \
         that postponed this ran out in H3h and is not coming back.\n{}",
        sin_documentar
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );

    // And nothing else. `check_commands` can grow new variants: one showing
    // up and this file silently ignoring it would be exactly the failure
    // the gate exists to not have.
    assert!(
        issues.is_empty(),
        "findings this gate does not classify:\n{}",
        lines(&issues)
    );
}

#[test]
fn the_corpus_contexts_are_screens_the_tui_has() {
    let contextos = contextos();
    let issues = check_contexts(&contextos);

    // One direction: no topic declares a made-up context, and two topics do
    // not fight over the same one. The list is not touched here — the fix
    // is in the topic's front matter, because the TUI defines the contexts.
    let del_corpus: Vec<&Issue> = issues
        .iter()
        .filter(|i| {
            matches!(
                i,
                Issue::UnknownContext { .. } | Issue::DuplicateContext { .. }
            )
        })
        .collect();
    assert!(
        del_corpus.is_empty(),
        "fix the topic's front matter, not this list: the TUI defines the \
         contexts.\n{}",
        del_corpus
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );

    // The other one: every context the TUI knows how to open has a page.
    // Without this, F1 on a page-less screen opens the index and nobody
    // complains.
    let no_page: Vec<&str> = issues
        .iter()
        .filter_map(|i| match i {
            Issue::ContextWithoutTopic { context, .. } => Some(context.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        no_page.is_empty(),
        "contexts with no page. Claim them from the `context` of the page \
         that explains them, or write that page: the allowlist that \
         postponed them ran out in H3h. {no_page:?}"
    );

    // And nothing else, for the same reason as the commands gate: a new
    // `Issue` variant this file silently ignored would be exactly the
    // failure the gate exists to not have.
    let sin_clasificar: Vec<&Issue> = issues
        .iter()
        .filter(|i| {
            !matches!(
                i,
                Issue::UnknownContext { .. }
                    | Issue::DuplicateContext { .. }
                    | Issue::ContextWithoutTopic { .. }
            )
        })
        .collect();
    assert!(
        sin_clasificar.is_empty(),
        "findings this gate does not classify:\n{}",
        sin_clasificar
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// One finding per line, the way `norte doctor` (H3g) would print them.
fn lines(issues: &[Issue]) -> String {
    issues
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}
