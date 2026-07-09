# 0004 — Convenciones de wire del protocolo v0 (tipos, tolerancia, evolución)

- Estado: accepted
- Fecha: 2026-07-09
- Decisores: Claude (sesión M0, fase 4); auditado por protocol-guardian y rust-reviewer

## Contexto y problema

La fase 4 de M0 congela los primeros tipos del protocolo más allá de `VPath`
(ADR 0001): `Entry`, `Capabilities`, `Task*`, la taxonomía de errores (spec
§17.7) y los métodos `fs.*`/`task.*`. Todo lo que se decida aquí queda fijado
por golden tests; cambiarlo después es bump de versión. Hace falta un convenio
único de representación y — crítico — una política de evolución que haga
verdad el "el core soporta N y N-1" de la spec §11.

## Decisión

### Representación

- **Enums con datos:** internally-tagged con `tag = "kind"`, variantes y
  campos en `snake_case`. Ejemplo: `{"kind": "conflict", "conflict": "exists"}`.
- **Enums unit** (`EntryKind`, `TaskKind`, `ConflictKind`): string plano
  `snake_case`.
- **`CapabilityFlags`:** string `"RENAME_ATOMIC | SYMLINKS"` (formato
  `bitflags`), también en encodings binarios futuros (legibilidad > 4 bytes).
- **`TaskId`:** número JSON transparente (u64 del contador del scheduler).
- **Timestamps:** `mtime_ms: i64`, milisegundos desde epoch UTC (negativo =
  pre-1970; los FS reales los tienen).
- **Campos opcionales:** el emisor canónico escribe `null` explícito; el
  receptor acepta ausencia (`#[serde(default)]`). `None` significa "no se
  sabe", jamás un 0 fingido.
- **Golden tests:** igualdad estructural (`serde_json::Value`) bidireccional.
  El orden de claves y el whitespace NO son contrato; nombres, tipos y valores sí.

### Tolerancia y evolución (N/N-1)

| Superficie | Elemento desconocido | Comportamiento |
|---|---|---|
| Structs | campo extra | se ignora (serde default) |
| `EntryKind` | kind nuevo | degrada a `other` (`#[serde(other)]`) |
| `Error` | categoría nueva | degrada a `Unknown` (oculto, jamás emitido por el core) |
| `TaskState` | estado nuevo | degrada a `Unknown`, tratado como NO terminal (el cliente sigue escuchando) |
| `CapabilityFlags` | nombre nuevo bien formado (`[A-Z0-9_]+`) | se ignora: una capability es un anuncio; no conocerla = no explotarla |
| `CapabilityFlags` | hex (`0x…`) o token malformado | ERROR: bits sin nombre no viajan (`from_str` de bitflags los retendría en silencio — inaceptable) |

- Añadir variante/categoría/flag/campo-opcional: compatible.
- Quitar, renombrar o añadir campo requerido a variante existente: breaking →
  bump de `PROTOCOL_VERSION` (semver, arranca en `0.1.0`).
- `Error` y `TaskState` llevan `#[non_exhaustive]`: los frontends compilan con
  brazo `_` desde el día 1.
- Los fallback `Unknown` NO tienen fixture golden con nombres inventados: su
  tolerancia se pinnea en tests unitarios (`types.rs`), no en el corpus.

### Taxonomía §17.7 — extensiones

A la lista de la spec (`NotFound, PermissionDenied, Conflict{kind},
ProviderUnavailable{retryable}, Cancelled, PolicyDenied{rule}, EncodingLoss`)
se añaden ya, porque el copy engine de M0 las mapea desde `io::Error` en el
borde: `NoSpace` (ENOSPC/EDQUOT), `Io{retryable}` (EIO a mitad de operación —
distinto de `ProviderUnavailable`, que significa "no responde"), `Unsupported`,
`InvalidPath` e `Internal{panic}` (política de panics de §17.7). El detalle
humano viaja en el `message` del error JSON-RPC, nunca dentro de la taxonomía.

### Estados de Task — reconciliación spec↔wire

La spec (§10) nombra `Queued|Running|Paused|Done|Failed|Cancelled`; el wire usa
`pending|running|paused|completed|cancelled|failed`. El wire manda (la spec se
alinea en su próxima revisión). `paused` queda reservado desde v0 aunque M0 no
lo emita: estrenarlo en M1 no romperá a ningún cliente. `task.cancel` entra ya
en M0 (la cancelación es criterio de salida); `task.pause`/`task.list` llegan
con el daemon.

### Paginación futura de `fs.list`

M0 devuelve el listado completo. Cuando llegue la paginación por cursor (M1),
un core nuevo DEBE seguir devolviendo el listado completo a clientes que no
envíen cursor — jamás truncar en silencio a un N-1.

## Consecuencias

- ＋ Añadir superficie al protocolo (lo normal en M1–M4) casi nunca será breaking.
- ＋ Los frontends renderizan por categoría con degradación definida, no parsean strings.
- － Dos fallback `Unknown` son superficie de API "fantasma" que hay que
  documentar como jamás-emitida (test lo pinnea).
- － El parser de flags es propio (~30 líneas) en vez del de `bitflags`, para
  poder ignorar nombres y rechazar hex; queda confinado en `caps.rs`.
