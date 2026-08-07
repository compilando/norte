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

## Decided since this was written

**ADR 0041 (2026-08-08) settled the boundary this document was circling.** The
four built-in providers — local, SFTP, object storage, archives — stay in the
core. The plugin path is how NEW backends arrive. The gaps in the `provider`
WIT close on demand, driven by the first plugin that needs each one. And the
package split moved to the front, because closing gaps on demand means the WIT
keeps moving and a third-party plugin currently survives exactly one release.

The reasoning is in the ADR; the short version is that S3 as a plugin would
speak plaintext HTTP (`aws-lc-rs` does not compile to wasm) and would lose
server-side copy, attributes, trash, resume and cancellation. Independent
maintenance, which was the actual motivation, does not need a plugin — a crate
boundary already gives it.

## The finding that shaped the plugin sections below

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

**And the specification is on the other side of that gap.** §7.1 says a
manifest "declares scoped filesystem, network-host, AI, and execution
capabilities" and that "the host mediates every capability **through WASI** and
norte policy". A WASI-mediated, policy-gated, read-only view of one directory
is therefore what §7.1 describes; `read-scoped` is a narrowing that ADR 0022
chose, not the boundary the spec drew. Rule 9 forbids *direct* access — it does
not forbid a mediated one, which is what "the host mediates every capability"
means.

So the sandbox boundary decides whether git status can be a plugin at all
(§17 says it must be). Under ADR 0041 that is no longer a gate over the whole
plan — it is one gap on the list, and git is the plugin that forces it. It gets
its own spike and its own ADR, in B3.

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
| **RAR read-only** | **Core provider that delegates** | Product decision 5: delegate to an installed `unrar`/`7z`, no non-free code in the graph. Not a plugin — a wasm guest cannot exec, and `exec` is a hard `none` in the manifest. Not §7.3 either: that is `openers.toml`, which opens a file in another application and is explicitly "declarative configuration, not plugins". So it is a provider in the core that shells out, next to the archive providers. |
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

That is a lie the UI tells. It cannot simply be deleted: §7.1 names "operation
hooks" among the interfaces WIT is meant to cover, so removing the category
would put the code further from the specification rather than closer. The two
honest moves are to build it — which first requires answering what an operation
hook hooks into, and what it may do when a mutation is about to happen, which
is a policy and journal question before it is a WIT question — or to keep the
category and stop offering it, so a manifest that declares one is rejected at
parse time with "not implemented yet" instead of installing something inert.

The second is a session; the first is a milestone. Take the second now.

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

### Phase A — make the plugin story true before extending it

**A0 [gate]. Split the WIT package.**
Promoted out of Phase C by ADR 0041 decision 4. Today any bump to the shared
package makes every previously compiled `.wasm` fail to instantiate, on the
import side, verified twice. So a third-party plugin survives exactly one norte
release — and decision 3 (close the gaps on demand) guarantees the WIT keeps
moving, which makes it worse, not better.

Split `norte:provider` out of `norte:plugin`. First thing the session does is
re-test ADR 0032's recorded blocker: `wit-parser` was 0.239 then and is 0.251
now. If the blocker is gone this is mechanical; if it is not, the session's
output is what it would take, and A4 ships with a stated version tie.

*Blocks: publishing anything a third party is meant to keep.*

**A1 [gate]. ADR 0041 — which providers are core, which are plugins. DONE.**
Decided 2026-08-08. The four built-in providers stay in the core; the plugin
path is how NEW backends arrive; the WIT gaps close on demand, driven by the
first plugin that needs them; and the package split (C1) is a precondition for
inviting anyone outside this repository to write one.

*What this displaced:* the session was originally scoped as "what may a plugin
read", aimed at unblocking a git plugin. That was the smaller question. The
sandbox boundary still has to be decided before B3, but it is now one item
inside the gap list rather than the gate over the whole plan — see B3.

**A2. Stop offering `Category::Hook`.**
Reject a manifest that declares one, with a reason that says "not implemented
yet" rather than installing something inert. The category stays, because §7.1
names operation hooks; what goes away is the pretence that declaring one does
anything.

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

**B3. Git status plugin.** *(needs its own sandbox decision first)*
The first plugin that forces a WIT gap, and therefore the first test of
ADR 0041's decision 3 — the gap closes because a real plugin needs it.

The gap here is reading: a git plugin has to read `.git`, and the guest's only
door is `read-scoped`, a token for one blob the host already read. Options are
a read-only WASI preopen scoped to one directory (which is what §7.1 describes
— "the host mediates every capability through WASI"), a host-mediated read over
the VFS, or gix in the core with §17 amended.

**Spike before deciding:** does any git implementation build for
`wasm32-wasip2`? If none does, the plugin route is closed on facts rather than
on preference, and the honest answer is the third option. The spike is a day;
the ADR after it writes itself.

**B4. Shell integration.**
cd-on-quit for bash/zsh/fish (NUL-delimited — a directory is bytes), `--pick`
mode, terminal in the pane.

**B5. Volumes, mounts and drive switching.**
Includes free space, which a copy should be checking before it starts.

**B6. GUI directory watching.** Closes #106.

### Phase C — the second extension mechanism

**C1 [gate]. WIT package split. — MOVED UP, see Phase A.**

**C2. RAR read-only, as a core provider that delegates.**
Product decision 5. `unrar`/`7z` invoked from the archive side of the core,
never from a guest; honest failure when the executable is absent; the delegate
gets an argument vector and a pipe, nothing else.

**C3. `renamer` plugin category.** *(needs B1)*
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
A1 ADR 0041 ✔ done
A0 [gate: WIT split] ──► a third-party plugin survives more than one release
A2 hook   A3 #120   A4 ship syntect
B1 rename ─┬─► B2 compare/sync
           └─► C3 renamer category
B3 git ──► needs its own spike + sandbox ADR (no longer gates anything else)
C2 RAR (core provider, delegates to unrar/7z)
B4 shell   B5 volumes   B6 GUI watch
D1 logs    D2 fs edges   D3 upgrade   D4 packaging tier 2
```

A0 first: it used to sit in Phase C on the assumption that nothing depended on
it, and ADR 0041 decision 3 turned that around — closing WIT gaps on demand
means the package will keep moving, so the split has to precede the movement
rather than follow it. A2–A4 next because each is small and each removes
something untrue. B1 before B2 because the compare plan wants a transactional
executor and B1 builds one. B3 is now free-standing: it needs a spike and an
ADR of its own, and nothing else waits on it.

---

## Not in this plan

CI remains off, so Windows and macOS claims stay unverifiable and #25 and #33
stay open. Four issues are blocked on other people's crates: #37, #48, #114,
#115. And a 1.0 that ships without B1, B2, B4 and B5 is either a different
version number or a smaller §17 — still a product decision, still open.
