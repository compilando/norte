# 0054 - A provider answers about a location, not only about itself

- Status: accepted
- Date: 2026-08-15
- Decision makers: Oscar González
- Related: #153 (`fs.compare` decides case folding per PROVIDER, not per
  mount), #145 (`name_key`: ext4 `+F` uses FULL fold, not simple), #164
  (a symlink at an intermediate path component can redirect a `Copy` or a
  `CreateDir` outside its root), ADR 0004 (unknown capability names are
  ignored on the wire), ADR 0005 (the `Provider` trait), ADR 0023 (the
  journal's tamper-evident hash chain), ADR 0051 (the shared fold key), debt
  wave W5 and its design
  (`docs/superpowers/specs/2026-08-15-per-location-provider-capabilities-design.md`).

## Context and problem statement

`Provider` is addressed by path and answers about **itself**. `capabilities()`
takes no path and is synchronous; `write` and `mkdir` take no root. Three open
issues founder on that one fact, from three directions:

| issue | asks |
| --- | --- |
| #153 | "is THIS mount case-insensitive" — not: is this provider |
| #145 | "and does it fold by EXPANDING, like ext4/f2fs `+F`" |
| #164 | "open this, but refuse to leave this root" |

The consequences are not symmetric but they are all silent. `norte-vfs-local`
probes case sensitivity once, for its `base`, so comparing `/home/user/docs`
(ext4) against `/mnt/usb` (exFAT) through one `LocalProvider` asks the same
mount twice and every `README`/`readme` collision on the exFAT side goes
unreported — against `CLAUDE.md`'s own pitfall, which says to evaluate
case-insensitive collisions against the DESTINATION filesystem. `name_key`
answers two keys for `straße.txt` and `strasse.txt`, which are ONE file on an
ext4 `+F` directory, so a batch-rename plan is approved with no warning and
dies mid-execution. And a synchronisation composes `dest_root + rel` per step,
so an intermediate component that is a symlink out of the root sends the write
somewhere else entirely, past three overlap checks that all reason about the
roots — and the roots are fine.

W3 tried to route around #153 by making `compare()` take `Sides` as a
parameter. That landed and is useful, but it moved WHERE the answer is supplied
without giving anyone a way to COMPUTE a per-mount one.

## Decision drivers

- Every provider implements this trait, so a change that forces all of them to
  reimplement something is a change that will be got wrong somewhere.
- `capabilities()` is called from synchronous context; a per-location answer
  needs I/O, and hard rule 2 forbids blocking I/O in an async context, so the
  probing has to be `async` and land in `spawn_blocking`.
- `norte-vfs-sftp` and `norte-vfs-object` have no `openat` and no `statfs`.
  Whatever they answer must be honest rather than emulated.
- Shipped functionality — sync and recursive copy against sftp, S3 and
  archives — must not be withdrawn in exchange for a guarantee.

## Considered options

1. **A per-location query on the trait plus a confined-root handle** (chosen).
2. **Keep the trait as it is; let `norte-core` map a path to its mount.**
3. **Pass the answers in as parameters, wherever they are needed.**
4. **A new `LocationInfo` type, separate from `Capabilities`.**

### Option 1 — per-location query, and a handle for confinement

`capabilities_at(p)` returns the SAME `Capabilities`, refined by I/O, with a
trait default that answers `capabilities()`. `open_root(root)` returns a
handle whose operations address segments relative to that root.

- **For:** no existing signature changes, so no provider is forced to do
  anything; the default is exactly today's behaviour. The handle carries the
  confinement in the only place it can be TOCTOU-free — there is no path to
  recompose between the check and the open. `fs.capabilities` already takes a
  path and `Engine::capabilities` is already async, so the wire and the daemon
  need no new method.
- **Against:** two new pieces of trait surface, and `Capabilities` still cannot
  say "I do not know".

### Option 2 — the core maps the path to its mount

`norte_core::volumes` already enumerates mounts with their `fs_type`; the core
could resolve which mount holds each root and derive folding from that.

- **For:** no trait change, no wire change at all.
- **Against:** ext4's `+F` is a property of a **directory**, not of a mount, so
  the one thing #145 needs is the one thing this cannot answer. It only ever
  works for `file://` — sftp stays exactly as blind as it is now — and it puts
  filesystem knowledge in the layer that was built not to have any.

### Option 3 — pass the answers in as parameters

The shape W3 already tried for `Sides`: whoever knows the two roots supplies
what they fold like.

- **For:** it landed once and it is genuinely useful at the call site.
- **Against:** it relocates the question without answering it. Nobody in the
  workspace can COMPUTE a per-mount answer, so every caller either passes the
  provider-wide value it already had — which is the bug — or invents a probe of
  its own outside the crate that owns the filesystem.

### Option 4 — a separate `LocationInfo` type

A new type designed for the job: an explicit `FoldMode` instead of two flags to
combine, and every field an `Option`, so `None` means "this backend does not
know", which is genuinely different from `false`.

- **For:** it can express ignorance, which option 1 cannot.
- **Against:** a second capability vocabulary on the wire, a second thing for
  every frontend and agent to learn, and a permanent question at each call site
  about which of the two to consult. The honesty it buys is real but narrow:
  the fallback for "did not answer" is "use what the provider declares", which
  is what option 1 does without a new type.

## Decision

**Option 1.** A provider answers three classes of question, and the trait now
distinguishes them:

| class | who answers | examples |
| --- | --- | --- |
| about the BACKEND | `capabilities()`, sync, no path | `SYMLINKS`, `READ_ONLY`, `TRASH` |
| about a LOCATION | `capabilities_at(p)`, async, may probe | case folding, `+F`, `CONFINED_WRITES` |
| operate CONFINED | `open_root(root)` → `ConfinedRoot` | `RESOLVE_BENEATH` |

`capabilities()` remains the declared default and does not change meaning.
`capabilities_at`'s trait default returns it, so a provider whose locations are
all alike implements nothing.

Three consequences of the decision are themselves decisions, and are recorded
here rather than left to be rediscovered:

**`Capabilities` cannot say "I do not know", on purpose.** An absent flag reads
as absent. The degradation — answer what the provider declares — is exactly
today's behaviour, so nothing regresses and no caller learns a third state.

**Nothing is refused for lack of confinement.** `openat2(RESOLVE_BENEATH)` on
Linux and a component walk with `O_NOFOLLOW` where that syscall is missing (and
on macOS) give a real guarantee. Windows has no `openat` and is out of scope
here; sftp, object and archive have none either. On those, a recursive write
degrades to the `lstat` walk, which closes the accidental case — the rsync-ed
tree with a symlink in it — and does not close an attacker. The caller is told
before the fact (`CONFINED_WRITES`, at the confirmation, the way free space is
reported since #149) and the core emits a `tracing` WARN when it happens.

**There is no tamper-evident record of an unconfined write.** `chain_hash`
(`norte-core/src/journal.rs`) states that `batch_id`'s trick — last field,
`None` feeds nothing — is not reusable: a second optional field added the same
way makes `(batch=Some(x), other=None)` and `(batch=None, other=Some(x))` hash
identically. A `confined` column that the chain authenticates therefore costs a
new chain format version, with the anchors and `verify_chain` behind it. An
unhashed column was considered and rejected by name: it would sit in
`norte audit export` looking like evidence while being editable by anyone who
can write the database.

## Consequences

### Positive

- The comparison, the synchronisation and the rename planner ask the ROOT they
  are about to work on, so a `README`/`readme` collision on a mounted exFAT
  volume and a `straße`/`strasse` collision on an ext4 `+F` directory are both
  reported before a human approves a plan.
- `fs.capabilities` starts telling the truth for the path it is already given,
  with no new method on the wire and no change of signature.
- A `Copy` or a `CreateDir` under a caller-named root cannot be redirected out
  of it on Linux or macOS, and the fix is in the provider, so it covers every
  caller that composes a path under a root — sync, recursive copy, archive
  extraction — rather than one of them.
- Where the guarantee is unavailable, it is stated instead of assumed. That is
  the objection #164 raises against the status quo, which promised it in a
  rustdoc.

### Negative

- Two more methods on a trait every provider implements, and `SessionProvider`
  must delegate both — a missed delegation compiles and silently serves the
  DEFAULT instead of the live provider (the trait's maintenance note and the
  contract suite both guard this, and now guard two more things).
- More `unsafe` in `norte-vfs-local`: `statfs`, `ioctl`, `openat`, `openat2`.
  Rule 5 permits it there with a `// SAFETY:` and a test, and each new call has
  both — but the crate's platform-conditional surface grows again.
- The probe costs a syscall or two per distinct directory. Mitigated by a
  bounded cache keyed on `(dev, ino)`, so two paths to one directory probe
  once; unmitigated for a caller that walks many roots.
- Confinement is uneven across platforms, and a user comparing behaviour
  between Linux and Windows will see a warning on one and not the other. This
  is a true statement about the platforms, but it is one more thing to explain.
- `+F` detection is Linux-only and no CI machine has such a volume, so the
  verdict is pinned by the canonical corpus and by `MemProvider` rather than by
  a real filesystem.
