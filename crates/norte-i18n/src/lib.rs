//! UI strings via Fluent (CLAUDE.md convention, issue #1): EMBEDDED es/en
//! catalogues with full parity verified by test, plus the EXTRA languages a
//! reader drops into `<config>/locales/` (ADR 0188). A missing id falls back
//! to English, and only then to the id itself (visible and greppable); it
//! never panics.
#![forbid(unsafe_code)]

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

use fluent::{FluentArgs, FluentResource, concurrent::FluentBundle};
use unic_langid::LanguageIdentifier;

/// Supported languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    /// Spanish.
    Es,
    /// English (fallback).
    En,
    /// A language installed from a `.ftl` file at startup
    /// ([`install_extras`]). Whatever it does not translate is English.
    Extra(Extra),
}

/// An installed extra language: an index into the process's registry, so
/// [`Lang`] stays `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Extra(u8);

impl Lang {
    /// Negotiates from a `LANG`/`LC_MESSAGES`-style value (`es_ES.UTF-8`) or
    /// a tag (`pt-BR`). Unknown or absent → English.
    ///
    /// An installed extra matches by its whole tag first and then by its
    /// language alone, so `pt_BR.UTF-8` finds `pt-BR.ftl` or else `pt.ftl`.
    ///
    /// ```
    /// use norte_i18n::Lang;
    /// assert_eq!(Lang::negotiate(Some("es_ES.UTF-8")), Lang::Es);
    /// assert_eq!(Lang::negotiate(Some("C.UTF-8")), Lang::En);
    /// assert_eq!(Lang::negotiate(None), Lang::En);
    /// ```
    #[must_use]
    pub fn negotiate(env: Option<&str>) -> Self {
        let Some(v) = env else {
            return Self::En;
        };
        // `es_ES.UTF-8@euro` → `es-es`: the encoding and the modifier say
        // nothing about the language.
        let tag = v
            .split(['.', '@'])
            .next()
            .unwrap_or_default()
            .replace('_', "-")
            .to_ascii_lowercase();
        let primary = tag.split('-').next().unwrap_or_default();
        match primary {
            "es" => return Self::Es,
            "en" => return Self::En,
            _ => {}
        }
        let extras = extras();
        let by = |want: &str| {
            extras
                .iter()
                .position(|e| e.tag.eq_ignore_ascii_case(want))
                .and_then(|i| u8::try_from(i).ok())
                .map(|i| Self::Extra(Extra(i)))
        };
        by(&tag).or_else(|| by(primary)).unwrap_or(Self::En)
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

    /// The value of a `--lang` flag: `es`, `en` or the tag of an installed
    /// extra, in any case.
    ///
    /// `None` for anything else. Unlike [`Self::negotiate`], a value the
    /// reader typed on the command line is not guessed at: `--lang fr` with
    /// no `fr.ftl` is an error to report, not a quiet English.
    ///
    /// ```
    /// use norte_i18n::Lang;
    /// assert_eq!(Lang::from_flag("ES"), Some(Lang::Es));
    /// assert_eq!(Lang::from_flag("en"), Some(Lang::En));
    /// assert_eq!(Lang::from_flag("es_ES"), None);
    /// assert_eq!(Lang::from_flag("fr"), None);
    /// ```
    #[must_use]
    pub fn from_flag(value: &str) -> Option<Self> {
        Self::all()
            .into_iter()
            .find(|l| l.tag().eq_ignore_ascii_case(value))
    }

    /// The language of one run: **`--lang` > `NORTE_LANG` > `[ui] lang` >
    /// the system's locale**, the same rule in every binary.
    ///
    /// What the environment says comes in as arguments (`norte_lang` is
    /// `NORTE_LANG`'s value, `locale` is [`Self::from_env`]), so the rule is
    /// testable: `std::env::set_var` is `unsafe` since the 2024 edition and
    /// rule 5 forbids it. An empty `NORTE_LANG` counts as unset.
    ///
    /// ```
    /// use norte_i18n::Lang;
    /// // The flag beats everything.
    /// assert_eq!(Lang::resolve(Some(Lang::Es), Some("en"), Some("en"), Lang::En), Lang::Es);
    /// // NORTE_LANG beats the configuration.
    /// assert_eq!(Lang::resolve(None, Some("en"), Some("es"), Lang::Es), Lang::En);
    /// // The configuration beats the locale.
    /// assert_eq!(Lang::resolve(None, Some(""), Some("es"), Lang::En), Lang::Es);
    /// // With nothing said, the locale.
    /// assert_eq!(Lang::resolve(None, None, None, Lang::Es), Lang::Es);
    /// ```
    #[must_use]
    pub fn resolve(
        flag: Option<Self>,
        norte_lang: Option<&str>,
        config: Option<&str>,
        locale: Self,
    ) -> Self {
        pick(pinned(flag, norte_lang), config, locale)
    }

    /// The tag: `es`, `en`, or the extra's (`pt-BR`). What `[ui] lang`,
    /// `--lang` and the window's `<html lang>` carry.
    ///
    /// ```
    /// assert_eq!(norte_i18n::Lang::Es.tag(), "es");
    /// ```
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Self::Es => "es",
            Self::En => "en",
            Self::Extra(e) => extras().get(usize::from(e.0)).map_or("en", |x| x.tag),
        }
    }

    /// The language's name IN ITSELF (`Español`, `English`), for a chooser
    /// read by someone who may not read the current one. An extra that does
    /// not say its name is called by its tag, never by English's name.
    ///
    /// ```
    /// assert_eq!(norte_i18n::Lang::Es.native_name(), "Español");
    /// ```
    #[must_use]
    pub fn native_name(self) -> String {
        format_own(self, "language-name", &[]).unwrap_or_else(|| self.tag().to_owned())
    }

    /// Every language this process can speak: English, Spanish, then the
    /// installed extras in the order they were installed.
    ///
    /// ```
    /// use norte_i18n::Lang;
    /// assert_eq!(&Lang::all()[..2], &[Lang::En, Lang::Es]);
    /// ```
    #[must_use]
    pub fn all() -> Vec<Self> {
        let mut out = vec![Self::En, Self::Es];
        out.extend(
            (0..extras().len())
                .filter_map(|i| u8::try_from(i).ok())
                .map(|i| Self::Extra(Extra(i))),
        );
        out
    }

    fn code(self) -> u8 {
        match self {
            Self::En => 1,
            Self::Es => 2,
            Self::Extra(e) => e.0.saturating_add(3),
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => None,
            1 => Some(Self::En),
            2 => Some(Self::Es),
            n => Some(Self::Extra(Extra(n - 3))),
        }
    }
}

/// What the run fixed above the configuration: `--lang`, else `NORTE_LANG`.
fn pinned(flag: Option<Lang>, norte_lang: Option<&str>) -> Option<Lang> {
    flag.or_else(|| {
        norte_lang
            .filter(|v| !v.is_empty())
            .map(|v| Lang::negotiate(Some(v)))
    })
}

fn pick(pinned: Option<Lang>, config: Option<&str>, locale: Lang) -> Lang {
    pinned.unwrap_or_else(|| config.map_or(locale, |c| Lang::negotiate(Some(c))))
}

/// A process-wide language that can change while the process runs: the
/// active language, and any other piece that must follow the same rule (the
/// key names in `norte-frontend`). Empty until set.
#[derive(Debug)]
pub struct LangCell(AtomicU8);

impl LangCell {
    /// An empty cell.
    #[must_use]
    pub const fn new() -> Self {
        Self(AtomicU8::new(0))
    }

    /// The language set, if any.
    ///
    /// ```
    /// use norte_i18n::{Lang, LangCell};
    /// static CELL: LangCell = LangCell::new();
    /// assert_eq!(CELL.get(), None);
    /// CELL.set(Lang::Es);
    /// assert_eq!(CELL.get(), Some(Lang::Es));
    /// ```
    #[must_use]
    pub fn get(&self) -> Option<Lang> {
        Lang::from_code(self.0.load(Ordering::Relaxed))
    }

    /// Sets it; says whether it changed.
    pub fn set(&self, lang: Lang) -> bool {
        self.0.swap(lang.code(), Ordering::Relaxed) != lang.code()
    }

    /// The language set, or `init`'s, stored only if nobody set one
    /// meanwhile: a lazy first read must not undo a concurrent [`Self::set`].
    pub fn get_or_init(&self, init: impl FnOnce() -> Lang) -> Lang {
        if let Some(lang) = self.get() {
            return lang;
        }
        let lang = init();
        match self
            .0
            .compare_exchange(0, lang.code(), Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => lang,
            Err(won) => Lang::from_code(won).unwrap_or(lang),
        }
    }
}

impl Default for LangCell {
    fn default() -> Self {
        Self::new()
    }
}

fn ftl(lang: Lang) -> &'static str {
    match lang {
        Lang::Es => include_str!("../i18n/es.ftl"),
        Lang::En | Lang::Extra(_) => include_str!("../i18n/en.ftl"),
    }
}

fn built_in(lang: Lang) -> &'static FluentBundle<FluentResource> {
    static ES: OnceLock<FluentBundle<FluentResource>> = OnceLock::new();
    static EN: OnceLock<FluentBundle<FluentResource>> = OnceLock::new();
    let (cell, id) = match lang {
        Lang::Es => (&ES, "es"),
        Lang::En | Lang::Extra(_) => (&EN, "en"),
    };
    cell.get_or_init(|| {
        // The embedded .ftl files are validated by the suite: an error here
        // is a broken build, not the user's runtime.
        let res = FluentResource::try_new(ftl(lang).to_owned()).unwrap_or_else(|(res, _)| res);
        new_bundle(id.parse().unwrap_or_default(), res)
    })
}

fn new_bundle(id: LanguageIdentifier, res: FluentResource) -> FluentBundle<FluentResource> {
    let mut b = FluentBundle::new_concurrent(vec![id]);
    let _ = b.add_resource(res);
    // No bidi isolation marks: the UI is a terminal.
    b.set_use_isolating(false);
    b
}

fn bundle(lang: Lang) -> &'static FluentBundle<FluentResource> {
    match lang {
        Lang::Extra(e) => extras()
            .get(usize::from(e.0))
            .map_or_else(|| built_in(Lang::En), |x| &x.bundle),
        other => built_in(other),
    }
}

// ---------------------------------------------------------------------------
// Extra languages (ADR 0188)
// ---------------------------------------------------------------------------

/// The most extra languages one process installs.
pub const MAX_EXTRAS: usize = 16;

/// The largest `.ftl` accepted, in bytes. The built-in catalogues are under
/// a quarter of this.
pub const MAX_FTL_BYTES: usize = 1024 * 1024;

struct ExtraLang {
    tag: &'static str,
    bundle: FluentBundle<FluentResource>,
}

static EXTRAS: OnceLock<Vec<ExtraLang>> = OnceLock::new();

fn extras() -> &'static [ExtraLang] {
    EXTRAS.get().map_or(&[], Vec::as_slice)
}

/// Why an extra language file was not installed, or only in part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The file name is not a language tag (`fr`, `pt-BR`).
    BadTag,
    /// Spanish and English are built in: a file cannot replace them, or a
    /// stray `en.ftl` would decide what every English reader sees.
    BuiltIn,
    /// Another file already installed this tag.
    Duplicate,
    /// Past [`MAX_EXTRAS`] or [`MAX_FTL_BYTES`].
    TooBig,
    /// Installed, but this many entries did not parse and were skipped:
    /// those messages show in English.
    Syntax {
        /// How many entries were skipped.
        errors: usize,
    },
    /// [`install_extras`] already ran in this process.
    AlreadyInstalled,
}

/// What [`install_extras`] has to say about one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// The tag as the file named it.
    pub tag: String,
    /// What happened.
    pub problem: Problem,
}

/// Installs the extra languages: `(tag, ftl source)` pairs, usually one per
/// `<config>/locales/<tag>.ftl`. ONCE per process, before anything
/// negotiates a language — a later call installs nothing and says so.
///
/// Returns what was refused or installed only in part; an empty vector means
/// every file went in whole.
///
/// ```
/// let notices = norte_i18n::install_extras(vec![
///     ("fr".into(), "language-name = Français\nhelp-title = Aide\n".into()),
///     ("en".into(), "help-title = Hijacked\n".into()),
/// ]);
/// assert_eq!(notices.len(), 1, "en is built in: {notices:?}");
/// let fr = norte_i18n::Lang::from_flag("fr").unwrap();
/// assert_eq!(norte_i18n::t_in(fr, "help-title"), "Aide");
/// // What fr does not translate is English, never the bare id.
/// assert_eq!(norte_i18n::t_in(fr, "modal-trash-title"), "To trash");
/// assert_eq!(fr.native_name(), "Français");
/// ```
pub fn install_extras(sources: Vec<(String, String)>) -> Vec<Notice> {
    let mut notices = Vec::new();
    let mut installed: Vec<ExtraLang> = Vec::new();
    for (tag, source) in sources {
        let notice = |problem| Notice {
            tag: tag.clone(),
            problem,
        };
        let Some(id) = parse_tag(&tag) else {
            notices.push(notice(Problem::BadTag));
            continue;
        };
        let normal = id.to_string();
        if matches!(id.language.as_str(), "es" | "en") {
            notices.push(notice(Problem::BuiltIn));
            continue;
        }
        if installed
            .iter()
            .any(|e| e.tag.eq_ignore_ascii_case(&normal))
        {
            notices.push(notice(Problem::Duplicate));
            continue;
        }
        if installed.len() >= MAX_EXTRAS || source.len() > MAX_FTL_BYTES {
            notices.push(notice(Problem::TooBig));
            continue;
        }
        let res = match FluentResource::try_new(defang(&source)) {
            Ok(res) => res,
            Err((res, errors)) => {
                notices.push(notice(Problem::Syntax {
                    errors: errors.len(),
                }));
                res
            }
        };
        installed.push(ExtraLang {
            // Leaked on purpose: at most `MAX_EXTRAS` short strings, once per
            // process, so `Lang::tag` can hand out `&'static str` like the
            // built-ins do.
            tag: Box::leak(normal.into_boxed_str()),
            bundle: new_bundle(id, res),
        });
    }
    if EXTRAS.set(installed).is_err() {
        notices.push(Notice {
            tag: String::new(),
            problem: Problem::AlreadyInstalled,
        });
    }
    let _ = TAGS.set(Lang::all().into_iter().map(Lang::tag).collect());
    notices
}

static TAGS: OnceLock<Vec<&'static str>> = OnceLock::new();

/// Every tag this process speaks, in [`Lang::all`]'s order: what a settings
/// chooser offers. Before [`install_extras`] ran, the two built in.
///
/// ```
/// assert_eq!(&norte_i18n::tags()[..2], &["en", "es"]);
/// ```
#[must_use]
pub fn tags() -> &'static [&'static str] {
    TAGS.get().map_or(&["en", "es"], Vec::as_slice)
}

/// Text without what a terminal would OBEY or hide instead of print —
/// controls (an ESC starts a sequence), bidi marks, invisibles: the repo's
/// one set, `is_terminal_hazard` — keeping line breaks and tabs, which a
/// message may carry. The built-in catalogues are reviewed; a translation
/// downloaded from anywhere is not, and the TUI paints these strings raw.
fn defang(text: &str) -> String {
    text.chars()
        .filter(|&c| matches!(c, '\n' | '\t') || !norte_encoding::is_terminal_hazard(c))
        .collect()
}

/// The longest text one extra-language message may produce. Fluent allows
/// 100 placeables per message, so `b = {a}{a}…` over a large `a` multiplies
/// a file into a hundred of it — on every frame that paints it.
const MAX_OUTPUT_BYTES: usize = 8 * 1024;

/// An extra language's formatted message, made safe to paint: Fluent decodes
/// `\u001B`-style escapes only when formatting, so the source filter alone
/// cannot catch them.
fn safe_output(text: &str) -> String {
    let mut out = defang(text);
    if out.len() > MAX_OUTPUT_BYTES {
        let mut end = MAX_OUTPUT_BYTES;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
    }
    out
}

/// A file stem as a language tag: letters, digits, `-` and `_` only, and
/// something `unic-langid` accepts.
fn parse_tag(tag: &str) -> Option<LanguageIdentifier> {
    if tag.is_empty()
        || !tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    tag.replace('_', "-").parse().ok()
}

// ---------------------------------------------------------------------------
// The active language
// ---------------------------------------------------------------------------

/// The process's active language. Changes at run time (ADR 0188): the
/// first-run wizard and the settings screen switch it without a restart.
static GLOBAL: LangCell = LangCell::new();

/// What `--lang` or `NORTE_LANG` fixed for this run, above `[ui] lang`.
static PINNED: LangCell = LangCell::new();

/// Sets the active language; says whether it changed.
pub fn force(lang: Lang) -> bool {
    GLOBAL.set(lang)
}

/// Startup: resolves `--lang` > `NORTE_LANG` > `[ui] lang` > the locale,
/// makes it active, and remembers what the run pinned so that
/// [`follow_config`] keeps honouring it. Install the extras first.
pub fn start(flag: Option<Lang>, config: Option<&str>) -> Lang {
    let pin = pinned(flag, std::env::var("NORTE_LANG").ok().as_deref());
    if let Some(p) = pin {
        PINNED.set(p);
    }
    let lang = pick(pin, config, Lang::from_env());
    force(lang);
    lang
}

/// What re-reading `[ui] lang` did to the active language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Followed {
    /// Nothing to do.
    Same,
    /// The active language is now this one: whatever caches text rebuilds.
    Changed(Lang),
    /// `--lang` or `NORTE_LANG` fixed this one for the run, and the
    /// configuration asks for another: it applies from the next start.
    Pinned(Lang),
}

/// The configuration was re-read: apply its `[ui] lang` now, unless the run
/// pinned a language ([`start`]).
pub fn follow_config(config: Option<&str>) -> Followed {
    follow(PINNED.get(), config, Lang::from_env())
}

fn follow(pin: Option<Lang>, config: Option<&str>, locale: Lang) -> Followed {
    let wanted = pick(None, config, locale);
    match pin {
        Some(p) if wanted != p => Followed::Pinned(p),
        // Against the ACTIVE language, not the cell: an unset one is the
        // locale's already, and "set it to what it was" is not a change.
        None if wanted != active() => {
            force(wanted);
            Followed::Changed(wanted)
        }
        Some(_) | None => Followed::Same,
    }
}

fn global() -> Lang {
    GLOBAL.get_or_init(Lang::from_env)
}

/// What `--lang`/`NORTE_LANG` fixed for this run ([`start`]), if anything:
/// a chooser has nothing to offer then, since the choice would not apply
/// until the next start.
#[must_use]
pub fn pinned_lang() -> Option<Lang> {
    PINNED.get()
}

/// The current global language: the one [`force`] or [`start`] set, or the
/// environment's if nobody set it.
///
/// Exists for callers who need to translate with [`t_in`] in the language
/// [`t`] would use — a resolver that stores the language in a field, for
/// example. Without this they had to re-derive the negotiation on their
/// own, and two derivations of the same fact end up disagreeing: the UI in
/// one language and a table inside it in another.
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

/// Translates with args in a specific language. Missing id → English →
/// the id itself.
#[must_use]
pub fn ta_in(lang: Lang, id: &str, args: &[(&str, &str)]) -> String {
    format_own(lang, id, args)
        .or_else(|| {
            (lang != Lang::En)
                .then(|| format_own(Lang::En, id, args))
                .flatten()
        })
        .unwrap_or_else(|| id.to_owned())
}

/// `id` in exactly this language's bundle, with no fallback.
fn format_own(lang: Lang, id: &str, args: &[(&str, &str)]) -> Option<String> {
    let b = bundle(lang);
    let pattern = b.get_message(id)?.value()?;
    let mut fargs = FluentArgs::new();
    for (k, v) in args {
        fargs.set(*k, *v);
    }
    let mut errors = Vec::new();
    let text = b.format_pattern(pattern, Some(&fargs), &mut errors);
    Some(match lang {
        Lang::Extra(_) => safe_output(&text),
        Lang::Es | Lang::En => text.into_owned(),
    })
}

/// All of a BUILT-IN locale's message ids (for the parity test and the
/// window's catalogue). An extra answers with English's, which is the
/// complete set it falls back to.
#[must_use]
pub fn message_ids(lang: Lang) -> Vec<String> {
    use fluent_syntax::ast::Entry;
    let res = FluentResource::try_new(ftl(lang).to_owned()).unwrap_or_else(|(res, _)| res);
    res.entries()
        .filter_map(|e| match e {
            Entry::Message(m) => Some(m.id.name.to_owned()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod follow_rule {
    use super::*;

    /// The rule `follow_config` applies, without the process globals: a
    /// pinned run reports instead of switching.
    #[test]
    fn a_pinned_run_does_not_follow_the_configuration() {
        assert_eq!(
            follow(Some(Lang::En), Some("es"), Lang::En),
            Followed::Pinned(Lang::En)
        );
        assert_eq!(follow(Some(Lang::En), Some("en"), Lang::Es), Followed::Same);
        assert_eq!(follow(Some(Lang::Es), None, Lang::Es), Followed::Same);
    }
}

#[cfg(test)]
mod no_duplicates {
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
