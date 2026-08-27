# The fourth layer is one you pick

> Status: accepted, not built.
>
> norte has three configuration layers — system, user, project — and one
> screen. This document adds a fourth layer the reader **chooses by name**,
> and gives each choice its own live screen state. A profile is a named
> workspace: its arrangement, its keymap, its theme, its columns, its
> favourites, and where every panel was standing when you left it.
>
> It closes a request that was never filed as an issue and survives only as a
> type parameter in ADR 0058: `layouts: Map<ProfileId, Layout>`, annotated
> "one entry in v1". The map exists in the code
> (`norte-frontend/src/session.rs:110`) and has exactly one key, `"default"`.

## The differential that started this

Every ingredient of a workspace already exists, and none of them can be tied
together:

| ingredient | where it lives today |
| --- | --- |
| arrangement | 5 factory presets + `layouts/<name>.toml`, a picker with preview (`layout_picker.rs`), `[ui] layout`, `--layout` |
| keymap | 7 presets, `[keymap] preset`, `keymap.toml` layers |
| theme | `[ui] theme` — a preset name or a path |
| the rest of the look | `[ui]`: lang, fonts, quick-search, reduce-motion, mouse, confirm-quit, show-hidden |
| columns and sort | `[ui.columns]`, with per-scheme overrides |
| favourites | `[[hotlist]]` |
| where each panel is | `SessionBody.slots`, held by the daemon |

Two consequences follow, and they are the whole reason for this document.

**The reader can switch one ingredient at a time, and only one.** Picking the
`norton` arrangement changes not a single key, and the layout picker has a
dedicated field to say so out loud — `Row::shares_keymap_name`, whose rustdoc
reads "they are two distinct settings that share a name, and without the line
the coincidence is a trap rather than a convenience". That line is an apology
for a missing feature. This is the feature.

**There is one screen state for one human.** Photos and servers and a code
tree want different arrangements, different favourites and different starting
directories, and today they overwrite each other's panels every time the
reader switches task by hand.

## What a profile is

**A profile is a configuration-layer directory**, under the user's own
configuration directory:

```
~/.config/norte/
  norte.toml
  keymap.toml
  layouts/
  profiles/
    work/
      norte.toml
      keymap.toml
      openers.toml
      layouts/
    photos/
      norte.toml
```

It has exactly the shape of a layer, because it **is** a layer. `Layers.dirs`
is already `Vec<(PathBuf, Layer)>` (`norte-config/src/dirs.rs:30`, debt #75),
so a profile is one more entry in that vector rather than a new mechanism. A
profile can therefore carry its own `keymap.toml` and its own `layouts/`
without polluting the user's, and copying a profile between machines is
copying a directory.

The two alternatives were rejected for concrete reasons. **One file per
profile** (`profiles/<name>.toml`, symmetric with `layouts/<name>.toml`) can
only *name* a theme or a keymap that already exists in the user's directory —
too thin for a workspace. **A section per profile** (`[profiles.work]` inside
`norte.toml`) makes one file grow without bound, teaches the `toml_edit`
settings writer nested paths it does not need, and makes "copy this profile"
stop being a copy of anything.

### D1 — The profile layer sits above user and below project

```
system → user → PROFILE → project
```

Ascending precedence, as today. Picking a profile overrides the reader's own
`norte.toml` — that is what picking it is for. A trusted project layer still
wins over it, so ADR 0026 and #260 are untouched: nothing about this weakens
or strengthens what a foreign repository can do.

`standard_layers_on` grows a sibling that takes the active profile name and
splices its directory in after the `User` entry. Under a non-empty
`NORTE_CONFIG_DIR` — the hermetic test path — the profile directory resolves
under that override, so the seam stays hermetic.

`standard_layers_no_project()` exists for value-only core consumers
(`[archive]`, `[ai]`) and must **also** exclude the profile layer. It is
already documented as dropping a layer whose values are carved out anyway;
D2 makes that true of the profile layer too, and a core consumer has no
business knowing which profile a frontend picked.

### D2 — The profile's whitelist is positive, never "not project"

The project layer's carve-out is written as `*kind != Layer::Project`
(`norte-config/src/load.rs:1932` and `:1985`). Adding a fourth `Layer` variant
therefore grants it, **silently and by default**, everything the user layer
can do: `[daemon]` (the transport socket), `[ai]` (providers and enablement),
`[log]` (where this process writes), `[archive]` (the anti-bomb limits). That
would be a real hole, and it would be introduced by an `enum` variant rather
than by any line anyone wrote on purpose.

So the profile layer is gated by an explicit allow-list, and the two existing
`if`s become a small predicate per section rather than a negation:

**A profile MAY set** the whole of `[ui]` (theme, lang, fonts, quick-search,
reduce-motion, mouse, confirm-quit, show-hidden, `layout`, and the whole of
`[ui.columns]` with its `spec`, `scheme` and `sort`), `[keymap] preset`
together with its own `keymap.toml`, `[[hotlist]]`, its own `openers.toml`,
its own `layouts/`, and the new `[profile]` section of D3.

**A profile MAY NOT set** `[daemon]`, `[log]`, `[ai]`, `[archive]`, and
nothing at all touching the policy engine. A profile file that names one of
these gets a **warning naming the file and the key**, in the same channel the
project layer's warnings already use, and the key is ignored. Silence here is
what turns a picker into a permission escalator: a profile is chosen from a
list mid-session, and a configuration layer is not.

**And a profile MAY NOT carry an `init.lua`.** This was missing from both lists
in the first draft, and both reviewers found it in the same place: the Lua
loader gates on `layer == Layer::Project`, the same negation written in another
file, so `Layer::Profile` inherited the user layer's **code execution** the
moment the variant existed. The Lua host is deliberately unsandboxed — full
stdlib, `os.execute` included — and a profile is picked from a list while the
program runs, so it would be the escalator this decision exists to prevent, and
without even the trust prompt a repository's `init.lua` has to pass.

The line, stated once: **a profile declares, it does not execute.** Everything
D2 grants is a file the reader can open and understand — a theme, a keymap, a
set of openers, a layout, a list of favourites. `openers.toml` stays granted
even though it names external programs, because it is a declaration you can
read in full, its directory is one D4 confines to the reader's own config tree,
and a profile that could not choose how files open would not be a workspace.
`init.lua` is the other kind of thing, and it is refused with a message rather
than ignored in silence.

Connections are deliberately in neither list, because they are not
configuration: the daemon owns the connection list (ADR 0074, #264). A profile
cannot define one. Reopening or highlighting a connection on activation is a
separate, later question.

The policy scope is out for the same reason stated positively: it bounds how
far agents and plugins reach (rule 9), and a gesture that silently widens it
is not a gesture anyone can audit.

### D3 — `[profile]`: a name, and where the panels start

One new section, valid **only** in a profile's `norte.toml`, ignored with a
warning anywhere else:

```toml
[profile]
# Optional. Display only; the identity is the directory name.
title = "Work"

# Where each slot opens when this profile has no saved state yet.
# Keys are slot ids as they appear in the profile's layout.
[profile.start]
1 = "~/src"
2 = "~/src/norte"
```

This is what makes a freshly created profile useful on its first start rather
than two panels sitting in `$HOME`. It applies **only** when the session
carries no `SlotState` for that slot under this profile; a saved state always
wins, because the reader's last position is more current than a file written
weeks ago.

Paths here are paths, so they are bytes (rule 1). They are stored as TOML
strings, which forces a lossy step for non-UTF-8 — the same compromise
`[[hotlist]]` already makes, and the reason `start` is a convenience for the
first run rather than the mechanism that remembers where the reader was.

### D4 — The identity of a profile is its directory name, in bytes

Not a `String`. `layouts/` learned this twice: #245 (a layout name compared
byte-exactly and then recomposed into a filename, giving the wrong layout on
macOS and Windows) and #246 (`--layout` going lossy into a filename). A
profile name reaches the filesystem in exactly the same way, so it is an
`OsString` from the picker to the directory, and the comparison against the
list is byte-for-byte.

**Bytes are not the whole of it: the name is also checked before it is
joined.** A name reaches the filesystem through `profiles_dir.join(name)`, and
`Path::join` with an absolute path — or with a Windows drive prefix — replaces
the base entirely, while `..` climbs out of it. `--profile ../../../tmp/pwn`
would point a configuration layer at a directory nobody vetted, and an empty
name would make `profiles/` itself the layer. So a profile name is refused
unless it can be a single directory entry: not empty, not `.` or `..`, no `/`
`\` `:` or NUL, no trailing dot or space, and none of the Win32 device names.
This repository already had that check for layout filenames; it moves down into
`norte-config`, which is where the security-relevant copy belongs, and the
layout loader defers to it. Two copies of one rule diverge.

The name must then appear **byte-for-byte in the listing** of `profiles/`
before it is used. Letting the filesystem resolve it means `WORK` opens `work`
on macOS and Windows, which is #245 again — and the listing and the loader must
answer that question the same way, or a profile loads by name and never appears
in the picker.

The one place it must become text is the key of `SessionBody.layouts`, which
is a JSON object and therefore UTF-8 by construction. The mapping is explicit
and lossless-or-refused: a profile whose directory name is not valid UTF-8
**cannot carry live state**, is listed in the picker with that stated, and can
still be activated for its configuration. Refusing to list it would hide a
directory the reader created; storing it under a lossy key would let two
different profiles share one state.

Two consequences, both deliberate. Such a profile is not sticky either —
`SessionBody.active` is the same UTF-8 key — so activating it lasts for that
run and the next start falls back to the previous profile, which the picker
row says in advance. And its panels open at `[profile.start]` every time,
because no state is ever written for it.

### D5 — The live state is `SessionBody.layouts`, finally keyed

`SessionBody.layouts: BTreeMap<String, Node>` stops having exactly one key.
**The key is the profile name.** No new type, no new map — the hole ADR 0058
left, used.

`slots: BTreeMap<u32, SlotState>` stays a flat map, and **slot ids are
allocated per profile and never reused across profiles**. This is the whole
migration strategy: a flat map of unique ids needs no schema change to hold
several profiles' panels, and `prune` already marks untouchable every slot
that any arrangement mentions — with N profiles, that is the slots of all N.
The alternative, sharing ids across profiles, means two profiles overwriting
each other's directory and history, which is the bug this document exists to
fix.

One new field:

```rust
pub struct SessionBody {
    pub active: String,      // NEW: the sticky profile. "" = no profile.
    pub layouts: BTreeMap<String, Node>,
    pub slots: BTreeMap<u32, SlotState>,
}
```

The active profile is **state, not configuration**: it is what the reader was
doing, not what they decided. It lives in the session the daemon holds, and
not in their `norte.toml`, which stays a file they wrote.

**This costs no protocol bump.** `Session.body` is opaque to the core, and
`session.rs` already states the contract: "adding a field to the body is
bumping the `version` INSIDE the body, in the crate that gives it meaning".
`SCHEMA_VERSION` goes 1 → 2 in `norte-frontend`. No golden, no compatibility
window, no `protocol-guardian` pass. A v1 body read by v2 has no `active`,
which `#[serde(default)]` renders as "no profile", which is exactly true.

### D6 — The 1 MiB body, and what gets thrown away first

`SESSION_BODY_MAX` is 1 MiB and the core refuses an oversized `put` outright,
leaving the stored session **as it was** — it never truncates a document whose
schema it does not know. N profiles times their slots times two history trails
is the growth this design introduces, so the pruning order gets one more rule
among the existing ones:

1. Trim each slot's history from the OLD end (unchanged).
2. Slots mentioned by any profile's arrangement are untouchable (unchanged,
   now across all profiles).
3. **NEW**: profiles with saved state are capped. Over the cap, the
   least-recently-activated profile's state is dropped whole — its
   configuration directory is untouched, so the profile still exists and still
   works; it just starts from its `[profile.start]` again.
4. Then the existing orphan age sweep and orphan cap.

The **active profile is never swept**, at any step, by anything.

### D7 — A broken profile has three answers, not one

`parse_layer` is explicit that user and system layers are fatal: "those ARE
yours, and starting while ignoring them silently would be worse than not
starting" (`load.rs:1858`). A profile is the user's own file, so a blunt
"always degrade" answer would contradict a rule that was written on purpose.
But a profile is also the only layer chosen from a picker while the program is
running, and aborting a running norte because of a typo in a directory the
reader merely browsed to is worse still.

Three situations, three answers:

- **`--profile <name>` and it does not parse** → fatal, naming the file and
  the diagnostic. The reader asked for that profile by name; starting as
  something else would be answering a different question.
- **The sticky profile from the session does not parse** → start with **no
  profile layer** and say so loudly. Nobody asked for it this run, and
  aborting would trap the reader outside the program with no way to pick a
  different one.
- **Switching to it at runtime does not parse** → the switch is **refused**,
  the current profile stays exactly as it was, and the reason is shown. A
  half-applied profile is not a state this design admits.

The same three-way rule covers a profile directory that has vanished. A
`[profile.start]` naming a path that no longer exists is not a broken profile
and does not trigger it: that is per slot, and the slot opens at its fallback
directory and says which path was missing.

### D8 — Switching is hot, and says what it could not do

The sequence, in this order:

1. Flush the outgoing profile's state into `layouts[old]` and its slots.
2. Rebuild the layers with the new profile directory and reload.
3. Apply theme, keymap, columns, favourites, openers.
4. Mount `layouts[new]` if the session has one; otherwise the arrangement its
   `[ui] layout` names; otherwise the factory default.
5. Seed each slot from its saved `SlotState`, or from `[profile.start]`, or
   from the fallback directory.
6. Emit **one line** naming what could not be applied without a restart.

That last line is not decoration. Some settings are established once per
process and a switch that silently leaves them behind is a switch that lies.
**Which settings are on that list is measured, not assumed.**

**Measured in P3, and the answer was already written down.** The TUI's watcher
hot-reload (`norte-tui/src/config_reload.rs`) already re-applies the theme, the
whole keymap, the columns with their re-sort, the favourites, the openers, the
quick-search mode and the quit confirmation — and its own rustdoc had recorded,
before profiles existed, that `[ui] lang` is session-fixed because
`norte_i18n::force` runs once per process. **`ui.lang` is the whole list**, and
the switch reuses that reload rather than reimplementing it — its
all-or-nothing behaviour turns out to be exactly what D7 asks of a switch.
Fonts and `reduce_motion` are not on the list at all: a terminal never applies
them, and saying "could not be applied" about something this frontend never
does would be noise. The classification is pinned by destructuring
`CommonConfig` with no `..`, so a new scalar does not compile until someone
puts it in a group.

**And the three sources of D7 do not share one path.** This corrects the
paragraph above, which assumed they did:

- **`--profile <name>` is known before anything connects**, so it goes into the
  *first* configuration load. Everything applies, `ui.lang` included, and a
  profile that cannot be used aborts naming the file — which is what D7 asked
  for and what a hot switch could never have delivered.
- **The sticky profile cannot do that**: it lives in the session, the session
  belongs to the daemon, and the daemon is reached with the configuration being
  loaded. It arrives with the session and switches hot, paying the `ui.lang`
  announcement.
- **An explicit profile beats the sticky one.** The reader named one for this
  run.

Reading `profiles/` is I/O and happens **off the event loop**. #244 is the
precedent: `apply_layout` doing blocking I/O on the event loop was a bug with
an issue number, and the layout picker's rows are read ahead for exactly this
reason.

### D9 — Every surface the picker already has

The profile picker is a sibling of `layout_picker.rs`, in `norte-frontend`,
pure: rows, cursor, and per-row facts (does it parse, what does it override,
does its name collide with a layout or a keymap preset — the warning the
layout picker already pioneered). The frontends paint it; neither owns it
(rule 7).

- **TUI**: overlay picker, a menu entry, `profile.pick` in the palette,
  `--profile <name>` at startup.
- **Window**: the same picker, a DTO in `norte-ui-host`, painted by the
  renderer. One bridge bump. Note that `just ci-fast` does **not** run
  `gui-ci`, so the renderer's `BRIDGE_VERSION` must be checked by hand or the
  desync ships green (it did, 36→37).
- **Settings**: not a row for "active profile" — the active profile is session
  state and the settings catalogue writes `norte.toml`. Instead, **"save the
  current workspace as a profile"**, which writes `profiles/<name>/norte.toml`
  with the `toml_edit` writer `settings.rs` already has, and the current
  arrangement into `profiles/<name>/layouts/`.
- **CLI**: `norte doctor` validates every profile directory — parses each
  file, reports the keys D2 refuses, and flags name collisions.

Commands added to the shared catalogue: `profile.pick`, `profile.next`,
`profile.prev`, `profile.save-as`. They ship unbound in every preset; #228 is
the lesson about presets that leave core commands unreachable, but binding
four new commands across seven presets without being asked is the opposite
mistake.

### D10 — A rebind made under a profile is written into that profile

`RebindSources::split_at` (`norte-frontend/src/keymap/rebind.rs:435`) decides
which layer the shortcut editor writes to: a leading run of `System`, then at
most one `User`, and **anything else lands in `above`, the fail-closed side**.
Its own rustdoc says an unexpected order makes the door refuse writes it
cannot model.

Left alone, an active profile carrying a `keymap.toml` sits above the user
layer, so every rebind would either be refused or be written where the profile
shadows it. Both are wrong for the same reason: the reader rebinding a key
inside a workspace means it in that workspace.

So the cut widens by exactly one step — `System`\* then `User`? then
`Profile`? — and the write target is the **last** of `User`/`Profile` present.
Every other order still falls into `above` and is still refused. This is why
this belongs to P2 rather than to the frontend phases: it is a rule about
layers, and it is testable with no UI at all.

## Phases

Each is a PR under ~400 net lines with one purpose.

**P1 — the layer.** `Layer::Profile`, its directory resolution (including the
`NORTE_CONFIG_DIR` seam), precedence, the positive whitelist of D2 with its
warnings, the `[profile]` section of D3, and the three-way failure rule of D7.
Mostly `norte-config`, but not confined to it: the new `Layer` variant breaks
three exhaustive matches (`norte-tui/src/lua/host.rs:33`,
`norte-tui/src/lua/api.rs:335`, `norte-gui-tauri/src/startup.rs:345`), and the
third maps into `norte_ui_host::settings::ConfigLayer`. That type is **not**
wire surface — it resolves to a localized string inside `PathRowView.label`
and neither `dto.rs` nor the renderer's `types.ts` names it — so the bridge
does not move here. It moves once, in P4, for the picker.

**P2 — the state.** `SessionBody.active`, arrangements keyed by profile, slot
ids allocated per profile, the pruning order of D6, `SCHEMA_VERSION` 1 → 2 and
its round-trip tests including a v1 body, and the widened rebind cut of D10.
`norte-frontend` only.

**P3 — the TUI.** The pure picker, `profile.pick`, `--profile`, and the hot
switch of D8 with its "what could not be applied" line, measured.

**P4 — the window.** DTO, renderer, the golden corpus entry the new picker
needs (#257 is the precedent for a corpus checked against a hand-written list
and missing variants), and the bridge bump the picker needs.

**P5 — creating one, and checking it.** "Save the current workspace as a
profile", `norte doctor` validation, help topics, Fluent keys in both locales.
Touching `norte-help/topics/**` means regenerating the `norte-cli` golden with
`NORTE_UPDATE_GOLDEN=1`, or `ci-fast` goes red there.

An **ADR is mandatory**: this changes the structure of configuration and its
precedence, which is exactly what ADR 0007 and ADR 0035 record. Next number is
0079.

## What this does not touch

`norte-proto`, `norte-vfs*` and `norte-core` are untouched, so there is no
protocol bump, no golden, no compatibility window, and `cov` — which only
moves for proto/vfs/core — cannot move. The gate for this branch is
`ci-fast` during the work and one `just ci` at the close.

## Risks

**The negation that grants everything.** D2 is the finding this design turned
up: `*kind != Layer::Project` means a new `Layer` variant inherits the user
layer's full powers by default. P1 must land the positive predicate *and* a
test that a profile setting `[daemon] socket`, `[ai] enabled`, `[log] dir` and
`[archive] max_entries` changes none of them and warns about all four.

**Bytes, again.** #245 and #246 were the same bug twice with layout names. A
profile name travels further — it is a directory, a session key and a picker
row. D4 is the rule; a hostile-corpus fixture with a non-UTF-8 profile
directory is the check.

**A switch that half-applies.** D8's step 1 flushes the outgoing state before
anything else changes, and D7 refuses a switch outright rather than
half-performing it. The test that matters is the interrupted one: a switch
that fails at step 3 leaves the reader in the profile they were in, with the
state they had.

**A session that outgrows its megabyte.** D6 caps profiles with saved state,
but the cap is a number chosen in advance. P2 measures a realistic body — N
profiles, full arrangements, full history — and picks the number from the
measurement rather than from taste.
