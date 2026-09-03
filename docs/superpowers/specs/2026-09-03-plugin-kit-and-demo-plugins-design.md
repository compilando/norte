# The plugin kit, and four demo plugins for the first release: design

**Date:** 2026-09-03
**Status:** approved in conversation, pending written review
**Builds on:** ADR 0022 (manifest and sandbox), ADR 0037 (data-out v2),
ADR 0041 (core versus plugin providers), ADR 0057 (location capability),
ADR 0093 (a provider plugin serves the scheme it declares),
`2026-08-07-plugins-and-delivery-plan.md`

## Why

M4's exit criterion is "a third party can ship a plugin without changing the
core". After ADR 0093 the mechanism is complete for five plugin kinds, and
still nobody outside this repository has ever compiled one: there is no author
guide, no template, no statement of what a WIT bump does to a compiled
binary, and a plugin built against the previous package dies with a wasmtime
error that names an import and nothing else.

The first release should also show what the mechanism is *for*. The two
plugins that exist are proofs (syntect, git status). This program adds four
that a person would install because they like them, and the kit that lets
someone else write the fifth.

## What is true today, and what shapes the design

- **A preview span carries `text`, an optional `role` and an optional `fg`.**
  No background colour, and the guest does not know the viewer's width. Half-
  block image rendering needs both. This is the first real WIT gap since
  ADR 0041 said gaps close when a plugin needs them.
- **The window paints plain lines in plugin previews.** `ViewerView.lines` is
  `Vec<String>`; the styled spans reach the shared viewer model and stop
  there. The TUI paints them.
- **Theme roles are chrome roles** (`title`, `error`, `match`, `mark`…), not
  content roles. Previews use `fg` for their own palette, as syntect does;
  decorators use existing roles. No new roles until the demos show a need.
- **Mimetypes are guessed by extension** in `norte-core::plugins::guess_mimetype`
  and know no `image/*` and no `text/markdown`. Two previewers may match one
  file (`text/*` and `text/markdown`); today the first in catalogue order wins.
- **A component's imports are readable without compiling it.** `wasmparser`
  (already in the tree under wasmtime) lists `norte:plugin/previewer@0.8.0`
  and friends from the binary. That is how the host can say which WIT a
  plugin was built against.
- **Demo plugins live in `plugins/<name>/`** like `git-status`: outside the
  workspace, own lockfile, `cdylib` + `rlib` so pure parsers get host-side
  tests, `wit` symlinked to the host's package directory, an end-to-end test
  in `norte-core/tests/` that builds, installs and runs the real guest.
  Shipping them inside release packages is phase 7's question (#256); this
  program installs them from source with `just`.

## Sub-projects

Six, each one session, in this order. K1 and K2 come first because every demo
depends on them: K1 is where a third party starts, and K2 is what makes a
previewer look the same in both frontends.

### K1 — the kit

**Deliverables**

1. `docs/plugins.md`, in English: what a plugin is (WASM component, sandbox,
   the five kinds), the manifest field by field (`[plugin]`, contributions per
   kind, `[capabilities]` with what each grants and what approving shows,
   `[config]`), the directory layout (`plugin.toml`, `plugin.wasm`, optional
   `help.md`, `config.toml` written by the host), how to build
   (`wasm32-wasip2`, `wit-bindgen` with `generate_all`, the `wit` symlink),
   install (`norte plugin install`), consent (nothing runs until approved and
   enabled; `--force` and `uninstall` withdraw it), `norte plugin list`,
   `norte doctor`, and the compatibility policy below. Linked from
   `docs/README.md` and the README's Plugins section.
2. `plugins/README.md`: the official plugins, one line each, and how
   `just plugins` installs them.
3. `plugins/template/`: the smallest previewer+command guest that builds
   (`norte-plugin` world), with a commented `plugin.toml`, `help.md`, the
   symlink, and a README saying which files to rename. Built by the gate like
   the other guests (a test in `norte-core/tests/` installs it and runs its
   command), so the template cannot rot.
4. ADR 0094, *a plugin says which WIT it was built against, and the host
   says whether it serves it*:
   - The package version is part of every import name, so a bump — any bump
     — makes a previously compiled guest fail to instantiate. This is a fact
     of the component model, not a policy; the policy is what norte does
     about it.
   - `Catalog::load_dir` reads the component's imports with `wasmparser` and
     records, per plugin, the `norte:*` packages and versions it imports.
     `PluginEntry` gains `wit: Vec<(String, String)>` (package, version).
   - A plugin whose imports name a package version the host does not serve
     is **listed, not loaded**: it goes to `errors` with a new
     `LoadError::WitMismatch { package, built_against, served }`, the manager
     shows it as broken with that reason, `norte plugin list` counts it, and
     `norte doctor` names it (`plugin-wit-mismatch`). The state file is not
     touched. A recompiled guest is a new binary, and the approval anchor
     covers the binary (#241), so it has to be approved again: the guide says
     so, and says why.
   - Norte's own packages are versioned like this: `norte:host` changes
     rarely and independently; `norte:plugin`, `norte:provider` and
     `norte:location` bump their minor for any change, and the CHANGELOG
     names the bump and the interfaces it touched. A release note template
     line: "plugins built against `norte:plugin@0.8.0` need a rebuild".
   - No compatibility window. The host serves exactly one version of each
     package. A window would mean keeping every old world linked, and the
     first plugin that needs a gap closed (D4) is the argument for not
     promising that yet.
5. `just plugin-git-status` (stage and install, like `plugin-syntect`) and
   `just plugins` (all official plugins, in one go, skipping the ones already
   installed unless `--force`).

**Tests.** `wit_packages.rs` gains a case that inspects an in-tree guest's
imports through the new reader and finds the served versions. A catalogue test
plants a guest built against a fake `norte:plugin@0.1.0` (a hand-written
component is not needed: rewrite the import name bytes of a real guest in the
test, or use `wasm-encoder`) and asserts `WitMismatch` with both versions.
Doctor and CLI list get a case each. The template's E2E.

**Definition of done.** Guide linked and read end to end by following it once
from an empty directory outside the repository (the session does this by
hand and records the commands in the guide). ADR 0094. `just plugins`
installs syntect and git-status on this machine. Gate green.

### K2 — the window paints styled previews

Bridge **49**. `ViewerView` gains `styled: Vec<Vec<SpanView>>` alongside
`lines` — one entry per visible line, each a list of `SpanView { text, role:
Option<String>, fg: Option<String> }` where `fg` is `#rrggbb`. `lines` stays
for the raw view and for renderers that do not paint spans; when `styled` is
non-empty it is the same text, and the renderer paints it instead. Roles are
validated against `norte_theme::Role` in the ui-host as the TUI already does;
the role wins over `fg` when both are present (ADR 0037 decision 3). Text is
masked per span at entry, as the TUI does.

`render.ts` paints spans as `<span>` with a class per role and an inline
colour for `fg`. `types.ts` mirrors the DTO.

**Tests.** A ui-host test feeds a styled preview through the backend fake and
asserts the DTO carries roles, colours and masked text; the bridge version
test; `just gui-ci`.

### D1 — `org.norte.file-icons` (decorator)

A badge per row from the name alone: kind by extension (code, document,
image, audio, video, archive, config, script) and the special names
(`Makefile`, `Cargo.toml`, `.gitignore`, dotfiles). No capabilities. `[config]
style = "emoji" | "ascii"` (default `emoji`; the ASCII set is for terminals
without emoji fonts: `{}` code, `[]` archive, `~` media, `#` config). No role
in v1: the badge is the glyph; the theme's regular text paints it.

The mapping table is data in the guest (`icons.rs`), tested on the host with
the hostile-name corpus: a name that is bytes is matched on its bytes, an
extension is the tail after the last dot as ADR-defined for `mark.extension`,
and no name produces a badge longer than the host's 8-cell cap after masking.

**Tests.** Host-side table tests; E2E installs the guest, lists a directory
with a dozen names and checks the badges positionally; `[config]` switched to
`ascii` through `plugin.set_config` changes the badges.

### D2 — `org.norte.media-info` (columns)

Two columns: `dims` (`1920×1080`) for PNG, JPEG, GIF and WebP, and
`duration` (`3:41`) for WAV, MP3 (frame header, CBR estimate; VBR from the
Xing/Info frame when present) and FLAC (STREAMINFO). `location = "read"`,
no root marker: the guest reads under the listed directory only. Every reader
is a header parser over a bounded prefix (64 KiB), never the whole file, and
an unrecognised or truncated header yields `None` for that cell. Files whose
extension does not claim a media type are not opened at all.

Parsers are pure functions over `&[u8]` with host-side tests on hand-built
headers (no binary fixtures); the E2E writes a 1×1 PNG and a short WAV into a
temp directory and reads the two cells through the real location token.

### D3 — `org.norte.markdown` (previewer)

`pulldown-cmark` (no HTML rendering). Headings in a heading colour with a
`#`-free prefix, emphasis and strong via `fg`, inline code and fenced code
blocks in a code colour with their fence markers dropped, lists with `•` and
indentation, block quotes with `│`, links as `text (url)`. Output is styled
lines; `render` is the same lines flattened.

Core changes that come with it: `guess_mimetype` maps `md`/`markdown` to
`text/markdown`; `resolve_previewer` prefers an exact mimetype match over a
glob, and among equals keeps catalogue order (documented in ADR 0037's
amendment log). Tests for both.

### D4 — `org.norte.image-ansi` (previewer)

The plugin that closes a WIT gap, by the rule of ADR 0041 decision 3.

`norte:plugin@0.9.0`: `span` gains `bg: option<tuple<u8, u8, u8>>` and
`preview-input` gains `columns: option<u32>` (the viewer's width in cells,
`none` when the host does not know it — the CLI). Every in-tree guest is
recompiled; the template is updated; ADR 0094's mechanism reports the
previous version for any stale binary, which is the policy's first live test.
The TUI paints `bg` on spans; the window does too (bridge 50, `SpanView.bg`).

The guest decodes PNG, JPEG and GIF (first frame) with the `image` crate, fits
the picture to `columns` (default 80) keeping aspect with 2:1 cell geometry,
and emits one `▀` per cell pair with `fg` = upper pixel and `bg` = lower
pixel. `guess_mimetype` learns `png`, `jpg`/`jpeg`, `gif`, `webp` →
`image/*`; the previewer declares `image/png`, `image/jpeg`, `image/gif`. The
host's 1 MiB read cap applies; a larger file is refused by the previewer
with a message rather than rendered from a truncated prefix (`content` is
what the host read, and a truncated JPEG decodes to garbage).

**Tests.** Host-side: fitting arithmetic and the half-block encoder over a
4×4 synthetic image. E2E: a generated 8×4 PNG through the real guest, asserting
line count, span count per line and two known colours. The WIT bump gets its
own commit with the recompiled guests, before the plugin.

## Not in this program

- Shipping demo plugins inside deb/RPM/AppImage (#256, phase 7).
- Content roles in the theme. Revisit after D3 and D1 are seen in both
  frontends.
- A plugin registry or signatures (spec §7.1 defers them).
- The `renamer` category and hooks (next items of the plugin roadmap).

## Order at a glance

```
K1 kit ──► K2 window colours ──► D1 icons ──► D2 media-info ──► D3 markdown ──► D4 image (WIT 0.9.0)
```

K1 before anything because the template and the guide are what the demos are
written against. K2 before D3/D4 so a previewer is judged in both frontends.
D1 and D2 before the previewers because they need no WIT change and show two
kinds the window already paints. D4 last: it moves the WIT, and everything
before it is what that move must not break.
