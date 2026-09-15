# 0026 - Embedded Lua scripting with mlua

- Status: accepted
- Date: 2026-07-17
- Decision makers: Oscar González
- Related: specification sections 7.2 and 8; ADR 0022; ADR 0110 (the window
  does not host Lua, and the API is frozen)

## Context

User `init.lua` files define named commands for keymaps and per-pane status
hooks through a high-level synchronous API. Lua scripts are user configuration,
distinct from sandboxed third-party WASM plugins and native built-ins. The
project must choose the module boundary, Lua implementation, trust model, and
async execution strategy.

## Options considered

- Put mlua in the TUI, where pane state and selection types already live, and
  route filesystem actions through `Backend`.
- Create a shared `norte-lua` crate immediately. This requires prematurely
  moving TUI-specific state before another frontend consumes it.
- Put Lua in the core. Unsandboxed user configuration would then run inside the
  process mediating policy and journal for every client, contrary to the
  frontend scripting model.

## Decision

Add `crates/norte-tui/src/lua/` using vendored Lua 5.4 through mlua 0.10 with
`async` and `serialize` features. Keep a clean `LuaHost` boundary so the module
can move to a shared crate if the GUI later needs it.

- Lua is **not sandboxed**. It is equivalent to `.bashrc` and has the user's
  permissions, including the complete `io`, `os`, and `load` standard library.
  `norte.fs.*` calls pass through `Backend`, the engine, journal, policy, and
  undo. Raw `io.*` and `os.*` calls do not and must be documented as such.
- Always trust system and user `init.lua` files. A project-local
  `./.norte/init.lua` may come from an untrusted repository and requires
  explicit TOFU approval bound to its canonical path and SHA-256. Changing the
  file requires new approval.
- Poll mlua's `!Send` futures inline in the TUI main loop. Never move them to
  `tokio::spawn`. A running Lua command yields while waiting for the engine and
  observes cancellation at that boundary.
- Vendoring fixes the Lua version across machines and avoids a system `liblua`.
  Serde integration moves entries and pane snapshots between Rust and Lua
  without bespoke field conversions.

## Consequences

mlua and vendored Lua become structural TUI dependencies. Unsafe code remains
inside the third-party FFI implementation, not norte. The clear trust boundary
requires every public scripting entry point to say which calls are journalled
and governed. The driver must preserve `!Send`; accidentally erasing and moving
that constraint could become a runtime failure rather than an obvious compile
error.
