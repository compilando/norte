# Mirror, pull, swap and going back — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the two panes the five gestures every orthodox file manager has and norte lacks — send this location to the other pane, take the other pane's, exchange the two, and walk a real back/forward trail.

**Architecture:** Mirror, pull, back and forward are all ordinary navigation, so they reuse `cd`, which stops being hard-wired to the focused pane and gains a target-pane parameter plus a flag saying whether the move should be recorded on the trail or is the trail replaying itself. Swap touches no disk: it exchanges `app.panes` and every other `[T; 2]` that is indexed by pane, and travels to the run loop as a new `Cd` variant, the way `pane.refresh` already does. The back/forward stacks live inside the existing `History` so `App` gains no new array.

**Tech Stack:** Rust, ratatui, `norte-proto::VPath`, nextest, insta snapshots, Fluent (`norte-i18n`), the `norte-help` corpus and its documentation gate.

**Spec:** `docs/superpowers/specs/2026-08-05-pane-mirror-swap-design.md`

---

## File structure

| File | Responsibility |
| --- | --- |
| `crates/norte-tui/src/nav.rs` | `History` — today an MRU deque; gains the back/forward trail. Pure, no I/O; the whole of Task 1 is testable here. |
| `crates/norte-tui/src/main.rs` | `cd` → `cd_in(pane, trail)`; the dispatch arms for the five commands; the run-loop half of the swap. |
| `crates/norte-tui/src/app.rs` | `App::swap_panes` — the part of the swap that only touches `App`. |
| `crates/norte-tui/src/keymap.rs` | Five entries in the `commands!` macro. The macro makes a missing dispatch arm a compile error. |
| `crates/norte-frontend/presets/keymap/{orthodox,vim,cua}.toml` | Default chords. |
| `crates/norte-i18n/i18n/{en,es}.ftl` | `help-cmd-*` for the five, plus the two status messages. |
| `crates/norte-help/topics/{en,es}/panes.md` | The paragraph the documentation gate demands. |

## Conventions that will bite you

- Tests run with `just t norte-tui` / lint with `just c norte-tui`. **Never** bare `cargo nextest run -p …`: a different feature set builds a second ~30 GB artifact universe that is never collected. Capture the real exit code in fish: `cmd 2>&1 | tail -5; echo "EXIT=$pipestatus[1]"`.
- New comments and rustdoc in English. The surrounding comments in `main.rs`/`nav.rs` are Spanish; leave them.
- `cargo insta` is not installed. Accept a moved snapshot by reading the `.snap.new`, checking the diff, and replacing the `.snap` with it minus the `assertion_line:` header line.
- Every new command needs a Fluent `help-cmd-*` entry in BOTH locales (the i18n parity suite fails otherwise) and a mention in the help corpus (`crates/norte-tui/tests/help_gate.rs` fails the build otherwise, and its `PENDIENTES` allowlist has a `const _` ceiling that may only shrink).

---

### Task 1: `History` grows a real trail

**Files:**
- Modify: `crates/norte-tui/src/nav.rs` (the `History` struct, ~line 23, and its `mod tests`)

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `crates/norte-tui/src/nav.rs`:

```rust
    #[test]
    fn el_rastro_no_oscila_entre_dos_directorios() {
        // El defecto que este rastro existe para no tener: recorrer la MRU
        // como si fuera un rastro lleva de A a B, de vuelta a A, y de vuelta
        // a B — el lector se queda atrapado entre dos dirs sin salida.
        let mut h = History::default();
        h.record(vp("mem:///a")); // salimos de A hacia B
        h.record(vp("mem:///b")); // salimos de B hacia C (estamos en C)
        assert_eq!(h.step_back(vp("mem:///c")), Some(vp("mem:///b")));
        assert_eq!(h.step_back(vp("mem:///b")), Some(vp("mem:///a")));
        assert_eq!(h.step_back(vp("mem:///a")), None, "el rastro se acaba");
    }

    #[test]
    fn adelante_deshace_atras_y_una_navegacion_nueva_lo_borra() {
        let mut h = History::default();
        h.record(vp("mem:///a"));
        h.record(vp("mem:///b"));
        assert_eq!(h.step_back(vp("mem:///c")), Some(vp("mem:///b")));
        assert_eq!(h.step_forward(vp("mem:///b")), Some(vp("mem:///c")));
        assert_eq!(h.step_forward(vp("mem:///c")), None);

        // Volver atrás y NAVEGAR a otro sitio corta la rama de delante: es
        // la semántica del navegador, y lo contrario ofrecería un «adelante»
        // hacia una historia que el lector ya abandonó.
        assert_eq!(h.step_back(vp("mem:///c")), Some(vp("mem:///b")));
        h.record(vp("mem:///b"));
        assert_eq!(h.step_forward(vp("mem:///z")), None, "rama podada");
    }

    #[test]
    fn el_rastro_no_toca_la_mru_del_popup() {
        // Son dos preguntas distintas: «¿dónde he estado?» (la MRU que pinta
        // el popup) y «¿dónde estaba hace un momento?» (el rastro). Ir atrás
        // no es visitar un sitio nuevo.
        let mut h = History::default();
        h.record(vp("mem:///a"));
        h.record(vp("mem:///b"));
        let antes: Vec<VPath> = h.entries().iter().cloned().collect();
        let _ = h.step_back(vp("mem:///c"));
        let _ = h.step_forward(vp("mem:///b"));
        let despues: Vec<VPath> = h.entries().iter().cloned().collect();
        assert_eq!(antes, despues, "la MRU es asunto aparte");
    }

    #[test]
    fn el_rastro_esta_acotado_como_la_mru() {
        let mut h = History::default();
        for i in 0..(HISTORY_MAX + 20) {
            h.record(vp(&format!("mem:///d{i}")));
        }
        assert_eq!(h.back_len(), HISTORY_MAX, "el rastro no crece sin fin");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `just t norte-tui 2>&1 | tail -5; echo "EXIT=$pipestatus[1]"`
Expected: FAIL — `no method named record found for struct History`.

- [ ] **Step 3: Implement the trail**

In `crates/norte-tui/src/nav.rs`, extend the struct and add the methods (keep `push`, `remove` and `entries` exactly as they are — the popup depends on them):

```rust
#[derive(Debug, Default)]
pub struct History {
    /// Más reciente al frente.
    deque: VecDeque<VPath>,
    /// The trail behind the reader: where `nav.back` goes, newest last.
    ///
    /// Separate from `deque` because they answer different questions. The
    /// deque is "where has this pane been", deduplicated and most-recent
    /// first, which is what the popup lists. The trail is "where was I just
    /// now", in order, with repeats — walking the deque as if it were a trail
    /// oscillates between the two most recent directories forever.
    back: Vec<VPath>,
    /// Where `nav.forward` goes: the branch a `nav.back` stepped off, newest
    /// last. Cleared by any navigation the user initiates.
    fwd: Vec<VPath>,
}
```

and, inside `impl History`:

```rust
    /// Records a navigation the USER initiated, leaving `prev` behind.
    ///
    /// Feeds both structures at once, which is the only way they cannot
    /// disagree: `prev` joins the MRU the popup lists and the trail
    /// `nav.back` walks, and the forward branch is pruned — the reader chose
    /// a different path, so the one they had stepped off no longer exists.
    pub fn record(&mut self, prev: VPath) {
        self.push(prev.clone());
        if self.back.last() != Some(&prev) {
            self.back.push(prev);
            if self.back.len() > HISTORY_MAX {
                self.back.remove(0);
            }
        }
        self.fwd.clear();
    }

    /// One step back: the directory to navigate to, having come FROM
    /// `current`. `None` when the trail is exhausted.
    ///
    /// Deliberately does NOT feed the MRU: going back is not visiting
    /// somewhere new, and a popup that grew an entry every time the reader
    /// pressed back would stop being a list of places they went.
    pub fn step_back(&mut self, current: VPath) -> Option<VPath> {
        let target = self.back.pop()?;
        self.fwd.push(current);
        Some(target)
    }

    /// One step forward, undoing a [`Self::step_back`].
    pub fn step_forward(&mut self, current: VPath) -> Option<VPath> {
        let target = self.fwd.pop()?;
        self.back.push(current);
        Some(target)
    }

    /// How many steps back are available (tests and the status message).
    #[must_use]
    pub fn back_len(&self) -> usize {
        self.back.len()
    }

    /// How many steps forward are available.
    #[must_use]
    pub fn fwd_len(&self) -> usize {
        self.fwd.len()
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `just t norte-tui 2>&1 | tail -5; echo "EXIT=$pipestatus[1]"`
Expected: PASS, including the pre-existing `historial_push_dedup_tope_y_retirada`.

- [ ] **Step 5: Lint**

Run: `just c norte-tui 2>&1 | tail -3; echo "EXIT=$pipestatus[1]"` — expected `EXIT=0`. Then `cargo fmt --all`.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-tui/src/nav.rs
git commit -m "feat(tui): the pane history grows a real back/forward trail"
```

---

### Task 2: `cd` learns which pane it is navigating, and why

**Files:**
- Modify: `crates/norte-tui/src/main.rs` (`async fn cd`, ~line 5930, and its call sites at ~3603, ~4498, ~4951, plus the TOFU retry inside `cd` itself)

- [ ] **Step 1: Write the failing test**

There is no unit seam for `cd` (it needs a `Backend` and an `EventStream`), so this task is pinned by the tests of Tasks 4–6, which drive it end to end. What this task must not break is every existing navigation test. Before touching anything, record the baseline:

Run: `just t norte-tui 2>&1 | tail -3; echo "EXIT=$pipestatus[1]"`
Write the passing count down; it must be identical at the end of this task.

- [ ] **Step 2: Add the trail flag and the pane parameter**

Above `async fn cd` in `crates/norte-tui/src/main.rs`:

```rust
/// Whether a navigation should be RECORDED on the pane's trail, or is the
/// trail replaying itself.
///
/// Without this distinction `nav.back` feeds its own trail: stepping back
/// from B to A would record "was at B", so the next back returns to B, and
/// the reader oscillates between two directories — the exact defect the trail
/// was built to avoid, one level up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trail {
    /// The user asked for this move: it joins the MRU and the trail, and
    /// prunes the forward branch.
    Record,
    /// `nav.back`/`nav.forward` are replaying; the trail already knows.
    Replay,
}
```

Rename `cd` to `cd_in` and give it the two new parameters, keeping a thin `cd` wrapper so the existing call sites do not change meaning:

```rust
/// Navigates the FOCUSED pane, recording the move on its trail. The shape
/// every key that navigates uses (`nav.enter`, `nav.parent`, the popups).
async fn cd(app: &mut App, backend: &Backend, events: &mut EventStream, dir: VPath) -> Cd {
    cd_in(app, backend, events, app.focus(), dir, Trail::Record).await
}

/// Navigates `pane` — which need not be the focused one, because
/// `pane.mirror` sends the OTHER pane somewhere while the focus stays put.
async fn cd_in(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    pane: usize,
    dir: VPath,
    trail: Trail,
) -> Cd {
    // …body of the old `cd`…
}
```

Inside the body, replace every use of the focused pane with `pane`:
- `let prev = app.focused().dir().clone();` → `let prev = app.panes[pane].dir().clone();`
- `app.focused_mut().begin_listing(...)` → `app.panes[pane].begin_listing(...)`
- `let pane = app.focus();` → DELETE (the parameter shadows it; keeping both is how the two drift apart)
- the history push becomes trail-aware:

```rust
                        // Un cd al MISMO dir (refresh-like) no ensucia el
                        // historial; el dedup consecutivo de `record` cubre
                        // el resto de redundancias. Un `Replay` no registra
                        // nada: el rastro ya sabe dónde estuvo el lector, y
                        // grabar aquí lo haría oscilar.
                        if prev != dir && trail == Trail::Record {
                            app.history[pane].record(prev);
                        }
```

- the TOFU retry inside `cd` (`return cd(...)`) must carry the pane and the trail: `return cd_in(app, backend, events, pane, dir, trail).await;`. Read the arm before editing — the retry is what makes `y` on the trust modal resume the navigation, and sending it to the focused pane would resume it in the wrong half.

Also check `Modal::TrustHostKey` — it stores the `dir` to retry. If it does not store the pane, add it, or a mirror onto an unknown host retries into the focused pane. Grep for `TrustHostKey` and fix every construction site.

- [ ] **Step 3: Verify nothing changed**

Run: `just t norte-tui 2>&1 | tail -3; echo "EXIT=$pipestatus[1]"`
Expected: the same passing count as Step 1, `EXIT=0`. Any difference is a regression in an existing navigation path — find it before continuing.

- [ ] **Step 4: Lint and commit**

```bash
just c norte-tui
cargo fmt --all
git add crates/norte-tui/src/main.rs
git commit -m "refactor(tui): cd navigates a named pane, and knows if it is replaying a trail"
```

---

### Task 3: the five commands, their chords and their labels

**Files:**
- Modify: `crates/norte-tui/src/keymap.rs` (the `commands!` macro)
- Modify: `crates/norte-frontend/presets/keymap/{orthodox,vim,cua}.toml`
- Modify: `crates/norte-i18n/i18n/en.ftl`, `crates/norte-i18n/i18n/es.ftl`

- [ ] **Step 1: Add the vocabulary**

In `crates/norte-tui/src/keymap.rs`, inside `commands! { … }`, after `"pane.switch" => PaneSwitch,`:

```rust
    "pane.mirror" => PaneMirror,
    "pane.pull" => PanePull,
    "pane.swap" => PaneSwap,
```

and after `"nav.parent" => NavParent,`:

```rust
    "nav.back" => NavBack,
    "nav.forward" => NavForward,
```

- [ ] **Step 2: Run the build to see the macro do its job**

Run: `just c norte-tui 2>&1 | tail -12; echo "EXIT=$pipestatus[1]"`
Expected: FAIL — `non-exhaustive patterns: Command::PaneMirror, Command::PanePull, … not covered` in `dispatch`. That error IS the safety net (a command in `COMMANDS` with no arm used to be a silent no-op); Tasks 4–6 fill the arms. To keep the tree compiling meanwhile, add the five arms as `=> Cd::Cancelled,` with a `// TODO(task 4/5/6)` and delete each TODO as its task lands. Do not leave one behind: `just ci` has no TODO check, but the definition of done requires every TODO to link an issue, and these must not survive the branch.

- [ ] **Step 3: Bind the chords**

`crates/norte-frontend/presets/keymap/orthodox.toml` and `cua.toml`, in the `[pane]` section:

```toml
    { on = ["alt+i"], run = "pane.mirror" },
    { on = ["alt+u"], run = "pane.pull" },
    { on = ["ctrl+u"], run = "pane.swap" },
    { on = ["alt+left"], run = "nav.back" },
    { on = ["alt+right"], run = "nav.forward" },
```

`vim.toml` gets the same five EXCEPT `pane.swap`, which takes `alt+s` there: `ctrl+u` is already `cursor.page-up` in that preset and the vim idiom outranks the borrowed one.

```toml
    { on = ["alt+i"], run = "pane.mirror" },
    { on = ["alt+u"], run = "pane.pull" },
    { on = ["alt+s"], run = "pane.swap" },
    { on = ["alt+left"], run = "nav.back" },
    { on = ["alt+right"], run = "nav.forward" },
```

Before writing them, verify none of those chords is already bound in that preset's `[pane]` or `[global]`: `grep -n 'alt+i\|alt+u\|ctrl+u\|alt+left\|alt+right\|alt+s' crates/norte-frontend/presets/keymap/*.toml`. If one collides, STOP and report it rather than shadowing an existing binding.

- [ ] **Step 4: Add the Fluent labels**

`crates/norte-i18n/i18n/en.ftl`, with the other `help-cmd-*`:

```
help-cmd-pane-mirror = send this location to the other pane
help-cmd-pane-pull = go where the other pane is
help-cmd-pane-swap = swap the two panes
help-cmd-nav-back = back to the previous directory
help-cmd-nav-forward = forward again
```

and the two status messages, with the other `msg-*`:

```
msg-nav-no-back = no further back
msg-nav-no-forward = nothing to go forward to
msg-pane-not-a-location = search results are not a location: nothing to send
```

`crates/norte-i18n/i18n/es.ftl`:

```
help-cmd-pane-mirror = mandar esta ubicación al otro panel
help-cmd-pane-pull = ir a donde está el otro panel
help-cmd-pane-swap = intercambiar los dos paneles
help-cmd-nav-back = volver al directorio anterior
help-cmd-nav-forward = avanzar otra vez
```

```
msg-nav-no-back = no hay más atrás
msg-nav-no-forward = no hay nada hacia delante
msg-pane-not-a-location = los resultados de búsqueda no son una ubicación: no hay nada que mandar
```

- [ ] **Step 5: Verify the locales and the presets**

Run: `just t norte-tui 2>&1 | tail -5; echo "EXIT=$pipestatus[1]"` — the preset validation tests must pass (they check every `run =` against `COMMANDS`).
Run: `cargo nextest run -p norte-i18n 2>&1 | tail -3; echo "EXIT=$pipestatus[1]"` — parity must pass.
Expected failure at this point, and ONLY this one: `help_gate` complains that the five new commands are undocumented. Task 7 pays it. Do not add them to `PENDIENTES`.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-tui/src/keymap.rs crates/norte-frontend/presets/keymap crates/norte-i18n/i18n
git commit -m "feat(keymap,i18n): vocabulary and chords for mirror, pull, swap, back and forward"
```

---

### Task 4: mirror and pull

**Files:**
- Modify: `crates/norte-tui/src/main.rs` (the `dispatch` arms added as stubs in Task 3)
- Test: `crates/norte-tui/src/main.rs` (a new `mod pane_gestures_tests`, next to the existing `help_key_tests`)

- [ ] **Step 1: Write the failing tests**

The existing in-file test modules build an `App` without a backend, so test the DECISION — which pane goes where — through a small helper the dispatch arms call, exactly as `on_help_key` was extracted for the help overlay. Add to `crates/norte-tui/src/main.rs`:

```rust
#[cfg(test)]
mod pane_gestures_tests {
    use super::*;

    /// Espejo: el panel SIN foco se va a donde está el que tiene el foco, y
    /// el foco no se mueve.
    #[test]
    fn el_espejo_manda_al_otro_panel_y_no_mueve_el_foco() {
        let mut app = app_para_test();
        app.panes[0].set_dir_for_test(vp("mem:///a"));
        app.panes[1].set_dir_for_test(vp("mem:///b"));
        app.set_focus(0);
        let plan = mirror_plan(&app).expect("con dos panes normales hay plan");
        assert_eq!(plan.pane, 1, "viaja el OTRO panel");
        assert_eq!(plan.dir, vp("mem:///a"), "a donde está el del foco");
        assert_eq!(app.focus(), 0, "el foco no se ha movido");
    }

    /// Traer: el panel CON foco se va a donde está el otro.
    #[test]
    fn traer_mueve_el_panel_con_foco() {
        let mut app = app_para_test();
        app.panes[0].set_dir_for_test(vp("mem:///a"));
        app.panes[1].set_dir_for_test(vp("mem:///b"));
        app.set_focus(0);
        let plan = pull_plan(&app).expect("plan");
        assert_eq!(plan.pane, 0);
        assert_eq!(plan.dir, vp("mem:///b"));
    }

    /// Los dos ya en el mismo sitio: no-op silencioso, no un cd redundante
    /// que reordene el listado del otro panel bajo el cursor del lector.
    #[test]
    fn en_el_mismo_dir_no_hay_nada_que_hacer() {
        let mut app = app_para_test();
        app.panes[0].set_dir_for_test(vp("mem:///a"));
        app.panes[1].set_dir_for_test(vp("mem:///a"));
        app.set_focus(0);
        assert!(mirror_plan(&app).is_none());
        assert!(pull_plan(&app).is_none());
    }

    /// Desde un panel VIRTUAL de resultados no hay ubicación que mandar.
    #[test]
    fn un_panel_virtual_no_es_una_ubicacion() {
        let mut app = app_para_test();
        app.panes[0].set_dir_for_test(vp("mem:///a"));
        app.panes[1].set_dir_for_test(vp("mem:///b"));
        app.panes[0].virtual_search = true;
        app.set_focus(0);
        assert!(mirror_plan(&app).is_none(), "no hay origen que mandar");
        app.set_focus(1);
        assert!(pull_plan(&app).is_none(), "ni de donde traer");
    }
}
```

`app_para_test`, `vp` and a way to place a pane on a directory already exist in this file's other test modules — find them (`grep -n "fn app_para_test\|fn vp(" crates/norte-tui/src/main.rs`) and reuse them rather than writing new ones. If placing a pane on a directory needs a helper, add `#[cfg(test)] fn set_dir_for_test` to `norte_frontend::pane::PaneState` and say so in your report.

- [ ] **Step 2: Run to verify failure**

Run: `just t norte-tui 2>&1 | tail -5; echo "EXIT=$pipestatus[1]"`
Expected: FAIL — `cannot find function mirror_plan in this scope`.

- [ ] **Step 3: Implement the plans and the arms**

In `crates/norte-tui/src/main.rs`, above `dispatch`:

```rust
/// Where a gesture wants to send a pane. `None` from the `*_plan` functions
/// means there is nothing to do, and WHY is not this type's business — the
/// arm decides whether that deserves a message.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PaneMove {
    /// The pane that will navigate.
    pane: usize,
    /// Where it will go.
    dir: VPath,
}

/// `pane.mirror`: the unfocused pane goes where the focused one is.
///
/// `None` when the focused pane is a virtual search listing (a list of hits
/// is not a location) or when both panes are already there.
fn mirror_plan(app: &App) -> Option<PaneMove> {
    let from = app.focus();
    let to = from ^ 1;
    if app.panes[from].virtual_search {
        return None;
    }
    let dir = app.panes[from].dir().clone();
    (app.panes[to].dir() != &dir).then_some(PaneMove { pane: to, dir })
}

/// `pane.pull`: the focused pane goes where the other one is.
fn pull_plan(app: &App) -> Option<PaneMove> {
    let to = app.focus();
    let from = to ^ 1;
    if app.panes[from].virtual_search {
        return None;
    }
    let dir = app.panes[from].dir().clone();
    (app.panes[to].dir() != &dir).then_some(PaneMove { pane: to, dir })
}
```

Then replace the two stub arms in `dispatch`:

```rust
        Command::PaneMirror => match mirror_plan(app) {
            Some(m) => cd_in(app, backend, events, m.pane, m.dir, Trail::Record).await,
            None => {
                // Un panel virtual sí merece explicación; estar ya en el
                // mismo sitio, no — el lector no ha pedido nada que haya
                // fallado.
                if app.focused().virtual_search {
                    app.message = Some(t("msg-pane-not-a-location"));
                }
                Cd::Cancelled
            }
        },
        Command::PanePull => match pull_plan(app) {
            Some(m) => cd_in(app, backend, events, m.pane, m.dir, Trail::Record).await,
            None => {
                if app.panes[app.focus() ^ 1].virtual_search {
                    app.message = Some(t("msg-pane-not-a-location"));
                }
                Cd::Cancelled
            }
        },
```

- [ ] **Step 4: Run the tests**

Run: `just t norte-tui 2>&1 | tail -5; echo "EXIT=$pipestatus[1]"`
Expected: the four new tests PASS; `help_gate` still fails for the undocumented commands (Task 7).

- [ ] **Step 5: Commit**

```bash
just c norte-tui && cargo fmt --all
git add crates/norte-tui/src/main.rs
git commit -m "feat(tui): pane.mirror and pane.pull"
```

---

### Task 5: swap, and the six things indexed by pane

**Files:**
- Modify: `crates/norte-tui/src/app.rs` (`App::swap_panes`, next to `switch_focus` ~line 1580)
- Modify: `crates/norte-tui/src/main.rs` (`Cd::Swapped`, the dispatch arm, the run-loop reconciliation in `apply_cd`)

- [ ] **Step 1: Write the failing tests**

In `crates/norte-tui/src/app.rs`'s test module:

```rust
    /// El intercambio cruza el panel Y su historial, y deja el foco en el
    /// mismo LADO: quien miraba a la izquierda sigue mirando a la izquierda,
    /// y ahora ahí está lo que había a la derecha.
    #[test]
    fn el_intercambio_cruza_panel_e_historial_y_no_mueve_el_foco() {
        let mut app = app_para_test();
        app.panes[0].set_dir_for_test(vp("mem:///izq"));
        app.panes[1].set_dir_for_test(vp("mem:///der"));
        app.history[0].record(vp("mem:///rastro-izq"));
        app.history[1].record(vp("mem:///rastro-der"));
        app.set_focus(0);

        app.swap_panes();

        assert_eq!(app.panes[0].dir(), &vp("mem:///der"));
        assert_eq!(app.panes[1].dir(), &vp("mem:///izq"));
        assert_eq!(app.focus(), 0, "el foco se queda en su lado");
        // El rastro viaja con el CONTENIDO, no con el lado: si no, el popup
        // ofrecería llevar «atrás» a sitios donde ese contenido nunca estuvo.
        assert_eq!(
            app.history[0].entries().front(),
            Some(&vp("mem:///rastro-der"))
        );
        assert_eq!(
            app.history[1].entries().front(),
            Some(&vp("mem:///rastro-izq"))
        );
    }

    /// Dos intercambios son la identidad.
    #[test]
    fn dos_intercambios_dejan_todo_como_estaba() {
        let mut app = app_para_test();
        app.panes[0].set_dir_for_test(vp("mem:///izq"));
        app.panes[1].set_dir_for_test(vp("mem:///der"));
        app.swap_panes();
        app.swap_panes();
        assert_eq!(app.panes[0].dir(), &vp("mem:///izq"));
        assert_eq!(app.panes[1].dir(), &vp("mem:///der"));
    }
```

And in `crates/norte-tui/src/main.rs`'s `pane_gestures_tests`, the one that catches the real bug:

```rust
    /// El relleno EN VUELO lleva su índice de panel: si el intercambio no lo
    /// voltea, los lotes del listado siguen llegando al panel de al lado y el
    /// lector ve crecer la lista equivocada. Es el bug que una suite verde no
    /// ve, porque el listado sigue llegando: solo llega al sitio que no es.
    #[test]
    fn el_intercambio_voltea_el_indice_del_relleno_en_vuelo() {
        let mut fill = Some(fill_de_prueba(0));
        let mut decorate: [Option<DecorateFetch>; 2] = [Some(decorate_de_prueba()), None];
        let mut probed = Probed::new();
        probed.mark(0, &vp("mem:///a"));

        reconcile_swap(&mut fill, &mut decorate, &mut probed);

        assert_eq!(fill.as_ref().expect("sigue vivo").pane, 1, "índice volteado");
        assert!(decorate[1].is_some() && decorate[0].is_none(), "cruzados");
        assert!(probed.is_empty(), "la caché de stat se tira, no se traduce");
    }
```

`fill_de_prueba`, `decorate_de_prueba` and `Probed::mark`/`is_empty` may not exist. Add the smallest constructors that let the test run, `#[cfg(test)]`, next to the types they build, and say so in your report. If `Fill`'s pane field is private to a module, make it `pub(crate)` rather than adding an accessor for one test.

- [ ] **Step 2: Run to verify failure**

Run: `just t norte-tui 2>&1 | tail -5; echo "EXIT=$pipestatus[1]"`
Expected: FAIL — `no method named swap_panes`, `cannot find function reconcile_swap`.

- [ ] **Step 3: Implement the `App` half**

In `crates/norte-tui/src/app.rs`, next to `switch_focus`:

```rust
    /// Exchanges the two panes and everything `App` keeps beside them.
    ///
    /// Touches no disk: no listing is refetched, nothing can fail, and marks,
    /// filters, sort and cursor all survive because the whole pane moves
    /// rather than being rebuilt.
    ///
    /// The focus stays on the same physical side on purpose. Moving it with
    /// the content would make the command a no-op from where the reader sits.
    ///
    /// The history moves WITH the pane: it belongs to the content, not to the
    /// side of the screen. Left behind, each pane would offer to take the
    /// reader "back" to places that content has never been.
    pub fn swap_panes(&mut self) {
        self.panes.swap(0, 1);
        self.history.swap(0, 1);
    }
```

- [ ] **Step 4: Implement the run-loop half**

In `crates/norte-tui/src/main.rs`, add the `Cd` variant next to `Refreshed` (whose comment already explains this pattern: an outcome that dispatch cannot finish because it does not see `fill`/`last_probed`):

```rust
    /// `pane.swap` cruzó los panes DESDE `dispatch`, que no ve el estado
    /// indexado por panel que vive en el run loop. El desenlace viaja para
    /// que [`reconcile_swap`] lo cruce también.
    Swapped,
```

the reconciliation:

```rust
/// The other half of `pane.swap`: the per-pane state that lives in the run
/// loop rather than in `App`.
///
/// `App::swap_panes` moves the panes and their histories; these three are
/// indexed by pane too, and leaving any of them behind is a bug a green
/// suite does not catch — the listing keeps arriving, just into the wrong
/// half of the screen.
///
/// The watcher needs nothing here: the run loop re-points it from
/// `watch_targets(app)` on every iteration, so the swapped directories reach
/// it on the next tick.
fn reconcile_swap(
    fill: &mut Option<Fill>,
    decorate_fetch: &mut [Option<DecorateFetch>; 2],
    last_probed: &mut Probed,
) {
    if let Some(f) = fill.as_mut() {
        f.pane ^= 1;
    }
    decorate_fetch.swap(0, 1);
    // Es una caché de dedup de `stat`, no estado: traducir sus claves cuesta
    // más que volver a sondear, y un sondeo de más es invisible.
    last_probed.clear();
}
```

the dispatch arm:

```rust
        Command::PaneSwap => {
            app.swap_panes();
            Cd::Swapped
        }
```

and the `apply_cd` arm:

```rust
        Cd::Swapped => reconcile_swap(fill, decorate_fetch, last_probed),
```

`apply_cd` currently takes `(&mut Option<Fill>, &mut Probed, Cd)`. It now needs `decorate_fetch` too; add the parameter and update every call site (there are seven — `grep -n "apply_cd(" crates/norte-tui/src/main.rs`).

- [ ] **Step 5: Run the tests**

Run: `just t norte-tui 2>&1 | tail -5; echo "EXIT=$pipestatus[1]"`
Expected: the three new tests PASS; only `help_gate` still red.

- [ ] **Step 6: Commit**

```bash
just c norte-tui && cargo fmt --all
git add crates/norte-tui/src/app.rs crates/norte-tui/src/main.rs
git commit -m "feat(tui): pane.swap, including the state indexed by pane outside app.panes"
```

---

### Task 6: back and forward

**Files:**
- Modify: `crates/norte-tui/src/main.rs` (the two remaining stub arms, and `pane_gestures_tests`)

- [ ] **Step 1: Write the failing tests**

```rust
    /// El rastro se recorre de verdad: A→B→C, dos veces atrás llega a A. La
    /// oscilación A→B→A→B que daría recorrer la MRU es lo que este test
    /// rechaza.
    #[test]
    fn atras_recorre_el_rastro_y_adelante_lo_deshace() {
        let mut app = app_para_test();
        app.set_focus(0);
        app.panes[0].set_dir_for_test(vp("mem:///c"));
        app.history[0].record(vp("mem:///a"));
        app.history[0].record(vp("mem:///b"));

        let a_donde = back_target(&mut app).expect("hay rastro");
        assert_eq!(a_donde, vp("mem:///b"));
        app.panes[0].set_dir_for_test(a_donde);
        assert_eq!(back_target(&mut app), Some(vp("mem:///a")));
        app.panes[0].set_dir_for_test(vp("mem:///a"));
        assert_eq!(back_target(&mut app), None, "se acabó el rastro");

        assert_eq!(forward_target(&mut app), Some(vp("mem:///b")));
    }

    /// Con el rastro vacío la tecla lo DICE: una tecla que calla es
    /// indistinguible de una rota.
    #[test]
    fn atras_sin_rastro_lo_dice() {
        let mut app = app_para_test();
        app.set_focus(0);
        app.panes[0].set_dir_for_test(vp("mem:///a"));
        assert_eq!(back_target(&mut app), None);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `just t norte-tui 2>&1 | tail -5; echo "EXIT=$pipestatus[1]"`
Expected: FAIL — `cannot find function back_target`.

- [ ] **Step 3: Implement**

```rust
/// One step back for the focused pane, or `None` when the trail is empty.
///
/// Takes `&mut App` because asking IS the step: the trail hands the target
/// over and moves the current directory to the forward branch in one
/// operation, so a caller cannot peek and then forget to walk.
fn back_target(app: &mut App) -> Option<VPath> {
    let pane = app.focus();
    let current = app.panes[pane].dir().clone();
    app.history[pane].step_back(current)
}

/// One step forward, undoing a [`back_target`].
fn forward_target(app: &mut App) -> Option<VPath> {
    let pane = app.focus();
    let current = app.panes[pane].dir().clone();
    app.history[pane].step_forward(current)
}
```

and the arms:

```rust
        Command::NavBack => match back_target(app) {
            // `Trail::Replay`: el rastro se está recorriendo a sí mismo. Si
            // esto grabara, volver de B a A registraría «estuve en B» y el
            // siguiente atrás devolvería a B — la oscilación otra vez.
            Some(dir) => cd_in(app, backend, events, app.focus(), dir, Trail::Replay).await,
            None => {
                app.message = Some(t("msg-nav-no-back"));
                Cd::Cancelled
            }
        },
        Command::NavForward => match forward_target(app) {
            Some(dir) => cd_in(app, backend, events, app.focus(), dir, Trail::Replay).await,
            None => {
                app.message = Some(t("msg-nav-no-forward"));
                Cd::Cancelled
            }
        },
```

Delete the `// TODO(task 6)` markers as you go; none may survive the branch.

- [ ] **Step 4: Verify the trail is not fed by its own replay**

Add one more test that drives the property end to end at the level this file can reach:

```rust
    /// La propiedad que impide el bucle: un `Replay` no registra. Se
    /// comprueba sobre el rastro, que es donde vive la decisión.
    #[test]
    fn el_rastro_no_se_alimenta_de_si_mismo() {
        let mut app = app_para_test();
        app.set_focus(0);
        app.panes[0].set_dir_for_test(vp("mem:///c"));
        app.history[0].record(vp("mem:///b"));
        let antes = app.history[0].back_len();
        let _ = back_target(&mut app);
        assert_eq!(
            app.history[0].back_len(),
            antes - 1,
            "un paso atrás CONSUME rastro; jamás lo produce"
        );
    }
```

Run: `just t norte-tui 2>&1 | tail -5; echo "EXIT=$pipestatus[1]"` — PASS.

- [ ] **Step 5: Commit**

```bash
just c norte-tui && cargo fmt --all
git add crates/norte-tui/src/main.rs
git commit -m "feat(tui): nav.back and nav.forward walk a real trail"
```

---

### Task 7: the paragraph the gate demands

**Files:**
- Modify: `crates/norte-help/topics/en/panes.md`, `crates/norte-help/topics/es/panes.md`
- Modify: `crates/norte-tui/tests/help_gate.rs` (only if the allowlist has entries that are now stale)

- [ ] **Step 1: Run the gate to see what it wants**

Run: `just t norte-tui 2>&1 | grep -A 8 "sin documentar"; echo "EXIT=$pipestatus[1]"`
Expected: the five new commands listed as undocumented.

- [ ] **Step 2: Document them in the English topic**

`crates/norte-help/topics/en/panes.md` — add the five ids to the front matter's `commands` list, keeping the existing four:

```toml
commands = [
    "pane.switch",
    "nav.enter",
    "nav.parent",
    "nav.back",
    "nav.forward",
    "pane.mirror",
    "pane.pull",
    "pane.swap",
    "pane.refresh",
]
```

and add two paragraphs to the body (read the existing prose first and match its voice — this file explains why the other pane is the destination):

```markdown
# Moving a location across

{{cmd:pane.mirror}} sends the other pane where this one is, without moving the
focus — the fastest way to line up a copy, because the destination is whatever
the other pane holds. {{cmd:pane.pull}} is the same gesture the other way
round: this pane goes where the other one is. {{cmd:pane.swap}} exchanges the
two, which is how you reverse the direction of a copy without navigating
anything: nothing is re-read, and the marks, the filter and the cursor go with
their pane.

Mirroring onto a host you have not visited connects, and asks about its key the
same way walking there would. If the destination cannot be reached, the pane
stays where it was and the reason goes to the status bar — a shortcut may not
leave you looking at nothing.

# Going back

{{cmd:nav.back}} returns the focused pane to where it was, and
{{cmd:nav.forward}} undoes that. It is a trail, not a list: from a directory to
a second and then a third, back twice reaches the first. Navigating somewhere
new from the middle of the trail forgets the branch you stepped off, exactly as
a browser does. The list of everywhere this pane has been is a different thing
and lives behind {{cmd:pane.history}}.
```

- [ ] **Step 3: Mirror it in Spanish**

`crates/norte-help/topics/es/panes.md` — the SAME `commands` list (locale parity is structural and enforced), with prose in the register the file already uses. Do not translate word for word; write it.

- [ ] **Step 4: Shrink the allowlist if the gate says so**

If `help_gate` now reports `StaleAllowEntry` for `pane.switch`, `nav.enter`, `nav.parent`, `pane.refresh` or `pane.history` (they may already be documented), delete those lines from `PENDIENTES` and lower the `const _: () = assert!(PENDIENTES.len() <= N, …)` ceiling by the same count, in the same edit. The ceiling may only go DOWN.

- [ ] **Step 5: Verify**

Run: `just t norte-tui 2>&1 | tail -5; echo "EXIT=$pipestatus[1]"` — everything green, `help_gate` included.
Run: `cargo nextest run -p norte-help 2>&1 | tail -3; echo "EXIT=$pipestatus[1]"` — corpus integrity, locale parity, and the hazard sweep over the shipped prose.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-help/topics crates/norte-tui/tests/help_gate.rs
git commit -m "docs(help): document the five pane and navigation gestures"
```

---

### Task 8: drive it, write it down, gate it

**Files:**
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Drive the real app**

A green suite has missed composition bugs in this TUI before — three of five bugs in one past session only appeared by piloting the app. Do it:

```bash
cargo build -q -p norte-tui
tmux kill-session -t nortep 2>/dev/null
tmux new-session -d -s nortep -x 113 -y 36 -c "$PWD" "NORTE_LANG=es target/debug/norte-tui"
sleep 2
tmux send-keys -t nortep Escape   # sin modales
tmux capture-pane -p -t nortep | head -5
```

Then exercise each gesture with `tmux send-keys` and `capture-pane`, checking the screen after each: navigate the left pane somewhere deep, `Alt+i` (the right pane must follow, focus stays left), `Alt+u`, `Ctrl+U` (the two swap, focus stays on the same side), `Alt+←` several times past the start of the trail (the message must appear, and the pane must not move), `Alt+→` back. Then open `F1` and confirm the five commands appear with their chords, and `Ctrl+P` and confirm they are in the palette.

Paste the captures in your report. Any anomaly becomes a failing test first, then a fix.

- [ ] **Step 2: Write the changelog entry**

In `CHANGELOG.md`, under `## [Unreleased]` → `### Added`, above the existing first entry. Match the file's voice — prose, concrete, explaining the rule rather than listing keys:

```markdown
- **Passing a location between the panes:** `Alt+i` sends this pane's location
  to the other one without moving the focus, `Alt+u` brings the other one's
  here, and `Ctrl+U` swaps the two — the fast way to reverse the direction of
  a copy, and the only one of the three that touches no disk: nothing is
  re-read, and the marks, the filter and the cursor travel with their pane.
  Mirroring onto a host you have not visited connects and asks about its key,
  exactly as walking there would; if it cannot be reached, the pane stays
  where it was and says why.
  `Alt+←` and `Alt+→` walk a real back/forward trail, not the list of
  everywhere you have been: from one directory to a second and then a third,
  back twice reaches the first. Navigating somewhere new from the middle of
  the trail forgets the branch you stepped off, as a browser does. The list of
  everywhere a pane has been is still behind its own key.
```

- [ ] **Step 3: Full gate**

Run: `just ci 2>&1 | tail -20; echo "EXIT=$pipestatus[1]"`
Expected: `EXIT=0`. This touches no proto/vfs/core logic, so coverage should not move; if `cov` fails, read it rather than assuming it is noise.

- [ ] **Step 4: Commit**

```bash
git add CHANGELOG.md
git commit -m "docs(changelog): the pane location gestures and the navigation trail"
```

- [ ] **Step 5: Reviewers**

Dispatch `rust-reviewer` over the branch diff. The seams worth naming for it: the six pieces of per-pane state that `pane.swap` has to keep in step, the `Trail::Replay` property that keeps `nav.back` from feeding itself, and whether `cd_in`'s new parameters left any call site navigating the wrong pane. Apply BLOCKER and MAJOR findings before merging.

---

## Self-review notes

- **Spec coverage.** Vocabulary and chords: Task 3. Mirror/pull semantics and the virtual-pane refusal: Task 4. Swap and all six indexed pieces: Task 5 (`panes`+`history` in `App::swap_panes`, `fill`+`decorate_fetch`+`last_probed` in `reconcile_swap`, watcher by the run loop's per-iteration `rewatch`). Trail and its no-self-feeding property: Tasks 1, 2 and 6. Corpus and locales: Tasks 3 and 7. The failure case (pane stays put, error to the bar) is `cd`'s existing `Cd::Failed` behaviour, unchanged by Task 2 and asserted by the tmux drive in Task 8.
- **Deliberately not built:** opening the directory under the cursor in the other pane; copying marks/filter/cursor with a location; persisting the trail across sessions. All three are in the spec's out-of-scope section.
- **Known risk.** Task 2 rewrites the parameters of the function every navigation goes through. Its verification is "the existing test count does not move", which catches a regression but not a wrong-pane bug in a path no test drives. That is what the tmux drive in Task 8 and the reviewer pass are for.
