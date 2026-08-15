# A provider answers about a LOCATION — design

**Debt wave W5.** Closes #153, #145 and #164. One spec, one ADR, two phases,
one branch.

## The one fact behind three issues

`Provider` is addressed by path and answers about **itself**. `capabilities()`
takes no path and is synchronous; `write`/`mkdir` take no root. Three open
issues founder on that single fact, from three directions:

| issue | asks |
| --- | --- |
| #153 | "is THIS mount case-insensitive" — not: is this provider |
| #145 | "and does it fold by EXPANDING, like ext4/f2fs `+F`" |
| #164 | "open this, but refuse to leave this root" |

W3 tried to route around #153 by making `compare()` take `Sides` as a
parameter. That landed and is useful, but it moved WHERE the answer is supplied
without giving anyone a way to COMPUTE a per-mount one. This spec is that
correction.

### What already exists, and changes the shape of the work

Three things were found in the code before planning, and each one shrinks a
task that the issue bodies describe as large:

- **`norte_encoding::FoldMode::Full` is already built** — expansions (`ß` →
  `ss`, the `ﬁ`/`ﬀ`/`ﬆ` ligatures), tests, and a corpus pair. What #145 is
  missing is not the algorithm; it is **something that selects `Full`**.
  Nothing probes a real filesystem for it.
- **`fs.capabilities` already takes a path**, and `Engine::capabilities(&path)`
  is already `async`. The wire plumbing exists; it currently lies, delegating
  to the provider-wide `capabilities()`.
- **`LocalProvider::capabilities()` is synchronous and cached once for
  `base`** (a `OnceLock` filled by the first async operation). Any per-location
  answer needs I/O, so it needs a new async method — not a parameter on the
  existing one.

## 1. The doctrine (the ADR)

A provider answers three classes of question, and today it conflates them:

| class | who answers | examples |
| --- | --- | --- |
| about the BACKEND | `capabilities()`, sync, no path | `SYMLINKS`, `READ_ONLY`, `TRASH` |
| about a LOCATION | `capabilities_at(p)`, async, may probe | case folding, `+F`, a mount's real `max_path` |
| operate CONFINED | `open_root(root)` → handle | `RESOLVE_BENEATH` |

The rule the ADR fixes: **`capabilities()` stays the declared default;
`capabilities_at` is the same type, refined by I/O.** A provider that does not
distinguish locations implements nothing — the trait default returns
`capabilities()`. Nothing breaks.

**`Capabilities` cannot say "I do not know", and that is accepted on purpose.**
An absent flag reads as absent, not as unknown. The degradation is "answer what
the provider declares", which is exactly today's behaviour, so nothing gets
worse and no caller has to learn a third state. It is written in the ADR as a
known limit rather than left as an omission.

## 2. Phase A — the question about a location (#153, #145)

### The trait

```rust
/// Capabilities REFINED for `p`: the same declaration, corrected by what the
/// backend can find out about that location. The default answers what the
/// provider declares — correct for any backend whose locations are alike.
async fn capabilities_at(&self, p: &VPath) -> Result<Capabilities, Error> {
    let _ = p;
    Ok(self.capabilities())
}
```

Delegated in `SessionProvider` (`norte-core/src/sessions.rs` — the trait's
maintenance note requires it: a cached remote session would otherwise serve the
DEFAULT instead of the live provider, with no compile error) and exercised by
the provider contract suite.

### The flag

`norte-proto`: `FULL_FOLD = 1 << 9`. Meaning: **this directory's folding
EXPANDS** (ext4/f2fs `+F`, whose kernel table is built from `CaseFolding.txt`
status `C + F`). It only makes sense with `CASE_SENSITIVE` absent, and the
`Capabilities` rustdoc says so. By ADR 0004 an N-1 client ignores an unknown
well-formed name, so the wire change is additive.

### The probe, in `norte-vfs-local`

In this order, and nothing mutates until the last step:

1. **Linux**: `statfs.f_type` (identifies ext4/f2fs/exfat/ntfs3/vfat/cifs) plus
   `ioctl(FS_IOC_GETFLAGS)` and `FS_CASEFOLD_FL`, which answers **per
   directory** — which is precisely what #145 needs and what a per-mount answer
   could not give.
2. **macOS**: `pathconf(_PC_CASE_SENSITIVE)`, which already exists in this
   crate and is already per volume.
3. **Windows**: the volume's `FILE_CASE_SENSITIVE_SEARCH` flag.
4. **Last resort, and only if the directory is writable**: today's write probe
   (`.norte-probe-<pid>-<seq>-A`).

Read-only first is not tidiness. `capabilities_at` will be called on every
comparison root, and today's probe CREATES a file: on a read-only mount it
fails and cannot distinguish "not writable" from "does not fold"; on someone
else's directory it is visible to watchers and to backup tools.

**Cache key: the directory's `(dev, ino)`, not its path.** Two paths to one
directory are one entry, and a `..` or a symlink does not multiply the probing.
A bounded map with LRU eviction inside `LocalProvider`, beside the existing
`OnceLock` (which keeps its job: the provider-wide default).

### `Sides` grows from a bool to a `FoldMode`

`norte_compare::Sides` currently holds `fold_case: bool` and translates it to
`FoldMode::Simple` or `FoldMode::None`; it never turns `Full` on. It grows to
carry each side's mode and to resolve the pair:

- either side folds → the pair folds (today's rule, unchanged);
- **either side EXPANDS → the pair expands.** `straße.txt` and `strasse.txt`
  are one name for whoever has to decide whether they collide, and the side
  that cannot hold both spellings is the one that decides.

`NameCaps` (`norte-core::rename::plan`, used by the batch-rename planner and by
`undo`) has the same shape and gets the same treatment: it becomes a `FoldMode`
rather than a `case_sensitive: bool`.

### Call sites

The three places that build `Sides` today swap `capabilities()` for
`capabilities_at(root)`:

- `norte-core/src/compare.rs:121`
- `norte-core/src/engine.rs:1272`
- `norte-core/src/sync/spool.rs:2548`

and `engine.rs:2152` for `NameCaps`. `fs.capabilities` starts telling the truth
per path without a signature change.

## 3. Phase B — confining the write (#164)

### The hole, restated

A sync validates its two roots — structurally, by `node_id`, and again during
the walk — and then composes `dest_root + rel` per step. If any INTERMEDIATE
component of that relative path is a symlink pointing outside the root, the
write lands outside `dest_root` and none of the three overlap checks sees it:
they all reason about the roots, and the roots are fine.

The exposure is `Copy` and `CreateDir`. The destructive kinds dodge it by
accident: the revalidation `stat` is an `lstat`, so a witness that says `Dir`
meets a `Symlink` and the step conflicts out; and `ops::walk` does not descend
links, so `DeleteTree` does not follow one either.

### The trait

Minimal surface — only the two exposed operations. Giving `DeleteTree` a handle
is scope nobody asked for.

```rust
/// Opens `root` as a confined root. `Unsupported` = this backend cannot.
async fn open_root(&self, root: &VPath) -> Result<Box<dyn ConfinedRoot>, Error> {
    let _ = root;
    Err(Error::Unsupported)
}

#[async_trait]
pub trait ConfinedRoot: Send + Sync {
    async fn mkdir(&self, rel: &[Segment]) -> Result<(), Error>;
    async fn write(&self, rel: &[Segment]) -> Result<Box<dyn ByteSink>, Error>;
    /// Same contract as `Provider::open_resumable`. Default: `(write(rel), 0)`.
    async fn open_resumable(&self, rel: &[Segment])
        -> Result<(Box<dyn ByteSink>, u64), Error> {
        Ok((self.write(rel).await?, 0))
    }
    async fn stat(&self, rel: &[Segment]) -> Result<Entry, Error>;
}
```

`open_resumable` is on the handle because the copy engine reaches for it
first (`ops.rs:1159` picks between `dst.open_resumable(to)` and `dst.write(to)`
by policy); a handle without it would route every resumable copy back around
the confinement. The sink it returns publishes with `renameat` against the same
root fd, so the staging-to-final step is confined too — that step composes a
path today and is exactly where the guarantee would otherwise leak.

The caller opens the root ONCE and then addresses relative segments. There is
no path to recompose, which is what makes it TOCTOU-free rather than
TOCTOU-with-a-check.

A relative path that escapes answers `Error::Conflict` with a new
`ConflictKind::EscapesRoot` — **not** `NotFound`, because a caller that sees `NotFound`
retries by creating the parent, which is exactly what must not happen here.

### The platform matrix, honest and incomplete

| platform | how | guarantee |
| --- | --- | --- |
| Linux ≥5.6 | `openat2(RESOLVE_BENEATH)` from the root's fd | kernel-enforced, per step |
| Linux <5.6 or seccomp (`ENOSYS`) | component walk, `openat(O_NOFOLLOW\|O_DIRECTORY)` relative to the previous fd | real: there is never a path to recompose |
| macOS | the same component walk | real |
| Windows | **not in this phase** | degrades, with a witness |
| sftp, object, archive | no `openat` of any kind | degrades, with a witness |

Windows is out by decision, not by oversight: it has no `openat`, and doing it
properly is `NtCreateFile` with a relative handle and
`FILE_FLAG_OPEN_REPARSE_POINT` — another spec. It gets its own issue, and the
ADR says so.

`unsafe` lives only in `norte-vfs-local` (hard rule 5), with a `// SAFETY:` per
site and a test that exercises the real rejection: an intermediate symlink
pointing at a sibling `TempDir`, and the assertion that the file does NOT
appear there.

### The witness

`CONFINED_WRITES = 1 << 10`, answered by `capabilities_at` — not by
`capabilities()`, because whether confinement is available depends on the
mount, the platform and the running kernel. It does two jobs:

- **before**: at the confirmation, the frontend asks `fs.capabilities` (already
  exists, already takes a path) and warns the way free space warns since #149 —
  it states the fact and lets the human decide.
- **after**: a `tracing` WARN on the effectful core function, with `task_id`
  and a redacted VPath, which is where this codebase already puts the facts
  about what an operation did.

**The journal does NOT get a `confined` column, and that is a cost decision.**
`chain_hash` (`norte-core/src/journal.rs:443`) carries an explicit warning that
`batch_id`'s trick — last field, `None` feeds nothing — is not reusable: a
second optional field added the same way makes `(batch=Some(x), other=None)`
and `(batch=None, other=Some(x))` hash identically. A hashed field therefore
costs a new chain format version, with the anchors, `verify_chain` and a
security review behind it. That is a wave of its own for a boolean, and the
witness that actually changes an outcome is the one BEFORE the operation, where
a human is still deciding. An unhashed column was considered and rejected: it
would sit in `norte audit export` looking like evidence while being editable by
anyone who can write the DB.

Without the flag, `norte-core` falls back to the `lstat` component walk it can
do everywhere. That closes the accidental case — the rsync-ed tree with a
symlink in it — and does not close an attacker. That distinction is written in
the rustdoc and in the ADR instead of being promised away, which is the state
#164 objects to today.

**Nothing is refused for lack of confinement.** Sync and recursive copy against
sftp/S3/an archive are shipped functionality; a guarantee that arrives by
breaking them is not an improvement.

## 4. The wire

One minor protocol bump for the whole wave:

- `CapabilityFlags::FULL_FOLD` (bit 9) and `CONFINED_WRITES` (bit 10)
- `ConflictKind::EscapesRoot` (additive: the enum already has a `#[serde(other)]
  `Unknown` fallback, so an N-1 client reads it as `Unknown` rather than failing)
All additive; ADR 0004 already requires an N-1 client to ignore an unknown
well-formed flag name. Goldens and the JSON Schema regenerate in the same
commit. `protocol-guardian` is mandatory on that commit.

## 5. Testing

**Phase A.** CI has no ext4 `+F` volume, so `Full` is proved two ways that do
not need one: a `MemProvider` that answers `FULL_FOLD` from `capabilities_at`,
and the corpus pair `ext4_full_fold_es_zett`/`ext4_full_fold_ss`
(`straße.txt`/`strasse.txt`) that #145 already left in place. Today they are
two keys; after this wave they are ONE key when a side declares `FULL_FOLD`,
and still two when none does. The probe's read-only ladder gets a unit test per
rung with the syscall faked at the boundary; the write probe keeps its existing
test.

**Phase B.** The sibling-symlink test (the one that would have caught #164);
one that forces `ENOSYS` and takes the component-walk path; a clean-cancellation
test with an open handle (hard rule 3); and provider conformance — `open_root`
answers `Unsupported` on sftp, object and archive.

## 6. Order and gate

Phase A lands FIRST and on its own. If phase B stalls in the platform matrix,
two issues are already closed and merged.

| when | run | budget |
| --- | --- | --- |
| RED→GREEN | `just t <crate>` (+ `just c` on lint surface) | unlimited |
| closing phase A | `just ci-fast` | ONE run |
| closing the branch | `just ci` | ONE run |

Reviewers, dispatched by whoever does the work, before committing:
`protocol-guardian` (mandatory — the wire grows), `security-reviewer` (phase B
is a security boundary), `encoding-auditor` (phase A is filename folding),
`rust-reviewer` (both phases are substantial Rust).

## 7. Out of scope, on purpose

- **Windows confinement.** Its own issue, opened by this wave.
- **`DeleteTree` and rename through a confined root.** They dodge the hole
  today for reasons that are written down; widening the handle to cover them is
  a change with no bug behind it.
- **A third capability state ("unknown").** Discussed in §1 and rejected.
- **A tamper-evident record of an unconfined write.** It needs chain format 2;
  see §3. Its own wave if anyone wants it.
- **#122** (M4-IA-2 deferred review items), which W5's plan file lists as
  arriving here. It shares no mechanism with these three and is not part of
  this spec.
