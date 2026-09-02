# The log panel reads the daemon too

> Status: accepted, not built. Closes #328.
>
> The window got its log panel in #326, and it shows the wrong process's
> lines. `norte-gui` starts its own daemon (#300), so the ring it paints holds
> the window's own lines — the bridge, the renderer, the startup — while the
> providers, the journal, the policy and the reason a connection failed are all
> in the daemon, on the other side of a socket. #326 shipped a chip saying
> **whose** log it is, which stops the panel from looking broken. This document
> is the other half: carrying the daemon's lines across.

## The shape of the hole

| process | has a ring? | what its lines are about |
| --- | --- | --- |
| `ntc` (embedded core) | yes (`norte-tui/src/main.rs:117`) | everything — core and frontend are one process |
| `norte-gui` | yes (`startup.rs:809`) | the window: bridge, renderer, startup |
| `norte daemon` | **no** — `logging::init`, `norte-cli/src/main.rs:640` | providers, journal, policy, connections |
| `ntc --socket <remote>` | its own only | same hole as the window |

Two things follow. The daemon has no ring at all, so there is nothing to read
before there is something to serve. And the hole is not the window's: any
frontend talking to a separate daemon has it, which is why the fix lands in
`norte-frontend` and not in the renderer (ADR 0077 — a decision one frontend
takes and the other does not diverges in silence).

## What crosses, and how

**Pull with a cursor, not push.** `log.tail { cursor, max }` answers with the
lines after `cursor`, the next cursor, and how many that client missed. The
frontend polls (~300 ms) only while the panel is open and only when the
selected source includes the daemon.

Push was the alternative and it loses on every axis that matters here. The ring
already owns a monotonic counter, `LogRing::pushed()`, so the cursor costs
nothing to produce and the daemon keeps **no per-client state** — no
subscription to register, no unsubscribe to miss when a client dies. A dropped
notification is a silent hole; a stale cursor is arithmetic, and the answer
says exactly how many lines fell off the back. And a closed panel costs zero,
where a subscription keeps paying.

The cost is latency: up to one poll interval. For a panel a human reads, that
is not a cost.

```
log.tail  { cursor: u64|null, max: u32 }
       →  { lines: [LogLine], next: u64, lost: u64, level, capacity: u32 }
log.level { level }  →  { level }
```

`cursor: null` means "whatever you have" — what the panel sends when it opens.
`lost` is what **this** cursor missed (`base.saturating_sub(cursor)`, where
`base = pushed - len`), and it is deliberately not `LogRing::dropped()`, which
counts everything the ring ever evicted. The panel already paints a gap marker
for its local ring; `lost` is what feeds the same marker for the remote one. A
log with a silent hole lies about what happened, because a missing line is
indistinguishable from an event that never occurred.

`level` rides in the tail result so the panel can show the daemon's current
level without a second round trip, and `capacity` so it can say how deep the
history goes.

## The cap does not travel

`norte-config::logring` documents why the ring has a floor: `suppaftp` logs
`PASS <password>` at the `log` crate's TRACE level (#43, rule 10), and this
ring's level is **raised from the interface**, so without the cap one keypress
in a panel would put an FTP password on screen. `bajo_cota` is a whitelist —
only `norte`/`ntc` targets go above INFO; everything third-party stays at INFO
whatever anyone asks for.

That cap survives the socket by construction, and the construction is the
design decision: **`log.level` is a method, not a parameter the client
applies.** The client asks for a level and the daemon calls
`LogRing::raise_to` itself, then answers with the level that actually took. No
new enforcement code, no second copy of the whitelist, and nothing to keep in
sync — the only way to raise the daemon's ring is to go through the daemon's
own setter.

Two consequences to state on screen rather than hide: the level is **global to
the daemon**, so one client raising it raises it for every client, and
`raise_to` never lowers — dropping back to errors and climbing again would show
a hole the size of the time spent down there.

## Agents may not read it

`log.tail` and `log.level` refuse an `Actor::Agent` in the dispatch, with the
same `INVALID_REQUEST` shape `policy.request_scope` uses for the mirror-image
case.

The reason is concrete, not a principle. The daemon's ring carries paths,
connection names and the activity of **other sessions**, so for a scoped agent
it is an oracle of existence for paths outside its sandbox — precisely the leak
`read_gate_all` already documents for `plugin.decorate` and
`plugin.column_values`. Raising the level would be worse: an agent could turn
up the verbosity of work it is not party to.

The catalogue does not record access (it says so in its own header: a second
source of truth about who may call what lies as soon as the first one
changes), so this lives where every other access decision lives — the daemon —
and a test pins it.

## Where the types live

`norte-config` depends on `norte-proto`, so the wire type goes in the protocol
crate and the presentation type stays where it is. They are not the same type
and should not be:

- **`norte-proto`** owns the wire form of a line — `epoch_ms`, `level`,
  `target`, `message` — frozen by the golden files, serialised by the wire
  vocabulary that `LogLevel::wire()` already fixed for bridge 46.
- **`norte-config::LogLine`** stays the presentation type: `label()` pads to
  five columns so the list can be scanned by eye, and `LogLevel` orders by
  verbosity because that is the comparison the filter makes. Neither belongs on
  a wire.

A test asserts the two level vocabularies are the same set, in both directions.
This is the pattern the repository already uses for `hashing`, where
`norte-core` keeps a copy frozen by the journal format and an equality test
holds the two together.

## What the reader sees

The chip from #326 becomes a selector: **window / daemon / both**, defaulting
to both, merged by `epoch_ms`, with each line carrying its origin. The state
lives in `norte-frontend::LogPanel` next to the level filter and the text
filter, so the window and the TUI get the same behaviour from the same code.

With an **embedded** backend there is one process and one ring, so there is no
selector to show and the chip stays exactly as it is today. This is not a
special case bolted on: the selector appears when there are two sources, which
is the same condition that makes the panel wrong today.

Clocks: both processes are on one machine and share a clock, so merging by
timestamp is honest. Against a genuinely remote daemon it is not, and the merge
says so rather than interleaving two clocks silently.

## When the daemon cannot serve it

**Corrected 2026-09-02 (protocol-guardian, task 2).** The first draft of this
section said a 0.64 daemon answers "method not found" and the panel degrades.
That cannot happen, and a later task wiring a branch for it would be writing
code that never runs.

`version_compatible` does not negotiate a client minor *higher* than the
server's (`crates/norte-proto/src/methods.rs`, the 0.x arm: `cn == sn ||
cn + 1 == sn`), and the daemon enforces it — `initialize` is refused with
`VERSION_MISMATCH` (`crates/norte-core/src/daemon/server.rs`). So a 0.65
client against a 0.64 daemon **dies at the handshake**. It never sends
`log.tail`, and what the human sees is a connection that was refused, with the
upgrade signal the version-mismatch code already carries. The reverse — a 0.64
client against a 0.65 daemon — is fine and loses only what it never had: it
does not call the methods and keeps painting its local ring.

The fallback the panel must actually implement is a **same-version daemon
built without the `logging` feature**. It knows both methods and has no ring to
serve, so it answers `METHOD_NOT_FOUND` (or `Unsupported`, depending on how the
daemon wires it). The panel then falls back to what #326 built — its own ring,
with the chip — and **says why**: this daemon cannot serve its log.

So there is exactly one degradation branch, keyed on the method being refused,
not on a version comparison.

## Testing

The one that matters most, and the reason this document exists at all:

1. **The cap holds under the socket.** Raise to TRACE through `log.level`, emit
   a line on a `suppaftp` target and a line on a third-party target, and assert
   neither comes back from `log.tail` — while a `norte_core::*` TRACE line
   does.
2. **The cursor is honest about a gap.** Overflow a small ring, ask with a
   cursor from before the overflow, and assert `lost` is exactly the number of
   evicted lines and `next` resumes correctly.
3. **An agent is refused**, on both methods, and the refusal is a protocol
   error rather than an empty result — an empty log and a forbidden log must
   not read alike.
4. **N−1 degrades and says so**: a backend that answers "method not found"
   leaves the panel on its local ring with the reason visible.
5. **The merge is stable** under equal timestamps, and each line keeps its
   origin.
6. Goldens: catalogue TSV, `methods.json`, `proto.schema.json`, and the
   ui-host bridge goldens.

## Scope

In: the daemon's ring, the two methods, the SDK wrappers, the source selector
in `norte-frontend`, the polling in `norte-ui-host`, the same for `ntc` against
`--socket`, ADR 0092, protocol 0.65.0.

Out: persisting the ring across daemon restarts, reading the log **file**
remotely (that is `norte paths` plus a pager, and it already works), filtering
server-side by target or text (the panel filters what it has, and the ring is
2000 lines), and any access for agents or plugins.
