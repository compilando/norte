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
  sha256(prev_hash ‖ campos con longitud prefijada y byte de presencia)`.
  **Alcance honesto de la integridad**: detecta corrupción y ediciones INGENUAS
  (las que no recomputan la cadena). NO es tamper-evidence frente a un atacante
  con acceso de escritura a la DB — reescritura total, truncación de COLA y
  rollback pasan `verify_chain` (keyless, genesis fijo). La evidencia real
  (firma/anclaje del head) es audit **M3-5** (issue #63). Hasta entonces, no
  presentar la garantía como tamper-evidence.

## Consecuencias

Dep estructural nueva (sqlx + su árbol de deps). Positivo: un solo motor para
journal/index/tags/embeddings (spec §4), ops/backup/tests simples. Negativas /
deuda:

- Los inserts se serializan (un escritor bajo `Mutex`) — aceptable (el journal
  no es el cuello de botella); si duele, batch. El `seq` se asigna DENTRO del
  lock (junto al encadenado) para que orden-seq == orden-hash; delegarlo al
  rowid rompería la cadena bajo concurrencia.
- **Durabilidad**: WAL + `synchronous=NORMAL` NO hace fsync por commit ante
  crash de OS/energía → una mutación recién ack-eada puede perder su entrada.
  «Durable antes del ack» es durabilidad de proceso, no de crash. Si el audit
  exige fsync duro, `synchronous=FULL` (coste) — decisión de M3-5.
- **Permisos**: el fichero se crea `0600` en unix (contiene metadatos de rutas,
  security-review). Los sidecars `-wal`/`-shm` heredan; el directorio per-user
  `0700` es responsabilidad del daemon.
- Licencias de sqlx verificadas en `cargo deny check licenses` (ok).
