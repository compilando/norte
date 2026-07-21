# 0017 - Connection-scoped cursor pagination for `fs.list`

- Status: accepted
- Date: 2026-07-13
- Decision makers: Oscar González
- Related: specification sections 11 and 12; ADRs 0004 and 0011; issue #27

## Context

The daemon originally drained every provider listing into one response. A
100,000-entry directory delayed first render by 254 ms and a 500,000-key bucket
would create an enormous frame. Providers already return lazy streams, so the
problem is the wire and its consumers. N-1 clients that omit pagination must
still receive the complete result.

## Options considered

- Relist and skip an index for every page. This becomes O(n²) and loses or
  duplicates entries when a directory changes.
- Expose provider-native tokens. Most providers do not have them and the VFS
  contract would grow solely for S3.
- Retain a live `EntryStream` per connection and use an opaque cursor ID. This
  preserves each provider's natural laziness and cancellation.
- Add explicit cursor close. Bounded LRU, TTL, and connection teardown already
  cover the expected one or two active TUI listings.

## Decision

- Protocol 0.8.0 adds optional `limit` and `cursor` to `FsListParams`, and
  optional `next_cursor` to the result. No limit or cursor still drains the
  complete listing. Reject a zero limit and cap pages at 10,000 entries.
- Require the path on continuation and verify it matches the retained stream.
  A missing or expired cursor returns `CursorExpired`; clients restart. An
  empty final page with no next cursor is valid. An empty page with another
  cursor is a client-side internal error to prevent infinite loops.
- Drop the retained stream after a stream error.
- Store connection-local open listings under opaque decimal IDs. Limit each
  connection to eight with LRU eviction and use a configurable 120-second TTL.
  Expire streams even on an otherwise idle live connection. Dropping a stream
  is cooperative cancellation.
- Cap retained listings globally at 256, below the blocking producer pool. At
  the cap, drain a new request immediately as a complete result rather than
  retaining it; never exhaust the pool or truncate data.
- Keep `Provider::list` unchanged.
- Add `Backend::list_stream`. Embedded mode passes the provider stream through;
  remote mode fetches an eager 1,000-entry page and unfolds later pages.
  Existing `Backend::list` drains this stream so CLI callers keep their API.
- The TUI paints the first 100 entries, marks the listing as loading, and fills
  in batches of 4,096 entries or 100 ms. It re-sorts each batch and restores
  selection by path. Navigating elsewhere drops and cancels the old fill.
- Cursors do not survive reconnection.

## Consequences

First render no longer waits for a complete large directory, response frames
are bounded, S3 benefits without VFS changes, and 0.7 clients retain complete
results. The daemon now owns bounded stream state and paged results are not a
consistent snapshot of a mutating directory.

Repeated full re-sorts during fill are acceptable for first render but remain a
candidate for an incremental merge. Local providers still stat the full
directory in their producer; lazy metadata is separate work. `CursorExpired`
is a new frontend-visible category that triggers a relist. The new first-page
benchmark closes #27 while complete-drain performance remains a regression
metric.
