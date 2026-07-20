# GUI-b — mutaciones dual-pane (M5 hito 2, sub-proyecto 2) — diseño

- Fecha: 2026-07-20
- Estado: **IMPLEMENTADO** (T1–T7, cerrado 2026-07-20; commits 5dbc767..48c2b16,
  rama `gui-b-mutaciones`). `just ci` EXIT=0; norte-gui 24 tests + norte-frontend
  27 tests + clippy limpio. Reviewers aplicados: rust (por task + final),
  encoding (T3/final/render), security (T5).
  **Verificación GUI interactiva PENDIENTE de oscar** (headless: los subagentes
  no abren ventana GPUI): copy/move/delete real entre panes, progreso en la
  franja, cancel (F9→dest limpio/`.norte-partial`), conflicto (overwrite/skip),
  modal visible con nombre hostil saneado.
  Desviaciones (deuda anotada, no bloqueante):
  - Marcas por `VPath` ABSOLUTO completo (no "bytes del nombre"): `Entry` ya
    lleva el path absoluto, y `VPath` es `Hash+Eq` byte-exacto — más simple y
    sin ambigüedad. Tests de identidad hostil (`0xFF`) + gemelos NFC/NFD.
  - Conflicto detectado en el TERMINAL de la task (`Failed{Conflict}`), no en el
    submit (`copy` solo devuelve `Err` inmediato en rechazos pre-task). Multi-
    conflicto resuelto con **cola simple** (`conflict_backlog`), no pérdida
    silenciosa (spec §Decisión: "cola simple si colisionan").
  - `SubmitFailed` va a banner sin reintento automático; el banner usa el foco
    del momento de llegada (async) — puede caer en el pane que no lanzó la op.
  - Read-after-write: Copy relista SOLO el destino (no pisa cursor/marcas del
    origen); Move ambos; Delete el dir del path.
  - Franja: poda solo `Completed`; `Failed`/`Cancelled` quedan visibles (deuda
    **#83**: dismiss/TTL + navegación). Bulk = N tasks → N relists (deuda **#84**).
  - Cobertura: `forward_progress`/`on_task_terminal` sin test de harness real
    (requieren `TaskRef` interno de norte-core); lo puro sí (deuda **#85**).
  - `norte-gui` pinnea edición 2021 vs workspace 2024 → fmt drift recurrente +
    sin let-chains (deuda **#86**).
- Estado previo: aprobado (oscar); plan `docs/superpowers/plans/2026-07-20-gui-b-mutaciones-dual-pane.md`
- Contexto: M5 hito 2 = MVP de la GUI. Sub-proyectos: **GUI-a** (navegación
  dual-pane read-only, IMPLEMENTADO, spec `2026-07-19-gui-a-...`), **GUI-b**
  (este, mutaciones), GUI-c (keymap configurable), GUI-d (viewer), GUI-e
  (i18n + AccessKit). Cada uno spec→plan→impl propio. Construye sobre el
  dual-pane read-only de GUI-a (`crates/norte-gui`, EXCLUIDO del workspace).

## Objetivo

Convertir la GUI read-only en operativa: `copy`/`move`/`delete` ortodoxo (pane
activo → pane destino), marcas multi-select, progreso y cancelación de tasks,
resolución de conflictos. Cero mutación fuera del daemon (reglas 7 y 9: el
frontend SOLO habla `norte-proto` vía el `RemoteBackend` de `norte-core`).

## Decisiones (con el porqué)

1. **Backend persistente compartido (cierra deuda T4 de GUI-a).** El progreso
   de una mutación (`task.progress`) llega por la BOMBA de notificaciones del
   `RemoteBackend`, que exige una conexión VIVA. El patrón de GUI-a (reconectar
   por cada `cd`) no puede seguir progreso ni ver `policy.approval_required`/
   cancelación. GUI-b introduce **un único `RemoteBackend` vivo** toda la
   sesión, en un hilo con runtime tokio propio; los `cd` (listados) migran a
   este backend único — adiós reconnect-per-cd. Alternativa descartada
   (reconectar + `poll` de `task.status` por op): frágil, ciega a
   notificaciones, multiplica conexiones.

2. **Marcas multi-select en `PaneState` (norte-frontend, compartido).** La op
   ortodoxa actúa sobre un CONJUNTO marcado, o sobre el target del cursor si no
   hay marcas. Las marcas viven en el `PaneState` PURO de `norte-frontend`
   (crate ya compartido por GUI-a) — la TUI las hereda cuando adopte
   `PaneState` (deuda #82, no bloqueante). Identidad por BYTES del nombre
   (VPath), no por índice: sobrevive re-sort, se limpia en un listado nuevo.

3. **Modal de conflicto (fiel a la TUI).** Al chocar con un destino existente
   (`CollisionPolicy::Fail` → error `Conflict`), la GUI ofrece reintentar con
   `Overwrite`, `Skip`, o cancelar (campo `TransferOptions.on_collision`, en
   `norte-core::engine`). Mapea al `RetrySpec` de la TUI.

4. **Franja de tasks al pie (estilo `board` de la TUI).** Las tasks activas y
   su progreso se ven en una franja fija bajo los dos panes; errores terminales
   quedan visibles hasta descartar. Selección + cancelar sobre la task.

5. **Confirmaciones = UX, no policy.** La GUI conecta como humano (`User` →
   allow-all); NO hay prompts de policy (`approval_required` es solo para
   agentes). Los diálogos de confirmación son decisión de UX del frontend.

6. **Cancelación en alcance.** Cancelar una task en curso (`task.cancel` por
   wire) cierra el modelo ortodoxo (parar una copia larga → dest limpio o
   `.norte-partial`, garantía del core). Poco extra sobre el backend
   persistente (la bomba ya enruta el terminal `Cancelled`).

## Arquitectura

### Componente A — `norte-gui/src/session.rs` (backend persistente)

Un hilo (`std::thread`) con runtime tokio propio sostiene UN `RemoteBackend`
conectado toda la sesión. Dos canales `mpsc` cruzan el borde GPUI↔tokio:

- **Comandos** (GPUI → tokio), enum `SessionCmd`:
  - `List { pane: usize, generation: u64, dir: VPath }`
  - `Copy { from: VPath, to: VPath, opts: TransferOptions }`
  - `Move { from: VPath, to: VPath, opts: TransferOptions }`
  - `Delete { path: VPath, mode: DeleteMode }`
  - `Cancel { task_id: TaskId }`
- **Eventos** (tokio → GPUI), enum `SessionEvent`:
  - `Listed { pane, generation, dir, outcome: Result<Vec<Entry>, String> }`
    (reusa el guard de generación de GUI-a: `PaneListOutcome`).
  - `TaskUpdate { task_id, kind: TaskKind, progress: TaskProgress, state: TaskState }`
  - `TaskEnded { task_id, result: Result<TaskState, String> }`

Flujo de una mutación en el hilo tokio:
1. Recibe `Copy`/`Move`/`Delete` → `backend.copy(from,to,opts).await` (o
   `move_`/`delete`) → `TaskRef`.
2. Registra `task_ref.canceller()` en un `HashMap<TaskId, TaskCanceller>` local
   del hilo tokio (para servir `Cancel`).
3. Spawnea un forwarder por task: observa `task_ref.progress()` (un
   `watch::Receiver<TaskProgress>`) reenviando `TaskUpdate` en cada cambio;
   `task_ref.join()` da el terminal → `TaskEnded` (y de-registra el canceller).
4. Un `Copy`/`Move`/`Delete` que falla ANTES de crear la task (p. ej.
   `Conflict` inmediato, permisos, `Unsupported`) devuelve `Err` → se emite
   como `TaskEnded { result: Err(_) }` con el `task_id` sintético del comando,
   para que la franja y el resolutor de conflictos reaccionen igual.

El borde GPUI: el receptor de eventos se drena en un `cx.spawn` loop
(`while let Some(ev) = rx.recv().await { this.update(cx, |st, cx| { st.apply(ev); cx.notify() }) }`),
patrón del oneshot de GUI-a generalizado a un stream. El runtime tokio vive
mientras viva la ventana; al cerrarse, los `tx` se sueltan y el hilo sale.

**Errores/no-panic:** un daemon caído en `connect` → la sesión arranca en
estado de error visible (la franja lo muestra), jamás panic. Toda op fallida
viaja como evento, nunca aborta el render.

### Componente B — `norte-frontend::PaneState` (marcas)

Amplía el `PaneState` PURO existente:

- Campo `marks: std::collections::HashSet<Vec<u8>>` — bytes del nombre de la
  entrada marcada (el último segmento, no el VPath completo: identidad estable
  bajo re-sort dentro del mismo `dir`).
- `set_listing`/`begin_loading` LIMPIAN `marks` (un listado nuevo resetea la
  selección — comportamiento ortodoxo; una marca no debe sobrevivir a un `cd`).
- API nueva:
  - `toggle_mark(&mut self)` — togglea la marca de la entrada bajo el cursor
    (no-op si vacío o si el cursor cae en un filtro quick — decisión: en quick
    Filter, marca la entrada VISIBLE bajo el cursor del filtro).
  - `is_marked(&self, entry: &Entry) -> bool`.
  - `marks_len(&self) -> usize`.
  - `marked_names(&self) -> impl Iterator<Item = &[u8]>` (para el render).
  - `marked_paths(&self) -> Vec<VPath>` — resuelve las marcas a VPaths
    absolutos bajo `self.dir`; **si `marks` está vacío, devuelve el target del
    cursor** (`selected()`), o vacío si no hay entrada. Fuente única de "sobre
    qué opera la acción".
  - `clear_marks(&mut self)`.
- TDD puro (va al gate `just ci`): toggle, limpieza al re-listar,
  `marked_paths` con/sin marcas, marca bajo filtro quick, identidad por bytes
  (nombre hostil `0xFF`).

### Componente C — `norte-gui/src/modal.rs` (máquina de modales)

Enum `Modal` puro + transición por tecla, testeable sin GPUI (patrón
`key_to_action` de GUI-a):

- `ConfirmTransfer { kind: TransferKind, items: Vec<VPath>, to: VPath }` —
  `y`=confirma (emite el comando), `n`/`Esc`=cancela.
- `ConfirmDelete { items: Vec<VPath>, permanent: bool }` — default `permanent
  = false` (Trash); una tecla (`p`/`Tab`) togglea; `y`=confirma, `n`/`Esc`=cancela.
- `ConflictResolve { pending: PendingTransfer, kind: ConflictKind }` —
  `o`=reintenta con `CollisionPolicy::Overwrite`, `s`=`Skip`, `Esc`/`c`=cancela.
  `PendingTransfer` guarda `{ kind, from, to }` para reemitir (equivale al
  `RetrySpec` de la TUI).

Los modales renderizan nombres con `norte_frontend::display_name` (saneo +
badge hostil, criterio de GUI-a). Uno a la vez (cola simple si colisionan).

### Componente D — acciones + input (`norte-gui/src/input.rs`, `main.rs`)

- Nuevas acciones en el enum de `key_to_action` (extiende GUI-a):
  `ToggleMark`, `Copy`, `Move`, `Delete`, `CancelTask`, y las teclas de modal.
- Teclas: `Insert`/`Space`=toggle marca; `F5`=copy, `F6`=move, `Del`/`F8`=delete;
  con modal abierto, las teclas van al modal (captura fija, como los overlays de
  la TUI). En la franja: navegar tasks + tecla cancel.
- Semántica ortodoxa: `origen` = `marked_paths()` del pane ACTIVO; `destino` =
  `dir()` del pane INACTIVO. `F5`/`F6` abren `ConfirmTransfer`; `Del`/`F8` abre
  `ConfirmDelete`. Confirmar emite el `SessionCmd`.

### Componente E — franja de tasks (render en `main.rs`)

- Estado `tasks: Vec<TaskRow>` en el `AppState` (task_id, kind, progress,
  state, error opcional). `TaskUpdate`/`TaskEnded` la actualizan.
- Render fijo bajo los dos panes: una fila por task activa (kind + % +
  estado); las terminadas con error quedan hasta descartar (tecla).
- Cursor de franja + tecla cancel → `SessionCmd::Cancel`.
- **Read-after-write:** al recibir `TaskEnded` OK de una op, la GUI relista el
  `dir` de origen y el de destino (emite `List` para los panes afectados) para
  reflejar el cambio.

## Flujo de datos (copy)

```
usuario F5 (pane activo con 2 marcas)
  → key_to_action → Action::Copy
  → main: items = pane_activo.marked_paths(); to = pane_inactivo.dir()
  → Modal::ConfirmTransfer{Copy, items, to}
usuario 'y'
  → SessionCmd::Copy por cada item (o un comando batch por item)  [decisión: uno por item, opts default = Fail]
  → hilo tokio: backend.copy(from,to,opts) → TaskRef → registra canceller → forwarder
  → SessionEvent::TaskUpdate* → franja pinta %  ... → TaskEnded OK
  → main: relista pane origen + pane destino
Si TaskEnded Err(Conflict):
  → Modal::ConflictResolve{pending, ConflictKind}
  → 'o' → SessionCmd::Copy con opts.on_collision = Overwrite
```

## Manejo de errores

- `connect` falla → estado de error en la franja, ventana usable (solo lectura
  falla igual, sin panic).
- Op falla (permisos, `Unsupported`, `Conflict`) → `TaskEnded Err` → franja o
  modal de conflicto. Nunca panic, nunca dest a medias sin marcar.
- `Delete` con `Trash` sobre un provider sin cap `TRASH` → `Unsupported`; el
  modal de delete permite elegir `Permanent` explícito o el error se muestra.
- Cancelación → `Cancelled` terminal → franja; el core garantiza dest limpio o
  `.norte-partial`.

## Testing

- **norte-frontend (gate `just ci`):** TDD puro de las marcas (toggle,
  limpieza al re-listar, `marked_paths` con/sin marcas y bajo filtro quick,
  identidad por bytes con nombre hostil).
- **norte-gui (excluido del workspace, política spike):** SIN tests de render;
  fn puras CON test dentro del crate: `key_to_action` extendido (nuevas
  acciones + captura de modal), `Modal` state machine (confirmar/cancelar/
  toggle permanent/resolución de conflicto → comando esperado), resolución
  marcas→acción. Verificación manual contra `norte daemon run`: copy/move/delete
  real entre panes, progreso en la franja, cancelación (dest limpio), conflicto
  (overwrite/skip), nombre hostil saneado en modales.

## Fuera de alcance (sub-proyectos posteriores)

- `mkdir`/`rename` (GUI-b2 o GUI-c).
- Keymap configurable (GUI-c) — GUI-b usa teclas hardcodeadas mínimas.
- Viewer F3 (GUI-d), i18n + AccessKit (GUI-e).
- Undo de sesión desde la GUI (futuro; el core ya lo soporta).
- Unificar el `Pane` de la TUI sobre `PaneState` (deuda #82).
- Compartir el `RemoteBackend` con la TUI (cada frontend tiene el suyo).

## Deuda esperada (a anotar en el cierre)

- El forwarder de progreso por task no es cancelable finamente aparte del
  `task.cancel` (la task del daemon SÍ se cancela; el forwarder local sale al
  ver el terminal).
- `SessionCmd::Copy` uno-por-item (no un batch atómico): N tasks
  independientes. Batch/cola es optimización posterior.
- El re-list post-op relista de una (sin streaming incremental — como GUI-a).
