# A daemon that says it is being replaced: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: use `superpowers:subagent-driven-development`
> or `superpowers:executing-plans` to implement this plan task by task. Steps
> use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A new daemon takes over from a running one without the frontends
losing their sessions — by giving the old daemon a way to say *"a replacement is
coming"*, which today it cannot, and giving the client permission to act on the
difference.

**Architecture:** Spec is
`docs/superpowers/specs/2026-08-15-daemon-handover-design.md` — read it first.
Reconnection already works (`RemoteBackend` re-initialises, resyncs through
`task.list` and reconciles). What is missing is that a replacement and a
user-requested stop are byte-identical on the wire, and one deliberate rule says
a reconnect must never resurrect a daemon the user just stopped. One
notification and one optional field make the two distinguishable.

**Tech stack:** Rust 2024, JSON-RPC over a Unix socket, `tokio`,
`norte-proto` (wire), `nextest` via `just`.

**Protocol: 0.46.0**, additive. `protocol-guardian` review is MANDATORY.

---

## One deviation from the spec, decided while planning

The spec says a handover **waits** for live tasks up to `grace_ms` and refuses
if the grace expires. Writing the tasks out showed that cannot work: the
`daemon.shutdown` reply is sent immediately, so a refusal decided minutes later
has nobody to tell — and by then the daemon has already stopped accepting, so
"refusing" would mean resuming acceptance, which is a state machine nobody
asked for.

**So a handover refuses up front, while there is still someone to answer.** If
any task is live, `mode: "handover"` returns an error naming how many, and
nothing happens: no notification, no stop, no exit. The caller waits and
retries. `grace_ms` disappears from the notification with the drain it was
describing.

This is smaller, fully deterministic, and testable in one pass. What it costs is
that a package manager upgrading during a large copy has to loop — which is
correct behaviour, and better than the alternative of guessing how long a
terabyte takes.

`graceful: false` still exists for a caller who genuinely wants the tasks
cancelled. No new door to that room.

---

## File map

| file | what it becomes responsible for |
| --- | --- |
| `crates/norte-proto/src/methods.rs` | `DAEMON_GOING_AWAY`, `DaemonGoingAway`, `ShutdownMode`, `PROTOCOL_VERSION` |
| `crates/norte-proto/tests/golden_types.rs` | the golden shapes of both |
| `crates/norte-core/src/daemon/server.rs` | broadcast, the live-task refusal, the mode |
| `crates/norte-core/src/backend.rs` | the client remembers and consumes the spawn permission |
| `crates/norte-core/tests/daemon.rs` | the end-to-end handover |

---

## Task 1: the wire, all of it, once

**Files:**
- Modify: `crates/norte-proto/src/methods.rs` (`DaemonShutdownParams` ~2219, `PROTOCOL_VERSION` ~606)
- Test: `crates/norte-proto/tests/golden_types.rs`, `crates/norte-proto/tests/types.rs`

One task for the whole wire change, so exactly one version bump and one
`protocol-guardian` pass covers it.

- [ ] **Step 1: write the failing tests**

In `crates/norte-proto/tests/types.rs`:

```rust
/// El default de `mode` reproduce EXACTAMENTE lo de hoy: un cliente que no
/// conoce el campo sigue apagando el daemon, no relevándolo.
#[test]
fn shutdown_sin_mode_es_un_stop() {
    let p: methods::DaemonShutdownParams =
        serde_json::from_str("{}").expect("todo-opcionales");
    assert_eq!(p.mode, methods::ShutdownMode::Stop);
    assert!(p.graceful, "y `graceful` no cambia de default");
}

/// `mode` y `graceful` son ejes ORTOGONALES y el tipo no los mezcla: relevar
/// dice quién viene después, `graceful` dice qué se hace con las tasks.
#[test]
fn relevar_y_cancelar_son_ejes_distintos() {
    let p: methods::DaemonShutdownParams =
        serde_json::from_str(r#"{"mode":"handover","graceful":false}"#).expect("json");
    assert_eq!(p.mode, methods::ShutdownMode::Handover);
    assert!(!p.graceful);
}

/// Un modo que este binario no conoce NO se adivina: apagar es destructivo y
/// «no sé qué me pides» tiene que ser un error, no el default más parecido.
#[test]
fn un_modo_desconocido_es_error() {
    let r: Result<methods::DaemonShutdownParams, _> =
        serde_json::from_str(r#"{"mode":"teletransportar"}"#);
    assert!(r.is_err(), "un modo inventado no puede degradar a `stop`");
}

/// La notificación lleva lo único que el cliente necesita decidir: si volver.
#[test]
fn going_away_dice_si_volver() {
    let n = methods::DaemonGoingAway { reconnect: true };
    let j = serde_json::to_value(&n).expect("json");
    assert_eq!(j["reconnect"], serde_json::json!(true));
}
```

In `crates/norte-proto/tests/golden_types.rs`, add the golden shapes of
`DaemonGoingAway` and of a `DaemonShutdownParams` with `mode` set, following the
file's existing pattern, and bump the protocol version assertion.

- [ ] **Step 2: run and watch them fail**

```bash
just t norte-proto
```
Expected: FAIL, no variant `ShutdownMode`.

- [ ] **Step 3: implement the wire**

```rust
/// `daemon.going_away` — el daemon avisa de que se va ANTES de dejar de
/// aceptar (0.46.0).
///
/// Existe porque un relevo y una parada son el MISMO evento visto desde el
/// cliente —una conexión cerrada— y la respuesta correcta es la contraria en
/// cada caso: volver, o rendirse. Sin esto, un cliente que reconectara siempre
/// resucitaría un daemon que el usuario acaba de parar, y uno que no
/// reconectara nunca dejaría la sesión muerta tras una actualización.
pub const DAEMON_GOING_AWAY: &str = "daemon.going_away";

/// Params de [`DAEMON_GOING_AWAY`].
///
/// ```
/// use norte_proto::methods::DaemonGoingAway;
/// let n = DaemonGoingAway { reconnect: true };
/// let j = serde_json::to_value(&n).expect("json");
/// assert_eq!(j["reconnect"], serde_json::json!(true));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonGoingAway {
    /// `true` = viene un relevo; vuelve a conectar, y arráncalo si no está.
    /// `false` = este daemon se para y se queda parado.
    pub reconnect: bool,
}

/// Qué clase de apagado es (0.46.0). ORTOGONAL a
/// [`DaemonShutdownParams::graceful`], que decide qué pasa con las tasks.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownMode {
    /// Se para y se queda parado. El comportamiento de siempre.
    #[default]
    Stop,
    /// Viene un relevo: los clientes deben volver.
    Handover,
}
```

**`ShutdownMode` has NO `#[serde(other)]` fallback, unlike most enums on this
wire, and that is deliberate**: the rest degrade because misreading them costs a
feature. Misreading this one shuts down a daemon in a way the caller did not
ask for. Unknown means error — the same criterion `unknown_policies_are_hard_errors`
already applies in `types.rs`.

Add `mode: ShutdownMode` with `#[serde(default)]` to `DaemonShutdownParams`, and
bump `PROTOCOL_VERSION` to `"0.46.0"`.

- [ ] **Step 4: run the tests**

```bash
just t norte-proto && cargo test -p norte-proto --doc
```
Expected: PASS. Accept the golden diffs only after reading them.

- [ ] **Step 5: commit**

```bash
git add -A crates/norte-proto
git commit -m "feat(proto): a daemon can say whether it is coming back (0.46.0)"
```

---

## Task 2: the daemon announces, and refuses a handover it cannot make

**Files:**
- Modify: `crates/norte-core/src/daemon/server.rs` (`handle_daemon_shutdown` ~2049)
- Test: `crates/norte-core/tests/daemon.rs`

- [ ] **Step 1: write the failing tests**

```rust
/// Un relevo avisa ANTES de dejar de aceptar, y lo que dice es «vuelve».
#[tokio::test]
async fn un_relevo_avisa_de_que_vuelve() {
    let h = daemon_con_cliente().await;
    let mut notifs = h.client.notifications();

    h.client.shutdown_with(ShutdownMode::Handover).await.expect("acepta");

    let n = espera_notif(&mut notifs, methods::DAEMON_GOING_AWAY).await;
    let p: methods::DaemonGoingAway = serde_json::from_value(n).expect("params");
    assert!(p.reconnect, "un relevo dice que vuelvas");
}

/// Y una parada normal avisa de lo contrario. Es lo que separa «el daemon se
/// paró» de «se cayó la conexión», que para el usuario no son lo mismo.
#[tokio::test]
async fn una_parada_avisa_de_que_no_vuelvas() {
    let h = daemon_con_cliente().await;
    let mut notifs = h.client.notifications();

    h.client.shutdown_with(ShutdownMode::Stop).await.expect("acepta");

    let n = espera_notif(&mut notifs, methods::DAEMON_GOING_AWAY).await;
    let p: methods::DaemonGoingAway = serde_json::from_value(n).expect("params");
    assert!(!p.reconnect);
}

/// Un relevo con una task viva se REHÚSA, en la respuesta, mientras todavía
/// hay alguien a quien contestar — y no toca nada: el daemon sigue aceptando y
/// la task sigue corriendo.
#[tokio::test]
async fn un_relevo_con_una_task_viva_se_rehusa_y_no_toca_nada() {
    let h = daemon_con_cliente().await;
    let _task = h.lanza_task_larga().await;

    let err = h
        .client
        .shutdown_with(ShutdownMode::Handover)
        .await
        .expect_err("con una task viva, no");

    assert!(
        h.client.ping().await.is_ok(),
        "el daemon sigue vivo y aceptando: {err:?}"
    );
}

/// Una conexión de AGENTE no releva, igual que no apaga: es un acto de
/// gobierno humano, como `policy.grant_scope`.
#[tokio::test]
async fn un_agente_no_puede_relevar() {
    let h = daemon_con_agente().await;
    let err = h
        .client
        .shutdown_with(ShutdownMode::Handover)
        .await
        .expect_err("un agente no");
    assert_eq!(codigo(&err), codes::INVALID_REQUEST);
}
```

`daemon_con_cliente`, `daemon_con_agente`, `espera_notif`, `lanza_task_larga`,
`codigo` and `shutdown_with` stand in for whatever that suite already calls its
equivalents — reuse them; `shutdown_with` is the only one likely to be new, and
it is a thin wrapper over the existing shutdown call with the params.

- [ ] **Step 2: run and watch them fail**

```bash
just t norte-core
```
Expected: FAIL — no notification arrives.

- [ ] **Step 3: implement**

In `handle_daemon_shutdown`, after the existing `Actor::User` check and the
params parse, and BEFORE any cancellation:

1. If `p.mode == Handover` and `!shared.tasks.lock().is_empty()`, return
   `RpcError::protocol(codes::INVALID_REQUEST, ...)` naming the count. Nothing
   else happens.
2. Build `DaemonGoingAway { reconnect: p.mode == Handover }`, encode it as a
   `Notification` with `DAEMON_GOING_AWAY`, and broadcast it to **every**
   connection — not `broadcast_humans`. An agent's session dies with the daemon
   exactly like a human's, and it needs to know. Use `broadcast_where` with a
   predicate that accepts all, next to the two that exist.
3. Then the existing `hard_shutdown` / `shutdown` cancellation, unchanged.

The order matters and is the test: the notification must be written to the
sockets before the listener stops, or a client learns nothing.

- [ ] **Step 4: run the tests**

```bash
just t norte-core
```
Expected: PASS.

- [ ] **Step 5: commit**

```bash
git add -A crates/norte-core/src/daemon
git commit -m "feat(core): the daemon says whether a replacement is coming"
```

---

## Task 3: the client acts on the difference

**Files:**
- Modify: `crates/norte-core/src/backend.rs` (`establish` ~2617, the pump loop)
- Test: `crates/norte-core/tests/daemon.rs`

- [ ] **Step 1: write the failing tests**

```rust
/// Tras un relevo, el cliente SÍ arranca al que viene. Es lo contrario de la
/// regla de la reconexión normal, y por eso hace falta la notificación: sin
/// ella las dos situaciones son la misma conexión cerrada.
#[tokio::test]
async fn tras_un_relevo_el_cliente_arranca_al_que_viene() {
    let h = daemon_con_backend_que_puede_spawnear().await;

    h.pide_relevo().await;
    h.espera_a_que_el_socket_muera().await;

    // Nadie arranca el reemplazo salvo el propio cliente.
    assert!(h.backend.ping().await.is_ok(), "volvió, y con daemon nuevo");
    assert_ne!(h.pid_del_daemon().await, h.pid_original, "es OTRO proceso");
}

/// Y tras una parada NO lo arranca. Ésta es la regla que ya existía
/// (`backend.rs`, M3 del rust-reviewer: reconectar jamás resucita un daemon
/// que el usuario acaba de parar), y este test es su red.
#[tokio::test]
async fn tras_una_parada_el_cliente_no_resucita_nada() {
    let h = daemon_con_backend_que_puede_spawnear().await;

    h.pide_parada().await;
    h.espera_a_que_el_socket_muera().await;

    assert!(
        h.backend.ping().await.is_err(),
        "el usuario lo paró: nadie lo vuelve a levantar"
    );
}

/// El permiso se GASTA en el intento que lo usa. Un frontend al que le
/// dijeron que venía un relevo, que no lo encontró y se rindió, no puede
/// llevarse esa licencia a la conexión de la semana que viene.
#[tokio::test]
async fn el_permiso_de_arranque_se_gasta_en_un_intento() {
    let h = daemon_con_backend_cuyo_spawn_falla().await;

    h.pide_relevo().await;
    h.espera_a_que_el_socket_muera().await;
    let _ = h.backend.ping().await; // gasta el permiso fallando

    assert_eq!(h.intentos_de_spawn(), 1, "uno, y solo uno");
}
```

- [ ] **Step 2: run and watch them fail**

```bash
just t norte-core
```
Expected: FAIL — the client never respawns.

- [ ] **Step 3: implement**

In `RemoteBackend`'s `Inner`, one flag:

```rust
    /// El daemon dijo que venía un relevo (`daemon.going_away` con
    /// `reconnect: true`), así que la PRÓXIMA reconexión puede arrancarlo.
    ///
    /// Se GASTA en el intento que lo usa, salgan las cosas bien o mal: si el
    /// relevo no estaba y el spawn falló, insistir sería exactamente lo que la
    /// regla de no-resucitar prohíbe, solo que más tarde.
    handover_expected: std::sync::atomic::AtomicBool,
```

The pump loop, which already routes notifications by method, sets it on
`DAEMON_GOING_AWAY` with `reconnect: true` and clears it on `reconnect: false`.
`establish` takes `spawn = first_connection || handover_expected.swap(false)` —
`swap` is what makes the consumption atomic and one-shot.

Everything else about reconnection is untouched.

- [ ] **Step 4: run the tests**

```bash
just t norte-core
```
Expected: PASS.

- [ ] **Step 5: commit**

```bash
git add -A crates/norte-core/src/backend.rs
git commit -m "feat(core): a client comes back for a replacement, and only then"
```

---

## Task 4: the handover, end to end, with two real clients

**Files:**
- Test: `crates/norte-core/tests/daemon.rs`

This is the test the whole item exists for. It must not be simulated: two real
connections, a real socket, a real replacement process.

- [ ] **Step 1: write the test**

```rust
/// Lo que el ítem 10 promete, entero: dos frontends conectados, un relevo, y
/// los dos vuelven con sus tasks resincronizadas.
///
/// Sin mocks. Dos conexiones de verdad sobre un socket de verdad, y un proceso
/// daemon distinto al final — que es la única forma de que este test signifique
/// algo, porque lo que se está probando es precisamente que sobrevivir a un
/// CAMBIO DE PROCESO es posible.
#[tokio::test]
async fn dos_clientes_sobreviven_a_un_relevo() {
    let h = daemon_con_backend_que_puede_spawnear().await;
    let otro = h.segundo_backend().await;
    let pid_viejo = h.pid_del_daemon().await;

    h.pide_relevo().await;
    h.espera_a_que_el_socket_muera().await;

    // Los dos vuelven…
    assert!(h.backend.ping().await.is_ok());
    assert!(otro.ping().await.is_ok());
    // …al MISMO daemon, que es uno nuevo.
    let pid_nuevo = h.pid_del_daemon().await;
    assert_ne!(pid_nuevo, pid_viejo, "hubo relevo de verdad");
    assert_eq!(
        otro.task_list().await.expect("resync").len(),
        h.backend.task_list().await.expect("resync").len(),
        "y los dos ven el mismo estado tras resincronizar"
    );
}
```

- [ ] **Step 2: run it**

```bash
just t norte-core
```
Expected: PASS with tasks 1–3 in place. If it does not, the pieces work
separately and not together, which is exactly what this test is for.

- [ ] **Step 3: commit**

```bash
git add -A crates/norte-core/tests/daemon.rs
git commit -m "test(core): two clients survive a daemon handover"
```

---

## Task 5: close the branch

- [ ] **Step 1: changelog**

In the user's terms: upgrading norte no longer drops what you had open, and
stopping the daemon still means stopped. Say what a handover does not carry
across — an approved-but-unapplied synchronisation plan has to be re-planned,
because a plan is held by the connection that approved it and that is
deliberate (ADR 0049). Say that a handover refuses while a copy is running,
rather than killing it.

- [ ] **Step 2: dispatch the reviewers**

`protocol-guardian` — **mandatory**, this is a wire change. Give it the specific
questions: whether `ShutdownMode` refusing to degrade on an unknown value is
right for a destructive verb (the rest of the wire degrades), and whether a 0.45
client against a 0.46 daemon and the reverse both land on today's behaviour
rather than a silent wrong answer.

`security-reviewer` — the real question is **the spawn permission**: it lets a
daemon tell a client to start a process. Ask it whether a daemon that is not
ours, or a socket that is not ours, can set that flag, and whether one-shot
consumption is enough.

`rust-reviewer` on the whole diff.

Apply BLOCKER and MAJOR in one pass. Say which MINORs were skipped and why.

- [ ] **Step 3: the one full gate run**

```bash
just ci
```
Foreground, one recipe at a time, never through `| tail`.

- [ ] **Step 4: merge**

Use `superpowers:finishing-a-development-branch`.

---

## What this plan does NOT do

- **Loopback TCP with a token.** Deferred; nothing needs it, and it is network
  surface with its own review.
- **Passing the listening socket between processes.** `SCM_RIGHTS` hands over
  the listener but not the connections already accepted on it, so it buys
  nothing the reconnect does not already give.
- **Carrying retained plans or approvals across the gap.** They are
  per-connection by design; surviving a connection would mean a daemon-side
  client identity, which the security model has not been asked about.
- **Deciding WHEN a handover happens.** That is the packaging item's problem,
  and it should stay something a human or a package manager does.
