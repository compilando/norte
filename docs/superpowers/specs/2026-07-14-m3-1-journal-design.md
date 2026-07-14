# M3-1 — Journal transaccional — Design

- Fecha: 2026-07-14
- Estado: diseño aprobado (pendiente plan + implementación)
- Relacionado: spec §5/§10 (journal + undo + audit), regla dura 4 (toda mutación
  pasa por el journal), spec §4/§291 (SQLite motor único), ADR 0009 (papelera →
  restore), issue #11 (journal + GC de staging), #32 (desambiguación del
  `Created`). Kickoff M3, primer sub-proyecto.

## Contexto

M3 (agéntico) exige un journal transaccional: toda mutación (humana o agéntica)
registrada con op, origen, antes/después y hash, para habilitar undo (M3-2) y
audit export (M3-5). El engine YA emite `Mutation` (Created/Removed/Trashed/
Renamed) por la costura `MutationObserver` (regla 4), no-op desde M0. M3-1
enchufa el journal real ahí. Undo y audit se construyen ENCIMA en sub-proyectos
posteriores.

## Decisiones (brainstorming)

1. **Storage = sqlx (SQLite)**, async. SQLite WAL, `synchronous=NORMAL`, un solo
   escritor. Runtime queries (sin DB en build). Coherente con spec §4 (SQLite
   motor único para journal/index/tags/embeddings).
2. **`MutationObserver::on_mutation` pasa a `async`** (`async_trait`): la task
   await-ea el insert antes de completar la op (regla 4: journal durable antes
   del ack). Consecuencia: migrar los call-sites en `ops.rs`.
3. **Record model** = actor + refs de reversa, SIN copiar contenido: cada entrada
   guarda cómo revertir (Trashed→ruta `.norte-trash/<id>`, Renamed→from,
   Created→delete, Removed→irreversible); el undo (M3-2) reconstruye desde
   trash/rename. Barato, habilita undo sin duplicar bytes (redundante con la
   papelera de fase 9).
4. **Ubicación**: módulo `journal` en `norte-core` (AGPL, spec §71).

## Arquitectura

### Storage y costura

- Dep `sqlx` (features `sqlite`, `runtime-tokio`). SQLite WAL. Runtime queries
  (`sqlx::query(...)`), no macros compile-time (evita DB en build).
- `MutationObserver::on_mutation(&self, m: &Mutation, actor: &Actor)` →
  **`async`**. `SqliteJournal` lo implementa; `NoopObserver` sigue para tests
  sin journal. El insert se await-ea en la task antes de marcar la op completa.

### Schema

Tabla `journal`:

```
seq          INTEGER PRIMARY KEY   -- monótono
ts_ms        INTEGER               -- epoch ms
actor_kind   TEXT                  -- 'user' | 'agent' | 'plugin'
actor_id     TEXT NULL             -- session id del agente / id del plugin
op           TEXT                  -- 'created'|'removed'|'trashed'|'renamed'
path         BLOB                  -- VPath::to_wire() (bytes, no-UTF8 safe)
path_to      BLOB NULL             -- renamed: destino
reversal     TEXT NULL             -- 'delete'|'rename_back'|'restore_trash'|'irreversible'
reversal_ref BLOB NULL             -- ej. ruta .norte-trash/<id> del Trashed
prev_hash    BLOB                  -- hash de la entrada anterior
entry_hash   BLOB                  -- H(prev_hash ‖ serialización canónica)
```

### Hash-chain

`entry_hash = sha256(prev_hash ‖ seq ‖ ts_ms ‖ actor_kind ‖ actor_id ‖ op ‖
path ‖ path_to ‖ reversal ‖ reversal_ref)` con separadores no ambiguos (longitud
prefijada por campo para evitar colisiones de concatenación). Génesis
`prev_hash = [0u8; 32]`. Verificación: recorrer las entradas recomputando el
hash; una manipulación rompe la cadena → base del audit export (M3-5). Dep `sha2`
(ya en el árbol, fase 4).

### Actor (origen)

Tipo `Actor` en `norte-core`:

```rust
pub enum Actor {
    User,
    Agent { session: String },
    Plugin { id: String },
}
```

Vive en el `TaskCtx`. El engine lo fija al lanzar la task: **default `User`**
(frontends locales). Los agentes lo pondrán vía scopes/MCP (M3-4); hoy toda
entrada es `User`. `on_mutation` recibe el actor de la task.

### Mapeo Mutation → reversal

| Mutation | reversal | reversal_ref |
|---|---|---|
| `Created(p)` | `delete` | — (borrar `p`) |
| `Renamed{from,to}` | `rename_back` | `from` (en `path_to` ya está) |
| `Trashed(p)` | `restore_trash` | ruta en `.norte-trash/<id>` (remotos, fase 9) o handle OS (local, ADR 0009) |
| `Removed(p)` | `irreversible` | — |

M3-1 REGISTRA el reversal; M3-2 lo EJECUTA. Para `Trashed` en remotos, la
`reversal_ref` es la ruta de papelera (el `trash()` de fase 9 la conoce — se
propaga por el `Mutation` o se deriva). Nota: hoy `Mutation::Trashed(p)` solo
lleva el path original; capturar la `reversal_ref` de la papelera lógica puede
exigir extender `Trashed` con la ruta de destino (decisión de plan 1b; para
trash NATIVO local el ref es el handle del crate `trash`, resuelto en M3-2).

### Durabilidad (regla 4)

El insert sqlx se await-ea en la task antes de marcar la op completa. Los
observers se llaman POST-efecto (la mutación ya se aplicó con éxito). Borde
documentado: mutación aplicada + fallo de journal → la task falla ruidosa (no se
traga); el arranque reconcilia por hash-chain (M3-5/undo detectan la última
entrada íntegra). No hay divergencia silenciosa FS↔journal.

### GC de staging huérfano (#11)

Al abrir el journal (arranque del core), barrer `.norte-partial.*` de crashes
previos reusando `gc_partials` de `vfs-local` (fase 4: reconoce el staging por
FORMA exacta, no por prefijo — no barre backups del usuario). El GC NO se
registra como mutación de usuario (no es op agéntica); solo `tracing`.

## Testing

- Unit (SQLite `:memory:` o tempfile por test):
  1. Cada `Mutation` → entrada correcta con actor y reversal esperados.
  2. **Hash-chain**: cadena íntegra sobre N entradas; alterar una entrada rompe
     la verificación.
  3. Durabilidad: la op no se marca completa hasta que el insert está.
  4. Paths hostiles: `to_wire()` en `path`/`path_to`/`reversal_ref` round-trip
     byte-exacto (corpus `norte-testkit`).
  5. GC: barre solo el staging con la forma exacta, respeta backups del usuario.
- Integración: engine con `SqliteJournal` real → copy/move/delete/trash generan
  las entradas esperadas en orden y con hash-chain válida.

## No-objetivos (YAGNI en M3-1)

- Undo (ejecutar la reversa) → M3-2.
- Audit export CSV/JSONL → M3-5.
- Enforcement por actor / policy → M3-3.
- Actor agéntico real (session id) → llega con MCP (M3-4); hoy default `User`.
- Índice/FTS5/embeddings (mismo SQLite, pero otro sub-proyecto/hito).

## Decomposición de PRs

- **1a** — dep `sqlx` + módulo `journal` (schema, open, insert, hash-chain +
  verificación) + tipo `Actor` + `on_mutation` async (migra call-sites de
  `ops.rs`). Unit tests puros del journal.
- **1b** — wiring en el engine (`Actor::User` default en `TaskCtx`) + captura de
  `reversal_ref` para `Trashed` (posible extensión de `Mutation`) + GC al
  arranque + tests de integración engine↔journal.
