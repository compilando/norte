# Estudio: la cola de operaciones y la barra de progreso ligera

Fecha: 2026-09-22. Punto 6 de `docs/batida-de-funcionalidades-2026-09-20.md`,
ampliado con una petición: una **barra de progreso pequeña y liviana** que
aparezca sola para las tareas —sobre todo las rápidas, como copiar un
fichero— sin tener que abrir nada.

## 1. Qué hay hoy

**En el core** (`norte-core/src/scheduler.rs`):

- Cada operación larga es una task con `CancellationToken` y un `watch` de
  `TaskProgress` que el emisor coalesce a ~30 Hz.
- El planificador tiene **4 huecos por scheme** (`Scheduler::new(4)`): cuatro
  copias a `file://` corren A LA VEZ, y la quinta espera en un `BinaryHeap`
  por prioridad y orden de llegada. No hay «cola» en el sentido de Krusader o
  Total Commander (una detrás de otra); hay paralelismo con techo.
- `TaskState::Paused` **existe en el protocolo** desde M0, reservado y nunca
  emitido: estrenarlo no rompe a clientes N-1. No hay `task.pause`.
- La copia en streaming comprueba la cancelación **por chunk**
  (`ops.rs` ~2008). La copia servidor-a-servidor (`SERVER_COPY`, y el
  `copy_file_range` local) no tiene chunks: dentro de un fichero no se puede
  parar, solo entre ficheros.
- Reintentar existe a medias: el terminal guarda un `RetrySpec` por fila, pero
  solo para colisiones.

**En los frontends**:

- La barra de estado lleva el item `tasks`, que dice **`⟳ N`** y abre el panel
  de procesos al pulsarlo. Es todo lo que se ve si el panel no está abierto.
- El panel de procesos (terminal y ventana) sí enseña porcentaje, ritmo, ETA
  y operando por task. La aritmética es compartida (`norte-frontend/src/tasks.rs`:
  `progress_pct`, `Rate`, `human_rate`, `human_eta`).
- La ventana tiene además la insignia con la cuenta en la barra de actividad
  (ADR 0131).

**El hueco**: copias un fichero de 300 MB y lo único que cambia en pantalla es
un `⟳ 1` en una esquina. Para saber cuánto falta hay que abrir un panel. Para
una copia de medio segundo, el `⟳ 1` parpadea y no dice nada.

## 2. Cómo lo resuelven otros

| aplicación | progreso ligero | cola |
| --- | --- | --- |
| VS Code | indicador en la barra de estado y **una línea fina** encima de la vista que trabaja | — |
| Nautilus | un botón en la barra de cabecera con un **quesito** que se llena; al pulsar, la lista | — |
| Finder | progreso **sobre el icono del fichero destino** | — |
| Explorador de Windows | diálogo que se pliega a «menos detalles», con **pausa** | — |
| Dolphin | progreso en la barra de estado y en las notificaciones del sistema | — |
| Total Commander | diálogo, o **F2 = en segundo plano / a la cola** | cola en serie |
| Krusader (JobMan) | barra de progreso **en la barra de herramientas**, con menú de tareas | botón «cola»: iniciar, pausar |
| navegadores | una **línea de carga** de 2 px | — |

Lo que se repite y funciona:

1. **No aparece si la tarea es instantánea.** Nadie pinta un progreso de
   50 ms: se ve un parpadeo. Suele haber un umbral de unos cientos de
   milisegundos antes de enseñar nada.
2. **Una vez aparece, se queda un mínimo**, aunque la tarea acabe al instante
   siguiente: si no, también parpadea.
3. **Agrega.** Con tres copias en marcha hay UNA barra (el total por bytes) y
   un número, no tres barras.
4. **No ocupa sitio propio**: vive en algo que ya estaba (barra de estado,
   borde, icono, línea de 2 px).
5. **Se va sola al acabar bien**, dejando un instante un «✓». **Un fallo NO se
   va solo**: se convierte en algo que hay que despachar.
6. **Si no se sabe el total, no finge un 0 %**: pulsa o gira (el wire ya
   distingue `None` de `0`, y `progress_pct` también).

## 3. Propuesta: la barra ligera

### 3.1 Dónde

**En el item `tasks` de la barra de estado**, que ya existe, ya abre el panel
de procesos y ya se puede mover o quitar con `status_items` (ADR 0132). Crece
de `⟳ 1` a:

```
 una task:    ⟳ foto-grande.jpg ▕██████▌   ▏ 62 % · 48 MiB/s
 varias:      ⟳ 3 ▕████▎     ▏ 41 % · 1 m 20 s
 sin total:   ⟳ copiando 1 240 ficheros ▕ ░▒▓▒░ ▏
 al acabar:   ✓ copiado foto-grande.jpg              (1,5 s y se va)
 si falla:    ✗ 1 falló · abrir                       (se queda)
```

En el terminal la barra usa los octavos de bloque (`▏▎▍▌▋▊▉█`): diez celdas
dan ochenta pasos, suficiente para ver moverse una copia lenta.

Se **degrada con el ancho**, que es lo que la barra de estado ya hace con
`fit()` al repartir el sitio: primero pierde el nombre, luego el ritmo, luego
la barra, y por último queda `⟳ 3 41 %` — nunca desaparece entera mientras hay
trabajo, que es lo que pasa hoy con un item que no cabe.

**Opcional, en la ventana: la línea de 2 px** (hecha en la fase E, ADR 0148) en el borde inferior del panel
DESTINO, como la de carga de un navegador. Dice «aquí está llegando algo» sin
texto. En el terminal el equivalente sería colorear el borde inferior del
panel destino proporcionalmente; es bonito pero pelea con los temas y con el
borde que ya indica el foco, así que queda como fase posterior, no en la
primera.

### 3.2 Cuándo (la política, que es lo delicado)

Una sola máquina de estados en `norte-frontend` (ADR 0077: una decisión, un
sitio), con reloj inyectado para probarla sin dormir:

| constante | valor propuesto | por qué |
| --- | --- | --- |
| umbral de aparición | 400 ms | una copia de un fichero pequeño acaba antes y no pinta nada |
| «✓» tras acabar | 1,5 s | se ve que terminó, y se va; hace también de permanencia mínima |
| «✗» tras un fallo | 10 s | lo que se queda la fila en el panel de procesos, donde se lee por qué |
| panel automático | 2 s | lo que acaba antes lo cuenta la barra |

(Implementado en la fase A, ADR 0146. Respecto a este estudio cambió una
cosa: la «permanencia mínima» sobra, porque el «✓» ya la da.)

Una tarea de **menos de 400 ms** no pinta barra, pero sí deja el «✓ copiado
x» del final: si no, una copia rápida no da ninguna señal de haber ocurrido,
que es la otra mitad de la queja.

Qué tasks cuentan: las que ya cuentan como trabajo (`counts_as_work`), no las
búsquedas ni la construcción del índice, que tienen su propia pantalla.

### 3.3 Coste

- **Sin cambio de protocolo.** Todo sale de `TaskProgress`, que ya llega.
- **Puente +1** para la ventana: el item de estado necesita un campo de
  progreso (`percent: Option<u8>`, `phase: running|done|failed`) para que el
  renderer pinte la barra y no un texto con bloques.
- Terminal: el item `tasks` y su texto; snapshots nuevos.
- Tiempo: una rama mediana. Es la parte más visible y la más barata.

## 4. Propuesta: la cola de verdad

Tres cosas distintas, en orden de valor/coste:

### 4.1 Pausar y reanudar

(Implementado en la fase B, ADR 0147: protocolo 0.82.0, puente 93.)

- `task.pause` / `task.resume` (protocolo +1 menor, `protocol-guardian`
  obligatorio). El estado `Paused` ya existe en el wire.
- `TaskCtx` gana una **puerta** (`watch<bool>`) junto al token de
  cancelación; el bucle por chunk, además de mirar si se canceló, espera
  mientras la puerta esté cerrada. Cancelar una task pausada tiene que seguir
  funcionando (la espera escucha también al token), y eso es un test de
  cancelación limpia obligatorio por la regla 3.
- **Honestidad**: una copia `SERVER_COPY` o `copy_file_range` solo se para
  **entre ficheros**. La fila tiene que decir «se pausará al acabar este
  fichero» en vez de fingir que ya paró.
- Una task **en espera** (sin hueco aún) también se puede pausar: se salta
  al sacarla del heap hasta que se reanude.
- Teclas: en el panel de procesos, en los siete presets (`p` en casi todos;
  hay que mirar qué atestiguan Krusader y Total Commander).

### 4.2 Cola en serie

(Implementada en la fase C, ADR 0149: protocolo 0.83.0.)

Hoy cuatro copias al mismo disco corren a la vez, y en un disco mecánico eso
es MÁS lento que una detrás de otra. Total Commander y Krusader lo resuelven
con un botón «a la cola» en el diálogo de copia.

- El diálogo de copia/mover gana un interruptor **«a la cola»**. Lo encolado
  va a una cola con UN hueco, aparte de las cuatro paralelas.
- Parámetro nuevo en las peticiones de transferencia (`queued: bool`,
  `#[serde(default)]`: un cliente viejo no lo manda y todo sigue igual).
- Decisión abierta para su ADR: ¿una cola global, o una por dispositivo
  destino? La global es la de Total Commander y es predecible; por dispositivo
  es más lista y más difícil de explicar. Propuesta: global.

### 4.3 Reordenar

(Implementado en la fase C, ADR 0149.)

Solo tiene sentido en la cola en serie y solo para lo que aún no empezó.

- `task.move { task_id, before: Option<TaskId> }` (o subir/bajar).
- El `BinaryHeap` de esa cola pasa a ser una `VecDeque` con orden explícito.
- Teclas en el panel de procesos (subir/bajar), siete presets.

### 4.4 Reintentar

(Implementado en la fase D, ADR 0148.)

Generalizar el `RetrySpec` del terminal a cualquier transferencia fallida,
y llevarlo a la ventana. Es de frontend (los parámetros los tiene quien
lanzó), sin protocolo. Barato, y encaja con el «✗ 1 falló · abrir» de la
barra: abrir → reintentar.

## 5. Orden propuesto

| fase | qué | protocolo | puente | tamaño |
| --- | --- | --- | --- | --- |
| **A** | barra ligera en el item `tasks` (terminal y ventana), con su política de tiempos | no | +1 | medio |
| **B** | pausar/reanudar | +1 menor | +1 | medio-grande (toca el bucle de copia) |
| **C** | cola en serie + reordenar | +1 menor | +1 | grande |
| **D** | reintentar cualquier transferencia fallida | no | quizá | pequeño |
| E | línea de 2 px en el panel destino (ventana) / borde (terminal) | no | +1 | pequeño, opcional |

**A primero**: es lo que se nota cada día, no toca el core, y deja la
política de tiempos escrita en un sitio antes de que B y C añadan estados
(pausada, en cola) que la barra también tiene que saber pintar.

## 6. Decisiones que tomo salvo que digas otra cosa

- La barra vive en el item `tasks` que ya existe, no en un sitio nuevo.
- Umbral 400 ms, «✓» 1,5 s, «✗» 10 s, panel automático a los 2 s.
- Agregado por bytes cuando todas las tasks los conocen; si alguna no, por
  entradas; si ninguna, animación sin porcentaje.
- La cola en serie es global, no por dispositivo.
- Nada de esto es configurable en la primera versión salvo lo que ya lo es
  (`status_items` puede quitar el item entero).
