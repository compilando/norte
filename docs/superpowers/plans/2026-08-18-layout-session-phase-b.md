# Layout phase B — the UI session, implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** The screen you left survives the daemon: layouts, paths, cursors,
history, sort and columns are kept by the core, handed back on reconnect, and
written to disk across a handover.

**Architecture:** The session crosses the wire as an **opaque document**
(`version`, `revision`, `body`), because `Node`, `SortSpec` and `ColumnId` live
in `norte-frontend`, which depends on `norte-proto` and not the other way
round — and because ADR 0058 already decided the core keeps the screen and does
not read it. The core stores it, versions it with `revision`, refuses a body
over 1 MiB, hands it back, and writes it to `<state_dir>/norte/session.json` by
temporary file and rename. What is inside the body is `norte-frontend`'s, and
its v1 schema is `SessionBody`. Concurrency is one number: a `put` with a stale
`revision` is `Conflict` and the client re-reads.

**Tech stack:** Rust, `serde`/`serde_json`, tokio, JSON-RPC over a unix socket,
nextest, `flock`/`LockFileEx` through the pattern `norte-config` already uses.

**Spec:** `docs/superpowers/specs/2026-08-18-layout-presets-and-ui-session-design.md`,
phase B (from «Phase B: L2, the UI session» onwards). Phase A is merged
(`355ae2d1`).

## Global constraints

- **Protocol 0.48.0**, additive. The bump moves the compatibility window; it
  does not widen it. `protocol-guardian` review is **mandatory** before merge.
- **`path: VPath`, never `String`.** The spec says it twice on purpose: every
  other field of `SlotState` is a number or an enum, and typing the one that is
  not as a `String` loses a filename no frontend test would notice.
- **Caps are the client's**, and they are in the specification because a cap
  discovered later is a migration: history **64** entries per slot per
  direction, orphan slots **128**, age sweep **30 days**. All three applied
  when WRITING.
- **Body ceiling 1 MiB**, enforced by the core, refused with a typed error and
  never truncated.
- **The core never parses the body.** It may serialise it to measure it; it may
  not look inside.
- **Marks do not travel.** A selection is the state of an operation, not of a
  session.
- **Never a blank screen.** Every failure path falls back to `[ui] layout`, or
  `orthodox`, with a diagnostic.
- User-facing strings go through Fluent in both locales (`en.ftl`, `es.ftl`).

## Before you start

Read, in this order:

- The spec's phase B, whole. It is short and every decision below argues from it.
- `crates/norte-core/src/daemon/server.rs:161` (`struct Shared`) and
  `crates/norte-core/src/daemon/server.rs:2427` (`handle_plugin_list`) — the
  shape of a handler and where per-daemon state lives.
- `crates/norte-config/src/load.rs:236-330` — `lock_config_file` and
  `write_config_file`: the advisory lock on a dedicated `<file>.lock` sibling
  and the temp-file-and-rename write. **The session copies this pattern; it
  does not invent a second one.**
- `crates/norte-frontend/src/layout/config.rs` — `to_toml`/`load`/`list`: how a
  `Node` is already serialised, and why the session is the same serde shape
  with a different writer.
- `crates/norte-tui/src/nav.rs:35` (`History`) and
  `crates/norte-frontend/src/pane.rs:225-330` — where `back`/`forward`,
  `cursor` and `show_hidden` actually live today.

## The gate

`just t <crate>` in the RED→GREEN loop, `just c` when you touch lint surface.
**ONE** `just ci-fast` after task 5. **ONE** `just ci` before the merge — this
branch touches `norte-proto` and `norte-core`, so `cov` CAN move and is not
optional this time.

A pre-commit hook runs `just ci-fast` on every commit, so each commit is
already a partial gate: do not add runs of your own on top of it.

`just t` does not run doctests and `just c` does not check intra-doc links.
Tasks 1, 2 and 6 add documented public items in crates that deny both:
`cargo test -p <crate> --doc` and `cargo doc -p <crate> --no-deps` after each.

## File structure

| file | responsibility |
| --- | --- |
| `crates/norte-proto/src/methods.rs` | **modify** — `SESSION_GET`/`SESSION_PUT`, params/results, `SESSION_BODY_MAX`, version 0.48.0 |
| `crates/norte-proto/tests/golden/types/session.json` | **create** — the golden of the three types |
| `crates/norte-proto/tests/golden_types.rs` | **modify** — the golden test |
| `crates/norte-proto/tests/types.rs` | **modify** — window test moves to 0.48/0.47 |
| `crates/norte-core/src/session.rs` | **create** — `SessionStore`: revision, ceiling, ownership |
| `crates/norte-core/src/lib.rs` | **modify** — `pub mod session;` |
| `crates/norte-core/src/session/disk.rs` | **create** — load, atomic write, the lock |
| `crates/norte-core/src/daemon/server.rs` | **modify** — two handlers, `Shared.session`, the write triggers |
| `crates/norte-core/tests/daemon.rs` | **modify** — RPC round trip, conflict, ceiling, ownership, handover |
| `crates/norte-frontend/src/session.rs` | **create** — `SessionBody`, `SlotState`, caps, sweep |
| `crates/norte-frontend/src/lib.rs` | **modify** — `pub mod session;` |
| `crates/norte-tui/src/app.rs` | **modify** — capture into `SessionBody`, apply from it |
| `crates/norte-tui/src/main.rs` | **modify** — read at start, coalesced push, `Conflict`/`TooLarge` paths |
| `crates/norte-tui/tests/session.rs` | **create** — capture/apply tests |
| `crates/norte-i18n/i18n/{en,es}.ftl` | **modify** — the detached line and the two failures |
| `docs/adr/0059-*.md` | **create** — the session on the wire and on disk |
| `CHANGELOG.md` | **modify** — what a user gets |

---

### Task 1: the wire — `session.get` and `session.put` (0.48.0)

The body is `serde_json::Value` and **not** `RawValue`: the crate's `schema`
feature derives `JsonSchema` for every wire type, `RawValue` has no such impl,
and the cost of `Value` is one re-serialisation of a kilobyte blob to measure
it. Opaque here means "the core never interprets it", not "the core never
touches its bytes".

**Files:**
- Modify: `crates/norte-proto/src/methods.rs:627` (version) and the method block
- Test: `crates/norte-proto/tests/types.rs`, `crates/norte-proto/tests/golden_types.rs`
- Create: `crates/norte-proto/tests/golden/types/session.json`

**Interfaces:**
- Produces: `methods::SESSION_GET`, `methods::SESSION_PUT`,
  `methods::SESSION_BODY_MAX: usize`, `methods::Session { version: u32,
  revision: u64, body: serde_json::Value }` (with `Default`),
  `methods::SessionGetResult { session: Session, owner: bool }`,
  `methods::SessionPutParams { version: u32, revision: u64, body:
  serde_json::Value }`, `methods::SessionPutResult { revision: u64 }`.

- [ ] **Step 1: Write the failing tests**

In `crates/norte-proto/tests/types.rs`:

```rust
/// L2: la sesión viaja como documento OPACO. El round trip conserva el body
/// entero —incluido un kind que ningún binario declara— porque nadie lo
/// interpreta por el camino.
#[test]
fn session_round_trips_an_opaque_body() {
    use norte_proto::methods::Session;
    let body = serde_json::json!({
        "version": 1,
        "layouts": { "default": { "kind": "kind-que-nadie-declara", "params": { "x": 1 } } },
        "slots": {}
    });
    let s = Session { version: 1, revision: 7, body: body.clone() };
    let ida = serde_json::to_string(&s).expect("serializa");
    let vuelta: Session = serde_json::from_str(&ida).expect("deserializa");
    assert_eq!(vuelta.revision, 7);
    assert_eq!(vuelta.body, body, "el body vuelve entero, sin normalizar");
}

/// El tope del body es del PROTOCOLO, no una constante suelta del daemon: el
/// cliente necesita el mismo número para decidir qué tirar antes de
/// reintentar.
#[test]
fn session_body_max_is_one_mebibyte() {
    assert_eq!(norte_proto::methods::SESSION_BODY_MAX, 1024 * 1024);
}

/// `owner` viaja en el GET: el segundo cliente recibe una copia y tiene que
/// saber que lo es antes de intentar escribir.
#[test]
fn session_get_result_says_who_owns_it() {
    use norte_proto::methods::{Session, SessionGetResult};
    let r = SessionGetResult {
        session: Session { version: 1, revision: 0, body: serde_json::json!({}) },
        owner: false,
    };
    let v = serde_json::to_value(&r).expect("serializa");
    assert_eq!(v["owner"], serde_json::json!(false));
}

/// Una sesión nunca escrita es el Default: revisión 0 y sin esquema. El store
/// del core lo construye así, y el cliente distingue «no hay» de «falló».
#[test]
fn session_default_is_revision_zero() {
    let s = norte_proto::methods::Session::default();
    assert_eq!(s.revision, 0);
    assert_eq!(s.version, 0);
}
```

And replace the body of `version_ventana_actual` in the same file with the
shifted window:

```rust
    // 0.48.0 (L2): acepta 0.48.x (N) y 0.47.x (N-1), rechaza 0.46.x (N-2). El
    // bump es ADITIVO (dos métodos y cuatro tipos nuevos; ningún tipo
    // existente cambia de forma), y aun así la ventana se DESPLAZA: un cliente
    // 0.46 no conoce `session.*`, arranca sin sesión y jamás la escribe, que
    // es degradar en silencio justo lo que esta fase existe para conservar.
    assert!(version_compatible("0.48.0"));
    assert!(version_compatible("0.48.9"));
    assert!(version_compatible("0.47.3"));
    assert!(!version_compatible("0.46.9"));
    assert_eq!(PROTOCOL_VERSION, "0.48.0");
```

- [ ] **Step 2: Run them and watch them fail**

Run: `just t norte-proto`
Expected: FAIL to compile — `methods::Session` does not exist — plus
`version_ventana_actual` failing on `PROTOCOL_VERSION`.

- [ ] **Step 3: Implement**

In `crates/norte-proto/src/methods.rs`, bump the constant:

```rust
pub const PROTOCOL_VERSION: &str = "0.48.0";
```

and add, beside the other method constants:

```rust
/// `session.get` — la sesión de UI del daemon (L2, 0.48.0): la disposición y
/// el estado por hueco que el cliente dejó, para que un relevo del daemon
/// (ADR 0055) no cueste la pantalla.
///
/// El resultado dice además si ESTA conexión es la DUEÑA. La primera conexión
/// humana que pregunta se la queda; las siguientes reciben una COPIA y corren
/// sueltas —misma pantalla, mismas rutas, y a partir de ahí divergen sin
/// escribir—. Abrir un segundo terminal da lo que el lector esperaba y nunca
/// hay dos escritores sobre un estado.
///
/// SOLO conexiones humanas: una sesión de agente no tiene pantalla que
/// guardar. Un agente recibe `INVALID_REQUEST`.
pub const SESSION_GET: &str = "session.get";

/// `session.put` — reemplaza la sesión ENTERA (L2, 0.48.0).
///
/// Viaja el blob completo y el cliente coalesce: el cursor se mueve en cada
/// flecha, y una familia de métodos por campo serían quince métodos, quince
/// goldens y un motor de fusión que nadie pidió.
///
/// `revision` es toda la historia de concurrencia: un `put` con una revisión
/// rancia se rechaza con [`crate::Error::Conflict`] y el cliente re-lee. No
/// está para editores simultáneos —no los hay— sino para el cliente que
/// reconecta tras un relevo con estado de antes.
pub const SESSION_PUT: &str = "session.put";

/// Tope del `body` de una sesión, en bytes serializados: 1 MiB.
///
/// Lo comprueba el core, que es lo ÚNICO que puede comprobar honestamente de
/// un documento que no lee. Por encima, [`crate::Error::LimitExceeded`] y la
/// sesión almacenada se queda como estaba: jamás se trunca un documento cuyo
/// esquema no se conoce.
pub const SESSION_BODY_MAX: usize = 1024 * 1024;

/// La sesión de UI tal y como cruza el wire (L2).
///
/// `body` es OPACO para el core: `Node`, `SortSpec` y `ColumnId` viven en
/// `norte-frontend`, que depende de este crate y no al revés, y espejarlos
/// aquí duplicaría cuatro tipos a través de una arista de dependencias y
/// convertiría cada campo nuevo de UI en un cambio de wire con su bump y su
/// golden. Añadir un campo al cuerpo es un bump de `version` DENTRO del
/// cuerpo, en el crate que le da significado.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Session {
    /// Esquema de `body`, propiedad de los frontends. 1 en esta versión; 0 en
    /// una sesión que nadie ha escrito todavía.
    pub version: u32,
    /// La sube el core en cada `put` aceptado. 0 = sesión nunca escrita.
    pub revision: u64,
    /// El documento. El core lo guarda, lo versiona y lo devuelve; no lo lee.
    pub body: serde_json::Value,
}

/// Resultado de [`SESSION_GET`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SessionGetResult {
    /// La sesión almacenada, o una vacía con `revision: 0`.
    pub session: Session,
    /// `true` si esta conexión es la dueña y sus `put` se aceptan.
    pub owner: bool,
}

/// Parámetros de [`SESSION_PUT`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SessionPutParams {
    /// Esquema de `body` que escribe este cliente.
    pub version: u32,
    /// La revisión que el cliente cree vigente. Rancia = `Conflict`.
    pub revision: u64,
    /// El documento entero.
    pub body: serde_json::Value,
}

/// Resultado de [`SESSION_PUT`]: la revisión NUEVA.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SessionPutResult {
    /// Revisión resultante; el cliente la guarda para su siguiente `put`.
    pub revision: u64,
}
```

- [ ] **Step 4: The golden**

Add to `crates/norte-proto/tests/golden_types.rs`, in the shape that file
already uses (read a neighbouring golden first — the helper name and the
`NORTE_UPDATE_GOLDEN` flow are established there):

```rust
/// L2: la forma en el wire de la sesión. El golden fija que `body` viaja tal
/// cual —un objeto arbitrario, sin envolver ni re-serializar a string— y que
/// `owner` va en el resultado del GET y no dentro de la sesión.
#[test]
fn golden_session() {
    let s = norte_proto::methods::Session {
        version: 1,
        revision: 3,
        body: serde_json::json!({ "slots": { "1": { "cursor": 12 } } }),
    };
    let g = norte_proto::methods::SessionGetResult { session: s.clone(), owner: true };
    let p = norte_proto::methods::SessionPutParams {
        version: 1,
        revision: 3,
        body: s.body.clone(),
    };
    let r = norte_proto::methods::SessionPutResult { revision: 4 };
    check_golden(
        "session",
        &serde_json::json!({ "get": g, "put_params": p, "put_result": r }),
    );
}
```

Generate it with `NORTE_UPDATE_GOLDEN=1 cargo nextest run -p norte-proto`, then
**read `crates/norte-proto/tests/golden/types/session.json`** — it is the wire
contract, and this is the only moment anybody looks at it.

- [ ] **Step 5: Run the tests**

Run: `just t norte-proto`
Expected: PASS, including the schema test (`schema.rs` asserts every type with
a schema is in the published artefact — if it fails, regenerate the artefact
the way that test says).

Run: `cargo test -p norte-proto --doc` and `cargo doc -p norte-proto --no-deps`
Expected: PASS, no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-proto
git commit -m "feat(proto): a UI session on the wire, as a document the core does not read (0.48.0)"
```

---

### Task 2: the store — revision, ceiling, ownership

Pure state, no I/O and no RPC: the piece every later task leans on, and the one
whose rules are cheapest to pin here.

**Files:**
- Create: `crates/norte-core/src/session.rs`
- Modify: `crates/norte-core/src/lib.rs`
- Test: inside `session.rs`

**Interfaces:**
- Consumes: `norte_proto::methods::{Session, SESSION_BODY_MAX}`.
- Produces: `session::SessionStore::new(Session) -> Self`,
  `SessionStore::default()`, `SessionStore::get(&self) -> Session`,
  `SessionStore::put(&self, version: u32, revision: u64, body: serde_json::Value)
  -> Result<u64, session::PutError>`, `SessionStore::claim(&self, conn: u64) ->
  bool`, `SessionStore::release(&self, conn: u64)`, `SessionStore::owner(&self)
  -> Option<u64>`, `SessionStore::dirty(&self) -> bool`,
  `SessionStore::take_dirty(&self) -> Option<Session>`,
  `enum PutError { Conflict { current: u64 }, TooLarge { bytes: usize },
  NotOwner }`.

- [ ] **Step 1: Write the failing tests**

Create `crates/norte-core/src/session.rs` with this test module (the tests
first; the module doc and the type come in step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn body(n: usize) -> serde_json::Value {
        serde_json::json!({ "relleno": "x".repeat(n) })
    }

    /// Una sesión nunca escrita es revisión 0 con un body vacío.
    #[test]
    fn una_sesion_nueva_es_revision_cero() {
        let s = SessionStore::default();
        let g = s.get();
        assert_eq!(g.revision, 0);
        assert_eq!(g.version, 0, "sin esquema hasta que alguien escriba uno");
    }

    /// Cada `put` aceptado sube la revisión, y la que devuelve es la que el
    /// cliente tiene que traer la próxima vez.
    #[test]
    fn cada_put_sube_la_revision() {
        let s = SessionStore::default();
        assert!(s.claim(1));
        assert_eq!(s.put(1, 0, body(1)).expect("primer put"), 1);
        assert_eq!(s.put(1, 1, body(1)).expect("segundo put"), 2);
        assert_eq!(s.get().revision, 2);
    }

    /// Una revisión rancia es `Conflict` CON la vigente: el cliente re-lee sin
    /// tener que preguntar otra vez para saber contra qué.
    #[test]
    fn una_revision_rancia_es_conflicto_y_no_escribe() {
        let s = SessionStore::default();
        assert!(s.claim(1));
        s.put(1, 0, body(1)).expect("primer put");
        let e = s.put(1, 0, body(2)).expect_err("rancia");
        assert!(matches!(e, PutError::Conflict { current: 1 }), "{e:?}");
        assert_eq!(s.get().body, body(1), "lo almacenado no se toca");
    }

    /// Por encima del tope: `TooLarge`, y lo almacenado SIGUE EN PIE. Truncar
    /// un documento cuyo esquema no se conoce es peor que rechazarlo.
    #[test]
    fn por_encima_del_tope_no_se_trunca_se_rechaza() {
        let s = SessionStore::default();
        assert!(s.claim(1));
        s.put(1, 0, body(10)).expect("cabe");
        let e = s
            .put(1, 1, body(norte_proto::methods::SESSION_BODY_MAX + 1))
            .expect_err("no cabe");
        assert!(matches!(e, PutError::TooLarge { .. }), "{e:?}");
        assert_eq!(s.get().revision, 1, "la sesión almacenada se queda");
    }

    /// El tope se mide sobre los BYTES serializados, que es lo que ocupa en el
    /// wire y en disco — no sobre el número de claves ni la profundidad.
    #[test]
    fn el_tope_se_mide_en_bytes_serializados() {
        let s = SessionStore::default();
        assert!(s.claim(1));
        let justo = serde_json::json!({
            "x": "y".repeat(norte_proto::methods::SESSION_BODY_MAX - 12)
        });
        let bytes = serde_json::to_vec(&justo).expect("serializa").len();
        assert!(bytes <= norte_proto::methods::SESSION_BODY_MAX, "{bytes}");
        s.put(1, 0, justo).expect("justo por debajo entra");
    }

    /// La dueña es la PRIMERA que la reclama; soltar la propiedad la libera
    /// para la siguiente. Nunca dos escritores sobre un estado.
    #[test]
    fn solo_la_duena_escribe() {
        let s = SessionStore::default();
        assert!(s.claim(1), "la primera se la queda");
        assert!(!s.claim(2), "la segunda corre suelta");
        assert_eq!(s.owner(), Some(1));
        s.put(1, 0, body(1)).expect("la dueña escribe");
        s.release(1);
        assert_eq!(s.owner(), None);
        assert!(s.claim(2), "al irse la dueña, la siguiente puede tomarla");
    }

    /// Soltar una propiedad que no se tiene no se la quita a nadie.
    #[test]
    fn soltar_lo_ajeno_no_hace_nada() {
        let s = SessionStore::default();
        assert!(s.claim(1));
        s.release(2);
        assert_eq!(s.owner(), Some(1), "la 2 no puede desalojar a la 1");
    }

    /// `take_dirty` es lo que consume el escritor a disco: devuelve la sesión
    /// UNA vez por cambio, y nada si no ha cambiado nada desde la última.
    #[test]
    fn lo_sucio_se_consume_una_sola_vez() {
        let s = SessionStore::default();
        assert!(s.claim(1));
        assert!(s.take_dirty().is_none(), "nada que escribir al arrancar");
        s.put(1, 0, body(1)).expect("put");
        assert!(s.dirty());
        assert!(s.take_dirty().is_some());
        assert!(!s.dirty(), "consumida");
        assert!(s.take_dirty().is_none());
    }
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-core`
Expected: FAIL to compile — the module is not declared and `SessionStore` does
not exist.

- [ ] **Step 3: Implement**

At the top of `crates/norte-core/src/session.rs`:

```rust
//! La sesión de UI que el daemon guarda (L2).
//!
//! El core la almacena, la versiona y la devuelve; **no la lee**. Es la misma
//! decisión de ADR 0058 —«una pantalla es un árbol que el core guarda y no
//! interpreta»— llevada al proceso de al lado: los tipos del cuerpo viven en
//! `norte-frontend`, que depende de `norte-proto` y no al revés.
//!
//! Lo único que este módulo hace valer son dos cosas, y las dos son sobre
//! protegerse a sí mismo: la `revision` (un escritor rancio no pisa al
//! vigente) y el tope de 1 MiB (un cliente con un bug no llena el disco).

use std::sync::Mutex;

use norte_proto::methods::{SESSION_BODY_MAX, Session};

/// Por qué se rehusó un `put`.
#[derive(Debug, thiserror::Error)]
pub enum PutError {
    /// La revisión que traía el cliente no es la vigente.
    #[error("revisión rancia; la vigente es {current}")]
    Conflict {
        /// La revisión vigente, para que el cliente re-lea contra ella.
        current: u64,
    },
    /// El cuerpo pasa de [`SESSION_BODY_MAX`].
    #[error("el cuerpo ocupa {bytes} bytes y el tope es {SESSION_BODY_MAX}")]
    TooLarge {
        /// Bytes serializados que traía.
        bytes: usize,
    },
    /// Quien escribe no es la conexión dueña.
    #[error("esta conexión no es la dueña de la sesión")]
    NotOwner,
}

/// La sesión viva del daemon.
#[derive(Debug, Default)]
pub struct SessionStore {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    session: Session,
    /// Conexión dueña, si alguna la reclamó.
    owner: Option<u64>,
    /// Hay cambios sin volcar a disco.
    dirty: bool,
}
```

Then the methods, all short:

- `new(session)` — the session loaded from disk at startup.
- `get()` — clone under the lock.
- `put(version, revision, body)` — measure with
  `serde_json::to_vec(&body).map(|v| v.len()).unwrap_or(usize::MAX)`; over the
  cap, `TooLarge`; revision mismatch, `Conflict { current }`; otherwise store,
  `revision += 1`, `dirty = true`, return the new revision.
- `claim(conn)` — `owner.is_none()` → set it and `true`; already this conn →
  `true`; otherwise `false`.
- `release(conn)` — only when `owner == Some(conn)`.
- `owner()`, `dirty()`, `take_dirty()` — the last clears the flag and returns
  the session.

`PutError::NotOwner` is constructed by the HANDLER, the layer that knows which
connection is speaking; the store carries the variant so both layers name the
refusal the same way.

- [ ] **Step 4: Declare the module**

In `crates/norte-core/src/lib.rs`, beside the other `pub mod` lines:

```rust
pub mod session;
```

- [ ] **Step 5: Run the tests**

Run: `just t norte-core`
Expected: PASS.

Run: `cargo test -p norte-core --doc` and `cargo doc -p norte-core --no-deps`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-core/src/session.rs crates/norte-core/src/lib.rs
git commit -m "feat(core): the session store — one revision, one ceiling, one writer"
```

---

### Task 3: the two handlers

**Files:**
- Modify: `crates/norte-core/src/daemon/server.rs` (`Shared`, dispatch, two handlers)
- Test: `crates/norte-core/tests/daemon.rs`

**Interfaces:**
- Consumes: task 1's types, task 2's `SessionStore`.
- Produces: `session.get`/`session.put` answering over the socket.

- [ ] **Step 1: Write the failing tests**

In `crates/norte-core/tests/daemon.rs`, following the harness that file already
uses (read `fs_capabilities_viaja_por_el_socket` — it is short — for how a
client is opened and a request sent, and use ITS helper names):

```rust
/// L2: la sesión va y vuelve por el socket, y la revisión sube.
#[tokio::test]
async fn session_get_y_put_por_el_socket() {
    let (dir, mut cli) = daemon_humano().await;
    let g: norte_proto::methods::SessionGetResult =
        cli.call(norte_proto::methods::SESSION_GET, serde_json::json!({})).await;
    assert_eq!(g.session.revision, 0);
    assert!(g.owner, "la primera conexión humana se la queda");

    let cuerpo = serde_json::json!({ "version": 1, "slots": {} });
    let p: norte_proto::methods::SessionPutResult = cli
        .call(
            norte_proto::methods::SESSION_PUT,
            serde_json::json!({ "version": 1, "revision": 0, "body": cuerpo }),
        )
        .await;
    assert_eq!(p.revision, 1);

    let g2: norte_proto::methods::SessionGetResult =
        cli.call(norte_proto::methods::SESSION_GET, serde_json::json!({})).await;
    assert_eq!(g2.session.body, cuerpo, "vuelve el mismo documento");
    drop(dir);
}

/// Una revisión rancia por el wire es la taxonomía `Conflict` en `data`, no un
/// error de transporte: el cliente distingue «vuelve a leer» de «el daemon se
/// rompió».
#[tokio::test]
async fn session_put_rancio_es_conflict() {
    let (dir, mut cli) = daemon_humano().await;
    let _: norte_proto::methods::SessionGetResult =
        cli.call(norte_proto::methods::SESSION_GET, serde_json::json!({})).await;
    let _: norte_proto::methods::SessionPutResult = cli
        .call(
            norte_proto::methods::SESSION_PUT,
            serde_json::json!({ "version": 1, "revision": 0, "body": {} }),
        )
        .await;
    let e = cli
        .call_err(
            norte_proto::methods::SESSION_PUT,
            serde_json::json!({ "version": 1, "revision": 0, "body": {} }),
        )
        .await;
    let tax: norte_proto::Error =
        serde_json::from_value(e.data.expect("taxonomía en data")).expect("taxonomía");
    assert!(matches!(tax, norte_proto::Error::Conflict { .. }), "{tax:?}");
    drop(dir);
}

/// Por encima del tope: `LimitExceeded`, y la sesión almacenada se queda.
#[tokio::test]
async fn session_put_sobre_el_tope_es_limit_exceeded() {
    let (dir, mut cli) = daemon_humano().await;
    let _: norte_proto::methods::SessionGetResult =
        cli.call(norte_proto::methods::SESSION_GET, serde_json::json!({})).await;
    let gordo = serde_json::json!({
        "x": "y".repeat(norte_proto::methods::SESSION_BODY_MAX + 1)
    });
    let e = cli
        .call_err(
            norte_proto::methods::SESSION_PUT,
            serde_json::json!({ "version": 1, "revision": 0, "body": gordo }),
        )
        .await;
    let tax: norte_proto::Error =
        serde_json::from_value(e.data.expect("taxonomía en data")).expect("taxonomía");
    assert!(matches!(tax, norte_proto::Error::LimitExceeded { .. }), "{tax:?}");
    let g: norte_proto::methods::SessionGetResult =
        cli.call(norte_proto::methods::SESSION_GET, serde_json::json!({})).await;
    assert_eq!(g.session.revision, 0, "no se escribió nada");
    drop(dir);
}

/// Un agente no tiene pantalla que guardar: `session.*` es `INVALID_REQUEST`,
/// el mismo criterio que `daemon.shutdown` y `plugin.set_approval`.
#[tokio::test]
async fn session_es_de_humanos() {
    let (dir, mut agente) = daemon_agente("a1").await;
    for m in [norte_proto::methods::SESSION_GET, norte_proto::methods::SESSION_PUT] {
        let e = agente
            .call_err(m, serde_json::json!({ "version": 1, "revision": 0, "body": {} }))
            .await;
        assert_eq!(e.code, norte_proto::rpc::INVALID_REQUEST, "{m}");
    }
    drop(dir);
}

/// El segundo cliente del MISMO daemon recibe una copia y corre suelto: su
/// `put` se rehúsa y la sesión del dueño se queda intacta.
#[tokio::test]
async fn el_segundo_cliente_recibe_copia_y_no_escribe() {
    let (dir, mut uno) = daemon_humano().await;
    let g1: norte_proto::methods::SessionGetResult =
        uno.call(norte_proto::methods::SESSION_GET, serde_json::json!({})).await;
    assert!(g1.owner);
    let _: norte_proto::methods::SessionPutResult = uno
        .call(
            norte_proto::methods::SESSION_PUT,
            serde_json::json!({ "version": 1, "revision": 0, "body": { "quien": "uno" } }),
        )
        .await;

    let mut dos = cliente_humano_extra(&dir).await;
    let g2: norte_proto::methods::SessionGetResult =
        dos.call(norte_proto::methods::SESSION_GET, serde_json::json!({})).await;
    assert!(!g2.owner, "la segunda corre suelta");
    assert_eq!(g2.session.body["quien"], serde_json::json!("uno"), "recibe COPIA");
    let e = dos
        .call_err(
            norte_proto::methods::SESSION_PUT,
            serde_json::json!({ "version": 1, "revision": 1, "body": { "quien": "dos" } }),
        )
        .await;
    assert_eq!(e.code, norte_proto::rpc::INVALID_REQUEST);
    let g3: norte_proto::methods::SessionGetResult =
        uno.call(norte_proto::methods::SESSION_GET, serde_json::json!({})).await;
    assert_eq!(g3.session.body["quien"], serde_json::json!("uno"));
    drop(dir);
}
```

If a second-human-client helper (`cliente_humano_extra`) does not exist, add it
beside the existing ones, built the same way.

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-core`
Expected: FAIL — `METHOD_NOT_FOUND` from the daemon.

- [ ] **Step 3: Implement**

In `struct Shared`, one field:

```rust
    /// La sesión de UI (L2): un solo documento por daemon, con su revisión y
    /// su dueña. `Arc` porque el escritor a disco la mira desde otra task.
    session: Arc<crate::session::SessionStore>,
```

Dispatch, beside the other arms:

```rust
        methods::SESSION_GET => handle_session_get(actor, conn_id, shared),
        methods::SESSION_PUT => handle_session_put(req.params, actor, conn_id, shared),
```

Both handlers refuse an agent with `RpcError::invalid_request(...)` — copy the
exact refusal `daemon.shutdown` uses so the two read the same. `get` calls
`claim(conn_id)` and returns `SessionGetResult { session, owner }`. `put`
checks ownership first (`NotOwner` → `INVALID_REQUEST`), then calls the store
and maps `Conflict`/`TooLarge` onto `norte_proto::Error::Conflict` /
`Error::LimitExceeded` in `data`, the way the file's other application errors
already do.

Release on disconnect: at `shared.connections.fetch_sub(1, ...)`
(`server.rs:1051`), add `shared.session.release(conn_id);` — a dead owner does
not hold the session hostage.

- [ ] **Step 4: Run the tests**

Run: `just t norte-core`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-core/src/daemon/server.rs crates/norte-core/tests/daemon.rs
git commit -m "feat(core): session.get and session.put, human-only, one owner"
```

---

### Task 4: on disk

**Files:**
- Create: `crates/norte-core/src/session/disk.rs`
- Modify: `crates/norte-core/src/session.rs` (`pub mod disk;`)
- Test: inside `disk.rs`

**Interfaces:**
- Produces: `session::disk::SCHEMA_VERSION: u32`,
  `session::disk::path(state_dir: &Path) -> PathBuf`,
  `session::disk::load(state_dir: &Path) -> LoadOutcome`,
  `session::disk::write(state_dir: &Path, s: &Session) -> std::io::Result<()>`,
  `enum LoadOutcome { Fresh, Loaded(Session), Corrupt { reason: String },
  FromTheFuture { version: u32 } }`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn sesion(v: u32, rev: u64) -> Session {
        Session { version: v, revision: rev, body: serde_json::json!({ "a": 1 }) }
    }

    /// Sin fichero no hay sesión, y eso NO es un fallo: es un primer arranque.
    #[test]
    fn sin_fichero_es_un_primer_arranque() {
        let d = tempfile::tempdir().expect("tmp");
        assert!(matches!(load(d.path()), LoadOutcome::Fresh));
    }

    /// Escribir y volver a leer devuelve la misma sesión, revisión incluida.
    #[test]
    fn round_trip_por_disco() {
        let d = tempfile::tempdir().expect("tmp");
        write(d.path(), &sesion(1, 9)).expect("escribe");
        let LoadOutcome::Loaded(s) = load(d.path()) else { panic!("cargada") };
        assert_eq!(s.revision, 9);
        assert_eq!(s.version, 1);
    }

    /// Un kind que ningún binario declara sobrevive al viaje por disco: el
    /// cuerpo es opaco también aquí.
    #[test]
    fn un_kind_desconocido_sobrevive_al_disco() {
        let d = tempfile::tempdir().expect("tmp");
        let body = serde_json::json!({ "layouts": { "default": {
            "kind": "kind-de-otro-binario", "params": { "x": [1, 2] } } } });
        write(d.path(), &Session { version: 1, revision: 1, body: body.clone() })
            .expect("escribe");
        let LoadOutcome::Loaded(s) = load(d.path()) else { panic!("cargada") };
        assert_eq!(s.body, body);
    }

    /// Un fichero corrupto NO es una pantalla en blanco: es un diagnóstico y
    /// un arranque desde la config. Y el diagnóstico no cita el CONTENIDO: un
    /// fichero de sesión lleva rutas, y una ruta no va a un log por un error
    /// de parseo.
    #[test]
    fn un_fichero_corrupto_es_diagnostico_no_pantalla_en_blanco() {
        let d = tempfile::tempdir().expect("tmp");
        std::fs::create_dir_all(path(d.path()).parent().expect("padre")).expect("mkdir");
        std::fs::write(path(d.path()), b"{ esto no es json").expect("escribe");
        let LoadOutcome::Corrupt { reason } = load(d.path()) else { panic!("corrupta") };
        assert!(!reason.is_empty());
        assert!(!reason.contains("esto no es json"), "{reason}");
    }

    /// Una sesión de una versión FUTURA no se pisa. Perder una sesión nueva
    /// contra un binario viejo no se recupera.
    #[test]
    fn una_version_del_futuro_no_se_pisa() {
        let d = tempfile::tempdir().expect("tmp");
        write(d.path(), &sesion(SCHEMA_VERSION + 1, 1)).expect("escribe");
        let LoadOutcome::FromTheFuture { version } = load(d.path()) else {
            panic!("del futuro")
        };
        assert_eq!(version, SCHEMA_VERSION + 1);
    }

    /// La escritura es atómica: no deja `.tmp` detrás.
    #[test]
    fn escribir_no_deja_temporales() {
        let d = tempfile::tempdir().expect("tmp");
        write(d.path(), &sesion(1, 1)).expect("escribe");
        write(d.path(), &sesion(1, 2)).expect("reescribe");
        let dir = path(d.path()).parent().expect("padre").to_owned();
        let restos: Vec<_> = std::fs::read_dir(&dir)
            .expect("lee")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tmp") || n.ends_with('~'))
            .collect();
        assert!(restos.is_empty(), "restos: {restos:?}");
    }

    /// El fichero no lo puede leer cualquiera: la sesión lleva las rutas por
    /// las que el lector se mueve.
    #[cfg(unix)]
    #[test]
    fn el_fichero_es_solo_del_dueno() {
        use std::os::unix::fs::PermissionsExt as _;
        let d = tempfile::tempdir().expect("tmp");
        write(d.path(), &sesion(1, 1)).expect("escribe");
        let modo = std::fs::metadata(path(d.path())).expect("stat").permissions().mode();
        assert_eq!(modo & 0o077, 0, "modo {modo:o}");
    }
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-core`
Expected: FAIL to compile — `session::disk` does not exist.

- [ ] **Step 3: Implement**

`path(state_dir)` is `state_dir.join("norte").join("session.json")` — check
`crates/norte-core/src/logging.rs` for how the log file composes its directory
and follow that, so the two land in one place and not two.

`write` creates the parent (mode `0o700` on unix, as logging does), serialises
with `serde_json::to_vec`, writes `<file>.tmp` with mode `0o600`, `sync_all`,
then renames. `load` reads, parses, and returns the four outcomes.
`SCHEMA_VERSION: u32 = 1` lives here and is the version the core refuses to
overwrite when the file carries a bigger one.

- [ ] **Step 4: Run the tests**

Run: `just t norte-core`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-core/src/session
git commit -m "feat(core): the session on disk, atomic, owner-only, and never a blank screen"
```

---

### Task 5: when it is written, and the cross-process lock

**Files:**
- Modify: `crates/norte-core/src/session/disk.rs` (the lock)
- Modify: `crates/norte-core/src/daemon/server.rs` (load at bind, writer task)
- Test: `crates/norte-core/tests/daemon.rs`

**Interfaces:**
- Produces: `session::disk::lock(state_dir: &Path) -> std::io::Result<Option<SessionLock>>`
  (`None` = held by somebody else), `struct SessionLock` whose `Drop` releases.

- [ ] **Step 1: Write the failing tests**

```rust
/// El relevo es el evento por el que esto existe: un daemon dice
/// `going_away`, y lo que el cliente había puesto está en disco cuando el
/// siguiente arranca.
#[tokio::test]
async fn la_sesion_sobrevive_a_un_relevo() {
    let estado = tempfile::tempdir().expect("tmp");
    let (dir, mut cli) = daemon_humano_con_estado(estado.path()).await;
    let _: norte_proto::methods::SessionGetResult =
        cli.call(norte_proto::methods::SESSION_GET, serde_json::json!({})).await;
    let _: norte_proto::methods::SessionPutResult = cli
        .call(
            norte_proto::methods::SESSION_PUT,
            serde_json::json!({ "version": 1, "revision": 0, "body": { "dir": "file:///casa" } }),
        )
        .await;
    let _: serde_json::Value = cli
        .call(
            norte_proto::methods::DAEMON_SHUTDOWN,
            serde_json::json!({ "mode": "graceful" }),
        )
        .await;
    drop(cli);
    drop(dir);

    let (dir2, mut cli2) = daemon_humano_con_estado(estado.path()).await;
    let g: norte_proto::methods::SessionGetResult =
        cli2.call(norte_proto::methods::SESSION_GET, serde_json::json!({})).await;
    assert_eq!(g.session.body["dir"], serde_json::json!("file:///casa"));
    assert_eq!(g.session.revision, 1, "la revisión también sobrevive");
    drop(dir2);
}

/// Un segundo core sobre el mismo estado NO escribe: clona y corre suelto.
#[test]
fn un_segundo_core_no_escribe_sobre_el_estado_ajeno() {
    let d = tempfile::tempdir().expect("tmp");
    let uno = norte_core::session::disk::lock(d.path()).expect("lock").expect("libre");
    assert!(
        norte_core::session::disk::lock(d.path()).expect("lock").is_none(),
        "el segundo no la toma"
    );
    drop(uno);
    assert!(
        norte_core::session::disk::lock(d.path()).expect("lock").is_some(),
        "al soltarla, el siguiente sí"
    );
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-core`
Expected: FAIL — `session::disk::lock` does not exist and the session does not
survive the handover.

- [ ] **Step 3: Implement the lock**

Copy the shape of `lock_config_file` (`crates/norte-config/src/load.rs:286`):
`flock`/`LockFileEx` on a dedicated `session.json.lock` sibling, **never on the
file itself** — the same reason it gives there. It is `try_lock`, not blocking:
whoever does not get it runs detached, and waiting would hang a start behind a
daemon that is alive.

- [ ] **Step 4: Implement the triggers**

At bind: take the lock, `load`, and build `Shared.session` from the outcome —
`Fresh`/`Corrupt`/`FromTheFuture` all give an empty store, and the last two log
a diagnostic (`tracing::warn!`). Without the lock the store is still built from
the loaded session, but the writer task is never spawned: a detached core has
the screen and writes nothing.

The writer is one task holding the `Arc<SessionStore>`:

- a `tokio::time::interval` of one second; on each tick, `take_dirty()` and, if
  it gives something, `disk::write`. **Coalescing is the point**: the cursor
  moves on every arrow key and the file is not a database;
- the same write, forced, on the shutdown path that emits
  `methods::DaemonGoingAway` (`server.rs:2124`) — the handover is the event
  this exists for;
- and when `connections` reaches zero.

A failed write is `tracing::warn!` and the next tick retries: losing a session
is a bad afternoon, and killing the daemon over it is worse.

- [ ] **Step 5: Run the tests**

Run: `just t norte-core`
Expected: PASS.

- [ ] **Step 6: Spend the first gate run**

Run: `just ci-fast`
Expected: green. This is **one** of the two runs this plan budgets. Red is
reproduced with `just t <crate>`, not with a second `ci-fast`.

- [ ] **Step 7: Commit**

```bash
git add crates/norte-core
git commit -m "feat(core): the session is written on quiet, on handover, and on the last goodbye"
```

---

### Task 6: the body — `SessionBody`, its caps and its sweep

**Files:**
- Create: `crates/norte-frontend/src/session.rs`
- Modify: `crates/norte-frontend/src/lib.rs`
- Test: inside `session.rs`

**Interfaces:**
- Produces: `session::SCHEMA_VERSION: u32`, `session::HISTORY_CAP: usize`,
  `session::ORPHAN_CAP: usize`, `session::MAX_AGE_MS: u64`,
  `session::SessionBody { layouts: BTreeMap<String, Node>, slots:
  BTreeMap<u64, SlotState> }`, `session::SlotState { path: VPath, cursor: u64,
  back: Vec<VPath>, forward: Vec<VPath>, sort: SortSpec, columns:
  Vec<ColumnId>, show_hidden: bool, touched_ms: u64 }`,
  `SessionBody::prune(&mut self, now_ms: u64)`,
  `SessionBody::to_value(&self) -> serde_json::Value`,
  `SessionBody::from_value(v: &serde_json::Value) -> Result<Self, SessionError>`.
- Consumes: `layout::Node`, `SortSpec`, `columns::ColumnId`, `norte_proto::VPath`.

`touched_ms` is not in the spec's sketch and is required by it: the age sweep
is "30 days", and a sweep needs a clock stamp to sweep by. The client writes
it, like every other cap.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn vp(s: &str) -> VPath {
        VPath::parse(s).expect("vpath")
    }

    fn slot(path: &str) -> SlotState {
        SlotState {
            path: vp(path),
            cursor: 0,
            back: Vec::new(),
            forward: Vec::new(),
            sort: SortSpec::default(),
            columns: Vec::new(),
            show_hidden: false,
            touched_ms: 0,
        }
    }

    /// Round trip por JSON: lo que sale es lo que entró.
    #[test]
    fn round_trip_por_json() {
        let mut b = SessionBody::default();
        b.slots.insert(1, slot("file:///casa"));
        let vuelta = SessionBody::from_value(&b.to_value()).expect("parsea");
        assert_eq!(vuelta, b);
    }

    /// **La prueba que la regla 1 pide**: un nombre que no es UTF-8 sobrevive
    /// entero. Este es el sitio exacto donde un `String` se lo habría comido.
    #[test]
    fn un_nombre_no_utf8_sobrevive_al_viaje() {
        for name in norte_testkit::corpus::HOSTILE_NAMES {
            let ruta = VPath::parse("file:///casa").expect("raiz").join(name);
            let mut b = SessionBody::default();
            b.slots.insert(1, SlotState { path: ruta.clone(), ..slot("file:///casa") });
            let vuelta = SessionBody::from_value(&b.to_value()).expect("parsea");
            assert_eq!(
                vuelta.slots[&1].path.as_bytes(),
                ruta.as_bytes(),
                "{name:?} no sobrevivió"
            );
        }
    }

    /// Un kind que este binario no declara vuelve intacto, `params` incluidos.
    #[test]
    fn un_kind_desconocido_vuelve_entero() {
        let toml = r#"
            kind = "kind-de-otro-binario"
            [params]
            grados = 3
        "#;
        let arbol: Node = toml::from_str(toml).expect("parsea");
        let mut b = SessionBody::default();
        b.layouts.insert("default".into(), arbol.clone());
        let vuelta = SessionBody::from_value(&b.to_value()).expect("parsea");
        assert_eq!(vuelta.layouts["default"], arbol);
    }

    /// El historial se recorta al ESCRIBIR, y por el extremo viejo: lo que se
    /// tira es lo más lejano, no lo que acabas de andar.
    #[test]
    fn el_historial_se_recorta_por_lo_viejo() {
        let mut b = SessionBody::default();
        let mut s = slot("file:///casa");
        s.back = (0..HISTORY_CAP + 10).map(|i| vp(&format!("file:///d{i}"))).collect();
        b.slots.insert(1, s);
        b.prune(0);
        let back = &b.slots[&1].back;
        assert_eq!(back.len(), HISTORY_CAP);
        assert_eq!(
            back.last().expect("último"),
            &vp(&format!("file:///d{}", HISTORY_CAP + 9))
        );
    }

    /// Un layout que no menciona un hueco NO borra su estado: cambiar de
    /// disposición no te tira el historial.
    #[test]
    fn el_estado_huerfano_sobrevive_al_cambio_de_layout() {
        let mut b = SessionBody::default();
        b.slots.insert(7, slot("file:///lejos"));
        b.layouts.insert("default".into(), Node::slot(SlotId(1), KindId::browser()));
        b.prune(0);
        assert!(b.slots.contains_key(&7), "el huérfano se queda");
    }

    /// Los huérfanos tienen tope, y cae el que hace más que no se toca.
    #[test]
    fn los_huerfanos_tienen_tope_y_cae_el_mas_viejo() {
        let mut b = SessionBody::default();
        let cap = u64::try_from(ORPHAN_CAP).expect("cabe");
        for i in 0..cap + 5 {
            let mut s = slot("file:///casa");
            s.touched_ms = i;
            b.slots.insert(i, s);
        }
        b.prune(1_000);
        assert_eq!(b.slots.len(), ORPHAN_CAP);
        assert!(!b.slots.contains_key(&0), "el más viejo se fue");
        assert!(b.slots.contains_key(&(cap + 4)));
    }

    /// Y una edad: treinta días sin tocarse y el hueco se va, aunque quepa.
    #[test]
    fn un_hueco_de_hace_treinta_dias_se_barre() {
        let mut b = SessionBody::default();
        let mut viejo = slot("file:///casa");
        viejo.touched_ms = 0;
        let mut nuevo = slot("file:///casa");
        nuevo.touched_ms = MAX_AGE_MS;
        b.slots.insert(1, viejo);
        b.slots.insert(2, nuevo);
        b.prune(MAX_AGE_MS + 1);
        assert!(!b.slots.contains_key(&1));
        assert!(b.slots.contains_key(&2));
    }

    /// Un hueco que el layout VIVO menciona no lo barre ni la edad ni el tope:
    /// lo que se ve en pantalla no se recicla.
    #[test]
    fn un_hueco_visible_no_se_barre_jamas() {
        let mut b = SessionBody::default();
        let mut s = slot("file:///casa");
        s.touched_ms = 0;
        b.slots.insert(1, s);
        b.layouts.insert("default".into(), Node::slot(SlotId(1), KindId::browser()));
        b.prune(MAX_AGE_MS * 10);
        assert!(b.slots.contains_key(&1));
    }

    /// Un cuerpo de una versión que este binario no conoce se rehúsa: mejor
    /// arrancar de la config que interpretar campos que no son los tuyos.
    #[test]
    fn un_esquema_del_futuro_se_rehusa() {
        let v = serde_json::json!({ "version": SCHEMA_VERSION + 1, "layouts": {}, "slots": {} });
        assert!(SessionBody::from_value(&v).is_err());
    }
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-frontend`
Expected: FAIL to compile — the module is not declared.

- [ ] **Step 3: Implement**

Constants, documented where they are declared because the spec says a cap
discovered later is a migration:

```rust
/// Esquema del cuerpo. Lo posee este crate, no el wire: añadir un campo a
/// [`SlotState`] es subir ESTE número, no la versión del protocolo.
pub const SCHEMA_VERSION: u32 = 1;
/// Entradas de historial por hueco y por sentido.
pub const HISTORY_CAP: usize = 64;
/// Huecos huérfanos —los que ningún layout menciona— que se guardan.
pub const ORPHAN_CAP: usize = 128;
/// Edad a la que un huérfano se barre: treinta días en milisegundos.
pub const MAX_AGE_MS: u64 = 30 * 24 * 60 * 60 * 1000;
```

`prune(now_ms)` does, in this order: truncate each `back`/`forward` keeping the
NEWEST `HISTORY_CAP`; collect the slot ids the layouts mention (`Node::
slot_ids()` over every layout) — those are never swept; drop orphans older than
`MAX_AGE_MS`; if more than `ORPHAN_CAP` orphans remain, drop the
least-recently-touched until they fit.

`to_value`/`from_value` go through `serde_json`, with `version` written into
the object and checked on the way in.

- [ ] **Step 4: Run the tests**

Run: `just t norte-frontend`
Expected: PASS.

Run: `cargo test -p norte-frontend --doc` and `cargo doc -p norte-frontend --no-deps`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-frontend/src/session.rs crates/norte-frontend/src/lib.rs
git commit -m "feat(frontend): the session body, with the caps in the type and not in the caller"
```

---

### Task 7: the TUI reads it, applies it, and pushes it

**Files:**
- Modify: `crates/norte-tui/src/app.rs` (capture/apply)
- Modify: `crates/norte-tui/src/main.rs` (read at start, coalesced push, failures)
- Modify: `crates/norte-i18n/i18n/{en,es}.ftl`
- Create: `crates/norte-tui/tests/session.rs`

**Interfaces:**
- Consumes: task 6's `SessionBody`, task 1's methods.
- Produces: `App::session_body(&self) -> norte_frontend::session::SessionBody`,
  `App::apply_session(&mut self, body: &SessionBody) -> Vec<SlotId>` (the slots
  whose listing must be filled), `App::apply_session_value(&mut self, v:
  &serde_json::Value)`, `App::session_detached: bool`.

- [ ] **Step 1: Write the failing tests**

```rust
/// Capturar y volver a aplicar deja la misma pantalla: mismo árbol, mismas
/// rutas, mismo cursor.
#[test]
fn capturar_y_aplicar_es_la_identidad() {
    let mut app = helpers::app_basica();
    app.set_layout(norte_frontend::layout::presets::tree("krusader").expect("preset"));
    let antes = app.session_body();
    let mut otra = helpers::app_basica();
    otra.apply_session(&antes);
    assert_eq!(otra.session_body(), antes);
}

/// Las marcas NO viajan: son el estado de una operación, no de una sesión.
#[test]
fn las_marcas_no_viajan() {
    let mut app = helpers::app_basica();
    app.focused_mut().mark_all();
    let v = serde_json::to_string(&app.session_body().to_value()).expect("json");
    assert!(!v.contains("mark"), "{v}");
}

/// Una sesión que menciona un hueco que este layout no tiene no rompe nada: se
/// guarda su estado y se pinta lo que el layout dice.
#[test]
fn un_hueco_que_el_layout_no_tiene_no_rompe_la_aplicacion() {
    let mut app = helpers::app_basica();
    let mut body = app.session_body();
    let uno = body.slots.values().next().expect("uno").clone();
    body.slots.insert(99, uno);
    app.apply_session(&body);
    assert!(app.session_body().slots.contains_key(&99), "se conserva");
}

/// Un cuerpo corrupto no deja pantalla en blanco: se ignora y queda el layout
/// de la config, con un aviso.
#[test]
fn un_cuerpo_corrupto_deja_la_pantalla_de_la_config() {
    let mut app = helpers::app_basica();
    let antes = app.layout_tree().clone();
    app.apply_session_value(&serde_json::json!({ "version": 999 }));
    assert_eq!(app.layout_tree(), &antes);
    assert!(app.message().is_some(), "y lo dice");
}
```

Match the helper names the `crates/norte-tui/tests/` files already use.

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-tui`
Expected: FAIL to compile.

- [ ] **Step 3: Implement capture and apply**

`session_body()` walks the visible slots and the layout tree; `apply_session`
sets the tree, then seeds each slot's `PaneState` (path, cursor, sort, columns,
`show_hidden`) and its `History`, and returns the ids whose listing has to be
fetched — `main.rs` owns the fetching, as it does for every other cd.

- [ ] **Step 4: Wire the client side in `main.rs`**

- After `initialize`, one `session.get`. `owner: false` sets
  `app.session_detached` and shows `msg-session-detached` in the status bar.
- A dirty flag set by the same places that already mark the UI dirty; a
  `tokio::time::interval` of one second in the event loop pushes when dirty and
  not detached, carrying the revision from the last `get`/`put`.
- `Conflict` → re-`get`, re-apply, and push again once.
- `LimitExceeded` → drop history (`back`/`forward` emptied) and retry **once**;
  if it still does not fit, stop pushing for this run and say so
  (`msg-session-too-large`).
- Never push while an operation is in flight behind a modal: the session is
  about where you are, not about what you are doing.

Strings, both locales:

```text
msg-session-detached = another window owns the session; this one runs on its own
msg-session-too-large = the session is too big to store; history dropped
msg-session-unreadable = the stored session could not be read; starting from the configured layout
```

- [ ] **Step 5: Run the tests, then drive it**

Run: `just t norte-tui`
Expected: PASS.

```bash
just link
tmux new-session -d -s norte -x 100 -y 30 'ntc'
# cambia de directorio, mueve el cursor, abre el sidebar, sal con F10,
# vuelve a entrar: la pantalla es la que dejaste.
tmux capture-pane -t norte -p | head -32
tmux kill-session -t norte
```

**Phase A found two real bugs this way with the suite green**, and one was a
dialog that painted nothing. Drive it: a second `ntc` must come up with the
same screen and say it runs detached.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-tui crates/norte-i18n
git commit -m "feat(tui): the screen you left comes back, and a second window says it is a copy"
```

---

### Task 8: close the branch

- [ ] **Step 1: ADR 0059**

Use the `/adr` command. It records what the session stores, who owns it, why
the body is opaque, and why there is no notification: the core does not tell
anybody the session changed, because the only writer is the client that changed
it.

- [ ] **Step 2: Changelog**

Under `## [Unreleased]` / `### Added`, in the register the rest of the file
uses — what a user gets, not what was refactored:

```markdown
- **The screen you left is the screen you get back.** norte now remembers the
  arrangement, the directories, the cursor, the history, the sort and the
  columns of every panel, and gives them back when you start it again — across
  a daemon that was replaced under you. A second window opens on the same
  screen and then goes its own way: it says so in the status bar, and it never
  writes over the first one's state.
```

- [ ] **Step 3: The one full gate run**

Run: `just ci`. This branch touches `norte-proto` and `norte-core`, so `cov` is
part of it. Run the recipes one at a time in the foreground if the whole thing
is killed as a background job, and never through `| tail`.

- [ ] **Step 4: Review before merging, not after**

Dispatch, in parallel, with the commit range and what the change is for:

- `protocol-guardian` — **mandatory**: the 0.48.0 bump, the additive shape, the
  golden, and the compatibility window.
- `security-reviewer` — the session file holds every path the reader walks: the
  mode of the file, the diagnostic that must not quote content, and the lock.
- `rust-reviewer` — the whole diff.

Apply BLOCKER and MAJOR; say which MINORs you skipped and why.

- [ ] **Step 5: Record the work**

Update `MEMORY.md` and its layout entry: phase B landed, what the wire looks
like now, and what is left of the layout line.

---

## Notes for whoever executes this

**Nothing will notify you.** No monitor exists. Do not `sleep`, do not
`timeout N tail -f /dev/null`, do not wait for a signal — run the next step.

**Two agents never share this tree** (`scripts/wt.sh <name>` gives a worktree).

**Read `git diff --cached --stat` before every commit.**

**The doctest trap is live in this plan.** `just t` runs nextest, which does not
run doctests, and the pre-commit hook runs `just ci-fast`, which does. Tasks 1,
2 and 6 add documented public items: `cargo test -p <crate> --doc` before you
commit them, not after the hook says no.

**`cargo-insta` is not installed and a hook forbids editing a `.snap` by hand.**
If you add a snapshot: `INSTA_UPDATE=always cargo nextest run -p <crate> -E
'test(<name>)'`.
