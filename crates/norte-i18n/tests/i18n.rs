//! i18n tests (issue #1): FULL id parity between locales, visible fallback
//! (never panic) and language negotiation.

use norte_i18n::{Lang, message_ids, t_in, ta_in};

#[test]
fn both_locales_have_exactly_the_same_ids() {
    let es = message_ids(Lang::Es);
    let en = message_ids(Lang::En);
    let missing_in_english: Vec<_> = es.iter().filter(|i| !en.contains(i)).collect();
    let missing_in_spanish: Vec<_> = en.iter().filter(|i| !es.contains(i)).collect();
    assert!(
        missing_in_english.is_empty() && missing_in_spanish.is_empty(),
        "untranslated ids — missing in en.ftl: {missing_in_english:?}; missing in es.ftl: {missing_in_spanish:?}"
    );
    assert!(!es.is_empty(), "the catalogue cannot be empty");
}

/// Every LITERAL key the code requests has to exist in the catalogue.
///
/// The gap this covers: `t("unknown-id")` does not fail, it falls back to
/// the id itself —that is deliberate, a missing string never crashes the
/// app— and what gets painted is `msg-organize-hidden` in the status bar.
/// The parity between locales above does not catch it either: a key that is
/// in NEITHER of the two is equally absent from both. So the only way to
/// find out was to look at the screen, and a bar that only shows up in a
/// rare case is one nobody looks at. It happened with `msg-organize-hidden`
/// in phase 8.
///
/// Only literal keys, and on purpose: the ones that are COMPOSED
/// (`help-cmd-` + the command name, the RPC catalogue's ids) have their own
/// gates —the CLI golden paints all of them— and chasing them here would
/// require evaluating the code instead of reading it.
#[test]
fn every_literal_key_in_the_code_exists_in_the_catalogue() {
    let ids = message_ids(Lang::Es);
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/");
    let mut missing: Vec<(String, String)> = Vec::new();
    let mut seen = 0usize;
    for crate_dir in std::fs::read_dir(root).expect("crates/") {
        let src = crate_dir.expect("entry").path().join("src");
        if !src.is_dir() {
            continue;
        }
        for file in rust_files(&src) {
            let text = std::fs::read_to_string(&file).expect("utf-8 source");
            for key in keys_of(&text) {
                seen += 1;
                if !ids.contains(&key) {
                    missing.push((file.display().to_string(), key));
                }
            }
        }
    }
    assert!(seen > 100, "the sweep found no keys: {seen}");
    assert!(
        missing.is_empty(),
        "keys the code requests that the catalogue does not have: {missing:?}"
    );
}

/// Every `.rs` under `dir`, recursively.
fn rust_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(d) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                pending.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    out
}

/// The literal keys of `t("…")` / `ta("…", …)` and their `_in` variants.
///
/// A `t(key)` with a variable does not match —there is no literal to
/// read— and that is what keeps the sweep free of false positives.
fn keys_of(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for call in ["t(\"", "ta(\"", "t_in(self.lang, \"", "ta_in(self.lang, \""] {
        let mut rest = text;
        while let Some(i) = rest.find(call) {
            // The call must not be the tail end of another identifier
            // (`format(`, `debug_assert(`…): what precedes it must not be
            // part of a name.
            let before = rest[..i].chars().next_back();
            let clean = before.is_none_or(|c| !c.is_alphanumeric() && c != '_' && c != ':');
            let after = &rest[i + call.len()..];
            if clean && let Some(end) = after.find('"') {
                let key = &after[..end];
                // A real key is kebab-case: this way any random string
                // starting with `t("` does not get in.
                if !key.is_empty()
                    && key
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                {
                    out.push(key.to_owned());
                }
            }
            rest = after;
        }
    }
    out
}

#[test]
fn translates_in_both_languages() {
    assert_eq!(t_in(Lang::Es, "modal-trash-title"), "A la papelera");
    assert_eq!(t_in(Lang::En, "modal-trash-title"), "To trash");
}

#[test]
fn unknown_id_falls_back_to_the_id_itself_without_panic() {
    // An id with a typo IS VISIBLE in the UI (greppable), never crashes.
    assert_eq!(t_in(Lang::Es, "made-up-id-xyz"), "made-up-id-xyz");
}

#[test]
fn fluent_args() {
    let msg = ta_in(Lang::Es, "msg-error", &[("error", "not found")]);
    assert!(msg.contains("not found"), "{msg}");
}

#[test]
fn language_negotiation() {
    assert_eq!(Lang::negotiate(Some("es_ES.UTF-8")), Lang::Es);
    assert_eq!(Lang::negotiate(Some("es")), Lang::Es);
    assert_eq!(Lang::negotiate(Some("en_US.UTF-8")), Lang::En);
    assert_eq!(Lang::negotiate(Some("de_DE")), Lang::En, "en fallback");
    assert_eq!(Lang::negotiate(None), Lang::En);
}
