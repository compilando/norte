# #72 — Cancelar un Ask de policy suspendido al cancelar el tools/call — Plan de implementación

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cuando un agente (por el puente MCP) cancela un `tools/call` que está suspendido en un Ask de policy, retirar ese Ask del daemon (fail-closed: nunca aprobar, solo denegar/retirar) para eliminar la divergencia agente↔mundo y liberar el dispatch serial de la conexión.

**Architecture:** Tres componentes, sobre la infra del reader concurrente de #64 ya existente:
- **A (proto):** notificación aditiva `rpc.cancel { id }` (el id JSON-RPC de la request en vuelo). Bump menor `0.18.0 → 0.19.0` + golden + ventana N/N-1.
- **B (daemon):** por cada `fs.copy/move/delete` en vuelo se registra un `CancellationToken` en un mapa `inflight_cancel` por conexión (por id, guard RAII). El inner loop de `serve_connection` gana un brazo que DRENA `inbox_rx` durante un dispatch en vuelo: un `rpc.cancel` cuyo id esté en el mapa dispara ese token (sin romper la conexión, a diferencia de `peer_gone`); cualquier otro frame se bufferiza (dispatch sigue serial). Al dispararse el token, `handle_value` DROPEA el dispatch — el gate del Ask muere pre-efecto (su `PendingGuard` limpia `policy.pending`), jamás aprueba, y la request responde `Error::Cancelled`.
- **C (puente MCP):** al recibir `notifications/cancelled { requestId }`, además de cancelar el token local, reenvía `rpc.cancel { id }` al daemon con el id JSON-RPC de la `fs.*` que lanzó (guardado al lanzarla).

**Decisión de diseño clave (mecanismo B, "drop del dispatch" en vez de "select en el gate"):** El diseño aprobado dejó abierto CÓMO el token cancela el gate ("el gate hace `select!` sobre decide + token"). Este plan elige observar el token en el `select!` de `handle_value` que envuelve `dispatch()` (donde ya vive el `select!` de shutdown), NO dentro del `ApprovalResolver`. Al dispararse, el future de `dispatch` (que contiene el gate suspendido en `copy_with_as(...).await`) se DROPEA; su `PendingGuard` RAII retira la pendiente. Ventajas: (1) **fail-closed por construcción** — dropear un future JAMÁS produce `Approved`, no hay rama que pueda aprobar; (2) **cero plumbing en el engine** — no hay que hilar un token por `copy_with_as`/`move_with_as`/`delete_with_as` → gate → `ApprovalRequest`. El gate del Ask es PRE-efecto y `register_task` es CERO-await tras él (invariante #64 en `dispatch_fs_task`), así que dropear durante el Ask nunca deja una Task corriendo. La protocol-guardian/security-reviewer deben confirmar esta elección.

**Decisión de wire (desenlace de la request retirada):** `Error::Cancelled` (ya existe en proto: "Cancelado por el usuario o por shutdown; estado limpio garantizado"), NO un `PolicyDenied{withdrawn}` nuevo. Razón: (1) semánticamente exacto — el estado ES limpio (gate pre-efecto); (2) no filtra NADA de policy (mejor que una categoría `withdrawn`); (3) NO amplía el vocabulario cerrado de `PolicyDenied.rule`, dejando la superficie de proto en SOLO la notificación aditiva. El agente ni ve esta respuesta (MCP cancel = el puente abandona el tool sin reenviar respuesta); solo la observa un cliente humano/observador y los tests. El diseño aprobado admite explícitamente "un desenlace de cancelación" como alternativa a `PolicyDenied{withdrawn}`. **protocol-guardian debe validar** esta elección.

**Tech Stack:** Rust (workspace Cargo), tokio + `tokio_util::sync::CancellationToken`, `serde`/`serde_json`, JSON-RPC 2.0 NDJSON sobre UDS. Tests con `nextest`. Gate: `just ci` verde local (CI GitHub OFF por billing).

**Reviewers obligatorios:** protocol-guardian (componente A + cualquier handler nuevo), security-reviewer (la retirada del Ask debe ser fail-closed — nunca aprobar, solo retirar/denegar; el desenlace no filtra policy), rust-reviewer (reglas duras de CLAUDE.md). test-engineer opcional para reforzar los tests de carrera.

---

## Estructura de archivos

**Modificados:**
- `crates/norte-proto/src/methods.rs` — const `RPC_CANCEL` + struct `RpcCancelParams` + bump `PROTOCOL_VERSION` + rustdoc de versión. (Componente A)
- `crates/norte-proto/tests/golden/types/methods.json` — fixture `rpc_cancel_params` (wire-freeze). (A)
- `crates/norte-proto/tests/golden_types.rs` — check del golden nuevo + count 62→63 + pin de versión 0.18.0→0.19.0. (A)
- `crates/norte-proto/tests/types.rs` — round-trip de `RpcCancelParams` (Num y Str). (A)
- `crates/norte-core/src/daemon/client.rs` — `Client::notify` (fire-and-forget) + `Client::call_tracked` (expone el id asignado). (B1)
- `crates/norte-core/src/daemon/server.rs` — `InflightCancel` type + `InflightCancelGuard` + `rpc_cancel_id` helper + `handle_value` cancelable-wrap + `serve_connection` inner-loop restructure. (B2/B3)
- `crates/norte-core/tests/daemon.rs` — tests de integración del cancel del Ask. (B4)
- `crates/norte-mcp/src/bridge.rs` — `map_client_err` extraído + `Bridge::call_tracked` + `Bridge::cancel_daemon_request` + `InflightTool` (token + celda daemon-id) + threading de la celda + handler de cancel que reenvía `rpc.cancel`. (C1/C2)
- `crates/norte-mcp/tests/transport.rs` — e2e: el agente cancela → el humano ya no puede aprobar-para-ejecutar. (C3)

Ningún archivo nuevo. Ningún crate nuevo. Ninguna dependencia nueva.

---

## Componente A — proto: notificación `rpc.cancel { id }`

### Task A1: const `RPC_CANCEL` + `RpcCancelParams` + bump de versión

**Files:**
- Modify: `crates/norte-proto/src/methods.rs` (consts ~L238-283; `PROTOCOL_VERSION` L83; rustdoc de versión ~L38-82; structs de params tras ~L283)

- [ ] **Step 1: Añadir la const del método** tras `PLUGIN_PREVIEW` (~L283 en `methods.rs`, junto al resto de consts de método):

```rust
/// `rpc.cancel` — notificación client→server (#72): retira la request en
/// vuelo cuyo `id` JSON-RPC se indica. Best-effort y SIN respuesta: la
/// confirmación real es que la request cancelada responde con su desenlace
/// ([`Error::Cancelled`](crate::Error::Cancelled) si estaba suspendida en un
/// Ask de policy). Es puramente de la CAPA RPC (no `policy.*`): cancela una
/// request, no una aprobación (el peticionario no conoce el `approval_id`, que
/// va al humano). En M3 el único camino largo suspendible en el dispatch es el
/// Ask; una op larga ya-Task se cancela con [`TASK_CANCEL`]. Un `id`
/// desconocido, ya resuelto o no suspendido = no-op benigno. Un daemon N-1 que
/// no la conozca la descarta en silencio (notificación desconocida, ADR 0004):
/// degrada al comportamiento previo (Ask zombi hasta el TTL), no rompe.
pub const RPC_CANCEL: &str = "rpc.cancel";
```

- [ ] **Step 2: Añadir el struct de params** tras los structs de policy (p. ej. tras `PolicyPendingResult`, ~L721 en `methods.rs`):

```rust
/// Params de [`RPC_CANCEL`] (#72): el id de la request en vuelo a cancelar.
///
/// El `id` es el mismo tipo que [`crate::wire::RequestId`] — número (emisor
/// canónico) o string (tolerancia JSON-RPC). No se valida contra un mapa aquí
/// (es una notificación best-effort): el daemon lo coteja con sus requests en
/// vuelo y un id sin correspondencia es un no-op.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcCancelParams {
    /// Id JSON-RPC de la request a cancelar.
    pub id: crate::wire::RequestId,
}
```

Verifica que `RequestId` sea nombrable — está en `crate::wire` (reexport en `wire/mod.rs`). Si `methods.rs` ya importa `wire`, usa la ruta corta; si no, la ruta completa `crate::wire::RequestId` funciona sin `use` nuevo.

- [ ] **Step 3: Bump `PROTOCOL_VERSION`** (L83 de `methods.rs`):

```rust
pub const PROTOCOL_VERSION: &str = "0.19.0";
```

- [ ] **Step 4: Documentar la versión** en el rustdoc de historial de versiones (bloque `//!`-adjacente sobre `PROTOCOL_VERSION`, tras la entrada de 0.18.0 ~L78-82). Añade:

```rust
/// 0.19.0 (#72): notificación `rpc.cancel { id }` (client→server) — retira la
/// request en vuelo suspendida en un Ask de policy. Aditiva sobre 0.18.x (un
/// cliente/daemon N-1 la ignora; degrada al Ask zombi hasta TTL, no rompe).
```

- [ ] **Step 5: Compilar el crate**

Run: `cargo build -p norte-proto`
Expected: PASS (compila; el golden aún no — se ajusta en A2).

- [ ] **Step 6: Commit**

```bash
git add crates/norte-proto/src/methods.rs
git commit -m "feat(proto): #72 notificación rpc.cancel { id } (proto 0.19.0)"
```

### Task A2: golden de `rpc.cancel` (wire-freeze) + round-trip

**Files:**
- Modify: `crates/norte-proto/tests/golden/types/methods.json`
- Modify: `crates/norte-proto/tests/golden_types.rs:349-359` (golden_methods: count) y ~L1089 (pin de versión); añadir el `check_one` del fixture nuevo
- Modify: `crates/norte-proto/tests/types.rs` (round-trip)

- [ ] **Step 1: Escribir el fixture golden** en `methods.json`. Añade la entrada (mantén el orden alfabético del fichero si lo tiene; si no, al final del objeto):

```json
  "rpc_cancel_params": {"id": 7}
```

(`RequestId` es `#[serde(untagged)]`: `Num(7)` serializa como el número desnudo `7`.)

- [ ] **Step 2: Añadir el check del golden** en `golden_types.rs`. Crea una función nueva para la familia RPC-layer y llámala desde `golden_methods`. Tras la línea `check_methods_plugin(&fixtures);` (L357) añade:

```rust
    check_methods_rpc(&fixtures);
```

y define la función (junto a las otras `check_methods_*`):

```rust
/// Familia de la CAPA RPC (0.19.0, #72): `rpc.cancel`.
fn check_methods_rpc(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::RpcCancelParams;
    use norte_proto::wire::RequestId;
    check_one(
        fixtures,
        "rpc_cancel_params",
        &RpcCancelParams {
            id: RequestId::Num(7),
        },
    );
}
```

- [ ] **Step 3: Subir el count** en `golden_methods` (L358):

```rust
    assert_eq!(fixtures.len(), 63, "[methods.json] fixtures sin caso Rust");
```

- [ ] **Step 4: Subir el pin de versión** (L1089, en el test de consts):

```rust
    assert_eq!(norte_proto::PROTOCOL_VERSION, "0.19.0");
```

Añade justo encima el comentario de la versión:

```rust
    // 0.19.0 (#72): rpc.cancel { id }. Aditivo sobre 0.18.x.
    assert_eq!(methods::RPC_CANCEL, "rpc.cancel");
```

- [ ] **Step 5: Round-trip en `types.rs`** — añade un test que cubra ambas formas de `RequestId`:

```rust
#[test]
fn rpc_cancel_params_round_trip_num_y_str() {
    use norte_proto::methods::RpcCancelParams;
    use norte_proto::wire::RequestId;
    for id in [RequestId::Num(42), RequestId::Str("abc".into())] {
        let p = RpcCancelParams { id: id.clone() };
        let wire = serde_json::to_string(&p).expect("serializa");
        let back: RpcCancelParams = serde_json::from_str(&wire).expect("deserializa");
        assert_eq!(back.id, id);
    }
    // La forma canónica (Num) es el número desnudo en `id`.
    assert_eq!(
        serde_json::to_value(RpcCancelParams { id: RequestId::Num(7) }).unwrap(),
        serde_json::json!({"id": 7}),
    );
}
```

- [ ] **Step 6: Correr los tests de golden + round-trip**

Run: `cargo nextest run -p norte-proto`
Expected: PASS (golden_methods, golden consts, rpc_cancel_params_round_trip_num_y_str).

- [ ] **Step 7: clippy del crate**

Run: `cargo clippy -p norte-proto --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/norte-proto/tests/golden/types/methods.json crates/norte-proto/tests/golden_types.rs crates/norte-proto/tests/types.rs
git commit -m "test(proto): #72 golden rpc.cancel (wire-freeze) + round-trip"
```

**→ REVIEWER: protocol-guardian OBLIGATORIO tras A2** (cambio de wire format). Confirmar: nombre `rpc.cancel`, forma `{ id }`, bump menor correcto, ventana N/N-1 (N-1 = 0.18.x), golden pinneado, y la elección de `Error::Cancelled` como desenlace (no vocabulario nuevo en `PolicyDenied`).

---

## Componente B — daemon

### Task B1: primitivas del `Client` — `notify` + `call_tracked`

Necesarias tanto por los tests de integración de B4 (que actúan de agente y deben enviar `rpc.cancel` con el id de su `fs.copy`) como por el puente (C). El `Client` hoy asigna el id INTERNO en `call()` y no lo expone, ni sabe enviar notificaciones.

**Files:**
- Modify: `crates/norte-core/src/daemon/client.rs:220-251` (añadir métodos tras `call`)
- Test: `crates/norte-core/src/daemon/client.rs` (mod tests) o `crates/norte-core/tests/connect.rs`

- [ ] **Step 1: Escribir el test que falla** — el id que `call_tracked` reporta es el mismo que el daemon ve (round-trip con una notificación). Añade a `crates/norte-core/tests/connect.rs` (usa el daemon de prueba de ese fichero; si el helper de spawn vive en `daemon.rs`, replica el patrón mínimo o mueve el test a `daemon.rs`). Test unitario mínimo del `Client` SIN daemon real para `notify` + `call_tracked` no es posible (necesitan I/O); el test real va en B4. Aquí, un test de compilación/uso:

```rust
#[tokio::test]
async fn call_tracked_reporta_el_id_asignado() {
    // Contra un daemon que responde a fs.stat: call_tracked invoca on_id ANTES
    // de esperar la respuesta y ese id es monotónico creciente.
    // (Usa el helper de spawn de daemon del módulo de tests.)
    let d = spawn_test_daemon().await;
    let client = connected_client(&d).await;
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
    let s = seen.clone();
    let _: Result<norte_proto::methods::FsStatResult, _> = client
        .call_tracked(
            norte_proto::methods::FS_STAT,
            &norte_proto::methods::FsStatParams { path: vp("mem:///nope") },
            move |id| s.lock().unwrap().push(id),
        )
        .await;
    assert_eq!(seen.lock().unwrap().len(), 1, "on_id se invoca exactamente una vez");
    assert!(seen.lock().unwrap()[0] > 0);
}
```

Run: `cargo nextest run -p norte-core call_tracked_reporta_el_id_asignado`
Expected: FAIL con "no method named `call_tracked`".

- [ ] **Step 2: Implementar `call_tracked` y `notify`** en `client.rs`, tras `call` (L251). Refactoriza `call` para delegar en `call_tracked` (DRY):

```rust
    /// Como [`Self::call`], pero invoca `on_id` con el id JSON-RPC asignado
    /// ANTES de esperar la respuesta — para que el llamante lo correlacione
    /// (p. ej. enviar un `rpc.cancel` de esa request mientras sigue en vuelo,
    /// #72).
    ///
    /// # Errors
    /// Iguales que [`Self::call`].
    ///
    /// # Panics
    /// Nunca: el lock interno no puede envenenarse.
    pub async fn call_tracked<P, R>(
        &self,
        method: &str,
        params: &P,
        on_id: impl FnOnce(u64),
    ) -> Result<R, ClientError>
    where
        P: serde::Serialize,
        R: serde::de::DeserializeOwned,
    {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        on_id(id);
        let params = serde_json::to_value(params)?;
        let req = Request {
            jsonrpc: JsonRpcVersion,
            id: RequestId::Num(id),
            method: method.to_owned(),
            params: Some(params),
        };
        let (tx, rx) = oneshot::channel();
        self.pending.lock().expect("pending lock sano").insert(id, tx);
        if self.closed.load(Ordering::SeqCst) {
            self.pending.lock().expect("pending lock sano").remove(&id);
            return Err(ClientError::ConnectionClosed);
        }
        let frame = encode_frame(&req)?;
        if self.frames_out.send(frame).is_err() {
            self.pending.lock().expect("pending lock sano").remove(&id);
            return Err(ClientError::ConnectionClosed);
        }
        let value = rx.await.map_err(|_| ClientError::ConnectionClosed)??;
        Ok(serde_json::from_value(value)?)
    }

    /// Envía una notificación JSON-RPC (sin id, sin respuesta): fire-and-forget.
    /// La usa el puente para reenviar `rpc.cancel` (#72). Si la conexión ya
    /// murió, se descarta en silencio (best-effort, como el propio `rpc.cancel`).
    ///
    /// # Errors
    /// [`ClientError`] solo si los params no serializan; un canal muerto NO es
    /// error (best-effort).
    pub fn notify<P: serde::Serialize>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<(), ClientError> {
        let notif = Notification {
            jsonrpc: JsonRpcVersion,
            method: method.to_owned(),
            params: Some(serde_json::to_value(params)?),
        };
        let frame = encode_frame(&notif)?;
        let _ = self.frames_out.send(frame); // canal muerto = no-op best-effort
        Ok(())
    }
```

Y reemplaza el cuerpo de `call` (L220-251) por una delegación:

```rust
    pub async fn call<P, R>(&self, method: &str, params: &P) -> Result<R, ClientError>
    where
        P: serde::Serialize,
        R: serde::de::DeserializeOwned,
    {
        self.call_tracked(method, params, |_| {}).await
    }
```

Asegura que `Notification` esté importado en `client.rs` (ya se usa el tipo en el receptor de notifs; si el `use` solo trae el receiver, añade `Notification` al `use norte_proto::wire::{...}`).

- [ ] **Step 3: Correr el test**

Run: `cargo nextest run -p norte-core call_tracked_reporta_el_id_asignado`
Expected: PASS.

- [ ] **Step 4: clippy + los tests de conexión existentes** (que `call` siga funcionando idéntico):

Run: `cargo nextest run -p norte-core --test connect && cargo clippy -p norte-core --all-targets -- -D warnings`
Expected: PASS (sin regresión en `connect.rs`).

- [ ] **Step 5: Commit**

```bash
git add crates/norte-core/src/daemon/client.rs crates/norte-core/tests/connect.rs
git commit -m "feat(core): #72 Client::call_tracked + notify (id en vuelo + rpc.cancel)"
```

### Task B2: `inflight_cancel`, guard, helper y wrap de `handle_value`

**Files:**
- Modify: `crates/norte-core/src/daemon/server.rs` (tipos/helpers junto a `ConnState` ~L866; `handle_value` L1228-1294)

- [ ] **Step 1: Añadir el type alias, el guard y el helper** en `server.rs` (junto a `ConnState`, ~L866, o antes de `handle_value`). Confirma que `VecDeque` está importado; si no, añade `use std::collections::VecDeque;` (se usa en B3). `RequestId`, `HashMap`, `Arc`, `Mutex`, `CancellationToken` ya están en scope:

```rust
/// Tokens de cancelación de las requests en vuelo cancelables (#72), por id
/// JSON-RPC. Vive FUERA de `ConnState` (que `handle_value` toma `&mut`): el
/// inner loop de `serve_connection` lo consulta EN PARALELO a un dispatch en
/// vuelo para disparar el token de un `rpc.cancel`. `Arc<Mutex<…>>` porque hay
/// dos dueños concurrentes: el loop (dispara) y `handle_value` (registra/retira).
type InflightCancel = Arc<Mutex<HashMap<RequestId, CancellationToken>>>;

/// Guard RAII del token en vuelo (#72): retira el id del mapa a CUALQUIER
/// salida del dispatch (respuesta normal, withdrawal por cancel, shutdown, o
/// drop por muerte del peer) — jamás un token huérfano que un `rpc.cancel`
/// tardío dispararía sobre una request ya resuelta.
struct InflightCancelGuard {
    map: InflightCancel,
    id: RequestId,
}

impl Drop for InflightCancelGuard {
    fn drop(&mut self) {
        self.map
            .lock()
            .expect("inflight_cancel lock sano")
            .remove(&self.id);
    }
}

/// Extrae el `id` de un frame `rpc.cancel` ya parseado (#72), o `None` si el
/// frame no es un `rpc.cancel` bien formado. Se aplica a los frames leídos del
/// inbox DURANTE un dispatch en vuelo — clasificación estructural sobre el
/// `Value`, sin deserializar el envelope entero.
fn rpc_cancel_id(value: &serde_json::Value) -> Option<RequestId> {
    if value.get("method").and_then(serde_json::Value::as_str) != Some(methods::RPC_CANCEL) {
        return None;
    }
    let id = value.get("params")?.get("id")?;
    serde_json::from_value::<RequestId>(id.clone()).ok()
}
```

- [ ] **Step 2: Cambiar la firma de `handle_value`** (L1228) para recibir el mapa:

```rust
async fn handle_value(
    value: serde_json::Value,
    conn_id: u64,
    tx: &mpsc::Sender<Arc<[u8]>>,
    conn: &mut ConnState,
    shared: &Arc<Shared>,
    inflight_cancel: &InflightCancel,
) {
```

- [ ] **Step 3: Envolver el dispatch de las requests cancelables.** Reemplaza el bloque `let response = tokio::select! { … }` (L1255-1263) por:

```rust
            // #72: fs.copy/move/delete pueden suspenderse en un Ask de policy.
            // Se registra un token por su id para que un `rpc.cancel` (leído
            // por el loop de serve_connection en paralelo) retire el Ask
            // dropeando este dispatch — el gate muere PRE-efecto (su
            // PendingGuard limpia policy.pending) y JAMÁS aprueba (fail-closed
            // por construcción: dropear un future no puede devolver Approved).
            let cancelable = matches!(
                req.method.as_str(),
                methods::FS_COPY | methods::FS_MOVE | methods::FS_DELETE
            );
            let response = if cancelable {
                let cancel = CancellationToken::new();
                inflight_cancel
                    .lock()
                    .expect("inflight_cancel lock sano")
                    .insert(id.clone(), cancel.clone());
                let _cancel_guard = InflightCancelGuard {
                    map: Arc::clone(inflight_cancel),
                    id: id.clone(),
                };
                tokio::select! {
                    biased;
                    // Una op que COMPLETA (aprobada, o rechazada por policy)
                    // gana a un cancel simultáneo: no se retira lo ya resuelto.
                    r = dispatch(req, conn_id, conn, shared) => r,
                    () = shared.shutdown.cancelled() => Err(RpcError::protocol(
                        codes::INTERNAL_ERROR,
                        "daemon shutting down",
                    )),
                    // El agente retiró la request suspendida en el Ask: estado
                    // limpio garantizado (gate pre-efecto), sin filtrar policy.
                    () = cancel.cancelled() => Err(RpcError::from(norte_proto::Error::Cancelled)),
                }
            } else {
                tokio::select! {
                    r = dispatch(req, conn_id, conn, shared) => r,
                    () = shared.shutdown.cancelled() => Err(RpcError::protocol(
                        codes::INTERNAL_ERROR,
                        "daemon shutting down",
                    )),
                }
            };
```

(El `let id = req.id.clone();` de L1251 ya existe y provee la clave; `norte_proto::Error` es nombrable por ruta completa sin `use` nuevo.)

- [ ] **Step 4: Compilar** (el callsite de `handle_value` aún no pasa el mapa; fallará hasta B3 — es esperado). Verifica que solo falta el argumento:

Run: `cargo build -p norte-core 2>&1 | head -20`
Expected: FAIL SOLO por el arg faltante en la llamada a `handle_value` (line ~1116). Sin otros errores de tipo.

- [ ] **Step 5: (sin commit todavía — continúa en B3, el crate no compila hasta cerrar el callsite).**

### Task B3: `serve_connection` — inner loop drena el inbox

**Files:**
- Modify: `crates/norte-core/src/daemon/server.rs:1097-1127` (creación de `conn` + loop principal)

- [ ] **Step 1: Crear el mapa `inflight_cancel`** justo tras `let mut conn = ConnState::new();` (L1097):

```rust
    let mut conn = ConnState::new();
    // #72: tokens de cancelación de las requests cancelables en vuelo.
    let inflight_cancel: InflightCancel = Arc::default();
```

- [ ] **Step 2: Reescribir el loop principal** (L1104-1127) para: (a) tomar el siguiente frame del búfer local o del inbox (con shutdown/sweep entre dispatches, como hoy); (b) despachar ESE frame vigilando en paralelo el inbox — `rpc.cancel` de un id en vuelo dispara su token, `peer_gone` cierra (#64), cualquier otro frame se bufferiza (dispatch serial):

```rust
    // #72: frames leídos del inbox DURANTE un dispatch en vuelo que NO son un
    // rpc.cancel de la request en vuelo se bufferizan aquí y se procesan tras
    // el desenlace — el dispatch sigue SERIAL (orden de frames = orden de
    // ejecución). Un agente MCP no pipelinea, así que en la práctica el único
    // frame durante un Ask es el cancel o el EOF.
    let mut pending_frames: VecDeque<serde_json::Value> = VecDeque::new();
    let result: std::io::Result<()> = loop {
        // Siguiente frame: primero el búfer local, luego el inbox (con
        // shutdown/sweep atendidos SOLO entre dispatches, como antes de #72).
        let value = if let Some(v) = pending_frames.pop_front() {
            v
        } else {
            tokio::select! {
                msg = inbox_rx.recv() => match msg {
                    None => break Ok(()),
                    Some(v) => v,
                },
                () = shared.shutdown.cancelled() => break Ok(()),
                _ = sweep.tick() => {
                    conn.sweep_expired(shared.listing_ttl);
                    continue;
                }
            }
        };
        // Despacha vigilando el inbox en paralelo (#72 + #64).
        let dispatch = handle_value(value, conn_id, &tx, &mut conn, shared, &inflight_cancel);
        tokio::pin!(dispatch);
        let peer_died = loop {
            tokio::select! {
                biased;
                // Un dispatch que completa, completa (incluida la respuesta
                // Cancelled del withdrawal): gana a la muerte del peer.
                () = &mut dispatch => break false,
                // Un dispatch SUSPENDIDO muere con su peticionario (#64).
                () = peer_gone.cancelled() => break true,
                // Frames que llegan mientras este dispatch sigue en vuelo:
                msg = inbox_rx.recv() => match msg {
                    // El reader terminó (EOF/shutdown): deja completar el
                    // dispatch y cierra por el camino común.
                    None => {
                        (&mut dispatch).await;
                        break true;
                    }
                    Some(frame) => {
                        if let Some(id) = rpc_cancel_id(&frame) {
                            // rpc.cancel: dispara el token de esa request si
                            // está en vuelo. NO rompe la conexión (a diferencia
                            // de peer_gone). Id desconocido/ya resuelto = no-op.
                            if let Some(tok) = inflight_cancel
                                .lock()
                                .expect("inflight_cancel lock sano")
                                .get(&id)
                            {
                                tok.cancel();
                            }
                        } else {
                            // Cualquier otro frame: se procesa TRAS el
                            // desenlace (dispatch serial).
                            pending_frames.push_back(frame);
                        }
                    }
                }
            }
        };
        if peer_died {
            break Ok(());
        }
    };
```

Nota: el `sweep` sigue solo entre dispatches (igual que hoy: un dispatch suspendido retiene el tick — comportamiento y comentario preexistentes en L1098-1101, sin regresión).

- [ ] **Step 3: Compilar el crate**

Run: `cargo build -p norte-core`
Expected: PASS.

- [ ] **Step 4: Correr TODA la suite del daemon existente** (sin regresión — los tests de Ask, #64 EOF, ping concurrente, etc. deben seguir verdes):

Run: `cargo nextest run -p norte-core --test daemon`
Expected: PASS (todos los existentes: `ask_aprobado_desbloquea_la_copia`, `ask_denegado_…`, `ask_sin_decision_vence_por_ttl`, etc.).

- [ ] **Step 5: clippy del crate**

Run: `cargo clippy -p norte-core --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit** (B2+B3 juntos: el crate solo compila con ambos)

```bash
git add crates/norte-core/src/daemon/server.rs
git commit -m "feat(core): #72 el dispatch suspendido observa rpc.cancel y retira el Ask (fail-closed)"
```

### Task B4: tests de integración del cancel del Ask

**Files:**
- Modify: `crates/norte-core/tests/daemon.rs` (nueva sección tras los tests del Ask, ~tras L1046; reusa `spawn_daemon_ask`, `connected_agent`, `connected_client`, `grant_copy_scope`, `copy_params`, `next_approval`, `wait helpers`)

- [ ] **Step 1: Test — Ask suspendido + `rpc.cancel` → `Cancelled`, pending vacío, pipeline liberado.** Añade:

```rust
// ---------- #72: cancelar un Ask suspendido con rpc.cancel ----------

/// Espera (con tope) a que `policy.pending` del humano tenga `n` entradas.
async fn wait_pending_len(human: &Client, n: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let r: norte_proto::methods::PolicyPendingResult = human
            .call(methods::POLICY_PENDING, &serde_json::json!({}))
            .await
            .expect("policy.pending");
        if r.pending.len() == n {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "pending nunca llegó a {n}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn rpc_cancel_retira_el_ask_y_responde_cancelled() {
    let d = spawn_daemon_ask(Duration::from_secs(30)).await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir proj");
    write_file(&d.mem, "mem:///proj/src.txt", b"hola").await;
    let agent = Arc::new(connected_agent(&d, "s1").await);
    let mut human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await;

    // La copia se suspende en el Ask; capturamos su id JSON-RPC.
    let copy_id = Arc::new(std::sync::Mutex::new(None::<u64>));
    let (a, cid) = (Arc::clone(&agent), Arc::clone(&copy_id));
    let copy = tokio::spawn(async move {
        a.call_tracked::<_, FsTaskResult>(
            methods::FS_COPY,
            &copy_params("mem:///proj/src.txt", "mem:///proj/dst.txt"),
            move |id| *cid.lock().unwrap() = Some(id),
        )
        .await
    });

    // El humano ve la pendiente (prueba que el Ask está realmente suspendido).
    let _notif = next_approval(&mut human).await;
    wait_pending_len(&human, 1).await;
    let id = copy_id.lock().unwrap().expect("call_tracked reportó el id");

    // El agente retira su request en vuelo.
    agent
        .notify(
            methods::RPC_CANCEL,
            &norte_proto::methods::RpcCancelParams { id: RequestId::Num(id) },
        )
        .expect("notify");

    // La copia responde Cancelled (estado limpio garantizado).
    let out = copy.await.expect("join");
    match out {
        Err(ClientError::Rpc(rpc)) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::Cancelled)),
            "esperaba Error::Cancelled, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc(Cancelled), fue {other:?}"),
    }
    // El Ask ya no está pendiente (el PendingGuard limpió).
    wait_pending_len(&human, 0).await;
    // El destino NUNCA se creó (gate pre-efecto): fail-closed.
    assert!(d.mem.stat(&vp("mem:///proj/dst.txt")).await.is_err(), "no debe existir dst");

    // El pipeline de la conexión del agente quedó libre: una request nueva
    // progresa (un stat responde).
    let _e = agent
        .call::<_, norte_proto::methods::FsStatResult>(
            methods::FS_STAT,
            &norte_proto::methods::FsStatParams { path: vp("mem:///proj/src.txt") },
        )
        .await
        .expect("la conexión sigue viva y serial-libre");
}
```

Ajusta los `use` de `daemon.rs`: añade `RequestId` (`use norte_proto::wire::RequestId;`) y `RpcCancelParams` si no están. Verifica el nombre real del método de stat del `MemProvider` (`stat` en el trait `Provider`).

Run: `cargo nextest run -p norte-core rpc_cancel_retira_el_ask_y_responde_cancelled`
Expected: PASS.

- [ ] **Step 2: Test — carrera cancel-vs-decide (cancel primero gana; decide posterior es no-op).**

```rust
#[tokio::test]
async fn cancel_gana_a_un_decide_posterior() {
    let d = spawn_daemon_ask(Duration::from_secs(30)).await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir proj");
    write_file(&d.mem, "mem:///proj/src.txt", b"hola").await;
    let agent = Arc::new(connected_agent(&d, "s1").await);
    let mut human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await;

    let copy_id = Arc::new(std::sync::Mutex::new(None::<u64>));
    let (a, cid) = (Arc::clone(&agent), Arc::clone(&copy_id));
    let copy = tokio::spawn(async move {
        a.call_tracked::<_, FsTaskResult>(
            methods::FS_COPY,
            &copy_params("mem:///proj/src.txt", "mem:///proj/dst.txt"),
            move |id| *cid.lock().unwrap() = Some(id),
        )
        .await
    });
    let notif = next_approval(&mut human).await;
    wait_pending_len(&human, 1).await;
    let id = copy_id.lock().unwrap().expect("id");

    // Cancel primero: retira el Ask.
    agent
        .notify(methods::RPC_CANCEL, &norte_proto::methods::RpcCancelParams { id: RequestId::Num(id) })
        .expect("notify");
    let out = copy.await.expect("join");
    assert!(matches!(out, Err(ClientError::Rpc(ref rpc)) if matches!(rpc.data, Some(norte_proto::Error::Cancelled))));

    // Un decide POSTERIOR sobre esa pendiente ya retirada es no-op:
    // el daemon lo traduce a INVALID_PARAMS (pendiente inexistente), como el
    // decide de un id desconocido.
    let decide: Result<PolicyDecideResult, _> = human
        .call(
            methods::POLICY_DECIDE,
            &PolicyDecideParams { approval_id: notif.approval_id, approve: true },
        )
        .await;
    assert!(decide.is_err(), "decide sobre una pendiente retirada falla");
    // Y sigue sin existir el destino.
    assert!(d.mem.stat(&vp("mem:///proj/dst.txt")).await.is_err());
}
```

Run: `cargo nextest run -p norte-core cancel_gana_a_un_decide_posterior`
Expected: PASS. (Verifica el contrato de `handle_policy_decide` para un id inexistente: si NO devuelve error sino un ack vacío, ajusta la aserción a "no-op sin efecto en el FS" en vez de `is_err`. Consulta `handle_policy_decide` en `server.rs:1598` y su test existente para el desenlace exacto de un id desconocido.)

- [ ] **Step 3: Test — decide primero gana; un `rpc.cancel` posterior es no-op benigno.**

```rust
#[tokio::test]
async fn decide_gana_a_un_cancel_posterior() {
    let d = spawn_daemon_ask(Duration::from_secs(30)).await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir proj");
    write_file(&d.mem, "mem:///proj/src.txt", b"hola").await;
    let agent = Arc::new(connected_agent(&d, "s1").await);
    let mut human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await;

    let copy_id = Arc::new(std::sync::Mutex::new(None::<u64>));
    let (a, cid) = (Arc::clone(&agent), Arc::clone(&copy_id));
    let copy = tokio::spawn(async move {
        a.call_tracked::<_, FsTaskResult>(
            methods::FS_COPY,
            &copy_params("mem:///proj/src.txt", "mem:///proj/dst.txt"),
            move |id| *cid.lock().unwrap() = Some(id),
        )
        .await
    });
    let notif = next_approval(&mut human).await;
    wait_pending_len(&human, 1).await;
    let id = copy_id.lock().unwrap().expect("id");

    // Decide aprueba: la copia procede y devuelve una Task.
    let _: PolicyDecideResult = human
        .call(methods::POLICY_DECIDE, &PolicyDecideParams { approval_id: notif.approval_id, approve: true })
        .await
        .expect("decide approve");
    let res = copy.await.expect("join").expect("aprobada procede");
    assert!(res.task_id.get() > 0);

    // Un rpc.cancel de ese id YA RESUELTO es no-op (el id no está en el mapa):
    // no cuelga ni afecta a la conexión — un stat siguiente responde.
    agent
        .notify(methods::RPC_CANCEL, &norte_proto::methods::RpcCancelParams { id: RequestId::Num(id) })
        .expect("notify");
    let _e = agent
        .call::<_, norte_proto::methods::FsStatResult>(
            methods::FS_STAT,
            &norte_proto::methods::FsStatParams { path: vp("mem:///proj/src.txt") },
        )
        .await
        .expect("conexión viva tras un cancel tardío");
}
```

Run: `cargo nextest run -p norte-core decide_gana_a_un_cancel_posterior`
Expected: PASS.

- [ ] **Step 4: Test — `rpc.cancel` de un id desconocido es no-op benigno** (no cuelga, no rompe la conexión):

```rust
#[tokio::test]
async fn rpc_cancel_de_id_desconocido_es_no_op() {
    let d = spawn_daemon_ask(Duration::from_secs(30)).await;
    let agent = connected_agent(&d, "s1").await;
    // Nadie en vuelo: un cancel de un id inventado no debe romper nada.
    agent
        .notify(methods::RPC_CANCEL, &norte_proto::methods::RpcCancelParams { id: RequestId::Num(9999) })
        .expect("notify");
    // La conexión sigue sirviendo requests.
    let r: norte_proto::methods::TaskListResult = agent
        .call(methods::TASK_LIST, &norte_proto::methods::TaskListParams {})
        .await
        .expect("task.list responde");
    assert!(r.tasks.is_empty() || !r.tasks.is_empty()); // solo confirma que responde
}
```

Run: `cargo nextest run -p norte-core rpc_cancel_de_id_desconocido_es_no_op`
Expected: PASS.

- [ ] **Step 5: Confirmar la NO-regresión de #64** — el test de EOF/peer_gone durante un Ask ya existe en la suite (busca en `daemon.rs`/`transport.rs` el que cierra la conexión con un tool suspendido). Si NO hay uno a nivel `daemon.rs`, añade:

```rust
#[tokio::test]
async fn eof_durante_el_ask_sigue_cerrando_la_conexion() {
    let d = spawn_daemon_ask(Duration::from_secs(30)).await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir proj");
    write_file(&d.mem, "mem:///proj/src.txt", b"hola").await;
    let agent = Arc::new(connected_agent(&d, "s1").await);
    let mut human = connected_client(&d).await;
    grant_copy_scope(&agent, &human, "s1").await;

    let a = Arc::clone(&agent);
    let copy = tokio::spawn(async move {
        a.call::<_, FsTaskResult>(
            methods::FS_COPY,
            &copy_params("mem:///proj/src.txt", "mem:///proj/dst.txt"),
        )
        .await
    });
    let _ = next_approval(&mut human).await;
    wait_pending_len(&human, 1).await;
    // Cerrar la conexión del agente (drop del Client) = EOF: peer_gone cierra
    // el dispatch suspendido (#64) y la pendiente se limpia.
    drop(agent);
    drop(copy); // no esperamos su respuesta: el peer murió
    wait_pending_len(&human, 0).await;
    assert!(d.mem.stat(&vp("mem:///proj/dst.txt")).await.is_err());
}
```

(Nota: `agent` es `Arc<Client>`; `drop(agent)` solo cierra si es la última ref. El `copy` task tiene un clon del Arc — por eso `drop(copy)` también. Si el `abort` es más fiable, usa `copy.abort()` antes de `drop(agent)`. Verifica que el patrón de cierre coincide con cómo los tests de #64 existentes fuerzan el EOF.)

Run: `cargo nextest run -p norte-core eof_durante_el_ask_sigue_cerrando_la_conexion`
Expected: PASS.

- [ ] **Step 6: Suite completa del daemon + clippy**

Run: `cargo nextest run -p norte-core --test daemon && cargo clippy -p norte-core --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/norte-core/tests/daemon.rs
git commit -m "test(core): #72 rpc.cancel retira el Ask — cancel/decide/EOF/id-desconocido"
```

**→ REVIEWERS tras B4: security-reviewer (fail-closed: el cancel jamás aprueba; el desenlace Cancelled no filtra policy; el guard RAII no deja pendientes ni tokens huérfanos) + rust-reviewer (reglas duras: sin unwrap fuera de tests, locks sin await cruzado, tipado de errores).**

---

## Componente C — puente MCP

### Task C1: recordar el id JSON-RPC de la `fs.*` en vuelo

**Files:**
- Modify: `crates/norte-mcp/src/bridge.rs` (`call`/error mapping L338-358; `tool_transfer` L226-257; `tool_delete` L259-277; `tools_call` L119-128; `call_tool` L132; `InflightGuard`/map de `serve_transport` L626, L699-711)

- [ ] **Step 1: Extraer el mapeo de error del `Client`** a una función libre (para reusar en `call` y `call_tracked`). Sobre `impl Bridge`, añade:

```rust
/// Traduce un error del `Client` al texto de tool (la taxonomía en `data`;
/// un `PolicyDenied` sale ACCIONABLE). Compartido por `call` y `call_tracked`.
fn map_client_err(e: ClientError) -> String {
    match e {
        ClientError::Rpc(rpc) => match rpc.data {
            Some(norte_proto::Error::PolicyDenied { ref rule }) => format!(
                "denied by policy ({rule}). If out-of-scope, call request_scope and ask the human to grant it."
            ),
            Some(err) => format!("{err}"),
            None => format!("rpc error {}: {}", rpc.code, rpc.message),
        },
        other => format!("daemon unreachable: {other}"),
    }
}
```

y reduce `call` (L340-358) a:

```rust
    async fn call<P, R>(&self, method: &str, params: &P) -> Result<R, String>
    where
        P: serde::Serialize,
        R: serde::de::DeserializeOwned,
    {
        self.client.call(method, params).await.map_err(map_client_err)
    }

    /// Como `call`, pero registra el id JSON-RPC asignado en `daemon_id` (una
    /// celda `OnceLock`) ANTES de suspenderse — para que el handler de
    /// `notifications/cancelled` pueda reenviar un `rpc.cancel` de esa request
    /// mientras sigue suspendida en un Ask (#72).
    async fn call_tracked<P, R>(&self, method: &str, params: &P, daemon_id: &DaemonIdCell) -> Result<R, String>
    where
        P: serde::Serialize,
        R: serde::de::DeserializeOwned,
    {
        self.client
            .call_tracked(method, params, |id| {
                // OnceLock: el PRIMER `fs.*` mutante de este tool fija el id;
                // los polls de task.list posteriores NO lo pisan.
                let _ = daemon_id.set(id);
            })
            .await
            .map_err(map_client_err)
    }
```

Añade el type alias cerca del top de `bridge.rs`:

```rust
/// Celda del id JSON-RPC de la `fs.*` mutante en vuelo de un tool (#72).
type DaemonIdCell = Arc<std::sync::OnceLock<u64>>;
```

- [ ] **Step 2: Hilar la celda por los tools mutantes.** Cambia `tools_call` (L119) para recibirla y pasarla a `call_tool`:

```rust
    pub async fn tools_call(&self, id: &Value, params: &Value, daemon_id: &DaemonIdCell) -> String {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        match self.call_tool(name, &args, daemon_id).await {
            Ok(v) => rpc_result(id, &tool_content(&v, false)),
            Err(text) => rpc_result(id, &tool_content(&json!(text), true)),
        }
    }
```

`call_tool` (L132) recibe la celda y la pasa SOLO a los mutantes:

```rust
    async fn call_tool(&self, name: &str, args: &Value, daemon_id: &DaemonIdCell) -> Result<Value, String> {
        match name {
            "list_dir" => self.tool_list_dir(args).await,
            "stat" => self.tool_stat(args).await,
            "read_file" => self.tool_read_file(args).await,
            "copy" | "move" => self.tool_transfer(name, args, daemon_id).await,
            "delete" => self.tool_delete(args, daemon_id).await,
            "task_status" => self.tool_task_status(args).await,
            "request_scope" => self.tool_request_scope(args).await,
            other => Err(format!("unknown tool: {other}")),
        }
    }
```

(Copia los brazos exactos del `match` actual de `call_tool` — verifica los nombres de tool reales en L133-146.)

`tool_transfer` (L226) usa `call_tracked` para la mutación:

```rust
    async fn tool_transfer(&self, name: &str, args: &Value, daemon_id: &DaemonIdCell) -> Result<Value, String> {
        let from = vpath_arg(args, "from")?;
        let to = vpath_arg(args, "to")?;
        let r: methods::FsTaskResult = if name == "copy" {
            self.call_tracked(
                methods::FS_COPY,
                &methods::FsCopyParams {
                    from, to,
                    on_collision: norte_proto::CollisionPolicy::default(),
                    symlinks: norte_proto::SymlinkPolicy::default(),
                    resume: norte_proto::ResumePolicy::default(),
                    verify: norte_proto::VerifyPolicy::default(),
                },
                daemon_id,
            )
            .await?
        } else {
            self.call_tracked(
                methods::FS_MOVE,
                &methods::FsMoveParams {
                    from, to,
                    on_collision: norte_proto::CollisionPolicy::default(),
                    symlinks: norte_proto::SymlinkPolicy::default(),
                    resume: norte_proto::ResumePolicy::default(),
                    verify: norte_proto::VerifyPolicy::default(),
                },
                daemon_id,
            )
            .await?
        };
        self.wait_terminal(r.task_id).await
    }
```

`tool_delete` (L259):

```rust
    async fn tool_delete(&self, args: &Value, daemon_id: &DaemonIdCell) -> Result<Value, String> {
        let path = vpath_arg(args, "path")?;
        let mode = match args.get("mode") {
            None | Some(Value::Null) => DeleteMode::Trash,
            Some(Value::String(s)) if s == "trash" => DeleteMode::Trash,
            Some(Value::String(s)) if s == "permanent" => DeleteMode::Permanent,
            Some(other) => return Err(format!("invalid mode {other}: use \"trash\"|\"permanent\"")),
        };
        let r: methods::FsTaskResult = self
            .call_tracked(methods::FS_DELETE, &methods::FsDeleteParams { path, mode }, daemon_id)
            .await?;
        self.wait_terminal(r.task_id).await
    }
```

- [ ] **Step 3: Actualizar el callsite inline** de `tools_call` en `handle_line` (L110). El camino inline (tests directos, sin transporte concurrente) no cancela: pásale una celda de usar-y-tirar:

```rust
            "tools/call" => self.tools_call(&id, &params, &Arc::default()).await,
```

- [ ] **Step 4: Compilar** (el callsite del transporte concurrente aún pasa la firma vieja — se cierra en C2):

Run: `cargo build -p norte-mcp 2>&1 | head -20`
Expected: FAIL SOLO por el arg faltante en la llamada a `tools_call` dentro de `dispatch_line` (spawn). Sin otros errores.

- [ ] **Step 5: (sin commit — continúa en C2).**

### Task C2: reenviar `rpc.cancel` al daemon al cancelar

**Files:**
- Modify: `crates/norte-mcp/src/bridge.rs` (`InflightTool` struct nuevo; map de `serve_transport` L626; admisión en `dispatch_line` L751-798; handler de cancel L738-746; teardown L684-686; `Bridge::cancel_daemon_request` nuevo)

- [ ] **Step 1: Definir `InflightTool`** (token + celda daemon-id) y cambiar el tipo del mapa. Reemplaza el `InflightGuard`/map de `CancellationToken` por:

```rust
/// Un tool en vuelo (#67 + #72): su token de cancelación local y la celda con
/// el id JSON-RPC de su `fs.*` mutante (para reenviar `rpc.cancel` al daemon).
#[derive(Clone)]
struct InflightTool {
    token: CancellationToken,
    daemon_id: DaemonIdCell,
}
```

Cambia el alias del mapa en `serve_transport` (L626):

```rust
    let inflight: Arc<std::sync::Mutex<std::collections::HashMap<String, InflightTool>>> =
        Arc::default();
```

Y `InflightGuard` (L699-711) mantiene el mismo `map` retipado:

```rust
struct InflightGuard {
    key: String,
    map: Arc<std::sync::Mutex<std::collections::HashMap<String, InflightTool>>>,
}
```

(El `Drop` no cambia: sigue haciendo `map.lock().remove(&self.key)`.)

- [ ] **Step 2: Añadir `Bridge::cancel_daemon_request`** en `impl Bridge`:

```rust
    /// Reenvía al daemon un `rpc.cancel` de la request `daemon_id` (#72): si
    /// esa `fs.*` sigue suspendida en un Ask, el daemon la retira (fail-closed).
    /// Best-effort: un id ya resuelto es no-op en el daemon; un canal muerto se
    /// descarta. El daemon gobierna: comprometer el puente NO salta la policy.
    pub fn cancel_daemon_request(&self, daemon_id: u64) {
        let _ = self.client.notify(
            methods::RPC_CANCEL,
            &methods::RpcCancelParams { id: norte_proto::wire::RequestId::Num(daemon_id) },
        );
    }
```

- [ ] **Step 3: Actualizar la admisión y el spawn en `dispatch_line`** (L751-798). El token y la celda se crean juntos; el spawn recibe la celda; el guard retira la entrada:

```rust
    if method == "tools/call" {
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let token = CancellationToken::new();
        let daemon_id: DaemonIdCell = Arc::default();
        let admission = {
            let mut map = inflight.lock().expect("inflight lock sano");
            if map.len() >= MAX_INFLIGHT_TOOLS {
                Err("too many concurrent tool calls")
            } else {
                match map.entry(id_key(&id)) {
                    std::collections::hash_map::Entry::Occupied(_) => {
                        Err("duplicate request id already in flight")
                    }
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(InflightTool { token: token.clone(), daemon_id: Arc::clone(&daemon_id) });
                        Ok(())
                    }
                }
            }
        };
        if let Err(msg) = admission {
            let _ = out_tx.send(rpc_error(&id, -32000, msg)).await;
            return;
        }
        let bridge = Arc::clone(bridge);
        let out_tx = out_tx.clone();
        let guard = InflightGuard { key: id_key(&id), map: Arc::clone(inflight) };
        tokio::spawn(async move {
            let _guard = guard;
            tokio::select! {
                biased;
                out = bridge.tools_call(&id, &params, &daemon_id) => {
                    let _ = out_tx.send(out).await;
                }
                () = token.cancelled() => {}
            }
        });
        return;
    }
```

- [ ] **Step 4: Actualizar el handler de `notifications/cancelled`** (L738-746) para retirar la entrada, disparar el token local Y reenviar `rpc.cancel` si hay un id de daemon registrado:

```rust
        if method == "notifications/cancelled"
            && let Some(req_id) = msg.pointer("/params/requestId")
            && let Some(tool) = inflight
                .lock()
                .expect("inflight lock sano")
                .remove(&id_key(req_id))
        {
            tool.token.cancel();
            // #72: si el tool había lanzado una fs.* mutante contra el daemon,
            // reenvía un rpc.cancel de ESA request — retira su Ask suspendido en
            // vez de dejarlo zombi hasta el TTL. Un id aún sin fijar (tool que no
            // llegó a llamar al daemon) = nada que cancelar.
            if let Some(&daemon_id) = tool.daemon_id.get() {
                bridge.cancel_daemon_request(daemon_id);
            }
        }
        return;
```

(`bridge` está en scope en `dispatch_line` como `&Arc<Bridge>`.)

- [ ] **Step 5: Actualizar el teardown** de `serve_transport` (L684-686) para el nuevo tipo del mapa. Al abandonar todo por EOF/error también reenvía cancels (retira los Asks colgados de tools abandonados):

```rust
    for (_, tool) in inflight.lock().expect("inflight lock sano").drain() {
        tool.token.cancel();
        if let Some(&daemon_id) = tool.daemon_id.get() {
            bridge.cancel_daemon_request(daemon_id);
        }
    }
```

(Nota: en el teardown por EOF, `peer_gone` del daemon YA cierra la conexión y limpia los Asks — el `rpc.cancel` aquí es redundante pero inocuo/best-effort. Si complica el borrow de `bridge` en el teardown, basta con `tool.token.cancel()` — el cierre de la conexión del `Client` al dropear el puente ya dispara `peer_gone` en el daemon. Mantén simple: si `bridge` no es fácilmente accesible en ese punto, omite el reenvío en el teardown y confía en `peer_gone`.)

- [ ] **Step 6: Compilar + tests + clippy del crate**

Run: `cargo build -p norte-mcp && cargo nextest run -p norte-mcp && cargo clippy -p norte-mcp --all-targets -- -D warnings`
Expected: PASS (los tests existentes de `bridge.rs`/`transport.rs`/`e2e_m3.rs` siguen verdes; el nuevo va en C3).

- [ ] **Step 7: Commit**

```bash
git add crates/norte-mcp/src/bridge.rs
git commit -m "feat(mcp): #72 el puente reenvía rpc.cancel al cancelar un tool suspendido en un Ask"
```

### Task C3: e2e — el agente cancela → el humano ya no puede aprobar-para-ejecutar

**Files:**
- Modify: `crates/norte-mcp/tests/transport.rs` (reusa `spawn_ask_daemon`, `spawn_transport`, `send_line`, `read_json`, `grant_scope_via_transport`, `copy_call`; el humano `Client` puede consultar `policy.pending`/`policy.decide` wire-directo)

- [ ] **Step 1: Escribir el test.** Modela sobre `cancelled_abandona_el_tool_en_vuelo_sin_respuesta` (L164), pero afirma la garantía de #72: tras el cancel, la pendiente del daemon desaparece y un `policy.decide` humano NO ejecuta la copia (el destino no se crea). Añade a `transport.rs`:

```rust
/// #72: un `notifications/cancelled` de un tool suspendido en un Ask reenvía
/// `rpc.cancel` al daemon — retira el Ask. El humano ya NO puede
/// aprobar-para-ejecutar: `policy.pending` queda vacío y un `policy.decide`
/// tardío no crea el destino.
#[tokio::test]
async fn cancel_del_agente_retira_el_ask_el_humano_no_ejecuta() {
    let (_dir, socket, mem) = spawn_ask_daemon().await;
    let (mut w, mut r) = spawn_transport(&socket).await;
    let human = grant_scope_via_transport(&mut w, &mut r, &socket).await;

    // Sembrar el origen (spawn_ask_daemon siembra mem:///proj/informe.txt;
    // usa el copy_call de este fichero — ajusta rutas a lo que exista).
    write_file(&mem, "mem:///proj/a.txt", b"datos").await;

    // Lanzar copy: se suspende en el Ask.
    send_line(&mut w, &copy_call(10)).await;

    // El humano ve la pendiente (espera con tope).
    let approval_id = {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let p: norte_proto::methods::PolicyPendingResult = human
                .call(norte_proto::methods::POLICY_PENDING, &serde_json::json!({}))
                .await
                .expect("pending");
            if let Some(a) = p.pending.first() {
                break a.approval_id;
            }
            assert!(tokio::time::Instant::now() < deadline, "el Ask nunca apareció");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };

    // El agente cancela su tools/call → el puente reenvía rpc.cancel.
    send_line(
        &mut w,
        &serde_json::json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":10}}),
    )
    .await;

    // La pendiente desaparece del daemon (el Ask se retiró).
    {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let p: norte_proto::methods::PolicyPendingResult = human
                .call(norte_proto::methods::POLICY_PENDING, &serde_json::json!({}))
                .await
                .expect("pending");
            if p.pending.is_empty() {
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "la pendiente no se retiró");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    // Un decide humano tardío sobre esa aprobación NO ejecuta la copia.
    let _ = human
        .call::<_, norte_proto::methods::PolicyDecideResult>(
            norte_proto::methods::POLICY_DECIDE,
            &norte_proto::methods::PolicyDecideParams { approval_id, approve: true },
        )
        .await; // Err o no-op: da igual, lo que importa es el efecto en el FS.

    // Fail-closed: el destino jamás se creó.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        mem.stat(&vp("mem:///proj/b.txt")).await.is_err(),
        "el destino NO debe existir tras la retirada del Ask"
    );

    // El transporte sigue vivo: un ping responde.
    send_line(&mut w, &serde_json::json!({"jsonrpc":"2.0","id":12,"method":"ping"})).await;
    let resp = read_json(&mut r).await;
    assert_eq!(resp["id"], 12);
}
```

Ajusta las rutas (`a.txt`/`b.txt` vs `informe.txt`) a lo que `copy_call`/`spawn_ask_daemon` de `transport.rs` usen realmente; verifica el método `stat` del `MemProvider`. Asegura los `use` (`Duration`, `write_file`, `vp`).

- [ ] **Step 2: Correr el test**

Run: `cargo nextest run -p norte-mcp cancel_del_agente_retira_el_ask_el_humano_no_ejecuta`
Expected: PASS.

- [ ] **Step 3: Suite completa del crate + clippy**

Run: `cargo nextest run -p norte-mcp && cargo clippy -p norte-mcp --all-targets -- -D warnings`
Expected: PASS (incluidos `cancelled_abandona_el_tool_en_vuelo_sin_respuesta` y `eof_con_tool_en_vuelo_termina_limpio` sin regresión).

- [ ] **Step 4: Commit**

```bash
git add crates/norte-mcp/tests/transport.rs
git commit -m "test(mcp): #72 e2e — el cancel del agente retira el Ask, el humano no ejecuta"
```

**→ REVIEWERS tras C3: security-reviewer (el puente NO gana poder; el cancel solo RETIRA, el daemon gobierna) + rust-reviewer.**

---

## Cierre

### Task FINAL: `just ci` + reviewers + memoria

- [ ] **Step 1: `just ci` completo local** (gate: CI GitHub OFF por billing):

Run: `just ci`
Expected: EXIT=0 — build, `cargo nextest run --workspace`, clippy `-D warnings`, `cargo fmt --all --check`, `cargo deny check`, coverage gate (85% en core/vfs/proto).

Si `fmt` marca diffs: `cargo fmt --all` y recommit el archivo tocado.

- [ ] **Step 2: Reviewers obligatorios** (subagent-driven o manual, según ejecución):
  - **protocol-guardian** — componente A (wire freeze de `rpc.cancel`, bump 0.19.0, N/N-1, golden) + la decisión de desenlace `Error::Cancelled`.
  - **security-reviewer** — la retirada es fail-closed: el cancel JAMÁS aprueba (solo dropea el gate pre-efecto), el desenlace no filtra policy, sin pendientes/tokens huérfanos (guards RAII), el puente no gana poder.
  - **rust-reviewer** — reglas duras de CLAUDE.md (sin unwrap/expect fuera de invariantes comentadas, locks sin await cruzado, errores tipados, `#[instrument]` donde toque).

  Aplica el feedback con la sub-skill `superpowers:receiving-code-review` (verificar, no obedecer ciegamente). Los reviewers NO commitean; el escritor único de la rama aplica.

- [ ] **Step 3: Actualizar la memoria del proyecto** (`backlog-deuda-estado.md` o `proyecto-norte-estado.md`): #72 CERRADO, proto 0.19.0, mecanismo (rpc.cancel + drop del dispatch fail-closed + reenvío del puente), tests, reviewers. Y cerrar/anotar la relación con #64 (el hermano explícito).

- [ ] **Step 4: `git log --oneline` de la rama** — confirmar commits atómicos (una PR = un propósito, < 400 líneas netas por commit donde sea posible), Conventional Commits.

---

## Self-Review (checklist del autor del plan)

**1. Cobertura del spec:**
- Componente A (proto `rpc.cancel`, bump menor, golden, N/N-1, protocol-guardian) → Tasks A1, A2. ✓
- Componente B (mapa `inflight_cancel` por conexión + guard RAII; inner `select!` de `serve_connection` gana brazo que lee `inbox_rx`; `rpc.cancel` dispara token sin romper conexión; gate del Ask retirado fail-closed → DENIEGA/retira, limpia `policy.pending`, libera dispatch; reusa infra #64) → Tasks B1, B2, B3, B4. ✓ El spec dice "el gate hace `select!` sobre decide + token"; el plan elige la variante equivalente y más simple "drop del dispatch en `handle_value`" — documentada en Architecture con rationale fail-closed y marcada para confirmación de reviewers. El spec deja el re-encolado "a fijar en el plan" → búfer `pending_frames` (VecDeque). ✓
- Componente C (puente reenvía `rpc.cancel` con el id de la fs.* en vuelo; guarda ese id al lanzarla; no-op/task.cancel si no estaba en Ask) → Tasks C1, C2. ✓
- Manejo de bordes del spec: id desconocido→no-op (B4 Step 4); EOF durante Ask sigue por `peer_gone` (B4 Step 5); doble cancel/cancel+decide carrera biased determinista (B4 Steps 2-3); N-1 (rustdoc A1, ventana 0.18.x). ✓
- Testing del spec: golden `rpc.cancel` (A2); integración daemon Ask+cancel→retira, pending vacío, pipeline liberado, carrera, #64 sin regresión (B4); e2e puente in-process agente cancela→humano no ejecuta (C3). ✓
- Reviewers obligatorios (protocol-guardian, security, rust) → Task FINAL Step 2. ✓

**2. Placeholders:** Sin TBD/TODO. Todo step con código lleva el código concreto; los comandos llevan salida esperada. Las verificaciones de "ajusta al nombre real" (métodos del `MemProvider`, brazos exactos de `call_tool`, contrato de `handle_policy_decide` para id inexistente) están señaladas explícitamente porque dependen de detalle local que el ejecutor confirma al abrir el archivo — no son huecos de diseño.

**3. Consistencia de tipos:** `RpcCancelParams { id: RequestId }` (A1) ↔ golden `{"id": 7}` (A2) ↔ `RequestId::Num(id)` en daemon/puente/tests (B4/C2). `InflightCancel = Arc<Mutex<HashMap<RequestId, CancellationToken>>>` (B2) usado en `handle_value` y `serve_connection` (B3). `Client::call_tracked(method, params, on_id: FnOnce(u64))` + `Client::notify` (B1) usados por daemon tests (B4) y `Bridge::call_tracked`/`cancel_daemon_request` (C1/C2). `DaemonIdCell = Arc<OnceLock<u64>>` + `InflightTool { token, daemon_id }` (C1/C2) coherentes en admisión, spawn, handler y teardown. Desenlace `Error::Cancelled` (ya en proto) consistente en B2 (síntesis) y B4/C3 (aserciones). ✓
