//! Every Fluent key the renderer asks for EXISTS in the catalogue.
//!
//! `Screen::t` answers a missing key with the key itself, so a missing one
//! does not crash: it gets PAINTED. Discovered with `hostile-name`, which did
//! not exist and had been showing up literally in the badge of every altered
//! name for a while — on eight surfaces, and with no test saying anything,
//! because the renderer tests' catalogue is a fixture that made it up.
//!
//! The test reads the TypeScript, which is the only source of truth for what
//! the painter asks for: a hand-written list would drift from it at the
//! first new surface.

use std::collections::BTreeSet;

/// The renderer files that ask for keys.
///
/// `main.ts` too: it calls `screen.t(...)`, and looking only at `render.ts`
/// left out everything painted from startup.
const SOURCES: &[(&str, &str)] = &[
    ("render.ts", include_str!("../ui/src/render.ts")),
    ("main.ts", include_str!("../ui/src/main.ts")),
];

/// The ways of asking for a key. All THREE, not one.
///
/// The sweep used to look only at `this.t("` with a double quote, so it did
/// not see the free function `tr(...)` nor the templates. `task-foreign`
/// slipped through there, which exists in no catalogue and which this test
/// used to call green.
const CALLS: &[&str] = &["this.t(", "screen.t(", "tr("];

/// The keys the renderer asks the catalogue for.
///
/// A call site whose argument is NOT a literal is required to be registered
/// in [`COMPOSED`]: silently ignoring it is what let the templates through.
fn keys_requested() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (name, source) in SOURCES {
        for call in CALLS {
            let mut from = 0usize;
            while let Some(i) = source[from..].find(call) {
                let start = from + i + call.len();
                from = start;
                // The WHOLE argument, up to the closing parenthesis: a
                // `t(a ? "x" : "y")` asks for TWO keys, and keeping only the
                // first — or none — is the usual hole.
                let arg = argumento(&source[start..]);
                let literales = quotes(arg);
                if !literales.is_empty() {
                    out.extend(literales);
                    continue;
                }
                // A template or a variable: only valid if it is declared as
                // composite, or if the key is chosen by RUST — and then
                // `norte-ui-host/tests/catalogo_del_host.rs` checks it, which
                // reads the host's code for the same reason this one reads
                // the renderer's.
                assert!(
                    COMPOSED.iter().any(|(p, _)| arg.contains(p))
                        || DEL_HOST.iter().any(|v| arg.starts_with(v)),
                    "{name}: `{call}{arg}` asks for a key this test \
                     cannot resolve and that is not in `COMPUESTAS`. A key \
                     the sweep does not see gets painted as its own \
                     identifier the day it is missing, and that is exactly \
                     what this test is here to prevent."
                );
            }
        }
    }
    out
}

/// A call's argument, from right after its `(` to the `)` that closes it.
fn argumento(rest: &str) -> &str {
    let mut level = 1i32;
    for (i, c) in rest.char_indices() {
        match c {
            '(' => level += 1,
            ')' => {
                level -= 1;
                if level == 0 {
                    return &rest[..i];
                }
            }
            _ => {}
        }
    }
    rest
}

/// The double-quoted literals in a chunk of TypeScript.
fn quotes(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find('"') {
        rest = &rest[i + 1..];
        let Some(fin) = rest.find('"') else {
            break;
        };
        out.push(rest[..fin].to_owned());
        rest = &rest[fin + 1..];
    }
    out
}

/// The arguments whose key is CHOSEN by the host, in Rust.
///
/// It is not an exception: it is a handoff. These keys are checked by
/// `norte-ui-host/tests/catalogo_del_host.rs`, reading the host's code, the
/// same way this test reads the renderer's. Naming them here forces adding a
/// new way of receiving a key from the host to go through both files; the
/// previous sweep simply did not see them.
const DEL_HOST: &[&str] = &[
    "ack.reason_key",
    "out.notice.key",
    "slot.state.reason_key",
    "top.title_key",
    // A program's output pane's title (#312): the host chooses it among its
    // own literals, which the host's sweep does follow.
    "output.title_key",
    "c.label_key",
    // `taskNode` receives the translator and composes `gui-task-kind-…`,
    // which is in `COMPOSED`.
    "k",
];

/// The ones composed with a variable suffix, with their possible values.
///
/// By hand and with their value: these are the only ones a `grep` cannot
/// resolve, and leaving them out would be the same hole this test is here to
/// close.
const COMPOSED: &[(&str, &[&str])] = &[
    ("help-callout-", &["note", "warn", "tip"]),
    // The log panel's level controls (#326). The suffix is `LogLevel::wire`'s
    // CLOSED vocabulary, and this list is the other half: a new level there
    // breaks here, which is where it needs to be noticed that it is missing
    // its string.
    ("log-level-", &["error", "warn", "info", "debug", "trace"]),
    // The suffix is `TaskView::kind`, produced by `task_class` in the host
    // with an exhaustive `match`: this list is that `match`'s other half,
    // and a new `TaskKind` variant breaks there first.
    (
        "gui-task-kind-",
        &[
            "copy",
            "move",
            "delete",
            "undo",
            "search",
            "mkdir",
            "index",
            "embed",
            "rename-batch",
            "compare",
            "dir-size",
            "pack",
            "test-archive",
            "split",
            "combine",
            "sync-plan",
            "sync",
            "unknown",
        ],
    ),
];

#[test]
fn the_renderer_does_not_ask_for_any_key_that_does_not_exist() {
    let existentes: BTreeSet<String> = [norte_i18n::Lang::En, norte_i18n::Lang::Es]
        .into_iter()
        .flat_map(norte_i18n::message_ids)
        .collect();

    let mut missing: Vec<String> = keys_requested()
        .into_iter()
        .filter(|k| !existentes.contains(k))
        .collect();
    for (prefix, suffixes) in COMPOSED {
        for s in *suffixes {
            let key = format!("{prefix}{s}");
            if !existentes.contains(&key) {
                missing.push(key);
            }
        }
    }
    assert!(
        missing.is_empty(),
        "the renderer paints these keys as-is, because they are not in the \
         catalogue: {missing:?}"
    );
}

/// And the catalogue's two halves say the same thing: a key that is only in
/// one language is a window that speaks two.
#[test]
fn both_languages_have_the_same_keys() {
    let en: BTreeSet<String> = norte_i18n::message_ids(norte_i18n::Lang::En)
        .into_iter()
        .collect();
    let es: BTreeSet<String> = norte_i18n::message_ids(norte_i18n::Lang::Es)
        .into_iter()
        .collect();
    let missing_in_spanish: Vec<&String> = en.difference(&es).collect();
    let missing_in_english: Vec<&String> = es.difference(&en).collect();
    assert!(
        missing_in_spanish.is_empty() && missing_in_english.is_empty(),
        "only in English: {missing_in_spanish:?}; only in Spanish: {missing_in_english:?}"
    );
}
