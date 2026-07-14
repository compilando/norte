# 0020 — Journal: sqlx sobre SQLite (WAL), hash-chain propia

- Estado: accepted
- Fecha: 2026-07-14
- Decisores: oscar (dirección), Claude (propuesta técnica)
- Relacionado: spec §4/§10 (SQLite motor único, journal), regla dura 4, #11.
  Diseño: `docs/superpowers/specs/2026-07-14-m3-1-journal-design.md`.

## Contexto y problema

M3 (agéntico) exige un journal transaccional: toda mutación registrada con
op/origen/reversa/hash para habilitar undo (M3-2) y audit (M3-5). El engine ya
emite `Mutation` por `MutationObserver` (regla 4, no-op desde M0). Falta el
storage y la costura real.

## Decisión

- **sqlx** (feature `sqlite`, `runtime-tokio`) como capa de journal — async,
  await-eable desde el core sin actor de hilos aparte; runtime queries (sin DB
  en build). SQLite **WAL** + `synchronous=NORMAL`, un solo escritor serializado
  por un `Mutex` de la cadena. Alternativas evaluadas: rusqlite (sync, exigiría
  actor bloqueante para respetar la regla 2) y redb (rompe el SQLite-motor-único
  de spec §4). sqlx encaja con el índice/FTS5/embeddings futuros (mismo motor).
- **`on_mutation` pasa a async**: el insert se await-ea antes de completar la op
  (regla 4 — journal durable antes del ack). Consecuencia: migrar los
  call-sites de `ops.rs`.
- **hash-chain propia** (sha2, ya en el árbol): `entry_hash =
  sha256(prev_hash ‖ campos con longitud prefijada)`. Tamper-evident → base del
  audit export (M3-5).

## Consecuencias

Dep estructural nueva (sqlx + su árbol de deps). Positivo: un solo motor para
journal/index/tags/embeddings (spec §4), ops/backup/tests simples. Negativas /
deuda: los inserts se serializan (un escritor bajo `Mutex`) — aceptable (el
journal no es el cuello de botella); si duele, batch. Licencias de sqlx a
revisar en `cargo deny` (MIT/Apache esperado). El `seq` lo asigna la app
(monótono) porque el hash lo incluye — no se delega al rowid de SQLite.
