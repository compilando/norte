# MCP Compare and Plan Implementation Plan (spec 3, phase B)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give an agent two tools — `compare` and `sync_plan` — so it can say
what differs between two trees and what a synchronisation would do. It does not
get `sync_apply`.

**Architecture:** The eight existing tools are request/response, or start a task
and poll `task.list` for a terminal state. **Neither shape works here**: the rows
of `fs.compare` and the steps of `sync.plan` arrive as *notifications*, and
nothing in the bridge drains them. Rather than build a second notification
router inside `norte-mcp` — the exact thing #155 shows is easy to get wrong —
the bridge lazily opens a `RemoteBackend`, which already owns that pump and
already returns `(TaskRef, mpsc::Receiver<…>)` for both methods. `RemoteBackend`
gains an agent-session constructor so that second connection carries the same
actor; policy scopes are keyed by **session**, not by connection
(`ScopeRegistry::grant(session, …)`), so the agent's granted scopes apply to it
unchanged.

**Tech Stack:** Rust, tokio, `serde_json`, `norte-core::backend::RemoteBackend`,
`norte-proto` methods (**no new ones**), `nextest`.

**Spec:** `docs/superpowers/specs/2026-08-13-compare-sync-surfaces-design.md` §4

**The spec undersold this phase and the spec is wrong, not this plan.** §4 calls
it two tool definitions "dispatched in `call_tool` next to the existing arms".
That is true of the last task only; the first is plumbing the bridge has never
had. Task 4 corrects the spec.

**Non-goals, and they are load-bearing:** no `sync_apply` tool, no actor change,
no approval routing, no proto change. If the work reaches `EMBEDDED_CONN_ID`,
`Actor`, or `norte-proto`, it has left the phase — stop and re-open the spec.

## Progress

| task | state | commit |
| --- | --- | --- |
| 1 — the bridge can consume a stream | done | `b8de8c5` |
| 2 — the `compare` tool | done | `7bd8097` |
| 3 — the `sync_plan` tool | done | `866ef0c` |
| 4 — the ADR, and the spec correction | done | `a18de30` |
| 5 — close the branch | in progress | reviews applied in `HEAD` |

### What the reviews changed, so the plan is not read as the truth

A security review and a coherence review ran over the whole branch. Their
findings were applied in one pass; the ones that contradict the task text above
are:

- **`compare`'s arguments are `left`/`right`, not `a`/`b`** (task 2, step 3).
  The rows answer in `left`/`right`.
- **An empty `criteria` list is an error, not "the wire default".** With no
  rung, `compare` calls every pair `same/presence/unknown` — two trees reported
  identical without comparing anything — and `sync_plan`, under its default
  `on_unknown: copy`, turns each of those into an `Overwrite`. The CLI's
  `--criteria` maps empty to the default only because clap cannot tell absent
  from empty.
- **`complete` needs more than the terminal state** (task 2, step 3, point 5).
  `compare.rows` routes with `OnFull::DropBatch`, so a lost batch still ends the
  task `Completed`; `complete` now also requires that the rows received match
  `TaskProgress::entries_done`, which the payload publishes as `rows_total`.
  `sync_plan` reconciles its steps against the sum of `counts` (`steps_total`).
- **Both tools have a `TASK_WAIT` deadline and return `task_id`.** Unbounded
  waits held one of eight tool slots forever; and the two connections are the
  same actor, so the tools connection really can poll and cancel the streams
  arm's task.
- **Abandoning a tool cancels its walk.** The plan only asked for cancellation
  on truncation (step 3, point 3); dropping the future for any other reason left
  a full-tree read+hash running for nobody.
- **`limit: 0`, a third-party cancellation and the daemon-side `Cancelled` of a
  truncation** all have tests now. The truncation tests, written over five flat
  files, could not have observed a cancellation at all: the walk finished first.
  They seed a slow tree.

## What the implementer needs to know before task 1

Verified against `bbbc19e`.

**The bridge today.** `crates/norte-mcp/src/bridge.rs`: `Bridge { client: Client,
session: String }`. `Bridge::connect` calls `initialize` with
`agent_session: Some(session)` — **that is what binds every mutation to the
agent actor server-side**, and anything that talks to the daemon without it is a
plain user connection with no agent gate. `call_tool(&self, …)` dispatches by
name; tools run concurrently up to `MAX_INFLIGHT_TOOLS = 8`.

**Why `Client` alone cannot do it.** `Client::take_notifications` needs
`&mut self` and `call_tool` has `&self`; and with eight concurrent tools a
single notification stream would have to be demultiplexed by `task_id`. That
demultiplexer exists already, in `RemoteBackend` (`BatchRoutes` in
`crates/norte-core/src/backend.rs`). Reuse it.

**What `RemoteBackend` gives you**, both already routed:

```rust
RemoteBackend::compare(FsCompareParams)
    -> Result<(TaskRef, mpsc::Receiver<CompareRowsBatch>), Error>
RemoteBackend::sync_plan(SyncPlanParams)
    -> Result<(TaskRef, mpsc::Receiver<norte_core::sync::SyncPlanEvent>), Error>
```

**The second connection, stated plainly.** The bridge will hold two daemon
connections with the same `agent_session`. Same actor, same scopes. Different
`conn_id`, which means a plan retained for the `RemoteBackend`'s connection is
not redeemable through the bridge's `Client` connection — irrelevant here,
because no tool applies, and it makes the spec's §2.2 doubly true. Say this in
the rustdoc rather than leaving the next reader to find it.

**Vocabulary comes from `norte-frontend`**, exactly as the CLI's does:
`compare::{verdict_label, confidence_label, criterion_label}` and
`sync::{step_label, undo_label, reason_label, trash_label}`. The tool output is
JSON for a model to read, so it carries the **stable wire values**, not the
localised labels — a tool result that changes with the operator's locale is not
a contract. Add the label only where it is genuinely explanatory, never instead
of the value.

---

## Task 1: The bridge can consume a stream

No tool yet. Just the connection and one test that a stream arrives.

**Files:**
- Modify: `crates/norte-core/src/backend.rs` (`RemoteBackend`, near `connect` at line 2416)
- Modify: `crates/norte-mcp/src/bridge.rs` (the `Bridge` struct and `connect`)
- Test: `crates/norte-mcp/tests/e2e_m3.rs` (it already stands a daemon up — read it first and follow its harness)

- [x] **Step 1: Write the failing test**

In `crates/norte-mcp/tests/e2e_m3.rs`, following whatever harness it already
uses to start a daemon and connect a bridge:

```rust
/// #155 en miniatura: el puente puede ABRIR el brazo que drena notificaciones,
/// y lo abre UNA vez. Sin esto, `compare` y `sync_plan` no tienen por dónde
/// recibir sus filas.
#[tokio::test]
async fn el_puente_abre_su_brazo_de_streams_una_sola_vez() {
    // … levantar daemon + Bridge::connect, como los tests vecinos …
    let primero = bridge.streams().await.expect("primer brazo");
    let segundo = bridge.streams().await.expect("segundo brazo");
    assert!(
        std::ptr::eq(primero, segundo),
        "el brazo se abre perezosamente pero UNA vez: dos conexiones por sesión \
         serían dos conn_id y ningún beneficio"
    );
}
```

**Note for the implementer:** the exact shape of `streams()`'s return
(`&RemoteBackend`, `Arc<RemoteBackend>`, …) is yours to choose; pick what a
`OnceCell` in a `&self` method can hand out, and adjust the assertion to match.
What the test must pin is *lazily, and once*.

- [x] **Step 2: Run it and watch it fail**

```sh
just t norte-mcp
```

Expected: FAIL — no method `streams`.

- [x] **Step 3: Add the agent-session constructor to `RemoteBackend`**

`RemoteBackend::connect` builds `InitializeParams` with `agent_session: None`.
Add a sibling that sets it, rather than changing `connect`'s signature — it has
three callers (`norte-cli`, `norte-tui`, `norte-gui`) and none of them wants
this.

```rust
        /// Como [`Self::connect`], pero declarando `agent_session`: la
        /// conexión queda ligada al actor de agente que el daemon gobierna.
        ///
        /// Existe para el puente MCP, que necesita un brazo capaz de drenar
        /// notificaciones (`fs.compare` y `sync.plan` entregan por ahí) sin
        /// dejar de ser el mismo actor que su conexión de tools. Los scopes de
        /// policy se guardan por SESIÓN (`ScopeRegistry::grant(session, …)`),
        /// no por conexión, así que los permisos concedidos valen igual en las
        /// dos.
        ///
        /// # Errors
        /// Los de [`Self::connect`], más sesión rechazada por el daemon
        /// (charset `[A-Za-z0-9._-]`, 1..=64).
        pub async fn connect_as_agent(
            socket: PathBuf,
            client_info: ClientInfo,
            agent_session: String,
        ) -> Result<Self, Error> {
```

Factor the body so `connect` and `connect_as_agent` share it with
`agent_session: Option<String>` — two copies of a handshake is how they diverge.

- [x] **Step 4: Give the bridge its lazy arm**

```rust
pub struct Bridge {
    client: Client,
    session: String,
    /// El socket, guardado para poder abrir [`Bridge::streams`] al vuelo.
    socket: std::path::PathBuf,
    /// La conexión que drena notificaciones, abierta en la PRIMERA tool que
    /// la necesita.
    ///
    /// Perezosa a propósito: un agente que solo lista y lee jamás la abre, y
    /// una segunda conexión al daemon no es gratis. Una sola, cacheada: dos
    /// serían dos `conn_id` sin ninguna ventaja.
    streams: tokio::sync::OnceCell<RemoteBackend>,
}
```

`Bridge::streams(&self)` resolves the cell with `connect_as_agent(self.socket,
…, self.session.clone())`.

- [x] **Step 5: Run the test and watch it pass**

```sh
just t norte-mcp
```

Expected: PASS.

- [x] **Step 6: Lint and commit**

```sh
just c
git add crates/norte-core/src/backend.rs crates/norte-mcp/src/bridge.rs \
        crates/norte-mcp/tests/e2e_m3.rs
git commit -m "feat(mcp): the bridge can open an arm that drains notifications"
```

---

## Task 2: The `compare` tool

**Files:**
- Modify: `crates/norte-mcp/src/bridge.rs` (`call_tool`, `tool_defs`, a new `tool_compare`)
- Test: `crates/norte-mcp/tests/e2e_m3.rs`

- [x] **Step 1: Write the failing test**

```rust
/// El agente ve QUÉ difiere, con el vocabulario del wire y no con etiquetas
/// traducidas: un resultado de tool que cambia con el idioma del operador no
/// es un contrato.
#[tokio::test]
async fn compare_devuelve_las_filas_con_valores_de_wire() {
    // … daemon + bridge, dos árboles: `a/x.txt` y `b/` vacío …
    let out = bridge
        .tools_call(&json!(1), &json!({
            "name": "compare",
            "arguments": {"a": a_wire, "b": b_wire}
        }), &Default::default())
        .await;
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let texto = v["result"]["content"][0]["text"].as_str().expect("texto");
    let payload: serde_json::Value = serde_json::from_str(texto).expect("payload");
    let filas = payload["rows"].as_array().expect("rows");
    assert!(!filas.is_empty(), "x.txt solo está en un lado");
    assert!(
        filas.iter().any(|f| f["verdict"] == "left_only"),
        "el veredicto viaja como valor de wire: {payload}"
    );
}
```

**Note for the implementer:** check the actual wire spelling of the verdict
(`norte_proto::methods::CompareVerdict`'s serde representation) and assert on
that, not on this plan's guess.

- [x] **Step 2: Run it and watch it fail**

```sh
just t norte-mcp
```

Expected: FAIL — `unknown tool: compare`.

- [x] **Step 3: Implement the tool**

Arm in `call_tool`, definition in `tool_defs`, and:

```rust
    /// `compare`: dos árboles, y qué difiere entre ellos. NO muta nada.
    async fn tool_compare(&self, args: &Value) -> Result<Value, String> {
```

Requirements:

1. Args `{a, b, criteria?, max_depth?, mtime_tolerance_ms?}`. `a`/`b` through
   `vpath_arg`, which the other tools already use.
2. `self.streams().await?.compare(params).await`.
3. **Cap the rows.** `list_dir` caps and paginates; a comparison of two large
   trees would otherwise put a million rows into one tool result and blow the
   model's context. Take a `limit` argument with a default, stop draining at the
   cap, **cancel the task** (`TaskRef::canceller()`), and return a `truncated:
   true` flag saying so. A silent truncation would be worse than the cap.
4. Each row carries the wire values: `verdict`, `confidence`, `criterion`, both
   paths as `to_wire()`. **Never a lossy string** — rule 1.
5. Wait for the task's terminal state and say so in the payload
   (`complete: bool`). An incomplete comparison that looks complete is the
   failure the CLI's exit code 2 exists to prevent; the agent needs the same
   distinction.

- [x] **Step 4: Run the test and watch it pass**

```sh
just t norte-mcp
```

- [x] **Step 5: Lint and commit**

```sh
just c
git add crates/norte-mcp/src/bridge.rs crates/norte-mcp/tests/e2e_m3.rs
git commit -m "feat(mcp): an agent can compare two trees"
```

---

## Task 3: The `sync_plan` tool

**Files:**
- Modify: `crates/norte-mcp/src/bridge.rs`
- Test: `crates/norte-mcp/tests/e2e_m3.rs`

- [x] **Step 1: Write the failing test**

```rust
/// El agente ve QUÉ haría una sincronización, y la descripción de la tool le
/// dice que el hash NO le sirve a nadie más.
#[tokio::test]
async fn sync_plan_devuelve_los_pasos_y_no_aplica_nada() {
    // … daemon + bridge, `src/nuevo.txt`, `dst/` vacío …
    let out = bridge
        .tools_call(&json!(1), &json!({
            "name": "sync_plan",
            "arguments": {"source": src_wire, "dest": dst_wire, "mode": "update"}
        }), &Default::default())
        .await;
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let texto = v["result"]["content"][0]["text"].as_str().expect("texto");
    let payload: serde_json::Value = serde_json::from_str(texto).expect("payload");
    assert_eq!(payload["steps"].as_array().expect("steps").len(), 1);
    assert!(payload["counts"].is_object(), "los totales que el core sí conoce");

    // Y el destino sigue vacío: planear no escribe.
    assert!(!dst.join("nuevo.txt").exists(), "sync_plan no aplica nada");
}

/// No hay tool de aplicar, y eso es la decisión, no un olvido.
#[tokio::test]
async fn no_existe_una_tool_de_aplicar() {
    let defs = /* … tools/list … */;
    assert!(
        !defs.iter().any(|t| t["name"] == "sync_apply"),
        "aplicar es acción de un humano en su propio cliente (spec 3 §2.1)"
    );
}
```

- [x] **Step 2: Run them and watch them fail**

```sh
just t norte-mcp
```

- [x] **Step 3: Implement the tool**

1. Args `{source, dest, mode, criteria?, on_unknown?}`. `mode` is
   `"update"|"mirror"`; a `mode` present but malformed is an **error**, never a
   silent default — `tool_delete` sets that precedent and its comment explains
   why.
2. Drive the `SyncPlanEvent` stream: `Steps` batches then `Done`. Cap and flag
   `truncated` as in Task 2.
3. Payload: the steps (wire values: `kind`, `rel`, `confidence`, `reversal`,
   `reason`), `counts`, `dest_trash`, and `blockers`. **`dest_trash` and the
   blockers are the honest half** — without them the agent reports a plan as
   clean when nothing it deletes could be recovered.
4. **The description must say the hash is not transferable.** Wording to use,
   because getting it wrong makes every agent try:

   > Returns what a one-way synchronisation would do. It does **not** apply
   > anything, and there is no tool that does: applying is a human action in
   > their own client. The plan's hash is retained for **this** connection only,
   > so it cannot be handed to a user or another tool — report what would change
   > and let the human plan it again in their client.

   Do not emit `plan_hash` in the payload at all. A value that cannot be used is
   an invitation to try.

- [x] **Step 4: Run the tests and watch them pass**

```sh
just t norte-mcp
```

- [x] **Step 5: Lint and commit**

```sh
just c
git add crates/norte-mcp/src/bridge.rs crates/norte-mcp/tests/e2e_m3.rs
git commit -m "feat(mcp): an agent can plan a synchronisation it cannot apply"
```

---

## Task 4: The ADR, and the spec correction

- [x] **Step 1: Write the ADR**

Use the `/adr` project command. It records the decision of spec 3 §2.1 and the
two things this phase discovered:

- **Why there is no `sync_apply` tool.** The three facts: `EMBEDDED_CONN_ID`
  runs as `Actor::User` with no policy gate and its own rustdoc says not to wire
  it to scripting; a `plan_hash` is an unkeyed deterministic digest and not a
  secret, so `conn_id` is the only thing binding a plan to its requester; and
  applying is the one operation here whose blast radius is a whole subtree. What
  it would take to add one: an actor of its own for the embedded bridge, an
  `Ask` approval path this method does not have, and a decision about what
  `Mirror` means under a scope.
- **The second connection.** Same session, same actor, same scopes (scopes are
  keyed by session, not connection); different `conn_id`, which makes the
  retained plan unreachable from the tools connection — consistent with the
  decision above rather than in tension with it.
- **Why the bridge borrows `RemoteBackend` instead of routing notifications
  itself.** The eight original tools are request/response by design (ADR 0024,
  "surface = what the wire already offers"); streaming broke that assumption,
  and duplicating `BatchRoutes` was the alternative.

- [x] **Step 2: Correct the spec**

`docs/superpowers/specs/2026-08-13-compare-sync-surfaces-design.md` §4 says
phase B is two tool definitions next to the existing arms, and §8 lists it as
mechanical work for a cheap model. Both are wrong: no existing tool consumes a
stream. Fix both sentences and say what it actually took.

- [x] **Step 3: Commit**

```sh
git add docs/adr/ docs/superpowers/specs/2026-08-13-compare-sync-surfaces-design.md
git commit -m "docs(adr): the agent plans and does not apply"
```

---

## Task 5: Close the branch

- [x] **Step 1: Dispatch the reviewers**

- `security-reviewer` — mandatory, whole branch. This adds an agent-facing
  surface and a second authenticated connection. Ask specifically: can the
  streams arm ever be opened without `agent_session`, i.e. as a plain user; and
  can a capped/truncated result be mistaken by a caller for a complete one.
- `rust-reviewer` — whole diff.

No `protocol-guardian`: nothing here touches `norte-proto`. If something does,
the phase left the spec.

Apply BLOCKER and MAJOR findings in ONE pass. Say which MINORs were skipped.

- [x] **Step 2: A whole-branch coherence review**

Phase A's four blockers were only visible across commits — each task was correct
against its own spec. Do the same pass here, and give it the questions the
per-task reviewers cannot have: do `compare` and `sync_plan` agree on what
"truncated" and "incomplete" mean and report them the same way, and does either
payload let a model conclude "these trees match" from a run that did not finish.

- [ ] **Step 3: One gate run**

```sh
just ci
```

- [ ] **Step 4: Update #162 and merge**

#162 can close with this: phase A gave it a CLI, phase B gives it the agent
surface, and the ADR records why the apply half is deliberate rather than
missing. Follow `superpowers:finishing-a-development-branch`.
