//! The embedded corpus: norte's own help pages, one directory per locale,
//! compiled INTO the binary with an explicit `include_str!` table.
//!
//! No runtime I/O, deliberately (ADR 0040). Help that lives in files next to
//! the binary is help that is missing from a `cargo install`, from a musl
//! container and from a `scp`'d single file — and the one moment a user needs
//! F1 is the moment nothing else is working. It is the same pattern as
//! `norte-theme`'s presets and `norte-i18n`'s catalogues.
//!
//! The table is written out by hand rather than generated, because
//! `include_str!` cannot walk a directory. That is a maintenance cost paid
//! once per topic, and `tests/corpus.rs` reads the directory to make sure the
//! cost was actually paid: a file added to the tree and forgotten here fails
//! the suite instead of silently not existing.

use std::sync::OnceLock;

use crate::model::{Topic, TopicId};
use crate::parse::parse_trusted;
use norte_i18n::Lang;

/// `(id, source)` of every English topic. The id repeats the filename so the
/// panic message and the coverage test can both name a topic without opening
/// the file.
const EN: &[(&str, &str)] = &[
    ("index", include_str!("../topics/en/index.md")),
    ("panes", include_str!("../topics/en/panes.md")),
    ("selection", include_str!("../topics/en/selection.md")),
    ("mouse", include_str!("../topics/en/mouse.md")),
    ("help", include_str!("../topics/en/help.md")),
    ("copying", include_str!("../topics/en/copying.md")),
    ("remote", include_str!("../topics/en/remote.md")),
    ("archives", include_str!("../topics/en/archives.md")),
];

/// `(id, source)` of every Spanish topic. Same ids, in the same order: the
/// suite pins both, because everything downstream of the corpus (the index,
/// `see_also`, the F1 context lookup) is keyed by id and must not depend on
/// the reader's language.
const ES: &[(&str, &str)] = &[
    ("index", include_str!("../topics/es/index.md")),
    ("panes", include_str!("../topics/es/panes.md")),
    ("selection", include_str!("../topics/es/selection.md")),
    ("mouse", include_str!("../topics/es/mouse.md")),
    ("help", include_str!("../topics/es/help.md")),
    ("copying", include_str!("../topics/es/copying.md")),
    ("remote", include_str!("../topics/es/remote.md")),
    ("archives", include_str!("../topics/es/archives.md")),
];

/// The raw table for a locale.
fn sources(lang: Lang) -> &'static [(&'static str, &'static str)] {
    match lang {
        Lang::En => EN,
        Lang::Es => ES,
    }
}

/// Parses one embedded topic, or PANICS naming it.
///
/// The panic is deliberate and it is not a runtime risk. The corpus ships
/// inside the binary, so a malformed topic is a build-time defect, not user
/// input; `tests/corpus.rs` parses every topic of every locale, so the defect
/// fails the suite long before it can reach anyone. The alternative —
/// returning an empty topic, or skipping it — hides the bug behind a blank
/// page, which is exactly the failure nobody reports.
///
/// The message names WHICH topic failed. A parse error alone ("front matter:
/// invalid TOML header") is useless across twelve files.
fn parse_one(lang: Lang, id: &str, source: &str) -> Topic {
    match parse_trusted(source) {
        Ok(parsed) => parsed.topic,
        Err(e) => panic!("built-in help topic `{id}` ({lang:?}) is malformed: {e}"),
    }
}

/// Every topic of a locale, parsed once and kept.
///
/// Lazy because the corpus is a dozen pages of markdown and most runs of
/// norte never open F1: parsing it at startup would be work done on the
/// chance that someone asks.
///
/// ```
/// use norte_help::{Lang, topics};
///
/// let en = topics(Lang::En);
/// assert!(!en.is_empty());
/// // Same structure in both locales; only the prose differs.
/// assert_eq!(en.len(), topics(Lang::Es).len());
/// ```
///
/// # Panics
/// If an embedded topic is malformed, naming the topic. That cannot happen in
/// a binary whose test suite ran: the corpus is compiled in, and
/// `tests/corpus.rs` parses every topic of every locale.
#[must_use]
pub fn topics(lang: Lang) -> &'static [Topic] {
    static EN_PARSED: OnceLock<Vec<Topic>> = OnceLock::new();
    static ES_PARSED: OnceLock<Vec<Topic>> = OnceLock::new();
    let cell = match lang {
        Lang::En => &EN_PARSED,
        Lang::Es => &ES_PARSED,
    };
    cell.get_or_init(|| {
        sources(lang)
            .iter()
            .map(|(id, src)| parse_one(lang, id, src))
            .collect()
    })
}

/// The ids of a locale's topics, in corpus order.
///
/// Corpus order and not alphabetical: the table is written in reading order
/// (`index` first), and an index that reorders itself per locale would be a
/// different table of contents in each language.
///
/// ```
/// use norte_help::{Lang, topic_ids};
///
/// let ids = topic_ids(Lang::En);
/// assert_eq!(ids.first().map(norte_help::TopicId::as_str), Some("index"));
/// ```
///
/// # Panics
/// If an embedded topic is malformed; see [`topics`].
#[must_use]
pub fn topic_ids(lang: Lang) -> Vec<TopicId> {
    topics(lang).iter().map(|t| t.id.clone()).collect()
}

/// One topic by id, or `None` if the corpus has no such page.
///
/// The comparison is byte-exact, like every other id comparison in this crate
/// ([`TopicId`] never normalises): `[[copying]]` and `[[Copying]]` are not the
/// same link, and a lookup that quietly folded them would make a broken link
/// in the corpus invisible to the checks.
///
/// ```
/// use norte_help::{Lang, topic};
///
/// assert!(topic(Lang::En, "copying").is_some());
/// assert!(topic(Lang::En, "Copying").is_none(), "ids are byte-exact");
/// assert!(topic(Lang::En, "no-such-topic").is_none());
/// ```
///
/// # Panics
/// If an embedded topic is malformed; see [`topics`].
#[must_use]
pub fn topic(lang: Lang, id: &str) -> Option<&'static Topic> {
    topics(lang).iter().find(|t| t.id.as_str() == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_locales_expose_the_same_ids_in_the_same_order() {
        assert_eq!(topic_ids(Lang::En), topic_ids(Lang::Es));
    }

    #[test]
    fn a_lookup_returns_the_topic_it_was_asked_for() {
        let t = topic(Lang::En, "panes").expect("the `panes` topic exists");
        assert_eq!(t.id.as_str(), "panes");
        assert!(!t.title.is_empty());
    }

    #[test]
    fn an_unknown_id_is_none_rather_than_a_panic() {
        assert!(topic(Lang::En, "").is_none());
        assert!(topic(Lang::Es, " panes ").is_none(), "no trimming");
    }
}
