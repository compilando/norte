# 0040 - Help corpus and markdown-lite

- Status: accepted
- Date: 2026-08-04
- Decision makers: Oscar González
- Related: design spec
  `docs/superpowers/specs/2026-08-04-help-system-redesign-design.md` (phase
  H3a); ADR 0008 (`norte-encoding` boundary: the single source of the terminal
  hazard set), ADR 0020 (shared theme presets, the embedded-table precedent),
  ADR 0022 (WASM plugin manifest and capabilities), ADR 0037 (plugin data-out:
  what a plugin is allowed to hand a frontend).

## Context

Help is a flat list of key bindings in one frontend
(`norte-tui/src/help.rs`). The GUI has none, the CLI has none, and plugins
cannot document themselves. The redesign needs prose that lives somewhere:
localized, embedded in the binary, renderable by ratatui, GPUI and plain text
alike, and safe to accept from a third-party plugin.

## Decision

### 1. A crate of its own

A new workspace crate `norte-help` owns the help model. It depends on no
frontend and on no part of `norte-core`; frontends and the CLI depend on it.

### 2. Topics are markdown with TOML front matter, embedded by an explicit table

Topics are markdown files with **TOML front matter between `+++` fences**,
embedded via an explicit `include_str!` table. TOML because the workspace
already parses TOML everywhere; a YAML dependency for six header fields would
not survive rule 8. An explicit table because that is what `norte-theme`
presets and `norte-i18n` catalogs already do — a test cross-checks the table
against the directory listing.

### 3. The markdown accepted is a closed subset

Headings, paragraphs, bullets, fenced code, tables, callouts; inline
strong/emph/code plus two custom marks. No HTML, no autolinked URLs, no
images, no nesting beyond one level. A closed subset is what makes a
third-party document safe to render in a terminal.

### 4. Two live marks stay unresolved in the model

`{{cmd:id}}` and `[[topic]]` reach the caller unresolved. Resolution happens
at render time against the effective keymap, so prose can never claim a key
the user has rebound.

### 5. Two parse modes

Trusted (built-in corpus): errors are hard, and a test parses the whole corpus
so a malformed topic cannot ship. Untrusted (plugin `help.md`): never fails,
applies caps (64 KiB, block count, line length), decodes invalid UTF-8
lossily, and masks terminal hazards at parse time, recording
`truncated`/`lossy` flags for the UI badge.

### 6. Masking reuses the existing hazard set

Masking reuses `norte_encoding::is_terminal_hazard`, already the single source
of the hazard set (`norte-frontend`'s `must_mask` is a one-line delegate to
it). `norte-help` must not depend on `norte-frontend`; the dependency runs the
other way.

## Consequences

- New crate in the workspace map and in the build graph, with no new external
  dependencies.
- A documentation gate becomes possible: a test asserts every command in
  `COMMANDS` appears in at least one topic. Adding a command now costs a
  paragraph. That friction is deliberate.
- Plugin help is cosmetic and stays out of the approval digest (P1
  precedent); the safety argument rests on the closed subset, the caps and the
  masking, not on consent.
- A richer markdown feature later means extending a closed vocabulary, which
  is a deliberate act rather than an accidental capability.

## Alternatives considered

- **A module inside `norte-frontend`.** Rejected: the CLI and `doctor` need
  the corpus without pulling in presentation state, `norte-frontend` is
  already large, and the corpus needs its own asset directory.
- **Fluent for prose.** Rejected: Fluent is built for interface strings, not
  for pages of documentation; the catalog would grow by thousands of lines and
  lose all structure.
- **Files read from disk at runtime.** Rejected: breaks the self-contained
  binary, adds path resolution and I/O failures to a help screen.
- **A full CommonMark crate.** Rejected: a large dependency whose whole point
  is accepting everything, which is the opposite of what a hostile-input
  renderer wants.
