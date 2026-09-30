# 0160 — Confined writes on Windows open, publish and delete by handle, and never cross a link

- Status: proposed
- Date: 2026-09-30
- Decision makers: Oscar González
- Related: ADR 0054 (confined writes), ADR 0158 (confined `location` on
  Windows), ADR 0157 (Windows builder); #164, #217, #220, #298

## Context

ADR 0054 closed #164 on unix: a recursive copy, move or delete into a local
destination opens the root once and addresses relative segments, so a link
planted in an INTERMEDIATE component cannot redirect the write outside. On
Windows `open_root` answered `Unsupported`, every such operation degraded to
the path route with a warning, and `CONFINED_WRITES` was not declared.

Win32 has no `openat`, and `CreateFileW` resolves whole paths — DOS devices,
`..`, reparse points — so composing `root\rel` and checking it afterwards is
the TOCTOU #164 is about. ADR 0158 already solved the READ side for the
plugin `location` capability by opening each component with `NtCreateFile`
relative to its parent's handle. Writes add three things reads do not have:
creating a name, publishing a staging under its final name, and deleting.

Two facts measured in the ADR 0157 VM shaped the answer:
`SetFileInformationByHandle(FileRenameInfo)` rejects a `RootDirectory` with
`ERROR_INVALID_PARAMETER`, and Windows refuses to rename a directory while a
file inside it is open without `FILE_SHARE_DELETE`.

## Options considered

### A. Handle-relative for every step, never crossing a link

Open every component relative to its parent's handle with
`FILE_OPEN_REPARSE_POINT`; refuse any name-surrogate reparse point
(junction, symlink) with `EscapesRoot`, even one that stays inside. Create
the staging with `FILE_CREATE` in the resolved directory, publish by
renaming the staging's OWN handle into the directory's handle with
`NtSetInformationFile(FileRenameInformation)` and no replace, and delete by
handle with a POSIX-semantics disposition.

- Good: no name below the root is ever resolved by Win32; publish and
  discard look nothing up by name, which is stronger than unix's
  `renameat`/`unlinkat` on a directory descriptor.
- Good: reuses ADR 0158's primitive, so reading and writing give the same
  verdicts (`win_nt` is shared).
- Bad: a tree whose destination contains a junction to another place
  INSIDE it cannot be written through that junction, which unix allows.

### B. Handle-relative, following a link that stays inside

Read the reparse target, resolve it against the root and follow it when it
does not leave, as unix's component walk does.

- Good: same verdicts as unix on every case.
- Bad: resolving a reparse target means reimplementing the NT path parser
  (`\??\`, volume GUIDs, mount points, relative symlinks) on the side that
  must not get it wrong; a mistake there is the hole this closes.

### C. Keep degrading to the path route on Windows

- Good: nothing to write.
- Bad: #164 stays open on the platform where planting a junction needs no
  privilege at all.

## Decision

Option A. A link in a component is never crossed on Windows; that is the
same verdict unix gives for an escaping link, applied on the safe side to
the non-escaping one too, and it is what ADR 0158 already does for reads.
`CONFINED_WRITES` is declared on Windows. A resumed staging is refused if it
is a reparse point, not a plain file, has another hard link, or is owned by
anyone but the process token's `TokenOwner` (an elevated administrator's own
files belong to BUILTIN\Administrators): in a shared folder another user can
plant the predictable name, and as its owner keep reading and rewriting what
we publish. Deleting prefers POSIX semantics and falls back to the classic
disposition where the filesystem has none (FAT, exFAT, many SMB servers). A
volume that gives no file identity answers `None`, not an error. `symlink`
under a confined root answers `Unsupported`, as the provider does there
(#220).

## Consequences

- Positive: recursive copy, move and delete into a local Windows
  destination are confined; the degradation warning no longer fires there.
- Positive: the publish-time swap unix defends against cannot even be set
  up on Windows while the staging is open; the test pins both branches.
- Negative: a destination that relies on an internal junction is refused
  with `EscapesRoot` where unix would follow. It is visible, not silent.
- Negative: `tests/confined.rs` stays unix-only (it builds unix symlinks);
  the Windows contract lives in `tests/confined_windows.rs` with junctions.
  A directory symlink (`mklink /D`) takes the same code path but needs a
  privilege the test VM lacks, so it is not exercised.
- Names are literal at the NT layer: `foo.`, `CON` or a lone surrogate are
  created as such (as the old `\\?\` path route did), and an 8.3 alias
  (`DOCUME~1`) resolves to the long name it abbreviates. None of that leaves
  the directory; segments come from listings, which give long names.
- Not covered: a root that sits inside a protected directory under another
  spelling (ADR 0158's note applies); the core's walk on Windows, whose
  test suite does not build there yet; the FAT/SMB delete fallback, which
  the NTFS-only VM does not exercise; a folder with per-directory case
  sensitivity.
