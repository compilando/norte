# The log panel reads the daemon too — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A frontend talking to a separate daemon can read the daemon's log ring — the lines about providers, journal, policy and failed connections — instead of only its own process's lines.

**Architecture:** The daemon mounts a `LogRing` it never had. Two new RPCs serve it: `log.tail { cursor, max }` returns the lines after a cursor plus how many that cursor missed, and `log.level { level }` raises the daemon's ring level **inside the daemon**, so the whitelist cap (`bajo_cota`) survives the socket without a second copy. Frontends poll only while the panel is open, and merge the remote lines with their own by timestamp behind a source selector that lives in `norte-frontend` so the window and the TUI share it.

**Tech Stack:** Rust, `tracing`/`tracing-subscriber`, JSON-RPC over a unix socket, `norte-proto` goldens + JSON Schema, TypeScript renderer for the Tauri window.

**Spec:** `docs/superpowers/specs/2026-09-02-daemon-log-over-the-wire-design.md`

## Global Constraints

- Protocol version goes **0.64.0 → 0.65.0**. Any change to `norte-proto` requires the version bump, catalogue entry, regenerated goldens, regenerated `docs/schema/proto.schema.json`, and a `protocol-guardian` review (CLAUDE.md).
- **The cap is not negotiable.** `bajo_cota` in `crates/norte-config/src/logring.rs` is a whitelist: only targets starting with `norte`/`ntc` go above INFO. No code in this plan may bypass, duplicate or reimplement it, and the client never applies a level itself.
- **Agents and plugins get nothing.** `log.tail` and `log.level` refuse any actor that is not `Actor::User`, the same way `methods::CONNECTION_LIST` does at `crates/norte-core/src/daemon/server.rs:2590`.
- Filenames are bytes; log lines are already `String` (the ring flattens them), so no `VPath` handling changes here.
- Typed errors: `thiserror` in libraries, `anyhow` only in binaries. No `unwrap()`/`expect()` outside tests without a stated invariant.
- User-facing strings go through Fluent in `crates/norte-i18n/i18n/{en,es}.ftl` — **both locales, always**.
- Tests: `just t <crate>` in the RED→GREEN loop. `just ci-fast` once after Task 3 and once after Task 6. `just ci` once at the end. Never use the gate as a debugger.
- `just t` does not run doctests and `just c` does not check intra-doc links: after touching rustdoc run `cargo test -p <crate> --doc` and `cargo doc -p <crate> --no-deps`.
- Branch: `feat/daemon-log-over-the-wire` (already created). Conventional Commits, Spanish commit bodies as in this repo.

---

## File map

| file | responsibility | task |
| --- | --- | --- |
| `crates/norte-config/src/logring.rs` | `since(cursor)`: the slice after a cursor, and what that cursor missed | 1 |
| `crates/norte-proto/src/methods.rs` | `LOG_TAIL`, `LOG_LEVEL`, their params/results, the wire `LogLine`/`LogLevel`, version history | 2 |
| `crates/norte-proto/src/catalog.rs` | the two new entries | 2 |
| `docs/adr/0092-*.md` | why polling with a cursor, and why the cap cannot travel | 2 |
| `crates/norte-cli/src/main.rs` | the daemon mounts a ring | 3 |
| `crates/norte-core/src/daemon/server.rs` | `Shared.log_ring`, the two dispatch arms, the actor gate | 3 |
| `crates/norte-client/src/remote/mod.rs` | `log_tail` / `log_level`, degrading on an older daemon | 4 |
| `crates/norte-frontend/src/logpanel.rs` | the source selector and the merge | 5 |
| `crates/norte-ui-host/src/backend.rs` + `controller/logpanel.rs` + `dto.rs` | the two backend methods, the poll, the projection | 6 |
| `crates/norte-gui-tauri/ui/src/{render.ts,types.ts,keys.ts}` | the selector in the window | 6 |
| `crates/norte-tui/src/logview.rs` + `app.rs` | the same for `ntc --socket` | 7 |

---

### Task 1: The ring answers "what came after this cursor?"

**Files:**
- Modify: `crates/norte-config/src/logring.rs`
- Test: same file (`mod tests` at the bottom, in Spanish like its neighbours)

**Interfaces:**
- Consumes: nothing.
- Produces: `LogRing::since(&self, cursor: u64, max: usize) -> Tail`, and

```rust
/// Lo que había después de un cursor, y lo que ese cursor se perdió.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tail {
    /// Las líneas posteriores al cursor, de la más vieja a la más nueva.
    pub lines: Vec<LogLine>,
    /// El cursor para la siguiente llamada.
    pub next: u64,
    /// Cuántas líneas cayeron del anillo antes de que este cursor las viera.
    pub lost: u64,
}
```

The arithmetic: `pushed` is monotonic and `lines.len()` is what survives, so the oldest line still held has index `base = pushed - len`. A cursor below `base` missed `base - cursor` lines. A cursor above `pushed` (a daemon restarted under a client that kept its cursor) is treated as `pushed` — nothing new, nothing lost.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/norte-config/src/logring.rs`:

```rust
/// El caso normal: pides desde donde te quedaste y te dan lo nuevo.
#[test]
fn desde_un_cursor_llegan_solo_las_nuevas() {
    let anillo = LogRing::new(10);
    for i in 0..4 {
        anillo.push(linea(LogLevel::Info, "norte_core", &format!("l{i}")));
    }
    let t = anillo.since(2, 100);
    assert_eq!(t.lines.len(), 2);
    assert_eq!(t.lines[0].message, "l2");
    assert_eq!(t.next, 4);
    assert_eq!(t.lost, 0);
}

/// Un cursor de antes del desbordamiento DICE cuántas se perdió. Un hueco
/// silencioso miente sobre lo que hubo, que es el motivo de que `dropped`
/// exista.
#[test]
fn un_cursor_rancio_dice_cuantas_se_perdio() {
    let anillo = LogRing::new(3);
    for i in 0..7 {
        anillo.push(linea(LogLevel::Info, "norte_core", &format!("l{i}")));
    }
    // El anillo guarda l4,l5,l6: base = 7 - 3 = 4.
    let t = anillo.since(1, 100);
    assert_eq!(t.lost, 3, "se perdió l1, l2 y l3");
    assert_eq!(t.lines.len(), 3);
    assert_eq!(t.lines[0].message, "l4");
    assert_eq!(t.next, 7);
}

/// `max` acota la respuesta y el cursor avanza SOLO lo entregado: pedir de
/// nuevo continúa donde se cortó, sin saltarse nada.
#[test]
fn max_acota_y_el_cursor_no_se_adelanta() {
    let anillo = LogRing::new(10);
    for i in 0..5 {
        anillo.push(linea(LogLevel::Info, "norte_core", &format!("l{i}")));
    }
    let t = anillo.since(0, 2);
    assert_eq!(t.lines.len(), 2);
    assert_eq!(t.next, 2);
    let t2 = anillo.since(t.next, 2);
    assert_eq!(t2.lines[0].message, "l2");
}

/// Un cursor del futuro —un daemon reiniciado bajo un cliente que guardó el
/// suyo— no es un pánico ni un hueco: no hay nada nuevo y no se perdió nada.
#[test]
fn un_cursor_del_futuro_no_inventa_nada() {
    let anillo = LogRing::new(10);
    anillo.push(linea(LogLevel::Info, "norte_core", "l0"));
    let t = anillo.since(99, 100);
    assert!(t.lines.is_empty());
    assert_eq!(t.next, 1);
    assert_eq!(t.lost, 0);
}
```

If the existing `mod tests` has no `linea(...)` helper, write one:

```rust
fn linea(level: LogLevel, target: &str, message: &str) -> LogLine {
    LogLine { epoch_ms: 0, level, target: target.into(), message: message.into() }
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `just t norte-config`
Expected: FAIL — `no method named 'since'`.

- [ ] **Step 3: Implement `since` and `Tail`**

In `impl LogRing`, next to `snapshot`. Take the lock once and compute `base` **inside** it, or `pushed` and `len` can disagree under a concurrent writer and `lost` comes out wrong.

- [ ] **Step 4: Run the tests and watch them pass**

Run: `just t norte-config`
Expected: PASS.

- [ ] **Step 5: Rustdoc check**

`Tail` and `since` are public items in a crate with `#![warn(missing_docs)]`. Give `since` a doctest, then:
Run: `cargo test -p norte-config --doc` and `cargo doc -p norte-config --no-deps`
Expected: both pass.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-config/src/logring.rs
git commit  # feat(config): el anillo contesta «qué hubo después de este cursor»
```

---

### Task 2: The wire — protocol 0.65.0

**Files:**
- Modify: `crates/norte-proto/src/methods.rs`, `crates/norte-proto/src/catalog.rs`
- Modify (regenerated): `crates/norte-proto/tests/golden/catalogo.tsv`, `crates/norte-proto/tests/golden/types/methods.json`, `docs/schema/proto.schema.json`
- Create: `docs/adr/0092-<slug>.md`

**Interfaces:**
- Consumes: `norte_config::logline::LogLevel::wire()` — the wire vocabulary (`"error"|"warn"|"info"|"debug"|"trace"`) already fixed for bridge 46. The protocol repeats it; it does not import it (`norte-config` depends on `norte-proto`, not the other way).
- Produces:

```rust
pub const LOG_TAIL: &str = "log.tail";
pub const LOG_LEVEL: &str = "log.level";

pub struct LogTailParams { pub cursor: Option<u64>, pub max: u32 }
pub struct LogTailResult {
    pub lines: Vec<LogLine>,
    pub next: u64,
    pub lost: u64,
    pub level: String,     // vocabulario de `LogLevel::wire()`
    pub capacity: u32,
}
pub struct LogLevelParams { pub level: String }
pub struct LogLevelResult { pub level: String }
/// Una línea de registro, tal como viaja.
pub struct LogLine { pub epoch_ms: i64, pub level: String, pub target: String, pub message: String }
```

`cursor: None` means "whatever you have" — what a panel sends when it opens. `max` is clamped server-side (Task 3); the type does not pretend to enforce it.

- [ ] **Step 1: Invoke the `proto-change` skill**

It fixes the ORDER that keeps the version, the goldens, the schema and the N/N−1 window consistent. Follow it rather than this task's step order where the two differ.

- [ ] **Step 2: Write the failing golden/catalogue test run**

The repository already fails the gate for a method constant missing from the catalogue. Add the constants and types first, run:
Run: `just t norte-proto`
Expected: FAIL — the catalogue test names `log.tail`/`log.level` as absent, and the golden diff is non-empty.

- [ ] **Step 3: Add the catalogue entries**

Both `Kind::Request`, `Shape::Direct`, with their params/result type paths, in `crates/norte-proto/src/catalog.rs`.

- [ ] **Step 4: Bump `PROTOCOL_VERSION` and document the version**

`PROTOCOL_VERSION` at `crates/norte-proto/src/methods.rs:996` goes to `"0.65.0"`. Add the version's paragraph to the history block in the same file, in the voice of the ones around it: what it adds, and what a peer that cannot serve it does instead.

**Corrected 2026-09-02 (protocol-guardian, task 2).** This step first said "what a 0.64 client/daemon does instead (answers method not found)". That direction does not exist: `version_compatible` accepts only `cn == sn || cn + 1 == sn`, and the daemon refuses `initialize` with `VERSION_MISMATCH` when the client's minor is higher, so a 0.65 client against a 0.64 daemon dies at the handshake. There is **exactly one** degradation branch, and tasks 4, 6 and 7 wire that one: a **same-version** daemon built without the `logging` feature, which has no ring and answers `METHOD_NOT_FOUND`.

- [ ] **Step 5: Regenerate the goldens and READ the diff**

```bash
NORTE_UPDATE_GOLDEN=1 just t norte-proto
git diff -- crates/norte-proto/tests/golden docs/schema
```

Expected: exactly two new catalogue rows, the new types in `methods.json`, the new schema definitions. Anything else in that diff is a bug — do not commit it.

- [ ] **Step 6: Write ADR 0092**

Use the `/adr` project command. Title it for the decision, not the feature. It records: pull-with-cursor over push (the ring already owns a monotonic counter, the daemon keeps no per-client state, a lost notification is a silent hole where a stale cursor is arithmetic); `log.level` as a **method** so `bajo_cota` is enforced by the only code that can raise the ring; and agents refused because the daemon's ring is an existence oracle for paths outside a scoped agent's sandbox — the same leak `read_gate_all` documents.

- [ ] **Step 7: Run the tests**

Run: `just t norte-proto`
Expected: PASS.

- [ ] **Step 8: Dispatch a `protocol-guardian` review**

Give it the commit range, that this adds a read-only pair of methods for the daemon's in-memory log ring, and the two questions you are actually unsure about: whether `level` and `capacity` belong in `LogTailResult` or in a separate call, and whether `cursor: Option<u64>` is the right shape for "give me what you have" versus a sentinel. Apply BLOCKER and MAJOR findings; say which MINORs you skipped and why.

- [ ] **Step 9: Commit**

```bash
git add crates/norte-proto docs/schema docs/adr
git diff --cached --stat   # míralo antes de commitear
git commit  # feat(proto): `log.tail` y `log.level` (0.65.0)
```

---

### Task 3: The daemon mounts a ring and serves it

**Files:**
- Modify: `crates/norte-cli/src/main.rs:640` (the daemon's `logging::init` call)
- Modify: `crates/norte-core/src/daemon/server.rs` — `Shared`, the `dispatch` match (arms next to `methods::CONNECTION_LIST`, `server.rs:2590`)
- Test: `crates/norte-core/tests/daemon.rs`

**Interfaces:**
- Consumes: `LogRing::since` (Task 1), `methods::{LOG_TAIL, LOG_LEVEL, LogTailParams, LogTailResult, LogLevelParams, LogLevelResult, LogLine}` (Task 2).
- Produces: a daemon that answers both methods; `Shared.log_ring: Option<LogRing>` (None when the binary was built without the `logging` feature or the ring failed to mount — then both methods answer `Error::Unsupported`, never an empty success, because an empty log and an absent log must not read alike).

`max` is clamped server-side to 1000; a client asking for more gets 1000 and the cursor tells it there is more.

- [ ] **Step 1: Write the failing tests**

In `crates/norte-core/tests/daemon.rs`, following that file's existing harness for spinning a daemon and calling it:

```rust
/// LA prueba de este trabajo: la cota sigue viva al otro lado del socket.
///
/// `suppaftp` escribe `PASS <contraseña>` en TRACE (#43, regla 10), y el nivel
/// del anillo se sube DESDE la interfaz. Si subir el nivel por `log.level`
/// dejara pasar un target de terceros, una pulsación en un panel pondría una
/// contraseña en pantalla.
#[tokio::test]
async fn subir_el_nivel_por_el_cable_no_levanta_la_cota() {
    let d = daemon_de_prueba().await;
    d.call(methods::LOG_LEVEL, &json!({ "level": "trace" })).await.unwrap();

    tracing::trace!(target: "suppaftp", "PASS secreto-de-verdad");
    tracing::trace!(target: "hyper::proto", "cabecera cruda");
    tracing::trace!(target: "norte_core::connect", "esto sí");

    let r: methods::LogTailResult =
        d.call_as(methods::LOG_TAIL, &json!({ "cursor": null, "max": 500 })).await.unwrap();
    let mensajes: Vec<_> = r.lines.iter().map(|l| l.message.as_str()).collect();
    assert!(mensajes.iter().any(|m| m.contains("esto sí")));
    assert!(!mensajes.iter().any(|m| m.contains("secreto-de-verdad")));
    assert!(!mensajes.iter().any(|m| m.contains("cabecera cruda")));
}

/// Un agente no lee el registro del daemon: lleva rutas, nombres de conexión y
/// actividad de OTRAS sesiones, o sea un oráculo de existencia fuera de su
/// scope. Y se le dice que está vedado, no que está vacío.
#[tokio::test]
async fn un_agente_no_lee_el_registro() {
    let d = daemon_de_prueba_como_agente("a1").await;
    let e = d.call(methods::LOG_TAIL, &json!({ "cursor": null, "max": 10 })).await.unwrap_err();
    assert!(matches!(e, norte_proto::Error::PolicyDenied { .. }), "{e:?}");
    let e = d.call(methods::LOG_LEVEL, &json!({ "level": "debug" })).await.unwrap_err();
    assert!(matches!(e, norte_proto::Error::PolicyDenied { .. }), "{e:?}");
}

/// El cursor sobrevive dos llamadas y no repite ni se salta líneas.
#[tokio::test]
async fn el_cursor_encadena_dos_llamadas() {
    let d = daemon_de_prueba().await;
    tracing::info!(target: "norte_core::prueba", "primera");
    let a: methods::LogTailResult =
        d.call_as(methods::LOG_TAIL, &json!({ "cursor": null, "max": 500 })).await.unwrap();
    tracing::info!(target: "norte_core::prueba", "segunda");
    let b: methods::LogTailResult =
        d.call_as(methods::LOG_TAIL, &json!({ "cursor": a.next, "max": 500 })).await.unwrap();
    assert!(b.lines.iter().any(|l| l.message.contains("segunda")));
    assert!(!b.lines.iter().any(|l| l.message.contains("primera")));
    assert_eq!(a.lost, 0);
}

/// `max` se acota en el servidor: pedir un millón no manda un millón.
#[tokio::test]
async fn el_servidor_acota_max() {
    let d = daemon_de_prueba().await;
    for i in 0..1200 { tracing::info!(target: "norte_core::prueba", "l{i}"); }
    let r: methods::LogTailResult =
        d.call_as(methods::LOG_TAIL, &json!({ "cursor": 0, "max": 100_000 })).await.unwrap();
    assert!(r.lines.len() <= 1000);
}
```

Note for the implementer: the test daemon must have a ring mounted **and** a subscriber routing into it. The existing `crates/norte-ui-host/tests/controller.rs:11193` (`con_lineas`) shows the pattern — a `tracing_subscriber::registry().with(ring_layer(&anillo))` held for the duration. A global subscriber can only be set once per process, so use the same scoped-dispatcher approach rather than `init`.

- [ ] **Step 2: Run the tests and watch them fail**

Run: `just t norte-core`
Expected: FAIL — method not found / no `log_ring` on `Shared`.

- [ ] **Step 3: Mount the ring in the daemon binary**

`crates/norte-cli/src/main.rs:640` uses `norte_core::logging::init`. Switch the **daemon** path to `init_to_file_with_ring(cfg, norte_config::logring::RING_DEFAULT)` and thread the returned ring into the server's `Shared`. Do not change the other subcommands: a one-shot CLI call has nobody to show a ring to and would pay 2000 lines of memory for nothing.

- [ ] **Step 4: Add the two dispatch arms**

Next to `methods::CONNECTION_LIST` in `dispatch`, and **with the actor gate before the params parse**, for the reason that arm already documents: an agent must not be able to tell "forbidden" from "bad params" by fuzzing. `log.level` calls `LogRing::raise_to` and answers with `LogRing::level()` — never a level the client computed. Instrument both with `#[instrument]`.

- [ ] **Step 5: Run the tests and watch them pass**

Run: `just t norte-core`
Expected: PASS.

- [ ] **Step 6: Dispatch a `security-reviewer` review**

The surface is daemon auth plus secret-adjacent logging. Ask it specifically: can any path in these two arms raise the ring above the whitelist, and can a non-`User` actor reach either arm through any other route (batch, notification, agent session upgrade)?

- [ ] **Step 7: `just ci-fast` — the one run for tasks 1-3**

Run: `just ci-fast`
Expected: green. If red, reproduce the single failure with `just t <crate>`, fix it there, and do not re-run the gate to check.

- [ ] **Step 8: Commit**

```bash
git add crates/norte-cli crates/norte-core
git diff --cached --stat
git commit  # feat(core,cli): el daemon monta su anillo y lo sirve con cota
```

---

### Task 4: The SDK asks, and degrades on an older daemon

**Files:**
- Modify: `crates/norte-client/src/remote/mod.rs`
- Test: `crates/norte-client/tests/` (the existing remote-call test module)

**Interfaces:**
- Consumes: the protocol types from Task 2.
- Produces:

```rust
pub async fn log_tail(&self, cursor: Option<u64>, max: u32) -> Result<methods::LogTailResult, Error>;
pub async fn log_level(&self, level: &str) -> Result<String, Error>;
```

Both go through `call_no_method_is_unsupported` — the helper `session_get` already uses (`remote/mod.rs:1909`) — so a daemon that answers `METHOD_NOT_FOUND` surfaces as `Error::Unsupported` rather than a raw protocol error. That is the value Task 6 turns into a sentence on screen.

**The reachable case is a same-version daemon built without the `logging` feature**, not an older daemon: a 0.65 client never completes `initialize` against a 0.64 daemon (`VERSION_MISMATCH`), so it never gets to send `log.tail`. Do not write a version comparison here — the answer to the method is the only signal, and it is enough.

- [ ] **Step 1: Write the failing test**

Against the crate's fake/loopback daemon harness: a server that answers "method not found" makes `log_tail` return `Error::Unsupported`, and a server that answers a normal result returns the parsed lines.

- [ ] **Step 2: Run it and watch it fail**

Run: `just t norte-client`
Expected: FAIL — no method `log_tail`.

- [ ] **Step 3: Implement both wrappers**

Rustdoc with `# Errors`, as every public item in this crate has. Keep `norte-client`'s dependency boundary: `norte-proto` and runtime crates only — `tests/dependency_boundary.rs` fails the build otherwise, so do **not** reach for `norte-config`'s `LogLine` here.

- [ ] **Step 4: Run it and watch it pass**

Run: `just t norte-client`
Expected: PASS.

- [ ] **Step 5: Doc checks**

Run: `cargo test -p norte-client --doc` and `cargo doc -p norte-client --no-deps`
Expected: both pass.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-client
git commit  # feat(client): `log_tail` y `log_level`, y un daemon viejo se dice
```

---

### Task 5: The panel learns it has two sources

**Files:**
- Modify: `crates/norte-frontend/src/logpanel.rs`
- Test: same file (`mod tests`)

**Interfaces:**
- Consumes: `norte_config::logline::{LogLine, LogLevel}`.
- Produces:

```rust
/// De dónde salen las líneas que el panel enseña.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogSource {
    /// Solo este proceso.
    Window,
    /// Solo el daemon.
    Daemon,
    /// Los dos, mezclados por marca de tiempo.
    #[default]
    Both,
}

impl LogPanel {
    pub fn source(&self) -> LogSource;
    pub fn set_source(&mut self, s: LogSource);
    pub fn cycle_source(&mut self);
}

/// Mezcla dos listas ya ordenadas por `epoch_ms`, marcando el origen de cada
/// línea. Estable: a igual marca, primero la local.
pub fn merge<'a>(local: &'a [LogLine], remote: &'a [LogLine], s: LogSource)
    -> Vec<(&'a LogLine, LogSource)>;
```

`merge` returns borrowed lines because the ring's `snapshot` already cloned once and the panel paints at most a screenful — a second clone of 2000 lines per frame is the waste `panel_de_registro` was written to avoid.

- [ ] **Step 1: Write the failing tests**

```rust
/// La mezcla respeta el reloj, y a igual marca no baila: primero la local.
#[test]
fn la_mezcla_ordena_por_marca_y_es_estable() {
    let local = vec![l(10, "ventana-a"), l(30, "ventana-b")];
    let remoto = vec![l(10, "daemon-a"), l(20, "daemon-b")];
    let m = merge(&local, &remoto, LogSource::Both);
    let ms: Vec<_> = m.iter().map(|(l, _)| l.message.as_str()).collect();
    assert_eq!(ms, ["ventana-a", "daemon-a", "daemon-b", "ventana-b"]);
    assert_eq!(m[1].1, LogSource::Daemon);
}

/// Elegir una fuente NO mezcla: enseña esa y nada más.
#[test]
fn una_fuente_sola_no_trae_la_otra() {
    let local = vec![l(10, "ventana")];
    let remoto = vec![l(20, "daemon")];
    assert_eq!(merge(&local, &remoto, LogSource::Window).len(), 1);
    assert_eq!(merge(&local, &remoto, LogSource::Daemon)[0].0.message, "daemon");
}

/// El ciclo recorre las tres y vuelve: es UN mando, no tres.
#[test]
fn el_ciclo_de_fuente_da_la_vuelta() {
    let mut p = LogPanel::default();
    assert_eq!(p.source(), LogSource::Both);
    p.cycle_source();
    p.cycle_source();
    p.cycle_source();
    assert_eq!(p.source(), LogSource::Both);
}

/// El filtro de nivel y el de texto siguen aplicándose DESPUÉS de mezclar: una
/// línea del daemon que no pasa el filtro no se cuela por venir de fuera.
#[test]
fn el_filtro_manda_tambien_sobre_lo_remoto() {
    let mut p = LogPanel::default();
    p.show_level(LogLevel::Error);
    let remoto = vec![LogLine { epoch_ms: 1, level: LogLevel::Debug,
        target: "norte_core".into(), message: "ruido".into() }];
    let m = merge(&[], &remoto, LogSource::Both);
    assert!(!p.matches(m[0].0));
}
```

with a helper `fn l(ms: i64, msg: &str) -> LogLine` in the test module.

- [ ] **Step 2: Run and watch them fail**

Run: `just t norte-frontend`
Expected: FAIL — `LogSource` not found.

- [ ] **Step 3: Implement `LogSource`, the accessors and `merge`**

- [ ] **Step 4: Run and watch them pass**

Run: `just t norte-frontend`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-frontend
git commit  # feat(frontend): el panel de registro tiene fuente, y sabe mezclar
```

---

### Task 6: The window reads the daemon

**Files:**
- Modify: `crates/norte-ui-host/src/backend.rs` (trait + both implementations), `crates/norte-ui-host/src/controller/logpanel.rs`, `crates/norte-ui-host/src/dto.rs`, `crates/norte-ui-host/src/action.rs`, `crates/norte-ui-host/src/bridge.rs`
- Modify: `crates/norte-gui-tauri/ui/src/{types.ts,render.ts,keys.ts}`
- Modify: `crates/norte-i18n/i18n/en.ftl`, `crates/norte-i18n/i18n/es.ftl`
- Modify (regenerated): `crates/norte-ui-host/tests/golden/*.json`
- Test: `crates/norte-ui-host/tests/controller.rs`, `crates/norte-gui-tauri/ui/tests/render.test.ts`

**Interfaces:**
- Consumes: `norte_client` `log_tail`/`log_level` (Task 4), `norte_frontend::logpanel::{LogSource, merge}` (Task 5).
- Produces: on the `Backend` trait,

```rust
fn log_tail(&self, cursor: Option<u64>, max: u32)
    -> BoxFuture<'static, Result<methods::LogTailResult, Error>>;
fn log_level(&self, level: String) -> BoxFuture<'static, Result<String, Error>>;
```

The **embedded** backend answers `Err(Error::Unsupported)` for both, and that is correct rather than lazy: embedded means one process and one ring, so there is no second source, and the selector must not appear. Bridge version goes to **48**.

`LogSlotView` (in `dto.rs`) gains `source_mode: String` (`"window"|"daemon"|"both"`), `sources_available: bool`, and `source_note: String` — the sentence shown when the daemon cannot serve its log.

- [ ] **Step 1: Write the failing controller tests**

In `crates/norte-ui-host/tests/controller.rs`, using its existing `host_con_registro()` harness (line ~11200) and its fake backend (`tests/backend_falso/mod.rs`):

```rust
/// Con backend embebido no hay dos anillos, así que no hay selector que
/// enseñar: el panel se queda exactamente como en #326.
#[tokio::test]
async fn embebido_no_ofrece_selector_de_fuente() {
    let (host, _anillo) = host_con_registro().await;
    let v = abrir_registro(&host).await;
    assert!(!v.sources_available);
    assert_eq!(v.source_mode, "window");
}

/// Con daemon, el panel trae las líneas de los DOS y cada una dice de dónde es.
#[tokio::test]
async fn con_daemon_se_mezclan_las_dos_fuentes() {
    let backend = BackendFalso::nuevo();
    backend.responde_log_tail(vec![linea_wire(20, "info", "norte_core", "del daemon")], 1);
    let (host, anillo) = host_con_backend_y_registro(backend).await;
    con_lineas(&anillo, || tracing::info!("de la ventana"));
    let v = abrir_registro_y_sondear(&host).await;
    assert!(v.sources_available);
    assert_eq!(v.source_mode, "both");
    let textos: Vec<_> = v.lines.iter().map(|l| l.text.as_str()).collect();
    assert!(textos.iter().any(|t| t.contains("del daemon")));
    assert!(textos.iter().any(|t| t.contains("de la ventana")));
}

/// Un daemon que no sabe servir su registro NO deja el panel mudo: vuelve al
/// anillo local y lo DICE. Es la mitad que #326 ya resolvió, aplicada al único
/// caso alcanzable: un daemon de la MISMA versión compilado sin la feature
/// `logging`. Uno más viejo no llega aquí — muere en el `initialize`.
#[tokio::test]
async fn un_daemon_sin_registro_se_dice_en_el_panel() {
    let backend = BackendFalso::nuevo();
    backend.log_tail_no_soportado();
    let (host, _anillo) = host_con_backend_y_registro(backend).await;
    let v = abrir_registro_y_sondear(&host).await;
    assert_eq!(v.source_mode, "window");
    assert!(!v.source_note.is_empty(), "tiene que decir por qué");
}

/// Subir el nivel con el daemon como fuente se lo pide AL DAEMON: el cliente
/// no aplica niveles, y el nivel que se enseña es el que el daemon contestó.
#[tokio::test]
async fn subir_el_nivel_con_fuente_daemon_va_al_daemon() {
    let backend = BackendFalso::nuevo();
    backend.log_level_contesta("debug");
    let (host, _anillo) = host_con_backend_y_registro(backend).await;
    poner_fuente(&host, "daemon").await;
    accion(&host, Action::LogLevel { level: "trace".into() }).await;
    assert_eq!(backend.log_level_pedidos(), vec!["trace"]);
    let v = foto_registro(&host).await;
    assert_eq!(v.level, "debug", "manda lo que contestó el daemon");
}

/// El sondeo encadena el cursor: la segunda vuelta pide desde donde acabó la
/// primera y no repite líneas.
#[tokio::test]
async fn el_sondeo_encadena_el_cursor() {
    let backend = BackendFalso::nuevo();
    backend.responde_log_tail(vec![linea_wire(10, "info", "norte_core", "a")], 1);
    let (host, _anillo) = host_con_backend_y_registro(backend).await;
    abrir_registro_y_sondear(&host).await;
    backend.responde_log_tail(vec![linea_wire(20, "info", "norte_core", "b")], 2);
    sondear(&host).await;
    assert_eq!(backend.cursores_pedidos(), vec![None, Some(1)]);
    let v = foto_registro(&host).await;
    let textos: Vec<_> = v.lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(textos.iter().filter(|t| t.contains('a')).count(), 1);
}
```

Write the missing harness helpers (`host_con_backend_y_registro`, `abrir_registro_y_sondear`, `sondear`, `poner_fuente`, `foto_registro`, `linea_wire`) alongside, following the file's existing deterministic-wait helpers (`hasta`, `asentar`, `foto_hasta`) — **no `sleep`**, the ui-host suite was converted away from them deliberately.

- [ ] **Step 2: Run and watch them fail**

Run: `just t norte-ui-host`
Expected: FAIL — no `log_tail` on `Backend`, no `sources_available` on `LogSlotView`.

- [ ] **Step 3: Extend the `Backend` trait and both implementations**

Remote delegates to the SDK; embedded answers `Err(Error::Unsupported)`.

- [ ] **Step 4: Wire the poll**

`tic_de_registro` (`controller/logpanel.rs:291`) already re-arms itself every 500 ms while the panel is open, guarded by `log_epoca`. Hang the remote fetch off the same tick — it must not add a second timer, and it must stop when the panel closes. The fetch is async and its answer comes back as a `Mensaje`, like every other backend answer in this controller; the actor stays the only writer. Keep the remote cursor and the remote lines on `Estado`, next to `log_visto`.

- [ ] **Step 5: Project the new fields and bump the bridge**

`LogSlotView` gains the three fields; `bridge.rs` goes to 48; `action.rs` gains the source-cycle action. Update `crates/norte-gui-tauri/ui/src/types.ts` to match — **the renderer's `BRIDGE_VERSION` must move with it**, and note `just ci-fast` does NOT run `gui-ci`, so a mismatch here goes unnoticed until someone runs it by hand.

- [ ] **Step 6: The two locales**

Add `log-source-window`, `log-source-daemon`, `log-source-both` and `log-source-unsupported` to **both** `en.ftl` and `es.ftl`. A string in one locale only is a hole that shows up as a raw key on someone else's screen.

- [ ] **Step 7: Renderer + its tests**

The chip becomes a selector in `render.ts`; add a case to `ui/tests/render.test.ts` asserting it is absent when `sources_available` is false and that clicking cycles.

- [ ] **Step 8: Regenerate the ui-host goldens and read the diff**

```bash
NORTE_UPDATE_GOLDEN=1 just t norte-ui-host
git diff -- crates/norte-ui-host/tests/golden
```

- [ ] **Step 9: Run everything for this task**

Run: `just t norte-ui-host` and `just gui-ci`
Expected: both green.

- [ ] **Step 10: `just ci-fast` — the one run for tasks 4-6**

Run: `just ci-fast`
Expected: green.

- [ ] **Step 11: Dispatch a `rust-reviewer` review**

Substantial Rust diff across a trait boundary and an actor. Ask it about the one thing that is genuinely uncertain: whether the remote fetch can outlive its panel epoch and write stale lines into a reopened panel.

- [ ] **Step 12: Commit**

```bash
git add crates/norte-ui-host crates/norte-gui-tauri crates/norte-i18n
git diff --cached --stat
git commit  # feat(ui-host,gui): el panel de registro lee también el daemon (puente 48)
```

---

### Task 7: `ntc --socket` gets the same thing

**Files:**
- Modify: `crates/norte-tui/src/logview.rs`, `crates/norte-tui/src/app.rs` (the `log_ring` field, ~line 553), `crates/norte-tui/src/ui/panels.rs` (~line 906, the dropped note)
- Modify: `crates/norte-tui/src/main.rs` (the keymap action for cycling the source, if the TUI binds one)
- Test: `crates/norte-tui/src/logview.rs` (`mod tests`)

**Interfaces:**
- Consumes: `norte_frontend::logpanel::{LogSource, merge}` (Task 5) and the SDK (Task 4). Nothing new is produced for later tasks.

The TUI repaints per frame, so it needs no timer: it fetches on the same cadence it already refreshes, and holds the remote lines and cursor next to its ring.

**Key rule from CLAUDE.md:** if this adds a binding, it lands in **all seven presets** (`orthodox`, `vim`, `cua`, `krusader`, `far`, `norton`, `total-commander`) or each preset's header says why not. Check the chord is deliverable (`shift+<single char>` is a dead key) and that it is bound on the right screen. If it adds a command: catalogue entry, `help-cmd-*` in both locales, the help topic, and the `norte-cli` golden (`NORTE_UPDATE_GOLDEN=1`).

- [ ] **Step 1: Write the failing test**

```rust
/// Con daemon aparte, la vista de registro de la TUI enseña las dos fuentes.
/// Es el mismo agujero que la ventana y la misma respuesta (ADR 0077): si solo
/// se arregla en un frontend, divergen en silencio.
#[test]
fn la_vista_mezcla_ventana_y_daemon() {
    let anillo = LogRing::new(10);
    anillo.push(/* línea local, epoch_ms 10 */);
    let remotas = vec![/* línea del daemon, epoch_ms 20 */];
    let v = LogView::nueva_con(anillo, remotas, LogSource::Both);
    let filas = v.filas(80, 10);
    assert!(filas.iter().any(|f| f.contains("de la ventana")));
    assert!(filas.iter().any(|f| f.contains("del daemon")));
}
```

Adjust the constructor name to whatever `logview.rs` actually exposes — the point of the test is the merge reaching the rendered rows, not the shape of the ctor.

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-tui`
Expected: FAIL.

- [ ] **Step 3: Implement**

Fetch through the SDK only when the backend is remote; on `Error::Unsupported` fall back to the local ring and say so in the panel's header line, same sentence key as the window.

- [ ] **Step 4: Run and watch it pass**

Run: `just t norte-tui`
Expected: PASS.

- [ ] **Step 5: Presets, if a binding was added**

All seven, or a written reason in that preset's divergences block. Then regenerate the `norte-cli` golden.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-tui
git commit  # feat(tui): `ntc --socket` lee el registro del daemon
```

---

### Task 8: Close the branch

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `crates/norte-help/topics/**` if the help topic for the log panel needs the new source selector (then regenerate the `norte-cli` golden)

- [ ] **Step 1: CHANGELOG entry**

Under `## [Unreleased]` → `### Added`, in the voice of the #326 and #327 entries above it: what the reader gets, protocol 0.65.0 and bridge 48, that the cap is enforced in the daemon, that agents are refused and why, and that an older daemon degrades with a sentence rather than silently.

- [ ] **Step 2: Docs check**

Run: `cargo doc --workspace --exclude norte-gui-tauri --no-deps`
Expected: pass — intra-doc links are denied here and neither `just c` nor `just t` checks them.

- [ ] **Step 3: `just ci` — the one full run of the branch**

Run: `just ci` (foreground, one recipe at a time if it does not fit: `lint`, `test`, `docs`, `cov` — never through `| tail`, a killed pipe reports nothing)
Expected: green. `cov` can only move if proto/vfs/core coverage changed; it did (new daemon arms), so watch the 85% floor.

- [ ] **Step 4: Whole-branch review**

This branch touches the wire and a security cap, which is exactly the case CLAUDE.md keeps a second external pass for. Dispatch `protocol-guardian` and `security-reviewer` over the full range `main..HEAD`, with the questions: is the N/N−1 window honest end to end, and is there any route by which a non-`User` actor or a raised level reaches a third-party target's TRACE.

- [ ] **Step 5: Push, and close the issue by hand**

```bash
git push -u origin feat/daemon-log-over-the-wire
gh issue close 328 -c "..."   # el commit en español NO la cierra
```

---

## Self-review

**Spec coverage:** ring `since` → T1; the two methods, version, ADR → T2; daemon ring + cap + agent gate → T3; SDK + N−1 → T4; source selector + merge → T5; window (poll, projection, renderer, i18n) → T6; `ntc --socket` → T7; changelog, gate, close-out → T8. Every "Testing" item in the spec has a step: cap under the socket (T3 step 1), cursor gap (T1 step 1 + T3), agent refused (T3), N−1 (T4 + T6), stable merge (T5), goldens (T2 + T6).

**Types:** `Tail{lines,next,lost}` (T1) is the daemon's input to `LogTailResult{lines,next,lost,level,capacity}` (T2), which `Backend::log_tail` returns unchanged (T6). `LogSource`/`merge` are defined once in T5 and used by T6 and T7. `LogSlotView` gains `source_mode`/`sources_available`/`source_note`, named identically in the tests and the renderer.

**Out of scope, stated so nobody adds it:** server-side filtering, reading the log file remotely, persisting the ring across daemon restarts, any agent or plugin access.
