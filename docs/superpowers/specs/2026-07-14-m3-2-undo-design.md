# M3-2 — Undo de sesión — Design

- Fecha: 2026-07-14
- Estado: diseño aprobado (pendiente plan + implementación)
- Relacionado: spec §5/§10 (journal + undo + audit), regla dura 4 (toda mutación
  pasa por el journal), ADR 0009 (papelera → restore), ADR 0020 (journal sqlx
  hash-chain), M3-1a/M3-1b (journal + `Reversal` persistido + `reversal_ref`).
  Criterio de salida M3: "Claude Code gestiona un dir bajo policy ask, con undo
  de sesión completa".

## Contexto

M3-1 dejó el journal registrando cada mutación con su `Reversal`
(`Delete`/`RenameBack`/`RestoreTrash`/`Irreversible`) y su `reversal_ref` (ruta
de papelera lógica; `None` para papelera nativa del OS). M3-2 **ejecuta** esas
reversas: `Engine::undo_session(actor)` deshace en LIFO todas las mutaciones
revertibles de una sesión, sin pisar nunca el trabajo del usuario, y registra
cada undo como una entrada compensatoria (append-only, la cadena sigue íntegra).

## Decisiones (brainstorming)

1. **Modelo = append de entradas compensatorias.** Un undo NO muta ni marca la
   entrada original: appendea la op inversa con `undoes_seq = seq_original`. La
   cadena hash sigue append-only e íntegra (preserva el tamper-evidence de 1b);
   redo queda habilitado (deshacer la compensación) aunque no se expone; el
   doble-undo se evita saltando lo ya compensado.
2. **Granularidad = sesión, sobre primitiva de 1 entrada.** Primitiva interna
   `revert_entry(&entry, ctx)` (1 reversa + 1 compensación). API pública
   `undo_session(actor)` que la aplica en LIFO a las entradas no-compensadas de
   esa sesión.
3. **Trash nativo local = restore por ruta original.** Nuevo
   `Provider::restore_trashed(original)`; local lo implementa vía
   `trash::os_limited::list()` + match por ruta original **más reciente** +
   `restore_all`. `reversal_ref` sigue `None`; la clave es `entry.path`. Falla
   limpio si la plataforma no soporta `os_limited` o hay ambigüedad irresoluble.
4. **Drift = estricto + para en el primer bloqueo.** Cada reversa verifica antes
   de actuar; jamás sobrescribe (patrón norte). La sesión aplica LIFO hasta el
   primer paso bloqueado, ahí para y reporta. Las compensaciones ya aplicadas
   son reales (no hay rollback del undo).
5. **Superficie = `Engine::undo_session` como Task; proto/MCP a M3-4.** Se prueba
   embebido (igual que 1b). La exposición JSON-RPC/daemon/norte-mcp llega cuando
   el daemon es dueño único del journal (M3-4).
6. **Irreversible se SALTA** (no bloquea): no hay nada que pisar; bloquear haría
   in-deshacible cualquier sesión con un borrado permanente.
7. **La compensación usa el actor de la Task de undo** (quién deshace), no el
   original.

## Arquitectura

### Selección de entradas revertibles (por actor)

Una entrada es revertible si:
- `undoes_seq IS NULL` (no es ella misma una compensación), **y**
- `seq NOT IN (SELECT undoes_seq FROM journal WHERE undoes_seq IS NOT NULL)`
  (nadie la ha compensado aún), **y**
- su actor `= target`.

`undo_session` las recorre en orden **DESC de `seq`** (LIFO: se deshace primero
lo más reciente).

Filtrado del actor: `Actor::Agent { session }` filtra por esa sesión exacta;
`Actor::User` (todo M3-1) no tiene frontera de sesión, así que `undo_session(
User)` cubre todas las entradas de usuario no-compensadas — suficiente para
probar el mecanismo; la semántica de sesión-de-agente se ejerce de verdad en
M3-4. El match de actor compara `(actor_kind, actor_id)`.

### Ejecución por `Reversal` (`revert_entry`)

| Reversal | Acción | Verificación estricta previa |
|---|---|---|
| `Delete` | `provider.remove(path)` | el nodo en `path` existe y (si el provider da `node_id`) conserva identidad de lo creado; si difiere/ausente → bloquea |
| `RenameBack` | `provider.rename(path, path_to)` (src=`path`=destino renombrado, dst=`path_to`=origen) | `path_to` (origen) LIBRE y `path` (destino actual) existe; origen ocupado → `Conflict`, bloquea |
| `RestoreTrash` + `reversal_ref=Some(dest)` | `provider.rename(dest, path)` | `path` (original) LIBRE; ocupado → bloquea |
| `RestoreTrash` + `reversal_ref=None` | `provider.restore_trashed(path)` | `path` LIBRE + item hallado en la papelera nativa; si no → bloquea |
| `Irreversible` | — (skip) | se cuenta en `skipped_irreversible`, no bloquea |

Notas de mapeo (recordar el encoding de 1b): en un `Renamed` el journal guardó
`path = to` (destino) y `path_to = from` (origen), con `Reversal::RenameBack`.
Deshacer = `rename(path=to → path_to=from)`, es decir devolver el nodo a su
nombre original.

"LIBRE" = `stat` da `NotFound`. La ventana TOCTOU entre el check y la acción es
la misma que el resto del engine (documentada); el objetivo es no pisar en el
caso común, no atomicidad dura.

### Parada y reporte

```rust
pub struct UndoReport {
    /// Entradas revertidas con éxito (compensaciones appendeadas).
    pub undone: u64,
    /// Entradas `Irreversible` encontradas y saltadas.
    pub skipped_irreversible: u64,
    /// Primer paso bloqueado (si lo hubo): seq original + motivo.
    pub blocked: Option<(i64, Error)>,
}
```

La Task recorre LIFO; en el primer `Reversal` que falla su verificación o su
acción, para y devuelve el reporte con `blocked = Some((seq, err))`. Cancelación
(`CancellationToken` chequeado entre pasos) = corte limpio: las compensaciones
ya aplicadas quedan, el resto no se toca; devuelve el reporte con lo hecho.

`undo_session` devuelve un `TaskHandle`; el `UndoReport` se entrega como
resultado de la Task (junto al `TaskState`). Progreso: `entries_total` = nº de
revertibles al arranque; `entries_done` avanza por paso.

### Journal (schema + hash-chain)

Tabla `journal` gana `undoes_seq INTEGER NULL` (FK lógica al `seq` compensado).
El campo entra en `chain_hash` (con byte de presencia, como los demás
opcionales) → sigue tamper-evident. Génesis intacto. Greenfield: no hay journals
en producción (el wiring de binarios se difirió a M3-4), así que `CREATE TABLE`
con la columna basta — sin migración.

Cambios de API del journal:
- `record(..., undoes_seq: Option<i64>)` — un parámetro más; los call-sites
  normales pasan `None`, las compensaciones pasan `Some(seq_original)`.
- `JournalEntry` gana `undoes_seq: Option<i64>`; `entries()` y `verify_chain`
  leen la columna nueva.
- Helper de lectura para la selección de revertibles: `revertible_for(actor)` (o
  `undo_session` consulta directamente). Devuelve los `JournalEntry` a revertir,
  en orden DESC, ya filtrados por no-compensados.

`SqliteJournal::on_mutation` mantiene `undoes_seq = None` (las mutaciones del
engine normales no compensan). Las compensaciones las escribe `revert_entry` con
un `record(..., Some(seq))` directo (no vía `Mutation`), porque necesita fijar el
`undoes_seq` y el mapeo op→Reversal inverso.

### Provider trait

```rust
/// Restaura desde la papelera NATIVA del OS el ítem cuya ruta original es
/// `original` (undo de un `Trashed` sin `reversal_ref`, ADR 0009). Default
/// `Unsupported`. Solo el provider local lo implementa (vía el crate `trash`,
/// os_limited): lista la papelera, casa por ruta original el ítem MÁS RECIENTE
/// y lo restaura. Falla limpio (`Unsupported`/`NotFound`/`Conflict`) si la
/// plataforma no soporta el listado, no hay match, o el destino está ocupado.
async fn restore_trashed(&self, original: &VPath) -> Result<(), Error> {
    let _ = original;
    Err(Error::Unsupported)
}
```

Local: `trash::os_limited::list()` en `spawn_blocking`; filtra por
`original_path == native(original)`; toma el de `time_deleted` mayor; verifica
que `original` está libre; `os_limited::restore_all([item])`. Windows/plataformas
sin `os_limited` → `Unsupported` (documentado; el undo lo reporta como bloqueo).

### Engine

```rust
/// Deshace en LIFO las mutaciones revertibles de la sesión `actor`, sin pisar
/// el trabajo del usuario (estricto: para en el primer conflicto). Cada undo se
/// registra como entrada compensatoria (append-only). Task cancelable.
pub async fn undo_session(&self, actor: Actor) -> Result<TaskHandle, Error>;
```

La Task: lee `revertible_for(actor)` del journal, fija `entries_total`, recorre
DESC, y por cada entrada llama `revert_entry` (resuelve el provider por el scheme
del path, ejecuta la reversa verificada, y `record` la compensación con
`undoes_seq`). El actor de las compensaciones = el `ctx.actor` de la Task de undo
(default `User` en modo embebido). Chequea `CancellationToken` entre pasos.

El Engine necesita el journal para leer entradas: hoy el observer es un
`Arc<dyn MutationObserver>` opaco. `undo_session` requiere acceso de LECTURA al
journal. Opción: el Engine guarda además un `Option<Arc<SqliteJournal>>` (o un
trait de lectura del journal) inyectado junto al observer. Decisión de plan:
extender `Engine::with_observer` o añadir un `with_journal(Arc<SqliteJournal>)`
que registre el mismo objeto como observer y como fuente de lectura del undo.
Sin journal → `undo_session` devuelve `Unsupported`.

## Testing

Embebido (`SqliteJournal::open_in_memory` + `MemProvider`/`LocalProvider`),
patrón de los tests de 1b:

- **Undo de `Created`**: copy → dst existe → `undo_session` → dst borrado;
  compensación `removed` con `undoes_seq` correcto; `verify_chain` ok.
- **Undo de `Renamed`**: move a→b → `undo_session` → a restaurado, b ausente.
- **Undo de `Trashed` lógico** (MemProvider con dest sintético o provider que dé
  `Some`): restaura al original. (Ver deuda H2 de 1b: para ejercitar `dest=Some`
  vía engine hace falta `logical_trash` en MemProvider; alternativa: sembrar el
  journal + FS y llamar `revert_entry`.)
- **Undo de `Trashed` nativo** (`LocalProvider`, tempdir): borrado a papelera →
  `undo_session` → restaurado; `#[cfg]`/skip si `os_limited` no soporta la
  plataforma del runner.
- **Drift bloquea**: recrear algo en el destino antes del undo → paso bloqueado,
  para, `blocked = Some((seq, Conflict))`, lo previo revertido.
- **Irreversible se salta**: sesión con un `Removed` permanente + un `Created` →
  undo revierte el created, cuenta 1 en `skipped_irreversible`, no bloquea.
- **Doble-undo idempotente**: `undo_session` dos veces → la 2ª no re-ejecuta
  (todo ya compensado); `undone = 0`.
- **Cancelación limpia**: cancelar a mitad → corte entre pasos, reporte parcial,
  compensaciones aplicadas coherentes.
- **Filtro de actor**: entradas de dos actores distintos → `undo_session(A)` no
  toca las de B.
- **Hash-chain con `undoes_seq`**: `verify_chain` sigue detectando manipulación
  con la columna nueva en el hash.

## No-objetivos (YAGNI en M3-2)

- Redo (la primitiva compensatoria lo habilita; no se expone).
- Exposición proto/JSON-RPC/daemon + norte-mcp → M3-4.
- `undo_last` / undo por rango arbitrario.
- Enforcement por policy/scope (quién puede deshacer qué) → M3-3.
- Anclaje criptográfico del head (#63) → M3-5.

## Deuda / riesgos

- `restore_trashed` depende de `trash::os_limited`, cuyo soporte varía por
  plataforma; Windows a validar (probable `Unsupported`, issue nueva). El test se
  skipea donde no aplica; el nightly/matrix real lo cubre.
- La lectura del journal desde el Engine acopla `undo_session` a `SqliteJournal`
  concreto (no al trait `MutationObserver`). Se acota con un accessor/inyección
  explícita; el trait de observer no cambia.
- Consumir la deuda H2 de 1b (MemProvider `logical_trash`) daría un test de undo
  de trash lógico vía engine end-to-end; si no, se cubre con `revert_entry`
  directo sobre journal+FS sembrados.

## Decomposición (para el plan)

- **2a** — journal: `undoes_seq` (schema + `chain_hash` + `record` + `entries`/
  `JournalEntry` + `verify_chain`) + selección `revertible_for(actor)`. Unit
  tests puros.
- **2b** — `Provider::restore_trashed` (trait default + impl local os_limited) +
  `Engine::undo_session` Task + `revert_entry` + `UndoReport` + acceso de lectura
  del journal desde el Engine. Tests de integración engine↔journal↔provider +
  cancelación.
