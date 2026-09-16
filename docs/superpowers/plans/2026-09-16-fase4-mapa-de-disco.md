# Fase 4 — Mapa de disco

Spec: `docs/superpowers/specs/2026-09-15-historia-y-wow-design.md`, «Fase 4 —
Mapa de disco». Sale de la fase 3 (`d8c3f1d0`), que dejó `StyledFrame`, `Hit` y
un hueco que un frontend sabe pintar.

**Qué es**: un hueco que enseña qué ocupa el directorio que estás mirando, como
un treemap. Pulsar un rectángulo entra en ese hijo.

## Lo que el reconocimiento dejó decidido

- **Core, no plugin.** Recorrer un `$HOME` son millones de entradas y una tarea
  cancelable (regla 3); el presupuesto de `norte:location` (4096 llamadas) lo
  haría fallar en cualquier árbol de verdad. La ADR lo dice.
- **Tarea + INFORME, no tarea a secas.** `fs.dir_size` no devuelve tipo alguno
  a propósito: su total «viaja en el progreso (`bytes_done`/`entries_done`)…
  el último snapshot ES el resultado». Eso vale para UN número. Una lista de
  hijos con su tamaño no cabe ahí, y el protocolo ya lo dice de sus hermanos:
  «N digests no caben en el desenlace de una Task… y el progreso solo sabe
  contar». Así que el reparto es el de `fs.checksum` / `archive.pack` /
  `fs.rename_batch`: la Task hace el trabajo y un método aparte dice qué salió.
- **El recorrido es HERMANO de `ops::dir_size`, no una reutilización.** Aquel
  acumula contadores planos y no materializa nada a propósito; esto necesita un
  acumulador POR HIJO. Se le copian las costumbres, que son las que importan:
  la cancelación devuelve `Cancelled` en el acto, un directorio ilegible suma a
  un contador en vez de matar el recuento, y ese contador es lo que `partial`
  cuenta.
- **El marco del mapa es NUESTRO.** `zona_puede` existe porque en un panel de
  plugin la etiqueta Y el comando los elige un tercero (ADR 0116). Aquí los
  elige `squarify`, así que sus `Hit` NO pasan por ese filtro — y un plugin no
  puede fabricar un marco de `disk-map`, porque el kind es de casa y su marco
  sale de esta función.
- **`Hit.arg` estrena uso, y sigue sin ser una ruta.** El arg de un rectángulo
  es el NOMBRE del hijo; quien lo recibe lo resuelve contra el directorio que el
  mapa está enseñando. ADR 0116 fijó que `arg` no es nunca una ruta y esta fase
  no lo rompe: una ruta en el arg sería un segundo camino para nombrar un
  fichero, esquivando el que ya pasa por el gate.

## T1 — Protocolo 0.75.0

1. `FS_DIR_USAGE` (`Request`, `Task` → `FsTaskResult`) con
   `FsDirUsageParams { path, depth }`. `depth` es `1` hoy y va en el wire
   porque un mapa de dos niveles es la primera cosa que alguien pedirá.
2. `FS_DIR_USAGE_REPORT` (`Request`, `Direct`) con
   `FsDirUsageReportParams { task_id }` → `FsDirUsageReportResult { children,
   total_bytes, total_entries, pending, listed, omitted }`, con
   `DirUsageChild { name: Segment, kind, bytes, entries, partial }`.

   **`name` es un `Segment`**, no bytes sueltos ni una ruta: es el tipo de
   nombre de este protocolo, codifica sin pérdida lo que no es UTF-8 (regla 1)
   y **rechaza separadores, NUL y `.`/`..` DESPUÉS de decodificar**, así que un
   `%2E%2E` no cuela un `..`. Eso convierte el «el arg nunca es una ruta» del
   ADR 0116 en un invariante del TIPO, y no en una comprobación que alguien
   tenga que acordarse de escribir.

   **Tres señales, tres preguntas distintas.** `pending` —cuántos hijos
   conocidos faltan por medir— solo significa algo con `listed` en `true`:
   hasta que termina el listado de la raíz no se sabe cuántos hijos hay, así
   que una Task cancelada listando informaría `0` y se leería como un mapa
   completo. `omitted` son los hijos que existen y no caben en
   `DIR_USAGE_MAX_CHILDREN`, y sus bytes SÍ están en los totales: lo que se
   pierde es su nombre, no su tamaño. Y `partial` va por HIJO, porque lo que
   un mapa puede pintar es el rectángulo que es una cota inferior; una bandera
   global solo sabe apagar el mapa entero.
3. `TaskKind::DirUsage`, y en `norte_frontend::tasks::counts_as_work` va con
   `DirSize` y `Checksum`: **no es trabajo de tablero**. Mide, no muta, y el
   panel de procesos no tiene por qué abrirse solo porque alguien mire un
   directorio.

   Ojo, porque aquí hay una trampa que costó encontrar: ese `!matches!` es por
   exclusión, así que una clase nueva entra como trabajo sin que nadie lo
   decida, y el test de al lado **tampoco** rompe la compilación — itera un
   array escrito a mano, y lo que no está en el array no se prueba. Hay que
   añadirla a los dos sitios.
4. Las seis puertas de completitud del ADR 0089: catálogo, `catalogo.tsv`,
   `golden_types.rs` con sus casos, `methods.json`, `tests/schema.rs` y
   `docs/schema/proto.schema.json`.
5. **`protocol-guardian` aquí**, antes de seguir.

## T2 — El núcleo mide

1. `ops::dir_usage(root, provider, depth, ctx)`: un acumulador por hijo, y el
   progreso sigue contando en `bytes_done`/`entries_done` para que la franja de
   tareas y el panel de procesos lo pinten como cualquier otra.
2. `engine::dir_usage_as` con su anillo de informes, calcado de
   `checksum_reports`: guarda `(task_id, actor, informe)`, el informe es un
   SNAPSHOT (parcial mientras corre, definitivo al terminar), y el camino de
   lectura no panica con un lock envenenado.
3. Daemon: `read_gate` sobre la ruta, y el informe además comprueba que el que
   pregunta podía ver esa task — por eso el anillo guarda el actor.
4. `Backend::dir_usage` + `dir_usage_report` (embebido y remoto) y el método
   del SDK.

### Lo que T2 dejó dicho (`f5a94171`)

Hecho, con cuatro revisores encima. Tres cosas que las tareas siguientes NO
tienen que volver a descubrir:

- **`Hit.arg` lleva la forma WIRE del `Segment`**, no `display_lossy` ni un
  `String` supuesto UTF-8, y quien lo recibe resuelve
  `padre.join(Segment::parse_wire(arg))`. El informe no trae la raíz a
  propósito, así que el cliente reconstruye — y ahí es donde un nombre no-UTF8,
  uno en NFD o uno llamado `!` se vuelve inalcanzable o abre el fichero
  equivocado. El terminal ENMASCARA para pintar: esa forma no puede ser la que
  vuelve.
- **`TaskKind::DirUsage` no tiene etiqueta en ningún frontend**, y nada se
  pondrá rojo por ello: los dos `match` acaban en comodín (`panels.rs` →
  `"task"`, `clase_de_task` → `"unknown"`) porque `TaskKind` es
  `#[non_exhaustive]`, y el guard de la ventana es una lista escrita a mano.
  Hace falta el arm en los dos, `gui-task-kind-dir-usage` en ambos locales, y
  un hermano de `DirSize` en `refresh.rs::habla_por_su_informe`.
- **La poda por `walk_exclusions` no está probada, y no se puede probar hoy.**
  Lee el directorio de config del proceso por la función libre
  `protected_roots()`, no por el inyectable
  `ScopeRegistry::with_protected_roots`, así que un test sobre `mem://` pasa
  con la poda y sin ella. Tampoco la prueban `fs.search`, `fs.compare` ni
  `archive.pack`, que la usan desde antes. Primero la costura; fingir el test
  es peor que no tenerlo.

## T3 — El treemap, compartido

1. `norte_frontend::treemap::squarify(children, cols, rows) -> StyledFrame`:
   el algoritmo clásico de rectángulos cuadrados, colores por CLASE de fichero
   con los roles del tema (nada de colores crudos: el tema manda, ADR 0037), y
   un `Hit` por rectángulo con `command = "nav.enter"` y `arg = nombre`.
2. Caché de tamaños por `VPath` en el frontend, invalidada por el ping de
   `DirWatch` (un `()` por ráfaga, que es justo «algo cambió aquí»).
3. Tests de reparto: los rectángulos no se solapan, cubren el área, y un hijo
   que no llega a una celda no se pinta pero tampoco desaparece de la cuenta.

## T4 — El terminal

1. Kind de serie `disk-map` en `builtin()`, con su mínimo justificado.
2. Comando `layout.disk-map` en el catálogo y **en los siete presets** (o el
   motivo en la cabecera de divergencias del que no lo ate).
3. Pintado con el `StyledFrame` que ya sabe pintar la fase 3, y el clic
   resolviendo `hit_at` → `nav.enter` del hijo.

## T5 — La ventana

1. `SlotView::DiskMap` (puente 71) y `render/diskmap.ts`.
2. La misma regla que el panel de plugin: el renderer manda la CELDA, el host
   resuelve. Aquí el host además NO filtra por `zona_puede`, porque el marco es
   suyo.

## T6 — ADR, ayuda y cierre

ADR 0117 (por qué core y no plugin; por qué tarea+informe; por qué el marco de
casa no pasa por el filtro de los de plugin), tema de ayuda en los dos idiomas,
CHANGELOG, memoria.
