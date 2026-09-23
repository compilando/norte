# 0151 — A destination that goes away mid-copy

- Status: accepted
- Date: 2026-09-23
- Decision makers: Oscar González
- Protocol: 0.83.0 → 0.84.0 (`ConflictKind::DestinationGone`).
  Bridge: unchanged.
- Related: ADR 0054 and #164 (the confined destination root), ADR 0005 (the
  `Unknown` fallback that makes a new subtype additive), #367, #368, #369

## Context and problem statement

A user copied a large folder and, with the progress bar running, deleted the
destination folder. Reproduced, the outcome was worse than reported: the task
did not stall, it finished saying **`Completed`**, and the files were in the
trash.

The chain is three facts, each reasonable on its own:

1. A copy addresses everything through a descriptor of the destination root,
   opened once (#164). That is what makes the copy confined and what stops a
   symlink swapped in halfway from diverting it.
2. Deleting in norte means moving to the trash, which is a `rename`
   (`trash_fdo::do_rename`).
3. **A `rename` does not invalidate an open directory descriptor.** The
   directory keeps existing, same inode, somewhere else.

So the copy kept filling a directory that no longer existed at the path the
user named, and nothing noticed. The identity check that catches exactly this
(`same_root_or_fail`, comparing the inode behind the descriptor against the
inode at the path) already existed — and ran once, between creating the root
and opening it.

**And the deletion does not have to come from norte.** Another file manager, an
`rm` in a terminal, another machine over the same mount. That rules out the
answer that looks obvious at first — refusing to delete a directory that is a
live destination — as a *solution*: it cannot cover the cases that matter, so
it could only ever be a convenience on top.

## Decision

**Detect it while it happens, stop, and say which thing went missing.**

Three parts, and the third is the one that needed a protocol change:

1. **Re-check during the copy**, not only at the start: every 32 entries or
   every 5 seconds, whichever comes first. Two triggers because one is not
   enough — counting entries alone leaves a plan of ten 50 GB files unchecked
   for hours, and counting time alone is a timer nobody needs on a tree of
   small files.
2. **Always re-check before a copy may report success.** The periodic check by
   construction skips the window around the last file, and that window is the
   only one where being wrong closes the matter: a `Completed` is a task
   nobody looks at again.
3. **`ConflictKind::DestinationGone`**, a new subtype. Before 0.84.0 the case
   had no answer at all — the copy reported `Completed` — so the question was
   which *existing* answer to reuse, and both are misleading:
   - `Error::NotFound` does not say *what* was not found. In the middle of
     copying thousands of files it reads as "something in the source is
     missing" — the opposite of what happened. (This is what the first,
     unreleased version of the fix returned, which is why the wire docs briefly
     claimed it was the old behaviour. It never was: the old behaviour was
     `Completed`.)
   - `ConflictKind::EscapesRoot` says the path leads elsewhere, usually through
     a symlink, so writing there would escape what the caller named. It is an
     answer about the *shape of the path*, and it reads as a security problem.
     Here there is nothing wrong with the path: the folder left.

   The remedies differ too — recreate it and retry — which is the practical
   reason they could not share a subtype.

`same_root_or_fail` keeps its meaning and its `EscapesRoot` for the open-time
check; the new checks go through a sibling that reports `DestinationGone`.

## Consequences

**For a reader**: a copy whose destination disappears fails, quickly, with a
sentence naming the destination folder rather than a generic "not found". What
had already been written stays in the trashed directory — see #369 for the
journal consequence, which is not solved here.

**For a client one version behind**: the subtype degrades to
`ConflictKind::Unknown` (ADR 0005) and shows "conflict". It loses the sentence,
not the protection: the check runs in the daemon, so the task fails and the
files do not end up in a directory nobody can see.

**Cost**: two `spawn_blocking` round-trips per check, amortised over 32 entries
or 5 seconds — negligible next to opening, writing and closing each file. The
check is skipped entirely for providers that cannot confine (SFTP, object,
Windows), where `open_dest_root` returns `None` and this class of protection
never existed.

**What this does NOT cover, and it is deliberate that it is written down**:
copying a single file (#367), `sync.apply` (#368), and the journal entries a
failed copy leaves pointing at paths it did not write (#369). Each is the same
mechanism in another operation, and each needs its own failing test first —
the test for this one passed while measuring nothing in three successive
versions, which is the reason none of the three is being fixed blind.

## Alternatives considered

**Refuse to delete a live destination.** Does not cover deletion from outside
norte, which is most of the ways it happens. Worth having as a warning later;
it is not the fix.

**Check on every entry.** Two blocking round-trips per file is measurable on a
tree of a hundred thousand small files, and buys a bound nobody can perceive:
the difference between "a handful of files went to the wrong place" and "one
did" does not change what the reader has to do.

**Reuse `EscapesRoot`.** Cheapest, and it would have made the message worse
rather than better: a reader who deleted a folder would be told their path
escapes its confined root.
