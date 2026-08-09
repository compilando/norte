# K2a — numeric counts, the sacred keys, and K1's two debts: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the keymap engine the two mechanisms the four presets of K2b
need — a numeric count prefix and a prohibition on rebinding `Tab` — and settle
the two debts K1's reviewers left, so the presets are written against an engine
that has stopped moving.

**Architecture:** A count is accumulated by the resolver and returned with the
command; **the frontend repeats the dispatch**, so none of the ~80 commands
changes signature and a command cannot forget to read a count. Both new load
rules (a digit key bound while counts are on, `Tab` bound to anything but
`pane.switch`) are load errors, the same way the prefix-free rule is: conflicts
surface when the file loads, not when a finger slips.

**Tech Stack:** Rust, `norte-frontend` (pure), `norte-tui`, `norte-gui`, Fluent
via `norte-i18n`, nextest.

**Spec:** `docs/superpowers/specs/2026-08-09-keymap-catalogue-and-presets-design.md`, section "K2a — the engine"
**Predecessor:** K1, `docs/superpowers/plans/2026-08-09-k1-keymap-catalogue.md`, merged as `9b750c0..9b37cb0`

---

## Gate budget

Per CLAUDE.md the gate is billed per PLAN. On K1 this budget held: 3 full-gate
runs against the 50 of the session before it.

- **Every task:** `just t norte-frontend`, `just t norte-tui`, `just c`,
  `just gui-ci`, and `cargo test --doc -p <crate>` for doctests.
- **After Task 3:** one `just ci-fast`.
- **After Task 5:** one `just ci`, then one `just gui-ci`.
- A red gate is never re-run to check a fix. Reproduce with `just t <crate>`,
  `cargo test --doc -p <crate>` for a doctest, `just docs` for rustdoc.

`norte-gui` is excluded from `just ci` (GPUI turns on
`serde_json/preserve_order`, which would contaminate the core's goldens), so
GUI work is verified with `just gui-ci`.

---

## File structure

| file | change |
| --- | --- |
| `crates/norte-frontend/src/keymap/resolve.rs` | `Count`, the count accumulator, `Resolution::{Run{command,count}, Counting}` |
| `crates/norte-frontend/src/keymap/layer.rs` | `KeymapFile.counts`, and the layer rule that forbids it outside a preset |
| `crates/norte-frontend/src/keymap/effective.rs` | `Effective.counts`, the digit rule, the sacred-key rule |
| `crates/norte-frontend/src/keymap/mod.rs` | `KeymapError::{DigitBoundWithCounts, SacredKey}`, re-exports, tests |
| `crates/norte-i18n/i18n/{en,es}.ftl` | the "count ignored" message |
| `crates/norte-tui/src/main.rs` | repeat the dispatch, paint the count, say when it was ignored |
| `crates/norte-gui/src/main.rs` | the same, on its two sites |
| `crates/norte-gui/src/keymap.rs` | `build_effectives_with` → `Result`; `means_command` without the round trip |
| `crates/norte-frontend/presets/keymap/vim.toml` | `counts = true` |

---

### Task 1: The count reaches the command

**Files:**
- Modify: `crates/norte-frontend/src/keymap/resolve.rs`
- Modify: `crates/norte-frontend/src/keymap/layer.rs`
- Modify: `crates/norte-frontend/src/keymap/effective.rs`
- Modify: `crates/norte-frontend/src/keymap/mod.rs`

- [ ] **Step 1: Write the failing tests**

In `crates/norte-frontend/src/keymap/mod.rs`'s `mod tests`. Note every fixture
sets `counts = true` at the top of the TOML — the flag is per preset:

```rust
/// `5j` runs the command five times. The count rides WITH the command; the
/// frontend is what repeats, so no command signature changes.
#[test]
fn un_contador_llega_con_el_comando() {
    let preset = parse_keymap(
        r#"
counts = true

[pane]
keymap = [ { on = ["j"], run = "cursor.down" } ]
"#,
    )
    .unwrap();
    let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap();
    let mut r = Resolver::new(eff);
    assert_eq!(r.push(parse_chord("5").unwrap()), Resolution::Counting(5));
    assert_eq!(
        r.push(parse_chord("j").unwrap()),
        Resolution::Run {
            command: "cursor.down".to_owned(),
            count: Count::Repeat(5),
        }
    );
}

/// Digits accumulate left to right, and the count survives a multi-key
/// sequence: `12gg` is twelve, not one then two.
#[test]
fn los_digitos_se_acumulan_y_sobreviven_a_una_secuencia() {
    let preset = parse_keymap(
        r#"
counts = true

[pane]
keymap = [ { on = ["g", "g"], run = "cursor.top" } ]
"#,
    )
    .unwrap();
    let eff = Effective::build_for(&preset, &[], &["cursor.top"], Screen::Browse).unwrap();
    let mut r = Resolver::new(eff);
    assert_eq!(r.push(parse_chord("1").unwrap()), Resolution::Counting(1));
    assert_eq!(r.push(parse_chord("2").unwrap()), Resolution::Counting(12));
    assert_eq!(r.push(parse_chord("g").unwrap()), Resolution::Pending(1));
    assert_eq!(
        r.push(parse_chord("g").unwrap()),
        Resolution::Run {
            command: "cursor.top".to_owned(),
            count: Count::Repeat(12),
        }
    );
}

/// A count over a command the catalogue says takes none is NOT swallowed: the
/// command runs once and the frontend is told to say the count was ignored.
#[test]
fn un_contador_sobre_un_comando_sin_contador_no_se_traga() {
    let preset = parse_keymap(
        r#"
counts = true

[global]
keymap = [ { on = ["q"], run = "app.quit" } ]
"#,
    )
    .unwrap();
    let eff = Effective::build_for(&preset, &[], &["app.quit"], Screen::Browse).unwrap();
    let mut r = Resolver::new(eff);
    r.push(parse_chord("3").unwrap());
    assert_eq!(
        r.push(parse_chord("q").unwrap()),
        Resolution::Run {
            command: "app.quit".to_owned(),
            count: Count::Ignored(3),
        }
    );
}

/// Zero never STARTS a count — `0` stays a bindable key, which is what vim's
/// "go to the first column" and mc's mask keys rely on. It does accumulate
/// once a count is open: `10` is ten.
#[test]
fn el_cero_no_abre_un_contador_pero_si_acumula() {
    let preset = parse_keymap(
        r#"
counts = true

[pane]
keymap = [
    { on = ["0"], run = "cursor.top" },
    { on = ["j"], run = "cursor.down" },
]
"#,
    )
    .unwrap();
    let known = ["cursor.top", "cursor.down"];
    let eff = Effective::build_for(&preset, &[], &known, Screen::Browse).unwrap();
    let mut r = Resolver::new(eff);
    // A bare 0 is the binding, not a count.
    assert_eq!(
        r.push(parse_chord("0").unwrap()),
        Resolution::Run { command: "cursor.top".to_owned(), count: Count::None }
    );
    // But 1 then 0 is ten.
    assert_eq!(r.push(parse_chord("1").unwrap()), Resolution::Counting(1));
    assert_eq!(r.push(parse_chord("0").unwrap()), Resolution::Counting(10));
    assert_eq!(
        r.push(parse_chord("j").unwrap()),
        Resolution::Run { command: "cursor.down".to_owned(), count: Count::Repeat(10) }
    );
}

/// Four digits is the ceiling. A fifth is dropped rather than wrapping the
/// accumulator — 99999 must not silently become something else.
#[test]
fn el_contador_topa_en_cuatro_digitos() {
    let preset = parse_keymap(
        r#"
counts = true

[pane]
keymap = [ { on = ["j"], run = "cursor.down" } ]
"#,
    )
    .unwrap();
    let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap();
    let mut r = Resolver::new(eff);
    for _ in 0..5 {
        r.push(parse_chord("9").unwrap());
    }
    assert_eq!(
        r.push(parse_chord("j").unwrap()),
        Resolution::Run { command: "cursor.down".to_owned(), count: Count::Repeat(9999) }
    );
}

/// Esc clears the count as well as the pending sequence. A count left stuck
/// to the next keystroke is the worst failure this feature can have.
#[test]
fn esc_limpia_el_contador() {
    let preset = parse_keymap(
        r#"
counts = true

[pane]
keymap = [ { on = ["j"], run = "cursor.down" } ]
"#,
    )
    .unwrap();
    let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap();
    let mut r = Resolver::new(eff);
    r.push(parse_chord("7").unwrap());
    assert_eq!(r.push(parse_chord("esc").unwrap()), Resolution::Reset);
    assert_eq!(
        r.push(parse_chord("j").unwrap()),
        Resolution::Run { command: "cursor.down".to_owned(), count: Count::None }
    );
}

/// An unbound key clears the count too — otherwise a typo leaves a number
/// glued to whatever you press next.
#[test]
fn una_tecla_sin_binding_limpia_el_contador() {
    let preset = parse_keymap(
        r#"
counts = true

[pane]
keymap = [ { on = ["j"], run = "cursor.down" } ]
"#,
    )
    .unwrap();
    let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap();
    let mut r = Resolver::new(eff);
    r.push(parse_chord("4").unwrap());
    assert_eq!(r.push(parse_chord("z").unwrap()), Resolution::Reset);
    assert_eq!(
        r.push(parse_chord("j").unwrap()),
        Resolution::Run { command: "cursor.down".to_owned(), count: Count::None }
    );
}

/// Without the preset flag a digit is just a key: `orthodox` and `cua` must
/// not grow counts behind their users' backs.
#[test]
fn sin_el_flag_del_preset_un_digito_es_una_tecla() {
    let preset = parse_keymap(
        r#"
[pane]
keymap = [ { on = ["5"], run = "cursor.down" } ]
"#,
    )
    .unwrap();
    let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap();
    let mut r = Resolver::new(eff);
    assert_eq!(
        r.push(parse_chord("5").unwrap()),
        Resolution::Run { command: "cursor.down".to_owned(), count: Count::None }
    );
}
```

- [ ] **Step 2: Run them to verify they fail**

```bash
just t norte-frontend 2>&1 | tail -20
```

Expected: FAIL — `cannot find Count`, `no variant Counting`, `Run` is a tuple
variant, `unknown field counts` from the TOML.

- [ ] **Step 3: The preset flag**

In `crates/norte-frontend/src/keymap/layer.rs`, add to `KeymapFile`:

```rust
    /// Whether a numeric prefix multiplies the next command (`5j`). Opt-in per
    /// PRESET: `vim` and `far` set it because their originals have counts;
    /// `orthodox`, `cua`, Total Commander, Krusader and Norton do not, and
    /// turning it on there would steal their digit keys.
    #[serde(default)]
    pub(super) counts: bool,
```

`KeymapFile` is `#[serde(deny_unknown_fields)]`, so this is what makes
`counts = true` parse at all.

A user layer must not set it — the count policy is the preset's, and a layer
silently turning counts on would change what every digit key means. Extend
`check_layer_keys`:

```rust
    for layer in layers {
        if layer.counts {
            return Err(KeymapError::WrongLayerKey {
                layer: "usuario",
                key: "counts",
            });
        }
        // … the existing per-section checks …
    }
```

- [ ] **Step 4: Carry the flag into the effective map**

In `crates/norte-frontend/src/keymap/effective.rs`, add `counts: bool` to
`Effective`, set it from `preset.counts` in `build_for_impl`, and expose it:

```rust
    /// Whether this effective keymap's preset enables numeric counts.
    #[must_use]
    pub fn counts(&self) -> bool {
        self.counts
    }
```

`build_diagnostics` builds no `Effective`, so it needs no change here.

- [ ] **Step 5: The `Count` type and the accumulator**

In `crates/norte-frontend/src/keymap/resolve.rs`:

```rust
/// The ceiling on a typed count, in digits. `9999` repetitions of a cursor
/// move on a listing of any realistic size lands on the last row; a fifth
/// digit is dropped rather than wrapping the accumulator into a number the
/// user did not type.
const MAX_COUNT_DIGITS: u32 = 4;
const MAX_COUNT: u32 = 9_999;

/// What a typed count did to the command it landed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Count {
    /// No count was typed.
    None,
    /// Run the command this many times.
    Repeat(u32),
    /// A count was typed and this command does not take one (the catalogue's
    /// `counts` field says so). Run it ONCE and tell the user the count was
    /// ignored — never swallow it.
    Ignored(u32),
}

/// The digit a bare chord spells, if it spells one. A digit with any modifier
/// is an ordinary chord: `ctrl+5` was never a count.
fn digit_of(chord: Chord) -> Option<u32> {
    match chord.parts() {
        (mods, KeyCode::Char(c)) if mods == Mods::default() => c.to_digit(10),
        _ => None,
    }
}
```

`Chord`'s fields are private. Add the accessor it needs in `chord.rs`:

```rust
    /// The chord's parts, for the few callers that must inspect it (the count
    /// accumulator asks "is this a bare digit?").
    #[must_use]
    pub fn parts(self) -> (Mods, KeyCode) {
        (self.mods, self.code)
    }
```

Add the two fields to `Resolver` and set them in `new`:

```rust
pub struct Resolver {
    eff: Effective,
    pending: Vec<Chord>,
    /// The count typed so far, if the preset enables counts.
    count: Option<u32>,
}
```

```rust
    pub fn new(eff: Effective) -> Self {
        Self { eff, pending: Vec::new(), count: None }
    }
```

Add the accessor the status bar needs:

```rust
    /// The count typed so far (for the status bar). `None` when no digit is
    /// in flight.
    #[must_use]
    pub fn count(&self) -> Option<u32> {
        self.count
    }
```

- [ ] **Step 6: Rewrite `push`**

```rust
    /// Push a key. With a sequence in flight, `Esc` always cancels (it never
    /// runs a binding); with nothing in flight, `Esc` is an ordinary key.
    /// A bare digit accumulates into the count when the preset enables counts
    /// and no sequence is in flight — mid-sequence, a digit is just a key.
    pub fn push(&mut self, chord: Chord) -> Resolution {
        if chord.is_bare_esc() && (!self.pending.is_empty() || self.count.is_some()) {
            self.pending.clear();
            self.count = None;
            return Resolution::Reset;
        }
        if self.eff.counts() && self.pending.is_empty() {
            if let Some(d) = digit_of(chord) {
                // Zero never OPENS a count: `0` stays bindable, which is what
                // vim's first-column key relies on. It accumulates fine once
                // a count is open, so `10` is ten.
                if self.count.is_some() || d != 0 {
                    let acc = self.count.unwrap_or(0);
                    let next = if acc > MAX_COUNT / 10 { acc } else { acc * 10 + d };
                    let next = next.min(MAX_COUNT);
                    self.count = Some(next);
                    return Resolution::Counting(next);
                }
            }
        }
        self.pending.push(chord);
        match self.eff.lookup(&self.pending) {
            Lookup::Exact(run, Availability::Here) => {
                self.pending.clear();
                let count = match self.count.take() {
                    None => Count::None,
                    // The catalogue is the authority on who takes a count. A
                    // `lua:` command is not in it, so a count over one is
                    // Ignored — honest: we cannot know what it would mean.
                    Some(n) if super::catalogue::lookup(run).is_some_and(|d| d.counts) => {
                        Count::Repeat(n)
                    }
                    Some(n) => Count::Ignored(n),
                };
                Resolution::Run { command: run.to_owned(), count }
            }
            Lookup::Exact(run, why) => {
                self.pending.clear();
                self.count = None;
                Resolution::Unavailable { command: run.to_owned(), why }
            }
            Lookup::Prefix => Resolution::Pending(self.pending.len()),
            Lookup::Miss => {
                self.pending.clear();
                self.count = None;
                Resolution::Reset
            }
        }
    }
```

`MAX_COUNT_DIGITS` is documentation for the ceiling; if clippy calls it dead,
fold it into `MAX_COUNT`'s doc comment rather than keeping an unused const.

- [ ] **Step 7: The two `Resolution` changes**

```rust
    /// Complete sequence: run this command, `count` times.
    Run {
        /// The command to run.
        command: String,
        /// What the typed count, if any, means for it.
        count: Count,
    },
    /// A count is being typed (current value). The status bar shows it.
    Counting(u32),
```

Re-export `Count` from `mod.rs` next to `Resolution`.

- [ ] **Step 8: Run the tests**

```bash
just t norte-frontend 2>&1 | tail -20
```

Expected: PASS. Every `Resolution::Run(x)` pattern in the workspace now fails
to compile; Task 3 fixes the frontends. To keep this task's tests runnable,
fix any *inside* `norte-frontend` now and leave the frontends to Task 3.

- [ ] **Step 9: Commit**

```bash
cargo fmt --all
git add crates/norte-frontend
git commit -m "feat(frontend): a numeric prefix rides with the command

Resolution::Run carries a Count, and the frontend is what repeats — so
none of the ~80 commands changes signature and none can forget to read a
count. Ignored(n) is the third state: a count over a command that takes
none runs it once and says so, rather than swallowing the number.

Opt-in per preset. Zero never opens a count, four digits is the ceiling,
and Esc clears it."
```

---

### Task 2: The two load-time rules

**Files:**
- Modify: `crates/norte-frontend/src/keymap/mod.rs` (`KeymapError` + tests)
- Modify: `crates/norte-frontend/src/keymap/effective.rs` (the checks)

- [ ] **Step 1: Write the failing tests**

```rust
/// With counts on, a digit key bound in the same context is a LOAD error, not
/// silent precedence. Same spirit as prefix-free: the conflict surfaces when
/// the file loads, not when a finger slips.
#[test]
fn un_digito_ligado_con_contadores_es_error_de_carga() {
    let preset = parse_keymap(
        r#"
counts = true

[pane]
keymap = [ { on = ["5"], run = "cursor.down" } ]
"#,
    )
    .unwrap();
    let e = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap_err();
    assert!(matches!(e, KeymapError::DigitBoundWithCounts { .. }), "{e:?}");
}

/// `0` is exempt, because a count never starts with zero — binding it stays
/// legal even with counts on.
#[test]
fn el_cero_sigue_siendo_ligable_con_contadores() {
    let preset = parse_keymap(
        r#"
counts = true

[pane]
keymap = [ { on = ["0"], run = "cursor.top" } ]
"#,
    )
    .unwrap();
    Effective::build_for(&preset, &[], &["cursor.top"], Screen::Browse)
        .expect("0 con contadores es legal");
}

/// Specification §12: Tab switches panes and a preset may not take it. K2b
/// imports four foreign keymaps, which is when this stops being theoretical.
#[test]
fn un_preset_no_puede_repinar_tab() {
    let preset = parse_keymap(
        r#"
[pane]
keymap = [ { on = ["tab"], run = "cursor.down" } ]
"#,
    )
    .unwrap();
    let e = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap_err();
    assert!(matches!(e, KeymapError::SacredKey { .. }), "{e:?}");
}

/// The prohibition is on the BROWSE screen only. Every bundled preset binds
/// `tab` to `dialog.pane` inside `[dialog]`, and that is not pane switching —
/// blanket-banning the key would break the dialogs we already ship.
#[test]
fn tab_sigue_siendo_libre_en_el_contexto_de_dialogo() {
    let preset = parse_keymap(
        r#"
[dialog]
keymap = [ { on = ["tab"], run = "dialog.pane" } ]
"#,
    )
    .unwrap();
    Effective::build_for(&preset, &[], &["dialog.pane"], Screen::Dialog)
        .expect("tab en dialog es legal");
}

/// A user layer cannot take Tab either — the rule is about the effective map,
/// not about who wrote the line.
#[test]
fn una_capa_de_usuario_tampoco_puede_tomar_tab() {
    let preset = parse_keymap(
        r#"
[pane]
keymap = [ { on = ["tab"], run = "pane.switch" } ]
"#,
    )
    .unwrap();
    let layer = parse_keymap(
        r#"
[pane]
prepend_keymap = [ { on = ["tab"], run = "cursor.down" } ]
"#,
    )
    .unwrap();
    let known = ["pane.switch", "cursor.down"];
    let e = Effective::build_for(&preset, &[layer], &known, Screen::Browse).unwrap_err();
    assert!(matches!(e, KeymapError::SacredKey { .. }), "{e:?}");
}

/// The three bundled presets must survive both rules unchanged.
#[test]
fn los_presets_de_fabrica_pasan_las_dos_reglas_nuevas() {
    for name in presets::NAMES {
        let src = presets::source(name).expect("NAMES resuelve");
        let kf = parse_keymap(src).expect("preset parsea");
        for screen in [Screen::Browse, Screen::Viewer, Screen::Dialog] {
            let known = preset_commands(screen);
            let known: Vec<&str> = known.iter().map(String::as_str).collect();
            Effective::build_for(&kf, &[], &known, screen)
                .unwrap_or_else(|e| panic!("{name} en {screen:?}: {e:?}"));
        }
    }
}
```

- [ ] **Step 2: Run them to verify they fail**

```bash
just t norte-frontend 2>&1 | tail -20
```

Expected: FAIL — the two error variants do not exist and the two illegal
presets currently build fine.

- [ ] **Step 3: Add the two error variants**

In `KeymapError` in `crates/norte-frontend/src/keymap/mod.rs`, following the
shape of the existing variants (they carry owned diagnostic strings):

```rust
    /// A digit key is bound in a context whose preset enables numeric counts.
    /// Both cannot be true at once, and choosing silently for the user is how
    /// a keymap becomes unpredictable.
    DigitBoundWithCounts {
        /// The offending chord, as spelled.
        chord: String,
        /// What it is bound to.
        run: String,
    },
    /// A binding takes a key the specification reserves (§12: `Tab` switches
    /// panes). A preset that imitates another program documents the
    /// difference; it does not take the key.
    SacredKey {
        /// The reserved chord, as spelled.
        chord: String,
        /// The command it is reserved for.
        reserved_for: &'static str,
        /// What the offending binding tried to run instead.
        run: String,
    },
```

Give each a `Display` arm matching the file's existing style, in the same
language the neighbouring arms use.

- [ ] **Step 4: Enforce them in `build_for_impl`**

After the dedup and before (or next to) `check_prefix_free`, so the checks run
over the same final binding list the resolver will see:

```rust
/// Specification §12: `Tab` switches panes, and no preset or layer takes it.
/// BROWSE only — every bundled preset binds `tab` to `dialog.pane` inside
/// `[dialog]`, which is not pane switching.
const SACRED_BROWSE: &[(&str, &str)] = &[("tab", "pane.switch")];

fn check_sacred(bindings: &[Binding], screen: Screen) -> Result<(), KeymapError> {
    if screen != Screen::Browse {
        return Ok(());
    }
    for (spelled, reserved_for) in SACRED_BROWSE {
        let sacred = parse_chord(spelled)?;
        for b in bindings {
            if b.seq.len() == 1 && b.seq[0] == sacred && b.run != *reserved_for {
                return Err(KeymapError::SacredKey {
                    chord: (*spelled).to_owned(),
                    reserved_for,
                    run: b.run.clone(),
                });
            }
        }
    }
    Ok(())
}

/// With counts on, a bare digit 1-9 cannot also be a binding. `0` is exempt:
/// a count never starts with zero, so the two never compete for it.
fn check_digits_free(bindings: &[Binding], counts: bool) -> Result<(), KeymapError> {
    if !counts {
        return Ok(());
    }
    for b in bindings {
        let Some(first) = b.seq.first() else { continue };
        if super::resolve::digit_of(*first).is_some_and(|d| d != 0) {
            return Err(KeymapError::DigitBoundWithCounts {
                chord: first.to_string(),
                run: b.run.clone(),
            });
        }
    }
    Ok(())
}
```

`digit_of` lives in `resolve.rs`; mark it `pub(super)` so `effective.rs` can
share it rather than growing a second copy — two definitions of "is this a
digit" is exactly how the count rule and the load rule would drift.

Call both from `build_for_impl` next to `check_prefix_free`, and add the same
two calls to `build_diagnostics` so `norte doctor` reports them instead of
being the one path that stays quiet.

- [ ] **Step 5: Run the tests**

```bash
just t norte-frontend 2>&1 | tail -20
just t norte-tui 2>&1 | tail -10
```

Expected: PASS. If a bundled preset trips either rule, that is a real conflict
the engine was hiding — fix the preset, do not exempt the binding, and say so
in your report.

- [ ] **Step 6: Turn counts on in `vim.toml`**

Add `counts = true` as the first line of
`crates/norte-frontend/presets/keymap/vim.toml`, above the first section, with
a comment saying why vim gets it and orthodox does not. (`far.toml` gets it in
K2b, when it exists.)

Then re-run Step 5's commands: if `vim.toml` binds a digit anywhere, the new
rule fires and you have found a real conflict.

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
git add crates/norte-frontend
git commit -m "feat(frontend): a digit cannot be both a count and a key, and Tab is not for sale

Two load-time rules, in the spirit of prefix-free: the conflict surfaces
when the file loads, not when a finger slips. Zero is exempt from the
first (a count never starts with it) and the second is Browse-only (every
preset binds tab to dialog.pane inside a dialog, which is not pane
switching).

vim.toml turns counts on. K2b's four imported keymaps are what make the
sacred-key rule stop being theoretical."
```

---

### Task 3: The frontends repeat, paint, and explain

**Files:**
- Modify: `crates/norte-i18n/i18n/en.ftl`, `crates/norte-i18n/i18n/es.ftl`
- Modify: `crates/norte-tui/src/main.rs` (7 `Resolution` match sites)
- Modify: `crates/norte-gui/src/main.rs` (2 sites)
- Modify: whatever renders `resolver.pending()` in each frontend's status bar

- [ ] **Step 1: Add the message**

`crates/norte-i18n/i18n/en.ftl`:

```
keymap-count-ignored = { $command } does not take a count ({ $count } ignored)
```

`crates/norte-i18n/i18n/es.ftl`:

```
keymap-count-ignored = { $command } no acepta un contador (se ignoró { $count })
```

- [ ] **Step 2: The shared sentence, with its test**

Next to `unavailable_message` in `crates/norte-frontend/src/keymap/mod.rs`:

```rust
/// The sentence for a count that landed on a command that takes none. Lives
/// here, like `unavailable_message`, so the two frontends cannot word it
/// differently.
#[must_use]
pub fn count_ignored_message(command: &str, count: u32) -> String {
    norte_i18n::ta(
        "keymap-count-ignored",
        &[("command", command), ("count", &count.to_string())],
    )
}
```

```rust
#[test]
fn el_mensaje_de_contador_ignorado_nombra_comando_y_numero() {
    let m = count_ignored_message("app.quit", 3);
    assert!(m.contains("app.quit"), "{m}");
    assert!(m.contains('3'), "{m}");
}
```

Give it a doctest (the crate warns on missing docs and its public items carry
them) and verify with `cargo test --doc -p norte-frontend`.

**What Task 1 already did here.** `just t <crate>` compiles the whole workspace
and filters at run time, so leaving `norte-tui` uncompilable would have stopped
Task 1 running its own tests. Task 1 therefore made the TUI's nine sites
compile against the struct variant, mechanically and with no count semantics:
`command` bound, count dropped, `Counting(_)` folded into the six overlay
or-patterns that already reset and ignore, and the main loop's `Counting` arm
left empty with a comment naming this task. `norte-gui` is untouched and still
does not compile — its sites are `main.rs:3681`, `main.rs:3742`, and test
expressions in `keymap.rs` at 571, 577, 582, 627, 690, 713, 736, 759, 886, 891
and 936. It uses fully-qualified paths, so it wants
`norte_frontend::keymap::Count`.

**So your job in the TUI is the behaviour, not the pattern surgery:** the
repeat loop, the `Ignored` message, and the status bar.

- [ ] **Step 3: The TUI main loop**

At the main-loop site in `crates/norte-tui/src/main.rs`, fill in the
`Resolution::Run { command, count }` arm. The repeat wraps only the fixed-command path:
`lua:` commands are never `Count::Repeat` (they are not in the catalogue), so
the Lua branch is unchanged and runs once.

```rust
                                Resolution::Run { command, count } => {
                                    app.pending.clear();
                                    if let Count::Ignored(n) = count {
                                        app.message = Some(
                                            norte_frontend::keymap::count_ignored_message(
                                                &command, n,
                                            ),
                                        );
                                    }
                                    if let Some(name) = command.strip_prefix("lua:") {
                                        run_lua_command(
                                            app, lua_host.as_ref(), backend, name,
                                            &mut lua_run, &mut lua_queue,
                                        );
                                        continue;
                                    }
                                    let Some(parsed) = Command::parse(&command) else {
                                        debug_assert!(false, "keymap fuera de COMMANDS");
                                        continue;
                                    };
                                    // A count repeats the DISPATCH: no command
                                    // signature changes and none can forget to
                                    // honour it. Stop early if the app is
                                    // quitting — `9999 q` must not queue 9998
                                    // more quits.
                                    let times = match count {
                                        Count::Repeat(n) => n.max(1),
                                        Count::None | Count::Ignored(_) => 1,
                                    };
                                    for _ in 0..times {
                                        let outcome = dispatch(/* … as today … */);
                                        // … the existing outcome handling …
                                        if app.quit {
                                            break;
                                        }
                                    }
                                }
```

Read the existing arm before writing this: it does more than call `dispatch`
(the outcome drives modals, tasks and redraws). Keep every one of those steps
inside the loop, and add the `app.quit` break. If the outcome handling contains
an early `continue` of the outer loop, restructure so the count still
terminates — a `continue` that skips the loop counter is how `5j` becomes an
infinite loop.

At the six overlay dispatchers, extend the existing
`Resolution::Pending(_) | Resolution::Unavailable { .. }` or-patterns to
include `Resolution::Counting(_)`: an overlay has no count and no status line,
so it resets and ignores. Do not add a separate arm — `match_same_arms` is
denied.

Wire `Resolution::Run { command, count }` at those six sites too: they bind the
command and ignore the count (they dispatch a fixed allowlist, none of which is
count-aware).

- [ ] **Step 4: Paint the pending count in the TUI status bar**

Find what renders `resolver.pending()` and prefix `resolver.count()` when it is
`Some`. The user must see the `5` sitting there; a count you cannot see is a
count you cannot cancel.

Add a render test in the TUI's existing render-test style asserting the status
bar shows the count after a digit.

- [ ] **Step 5: The GUI's two sites**

Same treatment: repeat the dispatch, set `self.flash` for `Count::Ignored`, and
include `Counting(_)` in the existing or-patterns where a site does not handle
counts. The GUI's dual-pane site is the one that must paint the count; check
whether it renders `pending()` at all, and if it does not, say so in your
report rather than inventing a surface.

- [ ] **Step 6: Run everything**

```bash
just t norte-frontend 2>&1 | tail -5
just t norte-tui 2>&1 | tail -5
just t norte-i18n 2>&1 | tail -5
just c 2>&1 | tail -5
just gui-ci 2>&1 | tail -15
cargo test --doc -p norte-frontend 2>&1 | tail -5
```

- [ ] **Step 7: Commit, then the plan's one `ci-fast`**

```bash
cargo fmt --all
git add crates/norte-frontend crates/norte-i18n crates/norte-tui crates/norte-gui
git commit -m "feat(tui,gui): a count repeats the dispatch, and is visible while you type it"
```

```bash
just ci-fast 2>&1 | tail -25
```

Expected: EXIT 0. One run. If red, reproduce the single failure with
`just t <crate>` and fix it there.

---

### Task 4: K1's two debts

Both were raised by K1's reviewers and deferred to here on purpose.

**Files:**
- Modify: `crates/norte-gui/src/keymap.rs`
- Modify: `crates/norte-gui/src/main.rs` (the callers)

- [ ] **Step 1: Write the failing test for the panic route**

`build_effectives_with` is the GUI's error-recovery path — it is what runs when
a user's keymap layer is broken — and K1's shadowing decision gave it a live
way to fail, because unavailable bindings now take part in the prefix-free
check. It currently ends in `.expect`.

In `crates/norte-gui/src/keymap.rs`'s test module:

```rust
/// The recovery path must not panic on the very input it exists to recover
/// from. A layer that makes the effective map ambiguous is a user's typo, not
/// a bug in norte.
#[test]
fn una_capa_ambigua_devuelve_error_en_vez_de_entrar_en_panico() {
    let layer = norte_frontend::keymap::parse_keymap(
        r#"
[pane]
prepend_keymap = [
    { on = ["z"], run = "cursor.down" },
    { on = ["z", "z"], run = "cursor.up" },
]
"#,
    )
    .expect("la capa parsea");
    assert!(build_effectives_with("orthodox", &[layer]).is_err());
}
```

- [ ] **Step 2: Run it to verify it fails**

```bash
just gui-ci 2>&1 | tail -20
```

Expected: FAIL — it panics, or does not compile because the function returns a
tuple rather than a `Result`.

- [ ] **Step 3: Make it a `Result`**

```rust
pub fn build_effectives_with(
    preset_name: &str,
    layers: &[KeymapFile],
) -> Result<(Effective, Effective), KeymapError> {
```

Propagate with `?` instead of `.expect`, and fix the callers in `main.rs`. The
GUI already has a startup banner for a broken keymap (`NorteGui::keymap_error`)
— route the error there and fall back to the bare preset with no layers, which
is what the surrounding code already does for other load failures. Follow that
existing pattern; do not invent a new recovery.

- [ ] **Step 4: Write the failing benchmark-shaped test for the round trip**

`means_command` runs on **every key event** and currently renders every binding
to a `String` and re-parses it:

```rust
    eff.bindings()
        .into_iter()
        .filter(|(seq, cmd)| *cmd == command && !seq.contains(' '))
        .filter_map(|(seq, _)| norte_frontend::keymap::parse_chord(&seq).ok())
        .any(|c| c == pressed)
```

That is about 140 allocations per keystroke today and about 450 once K2b lands
four more presets. Compare `Chord`s directly instead.

The behaviour must not change, so pin it first:

```rust
/// `means_command` answers the same question after the rewrite as before it,
/// including the two cases that made the string filter subtle: a multi-key
/// sequence never matches a single press, and an unavailable binding is not a
/// match either.
#[test]
fn means_command_ignora_secuencias_y_no_disponibles() {
    let (browse, _) = build_effectives_with("vim", &[]).expect("vim construye");
    // `g g` is a two-chord sequence: pressing `g` alone is not `cursor.top`.
    assert!(!means_command(&browse, "cursor.top", "g", Mods::default(), Some("g")));
}
```

Add whichever further pins the existing behaviour deserves — read the function's
callers first to see what it is actually asked.

- [ ] **Step 5: Rewrite it against chords**

Add to `Effective` in `norte-frontend` an accessor that answers the question
without rendering anything:

```rust
    /// Is `chord`, pressed alone, bound to `command` and runnable? The GUI
    /// asks this on every key event to decide whether a menu item's shortcut
    /// matched, so it must not allocate: the old implementation rendered every
    /// binding to a String and re-parsed it, ~140 allocations per keystroke.
    #[must_use]
    pub fn single_chord_runs(&self, chord: Chord, command: &str) -> bool {
        self.bindings.iter().any(|b| {
            b.avail == Availability::Here && b.run == command && b.seq.as_slice() == [chord]
        })
    }
```

and reduce `means_command` to `gpui_chord(...)` plus one call. Give the new
method a doctest.

- [ ] **Step 6: Run everything**

```bash
just t norte-frontend 2>&1 | tail -5
just gui-ci 2>&1 | tail -15
cargo test --doc -p norte-frontend 2>&1 | tail -5
```

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
git add crates/norte-frontend crates/norte-gui
git commit -m "fix(gui): the recovery path returns an error, and the hot path stops allocating

build_effectives_with is what runs when a user's keymap is broken, and K1's
shadowing decision gave it a live panic route through the prefix-free
check. It returns a Result now.

means_command rendered every binding to a String and re-parsed it on every
key event — ~140 allocations per keystroke, ~450 once K2b lands four more
presets. It compares chords."
```

---

### Task 5: ADR 0044, the gate, the reviews

**Files:**
- Create: `docs/adr/0044-numeric-counts-and-the-sacred-keys.md`
- Modify: `crates/norte-frontend/src/keymap/mod.rs` (ADR reference in the header)

- [ ] **Step 0: Close the sacred-key hole Task 2 left visible**

Task 2 implemented the rule as the plan wrote it: only a **single-chord** `tab`
binding is rejected. It then said plainly that a sequence like
`["tab", "j"]`, in a preset that binds no bare `tab`, would still take the key —
and declined to widen the rule, because that is policy a plan has to authorise.
It is authorised now: **pressing Tab and having it sit pending is exactly as
much a loss of pane switching as rebinding it**, so the rule is about the FIRST
chord.

In `check_sacred` in `crates/norte-frontend/src/keymap/effective.rs`, change the
condition from "the sequence is exactly the sacred chord" to "the sequence
*starts* with the sacred chord, and is not exactly that chord bound to its
reserved command":

```rust
            let starts_sacred = b.seq.first() == Some(&sacred);
            let is_the_reserved_binding = b.seq.as_slice() == [sacred] && b.run == *reserved_for;
            if starts_sacred && !is_the_reserved_binding {
```

Add the test that Task 2 correctly refused to write as a pin of the old hole:

```rust
/// Tab may not OPEN a sequence either. A preset that binds `["tab","j"]` and
/// no bare `tab` would leave Tab sitting pending, which loses pane switching
/// just as completely as rebinding it (specification §12).
#[test]
fn tab_tampoco_puede_abrir_una_secuencia() {
    let preset = parse_keymap(
        r#"
[pane]
keymap = [ { on = ["tab", "j"], run = "cursor.down" } ]
"#,
    )
    .unwrap();
    let e = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap_err();
    assert!(matches!(e, KeymapError::SacredKey { .. }), "{e:?}");
}
```

Re-run `just t norte-frontend` and `just t norte-tui`. Record the widening in
the ADR: the rule is about the first chord, and why.

- [ ] **Step 1: Write the ADR**

```
/adr numeric counts and the sacred keys
```

What it must record:

- **The count repeats the dispatch; it does not reach the command.** The
  alternative needs an arm per command, and a command that forgets to read its
  count returns to the silence K1 spent five commits removing.
- **The catalogue decides who takes a count**, and a count over a command that
  does not is `Ignored`, not swallowed. A `lua:` command is not in the
  catalogue, so a count over one is honestly `Ignored`.
- **Zero never opens a count** (so `0` stays bindable) but does accumulate.
- **Four digits is the ceiling**, and a fifth digit is dropped rather than
  wrapping.
- **Counts are opt-in per preset and forbidden in a user layer** — the policy
  belongs to the preset, and a layer flipping it would silently change what
  every digit key means.
- **A digit bound while counts are on is a load error**, and **`Tab` bound to
  anything but `pane.switch` on the Browse screen is a load error**. Both in
  the spirit of prefix-free: conflicts surface at load. The sacred rule is
  Browse-only because every bundled preset legitimately binds `tab` to
  `dialog.pane` inside `[dialog]`.
- **Resolution is still timing-free.** A count terminates on the first
  non-digit; there is no timeout anywhere, which is what ADR 0006 bought and
  this must not spend.
- **What repeating buys, and what it does not.** Task 1 found this while
  writing the tests: vim's `12gg` means "go to line 12", and repeating the
  dispatch twelve times cannot produce it — twelve "go to the top" is still the
  top. The catalogue is right to declare `cursor.top` as `counts: false`, and a
  count over it is honestly `Ignored`. A real `12gg` needs a new command that
  takes a line number (`cursor.goto-line`), not a flag flip. Say so, so that
  K2b does not try to buy it with `counts = true`.

It extends ADR 0006 and ADR 0043; say which parts.

- [ ] **Step 2: Commit the ADR**

```bash
git add docs/adr crates/norte-frontend/src/keymap/mod.rs
git commit -m "docs(adr): 0044 — counts repeat the dispatch, and two keys are not for sale"
```

- [ ] **Step 3: The plan's one full gate**

```bash
just ci 2>&1 | tail -30
```

Expected: EXIT 0. Report the coverage number; this plan touches none of
proto/vfs/core, so it should be unmoved.

- [ ] **Step 4: The GUI gate**

```bash
just gui-ci 2>&1 | tail -20
```

- [ ] **Step 5: Dispatch the reviewers**

Per CLAUDE.md you dispatch these yourself and report with the findings already
applied. Warranted here:

- **`rust-reviewer`** — a substantial Rust diff across `norte-frontend`,
  `norte-tui`, `norte-gui`.
- **`test-engineer`** — the count accumulator is a small state machine with an
  obvious property-test shape, and this repository already uses proptest for
  the keymap (`crates/norte-tui/tests/keymap_prop.rs`).

Not warranted: `protocol-guardian` (no proto, no JSON-RPC), `security-reviewer`
(no journal, policy, auth, secrets, plugin-host, MCP), `encoding-auditor` (the
chord parser is untouched; say so when you decide not to dispatch it, and
reconsider if you ended up touching `chord.rs` for more than `parts()`).

Give each the commit range, what the change is for, and these questions:

**`rust-reviewer`:**
1. The TUI repeats `dispatch` up to 9 999 times inside the key-event arm. Can
   any outcome in that loop re-enter the resolver, block, or spawn a task per
   iteration? A count over a task-submitting command would be 9 999 tasks —
   the catalogue is supposed to prevent it, but is the catalogue's `counts`
   flag actually correct for every command marked `true`?
2. `digit_of` is shared between the resolver and the load-time digit rule. Is
   there any input where the two disagree about what a digit is?
3. `Resolution::Counting` was added to six or-patterns in overlays that reset
   and ignore. Is there an overlay where swallowing a digit is wrong?

**`test-engineer`:**
1. What property should the count accumulator hold that the example tests do
   not pin? Write it as a proptest in the existing
   `crates/norte-tui/tests/keymap_prop.rs` style.
2. The sacred-key and digit rules are checked in both `build_for_impl` and
   `build_diagnostics`. Is there a test proving the two paths agree, in the
   way `check_binding`'s rustdoc says they must?

Apply every BLOCKER and MAJOR. Apply the cheap MINORs; name the ones you skip
and why.

---

## Self-review

**Spec coverage.** Every K2a bullet maps to a task: counts and the repeat model
(Task 1), the ceiling, the ignored case and the opt-in flag (Task 1, Task 2
Step 6), the digit rule and the sacred key (Task 2), the frontends repeating,
painting and explaining (Task 3), `build_effectives_with` → `Result` and the
`means_command` allocation (Task 4), the record (Task 5).

**Deliberately not here.** `far.toml` gets `counts = true` in K2b, when the file
exists. Sacred keys beyond `Tab` — the specification names only that one; the
`SACRED_BROWSE` table is a list so a second entry costs a line, but inventing
entries the spec does not name would be making policy in a plan.

**Type consistency.** `Count::{None, Repeat, Ignored}` is the one name, used
identically in `resolve.rs`, both frontends and the ADR. `Resolution::Run` is a
struct variant `{ command, count }` everywhere after Task 1. `digit_of` has one
definition, in `resolve.rs`, `pub(super)`, used by `effective.rs`.

**Known risk.** Task 3's repeat loop sits inside the TUI's largest match arm,
which already handles Lua dispatch, outcomes, modals and redraws. If that arm
contains a `continue` of the outer event loop, a naive wrap turns `5j` into an
infinite loop. The plan says to read the arm first and restructure; the
reviewer question about re-entry is aimed at the same place.
