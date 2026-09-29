# Windows confined writes (#217)

**Goal.** `LocalProvider::open_root` answers on Windows, so a recursive copy,
move or delete into a local destination cannot be redirected outside it by a
link planted in an intermediate component — the hole #164 closed on unix. Then
`CONFINED_WRITES` is declared on Windows and the degradation warning
(`ops::open_dest_root`, `norte_frontend::confine::warning`) stops firing there.

**Contract.** `crates/norte-vfs-local/tests/confined.rs` (unix today) is the
specification. Every test that is not about a unix-only mechanism
(`openat2` vs the walk, FIFOs, `0o600`) must run on Windows unchanged or with a
documented, safer verdict.

**Primitive.** ADR 0158 already opens one name at a time with `NtCreateFile`
relative to the parent handle (`location/windows.rs::nt_open`, `open_child`).
Writes need the same thing with a disposition, plus rename and delete relative
to a handle.

**Decision for the ADR (0160).** A name-surrogate reparse point (junction,
symlink) in a component is **never crossed**, even a relative one that stays
inside. Unix follows that case (`a_relative_symlink_that_does_not_escape_the_root_is_followed`).
Resolving a reparse target without leaving the handle discipline would mean
reimplementing the NT path parser; refusing is the same verdict on the safe
side, and it is what ADR 0158 already does for reads. The unix test becomes
`#[cfg(unix)]` with that reason, and a Windows twin asserts `EscapesRoot`.

## Tasks

Loop per task: edit on the host, `sync.sh` + `guest.sh` in the ADR 0157 VM
(never with a heavy host build running: OOM), `cargo test -p norte-vfs-local`
there. Linux `just t norte-vfs-local` must stay green: nothing here is compiled
on unix.

1. **Shared NT layer.** Move `nt_open` into `src/win_nt.rs` with a
   `disposition` parameter (`FILE_OPEN`, `FILE_CREATE`) and a `share` one.
   `location/windows.rs` keeps its behaviour; its tests are the check.
   Add `rename_relative(file, dir, name, replace: bool)` over
   `SetFileInformationByHandle(FileRenameInfoEx)` with `RootDirectory = dir`
   and `delete_on_close(file)` over `FileDispositionInfoEx`
   (`FILE_DISPOSITION_FLAG_DELETE | POSIX_SEMANTICS`).
2. **`WinConfinedRoot`: open, walk, identity.** `open_root` opens the root
   directory handle (`FILE_FLAG_BACKUP_SEMANTICS`, no `FILE_OPEN_REPARSE_POINT`
   on the ROOT itself: the root is the user's choice). `walk(parents)` uses
   `open_child` per component; a surrogate → `Conflict { EscapesRoot }`.
   `root_id`, `node_id` via `FILE_ID_INFO` (ADR 0158's `raw_id`), `stat`.
   Tests: `the_roots_identity_gives_away_a_path_replaced_by_a_link`,
   `identity_via_the_descriptor_*`.
3. **`mkdir`, `write`, publish.** `mkdir`: `FILE_CREATE | FILE_DIRECTORY_FILE`.
   `write`: ephemeral staging created `FILE_CREATE` in the parent handle; the
   sink publishes with `rename_relative(.., replace: false)`; collision →
   `Conflict { Exists }`; abort → `delete_on_close`. Tests:
   `an_intermediate_symlink_does_not_redirect_the_write_outside_the_root`,
   `…_a_mkdir_either`, `a_normal_nested_write_works`,
   `publication_is_not_diverted_by_a_symlink_slipped_in_midway`,
   `an_abort_leaves_no_unmarked_partial`,
   `an_occupied_destination_is_a_conflict_when_opening_the_sink`.
4. **Resume.** `open_resumable` opens or creates the STABLE staging relative
   to the parent, refuses a reparse point with its name (#298's hole on
   Windows, noted in `open_stable_staging`), opens for write (not append:
   #222) and seeks to the end. `partial_digest` reads its prefix the same way.
   Tests: the resume and digest block (minus the FIFO one).
5. **`remove`, `rmdir`.** Open the victim relative with `DELETE`,
   `FILE_OPEN_REPARSE_POINT` (a link is removed as the link, #220), then
   `delete_on_close`. `remove` refuses a directory, `rmdir` a non-empty one
   (`STATUS_DIRECTORY_NOT_EMPTY` → `Conflict { TypeMismatch }` as on unix).
   Tests: the removal and rmdir block.
6. **`symlink`: `Unsupported`** (no `SYMLINKS` on Windows, #220). Then flip
   `CONFINED_WRITES` to `cfg!(any(unix, windows))` in
   `capabilities_at`, make `tests/confined.rs` build on Windows with its
   unix-only tests gated one by one, each with its reason.
7. **ADR 0160, CHANGELOG, `security-reviewer` + `encoding-auditor`** before
   the commit that flips the flag. The flag is the promise; it goes last.

## Risks to check early (task 1)

- `FileRenameInfoEx` needs Windows 10 1709+; plain `FileRenameInfo` with
  `RootDirectory` is older and enough for no-replace. Measure which one
  honours `RootDirectory` for a handle opened relative.
- `OBJ_CASE_INSENSITIVE`: a destination folder marked case-sensitive
  (`fsutil file setCaseSensitiveInfo`) decides for itself; the collision check
  must use what the kernel answered (`STATUS_OBJECT_NAME_COLLISION`), not a
  comparison of ours.
- The walk holds one handle per component while publishing; a deep tree is
  one handle per level, released as the walk descends.
