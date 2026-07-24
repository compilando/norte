# G3 — WASM data-out surfaces (WIT v2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Plugins return STRUCTURED data the host paints: styled preview spans (syntax highlighting in real color), row decorations (git-style badges), real columns; plus GUI parity for the palette and extension manager.

**Spec:** G3 in `docs/superpowers/specs/2026-07-23-gui-visual-plugins-design.md:161-187`. Guardian mandatory (wire + WIT); security-reviewer (new guest surfaces); encoding-auditor (every new painted string).

**Facts (verified):**
- TUI ALREADY paints styled plugin previews via ANSI SGR parsing: `Viewer::with_plugin_preview` → `ansi::parse_sgr` → `StyledLine{StyledSpan{text, fg:Option<Rgb>}}` masked per span (norte-frontend viewer.rs:149, ansi.rs:18-48); TUI draw_viewer builds per-span ratatui Spans (ui.rs:531-549). GUI renders the same preview PLAIN (one SharedString per row, main.rs:1643).
- Proto 0.26.0; `PluginPreview{plugin_id, plugin_name, output:String}` all-or-nothing; PluginPreviewResult flattened Option.
- WIT `norte:plugin@0.5.0`; P2's EMPIRICAL lesson: ANY package bump invalidates ALL prebuilt guest artifacts (header comment wit:24-44) — in-repo guests rebuild via symlink; ftp-provider.wasm must be rebuilt via `just build-ftp-wasm`.
- Runtime return cap 4 MiB reused for v2 (spec:211). Backend plumbing for plugins_list/set_approval/set_enabled/run_command/preview all EXISTS and is GUI-reachable (backend.rs:596-819); the GUI simply never calls the management ones.
- Columns/hook manifest contributions exist since M4 with no WIT.

**Decisions locked (the ADR formalizes 1-3; guardian may veto → re-plan):**
1. **Wire shape: new method `plugin.preview_styled`** (NOT a flag on plugin.preview): `PluginPreviewStyledResult { preview: Option<PluginPreviewStyled{ plugin_id, plugin_name, lines: Vec<StyledLineWire> }> }`, `StyledLineWire = Vec<SpanWire{ text: String, role: Option<String>, fg: Option<[u8;3]> }>`. Rationale: all-or-nothing shape stays simple; old daemons reject unknown method cleanly (client falls back to plain `plugin.preview`); no flag-negotiation state. Caps ON THE WIRE: ≤10k lines, ≤64 spans/line, span text ≤4KiB, total ≤4MiB (server enforces; client re-validates fail-closed to plain).
2. **WIT 0.6.0**: previewer interface gains `render-styled: func(input: preview-input) -> result<styled-text, string>` (`span { text: string, role: option<string>, fg: option<tuple<u8,u8,u8>> }`, `styled-text = list<list<span>>`); NEW interfaces `decorator { decorate: func(entries: list<list<u8>>) -> list<decoration> }` (`decoration { badge: option<string>, role: option<string> }`, batched per visible page, positional 1:1 with input) and `columns { column-values: func(id: string, entries: list<list<u8>>) -> list<string> }`. Guests EXPORT render-styled optionally? Component model: a world's exports are fixed — so TWO worlds: `norte-plugin` (unchanged shape at 0.6.0: previewer+command with render-styled ADDED to the previewer interface — old in-repo guests recompile; render-styled is a new required export of the interface... WIT interfaces are all-or-nothing per export) → DECISION: `render-styled` is a new REQUIRED function of the previewer interface at 0.6.0 (guests recompile anyway per the bump lesson; a plain-only guest implements it as `render` + single span). Decorator/columns get their own NEW WORLDS (`norte-decorator`, `norte-columns`) mirroring `norte-provider`'s pattern, resolved by manifest category. Roles: closed validation host-side against norte-theme Role names (unknown role → None, warn).
3. Host paint: role resolves through the theme (TUI theme.role / GUI ChromeColors seam); raw fg clamps (validated RGB, glow applies GUI-side); EVERY span text masked (extend the existing per-span masking in with_plugin_preview's structured twin). Badges: ≤8 chars after masking, painted as an extra Span (TUI, in-band per existing badge note) / child div (GUI); role-colored.
4. GUI palette + extension manager: reuse H1/P1 designs 1:1 (COMMANDS join + plugin rows `[extensión]`-prefixed; manager rows with description + approve/enable via existing Backend calls; ConfirmQuit-style modal conventions; Enter-never-approves pinned). F12/ctrl+p bindings via the GUI's build_for_subset (commands `app.palette`, `app.extensions` added to GUI COMMANDS — shared presets already bind them).
5. Sub-phasing: G3a (T1-T3: ADR+wire+WIT+styled previews e2e both frontends) → G3b (T4: decorators+columns TUI-first; GUI columns minimal) → G3c (T5: GUI palette+manager). Each sub-phase lands green independently.

---

### Task 1: ADR 0037 + proto bump (guardian)

- [ ] ADR 0037 "Plugin data-out v2: styled previews, decorators, columns" — decisions 1-3 + the WIT-bump-invalidates-artifacts consequence (rebuild story; in-repo guests + ftp-provider.wasm) + caps table + role-validation contract.
- [ ] Proto 0.26.0→0.27.0: `PLUGIN_PREVIEW_STYLED` const + types per decision 1 (serde tolerant; goldens for empty/populated/hostile-shaped; N-1 window shift; the version-literal daemon-test trap — grep it proactively).
- [ ] protocol-guardian on the diff; apply findings. Commit(s).

### Task 2: WIT 0.6.0 + runtime + guests

- [ ] WIT bump per decision 2 (header comment updated with the 0.6.0 invalidation note); runtime render_styled path (cap enforcement BEFORE decode: line/span/text caps + 4MiB total; RuntimeError variants); decorator/columns worlds + instantiation paths (mirror provider's bindgen `with:` mapping); previewer-demo gains render-styled (real highlight demo: e.g. simple keyword coloring), command-demo recompiles; `just build-ftp-wasm` rerun (0.6.0 strings). e2e: styled preview round-trip real WASM; old-artifact instantiation failure EXPECTED (document, don't fight).
- [ ] Commit + targeted suites green.

### Task 3: styled previews end-to-end

- [ ] norte-core: `Backend::plugin_preview_styled` (embedded: resolve→render-styled→validate caps+roles→PluginPreviewStyled; remote: call method, on MethodNotFound fall back to plain — check the error taxonomy for unknown-method detection) + daemon handler + registry resolve reuse.
- [ ] norte-frontend: `StyledSpan` gains `role: Option<Role>` (parsed+validated); `Viewer::with_plugin_preview_styled(lines)` (mask per span; reuse StyledLine); TUI draw_viewer resolves role→theme style (fg fallback), GUI render_viewer paints flex-rows of per-span child divs (role→ChromeColors/entry seam; glow applies; mono font keeps).
- [ ] TUI/GUI viewer open paths try styled first, fall back to plain (F3 flow); tests: hostile spans (bidi in text → masked; unknown role → plain), snapshot TUI styled preview, GUI smoke screenshot with the demo highlighter.
- [ ] encoding-auditor on the styled path; apply. Commit. **G3a gate: just ci + check-gui.**

### Task 4: decorators + columns (G3b)

- [ ] Registry: resolve decorators/columns by manifest category (approved+enabled only); listing pipeline: after a page renders, batched `decorate(visible entries)` per plugin (async, never blocks listing — dead plugin = no decorations, same fallback contract; results keyed positionally then attached by path). TUI: badge Span in list_item (masked ≤8 chars, role-colored); columns: TUI DEFERRED (Rect-split restructure — note as follow-up; spec says "GUI (and later TUI)" — flip: GUI first per spec wording? spec says rendered in GUI and later TUI — do GUI column cells (flex fixed-width) + TUI deferred, honest note). Wire: decorations/columns need daemon transport for remote mode → NEW methods `plugin.decorate`/`plugin.column_values` (guardian again — fold into T1's bump if foreseeable: DO fold into 0.27.0 in T1 to avoid two bumps; adjust T1 scope accordingly).
- [ ] e2e with a demo decorator guest; tests hostile badges. Commit. G3b gate.

### Task 5: GUI palette + extension manager (G3c)

- [ ] GUI COMMANDS += app.palette/app.extensions (shared presets already bind ctrl+p/F12 — subset build picks them up); palette overlay (H1 design: filter, rows, plugin rows via plugins_list, Enter dispatch incl. plugin_run_command; masked); extension manager overlay (P1 display: rows+description+badges, y/e toggles via existing Backend calls, human-only semantics pinned: Enter never approves — reuse the modal conventions); reduce_motion-respecting (no animation needed).
- [ ] Settings display (the P2 deferral): manager shows `plugin-config`-style lines — requires settings on the wire → they are NOT (host-side only). Honest scope: EMBEDDED mode shows settings via a direct registry accessor behind the Backend enum's Embedded arm; Remote shows "(daemon mode: settings not exposed)" — OR defer again. DECIDE small: defer wire exposure, show embedded-only with the mode note; document.
- [ ] Tests: GUI palette/manager unit + hostile snapshots + smoke screenshots. encoding pass. Commit. **G3 final gate: just ci + check-gui + rust-reviewer whole-range + memory.**

## Self-review notes
- Spec G3 covered: styled previews ✓ (new method, ADR-decided), decorators ✓, columns GUI-first/TUI-deferred (honest inversion of the spec's parenthetical, documented), GUI palette+manager ✓, caps/masking everywhere, guardian at T1 (and T4's methods folded into the SAME 0.27.0 bump). P2's settings-display deferral resolved embedded-only with note.
- Biggest risks flagged: WIT world/export all-or-nothing semantics (decision 2 resolves via required-export + recompile-anyway), unknown-method fallback detection (T3 checks error taxonomy), two-bump avoidance (T4 folded into T1).
