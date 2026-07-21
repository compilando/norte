# 0008 - The norte-encoding boundary

- Status: accepted
- Date: 2026-07-11
- Decision makers: Oscar González
- Related: specification section 6, M1 phase 7, ADR 0003

## Context

The viewer needs encoding detection and decoding. `chardetng` and `encoding_rs`
are structural dependencies with large tables and broad APIs. The project must
choose where this presentation logic lives and prevent each frontend from
building a different integration.

## Options considered

- Use both crates directly in `norte-tui`. This avoids a crate but duplicates
  work in the GUI and third-party clients.
- Isolate them behind a small `norte-encoding` API covering detection, decoding,
  forced decoding, line-ending detection, and encoding cycling.
- Put detection in the core. Decoding is presentation logic: it does not mutate
  data or decide policy, and the protocol already carries source bytes unchanged.

Alternative detector ports were less accurate; UTF-8-only support would violate
the specification.

## Decision

Create `norte-encoding` under `MIT OR Apache-2.0`. Decoding remains on the client
side of the protocol; reads always pass through the core using the ranged-read
API from ADR 0005.

- A BOM has highest confidence. Otherwise, a NUL anywhere in the inspected
  buffer marks it as binary; BOM-less UTF-16 can still be selected manually.
  Finally, run `chardetng` over at most 64 KiB with UTF-8 allowed and ISO-2022-JP
  detection disabled.
- Streaming `decode(..., complete)` keeps a truncated trailing sequence pending
  until more input arrives rather than reporting a lossy conversion.
- `decode_forced` does not sniff BOMs. A user's explicit encoding choice takes
  precedence, and a stray BOM is treated as data.
- The library exposes stable technical display names; frontends localize them.

## Consequences

- GUI and third-party clients reuse one permissively licensed implementation.
- `chardetng` and `encoding_rs` remain confined to one auditable boundary.
- The testkit content corpus defines nine detectable and three forced-only
  cases.
- Both large dependencies are compiled only by consumers that need decoding.
- `encoding_rs` shares its GB18030 and GBK decoder; the public label follows the
  specification and uses GB18030.
- The viewer budgets 256 KiB for its initial buffer. If that grows, decoding and
  line indexing must leave the UI loop thread.
