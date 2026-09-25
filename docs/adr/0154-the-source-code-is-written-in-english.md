# 0154 — The source code is written in English

- Status: accepted and implemented
- Date: 2026-09-24
- Decision makers: Oscar González
- Protocol: unchanged. Config keys, CLI flags, Fluent ids, MCP tools, WIT:
  unchanged. Schemas: description text only.
- Related: `TRANSLATION_GLOSSARY.md`, `TRANSLATION_WIRE_SURFACE.md`,
  `docs/superpowers/plans/2026-09-24-translate-to-english.md`

## Context and problem statement

norte's user interface was already bilingual — every string a user sees went
through Fluent, with `en.ftl` and `es.ftl` at full parity, and `NORTE_LANG`
choosing between them. Its *source* was not: about 95,000 comment lines,
~5,500 identifiers and a few dozen file names were Spanish, next to an
English wire, English config keys and English ADRs.

That split has a cost for anyone who does not read Spanish — a contributor,
a reviewer, a tool — and it made the English half of the project read as a
translation of a Spanish original. The question was which language the code
itself is written in, and what that means for the text a user sees.

## Considered options

1. **Everything in English, UI through Fluent.** Identifiers, comments,
   rustdoc, log messages and internal errors in English; user-facing text
   stays in the Fluent catalogues, `es` keeping the original wording.
   - Good: one language to read the code in; the wire, the config and the
     code finally agree; the Spanish UI loses nothing.
   - Bad: a one-off churn over ~800 files, which collides with every open
     branch; a few terms had to be chosen (`hueco` → slot, `foto` →
     snapshot…) and now live in a glossary.
2. **Only public documentation in English**, internals left Spanish.
   - Good: a tenth of the cost; public API docs readable.
   - Bad: the code a contributor actually edits stays Spanish; the split
     just moves inward.
3. **Leave it as it is.**
   - Good: no churn.
   - Bad: the problem stays, and grows with every commit.

## Decision

Option 1. The source code is written in English: identifiers, comments,
rustdoc, log and internal error texts, test names and file names. Text a
user sees goes through Fluent, in `en` and `es`. The CLI's own `--help` and
its error messages are English inline, the convention `ntc --help` already
followed.

The wire surface was already English and did not change — verified before
anything was touched (`TRANSLATION_WIRE_SURFACE.md`).

## Consequences

- Positive: code, wire, config and ADRs are one language; the glossary fixes
  the vocabulary for new code.
- Positive: the renames were done by a recorded codemod
  (`docs/superpowers/plans/2026-09-24-translate-codemod/`), so how a name
  changed is auditable.
- Negative: `git blame` runs through the translation commits; use
  `--ignore-rev` for them.
- Negative: test data stays Spanish where the bytes are the point (accents,
  NFC/NFD, hostile names) or where a golden or the Spanish locale pins it —
  that Spanish is deliberate, not a leftover.
- Negative: a few Spanish words survive in identifiers where the English
  one is a technical term of its own (`parse_dos`, `serde::ser`, rustls'
  `CertificateDer`): the stop list in the codemod directory names them.
