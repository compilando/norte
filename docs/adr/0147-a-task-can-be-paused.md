# 0147 — A task can be paused

- Status: accepted
- Date: 2026-09-22
- Decision makers: Oscar González
- Protocol: 0.81.0 → 0.82.0 (`task.pause`, `task.resume`). Bridge: 92 → 93.
- Related: ADR 0004 (N/N-1), ADR 0146 (the light progress bar),
  `docs/estudio-cola-y-progreso-2026-09-22.md` §4.1

## Context and problem statement

A long copy could be watched or cancelled, nothing in between. Pausing is the
most asked-for job control after cancelling (Windows Explorer, Krusader's
JobMan, Total Commander's background dialog). `TaskState::Paused` has been on
the wire since M0, reserved and never emitted, so clients already read it as
non-terminal.

## Decision

**A task has a pause gate next to its cancellation token, and bodies honour
it at the same checkpoints where they honour cancellation.**

1. **Core.** `PauseGate` (a shared `watch<bool>`) lives in `TaskCtx` and
   `TaskHandle`. `TaskCtx::checkpoint()` returns `Cancelled` if cancelled and,
   if paused, waits — publishing `Paused`, then `Running` — until resumed or
   cancelled. Checkpoints are in the per-chunk loop of a streaming copy and
   between entries of copy, move-by-copy and delete. A task paused before it
   starts does not run its body until resumed; if cancelled while waiting, the
   body still runs so it can clean up and report, as with any cancellation.
2. **Honest granularity.** A copy with no chunks (server-to-server, the
   kernel's fast copy) pauses at the end of the current file. The frontends
   say "pausing…" when asked and show `Paused` only when the task says so.
   And only the kinds that HAVE checkpoints — copy, move, delete — can be
   paused once running (`norte_frontend::tasks::pausable`); a search or a
   checksum would accept the request and go on, so the frontends refuse it
   and say why. A task of any kind that has not started yet does wait.
3. **Protocol 0.82.0.** `task.pause` and `task.resume`, same params shape and
   same actor scope as `task.cancel`: another session's task, a terminal or an
   unknown one gets the identical empty ack and nothing happens. The SDK's
   call is NOT fire-and-forget: a 0.81 daemon answers `METHOD_NOT_FOUND`,
   which becomes `Unsupported`, and the frontends say pausing is not possible
   rather than showing a pause that did not happen.
4. **One command, `task.pause`, toggles**, on the same task `task.cancel`
   would pick. Bound to `Ctrl+Alt+K` next to cancel's `Ctrl+K` in orthodox,
   cua and vim; the four imported presets leave it unbound like `task.cancel`,
   with the reason in their headers.
5. **Seen everywhere.** The task board shows `⏸ 40%`; the light bar
   (ADR 0146) shows `⏸ paused photo.jpg 40 %` with no rate when every live
   task is paused; the window gets `TaskStateView::Paused` and a `paused`
   bar phase (bridge 93).

## Consequences

- While paused, a copy keeps its source stream and staging file open. On a
  remote provider an idle connection may time out; the copy then fails or
  retries on resume like any other transient error.
- A paused task keeps its scheduler permit: the other three per scheme still
  run. Queueing (the study's phase C) is a separate decision.
- `TaskCanceller` is unchanged; pausing travels in a sibling `TaskPauser`, so
  surfaces that do not pause (MCP) are untouched.

## Alternatives considered

- **One `task.set_paused { paused }` method.** Equivalent; two methods mirror
  `task.cancel` and read better in logs and in the catalogue.
- **A task-local pause gate** instead of a `TaskCtx` field: no signature
  churn, but a hidden coupling that a body could not see or test.
- **Releasing the permit while paused.** Lets a queued task run, but a resume
  would then have to wait for a permit, which reads as "resume did nothing".
