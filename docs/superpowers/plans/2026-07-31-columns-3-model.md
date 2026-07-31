# Columns block 3 — the shared model (sort + columns core)

> Spec: `docs/superpowers/specs/2026-07-24-columns-design.md` (approved).
> Issue: #108. Block 1 (wire, proto 0.30) is merged; block 2 (VFS attrs) is
> deliberately deferred — everything here works on built-ins (`Entry.size`,
> `Entry.mtime_ms`), which is what the beta criterion needs (choose by
> size/date, sort by date).

**Order note (deviation from the spec's 2→3 ordering, recorded):** block 3
before block 2. The model and the sort only need built-ins; the TUI render
(block 5) over the default column set delivers the beta value without any
provider work. Block 2 feeds `attr:*` columns later and slots in unchanged.

## Task 1 — sort behind a `SortSpec` (frontend, no behavior change by default)

`norte_frontend::sort` gains:

```rust
pub struct SortSpec { pub column: SortColumn, pub dir: SortDir, pub dirs_first: bool }
pub enum SortColumn { Name, Size, Mtime }        // attr/plugin columns arrive with block 2/7
pub enum SortDir { Asc, Desc }
impl Default for SortSpec { /* Name asc, dirs_first — today's order */ }
```

`cmp_keyed`/`sort_with_keys`/`merge_keyed`/`sort_entries` take `&SortSpec`.
`SortKey` is unchanged (the NFC name key stays the tie-break and the Name
key). Comparison order, all rules from the spec's Layer 7:

1. `dirs_first` group (always ascending).
2. Column value; `None` (a dir's size, a missing mtime) sorts LAST in BOTH
   directions.
3. `dir` inverts ONLY the column comparison — never the group, never the
   tie-break.
4. Tie-break: the existing name order, always ascending → total, stable,
   deterministic.

`PaneState` holds `sort: SortSpec`, passes it at every ingestion point, and
gains `set_sort(SortSpec)` (re-sort in place, cursor re-anchored by path,
quick search re-applied — the `extend` reconciliation pattern) plus `sort()`.

Tests: default spec ≡ today's order over the hostile corpus (pin);
size/mtime asc+desc with missing-last pinned both directions; proptest total
order + merge≡sort under a random spec; #54 bench path re-checked (Name path
must not regress — key material identical).

## Task 2 — `norte_frontend::columns` model core

`columns.rs` grows (module dir if needed):

- `ColumnId { Builtin(Builtin), Attr(String), Plugin { plugin, column } }` +
  `FromStr`/`Display` (`"size"`, `"attr:posix.mode"`,
  `"plugin:git-status/branch"`); parse error = diagnostic value, never panic.
- `ColumnSpec { id, header, width: WidthPolicy, align, format, truncate }`,
  `ColumnSet` (order = paint order), catalog-independent defaults per spec
  (Size → right/iec, Timestamp → relative, Mode → rwx).
- `layout(available, specs, measured) -> Vec<Placed>` — pure, cells,
  property-tested (sum ≤ available; name never dropped; floor respected).
- Formatters: `format_size` (exact/iec/si — reuse `human_bytes` for iec),
  `format_time` (relative/iso/local via Fluent; negative `mtime_ms` safe),
  text via existing masking. Unit tests at boundaries (`u64::MAX`, pre-1970).

Catalog/picker/config surfaces stay in blocks 4/6/7; this block only ships
the types + layout + formatters the renderers consume.

## Gates

Per task: `cargo nextest run -p norte-frontend` + clippy + the #54 bench
sanity. Reviewers: rust over the diff; encoding-auditor over formatters and
layout (hostile headers/cells are third-party text).
