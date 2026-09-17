//! The embedded corpus is a build-time input: everything asserted here is a
//! property the shipped binary must have, not a runtime possibility.
//!
//! Two families of check live in this file. The first is that the corpus
//! PARSES — the public API panics on a malformed topic on purpose, so this
//! suite is what stops such a topic from ever reaching a user. The second is
//! STRUCTURAL PARITY between locales: the prose differs, the structure does
//! not, because everything downstream (the index, `see_also` navigation, the
//! runnable command rows, the F1 context lookup) is built from the structure
//! and must behave identically whichever language the reader picked.

mod common;

use std::collections::BTreeSet;
use std::path::PathBuf;

use norte_help::{
    Issue, Lang, Span, Topic, TopicId, check_commands, check_contexts, check_corpus, parse_trusted,
    topic, topic_ids, topics,
};

/// Both locales, so every check below runs twice by construction instead of
/// by copy-paste.
const LANGS: [Lang; 2] = [Lang::En, Lang::Es];

/// The topics every locale must carry. Written out rather than derived from
/// the corpus so that DELETING a topic file is a test failure too: a check
/// that reads the corpus to decide what the corpus should contain cannot see
/// an absence.
const EXPECTED: [&str; 20] = [
    "index",
    "panes",
    "tabs",
    "history",
    "selection",
    "mouse",
    "help",
    "dialogs",
    "settings",
    "appearance",
    "copying",
    "finding",
    "columns",
    "viewer",
    "ai",
    "shell",
    "remote",
    "archives",
    "agents",
    "plugins",
];

/// The directory holding a locale's `.md` files, from `CARGO_MANIFEST_DIR`.
///
/// Reading the filesystem is test-only and deliberate: it is the ONLY way to
/// catch a topic file that was added to the tree and forgotten in the
/// `include_str!` table, which otherwise fails silently — the corpus still
/// parses, the new page just does not exist.
fn topics_dir(lang: Lang) -> PathBuf {
    let dir = match lang {
        Lang::En => "en",
        Lang::Es => "es",
    };
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("topics")
        .join(dir)
}

/// The `.md` file stems in a locale's directory.
fn file_stems(lang: Lang) -> BTreeSet<String> {
    let dir = topics_dir(lang);
    let entries = std::fs::read_dir(&dir).expect("the topics directory exists");
    let mut out = BTreeSet::new();
    for entry in entries {
        let path = entry.expect("a readable directory entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("a topic filename is ASCII by convention")
            .to_owned();
        out.insert(stem);
    }
    out
}

/// The ids the embedded table exposes, as a set.
fn embedded_ids(lang: Lang) -> BTreeSet<String> {
    topic_ids(lang)
        .into_iter()
        .map(|id| id.as_str().to_owned())
        .collect()
}

/// Looks a topic up, failing with the id when it is missing.
fn get(lang: Lang, id: &str) -> &'static Topic {
    topic(lang, id).unwrap_or_else(|| panic!("{lang:?}: the `{id}` topic exists"))
}

#[test]
fn every_locale_carries_the_expected_topics() {
    for lang in LANGS {
        let ids = embedded_ids(lang);
        assert!(!ids.is_empty(), "{lang:?}: the corpus must not be empty");
        let expected: BTreeSet<String> = EXPECTED.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(ids, expected, "{lang:?}: unexpected set of topic ids");
    }
}

#[test]
fn the_embedded_table_covers_the_topics_directory() {
    for lang in LANGS {
        assert_eq!(
            embedded_ids(lang),
            file_stems(lang),
            "{lang:?}: the `include_str!` table and the topics directory disagree. \
             A file present on disk but absent from the table is a page that \
             silently does not exist"
        );
    }
}

#[test]
fn a_topic_id_matches_its_filename() {
    // The directory check above compares SETS, so it would still pass if two
    // topics swapped ids. Anchoring each id to its own file is what makes
    // `topics/en/copying.md` a reliable place to look for `[[copying]]`.
    for lang in LANGS {
        for (id, expected) in topic_ids(lang).iter().zip(EXPECTED) {
            assert_eq!(
                id.as_str(),
                expected,
                "{lang:?}: the table is out of order or misnamed"
            );
        }
    }
}

#[test]
fn every_topic_has_a_title_and_at_least_one_block() {
    for lang in LANGS {
        for t in topics(lang) {
            assert!(
                !t.title.trim().is_empty(),
                "{lang:?}/{}: a nameless topic renders a blank page header",
                t.id
            );
            assert!(
                !t.blocks.is_empty(),
                "{lang:?}/{}: an empty body is invisible to the reader",
                t.id
            );
        }
    }
}

#[test]
fn the_structure_is_identical_across_locales() {
    // The prose differs; everything the UI is built from does not. A `tags`
    // set that drifts splits the index in one language, a `commands` set that
    // drifts hands one language runnable rows the other does not have, and a
    // `context` set that drifts makes F1 open a different page.
    for id in EXPECTED {
        let en = get(Lang::En, id);
        let es = get(Lang::Es, id);
        assert_eq!(en.tags, es.tags, "{id}: `tags` differ across locales");
        assert_eq!(
            en.see_also, es.see_also,
            "{id}: `see_also` differs across locales"
        );
        assert_eq!(
            en.commands, es.commands,
            "{id}: `commands` differ across locales"
        );
        assert_eq!(
            en.context, es.context,
            "{id}: `context` differs across locales"
        );
    }
}

#[test]
fn see_also_only_points_at_topics_that_exist() {
    for lang in LANGS {
        for t in topics(lang) {
            for target in &t.see_also {
                assert!(
                    topic(lang, target.as_str()).is_some(),
                    "{lang:?}/{}: `see_also` points at the unknown topic `{target}`",
                    t.id
                );
                assert_ne!(
                    &t.id, target,
                    "{lang:?}/{}: `see_also` points at itself",
                    t.id
                );
            }
        }
    }
}

/// Every `[[topic]]` link the body of `t` contains.
fn links_of(t: &Topic) -> BTreeSet<TopicId> {
    fn walk(spans: &[Span], out: &mut BTreeSet<TopicId>) {
        for span in spans {
            if let Span::TopicLink(id) = span {
                out.insert(id.clone());
            }
        }
    }
    let mut out = BTreeSet::new();
    for block in &t.blocks {
        match block {
            norte_help::Block::Paragraph(spans) | norte_help::Block::Callout { spans, .. } => {
                walk(spans, &mut out);
            }
            norte_help::Block::Bullets(items) => {
                for item in items {
                    walk(item, &mut out);
                }
            }
            norte_help::Block::Heading { .. }
            | norte_help::Block::Code { .. }
            | norte_help::Block::Table { .. } => {}
        }
    }
    out
}

#[test]
fn every_body_link_points_at_a_topic_that_exists() {
    for lang in LANGS {
        for t in topics(lang) {
            for target in links_of(t) {
                assert!(
                    topic(lang, target.as_str()).is_some(),
                    "{lang:?}/{}: `[[{target}]]` points at no topic",
                    t.id
                );
            }
        }
    }
}

#[test]
fn the_index_links_to_every_other_topic() {
    // The index is the front door: a topic it does not reach is a topic a
    // reader can only find by knowing its name already.
    for lang in LANGS {
        let index = get(lang, "index");
        let linked = links_of(index);
        for id in EXPECTED {
            if id == "index" {
                continue;
            }
            assert!(
                linked.contains(&TopicId::new(id)),
                "{lang:?}: the index does not link to `{id}`"
            );
        }
    }
}

#[test]
fn the_whole_corpus_round_trips_through_the_public_api() {
    // The API parses lazily behind a `OnceLock` and PANICS on a malformed
    // topic. Touching every topic of every locale, through every accessor, is
    // what turns that panic into a test failure here instead of a crash in
    // front of a user.
    for lang in LANGS {
        let all = topics(lang);
        assert_eq!(all.len(), topic_ids(lang).len());
        for t in all {
            let looked_up = get(lang, t.id.as_str());
            assert_eq!(
                looked_up, t,
                "{lang:?}/{}: lookup returned another topic",
                t.id
            );
        }
        assert!(
            topic(lang, "no-such-topic").is_none(),
            "{lang:?}: an unknown id must be `None`, not a panic"
        );
    }
}

#[test]
fn the_corpus_exercises_every_block_kind() {
    // H3b renders this model in three frontends, and a block kind no topic
    // produces is a renderer branch nothing ever exercises. Asserted per
    // LOCALE: a table that exists only in English is a code path the Spanish
    // reader never reaches.
    for lang in LANGS {
        let mut seen = [false; 6];
        for t in topics(lang) {
            for block in &t.blocks {
                let slot = match block {
                    norte_help::Block::Heading { .. } => 0,
                    norte_help::Block::Paragraph(_) => 1,
                    norte_help::Block::Bullets(_) => 2,
                    norte_help::Block::Code { .. } => 3,
                    norte_help::Block::Table { .. } => 4,
                    norte_help::Block::Callout { .. } => 5,
                };
                seen[slot] = true;
            }
        }
        let names = [
            "Heading",
            "Paragraph",
            "Bullets",
            "Code",
            "Table",
            "Callout",
        ];
        for (ok, name) in seen.iter().zip(names) {
            assert!(*ok, "{lang:?}: no topic produces a `{name}` block");
        }
    }
}

#[test]
fn no_topic_trips_a_limits_ceiling() {
    // A built-in topic that hits a `Limits` ceiling is a CORPUS bug: the
    // reader would silently get a page with its tail cut off, and the badge
    // that exists for hostile plugin help is not an excuse for our own text.
    // Parsed from the files rather than from the embedded table because the
    // flags do not survive into `Topic` — only the parse reports them.
    for lang in LANGS {
        for id in EXPECTED {
            let path = topics_dir(lang).join(format!("{id}.md"));
            let bytes = std::fs::read(&path).expect("a readable topic file");
            let src = String::from_utf8(bytes)
                .unwrap_or_else(|_| panic!("{lang:?}/{id}: a topic file must be valid UTF-8"));
            let parsed = parse_trusted(&src)
                .unwrap_or_else(|e| panic!("{lang:?}/{id}: the topic must parse: {e}"));
            assert!(!parsed.truncated, "{lang:?}/{id}: the topic was truncated");
            assert!(!parsed.lossy, "{lang:?}/{id}: the topic decoded lossily");
        }
    }
}

/// PROSE has no hazard gate anywhere else: this is it.
///
/// `check_corpus`'s whole issue vocabulary is about IDS — dangling links,
/// unknown commands, duplicate contexts, inert marks — and every check above
/// compares STRUCTURE between locales. Not one of them looks at a character.
/// Meanwhile the renderer masks nothing on purpose, on the grounds that a
/// built-in topic is trusted: code fences are painted unwrapped and unmasked,
/// so a raw `ESC` in one is an ANSI sequence delivered straight to the
/// reader's terminal; a `U+202E` in a title reorders a sidebar row; a
/// `U+200B` makes two rows visually identical. "Trusted" is a statement about
/// who may EDIT the file, and the thing standing between an edit and a
/// terminal is this test.
///
/// It is green on the first run and will stay green until someone changes a
/// `.md`, which is the point: its value is entirely in the edits it will
/// catch. The `.md` files are what a translator touches, and the day an RTL
/// locale lands the bidi isolates `U+2066`..`U+2069` — the CORRECT way to
/// wrap an `sftp://` run inside RTL prose — become legitimate editorial marks
/// that are also, every one of them, terminal hazards. This gate is what
/// forces that to be a deliberate decision (isolate the run in the model, or
/// teach the renderer to mask) instead of a silent regression.
///
/// The collector is the one `hostile.rs` sweeps plugin topics with, so our own
/// prose and third-party prose are held to the SAME definition of "every
/// string a renderer paints".
#[test]
fn no_shipped_topic_carries_a_terminal_hazard() {
    for lang in LANGS {
        for t in topics(lang) {
            for (label, s) in common::strings(t) {
                if let Some(c) = s.chars().find(|c| norte_encoding::is_terminal_hazard(*c)) {
                    panic!(
                        "{lang:?}/{}: raw hazard U+{:04X} in {label}: {s:?}\n\
                         The renderer masks NOTHING here — a control reaches the \
                         terminal and a bidi override reorders the line. Rewrite \
                         the topic, or mask at the seam that paints it.",
                        t.id, c as u32
                    );
                }
            }
        }
    }
}

/// …and the sweep above really fires. Corpus-driven: the canonical hostile
/// TITLES (`norte_testkit::corpus::hostile_titles`), planted one at a time in
/// every string slot the collector reaches.
///
/// Without this, `no_shipped_topic_carries_a_terminal_hazard` would pass just
/// as happily with a collector that returned an empty vector, or with an
/// `is_terminal_hazard` that had quietly stopped covering bidi. The fixture
/// that carries the sweep is `bidi_isolate_url`: it is the LEGITIMATE
/// editorial shape, so what is pinned is that legitimate-looking prose is
/// exactly what the gate catches.
#[test]
fn the_hazard_sweep_catches_a_hostile_title_in_every_slot() {
    let bidi = norte_testkit::corpus::hostile_titles()
        .into_iter()
        .find(|t| t.id == "bidi_isolate_url")
        .expect("the canonical corpus ships it");
    let planted = |topic: Topic| {
        let hits: Vec<&'static str> = common::strings(&topic)
            .into_iter()
            .filter(|(_, s)| s.chars().any(norte_encoding::is_terminal_hazard))
            .map(|(label, _)| label)
            .collect();
        assert!(
            !hits.is_empty(),
            "a hazard planted in this slot escaped the collector entirely"
        );
        hits
    };
    let base = || Topic {
        id: TopicId::new("x"),
        title: "clean".to_owned(),
        tags: Vec::new(),
        see_also: Vec::new(),
        commands: Vec::new(),
        context: Vec::new(),
        blocks: Vec::new(),
        origin: norte_help::Origin::BuiltIn,
    };

    // A title: the sidebar row.
    let mut t = base();
    t.title = bidi.text.to_owned();
    assert_eq!(planted(t), ["title"]);

    // A heading: painted like a title, one level down.
    let mut t = base();
    t.blocks = vec![norte_help::Block::Heading {
        level: 2,
        text: bidi.text.to_owned(),
    }];
    assert_eq!(planted(t), ["heading"]);

    // A code fence: the worst of the lot, since it is painted UNWRAPPED and
    // is where an `ESC` would sit.
    let mut t = base();
    t.blocks = vec![norte_help::Block::Code {
        lang: None,
        text: format!("norte cp {}\n", bidi.text),
    }];
    assert_eq!(planted(t), ["code.text"]);

    // A table cell.
    let mut t = base();
    t.blocks = vec![norte_help::Block::Table {
        header: vec!["Answer".to_owned()],
        rows: vec![vec![bidi.text.to_owned()]],
    }];
    assert_eq!(planted(t), ["table.cell"]);

    // And ordinary prose.
    let mut t = base();
    t.blocks = vec![norte_help::Block::Paragraph(vec![Span::Text(
        bidi.text.to_owned(),
    )])];
    assert_eq!(planted(t), ["span"]);
}

// --- Integrity checks (`norte_help::check`) against the SHIPPED corpus.
//
// These are the positive half: run over the real six topics, every check must
// come back with an empty list. The negative half — a dangling link, a
// duplicate id, an unknown command — lives in the crate's own unit tests and
// is driven by SYNTHETIC topics, because the way to prove a check fires is to
// hand it a broken corpus, not to break the one we ship.

/// Every command the six seed topics document: the union of their `commands`
/// front matter and the `{{cmd:…}}` marks in their bodies.
///
/// Written out rather than derived from the corpus, for the reason `EXPECTED`
/// is: a list computed from the corpus cannot notice that the corpus stopped
/// documenting something. A mark added or dropped shows up here as a diff, and
/// the number is the one phase H3h has to move.
const DOCUMENTED: [&str; 166] = [
    "app.extensions",
    "app.goto",
    "app.help",
    "app.menu",
    "app.palette",
    "app.pick-accept",
    "app.quit",
    "app.settings",
    "app.terminal",
    "app.theme",
    "app.toggle-panels",
    "cursor.bottom",
    "cursor.down",
    "cursor.page-down",
    "cursor.page-up",
    "cursor.top",
    "cursor.up",
    "dialog.add",
    "dialog.approve",
    "dialog.back",
    "dialog.cancel",
    "dialog.clear",
    "dialog.confirm",
    "dialog.confirm-other",
    "dialog.cycle-format",
    "dialog.deny",
    "dialog.down",
    "dialog.filter",
    "dialog.move-down",
    "dialog.move-up",
    "dialog.newer",
    "dialog.overwrite",
    "dialog.page-down",
    "dialog.page-up",
    "dialog.pane",
    "dialog.remove",
    "dialog.rename",
    "dialog.skip",
    "dialog.sort",
    "dialog.toggle-enabled",
    "dialog.up",
    "layout.close-slot",
    "layout.disk-map",
    "layout.equalize",
    "layout.focus-next",
    "layout.focus-prev",
    "layout.grow",
    "layout.log",
    "layout.metadata",
    "layout.pick",
    "profile.pick",
    "profile.next",
    "profile.prev",
    "profile.save-as",
    "layout.places",
    "layout.preview",
    "layout.processes",
    "layout.set-target",
    "layout.shrink",
    "layout.split-h",
    "layout.split-v",
    "mark.all",
    "mark.clear",
    "mark.invert",
    "mark.pattern-add",
    "mark.pattern-remove",
    "mark.extension-add",
    "mark.extension-remove",
    "mark.files",
    "mark.dirs",
    "mark.restore",
    "mark.toggle",
    "mark.toggle-up",
    "mark.toggle-page-down",
    "mark.toggle-page-up",
    "mark.to-top",
    "mark.to-bottom",
    "nav.back",
    "nav.enter",
    "nav.forward",
    "nav.jump-back",
    "nav.parent",
    "nav.set-jump-point",
    "pane.ai-rename",
    "pane.checksum",
    "pane.checksum-verify",
    "pane.columns",
    "pane.command-line",
    "pane.copy-path",
    "pane.compare-dirs",
    "pane.compare-files",
    "pane.connect",
    "pane.copy",
    "pane.delete",
    "pane.delete-permanent",
    "pane.dir-size",
    "pane.disconnect",
    "pane.edit",
    "pane.edit-new",
    "pane.history",
    "pane.history-left",
    "pane.history-right",
    "pane.hotlist",
    "pane.mirror",
    "pane.mirror-target",
    "pane.mkdir",
    "pane.move",
    "pane.names-encoding",
    "pane.pack",
    "pane.unpack",
    "pane.test-archive",
    "pane.split-file",
    "pane.combine-files",
    "pane.open",
    "pane.popular",
    "pane.properties",
    "pane.chmod",
    "pane.pull",
    "pane.quick-search",
    "pane.refresh",
    "pane.rename",
    "pane.rename-batch",
    "pane.search",
    "pane.select-drive",
    "pane.select-drive-left",
    "pane.select-drive-right",
    "pane.semantic-search",
    "pane.sort-ext",
    "pane.sort-menu",
    "pane.sort-name",
    "pane.sort-size",
    "pane.sort-time",
    "pane.swap",
    "pane.switch",
    "pane.sync-dirs",
    "pane.tab-close",
    "pane.tab-goto-1",
    "pane.tab-goto-2",
    "pane.tab-goto-3",
    "pane.tab-goto-4",
    "pane.tab-goto-5",
    "pane.tab-goto-6",
    "pane.tab-goto-7",
    "pane.tab-goto-8",
    "pane.tab-goto-9",
    "pane.tab-move-left",
    "pane.tab-move-right",
    "pane.tab-new",
    "pane.tab-next",
    "pane.tab-prev",
    "pane.toggle-hidden",
    "pane.tree",
    "pane.view",
    "task.cancel",
    "viewer.bottom",
    "viewer.close",
    "viewer.down",
    "viewer.encoding",
    "viewer.encoding-auto",
    "viewer.hex",
    "viewer.left",
    "viewer.page-down",
    "viewer.page-up",
    "viewer.right",
    "viewer.top",
    "viewer.up",
];

/// The UI contexts a topic may claim, mirroring `norte_tui::help_context`'s
/// closed vocabulary — one id per PLACE the reader can be, which since phase
/// H3c means one per modal rather than one per screen.
///
/// Written out because `norte-help` does not depend on a frontend (rule 7 in
/// reverse: the corpus knows nothing about ratatui or GPUI). The gate that
/// crosses this with the real vocabulary is `norte-tui`'s
/// `tests/help_gate.rs`, which reads `help_context::CONTEXTS` directly; this
/// is the pin that the corpus does not drift from it in the meantime, and a
/// disagreement surfaces there as an `UnknownContext`.
const CONTEXTS: [&str; 18] = [
    "browse",
    "viewer",
    "dialog.confirm",
    "dialog.collision",
    "dialog.approval",
    "dialog.trust-host",
    "dialog.ask-secret",
    "dialog.trust-lua",
    "dialog.plugin-approval",
    "dialog.quit",
    "dialog.mark-pattern",
    "dialog.transfer-name",
    "dialog.transfer-dest",
    "dialog.mkdir",
    "dialog.command-line",
    "dialog.ai-rename",
    "dialog.semantic-search",
    "dialog.properties",
];

#[test]
fn the_shipped_corpus_has_no_integrity_issues() {
    assert_eq!(
        check_corpus(),
        Vec::new(),
        "the shipped corpus must be internally consistent"
    );
}

#[test]
fn the_shipped_corpus_mentions_exactly_the_commands_it_documents() {
    // `known` == what the corpus documents, so BOTH directions must come back
    // clean: nothing mentioned that is outside the list (no `UnknownCommand`)
    // and nothing in the list that no topic documents (no
    // `UndocumentedCommand`). One list pins both halves.
    assert_eq!(check_commands(&DOCUMENTED, &[]), Vec::new());
}

#[test]
fn the_shipped_corpus_claims_only_contexts_the_ui_has() {
    let issues = check_contexts(&CONTEXTS);

    // What this file can pin: no topic points at a place the UI does not have,
    // and no two topics fight over one place.
    let del_corpus: Vec<&Issue> = issues
        .iter()
        .filter(|i| !matches!(i, Issue::ContextWithoutTopic { .. }))
        .collect();
    assert!(del_corpus.is_empty(), "{del_corpus:?}");

    // The mirror direction: every place the reader can be has a page to open.
    // It was real debt until H3h — a list of contexts nobody had written yet,
    // named here rather than filtered away in silence — and H3h emptied it.
    // What the assertion pins now is that it STAYS empty: a context added
    // without its page fails here as well as in the frontend's gate.
    let sin_pagina: Vec<&str> = issues
        .iter()
        .filter_map(|i| match i {
            Issue::ContextWithoutTopic { context, .. } => Some(context.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        sin_pagina.is_empty(),
        "every context the UI can open must have a page: {sin_pagina:?}"
    );
}

#[test]
fn a_command_the_corpus_never_mentions_is_reported_undocumented() {
    // The shape the frontend's gate depends on: every command the vocabulary
    // has and the corpus does not mention must surface as ONE
    // `UndocumentedCommand` rather than being rounded off silently.
    //
    // The uncovered command is SYNTHETIC and not a real id. It used to be
    // `app.quit`, which broke the day H3h documented it — and a test that has
    // to be rewritten every time a page is written measures the corpus, not
    // the check. This one measures the check.
    let mut known = DOCUMENTED.to_vec();
    known.push("no.such-command");
    let issues = check_commands(&known, &[]);
    assert_eq!(
        issues,
        vec![Issue::UndocumentedCommand {
            command: "no.such-command".to_owned(),
            lang: Lang::En,
        }],
        "exactly the uncovered command, and nothing else"
    );
    assert_eq!(
        check_commands(&known, &["no.such-command"]),
        Vec::new(),
        "the allowlist silences ONLY what it enumerates"
    );
}
