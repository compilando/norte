# 0163 — A provider plugin can say a file is busy: `norte:provider@0.2.0`

- Status: accepted
- Date: 2026-10-01
- Decision makers: Oscar González
- Related: ADR 0032 (the provider interface), ADR 0041 (decision 3: its
  gaps are filled as a real plugin asks; decision 4: its own package),
  ADR 0093 (the FTP backend is a guest), #221 (`Error::Busy`, protocol
  0.85.0)

## Context

Protocol 0.85.0 added `Error::Busy`: a file another program holds. The
engine retries it (100/200/400 ms) and, if it lasts, tells the reader to
close that program instead of saying "I/O error". The local provider
emits it; a provider plugin cannot, because the WIT `vfs-error` it answers
with is a closed `enum` without that value.

The case is not hypothetical. The FTP backend, which is a guest (ADR 0093),
receives `450 Requested file action not taken` — "file unavailable (e.g.,
file busy)" in RFC 959 — from a server whose file is being written by
someone else. Today it maps 450 to `io`: not retried, and reported as a
generic I/O error.

## Options

1. **Add `busy` to `vfs-error` and bump `norte:provider` to 0.2.0.**
   - Good: the guest says what happened, and the host maps it to the
     category the engine already retries. One value, one arm.
   - Bad: a WIT `enum` is closed and its package version travels in the
     interface's name, so every provider guest compiled against 0.1.0
     stops instantiating. There is no published third-party provider; the
     only compiled one is the embedded FTP backend, rebuilt in the same
     change.
2. **Link both 0.1.0 and 0.2.0 in the host.**
   - Good: old guests keep running.
   - Bad: two generated binding sets and two adapters for an interface
     that ADR 0041 expects to bump at every gap it fills, with nobody on
     the old side to keep.
3. **Keep the enum and let the host guess** (retry every `io` from a
   plugin).
   - Bad: a permanent failure would be retried and still be called I/O.
     The guest knows, and the host would be inventing.

## Decision

Option 1. `vfs-error` gains `busy`; `norte:provider` goes to 0.2.0; the
host maps `busy` to `Error::Busy`; the FTP guest maps 450 to `busy`. This is
the path ADR 0041 decision 3 set: each gap of `provider` is a bump of its
own package. That package was split precisely so that this bump does not
touch `norte:plugin`'s previewers, columns or commands.

## Consequences

- A provider guest built against `norte:provider@0.1.0` no longer
  instantiates; the catalog lists it as broken, naming both versions
  (ADR 0094, `SERVED_WIT`). Its author recompiles against the new WIT. Option 2 remains
  open for when there are published providers to keep.
- An FTP file another client is writing is retried, then reported as busy,
  not as an I/O error.
- `busy` promises the operation was NOT applied: the engine retries it
  without counting it as a possible effect (`sows_doubt` in `ops.rs`), so
  a later `NotFound` is not journalled as ours. FTP 450 says exactly that
  ("action not taken"); a guest must not use `busy` for a reply that may
  arrive after the effect — that is `provider-unavailable`. The FTP guest
  therefore keeps a 450 read after an APPE's data as `io`: the chunk may
  already be on the server, and a retry would append it twice.
- `busy` is the second value a guest can use for a transient condition,
  after `provider-unavailable`. They differ in scope: a file, against the
  whole backend.
