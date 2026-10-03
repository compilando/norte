# 0165 — A local copy fills its staging in the kernel

- Status: accepted
- Date: 2026-10-03
- Decision makers: Oscar González
- Related: ADR 0012 (resume), ADR 0016 (`copy_native`), ADR 0147 (pause),
  #393

## Context

Every local→local copy streams the file through user space: 256 KiB reads,
one `Bytes` copy per chunk, a channel, and a `spawn_blocking` per write.
`LocalProvider` implements no `copy_native` and does not declare
`SERVER_COPY`, so the kernel's own copies are never used: a 20 GB image on
btrfs or XFS, which a reflink copies in milliseconds and without taking
space, is read and written in full; on ext4 the in-kernel
`copy_file_range` is lost too.

`copy_native` cannot simply be implemented. It goes by path, from source to
destination, and the local destination is written through a confined
staging (ADR 0160 and its Unix sibling): opened `O_EXCL` under a directory
descriptor, published by a no-replace rename, removed on cancel. A path
copy would bypass all three.

## Options

1. **The sink fills itself from an open source file.** `Provider` gains
   `open_local(path) -> Option<File>` (a provider that can hand the source
   as a local descriptor) and `ByteSink` gains `fill_from(file, progress)
   -> Option<Result<u64>>`. The copy engine, when both say yes and nothing
   was resumed, lets the sink copy fd to fd; staging, publication, cancel
   and journal stay exactly as they are.
   - Good: the confined staging is kept; the fast path is one branch in
     `copy_file`, and every other provider keeps streaming.
   - Bad: two more trait methods; `std::fs::File` crosses the `norte-vfs`
     boundary (only as an opaque handle).
2. **`copy_native` for local, re-implementing the confinement in it.**
   - Good: no new trait surface.
   - Bad: a second copy of the staging, publish and cancel logic, which
     is exactly what has broken before (#217, #297).

## Decision

Option 1. The local sinks try `FICLONE` first (whole-file reflink), then
`copy_file_range` in 16 MiB blocks, reporting progress and checking
cancellation between blocks through the callback. They answer "cannot",
and the engine streams as before, when:

- the source says it is EMPTY: `/proc`, `/sys` and some FUSE files say 0
  and have content, and the kernel copies only what the size says;
- there is no reflink and the source is SPARSE: `copy_file_range` on ext4
  or tmpfs writes holes as zeros, which streaming avoids (#222);
- the kernel refuses before writing anything (`EXDEV`, `EOPNOTSUPP`,
  `ENOSYS`, `EINVAL`, `EPERM` under seccomp, or `EBADF` for an `O_APPEND`
  staging) or copies nothing at all.

A refusal after some bytes is an error, not a fallback: the staging holds
them. Only on Linux; elsewhere the default "cannot" stands.

The trait carries `std::fs::File`: an opaque, local handle and no I/O in
`norte-vfs` itself, but it ties this path to providers that have a real
descriptor, which is the point.

## Consequences

- Good: copies on CoW filesystems are instant and share blocks; on ext4
  the bytes never leave the kernel.
- Good: nothing about staging, publication, cancellation or journaling
  changes, and neither does the wire.
- Bad: a reflinked copy shares blocks with its source until written —
  that is the point, and also why `du` stops adding up.
- Bad: pause (ADR 0147) is honoured between files, not between 64 MiB
  blocks, as the pause gate's own doc already says for `copy_file_range`.
- Bad: a copy with resume on (`ResumePolicy::On`, the CLI's option) always
  streams: its stable staging is opened `O_APPEND`, which the kernel copy
  refuses. Resume is off by default.
- Bad: a local source going to a remote destination is opened twice (once
  to offer it to a sink that then declines, once to read it).
