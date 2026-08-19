# 0063 - A result that travels as progress, and a connection closed by path

- Status: accepted
- Date: 2026-08-19 (recorded retroactively for protocol 0.49.0, shipped
  2026-08-18)
- Decision makers: Oscar González
- Related: protocol 0.49.0, issues #139, #140, #247 (which found this record
  missing), ADR 0011 (the error taxonomy travels in `data`).

## Context and problem statement

Protocol 0.49.0 added two methods in one bump, and each made a design decision
worth recording. Neither got an ADR at the time — the branch was written under
a "no reviewers" session — and a `protocol-guardian` pass over 0.47→0.49 named
the omission. This ADR records the two decisions after the fact, with what was
actually built.

## Decision 1: `fs.dir_size` delivers its answer through PROGRESS

Counting a folder is a walk, so it is a task. The question was where the total
comes back.

**Options considered:** a result type of its own (`FsDirSizeResult { bytes,
entries }`, fetched when the task completes, the shape `fs.rename_batch_plan`
uses), or the task's existing progress frames.

**Chosen: the progress.** `bytes_done` sums the sizes and `entries_done`
counts the entries, so the last snapshot IS the result. `bytes_total` and
`entries_total` stay `None` throughout: they would be a bar advancing towards
an invented number, since knowing the total is the whole job.

**Why:** zero new result types and zero new notifications. A client that
already paints a copy's bar paints this one, and a partial count is visible
while it runs — which for a three-hour tree is most of the value. A result
type would have delivered the number once, at the end, and required a second
round trip to fetch it.

**What it costs, and this is the part worth writing down:** progress has no
field for "this number is a floor". An unreadable subtree is counted as
skipped, logged, and otherwise invisible, so a tree half of which was `EACCES`
reports `Completed` with a confident wrong total. Adding that field is a wire
change; it is tracked separately rather than pretended away.

## Decision 2: `connection.close` closes by PATH

**Options considered:** close by the connection's key (scheme + authority +
whatever the pool uses to identify a live session), or by a `VPath` the
frontend already has.

**Chosen: by path.** The caller passes a location; the core resolves it to the
connection that serves it and releases that one.

**Why:** the frontend has a panel pointing somewhere. It does not have — and
should not need — the pool's idea of a connection key, which is an
implementation detail of `norte-core` (it has changed once already). Closing
by path also means the gate is the one that already exists: the read gate on
that path. A key-based API would have needed its own authorisation story for
an identifier the policy engine cannot reason about.

**What it costs:** the result has to say whether anything was closed, because
"there was no connection" is a legitimate outcome the caller must be able to
tell from success — a local panel closes nothing, and answering "done" would
be a lie.

## Consequences

### Positive

- Both decisions kept the wire additive: no existing message changed shape,
  and both degrade honestly against an older daemon (`METHOD_NOT_FOUND` →
  `Unsupported`).
- `fs.dir_size` needed no client work beyond calling it: the progress path was
  already there.

### Negative

- The "lower bound" gap in `fs.dir_size` is real and unfixable without a wire
  change. It is named here so that the next person to add a field to
  `TaskProgress` knows there is a customer waiting for it.
- Recording a decision six weeks late means the alternatives above are
  reconstructed from the code and the issue, not from notes taken while
  choosing. That is worth less than an ADR written at the time, which is the
  argument for writing them at the time.
