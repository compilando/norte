---
description: Change the wire protocol — the version bump, the goldens, the schema and the compatibility window, in the order that keeps them consistent
argument-hint: <what the change adds, for example "a dest_anchor on fs.copy">
---
Change the wire protocol to add: $ARGUMENTS

**Read `docs/adr/0004-*` and `docs/adr/0011-*` before touching anything.** The
rules below are what this repository learned by breaking them.

1. **Decide whether it is additive.** A new optional field, a new enum variant
   on a `#[non_exhaustive]` type, a new method: additive. Renaming a field,
   changing a type, making an optional field required: not, and that needs its
   own ADR before code.

2. **Write the type change in `crates/norte-proto`.** New optional fields carry
   `#[serde(default, skip_serializing_if = "Option::is_none")]` so ordinary JSON
   does not change. Document *why the field exists*, not what it holds — the
   rustdoc is where the next person learns whether they may ignore it.

3. **Bump `PROTOCOL_VERSION`** and write the paragraph above it: what the
   version adds, and **what a peer one version behind loses**. That sentence is
   the contract. If the honest answer is "the check silently does not happen",
   say exactly that — see `expected_digest` (ADR 0071) and `dest_anchor`
   (ADR 0073) for the wording.

4. **Move the compatibility window** in `crates/norte-proto/tests/types.rs`
   (`version_ventana_actual`). N and N-1 pass, N-2 fails. The window *shifts*,
   it never widens — and being additive does not widen it either.

5. **Update the goldens.** `tests/golden/types/*.json` and their Rust cases
   cover each other 1:1, so a new error variant or params field needs both
   sides. A closed vocabulary gets **one fixture per value**: a single one lets
   the others be renamed with nothing noticing.

6. **Regenerate the schema**:
   `NORTE_UPDATE_SCHEMA=1 cargo test -p norte-proto --features schema --test schema`.
   `docs/schema/proto.schema.json` ships with the release binaries, so it is not
   a build artifact — it is the document a third party writes a client from.

7. **Follow it downstream.** The daemon handler, the SDK, `norte-mcp`, and any
   frontend that has to *decide* something with the new field. A field nobody
   reads is a field that does not exist.

8. **If the bridge DTOs change too, bump `BRIDGE_VERSION` on BOTH sides** —
   `crates/norte-ui-host/src/bridge.rs` and
   `crates/norte-gui-tauri/ui/src/types.ts` — and the `envelope.json` golden.
   `just ci-fast` does **not** run `gui-ci`, so a half-done bump goes green
   locally and breaks the renderer contract silently. It has happened.

9. **Dispatch `protocol-guardian`** before committing. It is mandatory for this
   surface (see CLAUDE.md), and it is the reviewer that has caught the
   compatibility mistakes.

10. Changelog entry naming the protocol version, and an ADR when the change
    encodes a decision rather than a field.
