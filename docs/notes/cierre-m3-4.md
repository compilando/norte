# Cierre M3-4 — norte-mcp server

**Estado: COMPLETO (2026-07-16).** `just ci` verde; el criterio de salida de
M3 está demostrado E2E (`crates/norte-mcp/tests/e2e_m3.rs`).

## Qué quedó

- **Puente MCP stdio** (`crates/norte-mcp`, ADR 0024): JSON-RPC 2.0 NDJSON sin
  SDK, reenvío 1:1 al daemon como cliente con `agent_session`. Tools v1:
  `list_dir, stat, read_file, copy, move, delete, task_status, request_scope`.
  El enforcement (scope, policy, ask, journal, actor) es 100% server-side.
- **Daemon dueño único del journal**: `norte daemon run` abre `SqliteJournal`
  (lock EXCLUSIVO del fichero) + instala `ScopedPolicy` (de `policy.toml`,
  ausente = fail-closed) + `DaemonApprovalResolver`. Cierra la deuda de M3-1b.
- **Undo humano de sesión de agente por wire**: `policy.undo_session`
  (proto 0.12.0, solo User) + split target/ejecutor en el engine
  (`undo_session_for`) — el humano deshace aunque el scope del agente expirase.
- **CLI del lado humano**: `norte mcp serve --session`, `norte policy grant`,
  `norte undo <session>`.
- **Criterio de salida M3 demostrado**: request_scope → grant → copy(ask) →
  approval_required → decide → completed → undo → revertido; fuera de scope
  sigue cerrado.

## Deuda anotada

- **#66** (daemon, pre-existente): `task.list`/`task.cancel`/
  `connection.trust_host_key` sin gate de actor — un agente wire-directo
  observa paths del humano, cancela sus tasks y bendice host keys. El puente
  no lo expone, pero el vector same-uid existe.
- **#67**: el transporte del puente es SERIAL — un tool mutante en vuelo
  retiene `ping`/`cancelled` hasta `TASK_WAIT`. Correcto para el patrón MCP
  típico (un tool a la vez); concurrencia por-request = mejora futura.
- **#65**: undo de `Created` sin cap TRASH borra permanente (mala interacción
  con remotos `logical_trash` OFF + sin `node_id`).
- **Transporte streamable-HTTP** (rmcp) cuando haya demanda; negociación de
  versión MCP si algún cliente rechaza la fija.
- **`write_file`/`mkdir`/`search`** como tools cuando tengan método de wire
  (jamás atajo del puente, regla 9).
- **Listado de scope-requests pendientes por wire** (hoy el `request_id` lo
  imprime el propio agente); **undo de la propia sesión** como tool del agente;
  **UI de grant de scope** en el TUI.
- Deuda M3-3b viva: m4 namespace scope key `(actor_kind,id)`, m6 symlink
  escape fixture, m7 revoke/GC de sesiones muertas.

## Siguiente

**M3-5**: audit trail exportable (CSV/JSONL sobre el journal) + **#63**
(anclaje/firma del head para tamper-evidence real). Cierra M3.
