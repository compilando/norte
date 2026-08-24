---
description: Implement or review the hard parts of a VFS provider — the questions the conformance suite cannot ask for you
argument-hint: <scheme, for example sftp>
---
Work on the `$ARGUMENTS` provider.

`/new-provider` scaffolds the crate and wires the conformance suite. This is
the other half: **what the suite passing does not prove.** Everything below is
a mistake this repository actually shipped and then fixed.

1. **Names are bytes.** `VPath` and `Segment` carry bytes, never `String`. A
   `to_str().unwrap()` is grounds for rejecting the change. Decoding belongs to
   display, with an explicit lossy conversion, and the corpus in
   `norte-testkit` is what proves the round trip — add a fixture when the
   provider can lose a byte in a way nothing else covers.

2. **Declare `Capabilities` conservatively, and per LOCATION where it matters.**
   `capabilities()` answers for the mount; `capabilities_at()` answers for a
   path. Under one `file://` a USB stick mounted somewhere else folds case and
   `/home` does not, and #215 is what happens when the difference is ignored.
   Claiming a capability you do not have makes the engine fail hard where a
   fallback would have worked.

3. **Answer `node_id` honestly, or answer `None`.** It is what the core uses to
   tell "the same file by two names" from "two files", and a wrong identity is
   how a copy destroys its own source. `None` is a fine answer: the core has a
   conservative fallback for backends without identity.

4. **`stat` never follows the final link.** Callers that want the target ask for
   it. Getting this backwards makes a symlink look like the file it points at,
   in exactly the places that decide whether to overwrite something.

5. **Writes are staged and published, never written in place.** Cancelling must
   leave a clean destination or a marked `.norte-partial`, never an unmarked
   partial file. If the staging name is *predictable*, whatever sits at that
   name may have been put there by someone else: open it with `O_NOFOLLOW`,
   check the descriptor (regular file, `st_nlink == 1`, ours) and never trust
   an `lstat` of the path — it answers about what *was* there (#297, #298).

6. **Every long operation is a Task that checks its `CancellationToken`**, and
   each new one needs a clean-cancellation test. Blocking I/O goes through
   `spawn_blocking`; only `norte-vfs-local` may touch `std::fs` directly.

7. **A provider never knows about another provider.** Cross-provider work is the
   engine's job. `tests/dependency_boundary.rs` exists because this drifts.

8. **Say what you did NOT implement,** in the crate rustdoc and as an issue.
   A `todo!()` with no issue behind it is a promise nobody can find.

Then dispatch `encoding-auditor` (names, paths, archives) and `rust-reviewer`.
