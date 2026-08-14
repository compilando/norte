# Debt wave W7 — the four that were waiting on a decision

Not a wave of findings: a wave of ANSWERS. Each of these four had its
diagnosis written and was blocked on a question only the human could settle, so
the session began by asking the four and building what came back.

| # | the question | the answer |
| --- | --- | --- |
| #171 | a mid-stream policy denial in the undo gate: report row, or fail the Task? | **report row**, matching the forward executor |
| #207 | a plan acting on a pair joined by a non-injective transform: skip, or blocker? | **skip with a reason**; the plan stays approvable |
| #203 | a `Busy` journal nobody can explain: escalate how? | **a different indicator** when no daemon is listening; never a refusal |
| #179 | the ownership window: both halves, or the safe one? | **the safe half** — and it turned out to be built already |

## What each one cost, and the two surprises

**#207 needed a wire token after all.** The option chosen said "no new
vocabulary", and that was my claim, not a fact: no existing `SyncReason` says
"the pairing itself cannot be trusted". `AmbiguousSource` means "two names on
the SOURCE collapse into one key" — a different fact, and painting it would
have explained the row to the human incorrectly. A wrong sentence is worse than
a bump, so 0.43.0 carries `NonInjectivePairing` and ADR 0053 records why. The
part that mattered — skip, not refuse — is exactly what was decided.

**#179 was already done.** W4a built the retry, the brake, the serialisation
and the recovery event; this issue predates that branch. Verified against the
code and the tests and said so in the issue, which now tracks only the half
that was deferred on purpose: releasing the lock when idle, where the
`ChainState` re-read hazard lives.

**#171 turned out to be two changes in one.** Moving the gate inside the Task
fixes the unbounded parsing AND the stale verdict, because both come from the
same shape: the gate did all its thinking up front. The report needed a way to
say "denied and skipped" that could not be confused with `blocked`, so
0.43.0 also carries `denied`/`denied_total`.

**#203 is a detection, not a defence.** "Busy for minutes AND no daemon
listening" is a state norte can tell apart and did not. It changes what the
human is told, never whether the mutation runs — turning "someone holds your
journal" into "the file manager does not work" is the fix #178 avoided.

## Gate

One `just ci` at the close, protocol goldens and schema regenerated with the
bump. `protocol-guardian` on the two proto changes (both additive, both in the
same version).
