# Configurable columns — block 1 (wire: provider attributes) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Put typed, on-demand provider attributes on the wire (proto 0.30.0) so later blocks can render protocol-specific columns — POSIX mode/owner, S3 storage class, archive packed size — without any frontend guessing.

**Architecture:** Three additive wire changes, all `skip_serializing_if`-guarded so a 0.29 peer sees byte-identical payloads: `FsCapabilitiesResult.attrs` advertises what a provider offers (`AttrInfo`), `FsListParams.attrs`/`FsStatParams.attrs` request ids, and `Entry.attrs` carries the values as a `BTreeMap<String, AttrValue>`. `AttrValue` is a typed enum with a hand-written `Deserialize` that degrades an unknown variant to `AttrValue::Unknown` instead of failing the whole `Entry`. This block is **wire-only**: no provider produces attributes yet and the daemon ignores requested ids (absence is already a valid answer). Block 2 wires the VFS.

**Tech Stack:** Rust, `serde` (hand-written impls for the degrading enum, same pattern as `CapabilityFlags`), `base64` 0.22 (new dependency for `norte-proto`, already a vetted workspace dep), `schemars` behind the existing `schema` feature, `cargo nextest`.

**Reference:** `docs/superpowers/specs/2026-07-24-columns-design.md` (§ Layer 2 — the wire).

---

## File Structure

| File | Responsibility |
|---|---|
| `docs/adr/0039-provider-attributes-wire.md` (create) | The decision record: why typed on-demand attributes, why a degrading enum, the caps. |
| `docs/adr/README.md` (modify, last table row) | Index row for 0039. |
| `crates/norte-proto/src/attrs.rs` (create) | `AttrId` validation, wire caps, `AttrType`, `AttrHint`, `AttrInfo`, `AttrValue` + its hand-written serde. One module, one responsibility: the attribute vocabulary. |
| `crates/norte-proto/src/lib.rs` (modify) | `pub mod attrs;` + re-exports next to the existing ones. |
| `crates/norte-proto/src/entry.rs` (modify) | `Entry.attrs` field + doctest. |
| `crates/norte-proto/src/methods.rs` (modify) | `FsListParams.attrs`, `FsStatParams.attrs`, `FsCapabilitiesResult.attrs`, `PROTOCOL_VERSION` = `0.30.0` + version-history rustdoc. |
| `crates/norte-proto/Cargo.toml` (modify) | `base64.workspace = true`. |
| `crates/norte-proto/tests/golden/types/entry.json` (modify) | Fixture with attributes + fixture proving an empty map is omitted. |
| `crates/norte-proto/tests/golden/types/attr_value.json` (create) | One fixture per `AttrValue` variant. |
| `crates/norte-proto/tests/golden/types/methods.json` (modify) | Fixtures for the three changed method types. |
| `crates/norte-proto/tests/golden_types.rs` (modify) | Rust cases for every new/changed fixture (the harness asserts 1:1 coverage). |
| `crates/norte-proto/tests/types.rs` (modify) | Degradation, validation, N/N-1 window tests. |
| `crates/norte-proto/tests/schema.rs` (modify) | New types as `ProtocolSchema` fields. |
| `docs/schema/proto.schema.json` (regenerate) | Published artifact (ADR 0038 gate). |
| `CHANGELOG.md` (modify) | Unreleased entry. |
| ~37 files across the workspace (modify) | Mechanical `attrs: Default::default(),` in `Entry { … }` literals, compiler-driven. |

---

### Task 1: ADR 0039

**Files:**
- Create: `docs/adr/0039-provider-attributes-wire.md`
- Modify: `docs/adr/README.md` (append one row to the table)

- [ ] **Step 1: Write the ADR**

Create `docs/adr/0039-provider-attributes-wire.md` with exactly this content:

```markdown
# 0039 - Provider attributes on the wire

- Status: accepted
- Date: 2026-07-24
- Decision makers: Oscar González
- Related: spec §5 (`Capabilities`), §6.1 (listing presentation); ADR 0004
  (wire evolution: an unknown value degrades, it never breaks), ADR 0017
  (cursor pagination), ADR 0037 (plugin columns), ADR 0038 (protocol JSON
  Schema gate). Design: `docs/superpowers/specs/2026-07-24-columns-design.md`.

## Context

`Entry` carries `path`, `kind`, `size` and `mtime_ms`. Every other piece of
metadata a real provider knows — POSIX mode/uid/gid on local and sftp, the
owner and group strings an SFTP server sends, an S3 object's storage class and
etag, the compression method and packed size of an archive member — has no way
to reach a frontend. Configurable columns (the design this ADR serves) need
exactly that data, and each protocol has a different set of it.

Plugin columns (ADR 0037) do not solve this: a `columns` plugin runs in a WASM
sandbox with no access to the provider, so it cannot read an SFTP mode bit.

Three shapes were considered:

1. Provider-formatted strings. Trivial, but the value arrives pre-rendered:
   sorting a "1.2 GiB" column is lexicographic nonsense and the user loses all
   control over the format.
2. A closed set of new typed `Entry` fields (`mode`, `uid`, `etag`, …). Types
   survive, extensibility does not — the next provider needs another wire bump.
3. A namespaced, typed, provider-declared attribute map. Extensible, sortable,
   formattable.

## Decision

### 1. Attributes are declared, requested, and typed

- `FsCapabilitiesResult` gains `attrs: Vec<AttrInfo>`, where `AttrInfo` is
  `{ id, label, type, hint }`. This is discovery: a client learns which
  attributes exist for a provider from the call it already makes.
- `FsListParams` and `FsStatParams` gain `attrs: Vec<String>` — the client asks
  for the ids it will paint. Nothing is delivered unrequested.
- `Entry` gains `attrs: BTreeMap<String, AttrValue>`, where `AttrValue` is
  `Uint | Int | Text | Bytes | TimeMs | Bool | Unknown`.

`AttrType` (the declared type) and `AttrHint` (`Size`, `Timestamp`, `Mode`,
`Identity`, `Opaque` — the default format and alignment a frontend should pick)
are separate: two `Uint` attributes are formatted very differently depending on
whether they are a byte count or a permission word.

### 2. On demand, not always

The data providers publish is already in the response they parse (`statx`, the
SFTP attribute record, `ListObjectsV2`, the ZIP/TAR header), so the cost is
materialising and serialising it, not fetching it. A listing of 100 000 entries
must not pay for columns nobody displays, and the lazy `d_type` fast path in
`norte-vfs-local` (#52) must stay reachable. Requested-ids-only keeps both.

### 3. An unknown value degrades; it does not break the listing

`AttrValue` carries data, so `#[serde(other)]` cannot express a catch-all
variant. `AttrValue` therefore implements `Deserialize` by hand — the same
route `CapabilityFlags` already takes — and maps an unrecognised variant tag to
`AttrValue::Unknown`. A protocol-N+1 daemon that adds a variant degrades one
cell in one entry rather than failing the whole page, which is the ADR 0004
contract applied at value granularity.

A malformed base64 payload in `bytes_b64` degrades the same way, for the same
reason: one corrupt cell must not cost the user their listing. A malformed
*envelope* (a non-object, or an object with no keys) is still a hard error —
that is a broken peer, not a newer one.

`AttrValue::Unknown` serialises as `{"unknown": null}`. A conforming daemon
never emits it; it exists so a value that was read can be written back without
panicking, exactly as `EntryKind::Other` round-trips as `"other"`.

### 4. Attribute ids are namespaced and validated, labels are untrusted

An id matches `[a-z0-9._-]{1,64}` and is namespaced by its origin:
`posix.mode`, `sftp.owner`, `s3.storage_class`, `archive.packed_size`. There is
no central registry — a provider owns its namespace. Both peers validate;
a malformed id in a request is `-32602`.

`AttrInfo::label` and any `Text`/`Bytes` value is **third-party text**: an SFTP
server controls `sftp.owner`, and a WASM provider plugin controls its own
labels. Frontends mask both through `norte_frontend::display_name` exactly as
they already mask a plugin's column header, and render `Bytes` through the
lossy-with-badge path used for non-UTF-8 filenames. The bytes themselves are
preserved (hard rule 1); only the rendering is lossy.

### 5. Caps

Enforced by the server, re-validated by the client: at most 16 requested ids
per call, id ≤ 64 bytes, `AttrInfo::label` ≤ 64 bytes, `Text` ≤ 256 bytes,
`Bytes` ≤ 256 bytes decoded. The ceiling a listing page can add is therefore
bounded and predictable. Requesting an unknown id is **not** an error: it comes
back absent, so a client holding a stale catalog degrades instead of failing.

## Consequences

- Protocol 0.30.0. Purely additive: all three fields are
  `skip_serializing_if`-guarded, so a 0.29 peer emits and receives exactly
  today's bytes. The N/N-1 window moves to N=0.30.x / N-1=0.29.x.
- `norte-proto` gains a `base64` dependency (0.22, already a vetted workspace
  dependency used by `norte-core` for `fs.read`). It is needed because
  `AttrValue::Bytes` owns its decode: leaving the value as a base64 `String`
  would push error handling onto every consumer and invite a second, divergent
  decode path.
- Adding a field to `Entry` touches every struct literal in the workspace
  (~120, mostly tests). Mechanical, compiler-driven, one commit.
- This block ships no producer: no provider advertises an attribute and the
  daemon ignores requested ids. That is honest under the contract — absence is
  a valid answer — and keeps the wire change reviewable on its own.
```

- [ ] **Step 2: Add the index row**

Append to the table at the end of `docs/adr/README.md`:

```markdown
| [0039](0039-provider-attributes-wire.md) | Provider attributes on the wire (typed, on-demand, degrading) | accepted |
```

- [ ] **Step 3: Commit**

```bash
git add docs/adr/0039-provider-attributes-wire.md docs/adr/README.md
git commit -m "docs(adr): 0039 — provider attributes on the wire"
```

---

### Task 2: `AttrType`, `AttrHint`, `AttrInfo`, id validation, caps

**Files:**
- Create: `crates/norte-proto/src/attrs.rs`
- Modify: `crates/norte-proto/src/lib.rs`
- Test: inline `#[cfg(test)]` in `crates/norte-proto/src/attrs.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/norte-proto/src/attrs.rs` containing ONLY this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_validos_y_rechazados() {
        for ok in ["posix.mode", "s3.storage_class", "archive.packed-size", "a"] {
            assert!(is_valid_attr_id(ok), "{ok} debería valer");
        }
        for bad in ["", "Posix.mode", "posix mode", "posix/mode", "ñ", &"a".repeat(65)] {
            assert!(!is_valid_attr_id(bad), "{bad:?} debería rechazarse");
        }
    }

    #[test]
    fn attr_type_desconocido_degrada() {
        let t: AttrType = serde_json::from_str("\"tipo_del_futuro\"").unwrap();
        assert_eq!(t, AttrType::Unknown);
    }

    #[test]
    fn attr_hint_desconocido_degrada() {
        let h: AttrHint = serde_json::from_str("\"pista_del_futuro\"").unwrap();
        assert_eq!(h, AttrHint::Unknown);
    }

    #[test]
    fn attr_info_round_trip() {
        let info = AttrInfo {
            id: "posix.mode".to_owned(),
            label: "Mode".to_owned(),
            ty: AttrType::Uint,
            hint: AttrHint::Mode,
        };
        let wire = serde_json::to_string(&info).unwrap();
        assert!(wire.contains("\"type\":\"uint\""), "el campo se llama `type` en el wire: {wire}");
        assert_eq!(serde_json::from_str::<AttrInfo>(&wire).unwrap(), info);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p norte-proto attrs`
Expected: FAIL — `cannot find function is_valid_attr_id` / `cannot find type AttrType`.

- [ ] **Step 3: Write the implementation**

Prepend to `crates/norte-proto/src/attrs.rs` (above the test module):

```rust
//! Provider attributes (0.30.0, ADR 0039): typed, on-demand metadata a
//! provider publishes beyond [`Entry`](crate::Entry)'s four fields — POSIX
//! mode, SFTP owner, S3 storage class, archive packed size.
//!
//! Three pieces: [`AttrInfo`] (what a provider offers, discovered through
//! `fs.capabilities`), an id requested in `fs.list`/`fs.stat`, and
//! [`AttrValue`] (the cell itself, carried in `Entry::attrs`).

use serde::{Deserialize, Serialize};

/// Maximum requested attribute ids per `fs.list`/`fs.stat` call. Enforced
/// server-side; more than this is `-32602`.
pub const ATTRS_MAX_REQUEST: usize = 16;
/// Maximum length of an attribute id, in bytes.
pub const ATTR_ID_MAX: usize = 64;
/// Maximum length of [`AttrInfo::label`], in bytes.
pub const ATTR_LABEL_MAX: usize = 64;
/// Maximum length of an [`AttrValue::Text`] value, in bytes.
pub const ATTR_TEXT_MAX: usize = 256;
/// Maximum length of an [`AttrValue::Bytes`] value, in bytes AFTER decoding.
pub const ATTR_BYTES_MAX: usize = 256;

/// Is `id` a well-formed attribute id: `[a-z0-9._-]`, non-empty, at most
/// [`ATTR_ID_MAX`] bytes. Namespaced by its origin (`posix.mode`), with no
/// central registry — a provider owns its namespace (ADR 0039).
///
/// ```
/// use norte_proto::attrs::is_valid_attr_id;
/// assert!(is_valid_attr_id("s3.storage_class"));
/// assert!(!is_valid_attr_id("S3.StorageClass"));
/// ```
#[must_use]
pub fn is_valid_attr_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= ATTR_ID_MAX
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
}

/// Declared type of an attribute. An unknown value (protocol N+1) degrades to
/// [`AttrType::Unknown`]: an old client ignores the attribute, it never breaks.
///
/// ```
/// use norte_proto::attrs::AttrType;
/// let t: AttrType = serde_json::from_str("\"tipo_del_futuro\"").unwrap();
/// assert_eq!(t, AttrType::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttrType {
    /// Unsigned integer.
    Uint,
    /// Signed integer.
    Int,
    /// UTF-8 text (third-party: mask before painting).
    Text,
    /// Raw bytes, base64 on the wire (a name that is not UTF-8).
    Bytes,
    /// Milliseconds since the UTC epoch; negative is valid (pre-1970).
    TimeMs,
    /// Boolean.
    Bool,
    /// Type of a newer protocol.
    #[serde(other)]
    Unknown,
}

/// What an attribute MEANS, so a frontend can pick a default format and
/// alignment without a hard-coded table of ids. Two `Uint`s render very
/// differently depending on this.
///
/// ```
/// use norte_proto::attrs::AttrHint;
/// let h: AttrHint = serde_json::from_str("\"pista_del_futuro\"").unwrap();
/// assert_eq!(h, AttrHint::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttrHint {
    /// A byte count: right-aligned, IEC/SI formatting applies.
    Size,
    /// A point in time: date formatting applies.
    Timestamp,
    /// A POSIX permission word: octal or `rwx` formatting applies.
    Mode,
    /// A user/group/owner identity: short text or a number.
    Identity,
    /// No presentation advice; render as plain text.
    Opaque,
    /// Hint of a newer protocol.
    #[serde(other)]
    Unknown,
}

/// One attribute a provider offers, as advertised by `fs.capabilities`
/// ([`FsCapabilitiesResult::attrs`](crate::methods::FsCapabilitiesResult)).
///
/// `label` is provider text and therefore THIRD-PARTY (a WASM provider plugin
/// writes it, an SFTP server influences it): mask it exactly like a plugin's
/// column header before painting, and clamp it to [`ATTR_LABEL_MAX`].
///
/// ```
/// use norte_proto::attrs::{AttrHint, AttrInfo, AttrType};
/// let info = AttrInfo {
///     id: "posix.mode".to_owned(),
///     label: "Mode".to_owned(),
///     ty: AttrType::Uint,
///     hint: AttrHint::Mode,
/// };
/// let wire = serde_json::to_string(&info).unwrap();
/// assert!(wire.contains("\"type\":\"uint\""));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AttrInfo {
    /// Attribute id, namespaced ([`is_valid_attr_id`]).
    pub id: String,
    /// Human label. THIRD-PARTY text — mask and clamp before painting.
    pub label: String,
    /// Declared type of the values of this attribute.
    #[serde(rename = "type")]
    pub ty: AttrType,
    /// Presentation hint.
    pub hint: AttrHint,
}
```

Add to `crates/norte-proto/src/lib.rs`, next to the existing module declarations
and re-exports:

```rust
pub mod attrs;
pub use attrs::{AttrHint, AttrInfo, AttrType};
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p norte-proto attrs && cargo test -p norte-proto --doc attrs`
Expected: PASS (4 unit tests, 3 doctests).

- [ ] **Step 5: Lint**

Run: `cargo clippy -p norte-proto --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-proto/src/attrs.rs crates/norte-proto/src/lib.rs
git commit -m "feat(proto): attribute vocabulary — AttrInfo, AttrType, AttrHint, id validation"
```

---

### Task 3: `AttrValue` with degrading deserialisation

**Files:**
- Modify: `crates/norte-proto/src/attrs.rs`
- Modify: `crates/norte-proto/Cargo.toml`
- Test: inline `#[cfg(test)]` in `crates/norte-proto/src/attrs.rs`

- [ ] **Step 1: Write the failing tests**

Append to the `mod tests` block in `crates/norte-proto/src/attrs.rs`:

```rust
    fn round_trip(v: &AttrValue) -> AttrValue {
        let wire = serde_json::to_string(v).unwrap();
        serde_json::from_str(&wire).unwrap()
    }

    #[test]
    fn cada_variante_round_trip() {
        for v in [
            AttrValue::Uint(33188),
            AttrValue::Int(-7),
            AttrValue::Text("STANDARD_IA".to_owned()),
            AttrValue::Bytes(vec![0xFF, 0xFE, b'a']),
            AttrValue::TimeMs(-86_400_000),
            AttrValue::Bool(true),
        ] {
            assert_eq!(round_trip(&v), v);
        }
    }

    #[test]
    fn wire_de_bytes_es_base64() {
        let wire = serde_json::to_string(&AttrValue::Bytes(vec![0xFF, 0xFE])).unwrap();
        assert_eq!(wire, r#"{"bytes_b64":"//4="}"#);
    }

    #[test]
    fn variante_del_futuro_degrada_a_unknown() {
        let v: AttrValue = serde_json::from_str(r#"{"quaternion":[1,2,3,4]}"#).unwrap();
        assert_eq!(v, AttrValue::Unknown);
    }

    #[test]
    fn base64_corrupto_degrada_a_unknown() {
        let v: AttrValue = serde_json::from_str(r#"{"bytes_b64":"no es base64 !!"}"#).unwrap();
        assert_eq!(v, AttrValue::Unknown);
    }

    #[test]
    fn envelope_roto_es_error_duro() {
        // Un peer ROTO (no uno más nuevo): no hay nada que degradar.
        assert!(serde_json::from_str::<AttrValue>("42").is_err());
        assert!(serde_json::from_str::<AttrValue>("{}").is_err());
    }

    #[test]
    fn unknown_serializa_y_vuelve_a_unknown() {
        assert_eq!(
            serde_json::to_string(&AttrValue::Unknown).unwrap(),
            r#"{"unknown":null}"#
        );
        assert_eq!(round_trip(&AttrValue::Unknown), AttrValue::Unknown);
    }

    #[test]
    fn una_entrada_con_una_celda_futura_sobrevive_entera() {
        // El punto de la degradación: la ENTRADA no se pierde por una celda.
        let json = r#"{"a":{"uint":1},"b":{"quaternion":[0]},"c":{"bool":false}}"#;
        let map: std::collections::BTreeMap<String, AttrValue> =
            serde_json::from_str(json).unwrap();
        assert_eq!(map["a"], AttrValue::Uint(1));
        assert_eq!(map["b"], AttrValue::Unknown);
        assert_eq!(map["c"], AttrValue::Bool(false));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p norte-proto attrs`
Expected: FAIL — `cannot find type AttrValue in this scope`.

- [ ] **Step 3: Add the dependency**

In `crates/norte-proto/Cargo.toml`, under `[dependencies]`, after `bitflags.workspace = true`:

```toml
# `AttrValue::Bytes` owns its decode (ADR 0039): the wire carries base64 and
# the type hands consumers real bytes, so there is exactly ONE decode path and
# a corrupt cell degrades in one place. Already a vetted workspace dependency
# (`norte-core` uses it for `fs.read`); MIT OR Apache-2.0, no transitive deps.
base64.workspace = true
```

- [ ] **Step 4: Write the implementation**

Append to `crates/norte-proto/src/attrs.rs` (before the test module):

```rust
/// The value of one attribute for one entry (ADR 0039).
///
/// Absence of a key in [`Entry::attrs`](crate::Entry) means the provider does
/// not know the value — there is never a fabricated `0`, exactly as
/// `Entry::size` is `None` rather than zero.
///
/// Wire: a one-key object tagged by variant — `{"uint": 33188}`,
/// `{"bytes_b64": "//4="}`. Deserialisation is hand-written (the same route
/// [`CapabilityFlags`](crate::CapabilityFlags) takes) so that a variant from a
/// newer protocol degrades to [`AttrValue::Unknown`] instead of failing the
/// whole entry: `#[serde(other)]` cannot express a catch-all on a
/// data-carrying enum.
///
/// ```
/// use norte_proto::attrs::AttrValue;
/// let v: AttrValue = serde_json::from_str(r#"{"variante_del_futuro": 1}"#).unwrap();
/// assert_eq!(v, AttrValue::Unknown);
/// let n: AttrValue = serde_json::from_str(r#"{"uint": 33188}"#).unwrap();
/// assert_eq!(n, AttrValue::Uint(33188));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AttrValue {
    /// Unsigned integer (mode word, packed size, uid).
    Uint(u64),
    /// Signed integer.
    Int(i64),
    /// UTF-8 text. THIRD-PARTY — mask before painting.
    Text(String),
    /// Raw bytes (base64 on the wire): an owner name that is not UTF-8.
    Bytes(Vec<u8>),
    /// Milliseconds since the UTC epoch; negative is valid.
    TimeMs(i64),
    /// Boolean.
    Bool(bool),
    /// A value this protocol version does not understand, or a corrupt
    /// base64 payload. Never emitted by a conforming daemon; it exists so one
    /// bad cell costs one cell, not the listing.
    Unknown,
}

impl Serialize for AttrValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use base64::Engine as _;
        use serde::ser::SerializeMap as _;

        let mut map = serializer.serialize_map(Some(1))?;
        match self {
            AttrValue::Uint(v) => map.serialize_entry("uint", v)?,
            AttrValue::Int(v) => map.serialize_entry("int", v)?,
            AttrValue::Text(v) => map.serialize_entry("text", v)?,
            AttrValue::Bytes(v) => map.serialize_entry(
                "bytes_b64",
                &base64::engine::general_purpose::STANDARD.encode(v),
            )?,
            AttrValue::TimeMs(v) => map.serialize_entry("time_ms", v)?,
            AttrValue::Bool(v) => map.serialize_entry("bool", v)?,
            AttrValue::Unknown => map.serialize_entry("unknown", &Option::<()>::None)?,
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for AttrValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ValueVisitor;

        impl<'de> serde::de::Visitor<'de> for ValueVisitor {
            type Value = AttrValue;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a one-key attribute value object, e.g. {\"uint\": 1}")
            }

            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<AttrValue, M::Error> {
                use base64::Engine as _;

                let Some(tag) = map.next_key::<String>()? else {
                    // Objeto VACÍO: peer roto, no peer nuevo. Error duro.
                    return Err(serde::de::Error::custom("empty attribute value object"));
                };
                let value = match tag.as_str() {
                    "uint" => AttrValue::Uint(map.next_value()?),
                    "int" => AttrValue::Int(map.next_value()?),
                    "text" => AttrValue::Text(map.next_value()?),
                    "bytes_b64" => {
                        let b64: String = map.next_value()?;
                        match base64::engine::general_purpose::STANDARD.decode(&b64) {
                            Ok(bytes) => AttrValue::Bytes(bytes),
                            // Celda corrupta: degrada la CELDA, no la entrada.
                            Err(_) => AttrValue::Unknown,
                        }
                    }
                    "time_ms" => AttrValue::TimeMs(map.next_value()?),
                    "bool" => AttrValue::Bool(map.next_value()?),
                    _ => {
                        // Variante de un protocolo MÁS NUEVO (ADR 0004).
                        map.next_value::<serde::de::IgnoredAny>()?;
                        AttrValue::Unknown
                    }
                };
                // Claves de más: se ignoran (mismo criterio indulgente que
                // cualquier campo desconocido de un struct del wire).
                while map.next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?.is_some() {}
                Ok(value)
            }
        }

        deserializer.deserialize_map(ValueVisitor)
    }
}

// `AttrValue` serializes as a one-key tagged object, so its schema is an
// object with one optional property per variant; the serde impls are
// hand-written and cannot derive (same situation as `CapabilityFlags`).
#[cfg(feature = "schema")]
impl schemars::JsonSchema for AttrValue {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "AttrValue".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "description": "One-key tagged attribute value; an unknown key degrades to Unknown.",
            "properties": {
                "uint": { "type": "integer", "minimum": 0 },
                "int": { "type": "integer" },
                "text": { "type": "string" },
                "bytes_b64": { "type": "string" },
                "time_ms": { "type": "integer" },
                "bool": { "type": "boolean" },
                "unknown": { "type": "null" }
            },
            "minProperties": 1
        })
    }
}
```

Extend the `lib.rs` re-export from Task 2:

```rust
pub use attrs::{AttrHint, AttrInfo, AttrType, AttrValue};
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo nextest run -p norte-proto attrs && cargo test -p norte-proto --doc attrs`
Expected: PASS (11 unit tests, 4 doctests).

- [ ] **Step 6: Check the schema feature compiles**

Run: `cargo clippy -p norte-proto --all-targets --features schema -- -D warnings`
Expected: clean. If `json_schema!` is unavailable in the pinned `schemars`
version, build the schema with `serde_json::from_value(serde_json::json!({…}))`
into `schemars::Schema` instead — check how `CapabilityFlags` does it in
`crates/norte-proto/src/caps.rs` and mirror that exactly.

- [ ] **Step 7: Commit**

```bash
git add crates/norte-proto/src/attrs.rs crates/norte-proto/src/lib.rs crates/norte-proto/Cargo.toml Cargo.lock
git commit -m "feat(proto): AttrValue — typed cell that degrades an unknown variant"
```

---

### Task 4: `Entry.attrs` and the workspace-wide mechanical migration

**Files:**
- Modify: `crates/norte-proto/src/entry.rs`
- Modify: ~37 files across `crates/` (compiler-driven, `Entry { … }` literals)
- Test: `crates/norte-proto/tests/golden/types/entry.json`, `crates/norte-proto/tests/golden_types.rs`

- [ ] **Step 1: Write the failing golden fixtures**

Add two cases to `crates/norte-proto/tests/golden/types/entry.json` (keep the
five existing ones untouched — that is the proof the change is additive):

```json
  "con_attrs": {
    "path": "file:///home/user/doc.txt",
    "kind": "file",
    "size": 1234,
    "mtime_ms": 1720000000000,
    "attrs": {
      "posix.mode": { "uint": 33188 },
      "posix.uid": { "uint": 1000 },
      "sftp.owner": { "bytes_b64": "//4=" },
      "s3.storage_class": { "text": "STANDARD_IA" }
    }
  },
  "attrs_vacios_se_omiten": {
    "path": "file:///home/user/otro.txt",
    "kind": "file",
    "size": null,
    "mtime_ms": null
  }
```

Add the matching Rust cases in `crates/norte-proto/tests/golden_types.rs`, in
the `entry.json` family (the harness asserts fixture names and case names cover
each other 1:1):

```rust
            (
                "con_attrs",
                Entry {
                    path: vpath("file:///home/user/doc.txt"),
                    kind: EntryKind::File,
                    size: Some(1234),
                    mtime_ms: Some(1_720_000_000_000),
                    attrs: BTreeMap::from([
                        ("posix.mode".to_owned(), AttrValue::Uint(33188)),
                        ("posix.uid".to_owned(), AttrValue::Uint(1000)),
                        ("sftp.owner".to_owned(), AttrValue::Bytes(vec![0xFF, 0xFE])),
                        (
                            "s3.storage_class".to_owned(),
                            AttrValue::Text("STANDARD_IA".to_owned()),
                        ),
                    ]),
                },
            ),
            (
                "attrs_vacios_se_omiten",
                Entry {
                    path: vpath("file:///home/user/otro.txt"),
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                    attrs: BTreeMap::new(),
                },
            ),
```

Add `AttrValue` to the `use norte_proto::{…}` list at the top of
`golden_types.rs` (`BTreeMap` is already imported).

- [ ] **Step 2: Run the golden test to verify it fails**

Run: `cargo nextest run -p norte-proto --test golden_types`
Expected: FAIL — `struct Entry has no field named attrs`.

- [ ] **Step 3: Add the field**

In `crates/norte-proto/src/entry.rs`, add to `struct Entry` after `mtime_ms`:

```rust
    /// Provider attributes (0.30.0, ADR 0039): typed metadata BEYOND the four
    /// fields above, delivered only for the ids the client requested in
    /// `fs.list`/`fs.stat`. An absent key means the provider does not know the
    /// value — never a fabricated one. Empty (the default, and the only
    /// possibility for a 0.29 peer) is omitted from the wire entirely.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub attrs: std::collections::BTreeMap<String, crate::attrs::AttrValue>,
```

Update the module-level `use` line and the `Entry` doctest in the same file:

```rust
/// ```
/// use norte_proto::{Entry, EntryKind, VPath};
/// use norte_proto::attrs::AttrValue;
/// let e = Entry {
///     path: VPath::parse("file:///a.txt").unwrap(),
///     kind: EntryKind::File,
///     size: Some(42),
///     mtime_ms: None,
///     attrs: [("posix.mode".to_owned(), AttrValue::Uint(0o100_644))].into(),
/// };
/// let json = serde_json::to_string(&e).unwrap();
/// assert_eq!(serde_json::from_str::<Entry>(&json).unwrap(), e);
///
/// // Sin attrs, el wire es EXACTAMENTE el de 0.29.
/// let plain = Entry { attrs: Default::default(), ..e };
/// assert!(!serde_json::to_string(&plain).unwrap().contains("attrs"));
/// ```
```

- [ ] **Step 4: Migrate every `Entry { … }` literal in the workspace**

The field is required at construction, so ~120 literals across ~37 files stop
compiling. Fix them compiler-driven, never by hand-hunting:

```bash
cd /home/oscar/work/wot/projects/high/norte
cat > /tmp/claude-1000/fix_entry_attrs.py <<'PY'
import re, subprocess, sys
from collections import defaultdict

PAT = re.compile(r"^(.+?):(\d+):\d+: error\[E0063\]: missing field `attrs`")

for _ in range(20):
    out = subprocess.run(
        ["cargo", "check", "--workspace", "--all-targets",
         "--message-format=short"],
        capture_output=True, text=True).stderr
    hits = defaultdict(list)
    for line in out.splitlines():
        m = PAT.match(line.strip())
        if m:
            hits[m.group(1)].append(int(m.group(2)))
    if not hits:
        print("no quedan literales por migrar")
        sys.exit(0)
    for path, lines in hits.items():
        src = open(path).read().splitlines(keepends=True)
        for ln in sorted(set(lines), reverse=True):
            opening = src[ln - 1]
            assert opening.rstrip().endswith("{"), f"{path}:{ln} literal en una sola línea: arréglalo a mano"
            indent = " " * (len(opening) - len(opening.lstrip()) + 4)
            src.insert(ln, f"{indent}attrs: Default::default(),\n")
        open(path, "w").write("".join(src))
    print("ronda aplicada:", sum(len(v) for v in hits.values()), "literales")
print("no convergió en 20 rondas", file=sys.stderr); sys.exit(1)
PY
python3 /tmp/claude-1000/fix_entry_attrs.py
```

The assertion is the guard: a single-line literal (`Entry { path, kind, size: None, mtime_ms: None }`)
would get the insertion in the wrong place, so the script refuses and you fix
that one by hand. The loop re-runs `cargo check` because fixing one file can
reveal literals in a crate that previously failed to build.

- [ ] **Step 5: Verify the workspace builds and the goldens pass**

Run: `cargo check --workspace --all-targets && cargo nextest run -p norte-proto`
Expected: build clean; all `norte-proto` tests PASS, including the two new
`entry.json` cases and the untouched five.

- [ ] **Step 6: Verify the WASM guests still build**

Run: `just build-ftp-wasm`
Expected: success. The guests under `crates/norte-plugin-host/examples-wasm/`
build against WIT bindings, not `norte_proto::Entry` — if this fails, the guest
in question defines its own `Entry` and needs the same field only if it mirrors
the proto type; check before touching it.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(proto)!: Entry.attrs — provider attributes, omitted when empty

Additive on the wire (skip_serializing_if): a 0.29 peer emits and reads
exactly today's bytes. The struct literal migration across the workspace is
mechanical and compiler-driven."
```

---

### Task 5: request and discovery fields

**Files:**
- Modify: `crates/norte-proto/src/methods.rs` (`FsListParams`, `FsStatParams`, `FsCapabilitiesResult`)
- Test: `crates/norte-proto/tests/golden/types/methods.json`, `crates/norte-proto/tests/golden_types.rs`, `crates/norte-proto/tests/types.rs`

- [ ] **Step 1: Write the failing golden fixtures**

Add to `crates/norte-proto/tests/golden/types/methods.json` (leave the existing
`fs_list_params`, `fs_stat_params` and `fs_capabilities_result` entries exactly
as they are — unchanged fixtures are the additivity proof):

```json
  "fs_list_params_con_attrs": {
    "cursor": null,
    "limit": 500,
    "path": "file:///home/user",
    "attrs": ["posix.mode", "posix.uid"]
  },
  "fs_stat_params_con_attrs": {
    "path": "file:///home/user/doc.txt",
    "attrs": ["s3.storage_class"]
  },
  "fs_capabilities_result_con_attrs": {
    "capabilities": { "flags": "RENAME_ATOMIC | SYMLINKS", "max_path": null },
    "attrs": [
      { "id": "posix.mode", "label": "Mode", "type": "uint", "hint": "mode" },
      { "id": "sftp.owner", "label": "Owner", "type": "bytes", "hint": "identity" }
    ]
  }
```

Add the matching Rust cases to the three families in
`crates/norte-proto/tests/golden_types.rs`:

```rust
            (
                "fs_list_params_con_attrs",
                FsListParams {
                    path: vpath("file:///home/user"),
                    limit: Some(500),
                    cursor: None,
                    attrs: vec!["posix.mode".to_owned(), "posix.uid".to_owned()],
                },
            ),
```

```rust
            (
                "fs_stat_params_con_attrs",
                FsStatParams {
                    path: vpath("file:///home/user/doc.txt"),
                    attrs: vec!["s3.storage_class".to_owned()],
                },
            ),
```

```rust
            (
                "fs_capabilities_result_con_attrs",
                FsCapabilitiesResult {
                    capabilities: Capabilities {
                        flags: CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::SYMLINKS,
                        max_path: None,
                    },
                    attrs: vec![
                        AttrInfo {
                            id: "posix.mode".to_owned(),
                            label: "Mode".to_owned(),
                            ty: AttrType::Uint,
                            hint: AttrHint::Mode,
                        },
                        AttrInfo {
                            id: "sftp.owner".to_owned(),
                            label: "Owner".to_owned(),
                            ty: AttrType::Bytes,
                            hint: AttrHint::Identity,
                        },
                    ],
                },
            ),
```

Import `AttrHint, AttrInfo, AttrType` in `golden_types.rs`. If the existing
`Capabilities` literal in that file uses different field names, copy the shape
from the neighbouring `fs_capabilities_result` case rather than this snippet.

- [ ] **Step 2: Run the goldens to verify they fail**

Run: `cargo nextest run -p norte-proto --test golden_types`
Expected: FAIL — `struct FsListParams has no field named attrs`.

- [ ] **Step 3: Add the fields**

In `crates/norte-proto/src/methods.rs`, add to `FsListParams` after `cursor`:

```rust
    /// Ids of the provider attributes to deliver with each entry (0.30.0,
    /// ADR 0039). Empty (the default, and the only possibility for a 0.29
    /// client) = none: nothing is delivered unrequested. At most
    /// [`ATTRS_MAX_REQUEST`](crate::attrs::ATTRS_MAX_REQUEST) ids, each
    /// well-formed ([`is_valid_attr_id`](crate::attrs::is_valid_attr_id)) —
    /// violating either is `-32602`. An id the provider does not offer is NOT
    /// an error: it comes back absent, so a client with a stale catalog
    /// degrades instead of failing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attrs: Vec<String>,
```

Add the identical field (same doc comment, adjusted to `fs.stat`) to
`FsStatParams`, and to `FsCapabilitiesResult`:

```rust
    /// Provider attributes this provider offers (0.30.0, ADR 0039): the
    /// discovery half of `Entry::attrs`. Empty = the provider publishes none.
    /// `AttrInfo::label` is THIRD-PARTY text — a frontend masks and clamps it
    /// exactly as it does a plugin's column header.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attrs: Vec<crate::attrs::AttrInfo>,
```

- [ ] **Step 4: Run the goldens to verify they pass**

Run: `cargo nextest run -p norte-proto --test golden_types`
Expected: PASS. If a call site elsewhere in the workspace fails to build, add
`attrs: Vec::new(),` there (these three types have far fewer literals than
`Entry`; `cargo check --workspace --all-targets` lists them).

- [ ] **Step 5: Write the additivity and validation tests**

Append to `crates/norte-proto/tests/types.rs`:

```rust
/// (0.30.0, ADR 0039) Los tres campos nuevos son aditivos: un wire N-1 (0.29.x)
/// SIN ellos deserializa, y un valor vacío NO se emite.
#[test]
fn attrs_son_aditivos_en_ambas_direcciones() {
    use norte_proto::methods::{FsCapabilitiesResult, FsListParams};

    let n1 = r#"{"path":"file:///home","limit":null,"cursor":null}"#;
    let params: FsListParams = serde_json::from_str(n1).expect("wire N-1 válido");
    assert!(params.attrs.is_empty(), "ausente = ninguno pedido");

    let wire = serde_json::to_string(&params).unwrap();
    assert!(
        !wire.contains("attrs"),
        "vacío no se emite (byte-idéntico a 0.29): {wire}"
    );

    let caps_n1 = r#"{"capabilities":{"flags":"RENAME_ATOMIC","max_path":null}}"#;
    let caps: FsCapabilitiesResult = serde_json::from_str(caps_n1).expect("wire N-1 válido");
    assert!(caps.attrs.is_empty());
}

/// (0.30.0) Los ids del vocabulario que este bloque congela son válidos, y las
/// formas hostiles NO lo son — el gate vive en el tipo, no en cada llamador.
#[test]
fn ids_del_vocabulario_inicial_son_validos() {
    use norte_proto::attrs::is_valid_attr_id;

    for id in [
        "posix.mode",
        "posix.uid",
        "posix.gid",
        "posix.nlink",
        "posix.ctime_ms",
        "win.attributes",
        "sftp.owner",
        "sftp.group",
        "s3.storage_class",
        "s3.etag",
        "s3.content_type",
        "archive.method",
        "archive.packed_size",
        "archive.crc32",
    ] {
        assert!(is_valid_attr_id(id), "{id} es del vocabulario de ADR 0039");
    }
    for hostil in ["../etc/passwd", "posix.mode\u{202E}", "POSIX.MODE", ""] {
        assert!(!is_valid_attr_id(hostil), "{hostil:?} debe rechazarse");
    }
}
```

- [ ] **Step 6: Run them**

Run: `cargo nextest run -p norte-proto --test types`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/norte-proto/src/methods.rs crates/norte-proto/tests
git commit -m "feat(proto): fs.list/fs.stat request attrs, fs.capabilities advertises them"
```

---

### Task 6: version bump to 0.30.0

**Files:**
- Modify: `crates/norte-proto/src/methods.rs` (`PROTOCOL_VERSION` + the version-history rustdoc above it)
- Test: `crates/norte-proto/tests/types.rs:702-710` (the N/N-1 window test)

- [ ] **Step 1: Update the window test first**

In `crates/norte-proto/tests/types.rs`, replace the 0.29.0 block (around line 704):

```rust
    // 0.30.0 (ADR 0039): acepta 0.30.x (N) y 0.29.x (N-1), rechaza 0.28.x (N-2).
    assert!(version_compatible(PROTOCOL_VERSION, "0.30.9"), "N");
    assert!(version_compatible(PROTOCOL_VERSION, "0.29.0"), "N-1");
    assert!(
        !version_compatible(PROTOCOL_VERSION, "0.28.9"),
        "N-2 fuera de ventana"
    );
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo nextest run -p norte-proto --test types version`
Expected: FAIL — `PROTOCOL_VERSION` is still `0.29.0`, so `"0.30.9"` is a
client from the future and `version_compatible` returns `false`.

- [ ] **Step 3: Bump and document**

In `crates/norte-proto/src/methods.rs`, append to the version-history rustdoc
immediately above `PROTOCOL_VERSION` (after the `0.29.0` paragraph):

```rust
/// 0.30.0 (ADR 0039, columns block 1): ATRIBUTOS DE PROVIDER, tipados y bajo
/// demanda. [`FsCapabilitiesResult`] gana `attrs: Vec<`[`AttrInfo`](crate::attrs::AttrInfo)`>`
/// (discovery: qué publica ese provider, con tipo y pista de presentación);
/// [`FsListParams`] y [`FsStatParams`] ganan `attrs: Vec<String>` (el cliente
/// pide SOLO los ids que va a pintar); [`Entry`](crate::Entry) gana
/// `attrs: BTreeMap<String, `[`AttrValue`](crate::attrs::AttrValue)`>`. Los tres
/// llevan `skip_serializing_if` sobre el vacío, así que un peer 0.29 emite y
/// lee EXACTAMENTE los bytes de antes — aditivo en el sentido fuerte, no solo
/// en el de "campo ignorable". `AttrValue` deserializa a mano (como
/// [`CapabilityFlags`](crate::CapabilityFlags)) porque `#[serde(other)]` no
/// existe para una variante con datos: una variante de un protocolo MÁS NUEVO,
/// o un base64 corrupto, degradan a `AttrValue::Unknown` — UNA celda, jamás la
/// entrada ni la página. Topes del wire (server ENFORCE, cliente re-valida):
/// ≤16 ids por llamada, id ≤64 bytes con forma `[a-z0-9._-]`, label ≤64 bytes,
/// texto ≤256 bytes, bytes ≤256 tras decodificar. Pedir un id que el provider
/// no ofrece NO es error: vuelve ausente (un catálogo rancio degrada, no
/// rompe). Este bump es SOLO de wire: ningún provider anuncia atributos
/// todavía y el daemon ignora los ids pedidos — la ausencia ya es una
/// respuesta válida del contrato. La ventana pasa a N=0.30.x/N-1=0.29.x.
pub const PROTOCOL_VERSION: &str = "0.30.0";
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo nextest run -p norte-proto --test types version`
Expected: PASS.

- [ ] **Step 5: Check the daemon handshake tests still pass**

Run: `cargo nextest run -p norte-core initialize`
Expected: PASS. Daemon tests that pin an N-1 version string may hard-code
`"0.29.0"`; if one now fails as N-2, update it to the new window — the daemon
must keep accepting exactly N and N-1.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-proto/src/methods.rs crates/norte-proto/tests/types.rs crates/norte-core
git commit -m "feat(proto): 0.30.0 — provider attributes, window N=0.30.x/N-1=0.29.x"
```

---

### Task 7: JSON Schema artifact (ADR 0038 gate)

**Files:**
- Modify: `crates/norte-proto/tests/schema.rs` (the `ProtocolSchema` aggregate)
- Regenerate: `docs/schema/proto.schema.json`

- [ ] **Step 1: Run the gate to verify it fails**

Run: `cargo nextest run -p norte-proto --features schema --test schema`
Expected: FAIL on `todo_tipo_con_schema_esta_en_el_artefacto` — `AttrType`,
`AttrHint`, `AttrInfo` and `AttrValue` carry a schema impl but are unreachable
from `ProtocolSchema`. (`el_schema_del_protocolo_no_diverge` may also fail,
since `Entry` and the three method types changed shape.)

- [ ] **Step 2: Add the new types to the aggregate**

In `crates/norte-proto/tests/schema.rs`, add these fields to `ProtocolSchema`,
keeping the list alphabetical (they belong just before `byte_range`):

```rust
    attr_hint: AttrHint,
    attr_info: AttrInfo,
    attr_type: AttrType,
    attr_value: AttrValue,
```

The file already does `use norte_proto::*;`, and Task 3 re-exported all four
from the crate root, so no import change is needed.

- [ ] **Step 3: Regenerate the artifact**

Run:
```bash
NORTE_UPDATE_SCHEMA=1 cargo test -p norte-proto --features schema --test schema
```
Expected: passes and rewrites `docs/schema/proto.schema.json`.

- [ ] **Step 4: Inspect the diff before trusting it**

Run: `git diff --stat docs/schema/proto.schema.json && git diff docs/schema/proto.schema.json | head -80`
Expected: `$defs` gains `AttrHint`, `AttrInfo`, `AttrType`, `AttrValue`;
`Entry`, `FsListParams`, `FsStatParams` and `FsCapabilitiesResult` gain an
`attrs` property. Nothing else changes. **A change to any unrelated type means
something else drifted — stop and investigate rather than committing it.**

- [ ] **Step 5: Verify the gate is green**

Run: `cargo nextest run -p norte-proto --features schema --test schema`
Expected: PASS (both tests).

- [ ] **Step 6: Commit**

```bash
git add crates/norte-proto/tests/schema.rs docs/schema/proto.schema.json
git commit -m "docs(schema): publish provider attributes in proto.schema.json"
```

---

### Task 8: changelog, full gate, protocol review

**Files:**
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Add the changelog entry**

Under `## [Unreleased]` → `### Added` in `CHANGELOG.md`, following the style of
the entries already there (the ADR 0038 entry is the immediate neighbour):

```markdown
- **Protocol 0.30.0 — provider attributes (ADR 0039).** `fs.capabilities`
  advertises the typed attributes a provider offers, `fs.list`/`fs.stat`
  request the ids a client will paint, and `Entry` carries the values. Purely
  additive: a 0.29 peer sees byte-identical payloads. An attribute value from
  a newer protocol degrades to `Unknown` — one cell, never the listing.
```

- [ ] **Step 2: Run the full gate**

Run: `just ci`
Expected: `EXIT=0`. This change touches `norte-proto`, one of the crates under
the 85% coverage gate, so the full suite (not `ci-fast`) is the right call
here — see `CLAUDE.md` on CI pacing.

- [ ] **Step 3: Commit the changelog**

```bash
git add CHANGELOG.md
git commit -m "docs(changelog): protocol 0.30.0 provider attributes"
```

- [ ] **Step 4: Protocol review — mandatory**

Dispatch the `protocol-guardian` agent over the diff of this block
(`git diff main...HEAD -- crates/norte-proto docs/schema`). It is mandatory for
any `norte-proto` change (CLAUDE.md workspace map). Ask it specifically about:

1. Whether a 0.29 peer's payloads are truly byte-identical (the
   `skip_serializing_if` claim), and whether any existing golden fixture had to
   change — one that did would falsify the additivity claim.
2. Whether `AttrValue`'s hand-written deserialisation degrades where ADR 0004
   says it must and errors where a peer is genuinely broken.
3. Whether the caps in the rustdoc match the constants, given that nothing
   enforces them until block 2 wires the daemon.

- [ ] **Step 5: Apply the review findings**

Fix what the review lands, re-run `just ci`, and commit with
`fix(proto): guardian review — <what>`. If a finding argues the wire shape is
wrong, stop and revisit the ADR before writing code: a wire shape is cheap to
change now and expensive after block 2.

---

## What this block deliberately does NOT do

- No provider advertises or produces an attribute (block 2).
- The daemon does not forward, validate or cap requested ids yet (block 2). The
  caps live in `attrs.rs` as constants and in the rustdoc as the contract, so
  block 2 wires them in one place rather than inventing its own numbers.
- No frontend reads `Entry::attrs` (blocks 3–7).

Each of those is a listed follow-up in the design document, not an omission.
