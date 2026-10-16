//! The first-run wizard (spec 2026-09-10): the language (ADR 0188), the key
//! preset, the theme, the icons, and whether to check for new releases (ADR
//! 0183), when there is no user `norte.toml`. Pure: which step, which rows,
//! what was chosen. Each frontend paints it and writes what was chosen
//! through its own settings path.
//!
//! Esc at any step means "do not ask again": the frontend writes a file with
//! what it already had, and the file's existence is the mark.

use norte_i18n::{Lang, t_in};

/// The steps, in order. `Language` only when the frontend offers it
/// ([`Wizard::with_languages`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Which language: FIRST, so every question after it is read in it.
    Language,
    /// Which key preset.
    Preset,
    /// Which theme.
    Theme,
    /// Whether the font paints the icons.
    Icons,
    /// Whether to check for new releases (ADR 0183).
    Updates,
}

impl Step {
    /// Its slot in the per-step cursor array: fixed, whether or not
    /// `Language` is shown.
    fn index(self) -> usize {
        match self {
            Self::Language => 0,
            Self::Preset => 1,
            Self::Theme => 2,
            Self::Icons => 3,
            Self::Updates => 4,
        }
    }
}

/// What was chosen. `None` = the reader did not get to that step.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Choices {
    /// The language's tag.
    pub lang: Option<String>,
    /// The key preset.
    pub preset: Option<String>,
    /// The theme.
    pub theme: Option<String>,
    /// `Some(true)` = the icons are shown (emoji); `Some(false)` = ASCII.
    pub icons: Option<bool>,
    /// `Some(true)` = check for new releases once a day (`[updates] check`).
    pub updates: Option<bool>,
}

/// What happened on confirming or on leaving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Another step ahead.
    Continue,
    /// The language was chosen and the wizard goes on: write `[ui] lang` =
    /// this tag NOW, so the questions after it are read in it (ADR 0188).
    /// The frontend's configuration reload is what switches the language.
    Language(String),
    /// The last step answered: write this.
    Done(Choices),
    /// The reader left: do not ask again, without changing anything. Carries
    /// the CURRENT theme so the frontend writes exactly that one and the file
    /// exists — never `default` over a theme that was already there (review
    /// B1).
    Dismissed {
        /// The theme the wizard was opened with.
        keep_theme: String,
    },
}

/// The wizard.
#[derive(Debug, Clone)]
pub struct Wizard {
    step: Step,
    languages: Vec<Lang>,
    presets: Vec<String>,
    themes: Vec<String>,
    cursor: [usize; 5],
    choices: Choices,
    /// The theme it was opened with: what is kept on leaving with Esc.
    current_theme: String,
}

impl Wizard {
    /// With the presets and themes there are. The cursor starts on the
    /// CURRENT preset and theme, so pressing Enter without looking leaves
    /// everything as it was.
    #[must_use]
    pub fn new(
        presets: &[&str],
        themes: &[&str],
        current_preset: &str,
        current_theme: &str,
    ) -> Self {
        let presets: Vec<String> = presets.iter().map(|s| (*s).to_owned()).collect();
        let themes: Vec<String> = themes.iter().map(|s| (*s).to_owned()).collect();
        let p = presets
            .iter()
            .position(|s| s == current_preset)
            .unwrap_or(0);
        let t = themes.iter().position(|s| s == current_theme).unwrap_or(0);
        Self {
            step: Step::Preset,
            languages: Vec::new(),
            presets,
            themes,
            // Updates starts on "no": Enter without reading keeps norte off
            // the network, which is the default it is asking about.
            cursor: [0, p, t, 0, 0],
            choices: Choices::default(),
            current_theme: current_theme.to_owned(),
        }
    }

    /// Asks the language FIRST (ADR 0188), among `languages`, starting on
    /// `current` — the one the locale already chose, so Enter without
    /// reading keeps it.
    #[must_use]
    pub fn with_languages(mut self, languages: &[Lang], current: Lang) -> Self {
        if languages.is_empty() {
            return self;
        }
        self.cursor[Step::Language.index()] =
            languages.iter().position(|l| *l == current).unwrap_or(0);
        self.languages = languages.to_vec();
        self.step = Step::Language;
        self
    }

    /// The steps this wizard asks, in order.
    fn steps(&self) -> &'static [Step] {
        const ALL: [Step; 5] = [
            Step::Language,
            Step::Preset,
            Step::Theme,
            Step::Icons,
            Step::Updates,
        ];
        if self.languages.is_empty() {
            &ALL[1..]
        } else {
            &ALL
        }
    }

    /// The current step.
    #[must_use]
    pub fn step(&self) -> Step {
        self.step
    }

    /// The step's title, in the given language: `norte · 1/5 · Language`.
    #[must_use]
    pub fn title(&self, lang: Lang) -> String {
        let key = match self.step {
            Step::Language => "wizard-step-language",
            Step::Preset => "wizard-step-preset",
            Step::Theme => "wizard-step-theme",
            Step::Icons => "wizard-step-icons",
            Step::Updates => "wizard-step-updates",
        };
        let steps = self.steps();
        let number = steps.iter().position(|s| *s == self.step).unwrap_or(0) + 1;
        format!(
            "{} · {number}/{} · {}",
            t_in(lang, "wizard-title"),
            steps.len(),
            t_in(lang, key)
        )
    }

    /// The step's question, in the given language.
    #[must_use]
    pub fn question(&self, lang: Lang) -> String {
        let key = match self.step {
            Step::Language => "wizard-ask-language",
            Step::Preset => "wizard-ask-preset",
            Step::Theme => "wizard-ask-theme",
            Step::Icons => "wizard-ask-icons",
            Step::Updates => "wizard-ask-updates",
        };
        t_in(lang, key)
    }

    /// The step's rows, in the given language: each language by its OWN
    /// name, each preset with its line, the themes by name, and yes/no for
    /// the icons.
    #[must_use]
    pub fn rows(&self, lang: Lang) -> Vec<String> {
        match self.step {
            // In itself, not in `lang`: whoever cannot read the current
            // language is exactly who is looking for theirs.
            Step::Language => self
                .languages
                .iter()
                .map(|l| format!("{} ({})", l.native_name(), l.tag()))
                .collect(),
            Step::Preset => self
                .presets
                .iter()
                .map(|p| {
                    let key = format!("wizard-preset-{p}");
                    let line = t_in(lang, &key);
                    if line == key {
                        p.clone()
                    } else {
                        format!("{p} — {line}")
                    }
                })
                .collect(),
            Step::Theme => self.themes.clone(),
            Step::Icons => vec![
                t_in(lang, "wizard-icons-yes"),
                t_in(lang, "wizard-icons-no"),
            ],
            Step::Updates => vec![
                t_in(lang, "wizard-updates-no"),
                t_in(lang, "wizard-updates-yes"),
            ],
        }
    }

    /// Which row is chosen at the current step.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor[self.step.index()]
    }

    fn len(&self) -> usize {
        match self.step {
            Step::Language => self.languages.len(),
            Step::Preset => self.presets.len(),
            Step::Theme => self.themes.len(),
            Step::Icons | Step::Updates => 2,
        }
    }

    /// Moves up one row (stops at the top).
    pub fn up(&mut self) {
        let i = self.step.index();
        self.cursor[i] = self.cursor[i].saturating_sub(1);
    }

    /// Moves down one row (stops at the bottom).
    pub fn down(&mut self) {
        let i = self.step.index();
        if self.cursor[i] + 1 < self.len() {
            self.cursor[i] += 1;
        }
    }

    /// Puts the cursor on `row`, if it exists: what a click does.
    pub fn select(&mut self, row: usize) {
        if row < self.len() {
            self.cursor[self.step.index()] = row;
        }
    }

    /// The theme under the cursor while choosing a theme: for the live
    /// preview. `None` in the other steps.
    #[must_use]
    pub fn preview_theme(&self) -> Option<&str> {
        (self.step == Step::Theme)
            .then(|| self.themes.get(self.cursor()).map(String::as_str))
            .flatten()
    }

    /// Enter: saves the row and moves to the next step, or finishes.
    pub fn confirm(&mut self) -> Outcome {
        match self.step {
            Step::Language => {
                let chosen = self
                    .languages
                    .get(self.cursor())
                    .map(|l| l.tag().to_owned());
                self.step = Step::Preset;
                match chosen {
                    Some(tag) => {
                        self.choices.lang = Some(tag.clone());
                        Outcome::Language(tag)
                    }
                    None => Outcome::Continue,
                }
            }
            Step::Preset => {
                self.choices.preset = self.presets.get(self.cursor()).cloned();
                self.step = Step::Theme;
                Outcome::Continue
            }
            Step::Theme => {
                self.choices.theme = self.themes.get(self.cursor()).cloned();
                self.step = Step::Icons;
                Outcome::Continue
            }
            Step::Icons => {
                self.choices.icons = Some(self.cursor() == 0);
                self.step = Step::Updates;
                Outcome::Continue
            }
            Step::Updates => {
                self.choices.updates = Some(self.cursor() == 1);
                Outcome::Done(self.choices.clone())
            }
        }
    }

    /// Backspace: the previous step, if there is one.
    pub fn back(&mut self) {
        let steps = self.steps();
        let i = steps.iter().position(|s| *s == self.step).unwrap_or(0);
        self.step = steps[i.saturating_sub(1)];
    }

    /// Esc: leave without changing anything and do not ask again. Carries the
    /// current theme so whoever writes it keeps exactly that one.
    #[must_use]
    pub fn dismiss(&self) -> Outcome {
        Outcome::Dismissed {
            keep_theme: self.current_theme.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w() -> Wizard {
        Wizard::new(&["orthodox", "vim"], &["default", "nord"], "vim", "nord")
    }

    /// Starts on the current values; pressing Enter four times returns what
    /// was chosen; each step keeps its own cursor; Backspace goes back.
    #[test]
    fn four_steps_and_what_was_chosen() {
        let mut w = w();
        assert_eq!(w.step(), Step::Preset);
        assert_eq!(w.cursor(), 1, "starts on the current preset");
        assert!(w.title(Lang::En).contains("1/4"));
        assert!(w.rows(Lang::En)[0].starts_with("orthodox"));
        w.up();
        assert_eq!(w.confirm(), Outcome::Continue);
        assert_eq!(w.step(), Step::Theme);
        assert_eq!(
            w.preview_theme(),
            Some("nord"),
            "the current theme, under the cursor"
        );
        w.up();
        assert_eq!(w.preview_theme(), Some("default"));
        w.back();
        assert_eq!(
            (w.step(), w.cursor()),
            (Step::Preset, 0),
            "each step keeps its own cursor"
        );
        w.back();
        assert_eq!(w.step(), Step::Preset, "no language step: Preset is first");
        assert_eq!(w.confirm(), Outcome::Continue);
        assert_eq!(w.confirm(), Outcome::Continue);
        assert_eq!(w.step(), Step::Icons);
        assert_eq!(w.rows(Lang::Es).len(), 2);
        w.down();
        w.down();
        assert_eq!(w.cursor(), 1, "stops at the bottom");
        assert_eq!(w.confirm(), Outcome::Continue);
        assert_eq!(w.step(), Step::Updates);
        assert_eq!(w.cursor(), 0, "the release check starts on no");
        w.back();
        assert_eq!(w.step(), Step::Icons);
        assert_eq!(w.confirm(), Outcome::Continue);
        assert_eq!(
            w.confirm(),
            Outcome::Done(Choices {
                lang: None,
                preset: Some("orthodox".into()),
                theme: Some("default".into()),
                icons: Some(false),
                updates: Some(false),
            }),
            "Enter without reading keeps norte off the network"
        );
        assert_eq!(
            w.dismiss(),
            Outcome::Dismissed {
                keep_theme: "nord".into()
            },
            "leaving keeps the theme it was opened with, not `default`"
        );
    }

    /// The language goes FIRST, starts on the one already active, names
    /// each language in itself, and confirming it asks for the write right
    /// away instead of at the end (ADR 0188).
    #[test]
    fn the_language_is_asked_first_and_written_at_once() {
        let mut w = w().with_languages(&[Lang::En, Lang::Es], Lang::Es);
        assert_eq!(w.step(), Step::Language);
        assert!(w.title(Lang::En).contains("1/5"), "{}", w.title(Lang::En));
        assert_eq!(w.cursor(), 1, "starts on the active language");
        assert_eq!(w.rows(Lang::Es), vec!["English (en)", "Español (es)"]);
        w.up();
        assert_eq!(w.confirm(), Outcome::Language("en".into()));
        assert_eq!(w.step(), Step::Preset);
        assert!(w.title(Lang::En).contains("2/5"));
        w.back();
        assert_eq!((w.step(), w.cursor()), (Step::Language, 0));
        w.back();
        assert_eq!(w.step(), Step::Language, "nothing before the first");
    }

    /// A click outside the rows moves nothing; one inside does.
    #[test]
    fn select_only_inside() {
        let mut w = w();
        w.select(7);
        assert_eq!(w.cursor(), 1);
        w.select(0);
        assert_eq!(w.cursor(), 0);
    }
}
