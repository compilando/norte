# 0072 - The approved directory is the root a leaf hangs from

- Status: accepted (partial — #219 narrowed, not closed; see Consequences)
- Date: 2026-08-23
- Decision makers: Oscar González
- Related: ADR 0054 (confined writes), issues #164, #218, #219.

## Context and problem statement

Phase W5-B closed #164 for **recursive** operations: `ops::copy_tree` and the
sync executor open a `ConfinedRoot` on their destination once, and every step
afterwards addresses a relative path against a directory descriptor. A symlink
planted in an intermediate component cannot redirect them.

`ops::copy_task` and `ops::move_by_copy` were deliberately left out for a
single file or a single symlink, on the reasoning that *"a lone leaf hangs
from no approved root and has no window"*.

**The security review showed that reasoning is half right and the wrong half.**
A single-file copy has the same window:

- the policy gate resolves `to` by path;
- `resolve_collision` stats it by path;
- `LocalProvider::write` composes the staging path, creates it, and later
  renames it — three more resolutions of `dest/sub`;
- and `copy_file_retrying`'s three backoffs (100/200/400 ms) widen all of it.

Plant `dest/sub -> /etc` and the file lands in `/etc` with the daemon's
credentials. That is #164, unmodified, **for the single most common operation
in the product**.

The destructive half (#218) was worse in a quieter way. Under
`CollisionPolicy::Overwrite` or `Newer`, `resolve_collision` lstats the
destination by path and `overwrite_existing` unlinks by path — both **before**
the confined write is ever opened. With the same hostile component, the unlink
takes a file in the attacker's tree, the confined write then refuses, and the
net result is a file destroyed outside the approved root, nothing written in
its place, and a journal entry naming a path that was not touched.

## Decision drivers

- The recursive path already pays for a `ConfinedRoot`; the leaf path is the
  one users hit constantly.
- Whatever root is chosen must exist for **both** callers: an agent's
  `fs.copy` and a human's F5.
- Refusing to copy to destinations that cannot confine is not on the table
  (ADR 0054 settled that).

## Considered options

**A. The policy scope as the root, with `rel` being everything below it.**
What issue #219 proposed. Rejected: **a human copy has no policy scope.**
Scopes exist only for agent sessions, so for the more common caller there
would be nothing to open. It is also the largest change — the scope has to
reach `ops`, and every provider that can confine has to open its scope root
once per session rather than once per task.

**B. The destination DIRECTORY (`to.parent()`) as the root, `rel` being the
final name.** Chosen, with the identity check applied only when that directory
is not itself a link — see Consequences for what that costs.

**C. Refuse `Overwrite`/`Newer` on a confined destination until there is a
confined remove.** Rejected as the only fix: it turns a security gap into a
missing feature. Kept as the *fallback* — a root that cannot remove makes the
policy refuse rather than silently deleting by path.

## Decision

**The root of a leaf transfer is its destination directory**, opened once per
task and verified with the same `same_root_or_fail` identity check the
recursive path uses.

That directory has exactly the standing `copy_tree`'s `to` has: it is what the
panel showed, what `pedir_transferencia` composes `to` from, and what the
confirmation dialog names in its own field. The human approved it.

And `ConfinedRoot` grows a `remove`, implemented in `norte-vfs-local` with
`unlinkat` (no `AT_REMOVEDIR` — replacing a directory with a leaf is
`TypeMismatch`, which is an answer, not a policy). `resolve_collision`'s stat,
`ensure_dir`'s pre-stat, the merge disambiguation and the post-transient
disambiguation all move onto the same handle.

## Consequences

**What this closes, and it is narrower than #219 asked for.**

The *swap* — #164's actual model, where the destination is observed as a real
directory and replaced with a link before the bytes are written. Once the root
is open, staging creation and the publish rename both go through the
descriptor, so `dest/sub` is resolved **once** instead of three times plus a
retry each, and a substitution after that point redirects nothing. When the
destination directory is a real directory, the identity check also catches a
swap that lands between our own observation and our own open.

And #218 in full for anything that has a root: the `stat` that decides a
collision and the `remove` that executes it both go through the descriptor, so
neither can reach outside the approved directory. That half is demonstrated at
the provider level, where the descriptor is observable.

**What this does NOT close, and the first draft of this record claimed it did.**

A symlink **already in place** when the core first looks. From inside the core
that link and a legitimate `~/copias -> /mnt/disco/copias` are the same thing:
both resolve elsewhere. The first version of this change rejected both — it
compared the opened root's identity against an `lstat` of the destination
directory, which never matches for a link — and that would have broken copying
to `/tmp`, `/var` and `/etc` on macOS, to `/bin` and `/lib` on a usrmerge
Linux, and to any `~/Descargas -> /mnt/datos/Descargas`. The security review
caught it before it shipped.

So on a linked destination the root is still opened (the once-resolution is
worth having) and only the identity check is skipped, because over a link it
could not say anything true.

**Closing the other half needs the wire.** The one thing that distinguishes a
legitimate link from a planted one is the identity observed **when the human
approved** — the listing they were looking at. That identity would have to
travel with the request, which `fs.copy` has no field for. That is the
follow-up, and it is why #219 stays open, narrowed.

**Other paths that still resolve by path**, listed rather than implied, and now
also in the `ConfinedRoot` rustdoc: the source deletion of a move-by-copy,
`RenameAuto`'s candidate probing (up to a thousand stats), and `copy_native`
(unreachable today — only `norte-vfs-object` declares `SERVER_COPY` and it has
no `open_root` — but it bypasses the root entirely the day some provider has
both).

The sync executor's `destroy_leaf` and `destroy_tree` were on that list and are
not any more (#296): `ConfinedRoot` grew `rmdir` — the twin of `mkdir`, separate
from `remove` for the same reason `unlinkat` has `AT_REMOVEDIR` — so a
`Mirror`'s post-order walk names the class of what it destroys instead of
letting the path decide. Deciding the class from the walk's older snapshot is
fail-safe by construction: `unlinkat(0)` cannot remove a directory and
`unlinkat(AT_REMOVEDIR)` cannot remove anything else, so a substitution between
the walk and the deletion produces a refusal, never a destruction of the wrong
kind. `Provider::remove` decided in the moment and therefore *applied* the
substitution.

**Resume for a single file was off, and is on again** (#297). Confining a leaf
made `Dest::resumes()` false, because a confined staging name was ephemeral per
sink and nothing could find it again — so `ResumePolicy::On` became a no-op for
exactly the case where resume matters most, one large file over a flaky link.
`LocalConfinedRoot` now opens the *stable* staging name, the one ADR 0012
already defined for the by-path route, and `ConfinedRoot::resumes()` says so.

That has three consequences worth stating rather than discovering:

- **The stable name is predictable**, since it is a hash of the final name. So
  the reopen is not blind: after `openat` (with `O_NOFOLLOW` *and*
  `O_NONBLOCK`, because a planted FIFO would otherwise hang a blocking-pool
  thread forever with no way to cancel it) the descriptor is `fstat`ed and
  refused unless it is a regular file, with one link, owned by us. Without
  that, someone with write access to the destination directory could plant the
  staging and have their bytes published under the legitimate name — with their
  owner, their permissions, and their write descriptor still open on it.
  `partial_digest` gets the same treatment for the same reason: verifying the
  prefix of a file that is not the one being continued verifies nothing.
- **Two concurrent writers to the same destination now share a staging.** That
  is inherent to a stable name and is exactly why the ephemeral one carries pid
  and sequence. The by-path route has had this since ADR 0012; the confined one
  was immune until now. Interleaved appends can publish a mixture of the right
  size, which the post-transient disambiguation would accept.
- **A cancelled confined copy now leaves a partial behind.** `keep` keeps and
  `Drop` keeps, which is the point — but there is no automatic sweeper:
  `gc_partials` is single-directory and only `norte gc <dir>` calls it. The
  confined destination is no longer left clean on cancellation. Same contract
  the by-path route already had, extended, and said out loud here.

**And the by-path route had the same hole, older and barer** (#298). The three
checks above were written for the confined reopen, but the predictable name is
not a property of confinement — it comes from ADR 0012, and the provider's
`open_resumable` had been reopening it with `append(true).create(true)` ever
since: following symlinks, never asking what it had opened, and creating with
`0o666`. The same set applies there and now does, on the descriptor rather than
on the path: `O_NOFOLLOW | O_NONBLOCK`, mode `0o600`, then regular file,
`st_nlink == 1`, ours — and `partial_digest` alongside it. What the by-path
route still does not get is confinement of the intermediate components, which
is #219's remainder and a different shape of change; this is the leaf, and the
leaf is where the planted staging lives.

One consequence is stated in #299 rather than decided here: a staging created
`0o600` is *published* `0o600`, so a resumed copy leaves a private file where a
plain copy leaves `0o644`. Matching them means either reading the umask (which
`umask(2)` will only tell you by changing it) or preserving the source mode,
which norte does not do today — a product decision, not a security fix.

**Cost.** Three extra syscalls per leaf transfer (`open`, `fstat`, two
`node_id`s), against the five to ten each leaf already pays. `open_leaf_root`
lives **inside** the two leaf arms and not before the `match`, so a recursive
copy neither pays it nor inherits its errors — its own root comes from
`copy_tree`, over a `to` that `ensure_dir` just created, where "the root I
opened is the one I made" is a real invariant rather than a new policy.

**A root that cannot remove.** `ConfinedRoot::remove` defaults to
`Unsupported`, and callers must treat it as a refusal of the *policy*, never as
a reason to fall back to removing by path — that would reopen the hole in the
one place the operation destroys, and do it silently. The retry loop is
**shared** with the by-path route (`bucle_de_borrado`): a confined `unlinkat`
that hits a transient and then answers `ENOENT` counts as done, exactly as
`remove_retrying` has since #186.

**Non-goals.** `rename_with_policy` stays unconfined and says so: an in-place
rename opens no root, and the `renameat` that follows could not use one
either. What that path has instead is `from_id`, which `rename_retrying`
checks.
