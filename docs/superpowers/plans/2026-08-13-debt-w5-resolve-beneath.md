# Debt wave W5 — the `Provider` trait answers questions about a path

**Tier T3: this is not debt, it is a capability.** It gets its own spec and its
own branch, and it is last.

W3 sent two more issues here, and they turn out to be the same question asked
from three directions:

| issue | asks |
| --- | --- |
| #164 | "open this, but refuse to leave this root" |
| #153 | "is THIS mount case-insensitive" (not: is this provider) |
| #145 | "and does it fold by expanding, like ext4 `+F`" |

All three founder on the same fact: **`Provider` is addressed by path and
answers about itself.** `capabilities()` takes no path, and `open` takes no
root. W3 tried to route around it for #153 by making `compare()` take `Sides`
as a parameter — that landed and is useful, but it moves WHERE the answer is
supplied without giving anyone a way to COMPUTE a per-mount one, which is the
correction this wave inherits.

So the spec is one spec, and the ADR is one ADR: what does a provider answer
about a *location* rather than about itself, and what does a provider that
cannot answer say instead. `norte-vfs-sftp` and `norte-vfs-object` have no
`openat` and no `statfs`; for them the honest answer is a declared
`Capabilities` bit, not an emulation.

**#164:** a symlink at an intermediate path component can redirect a `Copy` or a
`CreateDir` outside its root.

**Why it is not a one-line syscall swap.** The `Provider` trait is addressed by
path and does not know the caller's root, so "open beneath this root" is a new
capability on the trait, not a flag on an `openat`. Every provider has to answer
for it — and `norte-vfs-sftp` and `norte-vfs-object` have no `openat` at all, so
the honest answer for them may be a declared `Capabilities` bit rather than an
emulation.

**Path:** `superpowers:brainstorming` → spec in `docs/superpowers/specs/` → ADR
(it changes a trait every provider implements) → plan → build.

**Do not start it inside another wave.**
