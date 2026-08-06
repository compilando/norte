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
applies caps, decodes with loss rather than refusing, and masks terminal
hazards at parse time, recording `truncated`/`lossy` flags for the UI badge.

Two points moved during implementation and are recorded here rather than left
to the code to explain:

- The caps are **four**, not three: source bytes (64 KiB), block count, line
  length, and **total table cells**. The fourth is not redundant — rows are
  padded to the header width, so a wide header over many short rows amplifies
  a bounded source into an unbounded number of cells, and neither the block
  count (a table is one block) nor the line length (a bound on width, never on
  the product) can see it.
- Decoding goes through the house text boundary (ADR 0008):
  `norte_encoding::detect` then `decode`, the same pipeline the viewer uses,
  rather than a bare `from_utf8_lossy`. A `help.md` saved by a Windows editor
  carries a UTF-8 BOM, which a raw UTF-8 read leaves in front of the `+++`
  fence — the header then fails to parse and the topic silently loses its
  title and its commands.

### 6. Masking reuses the existing hazard set

Masking reuses `norte_encoding::mask_terminal_hazards` and its
`is_terminal_hazard` predicate, already the single source of the hazard set
(`norte-frontend`'s `must_mask` is a one-line delegate to it). `norte-help`
must not depend on `norte-frontend`; the dependency runs the other way.

With one stopgap, recorded because it is a deviation from "single source":
`norte-help` carries a small local `INVISIBLE` set used by its blank-id
predicate, because `is_terminal_hazard` enumerates code points instead of
testing a Unicode property and misses several invisibles — `U+3164` HANGUL
FILLER among them, which is general category `Lo` and no `Cf`/`Zl`/`Zp`
enumeration will ever reach. Fixing the shared predicate touches
`norte-encoding` plus snapshot pins in `norte-tui` and `norte-gui`, so it was
deliberately kept out of this phase: **issue #125**. The local set goes away
when #125 lands.

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

## Amendment (2026-08-06, phase H3e — the core depends on `norte-help` too)

Decision 1 said frontends and the CLI depend on `norte-help`. Phase H3e adds
`norte-core` to that list: `plugin.help` hands a plugin's `help.md` over the
wire as markdown TEXT, so the HOST is what cuts it at the 64 KiB untrusted cap
and decodes it through the `norte-encoding` boundary — with
`norte_help::cut_and_decode_untrusted`, the same cap and the same detection the
frontend's `parse_untrusted` applies when it parses the text on arrival. That
name is deliberate and was corrected during the phase: the function does not
sanitize, and the text it returns still carries every terminal hazard the
plugin wrote — only `parse_untrusted` masks, on arrival, where the model is
built. The cap is applied to the source AND to the decoded string, because a
single byte can decode to three and a cap that only bounds the source lets the
host and the frontend disagree about where the same page ends. The
alternative was re-implementing the cut in `norte-core`, and two caps that
start out equal do not stay equal; the whole hostile-input story here rests on
there being one. The direction stays acyclic — `norte-help` still knows nothing
about the core, the frontends, or the protocol.

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
