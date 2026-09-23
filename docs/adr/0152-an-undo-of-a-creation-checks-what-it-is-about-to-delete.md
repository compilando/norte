# 0152 — An undo of a creation checks what it is about to delete

- Status: accepted and implemented
- Date: 2026-09-23
- Decision makers: Oscar González
- Protocol: 0.84.0 gains `ConflictKind::NotTheSameNode` (that version has not
  shipped). Bridge: unchanged. Journal rows gain a value in an existing
  free-form column (`reversal_ref` on `created`), which no peer reads.
- Related: #369, ADR 0151 (the destination that goes away), hard rule 4,
  ADR 0073 (the destination anchor — the same idea, one layer up)

## Context and problem statement

Undoing a `created` entry trashes whatever is at that path. It does not check
that what is there is what the entry created.

That is normally invisible, because the path usually still holds the same file.
#369 is where it stops being invisible, and it was found by a reviewer looking
at the fix for #362:

1. a copy writes through the descriptor of the destination root and the journal
   records the LOGICAL path (`created /destino/f0001`);
2. that folder is deleted mid-copy — in norte, a `rename` to the trash — so the
   bytes go to the trash while the journal keeps recording `/destino/...`;
3. since ADR 0151 the task FAILS, which is right, and those entries stay,
   describing files that are not at those paths;
4. the reader does the natural thing: recreates the folder and repeats the copy.
   Now those paths hold the GOOD copy;
5. undoing that failed batch trashes it. **A delete caused by an operation that
   did not happen.**

Reproduced in `crates/norte-core/tests/engine_undo_de_copia_fallida.rs`:
`undone: 33`, nothing blocked. With this ADR in, the same test reports
`undone: 0` and blocks on the first entry with `Conflict { Exists }`, and every
file the reader put back is still there.

Two things the reproduction taught, and both shaped this decision:

**Today's apparent safety is an accident.** The first version of that test
recreated ONE file and passed: the undo walks newest-first, hit the second path
that does not exist, answered `blocked(NotFound)` and did nothing. The file
survived because of *the order of a failure*, not because anything protected
it. With the copy repeated there is nothing left to stop it.

**And the class is wider than #369.** Replace a file by hand after a successful
copy, undo that copy, and the same thing happens: the replacement goes to the
trash. Nobody has reported it, and it is the same defect.

## Decision

**A `created` entry records the identity of what it created, and its undo
refuses when the identity does not match.**

- At record time, `Mutation::Created` carries the `NodeId` of what was just
  published, and the journal keeps it in `reversal_ref` — an existing free-form
  column that `created` entries left empty — as `<volume>:<index>`.
- The identity is asked **through the same handle the bytes went through**, not
  by path: `Dest::node_id()` routes to `ConfinedRoot::node_id(rel)` when there
  is a root and to `Provider::node_id(path)` when there is not. Asking by path
  is what the first attempt did, and it fails exactly where this matters — see
  below.
- At undo time, the `delete` reversal compares the recorded identity against
  what is at the path now. Different: it refuses with `NotTheSameNode`, exactly
  as it already refuses to remove a created directory that has children it did
  not put there (the `has_children` guard). The subtype is its own rather than
  the `Exists` that guard reuses, because on an undo "it already exists" is a
  tautology — of course it exists, it is what was about to be deleted — while
  "that is not yours" is the one thing the reader can act on.
- No identity recorded, a stored value that does not parse as one, or a
  provider that cannot report one: the undo behaves as it does today. The check
  can only ever make it refuse more, never less. The comparison is by `NodeId`
  and not by bytes precisely so that an unparseable value degrades instead of
  blocking that entry forever.

**Where identity is recorded**: every path that creates through `ops` or
`pack` — a copied file, a copied symlink, a directory made by a copy,
`fs.mkdir`, `fs.create`, `fs.write` (including the restore of the previous file
when a replace fails), packing, splitting and combining. **`sync.apply` is not
covered**: it journals through its own `StepJournal`, which has no place to put
an identity yet, so its `created` rows undo the way they did before. That is a
gap, not a decision, and it belongs with #368 — the other thing `sync.apply`
is missing from this family.

**It is not paid for when nobody will use it.** A copy asks the destination for
the identity once per created node, and against a remote destination that is a
round trip. `sync.apply` copies through a no-op observer and records separately,
so it would have paid that cost for a value it throws away; `MutationObserver`
gained `quiere_identidad()` so it can decline, defaulting to `true` because an
observer that stores and forgets to answer should lose performance, not
protection.

## Consequences

**For a reader**: an undo that would have deleted something the operation did
not create now stops and says so. The `has_children` guard already establishes
that an undo which cannot prove what it is touching leaves it alone; this is the
same rule for files.

**For #369 specifically**: the failed copy's entries survive, and every one of
them refuses, because the good re-copy has different inodes. The reader keeps
their files. The entries remain in the timeline describing a thing that did not
land — honest, if untidy.

**Cost**: one `node_id` per created file at record time. On the local provider
that is an `lstat` on a path that was just written, so it is warm in the dentry
cache; against a provider that cannot answer it is `None` and nothing is
recorded. It is on the hot path of a large copy, and that is the price: 100k
small files pay 100k extra stats. Measured against what it buys — an undo that
cannot delete a file it did not create — it is worth paying, and it is the same
trade the destination anchor (ADR 0073) already makes one layer up.

**What it does NOT fix**: the journal still claims a creation at a path where
nothing landed. A reader browsing the timeline sees an entry for a file that is
not there. Fixing *that* means voiding recorded mutations, which needs a
capability the observer does not have — see below.

**Which edits it catches, and that is not the intuitive answer.** An edit
written *in place* — `>>`, a database, an editor with `backupcopy=yes` — keeps
the inode, records no new `created`, and passes the check: identity says "mine"
and the undo proceeds. That is why the undo still goes through the trash and
still refuses to fall back to a permanent delete where there is none (#65).

But **an atomic save changes the inode**, and atomic saves are the common case:
vim with its default `backupcopy=auto` on most filesystems, VS Code, Emacs,
`sed -i`, `git checkout` — all write a temp file and rename it over the target.
So editing a copied file with an ordinary editor and then undoing the copy now
**stops the undo at that file**, and under strict LIFO everything older stays
un-undone until the reader deals with it. That is new behaviour, it is the safe
direction, and it has to be said out loud rather than discovered: a reader told
"identity does not see edits" would conclude the opposite of what happens.

Whether a node the reader replaced should *halt* the session or be a counted
skip that continues is a real question, and it is deliberately not answered
here: the module blocks on every drift today — a copied file the reader deleted
already halts an undo — so this change keeps that contract rather than carving
an exception into it for one case. Revisiting it is #371.

**Inode reuse is the remaining hole, and it is not closed.** `dev:ino` is not
generation-safe. Delete a created file and create another at the same path and
ext4/xfs will often hand out the freed inode, at which point the fingerprint
matches and the undo trashes the new file — the hand-replacement case this ADR
advertises. The window is narrow and the outcome is recoverable (it goes to the
trash, never a permanent delete), which is why `dev:ino` was enough to ship.
Closing it means widening the fingerprint — `reversal_ref` is free-form, so
`<volume>:<index>:<ctime>` would fit and `huella_a_nodo` is the single place
that would have to tolerate both shapes — but `ctime` also moves on a `chmod`,
which would make a legitimate undo-after-`set_mode` refuse. That trade needs
its own decision, not a line in this one.

## What the first attempt found, and why it is written here

The obvious implementation — ask `Provider::node_id(path)` right after
publishing the file — **does not work for the case this ADR exists for**, and
the reproduction said so immediately: of 32 entries, 28 had no identity.

The reason is the whole point of #369. The file was written through the
descriptor of a directory that has been renamed away, so the LOGICAL path no
longer resolves: `node_id(/destino/f0001)` answers `NotFound`, and the entries
that most need identity are exactly the ones that cannot get it by path.

So the identity comes from the same descriptor the bytes went through.
`ConfinedRoot` already exposed `root_id()` for the root itself and `stat(rel)`
for a child, but nothing that gave a child's *identity*; it gained
`node_id(rel)`, defaulting to `None` and implemented in the local provider as
one `fstatat(rootfd, rel, AT_SYMLINK_NOFOLLOW)`. Under a renamed-away
destination that call keeps working — the descriptor is what the kernel
follows, and it does not care what the directory is called now.

**This is what the reproduction is for.** The first test asserts that *every*
`created` entry of the failed copy carries an identity, not most of them: the
by-path sketch left 28 of 32 empty and it compiled, ran and looked like
progress. A fix for #369 that records identity for the four entries written
before the folder was deleted has fixed nothing.

A failure to read the identity does not fail the copy. It is an improvement to
the reversal, not a requirement of the transfer, and trading a working copy for
a smarter undo would be the wrong way round; the entry is then recorded with no
identity and the undo behaves as it did before.

## Alternatives considered

**Void the entries when the copy fails.** Append, per affected entry, a
compensation with `undoes_seq` so the undo treats the unit as already undone.
It is the tidier answer for #369 — the timeline would stop claiming a creation
that did not land — and it is what the issue proposed.

Rejected for now, on two grounds. It needs a new capability across
`MutationObserver`, the journal and the undo walker, because `on_mutation` has
no way to say "the mutation I recorded does not stand"; and it fixes only the
case we know about, leaving the hand-replaced file undeleted by luck. The
identity check fixes the class with no new trait surface. Voiding remains the
right follow-up for the timeline half, and it is easier to add on top of this
than instead of it.

**Rewrite the entries.** Impossible, and correctly so: the journal is
append-only with a hash chain (`verify_chain`).

**Leave it, because the undo trashes rather than deletes.** True and not
enough: the reader asked to undo one thing and got another thing moved out from
under them, with no message. What the trash buys is that it is recoverable, not
that it is acceptable.
