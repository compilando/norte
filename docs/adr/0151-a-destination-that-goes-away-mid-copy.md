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

**What this did not cover when it was written, and what happened to it**:
copying a single file (#367), `sync.apply` (#368), and the journal entries a
failed copy leaves pointing at paths it did not write (#369). Each was the same
mechanism in another operation, and each needed its own failing test first —
the test for this one passed while measuring nothing in three successive
versions, which is why none of the three was fixed blind.

All three are done now. #367 and #368 apply this same decision unchanged: one
file checks its destination directory before reporting success, and a sync
checks its root on open, on the same cadence while it runs, and once before
returning. #369 went further and needed its own decision: ADR 0152.

A fourth case turned up while reviewing those two, and it is the worst of the
family: **a cross-filesystem move**, which copies the leaf and then deletes the
source. Trash the destination mid-move and the outcome was bytes in the trash,
source destroyed, task `Completed`. It gets the same check, but placed
*before the deletion* rather than before the success message — the only spot in
this family where the check gates an irreversible effect instead of the wording
of an outcome.

**And one correction to this ADR's own reasoning.** The first attempt at #367
copied `open_leaf_root`'s symlink exemption: skip the check when the
destination directory is a link, because the open root's identity is the node
the link points to while the path's is the link, so they never match. That is
true of the question `same_root_or_fail` asks — "was a link planted where a
directory was" — and false of the question *this* check asks, which is about
the target. The exemption also would have left #367 live in the case where it
is most likely (a leaf's destination directory is one the human chose and may
well be a link) and, applied to `sync.apply`, would have rejected every
symlinked destination root outright. The fix is to resolve **following links**:
an intact `~/copias -> /mnt/disco/copias` matches, a link left dangling reports
that nothing is there, and a repointed link reports a different node. Three
correct answers instead of one correct-by-accident and two lost.

Getting the single-file test to fail first needed a source that could be
paused, since a tree gets its window free from having four thousand entries and
one file would have bought it with either a race or a huge fixture. The test
provider serves the first chunk, says so, and waits for the test's permission —
a fact and an order, not a timeout.

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
