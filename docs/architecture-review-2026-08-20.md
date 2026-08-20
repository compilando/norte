# Revisión de arquitectura y código — 2026-08-20

> **Veredicto corto:** la base es sana. Las dependencias entre crates forman
> capas limpias sin ciclos, el `unsafe` está donde debe y con su `SAFETY`, no
> hay un solo `TODO` sin cerrar, la ratio test/producción del repositorio es
> **0,96** y el gate pasa entero (5.077 tests, 88 % de cobertura en lo
> gateado).
>
> Lo que hay que arreglar no es la estructura: es el TAMAÑO de tres o cuatro
> sitios concretos, una capa que se está convirtiendo en la segunda cocina, y
> una decisión aplazada que cuesta 32.000 líneas.
>
> Medido sobre `main 17e64275`, con `norte-gui` aparte cuando se dice.

## 1. Las cifras

| | |
| --- | --- |
| líneas de producción (sin GUI) | **145.330** |
| líneas de test (inline + `tests/`) | **143.219** — ratio **0,96** |
| crates | 25 (24 en el gate; `norte-gui` tiene el suyo) |
| tests | 5.077, **cero** `#[ignore]` |
| cobertura | 88 % (gate: 85 %, solo `proto` + `vfs` + `core`) |
| `TODO`/`FIXME` reales | **0** |
| funciones ≥ 180 líneas | 12, y 9 son tests |
| ficheros > 1.500 líneas | 32 |
| `unsafe` | 57 usos, **todos** en `norte-vfs-local`, todos con `SAFETY` |
| dependencias externas directas | 108; **208** crates duplicados por versión |

Por crate, lo que pesa:

| crate | prod | test | ratio |
| --- | --- | --- | --- |
| `norte-core` | 36.109 | 40.319 | 1,12 |
| `norte-tui` | 31.771 | 28.911 | 0,91 |
| `norte-frontend` | 22.125 | 22.689 | 1,03 |
| `norte-gui` | 17.924 | 14.151 | 0,79 |
| `norte-proto` | 10.439 | 9.979 | 0,96 |
| `norte-vfs-local` | 5.554 | 2.886 | **0,53** |

## 2. Lo que está bien, y conviene no romper

- **Las capas se respetan.** `proto` no depende de nadie; `vfs` solo de
  `proto`; cada provider solo de `vfs`; `core` de todo lo de abajo; los
  frontends de `core` + `norte-frontend`. Ningún ciclo, y **ningún provider
  conoce a otro**, que es la regla que más fácil se rompe sola.
- **`norte-frontend` no sabe pintar.** Cero dependencias de `ratatui`,
  `crossterm`, `gpui` o `tauri`. Es lógica de presentación PURA, y por eso el
  71 % de su volumen son tests inline que corren en milisegundos.
- **El `unsafe` está acorralado.** 57 usos, todos en `norte-vfs-local`, con
  `#![deny(unsafe_code)]` en el crate y un `#[allow]` por ítem acompañado de
  su `// SAFETY:`. Es exactamente lo que la regla 5 pide.
- **`norte-testkit` es `dev-dependency` en todos sus consumidores**: el corpus
  hostil no viaja en el binario.
- **La disciplina de comentarios es real.** Los `#[allow(clippy::…)]` (21 de
  `too_many_arguments`, 13 de `too_many_lines`) llevan todos su motivo escrito
  al lado. Cero `TODO` colgando.
- **Cuatro rondas de refactor de la TUI ya han pagado**: `main.rs`
  11.612 → 592, `ui.rs` repartido en diez, `app.rs` 7.377 → 991, y `run`
  2.518 → 660.

## 3. Hallazgos

Ordenados por lo que cuesta convivir con ellos, no por tamaño.

### H1 — `norte-gui`: 32.000 líneas esperando una decisión

`crates/norte-gui` está en `members` pero fuera de `default-members`, con gate
propio (`just gui-ci`), y el plan
`docs/superpowers/plans/2026-08-19-multi-frontend-tauri-transition.md` lo marca
para BORRADO en su fase 8.3. Mientras tanto duplica presentación que ya vive
en `norte-frontend` (`sync_view.rs` son 3.465 líneas frente a las 4.566 de
`norte-frontend/src/sync.rs`) y su ratio de test es la más baja del
repositorio (0,79).

Es el único punto donde el repositorio mantiene dos verdades a la vez. Y no es
gratis: cada cambio en la superficie de sincronización o de comparación hay que
pensarlo dos veces o aceptar que divergen.

**Qué hacer:** decidir. Si la transición a Tauri sigue en pie, borrarlo ya y
quedarse con los mockups; si no, meterlo en el gate principal. Aplazarlo es la
opción cara.

### H2 — El host de Lua vive dentro de la TUI

`crates/norte-tui/src/lua/` son ~2.700 líneas en cinco ficheros que **no
mencionan `App` ni `ratatui`**: es un subsistema (host por capas, trust TOFU,
API de scripting, driver de comandos), no presentación de terminal. Hoy
funciona porque solo la TUI script-ea; el día que cualquier otro frontend
quiera scripts, se copia o se mueve con prisa.

**Qué hacer:** crate propio (`norte-script`), con el mismo trato que
`norte-plugin-host`. Es un movimiento mecánico: el compilador lo verifica.

### H3 — Modelos compartibles que siguen en la TUI

`tree.rs` (297), `tasks.rs` (285), `palette.rs` (160), `processes.rs` (80): sin
`ratatui`, sin `App`, y con equivalentes escritos a mano en la GUI. La regla 7
dice que la lógica que no es de una superficie concreta va al crate compartido.

**Qué hacer:** subirlos a `norte-frontend` cuando se toque cada uno. Barato y
sin riesgo; hacerlo de golpe no aporta nada.

### H4 — `norte-frontend` va camino de ser la segunda cocina

22.125 líneas de producción en 57 ficheros, con `sync.rs` (2.811 de prod),
`pane.rs` (1.702) y `columns.rs` a la cabeza. Las dependencias son impecables,
pero el tamaño por fichero repite el patrón que ya obligó a repartir `app.rs` y
`ui.rs` en la TUI.

**Qué hacer:** el mismo tratamiento, y con la misma receta que ya funcionó dos
veces: rebanadas temáticas verificadas por compilación, los tests viajando con
su código. Empezar por `sync.rs` y `pane.rs`.

### H5 — Los tres ficheros gordos del núcleo, y cuál importa

| fichero | prod | veredicto |
| --- | --- | --- |
| `norte-proto/src/methods.rs` | 6.705 | **Se queda.** Son 152 structs de datos y ~250 líneas de lógica; grande por volumen y congelado por los goldens del wire. |
| `norte-core/src/daemon/server.rs` | 5.177 | **Se queda plano** (decisión ya escrita). 58 funciones, la mayor de 154 líneas: no hay monstruo dentro. |
| `norte-core/src/backend.rs` | 4.812 | **Este sí vigilar.** 75 métodos públicos: es la fachada por la que pasa TODO frontend, y crece con cada método del protocolo. |

**Qué hacer:** nada urgente. Si `backend.rs` sigue creciendo, partirlo por
familias (`fs`, `sync`, `plugins`, `journal`) como `impl` en módulos hijos —
la misma técnica que se acaba de usar en `App`.

### H6 — El crate con todo el `unsafe` es el menos probado

`norte-vfs-local`: 5.554 líneas de producción, ratio de test **0,53** (la media
del repo es 0,96), los 57 `unsafe` del proyecto… y **fuera del gate de
cobertura**, que solo cubre `proto`, `vfs` y `core`.

No es que esté mal probado en absoluto —tiene 2.886 líneas de test y pasa el
contrato de providers—, es que es justo el sitio donde una regresión no avisa:
syscalls, `openat2`, papelera freedesktop, montajes por plataforma.

**HECHO el mismo día.** `norte-vfs-local` entra en `just cov`, y el número
resultó barato: el total del gate se queda en **88,10 %** (suelo 85 %). Por
fichero, lo que enseña:

| fichero | líneas cubiertas |
| --- | --- |
| `location.rs` | 92,0 % |
| `native_path.rs` | 91,2 % |
| `caps_at.rs` | 90,1 % |
| `trash_fdo.rs` | 89,8 % |
| `provider.rs` | 83,1 % |
| **`confined.rs`** | **79,8 %** |

O sea: el punto flojo es justo el confinamiento (`openat2` /
`RESOLVE_BENEATH`), que es lo que sujeta las escrituras de un agente. Ahí es
donde poner los tests siguientes, y ahora hay un número que lo dice.

### H7 — 32 ficheros por encima de 1.500 líneas

El propio repositorio escribió la norma en `norte-tui/src/jobs/mod.rs`:
«ningún fichero de producción por encima de las mil líneas». Hoy hay 32 por
encima de 1.500 (sin contar la GUI). No es deuda urgente —muchos son
mayoritariamente tests inline— pero la norma o se aplica o se cambia.

**Qué hacer:** aplicarla al tocar cada fichero, no en una ola. Y considerar
medirla: un test que falle si un fichero de producción pasa de N líneas es
barato y no se olvida.

### H8 — 82 esperas de reloj en la suite

82 `sleep(Duration…)` entre `src` y `tests`. CLAUDE.md ya avisa de que bajo
carga cualquiera puede perder su carrera, y que un test intermitente es un bug.
Con 90 marcas de serialización conviviendo, es la fuente más probable de rojo
no reproducible.

**Qué hacer:** cazarlos de uno en uno cuando fallen, sustituyendo espera por
señal (canal, `Notify`, o el propio evento que se espera). No merece una ola.

### H9 — 208 crates duplicados por versión

`cargo tree --duplicates` da 208 líneas: `base64` en tres versiones, `aes`,
`aead`, `atoi`, `rand`… Es lo normal en un árbol con 108 dependencias
directas, y no rompe nada, pero engorda el binario y multiplica la superficie
que hay que auditar cuando salga un aviso de seguridad.

**Qué hacer:** revisarlo en la próxima `release-check`, no antes.

### H10 — El `target/` estaba en 139 GB, y `just prune` estaba ROTO

El presupuesto de disco de CLAUDE.md fija el estado estable en ~30 GB.

Al ejecutarlo se vio por qué llevaba días sin bajar: la receta barre
`target/debug/deps` y `target/release/deps`, y en una máquina que solo compila
en debug el segundo no existe → `find` sale con error → con `set -euo
pipefail` la receta MORÍA justo antes de barrer nada, después de haber hecho
solo la parte barata. Arreglado (se barren los directorios que existan).

Con eso: **139 GB → 64 GB**. Lo que queda son artefactos de hoy; decaen solos.

## 4. Qué haría, y en qué orden

| ola | qué | por qué ahí |
| --- | --- | --- |
| ~~**0**~~ | ~~`just prune` (H10) y `-p norte-vfs-local` en `cov` (H6)~~ | **HECHO**: 139 → 64 GB, `prune` arreglado, gate en 88,10 % con el crate del `unsafe` dentro |
| **1** | decidir `norte-gui` (H1) | bloquea todo lo demás de arquitectura: si se borra, H3 y H4 cambian de forma |
| **2** | `norte-script` fuera de la TUI (H2) | mecánico, verificado por el compilador, y quita 2.700 líneas del crate más grande de frontend |
| **3** | repartir `norte-frontend/src/sync.rs` y `pane.rs` (H4) | la receta ya está probada dos veces en este mismo repositorio |
| **continuo** | H3, H7, H8 al tocar cada sitio | ninguno justifica una ola propia |

Lo que **no** haría: tocar `methods.rs`, aplanar más `server.rs`, ni empezar
por `backend.rs`. Los tres son grandes por razones que ya están escritas y
revisadas, y ninguno duele hoy.

---

## Addendum — el estado al cerrar la fase 2 (mismo día, más tarde)

Esta revisión se escribió por la mañana. Lo que vino después la contesta en
parte, así que aquí queda la foto, para que el documento no envejezca
mintiendo.

### Lo que cambió

| | antes | ahora |
| --- | --- | --- |
| crates | 25 | **26** (`norte-client`, `norte-ui-host`; fuera `norte-gui`) |
| producción (sin GUI) | 145.330 | **148.073** |
| test / producción | 0,96 | **0,99** |
| tests del gate | 5.077 | **5.141** |
| cobertura (proto, vfs, core, vfs-local, client) | 88,10 % | 88 % largo, con el SDK dentro |
| `norte-core/src/backend.rs` | 5.271 | **2.416** |
| funciones ≥ 300 líneas | 6 | **3**, y las tres son tests de corpus |
| `target/` | 139 GB | **74 GB** |

### Los hallazgos, uno a uno

- **H1 (`norte-gui`): RESUELTO.** Retirado (ADR 0065). Con él se fueron el
  `--exclude` del gate, `gui-ci`, `check-gui`, su `deny.toml` con nueve
  licencias que la política del workspace prohíbe, y el reparto
  `members`/`default-members`. **El gate cubre ahora el workspace entero.**
- **H5 (`backend.rs`, la fachada a vigilar): RESUELTO por otra vía.** No hizo
  falta partirla por familias: la mitad que era cliente remoto se fue al SDK
  y quedó en 2.416 líneas.
- **H6 (el crate del `unsafe`, sin cobertura): RESUELTO.**
  `norte-vfs-local` está en `just cov`, y enseña lo que la media tapaba:
  `confined.rs` al 79,8 % es el punto flojo, y es justo el `openat2` que
  sujeta las escrituras de un agente.
- **H10 (disco): RESUELTO, y era un bug.** `just prune` llevaba roto desde
  siempre en una máquina que solo compila en debug: `find` sobre un
  `release/deps` inexistente mataba la receta antes de barrer nada.
- **H2 (el host de Lua dentro de la TUI): SIGUE.** Sin cambios.
- **H3 (modelos compartibles en la TUI): a medias.** `History` y `Trail`
  subieron a `norte-frontend` porque el host los necesitaba —que es
  exactamente el mecanismo que esta revisión predijo: se suben cuando el
  segundo frontend los pide—. Quedan `tree.rs`, `tasks.rs`, `palette.rs`.
- **H4 (`norte-frontend`, la segunda cocina): SIGUE, y ahora con más razón.**
  Es el crate del que cuelgan TUI y host: `sync.rs` (4.566) y `pane.rs`
  (4.372) siguen siendo los dos ficheros más grandes del repositorio después
  de `methods.rs` y `daemon/server.rs`.
- **H7 (ficheros > 1.500 líneas), H8 (82 esperas de reloj), H9 (208 crates
  duplicados): SIGUEN.** Ninguno se tocó.

### Lo que la fase 2 dejó a deber

Está escrito task por task en el plan multi-frontend, y es deuda ELEGIDA, no
olvidada: paginación del listado en el host, quick search, columnas y
catálogo de atributos, watcher; el panel which-key y las vistas de
menú/palette/atajos; el `session.put` periódico y la propiedad adquirida más
tarde; tasks ajenas, avisos de conexión, aprobaciones de policy y diálogos
con campo de texto.
