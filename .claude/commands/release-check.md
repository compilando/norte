---
description: Run a release dry run covering semver, licenses, advisories, changelog, docs, and protocol schemas
---
Run the release checklist without publishing:

1. Run `just semver` (cargo-semver-checks against the latest tag). It is
   installed; an absent tool is now a BLOCKER, not a note. It refuses to run
   until the workspace version has been raised above the baseline tag's,
   because at equal versions the tool skips every check and reports success —
   so bump the version FIRST, then run this. It covers the publishable
   MIT/Apache libraries only; the AGPL binaries have no public API to break.
2. Run `cargo deny check` for licenses and advisories, and
   `cargo deny --manifest-path crates/norte-gui/Cargo.toml check --config
   crates/norte-gui/deny.toml` for the GUI, which the workspace graph excludes.
3. Confirm that release-plz can derive changelog entries from commits since the
   latest tag.
4. Run `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --exclude norte-gui
   --no-deps`.
5. Regenerate the protocol JSON Schema and compare it with
   `norte-proto/schema/`. Any change without a protocol version bump is a
   BLOCKER.
6. Run the complete `just ci` suite.
7. Confirm the artefacts: `just dist && just dist-smoke`. A release that does
   not start is the one packaging failure the user finds before we do.

Return GO or NO-GO and list every blocker.
