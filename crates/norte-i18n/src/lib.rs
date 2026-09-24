//! UI strings via Fluent (CLAUDE.md convention, issue #1): EMBEDDED es/en
//! catalogues with full parity verified by test. A missing id falls back to
//! the id itself (visible and greppable), never panics.
#![forbid(unsafe_code)]

use std::sync::OnceLock;

use fluent::{FluentArgs, FluentResource, concurrent::FluentBundle};
use unic_langid::LanguageIdentifier;

/// Supported languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    /// Spanish.
    Es,
    /// English (fallback).
    En,
}

impl Lang {
    /// Negotiates from a `LANG`/`LC_MESSAGES`-style value (`es_ES.UTF-8`).
    /// Unknown or absent → English.
    #[must_use]
    pub fn negotiate(env: Option<&str>) -> Self {
        match env {
            Some(v) if v.to_ascii_lowercase().starts_with("es") => Self::Es,
            _ => Self::En,
        }
    }

    /// Negotiates from the process environment: `NORTE_LANG` >
    /// `LC_ALL` > `LC_MESSAGES` > `LANG`.
    #[must_use]
    pub fn from_env() -> Self {
        for var in ["NORTE_LANG", "LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(v) = std::env::var(var)
                && !v.is_empty()
            {
                return Self::negotiate(Some(&v));
            }
        }
        Self::En
    }

    fn ftl(self) -> &'static str {
        match self {
            Self::Es => include_str!("../i18n/es.ftl"),
            Self::En => include_str!("../i18n/en.ftl"),
        }
    }

    fn langid(self) -> LanguageIdentifier {
        match self {
            Self::Es => "es".parse().expect("constant langid"),
            Self::En => "en".parse().expect("constant langid"),
        }
    }
}

fn bundle(lang: Lang) -> &'static FluentBundle<FluentResource> {
    static ES: OnceLock<FluentBundle<FluentResource>> = OnceLock::new();
    static EN: OnceLock<FluentBundle<FluentResource>> = OnceLock::new();
    let cell = match lang {
        Lang::Es => &ES,
        Lang::En => &EN,
    };
    cell.get_or_init(|| {
        // The embedded .ftl files are validated by the suite: an error here
        // is a broken build, not the user's runtime.
        let res = FluentResource::try_new(lang.ftl().to_owned()).unwrap_or_else(|(res, _)| res);
        let mut b = FluentBundle::new_concurrent(vec![lang.langid()]);
        let _ = b.add_resource(res);
        // No bidi isolation marks: the UI is a terminal.
        b.set_use_isolating(false);
        b
    })
}

/// The process's global language (set by the frontend at startup).
static GLOBAL: OnceLock<Lang> = OnceLock::new();

/// Sets the global language. Only the FIRST call wins; `false` if it was
/// already set (or used) with a different value.
pub fn force(lang: Lang) -> bool {
    GLOBAL.set(lang).is_ok() || GLOBAL.get() == Some(&lang)
}

fn global() -> Lang {
    *GLOBAL.get_or_init(Lang::from_env)
}

/// The current global language: the one [`force`] set, or the
/// environment's if nobody set it.
///
/// Exists for callers who need to translate with [`t_in`] in the language
/// [`t`] would use — a resolver that stores the language in a field, for
/// example. Without this they had to re-derive the negotiation on their
/// own, and two derivations of the same fact end up disagreeing: the UI in
/// one language and a table inside it in another.
///
/// NOTE: reading it SETS the language if nobody had set it (`get_or_init`),
/// same as translating. Calling it before [`force`] makes that later
/// `force` return `false` unless it matches.
///
/// ```
/// let lang = norte_i18n::active();
/// assert_eq!(norte_i18n::t("help-title"), norte_i18n::t_in(lang, "help-title"));
/// ```
#[must_use]
pub fn active() -> Lang {
    global()
}

/// Translates `id` in the global language.
#[must_use]
pub fn t(id: &str) -> String {
    t_in(global(), id)
}

/// Translates `id` with args in the global language.
#[must_use]
pub fn ta(id: &str, args: &[(&str, &str)]) -> String {
    ta_in(global(), id, args)
}

/// Translates `id` in a specific language (tests and previews).
#[must_use]
pub fn t_in(lang: Lang, id: &str) -> String {
    ta_in(lang, id, &[])
}

/// Translates with args in a specific language. Missing id → the id itself.
#[must_use]
pub fn ta_in(lang: Lang, id: &str, args: &[(&str, &str)]) -> String {
    let b = bundle(lang);
    let Some(msg) = b.get_message(id) else {
        return id.to_owned();
    };
    let Some(pattern) = msg.value() else {
        return id.to_owned();
    };
    let mut fargs = FluentArgs::new();
    for (k, v) in args {
        fargs.set(*k, *v);
    }
    let mut errors = Vec::new();
    b.format_pattern(pattern, Some(&fargs), &mut errors)
        .into_owned()
}

/// All of a locale's message ids (for the parity test).
#[must_use]
pub fn message_ids(lang: Lang) -> Vec<String> {
    use fluent_syntax::ast::Entry;
    let res = FluentResource::try_new(lang.ftl().to_owned()).unwrap_or_else(|(res, _)| res);
    res.entries()
        .filter_map(|e| match e {
            Entry::Message(m) => Some(m.id.name.to_owned()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod sin_duplicados {
    use std::collections::BTreeSet;

    /// No key is defined twice.
    ///
    /// Fluent keeps the FIRST definition and SILENTLY drops the second, so
    /// a duplicate is a translation someone wrote, that the file shows, and
    /// that nobody will ever read. There was one
    /// (`layout-picker-factory`, with two different English texts) and an
    /// audit found it, not the catalogue.
    #[test]
    fn no_key_is_defined_twice() {
        for (lang, source) in [
            ("en", include_str!("../i18n/en.ftl")),
            ("es", include_str!("../i18n/es.ftl")),
        ] {
            let mut seen: BTreeSet<&str> = BTreeSet::new();
            let mut repeated: Vec<&str> = Vec::new();
            for line in source.lines() {
                // A definition starts at column zero; a continuation is
                // indented and a comment carries `#`.
                let Some((id, _)) = line.split_once(" = ") else {
                    continue;
                };
                if id.starts_with([' ', '#', '.', '*', '[']) || id.is_empty() {
                    continue;
                }
                if !seen.insert(id) {
                    repeated.push(id);
                }
            }
            assert!(
                repeated.is_empty(),
                "{lang}.ftl defines twice: {repeated:?} — Fluent keeps \
                 the first and silently drops the other"
            );
        }
    }
}
