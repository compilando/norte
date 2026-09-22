# 0145 — Owner and group by name

- Status: accepted
- Date: 2026-09-22
- Decision makers: Oscar González
- Protocol: unchanged (two new attribute ids, which ADR 0039 already allows).
  Bridge: unchanged.
- Related: ADR 0039 (provider attributes), ADR 0128 (the permissions column),
  ADR 0144 (attribute columns sort), `docs/batida-de-funcionalidades-2026-09-20.md`
  §2.3 and item 5

## Context and problem statement

The local provider has offered `posix.uid` and `posix.gid` since #108, as
numbers. A reader coming from `ls -l`, Midnight Commander or Krusader expects
names. Resolving a uid to a name is not a `stat` field: it is a question to
the C library, which asks NSS, which may ask LDAP or SSSD over the network.

## Decision

**The local provider offers two more attributes, `posix.owner` and
`posix.group`, resolved with `getpwuid_r`/`getgrgid_r` through `libc`, as
`Bytes` with hint `Identity`.**

1. **The libc, not `/etc/passwd`.** Only the libc sees what NSS resolves; a
   name parsed from a file would disagree with `ls -l` on any machine with a
   directory service.
2. **No new dependency.** `libc` is already a dependency of
   `norte-vfs-local`, the one crate allowed `unsafe` (rule 5). The two calls
   are a few lines each with `// SAFETY:` comments and tests. `uzers` /
   `users` would wrap the same calls, and the latter is unmaintained.
3. **Bytes, not text.** POSIX does not require a user name to be UTF-8. The
   frontends already mask `Bytes` values for display, and sort them by byte.
4. **Resolved only when asked, cached per id for 60 s.** The names cost a
   lookup per distinct id, so they are separate attributes and not a format of
   `posix.uid`: a listing that does not show the column never asks. The cache
   remembers misses too — an orphaned uid on ten thousand files is one lookup
   — never holds its lock while asking, and is emptied past 4096 ids. Sixty
   seconds lets a new or renamed user show up without restarting the daemon.
   A FAILED lookup (an error, `EINTR` after three retries) is not a miss and
   is not remembered: it would leave a real owner blank for a minute.
5. **Every lookup has a 200 ms deadline.** A listing cannot be cancelled
   while it is inside the libc, and a hung directory server would otherwise
   hang it. Each lookup runs on its own short-lived thread; past the deadline
   the cell is blank, and for the next minute no NEW id is asked about (a
   brake), so a dead NSS costs one deadline a minute rather than one per
   owner. A thread that answers late still fills the cache for the next
   listing.
6. **A missing name is a blank cell.** A uid with no name does not fall back
   to the number: that would make one column mean two things, and the number
   already has its own.
7. **Off by default.** The permissions column is on by default (ADR 0128)
   because it is free; these are not, so the reader turns them on.

## Consequences

- Owner and group names show and sort (ADR 0144) on local disks in both
  frontends, via the columns picker.
- SFTP still offers only the numbers. Names there need the server's
  `longname`, which is #114.
- Windows offers neither: its owner is a SID, a different question.

## Alternatives considered

- **A format of `posix.uid` that shows the name.** Formats are presentation
  in the frontend, which cannot do the lookup (rule 9: no filesystem or system
  access outside the core and providers), and the frontend may be on another
  machine than the files.
- **Resolve in the core, after the listing.** The provider is the one that
  knows which machine the uid belongs to; the core would have to guess.
