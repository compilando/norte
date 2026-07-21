# #44 — la degradación de `tls = allow` debe llegar al USUARIO por protocolo — diseño

- Fecha: 2026-07-21
- Estado: **aprobado** (oscar); pendiente de plan
- Origen: security-review de fase 6d (S1-deuda, N3, N6). ADR 0015 F: "FTP plano
  nunca es silencioso".

## Problema

`connections.toml` admite `tls = "allow"` para FTP: intenta FTPS y, si el
servidor rechaza `AUTH TLS`, **cae a FTP en claro**. Hoy esa degradación se
anuncia SOLO con `tracing::warn!` en el connector
(`crates/norte-connect/src/ftp.rs`, arm `(TlsMode::Allow, Some(tls))` sobre
`SecureError::AuthRejected`): suficiente para el log del daemon, **invisible para
el usuario del frontend**. ADR 0015 F exige que el FTP plano "nunca sea
silencioso"; el `warn!` no cumple el espíritu porque no llega al usuario.

La cadena por encima del connector no tiene forma de transportar la señal: el
connector devuelve `Arc<dyn Provider>` (sin canal de degradación), el connect es
**lazy** (no hay método/resultado `connect` explícito; la sesión se establece la
primera vez que se toca un `VPath` remoto, dentro de `Engine::provider_for`), y
no existe ninguna notificación `connection.*` de estado. Falta plumbing en TODAS
las capas.

## Alcance

**DENTRO:** surfacear la degradación `tls=allow` → plano por protocolo, extremo a
extremo (connector → engine → daemon → frontends TUI/CLI).

**FUERA (issues aparte):**
- Envolver el connect en la Task cancelable con timeout (acoplado a #47, ciclo de
  vida de sesiones — rebanada mayor).
- Aviso UX cuando `auth = "agent"` (anónimo) se combina con `tls = "plain"`
  (comparte mecanismo; se difiere para acotar esta rama).

## Decisiones (aprobadas)

### Mecanismo: notificación `connection.degraded` (no método connect nuevo)
El connect es lazy, así que no hay resultado donde colgar un campo sin inventar
un `connection.open` explícito (contradiría el modelo vigente). Una
**notificación server→client** encaja con el patrón push existente
(`task.progress`, `search.hits`, `policy.approval_required`). Aditiva → proto
**0.20.0** + golden + ventana N/N-1 + protocol-guardian OBLIGATORIO.

### Destinatarios: broadcast a HUMANOS (`broadcast_humans`)
La degradación es propiedad de la SESIÓN (compartida en el daemon), no de una
conexión. Todo humano del daemon debe saber "hay una sesión en claro". Se difunde
con el mismo `broadcast_humans` que `policy.approval_required` (solo suscriptores
NO-agente). Un agente que dispare el connect lazy no la recibe (no tiene UI; el
issue es sobre el usuario). Bajo el threat model UDS same-uid no hay fuga: todos
los humanos son el mismo uid.

### Plumbing tipado (el warning viaja de vuelta, no un canal lateral)
El warning se DEVUELVE junto al provider y el engine decide emitirlo; no un
callback inyectado en el connector. Más simple de razonar y testear.

## Arquitectura

```
FtpConnector::connect  →  ConnectionManager (impl RemoteConnector)  →  Engine::provider_for
   detecta downgrade        recoge el/los warning(s)                     emite por el sink
   (conserva el warn!)                                                         │
                            daemon: ConnectionObserver → broadcast_humans ─────┘
                                                                                │
                                     TUI (indicador persistente) + CLI (stderr warning)
```

### Componente A — proto (`norte-proto`, 0.20.0)
- const `CONNECTION_DEGRADED = "connection.degraded"`.
- `struct ConnectionDegraded { scheme: String, host: String, reason: String,
  detail: Option<String> }`:
  - `scheme` — p. ej. `"ftp"`.
  - `host` — REDACTADO (sin userinfo `user:pass@`), rule 10. Reutiliza el helper
    de redacción que usan las notifs de policy para las rutas.
  - `reason` — vocabulario CERRADO comparable por igualdad (como
    `PolicyDenied.rule`). Primer valor: `"tls-auth-rejected"` (el servidor rechazó
    `AUTH TLS` bajo `tls=allow`; sesión en claro). Ampliable aditivo.
  - `detail` — string humano opcional (presentación, jamás contrato).
- Golden en `methods.json` (wire-freeze) + round-trip. Bump 0.19.0 → 0.20.0,
  ventana N/N-1 → 0.19.x. protocol-guardian OBLIGATORIO.

### Componente B — connector (`norte-connect`)
- `FtpConnector::connect` devuelve, además del stream, un flag de degradación
  (p. ej. `FtpConnectOutcome { stream, tls_degraded: bool }` o `(stream, bool)`;
  el plan fija la forma exacta). Se CONSERVA el `tracing::warn!` actual (defensa
  en profundidad: el log del daemon no depende del wire).
- `tls=plain` y `tls=require` sin cambio de comportamiento (plain siempre avisa;
  require fail-closed).

### Componente C — engine (`norte-core`)
- `RemoteConnector::connect` cambia su retorno de `Arc<dyn Provider>` a
  `Connected { provider: Arc<dyn Provider>, warnings: Vec<ConnectionWarning> }`
  (tipo nuevo de `norte-core`; `ConnectionWarning` lleva `scheme` + `host`
  redactado + `reason`). `ConnectionManager::establish` mapea el flag del
  connector a un `ConnectionWarning`.
- `Engine` gana `Option<Arc<dyn ConnectionObserver>>` (mismo patrón de inyección
  que el `ApprovalResolver`: nace antes, el daemon lo instala). Tras un connect
  con warnings, `provider_for` llama `observer.on_connection_warning(w)` por cada
  uno, ANTES de devolver el provider. Sin observer (engine embebido sin daemon) →
  se dropea (el `warn!` sigue cubriendo el log).
- `trait ConnectionObserver { fn on_connection_warning(&self, w: ConnectionWarning); }`.

### Componente D — daemon (`norte-core/src/daemon`)
- Instala un `ConnectionObserver` cuyo `on_connection_warning` codifica un
  `ConnectionDegraded` y hace `broadcast_humans` (el mismo canal de suscriptores
  del broadcast de policy). Se inyecta al enlazar (junto a policy/approvals).

### Componente E — frontends
- **CLI** (`norte-cli`, `norte connect`): el connect lazy ocurre DENTRO del
  dispatch de `fs.capabilities` (lo que `connect_cmd` usa para forzar el
  establecimiento). El broadcast se encola en la outbox de la conexión ANTES de
  la respuesta de capabilities (orden garantizado por la outbox única). Tras
  `capabilities().await`, la CLI drena las notificaciones pendientes y hace
  `eprintln!` (i18n `t!`) de cualquier `connection.degraded`. Exit 0 (degradar no
  es error).
- **TUI** (`norte-tui`): la bomba de notificaciones recibe `connection.degraded`
  → **indicador PERSISTENTE** en la status bar / cabecera del pane
  (p. ej. `⚠ ftp://host — texto plano`), string i18n. No modal (no bloquea la
  navegación); es un estado, no una decisión.

## Manejo de errores / bordes
- **Redacción**: `host` jamás lleva userinfo; se redacta en el connector/engine
  antes de cruzar el wire (rule 10), reutilizando el helper de policy.
- **Idempotencia**: una notificación por establecimiento de sesión. Si #47
  re-establece la sesión (evicción/reconexión), re-dispara — aceptable.
- **Sin observer**: engine embebido sin daemon → el warning se dropea, el `warn!`
  del connector sigue siendo el registro. Sin pánico, sin bloqueo.
- **Connects no degradados** (`require` OK, `plain` explícito): `plain` ya avisa
  por su propio `warn!`; NO emite `connection.degraded` (el usuario ya lo eligió
  explícitamente — fuera de alcance el aviso extra, ver más arriba). Solo el
  camino `allow`→degradado emite la notificación.

## Testing
- **proto**: golden `connection_degraded` (wire-freeze) + round-trip; guardian.
- **norte-connect**: `FtpConnector` contra un servidor mock que rechaza `AUTH
  TLS` bajo `tls=allow` → `tls_degraded=true`; `tls=plain`/`require` sin
  regresión (comprobar el harness de test FTP existente y extenderlo).
- **engine**: connector mock que devuelve un `ConnectionWarning` → un
  `ConnectionObserver` de test lo recibe exactamente una vez; connector sin
  warning → observer no se llama.
- **daemon**: integración — un request (p. ej. `fs.capabilities`) que dispara un
  connect degradado (connector mock inyectado) → el humano suscrito recibe
  `connection.degraded` con el host redactado y `reason="tls-auth-rejected"`; un
  agente suscrito NO la recibe (broadcast_humans).
- **CLI**: `connect_cmd` con degradación → drena la notif y emite el warning por
  stderr, exit 0.
- **TUI**: recibir `connection.degraded` pinta el indicador persistente (render
  test).

## Riesgo
Cambio multi-capa (proto aditivo + retorno del trait `RemoteConnector` + engine
+ daemon + 2 frontends), pero cada capa es acotada y sigue patrones existentes
(broadcast de policy, inyección del approval resolver, notif push). El cambio del
retorno de `RemoteConnector::connect` toca a todos los connectors (ssh/s3/ftp):
ssh/s3 devuelven `warnings: vec![]` (sin degradación). protocol-guardian (proto)
+ security-reviewer (redacción del host + que el broadcast sea solo a humanos) +
rust-reviewer obligatorios.
