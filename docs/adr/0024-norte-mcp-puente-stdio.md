# 0024 — norte-mcp: puente MCP stdio → daemon, sin SDK

- Estado: accepted
- Fecha: 2026-07-16
- Decisores: oscar (dirección), Claude (propuesta técnica)
- Relacionado: spec §10 (integración agéntica), reglas duras 8/9, M3-3
  (policy+scopes, ADRs implícitos en `docs/superpowers/plans/2026-07-14-m3-3b-*`),
  ADR 0023 (journal). Plan: `docs/superpowers/plans/2026-07-16-m3-4-mcp-server.md`.

## Contexto y problema

M3-4 debe exponer norte a agentes MCP (Claude Code, Codex CLI, custom): que un
agente gestione archivos *a través de norte* — scope concedido por humano,
policy `ask` con aprobación interactiva, todo journalizado y deshacible — y no
contra el FS desnudo. Preguntas: ¿dónde vive el servidor MCP?, ¿con qué SDK?,
¿qué superficie de tools?, ¿quién es el dueño del journal?

## Decisión

### Puente de proceso separado, no servidor MCP embebido en el daemon

`norte-mcp` es un proceso PUENTE (`norte mcp serve`): habla MCP por stdio con
el agente y reenvía cada tool al daemon por UDS como un cliente MÁS, declarando
`agent_session` en el `initialize`. Consecuencias:

- El daemon habla UN solo protocolo (el de norte); MCP es un dialecto de
  cliente, intercambiable y no-privilegiado.
- El enforcement (frontera de scope, reglas de policy, Ask suspendido, journal,
  actor server-side) ocurre ÍNTEGRO en el daemon — el puente no toma ninguna
  decisión de seguridad ni toca el FS (regla 9). Comprometer o parchear el
  puente no salta la policy: es un cliente-agente como cualquier otro.
- El agente MCP es un proceso local del mismo uid: la policy es un guardarraíl
  para agentes que COOPERAN, no un sandbox contra código local hostil (mismo
  threat model que M3-3b, spec §14).

### Sin `rmcp` (ni ningún SDK MCP) en v1

El transporte stdio de MCP es JSON-RPC 2.0 con un mensaje por línea — el mismo
framing NDJSON que `norte-proto::wire` ya implementa y fuzzea. El puente v1
necesita 4 métodos (`initialize`, `ping`, `tools/list`, `tools/call`) y ninguna
capability compleja: un SDK completo (rmcp trae HTTP/SSE, macros, schemars…)
sería una dependencia estructural (regla 8) para usar el ~10 %. Se implementa a
mano (~300 líneas, `serde_json` + `tokio::io`).

- Versión MCP respondida: `2025-06-18`, fija (el handshake MCP permite que el
  server conteste la versión que soporta).
- **Deuda**: transporte streamable-HTTP cuando haya demanda real — ahí se
  reevalúa rmcp; negociación de versión MCP si algún cliente rechaza la fija.

### Superficie de tools v1 = lo que el wire ya ofrece

`list_dir, stat, read_file, copy, move, delete, task_status, request_scope`.
Sin `write_file`/`mkdir`/`search`: no existen en el protocolo hoy; entrarán
cuando tengan método de wire (con su gate de policy), jamás como atajo del
puente (regla 9). El tool `delete` default `trash` (spec §10: borrados
agénticos SIEMPRE recuperables); `permanent` existe pero policy decide.

`request_scope` devuelve el `request_id` y queda PENDIENTE hasta que un humano
lo conceda (`norte policy grant <id>`). **Deuda**: método de wire para listar
scope-requests pendientes (hoy el id viaja por el propio agente); undo de la
propia sesión como tool del agente; UI de grant en el TUI.

### El daemon es el dueño único del journal

`norte daemon run` abre `SqliteJournal` en `config_dir()/journal.db`
(`Engine::with_journal`) e instala `ScopedPolicy` + `DaemonApprovalResolver`
(`bind_with_policy`). Cierra la deuda de M3-1b: el hash-chain es single-writer
(spec §4) y el único proceso longevo con derecho a escribirlo es el daemon. Los
modos embebidos (TUI/CLI sin daemon) siguen SIN journal.

`policy.toml` (en el dir de config) se carga al arrancar; **ausente = sin
reglas = fail-closed**: un agente DENTRO de su scope aún se deniega (`no-rule`).
`docs/policy-example.toml` trae el punto de partida (`action = "ask"`). Se
prefiere el arranque cerrado + ejemplo documentado a un default `ask` implícito
que nadie revisó.

### Undo humano de sesión de agente por el wire

`session.undo {session}` (proto 0.12.0, solo conexiones User) revierte la
sesión completa de un agente en LIFO estricto. En el engine, el undo separa
**target** (de quién son las entradas) de **ejecutor** (quién pasa el gate y
firma las compensaciones): el humano deshace aunque el scope del agente haya
expirado — cierra la deuda de M3-2.

## Consecuencias

- Un `claude_desktop_config.json`/`.mcp.json` apunta a `norte mcp serve
  --session <nombre>`; el flujo humano es: el agente pide scope → `norte policy
  grant` → cada op `ask` llega al modal del TUI → `norte undo <sesión>` si algo
  salió mal. Criterio de salida de M3 cubierto por E2E.
- El puente añade una latencia de hop UDS por tool: irrelevante frente al coste
  de la op de FS y del turno del LLM.
- stdout del puente es exclusivo del transporte MCP; todo diagnóstico va a
  stderr (regla 10 aplica igual: jamás secretos/paths crudos en logs).
