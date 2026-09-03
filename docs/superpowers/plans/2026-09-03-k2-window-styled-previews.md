# K2 — the window paints styled previews: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a plugin preview with roles and colours looks the same in the
window as in the TUI. Today `ViewerView.lines` is plain text; the styled
spans reach the shared viewer model and stop there.

**Architecture:** the ui-host projects the shared viewer's styled rows
(`norte_frontend::viewer::Viewer::plugin_styled_rows`) into a new DTO field
`ViewerView.styled: Vec<Vec<SpanView>>` — role as its kebab name, colour as
`#rrggbb`, text already masked at entry and clamped here like every other
string. Bridge **49**. The renderer paints one `<span>` per span: a
`data-role` the stylesheet maps to the theme variables (as row badges already
do), or an inline colour when there is no role. Role wins over `fg`, as in
the TUI (ADR 0037 decision 3). `lines` stays for the raw view.

**Tech Stack:** norte-ui-host (Rust), norte-gui-tauri renderer (TypeScript,
`render.ts`, `types.ts`, `style.css`), `just gui-ci`.

**Spec:** `docs/superpowers/specs/2026-09-03-plugin-kit-and-demo-plugins-design.md`, section K2.

## Global Constraints

- No protocol change: the wire already carries `SpanWire { text, role, fg }`.
- Bridge bump 48 → 49 in BOTH `crates/norte-ui-host/src/bridge.rs` and
  `crates/norte-gui-tauri/ui/src/types.ts`, with the log line in `bridge.rs`.
- Third-party text is masked once at entry (`with_plugin_preview_styled`
  already does) and clamped in the DTO (`clamp_display`).
- `just t norte-ui-host` in the loop; `just gui-ci` for the renderer; one
  `just ci-fast` for the plan; `just ci` at the close. `ci` does NOT run
  `gui-ci`.

---

### Task 1: the DTO carries spans

**Files:**
- Modify: `crates/norte-ui-host/src/dto.rs` (`ViewerView`, new `SpanView`)
- Modify: `crates/norte-ui-host/src/controller/views.rs` (`vista_visor`)
- Modify: `crates/norte-ui-host/src/bridge.rs` (49 + log line)
- Test: `crates/norte-ui-host/tests/controller.rs` (next to the existing
  "PDF de ACME" styled-preview test around line 7530), `tests/golden.rs`
  (the viewer golden gains the field; regenerate with the crate's golden
  update switch — read the header of `golden.rs` for the variable)

**Produces:**
```rust
/// One styled fragment of a plugin preview line.
pub struct SpanView {
    pub text: String,             // masked at entry, clamped here
    pub role: Option<String>,     // kebab name of a norte_theme::Role, validated
    pub fg: Option<String>,       // "#rrggbb", the plugin's own colour
}
// ViewerView gains:
pub styled: Vec<Vec<SpanView>>,   // empty for the raw view; else one entry per `lines` row
```

- [ ] **Step 1: failing test** — with the backend fake returning a styled
  preview of two lines (`[{text:"TODO", role:"title", fg:[255,0,0]}, {text:" x"}]`,
  `[{text:"plain"}]`), open the viewer and assert `v.styled.len() == 2`,
  `v.styled[0][0].role == Some("title")`, `v.styled[0][0].fg == Some("#ff0000")`,
  `v.styled[0][1].role.is_none()`, `v.lines` equals the flattened text, and a
  raw file (no previewer) yields `styled.is_empty()`. A span whose text
  carries a bidi override arrives masked (`display_name` marks it) — assert
  the mark, not the raw byte.
- [ ] **Step 2:** `just t norte-ui-host` — RED.
- [ ] **Step 3:** implement: in `vista_visor`, `styled: v.plugin_styled_rows(alto).map(|rows| rows.iter().map(|line| line.iter().map(span_view).collect()).collect()).unwrap_or_default()`,
  with `span_view` mapping `role.map(Role::kebab)` (find the accessor the
  theme exposes — `from_kebab` exists; if only serde produces the name, add
  `Role::as_kebab()` next to it with a roundtrip test) and
  `fg.map(|(r,g,b)| format!("#{r:02x}{g:02x}{b:02x}"))`. Bump the bridge.
- [ ] **Step 4:** GREEN; regenerate the golden; `just t norte-ui-host`.
- [ ] **Step 5:** commit `feat(ui-host): the viewer projection carries a plugin preview's spans (bridge 49)`.

### Task 2: the renderer paints them

**Files:**
- Modify: `crates/norte-gui-tauri/ui/src/types.ts` (`BRIDGE_VERSION = 49`, `SpanView`, `ViewerView.styled`)
- Modify: `crates/norte-gui-tauri/ui/src/render.ts` (`paintViewer`: when `viewer.styled.length > 0`, build the body from spans instead of `lines.join`)
- Modify: `crates/norte-gui-tauri/ui/src/style.css` (`.viewer-span[data-role=…]` → the theme variables the badges use: `--title-fg`, `--warning-fg`, `--error-fg`, `--info-fg`, `--match-*`; unknown role = inherit)
- Test: the renderer's existing test suite under `crates/norte-gui-tauri/ui/` (find the viewer test and add: a styled view produces one `span` per span, `data-role` set when present, `style.color` set only when there is no role, text content equals the line).

- [ ] **Step 1:** failing renderer test. **Step 2:** `just gui-ci` — RED.
- [ ] **Step 3:** implement. Each line is a `div.viewer-line` (keeps `pre`
  semantics and one row per line for `viewerRows` arithmetic); each span
  `span.viewer-span` with `textContent` (never `innerHTML`), `dataset.role`
  when the role is present, `style.color = fg` only when the role is absent.
- [ ] **Step 4:** `just gui-ci` — GREEN. Open the window on this machine with
  syntect approved and a `.rs` file: colours visible; a plain file: unchanged.
- [ ] **Step 5:** commit `feat(gui): the window paints a plugin preview's roles and colours`.

### Task 3: close

- [ ] CHANGELOG entry (bridge 49; what the window shows now; role over fg).
- [ ] `just ci-fast` (ONE). `just ci` (ONE, recipes one at a time if needed) — and `just gui-ci`, which `ci` does not run.
- [ ] Memory note; merge.
