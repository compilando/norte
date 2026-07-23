# H1 — dialog keymap context + command palette Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Modal/overlay keys become keymap data in a new `dialog` context with GENERATED footer hints (closes issue #24 — rebinding can no longer desync a hint), and the TUI gains the spec-promised command palette (`app.palette`).

**Architecture:** The engine's closed context set (`global`/`pane`/`viewer`) grows a `dialog` section + `Screen::Dialog`. A new closed vocabulary of `dialog.*` commands expresses overlay actions; each overlay declares which it supports and ignores the rest — safety semantics (Enter never approves/trusts) stay in code as allowlists, pinned by tests. Footer hints are computed as the join of an overlay's supported commands × the effective dialog keymap × Fluent labels — the same pattern as F1 help. The palette reuses `nav::QuickSearch` filtering over `COMMANDS` + effective bindings + help descriptions.

**Spec:** H1 in `docs/superpowers/specs/2026-07-23-help-config-system-design.md`. Palette plugin rows wait for P1 (`PluginInfo.commands`) — this plan ships human commands only, with the plugin slot noted.

**Facts (verified):** contexts are hardcoded struct fields (`KeymapFile.global/pane/viewer`, keymap.rs:282-302) + fixed arrays at keymap.rs:529/538 + `Screen` closed enum (330) + screen→section match (550-553). Modal key handling: run-loop priority chain main.rs:686-735; pure `dialog_key` app.rs:1163-1190; overlay handlers `on_theme_picker_key` (959), `on_extensions_key` (1015), `on_nav_popup_key` (1064), help inline (712-723). Static hint strings: `modal-collision-keys`/`modal-confirm-keys`/`modal-approval-keys`/`modal-trust-host-keys` (en.ftl:12-23). Palette raw material: `nav::QuickSearch` (nav.rs:118, `matches` at 103), overlay render idioms `draw_nav_popup` (input footer, ui.rs:143-211) + `draw_theme_picker` (ui.rs:295-316), overlay state pattern `ExtensionManager` (app.rs:613-655), dispatch arm pattern main.rs:2435-2461, coverage test `todo_comando_tiene_ayuda_traducida` (tests/keymap.rs:486).

**Decisions locked:**
1. `dialog.*` command vocabulary (closed, engine-validated like any command): `dialog.confirm`, `dialog.cancel`, `dialog.approve`, `dialog.deny`, `dialog.overwrite`, `dialog.skip`, `dialog.rename`, `dialog.newer`, `dialog.up`, `dialog.down`, `dialog.page-up`, `dialog.page-down`, `dialog.toggle-approval`, `dialog.toggle-enabled`, `dialog.add`, `dialog.remove`. One flat `dialog` context; per-overlay ALLOWLISTS give each modal its semantics.
2. Safety stays code: ApproveAgentOp's allowlist maps `dialog.approve`→approve and `dialog.deny`/`dialog.cancel`→deny and EXCLUDES `dialog.confirm` (so Enter — bound to confirm — is inert there); TrustHostKey likewise (`dialog.approve` = trust). Pinned by tests.
3. Preset `[dialog]` bindings identical across orthodox/vim/cua (uniform muscle memory): y→approve+confirm? NO — one chord one command: `enter→dialog.confirm`, `y→dialog.approve`, `n→dialog.deny`, `esc→dialog.cancel`, `o→dialog.overwrite`, `s→dialog.skip`, `r→dialog.rename`, `w→dialog.newer` (was `n` for newer in collision — COLLISION CONFLICT: old collision used `n`=newer, but `n` now globally means deny/cancel-ish. Resolve: `w` (neWer) with the generated hint making it discoverable; ADR-less UX change, note in commit), `up/down/pgup/pgdn→dialog.up/down/page-up/page-down`, `k/j→dialog.up/down` (vim only, appended in vim preset), `a→dialog.toggle-approval`, `e→dialog.toggle-enabled`, `d→dialog.remove` (nav popup delete), `insert→dialog.add`? (nav popup add was `a` — conflicts with toggle-approval; allowlists disambiguate: nav popup supports add/remove but not toggle-*, so `a` CAN map to both… NO — one chord one command in the effective map. Resolve: `a→dialog.add`, extensions' approval toggle moves to `y→dialog.toggle-approval`? y is approve… Extensions allowlist: toggle-approval + toggle-enabled + up/down/cancel. Map `a` to BOTH add and toggle-approval is impossible. FINAL: `a→dialog.add`; extensions approval toggle = `dialog.approve` (semantic fit: approving a plugin IS approval — reuse `y→dialog.approve` for the extension manager's approval toggle; drop dialog.toggle-approval from the vocabulary), `e→dialog.toggle-enabled`, `d→dialog.remove`. Vocabulary shrinks to 14 (no toggle-approval).
4. Confirm modals (`ConfirmDelete`/`ConfirmTransfer`) allowlist: confirm+approve both → Confirmed (y and Enter both work, as today), deny+cancel → Cancelled. Collision: overwrite/skip/rename/newer/cancel (NO confirm — no Enter default, as today).
5. Legacy `y`/`n` behavior on confirm modals preserved via decision 4; collision's `n`→newer becomes `w`→newer with `n` now cancel-ish? Collision allowlist EXCLUDES deny; `n` bound to dialog.deny is simply inert in collision (hint shows only supported keys). Old muscle memory `n`=newer breaks — acceptable, hint is generated and visible; noted in CHANGELOG-worthy commit message.
6. Hint generation: `fn dialog_hints(supported: &[&str], eff: &Effective, ...) -> String` renders `[chord] label` per supported command, chords from a reverse map of `eff.bindings()` (first chord wins), labels from Fluent `dialog-cmd-*` keys (en+es, coverage-tested like help-cmd-*). Static `modal-*-keys` strings DELETED.
7. Palette: `app.palette` on `ctrl+p` (all presets; vim also `:` — check `:` availability in vim preset first, skip if taken). Rows = every `COMMANDS` entry: `name — description (chord|unbound)`, description via existing `help_id`/Fluent, chord from browse effective (viewer commands show their viewer chord). Filter = `nav::matches` over the name+description fold. Enter = run the selected command through the EXISTING dispatch (palette closes first; commands needing pane context run exactly as if key-invoked). No free-text execution; the list is closed and trusted (plugin rows arrive with P1).
8. Overlays NOT migrated in H1 (stay hardcoded, listed as explicit deferred debt in the closing commit): search dialog free-text editor, lua-trust modal (special resolve path), help overlay scroll keys (its hint is `help-hint`, already accurate; migrating adds risk for zero drift-reduction since F1 help IS the generated join already). Theme picker, extensions, nav popup DO migrate (T2).

---

### Task 1: engine — `dialog` context + preset `[dialog]` sections

**Files:** `crates/norte-frontend/src/keymap.rs`, `crates/norte-frontend/presets/keymap/{orthodox,vim,cua}.toml`

- [ ] Failing tests first (norte-frontend): `dialog_context_se_parsea_y_construye` — a preset TOML with `[dialog] keymap = [{on=["y"], run="dialog.approve"}]` builds via `Effective::build_for(_, _, &["dialog.approve"], Screen::Dialog)` and resolves `y`; `capa_puede_extender_dialog` — a layer `[dialog] prepend_keymap` rebinds and wins; `has_full_keymap_ve_dialog` — a layer with bare `[dialog] keymap` errors.
- [ ] Implement: `KeymapFile.dialog: RawSection` (serde default; keymap.rs:282-302), extend `has_full_keymap` (308), both fixed arrays (529, 538), `Screen::Dialog` variant (330; update its rustdoc — issue #24 closing) + match arm (550-553). `merge_ctx` untouched (dialog ∪ global like the others — VERIFY global merging into dialog is DESIRABLE: global has app.quit etc. — overlays intercept before global commands run… the run-loop consults the dialog Effective ONLY for overlay keys; a global binding resolving in dialog context is harmless because allowlists filter. Keep the standard union; note it).
- [ ] Preset `[dialog]` sections (identical in the three files; vim appends k/j): the decision-3 final table. Bindings reference `dialog.*` names — engine validates against known_commands passed by the caller, so presets stay frontend-agnostic (the TUI passes its DIALOG_COMMANDS; the GUI passes nothing yet and `build_for_subset` skips — VERIFY the GUI's subset build tolerates the new section: it validates all contexts? GUI builds Browse/Viewer screens only; the dialog section is parsed but only validated when building Screen::Dialog — CONFIRM by reading build_for_impl's section loop (529: iterates global/pane/viewer/dialog for WrongLayerKey checks only, command validation happens per built screen — verify and report).
- [ ] `cargo nextest run -p norte-frontend -p norte-tui` + GUI `cargo check` (out of workspace) — GUI must stay green WITHOUT changes; if its subset build chokes on dialog sections, fix build_for_subset accordingly (report).
- [ ] Commit `feat(frontend): dialog keymap context + preset [dialog] sections (H1, closes #24 groundwork)`

### Task 2: TUI — overlays resolve through the dialog keymap

**Files:** `crates/norte-tui/src/keymap.rs` (DIALOG_COMMANDS), `crates/norte-tui/src/main.rs`, `crates/norte-tui/src/app.rs`

- [ ] `DIALOG_COMMANDS: &[&str]` (the 14) in norte-tui keymap.rs next to COMMANDS. Build a third Effective (dialog) wherever browse/viewer effectives are built (startup + hot reload — grep `build_keymaps`/`Effective::build_for` call sites; same layers).
- [ ] `app.rs`: `dialog_key(modal, code)` REPLACED by `dialog_action(modal, cmd: &str) -> Option<DialogOutcome>` — pure mapping from a RESOLVED dialog command to the outcome, per-modal allowlist (decision 2/4/5 tables). The run-loop modal branch resolves the key via the dialog Effective's Resolver first, then calls `dialog_action`. Ctrl+C quit stays hardcoded BEFORE resolution (as today). TrustLuaInit path untouched.
- [ ] Same treatment for `on_theme_picker_key` (supported: up/down/confirm/cancel — F9-to-close: F9 isn't a dialog binding; keep the hardcoded F9 close as an overlay-specific extra, commented), `on_extensions_key` (up/down/cancel/approve/toggle-enabled), `on_nav_popup_key` list mode (up/down/confirm/cancel/add/remove; name-input mode stays a raw text editor — hardcoded, documented). Help overlay: NOT migrated (decision 8).
- [ ] Safety pins (tests, app.rs or tests/): `aprobacion_ignora_confirm` (ApproveAgentOp + dialog.confirm → None; Enter inert), `trust_host_ignora_confirm`, `colision_ignora_confirm_y_deny`, `confirm_acepta_confirm_y_approve`. Plus a rebind test: layer rebinds `y`→dialog.deny in [dialog] → approval modal's `y` now DENIES (data-driven proof).
- [ ] Full TUI suite green (existing modal tests adapt: they drive keys through the new resolution — check tests/ for dialog_key usages and migrate).
- [ ] Commit `feat(tui): modal/overlay keys resolve through the dialog keymap (H1)`

### Task 3: generated footer hints

**Files:** `crates/norte-tui/src/app.rs` or new `crates/norte-tui/src/hints.rs`, `crates/norte-tui/src/ui.rs`, `crates/norte-i18n/i18n/{en,es}.ftl`, `crates/norte-tui/tests/keymap.rs`

- [ ] `dialog-cmd-*` Fluent labels for the 14 commands, en+es (short: "approve", "cancel", "overwrite"…). Extend the coverage test (tests/keymap.rs:486 pattern) to force every DIALOG_COMMANDS entry has both locales.
- [ ] `fn dialog_hints(supported: &[&str], eff: &Effective) -> String`: reverse map bindings() (command → first chord, deterministic order), render `[{chord}] {label}` joined with spaces. Overlay draw sites replace `t("modal-*-keys")` with the generated string (each overlay passes its allowlist — the SAME slice used for dispatch, one source of truth: define the allowlists as consts shared by dispatch and hints). DELETE the 4 static `modal-*-keys` from both ftl files (+ any test referencing them).
- [ ] Snapshot tests (snapshots_ui.rs) re-record where footers changed — inspect each diff: chords match the preset bindings.
- [ ] Commit `feat(tui,i18n): generated dialog hints — rebinding can't desync (closes #24)`

### Task 4: command palette

**Files:** `crates/norte-tui/src/app.rs` (Palette state), `crates/norte-tui/src/ui.rs` (draw), `crates/norte-tui/src/main.rs` (dispatch + run-loop), `crates/norte-tui/src/keymap.rs` (COMMANDS + presets), `crates/norte-frontend/presets/keymap/*.toml`, `crates/norte-i18n/i18n/{en,es}.ftl`

- [ ] `"app.palette"` in COMMANDS; `ctrl+p → app.palette` in `[global]` of the three presets (vim: also `:` IF free — check vim preset; report). `help-cmd-app-palette` en+es (coverage test forces it).
- [ ] `Palette` in app.rs (mirror ExtensionManager + QuickSearch): rows built at open from `COMMANDS × (help_id Fluent description) × (browse/viewer effective chord | "—")`; `query: Vec<u8>` + filtered `visible` via `nav::matches` over folded name+description; cursor up/down; `selected() -> Option<&'static str>` (the command name).
- [ ] Run-loop branch (before help, after search dialog — order: it's an overlay like the others; FIRST in the chain after Ctrl+C? Place after `search_dialog` guard, before `help`): printable→push, backspace→pop, esc→close, up/down/pgup/pgdn→cursor, enter→take selected, close palette, then dispatch through the same `dispatch(cmd)` used by the keymap (await it exactly as a key would).
- [ ] `draw_palette` in ui.rs: `draw_nav_popup` idiom (centered, input footer masked — query is user-typed but mask anyway via query_display pattern —, filtered List, Selection role highlight, title `palette-title` Fluent). New Fluent keys `palette-title`/`palette-hint` en+es.
- [ ] Tests: `palette_filtra_y_selecciona` (unit on Palette: filter "quit" → app.quit visible+selected), `palette_enter_despacha` if the dispatch seam allows (else document manual smoke), snapshot of the open palette (default theme).
- [ ] Commit `feat(tui): command palette — app.palette with filter + bindings + descriptions (H1)`

### Task 5: gate + reviewers + close

- [ ] `just ci`; rust-reviewer on the H1 range (allowlist/hint single-source, engine context addition, palette dispatch reuse, per-commit compilability) + encoding-auditor (palette query masking, hint rendering, hostile layer keymap.toml driving dialog bindings — chord display strings). Apply findings.
- [ ] Deferred-debt note in final commit message: search dialog + lua-trust + help overlay keys stay hardcoded (decision 8); palette plugin rows await P1.
- [ ] Memory update.

## Self-review notes
- #24 closes via T2+T3 (data-driven keys + generated hints, single allowlist source). Spec H1 palette scope minus plugin rows (explicit P1 dependency, spec'd). Collision `n`→`w` UX change documented (decision 5). Engine change additive; GUI unaffected (T1 verifies). Coverage tests extended for every new Fluent surface. One-chord-one-command conflicts resolved in decision 3 (final vocabulary 14).
- Type consistency: DIALOG_COMMANDS referenced in T2/T3 defined in T2; allowlist consts shared dispatch↔hints (T3); Palette fields defined T4 where used.
