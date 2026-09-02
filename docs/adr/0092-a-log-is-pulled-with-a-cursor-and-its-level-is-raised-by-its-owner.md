# 0092 — A log is pulled with a cursor, and its level is raised by whoever owns the ring

- Status: accepted
- Date: 2026-09-02
- Decision makers: Oscar González
- Issue: #328
- Protocol: 0.65.0
- Related: ADR 0004 (wire conventions), ADR 0011 (envelope and daemon), ADR 0077
  (the same command means the same thing in both frontends), #43 (rule 10),
  #326 (the window's log panel)

## Context

The log panel a frontend paints reads the ring of **its own process**. That is
the right answer exactly once: when the core is embedded, as in `ntc`, where
frontend and core are one process and one ring.

Everywhere else it is the wrong process. `norte-gui` starts its own daemon
(#300), so the ring it paints holds the window's lines — the bridge, the
renderer, the startup — while the providers, the journal, the policy engine and
the reason a connection could not be opened all happen on the other side of a
socket. `ntc --socket <remote>` has the same hole. And the daemon has no ring at
all: `norte-cli` installs `logging::init` without one, so there is nothing to
read before there is something to serve.

#326 shipped a chip saying **whose** log the panel is showing, which stops it
from looking broken. Saying it is not the same as being able to read the other
one, and the question this ADR answers is how the daemon's lines cross.

Two constraints shape the answer, and neither is negotiable.

**The ring has a floor that cannot travel.** `suppaftp` emits
`PASS <password>` at the `log` crate's TRACE level (#43, rule 10), and this
ring's level is raised *from the user interface* — that is the whole point of
the panel. `norte_config::logring::bajo_cota` is therefore an allowlist: only
`norte`/`ntc` targets go above INFO, and everything third-party stays at INFO
no matter who asks for what. Whatever crosses the wire must not give anyone a
way around it.

**A log with a silent hole lies.** A missing line is indistinguishable from an
event that never happened, which is the one failure a log cannot have. The
local panel already paints a gap marker fed by the ring's own eviction count;
the remote one needs an equivalent that is honest about *this* reader's gap,
not about everything the ring ever dropped.

## Decision

**The daemon's log is PULLED with a monotonic cursor, and its level is raised
through a method that the daemon itself applies.** Protocol 0.65.0 adds two
read-only request methods and their types:

```
log.tail  { cursor: u64|null, max: u32 }
       →  { lines: [LogLine], next: u64, lost: u64, level, capacity: u32 }
log.level { level }  →  { level }
```

Four things follow, and they are the decision.

**Pull, not push.** The ring already owns `LogRing::pushed()`, a counter that
only goes up, so a cursor costs nothing to produce and **the daemon keeps no
per-client state**: no subscription to register, and no unsubscribe to miss
when a client dies without saying so. A dropped notification is a silent hole;
a stale cursor is arithmetic, and `lost` says exactly how many lines fell off
the back before this cursor saw them. A closed panel costs zero, where a
subscription keeps paying. What is paid instead is latency — up to one poll
interval, ~300 ms — and for a list a human reads that is not a cost.

**`cursor: null` is "whatever you have", and it is not `0`.** A zero asserts
that the caller saw line number zero and wants everything after it, so against
a ring that has already wrapped the daemon would have to answer a large `lost`
— and that gap would be a lie, because nobody lost lines they never expected.
With `null` the daemon starts at the oldest line it still holds and answers
`lost: 0`. These are two different questions, so they get two different values
rather than one sentinel doing double duty.

**`log.level` is a METHOD, not a parameter the client applies.** This is the
part that makes the cap survive the socket *by construction*. The client asks
for a level; the daemon calls `LogRing::raise_to` itself and answers with the
level that actually took. There is no second copy of the allowlist on the far
side of the cable, and therefore nothing to keep in sync. The result carries
the level rather than a boolean because `raise_to` never lowers: asking for
`warn` while the ring is at `debug` answers `debug`, and that is the correct
answer, not a failure. Two more facts get stated on screen rather than hidden:
the level is **global to the daemon**, and it **never goes down** — dropping
back and climbing again would show a hole the size of the time spent down
there.

**Agents are refused, on both methods**, with the same `INVALID_REQUEST` shape
`policy.request_scope` uses for the mirror-image case. The reason is concrete,
not a principle: the daemon's ring carries paths, connection names and the
activity of *other sessions*, so for a scoped agent it is an existence oracle
for paths outside its sandbox — precisely the leak `read_gate_all` already
documents for `plugin.decorate` and `plugin.column_values`. Raising the level
would be worse: an agent could turn up the verbosity of work it is not party
to. The catalogue does not record access (a second source of truth about who
may call what lies as soon as the first one changes), so this lives where every
other access decision lives — in the daemon — and a test pins it.

### The wire type is not the presentation type

`norte-config` depends on `norte-proto`, so the wire form of a line lives in
the protocol crate and the presentation form stays where it is. They are
deliberately not the same type:

- `norte_proto::methods::LogLine` is `epoch_ms`, `level`, `target`, `message`,
  frozen by the golden files, with `level` one of the closed strings in
  `methods::LOG_LEVELS`.
- `norte_config::logline::LogLine` keeps `label()`, which pads to five columns
  so the list can be scanned by eye, and a `LogLevel` that orders by verbosity
  because that is the comparison the filter makes. A column width that may
  change cannot be a wire change, and an enum's ordering does not serialise.

The vocabulary therefore exists twice, which is why a test in `norte-config` —
the only crate that can see both — asserts the two sets are equal **in both
directions**, and that every wire string converts back. That is the pattern the
repository already uses for hashing, where `norte-core` keeps a copy frozen by
the journal format and an equality test holds the two together.

## Consequences

**A frontend polls only while its panel is open**, and only when the selected
source includes the daemon. Nothing is spent when nobody is looking, which is
most of the time.

**The gap is visible instead of implied.** `lost` is `base - cursor`, what
*this* cursor missed — deliberately not `LogRing::dropped()`, which counts
everything the ring ever evicted and says nothing about the reader. It feeds
the same gap marker the local ring already has.

**`level` and `capacity` ride in the tail result** rather than in a call of
their own. The panel has to show which level is set — otherwise "no DEBUG
lines" is indistinguishable from "DEBUG is not being captured" — and the level
is global, so another client may have raised it a second ago; a separate call's
answer is born stale. `capacity` says how deep the history goes, so the panel
can say "this is all there is" instead of implying there is more, and it
doubles as the discoverable ceiling for `max`: there can never be more lines
than the ring holds.

**Raising the level is a decision one client takes for everyone**, and the
panel says so rather than pretending it is private.

**A peer one version behind loses only what it never had.** A 0.64 client does
not call the methods and keeps painting its local ring — the status quo. A 0.65
client against a 0.64 daemon, or against one built without the `logging`
feature and so with no ring to serve, gets "method not found" and **falls back
saying why**. That last part is not optional: a panel that degrades in silence
is indistinguishable from a daemon that did nothing, which is the confusion
#326 started to fix.

**`max` is a request, not a contract.** The daemon clamps, the way `fs.list`
clamps against `FS_LIST_MAX_PAGE` and `fs.read` against `FS_READ_MAX_CHUNK`.
Asking for more is not an error and loses nothing: what does not fit is still
after `next`.

**The daemon has to grow a ring at all**, and expose its capacity. That is the
task this ADR precedes, not part of the wire.

## Alternatives considered

**Push: the daemon notifies each subscribed client of new lines.**
*For:* no polling, no latency, and it matches how `task.progress` and
`policy.approval_required` already work. *Against:* it makes the daemon keep
per-client state — a subscription to register and an unsubscribe that a dead
client never sends — and, decisively, a dropped notification is a silent hole
in a log, which is the one failure a log must not have. It also keeps paying
while the panel is closed. Rejected.

**Push with sequence numbers, so gaps are detectable.** *For:* keeps the low
latency and makes a hole visible. *Against:* it is pull's bookkeeping with
push's per-client state on top, and the client still has to be able to ask for
what it missed — which is `log.tail` again, now as a second mechanism. The
simpler design is the one that only has the second half. Rejected.

**Level as a field on `log.tail`, applied by the client.** *For:* one method
instead of two. *Against:* the client would need its own copy of `bajo_cota`'s
allowlist to know what it may ask for, and two copies of a defence drift as
soon as one changes. Worse, the honest answer to "what level did I get" would
have to be reconstructed rather than reported. Rejected — this is the inverse
of the decision.

**Read the daemon's log FILE remotely instead.** *For:* no new methods; the
file already exists. *Against:* it is a second path to the same information
with different content (the file has the file's filter, not the ring's), and it
turns a bounded in-memory read into remote file access with its own policy
questions. `norte paths` plus a pager already covers reading the file, and it
works today. Rejected, and explicitly out of scope.

**Server-side filtering by target or text.** *For:* less data on the wire.
*Against:* the ring is 2000 lines, the panel already filters what it has, and
the filter would then exist in two places with the frontend's version being the
one people actually see. Rejected as out of scope.
