# Big rail icons, phase F2 (sixel) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Terminals that speak sixel but not kitty graphics (foot, xterm `-ti vt340`, Windows Terminal, mlterm, Contour; tmux ≥ 3.4 built with sixel) get the same 2×2 icons.

**Architecture:** The startup probe already reads a DA1 reply; it now also reports attribute `4` (sixel). `RailBackend` gains `Sixel`. A pure encoder turns the rasterised icon into a DCS sixel string painted over the slot after `terminal.draw`, through the same `sync` diff. Sixel pixels ARE cells: ratatui repainting a cell erases them, so there is no delete escape and nothing to erase on exit.

**Tech Stack:** Rust, resvg (already in), DEC sixel.

**Spec:** `docs/superpowers/specs/2026-10-05-panel-rail-big-icons-design.md` (phase F2). Builds on F1 (ADR 0169).

## Global Constraints

- Detection by probe only: DA1 attribute `4`, never `TERM`.
- Sixel chosen only if kitty graphics is NOT available and the cell size in pixels is known (`rail_icons::cell_px() != (0, 0)`); kitty wins when both.
- `[ui] images = "off"`/`"blocks"` keep glyphs; `"kitty"` keeps forcing kitty.
- Sixel raster is EXACTLY 2·cell_w × 2·cell_h pixels (no terminal scaling), every pixel inside painted (level 0 = the rail's background) so a recolour overwrites the old image; `P2=1` so rows past the height in the last 6-row band stay untouched.
- Palette: 8 levels from background to stroke colour; no general quantizer.
- Exit/suspend/panic write nothing for sixel.

## Review Focus

1. An overlay over the column then closed: ratatui repaints the cells (erasing the sixel); the next frame must emit it again. Pinned in Task 3 (`sixel_hidden_then_shown_emits_again`).
2. A colour change with the slot unchanged: the new image must fully cover the old. Pinned in Task 2 (`every_pixel_of_the_canvas_is_painted`).
3. Height not a multiple of 6: no pixel set below the declared height. Pinned in Task 2 (`the_last_band_sets_no_row_past_the_height`).
4. Terminal that answers DA1 with `4` but reports no pixel size: glyphs, never a guessed size. Pinned in Task 3 (`choose` cases).
5. Exit after sixel icons: no APC, no DCS written. Pinned in Task 3 (`delete_all_after_sixel_writes_nothing`).

---

### Task 1: The probe also answers sixel

**Files:** Modify `crates/norte-tui/src/kitty_graphics.rs`. Test: unit tests there.

**Produces:** `pub fn sixel_supported() -> bool` (cached, `false` if never asked); private `fn da1_says_sixel(bytes: &[u8]) -> bool`.

- [ ] Failing tests:

```rust
#[test]
fn da1_attribute_4_is_sixel() {
    assert!(da1_says_sixel(b"\x1b[?62;4;22c"));
    assert!(da1_says_sixel(b"\x1b_Gi=31;OK\x1b\\\x1b[?65;1;4c"));
    assert!(!da1_says_sixel(b"\x1b[?62;22c"));
    assert!(!da1_says_sixel(b"\x1b[?62;42c"), "42 is not 4");
    assert!(!da1_says_sixel(b""));
}
```

- [ ] Implement: `SUPPORT: OnceLock<Answer { kitty: bool, sixel: bool }>`; `ask()` returns both from the same bytes; `supported()` reads `.kitty`; log both.
- [ ] `just t norte-tui`; commit `feat(tui): the graphics probe also reports sixel`.

### Task 2: A sixel encoder

**Files:** Create `crates/norte-tui/src/sixel.rs`; `lib.rs`. Test: unit tests there.

**Produces:** `pub fn encode(alpha: &[u8], w: u32, h: u32, fg: [u8; 3], bg: [u8; 3]) -> String` — `alpha` is one coverage byte per pixel, row-major.

- [ ] Failing tests:

```rust
#[test]
fn header_raster_and_palette() {
    let s = encode(&[0; 4], 2, 2, [255, 0, 0], [0, 0, 0]);
    assert!(s.starts_with("\x1bP0;1;q\"1;1;2;2"), "{s:?}");
    assert!(s.contains("#0;2;0;0;0") && s.contains("#7;2;100;0;0"), "{s:?}");
    assert!(s.ends_with("\x1b\\"));
}
#[test]
fn every_pixel_of_the_canvas_is_painted() {
    // 3×7: two bands. Each pixel's bit set in exactly one colour.
    let alpha: Vec<u8> = (0..21).map(|i| (i * 12) as u8).collect();
    let bands = decode_bits(&encode(&alpha, 3, 7, [9, 9, 9], [0, 0, 0]), 3);
    for y in 0..7 { for x in 0..3 { assert_eq!(bands.count(x, y), 1, "({x},{y})"); } }
}
#[test]
fn the_last_band_sets_no_row_past_the_height() {
    let bands = decode_bits(&encode(&[255; 3 * 7], 3, 7, [9, 9, 9], [0, 0, 0]), 3);
    for y in 7..12 { for x in 0..3 { assert_eq!(bands.count(x, y), 0); } }
}
#[test]
fn runs_are_compressed() {
    assert!(encode(&[255; 40 * 6], 40, 6, [9, 9, 9], [0, 0, 0]).contains("!40~"));
}
```

`decode_bits` is a test helper (~30 lines) that walks `#n`, `!n`, `$`, `-` and sixel chars `?`..`~`, counting per (x, y) how many colours set the bit.

- [ ] Implement: level = `(alpha * 7 + 127) / 255`; palette `#i;2;r;g;b` in percent, colour i = bg + (fg − bg)·i/7; per band of 6 rows, per used colour: `#i`, run-length (`!n` when n ≥ 4) sixel chars, `$`; `-` between bands; no bit for rows ≥ h.
- [ ] `just t norte-tui`; commit `feat(tui): a sixel encoder for the panel icons`.

### Task 3: The Sixel backend

**Files:** Modify `rail_icons.rs`, `ui.rs` (`rail_icons_to_place`), `event_loop.rs`. Tests: `rail_icons.rs`, `theme_render.rs`.

**Consumes:** `sixel_supported`, `sixel::encode`, `cell_px`.

- [ ] Failing tests:

```rust
#[test]
fn choose_prefers_kitty_and_needs_cells_for_sixel() {
    use norte_config::Images;
    let px = (9, 19);
    assert_eq!(choose(Images::Auto, true, true, px), RailBackend::KittyGraphics);
    assert_eq!(choose(Images::Auto, false, true, px), RailBackend::Sixel);
    assert_eq!(choose(Images::Auto, false, true, (0, 0)), RailBackend::Glyph);
    assert_eq!(choose(Images::Off, false, true, px), RailBackend::Glyph);
    assert_eq!(choose(Images::Kitty, false, true, px), RailBackend::KittyGraphics);
}
#[test]
fn sixel_sync_paints_a_dcs_and_never_an_apc() { /* sync_with(out, want, Sixel) → contains "\x1bP", not "\x1b_G" */ }
#[test]
fn sixel_hidden_then_shown_emits_again() { /* sync(want); sync(&[]) writes nothing; sync(want) emits DCS again */ }
#[test]
fn delete_all_after_sixel_writes_nothing() { /* sync(want, Sixel); delete_all → "" */ }
```

- [ ] Implement: `RailBackend::Sixel`; `choose(images, kitty, sixel, cell_px)`; `backend(app)` passes `sixel_supported()` and `cell_px()`; `sync(out, want, backend)` — Sixel: on change `MoveTo` + `sixel::encode` of the raster (alpha channel of `png`'s pixmap: add `raster(kind, rgb, canvas) -> Option<Arc<Pixmap>>` and build `png` on it), no delete escapes; the placed state records the backend, and `delete_all` writes only for kitty. Sixel canvas = `(2·cw, 2·ch)` exactly (`canvas_exact`), kitty keeps `canvas_for`. Background for level 0: `Role::Background` bg, else `[0, 0, 0]`.
- [ ] `just t norte-tui`, `just c`; commit `feat(tui): big panel icons over sixel`.

### Task 4: Verify in a real sixel terminal, docs, close

- [ ] Manual: `xvfb-run -s "-screen 0 1000x700x24" xterm -ti vt340 -xrm 'XTerm*decTerminalID: vt340' -xrm 'XTerm*numColorRegisters: 256' -e ntc` (sandboxed HOME/XDG as in memory `tui-harness-tmux`), screenshot with `import -window root`, read the PNG. Expect: icons 2×2, open bright, closed dim, no stretching.
- [ ] ADR 0169 gains a "Sixel (F2)" paragraph; CHANGELOG line extended; help sentence lists foot/xterm; `NORTE_UPDATE_GOLDEN=1 just t norte-cli`.
- [ ] `just ci-fast`; review (rust-reviewer, opus) before merge; `just link`, `just link-gui`.
