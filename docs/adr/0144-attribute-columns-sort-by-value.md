# 0144 — Attribute columns sort by their value

- Status: accepted
- Date: 2026-09-22
- Decision makers: Oscar González
- Protocol: unchanged. Bridge: unchanged (91). Session schema: unchanged.
- Related: ADR 0077 (one decision, one place), ADR 0128 (the permissions
  column), `docs/batida-de-funcionalidades-2026-09-20.md` §2.3 and item 5

## Context and problem statement

The permissions column has been on by default since ADR 0128, and `posix.uid`
and `posix.gid` can be shown as columns, but none of them could be sorted by.
`SortColumn` was a closed set — name, size, mtime, extension — and clicking an
attribute header did nothing. Krusader sorts by any column it shows.

Opening the set raises four questions: how values of an open type compare,
where the ones that are missing go, what happens to plugin columns, and what
the configuration file does with a sort it cannot name.

## Decision

**`SortColumn` gains `Attr(String)`, carrying the attribute id, and
`sort_column_id` — the one function both frontends ask whether a column
sorts — returns it for every `attr:` column.**

1. **By value, not by text.** `AttrValue` is compared as what it is: `Uint`
   and `Int` as one number line, `TimeMs` as time, `Text` and `Bytes` by
   bytes, `Bool` false before true. So `0o100` sorts after `0o77`, which a
   text sort would get backwards.
2. **A total order over mixed types.** A provider may send different types
   under one id. Rather than fall over, values are first grouped by rank —
   bool, number, time, text, bytes — then compared within the group. Sorting
   needs a total order; a partial one reorders on every refresh.
3. **Missing goes last, in both directions.** An entry without the attribute,
   or with `Unknown`, sorts after every known value whether the order is
   ascending or descending, as a size the backend cannot tell already does.
   Reversing must not float the blanks to the top.
4. **Plugin columns still do not sort.** Their values live in the pane's side
   map, not in the `Entry`, and arrive after the listing; sorting by them
   would move rows under the cursor while the reader is reading.
5. **The attribute sort is a session sort.** `[ui.columns] sort` is a closed
   key (`name`/`size`/`mtime`/`extension`), and widening it would make an
   older norte reject a configuration file a newer one wrote.
   `persist_columns` now takes `Option<PersistSort>`, and `None` leaves the
   existing `sort` key untouched; the terminal tells the reader the column
   list was saved and the attribute order holds for this session.

Nothing on the wire changes. The bridge already names columns as strings
(`SortBy { column }`, `ColumnHeaderView.sort`), and the session file already
reads its sort tolerantly, so `Attr` is a new value, not a new schema.

`SortColumn` and `SortSpec` lose `Copy` — a `String` cannot be copied — and
the key-driven sort command in the window keeps `Copy` by carrying
`SortColumnKey` instead, since no key sorts by an attribute.

## Consequences

- Clicking a permissions, UID or GID header sorts, in both frontends, with the
  arrow in the header; the terminal's columns dialog does the same.
- Owner and group NAMES are not part of this: resolving a uid to a name is I/O
  in `norte-vfs-local` and possibly a new dependency. Sorting by `posix.uid`
  groups by owner already; the names come in their own change.
- The order of equal values is decided by name, ascending, as for every other
  column.

## Alternatives considered

- **Sort by the rendered cell text.** Cheap, and wrong for every number: it
  depends on the format the reader chose, and `1000` sorts before `999`.
- **Widen the configuration key to `attr:<id>`.** Persistence, but a file that
  an older norte refuses to load. Worth revisiting with a config-schema bump,
  not smuggled in here.
- **Intern ids to keep `Copy`.** A leak or a global table to save a few
  `.clone()`s on a type that is cloned once per sort.
