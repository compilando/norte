# 0052 - The daemon's own state directory is a root no grant reaches

- Status: accepted
- Date: 2026-08-14
- Decision makers: Oscar González
- Related: #165 (the state directory is 0700 but nothing excludes it from a
  scope over `$HOME`), #164 (`RESOLVE_BENEATH`: a symlink at an intermediate
  component), #80 (the read gate for agents), ADR 0023 (the journal),
  ADR 0035 (where the config directory lives), ADR 0049 (the retained sync
  plan and what "owner-only" covers), hard rules 9 and 10, debt wave W4c
  (`docs/superpowers/plans/2026-08-13-debt-w4-safety.md`).

## Context and problem statement

The daemon writes its state — `journal.db`, the `sync.plan` spools,
`index.db`, `secrets.age`, `connections.toml`, `policy.toml` — into one
directory, created `0700` with `0600` files. That protects it from **other OS
users**. It does not protect it from an **in-daemon actor**, and the default
layout puts it at `$HOME/.config/norte`.

So an agent granted read scope over `$HOME` — an ordinary, reasonable grant —
could `fs.read` the complete mutation history of this user, plus a full
recursive listing of any two trees a `sync.plan` had inventoried, for trees it
has no scope over at all. Hard rule 9 held (the access went through the core
and the policy engine); the policy engine simply said yes, because a scope
over `$HOME` contains `$HOME/.config/norte`.

The class is old — `journal.db` has been there since M3. What the sync work
added is volume and a trigger: anyone who can call `sync.plan` can cause the
inventory to be written on demand and then read it back through a scope
granted for something else.

## Decision drivers

- The grant is **incidental**. Nobody types `$HOME/.config/norte` into
  `policy.toml`; they type `$HOME`. A protection an operator has to remember
  to write would fail in exactly the case that motivates it.
- A refusal an agent sees is a wire contract. `PolicyDenied.rule` is a CLOSED
  vocabulary, and adding to it is a protocol change.
- The read gate (#80) evaluates the **root** of a request. Anything recursive
  — `fs.search`, `fs.compare`, `index.query` — starts from a legitimate root
  and travels.
- A gate that covers three of five doors reads, to the next person, as if it
  covers all five.

## Options considered

### Option A — a default `deny` rule shipped in `policy.toml`

Ship `docs/policy-example.toml` with a deny for the state directory.

- **Good:** zero code, and visible where policy lives.
- **Bad:** it is a *default*, so it is editable, absent on any deployment that
  wrote its own file, and silently gone the day someone starts from scratch.
  The whole point is that this must not be grantable.

### Option B — a new `DenyReason` for it

Add `DenyReason::ProtectedRoot` → `"protected-root"` on the wire.

- **Good:** the sharpest diagnostic; a client could explain the refusal.
- **Bad:** a protocol version bump to tell a denied agent *more* about why it
  was denied — which is the one caller who should learn least.
- **Bad:** `w4c` is the branch that is deliberately not a wire branch.

### Option C — protected roots in the scope registry, denied as `out-of-scope`, plus walk exclusions

Give `ScopeRegistry` a set of protected roots, fixed at construction, checked
before any grant is consulted in all three gates (`permits`, `covers_read`,
`covers_content`). `ScopeRegistry::new()` installs the process's own state
directory, so the protection is not something a caller can forget. Recursive
readers additionally get an exclusion list, so a legitimate root does not drag
the protected subtree along with it.

- **Good:** unconditional, and evaluated before the thing it has to beat.
- **Good:** no wire change. "The path is not inside any reachable scope" is
  literally what `out-of-scope` means.
- **Bad:** two mechanisms (the gate and the walk exclusions) instead of one,
  because the gate reasons about a path and a walk reasons about a subtree.

## Decision

**Option C.**

- `ScopeRegistry::new()` protects `daemon_state_root()` — the same directory
  ADR 0035 resolves, which is why one root covers the journal, the spools, the
  keyring references and `policy.toml` at once, answering the second half of
  #165 without a second mechanism.
- The three gates return `ScopeVerdict::OutOfScope` for a path at or under a
  protected root, **before** looking at any grant. Ancestors are untouched:
  `fs.list` of `$HOME` still works and still shows the directory's name, which
  is what `ls` shows too. What cannot happen is entering it.
- `Actor::User` is unaffected everywhere. The human is not sandboxed, and
  these are the human's own files.
- Recursive readers take `walk_exclusions(actor)`: empty for `User`, the
  protected root for an agent or a plugin. `fs.search`'s walk drops the
  subtree entirely — no descent, no name hit, and not even into the `current`
  field of the progress snapshot, which is broadcast. `index.query` filters
  its hits, because the index is normally built by the human and therefore
  contains paths the querying agent could not have listed.
- A grant whose root touches a protected root is accepted and logged as a
  warning. It is quietly narrower than it looks, and an operator debugging an
  agent that "has `$HOME`" deserves the line.

## Consequences

- An agent with `$HOME` can no longer read the journal, the spools or the
  connection secrets, by any of the direct methods, by search, or by index
  query. Its refusal is `out-of-scope`, the category it already understands.
- **The protection is on the LOGICAL `VPath`.** A different path that resolves
  to the same directory — a symlink from inside the scope — walks around it.
  That is #164's family (`RESOLVE_BENEATH`), not something a registry of paths
  can decide, and it is stated in the rustdoc rather than left to be
  discovered.
- ~~**`fs.compare` still descends into it.**~~ Closed by #209: `compare()`
  takes an exclusion list and drops excluded entries from the LISTING, before
  pairing — so a protected subtree produces no row, no descent and no `stat`.
  Filtering at the listing and not at the descent is the point: a row saying
  "only on the left: journal.db" already tells what the gate meant to keep
  quiet. `sync.plan` reads two trees the same way and got the same exclusions.
- A process with no `HOME` and no passwd entry resolves a relative state
  directory, which cannot be named as a `VPath`, so it gets no protection.
  `daemon_state_root()` returns `None` there and says so.
