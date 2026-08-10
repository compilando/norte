# Volumes, free space and drive switching — design

**Date:** 2026-08-10
**Status:** approved
**Roadmap item:** 3 of `2026-08-07-post-alpha-roadmap.md`
**Closes:** #131 (`pane.select-drive`, `pane.select-drive-left`, `pane.select-drive-right`)

## Why

Norton Commander had `Alt+F1`/`Alt+F2` and every orthodox manager since has had
them. norte has no command for it, and `statvfs` appears nowhere in the tree.
The four imported presets bind those keys today and answer "not built yet
(#131)", which is honest and is not a feature.

Free space is also the answer to a question a copy should ask before it starts
and currently does not — but that question is not this item (see "Out").

## Scope

In:

- Enumerating the host's volumes, with label, filesystem type, kind, total and
  free space.
- A picker in both frontends, bound to the three commands the catalogue already
  declares.
- The wire method that carries it, so the CLI and an agent-facing surface are
  not locked out by construction.

Out, each deliberately and each its own issue:

- **Eject.** "Safe" means the write cache is flushed and nothing of ours holds
  the mount, and saying so wrongly loses data. It is also a whole platform
  surface per OS (udisks2, `diskutil`, Win32) and a new failure mode in an item
  that otherwise has none.
- **A pre-copy free-space check.** It needs the source's total size, a policy
  for what to do when it does not fit, and an answer for a destination that
  cannot report its space. Sparse files, filesystem compression and quotas make
  "it does not fit" wrong often enough that the policy question is real.
- **Mounting and unmounting.**

## A. Where enumeration lives

**`norte-core::volumes`, a host service — not a `Provider` method.**

A volume is a property of the HOST, not of a path. Putting `volumes()` on the
trait would mean every wrapper implements it — `SessionProvider` already
delegates seventeen methods, and the archive provider composes over an inner
one — to have exactly one implementor answer and the rest decline. The cost is
paid forever for a case that does not exist yet.

The trade, stated so it is not discovered later: if an sftp provider should one
day list its host's mounts (`df` over the connection), there is no seam to hang
it on and this becomes a refactor. That is accepted; the day it happens, a
`Provider::volumes` can be added beside the host service rather than instead of
it.

The type:

```rust
pub struct Volume {
    /// Mount point. A VPath, so the bytes survive (rule 1).
    pub mount: VPath,
    /// What the OS or the filesystem calls it, when it says.
    pub label: Option<String>,
    /// `ext4`, `apfs`, `ntfs`, `nfs4`… as the platform spells it.
    pub fs_type: String,
    pub kind: VolumeKind,
    /// `None` when the filesystem did not answer in time — never a zero
    /// standing in for "unknown".
    pub total_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    pub read_only: bool,
}

pub enum VolumeKind { Fixed, Removable, Network, Pseudo, Unknown }
```

### The two hazards that shape it

**A mount point is bytes.** `/proc/mounts` escapes space, tab, newline and
backslash as octal (`\040`, `\011`, `\012`, `\134`). The unescape produces
BYTES, never a `String`: a volume mounted at `/media/USB de Ñico`, or at a name
that is not UTF-8 at all, has to survive enumeration, the wire and the picker
intact. This is where a corpus fixture goes, because it is exactly the class of
bug this repository keeps finding.

**`statvfs` on a hung NFS mount never returns.** Enumeration cannot block on
it. Each mount's space query runs with a deadline; a mount that does not answer
comes back with `total_bytes: None` / `free_bytes: None` and the row says so.
A picker that hangs because one mount is dead is worse than no picker, and the
failure is not hypothetical — a laptop that suspended with a share mounted has
it every time.

All of it is blocking I/O and runs in `spawn_blocking` (rule 2).

## B. The wire

A new method, `host.volumes`, with an additive result. Minor protocol bump,
new goldens, `protocol-guardian` mandatory — the standing rule for any
`norte-proto` change.

`VolumeKind` carries an `Unknown` variant and `#[serde(other)]`, so a volume
kind added in N+1 degrades instead of breaking an older client. That is the
same shape `EntryKind::Other` and `TaskKind::Unknown` already use.

The mount point crosses the wire as a `VPath`, which is lossless for
non-UTF-8 bytes. Nothing in this feature may carry a mount point as a `String`.

## C. The daemon answers humans, not agents

The mount table says which disks you have, which servers you mount and what
your removable media are called. An agent operating under a scope does not need
it, and handing it over is information disclosure for no capability gained.

So the handler requires the connection's actor to be `User` and answers an
agent with `PolicyDenied` — the same shape `policy.decide` and `policy.pending`
already use, and the same reasoning (`DenyReason::rule_id()`'s closed
vocabulary, never the concrete rule).

## D. The frontends

`NavPopupKind` already has `History` and `Hotlist`; volumes is a third kind,
not a new screen. Three commands, matching what the catalogue declares:

- `pane.select-drive` — the focused pane.
- `pane.select-drive-left` / `pane.select-drive-right` — Total Commander's
  `Alt+F1` / `Alt+F2`, which name a side rather than the focus.

A row shows the label, the mount point through `path_display` (so a hostile
name is masked and badged like everywhere else), the filesystem type, and free
of total. `Enter` changes that pane's directory to the mount point. A key
inside the popup toggles the unfiltered list.

The GUI gets the same list with its own rendering. Both frontends flip the
three catalogue entries from `Planned` to `Live`, which is what closes #131.

## E. The filter, declared and tested

Hidden by default, by `fs_type`: `proc`, `sysfs`, `cgroup`, `cgroup2`,
`devtmpfs`, `devpts`, `tmpfs`, `ramfs`, `overlay`, `squashfs`, `autofs`,
`debugfs`, `tracefs`, `securityfs`, `pstore`, `bpf`, `configfs`, `fusectl`,
`mqueue`, `hugetlbfs`, `binfmt_misc`, `efivarfs`, `nsfs`.

The list lives in one constant with a test, not spread across branches, and a
key in the popup shows the raw table. Hiding something has to be a decision
somebody can read, and the escape hatch exists because the day the criterion is
wrong, the user still needs to reach their mount.

`tmpfs` is the arguable one and is hidden on purpose: `/run`, `/dev/shm` and
the per-user runtime directories are tmpfs and are noise, while a deliberate
`tmpfs` on `/scratch` is reachable through the unfiltered list.

## F. Platforms

| platform | source | verified here |
| --- | --- | --- |
| Linux | `/proc/mounts` + `statvfs`; `/sys/class/block/*/removable` for the kind | yes |
| macOS | `getmntinfo` + `statfs`; `MNT_LOCAL`/`MNT_RDONLY` flags | no |
| Windows | `GetLogicalDrives` + `GetDriveType` + `GetDiskFreeSpaceEx` | no |

All three are written and must compile. Only Linux is verified, because the
gate is one Linux machine and GitHub CI is off. The closing note and the
platform modules' rustdoc say which is which; nothing claims support it has not
demonstrated. `GetDriveType` is what distinguishes removable from fixed from
network on Windows, so the kind is not a guess there either.

## Testing

- **The `/proc/mounts` parser is pure** and gets the bulk of the tests: octal
  escapes for all four characters, a mount point that is not UTF-8, a line with
  more fields than expected, and a truncated line. Fixtures are real
  `/proc/mounts` samples, not invented ones.
- **A corpus fixture** for an escaped mount name, in the canonical
  `norte-testkit` corpus, because this is a path that reaches a display.
- **The filter criterion is pinned per `fs_type`**, so adding a hidden type is
  a visible diff.
- **A mount whose `statvfs` does not answer** yields `None`, not zero, and does
  not delay the others past the deadline. Staged with a fault-injecting
  space-query, not with a real hung mount.
- **The wire** gets goldens for the new method and a round-trip of a non-UTF-8
  mount point.
- **The picker** renders a hostile mount name masked and badged.

## Decomposition

1. **V1 — the host service.** `norte-core::volumes`: the type, the Linux
   implementation, the parser, the deadline on the space query, the filter
   constant. No wire, no UI.
2. **V2 — the wire.** `host.volumes` in proto with goldens and the version
   bump, the daemon handler with its `User`-only gate, and `Backend::volumes`
   in both modes.
3. **V3 — the TUI.** `NavPopupKind::Volumes`, the three commands, the
   unfiltered toggle, the catalogue flip.
4. **V4 — the GUI and the other platforms.** The GPUI list, plus the macOS and
   Windows implementations behind their `cfg`, compiled and unverified.

V1 before V2 before V3. V4 is independent of V3 and last because it is the part
that cannot be verified here.
