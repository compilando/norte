# 0156 — A sync plan does not measure the orphans it copies

- Status: accepted
- Date: 2026-09-25
- Decision makers: Oscar González
- Protocol: unchanged. This records a behaviour that already exists.
- Related: ADR 0049 (the retained sync plan), issue #52 (local listings carry
  no size), issue #157 (compare does not hydrate orphans),
  `docs/superpowers/specs/2026-08-11-directory-sync-design.md`

## Context and problem statement

Shooting the landing, a sync (update) from `~/projects/aurora` to
`~/Backup/aurora` said *"468 B to write, plus 2 files whose size the provider
did not give"*. The two files were local — a `LICENSE` missing at the
destination and a new `src/sensor.rs` — and the local provider can certainly
say how big they are. It was filed as a bug: "the only-left entries of the
planner do not carry the size of the source's stat".

They do not, and that is by design. Three earlier decisions meet here:

- `norte-vfs-local` lists without a size (#52): the size is hydrated on
  demand, because a `stat` per entry is what made large directories slow.
- A comparison only pays for the `stat` a rung of its criterion will use
  (C7b). An `OnlyLeft`/`OnlyRight` row is decided by presence alone, so it is
  never hydrated (#157). The 468 B are the pair that reached the size rung.
- The sync planner turns compare rows into steps and records the row's size
  as the step's `size`. The spec and the wire define `SyncCounts::bytes` as a
  **lower bound**, with the steps whose size is unknown counted in
  `unmeasured_steps` rather than summed as zero. The proto tests call an
  unmeasured copy "the NORMAL case and not the rare one".

So the plan reports faithfully what it has. The question is whether it
should have more.

## Considered options

1. **Keep it: the planner does not measure, the total is a lower bound.**
   - Good: a plan costs one listing walk, as ADR 0049 budgets it. Over SFTP or
     S3 a `stat` per new file is a round trip each; a first sync of 100 000
     new files would pay 100 000 of them before the dialog could open, only
     to paint one number.
   - Good: the copy itself learns every size when it runs, and its progress
     bar is exact.
   - Bad: on a small local sync the dialog says less than it could, and the
     reader cannot tell a cheap unknown from an expensive one.
2. **Stat every `Copy`/`Overwrite` step without a size while planning.**
   - Good: an exact total whenever the provider can give one.
   - Bad: it breaks C7b's rule for a number nothing acts on, and the cost
     grows with exactly the plans where a total matters least to wait for.
   - Bad: the sizes can be stale by the time the plan is applied (the spool's
     TTL is ten minutes), so the precision bought is for the dialog only.
3. **Stat only when the provider is local, or under a count threshold.**
   - Good: exact where it is cheap.
   - Bad: the planning path would branch on the kind of provider, and the
     threshold is a number to defend.

## Decision

Option 1. The plan does not measure the orphans it copies; `counts.bytes`
stays a lower bound and `unmeasured_steps` says how many steps it leaves out.
The landing report is not fixed in code.

## Consequences

- Good: planning cost stays one walk on every provider, and the dialog never
  shows a confident zero.
- Bad: the sentence *"plus N files whose size the provider did not give"*
  puts the unknown on the provider, and for a local tree that reads as false:
  the provider would have answered, norte did not ask. Rewording it (for
  example "whose size was not measured") is a Fluent change in both locales
  and is left to a separate commit.
- If someone wants exact totals later, option 3 is the one to revisit, and it
  belongs in the planner's caller, not in `norte-sync`.
