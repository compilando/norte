# Post-alpha roadmap — ordered by the functionality worth building

**Date:** 2026-08-07
**Status:** accepted, 2026-08-10. Items 2, 3, 4 and 5 are built; item 1
(comparison and synchronisation) is next, and is the largest thing left.
**Context:** every milestone M0–M5 is met and packaging now turns a tag into
downloadable artefacts. What remains is not debt — it is the part of
specification §17 that was never built. This orders it by what the software
gains, and says for each what already exists, because that is what decides
whether a feature is a week or a milestone.

Nothing here is scheduled. The point of the order is that each item makes the
next one cheaper or more obviously worth doing.

---

## 1. Directory comparison and synchronization

**§17.** Compare panes by metadata or hash, produce an approved one-way or
two-way plan.

The largest missing capability, and the one an orthodox file manager is judged
on. It is also the one whose machinery is most nearly complete: two panes with
independent listings, first-class selection that survives sorts and refreshes,
a task scheduler with progress and cancellation, a journal with undo, a policy
gate, and `sha2` already in `norte-core`'s dependency tree — comparison by hash
costs no new dependency.

What has to be built is the middle: a comparison that streams over two
providers without holding both listings in memory, a plan as a first-class wire
type (so the CLI and an agent can produce and approve one, not just the TUI),
and a result the user can operate rather than read — the diff belongs in a
virtual pane, the way search results already do.

The interesting design question is what "same" means across providers that
disagree about what they can tell you: mtime resolution differs, S3 has an
ETag that is a hash only sometimes, an archive has neither owner nor mtime you
should trust. The honest answer is that the comparison declares its own
confidence, and the plan records which criterion produced each decision.

**Depends on nothing. Unblocks:** a real answer to "did the copy work".

---

## 2. Batch rename, and the executor AI rename was supposed to feed

**Built, 2026-08-09** — the executor, not the rules engine. `fs.rename_batch`
plans and executes as one transactional unit with whole-plan collision preview,
permutation support and single-step undo (proto 0.36.0, ADR 0042); AI rename now
feeds it. What remains of this item is the rules engine on top — counters,
slices, regex, case, cleanup — plus CLI/MCP surfaces and #121.


**§17.** Counters, slices, regular expressions, case changes and character
cleanup, with collision preview and transactional undo. "AI rename feeds the
same executor."

Today that sentence is inverted: AI rename exists and the executor does not.
`apply_ai_rename` walks the approved pairs and submits one `fs.move` per pair,
in plan order. Three things follow, and all three are visible to a user:

- **No transaction.** The fifth move failing leaves four done. Undo is
  per-task, so undoing the batch means undoing four things by hand.
- **No collision preview.** Each move meets the engine's collision handling on
  its own; the plan as a whole is never checked against itself.
- **Chained renames cannot work.** `a→b, b→c` collides on the first move even
  though the plan is perfectly consistent. Any rename that permutes names — the
  common case for "number these episodes correctly" — fails.

So the batch-rename engine is not a new feature bolted next to the AI one; it
is the thing that makes the AI one correct. Build the rules engine (counters,
slices, regex, case, cleanup) and the planner underneath it: cycle detection,
topological ordering with temporary names where a permutation demands one,
whole-plan collision preview against the destination's real case-sensitivity,
and a batch that lands in the journal as ONE undoable unit.

**Closes #121** on the way (AI rename over the first-class selection), because
the executor takes a selection rather than a directory.

**Depends on nothing. Unblocks:** honest undo for anything that moves many
files at once, which item 1 also wants.

---

## 3. Volumes, mounts and drive switching

**Built, 2026-08-10** — enumeration, free space and drive switching; ejection is
not built and is its own issue. `norte-core::volumes` answers as a HOST service
rather than a `Provider` method, `host.volumes` carries it (proto 0.38.0, ADR
0047), the daemon answers it only to a human, and both frontends have the
picker. Closes #131. Linux is verified; macOS and Windows are written and
type-checked against real targets but run on no machine here. New debt: #148
(eject), #149 (the pre-copy free-space check), #150 (macOS volume labels need
`getattrlist`).

**§17.** Enumerate platform volumes, show free space, support removable media
and safe ejection, expose drive switching as commands.

Norton Commander had `Alt+F1`/`Alt+F2` and every orthodox manager since has
had them. Nothing in the tree enumerates a volume or reports free space today —
`statvfs` appears nowhere.

Small, self-contained, and platform-shaped: a `Volumes` capability on the
provider trait (local answers it, remote providers decline), a wire type, a
picker in both frontends. Free space is also the answer to a question a copy
should be asking before it starts and currently does not.

Eject is the part that deserves care: "safe" means the write cache is flushed
and nothing of ours holds the mount, and saying so wrongly loses data.

**Depends on nothing.**

---

## 4. Shell integration

**Built, 2026-08-10** — all of it except an embedded pty, which was never the
plan. `norte shell-init bash|zsh|fish` prints a cd-on-quit wrapper that reads a
NUL-delimited file (a directory is bytes, and `$(...)` eats trailing
newlines); `ntc --pick` writes the selection NUL-terminated to stdout, which
required moving the whole interface onto the controlling terminal; and `F9`,
`Ctrl+O` and the command line work by suspending the TUI, with the GUI
launching the system terminal emulator instead. Closes #135. New debt: #142
(no persistent subshell behind `Ctrl+O`), #143 (bracketed paste), #144 (an
opener still runs without the pane's cwd).

**§17.** cd-on-quit wrappers, file-picker mode, opening a terminal in the
active pane.

The smallest surface on this list and the one that changes daily use the most:
it is what makes a terminal file manager something you stay in rather than
visit. None of it exists.

- **cd-on-quit**: the binary writes the final directory somewhere the shell
  wrapper reads, and we ship the wrapper for bash, zsh and fish. The trap is
  that a directory is BYTES — the wrapper must survive a non-UTF-8 path, which
  means a NUL-delimited file and not an `echo`.
- **Picker mode**: `ntc --pick` prints the selection and exits, so other tools
  can use it to choose files. Nearly free once selection is first-class, which
  it already is.
- **Terminal in the pane**: spawn the user's shell with the pane's directory as
  cwd. Only meaningful for `file://`; on a remote pane it must say so rather
  than open a shell somewhere surprising.

**Depends on nothing. Cheap.**

---

## 5. The keyboard of the managers people already know

**Built, 2026-08-10** — the whole item. A shared catalogue where every command
declares itself live or planned-with-a-reason, the `mod+` alias, seven presets
(Total Commander, Krusader, Norton Commander, Far, plus the three that already
existed), numeric counts, a which-key overlay, a reference sheet generated from
the active preset, and a shortcut editor in both frontends (ADRs 0043/0044/0045).
Its side effect is the ranked list this roadmap now works through: the planned
entries are issues #131–#140, and each one is a key a preset already shows a
user. Remaining debt: #141.

**§12** bundles orthodox, Vim and CUA presets. Three is not the sector: someone
arriving from Total Commander, Krusader, Norton Commander or Far Manager has to
relearn the keyboard, which is the one thing an orthodox file manager should
never ask of them.

The engine is in good shape and in the right place — `norte-frontend::keymap`,
shared by both frontends, with layers, contexts, multi-key sequences and a
prefix-free guarantee that makes resolution timing-free and testable (ADR 0006).
What blocks the four presets is not the engine's resolution model but its
honesty model, and it is worth stating plainly because it is the whole cost:

- **The command catalogue belongs to each frontend, separately.** `COMMANDS`
  lives twice, once in the TUI and once in the GUI, and the two lists differ. So
  the same preset resolves differently in each, silently. This has already shipped
  as a bug: F1 did nothing in the GUI for several releases because the shared
  presets bound it and the GUI's private list did not name it.
- **A binding to a command norte has not built is a load error, or vanishes.**
  Around a third of a faithful Total Commander preset names commands that do not
  exist yet — `Alt+F1` selects a drive, and volumes are item 3. A preset cannot
  be both faithful and loadable until "not built yet" is a declared state with a
  reason, rather than a typo or a silent deletion.

So the work is a shared catalogue where each command declares whether it is live
or planned-with-a-reason, one modifier alias (`mod+` = Cmd on macOS, Ctrl
elsewhere) so a preset stays one file, and then the four presets — Far included
deliberately, because its full F1–F12 × four-modifier matrix is the hardest case
the engine will ever be asked to carry. Numeric counts (`5j`) come with them.

On top of that the keyboard finally becomes discoverable: a which-key overlay on
a pending prefix, a reference sheet generated from the *active* preset rather
than written by hand — unavailable keys in grey, with the reason — and a
shortcut editor in settings that shows the collision instead of making the user
find it by reloading TOML.

A pleasant side effect: the planned entries, each with an issue, are an honest
ranked list of what norte still owes an orthodox user.

**Design:** `docs/superpowers/specs/2026-08-09-keymap-catalogue-and-presets-design.md`
(decomposed K1 catalogue → K2 counts and presets → K3 surface).
**Depends on nothing.** Gets better as items 1 and 3 land, because the greyed-out
keys turn on.

---

## 6. Git status as the official columns plugin

**§17.** "Ship status as an official columns plugin, not a Git client in the
core."

M4's exit criterion is "a third party can ship a plugin without changing the
core", and the columns interface is wired end to end — declared columns,
approval, per-pane values, cells in both frontends. What is missing is the
proof: a plugin somebody actually wants.

Git status is that proof. It is also the honest test of the interface's
performance story, because status for a large repository is not free and the
plugin must be able to answer late without stalling a listing — which the
per-pane value fetch already allows and nothing has yet exercised under load.

If the interface turns out to be wrong, it is far better to learn it here than
from a third party.

**Depends on:** nothing, but is most valuable AFTER item 1, because a compare
between a working tree and a branch is the natural next thought.

---

## 7. Directory watching in the graphical frontend (#106)

The TUI half landed: `notify` over the visible `file://` directories, a
debouncer with a real floor between refreshes, and a documented degradation to
mtime polling when the inotify watch count runs out. The GUI is still blind to
external changes.

Not new design — the model is written and the pitfall is already handled once.
It is the second half of a feature that is currently half-true in the release
notes.

**Depends on nothing. Closes #106.**

---

## 8. The filesystem edge cases the spec names

**§17.** Explicit symlink policy, cycle detection, sparse files, Windows
reparse points, bounded retry for locked files.

Correctness rather than capability, and the reason it sits here rather than
first is that items 1 and 2 both need the first two of them and will force the
design anyway: a comparison that follows symlinks into a cycle never ends, and
a recursive copy needs to have decided.

Sparse files matter the moment somebody copies a VM image. Bounded retry for
locked files is what makes Windows usable at all — and is untestable here while
CI is off, which is its own decision (see the end).

---

## 9. Local observability

**§17.** Structured tracing by task and session, rotating local logs,
inspectable task traces; nothing leaves the machine.

`#[instrument]` is on the effectful core functions already, so the events
exist. What does not exist is anywhere for them to go: no rotating appender,
no way for a user to hand over what happened.

This is what makes a bug report possible from someone who is not us — which a
stable release needs more than it needs another feature. It pairs with
`norte doctor`, which already exists and would be the natural place to say
"the log is here".

---

## 10. Daemon lifecycle hardening

**§17.** Start on demand, shut down after configurable idle time, upgrade
gracefully, authenticate local peers, never run as root, loopback TCP with a
token.

Idle shutdown and on-demand start work. Two pieces are missing and only one is
interesting: **graceful upgrade** (a new daemon takes over without dropping the
sessions of a running GUI and TUI) and **loopback TCP with a token**, which
only matters if a client should ever be somewhere the socket is not.

The GUI and TUI already share a daemon session, so upgrade is the piece that
protects something real.

---

## 11. RAR, read-only, by delegation

**Product decision 5.** Read-only RAR through an installed `unrar` or `7z`,
with no non-free code in the dependency graph.

Small and genuinely wanted — RAR is what a decade of downloads is in. The
archive provider composition already exists (ADR 0018), so this is a delegating
provider plus the honest failure when the executable is absent.

The interesting constraint is rule 9: the delegate is an external program, so
it gets a path and a pipe, never the user's whole filesystem.

---

## 12. Packaging, tier two

**§17.** Signed artifacts, common package managers, update notification.

Deliberately out of scope of the packaging work just merged, and the reason
stands: all three need infrastructure decisions rather than code — where the
signing key lives, who maintains the AUR and Homebrew formulae, and what a
notification is allowed to do (notify, never install unattended).

Worth doing when there is a release cadence to attach them to.

---

## Not on this list, on purpose

**CI.** Off for billing, so there is no three-OS matrix and the gate is one
developer's Linux machine. That is not a feature to build; it is a decision to
make, and it decides whether items 3, 8 and 10 can honestly claim Windows and
macOS support. Two open issues — a Windows trash path that can destroy
non-recyclable items (#25) and the missing named-pipe transport (#33) — are
unverifiable until it comes back.

**The four upstream-blocked issues.** #37 (russh-sftp decodes names lossily),
#48 (opendal trims paths), #114 and #115 (attributes the libraries discard).
Each needs a patch to somebody else's crate; none is closable here.

**A 1.0 with the specification unchanged.** Items 1–4 are §17 capabilities. A
release that calls itself stable while four of them are absent is either a
different version number or a smaller specification, and that is a product
decision rather than an engineering one.
