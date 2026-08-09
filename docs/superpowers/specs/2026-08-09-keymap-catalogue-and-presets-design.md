# Keymap presets for the managers people already know — design

**Date:** 2026-08-09
**Status:** approved. **K1 built** (2026-08-09, `9b750c0..9b37cb0`, ADR 0043,
`just ci` and `just gui-ci` green). **K2a built** (2026-08-09, `5d770e2..27a565e`,
ADR 0044). **K2b built** (2026-08-09, `562dbbc..e4528f5`, `just ci`/`just gui-ci`
green — see its section below for the numbers). K3 still a sketch.
**Related:** ADR 0006 (keymap resolution), specification §12, roadmap item 5

## The problem

norte ships three keymap presets — `orthodox` (the default, "classic mc"),
`vim` and `cua`. Somebody arriving from Total Commander, Krusader, Norton
Commander or Far Manager has to relearn the keyboard, which is the one thing an
orthodox file manager should never ask.

Shipping those four presets is not a matter of writing four TOML files, because
two things in the current engine make a faithful preset impossible:

**A binding to a command norte does not implement is either a load error or a
silent disappearance.** `Effective::build_for` rejects an unknown `run` name
with `UnknownCommand`; `build_for_subset` filters it out instead, and says
nothing. Both are wrong for a preset whose whole purpose is fidelity to another
program: Total Commander's `Alt+F1` selects a drive, and norte has no volume
enumeration (roadmap item 3). Around a third of a faithful TC preset names
commands norte has not built yet.

The silent-filtering branch has already caused this exact bug in production
code. From `crates/norte-gui/src/keymap.rs`:

> H3f: the three shared presets have bound `f1` → `app.help` since H3a, but
> this table did not list it — and `Effective::build_for_subset` filters by it,
> so the binding was dropped and F1 did nothing, silently.

**The command catalogue belongs to each frontend, separately.** `COMMANDS` is a
macro-generated list in `crates/norte-tui/src/keymap.rs` and a second, hand-written
list in `crates/norte-gui/src/keymap.rs`; each frontend passes its own as
`known_commands`. The two lists differ — the GUI has `task.next`, `task.prev`,
`task.dismiss` and `pane.copy-path`, the TUI has `pane.hotlist`, `pane.history`,
`pane.search` and the whole `dialog.*` context — so **the same preset resolves
differently in the two frontends, and the difference is invisible to the user.**
With four new presets full of not-yet-available keys, that invisibility stops
being a wart and becomes the feature's central lie.

## Decomposition

Too large for one plan. Three, in order:

| | scope | why it must come first |
| --- | --- | --- |
| **K1** (this spec) | Shared command catalogue, declared availability, `mod+` alias per OS | Without an honest catalogue a Total Commander preset cannot load at all |
| **K2** | Numeric counts, the four presets, preset validation in the gate | The presets are what prove the catalogue against real cases rather than invented ones |
| **K3** | Which-key overlay, per-preset reference sheet (F1 and `ntc keys`), shortcut editor in settings | All three render catalogue data; building them first means painting data that does not exist |

K2 and K3 are sketched at the end so K1's interfaces are designed against them,
not discovered by them.

## K1 — the design

### The catalogue is one table, and it is shared

The command vocabulary moves to `norte-frontend` as the single source. Each
entry carries what every consumer needs and nothing more:

```rust
/// One command in the shared vocabulary. Bindings name these; frontends
/// implement a subset of them; the help and the reference sheet describe them.
pub struct CommandDef {
    /// Wire-stable name, e.g. `pane.copy`. The same string a preset binds,
    /// the palette shows, and `help_id` mangles into a Fluent id.
    pub name: &'static str,
    /// Whether a numeric count prefix means anything here (K2). `cursor.down`
    /// yes, `app.quit` no.
    pub counts: bool,
    /// Why a binding to this name may resolve to nothing.
    pub status: Status,
}

pub enum Status {
    /// At least one frontend implements it. Which ones is not the catalogue's
    /// business — each frontend declares its own set.
    Live,
    /// A preset may legitimately name it; norte has not built it yet. Carries
    /// the reason a user is owed and the issue that tracks it.
    Planned { reason: &'static str, issue: u32 },
}
```

`Planned` is what makes a faithful preset possible. `pane.select-drive` enters
the catalogue as `Planned` with the reason "volume enumeration" and the issue
that tracks it, on the same day `total-commander.toml` binds `Alt+F1` to it — so
the binding is legal, the key is documented, and pressing it says why it does
nothing. K2 opens those issues as it writes each preset; none is invented here.

The two frontend lists stay, narrowed to their real job: *which catalogue
entries this frontend implements*. The TUI's `commands!` macro keeps generating
its `Command` enum and its exhaustive `dispatch` match (#112) — that safety is
untouched. What changes is that the macro's names are checked against the
catalogue at compile time, so a frontend cannot invent a command the catalogue
has never heard of, and the two frontends cannot drift apart by accident.

### An unavailable binding survives, marked

Today an unimplemented binding is dropped during the build. It will instead
reach the effective map carrying its unavailability, and resolution grows a
third outcome next to `Run` and `Pending`:

```rust
pub enum Resolution {
    Run(String),
    Pending(usize),
    /// The key is bound, and the command behind it is not available here.
    /// The frontend says so; it never does nothing.
    Unavailable { command: String, why: Unavailable },
    None,
}

pub enum Unavailable {
    /// In the catalogue as `Planned`. Carries the reason and issue.
    NotBuilt { reason: &'static str, issue: u32 },
    /// `Live`, but this frontend does not implement it — e.g. `pane.hotlist`
    /// in the GUI. Carries which frontends do.
    NotHere,
}
```

Three consequences, all deliberate:

- **Pressing an unavailable key produces a message**, in the status bar,
  routed through Fluent like every other user-facing string. Never a silent
  no-op.
- **Unavailable bindings participate in the prefix-free check.** A preset that
  binds `g` (unavailable) and `g g` (available) is still a load error. The rule
  from ADR 0006 is about the shape of the map, not about what happens to run.
- **The reachability pin survives.** The GUI's
  `todo_comando_gui_es_alcanzable_desde_el_preset_default` asserts every GUI
  command is reachable from the default preset. It keeps asserting exactly
  that, over `Live` commands the GUI implements — `Planned` entries are outside
  its question.

`UnknownCommand` keeps its meaning and stays an error: a name absent from the
**catalogue** is a typo, and typos must fail loudly at load. The distinction is
the whole point — "norte has not built this yet" and "you misspelled it" stop
being the same event.

### `mod+` is the only per-OS mechanism

A preset stays one file. The chord parser gains one modifier alias:

- `mod+c` is Cmd on macOS and Ctrl everywhere else.
- `ctrl+c` stays literal Ctrl on every platform, for the presets that mean it.

No per-OS sections, no duplicated files. If a preset ever needs a whole key
changed on one platform, a `[target.macos]` override table can be added then —
not now.

**One honest limitation, stated up front.** The TUI cannot observe Cmd.
`crates/norte-tui/src/keymap.rs` notes it at the crossterm adapter: super/meta
are not reported without `PushKeyboardEnhancementFlags`, which norte does not
enable. So on macOS `mod+` resolves to Cmd in the GUI and to Ctrl in the TUI,
and the reference sheet (K3) prints which one it got rather than promising a
key the terminal will never deliver. Enabling the enhancement flags is a
separate decision with its own compatibility surface; it gets an issue, not a
paragraph here.

### The sacred keys are not negotiable

Specification §12 makes `Tab` pane switching sacred. A preset may not rebind a
sacred key: doing so is a load error, and the preset documents the difference
from its original instead of quietly diverging. This costs almost nothing in
practice — all four target managers use `Tab` for the same thing.

### Files

`crates/norte-frontend/src/keymap.rs` is 2 220 lines and would reach roughly
3 500 with this. It splits into a module directory along the seams it already
has internally:

```
keymap/
  mod.rs         re-exports, the public surface today's callers import
  chord.rs       Chord, Mods, KeyCode, parsing, the mod+ alias
  catalogue.rs   CommandDef, Status, the shared table
  layer.rs       KeymapFile, layer merging, prepend/append
  effective.rs   Effective, the prefix-free check, build_for*
  resolve.rs     Resolver, pending state, Resolution
```

Pure movement, no behaviour change in the same commit as the split.

### Testing

- A property test that the catalogue and each frontend's implemented set agree:
  every implemented name is in the catalogue, no `Planned` name is implemented.
- Every bundled preset builds against the catalogue, in both frontends, for
  every `Screen` — the existing preset tests extended with the availability
  dimension rather than replaced.
- A binding to a `Planned` command resolves to `Unavailable::NotBuilt` carrying
  the right issue, and the message renders in both locales.
- `mod+` parses to Cmd under a macOS target and Ctrl otherwise, asserted
  without a real macOS by testing the resolution function rather than the
  platform.
- The prefix-free check rejects an unavailable/available prefix pair.
- Fluent coverage: every `Planned` reason has a string in both locales, pinned
  the same way `help_id` coverage already is.

## K2 — counts and the four presets

Split in two, because the engine and the data fail in different ways and the
presets should be written against an engine that has stopped moving.

### K2a — the engine

**Numeric counts.** `Resolution::Run` becomes `Run { command, count:
Option<u32> }` and **the frontend repeats the dispatch**. That leaves the ~80
commands' signatures untouched and works for everything the catalogue marks
`counts: true` without a per-command arm — the alternative, passing the count
into `dispatch`, needs an arm per command and a command that forgets to read it
returns to exactly the silence K1 removed. Four rules:

- The count is capped at four digits. `5000j` on a 200-entry listing stops at
  the end; it does not hang.
- A count over a command whose `counts` is false is **not swallowed** — the
  status bar says it was ignored.
- A digit key bound in a context whose preset enables counts is a **load
  error**, not silent precedence. Same spirit as the prefix-free rule:
  conflicts surface when the file loads, not when a finger slips.
- Counts are opt-in per preset (`counts = true`), and only `vim` sets it. Far
  has no numeric prefix either — it spends `Ctrl+1..Ctrl+0` on panel view
  modes — so it does not set the flag; neither do Total Commander, Krusader,
  Norton or CUA. Enabling it on any of them would steal digit keys their
  originals spend elsewhere.

**The sacred keys.** A preset that rebinds `Tab` is a load error (`SacredKey`,
specification §12). K1 deliberately left this out because a prohibition with no
violator ends up untested; K2a is where the violator becomes possible.

**Two debts settled here**, both raised by K1's reviewers:

- `build_effectives_with` becomes `Result`. It is the GUI's error-recovery
  path, and K1's shadowing decision gave it a live panic route.
- `means_command` stops round-tripping every binding through
  `Display`/`parse_chord` on every key event — about 140 allocations per
  keystroke today, about 450 once four more presets exist.

### K2b — the four presets

**Built** (2026-08-09, `562dbbc..e4528f5`, `just ci`/`just gui-ci` green).

Each file records the program, its version, the source, and the date it was
transcribed, so that when it ages the staleness is dated rather than unknown.

| preset | source | status | bindings |
| --- | --- | --- | --- |
| `total-commander.toml` | `KEYBOARD.TXT` from Total Commander **11.58** (2026-07-01), 141 entries | First-hand. Not published on the web — it ships inside the installer, which is a zip SFX containing `INSTALL.CAB`; the text file is extracted without running anything | 68 |
| `krusader.toml` | KDE handbook, Key-Bindings chapter (`docs.kde.org/trunk_kf6/en/krusader/krusader/key_bindings.html`), ~150 entries | First-hand, fetchable | 64 |
| `far.toml` | `far/FarEng.hlf.m4` in the official `FarGroup/FarManager` repository | First-hand, fetchable — it is the source the program's own help is compiled from | 53 |
| `norton.toml` | none | **No first-hand source.** `NC.HLP` is internally compressed and the distribution ships no plaintext key list. Transcribes the uncontroversial core (F1–F10, `Ctrl+O`, `Ctrl+U`, `Alt+F1`/`Alt+F2`, `Insert`, grey `+`/`-`, `Ctrl+\`) and says so in its header. The fabrication risk is low here in a way it is not for Total Commander's modifier matrix | 35 |

(For scale: the three native presets — `orthodox`, the `dialog_from` donor
every K2b preset inherits, plus `vim` and `cua` — sit at 85, 97 and 85
bindings respectively.)

Far is deliberately included: its full F1–F12 × four-modifier matrix is the
hardest case, so an engine that carries Far carries the rest. Double Commander
is a near-duplicate of Total Commander and is a cheap follow-up, not first-cut.

**`Planned` entries point at the capability, not the command.** The four
presets name roughly a hundred commands norte does not have, but those are
nine NEW capabilities — volumes' two siblings joined the one that already
existed (`pane.select-drive`, issue #131), then archive write, editor,
directory compare/sync, shell, tree view, tabs, sort, properties, and
connections (issues **#132–#140**, one new issue per capability). Twenty-eight
`Planned` catalogue entries in total, several commands pointing at the same
issue apiece — a hundred issues would be noise; nine is the actual work, and
it already lines up with the roadmap's items.

The presets do **not** carry their own `[dialog]` section: they inherit
`orthodox`'s, because norte's dialogs are norte's, not the imitated program's.

## K3 — the surface (sketch)

- **Which-key overlay.** On a pending prefix, a panel of valid continuations.
  The `pending` state already exists in the resolver; this is mostly painting.
  It gains value with counts, because it shows the `5` that is still stuck.
- **Reference sheet per preset**, generated from the *active* preset rather
  than written by hand: every command, its key, and unavailable keys in grey
  with their reason. Same text from `ntc keys` in the CLI — `norte-cli` already
  depends on `norte-frontend` and `norte-help`, so no new crate.
- **Shortcut editor in settings.** Capture a key, show what it collides with,
  write the user's `append_keymap` layer. Today rebinding means hand-editing
  TOML and discovering the conflict on reload.

## Out of scope, on purpose

- **Modal states** (vim/ranger visual mode). Rejected during design. The
  contexts and a mode stack would carry it later; nothing here forecloses it.
- **Esc-as-Meta** (mc over a terminal that eats Alt). Rejected: it collides
  with ADR 0006's "Esc clears pending", and buying it back means an opt-in
  exception in the one rule that keeps resolution timing-free.
- **An installable preset catalogue.** Four TOML files are ~30 KB embedded;
  a catalogue needs distribution, signing and trust, which is a milestone of
  its own.
- **Importing another program's config file** (`wincmd.ini`,
  `krusaderui.rc`). Third-party, unspecified, independently versioned formats.
- **The command catalogue on the wire.** No agent needs it yet (YAGNI); it
  would cost a protocol bump and a guardian review.

## Risks

- **`Planned` becomes a graveyard.** An entry with an issue nobody closes is a
  key that never works. Mitigation: the reference sheet lists them, so they are
  visible to users, not just to us.
- **Catalogue churn.** Every new command now touches a shared table. That is
  the intent — it is the cost of the two frontends not drifting — but it makes
  the table a merge point.
- **Preset fidelity rots.** When Total Commander moves a shortcut, our file
  lies. Mitigation: each preset header records the program version it was
  transcribed from, so the staleness is dated rather than unknown.
