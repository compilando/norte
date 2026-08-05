# H3d — the help stops offering what the app would refuse

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A help row for a command that cannot run right now renders dimmed with the reason, instead of promising something the app will refuse — and the verdict comes from ONE table, shared with the GUI's context menu, which already answers the same question.

**Architecture:** The GUI already computes exactly this (`norte-gui/src/context_menu.rs`: `Facts` + `items()`, against `norte_help::Availability`/`Reason`). That table moves to `norte-frontend`, both frontends consume it, and the TUI feeds it facts it can get cheaply — plus one it can get for free, because `fs.capabilities` already returns the capability flags alongside the attribute catalogue the TUI fetches per scheme and currently throws half of away.

**Tech Stack:** Rust, `norte-help` (the `Availability`/`Reason` vocabulary), `norte-frontend` (the shared table), ratatui, Fluent, nextest.

**Spec:** `docs/superpowers/specs/2026-08-04-help-system-redesign-design.md`, phase H3d ("`Availability` wiring (caps, policy, plugin state, connection)").

---

## What the recon found, and what it changes

Four facts drive every decision below.

**1. The GUI already has the table.** `context_menu.rs:129` defines `Facts { kind, count, source_read_only, dest_read_only }`, frozen while the menu is open, and `items(&Facts)` gives a per-command verdict using the same `norte_help::Reason` vocabulary, with `no`/`gated`/`first_failure` helpers and an ordering decision inside `pane.rename` (read-only before not-single). Writing a second table for the TUI guarantees the two drift. It moves to `norte-frontend`, which both frontends already depend on.

**2. One TUI divergence must be modelled, not flattened.** In the GUI `nav.enter` needs `kind == Dir`; in the TUI a `.zip` FILE is enterable (`main.rs:6665` falls back to `archive_root_for`). So the fact is not "is a directory" but "can be entered", and the frontends compute it differently. That belongs in `Facts` as a boolean the caller fills, not as a `kind` the table interprets.

**3. The capabilities round trip is already being made.** `fs.capabilities` returns `capabilities` AND `attrs` (`proto/methods.rs:1167`); `App` caches only the attrs (`app.rs:721`, `attr_catalogs`). Caching the caps half beside them costs zero extra calls and gives the honest answer for `READ_ONLY` and `TRASH` instead of the scheme-syntactic guess. The syntactic helper still earns its place as the answer before the first fetch lands.

**4. Two things are NOT available and must not be faked.**
- `Reason::PolicyDenied` is unreachable in the embedded TUI: `ScopedPolicy::evaluate` returns `Allow` for `Actor::User` (`policy.rs:350`), and the embedded engine gets `AllowAll` anyway. The TUI is the approver of an agent's request, never a denied actor. Do not compute it; say so where someone would look.
- "Digest changed, re-consent" is not distinguishable on the wire: `PluginInfo` carries only `approved: bool`, and a stale digest arrives as `approved: false` (`plugins.rs:897`). `PluginInactive` covers both, and the finer distinction would need a proto field — out of scope here.

One bug surfaces on the way and gets fixed because H3d needs the data: `app.connection_warning` is a single pre-formatted string, never cleared, last-writer-wins, and the structured `ConnectionDegraded { scheme, host, reason }` is discarded into it (`main.rs:1646`). H3d keeps the structured value per scheme; the banner keeps working, and "which connection degraded" becomes answerable.

## File structure

| File | Responsibility |
| --- | --- |
| `crates/norte-frontend/src/availability.rs` (new) | The moved table: `Facts`, `verdict(command, &Facts) -> Availability`, the `no`/`gated`/`first_failure` helpers, and `reason_key(Reason) -> &'static str` for Fluent. |
| `crates/norte-gui/src/context_menu.rs` | Consumes it; keeps only its own labels and painting. |
| `crates/norte-tui/src/app.rs` | `caps: HashMap<String, Capabilities>`; `degraded: HashMap<String, ConnectionDegraded>`; `Facts` assembly. |
| `crates/norte-tui/src/help.rs` | `TuiChords` gains the facts and answers `availability` from the table. |
| `crates/norte-tui/src/help_render.rs` | Paints the reason next to a dimmed row. |
| `crates/norte-i18n/i18n/{en,es}.ftl` | `reason-*` keys, shared by both frontends (the current ones are `gui-`prefixed). |

---

### Task 1: move the verdict table into `norte-frontend`

**Files:**
- Create: `crates/norte-frontend/src/availability.rs`
- Modify: `crates/norte-frontend/src/lib.rs`
- Modify: `crates/norte-gui/src/context_menu.rs`
- Modify: `crates/norte-i18n/i18n/{en,es}.ftl`

- [ ] **Step 1: Read the source before moving it**

Read `crates/norte-gui/src/context_menu.rs` in full — the module docs, `Facts`, `items`, `no`/`gated`/`first_failure`, `reason_key`, and every test. The verdicts and their ORDER are the asset; the labels and the painting are the GUI's and stay there.

- [ ] **Step 2: Write the failing tests**

In `crates/norte-frontend/src/availability.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use norte_help::Reason;

    fn one_file() -> Facts {
        Facts {
            enterable: false,
            viewable: true,
            single: true,
            source_read_only: false,
            dest_read_only: false,
            degraded: false,
        }
    }

    #[test]
    fn copiar_hacia_un_destino_de_solo_lectura_esta_vetado() {
        let f = Facts {
            dest_read_only: true,
            ..one_file()
        };
        assert_eq!(
            verdict("pane.copy", &f).reason(),
            Some(Reason::ReadOnlyBackend)
        );
        // Y al revés: leer DESDE un origen de solo lectura es el caso que la
        // función existe para permitir — copiar de un .zip a un bucket.
        let desde_zip = Facts {
            source_read_only: true,
            ..one_file()
        };
        assert!(verdict("pane.copy", &desde_zip).is_available());
    }

    #[test]
    fn renombrar_reporta_el_primer_fallo_no_el_ultimo() {
        // El orden es la decisión: con un lote de 3 ficheros dentro de un
        // zip, «es de solo lectura» explica más que «hay más de uno».
        let f = Facts {
            single: false,
            source_read_only: true,
            ..one_file()
        };
        assert_eq!(
            verdict("pane.rename", &f).reason(),
            Some(Reason::ReadOnlyBackend)
        );
    }

    #[test]
    fn entrar_lo_decide_el_llamador_no_el_tipo_de_entrada() {
        // La divergencia REAL entre frontends: en la TUI un .zip se ENTRA
        // (se compone el scheme), en la GUI no. Por eso el hecho es
        // «entrable», no «es un directorio».
        let zip = Facts {
            enterable: true,
            ..one_file()
        };
        assert!(verdict("nav.enter", &zip).is_available());
        assert_eq!(
            verdict("nav.enter", &one_file()).reason(),
            Some(Reason::WrongTarget)
        );
    }

    #[test]
    fn un_comando_que_la_tabla_no_conoce_esta_disponible() {
        // Fail-OPEN a propósito, y es lo contrario de lo que suele pedirse:
        // la tabla no puede saber de cada comando del vocabulario, y atenuar
        // por defecto convertiría cada comando nuevo en un comando que la
        // ayuda declara roto. Ofrecerlo y que falle honestamente es mejor
        // que negarlo por ignorancia.
        assert!(verdict("app.quit", &one_file()).is_available());
        assert!(verdict("no.such.command", &one_file()).is_available());
    }

    #[test]
    fn cada_razon_tiene_clave_fluent_y_ninguna_se_solapa() {
        use std::collections::BTreeSet;
        let mut vistas = BTreeSet::new();
        for r in [
            Reason::ReadOnlyBackend,
            Reason::Unsupported,
            Reason::PluginInactive,
            Reason::PolicyDenied,
            Reason::ConnectionDegraded,
            Reason::WrongTarget,
        ] {
            let k = reason_key(r);
            assert!(!k.is_empty(), "{r:?} sin clave");
            assert!(vistas.insert(k), "clave repetida: {k}");
        }
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo nextest run -p norte-frontend availability 2>&1 | tail -5; echo "EXIT=$pipestatus[1]"`
Expected: FAIL — the module does not exist.

- [ ] **Step 4: Move the table**

Write `crates/norte-frontend/src/availability.rs` with the module doc stating what it is and why it is shared:

```rust
//! Whether a command can run RIGHT NOW, and why not.
//!
//! One table, two frontends. The GUI dims a context-menu entry and the TUI
//! dims a help row for the same reasons, and they must agree: a menu that
//! greys out "copy" while the help page says it is available is worse than
//! either alone. The vocabulary is `norte_help::Reason`, so the third
//! consumer — the help corpus' own rendering — reads the same answers.
//!
//! The table is a function of FACTS the caller gathers, never of state it
//! reaches for: a verdict computed while a menu is open must not change under
//! the reader's cursor, and the frontends disagree about how to compute some
//! of the facts (a `.zip` is enterable in the TUI and not in the GUI).
```

`Facts` carries booleans the caller fills, not raw state:

```rust
pub struct Facts {
    /// The focused entry can be entered (a directory, or an archive the
    /// frontend knows how to compose a scheme for).
    pub enterable: bool,
    /// The focused entry has something to show in a viewer.
    pub viewable: bool,
    /// Exactly one entry is the target.
    pub single: bool,
    /// The pane the command reads FROM refuses mutation.
    pub source_read_only: bool,
    /// The pane the command writes TO refuses mutation.
    pub dest_read_only: bool,
    /// The connection behind the acting pane is degraded.
    pub degraded: bool,
}
```

`verdict(command: &str, facts: &Facts) -> Availability` ports every arm of the GUI's `items`, keyed by command id rather than returning labels. Keep `first_failure`'s ordering decisions and their comments — they are the part someone would otherwise get wrong. Fail OPEN for unknown commands, with the reasoning from the test.

`reason_key(Reason) -> &'static str` returns Fluent ids WITHOUT the `gui-` prefix (`reason-read-only`, `reason-wrong-target`, `reason-unsupported`, `reason-plugin-inactive`, `reason-policy-denied`, `reason-connection-degraded`). `Reason` is `#[non_exhaustive]`, so the match needs a wildcard — give it a comment saying which key an unknown reason falls back to and why that is safe.

Add the new keys to BOTH locales. Keep the `gui-menu-reason-*` keys for now if anything still reads them; delete them in Step 5 once the GUI is switched.

- [ ] **Step 5: Switch the GUI to the shared table**

`context_menu.rs` keeps `Item`, its `label_key`s and its painting, and calls `norte_frontend::availability::verdict` for each command. Its `Facts` disappears in favour of the shared one — the GUI fills `enterable: single && kind == Dir` and `viewable: single && kind == File`, which is exactly what its old arms said.

Its existing tests must keep passing unchanged where they assert verdicts; if one asserts a verdict this move changes, STOP and report rather than editing the assertion.

- [ ] **Step 6: Verify both crates**

Run: `cargo nextest run -p norte-frontend -p norte-tui --features norte-tui/schema --features norte-config/watch --features norte-proto/schema 2>&1 | tail -5; echo "EXIT=$pipestatus[1]"`
Run: `just check-gui 2>&1 | tail -3; echo "EXIT=$pipestatus[1]"` — and then `just gui-ci 2>&1 | tail -5; echo "EXIT=$pipestatus[1]"`, because the GUI is out of the workspace and `just ci` will not run its tests.
Run: `cargo nextest run -p norte-i18n 2>&1 | tail -3; echo "EXIT=$pipestatus[1]"`

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
git add crates/norte-frontend crates/norte-gui crates/norte-i18n
git commit -m "refactor(frontend): one availability table, shared by the menu and the help (H3d)"
```

---

### Task 2: the TUI keeps the capabilities it is already fetching

**Files:**
- Modify: `crates/norte-tui/src/app.rs` (the cache next to `attr_catalogs`)
- Modify: `crates/norte-tui/src/main.rs` (fill it where the attr catalogue is filled)

- [ ] **Step 1: Write the failing test**

In `crates/norte-tui/src/app.rs`'s tests:

```rust
    /// Las caps se cachean por SCHEME, como el catálogo de atributos, y por
    /// la misma razón: `fs.capabilities` devuelve las dos mitades en UNA
    /// llamada y la TUI ya la hace para las columnas. Tirar la mitad de caps
    /// y luego sondear otra vez sería pagar dos rondas por un dato que ya
    /// llegó.
    #[test]
    fn las_caps_se_cachean_por_scheme() {
        let mut app = app_dos_panes();
        assert!(app.caps("file").is_none(), "sin sembrar, no se inventa nada");
        app.insert_caps("file".to_owned(), caps_de_test());
        assert!(app.caps("file").is_some());
        assert!(app.caps("sftp").is_none(), "un scheme no responde por otro");
    }

    /// Antes de que llegue la primera respuesta, la respuesta honesta es «no
    /// lo sé», y quien pregunta cae al criterio SINTÁCTICO (el scheme dice si
    /// es un archivo comprimido). Lo que no puede hacer es afirmar que se
    /// puede escribir.
    #[test]
    fn sin_caps_todavia_el_solo_lectura_lo_decide_el_scheme() {
        let app = app_dos_panes();
        assert!(!app.pane_read_only(0), "file:// no es de solo lectura");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `just t norte-tui 2>&1 | tail -5; echo "EXIT=$pipestatus[1]"` — FAIL, no `caps` method.

- [ ] **Step 3: Implement the cache and the read-only question**

`App` gains `caps: HashMap<String, norte_proto::Capabilities>` beside `attr_catalogs`, with `caps(&self, scheme) -> Option<&Capabilities>` and `insert_caps`. Rustdoc must say the two are filled from ONE call and why keeping only half was waste.

Then the question the table needs:

```rust
    /// Whether the pane's location refuses mutation.
    ///
    /// Answered from the capability flags when they have arrived, and
    /// SYNTACTICALLY from the scheme until they do — an archive scheme is
    /// read-only by construction, so the guess is right for the case that
    /// matters and wrong only in the direction of offering something that
    /// will then fail honestly. It never claims a location is writable that
    /// the flags say is not: `READ_ONLY`'s own contract is that the UI vetoes
    /// upfront.
    #[must_use]
    pub fn pane_read_only(&self, pane: usize) -> bool { … }
```

The syntactic half is `norte_proto::scheme_archive_format(scheme).is_some()`. The GUI has the same helper inline (`norte-gui/src/main.rs:5159`); move it to `norte-frontend` so there is one, and have the GUI call it.

- [ ] **Step 4: Fill the cache where the other half already lands**

Find where `attr_catalogs` is populated (`main.rs`, around the `first_page`/`fetch_catalog` path and `app.insert_attr_catalog`) and insert the caps from the SAME response. Do not add a call. If the response type does not carry caps in some path, say so in your report rather than adding a probe.

- [ ] **Step 5: Verify and commit**

```bash
just t norte-tui && just c norte-tui && cargo fmt --all
git add crates/norte-tui crates/norte-frontend crates/norte-gui
git commit -m "feat(tui): keep the capabilities that arrive with the attribute catalogue (H3d)"
```

---

### Task 3: which connection degraded, instead of a sentence about it

**Files:**
- Modify: `crates/norte-tui/src/app.rs`, `crates/norte-tui/src/main.rs`, `crates/norte-tui/src/ui.rs`

- [ ] **Step 1: Write the failing tests**

```rust
    /// #44 guardaba la degradación como PROSA ya formateada: el scheme y el
    /// host se metían en el mensaje y se tiraban, así que «¿qué conexión se
    /// degradó?» no tenía respuesta. H3d la necesita por pane.
    #[test]
    fn la_degradacion_se_guarda_por_scheme() {
        let mut app = app_dos_panes();
        app.note_degraded(degradacion_de_test("sftp", "ejemplo.org"));
        assert!(app.degraded_for("sftp").is_some());
        assert!(app.degraded_for("file").is_none());
    }

    /// Y dos conexiones degradadas no se pisan: antes la última ganaba y la
    /// primera desaparecía de la barra sin que nada la hubiera resuelto.
    #[test]
    fn dos_degradaciones_conviven() {
        let mut app = app_dos_panes();
        app.note_degraded(degradacion_de_test("sftp", "a.org"));
        app.note_degraded(degradacion_de_test("ftp", "b.org"));
        assert!(app.degraded_for("sftp").is_some());
        assert!(app.degraded_for("ftp").is_some());
    }
```

- [ ] **Step 2: Run to verify failure, then implement**

`App` gains `degraded: HashMap<String, norte_proto::methods::ConnectionDegraded>` plus `note_degraded` and `degraded_for`. The status-bar banner keeps rendering — build its text from the map (most recent, or a count when there is more than one) so the existing behaviour survives and the data stops being lost.

Leave the "never cleared" half alone unless it is free: a degradation is not known to be resolved without a successful reconnect, and inventing a clearing rule here would be guessing. Say so in the rustdoc, and note it as follow-up work.

- [ ] **Step 3: Verify and commit**

```bash
just t norte-tui && just c norte-tui && cargo fmt --all
git add crates/norte-tui
git commit -m "feat(tui): keep which connection degraded, not just a sentence about it (H3d)"
```

---

### Task 4: the help asks the table

**Files:**
- Modify: `crates/norte-tui/src/help.rs` (`TuiChords`)
- Modify: `crates/norte-tui/src/app.rs` (assembling `Facts`)
- Modify: `crates/norte-tui/src/help_render.rs` (painting the reason)

- [ ] **Step 1: Write the failing tests**

```rust
    /// H3d: la fila de un comando que no puede correr AHORA sale atenuada y
    /// con su razón, en vez de prometer algo que la app va a rechazar.
    #[test]
    fn dentro_de_un_zip_copiar_hacia_aqui_esta_vetado() {
        let r = resolver_con(Facts {
            dest_read_only: true,
            ..facts_normales()
        });
        assert_eq!(
            r.availability("pane.copy").reason(),
            Some(norte_help::Reason::ReadOnlyBackend)
        );
    }

    /// Y el caso que la fase existe para NO romper: un comando que sí puede
    /// correr sigue disponible. Una ayuda que atenúa de más es tan inútil
    /// como una que no atenúa nada.
    #[test]
    fn lo_que_puede_correr_sigue_disponible() {
        let r = resolver_con(facts_normales());
        assert!(r.availability("pane.copy").is_available());
        assert!(r.availability("app.quit").is_available());
    }
```

and in `help_render.rs`:

```rust
    #[test]
    fn una_fila_vetada_pinta_su_razon() {
        // Atenuar sin decir por qué deja al lector adivinando si es un bug.
        let out = render_topic(topic(Lang::En, "copying").expect("copying"), Lang::En, &Vetado, 60, &theme());
        let texto = text_of(&out.lines);
        assert!(
            texto.contains(&norte_i18n::t("reason-read-only")),
            "la razón acompaña a la fila atenuada: {texto}"
        );
    }
```

- [ ] **Step 2: Run to verify failure, then implement**

`TuiChords` gains the facts (a `Facts` value, captured when the help opens — the same freezing decision the GUI menu makes, and for the same reason: a verdict must not change under the reader's cursor mid-page). `availability` delegates to `norte_frontend::availability::verdict`.

`App` assembles the `Facts`: `enterable` from the same predicate `nav.enter` uses (`Dir | Symlink`, plus `archive_root_for`), `viewable` from `pane.view`'s (`File | Symlink`), `single` from the marked set, `source_read_only`/`dest_read_only` from `pane_read_only`, `degraded` from `degraded_for(scheme)`.

The renderer paints the reason after the label on an unavailable row, using `reason_key` + Fluent, and keeps the dim style it already applies (`help_render.rs:336` already branches on `is_available`).

`availability` must NOT compute `PolicyDenied`: document at the call site that the embedded TUI's actor is `User`, which the policy engine allows unconditionally, so the reason is meaningful only for an agent going through the daemon — and that faking it here would dim a row for a rule that does not apply.

- [ ] **Step 3: Verify and commit**

```bash
just t norte-tui && just c norte-tui && cargo fmt --all
git add crates/norte-tui
git commit -m "feat(tui): help rows dim with their reason when a command cannot run (H3d)"
```

---

### Task 5: drive it, write it down, gate it

- [ ] **Step 1: Drive the real app**

```bash
cargo build -q -p norte-tui
tmux kill-session -t h3d 2>/dev/null
tmux new-session -d -s h3d -x 113 -y 30 -c "$PWD" "NORTE_LANG=es target/debug/norte-tui"
sleep 2
```

Navigate a pane INTO a `.zip` (there are archives under `crates/norte-testkit` fixtures, or make one with `zip`), then press `F1` and open the copying page. The rows for the commands that write into the archive must be dim with a reason; the ones that read out of it must not. Capture and paste. Then check the same page from an ordinary directory: nothing dim.

- [ ] **Step 2: Changelog**

Under `## [Unreleased]` → `### Added`, in the file's voice: help rows now dim with the reason when a command cannot run where you are — inside an archive, over a degraded connection — and the verdict comes from the same table the GUI's context menu uses, so the two cannot disagree. Say plainly what is NOT computed: policy denial (the human is never the denied actor in the embedded app) and the difference between "never approved" and "approval expired" for a plugin (the wire does not carry it).

- [ ] **Step 3: Full gate**

Run: `just ci > /tmp/ci.log 2>&1; echo "EXIT=$status"; grep -E "^error|Summary" /tmp/ci.log | tail -4`
Then `just gui-ci 2>&1 | tail -5; echo "EXIT=$pipestatus[1]"` — the GUI is out of the workspace and `just ci` only `cargo check`s it, but Task 1 changed its code.

- [ ] **Step 4: Reviewers**

`rust-reviewer` over the diff. Ask specifically whether the frozen `Facts` can go stale in a way the reader would notice (the help stays open while a task completes and a pane changes), and whether the fail-open default for unknown commands is the right call for every command in the vocabulary.

---

## Self-review notes

- **Spec coverage.** caps: Tasks 2 and 4. connection: Tasks 3 and 4. plugin state: NOT wired — see below. policy: deliberately not computed, documented in Task 4.
- **Plugin state is deferred and this is a scope decision, not an oversight.** `plugin.list` is refetched on every call, builds an ephemeral registry per call even embedded, and a plugin's commands are already filtered OUT of the palette entirely when inactive (`norte-frontend/src/palette.rs:73`) — so `PluginInactive` has no surface that shows the row at all today. Wiring it means either caching plugin state in `App` (a new invalidation problem) or a round trip while painting a frame (unacceptable). It belongs with H3e, which puts plugin help on the wire and has to solve plugin-state caching anyway. Record it in the changelog as not-done rather than pretending.
- **Known risk.** Task 1 moves code the GUI depends on, and the GUI is outside the workspace, so `just ci` will not catch a break — `just gui-ci` is not optional in Steps 6 and 3.
