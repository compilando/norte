# 0118 — The TUI paints an image as pixels, outside ratatui, or falls back

- Status: accepted
- Date: 2026-09-17
- Decision makers: Oscar González
- Related: ADR 0037 (plugin kinds, the closed set of roles; a broken preview
  never blocks the raw view), ADR 0077 (the same command means the same thing
  in both frontends), ADR 0089 (the RPC catalogue), ADR 0097 (parity between
  the terminal and the window is a test; a documented terminal-only key is not
  a divergence), ADR 0107 (`thumbnail` is a plugin kind of its own package),
  memory `funcion-compartida-no-basta`, spec
  `docs/superpowers/specs/2026-09-15-historia-y-wow-design.md` (phase 5), plan
  `docs/superpowers/plans/2026-09-17-fase5-imagenes-en-la-tui.md`

## Context and problem statement

The viewer already distinguished an image file (`Viewer.image: Option<ImageFmt>`)
and its own comment admitted that "frontends without image rendering (TUI) fall
back to hexview". Phase 5 gives the TUI that render: real pixels on a terminal
that speaks kitty's graphics protocol, coloured half-blocks otherwise, and
hexview when neither applies. Five questions had no obvious answer, and each
is a place this could have gone wrong quietly.

## Decision

**1. The pixel source is the existing `thumbnail` plugin kind
(`backend.plugin_thumbnail`, protocol 0.73.0, ADR 0107), not a new previewer.**
The window already asks that kind for a picture when its own decoder cannot
show one. A `thumbnail` guest turns bytes into a small PNG/JPEG/WebP raster —
exactly what the terminal needs to hand to kitty's protocol. Building a second
plugin kind so the TUI could ask the "same" question in its own vocabulary is
the divergence ADR 0077 exists to forbid: one command, two meanings, decided
twice and drifting the moment one side changes. One kind, two callers.

**2. The pixels are written to the tty AFTER `terminal.draw`, outside
ratatui, never as cell content.** A kitty graphics APC (`\x1b_G…\x1b\\`) is not
text ratatui can put in a `Cell` — it is a side channel the terminal
overlays on top of whatever cells are there. Three things follow from writing
outside the frame instead of inside it:
   - **The rect is one function, not two.** `draw_viewer` (`ui/panels.rs`)
     leaves the viewer's content area blank when an image is placed, and the
     run loop (`event_loop.rs`, after `terminal.draw`) computes where to put
     the pixels. Both go through `ui::geometry::rect_del_visor`, not two
     separate `Layout::split` calls — two counts of the same hole drift
     silently (memory `funcion-compartida-no-basta`), and an early version of
     this code proved it: it returned the frame WITH its border, two cells off
     on each axis from the interior `draw_viewer` actually leaves empty.
   - **Erasing has four call sites plus the panic hook.** Nothing in ratatui's
     diff-based repaint knows an image is on screen, so nothing erases it for
     free. It is erased in the run loop when the frame that would place a
     different id or `None` runs (covers closing the viewer and moving it to
     another file, by the ordinary comparison against process state),
     in `suspend::suspend_terminal` before yielding the terminal, in
     `tty::restore` before leaving the alternate screen, and in the panic
     hook — a crash while an image is placed must not leave it stuck on top
     of the backtrace the hook exists to make readable. All four go through
     the single `kitty_graphics::borrar_colocada`, keyed off one piece of
     process state (`COLOCADA`, an atomic id) rather than four places trusting
     `App` to still be reachable — two of the four (suspend, exit) have no
     guaranteed next frame that would run the normal comparison.
   - **The hole declared to the terminal is the block's INTERIOR, not its
     frame.** `c`/`r` in the placement escape are cells, and the terminal
     fits the raster to them; giving it the bordered rect would paint two
     rows and two columns of image over the viewer's own border and scroll
     bars.

**3. Sixel is out of scope.** Encoding it needs a colour quantizer and there is
no real terminal in front of this project that speaks sixel and not kitty's
protocol. The spec already says so (`docs/superpowers/specs/2026-09-15-historia-y-wow-design.md:285`);
this ADR is where a future reader who wants it back should start, not where it
gets built by surprise.

**4. Task 1's gate resolved to a REAL I/O probe, not an environment-variable
fallback.** `crossterm` has no API for the graphics protocol — only
`supports_keyboard_enhancement()` for the keyboard one — and does not parse
APC responses, so `kitty_graphics::consultar_soporte` writes the query APC
(`a=q`, a throwaway 1×1 RGB image) followed by a DA1 to `/dev/tty` and reads
the reply with a 200 ms deadline, once at startup, before the event reader
exists (same slot and same two reasons as `alt_menu::consultar_soporte`: the
event reader would otherwise own the lock, and stdout under `--pick` is a data
pipe, not a terminal). The evidence that made the probe the answer instead of
the fallback: piloted in tmux without `allow-passthrough`, the terminal
answered only the DA1 (`\x1b[?1;2;4c`) — no support, correctly read as
"blocks". Piloted in kitty 0.48.2, the terminal answered
`\x1b_Gi=31;OK\x1b\\` with our own id, in under 200 ms, with nothing visible
written to the screen. An environment fallback (`TERM=xterm-kitty`,
`KITTY_WINDOW_ID`, `TERM_PROGRAM=ghostty`, `TERM=foot`) was the documented
retreat if the read proved unreliable — it never did, so it was not built. It
would have been strictly worse: blind to any new terminal that speaks the
protocol without matching one of those strings, and fooled by a `TERM`
exported by hand that says nothing about what actually answers on the wire.

**5. Erasing sends `d=I` (uppercase), not `d=i`.** `d=i` removes the
placement only and leaves the image's DATA resident in the terminal;
`d=I` removes both. Image ids are minted once per process and never
recycled (`mint_image_id`), so with `d=i` every file a session ever looked at
would leave a permanent copy of its raster sitting in the terminal's memory
for the rest of that session — a slow leak with no way to reclaim it short of
closing the terminal. The task's own worked-out test in the plan used `d=i`;
that was corrected during implementation (`escape_borrar`,
`crates/norte-tui/src/kitty_graphics.rs`) once a reviewer traced what each
flag actually frees, and the test now asserts `d=I`.

**6. `blocks` does not build anything new.** Half-block rendering already
existed before this phase, delivered by an approved `previewer` plugin
(`image-ansi`) through the existing preview chain (`plugin_preview_styled` /
`plugin_preview`, ADR 0037). What phase 5 adds on that path is not the
rendering — it is saying, in the viewer's status bar, that nothing is
approved yet when nothing is: see point 7.

**7. The warning names the RIGHT extension, because the two paths need
different ones.** `Modo::Kitty` is fed by a `thumbnail` plugin
(`image-thumb`); `Modo::Bloques` is painted by a `previewer` plugin
(`image-ansi`). Sending a reader to approve the wrong kind is worse than
saying nothing, so `viewer_open::aviso_de_imagen` carries two distinct
messages (`viewer-image-needs-thumbnail`, `viewer-image-needs-previewer`) and
picks by `Modo`, not by a single "is anything approved" boolean. This needed a
second pass mid-phase (task 5b, not in the original plan): the first version
of the warning only fired in `Modo::Bloques`, on the premise that "in kitty
mode the terminal paints on its own — there is nothing to approve". That
premise is false: the pixels kitty places still come from a plugin
(`image-thumb`), exactly as optional and exactly as silently absent as the
`previewer` on the other path. Both modes warn now; `Modo::Nada`
(`images = "off"`) does not, because there the reader asked for hexview
themselves and nothing is missing.

## Consequences

- Positive: one plugin kind serves both frontends' pixel needs, so a
  `thumbnail` guest built for the window's viewer works for the TUI's for
  free, and a divergence in what "thumbnail" means never gets the chance to
  start.
- Positive: an image that cannot be shown — no plugin approved, a broken
  guest, a terminal that lied about its own protocol — still falls back to
  hexview, never to an error. This is the same degradation ladder ADR 0037
  already committed the viewer to; phase 5 adds a rung, it does not change the
  contract.
- Positive: `[ui] images` is documented as terminal-only, the same pattern
  already established for `[ui] mouse` and `[ui] alt_menu` (ADR 0097): the
  window paints images through its own webview and does not read this key.
- Negative / residual risk — **a read race on `/dev/tty` that is possible but
  unobserved.** The probe's read runs on a separate thread because a
  deadline-bounded read on `/dev/tty` needs `poll(2)`, which is `unsafe` and
  reserved to `norte-vfs-local` by hard rule 5. If the terminal's answer
  arrives so late that the event loop's own reader has already started before
  the probe's thread finishes draining the reply, the two compete for bytes on
  the same fd with no defined winner. Not observed in tmux or in kitty during
  this phase's piloting; it needs latency well beyond the 200 ms deadline AND
  overlapping the event loop's startup to manifest. Closing it fully would
  need either an unbounded wait here (losing the whole point of a short
  deadline) or flushing the tty's input buffer (`tcflush`, `unsafe`/`libc`,
  forbidden outside `norte-vfs-local`). The risk is accepted and written down
  rather than engineered away.
- Negative / residual risk — **no end-to-end test of the full path.** Nothing
  in `norte-tui/tests` exercises viewer-open → `plugin_thumbnail` → an
  approved, real `thumbnail` guest the way
  `crates/norte-core/tests/plugins_preview_image_e2e.rs` does for previews.
  What is tested is the parsing (`respuesta_dice_si`), the escapes
  (`escape_colocar`/`escape_borrar`), and the mode/warning decisions
  (`modo_efectivo`, `aviso_de_imagen`) in isolation. The infrastructure for a
  guest-backed end-to-end test does not exist yet in `norte-tui`.
- Negative: the coupled/docked preview (a viewer shown inline in a pane, not
  at full screen) does not paint pixels in this phase — `spawn_preview_fetch`
  passes `Modo::Nada` unconditionally. It falls back the same way the
  full-screen viewer would with `images = "off"`: correctly, but with no
  pixels either. Left for whoever needs it; the full-screen path proved out
  the mechanism first.
- Neutral: a protocol client older than 0.73.0 already has no `plugin_thumbnail`
  RPC to call, so an old client against a new daemon simply never asks — no
  new compatibility surface opened by this phase, since ADR 0107 already drew
  that line.
