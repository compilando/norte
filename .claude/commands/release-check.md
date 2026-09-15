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
2. Run `cargo deny check` for licenses and advisories. It now covers the whole
   workspace: the GPUI frontend, which had its own policy because its tree
   pulled git sources and licences the workspace does not allow, was retired
   (ADR 0065).
3. Confirm that release-plz can derive changelog entries from commits since the
   latest tag.
4. Run `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`.
5. Regenerate the protocol JSON Schema and compare it with
   `norte-proto/schema/`. Any change without a protocol version bump is a
   BLOCKER.
6. Run the complete `just ci` suite.
7. Confirm the artefacts: `just baseline <tag>` must end with `verificado:`.
   It builds every artefact of the tag on the pinned Ubuntu 22.04 image,
   refuses a binary above glibc 2.35 or with another revision, and
   smoke-tests each artefact on the pinned distribution matrix (ADR 0112). A
   release that does not start is the one packaging failure the user finds
   before we do; a smoke run on the build machine cannot see it.

Return GO or NO-GO and list every blocker.
