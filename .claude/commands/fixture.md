---
description: Add a hostile fixture to the canonical norte-testkit corpus
argument-hint: <case description, for example "filename with an unpaired surrogate">
---
Add a `norte-testkit` fixture for: $ARGUMENTS

1. Store the exact bytes in `fixtures/names.toml` as hexadecimal for a filename,
   or under `fixtures/content/` for binary content.
2. Document the real-world case, affected operating system or provider, and the
   regression it prevents. Link an issue when one exists.
3. Add it to `fixtures::hostile_names()` or `fixtures::content_corpus()`.
4. Confirm that the provider conformance suite exercises the new round trip
   against both memory and local providers.
5. Add the fixture and failing test before fixing an encoding or path bug.
