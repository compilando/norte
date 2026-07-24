# 0038 - Protocol JSON Schema artifact and cargo-semver-checks gate

- Status: accepted
- Date: 2026-07-24
- Decision makers: Oscar González
- Related: #13; ADR 0007 (layered configuration JSON Schema goldens), ADR 0035
  (norte-config crate); spec §11 (protocol artifact) and §13 (published
  schemas). The wire format is frozen per the `norte-proto` golden tests
  (`methods.json`) and the N/N-1 protocol-version window.

## Context

Two pre-release commitments from the M0 close (#13) are still open:

1. **Protocol JSON Schema (spec §11).** `norte-proto` freezes the wire format
   with hand-maintained golden fixtures (`tests/golden/types/methods.json`) and
   a protocol-version window, but publishes no machine-readable schema of the
   request/response/notification types. External clients (the MCP bridge, future
   third-party tooling) have no artifact to validate against or generate code
   from. `norte-config` already publishes JSON Schemas for `norte.toml` and the
   keymap file (ADR 0007), generated from the same serde structs that parse, and
   pinned by a golden test that fails when the code drifts
   (`crates/norte-tui/tests/schema.rs`, `NORTE_UPDATE_SCHEMA=1` to regenerate).
   The protocol has no equivalent.

2. **API break detection.** Nothing mechanically catches a breaking change to
   the publishable crates' public API before it ships. `norte-proto` in
   particular couples its Rust API to the wire format; a silent signature change
   can pass `just ci` today.

`norte-proto` serializes some types with hand-written `Serialize`/`Deserialize`
impls rather than derives — notably `VPath`, which treats filenames as bytes and
serializes as a single wire string (`to_wire`). Any schema generator must match
the *actual* serde output for those types, not a naive field walk.

## Decision

### 1. Generate the protocol JSON Schema from the serde types with `schemars`

Add an optional `schema` feature to `norte-proto` (mirroring `norte-config`):

- `schema = ["dep:schemars"]`; `schemars` is already a vetted workspace
  dependency (used by `norte-config`, cargo-deny clean), so no new supply-chain
  surface — this only extends its use to the protocol crate.
- `#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]` on every wire
  type reachable from the schema root. The feature is off by default, so the
  shipped `norte-proto` gains nothing at runtime and no dependency; the derive
  exists only when the schema is generated.
- A **hand-written `JsonSchema` impl for `VPath`** (behind the same feature)
  declaring it a string, matching its `to_wire`/`Serialize` contract. This is
  the one type whose schema cannot be derived because its serde is custom.
- A golden test generates `docs/schema/proto.schema.json` via `schema_for!` over
  an aggregate root of the top-level wire *payload* types (the typed
  request/response/notification bodies; the JSON-RPC envelope wrappers carry
  opaque `Value` params/results and are intentionally excluded) and pins it,
  byte-for-byte, exactly like the config schema goldens (regenerate with
  `NORTE_UPDATE_SCHEMA=1`). The proto
  `schema` feature joins the `just ci` test line so the pin runs in the gate.

The schema is generated from the *same* structs that (de)serialize the wire, so
it cannot silently diverge from what the daemon actually speaks — the golden
turns any drift red.

### 2. Add `cargo-semver-checks` for the publishable crates

Add a `just semver` recipe running `cargo semver-checks` over the publishable
crates against a git-tag baseline, and wire it into `just ci` per the maintainer
decision, so an accidental breaking change to a public API turns the gate red
before commit.

**Caveat (implementation status):** `cargo-semver-checks` is a developer tool
installed out of band (`cargo install cargo-semver-checks`), not a crate
dependency, and it is not yet present on the primary dev machine. Wiring it into
`ci` before it is installed would break every `just ci` run. Therefore the gate
wiring lands only once the binary is installed; until then the recipe exists
standalone and the `ci` target keeps its current steps. This is tracked on #13.

## Options considered

### Schema generation

- **`schemars` derive (chosen).** Same mechanism as `norte-config`, one vetted
  dependency, generated from the parsing structs so it cannot drift, golden-pinned.
  Drawback: ~90 wire types each gain a feature-gated derive line, and `VPath`
  (plus any other custom-serde type) needs a hand-written impl.
- **Hand-written schema generator.** No new dependency use, but reimplements
  what `schemars` does, is fragile against type changes, and re-introduces the
  exact drift risk the golden is meant to remove. Rejected.
- **Defer the schema, ship only semver-checks now.** Smaller, but leaves the
  spec §11 artifact — the concrete external-client deliverable — unbuilt.
  Rejected: the schema is the higher-value half and is self-contained.

### Semver gate placement

- **In `just ci` (chosen).** Catches breaks before commit, at the cost of gate
  time and an install prerequisite. Accepted with the install caveat above.
- **Separate `just semver` recipe only.** Cheaper gate, but a break can land
  and only surface at release time. The recipe still exists for manual/pre-release
  runs; the decision is to *also* wire it into `ci` once installable.

## Consequences

### Positive

- External clients get a machine-readable, always-current protocol schema
  (`docs/schema/proto.schema.json`), generated from the code that speaks the wire.
- Wire drift is caught by a byte-exact golden, consistent with the config schemas.
- Public-API breaks in publishable crates become a gate failure rather than a
  release-day surprise.

### Negative

- ~90 `norte-proto` types carry a feature-gated `JsonSchema` derive; custom-serde
  types need a hand-written impl kept in sync with their `Serialize`.
- A regenerated `proto.schema.json` must accompany any wire-shape change, one
  more golden to republish (a deliberate forcing function).
- The semver gate depends on an out-of-band binary; the gate wiring is staged
  behind that install to avoid breaking `just ci` for contributors without it.
