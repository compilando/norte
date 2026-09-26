# 0158 — A confined root on Windows opens one name at a time, and identity is a `NodeId`

- Status: accepted
- Date: 2026-09-26
- Decision makers: Oscar González
- Related: ADR 0054 (confined writes), ADR 0057 (plugin `location`), ADR
  0100 (hook locations and ceilings), ADR 0157 (Windows builder); #238,
  #240, #241

## Context

The plugin `location` capability hands a guest budgeted, read-only access
under one directory (`norte_vfs_local::ConfinedRoot`). On unix the kernel
confines it: `openat2(RESOLVE_BENEATH)`, `O_NOFOLLOW` on the last component,
and a veto on protected roots compared by `(dev, ino)`. None of that existed
on Windows, so `norte-core` did not compile there.

Win32 has no `openat`. `CreateFileW` resolves a whole path, with DOS device
names, `..` and reparse points along the way, so composing `root\rel` and
checking it afterwards is the TOCTOU #164 closed on unix. Std exposes no
stable file identity on Windows either, and inventing an inode from a
`FileId` would give a guest numbers that match nothing Git for Windows
records.

## Decision

1. **`location.rs` is platform-neutral** (budget, lexical `..`, veto by
   identity); primitives live in `location/unix.rs` (moved unchanged) and
   `location/windows.rs`. `unsafe` stays in `norte-vfs-local` (rule 5).
2. **Windows opens each component with `NtCreateFile` relative to its
   parent's handle**, `FILE_OPEN_REPARSE_POINT`, sharing everything. No path
   below the root reaches Win32.
   - A name-surrogate reparse point (symlink, junction) is **never
     crossed**, even one that stays inside. Unix follows a relative link
     that does not escape; refusing is the same verdict on the safe side.
   - Any other reparse point (`OneDrive`, dedup, WOF) redirects no name. It
     is reopened through its filter and accepted only if its `FileId`
     equals the node looked at.
   - A directory reopened as a fresh file object (an empty name, so that
     concurrent `list`s do not share a cursor) must still be the same node:
     an empty name is resolved again, reparse data included.
   - A component containing `\` or `:` is refused before the kernel sees
     it (separator, alternate data stream).
3. **Identity is `norte_vfs::NodeId`.** Unix: `(dev, ino)`. Windows:
   `FILE_ID_INFO`, falling back to `BY_HANDLE_FILE_INFORMATION`; an index of
   0 is no identity and fails. `ConfinedRoot::identify` is the look that
   `open_verified` later compares against. A protected root that exists but
   cannot be opened or identified refuses the whole root.
4. **`LocationMeta` on Windows reports what Git for Windows records**:
   `ino = dev = 0`, `ctime` = creation time, `mode` synthesised from the
   attributes.
5. **The home ceiling on Windows is `USERPROFILE`, compared by node**, and a
   drive's root is a ceiling. A project marker (`.git`) counts only when its
   directory's FINAL path is inside the profile's final path: any
   authenticated user may create `C:\.git`, and a junction inside the
   profile can lead elsewhere. Reading ACLs is not attempted.

## Consequences

- `norte-core` compiles for `x86_64-pc-windows-msvc`; the location tests,
  junctions included, pass on NTFS in the ADR 0157 VM.
- On Windows a repository reached through a junction gets no git column.
  Accepted: that is the direction confinement fails.
- Git's change detection loses `ino`/`dev` on Windows, exactly as Git for
  Windows itself does.
- UNC panes are not served (`mint_for` refuses an authority); a share's
  confinement would be only as good as its server.
- Not covered: a root that sits INSIDE a protected directory under another
  spelling (`NORTE~1\sub`) is compared by `VPath` bytes before the identity
  veto applies. Pre-existing on unix, easier on Windows.
- The daemon listener, the TUI subshell (ConPTY) and the ssh agent's named
  pipe are still unix-only; they are the next Windows milestones.
