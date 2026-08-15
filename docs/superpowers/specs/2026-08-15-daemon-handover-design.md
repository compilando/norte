# A daemon that says it is being replaced: design

> Roadmap post-alpha item **10**, the half worth building. A new daemon takes
> over from a running one without the graphical and terminal frontends losing
> their sessions. Loopback TCP with a token is deliberately not here — it only
> matters if a client should be somewhere the socket is not, which is not true
> of anything that exists, and it opens network surface that would want its own
> security pass.

## The problem is not reconnection. Reconnection already works.

`RemoteBackend` (`crates/norte-core/src/backend.rs:2489`) is an
auto-reconnecting connection: it re-`initialize`s, re-declares its actor —
the daemon fixes it at the handshake and does not remember the previous one —
resyncs live tasks through `task.list`, and reconciles. `ConnEvent::Lost` and
`ConnEvent::Restored` already reach the message bar.

What blocks an upgrade is one deliberate rule, four lines above that code:

```rust
// La 1ª conexión SÍ arranca el daemon (spawn); las reconexiones
// NO (M3 del rust-reviewer: reconectar jamás debe resucitar un
// daemon que el usuario acaba de parar).
```

That rule is right. Without it, `daemon.shutdown` would be unwinnable: every
connected frontend would race to restart what the user just stopped.

**And it is exactly what makes an upgrade impossible**, because the client
cannot tell the two cases apart. The old daemon exits; the socket goes away; the
frontends reconnect in a loop against nothing, forever, and nobody starts the
new one. The two situations are byte-identical on the wire — a closed
connection.

So the work is not reconnection. It is **giving the daemon a way to say which
of the two is happening**, and giving the client permission to act on the
difference.

## The shape

One new notification and one new field.

### `daemon.going_away`

Broadcast to every connection at the start of a handover, before the listener
stops accepting:

| field | meaning |
| --- | --- |
| `reconnect` | `true` = come back; a replacement is expected. `false` = this daemon is stopping and staying stopped. |
| `grace_ms` | how long the daemon will wait for live tasks before exiting, so a client can show something honest instead of a spinner with no end |

`grace_ms` is the daemon's, not the caller's: it comes from `DaemonConfig`
alongside `idle_timeout`, and is reported in the notification rather than
accepted in the request. A client that could name its own grace period could
name a very long one, and a shutdown is an act of governance over a process the
caller does not own. Its default is the same order as the idle timeout —
minutes, not seconds — because the thing it waits for is a file copy.

`reconnect: false` is not decoration. Sending it on an ordinary shutdown is what
lets a frontend say "the daemon was stopped" instead of "connection lost", which
is the difference between a state and an accident — and it costs one boolean.

### `daemon.shutdown` grows `mode`

Today: `graceful: bool`. It stays, because it is on the wire and it means
something orthogonal (wait for tasks, or cancel them first).

The new field is `mode`, defaulting to the current behaviour:

| `mode` | what it means |
| --- | --- |
| `stop` (default) | what happens today, plus `going_away { reconnect: false }` |
| `handover` | a replacement is coming: `going_away { reconnect: true }`, then stop accepting, drain, exit |

A client that never learned about `mode` keeps getting `stop`, which is what it
already got. A client that never learned about `going_away` ignores an unknown
notification, which the wire already requires it to do, and degrades to exactly
today's behaviour — a lost connection and a reconnect loop. **The upgrade path
is therefore only as good as the client, and never worse than the present.**

### What the client does with it

`RemoteBackend` remembers the last `going_away` it saw. On the next reconnect:

- saw `reconnect: true` → allowed to spawn the daemon, once, exactly as the
  first connection does.
- saw `reconnect: false`, or saw nothing → today's rule, unchanged. No spawn.

The permission is **consumed by the reconnect that uses it**. A frontend that
was told a replacement is coming, failed to find one, and gave up must not carry
that licence into next week's connection attempt.

## Sequencing, and what a handover costs

1. `daemon.shutdown { mode: "handover" }` arrives on a human connection. Agent
   connections cannot shut the daemon down and cannot hand it over either —
   same rule, same reason (it is an act of human governance, like
   `policy.grant_scope`).
2. The daemon broadcasts `going_away { reconnect: true, grace_ms }`.
3. It stops accepting. The `shutdown` token already does this first; the code is
   there.
4. It waits for live tasks up to `grace_ms`. **Waiting, not cancelling**, and
   this is the whole reason `graceful` and `mode` are separate axes: a copy
   killed mid-tree is precisely the mess the journal then has to clean up.
5. It exits. The socket is released.
6. Clients reconnect, spawning the replacement if it is not up yet.

**What does not survive, stated rather than discovered:**

- **Retained synchronisation plans.** They are held per connection, by design
  (ADR 0049) — a plan cannot be redeemed by anybody else. A new connection is a
  new holder, so a plan approved but not applied is gone and must be re-planned.
  This is correct behaviour, not a defect: applying a ten-minute-old plan across
  a daemon replacement is exactly the thing the retention rule exists to stop.
- **In-flight approvals.** A `policy.approval_required` awaiting an answer dies
  with the daemon that asked. The operation behind it does not proceed.
- **Task progress subscriptions.** Restored by the existing `task.list` resync,
  which is what it was built for.
- **Live tasks.** They are waited for, so they finish before the exit. What the
  grace period cannot cover — a copy of a terabyte — is reported to the caller
  as "still running" instead of being killed, and the handover is refused.

That last point is a decision: **a handover that cannot drain does not
happen.** The alternative is a `--force` that cancels, and it already exists
under a different name (`graceful: false`). No new door to the same room.

## Protocol impact

Additive: one notification, one optional field with a default that reproduces
current behaviour. Version bump to **0.46.0**, golden tests updated,
`protocol-guardian` review mandatory per `CLAUDE.md`.

A 0.45 client against a 0.46 daemon sees an unknown notification and ignores it.
A 0.46 client against a 0.45 daemon sends `mode`, which the older daemon's
deserialiser ignores as an unknown field, and gets today's shutdown. Neither is
an error, and neither is a silent wrong answer: the worst case in both
directions is that the handover degrades to a stop, which is where we are now.

## Testing

- A handover broadcasts `going_away { reconnect: true }` to every connection,
  including one that connected mid-shutdown, before the listener stops.
- An ordinary shutdown broadcasts `reconnect: false`, and a client that receives
  it does **not** spawn on the next attempt — the regression test for the rule
  this design is careful not to break.
- A client that saw `reconnect: true` spawns exactly once. If that spawn fails,
  the permission is consumed and the next attempt does not spawn again.
- End to end: a daemon with two connected clients hands over to a replacement,
  and both clients come back with their tasks resynced. This is the test the
  whole item exists for, and it is the one that must not be simulated with a
  mock — two real connections, a real socket, a real replacement process.
- A handover with a live task waits for it, and reports rather than killing it
  when the grace period runs out.
- An agent connection is refused `mode: "handover"` with `INVALID_REQUEST`, like
  every other act of human governance.

## What this spec does not do

- **Loopback TCP with a token.** Deferred; nothing needs it, and it is network
  surface with its own review.
- **Passing the listening socket between processes.** `SCM_RIGHTS` would hand
  over the listener, but not the connections already accepted on it — those
  belong to the old process and would still break. It buys nothing that the
  reconnect does not already give, at the cost of an fd-passing dance.
- **Preserving retained plans or approvals across the gap.** They are
  per-connection by design; carrying them over would mean a daemon-side identity
  for a client that survives its connection, which is a much larger idea and one
  the security model has not been asked about.
- **Automatic upgrade.** Nothing here decides *when* a handover happens. That is
  the packaging item's problem, and it should stay a thing a human or a package
  manager does.
