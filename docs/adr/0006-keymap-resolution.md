# 0006 - Keymap resolution semantics

- Status: accepted
- Superseded in part by [0043](0043-keymap-availability-and-the-mod-alias.md):
  a binding to a command this build does not implement is no longer a load
  error, it is a declared unavailability. Everything else here stands.
- Date: 2026-07-11
- Decision makers: Oscar González
- Related: specification section 12 and M1 phase 4

## Context

The keymap maps a context and key sequence to a named command. It supports
Vim-style multi-key sequences, hierarchical contexts, bundled orthodox/Vim/CUA
presets, and user prepend/append layers. The unresolved questions are how to
handle shared prefixes, when to resolve pending input, how contexts merge, and
where the engine lives.

## Options considered

- A Vim-style timeout permits both `g` and `g g`, but makes behaviour depend on
  input timing and prevents deterministic testing.
- Requiring an effective keymap to be prefix-free catches conflicts at load
  time and resolves every input without a clock.
- Searching contexts for every keypress defers cross-context ambiguity to
  runtime. Precomputing an effective map for a context stack allows complete
  validation when that stack is activated.
- A dedicated crate would be reusable but adds maintenance before a second
  consumer exists. A TUI module can be extracted when the GUI needs it.

## Decision

- Merge layers per context, with context specificity taking precedence over
  layer order. A user append in `pane` therefore overrides a preset binding in
  `global`; a user prepend in `global` does not override `pane`.
- User layers accept only `prepend_keymap` and `append_keymap`; using the preset's
  `keymap` form in a user layer is a load error.
- Precompute the effective map for the active context stack and reject any
  sequence that is a strict prefix of another. Resolve through a trie without
  timeouts.
- Esc clears pending input. An input with no valid continuation is discarded and
  resets the sequence. Because Esc inside a multi-key sequence would be
  unreachable, reject it with `EscInSequence`; a standalone Esc binding remains
  valid.
- Display pending input in the status bar. A which-key overlay can reuse that
  state when overlays are introduced.
- Keep the engine in `norte_tui::keymap` until a second frontend needs it.
- Use stable command names such as `app.quit`, `pane.switch`,
  `cursor.up`, `cursor.down`, `cursor.page-up`, `cursor.page-down`,
  `cursor.top`, `cursor.bottom`, `nav.enter`, and `nav.parent`.
- TOML accepts values such as `f5`, `ctrl+c`, `alt+enter`, `g`, `G`, `tab`,
  `esc`, `backspace`, `enter`, `space`, `plus`, arrows, `pgup`, `pgdn`, `home`,
  and `end`. A character's case encodes Shift; use `shift+` only with non-character
  keys. The `+` character is the modifier separator, so the key itself is
  spelled `plus` — the only spelling (a bare `"+"` is `BadChord`).
- Bundle `orthodox.toml`, `vim.toml`, and `cua.toml` with `include_str!` and
  parse them through the same path as user maps. `orthodox` is the default.
- M1 defines `global` and `pane`; dialogs and viewers add contexts when their UIs
  exist.

## Consequences

- Resolution is deterministic and directly property-testable.
- User-layer conflicts produce actionable load errors.
- No timing logic means no latency-dependent behaviour or flaky tests.
- Some legitimate Vim patterns, such as binding both `d` and `d d`, are not
  possible in one context. Future modal behaviour should use new contexts rather
  than timeouts.
- Effective maps are recomputed per context stack. M1 has only two contexts; a
  future larger hierarchy may cache the small set of static stacks.
