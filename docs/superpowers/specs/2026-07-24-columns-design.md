# Configurable columns — design

**Date:** 2026-07-24
**Status:** approved
**Related:** spec §6.1 (listing presentation), §5 (`Capabilities`), ADR 0004
(wire evolution: unknown → degrade, never break), ADR 0017 (cursor pagination),
ADR 0037 (plugin data-out v2: decorators and plugin columns), ADR 0038
(protocol JSON schema + semver gate, in flight). New ADR 0039 (provider
attributes on the wire) lands with sub-project 1.

## Scope

A complete column system for the listing panes: the user chooses **which**
columns are shown, in **which order**, and **how each one renders** (width,
alignment, value format, header label), and can **sort** by any of them.
Columns come from three sources that the model unifies:

1. **Built-ins** — name, size, mtime, kind. Derived from `Entry` as it exists
   today.
2. **Provider attributes** — protocol-specific metadata that does **not**
   exist on the wire yet: POSIX mode/uid/gid on local and sftp, storage class
   and etag on S3, compression method and packed size inside an archive.
   Sub-project 1 puts these on the wire.
3. **Plugin columns** — already shipped (ADR 0037): a `columns` plugin
   declares `{id, header}` in its manifest, the client discovers them through
   `PluginInfo::columns` and fetches cells with `plugin.column_values`.

Confirmed in brainstorming: attributes travel **on demand** inside `fs.list`;
attribute values are **typed** (not pre-formatted strings); per-column
settings are visibility+order, width, format+alignment, and header override;
selection is **global with per-scheme overrides**; **sorting by column is in
scope**; both TUI and GUI get rendering *and* an interactive picker.

### Starting point

- `norte_proto::Entry` is `{path, kind, size, mtime_ms}`. Nothing else travels.
- The TUI pane paints **only** the name plus the hostile badge and the
  decorator badge (`ui.rs::entry_item`). Neither frontend shows size or mtime
  today, and no size/time formatter exists anywhere in the workspace.
- The GUI already paints plugin column cells (`main.rs`, G3c).
- `norte_frontend::sort` is fixed: directories first, then NFC name with a
  raw-byte tie-break. There is no sort selection.

### Out of scope

- Column widths dragged with the mouse in the GUI.
- Derived/computed columns (e.g. an archive compression *ratio* synthesised
  from `size` and `archive.packed_size`). Providers publish facts; the UI does
  not invent new ones.
- Server-side sorting. Providers list in their own order; sorting is a
  presentation concern and stays client-side.
- Per-directory (as opposed to per-scheme) column memory.

## Architecture

### Layer 1 — the model, `norte-frontend::columns`

The existing `columns.rs` (plugin cell sanitisation) grows into a module
directory. All decisions live here; frontends only paint (hard rule 7).

```rust
pub enum ColumnId {
    Builtin(Builtin),                        // Name | Size | Mtime | Kind
    Attr(AttrId),                            // provider attribute
    Plugin { plugin: String, column: String },
}
```

Config-stable string form, parsed and rendered by `FromStr`/`Display`:
`"name"`, `"size"`, `"mtime"`, `"kind"`, `"attr:posix.mode"`,
`"plugin:git-status/branch"`. An unparsable id is a config diagnostic, never a
panic and never a silent drop (see *Diagnostics*).

```rust
pub struct ColumnSpec {
    pub id: ColumnId,
    pub header: Option<String>,   // user override; None = catalog label
    pub width: WidthPolicy,       // Fixed(u16) | Auto | Flex { min: u16, weight: u16 }
    pub align: Align,             // Left | Right
    pub format: Format,           // see below
    pub truncate: Truncate,       // End | Middle
}

pub struct ColumnSet { specs: Vec<ColumnSpec> }   // order = paint order
```

**`ColumnCatalog`** — what is *available* for one pane right now: the
built-ins (always), the provider attributes advertised by `fs.capabilities`
for that pane's path, and the columns of approved+enabled `columns` plugins
from `plugin.list`. The picker enumerates it; cell resolution consults it. A
`ColumnSet` entry with no catalog match is not painted.

**`layout(available: u16, specs: &[ResolvedSpec], measured: &Measured) -> Vec<Placed>`**
— a pure function, shared by both frontends, working in **terminal cells**
(the GUI paints listings in a monospace font already, so one cell = one
advance width):

1. Fixed columns take their width. `Auto` columns take the widest measured
   cell on the current page, clamped to a per-column ceiling.
2. Remaining space is distributed among `Flex` columns by weight, never below
   `min`.
3. If the total does not fit, columns are dropped from the **right-most,
   lowest-weight** end until it does. The name column is never dropped and
   never shrinks below a floor; it is `Flex` by default.
4. Every returned width is ≥ 0 and the sum is ≤ `available`. Property-tested.

**`format`** — value → display text:

| Format | Applies to | Variants |
|---|---|---|
| `Size` | `size`, `Uint` attrs hinted `Size` | `exact` (raw digits, no separators — locale-free and snapshot-stable) · `iec` (KiB/MiB) · `si` (kB/MB) |
| `Time` | `mtime`, `TimeMs` attrs | `relative` ("2h ago", Fluent) · `iso` (RFC 3339 UTC) · `local` (locale short) |
| `Mode` | `Uint` attrs hinted `Mode` | `octal` (`0644`) · `rwx` (`-rw-r--r--`) |
| `Text` | `Text`/`Bytes` attrs, plugin cells, kind | masked, capped |

`relative` and `local` render through Fluent (hard rule: no hard-coded
user-facing strings). Time formatting must survive negative `mtime_ms`
(pre-1970 timestamps exist on real filesystems, as `Entry` already documents).

### Layer 2 — the wire (proto 0.30.0, ADR 0039)

Additive throughout. A 0.29 client sends no `attrs`, receives no `attrs`, and
sees exactly today's payloads.

```rust
// Discovery — FsCapabilitiesResult gains:
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub attrs: Vec<AttrInfo>,

pub struct AttrInfo {
    pub id: String,        // validated [a-z0-9_.-]{1,64}, namespaced by provider
    pub label: String,     // human label — THIRD-PARTY text for a WASM provider
    pub ty: AttrType,
    pub hint: AttrHint,    // suggests default format/alignment
}

pub enum AttrType { Uint, Int, Text, Bytes, TimeMs, Bool, #[serde(other)] Unknown }
pub enum AttrHint { Size, Timestamp, Mode, Identity, Opaque, #[serde(other)] Unknown }

// Request — FsListParams and FsStatParams gain:
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub attrs: Vec<String>,

// Data — Entry gains:
#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
pub attrs: BTreeMap<String, AttrValue>,

pub enum AttrValue { Uint(u64), Int(i64), Text(String), Bytes(Vec<u8>), TimeMs(i64), Bool(bool), Unknown }
```

Hard points:

- **`AttrValue` implements `Deserialize` by hand** (precedent:
  `CapabilityFlags`). Serde cannot express `#[serde(other)]` on a
  data-carrying variant, and a protocol-N+1 variant must degrade to `Unknown`
  instead of failing the whole `Entry`. `Bytes` is base64 on the wire (a
  non-UTF-8 owner name is bytes, hard rule 1).
- **No float variant**, so `Entry` keeps its derived `Eq`/`Hash`. `BTreeMap`
  (not `HashMap`) keeps golden tests deterministic.
- **Absence means absence.** A provider that does not know an attribute omits
  the key. Never a fabricated `0`, same rule `size: None` already follows.
- **Caps, enforced server-side:** ≤ 16 requested attribute ids per call, ids
  rejected (`-32602`) if malformed, `Text` ≤ 256 bytes, `Bytes` ≤ 256 bytes
  after decoding. Requesting an unknown id is *not* an error — it comes back
  absent, so a client with a stale catalog degrades instead of failing.
- The daemon forwards only ids the target provider advertises; it never
  invents cells.
- ADR 0038's schema gate applies: `docs/schema/proto.schema.json` is
  regenerated in the same change, and the `methods.json` goldens grow cases
  for every `AttrValue` variant plus the `Unknown` degradation.

### Layer 3 — the VFS

`Provider::list(&self, p) -> EntryStream` carries no options today
(pagination lives in the core). Rather than break every provider and the WASM
provider bridge at once, the trait gains **defaulted** members:

```rust
fn attrs(&self) -> &[AttrInfo] { &[] }

async fn list_with(&self, p: &VPath, opt: &ListOptions) -> Result<EntryStream, Error> {
    let _ = opt;            // default: ignore the request, yield bare entries
    self.list(p).await
}

async fn stat_with(&self, p: &VPath, opt: &ListOptions) -> Result<Entry, Error> { … }
```

`ListOptions { attrs: AttrRequest }` is a struct so future listing options do
not churn the signature again. A provider that advertises attributes **must**
override both; the conformance suite enforces it.

| Provider | Attributes |
|---|---|
| `local` | `posix.mode`, `posix.uid`, `posix.gid`, `posix.nlink`, `posix.ctime_ms`; `win.attributes` on Windows |
| `sftp` | `posix.mode`, `posix.uid`, `posix.gid`; `sftp.owner`, `sftp.group` as `Bytes` |
| `object` | `s3.storage_class`, `s3.etag`, `s3.content_type` |
| `archive` | `archive.method`, `archive.packed_size`, `archive.crc32` |

The data is already in the response the provider parses (statx, SFTP attrs,
`ListObjectsV2`, the ZIP/TAR header), so the cost is materialising it, not
fetching it. This composes with #52: with no attributes requested, `vfs-local`
keeps its lazy `d_type` fast path untouched; requesting a POSIX attribute is
what promotes the entry to a full `statx`.

`MemProvider` (testkit) advertises synthetic attributes, including deliberately
hostile ones (non-UTF-8 owner, RTL text, zero-width joiner) for render tests.

### Layer 4 — configuration

```toml
[ui.columns]
default = ["name", "size", "mtime"]
sort = { column = "name", dir = "asc", dirs_first = true }

[ui.columns.scheme.sftp]
columns = ["name", "attr:posix.mode", "attr:sftp.owner", "size", "mtime"]
sort = { column = "mtime", dir = "desc", dirs_first = true }

[[ui.columns.spec]]
id = "size"
width = { min = 9, weight = 0 }     # or { fixed = 9 } or "auto"
align = "right"
format = "iec"
header = "Size"
```

- A scheme override **replaces** the column list; it does not merge. Merging
  makes "why is this column here?" unanswerable.
- `[[ui.columns.spec]]` entries are keyed by column id and apply wherever that
  column appears; a scheme block may carry its own `spec` entries that win.
- Defaults come from the catalog: a `Size`-hinted attribute defaults to
  right-aligned `iec`, a `Timestamp` to `relative`, a `Mode` to `rwx`.
- `norte.schema.json` is regenerated (existing gate).

### Layer 5 — rendering

**TUI.** A dim header line above the list inside the pane block, carrying the
sort indicator (`▲`/`▼`) on the active column. Rows are built from the shared
`layout()` result and truncated with the existing width-aware helpers
(`middle_ellipsis` already budgets by cell width, so CJK and emoji do not
overflow). The hostile badge and the decorator badge keep their current
positions **inside** the name cell, so a pane with only `["name"]` configured
renders byte-identically to today.

**GUI.** Same `layout()`, same widths in monospace cells. The header is
clickable (toggles sort) and offers a context menu (toggle column, cycle
format).

### Layer 6 — the picker

TUI `Modal::Columns`, driven entirely by `ColumnCatalog`/`ColumnSet`: space
toggles, `J`/`K` reorder, `w` cycles width policy, `f` cycles format, `Enter`
applies and persists through the existing `persist_set`, `Esc` discards. It
takes a new keymap action `pane.columns`, registered with the one-pass
diagnostics and bound to `alt+c` in all three presets — `f2` and `f7` stay
free for the orthodox user menu and mkdir, and `ctrl+shift+…` is not a chord
this keymap parses. The GUI panel exposes the same operations over the same
model. Sort selection lives inside the picker and on the GUI header; it gets
no separate binding.

### Layer 7 — sorting

`SortSpec { column: ColumnId, dir: Dir, dirs_first: bool }`, persisted
globally and per scheme. `norte_frontend::sort` keeps its current ordering as
the `name`/`asc`/`dirs_first` case, so nothing changes for the default.

Rules, all of them load-bearing:

- **Only drained entries are sorted.** A listing still filling keeps its
  `[n]` marker; the order shown is never presented as final. `extend()`
  inserts each incoming batch at its sorted position, which the incremental
  merge from #54 already supports.
- **A missing cell sorts last in both directions**, with the existing name key
  as tie-break. This keeps the order total and deterministic, and stops a
  descending sort from filling the top of the pane with blanks.
- Sorting by a **plugin** column only covers the pages whose cells have been
  fetched; that state is marked partial, not hidden.
- A column used for sorting is **requested over the wire even when hidden**.

## Security and encoding

- Every `Text`/`Bytes` attribute value, every provider `AttrInfo::label`, and
  every plugin header/cell is third-party text. All of them pass through
  `norte_frontend::display_name` masking plus the per-cell character cap
  before reaching a frontend — a hostile SFTP server controls `sftp.owner`
  exactly as a hostile plugin controls its cell text.
- `Bytes` values render through the same lossy-with-badge path as a non-UTF-8
  filename; the raw bytes are preserved, only the rendering is lossy.
- The wire caps above bound a malicious daemon or provider: 16 attributes ×
  256 bytes per entry is the ceiling a listing page can add.
- Attribute ids are validated on both sides; an id is never interpolated into
  a path, a query, or a log line unmasked.

## Diagnostics

A configured column that does not parse, or that no source in the current
pane provides, is skipped at paint time **and reported by `norte doctor`**,
following the keymap-diagnostics precedent (one pass, all problems, actionable
text). Silent disappearance of a column the user configured is a bug, not a
degradation.

## Testing

- **proto:** goldens for every `AttrValue` variant; a test that an unknown
  variant deserialises to `Unknown` while the rest of the `Entry` survives;
  regenerated `proto.schema.json`; N-1 round-trip (0.29 payload → 0.30 type →
  identical bytes back).
- **VFS:** the provider conformance suite gains an attributes contract — every
  advertised id has a declared type, values match that type, and no id appears
  that was not requested.
- **frontend:** proptests for `layout()` (sum ≤ available, never panics, wide
  characters never overflow, name never dropped); proptests for the sort (total
  order, stable, missing-last in both directions); formatter unit tests at IEC
  and SI boundaries, on pre-1970 times, and on `u64::MAX`.
- **encoding:** new hostile fixtures in the canonical testkit corpus (non-UTF-8
  owner, RTL override in a cell, zero-width smuggling in a header) plus TUI
  snapshot tests of a pane rendering them.
- **e2e:** local directory with POSIX columns; an archive showing packed size;
  an S3 bucket showing storage class; a configured column that the provider
  does not offer, verified absent from the pane and present in `doctor`.

## Decomposition

Seven plans, one PR each, each under the 400-net-line convention.

1. **Wire** — ADR 0039, proto 0.30.0, `AttrInfo`/`AttrType`/`AttrHint`/
   `AttrValue`, the three additive fields, goldens, schema. Protocol-guardian
   review is mandatory.
2. **VFS** — `attrs()`, `list_with`/`stat_with`, `ListOptions`, the four
   providers, `MemProvider`, conformance contract, core plumbing of the
   requested ids through the daemon.
3. **Model** — `norte-frontend::columns` (catalog, spec, layout, formatters)
   and the sort rewrite behind the existing default.
4. **Config** — `[ui.columns]`, per-scheme overrides, schema regeneration,
   doctor diagnostics.
5. **TUI render** — header line, cells, sort indicator.
6. **GUI render** — header, cells, clickable sort.
7. **Picker** — TUI modal and GUI panel over the shared model.

Blocks 1–3 are the foundation; 4–7 each deliver visible value on their own.

## Risks

- **Provider trait growth.** Defaulted methods keep every existing provider and
  the WASM provider bridge compiling, at the cost of two listing paths. The
  conformance suite closes the gap by forcing any provider that advertises
  attributes to implement the option-carrying path.
- **Perf.** Attributes are opt-in per call, so the fast paths from #52/#54 stay
  intact when no attribute column is configured. The TUI's first-render budget
  is re-measured in block 5; a regression there blocks the block.
- **First metadata in the pane.** Block 5 is the first time the TUI paints
  anything but the name, so it is also where column layout meets hostile names
  in anger. It ships with snapshot tests over the corpus, not after.
