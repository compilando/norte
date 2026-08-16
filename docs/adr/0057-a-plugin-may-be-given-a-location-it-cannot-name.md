# 0057 - A plugin may be given a location it cannot name

- Status: accepted
- Date: 2026-08-16
- Decision makers: Oscar González
- Related: roadmap post-alpha item 6 and its design
  (`docs/superpowers/specs/2026-08-16-git-status-columns-design.md`), ADR 0022
  (plugin capabilities and consent), ADR 0032/0033 (the WASM sandbox), ADR 0037
  (plugin data-out v2, where `columns` was introduced), ADR 0052 (protected
  roots), ADR 0054 and #164 (confined writes, `openat2(RESOLVE_BENEATH)`),
  hard rule 9, WIT packages `norte:plugin@0.8.0` and `norte:location@0.1.0`.

## Context and problem statement

`columns` shipped in ADR 0037 with a deliberately thin contract: a guest is
handed the **basenames** of the visible page and nothing else. That was the
right default — a plugin that only ever sees names cannot leak where you are —
and it is also why the only columns plugin anyone could write was a demo that
counts characters.

Every column worth having says something about the *file*, not about its name:
git status, image dimensions, media duration, build freshness. All of them need
to read something. The interface had no way to let them, and the sandbox has no
way for a guest to reach the filesystem itself: `exec` is permanently `none`,
there are no preopens, and the `WasiCtx` is empty by construction.

So the question is not "should a plugin read files" — it is **what is the
smallest thing we can hand it that makes a real column possible**.

## Decision

**A guest gets a token, not a path, and the token is the filesystem it has.**

### The token

- Minted **per call** from the directory the page belongs to, as 32 bytes from
  the system CSPRNG. Nothing about the path is derivable from it.
- It dies when the call's session is dropped. That `Drop` **is** the expiry:
  there is no TTL to tune and no sweep to forget, and a token kept from the
  previous page resolves to nothing.
- Resolution goes through `norte-vfs-local`'s confined opener — the same
  `openat2(RESOLVE_BENEATH)` that closed #164. A `..` that climbs out, an
  absolute `rel`, and a symlink pointing outside are refused **by the kernel**,
  not by a check someone can forget to write.
- The basename-only decision of ADR 0037 is **preserved, not reversed**: the
  guest still never receives a path. It receives a handle and relative names.

### The bounds, and that failures cost

Bytes per read, calls per session, total bytes per session, entries per listing.
All fail-closed. A call is charged **before** the work and also when the work
fails — if a failing call were free, probing the tree by failing on purpose
would be free too.

### The capability

`location = "read"` in the manifest. It enters the approval digest, so a plugin
that adds it needs approving again, and it shows as its own badge in the
extension manager. It is emitted into the digest **only when granted**, because
emitting it always would move the digest of every manifest that does not ask for
it and reset consent that people already gave.

### The root marker, and why the design changed

The design said the guest would find its project root by walking up from the
location. It cannot: the confinement is real, so `..` is refused — that is the
entire point of it. A columns plugin would therefore only work with the pane
standing exactly on the repository root, which is not a feature.

So the manifest declares what its project root looks like:

```toml
[capabilities]
location = "read"
location-root-marker = ".git"
```

and the host opens the **nearest ancestor containing an entry by that name**,
handing the guest the `prefix` of the directory the user is actually looking at.
The host learns nothing about git; it learns "climb to the ancestor holding this
name".

What bounds the climb:

- the marker must be a **single name** — a slash would make the host walk a path
  the plugin chose, which is a different capability;
- it enters the approval digest **with** the capability, so changing `.git` for
  something else needs approving again;
- the climb stops at the first protected root (ADR 0052) and at 64 levels;
- it happens **only for the human actor**. An agent or a plugin is confined to
  its scope, and climbing above it is exactly what the read gate prevents.

### Where the host functions live

`norte-plugin-host` implements the WIT interface but does **not** touch the
filesystem: it calls an injected `LocationHost` trait, implemented in
`norte-core`. The dependency `norte-plugin-host → norte-vfs-local` is the one
this design refuses to create — that direction puts the filesystem inside the
sandbox crate, and the next person to need "just one path" would find it already
there.

### One implementation, two callers

Minting, bounds and the guest invocation live in `norte-core::plugins`, and both
the daemon handler and the embedded backend call it. A capability enforced on
one path and not the other is the failure this repository has already written
down three times (#165, #201, #181).

## Consequences

- A real columns plugin can exist. The first one, `org.norte.git-status`, is
  installed under `config_dir/plugins/` exactly like a stranger's would be —
  that install path is the thing being proven, so it is not embedded in the
  binary.
- `norte:plugin` bumps 0.7.0 → 0.8.0 and every `.wasm` compiled against 0.7.0
  stops instantiating. Known cost, verified three times now, and the in-tree
  guests are rebuilt in the same change.
- **No instance pool.** The design called for one, keyed by (plugin, location).
  Measured instead: 167 ms for a twenty-row page over a two-thousand-entry
  index, instantiating the component and parsing the index from scratch each
  time. Values arrive after the page is painted, so that is not a stall the user
  waits on — but it is the obvious next optimisation, and it is filed as #224
  rather than done.
- What the first plugin does not do, said plainly and filed as #225: staged
  status (index versus HEAD) would need an object-database reader in the guest;
  submodules are not handled; a `.git` FILE (worktrees) is not followed; and a
  location on a provider that is not `file://` mints no token at all, so the
  column is simply empty there.
