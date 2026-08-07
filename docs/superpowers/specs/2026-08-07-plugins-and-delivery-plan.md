# What belongs in a plugin, what we already half-built, and the order to do it in

**Date:** 2026-08-07
**Status:** proposal, not approved
**Supersedes:** the ordering in `2026-08-07-post-alpha-roadmap.md` (that document
still holds for the description of each capability)

Two questions, one answer. Which of the remaining §17 capabilities should be
plugins rather than core, and what happened to the plugin surfaces that were
started and never finished. They turn out to be the same question, because the
thing that decides both is a limit nobody has written down yet.

---

## The finding that governs everything below

**The plugin system cannot express the one plugin the specification explicitly
promises.**

§17 says: "Git awareness: ship status as an official columns plugin, not a Git
client in the core." The `decorator` and `columns` WIT interfaces exist, are
wired end to end, and are exactly the right shape — a badge and a role per
entry, batched per visible page, positionally 1:1 with the listing.

A git plugin has to read `.git`. A plugin cannot read anything. Rule 9 and
ADR 0022 put the guest in a sandbox with no filesystem, and the one door —
`host-log::read-scoped` — takes a token for a **single resource the host
already opened and read**. There is no way to walk a directory, and a
per-file-token API for a repository is not a workaround, it is a different
program.

So the sandbox boundary is not a detail of the git feature. It decides:

- whether git status can be a plugin at all (§17 says it must be),
- whether RAR-by-delegation can be a plugin (it cannot — a wasm guest cannot
  execute `unrar`; that needs §7.3, external programs, which does not exist),
- what a third-party plugin is actually able to do, which is the whole point of
  M4's exit criterion.

**Nothing expensive below should start before this is decided.** It is a design
session, not an implementation one.

---

## Core or plugin, item by item

The test: a plugin is right when the thing is optional or taste-dependent,
does not mutate the filesystem, does not need to hold a core invariant, and
benefits from being replaceable. Core is right when it mutates (rules 3 and 4:
cancellable task, journal, undo), needs streaming, or is a cross-cutting part
of the interface.

| Capability | Verdict | Why |
| --- | --- | --- |
| Directory compare + sync | **Core** | Mutates → journal, undo, policy, cancellable task. WIT cannot express streaming over two providers. The *comparison criterion* is a plausible plugin surface much later; the executor never is. |
| Batch rename executor | **Core** | Same: one transaction, one journal entry, one undo. |
| …its **rule sets** | **Plugin, later** | "Rename by EXIF date", "by ID3 tag". `ai.rename_plan` already established the shape — something PROPOSES name pairs, the core executes them under policy and journal. A `renamer` category reuses that shape exactly. Needs a new interface and world; wants the core executor to exist first. |
| Volumes and mounts | **Core** | Platform enumeration and eject. Eject is destructive if it lies. |
| Shell integration | **Core** | It is the binary's own lifecycle — the exit path, a CLI flag, spawning the user's shell. A sandboxed guest can do none of the three. |
| **Git status** | **Plugin — blocked** | §17 mandates it and the interfaces fit. Blocked on the sandbox question above. |
| GUI directory watching | **Core** | |
| Filesystem edge cases | **Core** | Symlink policy is an invariant, not a preference. |
| Observability | **Core** | |
| Daemon lifecycle | **Core** | |
| **RAR read-only** | **Neither — §7.3** | Product decision 5 says delegate to an installed `unrar`/`7z` with no non-free code in the graph. A wasm guest cannot exec. This needs the external-program mechanism the spec declares in §7.3 and nobody has built. |
| Packaging tier two | **Core** | |

The pattern: **everything that mutates stays in the core, and everything that
only reads and decides wants to be a plugin — and cannot be, because reading is
exactly what a plugin cannot do.**

---

## The plugin surfaces we already half-built

Recovered by reading the tree, not the notes.

### 1. `Category::Hook` — declarable, unrunnable

`Category::Hook` and `HookContrib` exist in the manifest, are parsed, are in
the catalog's category list, and appear in the plugin UI. There is **no `hook`
interface in the WIT, no world, and no call site anywhere in the host or the
core.** A manifest can declare a hook today; nothing will ever run it.

That is a lie the UI tells. Either build it or delete it — and it should be
deleted until there is a hook somebody wants, because "what should a hook hook
into" has never been answered and inventing an answer to justify an enum
variant is backwards.

### 2. `previewer-syntect` — a finished plugin nobody can install

A real syntax-highlighting previewer, built as a WASM guest, exercised by an
end-to-end test against the real runtime. It exists **only** as a test fixture.

Discovery reads `~/.config/norte/plugins/<id>/plugin.toml`. Nothing in the
project ever puts anything there — there is no `norte plugin install`, no
bundled plugin, no documented way to place one by hand. The CLI's whole plugin
surface is `norte plugin run`.

So the plugin system has been shipped with zero installable plugins, and the
first one is already written.

### 3. The shared WIT package breaks every compiled plugin on every bump

ADR 0032's known debt, verified empirically twice in the WIT header's own
comments: the package version travels inside every interface name
(`norte:plugin/previewer@0.5.0` → `@0.6.0`), so **any** bump — additive or not —
makes every previously compiled `.wasm` fail to instantiate, on the import side.

Inside this repo it does not matter: the example guests are recompiled from the
current WIT every time. It matters completely the day a third-party plugin
exists, which is M4's exit criterion. Until `provider` splits into its own
package, "a third party can ship a plugin without changing the core" is true
only within a single release.

ADR 0032 recorded the blocker as `wit-parser` 0.239 not supporting nested
`wit/deps/` packages. **The tree is now on 0.251.** Worth re-testing before
assuming the door is still shut.

### 4. `ftp-provider` ships embedded, so the plugin path is untested

It works and it dogfoods the `provider` interface, which is what #30 asked
for. But it is compiled into `norte-core/resources/ftp-provider.wasm` as a
built-in, not discovered as a plugin. The distribution path — manifest,
approval, enable, load from the user's config directory — has never carried a
provider.

### 5. #120 — the columns picker does not offer what plugins declare

Open. The picker knows announced attributes but not declared plugin columns,
and duplicate bare column ids are not disambiguated.

### 6. The demo guests are correctly just fixtures

`previewer-demo`, `command-demo`, `columns-demo`, `decorator-demo`,
`provider-mem`, `provider-mem-rw`, `net-probe`, `ftp-probe`. These are test
material and should stay that way. Listing them so nobody mistakes the count of
directories in `examples-wasm/` for a plugin ecosystem.

---

## The plan, in session-sized pieces

Each block is one session's work with a single purpose. Blocks marked
**[gate]** unblock later ones and should not be skipped or reordered.

### Phase A — decide, then stop lying

**A1 [gate]. ADR: what may a plugin read?**
No code. Decide between: a scoped read-only `wasi:filesystem` preopen for the
pane's directory; a host-side batched directory read behind a capability; the
§7.3 external-program mechanism; or "git stays in the core and §17 changes".
Whatever is chosen, the security review is part of this session, not after it —
this is the sandbox boundary, and rule 9 exists because of it.
*Decides A4, B3 and C2.*

**A2. Delete `Category::Hook`, or specify it.**
Recommended: delete. A category that can be declared and never runs is worse
than an absent one, because the UI shows it. If it is kept instead, this
session writes the interface, the world and the call site — not the enum
variant alone, which is what it has today.

**A3. Close #120.**
Picker offers declared plugin columns; duplicate bare ids disambiguated.
Small, and it makes the columns surface honest before anything new lands on it.

**A4. Ship the first plugin: `previewer-syntect`.**
`norte plugin install <path>`, a documented layout, and syntect moved out of
`examples-wasm/` into something distributable. This is what turns the plugin
system from a mechanism into a feature. Trust and signing are named here and
deferred deliberately — installing from a local path needs no key.

### Phase B — the core capabilities, in value order

**B1. Batch rename executor.**
Rules engine (counters, slices, regex, case, cleanup), a planner with cycle
detection and temporary names for permutations, whole-plan collision preview,
one journal entry and one undo for the batch. Fixes AI rename, which today
submits one `fs.move` per pair — no transaction, no whole-plan preview, and
`a→b, b→c` cannot work at all. Closes #121.

**B2. Directory comparison and synchronization.**
Streaming comparison across two providers, a plan as a first-class wire type,
results in an operable virtual pane, execution through B1's transactional
executor. `sha2` is already in the tree.

**B3. Git status plugin.** *(needs A1)*
The proof that the plugin interfaces are real, on something people want, under
whatever the A1 decision permits.

**B4. Shell integration.**
cd-on-quit for bash/zsh/fish (NUL-delimited — a directory is bytes), `--pick`
mode, terminal in the pane.

**B5. Volumes, mounts and drive switching.**
Includes free space, which a copy should be checking before it starts.

**B6. GUI directory watching.** Closes #106.

### Phase C — the second extension mechanism

**C1 [gate]. WIT package split.**
`norte:provider` out of `norte:plugin`, re-testing whether `wit-parser` 0.251
lifts ADR 0032's blocker. Without this a third-party plugin cannot survive a
norte release, which makes A4's install path a promise with an expiry date.

**C2. §7.3 external programs.** *(shaped by A1)*
The declared-but-unbuilt third mechanism. Under policy, with the argument
vector and the working directory as the whole of what the program gets.

**C3. RAR read-only by delegation.** *(needs C2)*

**C4. `renamer` plugin category.** *(needs B1)*
A plugin proposes name pairs; the core executes them transactionally. The
interface is nearly `ai.rename_plan` with a different producer.

### Phase D — what a stable release needs that is not a feature

**D1. Observability** — rotating local logs, inspectable task traces, wired
into `norte doctor`.
**D2. Filesystem edge cases** — symlink policy and cycle detection first; B2
needs them.
**D3. Daemon graceful upgrade.**
**D4. Packaging tier two** — signing, package managers, update notification.

---

## Order at a glance

```
A1 [gate: sandbox] ──┬─────────────► B3 git plugin
                     └─► C2 external ─► C3 RAR
A2 hook  A3 #120  A4 ship syntect
B1 rename ─┬─► B2 compare/sync
           └─► C4 renamer category
C1 [gate: WIT split] ─► third-party plugins survive releases
B4 shell   B5 volumes   B6 GUI watch
D1 logs    D2 fs edges   D3 upgrade   D4 packaging tier 2
```

A1 first because it is cheap and three blocks depend on it. A2–A4 next because
they are small and each removes something untrue. B1 before B2 because the
compare plan wants a transactional executor and B1 builds one. C1 whenever —
but before anybody outside this repo is invited to write a plugin.

---

## Not in this plan

CI remains off, so Windows and macOS claims stay unverifiable and #25 and #33
stay open. Four issues are blocked on other people's crates: #37, #48, #114,
#115. And a 1.0 that ships without B1, B2, B4 and B5 is either a different
version number or a smaller §17 — still a product decision, still open.
