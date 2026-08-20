# The Tauri vertical slice: what it measured, and the go/no-go

- Date: 2026-08-20
- Phase 3 of `docs/superpowers/plans/2026-08-19-multi-frontend-tauri-transition.md`
- Machine: Arch Linux, Wayland (KDE), WebKitGTK 2.52.6, libsoup 3.6.6,
  displays at 100 Hz / 60 Hz / 144 Hz. Release build, daemon on a UNIX socket.

## What exists now

`crates/norte-gui-tauri`: a Tauri 2 window over `norte-ui-host`, and a
plain-TypeScript renderer (ADR 0067) that paints and does nothing else. Against
a real daemon it lists two panes laid out by Rust, navigates, moves the cursor,
marks (including a range in one gesture), quick-searches, shows the task strip
and the status bar, and confirms before deleting. The webview's whole callable
surface is four commands, and a test fails if a fifth appears.

## The numbers

### The bridge (measured in `norte-ui-host/tests/payload.rs`, 100 000 entries)

| what | measured | budget | verdict |
| --- | --- | --- | --- |
| cursor patch | **204 bytes** | ≤ 16 KiB | pass, by two orders of magnitude |
| 60-row window patch | **11 067 bytes** | window-bounded | pass |
| initial snapshot | **24 194 bytes, 128 rows** | no full-directory resend | pass |

Cursor movement in a 100 000-entry directory sends the cursor, not the
directory. This is the question the phase existed to answer, and the answer is
clean.

### Start-up (process exec → renderer asks for its first snapshot)

| directory | runs |
| --- | --- |
| 27 entries | 676 ms, 694 ms, 1061 ms |
| 100 000 entries | 401 ms, 398 ms |

Budget: cold start ≤ 3 s. **Pass**, with room. (The large directory is *faster*
because the first page is 100 rows either way, and the small run included a
cold page cache.)

### Latency (release build, synthetic events through the complete renderer path)

| metric | 27 entries | 100 000 entries | budget |
| --- | --- | --- | --- |
| idle frame (the machine's floor) | p50 32 ms, p95 33 ms | p50 32 ms, p95 33 ms | — |
| key → painted | p50 33 ms, p95 **33 ms** | p50 40 ms, p95 **46 ms** | p95 ≤ 50 ms |
| local scroll frame | p50 32 ms, p95 33 ms | p50 32 ms, p95 60 ms | p95 ≤ 20 ms |
| scroll → new rows painted | (one sample, 65 ms) | p50 68 ms, p95 77 ms | — |

**The floor dominates every one of these.** An idle page, painting nothing,
gets a frame every 32–33 ms — about 30 Hz — on a machine whose displays run at
100 Hz and 144 Hz. That is WebKitGTK's cadence here, not ours: our renderer
adds 0 ms to it while scrolling a 27-entry directory, and about one extra frame
on 100 000.

Two consequences follow, and they matter more than the pass/fail marks:

- **Key-to-paint passes** (33 ms and 46 ms against a 50 ms budget), but it
  passes *at* the platform's floor. There is roughly one frame of headroom, and
  it is not ours to spend.
- **The 20 ms scroll budget is unreachable by construction on this machine.**
  Nothing that waits for a frame can beat 32 ms. The budget was written against
  a 60 Hz assumption; it needs restating as "no worse than the platform's idle
  frame", which we meet exactly.

The measurement excludes the OS delivering the input event and the compositor
presenting the frame; it covers everything from the DOM event to the frame
after the DOM was updated.

### Memory (release, idle)

| directory | main process | WebKitWebProcess | WebKitNetworkProcess | total |
| --- | --- | --- | --- | --- |
| 27 entries | 233 MB | 263 MB | 67 MB | **≈ 563 MB** |
| 100 000 entries | 392 MB | 315 MB | 67 MB | ≈ 774 MB |

Drift over 25 minutes idle (debug build, 27 entries, sampled every 60 s):
216.8 → 229.0 MB, in four steps with long plateaus and the last eleven minutes
flat. It is step-and-plateau, not a slope; nothing here looks like a leak.

The **563 MB floor for showing 27 filenames** is the headline cost of this
toolkit, and roughly half of it is WebKit's own processes. It is the number to
put in front of anyone who has to accept the choice.

## What the spike found in the layers below it

The slice was the instrument, and it caught four things no unit test had:

1. **Two bridge patches could never be serialized.** `ViewChange::Tasks` and
   `ViewChange::Dialogs` wrapped a `Vec` in a newtype variant of an internally
   tagged enum — serde refuses that at run time. Every task-board and dialog
   update would have failed on the wire. Fixed (struct variants), and the
   golden corpus now covers every `ViewChange` individually.
2. **A snapshot did not replace the whole screen.** `dialogs` and `tasks` were
   projected as empty, so a resync while a delete confirmation was open would
   have painted the question away with the operation still waiting. Fixed.
3. **The screen's layout was not projected at all.** The renderer had no way to
   know where two panes go, or which is the target, without inventing a rule
   TypeScript is forbidden to have (D14). `LayoutView` now travels, focus
   changes travel as patches instead of whole screens, and the bridge is at
   version 2.
4. **The size and date columns were blank on every local directory.** The local
   provider lists lazily by design (#52) and `norte-ui-host` never probed, so
   the two most ordinary columns in a file manager were empty on `file://` —
   the default view. The terminal frontend already drives the shared
   `needs_stat_at`/`hydrate` rule; the host does now too.

Each of these was invisible to a suite that was green, and visible in the first
window that painted real files.

## Test matrix: what ran

Automated, and in the gate (`just gui-ci`):

- 31 renderer tests (Vitest + jsdom) over a fake bridge: sequence discipline
  (gap, stale sequence, foreign instance, wrong base, unknown version),
  virtualization (40 rows for a 100 000-entry directory, full-height canvas),
  empty/loading/error/hostile states, accessibility roles and
  `aria-activedescendant`, mouse gestures, dialogs.
- A contract suite that reads **the same golden JSON the Rust side pins**, so a
  DTO renamed in Rust breaks the TypeScript build rather than silently
  producing `undefined`.
- 25 Rust tests in the adapter: ordered pump with a fake window sink, the
  command surface pinned against the registered handler, the CSP and capability
  file, the built bundle scanned for remote origins and `eval`, the external
  link allowlist, start-up argument handling, and an end-to-end host over a
  daemon running the **local** provider.
- 87 tests in `norte-ui-host`, including the payload budgets above.

Run by hand, on this machine: the window against a real daemon (screenshots in
the session), a 100 000-entry directory, keyboard navigation (200 synthetic
keys with a repaint each), and the production bundle.

**Not run, and they are the honest gaps:** IME and dead keys, clipboard,
fractional scaling and 150/200 %, a screen reader (Orca/AT-SPI), drag
selection, context menu, daemon handover with the window open, compositor
restore, and packaging to `.deb`/AppImage. The accessibility work so far is
roles and names asserted in jsdom — a real AT traversal is a different claim
and has not been made.

## Go / no-go

There is no GPUI baseline: that frontend was retired before this one existed
(ADR 0065), so every "no worse than X %" line in the plan's budget table is
vacuous and the absolute targets were used instead. This is stated here so that
nobody later reads a relative comparison into these numbers.

**Recommendation: go, with two conditions and one number to accept.**

The reasons to go:

- The bridge holds. A cursor movement over 100 000 entries costs 204 bytes, and
  no measurement needed semantic state to move into TypeScript to meet a
  budget. The renderer has no comparator, no formatter, no keymap and no
  layout rule; every time it needed one, the answer was a projection in Rust —
  four times, listed above.
- The security boundary is narrow and testable: four commands, a closed CSP, a
  capability file granting only event listening, no `window.__TAURI__`, no
  asset protocol, no dev server, and a bundle asserted free of remote origins.
- Start-up and patch sizes pass with room; key-to-paint passes at the
  platform's floor.
- The spike paid for itself before the go/no-go: four defects in the layers
  below, one of which (unserializable patches) would have hit the first real
  renderer on day one.

The conditions:

1. **Restate the frame budgets against the platform's idle frame**, not against
   60 Hz. On this machine WebKitGTK presents at ~30 Hz whatever the display
   does; "p95 ≤ 20 ms" cannot be met by anything that waits for a frame, and
   measuring against the idle floor is the question that has an answer.
2. **A security review before the first mutation is wired.** The slice is
   read-only today; task 3.3 asks for the review while it still is.

The number to accept: **≈ 563 MB of resident memory to display 27 filenames**,
about half of it WebKit's. If that is unacceptable, this is a no-go regardless
of everything above, and the plan's fallback applies — keep `norte-client` and
`norte-ui-host`, archive the renderer, and evaluate a small Slint/Iced one
against the same host. Nothing in phases 1 and 2 depends on this answer.

## Two things about driving this window from a script

- **`GDK_BACKEND=x11` renders a blank webview here.** The process starts, the
  window opens, the title is right — and the page never paints. Under Wayland
  the same binary paints correctly. Practical consequence: on this machine the
  app is Wayland-only, and therefore `xdotool` cannot drive it (it only reaches
  X clients), which is why the scripted checks go through synthetic DOM events
  in the measurement pass rather than through the compositor.
- **Screenshots go through `spectacle -b -n -a -o <file>`.** `grim` refuses
  (the compositor does not expose the screencopy protocol) and `import -window
  root` captures only the X layer.

## Debt this spike leaves

- Watcher, which-key, menu/palette/shortcut views and periodic `session.put`
  are phase-2 debt the renderer will want (already recorded in the plan).
- The host emits ~200 row patches while filling a 100 000-entry listing, which
  overruns a 64-deep subscriber buffer and costs a resync. Correct, but noisy:
  the fill should coalesce to the visible window.
- Log setup is duplicated (~25 lines) rather than shared with `norte-core`,
  because a daemon-only frontend must not depend on the engine to write a line.
- Packaging (`.deb`/AppImage) has not been exercised; `just gui-package` exists
  and is untested.
