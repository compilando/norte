# 0062 - The session's schema version has one home, and the core refuses what it cannot read

- Status: accepted
- Date: 2026-08-19
- Decision makers: Oscar González
- Related: ADR 0059 (the session is a document with one writer), protocol
  0.51.0, issue #247, hard rule 4 (nothing is lost silently).

## Context and problem statement

`session.*` (protocol 0.48.0) carries a document the core stores and does not
read. Two numbers describe it, and a `protocol-guardian` pass over 0.47→0.49
found that neither was doing the job the other was documented to do:

1. **`Session.version` / `SessionPutParams.version`** — the field the protocol
   documents: "schema of `body`, owned by the frontends". The core reads it in
   exactly one place, the disk load guard.
2. **an undocumented `version` key inside `body`**, injected by
   `norte_frontend::session::SessionBody::to_value` and read by its
   `from_value`. This is the one the client actually checked.

Two consequences, both silent:

- **A third-party client that follows the contract loses data.** Set
  `version: 2` in the envelope, put a v2 body. An older norte reads
  `body["version"]`, finds nothing, takes it for 0, passes its own
  "from the future" check, drops every field it does not know, and writes the
  remains back at `version: 1`. ADR 0059 promises the opposite; it held only by
  the accident of a duplicated field that norte's own frontends both wrote and
  read.
- **An N/N-1 pairing poisons the file permanently.** `SessionStore::put`
  assigned `version` with no check, so a newer `ntc` (frontend
  `SCHEMA_VERSION = 2`) against an older running `norte` (core
  `SCHEMA_VERSION = 1` — the two constants live in different crates and only a
  test pins them together) made the old core write a v2 file. From then on
  every start of that daemon read its own file as "from the future": no owner,
  no persistence, forever, until somebody deleted it by hand. The core wrote a
  document it had already decided it would refuse to read.

## Options

### A. Document the inner key and keep both

Write down that the body carries its own `version` and that the envelope's is
advisory.

- **Good**: nothing changes; no bump.
- **Bad**: it blesses a duplicated source of truth in a field the core is
  documented not to read. A client that implements the written contract stays
  wrong, and the failure is silent data loss.

### B. The envelope is the schema version; the core refuses what it cannot read

The frontends stop injecting the copy. The reader takes the version from the
envelope, and the store rejects a `put` whose version it cannot read.

- **Good**: one home for the number, and it is the documented one.
- **Good**: the poisoning becomes impossible at its source — the core will not
  write what it cannot read back.
- **Bad**: a wire behaviour change (a `put` that used to be accepted is now
  refused), so it costs a MINOR bump even though no type changed shape.
- **Bad**: bodies already on disk carry the inner copy, so the reader cannot
  simply stop looking at it.

## Decision

**B, with the reader taking the greater of the two.**

1. **`SessionBody::to_value` no longer injects `version`.** The schema of the
   body is declared by `SessionPutParams::version`, which is what the protocol
   documents and what the core stores.
2. **`SessionBody::from_value(envelope, body)` takes the envelope's version**
   and uses `max(envelope, inner)`. The inner copy is still read because bodies
   written before this ADR carry it; taking the MAXIMUM is the safe direction —
   either number claiming "newer than you" is enough to refuse, and refusing is
   what keeps a document from being read half-understood and written back
   short.
3. **`SessionStore::put` refuses a version it cannot read**, with
   `Error::Unsupported` — "your daemon is older", which is what that error
   already means everywhere else on the wire. `0` counts as unknown: the stored
   session is interpreted by that number, and a zero next to a real body is a
   file nobody can classify.

Protocol **0.51.0**. The bump buys no new type and no new field; what changed
is what `session.put` accepts, and that is wire behaviour.

## Consequences

### Positive

- A client that implements the documented contract is safe: its v2 body is
  refused by a v1 core rather than silently truncated.
- The N/N-1 poisoning is closed at the only place that could close it — the
  writer. A newer frontend against an older core now fails loudly at each
  `put` instead of killing persistence from the next start onwards.
- The two `SCHEMA_VERSION` constants can still drift, but drifting now costs a
  refused write instead of a dead session file.

### Negative

- A frontend that legitimately runs ahead of its core (an `ntc` upgraded before
  the daemon) stops saving the screen entirely instead of saving it into a file
  the core will later reject. That is the intended trade — a refusal that says
  so beats a loss that does not — but it IS a functional regression for that
  pairing.
- `from_value` grew a parameter, and every caller had to say which version it
  was reading. That is the point: there is no longer a way to read a body
  without stating what schema you believe it is.

### Neutral

- The window is N=0.51.x / N-1=0.50.x. Against a 0.50 daemon a 0.51 client
  loses the protection, not the feature: the old core keeps accepting what it
  always accepted.
