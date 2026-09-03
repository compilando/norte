# D2 — `org.norte.media-info`: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** two columns a person adds to a listing — `dims` (`1920×1080`) for
images and `duration` (`3:41`) for audio — computed from file HEADERS read
under the location token, never from whole files.

**Architecture:** a `columns` guest (`norte-columns` world) with
`location = "read"` and no root marker: it reads under the listed directory
only. For each entry whose extension claims a media type it calls
`location::read-prefix(token, name, 64 KiB)` and, for MP3 (CBR estimate),
`stat` for the size; anything else is `None` without opening the file. Each
format is a pure parser over `&[u8]` in its own module, tested on the host
with hand-built headers. Prerequisite done: `norte:location@0.2.0` with
`read-prefix` (its own commit, ADR 0094 amendment).

**Tech Stack:** wit-bindgen 0.46 guest on `wasm32-wasip2`, std, no crates;
`run_column_values_for_test` in `norte-core` (feature `testing`) for the e2e.

**Spec:** `docs/superpowers/specs/2026-09-03-plugin-kit-and-demo-plugins-design.md`, section D2.

## Global Constraints

- Prefix of 64 KiB (`PREFIX_MAX`), never `read`. A truncated or unknown
  header yields `None`, never a guess.
- Files are opened only when the extension claims a media type (bytes,
  ASCII case-insensitive, last dot).
- Cells are short ASCII: `WxH` with `×`, `m:ss` / `h:mm:ss`.
- Guest outside the workspace; `cdylib` + `rlib` with the WIT glue behind
  `#[cfg(target_arch = "wasm32")]` (as `file-icons`).

---

### Task 1: the parsers

**Files:** `plugins/media-info/src/{image.rs, audio.rs, format.rs}`

**Produces:**
```rust
// image.rs
pub fn dims(bytes: &[u8]) -> Option<(u32, u32)>;   // PNG, JPEG, GIF, WebP
// audio.rs
pub fn duration_secs(bytes: &[u8], file_len: u64) -> Option<u64>; // WAV, MP3, FLAC
// format.rs
pub fn dims_cell(w: u32, h: u32) -> String;        // "1920×1080"
pub fn duration_cell(secs: u64) -> String;         // "3:41", "1:02:03"
```
Formats: PNG (`\x89PNG\r\n\x1a\n`, IHDR at 16: two BE u32); JPEG (`FFD8`,
walk markers until SOF0/1/2 `FFC0/C1/C2`: height then width BE u16 at
offset 5 of the segment); GIF (`GIF8?a`, LE u16 at 6 and 8); WebP (`RIFF…WEBP`,
`VP8 ` frame at 20: LE u16 & 0x3fff at 26/28; `VP8L`: 14-bit fields from
bytes 21..; `VP8X`: 24-bit LE at 24 and 27, plus one). WAV (`RIFF…WAVE`,
`fmt ` chunk: byte rate at +8; `data` chunk size → `data / byte_rate`);
FLAC (`fLaC`, STREAMINFO block: sample rate 20 bits at byte 10, total
samples 36 bits at byte 13); MP3 (skip `ID3` tag if present, first frame
header `FFF`/`FFE`: version, layer, bitrate index, sample rate index; if a
Xing/Info frame follows with the frames flag, `frames × samples_per_frame /
sample_rate`; else CBR: `(file_len − tag) × 8 / bitrate`).

- [ ] **Step 1:** tests in each module over hand-built byte arrays (a 1×1
  PNG header, a JPEG with SOF0 for 640×480 after an APP0 segment, a GIF
  `GIF89a` 16×8, a WebP VP8X 100×50, a 44-byte WAV header claiming 2 s, a
  FLAC STREAMINFO for 44.1 kHz × 441 000 samples = 10 s, an MP3 frame
  header for 128 kbps / 44.1 kHz with `file_len` giving 60 s, and one with a
  Xing frame count giving 30 s); truncation of each (header cut short) →
  `None`; `format::duration_cell(3661) == "1:01:01"`, `dims_cell(1920,1080)
  == "1920×1080"`.
- [ ] **Step 2:** RED. **Step 3:** implement. **Step 4:** `cargo test` in the plugin dir — GREEN.

### Task 2: the guest, the manifest, the e2e

**Files:** `plugins/media-info/{Cargo.toml, plugin.toml, help.md, .gitignore, wit, src/lib.rs}`,
`crates/norte-core/tests/columns_media_e2e.rs`, `justfile` (`plugin-media-info`, `plugins`),
`plugins/README.md`, `CHANGELOG.md`.

Manifest: id `org.norte.media-info`, category `columns`, two
`[[contributions.columns]]` (`dims`/`Dims`, `duration`/`Length`),
`[capabilities] location = "read"`. `lib.rs`: for `column-values(id, location,
entries)`, without a location every cell is `None`; per entry, if the
extension is an image extension and `id == "dims"` → `read-prefix` + `dims`;
if audio and `id == "duration"` → `read-prefix` + `stat` + `duration_secs`;
else `None`. Unknown id → all `None`.

- [ ] **Step 1: failing e2e** (pattern: `columns_git_e2e.rs`): write into a temp
  directory a 1×1 PNG (real bytes), a 2 s WAV, a `notes.txt`, a `.png` that is
  three bytes of garbage, and a 200 KiB file named `big.png` whose first 24
  bytes are a valid 8×4 PNG header; install the plugin from `plugins/media-info`,
  approve+enable, `run_column_values_for_test(…, "dims", Some(dir), false, names…)`
  → `[Some("1×1"), None, None, None, Some("8×4")]`; `"duration"` → `[None,
  Some("0:02"), None, None, None]`; `"nope"` → all `None`; `location_dir: None` →
  all `None`. The `big.png` case is what `read-prefix` buys: the cell exists
  and the budget after the call is under 64 KiB for that file (assert through
  a second call with a `Bounds` of `max_total_bytes = 128 KiB` if the test
  helper exposes bounds; if not, the cell alone proves the prefix path).
- [ ] **Step 2:** RED. **Step 3:** guest + manifest. **Step 4:** `just t norte-core` — GREEN.
- [ ] **Step 5:** recipe, README row, CHANGELOG. `just plugin-media-info` here.
- [ ] **Step 6:** commit `feat(plugins): media-info, dims and duration from headers under the location token (D2)`.

### Task 3: close

- [ ] `just ci-fast` (ONE; the hook runs it on commit). `just ci`'s `cov` if
  core's coverage moved (core gained code: `read_prefix` in vfs-local is
  under the 85 % gate — run `just cov`). Memory; merge.
