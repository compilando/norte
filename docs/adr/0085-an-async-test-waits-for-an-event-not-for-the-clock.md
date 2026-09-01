# 0085 - An async test waits for an event, not for the clock

- Status: accepted
- Date: 2026-09-01
- Decision makers: Oscar González
- Related: ADR 0066 (`norte-ui-host` over the SDK), CLAUDE.md ("an
  intermittently red test is a bug, not noise"), plan
  `docs/superpowers/plans/2026-09-01-tests-asincronos-deterministas.md`.

## Context and problem statement

`crates/norte-ui-host/tests/controller.rs` held **111 `tokio::time::sleep`**
calls across 376 tests — 227 in the whole workspace, 185 of them in tests. The
shape was always one of two:

```rust
h.dispatch(...).await;
tokio::time::sleep(Duration::from_millis(30)).await;
assert_eq!(backend.borrados.lock().unwrap().len(), 1);
```

and

```rust
for _ in 0..40 {
    if backend.listados() > antes { return; }
    tokio::time::sleep(Duration::from_millis(25)).await;
}
panic!("nadie relistó");
```

Neither is a wait for anything. Both are a bet on how long a machine takes,
placed while nextest runs 423 tests in parallel. The first has no retry at all,
so when it loses it fails an assertion twenty lines away from the actual cause;
the second has a one-second budget of wall clock that a loaded machine can
exhaust with nothing broken. Both are the shape that teaches people to re-run a
red test instead of reading it.

Measuring first corrected the reason to care. The suite is **not** slow because
of them: 423 tests run in 15.1 s, of which three `payload` tests are 30 s of
CPU and all ~376 `controller` tests together are 3.9 s. What the sleeps cost is
**trust**, not seconds.

## What makes it fixable

`norte-ui-host` is a single-writer actor with a mailbox (ADR 0066). Every
mutation follows the same path (`controller.rs`, `crear_directorio`): the actor
validates, `tokio::spawn`s the backend call, and **returns the ack**. The test
double records what it was asked for synchronously, on entry. Three properties
fall out, and the decision rests on them:

1. If the ack came back, the actor has already decided — if it was going to
   enqueue, the `spawn` has happened.
2. Spawned tasks record in the double in the order they were spawned
   (`current_thread` executor, FIFO queue).
3. The double can **announce** what it records.

## Decision

**A test waits for the event it is about. There are three tools and no fourth.**

### `hasta` — "has X happened yet?"

The double carries one `Notify` and calls `latido()` after every recording
(`creados`, `borrados`, `transferencias`, `lotes`, `sondeos`, `listados`,
`escrituras`, `gobierno`, `aplicados`, `permisos`, …). `Falso::hasta` arms the
notification *before* re-reading the predicate — the same order `Puerta::esperar`
already used — so a beat that lands mid-check is not lost.

Its 15-second deadline is a **failure budget, not a wait**: on the green path
it is never touched, and when it expires the test says which event it was
waiting for instead of exploding in a later assertion that explains nothing.
`anotados(f, qué, n, campo)` is the short form for the overwhelmingly common
"at least `n` things landed in this list".

### `asentar` — "is it certain that nothing happened?"

For a negative claim there is no event to wait for. What has to be guaranteed
is that anything the actor may have spawned before answering the ack has had
its turn, so `asentar` yields the executor 32 times. Yielding is not a clock:
it does not get worse under load, which is the entire difference. The file
already used this shape (`siguiente_recuento`, 200 `yield_now`); this names it.

For "no further snapshot arrived", `asentar` is followed by
`timeout(Duration::ZERO, sub.recv())` — a single poll, not a wait.

### `foto_hasta` — "when the double cannot say"

Some things the double never sees: a config write that returns through
`spawn_blocking`, a sidebar reseeded when it does. `foto_hasta` re-dispatches
`Resync` and reads snapshots until the screen says what is expected. Each turn
is a round trip through the actor, so the loop advances at the host's pace.
Same deadline, same meaning.

### Real deadlines are `start_paused`, and simulated latency stays in the double

A genuine timer (the ten-second TTL of the task board, an extension timeout) is
`#[tokio::test(start_paused = true)]` plus `tokio::time::advance`, which
**skips** the deadline rather than serving it.

The four `sleep`s left in the workspace's `backend_falso` are `retraso_ms`, and
they stay: that is the latency the double simulates, not a guess by the test.
What was wrong was how tests waited it out. The double now counts `pedidos` and
`servidos`, and `en_calma()` means "nothing is in flight" — so a test asserting
what the host does with a LATE answer waits for that answer to have arrived,
instead of proving nothing by not having waited long enough.

The one remaining guessed sleep inside the double is gone too: `create_file`
kept its progress sender alive for 50 ms so the channel would not close before
the host pumped it. The sender is now held by the double, which is the same
guarantee without the bet.

## Consequences

- `tests/controller.rs`: 111 sleeps → **0**. Workspace: 227 → 117.
- 423 tests, 15.1 s → 13.2 s. The gain is small because the sleeps were never
  the bottleneck; that was never the point.
- Every wait now names what it waits for, in the failure message.
- New async tests in this crate have three named tools and no reason to reach
  for a duration. A `tokio::time::sleep` appearing in a test of this crate is
  now something to question in review.
- The same treatment is available to the 117 that remain (`norte-core/tests/daemon.rs`
  has 19, `norte-mcp/tests/transport.rs` 6), and to the ~26 wall-clock sleeps
  CLAUDE.md already flags as the source of load-dependent flakes. This ADR is
  the pattern; applying it elsewhere is separate work.
