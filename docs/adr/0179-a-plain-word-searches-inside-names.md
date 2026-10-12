# 0179 - A plain word in a search form searches inside names

- Status: accepted
- Date: 2026-10-12
- Decision makers: Oscar González
- Related: ADR 0077 (one model, both frontends), ADR 0088 (structured
  output: what agents and the CLI rely on).

## Context and problem statement

The name field of the search form (`Ctrl+s` in krusader, `Alt+F7` in the
others) and of the search panel sent its text to the core as a glob,
verbatim. A glob without a wildcard matches the WHOLE name, so typing
`bash` found a file called exactly `bash` and never `.bashrc`. Every search
box people know reads a plain word as "names containing it"; the reader
took it as a broken search.

## Decision

The frontends' shared form (`norte_frontend::search::params`) reads the
name field through `name_glob_of`: text with no wildcard (`*`, `?`, `[`,
`{`) becomes `*text*`; text with one is the glob it says. Case follows the
form's toggle, as before. With the regex toggle on, the text is the
reader's regex, untouched.

The wire does NOT change: `name_glob` keeps its exact glob meaning, and the
CLI, MCP and agents, which write globs on purpose, are not affected.

## Alternatives considered

- **Change `name_glob` in the core.** Rejected: it would change what an
  agent's or a script's exact pattern means, silently, for a problem that
  only exists where a human types into a box.
- **A "contains / glob" toggle.** Rejected: one more control to explain, for
  a choice the presence of a wildcard already makes.

## Consequences

- A reader who wants an exact name types it with no wildcard and gets
  more results than before; `bash` also finds `.bashrc`. An anchored name
  is still a glob away (`bash` vs `[b]ash`).
- The field's label no longer says "(glob)": its example says what it takes.
