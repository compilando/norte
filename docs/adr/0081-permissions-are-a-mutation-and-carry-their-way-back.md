# 0081 - Permissions are a mutation, and they carry their way back

- Status: accepted
- Date: 2026-08-29
- Decision makers: Oscar González
- Related: ADR 0004 (wire compatibility), ADR 0009 (long operations are Tasks),
  ADR 0039 (per-entry attributes), ADR 0080 (a digest is a read), issue #314.

## Context and problem statement

Permissions were the one category where all three reference managers TOUCH and
norte only LOOKED. The properties dialog showed the POSIX mode, the listing
could sort by it, and there was no way to change it — not because anyone
decided against it, but because the protocol had no method.

Verified before deciding, which is what the issue asked for: Krusader edits
permissions and ownership from its properties dialog (Edit → Properties,
`Alt+Return`), numeric permissions included; Total Commander has Files → Change
Attributes, which on Windows means the DOS attribute bits and the timestamps.
Two different things wearing one name.

## Decision

**Add `fs.set_mode`, protocol 0.60.0.** It sets the twelve POSIX permission
bits of N paths as a cancellable Task.

### POSIX permissions, and not "attributes"

"Attributes" is three questions in a trench coat: the permission bits, the
timestamps, and the owner. They are fixed in different places, they need
different privileges, and they have different reversals. `mtime` is easy to
promise and hard to undo honestly; changing the owner needs privileges norte
does not ask for and cannot get. Each will arrive with its own method and its
own policy op, or not at all — not as an optional field on this one, where a
caller could set three things and have two of them silently ignored.

The twelve bits are `chmod(2)`'s: `rwx` for owner, group and others, plus
setuid, setgid and sticky. The bits above say what CLASS the node is, and that
is not changed, it is what it is: a mode carrying them is REJECTED rather than
masked, because masking would quietly apply a permission nobody asked for.

### It is a mutation, with everything that drags along

Hard rule 4 asks for a journal entry with an undo path, and here that is easy
to honour and would be dishonest to skip: **the reversal is the previous mode**,
read immediately before the new one is written.

Read, not assumed. When it cannot be read — a provider that does not publish
`posix.mode`, a `stat` that failed — the change is still made and the entry is
recorded as `Irreversible` with that as its reason. The alternative would be to
store a mode nobody had and let a later undo apply it as if it were the one from
before, which is worse than saying "no way back".

The mode recorded as the RESULT is re-read after writing, not assumed:
`chmod(2)` silently clears setgid when the caller does not belong to the file's
group, and a journal claiming `2755` over a real `755` would be lying in the
dangerous direction.

The previous mode travels in the journal's `reversal_ref` column as decimal
ASCII. That column exists for "whatever the reversal needs"; adding a schema
column for twelve bits would cost more than saying here what is inside it.

A batch leaves **one entry per path**, not one per batch: a batch that stops
halfway has to leave undone exactly what it did, and one entry could not say
which.

### Its own policy op, on the way there AND on the way back

`PolicyOp::SetMode`, apart from `Create` and `Mkdir` for the same reason they
are apart from each other: letting something create files is not letting it
change who can read them. The gate runs over the WHOLE list before the first
write, so a batch cannot get halfway through a request that was denied.

**The undo asks for the same permission.** The first version of this let the
reversal fall into the undo's `delete` bucket, which had both bad faces: an
actor holding `delete` could undo a chmod the policy does not grant it, and one
holding `set-mode` could not undo its own — and under strict LIFO that blocks
the whole session behind it. It is the same bug this repository already fixed
once for `move`, and there is now a test that fails if the mapping regresses.

A `policy.toml` rule with no `op` is a wildcard, so **an existing "allow
everything under this prefix" rule starts granting permission changes on
upgrade.** That is in the changelog; deployments that scope by op are
unaffected.

### A provider that has no permissions says so

`CapabilityFlags::POSIX_MODE` is declared by the providers that can both read
and write them: local on unix, and SFTP. A `.zip` has nothing to change and an
object bucket has no mode, so they answer `Unsupported` and change nothing —
and the flag is what lets a frontend dim the gesture instead of offering it to
fail.

Windows is excluded deliberately: `set_permissions` there knows only the
read-only bit, and announcing a POSIX mode would be promising something the
call does not do.

### A symlink is not changed, and that is a containment decision

`chmod(2)` FOLLOWS the link. The `stat` that reads the reversal does not — it
is `lstat`, by the provider trait's own contract. Those two facts together are
a trap with two floors: the mode stored as "the one before" would be the LINK's
(`0777`, always, on Linux), so an undo would leave the TARGET world-readable;
and the target can live outside the subtree somebody approved, which makes a
chmod through a link a write that leaves its root — the confused-deputy case
`SECURITY.md` names.

So `fs.set_mode` skips symlinks: the path is counted as not done and the
frontend says so. Nothing legitimate is lost, because permissions on a symlink
mean nothing on Linux to begin with. What this does NOT close is a symlinked
INTERMEDIATE component; that is the general problem `CONFINED_WRITES` and the
`openat` walk exist for, and this method does not use them yet.

### The mode travels in the approval — and setuid stays out of an agent's reach

The approval request carried the op and the paths, and for every other op that
IS the decision: approving "copy these twelve" is approving copying those
twelve. `set-mode` is the first op where two requests with the SAME op and the
SAME paths mean opposite things — `0600` and `4777` — so the human was not
consenting to what they thought. `ApprovalDetail` (protocol 0.61.0) carries the
mode, both frontends show it, and the argument is exactly the one
`paths_total` already makes for the count.

setuid and setgid still cannot be set by an agent. Not because those bits are
the danger — `chmod 0777` on `~/.ssh` does far more harm and carries none of
them — but because the question that would authorise them is only asked when a
rule says `ask`: a rule that plainly `allow`s `set-mode` never shows a human
anything. So the human sets them, from a dialog that does show them, and an
agent does not. Sticky is not in that set: it grants nobody's privilege.

There is a case this method does not defend against and that must be written
down rather than discovered: **a daemon running as root**. Then `set-mode` on a
root-owned binary is a local root escalation, and no per-bit rule fixes that.
norte's threat model says same-uid and unprivileged; this is the first method
whose blast radius changes qualitatively if that stops being true.

### Two windows that stay open, named

The mode is read and then written, so someone else can change it in between:
the reversal is then stale, restoring a mode that was real a moment earlier.
Every undo in this repository carries that staleness and accepts it.

And a cancelled task can still have changed permissions on the paths it got
through before the cancel — the same property `fs.copy` has, said here because
for permissions the leftover is a policy change nobody is watching.

### No recursion in this pass

Applying to a tree is a different question — what mode does a directory get
when the one you typed is a file's, and what happens to what fails halfway —
and answering it halfway would be worse than not answering it. The paths sent
are the paths changed.

## Consequences

- Protocol **0.60.0**: `FS_SET_MODE`, `FsSetModeParams`, `TaskKind::SetMode`,
  `CapabilityFlags::POSIX_MODE`. Additive; a 0.59 client does not call the
  method and keeps the read-only surface it had, and unknown capability names
  are ignored on parse (ADR 0004), so the flag is invisible to it.
- `Provider::set_mode` defaults to `Unsupported`, so every existing provider
  keeps compiling and answers honestly.
- `MemProvider` gained real per-path modes and publishes `posix.mode`, which is
  what makes the Task and its undo testable without touching a disk.
- The terminal gets `pane.chmod`: an octal field prefilled with the mode of the
  entry under the cursor, over the usual operand, with the COUNT in the title —
  typing a mode believing it applies to one entry and having it apply to fifty
  is the mistake the dialog exists to make hard. The window is classified as
  deferred against #314.
- Far's `Ctrl+A` finally binds what its own source calls it: "Set file
  attributes". It used to land on `pane.properties` because looking was all
  norte could do.
- The i18n args of this repository are STRINGS, so Fluent plural selectors
  never match; the singular gets its own message id and the code picks. Written
  down here because the next plural will meet the same wall.
