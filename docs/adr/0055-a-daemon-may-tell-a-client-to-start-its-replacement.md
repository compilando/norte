# 0055 - A daemon may tell a client to start its replacement

- Status: accepted
- Date: 2026-08-16
- Decision makers: Oscar González
- Related: roadmap post-alpha item 10 and its design
  (`docs/superpowers/specs/2026-08-15-daemon-handover-design.md`), ADR 0004
  (unknown notifications are ignored on the wire), ADR 0005 (unknown values of
  an ORDER are a hard error, of a REPORT are degraded), ADR 0011 (the
  `initialize` handshake), ADR 0049 (a retained sync plan belongs to the
  connection that approved it), protocol 0.46.0.

## Context and problem statement

Replacing a running daemon — an upgrade — used to be indistinguishable, from a
connected frontend's point of view, from the user stopping it. Both are a closed
socket.

`RemoteBackend` already reconnects, re-`initialize`s, re-declares its actor and
resyncs live tasks through `task.list`. What it could not do is decide **whether
to bring the daemon back**, and there is a deliberate rule saying it must not:

> La 1ª conexión SÍ arranca el daemon (spawn); las reconexiones NO — reconectar
> jamás debe resucitar un daemon que el usuario acaba de parar.

That rule is correct. Without it `daemon.shutdown` would be unwinnable: every
connected frontend would race to restart what the user just asked to stop.

It is also exactly what makes an upgrade impossible. The old daemon exits, the
socket goes away, and every frontend reconnect-loops against nothing while
nobody starts the new one. A graphical session that has been open all afternoon
is dead, silently, after a routine package upgrade.

## Decision

**The daemon tells the client which of the two is happening, and the client is
allowed to act on the difference.**

Concretely (protocol 0.46.0):

- `daemon.going_away { reconnect: bool }`, broadcast to every connection before
  the listener stops accepting.
- `daemon.shutdown` grows `mode: ShutdownMode` (`stop` | `handover`), defaulting
  to `stop`.
- A client that received `reconnect: true` may spawn the daemon on reconnect —
  the one exception to the rule above. A client that received `reconnect: false`,
  or nothing, keeps today's behaviour exactly.

### Why this is a security decision and not only a protocol one

**It lets one process tell another to start a program.** That deserves to be
written down rather than discovered.

What bounds it:

- **The daemon supplies a boolean, never an argv.** `spawn_cmd` is chosen by the
  client at construction and resolved absolutely from `current_exe`, never
  through `PATH`. The daemon cannot influence *what* runs, only *whether*.
- **The notification can only arrive from a same-uid daemon.** The client
  refuses any peer whose uid differs before reading a frame; the daemon refuses
  the same in the other direction; the socket is 0600 in a 0700 directory, and
  `[daemon] socket` is never honoured from the project layer. The only party
  that can inject the notification is one already running as the user, which can
  rewrite `policy.toml` or run the daemon itself — not a boundary.
- **An agent never gets the permission.** `connect_as_agent` passes no spawn
  command, and the client additionally refuses to store the permission on an
  agent backend. Both, deliberately: the first is the property today, the second
  is what keeps it true if somebody later gives the MCP bridge a spawn command.
- **A handover is human-only**, like `policy.grant_scope` and like an ordinary
  shutdown. An agent asking for one gets `INVALID_REQUEST` before the parameters
  are even parsed.

### The permission expires by TIME, not by attempts

This is the part that was wrong in the first implementation and is worth
recording, because the obvious design is the broken one.

Spending the permission on the first reconnect looks right — one licence, one
use, no way to accumulate. It does not work: the first reconnect fires 250 ms
after the connection drops, and at that moment the old daemon has **not yet left
its process**, so it still holds the exclusive lock on `journal.db`. Opening
that journal is the first thing a replacement does, and it aborts if it cannot.
So the replacement died on the lock, the permission went with the failed
attempt, and no later attempt would start anything: a permanently dead session
after a routine upgrade — precisely the failure this whole item exists to
prevent.

What the no-resurrect rule actually wants is that a licence must not survive
"until next week". That is a bound on **time**. Within a thirty-second window
the client may try as often as it needs; after it, "the daemon is not there"
means what it has always meant.

### A handover with live tasks is refused, up front

The refusal happens in the reply to `daemon.shutdown`, which is the only moment
there is anybody left to tell — the reply goes out immediately, so a refusal
decided after draining would have no recipient, and by then the listener would
have stopped, making "refuse" mean "resume accepting".

A handover therefore **never cancels a task**, not even with `graceful: false`.
A caller who wants them cancelled uses a plain stop, which already does exactly
that. No second door to the same room.

### The socket is unlinked before draining, not after

A consequence of this ADR rather than a detail of it. Once handovers exist,
clients start replacements while the old daemon is still draining — so if the
old daemon unlinks the socket path *after* it finishes draining, it deletes the
**replacement's** socket, leaving a live daemon listening on an unlinked inode
that nobody can reach and whose permission has already been spent. The unlink
now happens immediately after the listener is dropped.

## Considered alternatives

**Pass the listening socket to the replacement (`SCM_RIGHTS`).** Hands over the
listener but not the connections already accepted on it — those belong to the
old process and still break. It buys nothing the reconnect does not already give
and costs an fd-passing dance.

**Let the client always respawn on reconnect.** Deletes the rule this ADR is
built around, and makes `daemon.shutdown` unwinnable.

**Have the package manager start the replacement.** Works, and is what a
system-managed service would do — but it does not cover a user-run daemon
started on demand by a frontend, which is the default mode. It also cannot know
when the old one has released the journal.

**Make the daemon survive its own upgrade** (exec into the new binary, keeping
fds). Preserves everything and is a much larger change with its own failure
modes; it is not ruled out later, and this design does not block it.

## Consequences

- Upgrading no longer drops an open session. The frontends come back with their
  tasks resynced.
- Stopping the daemon still means stopped, and there is now a test that proves
  it rather than a rule that merely has no override.
- A handover is refused while any task runs, so an upgrade waits for a copy
  instead of killing it. A package-manager hook therefore has to retry.
- **What a handover does not carry across**, all of it by design and none of it
  new: a retained sync plan (per connection, ADR 0049 — a replacement cannot
  redeem it, and the plan registry is in memory), a pending policy approval, and
  granted scopes. Each dies with the process, fail-closed.
- A client older than 0.46 ignores the notification and reconnect-loops as it
  does today; `norte daemon stop --handover` against a daemon older than 0.46
  says so rather than reporting success for something that did not happen.
- `ShutdownMode` has no `#[serde(other)]`, against the habit of this wire: ADR
  0005's asymmetry says an unknown value of an ORDER is an error, and this order
  shuts down a daemon.
