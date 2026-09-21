# 0136 — An optional custom title bar

- Status: accepted
- Date: 2026-09-21
- Decision makers: Oscar González
- Protocol: unchanged. Bridge: unchanged. Config: new `[ui] titlebar =
  "native" | "custom"`, default `native`. Window: one more binary command,
  `window_control`.
- Related: ADR 0066 (D11, the webview's minimal capability), ADR 0131–0135
  (the same VS Code pass)

## Context and problem statement

VS Code draws its own title bar: the menu sits in it, and the row the
desktop would spend on a title holds the menus instead. norte's window
spends two rows there, the desktop's title and the menu bar. The obvious
cost of drawing it ourselves is that the window then has to do what the
desktop did: move, maximize, minimize and close. And it has to do so
without the Tauri window permissions, which ADR 0066 D11 keeps out of the
webview's capability on purpose.

## Decision

**Optional, and off by default.** `[ui] titlebar = "custom"` removes the
desktop's decorations when the window is created; `native` (the default)
leaves them alone. The desktop's title bar is the one every other window
has and it cooperates with whatever the window manager does (tiling,
snapping, themes); a custom one is a preference, not an improvement for
everyone. It is read at start-up, so the settings screen marks it
`applies_live: false`. The terminal has no title bar and ignores it.

**The menu bar is the title bar.** With `custom`, the free space of the
menu bar drags the window (a press on the bar itself, never on a title or
a button, whose clicks it would eat), a double click maximizes or
restores it, and three buttons at the right end — minimize, maximize,
close, 46 px wide as on the desktop — do the rest. Close lights up red on
hover, the system's colour and not the theme's. The row exists even with
`[ui] menu_bar = false`: it is then the only way to move and close the
window with the mouse, and hiding it would leave a window that cannot be.

**A binary command, not a permission.** The webview asks through
`window_control(verb)`, a command of our binary with a closed vocabulary
(`minimize`, `toggle_maximize`, `close`, `drag`) that acts only on the
window that calls it, and refuses outright when the title bar is native.
Tauri's `core:window:*` permissions would have granted the whole webview
control over any window; the capability file keeps granting only event
listening, and the boundary tests pin both the command list and what the
renderer invokes. `close` goes through `CloseRequested`, so `[ui]
confirm_quit` asks exactly as with the desktop's X.

The flag travels in the catalog's `appearance` and is frozen at boot: a
theme change rebuilds the catalog but keeps the appearance, so the
renderer can never lose the controls while the decorations stay off. The
fatal screen (a dead daemon, a stale bundle) covers everything, the menu
bar included, so it carries its own copy of the title bar: without it that
window could be neither moved nor closed with the mouse. The command's
shape is pinned by a test: the four verbs and nothing else, the refusal
before any window call, and `close()`, never `destroy()`, which would skip
`confirm_quit` and the session flush.

## Consequences

- One row less of chrome for whoever wants it; nothing changes for anyone
  else.
- Resizing an undecorated window by its edges is left to Tauri's own
  handling for undecorated windows, and snapping gestures depend on the
  window manager; with `native` neither question exists, which is part of
  why it stays the default.
- One more command in the webview's surface, narrow and inert with the
  default configuration.
- The full-window overlays (help, settings, the first-run splash) cover the
  title bar while they are open, as they cover the menu bar; they close with
  Esc, so the window is never stuck, only momentarily without its buttons.
- The drag starts over IPC after the press, and `start_dragging` moves with
  the button still held: a very quick click may not start one, and once a
  window-manager drag grabs the pointer it may swallow the double click.
  Neither was verified by hand on X11 and Wayland when this was written.
