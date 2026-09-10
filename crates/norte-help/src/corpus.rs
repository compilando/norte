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
    ("tabs", include_str!("../topics/en/tabs.md")),
    ("selection", include_str!("../topics/en/selection.md")),
    ("mouse", include_str!("../topics/en/mouse.md")),
    ("help", include_str!("../topics/en/help.md")),
    ("dialogs", include_str!("../topics/en/dialogs.md")),
    ("settings", include_str!("../topics/en/settings.md")),
    ("appearance", include_str!("../topics/en/appearance.md")),
    ("copying", include_str!("../topics/en/copying.md")),
    ("finding", include_str!("../topics/en/finding.md")),
    ("columns", include_str!("../topics/en/columns.md")),
    ("viewer", include_str!("../topics/en/viewer.md")),
    ("ai", include_str!("../topics/en/ai.md")),
    ("shell", include_str!("../topics/en/shell.md")),
    ("remote", include_str!("../topics/en/remote.md")),
    ("archives", include_str!("../topics/en/archives.md")),
    ("agents", include_str!("../topics/en/agents.md")),
    ("plugins", include_str!("../topics/en/plugins.md")),
];

/// `(id, source)` of every Spanish topic. Same ids, in the same order: the
/// suite pins both, because everything downstream of the corpus (the index,
/// `see_also`, the F1 context lookup) is keyed by id and must not depend on
/// the reader's language.
const ES: &[(&str, &str)] = &[
    ("index", include_str!("../topics/es/index.md")),
    ("panes", include_str!("../topics/es/panes.md")),
    ("tabs", include_str!("../topics/es/tabs.md")),
    ("selection", include_str!("../topics/es/selection.md")),
    ("mouse", include_str!("../topics/es/mouse.md")),
    ("help", include_str!("../topics/es/help.md")),
    ("dialogs", include_str!("../topics/es/dialogs.md")),
    ("settings", include_str!("../topics/es/settings.md")),
    ("appearance", include_str!("../topics/es/appearance.md")),
    ("copying", include_str!("../topics/es/copying.md")),
    ("finding", include_str!("../topics/es/finding.md")),
    ("columns", include_str!("../topics/es/columns.md")),
    ("viewer", include_str!("../topics/es/viewer.md")),
    ("ai", include_str!("../topics/es/ai.md")),
    ("shell", include_str!("../topics/es/shell.md")),
    ("remote", include_str!("../topics/es/remote.md")),
    ("archives", include_str!("../topics/es/archives.md")),
    ("agents", include_str!("../topics/es/agents.md")),
    ("plugins", include_str!("../topics/es/plugins.md")),
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

/// The topic that explains `context`, if any topic claims it.
///
/// The mapping lives in the corpus — a topic's `context` front matter — and
/// not in the frontend, so moving the explanation of a screen from one page to
/// another is an edit to prose rather than a code change. The frontend owns
/// only the vocabulary of context IDS (which places the app has), which is the
/// half it is the authority on: this crate does not know what a modal is.
///
/// `None` is a normal answer: a context whose page has not been written yet.
/// [`crate::check_contexts`] is what refuses to let that state ship unnoticed,
/// through [`crate::Issue::ContextWithoutTopic`].
///
/// First claimant in corpus order wins, and that is not a tie-break worth
/// relying on: two topics claiming one context is itself a finding
/// ([`crate::Issue::DuplicateContext`]), because otherwise which page F1 opens
/// would depend on the order of a table.
///
/// ```
/// use norte_help::{Lang, topic_for_context};
///
/// assert_eq!(
///     topic_for_context(Lang::En, "browse").map(|t| t.id.as_str()),
///     Some("panes")
/// );
/// assert!(topic_for_context(Lang::En, "no-such-context").is_none());
/// ```
///
/// # Panics
/// If an embedded topic is malformed; see [`topics`].
#[must_use]
pub fn topic_for_context(lang: Lang, context: &str) -> Option<&'static Topic> {
    topics(lang)
        .iter()
        .find(|t| t.context.iter().any(|c| c == context))
}

/// The topic that documents `command`, if one names it.
///
/// The reverse of what the documentation gate walks: it checks that every
/// command IS named by some topic, and this is the lookup that makes the check
/// pay off at runtime — the palette's row for a command can open its page, so
/// the fast gesture and the one that explains are two views of one model rather
/// than two places to look.
///
/// `None` is a normal answer — a command no page names yet, which the gate
/// carries on a shrinking allowlist — and a caller with no page to open should
/// say so rather than open the index: a reader sent to the index has to work out
/// for themselves what it had to do with what they asked.
///
/// The comparison is byte-exact, like every id comparison in this crate, and the
/// key is a DISPATCH key: a plugin row's `plugin:{id}:{command}` is answered
/// `None` by the same rule that answers an unknown command, with no special case
/// here.
///
/// # Which page, when several name it
///
/// Unlike a context, a command is legitimately named by several topics: the
/// mouse page names `pane.copy` to say a drag does what that key does, and the
/// copying page is where copying is explained. So this cannot be "the first
/// claimant in corpus order" — corpus order is READING order (the mouse page is
/// basics, copying is later), which says nothing about which page owns a
/// command, and taking it would open *Using the mouse* for `pane.copy`.
///
/// The corpus already carries the answer in data the author maintains anyway:
/// among the pages that name the command, one that ALSO links to another of them
/// through `see_also` is deferring — "I mention this; it is explained over
/// there". So the winner is the first claimant, in corpus order, that defers to
/// no other claimant. If every claimant defers (a mutual `see_also` between two
/// pages that both name it) the first in corpus order wins, which is arbitrary
/// but total: this function always answers the same page for the same corpus.
///
/// ```
/// use norte_help::{Lang, topic_for_command};
///
/// // `mouse` and `copying` both name it, and `mouse` comes first in the
/// // corpus — but it links to `copying`, so it is pointing rather than
/// // explaining.
/// assert_eq!(
///     topic_for_command(Lang::En, "pane.copy").map(|t| t.id.as_str()),
///     Some("copying")
/// );
/// assert!(topic_for_command(Lang::En, "no.such.command").is_none());
/// ```
///
/// # Panics
/// If an embedded topic is malformed; see [`topics`].
#[must_use]
pub fn topic_for_command(lang: Lang, command: &str) -> Option<&'static Topic> {
    let claimants: Vec<&'static Topic> = topics(lang)
        .iter()
        .filter(|t| t.commands.iter().any(|c| c == command))
        .collect();
    let defers = |t: &Topic| {
        claimants
            .iter()
            .any(|other| other.id != t.id && t.see_also.contains(&other.id))
    };
    claimants
        .iter()
        .copied()
        .find(|t| !defers(t))
        .or_else(|| claimants.first().copied())
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

    #[test]
    fn un_contexto_declarado_resuelve_a_su_tema() {
        // `panes` declara `context = ["browse"]`: F1 en el pane abre esa
        // página, y el mapa vive en el CORPUS, no en código de frontend.
        let t = topic_for_context(Lang::En, "browse").expect("browse tiene página");
        assert_eq!(t.id.as_str(), "panes");
        // Y en el otro locale resuelve al MISMO id: la paridad es estructural.
        let es = topic_for_context(Lang::Es, "browse").expect("browse en es");
        assert_eq!(es.id, t.id);
    }

    #[test]
    fn un_contexto_sin_tema_es_none_no_un_panico() {
        assert!(topic_for_context(Lang::En, "no-existe-este-contexto").is_none());
    }

    #[test]
    fn un_comando_resuelve_a_la_pagina_que_lo_documenta() {
        // La vuelta de lo que camina la puerta de documentación: ella
        // garantiza que TODO comando fuera de su allowlist lo nombra algún
        // tema, y esto es la consulta que cobra esa garantía en runtime (F1
        // sobre una fila de la palette).
        let t = topic_for_command(Lang::En, "pane.copy").expect("pane.copy está documentado");
        assert_eq!(t.id.as_str(), "copying");
        // Y el id no depende del idioma del lector: la paridad es estructural.
        assert_eq!(
            topic_for_command(Lang::Es, "pane.copy").map(|es| es.id.clone()),
            Some(t.id.clone())
        );
        assert!(topic_for_command(Lang::En, "no.such.command").is_none());
    }

    #[test]
    fn la_pagina_de_un_comando_es_la_que_lo_explica_no_la_que_lo_menciona() {
        // `mouse` nombra `pane.copy`/`pane.move` (un arrastre hace lo que hace
        // esa tecla) y va ANTES que `copying` en el corpus, así que "el primer
        // reclamante" abriría «Using the mouse» sobre la fila `pane.copy` de la
        // palette. El desempate es la deferencia: `mouse` enlaza a `copying`
        // por `see_also`, luego está señalando, no explicando.
        for cmd in ["pane.copy", "pane.move"] {
            assert_eq!(
                topic_for_command(Lang::En, cmd).map(|t| t.id.as_str()),
                Some("copying"),
                "{cmd} se explica en la página de copiar"
            );
        }
        // Y al revés: cuando la primera página en orden de corpus NO defiere,
        // gana ella. `panes` nombra `nav.enter` igual que `mouse` y
        // `archives`, y no enlaza a ninguna de las dos.
        assert_eq!(
            topic_for_command(Lang::En, "nav.enter").map(|t| t.id.as_str()),
            Some("panes")
        );
        assert_eq!(
            topic_for_command(Lang::En, "mark.toggle").map(|t| t.id.as_str()),
            Some("selection")
        );
    }

    #[test]
    fn una_clave_de_fila_de_plugin_no_documenta_nada() {
        // La `key` de una fila de plugin de la palette
        // (`plugin:{id}:{command}`) no es un comando del host y ningún tema
        // del corpus la nombra: `None`, jamás un pánico ni una página ajena.
        assert!(topic_for_command(Lang::En, "plugin:dev.norte.demo:greet").is_none());
    }
}
