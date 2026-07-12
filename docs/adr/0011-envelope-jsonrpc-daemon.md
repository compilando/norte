# 0011 — Envelope JSON-RPC 2.0, framing, transporte y ciclo de vida del daemon

- Estado: accepted
- Fecha: 2026-07-12
- Decisores: oscar (dirección), Claude (propuesta técnica)
- Relacionado: spec §11 (protocolo), §17.6 (lifecycle/auth), §17.7 (errores);
  ADR 0004 (convenciones wire, N/N-1); plan M2 fase 2; kickoff M2 decisión 4
  (MessagePack negociado-pero-solo-JSON); issue #31.

## Contexto y problema

M1 congeló los tipos de params/results (`fs.*`, `task.*`, proto 0.3.0) pero
todo corre embebido: no hay envelope, ni transporte, ni daemon. La fase 2 de
M2 los estrena. Decisiones que quedan fijadas por golden y por contrato de
seguridad: forma del envelope, framing, códigos de error, handshake y
versionado, transporte + autenticación, y ciclo de vida del daemon.

## Opciones consideradas

### A. Framing sobre el stream

- **A1 — headers estilo LSP (`Content-Length: N\r\n\r\n`)**: soporta
  payloads binarios (msgpack futuro) con el mismo framing. Contra: parser de
  headers propio (estado, límites, CRLF), menos inspeccionable a mano.
- **A2 — NDJSON (un mensaje JSON por línea `\n`)**: trivial de parsear y de
  fuzzear (spec §12 pide fuzzing del framing), debuggable con `socat`/`jq`,
  sin estado. serde_json jamás emite `\n` sin escapar. Contra: solo sirve
  para encodings de texto — un encoding binario futuro necesita otro framing.

### B. Códigos de error JSON-RPC

- **B1 — un código propio por categoría de la taxonomía**: duplica la fuente
  de verdad (código ↔ `kind`), y cada categoría nueva exige asignar código.
- **B2 — códigos estándar para errores DE PROTOCOLO (-32700 parse, -32600
  invalid request, -32601 method not found, -32602 invalid params, -32603
  internal) y UN código de aplicación (-32000) con la taxonomía completa
  (`Error` de §17.7) en `error.data`**: los frontends hacen match por
  `data.kind` — exactamente el contrato que ya tienen; `code`/`message` son
  presentación/depuración.

### C. Transporte Windows en esta fase

- **C1 — named pipe ya**: el SID check del cliente exige llamadas Win32
  (`GetNamedPipeClientProcessId` + tokens) = `unsafe` fuera de
  `norte-vfs-local` (prohibido por regla 5) o una dependencia nueva; y el
  security descriptor POR DEFECTO de un named pipe deja READ a Everyone —
  otro usuario podría recibir los broadcasts (paths de archivos = fuga).
- **C2 — diferir Windows con issue**: UDS completo ahora (SO_PEERCRED está
  trillado y tokio lo expone sin unsafe); el modo embebido sigue siendo el
  camino en Windows (red de seguridad ya prevista en el plan). El pipe llega
  cuando se decida dónde vive su `unsafe` (candidato: crate de transporte
  con excepción a la regla 5 vía ADR propio, o dependencia auditada).

## Decisión

- **Envelope JSON-RPC 2.0** en `norte-proto::wire` (público): `Request`
  (`jsonrpc:"2.0"`, `id`, `method`, `params`), `Response` (`result` XOR
  `error`), `Notification` (sin `id`). `RequestId`: el emisor canónico
  escribe u64; el receptor acepta número o string (tolerancia JSON-RPC).
  La clasificación de un mensaje entrante es estructural: `method`+`id` =
  request; `method` sin `id` = notification; sin `method` = response.
- **A2 — NDJSON** con límite de frame de 16 MiB (anti-DoS; frame mayor =
  error de parse y cierre). El encoding negociado en el handshake es
  `"json"`; un encoding binario futuro (msgpack, decisión 4 del kickoff)
  negociará también su framing — esta ADR no lo hipoteca.
- **B2** para errores: estándar JSON-RPC para protocolo, `-32000` +
  taxonomía en `data` para aplicación. `message` = `Display` del error
  (inglés estable, ya testeado); JAMÁS se parsea.
- **Handshake `initialize`**: params `{client_info{name,version},
  protocol_version, encodings}` → result `{server_info{name,version},
  protocol_version, encodings:["json"]}`. Compatibilidad N/N-1 (spec §11):
  en 0.x el minor manda — el core acepta su minor y el anterior; cliente
  más nuevo o más viejo → error `initialize` y cierre (el "upgrade dance"
  completo del daemon viejo llega con el resto del lifecycle en M2+).
  `initialize` es OBLIGATORIO antes de cualquier otro método (error
  -32002 "not initialized").
- **`Error::Loop`** (issue #31): categoría nueva para ciclos de symlinks
  (visited set de #19). Compatible: N-1 degrada a `Unknown` (fallback de
  ADR 0004). El engine deja de disfrazarla de `InvalidPath`.
- **`PROTOCOL_VERSION` 0.3.0 → 0.4.0**: el envelope, `initialize` y `Loop`
  son superficie nueva coherente ("protocolo con daemon"); 0.3.0 pasa a ser
  el N-1 aceptado.
- **Transporte: UDS** en `$XDG_RUNTIME_DIR/norte/daemon.sock`; sin
  `XDG_RUNTIME_DIR`, fallback `/tmp/norte-<uid>/daemon.sock` con el dir
  creado 0700 y VERIFICADO (dueño = uid, modo 0700, sin symlink) antes de
  bindear. **Auth: SO_PEERCRED** (`UnixStream::peer_cred()`, sin unsafe):
  `uid` del peer == `euid` del daemon o la conexión se cierra ANTES de leer
  un byte. **Jamás root**: el daemon rechaza arrancar con euid 0 (spec
  §17.6). TCP loopback (con token) queda fuera de M2.
- **C2**: Windows diferido con issue propia; embebido sigue siendo el modo
  Windows.
- **Ciclo de vida**: `norte daemon run` (foreground). Shutdown por
  inactividad: sin clientes Y sin tasks vivas durante `idle_timeout`
  (default 300 s, flag/config). Graceful: SIGTERM o método
  `daemon.shutdown{graceful:true}` (autenticado como todo) → deja de
  aceptar, espera tasks, cierra; `graceful:false` cancela tasks primero.
  Autoarranque por el primer cliente: helper de cliente `connect_or_spawn`
  (comando de arranque parametrizado; los frontends lo cablean en fase 3).
- **Broadcast**: `task.progress` (coalescido ≤30 Hz, ya garantizado por el
  watch del scheduler) se difunde a TODOS los clientes autenticados — la
  base de "dos frontends sobre la misma sesión" de fase 3.

## Consecuencias

Positivas:

- Frontends y agentes hablan un protocolo estándar inspeccionable con
  herramientas de a bordo (`socat` + `jq`); el fuzzing de framing (fase 10)
  es sobre líneas JSON, no sobre un parser de headers propio.
- La taxonomía sigue siendo EL contrato de errores: el envelope no crea una
  segunda jerarquía que mantener.
- El check de auth es de 5 líneas sin unsafe y testeable por inyección.

Negativas / deuda asumida:

- Windows sin daemon hasta su issue (embebido funciona igual; la paridad
  Windows era ya riesgo declarado del plan).
- NDJSON obliga a que el encoding binario futuro renegocie framing (coste
  asumido a cambio de simplicidad hoy; la negociación de encoding nace en
  el handshake precisamente para eso).
- El "upgrade dance" completo (daemon viejo rechaza nuevos y se despide)
  queda en la parte diferida del lifecycle; 0.4.0 solo rechaza versiones
  incompatibles en `initialize`.
- `-32000` único para toda la aplicación: herramientas JSON-RPC genéricas
  no distinguen categorías sin mirar `data` (aceptado: nuestros frontends
  sí miran `data`). Excepciones con código propio: `-32001` mismatch de
  versión en `initialize` (la señal de upgrade se distingue por CÓDIGO,
  jamás parseando `message`) y `-32003` sobrecarga (límites de recursos).

Enmiendas de la revisión de fase 2 (protocol-guardian + security-reviewer +
rust-reviewer):

- **Límites**: outbox por conexión acotada (el cliente que no drena pierde
  broadcast y conexión, jamás memoria sin límite), tope de conexiones
  simultáneas y de tasks vivas (`-32003`), corte tras N errores de parse
  consecutivos.
- **Auth simétrica**: el CLIENTE también verifica por `peer_cred` que el
  daemon del socket es de su uid (anti-spoof del fallback `/tmp`).
- **La suscripción al broadcast nace con `initialize`**, no con el connect.
- **`initialize` repetido = `INVALID_REQUEST`** (como LSP).
- **JSON roto = `-32700`; JSON válido con envelope inválido = `-32600`**;
  una request con `id` de tipo ilegal recibe `-32600`, jamás silencio
  (clasificación estructural `wire::classify`).
- **Hardening diferido con issue**: squat del dir `/tmp` (DoS de arranque),
  TOCTOU residual sin sticky bit (operar por fd), y — ANTES de conectar
  agentes al socket — gating de política por cliente para `daemon.shutdown`
  y `task.cancel` (hoy: mismo uid = mismo poder, correcto para frontends
  de confianza; insuficiente para agentes, M3/M4).

Fase 3 (0.5.0, `task.list`/`fs.read`/`fs.capabilities` + backend unificado):

- **`task.list`** devuelve tasks vivas + desenlaces recientes (anillo
  acotado); el receptor DEDUPLICA por `task_id`. El desenlace se retiene
  ANTES de difundirse (invariante: visible por broadcast o por el anillo,
  jamás por ninguno).
- **Reconciliación de huérfanas**: al reconectar, una task en vuelo que el
  daemon ya no conoce se resuelve `Failed{ProviderUnavailable}` — su
  `join()` jamás cuelga (revisión de fase 3).
- **Reconexión NO re-arranca el daemon** (solo el primer connect) y usa un
  `Weak` para que la bomba muera con el backend; llamadas remotas con
  timeout; auth simétrica ya vigente.
- **`fs.read`** transporta bytes en base64 (`content_b64`), tope
  `FS_READ_MAX_CHUNK`=8 MiB por llamada, `eof` para reanudar.
