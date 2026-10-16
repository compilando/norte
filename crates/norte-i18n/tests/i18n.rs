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

/// Whoever downloads norte without a Spanish locale reads English: no
/// locale, the C one, POSIX, or one norte does not speak. A word that merely
/// STARTS like `es` is not Spanish either.
#[test]
fn without_a_spanish_locale_the_language_is_english() {
    for v in [
        "C",
        "C.UTF-8",
        "POSIX",
        "",
        "en_GB.UTF-8",
        "fr_FR",
        "et_EE.UTF-8",
    ] {
        assert_eq!(Lang::negotiate(Some(v)), Lang::En, "{v:?}");
    }
    assert_eq!(Lang::negotiate(Some("es_MX.UTF-8@euro")), Lang::Es);
    assert_eq!(
        Lang::resolve(None, None, None, Lang::negotiate(None)),
        Lang::En
    );
}

/// The English catalogue carries no Spanish: an English reader who sees
/// `¿` or `ñ` is reading a string someone pasted into the wrong file.
///
/// Letters with accents alone are not the sign — `café` and `Tromsø` are
/// example names — so the check is the marks only Spanish prose uses, with
/// the keys that quote them on purpose listed.
#[test]
fn the_english_catalogue_has_no_spanish() {
    const QUOTES_ON_PURPOSE: &[&str] = &["language-name"];
    let source = include_str!("../i18n/en.ftl");
    let spanish: Vec<&str> = source
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter(|l| {
            let key = l.split(" = ").next().unwrap_or_default();
            !QUOTES_ON_PURPOSE.contains(&key)
        })
        .filter(|l| l.contains(['¿', '¡', 'ñ', 'Ñ']))
        .collect();
    assert!(spanish.is_empty(), "Spanish in en.ftl: {spanish:?}");
}

/// An extra language is a whole interface even half translated: what it
/// lacks is English, a bad file name or a built-in tag is refused, and the
/// locale finds it by tag or by language.
#[test]
fn an_extra_language_falls_back_to_english() {
    let notices = norte_i18n::install_extras(vec![
        (
            "pt-BR".into(),
            format!(
                "language-name = Português\nmodal-trash-title = Para a lixeira\n\
                 help-title = \u{1b}]0;pwned\u{7}Aj\u{202e}uda\n\
                 modal-trash-note = {{ \"\\u001B]0;owned\\u0007\" }}x{{ \"\\u202E\" }}{{ \"\\u200F\" }}y\n\
                 amp-a = {}\n\
                 amp-b = {}\n",
                "x".repeat(6000),
                "{amp-a}".repeat(4)
            ),
        ),
        ("../evil".into(), "x = y\n".into()),
        ("es".into(), "modal-trash-title = secuestrado\n".into()),
        ("de".into(), "modal-trash-title = { broken\n".into()),
    ]);
    let problems: Vec<_> = notices
        .iter()
        .map(|n| (n.tag.as_str(), &n.problem))
        .collect();
    assert_eq!(
        problems,
        vec![
            ("../evil", &norte_i18n::Problem::BadTag),
            ("es", &norte_i18n::Problem::BuiltIn),
            ("de", &norte_i18n::Problem::Syntax { errors: 1 }),
        ]
    );
    let pt = Lang::from_flag("pt-br").expect("installed");
    assert_eq!(t_in(pt, "modal-trash-title"), "Para a lixeira");
    // A downloaded translation cannot drive the terminal: the escape that
    // would retitle it and the bidi override are gone, the text stays.
    assert_eq!(t_in(pt, "help-title"), "]0;pwnedAjuda");
    // Nor through Fluent's own `\u` escapes, decoded only when formatting.
    assert_eq!(t_in(pt, "modal-trash-note"), "]0;ownedxy");
    // A message built from copies of a big one is cut, not multiplied.
    assert!(t_in(pt, "amp-b").len() <= 8 * 1024);
    assert_eq!(
        t_in(pt, "modal-trash-subtitle-none-such"),
        "modal-trash-subtitle-none-such"
    );
    assert_eq!(
        t_in(pt, "modal-delete-title"),
        t_in(Lang::En, "modal-delete-title"),
        "what it lacks is English"
    );
    assert_eq!(
        t_in(Lang::Es, "modal-trash-title"),
        "A la papelera",
        "es untouched"
    );
    assert_eq!(Lang::negotiate(Some("pt_BR.UTF-8")), pt);
    let de = Lang::from_flag("de").expect("installed in part");
    assert_eq!(
        de.native_name(),
        "de",
        "no name of its own: its tag, not English's"
    );
    assert_eq!(pt.tag(), "pt-BR");
    assert_eq!(Lang::all().len(), 4);
}
