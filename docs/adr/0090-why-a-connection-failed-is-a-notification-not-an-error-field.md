# 0090 — Why a connection failed is a notification, not a field on the error

- Status: accepted
- Date: 2026-09-01
- Issue: #322
- Protocol: 0.64.0

## Context

Opening a remote connection can fail for a dozen different reasons, and the
person in front of the screen was told exactly one thing: **permission
denied**.

That is not an accident of wording. `norte_proto::Error` is a closed taxonomy
with no free text in it, on purpose: the error is what code *decides* with, and
a sentence is not a category. A `ConnectError::SecretEmpty` — "the secret for
«rosetta» is defined but EMPTY" — maps to `PermissionDenied`, and so does a
wrong password, a rejected key, and a bucket the account cannot see. The
sentence that separates them was written by `log_and_map`:

```rust
tracing::warn!(error = %e, "fallo de conexión remota");
Error::from(e)
```

…into the daemon's log, and dropped.

The embedded CLI *did* show it, because `tracing` there goes to the process's
own stderr. So the same failure was diagnosable or not **depending on the
transport**, which is the worst thing for it to depend on. And in the TUI even
the embedded case is invisible: the alternate screen eats stderr.

## Decision

**The reason travels as its own server→client notification,
`connection.failed`, with a closed `reason` vocabulary and an optional human
`detail`.** The error keeps its category and changes in no way.

```
{ conn?: string, scheme: string, host: string, reason: string, detail?: string }
```

This is not a new shape. `connection.degraded` (#44) is the same thing for the
sibling condition — a connection that opened but travels badly — and it earned
the shape: a closed vocabulary you can compare by equality, plus a sentence for
a human to read, each in its own field.

Three things follow from that, and they are the decision:

**The taxonomy does not grow a text field.** Adding one would mean every
consumer that switches on the category now has a string it might be tempted to
parse. `detail` is explicitly presentation: not parsed, not compared, and it
can be absent.

**`reason` is closed and can only grow additively.** Whoever receives an
unknown one degrades gracefully onto `detail` — it never rejects the
notification, and it never inherits the phrase of a reason it does know. That
last part is not hypothetical: #279 fixed exactly that bug in the degradation
banner, where a newer daemon reporting a new reason was painted as "FTP in the
clear", i.e. a security notice asserting a cause nobody had stated.

**And the vocabulary lives in one place, `methods::CONNECTION_FAILURE_REASONS`,
because three copies of a string is three chances to drift.** The first draft of
this change had the seven values written out in the emitter (`norte-core`), in
the translator (`norte-frontend`) and in the goldens — and the goldens froze a
copy the emitter was not on the other end of. Renaming a value in the emitter
turned nothing red: the notification kept going out, the frontend stopped
recognising it, and every failure started painting "unknown reason" forever, in
silence. Now the emitter's reason is a typed enum whose `wire()` is asserted
equal to the proto's list in both directions, and the frontend is asserted to
translate every entry of that list in both locales. Same trick
`ConnectionWarningReason` was already using next door — the sibling had it
right and this one had not copied it.

**`detail` is an allowlist, not a `to_string()`.** Only the variants whose
sentence norte composes out of its own fields can fill it —
`ConnectError::detalle_publico`. The ones that wrap third-party text, paths, or
the configuration file do not reach the wire: rule 10 does not distinguish
between "a secret" and "something that may contain a secret", and `Config(_)`
in particular wraps the `toml` parse error, which echoes the offending line.

## Consequences

**The two facts stay two channels.** A degraded session and a failed dial are
different things — one describes a session that exists and keeps existing while
you look at it, the other an attempt that is over — and merging them would make
one get painted as the other. So the SDK has `take_degraded` and `take_failed`,
and the frontends give them different lifetimes: the degradation is a
persistent banner, the failure is a transient notice. A permanent indicator
about something that is not open would never turn off.

**The engine's observer slot had to learn to chain.** It is a slot of one, and
there are now two `take_*` that install into it; before this, the second
silently left the first one's channel mute. `Engine::connection_observer()`
exists so the second reads the first and chains. Mute-in-silence was the
failure mode worth spending a public method on.

**The negative cache had to learn to repeat itself.** A failed dial goes into a
cooldown (1 s, doubling to 30 s) and the next attempt inside that window is
served from the cache without dialling — so it never passed the observer. The
explanation therefore vanished at the exact moment someone goes looking for it:
you read "the SSH agent could not authenticate", you press the key again, and
you get the bare category back. The cooldown entry now carries the `Causa` and
re-emits it. A diagnostic that only survives the first attempt is not a
diagnostic.

**The reason is announced only for a job that is still current**, the same rule
the #44 warnings already followed. A dial whose last waiter walked away, or one
replaced by a newer job on the same key, has nobody waiting for its answer — its
banner would land on the status line of someone who did not ask for anything.

**A peer one version behind loses exactly what it had before.** ADR 0004 says
an unknown notification is dropped in silence, so a 0.63 client keeps getting
the failure as a category with no sentence — the status quo, not a regression.

**Agents do not get it.** `broadcast_humans`, like `connection.degraded` and
`policy.*`. An agent connection does not read sentences; it decides by
category, and the category already reaches it in its operation's error.

**`AuthFailed` publishes no sentence, and that is the shape of the rule.** Its
`Display` is "authentication rejected for {user}@{host}" — the very userinfo the
core burns a `rsplit('@')` to keep out of `host`. Handing it back through
`detail` on the same notification would undo that, and nothing is lost: the
translated reason already says everything the sentence did. The allowlist is
about what a phrase *contains*, not about whether the variant has a good reason
to speak.

**One composer, two frontends.** `norte_frontend::banners::failure_line` is the
only place the sentence is built, and the TUI and the window both call it. Two
implementations of "why you could not get into that machine" diverge in
silence — which is the thing ADR 0077 exists to prevent — and one of the two
would be the copy that forgets to mask.

## Alternatives considered

**A `message` field on `norte_proto::Error`.** Rejected: it puts free text
where the decisions are made, and the SDK's `to_taxonomy` throws away
`rpc.message` anyway, so the fix would have had to cross five crates to make a
field that should not exist work.

**Widening the taxonomy — a category per cause.** `SecretEmpty` as its own
`Error` variant, and so on. Rejected: the taxonomy is what *behaviour* branches
on, and no caller behaves differently for an empty secret than for a wrong one.
It would grow the wire's decision surface to carry a diagnostic.

**Leaving it in the log and telling people to read the daemon's journal.** That
is the status quo, and it fails the case the issue is about: a laptop, one
person, a config file they wrote five minutes ago.
