# 0188 - The language is chosen at first run, switches live, and extends by `.ftl`

- Status: accepted
- Date: 2026-10-16
- Decision makers: Oscar González
- Related: ADR 0154 (source in English, interface through Fluent), ADR 0077
  (one rule for both frontends), the first-run wizard (spec 2026-09-10).

## Context and problem statement

norte is about to be announced to an English-speaking audience. The rule
that picks the language (`--lang` > `NORTE_LANG` > `[ui] lang` > locale,
English when nothing says Spanish) was already right, but three things
made it fragile or invisible:

- Spanish leaked outside Fluent: the deb/rpm/AppImage description, the
  `.desktop` comment, two plugin help pages, the window's `<html lang>`, a
  config warning.
- Nobody was asked. A reader on an English locale who wanted Spanish, or
  the reverse, had to find `[ui] lang` — and changing it took a restart,
  although the settings catalogue already said `applies_live: true`.
- Only two languages could exist, compiled in.

## Decision outcome

- **English is the floor, and it is tested.** Leaks fixed; a test pins that
  no locale, `C`, `POSIX` and unknown locales give English, and another that
  `en.ftl` carries no Spanish-only marks. Package metadata is English with
  `[es]` keys in the `.desktop`.
- **The wizard asks the language first**, preselected from the locale,
  naming each language in itself. Confirming it writes `[ui] lang` at once
  (`Outcome::Language`), and the frontend's ordinary configuration reload
  applies it — the wizard and the settings screen share one path.
- **The language switches live.** The active language is a `LangCell`
  (atomic) instead of a `OnceLock`, and so is the chord-name language.
  `norte_i18n::start` records what `--lang`/`NORTE_LANG` pinned;
  `follow_config` re-resolves on each reload and answers Same, Changed or
  Pinned — a pinned run keeps its language and says so.
  - Terminal: `reload_config` follows it FIRST, before rebuilding help,
    palette, key bars and chords. A wizard write also asks for an immediate
    reload, because on a first run the config dir did not exist and only
    the slow poll would see it.
  - Window: the host emits `NativeEffect::LanguageChanged`; the house
    rebuilds the catalogue strings and the renderer RELOADS the page,
    then asks for a fresh frame. In place would leave stale text: the paint
    caches key on shape, not on text. The host outlives the reload;
    `wizard_open` is once per host session.
- **Extra languages are `<config>/locales/<tag>.ftl`.** Installed once at
  startup (before `--lang` is validated), indexed by `Lang::Extra`. Whatever
  a file lacks is English, never the bare id; the window's catalogue
  iterates English's ids for the same reason. A file cannot replace `es` or
  `en`, so a stray `en.ftl` cannot decide what English readers see. A
  translation is something people download and the TUI paints it raw, so
  terminal hazards (`norte_encoding::is_terminal_hazard`, the repo's one
  set) are stripped from every FORMATTED message — Fluent decodes `\u001B`
  escapes only then, so filtering the source alone let them through — and
  a message is cut at 8 KiB, since 100 placeables per message can multiply
  a large one. Only regular files are read (a FIFO would hang the start);
  16 files, 1 MiB each. Help topics (F1) stay English for an extra language.

### Not chosen

- **Previewing each language while moving the cursor**, like the theme
  step: in the window every switch reloads the page, and a reload per arrow
  is a flicker, not a preview.
- **Updating the window in place.** Every paint cache would need a language
  key; a reload is one path that cannot miss one.
- **Language packs as plugins.** A `.ftl` is data, not code; a plugin kind
  for it would add the manifest, approval and WASM surface for nothing. A
  later plugin kind can still install into the same registry.

### Consequences

- `[ui] lang` written anywhere (wizard, settings, by hand, a profile)
  applies without a restart in both frontends.
- `Lang` is no longer two variants: an exhaustive `match` must decide what
  an extra language does (help corpus and chord names fall back to English).
- Translators start from `crates/norte-i18n/i18n/en.ftl` and should set
  `language-name`; norte's log names files skipped or loaded in part.
