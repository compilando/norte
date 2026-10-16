//! The interface language of a frontend process (ADR 0188): the extra
//! languages a reader installs, and the one place that sets the active
//! language together with the language keys are named in — two settings
//! that drift apart paint `Retroceso back`.

use norte_i18n::{Followed, Lang, Notice, Problem};

/// Installs the extra languages under `<config>/locales/`. Run it before
/// anything negotiates a language, `--lang` included. **Blocks** (rule 2).
///
/// Returns one line per file skipped or installed in part, for the log: a
/// translation that does not load must not fail the start.
#[must_use]
pub fn install_locales() -> Vec<String> {
    let Some(dir) = norte_config::user_config_dir() else {
        return Vec::new();
    };
    let found = norte_config::locales::read_locales(
        &dir.join(norte_config::locales::DIR),
        norte_i18n::MAX_FTL_BYTES,
        norte_i18n::MAX_EXTRAS,
    );
    let mut lines = found.skipped;
    lines.extend(
        norte_i18n::install_extras(found.sources)
            .iter()
            .map(describe),
    );
    // A file name is bytes someone chose, and these lines reach the log panel.
    lines
        .iter()
        .map(|l| norte_encoding::mask_terminal_hazards(l))
        .collect()
}

fn describe(n: &Notice) -> String {
    let why = match &n.problem {
        Problem::BadTag => "the file name is not a language tag".to_owned(),
        Problem::BuiltIn => "es and en are built in and cannot be replaced".to_owned(),
        Problem::Duplicate => "another file already installed this language".to_owned(),
        Problem::TooBig => "too many languages, or the file is too big".to_owned(),
        Problem::Syntax { errors } => format!("{errors} entries did not parse and show in English"),
        Problem::AlreadyInstalled => "the languages were already installed".to_owned(),
    };
    format!("locales/{}.ftl: {why}", n.tag)
}

/// Startup: `--lang` > `NORTE_LANG` > `[ui] lang` > the locale, made active.
#[must_use]
pub fn start(flag: Option<Lang>, config: Option<&str>) -> Lang {
    let lang = norte_i18n::start(flag, config);
    crate::keymap::set_chord_lang(lang);
    lang
}

/// The configuration was re-read: follow its `[ui] lang` unless the run
/// pinned one. On [`Followed::Changed`] the caller rebuilds what it cached.
#[must_use]
pub fn follow(config: Option<&str>) -> Followed {
    let followed = norte_i18n::follow_config(config);
    if let Followed::Changed(lang) = followed {
        crate::keymap::set_chord_lang(lang);
    }
    followed
}

/// The `--lang` error's list: every tag this process speaks.
///
/// ```
/// assert!(norte_frontend::language::tags().starts_with("en, es"));
/// ```
#[must_use]
pub fn tags() -> String {
    Lang::all()
        .iter()
        .map(|l| l.tag())
        .collect::<Vec<_>>()
        .join(", ")
}
