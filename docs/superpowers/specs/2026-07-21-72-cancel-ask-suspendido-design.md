# #72 — cancelar un Ask suspendido cuando el agente cancela el tools/call — diseño

- Fecha: 2026-07-21
- Estado: **aprobado** (oscar); pendiente de plan
- Contexto: hallazgo M2 del security-reviewer al cerrar #67. Cierra #72 y el
  hermano explícito de #64 (la muerte del peer ya se maneja; falta el cancel
  EXPLÍCITO del agente).

## Problema

Cuando un agente (por el puente MCP) cancela un `tools/call` que está suspendido
en un Ask de policy (`ask` → espera a un humano), hoy:
- El puente aborta SU espera (`token.cancel`, sin respuesta — spec MCP), pero el
  **Ask del daemon sigue en `policy.pending` y es aprobable**: si el humano lo
  aprueba, la mutación se EJECUTA sin que el agente la espere → **divergencia
  agente↔mundo** (el agente cree que canceló; el mundo cambia).
- El **dispatch serial** de esa conexión sigue ocupado por el Ask hasta el
  TTL/decide — el cancel no libera el pipeline.

**Raíz** (compartida con #64): el daemon YA lee el socket concurrentemente
durante un dispatch suspendido (infra #64: `read_frames` → `inbox_rx` + token
`peer_gone`), pero el dispatch suspendido en el Ask solo vigila `handle_value` +
`peer_gone` (EOF/muerte). Un `notifications/cancelled` es un FRAME, no un EOF:
se queda **sin leer en `inbox_rx`** mientras el dispatch está suspendido. El
agente tampoco conoce el `approval_id` (el `policy.approval_required` va al
HUMANO, no al agente): la cancelación se expresa contra la REQUEST en vuelo, no
contra la aprobación.

## Decisión (mecanismo elegido por oscar: loop concurrente)

Extender la infra #64 para que el dispatch suspendido observe también un CANCEL
de su propia request en el inbox y cancele el gate del Ask — sin romper la
conexión (a diferencia de `peer_gone`, que sí la mata). El dispatch sigue SERIAL.

### Componente A — proto: notificación de cancelación de request
- Añadir una **notificación** `rpc.cancel { id }` (nombre a fijar con
  protocol-guardian; alternativa `request.cancel`) donde `id` es el id JSON-RPC
  de la request en vuelo a cancelar. Notificación (sin respuesta, best-effort:
  la confirmación real es que la request cancelada responde con su desenlace —
  aquí, `PolicyDenied`/`Cancelled`). Aditiva → **bump menor** + golden en
  `methods.json` (wire-freeze) + ventana N/N-1 + revisión **protocol-guardian
  OBLIGATORIA**. Un daemon N-1 que no la conozca la ignora (notificación
  desconocida = descarte silencioso, ADR 0004) — degrada al comportamiento
  actual (Ask zombi hasta TTL), no rompe.
- Semántica: cancelar una request en vuelo. En M1 el único camino suspendible
  largo es el Ask de policy (las Tasks largas ya tienen `task.cancel`); `id` de
  una request no-suspendida o ya resuelta = no-op benigno.

### Componente B — daemon: dispatch suspendido observa el cancel
- `handle_value` de una request que PUEDE entrar en un Ask recibe (o crea) un
  `CancellationToken` por-request, registrado por `id` en un mapa de la conexión
  (`inflight_cancel: HashMap<id, CancellationToken>`), retirado con guard RAII al
  terminar. El gate del Ask (`DaemonApprovalResolver` / la espera del gate)
  hace `select!` sobre `decide` **+ este token**; al cancelarse el token →
  trata el Ask como **DENEGADO** y limpia (retira de `pending`, libera el
  dispatch), devolviendo a la request `Error::PolicyDenied{rule: withdrawn}` o
  un desenlace de cancelación (a decidir con guardian: reusar `PolicyDenied` con
  una razón `withdrawn`, categoría gruesa, no filtra policy).
- El loop de `serve_connection`: mientras `handle_value` está suspendido, el
  inner `select!` gana un brazo que **lee `inbox_rx`**; si el frame es una
  `rpc.cancel { id }` cuyo `id` está en `inflight_cancel`, dispara ese token (NO
  rompe la conexión; a diferencia de `peer_gone`). Otros frames durante la
  suspensión: se re-encolan/procesan tras el desenlace (el dispatch sigue
  serial; un agente MCP no pipelinea, así que en la práctica el único frame
  durante un Ask es el cancel o EOF). El diseño exacto del re-encolado (peek vs
  buffer de un frame) se fija en el plan.
- Aviso al humano: al retirarse el Ask, el `policy.pending` deja de listarlo (o
  lo marca `withdrawn`); un `policy.decide` posterior sobre ese id → no-op
  (ya retirado, anti doble-decide, como el `decide` de un id inexistente hoy).
  Considerar un campo/nota en el pending mostrado («el peticionario ya no
  espera») si el Ask se retira mientras el modal humano está abierto.

### Componente C — puente MCP: reenviar el cancel al daemon
- En `dispatch`/el manejo de `notifications/cancelled { requestId }` (bridge.rs):
  además de cancelar su `inflight` token local, **reenviar** al daemon una
  `rpc.cancel { id }` para la request `fs.*` en vuelo correspondiente a ese
  `requestId`. El puente ya mapea `requestId`→su tool en vuelo; debe además
  conocer el `id` JSON-RPC de la `fs.copy/move/delete` que lanzó contra el
  daemon para ese tool (guardarlo al enviar). Si el tool no estaba en un Ask
  (ya terminó o corría como Task), el `rpc.cancel` es no-op / se usa
  `task.cancel` como hoy — sin regresión.

## Manejo de errores / bordes
- `rpc.cancel` de un id desconocido/ya resuelto → no-op (fail-safe).
- Muerte del peer (EOF) DURANTE el Ask → sigue cerrando por `peer_gone` (#64),
  sin cambio.
- Doble cancel / cancel + decide carrera → el primero gana (el token/oneshot es
  de un solo disparo; `select!` biased resuelve determinista); el segundo es
  no-op.
- N-1: un daemon viejo ignora `rpc.cancel` → comportamiento actual (Ask hasta
  TTL). Un puente viejo no envía `rpc.cancel` → igual. Compat preservada.

## Testing
- **proto:** golden de `rpc.cancel` (wire-freeze) + round-trip; protocol-guardian.
- **daemon (norte-core):** test de integración — un Ask suspendido + un
  `rpc.cancel` de su id → el gate DENIEGA, la request responde
  `PolicyDenied{withdrawn}`, `policy.pending` ya no lo lista, el dispatch se
  libera (una request siguiente en la misma conexión progresa). Test de la
  carrera cancel-vs-decide (biased determinista). Test de que `peer_gone` (EOF)
  sigue cerrando la conexión (sin regresión de #64).
- **puente (norte-mcp):** `notifications/cancelled` de un tool en Ask →
  reenvía `rpc.cancel` con el id correcto; un tool ya-Task usa `task.cancel`.
  E2E in-process (como los `e2e_m3`): agente lanza fs.copy bajo `ask`, cancela,
  el humano ya NO puede aprobar-para-ejecutar (la mutación no ocurre).

## Fuera de alcance
- Cancelación de requests suspendidas que NO sean el Ask de policy (hoy no hay
  otro camino largo suspendible en el dispatch; las Tasks usan `task.cancel`).
- `policy.cancel_scope` (cancelar un scope pendiente por el agente) — familia de
  deuda distinta.
- Buffer/pipeline de múltiples frames concurrentes por conexión (el dispatch
  sigue serial; solo se observa el cancel del in-flight).

## Riesgo
El cambio toca el corazón del dispatch del daemon (el inner `select!` de
`serve_connection`) y añade wire → protocol-guardian OBLIGATORIO + security
(la retirada del Ask debe ser fail-closed: un cancel jamás debe APROBAR, solo
DENEGAR/retirar; y la razón `withdrawn` no debe filtrar policy). Mitigado con la
infra #64 ya existente (reader concurrente) como base + tests de la carrera.
