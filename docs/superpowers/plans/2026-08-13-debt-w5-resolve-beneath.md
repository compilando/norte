# Debt wave W5 — #164, `RESOLVE_BENEATH`

**Tier T3: this is not debt, it is a capability.** It gets its own spec and its
own branch, and it is last.

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
