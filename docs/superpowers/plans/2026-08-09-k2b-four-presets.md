# K2b — the four presets (Total Commander, Krusader, Norton, Far)

**Spec:** `docs/superpowers/specs/2026-08-09-keymap-catalogue-and-presets-design.md`
(section "K2b — the four presets"). **Depends on:** K1 (ADR 0043, shared
catalogue + `Availability`), K2a (ADR 0044, counts + sacred keys).

Sources transcribed on **2026-08-09**, kept in the session scratchpad
`keymap-sources/`:

| file | what it is |
| --- | --- |
| `tc-11.58-KEYBOARD.TXT` | Total Commander 11.58 `KEYBOARD.TXT`, extracted from the installer (SFX zip → `INSTALL.CAB`), 141 entries |
| `krusader-keys.txt` | `docs.kde.org/trunk_kf6/en/krusader/krusader/key_bindings.html` rendered to text |
| `FarEng.hlf.m4`, `far-panelcmd.txt`, `far-funccmd.txt`, `far-misccmd.txt` | `far/FarEng.hlf.m4` from `FarGroup/FarManager`, the source Far's own help is compiled from |
| — | Norton Commander has **no** first-hand source (`NC.HLP` is internally compressed): the preset transcribes the uncontroversial core and its header says so |

## The rules every preset in this plan obeys

1. **A key is bound only when a catalogue command means what the original
   means.** Where the original's command is a norte non-concept (button bars,
   breadcrumb bars, macro recording, panel view modes, Plasma-global
   shortcuts, plugin panels), the key stays **unbound** and the preset header
   lists the omitted family. Approximating is worse than omitting: an
   approximated key teaches a lie that no `Unavailable` message corrects.
2. **`Tab` is `pane.switch`** in every one of them (sacred, spec §12, enforced
   by K2a's `SacredKey`). All four originals agree, so this costs nothing.
3. **No preset here sets `counts`.** Not even Far: Far has no numeric prefix,
   it spends `Ctrl+1..Ctrl+0` on panel view modes. `counts = true` remains
   vim-only, and the doc comment on `KeymapFile::counts` — which still says
   "vim and far" from K2a — is corrected in Task 1.
4. **No `[dialog]` section.** norte's dialogs are norte's. Task 1 adds the
   `dialog_from` key so the four inherit `orthodox`'s instead of copying 26
   bindings four times.
5. **No sequences.** None of the four originals has a multi-key chord that
   norte can honour (Far's `Ctrl+Shift+0..9` folder shortcuts are omitted with
   the rest of the unmapped families), so every `on` is one chord — which also
   keeps the prefix-free check trivially satisfied.
6. **Literal `ctrl`, never `mod+`.** These are Windows/Linux programs; a
   Total Commander user on macOS wants the Ctrl their fingers know. `mod+`
   stays for norte's own presets.
7. **Every preset binds `viewer.close` and the six viewer movers**, so F3 is
   never a room with no door.
8. **Header block, mandatory**, in this shape:

   ```toml
   # <Program> <version> — transcribed 2026-08-09 from <source>.
   # Divergences from the original: <list, or "none">.
   # Omitted families (norte has no concept): <list>.
   ```

## Task 1 — the engine bits: `dialog_from`, the Planned catalogue, the reasons

**Files:** `crates/norte-frontend/src/keymap/{layer,catalogue}.rs`,
`crates/norte-frontend/src/keymap/mod.rs` (re-exports/tests),
`i18n/en/*.ftl`, `i18n/es/*.ftl`, `docs/adr/`.

### 1a. `dialog_from` — a preset inherits norte's dialog context

`KeymapFile` gains one optional key:

```rust
/// The preset whose `[dialog]` section this one adopts. norte's dialogs are
/// norte's, not the imitated program's: a Total Commander user expects TC's
/// panel keys, not a TC confirmation dialog that TC never had. Only legal in
/// a PRESET (a user layer that set it would silently redefine every overlay
/// key — the same reason `counts` is refused there), only one level deep, and
/// only naming a bundled preset.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub(super) dialog_from: Option<String>,
```

- Resolved inside `parse_keymap`, after the TOML parses: look the name up in
  `presets::source`, parse THAT (a source with its own `dialog_from` is an
  error — one level, no chains), and move its `dialog` section in.
- A preset that declares both `dialog_from` and a non-empty `[dialog]` is a
  load error: two answers to one question.
- Unknown name → load error naming the key and the value.
- `check_layer_keys` refuses `dialog_from` in a user/project layer with the
  existing `KeymapError::WrongLayerKey { layer: "usuario", key: "dialog_from" }`.

New error variants go on `KeymapError` next to `WrongLayerKey`; keep the
existing `Display` style.

**Tests (in `layer.rs`):** inheritance actually populates `[dialog]`; both-keys
is an error; unknown name is an error; a chain is an error; a layer with the
key is `WrongLayerKey`; `orthodox` itself (no key) is unchanged.

`KeymapFile` derives `schemars::JsonSchema` under the `schema` feature — check
whether `norte-config`'s published JSON Schema has a golden that needs
regenerating, and regenerate it in this task if so.

### 1b. The Planned entries

Add to `CATALOGUE`, each with the reason id and issue below. `planned()`
already forces `counts: false`, which is right for all of them.

| command(s) | reason id | issue |
| --- | --- | --- |
| `pane.select-drive-left`, `pane.select-drive-right` (join the existing `pane.select-drive`) | `keymap-reason-volume-enumeration` | 131 |
| `pane.pack`, `pane.unpack`, `pane.test-archive`, `pane.split-file`, `pane.combine-files` | `keymap-reason-archive-write` | 132 |
| `pane.edit`, `pane.edit-new` | `keymap-reason-editor` | 133 |
| `pane.compare-dirs`, `pane.sync-dirs` | `keymap-reason-compare-sync` | 134 |
| `app.terminal`, `app.toggle-panels`, `pane.command-line` | `keymap-reason-shell` | 135 |
| `pane.tree` | `keymap-reason-tree` | 136 |
| `pane.tab-new`, `pane.tab-close`, `pane.tab-next`, `pane.tab-prev` | `keymap-reason-tabs` | 137 |
| `pane.sort-name`, `pane.sort-ext`, `pane.sort-size`, `pane.sort-time`, `pane.sort-menu` | `keymap-reason-sort` | 138 |
| `pane.properties`, `pane.dir-size` | `keymap-reason-properties` | 139 |
| `pane.connect`, `pane.disconnect` | `keymap-reason-connections` | 140 |

Nine new Fluent ids, EN and ES, short noun phrases in the register the existing
`keymap-reason-volume-enumeration` uses (it is the model — copy its tone, and
put the new ones beside it in the same file). The catalogue's
`todo_motivo_planned_esta_traducido_en_ambos_locales` test already fails
without them.

The three catalogue tests that pin shape (`no_hay_nombres_duplicados`,
`todo_planned_tiene_motivo_e_issue`, `el_conjunto_con_contador_es_exactamente_este`)
must stay green untouched — in particular no Planned command gets `counts`.

### 1c. ADR

`docs/adr/` — one short MADR (next free number, use the `/adr` skill) for
`dialog_from`: it is a change to the keymap FILE FORMAT, which is user-visible
config surface and therefore ADR-worthy; K1's ADR 0043 and K2a's 0044 are the
neighbours. Record the rejected alternatives: copying the block into each
preset (drift across seven files, and a new `dialog.*` command becomes a
seven-file edit), and a general `inherit` of all sections (a TC preset that
silently inherited orthodox's pane keys would bind keys TC never had, which is
exactly the fidelity failure the whole feature exists to avoid).

**Verify:** `just t norte-frontend` and `just c`.

## Task 2 — `total-commander.toml` and `krusader.toml`

**Files:** two new files in `crates/norte-frontend/presets/keymap/`,
`crates/norte-frontend/src/keymap/presets.rs`.

Read the header rules above and the sources named at the top. Then:

- Write the two presets. Bind every key of the source that has a catalogue
  command, Live or Planned, in `[global]`/`[pane]`/`[viewer]`; add
  `dialog_from = "orthodox"`; omit the rest and say so in the header.
- Known mappings that must not be got wrong, because they are the ones a user
  notices first: TC `F3` view / `F4` edit (`pane.edit`, Planned) / `F5` copy /
  `F6` move / `F7` mkdir / `F8` delete / `Alt+F1`,`Alt+F2` drives /
  `Alt+F7` find / `Alt+F5`,`Alt+F6` pack-unpack / `Ctrl+R` reread /
  `Alt+Enter` properties / `Ctrl+U` swap panels. Krusader `F2` **rename**
  (not TC's F6 — this is Krusader's one famous divergence), `F9` terminal,
  `F10` quit, `Ctrl+U` swap, `Ctrl+D` bookmarks (`pane.hotlist`), `Ctrl+F`
  quicksearch, `Ctrl+S` search, `Ctrl+H` history, `Alt++`/`Alt+-`/`Alt+*`
  select-all/unselect-all/invert, `Alt+.` hidden files.
- Register both in `presets.rs`: `NAMES`, `source()`, a `pub const` each, and
  bump `names_tiene_los_tres_presets_de_fabrica` (rename it — it is now the
  size pin for a bigger catalogue).
- Replace the hardcoded `["orthodox", "vim", "cua"]` arrays in
  `crates/norte-gui/src/keymap.rs` (four sites), `crates/norte-gui/src/main.rs`
  (one site) and `crates/norte-tui/src/keymap.rs::presets()` with
  `norte_frontend::keymap::presets::NAMES` / `source()`, so a new preset is
  covered by the existing per-preset loops without touching them again. The
  TUI's `presets()` keeps its panic-on-invalid-embedded contract.
- The GUI's `build_effectives_preset_only` panics on a preset that fails to
  build, and only `just gui-ci` compiles it (`norte-gui` is out of the
  workspace): run `just gui-ci` for these two presets in this task.

**Verify:** `just t norte-frontend`, `just t norte-tui`, `just gui-ci`.

## Task 3 — `far.toml` and `norton.toml`

Same shape as Task 2, same registration steps (the `NAMES` size pin moves
again), for:

- **Far**: `far-funccmd.txt` (F1–F12 × modifiers) and `far-panelcmd.txt`
  (panel control). Far is the hard case and the reason it is in the plan:
  `Ctrl+F3..Ctrl+F11` sorting (`pane.sort-*`, Planned), `Ctrl+T` tree,
  `Ctrl+L` info panel (**omit** — no catalogue concept), `Ctrl+Q` quick view
  (**omit**), `Ctrl+O` hide panels (`app.toggle-panels`, Planned),
  `Ctrl+1..Ctrl+0` view modes (**omit**, documented family),
  `Shift+F1/F2/F3` archive commands, `Alt+F1/F2` drives, `Alt+F7` find,
  `Alt+F8` command history, `Alt+F12` folder history, `Shift+Del`/`Alt+Del`
  delete and wipe, gray `+`/`-`/`*` selection, `Ctrl+M` restore selection
  (**omit**), `Alt+Shift+Ins` copy full names (`pane.copy-path`).
  **No `counts`** (rule 3).
- **Norton**: `F1`–`F10`, `Ctrl+O`, `Ctrl+U`, `Alt+F1`/`Alt+F2`, `Insert`,
  gray `+`/`-`/`*`, `Ctrl+\` (root), `Ctrl+PgUp`/`Ctrl+PgDn`, `Ctrl+R`,
  `Ctrl+Q`(omit), `Ctrl+L`(omit). The header states in one sentence that there
  is no first-hand source, that this is the uncontroversial core, and what a
  reader should do if they know better (open an issue).

**Verify:** `just t norte-frontend`, `just t norte-tui`, `just gui-ci`.

## Task 4 — the gate keeps the presets honest

**Files:** `crates/norte-frontend/src/keymap/presets.rs` (tests),
`crates/norte-tui/src/keymap.rs` or its test module, spec, memory.

One test module, over `presets::NAMES` — every check runs for every bundled
preset, so preset number six inherits all of them for free:

1. Parses, and builds for `Screen::{Browse, Viewer, Dialog}` against the TUI's
   command set **and** the GUI's — the K1 lesson: a preset that builds in one
   frontend and not the other is the bug the shared catalogue exists to kill.
2. Binds `tab` to `pane.switch` and to nothing else (sacred key, and the four
   new ones are the first presets where getting this wrong was possible).
3. Binds `viewer.close` in `[viewer]`, and the six movers.
4. Has a non-empty `[dialog]` context after resolution — the `dialog_from`
   inheritance actually landed.
5. Every `run` name is in the shared catalogue (this is `build_for`'s
   `UnknownCommand`, asserted directly so the failure names the preset).
6. Only `vim` sets `counts` (rule 3, pinned by name so a future preset has to
   argue for it in review).
7. Every preset file's first line is a header comment naming a source and the
   transcription date — the staleness rule from the spec, enforced rather than
   hoped for.

Then: update the spec's K2b section to "built" with the real numbers, correct
its "vim and far" sentence, and add the memory note.

**Verify:** `just ci-fast` once for the batch, `just ci` at the close.

## Reviewers

Dispatched by the agent doing the work, before committing:

- Task 1: `rust-reviewer` (parser change, new error variants, one-level
  recursion in `parse_keymap`).
- Tasks 2 and 3: no Rust logic — `encoding-auditor` on the chord strings is
  not warranted, but a reviewer pass on **fidelity** is: give `rust-reviewer`
  the diff plus the source file and ask it to spot-check ten bindings against
  the source and to look for a key bound to a command that does not mean the
  same thing.
- Task 4: `test-engineer` on the pinned set.

## Out of scope

Double Commander (a near-duplicate of TC, cheap follow-up), importing
`wincmd.ini`, panel view modes / info panel / quick view as commands, and
everything K3 (which-key overlay, reference sheet, shortcut editor).
