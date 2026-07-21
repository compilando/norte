---
description: Run a release dry run covering semver, licenses, advisories, changelog, docs, and protocol schemas
---
Run the release checklist without publishing:

1. Run `cargo semver-checks` when installed; report its absence and continue
   otherwise.
2. Run `cargo deny check` for licenses and advisories.
3. Confirm that release-plz can derive changelog entries from commits since the
   latest tag.
4. Run `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`.
5. Regenerate the protocol JSON Schema and compare it with
   `norte-proto/schema/`. Any change without a protocol version bump is a
   BLOCKER.
6. Run the complete `just ci` suite.

Return GO or NO-GO and list every blocker.
