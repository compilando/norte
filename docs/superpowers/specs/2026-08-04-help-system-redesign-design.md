# Help system redesign — navigable, executable, state-aware, plugin-extensible

- Date: 2026-08-04
- Status: approved
- Related: ADR 0006 (keymap), ADR 0022 (plugin host), ADR 0032 (shared WIT
  package debt), spec §8/§13, design `2026-07-23-help-config-system-design.md`
  (phases C1/C2/H1/H2/P1/P2, all shipped)

## Problem

Help today is a flat list of key bindings, and only in one frontend.

1. **TUI F1 is a `Vec<String>`.** `norte-tui/src/help.rs` joins the effective
   keymap with the Fluent catalog and emits pre-formatted lines: three
   sections, one line per binding, scroll and nothing else. No search, no
   grouping by task, no navigation, no prose. It answers "which key does
   what" and never "how do I copy a file out of a zip into S3".
2. **The GUI has no help at all.** The shared presets bind `f1 → app.help`,
   but `app.help` is not in the GUI's `COMMANDS`, so `build_effectives_layers`
   silently drops the binding. F1 in the GUI does nothing.
3. **The CLI has no help beyond clap.** `--help` is English, non-Fluent, and
   lists flags. `norte doctor` diagnoses; it does not teach.
4. **Plugins are still mute.** P1 gave the manifest a `description` (280
   chars, cosmetic, excluded from the approval digest) and put
   `commands[{id,title}]` on the wire. A plugin cannot explain itself: no
   prose, no examples, no per-command documentation.
5. **Help does not know what is possible.** Every command is listed
   unconditionally: copying *into* a read-only archive, a plugin command whose
   plugin is disabled, an agent operation outside its policy scope. The user
   discovers the impossibility by trying.

## Design principle

Unchanged from the 2026-07-23 design, extended one dimension further: **a
single declared source of truth, a join computed at runtime, and a test that
forces coverage.**

F1 already does this for key bindings. This design applies the same shape to
prose: topic corpus × effective keymap × runtime availability × plugin
contributions, joined at render time, with tests that break the build when a
command is undocumented, a link dangles, or a locale falls behind.

## Architecture — `norte-help`

New workspace crate `crates/norte-help` (lib, `thiserror`, `#![warn(missing_docs)]`).
Depends on nothing from `norte-core` or any frontend; every frontend and the
CLI depend on it. Requires an ADR (structural change plus a new declared file
format).

### 1. Embedded topic corpus

`crates/norte-help/topics/{en,es}/*.md`, embedded with `include_dir!`. No
runtime I/O, no path resolution, works in a static binary. Front matter:

```markdown
---
id: copying
title: Copying across backends
tags: [doing, transfer]
see_also: [selection, remote, archives]
commands: [fs.copy, fs.copy-as, fs.move]
context: ["dialog.collision"]
---
Pick files in the left pane, then {{cmd:fs.copy}}. The **other** pane is the
destination — always, whatever it holds: a local directory, an SFTP host, an
S3 bucket, the inside of a `.zip`.

> ⚠ Inside an archive the source is read-only: copying **out** works, copying
> **in** does not. See [[archives]].
```

Fields: `id` (unique, kebab-case), `title`, `tags` (grouping in the sidebar),
`see_also` (topic ids), `commands` (command ids that become executable rows),
`context` (optional, see "Contextual help").

### 2. Markdown-lite parser → typed blocks

One parser, two modes.

Block vocabulary — closed, no HTML, no autolinked URLs:
`Heading{level,text}`, `Paragraph{spans}`, `Bullets{items}`,
`Code{lang,text}`, `Table{header,rows}`, `Callout{kind,spans}` where `kind ∈
{Note, Warn, Tip}`.

Span vocabulary: `Text`, `Strong`, `Emph`, `InlineCode`, `CommandRef{id}`
(from `{{cmd:…}}`), `TopicLink{id}` (from `[[…]]`).

**Trusted mode** (built-in corpus): parse errors are a compile-time-adjacent
failure — a unit test parses the whole corpus, so a malformed topic breaks the
build, never ships.

**Hostile mode** (plugin `help.md`, see below): total size cap 64 KiB, block
count cap, nesting depth cap, invalid UTF-8 decoded lossily with a badge
recorded on the topic, and every rendered span passed through the existing
`must_mask` treatment for bidi/invisible characters (the same corpus that
guards the extension manager and the approval modal). Exceeding a cap
truncates with an explicit marker; it never fails the plugin load — help is
cosmetic and must not brick an approved plugin.

### 3. Live marks — the join

Two marks make prose stop lying:

- `{{cmd:fs.copy}}` renders **the user's effective chord** plus the Fluent
  description: `F5` under orthodox, `yy` under vim, `—` when the user unbound
  it. Resolution happens at render time against the `Effective` keymap the
  frontend already built. A rebind changes the prose.
- `[[selection]]` is a jump to another topic, with a back-history.

### 4. Render-agnostic model

`norte-help` returns data, not strings-with-layout:

```rust
pub struct Topic { pub id: TopicId, pub title: String, pub tags: Vec<Tag>,
                   pub see_also: Vec<TopicId>, pub blocks: Vec<Block>,
                   pub rows: Vec<CommandRow>, pub origin: Origin }
pub enum Origin { BuiltIn, Plugin { id: String, publisher: Option<String>,
                                    truncated: bool, lossy: bool } }
pub struct CommandRow { pub command: String, pub label: String,
                        pub chord: Option<String>, pub avail: Availability }
```

The TUI renders blocks with ratatui, the GUI with GPUI, the CLI as plain text.
Business logic stays out of frontends (rule 7); each frontend owns only its
own painting.

### 5. Availability layer

`Availability { Available, Unavailable { reason: Reason } }`, computed by the
frontend from data it already holds — no new wire traffic:

- provider capabilities of the focused pane (`READ_ONLY` inside an archive,
  no `copy_native` on a backend, no trash);
- plugin state (disabled, not approved, digest mismatch);
- policy (an agent-scoped command outside its granted scope);
- connection state (`connection.degraded` already exists on the wire).

Unavailable rows render dimmed with the reason inline. The help never offers
what the app would refuse.

### 6. Tests that force coverage

House pattern, extended:

- **Locale parity**: every topic id exists in `en` and `es`, with the same
  `commands`/`see_also`/`context` sets (prose may differ, structure may not).
- **Link integrity**: every `[[id]]` and every `see_also` resolves.
- **Command integrity**: every `{{cmd:id}}` and every `commands:` entry exists
  in the frontend's `COMMANDS`.
- **Documentation gate**: every command in `COMMANDS` appears in at least one
  topic. A new command without documentation fails the build. Until phase H3h
  an explicit, shrinking allowlist carries the undocumented remainder — the
  allowlist is data in the test, so it is visible and reviewable.
- **Context integrity**: every `context:` value is a real context, and every
  known context has exactly one topic.
- **Hostile corpus**: plugin-mode parsing of the `norte-testkit` hostile
  strings (bidi, ZWJ, tag characters, invalid UTF-8, 64 KiB+ input, deep
  nesting) produces bounded, masked output.

## Surfaces

### TUI overlay (replaces the flat F1)

```
┌ Help — norte ──────────────────────────────── [/] search  [?] keys ┐
│ TOPICS              │  Copying across backends                     │
│  ▸ Basics           │  ──────────────────────────────────────      │
│    · Two panes      │  Pick files in the left pane, then F5. The    │
│    · Moving around  │  right pane is the destination — always.      │
│    · Selection      │                                              │
│  ▾ Doing things     │    F5      Copy            ⏎ run             │
│    · Copying   ◀    │    ⇧F5     Copy as…        ⏎ run             │
│    · Renaming       │    F6      Move            ⏎ run             │
│  ▸ Remote & archives│                                              │
│  ▾ Extensions       │  ⚠ Inside a .zip the source is read-only:    │
│    · ftp-provider   │    copying OUT works, copying IN does not.    │
│    · previewer-demo │                                              │
│                     │  See also: [[selection]]  [[remote]]          │
├─────────────────────┴──────────────────────────────────────────────┤
│ /copy_                    3 topics · 7 commands   ⇥ pane  ⌫ back   │
└────────────────────────────────────────────────────────────────────┘
```

Keys, bound through the `dialog` context like every other overlay (H1
precedent — no hardcoded legends): `/` incremental filter over topics,
commands and plugin contributions; `⏎` runs the focused command row or follows
the focused link; `⇥` switches pane; `⌫` history back; `Ctrl+P` hands the
current filter to the command palette; `Esc`/`q`/`F1` close.

Executing from help goes through the same dispatch as the palette — no second
path, no bypass of policy or approval.

### Help ↔ palette

Two views of one model, at two densities. The palette stays the fast gesture
(`Ctrl+P`, type, Enter, gone). Help is the learning view (topics, prose,
executable rows). `Ctrl+P` from help carries the filter across; the palette
row for a command offers a "help" affordance that opens its topic.

### Contextual help

The context→topic mapping lives in the corpus (`context:` front matter), not
in code. F1 inside the viewer opens *Viewer*; inside the approval modal opens
*Agents & policy*; inside the collision dialog opens *Copying*; in the pane it
opens the index. `Esc` returns to the modal underneath, it does not cancel it.

### GUI

Same `norte-help` model, GPUI view: sidebar, body, filter, same key
vocabulary. `app.help` joins the GUI's `COMMANDS` so the shared preset's F1
stops being silently dropped. Inherits the active theme and `[effects]`
(retro-crt/amber, ADR 0036).

### CLI

```
norte help                 # index
norte help copying         # one topic, plain text, no daemon needed
norte help --search zip    # search across topics and commands
norte help keys            # cheatsheet of the effective keymap
norte help --json          # whole model, for agents and golden tests
```

Runs embedded, no daemon. Pipe-safe (respect the known exit-after-pipe
pitfall). Localized from the same corpus. Localizing `clap --help` stays out
of scope, as decided in the 2026-07-23 design.

### Keyboard cheatsheet

`help keys` (and the in-app *Keyboard* topic) renders a grid grouped by
category, generated from the effective keymap — not a maintained list. Theme
roles carry the categories. Rebinding changes the sheet.

## Plugin help

**Author side.** A plugin ships `help.md` next to `plugin.toml`, same front
matter. It may use `{{cmd:…}}` **only for its own command ids**; referring to
a host command is a catalog error (fail-closed — a plugin does not document
what it does not own).

**Host side.** `PluginRegistry` reads `help.md` during discovery, parses it in
hostile mode, and stores the bounded result. The file is **excluded from the
approval digest**, exactly like `description` (P1 precedent): fixing a typo in
the help must not reset an approval and must not fatigue the user into
rubber-stamping. The mitigation is that help is never executed and always
rendered masked. `norte doctor` reports a missing, oversized, truncated,
lossy, or unparsable `help.md` as a `plugin-help` finding.

**Wire** (one protocol bump, `protocol-guardian` review, goldens updated):

- `PluginInfo.has_help: bool` — cheap, rides `plugin.list`, decides whether
  the sidebar shows a node for the plugin.
- `plugin.help { id } -> { markdown: String, truncated: bool, lossy: bool }` —
  **on demand**. 64 KiB per plugin must not ride every listing. The host sends
  bounded, valid-UTF-8 markdown (caps and lossy decoding already applied, with
  the flags reporting what happened); the frontend parses it with the same
  `norte-help` hostile-mode parser and masks at render. Parsing happens on
  both sides on purpose: host-side so `doctor` and the catalog can report
  problems without a frontend, frontend-side because the wire carries text,
  not a parsed tree.
- Opportunistic, same bump: `PluginInfo.settings`, the display the P2 phase
  explicitly deferred to "whichever future change bumps `PROTOCOL_VERSION`".
  One bump, two debts closed.

**Rendering.** The plugin node shows title, publisher, capability badges,
description and help. Its commands are executable rows subject to the usual
approval and policy path via `plugin.run_command`. All third-party text is
masked with the extension-manager criterion.

## Error handling

Consistent with the house philosophy. A malformed built-in topic fails a test,
never ships. A malformed plugin `help.md` degrades: truncated or lossy content
with a visible badge, plus a `doctor` finding — the plugin still loads. An
unresolvable `{{cmd:}}` in a built-in topic is a test failure; in a plugin
topic it renders as literal text, never as a phantom command row. A command
row that cannot run renders as unavailable with a reason instead of failing on
Enter.

## Testing

- H3a: corpus parity/link/command/context suites, hostile-mode parser matrix,
  documentation gate with its allowlist.
- H3b: TUI snapshots (index, topic, filter, plugin node), hostile render test,
  dialog-context binding coverage.
- H3c: F1-from-each-context test; palette-filter handoff test.
- H3d: availability matrix (read-only archive, disabled plugin, out-of-scope
  agent command, degraded connection).
- H3e: proto goldens; digest test proving `help.md` edits do not reset
  approval; registry caps (64 KiB, invalid UTF-8, deep nesting); doctor
  findings; cross-plugin `{{cmd:}}` rejection.
- H3f: GUI view test with themed and effects-enabled render.
- H3g: `--json` golden; plain-output pipe test.
- H3h: documentation gate with an empty allowlist.

## Phases and order

| # | Scope | Reviewers |
|---|---|---|
| H3a | `norte-help`: model, parser, seed corpus EN/ES (~6 topics), marks, coverage tests. ADR. | rust, encoding |
| H3b | New TUI overlay (sidebar/body/filter/history/Enter-runs); replaces flat `help.rs` | rust, encoding |
| H3c | Contextual topics + palette bridge | rust |
| H3d | `Availability` wiring (caps, policy, plugin state, connection) | rust, security |
| H3e | Proto bump (`has_help`, `plugin.help`, `settings`) + registry `help.md` + doctor + extension manager | protocol-guardian, security, encoding |
| H3f | GUI view; `app.help` in GUI `COMMANDS` | rust |
| H3g | CLI `norte help` (`--list/--search/--json/keys`) | rust |
| H3h | Full EN/ES corpus; documentation gate at zero allowlist | encoding |

H3a first — everything consumes it. Then H3b → H3c → H3d in sequence; H3e and
H3g in parallel with them; H3f after H3b (it copies the settled layout); H3h
closes.

## Out of scope

- Localizing `clap --help`; man pages.
- A user- or Lua-editable corpus.
- Fuzzy search ranking; the filter is a substring match over title, tags,
  command ids and body text.
- Interactive tour, animation, which-key overlay (still conditional in ADR
  0006).
- Editing plugin settings in-app (the P2 follow-up; this design only carries
  the wire field that makes the display possible).

## Risks

- **H3h is writing work, not coding.** 53 commands and roughly 15 topics in
  two languages. This is where a project of this shape dies; the allowlist
  keeps every earlier phase shippable without it.
- **The documentation gate adds permanent friction** to adding a command.
  Deliberate, and worth stating plainly.
- **H3b likely exceeds the 400-line PR guideline.** Split into state and
  render if it does.
