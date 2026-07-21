# Documentation

This directory contains the design and operating documentation for norte.

## Start here

- [Project specification](spec/norte-spec.md): product goals, system design,
  milestones, and constraints.
- [Architecture overview](../ARCHITECTURE.md): a concise map of the workspace
  and its dependency rules.
- [Theme configuration](theming.md): bundled themes and custom theme files.
- [Architecture decisions](adr/README.md): the ADR index and decision history.
- [Contributing guide](../CONTRIBUTING.md): development workflow and review
  expectations.
- [Security policy](../SECURITY.md): private vulnerability reporting and trust
  boundaries.

## Reference files

- `schema/` contains the JSON Schemas for application and keymap
  configuration.
- `policy-example.toml` is a commented policy configuration example.

## Documentation conventions

- Write public documentation in English.
- Prefer short, concrete sentences and descriptive headings.
- Use repository-relative links so documentation works both on GitHub and
  locally.
- Wrap prose at roughly 80 characters where practical. Do not reflow code,
  tables, URLs, or generated schemas solely to meet that width.
- Record durable architectural decisions as ADRs; keep implementation steps in
  plans rather than user-facing guides.
