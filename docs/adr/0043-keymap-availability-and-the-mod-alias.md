# 0043 - Keymap availability is declared, and `mod+` is a process policy

- Status: accepted
- Date: 2026-08-09
- Decision makers: Oscar González
- Related: specification section 12 (keymap, sacred keys); ADR 0006 (keymap
  resolution semantics — **superseded in part**, see below), ADR 0007 (keymap
  hot reload), ADR 0035 (shared `norte-config` layer loading), ADR 0040 (help
  corpus). Design:
  `docs/superpowers/specs/2026-08-09-keymap-catalogue-and-presets-design.md`;
  plan: `docs/superpowers/plans/2026-08-09-k1-keymap-catalogue.md`.

## Context

K2 will ship faithful Total Commander, Krusader, Norton Commander and Far
presets. A faithful preset is the whole point: a user arrives with twenty years
of muscle memory, and a preset that quietly drops the third of it norte has not
built is not a migration path, it is a trap.

Before this ADR the engine had exactly two answers for a binding whose command
this build cannot run, and both were wrong for that job.

**Reject the load.** ADR 0006 decided that a binding must name a command in
`known_commands` or fail. Roughly a third of a Total Commander preset names
commands norte has not built (drive selection, the compare-directories family,
the FTP panel). Under that rule the preset cannot exist at all.

**Drop the binding.** `Effective::build_for_subset` existed precisely to escape
the first rule: it filtered away bindings the caller's list did not name, in
silence. That is the H3f bug. The three shared presets have bound `F1` to
`app.help` since H3a; the GUI's private `COMMANDS` list did not name it; the
filter deleted the binding; **F1 did nothing in the GUI for several releases**
and nothing anywhere said so. The bug was not a typo — it was the mechanism
working as designed.

Underneath both sat the real defect: **each frontend owned the vocabulary.**
`known_commands` was a private list per frontend, and the same preset therefore
resolved differently in the TUI and the GUI with nothing to notice the drift.
Measured before the change: the union of the two lists was **81 names**, of
which only **39 were known to both**. 38 were TUI-only — including all 22
`dialog.*`, which the GUI genuinely cannot implement because it never builds a
`Screen::Dialog` effective — and 4 were GUI-only (`pane.copy-path`,
`task.next`, `task.prev`, `task.dismiss`). When the filter was removed and the
GUI's Browse keymap rebuilt against the shared presets, it turned out to have
been **silently dropping 16 of its 47 bindings**.

A second, smaller problem blocks the same presets. Every target manager's macOS
build uses ⌘ where its Windows build uses Ctrl. Writing that as two preset
files per manager doubles the surface a contributor must keep in step, for a
difference that is one modifier.

## Options considered

**A. Keep the reject-or-drop pair and give each frontend a bigger list.**
Cheapest change, no new types. Rejected: it treats the symptom. The two lists
would drift again the first time one frontend gained a command, and drift is
invisible by construction — that is what produced a dead F1 and 16 dropped
bindings. It also still cannot express a preset binding a command *nobody* has
built, which is the case K2 is made of.

**B. Make every unknown command lenient everywhere.** Delete
`UnknownCommand` and let any name bind to nothing. Simple and uniform.
Rejected: a typo and an unbuilt command become the same event. `pane.reneme`
would load clean and do nothing forever, which is precisely the failure mode
this work exists to end — the user gets silence either way, so the distinction
that matters to them is destroyed to save one lookup.

**C. One shared catalogue; each frontend declares which entries it implements;
a binding to a name in the catalogue but not implemented here survives, tagged
with why.** More types, one table to maintain by hand, and the resolver grows a
third outcome that nine call sites must handle. Chosen: it is the only option
that keeps a typo fatal while making an unbuilt command expressible, and it is
the only one where drift between the two frontends fails a test instead of
changing behaviour.

For the modifier: **two preset files per manager** (rejected — duplicated
surface), **a per-build `cfg(target_os)` constant** (rejected — untestable off
the platform, and a Linux GUI under a mac-style keyboard layout is a real
configuration), or **an input alias resolved by a process-wide policy**
(chosen).

## Decision

### 1. An unavailable binding is kept and tagged, not rejected and not dropped

`Availability` rides on every binding of the effective keymap:

- `Here` — bound and runnable.
- `NotBuilt { reason, issue }` — the catalogue says `Planned`; norte has not
  built it anywhere.
- `NotHere` — the catalogue says `Live`, but *this* frontend does not implement
  it.

`build_for_subset` and its silent filter are **deleted**. `build_for` is the
only builder, and its signature is unchanged, so the ~60 existing call sites
kept compiling.

The two states are distinct because the user is owed different sentences.
`NotBuilt` can name an issue to follow; `NotHere` cannot, and instead tells a
user of one interface that the other has the command. Collapsing them into one
"unavailable" would have printed an issue number for a command that is finished
and running in the other frontend.

### 2. Absent from the catalogue is still a hard load error

`KeymapError::UnknownCommand` stays fatal — in a preset and in a user layer
alike. **A typo and an unbuilt command must not be the same event.** The whole
value of decision 1 is that the user learns *why* a key does nothing; a
misspelling that loaded clean would put them back where they started, with a
key that does nothing and a keymap that claims to be fine.

Consequently `check_binding` takes neither the binding's origin nor a
strictness mode. Since K1 the verdict does not depend on WHERE a binding came
from, which is what let the lenient/strict split — the thing
`build_for_subset` was — disappear rather than be repaired.

`lua:<name>` bindings are the one exception, and a principled one: the Lua
command registry is populated at runtime, so a static catalogue can never know
it. A `lua:` binding validates only its name charset (`valid_lua_name`, the
single source shared with the Lua host, #88) and is always `Availability::Here`;
invoking a Lua command that was never registered is a runtime notice from the
frontend, not a keymap load error.

### 3. The catalogue is shared; a frontend declares only which entries it implements

`norte_frontend::keymap::catalogue::CATALOGUE` is the vocabulary: 81 `Live`
entries plus one `Planned` today. A frontend's `COMMANDS` list is now a
**subset declaration**, not a vocabulary — it answers "which of these do I
run", never "which of these exist".

Both frontends carry a test that every name they declare is in the catalogue
**and** is `Live`. Drift now fails the suite instead of changing resolution.

Each entry also declares `counts: bool` — whether a numeric prefix means
anything (`5j` moves five; `5` before `app.quit` is nonsense). It is recorded
now, unread, because the answer is known where the command is defined and
unknowable where a preset binds it. K2 consumes it.

### 4. An unavailable binding still participates in prefix-freedom, and still shadows

An unavailable binding takes part in the prefix-free check of ADR 0006, and a
higher-precedence unavailable binding still shadows a lower-precedence
available one.

**The shape of the map is a load-time property, independent of what this build
happens to run.** If an unavailable `pane` binding fell through to an available
`global` one, a Total Commander user pressing a key their manager used for
directory compare would get whatever norte's `global` layer put there — a
surprise action instead of an answer. Falling through is a wrong action; being
told is an answer.

Two existing tests pinned the opposite behaviour. They were **inverted, not
deleted**: the old expectation is still exercised, now as the thing that must
not happen.

### 5. `Status::Planned.reason` is a Fluent id, not prose

The catalogue is a `const` table in a pure crate and must not carry a locale. A
`Planned` entry names a Fluent id (`keymap-reason-volume-enumeration`); the
frontend translates it when it builds the sentence, through the shared
`unavailable_message`, so the two frontends cannot word it differently.

A test resolves every `Planned` reason in **both** locales, because an
untranslated id renders as the raw id: an unbuilt key would then explain itself
as `keymap-reason-...`, which is worse than saying nothing. A second test pins
that no `Planned` entry has an empty reason or a zero issue — a promise nobody
can chase is the exact failure this state exists to prevent. Issue numbers are
never invented; K2 opens one per preset command it cannot bind.

### 6. `mod+` is an input alias resolved by a process-wide policy

`mod+` resolves to whatever `ModKey` this process fixed at startup: `Ctrl` by
default, `Cmd` where a frontend can observe ⌘. One preset file serves macOS and
Linux/Windows. `cmd+` remains available and literal, for a preset that means ⌘
and nothing else.

The policy is a process-wide `OnceLock`, set once by the frontend before any
keymap is built — **not** a `build_for` parameter. `build_for` has ~60 call
sites, and threading a platform fact through all of them would put a decision
that is uniform for the life of the process into 60 places that could disagree.
This is the same justification `norte_i18n`'s global language already runs on:
a platform fact decided once, read everywhere, never changed mid-run.

`set_mod_key` returns `false` rather than panicking if the policy was already
fixed to a different value, and there is deliberately **no reset**: `parse_chord`
reads it, so changing it mid-run would make two keymaps built from the same
file disagree. That constraint reaches tests too — including doctests, which
since the 2024 edition are merged into one binary and share the `OnceLock`. The
mapping is tested through `ModKey::apply`, which is pure.

`Display` never writes `mod`. `mod` is an *input* spelling; the canonical form
writes the physical modifier it became (`ctrl+x` or `cmd+x`), so a reader is
always shown the key they actually press rather than a name they cannot find on
their keyboard.

### 7. `cmd` sorts before `ctrl` in the canonical spelling

Required, not aesthetic: a chord carrying both must have exactly one spelling
for `parse(display(c)) == c` to hold, and that round trip is what the whole
chord layer is pinned on.

### 8. The repeated-modifier check sees through the alias

`ctrl+mod+x` under the `Ctrl` policy is the same key twice, and is
`KeymapError::BadChord`. The alias counts as what it resolves to. The
alternative — accepting it because the two tokens differ as text — would let a
preset ship a chord that is a duplicate on one platform and not on the other,
discovered by whoever runs the platform where it is broken.

### 9. The TUI cannot observe ⌘, and says so instead of pretending

crossterm does not report super without `PushKeyboardEnhancementFlags` (the
Kitty keyboard protocol), which norte does not enable. `Mods.cmd` is therefore
hard-wired `false` in the TUI's crossterm adapter, and `mod+` is Ctrl in the
TUI on **every** platform, macOS included.

The TUI deliberately does **not** re-export `ModKey`/`set_mod_key`. It cannot
honour any policy but `Ctrl`, so it should not be able to name one: an API that
accepts a setting it will ignore is worse than one that does not offer it.

### 10. `ModKey::Cmd` is unverified on real hardware

GitHub CI is off and nobody on this project has a mac. `ModKey::Cmd` is
verified by the purity of `ModKey::apply` and by the `cmd+` parse and
round-trip sweep — **not** by anyone pressing ⌘. This ADR does not claim macOS
support; it claims a mapping whose logic is tested and whose platform is not.
The first person to run the GUI on a mac is doing the acceptance test.

### 11. The token table of ADR 0006 grows `cmd` and `mod`

ADR 0006's accepted TOML token list (`0006-keymap-resolution.md:48`) is
extended with `cmd` (literal) and `mod` (alias). ADR 0006 itself is otherwise
untouched apart from the supersession note at its head: **ADRs are records, not
living documents.**

### 12. What this supersedes in ADR 0006

ADR 0006's rule that "a binding must name a command in `known_commands` or fail
the load" is superseded by decisions 1 and 2: a name absent from *this
frontend's* set is now a declared unavailability, and only a name absent from
the *shared catalogue* fails the load. Everything else in 0006 — layer merge
order, prefix-freedom without timeouts, Esc semantics, the preset bundle,
`plus` as the only spelling of `+` — stands unchanged.

## Consequences

**Positive.**

A faithful preset for another manager becomes writable: the third of it norte
has not built binds, loads, and explains itself when pressed. The dead-F1 class
of bug is structurally gone — there is no silent filter left in the builder, and
a frontend that drifts from the shared vocabulary fails a test. `bindings()`
still returns only what runs, so help rendering, the hint bar and the CLI's
`ntc keys` are byte-identical to before; `bindings_all()` is additive and is
what K3's per-preset reference sheet will consume. A preset ships as one file
for all platforms.

**Negative, and accepted.**

The catalogue is a hand-maintained `const` table, and adding a command means
editing it as well as the frontend that implements it. Two tests make the
omission loud, which is the trade: a compile-time list that a macro derived
would be tighter, but the two frontends do not share a command enum to derive
from.

`Effective::lookup` remains a linear scan over **every** binding, unavailable
ones now included, and K2 adds roughly 400 more across four presets. ADR 0006
mentioned a trie; this stays a scan until a measurement says otherwise, and the
number to measure against is now larger than it was.

`discarded_lua_bindings` is the last silent-drop mechanism in the engine: it
counts `lua:` bindings dropped from a project layer for security, but does not
name them. K1 deliberately left it alone — it is a *security* drop with a
distinct rationale, and the frontend does warn once — but it is the same shape
as the bug these five commits removed, and it is recorded here so it is not
mistaken for having been reviewed and blessed.

`ModKey::Cmd` and the GUI's ⌘ handling are unexercised on the platform they
exist for (decision 10).

The sacred-key rule of specification §12 — a preset may not rebind `Tab` — is
**not implemented**. It has no violator until the first preset exists, all four
target managers use `Tab` the same way, and a prohibition with nothing to
prohibit is how a rule ends up untested. It belongs to K2, with the first
preset.

## Alternatives considered

**A `#[cfg(target_os = "macos")]` constant instead of a runtime policy.**
Rejected: it cannot be tested from the platform that is not it — and with no
mac and no CI, "cannot be tested here" means "is not tested at all". A runtime
policy is exercised in both directions from one machine, and it also serves the
real configuration where a user on Linux drives a mac-style keyboard.

**A `ModKey` parameter on `build_for`.** Rejected on decision 6: ~60 call sites
for a value that is constant for the life of the process, with the failure mode
that two of them disagree and two keymaps built from one file resolve
differently.

**One `Unavailable` state instead of `NotBuilt` and `NotHere`.** Rejected on
decision 1: it would print an issue number for a command that is built and
running in the other frontend.

**Letting an unavailable binding fall through to the next layer.** Rejected on
decision 4: it converts "this key is not available" into a different action
happening. The two tests that pinned the fall-through were inverted to pin its
absence.
