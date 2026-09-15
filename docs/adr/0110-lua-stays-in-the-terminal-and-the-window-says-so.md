# 0110 — Lua stays in the terminal, and the window says so

- Status: accepted
- Date: 2026-09-15
- Decision makers: Oscar González
- Related: ADR 0026 (embedded Lua with mlua), ADR 0066 (renderers over a Rust
  UI host), ADR 0077 (the same command means the same thing in both
  frontends), ADR 0100 (hooks observe the journal and leave Lua out), spec
  §7.2, CLAUDE.md hard rule 7

## Context and problem statement

ADR 0026 put Lua in `crates/norte-tui/src/lua/` and kept a `LuaHost` boundary
"so the module can move to a shared crate if the GUI later needs it". The
window now exists (ADR 0087), reaches command parity through
`crates/norte-ui-host/tests/paridad.rs`, and does not run Lua. The question
ADR 0026 postponed has arrived: does the window get Lua?

What is there today:

- About 3,200 lines in the TUI: `init.lua` layers with TOFU trust for a
  project file, `norte.command` for keymaps, a status bar hook, and
  `norte.fs.*` through `Backend`. The runtime is `!Send`, polled inline in the
  TUI loop, and the host takes `norte_core::backend::Backend` directly.
- The window knows about Lua in exactly one place: `startup.rs` counts the
  `lua:` bindings a PROJECT layer carried and were discarded, and says so.
- A `lua:<name>` binding from the USER layer is `Availability::Here` in every
  frontend (`keymap/effective.rs`), because the Lua registry is dynamic and
  never in the catalogue. The window has no handler for it. The key is
  announced — reference sheet, which-key, palette — and does nothing. That is
  the failure the keymap rules in CLAUDE.md name three times over.
- Nobody uses it: the only human on the project has no `init.lua`, and the
  WASM plugin kinds (previewer, command, decorator, renamer, hook, thumbnail)
  now cover most of what spec §7.2 asked Lua to do, governed and in both
  frontends.

## Considered options

### A — Move Lua to a shared crate and host it in the window

A `norte-lua` crate, a host in `norte-ui-host`, the status bar hook and
`norte.command` over the bridge.

- Good: ADR 0077 parity; one scripting story for both frontends.
- Bad: the host drives `Backend` today and the window only reaches the daemon
  through `norte-client` (ADR 0066, D10), so this is a rewrite of `fs.rs` and
  the driver, not a move. `!Send` futures must be polled by whoever owns the
  UI host's loop, which a Tauri command thread is not.
- Bad: user scripts that can read the pane state would sit next to logic that
  hard rule 7 keeps out of frontends. The more the API grows, the more
  business logic lives in a script host instead of the core.
- Bad: thousands of lines for no current user.

### B — Retire Lua

Delete the module, the `lua:` binding form, `dialog.trust-lua` and the help
text; point people at WASM command plugins.

- Good: 3,200 lines and a vendored C runtime gone; one extension story.
- Bad: breaks anyone with an `init.lua`, with no migration, for a feature the
  spec still lists. WASM plugins are sandboxed third-party code; Lua is the
  user's own `.bashrc`, and there is no WASM equivalent of "a keymap command I
  wrote in five lines".
- Bad: irreversible in practice. Once the binding form is gone, bringing it
  back is a new feature, not a revert.

### C — Freeze Lua in the TUI, and make the window honest about it

Lua stays exactly where ADR 0026 put it. The window does not run it and is
not going to until someone asks. What the window must stop doing is
pretending: a `lua:` binding is not available there.

- Good: no user breaks, no rewrite, and the decision is reversible — option A
  stays open with its costs written down here.
- Good: the only real defect (a dead key the window advertises) is fixed by a
  small change in the shared keymap, not by a scripting host.
- Bad: a documented divergence between frontends, the kind ADR 0077 argues
  against. It is acceptable here because it is not the SAME command meaning
  two things: in the window it means nothing, and says so.

## Decision

**Option C.**

1. Lua is a TUI feature. The API does not grow while it is frozen: no new
   `norte.*` bindings, no Lua subscription to journal events (the question
   ADR 0100 left open closes here, as "no").
2. A frontend without a Lua host resolves a `lua:<name>` binding as
   `Availability::NotHere`, not `Here`. The frontend declares that it hosts
   Lua; the keymap does not guess. The name-charset check stays shared
   (`valid_lua_name`), so a malformed name is still a load error everywhere.
3. The help text that describes `init.lua` says it runs in the terminal
   frontend only.
4. Reopening this needs a real use that WASM command plugins cannot serve,
   and then option A with its costs as written.

## Consequences

- Positive: the window stops advertising keys it cannot run; the which-key,
  reference sheet and palette treat a `lua:` binding like any other command
  this frontend does not have.
- Positive: the Lua surface has a ceiling, so it cannot quietly become the
  place business logic goes.
- Positive: nothing is removed, and nothing a user wrote stops working in the
  TUI.
- Negative: a person who configures `lua:` keys and switches to the window
  loses them; the window says the key is not available here, which is honest
  but not a replacement.
- Negative: `mlua` and vendored Lua stay a structural TUI dependency, with the
  maintenance that implies, for a feature with no known user.
- Neutral: `norte help` builds its own map from the bundled presets, without
  `LUA_HOST`, so a user's `lua:` binding is `NotHere` there too. Nothing
  visible changes today — the keys page prints `Here` and `NotHere` alike, and
  no help topic names a `lua:` command in a `{{cmd:…}}` mark — but a topic
  that ever did would find that mark unresolved.
- Follow-up (code, separate change): point 2 in `norte-frontend` with a test
  in both directions, and point 3 in `crates/norte-help/topics/{en,es}` with
  the `norte-cli` help golden regenerated.
