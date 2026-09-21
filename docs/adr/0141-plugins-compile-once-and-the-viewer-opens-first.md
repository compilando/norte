# 0141 — Plugins compile once, and the viewer opens first

- Status: accepted
- Date: 2026-09-21
- Decision makers: Oscar González
- Protocol: unchanged. Bridge: unchanged. WIT: unchanged.
- Related: ADR 0037 (styled previews), ADR 0107 (thumbnails), ADR 0033
  (embedded provider), #68 (artifact cap), #241/#282 (consent)

## Context and problem statement

F3 in the window took seconds. Measured: the window's own path — a bounded
256 KiB read, a viewer built from it, the visible lines across the bridge —
opens a 50 MB file in about 70 ms. The seconds were the plugins:

- Every plugin call compiled its `.wasm` from scratch with cranelift. The
  syntax-highlighting previewer (1.9 MB) takes 2–3 s to compile and 20–80 ms
  to run; an image thumbnailer, 3.4 s. The daemon kept one runtime but no
  compiled code; the embedded backend built a whole runtime — an engine and
  an epoch ticker thread — per call.
- The window waited for the previewer before opening the viewer at all.
- With an image previewer installed, its ANSI rendering of a PNG won over
  the window's own image, and the window then asked a thumbnailer too:
  two plugins compiled to open a photo the window can draw by itself.

## Decision

**Compile once, by content.** `PluginRuntime` keeps compiled components
keyed by the SHA-256 of the artifact's bytes. It reads the file (bounded
by the artifact cap), hashes it, and compiles from those same bytes, so a
changed file is a different entry and old code never runs for new bytes
or the other way round. What is reused is machine code: each call still
gets a new store, instance and WASI context. The cache holds sixteen
components and is emptied when full; recompiling a changed plugin drops
its previous version, so development rebuilds do not pile up.

**One runtime per process.** The embedded backend uses the same runtime
the plugin columns already kept for the whole process, instead of one per
call.

**No silent cut.** Cutting the text handed to a previewer at the line cap
was tried and dropped in review: it produced a styled view that looked
like the whole file while hiding everything past the cut — a place to hide
a payload from someone inspecting a script. A file whose styled preview
would exceed ten thousand lines keeps the raw view, which is whole; with
the compile cached, the wasted render is milliseconds.

**Open first, colour later.** F3 opens the viewer with the raw view as soon
as the bytes are read; the styled preview is requested in the background
and replaces the raw one when it arrives, if the viewer is still the same
one, at the row the reader had scrolled to — unless the reader already
chose how to see it (hex, a forced encoding), which is respected.

**The window's own images skip the plugins.** A file the window can draw
as an image is drawn: no styled preview, no thumbnail. Plugins still
cover the formats the window cannot decode.

## Consequences

- The first F3 on a file type still pays one compilation per process; the
  next ones take milliseconds.
- A styled preview can appear a moment after the viewer opens, replacing
  the raw text.
- Compiled code stays in memory while the process lives, up to sixteen
  plugins.
- **Still open, found in this review and older than it:** the approved
  `.wasm` digest is checked when a plugin is discovered, and for providers
  when they connect, but not when a previewer, thumbnailer, command,
  decorator, panel, renamer or organizer is loaded. Something able to write
  a plugin's `.wasm` after approval runs with the approved capabilities
  until the next discovery. The cache neither opens nor closes this; it
  makes the fix cheap, since `prepare` already holds the bytes' SHA-256 and
  only needs the expected digest passed in.
