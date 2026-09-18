# 0123 — Handing the screen over releases it, rather than dying with it

- Status: accepted
- Date: 2026-09-18
- Decision makers: Oscar González
- Protocol: 0.77.0 → **0.78.0** (`session.release`)
- Session schema: unchanged at 2 — `SlotState::marks` is additive
- Related: ADR 0058 (layout tree and the session), ADR 0059 (a session body
  from the future is not read and above all not overwritten), ADR 0066 (the
  SDK boundary), ADR 0077 (the same command means the same thing in both
  frontends), ADR 0122 (phase 8), spec
  `docs/superpowers/specs/2026-09-15-historia-y-wow-design.md` (phase 9)

## Context and problem statement

Since 0.48 the daemon holds a UI session: the layout tree, where each slot is,
its cursor and its history. The first human connection claims it; later ones
get a copy and run loose. That is what makes `ntc` and the window show the same
screen.

What it did not make possible is **moving between them**. To hand the screen
from the terminal to the window you had to quit `ntc` — because ownership was
released only on DISCONNECT — and then hope the window started before you
changed your mind. If it did not start, you had killed the thing that was
showing you your work.

And one part of the screen never travelled at all: **the marks**. Everything
else is rebuilt with a `cd`; forty files picked out by hand are not.

## Decision

**1. `session.release`: give up ownership without disconnecting.** One method,
no params — who releases is the CONNECTION, and an id on the wire would be an
id anyone could send. It releases the ownership and NOT the body: the document
stays where it was, with its revision, which is exactly what the other
frontend is about to read.

**2. Releasing what is not yours does nothing, and it SAYS so**
(`released: false`). This is the field that makes the whole thing safe. The one
handing over needs to know whether the session actually came free before it
launches the other frontend: answering `true` unconditionally would turn a
refused handover into a window that opens onto nobody's screen.

**3. The order is write → release → launch, and each step gates the next.** The
screen is written first, because releasing first would leave the other frontend
reading the screen from a second ago. The release only happens if the `put`
landed — releasing after a failed write leaves the other one claiming a stale
body, which is worse than not handing over. And the launch only happens if the
release said `true`.

**4. When it fails, nothing happens and the process stays.** That is the
cheapest failure available: the screen is still in the core, this frontend is
still showing it, and the reader is told. The expensive failure — the one that
was structurally possible before and is not now — is the screen being released
by a process that then dies with nothing to take it.

**5. The marks travel in the session, by PATH, capped at 4096.** A path is the
row's identity; an index is a guess about a list that reorders itself and loses
neighbours above. Restoring by index would put a selection nobody made under a
cursor that is about to press delete.

**6. They are written only for a handover and read only under `--attach`.** Two
belts, and the doctrine behind them is one the codebase already wrote down
twice: marks are the state of a job in progress, not of a session, and
returning them at start-up would put an `F8` on what you marked yesterday. A
handover is not a start-up — seconds pass, not hours — so the same reasoning
gives the opposite answer, and the way to have both is to distinguish the two
cases explicitly rather than to pick one.

**7. `SlotState::marks` does not bump `SCHEMA_VERSION`.** It has `default` and
is skipped when empty, so an old body reads clean and a slot with no marks
produces byte-identical output to before. That matters beyond tidiness: the
session writer coalesces by comparing bodies, and a field that always
serialized would make every tick differ from the last.

**8. `app.handoff` declares its two impediments, separately.** No daemon means
there is no session to share; no desktop (SSH) means there is nowhere to put
the window. Both are announced through the availability table with their own
reason, because they are fixed differently — start norte against the daemon, or
sit at the machine — and because the fail-OPEN default would otherwise offer,
over SSH, a command that releases the screen and launches a window nobody sees.

**9. No preset binds it**, like `pane.ai-rename` and `pane.organize`. What
decides whether it can run is runtime state a keymap file cannot express, and
none of the imported managers attests a command like this.

## Consequences

**Good.** The machinery was almost all there: `session.get`/`put`, the
ownership claim, the session writer actor, the availability table with reasons.
What `session.release` adds is the one state that was missing — "free, with a
body in it" — and that state was already reachable by disconnecting, so
nothing new had to be made safe.

**The cost.** A new wire method (minor bump) and a `NativeEffect` the window's
host has to honour. A client 0.77 against a daemon 0.78 simply does not know to
ask; a client 0.78 against a daemon 0.77 gets `Unsupported` and degrades to not
releasing, which is the honest degradation — the handover does not happen, and
it says so.

**What is NOT verified here.** The window half of the handover needs a machine
with WebKitGTK, and the terminal it opens needs a desktop. This session
verified the terminal half (`--attach`, the marks round trip, the
availability reasons) and the protocol and controller halves with tests; the
`window → terminal` direction is exercised by controller tests against a
double, not by a human watching a terminal appear. `just gui-smoke` is what
checks the packaged window starts at all, and it is the next thing to run for
this.

## Alternatives considered

**Keep releasing only on disconnect and have `app.handoff` quit.** This is what
the code did, and it is the failure the ADR exists to remove: if the other
frontend does not start, the screen went with the process that quit.

**Have the arriving frontend take ownership by force.** It would remove the
`released: false` case and the whole race — and it would also let any second
`ntc` steal the screen from a window in use. The claim rule ("the first human
connection gets it, the rest run loose") is what makes two frontends safe, and
handing over is the ONE case where the owner wants to give it up. So the owner
gives it up, explicitly.

**Bump `SCHEMA_VERSION` for the marks.** Would have been defensible, and would
have cost every reader of an older norte their session for a field their code
ignores. `default` plus `skip_serializing_if` gets the same safety for free,
which is what `jump` and `palette_recent` already established.

**Restore the marks on any start.** Simpler, one fewer flag, and it undoes a
decision both frontends had written down: a start-up is not a handover.
