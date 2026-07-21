# #44 — Surfacear la degradación `tls=allow` al usuario por protocolo — Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** cuando una conexión FTP con `tls="allow"` se degrada a texto plano (el servidor rechaza `AUTH TLS`), el usuario del frontend se entera — por protocolo (notificación), no solo por el log del daemon.

**Architecture:** el warning se DETECTA en `FtpConnector::connect`, VIAJA de vuelta tipado (`Connected{provider, warnings}` del trait `RemoteConnector`), el `Engine` lo EMITE por un `ConnectionObserver` inyectable, el daemon lo DIFUNDE como notificación `connection.degraded` solo a humanos (`broadcast_humans`), y los frontends lo CONSUMEN (TUI: indicador persistente en la status bar; CLI: warning por stderr). Se conserva el `tracing::warn!` existente (defensa en profundidad). Alcance: SOLO la degradación TLS; timeout-en-Task (#47) y aviso `auth=plain` diferidos.

**Tech Stack:** Rust workspace (norte-proto/connect/core/tui/cli/i18n), tokio, serde/serde_json, JSON-RPC 2.0 NDJSON sobre UDS, Fluent (`norte-i18n`). Tests: `nextest`. Gate: `just ci` local (CI GitHub OFF).

**Reviewers obligatorios:** protocol-guardian (proto, componente A), security-reviewer (redacción del host + broadcast solo-humanos + que un connector comprometido no filtre secretos por el warning), rust-reviewer. Método: subagent-driven, subagentes NO commitean, controller único escritor.

---

## Estructura de archivos

**Modificados:**
- `crates/norte-proto/src/methods.rs` — const `CONNECTION_DEGRADED` + `ConnectionDegraded` + bump 0.19.0→0.20.0 + rustdoc de versión. (A)
- `crates/norte-proto/tests/golden/types/methods.json` + `tests/golden_types.rs` + `tests/types.rs` — golden + pins de versión. (A)
- `crates/norte-connect/src/ftp.rs` — `FtpConnectOutcome { stream, tls_degraded }`. (B)
- `crates/norte-core/src/connect.rs` — `Connected`, `ConnectionWarning`, `ConnectionWarningReason`, `ConnectionObserver`; `RemoteConnector::connect` retorna `Connected`; `establish`/`connect`/`connect_named` propagan warnings. (C)
- `crates/norte-core/src/engine.rs` — `connection_observer` + `set_connection_observer`; `provider_for` emite warnings. (C)
- `crates/norte-core/tests/connect.rs` + `connect_real.rs` — actualizar las 2 impls fake de `RemoteConnector`. (C)
- `crates/norte-core/src/daemon/server.rs` — `DaemonConnectionObserver` + wiring en `bind_with_policy`. (D)
- `crates/norte-core/tests/daemon.rs` — test integración de la notificación. (D)
- `crates/norte-core/src/backend.rs` — `degraded_tx/rx` + `take_degraded` en `RemoteBackend` + `Backend::take_degraded`; arm en `pump_loop`. (E1)
- `crates/norte-cli/src/main.rs` — observer stderr en modo embebido + drenado en `connect_cmd` (remoto). (E2)
- `crates/norte-i18n/i18n/en.ftl` + `es.ftl` — strings nuevos (paridad). (E2/E3)
- `crates/norte-tui/src/main.rs` + `app.rs` + `ui.rs` — arm de notif + campo persistente + render. (E3)

Sin crates nuevos. Sin dependencias nuevas.

---

## Componente A — proto: notificación `connection.degraded`

### Task A1: const + struct + bump de versión

**Files:** Modify `crates/norte-proto/src/methods.rs`.

- [ ] **Step 1: const del método** — junto a `CONNECTION_TRUST_HOST_KEY` (~L240):

```rust
/// `connection.degraded` — notificación server→client (#44): una sesión remota
/// se estableció con seguridad DEGRADADA (hoy: FTP con `tls="allow"` cayó a
/// texto plano porque el servidor rechazó `AUTH TLS`). Solo informa (el usuario
/// debe SABER que la sesión es en claro, ADR 0015 F "nunca silencioso"); no pide
/// decisión. Se difunde solo a conexiones humanas. Un cliente N-1 la ignora
/// (notif desconocida, ADR 0004) — degrada al comportamiento previo (solo log).
pub const CONNECTION_DEGRADED: &str = "connection.degraded";
```

- [ ] **Step 2: struct** — junto a los otros structs `Connection*`/notif (tras `ConnectionTrustHostKeyResult`, ~L648):

```rust
/// Notificación [`CONNECTION_DEGRADED`] (server→client): una sesión remota se
/// estableció con seguridad degradada. Las rutas/host van REDACTADOS (rule 10):
/// `host` jamás lleva userinfo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionDegraded {
    /// Scheme de la sesión (p. ej. `"ftp"`).
    pub scheme: String,
    /// Host de la sesión, SIN userinfo (rule 10).
    pub host: String,
    /// Causa, vocabulario CERRADO comparable por igualdad (como
    /// `PolicyDenied.rule`). Valor actual: `"tls-auth-rejected"` (el servidor
    /// rechazó `AUTH TLS` bajo `tls="allow"`; la sesión viaja en claro).
    pub reason: String,
    /// Detalle humano opcional (presentación, jamás contrato).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}
```

- [ ] **Step 3: bump** `PROTOCOL_VERSION` L87 `"0.19.0"` → `"0.20.0"`, y añade la entrada de historial en el rustdoc sobre la const (tras la de 0.19.0):

```rust
/// 0.20.0 (#44): notificación `connection.degraded` (server→client) — una sesión
/// remota se estableció con seguridad degradada (FTP `tls="allow"` → plano).
/// Aditiva sobre 0.19.x (un cliente N-1 la ignora; solo-log, no rompe).
```

- [ ] **Step 4: compilar** — `cargo build -p norte-proto` → PASS.

### Task A2: golden + round-trip + pins

**Files:** Modify `methods.json`, `golden_types.rs`, `tests/types.rs`.

- [ ] **Step 1: fixture** en `crates/norte-proto/tests/golden/types/methods.json` (nueva key; JSON válido, cuidado comas):

```json
  "connection_degraded": {"scheme": "ftp", "host": "backup.example", "reason": "tls-auth-rejected"}
```

(sin `detail` → `skip_serializing_if` lo OMITE; el golden pinnea esa ausencia.)

- [ ] **Step 2: check** en `golden_types.rs` — en `check_methods_connection` (busca la fn que valida la familia `connection.*`; si no existe helper propio, añade al que cubre `connection.trust_host_key`) añade:

```rust
    check_one(
        fixtures,
        "connection_degraded",
        &ConnectionDegraded {
            scheme: "ftp".into(),
            host: "backup.example".into(),
            reason: "tls-auth-rejected".into(),
            detail: None,
        },
    );
```

Importa `ConnectionDegraded` en esa fn (`use norte_proto::methods::ConnectionDegraded;` o amplía el `use` existente). Sube el count total en `golden_methods` (`assert_eq!(fixtures.len(), 63, ...)` → `64`).

- [ ] **Step 3: pins de versión** — en `golden_types.rs` (test de consts, donde está `PROTOCOL_VERSION`): `assert_eq!(norte_proto::PROTOCOL_VERSION, "0.19.0")` → `"0.20.0"`, y añade `assert_eq!(methods::CONNECTION_DEGRADED, "connection.degraded");`. En `crates/norte-proto/tests/types.rs`, `version_ventana_actual`: shift a N=0.20.x, N-1=0.19.x, rechaza 0.18.x (mismo patrón que el bump de #72; ajusta los 3 asserts + comentario).

- [ ] **Step 4: round-trip** en `types.rs`:

```rust
#[test]
fn connection_degraded_round_trip_y_detail_omitido() {
    use norte_proto::methods::ConnectionDegraded;
    let sin = ConnectionDegraded { scheme: "ftp".into(), host: "h".into(), reason: "tls-auth-rejected".into(), detail: None };
    // detail None se OMITE (no null).
    assert_eq!(
        serde_json::to_value(&sin).unwrap(),
        serde_json::json!({"scheme":"ftp","host":"h","reason":"tls-auth-rejected"}),
    );
    let con = ConnectionDegraded { detail: Some("server rejected AUTH TLS".into()), ..sin.clone() };
    let back: ConnectionDegraded = serde_json::from_value(serde_json::to_value(&con).unwrap()).unwrap();
    assert_eq!(back, con);
}
```

- [ ] **Step 5: verificar** — `cargo nextest run -p norte-proto && cargo clippy -p norte-proto --all-targets -- -D warnings` → PASS.

- [ ] **Step 6: commit** (proto A1+A2 juntos, commit verde):

```bash
git add crates/norte-proto
git commit -m "feat(proto): #44 notificación connection.degraded + golden (proto 0.20.0)"
```

**→ REVIEWER protocol-guardian OBLIGATORIO tras A2** (wire freeze): nombre `connection.degraded`, forma, bump menor, N/N-1→0.19.x, golden, vocab cerrado de `reason`.

---

## Componente B — connector FTP devuelve el flag de degradación

**Files:** Modify `crates/norte-connect/src/ftp.rs`.

- [ ] **Step 1: test rojo** — servidor mock que acepta la conexión pero rechaza `AUTH TLS`. Revisa PRIMERO el harness de test FTP existente en `ftp.rs`/`tests/` (busca `mod tests`, mocks de suppaftp, o un servidor de prueba). Si hay un mock, extiéndelo; si NO hay forma de simular AUTH-rejection en unit test, marca este step como cubierto por el test de engine (Task C) con un connector fake y añade solo un test de construcción del outcome. Documenta cuál eliges.

- [ ] **Step 2: tipo de retorno** — define junto a `FtpConnector`:

```rust
/// Resultado de [`FtpConnector::connect`] (#44): el stream + si la sesión se
/// DEGRADÓ a texto plano (solo posible con `tls="allow"` y el servidor
/// rechazando `AUTH TLS`). El llamante (core) surfacea la degradación al usuario.
pub struct FtpConnectOutcome {
    /// El stream de control ya logueado.
    pub stream: AsyncRustlsFtpStream,
    /// `true` si `tls="allow"` cayó a claro por rechazo de `AUTH TLS`.
    pub tls_degraded: bool,
}
```

- [ ] **Step 3: cambiar la firma** de `connect` a `Result<FtpConnectOutcome, ConnectError>`. Introduce `let mut tls_degraded = false;` antes del match `(spec.tls, tls)`; en el arm `(TlsMode::Allow, Some(tls))` rama `Err(SecureError::AuthRejected)`, DESPUÉS del `tracing::warn!` (que se CONSERVA), pon `tls_degraded = true;` antes del `dial(...)` de re-disca. Al final, `Ok(FtpConnectOutcome { stream, tls_degraded })` en vez de `Ok(stream)` (el login devuelve `stream`; envuélvelo). Los demás arms (`Require`, `Plain`) dejan `tls_degraded=false`.

- [ ] **Step 4: exportar** `FtpConnectOutcome` en `norte-connect/src/lib.rs` (`pub use ftp::{FtpConnector, FtpConnectOutcome};` — amplía el `use` existente de `ftp`).

- [ ] **Step 5: verificar** — `cargo build -p norte-connect` (fallará en `norte-core` que consume la firma vieja — se arregla en C; el crate `norte-connect` solo debe compilar). `cargo nextest run -p norte-connect` (los tests del propio crate). `cargo clippy -p norte-connect --all-targets -- -D warnings`.

- [ ] **Step 6: (sin commit hasta cerrar C — el workspace no compila).**

---

## Componente C — core: warning tipado + observer + engine

**Files:** Modify `crates/norte-core/src/connect.rs`, `engine.rs`, `tests/connect.rs`, `tests/connect_real.rs`.

### Task C1: tipos `Connected` / `ConnectionWarning` / `ConnectionObserver`

- [ ] **Step 1:** en `connect.rs`, tras el trait `RemoteConnector` (~L50), añade:

```rust
/// Causa de una degradación de seguridad al conectar (#44). Vocabulario CERRADO:
/// su `wire()` es el `reason` de [`norte_proto::methods::ConnectionDegraded`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionWarningReason {
    /// FTP `tls="allow"`: el servidor rechazó `AUTH TLS` → sesión en claro.
    TlsAuthRejected,
}

impl ConnectionWarningReason {
    /// El string de wire (cerrado y contractual; ver `ConnectionDegraded.reason`).
    #[must_use]
    pub fn wire(self) -> &'static str {
        match self {
            ConnectionWarningReason::TlsAuthRejected => "tls-auth-rejected",
        }
    }
}

/// Un aviso de seguridad producido al establecer una sesión remota (#44). El
/// `host` va SIN userinfo (rule 10) por construcción — es el `Endpoint.host`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionWarning {
    /// Scheme de la sesión (p. ej. `"ftp"`).
    pub scheme: String,
    /// Host, sin userinfo.
    pub host: String,
    /// Causa.
    pub reason: ConnectionWarningReason,
}

/// La sesión establecida + los avisos de seguridad emitidos al establecerla.
pub struct Connected {
    /// El provider vivo.
    pub provider: Arc<dyn Provider>,
    /// Avisos (p. ej. degradación TLS); vacío en el caso normal.
    pub warnings: Vec<ConnectionWarning>,
}

/// Observa los avisos de conexión (#44): el daemon lo implementa para difundir
/// `connection.degraded`; la CLI embebida para imprimir por stderr. Inyectado en
/// el [`Engine`](crate::Engine) con `set_connection_observer`.
pub trait ConnectionObserver: Send + Sync {
    /// Un aviso ocurrió al establecer una sesión. Best-effort, no bloqueante.
    fn on_connection_warning(&self, warning: &ConnectionWarning);
}
```

- [ ] **Step 2: cambiar el trait** `RemoteConnector::connect` — retorno `Result<Arc<dyn Provider>, Error>` → `Result<Connected, Error>` (L36). Ajusta el rustdoc.

- [ ] **Step 3: `establish`** (L152) — retorno → `Result<Connected, Error>`. Añade `let mut warnings: Vec<ConnectionWarning> = Vec::new();` tras `let ep = ...`. Reescribe los arms:
  - `sftp` → `Ok(Connected { provider: Arc::new(SftpProvider...), warnings })`.
  - `ftp` → `let main = self.ftp.connect(...).await.map_err(log_and_map)?;` ahora es `FtpConnectOutcome`; `let reader = self.ftp.connect(...).await.map_err(log_and_map)?;`. Tras ambos: `if main.tls_degraded { warnings.push(ConnectionWarning { scheme: ep.scheme.clone(), host: ep.host.clone(), reason: ConnectionWarningReason::TlsAuthRejected }); }`. Provider con `main.stream`/`reader.stream`: `FtpProvider::with_reader(main.stream, reader.stream, "/").await?`. `Ok(Connected { provider: Arc::new(provider), warnings })`.
  - `s3` → `Ok(Connected { provider: Arc::new(ObjectProvider...), warnings })`.
  - `_ => Err(Error::Unsupported)`.

- [ ] **Step 4: `connect` (impl)** (L220) — retorno → `Result<Connected, Error>`; el cuerpo ya delega en `establish` → devuelve su `Connected`.

- [ ] **Step 5: `connect_named`** (L128) — retorno `Result<(String, String, Arc<dyn Provider>), Error>` → `Result<(String, String, Connected), Error>`; usa `let connected = self.establish(...).await?;` y `Ok((ep.scheme, authority, connected))`.

- [ ] **Step 6: verificar tipos** — `cargo build -p norte-core 2>&1 | head` fallará en `engine.rs` (caller) y en los tests fake — se arregla en C2/C3.

### Task C2: engine emite los warnings

- [ ] **Step 1:** en `engine.rs`, añade el campo (junto a `connector`, L55): `connection_observer: RwLock<Option<Arc<dyn crate::connect::ConnectionObserver>>>,` y en LOS TRES constructores (`with_observer` L82, `with_journal` L97, y cualquier otro `Self { ... }`) inicialízalo a `RwLock::new(None)`.

- [ ] **Step 2: setter** (junto a `set_connector` L177):

```rust
    /// Instala el observer de avisos de conexión (#44): el engine le entrega
    /// cada `ConnectionWarning` de un establecimiento remoto. Sin observer, los
    /// avisos se dropean (el `tracing::warn!` del connector sigue cubriendo el log).
    ///
    /// # Panics
    /// Nunca en la práctica: solo por envenenamiento del lock interno.
    pub fn set_connection_observer(&self, observer: Arc<dyn crate::connect::ConnectionObserver>) {
        *self.connection_observer.write().expect("connection_observer lock sano") = Some(observer);
    }
```

- [ ] **Step 3: emitir en `provider_for`** — el `connector.connect(...)` (L269-277) ahora devuelve `Connected`. Reescribe:

```rust
        let connected = tokio::time::timeout(
            CONNECT_TIMEOUT,
            connector.connect(p.scheme(), authority),
        )
        .await
        .map_err(|_| {
            tracing::warn!(scheme = %p.scheme(), "timeout estableciendo la conexión remota");
            Error::ProviderUnavailable { retryable: true }
        })??;
        // #44: emite los avisos ANTES del double-check (una vez por
        // establecimiento; un connect duplicado por carrera —#47— re-avisa,
        // aceptable). Sin observer, se dropean (el warn! del connector persiste).
        if !connected.warnings.is_empty()
            && let Some(obs) = self.connection_observer.read().expect("connection_observer lock sano").clone()
        {
            for w in &connected.warnings {
                obs.on_connection_warning(w);
            }
        }
        let provider = connected.provider;
        let mut providers = self.providers.write().expect("providers lock sano");
        let entry = providers.entry(key).or_insert_with(|| Arc::clone(&provider));
        Ok(Arc::clone(entry))
```

- [ ] **Step 4: test del engine** (en `crates/norte-core/tests/connect.rs`): un `FakeConnector` que devuelve un `Connected` con un warning → un `ConnectionObserver` de test lo recibe UNA vez; un connect sin warning → observer no llamado. Primero actualiza las impls fake (C3), luego este test.

### Task C3: actualizar las impls fake de `RemoteConnector`

- [ ] **Step 1:** en `crates/norte-core/tests/connect.rs`, `FakeConnector` (L83) y `HangingConnector` (L193): cambia `async fn connect(...) -> Result<Arc<dyn Provider>, Error>` a `-> Result<Connected, Error>`. `FakeConnector` devuelve `Ok(Connected { provider, warnings: vec![] })` (o con un warning inyectable para el test de C2 — dale un campo `warn_on_connect: Option<ConnectionWarning>` al fake). `HangingConnector` no cambia su cuerpo (cuelga). Importa `Connected`/`ConnectionWarning` (`use norte_core::connect::{...}`). Repite para cualquier impl en `connect_real.rs`.

- [ ] **Step 2: verificar** — `cargo build -p norte-core && cargo nextest run -p norte-core --test connect --test connect_real && cargo clippy -p norte-core --all-targets -- -D warnings` → PASS.

- [ ] **Step 3: commit** (B+C juntos, primer punto donde el workspace compila):

```bash
git add crates/norte-connect crates/norte-core/src/connect.rs crates/norte-core/src/engine.rs crates/norte-core/tests/connect.rs crates/norte-core/tests/connect_real.rs
git commit -m "feat(core): #44 avisos de conexión tipados (Connected{warnings}) + ConnectionObserver"
```

---

## Componente D — daemon difunde `connection.degraded`

**Files:** Modify `crates/norte-core/src/daemon/server.rs`, `tests/daemon.rs`.

- [ ] **Step 1: observer del daemon** — junto a los otros helpers de `Shared`/broadcast (tras `broadcast_humans` ~L288 o cerca del wiring del approval broadcaster). Define:

```rust
/// Observer de avisos de conexión del daemon (#44): codifica cada aviso como
/// `connection.degraded` y lo difunde SOLO a humanos (como `policy.*`). `Weak`
/// rompe el ciclo Shared→engine→observer→Shared.
struct DaemonConnectionObserver {
    shared: std::sync::Weak<Shared>,
}

impl crate::connect::ConnectionObserver for DaemonConnectionObserver {
    fn on_connection_warning(&self, w: &crate::connect::ConnectionWarning) {
        let Some(shared) = self.shared.upgrade() else { return };
        let notif = norte_proto::methods::ConnectionDegraded {
            scheme: w.scheme.clone(),
            host: w.host.clone(),
            reason: w.reason.wire().to_owned(),
            detail: None,
        };
        let Ok(params) = serde_json::to_value(&notif) else { return };
        let n = Notification {
            jsonrpc: norte_proto::wire::JsonRpcVersion,
            method: methods::CONNECTION_DEGRADED.into(),
            params: Some(params),
        };
        if let Ok(frame) = encode_frame(&n) {
            // Solo humanos: la sesión degradada es info de seguridad para el
            // usuario, no para el agente (mismo criterio que policy.*).
            shared.broadcast_humans(&Arc::from(frame.into_boxed_slice()));
        }
    }
}
```

- [ ] **Step 2: wiring** — en `bind_with_policy` (~L495, tras `approvals.set_broadcaster(...)` y antes de `Ok(Self {...})`), instala el observer en el engine reutilizando el `weak` ya creado (o crea otro `Arc::downgrade(&shared)`):

```rust
        shared.engine.set_connection_observer(Arc::new(DaemonConnectionObserver {
            shared: Arc::downgrade(&shared),
        }));
```

(Confirma que `Shared.engine` es accesible aquí — es `shared.engine: Arc<Engine>`; `set_connection_observer` toma `&self`.)

- [ ] **Step 3: test integración** en `tests/daemon.rs`: un daemon `bind_with_policy` + un `Engine` con un connector fake que, al conectar `ftp://host`, devuelve un `Connected` con un `ConnectionWarning{ scheme:"ftp", host:"backup.example", reason:TlsAuthRejected }`. Un cliente HUMANO conecta; dispara el connect lazy (p. ej. `fs.capabilities` sobre `ftp://backup.example/`); el humano recibe `connection.degraded` con `scheme=="ftp"`, `host=="backup.example"`, `reason=="tls-auth-rejected"`. Un cliente AGENTE suscrito NO la recibe (broadcast_humans). Reusa el patrón de `next_approval` para drenar la notif por método. Necesitarás inyectar el connector fake en el engine ANTES de `bind_with_policy` (`engine.set_connector(Arc::new(fake))`) — define un fake mínimo en el módulo de test que devuelva un `MemProvider` para `ftp://` + el warning.

- [ ] **Step 4: verificar** — `cargo nextest run -p norte-core --test daemon && cargo clippy -p norte-core --all-targets -- -D warnings` → PASS (sin regresión).

- [ ] **Step 5: commit:**

```bash
git add crates/norte-core/src/daemon/server.rs crates/norte-core/tests/daemon.rs
git commit -m "feat(core): #44 el daemon difunde connection.degraded (solo humanos)"
```

**→ REVIEWER security-reviewer tras D** (redacción del host, broadcast solo-humanos, un connector comprometido no inyecta secretos por el warning).

---

## Componente E1 — `RemoteBackend`: pump + `take_degraded`

**Files:** Modify `crates/norte-core/src/backend.rs`.

Espeja EXACTAMENTE el seam de `PolicyApprovalRequired` (`approvals_tx`/`take_approvals`/arm de `pump_loop`).

- [ ] **Step 1: canales** — en `Inner` (~L853) añade `degraded_tx: mpsc::UnboundedSender<norte_proto::methods::ConnectionDegraded>,` junto a `approvals_tx`. En `RemoteBackend` (~L896) añade `degraded_rx: Mutex<Option<mpsc::UnboundedReceiver<ConnectionDegraded>>>,`. En `connect` (~L933/L950) crea el par `let (degraded_tx, degraded_rx) = mpsc::unbounded_channel();` y pásalos al construir `Inner`/`RemoteBackend`.

- [ ] **Step 2: push** — junto a `Inner::push_approval` (~L871) añade `fn push_degraded(&self, d: ConnectionDegraded) { let _ = self.degraded_tx.send(d); }` (best-effort, sin dedup — cada degradación es un evento).

- [ ] **Step 3: arm en `pump_loop`** (~L1681, junto al arm de `POLICY_APPROVAL_REQUIRED`):

```rust
            if n.method == methods::CONNECTION_DEGRADED {
                if let Some(params) = n.params
                    && let Ok(d) = serde_json::from_value::<ConnectionDegraded>(params)
                {
                    inner.push_degraded(d);
                }
                continue;
            }
```

- [ ] **Step 4: `take_degraded`** — en `RemoteBackend` (junto a `take_approvals` ~L1510):

```rust
    /// Se lleva el receptor de avisos `connection.degraded` (#44). Uno solo (el
    /// primer dueño), como los otros `take_*`.
    pub fn take_degraded(&self) -> Option<mpsc::UnboundedReceiver<ConnectionDegraded>> {
        self.degraded_rx.lock().expect("degraded_rx lock sano").take()
    }
```

- [ ] **Step 5: `Backend::take_degraded`** — (junto a `take_approvals` ~L381):

```rust
    /// Receptor de avisos `connection.degraded` (#44). `None` en `Embedded`
    /// (la CLI embebida usa un observer directo, ver E2).
    pub fn take_degraded(&mut self) -> Option<mpsc::UnboundedReceiver<norte_proto::methods::ConnectionDegraded>> {
        match self {
            Self::Embedded(_) => None,
            #[cfg(unix)]
            Self::Remote(r) => r.take_degraded(),
        }
    }
```

Asegura `use norte_proto::methods::ConnectionDegraded;` donde aplique. Actualiza el doc-comment de `Clone` de `Backend` si enumera los `take_*` one-shot (añade `degraded`).

- [ ] **Step 6: verificar** — `cargo build -p norte-core && cargo clippy -p norte-core --all-targets -- -D warnings` → PASS. Añade un test de `backend.rs` si hay `mod tests` para pump (opcional; el e2e real es E2/E3 + el daemon test de D).

- [ ] **Step 7: commit:**

```bash
git add crates/norte-core/src/backend.rs
git commit -m "feat(core): #44 RemoteBackend enruta connection.degraded (take_degraded)"
```

---

## Componente E2 — CLI: stderr (embebido directo + remoto drenado)

**Files:** Modify `crates/norte-cli/src/main.rs`, `crates/norte-i18n/i18n/{en,es}.ftl`.

- [ ] **Step 1: strings i18n** — en `en.ftl` y `es.ftl` (paridad; hay test `message_ids`):

```
# en.ftl
cli-connection-degraded = ⚠ { $scheme }://{ $host }: sesión SIN cifrar (el servidor rechazó AUTH TLS, tls="allow"). Datos y credenciales viajan en claro.
```
```
# es.ftl
cli-connection-degraded = ⚠ { $scheme }://{ $host }: sesión SIN cifrar (el servidor rechazó AUTH TLS, tls="allow"). Datos y credenciales viajan en claro.
```
(Ajusta el en.ftl a inglés real; el patrón `ta("cli-connection-degraded", &[("scheme",..),("host",..)])`.)

- [ ] **Step 2: observer stderr para el modo embebido** — define en `main.rs`:

```rust
/// Observer de avisos de conexión de la CLI embebida (#44): imprime por stderr
/// (el daemon no media; el proceso CLI ES el que conecta). Nunca silencioso.
struct CliStderrObserver;
impl norte_core::connect::ConnectionObserver for CliStderrObserver {
    fn on_connection_warning(&self, w: &norte_core::connect::ConnectionWarning) {
        // Solo hay una razón hoy; cuando haya más, mapea w.reason a strings.
        eprintln!("{}", norte_i18n::ta("cli-connection-degraded",
            &[("scheme", w.scheme.as_str()), ("host", w.host.as_str())]));
    }
}
```

- [ ] **Step 3: instalar en el backend embebido** — donde se construye `Backend::Embedded(Arc::new(engine))` (~L803), ANTES de envolver: `engine.set_connection_observer(Arc::new(CliStderrObserver));`. (El `engine` es local ahí; instálalo antes del `Arc::new`/return.)

- [ ] **Step 4: drenar en modo remoto** — en `connect_cmd` (~L1088), TRAS el `result` de `capabilities` OK, drena los avisos remotos:

```rust
    // #44: en modo remoto, el daemon difundió connection.degraded ANTES de la
    // respuesta de capabilities (outbox ordenada); drénalos ahora a stderr.
    if let Some(mut rx) = backend.take_degraded() {
        while let Ok(d) = rx.try_recv() {
            eprintln!("{}", norte_i18n::ta("cli-connection-degraded",
                &[("scheme", d.scheme.as_str()), ("host", d.host.as_str())]));
        }
    }
```

(`connect_cmd` recibe `&Backend`; `take_degraded` toma `&mut self` — cambia la firma a `&mut Backend` o usa un `Backend` propio del comando. Revisa cómo se pasa el backend a `connect_cmd` y ajusta a `&mut` si hace falta; los otros `take_*` ya obligan a `&mut` en el arranque del TUI, patrón conocido.)

- [ ] **Step 5: verificar** — `cargo build -p norte-cli && cargo nextest run -p norte-cli && cargo clippy -p norte-cli --all-targets -- -D warnings` + `cargo nextest run -p norte-i18n` (paridad de locales) → PASS.

- [ ] **Step 6: commit:**

```bash
git add crates/norte-cli crates/norte-i18n
git commit -m "feat(cli): #44 aviso de degradación TLS por stderr (embebido + remoto)"
```

---

## Componente E3 — TUI: indicador persistente

**Files:** Modify `crates/norte-tui/src/main.rs`, `app.rs`, `ui.rs`, y strings en `en.ftl`/`es.ftl` (Task E2 ya los creó para la CLI; añade AQUÍ la clave del indicador TUI).

- [ ] **Step 1: string i18n** — en `en.ftl` y `es.ftl`:

```
status-connection-degraded = ⚠ { $scheme }://{ $host } — texto plano
```

- [ ] **Step 2: campo de App** — en `app.rs` (junto a `lua_status` ~L419): `/// #44: sesión remota degradada a claro; indicador persistente en la status bar.\n    pub connection_warning: Option<String>,` e inicialízalo a `None` en el constructor de `App`.

- [ ] **Step 3: tomar el canal** — en `main.rs` (~L230, junto a `take_approvals`): `let mut degraded = backend.take_degraded();` y pásalo al run loop (~L264).

- [ ] **Step 4: arm del select** — en el run loop (junto al arm de approvals ~L465), añade el drenado (guardando `Option<Receiver>`):

```rust
            Some(d) = async { match degraded.as_mut() { Some(rx) => rx.recv().await, None => None } } => {
                app.connection_warning = Some(norte_i18n::ta("status-connection-degraded",
                    &[("scheme", d.scheme.as_str()), ("host", d.host.as_str())]));
            }
```

(Espeja el idioma exacto de los otros arms `take_*` del loop — si usan un helper para "canal opcional", reúsalo.)

- [ ] **Step 5: render** — en `ui.rs` `draw_status` (~L793): añade una rama de precedencia para `app.connection_warning`. Debe ser PERSISTENTE pero no pisar un `app.message` transitorio: ponla por DEBAJO de `app.message` y del search-status, pero como segmento propio si el layout lo permite, o como fallback antes del `dir/pos/total`. Decisión simple: si `app.connection_warning` es `Some`, muéstralo con estilo de aviso (p. ej. amarillo) cuando no haya `message`/search-status activos. Sigue el patrón de `SearchState::Failed` (persistente) en `ui.rs:823-834`.

- [ ] **Step 6: verificar** — `cargo build -p norte-tui && cargo nextest run -p norte-tui && cargo clippy -p norte-tui --all-targets -- -D warnings`. Añade un render test que, con `app.connection_warning = Some(...)`, la status bar contiene el host (mira los render tests existentes de `ui.rs`).

- [ ] **Step 7: commit:**

```bash
git add crates/norte-tui crates/norte-i18n
git commit -m "feat(tui): #44 indicador persistente de sesión degradada a texto plano"
```

**→ REVIEWERS tras E: rust-reviewer (todos los crates) + una pasada final de security-reviewer si D cambió.**

---

## Cierre

- [ ] **Step 1: `just ci`** → EXIT=0 (build, nextest workspace, clippy -D, fmt, deny, coverage ≥85% core/vfs/proto). `cargo fmt --all` si hace falta.
- [ ] **Step 2: reviewers** — protocol-guardian (A), security-reviewer (D + C), rust-reviewer (todo). Aplica feedback (sub-skill receiving-code-review); el controller aplica y committea.
- [ ] **Step 3: memoria** — actualizar `backlog-deuda-estado.md`: #44 cerrado (proto 0.20.0, connection.degraded, mecanismo, reviewers), restar del backlog.
- [ ] **Step 4: `git log --oneline`** — commits atómicos, Conventional Commits.

---

## Self-Review (checklist del autor)

**1. Cobertura del spec:** A (proto notif connection.degraded, 0.20.0, golden, guardian) → Task A. B (connector devuelve degradación, conserva warn!) → Task B. C (RemoteConnector→Connected, engine observer, plumbing tipado; ssh/s3 sin warnings) → Task C. D (daemon broadcast_humans) → Task D. E (RemoteBackend pump+take_degraded; CLI stderr embebido+remoto; TUI indicador persistente; i18n en+es) → Tasks E1/E2/E3. Redacción del host (Endpoint.host sin userinfo) → C3 nota + D. Idempotencia/sin-observer → C2. Fuera de alcance (timeout #47, auth=plain) respetado. ✓

**2. Placeholders:** ninguno de diseño. Los steps que dicen "revisa el harness real / confirma la firma / ajusta a &mut" son verificaciones de detalle local (el implementador las resuelve al abrir el fichero), no huecos — señalados explícitamente (el mock FTP de B, el `&mut Backend` de E2, el idioma exacto del select-arm de E3).

**3. Consistencia de tipos:** `ConnectionDegraded { scheme, host, reason, detail:Option }` (A) ↔ golden (A) ↔ lo construye el daemon (D) ↔ lo deserializa el pump (E1). `Connected { provider, warnings }` (C1) devuelto por las 3 impls de `RemoteConnector` (C3) y consumido por `provider_for` (C2). `ConnectionWarning { scheme, host, reason: ConnectionWarningReason }` con `.wire()="tls-auth-rejected"` (C1) ↔ `reason` del proto (D). `FtpConnectOutcome { stream, tls_degraded }` (B) ↔ consumido en `establish` (C3-Step3). `take_degraded` → `Receiver<ConnectionDegraded>` en RemoteBackend (E1), Backend (E1), CLI (E2) y TUI (E3). `set_connection_observer` (C2) usado por daemon (D) y CLI embebida (E2). ✓

**4. Ambigüedad:** `reason` cerrado (un valor). Destinatarios = solo humanos (D, explícito). Sin resync v1 (YAGNI, notado). Precedencia del indicador TUI fijada (bajo message/search-status). ✓
