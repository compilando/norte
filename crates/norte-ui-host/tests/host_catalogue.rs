//! Every Fluent key the HOST chooses EXISTS in both catalogues.
//!
//! The Rust twin of `norte-gui-tauri/tests/full_catalogue.rs`, and it was
//! needed because that one only sees what the TypeScript requests. These
//! keys are chosen by the host — `ActionAck::Unavailable { reason_key }`, a
//! dialog's title, each response's label — and they travel across the
//! bridge as data, so the renderer paints them with `t(key)` without them
//! appearing in any literal of its own.
//!
//! TWENTY-ONE were missing, among them the two responses of the dialog where
//! a human approves the mutation an agent requested: the buttons were
//! painting `dialog-approve` and `dialog-deny`. `t` answers an absent key
//! with the key itself, so nothing crashes — it just reads oddly.
//!
//! The test reads the CODE and not a hand-written list: a list drifts away
//! from the code at the first new surface, which is exactly what happened.

use std::collections::{BTreeMap, BTreeSet};

/// The host's files where keys are chosen: ALL of `src/`, walked.
///
/// It used to be a list of five `include_str!`. When `controller.rs` was
/// split into thirty-three files (ADR 0086) the list was left naming one
/// that no longer existed, and maintaining it by hand would have stopped
/// looking at the thirty-two new ones without saying a word — which is
/// exactly the drift this file's header says must be avoided. The tree is
/// walked: a new file joins on its own.
fn sources() -> Vec<(String, String)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
            .map(|e| e.expect("entry").path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .expect("name")
                    .to_owned();
                let text = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
                out.push((name, text));
            }
        }
    }
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    walk(&src, &mut out);
    assert!(
        out.len() >= 30,
        "the sweep has to see the whole host, and it sees {}",
        out.len()
    );
    out
}

/// The fields whose value IS a Fluent key.
const FIELDS: &[&str] = &["reason_key", "title_key", "label_key"];

/// The CALLS that translate a key on the spot.
///
/// The fields above only see the keys that TRAVEL to the renderer. One the
/// host translates itself — for the status bar, for a line in a dialog —
/// does not go through any `*_key` field, so this sweep did not see it:
/// `err-bad-name` had gone since phase 2 without existing in any language,
/// and confirming an illegal name put the raw identifier on the bar.
const CALLS: &[&str] = &[
    "norte_i18n::t(",
    "norte_i18n::t_in(",
    "norte_i18n::ta(",
    "norte_i18n::ta_in(",
];

/// The SHARED functions that return a key, with their file.
///
/// The host does not write them, it calls them, so the literal sweep does
/// not see them. All three are `match` blocks closed over `&'static str`, so
/// their WHOLE vocabulary is in their body and can be checked the same way.
const INDIRECT: &[(&str, &str, &str)] = &[
    (
        "error.rs",
        include_str!("../../norte-frontend/src/error.rs"),
        "pub fn error_key(",
    ),
    (
        "availability.rs",
        include_str!("../../norte-frontend/src/availability.rs"),
        "pub fn reason_key(",
    ),
    (
        "nav.rs",
        include_str!("../../norte-frontend/src/nav.rs"),
        "pub fn empty_message(",
    ),
];

/// What is accepted as a NON-literal key at a spot in the host.
///
/// Each one is covered by `INDIRECT` or by another spot in the sweep itself,
/// and it is named here so that adding a fourth way to compute a key fails
/// instead of slipping through.
const COMPUTED: &[&str] = &[
    "error::error_key",
    "availability::reason_key",
    "empty_message()",
    // Returned by `choose_page`, and it is one of `availability`'s.
    "reason_key: key",
    // The reason a dialog response did NOTHING. It comes from
    // `bytes_del_rename` / `segment_typed`, which return literal keys
    // and so this sweep DOES see them at their origin.
    "reason_key: reason_key",
];

/// How much text is looked at after a field to find its literals.
///
/// A three-arm `match` or an `if/else` fit comfortably; the cutoff is there
/// so a field with no literal is detected instead of swallowing the rest of
/// the file.
const WINDOW: usize = 600;

/// Trims `source` up to `upto` without splitting a character.
///
/// The sweep cuts by BYTES — a 600 window after a call — and this host's
/// code is commented in Spanish: a dash or an accent straddling the cut
/// would panic the test, which is a harness failure and not the host's.
/// Backs up to the nearest character boundary.
fn upto_boundary(source: &str, upto: usize) -> usize {
    let mut end = upto.min(source.len());
    while end > 0 && !source.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// The same from the other end: advances to a character boundary.
fn from_boundary(source: &str, from: usize) -> usize {
    let mut start = from.min(source.len());
    while start < source.len() && !source.is_char_boundary(start) {
        start += 1;
    }
    start
}

/// A literal that could be a Fluent key: lowercase, digits and dashes, with
/// at least one dash. Discards paths, formats and kind names.
fn looks_like_key(s: &str) -> bool {
    s.contains('-')
        && !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

/// Is what follows the field a TYPE, meaning this declares rather than
/// chooses?
///
/// The types a key field can carry are few and all start with an uppercase
/// letter or with `&`; a chosen value starts with a quote, with `self`, with
/// a call or with a `match`. Only the first token is looked at.
fn is_declaration(window: &str) -> bool {
    let t = window.trim_start();
    ["String", "Option<", "Cow<", "&'static str", "&str"]
        .iter()
        .any(|ty| t.starts_with(ty))
}

/// The keys the host chooses, by file and site.
fn chosen_keys() -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let sources = sources();
    for (name, source) in &sources {
        for field in FIELDS {
            let needle = format!("{field}:");
            let mut from = 0usize;
            while let Some(i) = source[from..].find(&needle) {
                let start = from + i + needle.len();
                from = start;
                let end = upto_boundary(source, start + WINDOW);
                let window = &source[start..end];
                // DECLARING the field is not CHOOSING a key. Since the sweep
                // looks at all of `src/` it also sees the type's definition
                // (`reason_key: String` in `bridge.rs`), and a type has no
                // literal to follow. It is recognized because what follows
                // is a TYPE: in a choice, what follows is a literal, a
                // `self.` or a call, never `String` nor `Option<`.
                if is_declaration(window) {
                    continue;
                }
                let found: Vec<String> = literals(window)
                    .into_iter()
                    .filter(|s| looks_like_key(s))
                    .collect();
                let line = source[..start].lines().count();
                if found.is_empty() {
                    // A field whose value is computed only counts if it is
                    // computed by something this test DOES look at.
                    // With the field's name in front: it is part of the
                    // shape being recognized.
                    let from_field = from_boundary(source, start.saturating_sub(needle.len() + 2));
                    let header = &source[from_field..upto_boundary(source, start + 80)];
                    assert!(
                        COMPUTED.iter().any(|c| header.contains(c)),
                        "{name}:{line}: `{field}` with no literal and not in \
                         `COMPUTED`. A key this test cannot follow is a key \
                         that will paint as its own identifier the day it is \
                         missing: either it is a literal, or its origin is \
                         named here."
                    );
                    continue;
                }
                out.entry(format!("{name}:{line}"))
                    .or_default()
                    .extend(found);
            }
        }
    }
    // The ones the host translates on the spot. The FIRST literal that looks
    // like a key within the window is taken: `t_in` carries the language in
    // front and `ta_in` the arguments behind, so the first one is always the
    // id.
    for (name, source) in &sources {
        for call in CALLS {
            let mut from = 0usize;
            while let Some(i) = source[from..].find(call) {
                let start = from + i + call.len();
                from = start;
                let end = upto_boundary(source, start + WINDOW);
                let found: Vec<String> = literals(&source[start..end])
                    .into_iter()
                    .filter(|s| looks_like_key(s))
                    .take(1)
                    .collect();
                if found.is_empty() {
                    // A computed key: a variable carries it, and its origin
                    // has to be something this test DOES look at.
                    continue;
                }
                let line = source[..start].lines().count();
                out.entry(format!("{name}:{line}"))
                    .or_default()
                    .extend(found);
            }
        }
    }
    for (name, source, signature) in INDIRECT {
        let i = source
            .find(signature)
            .unwrap_or_else(|| panic!("{name}: `{signature}` is not there anymore"));
        let body = &source[i..];
        let end = body.find("\n}").unwrap_or(body.len());
        let keys: Vec<String> = literals(&body[..end])
            .into_iter()
            .filter(|s| looks_like_key(s))
            .collect();
        assert!(
            !keys.is_empty(),
            "{name}: `{signature}` returns no literal key; the sweep stopped \
             seeing its vocabulary"
        );
        let line = source[..i].lines().count();
        out.entry(format!("{name}:{line}"))
            .or_default()
            .extend(keys);
    }
    out
}

/// The string literals in a chunk of Rust, without interpreting escapes:
/// here there are only ASCII keys.
fn literals(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'"' {
            let mut j = i + 1;
            while j < b.len() && b[j] != b'"' {
                if b[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            if j < b.len() {
                out.push(s[i + 1..j].to_owned());
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}

#[test]
fn the_host_chooses_no_key_that_does_not_exist() {
    let en: BTreeSet<String> = norte_i18n::message_ids(norte_i18n::Lang::En)
        .into_iter()
        .collect();
    let es: BTreeSet<String> = norte_i18n::message_ids(norte_i18n::Lang::Es)
        .into_iter()
        .collect();
    let mut missing: Vec<String> = Vec::new();
    for (site, keys) in chosen_keys() {
        for k in keys {
            if !en.contains(&k) || !es.contains(&k) {
                missing.push(format!(
                    "{site}: `{k}` (en={} es={})",
                    en.contains(&k),
                    es.contains(&k)
                ));
            }
        }
    }
    missing.sort();
    missing.dedup();
    assert!(
        missing.is_empty(),
        "the host chooses keys the catalogue does not have, and the \
         renderer PAINTS them as-is:\n{}",
        missing.join("\n")
    );
}

/// And the test looks at itself: if it stops finding keys, it stops serving
/// without saying a word.
#[test]
fn the_sweep_finds_something_to_check() {
    let sites = chosen_keys();
    assert!(
        sites.len() > 20,
        "the sweep found {} sites: either the host changed how it names its \
         keys, or this test no longer looks where they are",
        sites.len()
    );
}
