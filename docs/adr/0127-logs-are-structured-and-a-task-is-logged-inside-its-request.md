# 0127 — Logs are structured, and a task is logged inside the request that asked for it

- Status: accepted
- Date: 2026-09-19
- Decision makers: Oscar González
- Related: ADR 0092 (the daemon's log over the wire), #255 (the log file's
  permissions), #43 (the `suppaftp` cap), the architecture review of
  2026-09-18

## Context and problem statement

norte logs through `tracing`: one `EnvFilter` per layer, a rotated daily file,
the in-memory ring the log panel reads, and a hard cap that keeps
`suppaftp`'s `PASS` line out of every sink. Two things were missing.

**The file was text only.** A line like `INFO norte_core::scheduler: …` is for
a person. Nothing can filter it by task or by request without a regular
expression that breaks the day a message changes.

**Nothing could be correlated.** The daemon's `dispatch` had a span
(`dispatch{method}`), and `Scheduler::submit` and `run_job` had their own. But
the runner is `tokio::spawn`ed, so `run_job`'s span had no parent, and
everything a task logged was cut off from the request that asked for it. A warning from
inside a copy did not say which request asked for the copy, or on which
connection. Worse, a runner pops *whatever job the priority heap hands it*,
which is not necessarily the one its own `submit` pushed. So "the span of the
submit that spawned this runner" is the wrong answer even if it could be
found.

## Options

1. **A `Logger` trait injected into the core.**
   - Good: textbook dependency injection; logging is visible in signatures.
   - Bad: every function that wants to warn gains a parameter. `tracing` is
     already that abstraction: a facade the binaries bind to a subscriber.
     Wrapping it would add a layer and remove nothing.
2. **Correlation ids passed as arguments and logged by hand in each message.**
   - Good: explicit.
   - Bad: each message has to remember to add them, and the first one that
     forgets is the one that matters.
3. **Spans at the two boundaries (request, task) plus a JSON file format.**
   - Good: every event inside the boundary inherits the ids with no change
     at the call site; JSON makes them fields rather than text.
   - Bad: the hierarchy has to be kept at the boundaries, and a `spawn`
     that forgets `.instrument(..)` silently drops it.

## Decision

Option 3.

**1. `[log] format = "text" | "json"`**, default `text`. It changes the
**file** layer only. stderr is for a person reading a terminal, and the ring
already stores structured `LogLine`s. JSON lines carry `with_current_span`
and `with_span_list`, so each event lists the spans it happened in.

It needs `tracing-subscriber`'s `json` feature. That adds `tracing-serde`
(tokio-rs/tracing, the same repository and maintainers as `tracing-subscriber`,
MIT, a few hundred lines) and `serde_json`, which is already in the tree.
There is no alternative worth a separate crate: hand-writing a JSON
formatter is exactly what that feature is.

**2. The span hierarchy is fixed:**

```text
rpc{conn_id, req_id, method}          daemon: around `dispatch`
└─ task{task_id, kind, provider}      created in `Scheduler::submit`
   └─ … every event the task body emits
```

- `req_id` is the JSON-RPC `id` that already travels, and the client knows
  its own `id`. **Correlating does not need a protocol change.** `conn_id` +
  `req_id` identifies a request within one daemon's lifetime as long as the
  client does not reuse ids. norte's SDK sends a counter, so it doesn't.
- `rpc` replaces `dispatch`'s old `dispatch{method}` span. Both stacked would
  nest (`dispatch → rpc`), repeat the method and put the wrong span first.
- `method` and `req_id` are chosen by the peer. They are recorded escaped and
  cut to 64 characters, so a `\n` cannot forge a line in the text log and a
  16 MiB id is not copied onto every line of its tasks. JSON would escape
  them anyway. Text does not.
- The `task` span is created in `submit`, as a child of whatever is current
  there (the `rpc`, or nothing in embedded mode), and **stored in the
  `QueuedJob`**. The runner instruments the job it POPS with that job's own
  span, which is the only span that is right for it.
- The fields of `rpc` and `task` are ids, kinds, a scheme and method names.
  **Never a path or a parameter.** The engine's own `#[instrument]` spans
  sit between the two (`copy_anchored{from, to}`), and they do carry paths,
  through `span_path`, which redacts `user:pass@`. A task used to log with no
  parent, so its events now also carry which copy they belong to. That is
  deliberate: it is what makes a line useful, the file is 0600 (#255), and
  hostile names arrive already made safe by `display_lossy`. One of those
  spans recorded a `VPath` by `Debug`, which bypassed the redaction
  (`rename_batch_plan_as`). It now goes through `span_path` like the rest.

**3. What each level means.** It is written in `logging.rs`'s module
rustdoc, because that is where someone choosing a level will look:

| level | when |
| --- | --- |
| `error!` | something the user asked for failed and will not recover |
| `warn!` | something degraded and carried on (inotify → polling) |
| `info!` | lifecycle: start, connect, a task starts and ends |
| `debug!` | decisions: policy verdicts, keymap resolution |
| `trace!` | per entry or per block; never on by default |

There is no `fatal`. A fatal condition is an `error!` in a binary's `main`
followed by an exit through `anyhow`. A library never ends the process.

## Consequences

- Good: `jq 'select(.spans[]?.task_id == "…")'` over a day's file gives
  everything one task logged, and the `rpc` above it says who asked.
- Good: the core stays free of logging plumbing. It opens spans at two
  boundaries and emits events as it already did.
- Bad: one more transitive crate (`tracing-serde`).
- Bad: a `tokio::spawn` or `spawn_blocking` inside a task body loses the
  `task` span unless it is `.instrument(Span::current())`-ed, and **many
  already do**: every `spawn_blocking` in `norte-vfs-local`,
  `norte-vfs-archive`, `sync/spool.rs` and `pack.rs`, plus `hooks.rs`.
  Events emitted inside those closures still have no parent. The async code
  around them keeps it. Two tests pin the boundaries (the scheduler's, and
  one through the daemon's socket). They cannot pin every spawn.
- Bad: a `task` holds its parent `rpc` open until the task ends. A layer
  that timed spans on close would report a request as lasting as long as its
  longest task.
