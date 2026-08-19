# Extraer `norte-tui/src/main.rs` — lo hecho, y el grafo real de dependencias

> **Estado:** fase 1 completa y mergeable (13 commits en
> `refactor/tui-main-extraction`). El resto está pendiente, con el orden ya
> medido más abajo.
>
> **Fecha:** 2026-08-19.

## Por qué existe esto

`crates/norte-tui/src/main.rs` tenía **11.612 líneas de producción** y 5.543 de
test inline repartidas en 24 bloques `#[cfg(test)]`. La causa no es descuido: es
el root de un **binario**, un crate distinto de la lib del mismo paquete, así
que nada de lo que hay ahí se puede importar — ni desde los 36 ficheros de
`crates/norte-tui/tests/`, ni desde un futuro `norte-ui-host`. Los 24 bloques de
test son ficheros de `tests/` que no podían serlo.

Medición del repo entero, para situar: 172.697 líneas de producción, 77 de 4.294
funciones pasan de 100 líneas, y solo **dos** pasan de 400 — las dos en este
fichero (`run`, 2.518; `dispatch`, 637). La distribución global es buena; el
daño estaba concentrado.

## Lo hecho

`main.rs`: **11.612 → 6.583** líneas de producción (−43%); test inline
5.543 → 3.100. Doce módulos nuevos, ninguno por encima de 570 líneas de
producción:

| módulo | prod | qué |
| --- | --- | --- |
| `probes.rs` | 270 | las cuatro sondas de fondo (stat lazy, stat de comparación, decoraciones, preview) y el dedup `Probed` |
| `fill.rs` | 178 | el canal por el que llega un listado paginado, uno por hueco |
| `navigate.rs` | 588 | el ritual de `cd`: `Cd`, `apply_cd`, `first_page`, `listing`, capacidades, `trust_host_retry` |
| `trail.rs` | 213 | el rastro de navegación y el rebobinado |
| `jobs/{search,compare,sync}.rs` | 308/427/480 | las tres tareas largas de panel, con sus tablas de teclas |
| `mutations.rs` | 530 | contestar un modal y mandar la mutación que autoriza |
| `overlays.rs` | 446 | quién se come la tecla con overlays apilados, y la ayuda contextual |
| `shortcuts_editor.rs` | 535 | el editor de atajos y `build_keymaps` |
| `session_push.rs` | 562 | persistir y restaurar la sesión de UI |
| `lua/host.rs` | 412 | cargar el host Lua, su confianza y sus comandos |
| `paste.rs` | 231 | enrutar un pegado al campo con foco |
| `viewer_open.rs` | 136 | abrir el visor y mover el que ya está abierto |
| `listing.rs` | 39 | `initial_pane` (crece en la fase siguiente) |

Invariante respetado en todos los movimientos: **mismos nombres, mismas firmas,
mismo orden, mismos comentarios**. Lo único que cambia es en qué fichero está
cada línea, más `pub` y `use`.

## Dos bugs preexistentes, encontrados sin buscarlos

Los dos son de la misma familia y ninguno se veía en CI.

1. **7 tests de `norte-tui`** afirmaban strings del corpus INGLÉS sin fijar el
   idioma, así que `Lang::from_env` lo resolvía por `LANG`. Verdes en CI, rojos
   en cualquier máquina con `LANG=es_*` — y como `just t` no lleva
   `--no-fail-fast`, el árbol limpio abortaba a los **189 de 892 tests**, que se
   lee como «2 fallos» cuando en realidad son 703 sin correr.
   Commit `73317d98`.
2. **1 test de `norte-frontend`** (`whichkey::an_unavailable_row_...`) construía
   el panel con `Lang::En` explícito y comparaba contra `norte_i18n::t(..)`, que
   traduce con el idioma GLOBAL. Inglés contra español. Arreglado con `t_in`,
   que lo hace independiente del entorno por construcción — mejor que fijar el
   global. Commit `6cefe811`. **Solo `just ci-fast` podía verlo**: otro paquete,
   así que `just t norte-tui` nunca lo tocaba.

**Consecuencia práctica:** en una máquina con locale no inglés, la línea base de
este repo es roja hasta esos dos commits. Cualquiera que mida «cuántos tests
pasan» antes de tocar nada tiene que fijar `NORTE_LANG=en` o aplicarlos.

## El grafo de dependencias real (esto es lo que el plan tenía mal)

El plan original ordenaba las tareas «de menor a mayor acoplamiento con `run`» y
ponía las bandas grandes primero. Es al revés. Medido a base de intentarlo:

```
                        event_loop (run)
                              |
        +---------------------+---------------------+
        |                     |                     |
     dispatch            mutations               screens
        |                     |                     |
        |         +-----------+-----------+         |
        |         |           |           |         |
        |      trail      overlays     jobs/*  <----+
        |         |           |        /  |
        +---------+-----------+-------+   |
                  |                       |
              navigate  <-----------------+   (ciclo: navigate <-> jobs)
                  |
        +---------+---------+
        |         |         |
      fill     probes    listing
```

Las **hojas** son `probes`, `fill` y `listing`. Intentar `jobs` primero (la
banda más gorda, 1.069 líneas) falla a compilar contra `Fill`, `DecorateFetch`,
`Probed`, `cd` y `apply_cd`: las tareas largas de panel están ENCIMA de las
sondas y del camino de `cd`, no al lado.

**Y hay un ciclo genuino** entre `navigate` y `jobs`: un `cd` fuera del pane
virtual de búsqueda tiene que soltar la búsqueda viva, y lanzar una búsqueda
necesita el `cd`. Un ciclo entre módulos del MISMO crate es legal en Rust, así
que el orden de salida da igual — lo que no vale es dejar una mitad en el
binario, que sí es otro crate. Se resolvió abriendo `jobs.rs` con `SearchRun`
solo y llenándolo después.

## Tres impuestos que se pagan en cada movimiento, y ninguno es el movimiento

Esto es el 60% del trabajo real por tarea. El plan no lo previó.

### 1. El impuesto de visibilidad

Un item privado en un binario y el mismo item `pub` en una lib no están sujetos
a los mismos lints. Al mover, aparecen:

- `clippy::missing_errors_doc` — toda función `pub` que devuelva `Result`
  necesita `# Errors`. Pagado 4 veces.
- `clippy::must_use_candidate` — pagado 12 veces.
- `missing_docs` (denegado en el workspace) — structs y funciones que nunca
  tuvieron rustdoc porque privadas en un binario no lo necesitan. Pagado 6 veces.
- **Campos privados cuyo único lector está fuera.** Se manifiesta como
  `never read` o `field is private`, y hay que abrirlos con doc por campo.
  Pagado en los 7 structs de sonda y de tarea.

### 2. Los doc links, que solo `cargo doc` ve

Ni `just t` (nextest, no corre doctests) ni `just c` (clippy, no comprueba
intra-doc links) los detectan. Pagado ~20 veces, en tres formas:

- un item `pub` que enlaza a uno que se quedó privado;
- un link relativo cuyo objetivo se fue a otro módulo (→ ruta completa);
- un link que dejó de resolver porque su objetivo pasó a importarse bajo
  `#[cfg(test)]`.

**Y son señal útil, no ruido:** cuando dos items se enlazan mutuamente están
diciendo que van en el mismo fichero. Así se descubrió que las dos LECTURAS de
precedencia de overlay (`modal_wins`, `help_owns_keys`) y las dos ESCRITURAS que
mantienen su respuesta honesta (`settle_help_over_modal`, `close_stale_overlays`)
son un único módulo.

### 3. Imports que mueren, y son la métrica de que sale código real

Tras sacar Lua, el binario no nombra ni un tipo de Lua. Tras sacar las sondas,
no nombra el visor en producción. Tras sacar el editor de atajos, cuatro tipos
de keymap pasan a `#[cfg(test)]`.

**Cuidado con `cargo fix`:** quita imports que los `mod` de test del binario
usan vía `use super::`, porque el build de producción no los ve. Pasó dos veces.
Lo correcto es `#[cfg(test)] use …` explícito, con un comentario de por qué.

## Los módulos de test se quedan atrás a propósito

Un `mod …_tests` que prueba un ritual completo nombra items de varias bandas.
`help_key_tests` son 945 líneas y toca la ayuda, los modales y el keymap;
`pane_gestures_tests`, 678, y toca el rastro, el `cd` y la propiedad del
teclado. Moverlos con su banda obliga a mover las otras en el mismo commit, que
es exactamente el commit irrevisable que este plan evita.

Por eso el test inline bajó menos que la producción (−2.443 frente a −5.029):
12 módulos siguen en `main.rs` esperando a que salgan sus dependencias. Es deuda
consciente, no olvido.

## Verificación: el compilador no basta

Por tarea: `just t norte-tui` verde y **el recuento de tests no puede bajar**
(892, fijo desde el primer commit). Un test que desaparece al mover su módulo es
el único fallo que la compilación no ve.

Y una comprobación más fuerte que «compila», que conviene repetir: comparar el
**multiconjunto de líneas no vacías** antes y después.

```sh
git show HEAD~1:crates/norte-tui/src/main.rs > /tmp/old.rs
cat crates/norte-tui/src/main.rs crates/norte-tui/src/<nuevo>.rs > /tmp/new.rs
# y contar qué líneas de /tmp/old.rs no están en /tmp/new.rs, y al revés
```

En el commit de `jobs` esto demostró que de **1999 «inserciones» que reportó
git en `main.rs`, solo 13 líneas eran realmente nuevas** (todas `use`), y las 72
«perdidas» eran imports reflowados por rustfmt más campos que ganaron `pub`.
Cero líneas de lógica en cualquier dirección. Sin esa comprobación, un commit
que solo borra código parece un reescritura.

Al cerrar: `cargo test -p norte-tui --doc` y `cargo doc -p norte-tui --no-deps`
por tarea (segundos, y son los puntos ciegos), `just ci-fast` cada ~3 tareas,
`just ci` una vez.

## El bucle de verificación, medido (tarea 1, 2026-08-19)

La tarea 1 se hizo con `just t` FUERA del bucle, y no se perdió nada. Los
tiempos son de esta máquina, en caliente, tocando solo `norte-tui`:

| comando | coste | qué cazó en la tarea 1 |
| --- | --- | --- |
| `grep -c '#\[test\]'` sobre `src/` + `tests/` | 0 s | el invariante de 888 atributos, en cada paso |
| multiconjunto de líneas no vacías (`scripts` ad hoc) | 0 s | que de ~1.900 «inserciones» ni una fuese lógica |
| `just c` | **4–61 s** | **11 fallos**: imports muertos, `must_use_candidate`, `items_after_test_module`, un `use super::Pane` sin dueño |
| `cargo doc -p norte-tui --no-deps` | 10 s | **3 fallos**, los tres invisibles a clippy y a nextest |
| `cargo test -p norte-tui --doc` | 11 s | 0 |
| `just t norte-tui` | 31 s | **0** |

`just c` es el bucle. No linka nada (clippy no produce binarios), en vacío
cuesta 1,6 s, y es el único de los tres que ve el impuesto de visibilidad —que
es el 60% del trabajo de cada movimiento—. Los 61 s del peor caso son cuando
cambia la superficie que compilan los 30 targets de test; el caso normal son 4
a 12 s.

**Y `just t` no cazó nada, estructuralmente, no por suerte.** En un movimiento
puro el comportamiento no puede cambiar: lo único que nextest puede descubrir
es un módulo de test que desapareció al mover su fichero, y eso lo dice
`grep -c '#[test]'` gratis y al instante. El coste real de `just t` no son sus
31 s sino que hay que esperarlos con el árbol quieto, veinte veces por tarea.

Política, entonces:

| cuándo | qué |
| --- | --- |
| cada paso (docenas) | `just c` + recuento por grep + multiconjunto de líneas |
| cada tarea (una vez) | `cargo doc -p norte-tui --no-deps` y `cargo test -p norte-tui --doc` |
| cada ~3 tareas | `just ci-fast` UNA vez — y aquí es donde entra `just t` de verdad |
| al cerrar la rama | `just ci` UNA vez |

Lo que se pierde por sacar nextest del bucle es real y es pequeño: un test que
compila pero afirma sobre otro item del mismo nombre. No ha pasado en 16
commits de esta rama.

### Tres herramientas que valen más que el gate

**Un extractor de items de primer nivel** (unas 60 líneas de Python que caminan
a profundidad 0 de llaves y emiten `inicio-fin  tipo  firma`). Con él, mover
1.685 líneas no exigió LEER ni el cuerpo de `on_help_key` (199 líneas) ni las
951 de `help_key_tests`: los límites salen calculados. Es el mayor ahorro de la
tarea, y no es de tiempo de máquina sino de tokens del controlador.

**Extraer por rango de líneas, no reescribiendo.** El movimiento lo hace un
`del L[a-1:b]` y un `'\n'.join(bloques)`. Así el multiconjunto de líneas cuadra
POR CONSTRUCCIÓN, y el `pub`/`crate::` se aplica después con un `sed` de siete
patrones que el propio diff enumera.

**El multiconjunto de líneas, en un script y no a mano.** Cuatro commits, cuatro
ejecuciones, cero tokens de razonamiento por ejecución.

## Cuatro rustdoc desplazados, y es una familia

Sin buscarlos, en las 1.685 líneas de la tarea 1 aparecieron **cuatro** bloques
de rustdoc separados de su función, todos secuela de movimientos mecánicos
anteriores:

1. el de `reload_config` colgaba de `apply_theme`;
2. el de `on_layout_picker_key`, de `on_connections_picker_key`;
3. los de `on_places_key` y `on_nav_popup_key`, APILADOS sobre `on_tree_key`
   (tres doc-comments seguidos delante de una sola función);
4. el de `shortcuts_editor_tests` se quedó en `main.rs` cuando su módulo se fue
   a `shortcuts_editor.rs`, y acabó documentando un módulo de test de i18n con
   el que no tiene nada que ver.

El quinto era peor: el rustdoc de `on_help_key` estaba **partido en dos**. La
introducción, hasta el «TWO REGIMES, the same split ... already have:» que
anuncia una lista, había quedado sobre `HelpDispatch`; la lista que ese dos
puntos promete seguía sobre la función.

**La regla que sale de esto, y es accionable para las tareas que faltan:** un
`missing_docs` sobre un item que acabas de mover NO es una invitación a
escribir documentación nueva. Es la señal de que su documentación está varada
unas líneas más arriba, encima de otra función. Mirar hacia arriba antes de
escribir. Dos de los cuatro se encontraron exactamente así.

## Lo que queda, en orden de dependencias

| # | módulo | prod aprox. | notas |
| --- | --- | --- | --- |
| ~~1~~ | ~~`screens/{help,settings,pickers,extensions,side_nav}.rs`~~ | 1.685 | **HECHA** (4 commits, 892/892). El estimado de ~1.700 dio en el clavo. Cuatro ficheros salieron en cuatro commits y `pickers`+`settings` en uno solo: parecían un ciclo y no lo eran —las dos referencias mutuas son menciones en comentarios, no llamadas—. `main.rs`: 6.583 → 5.001 de producción, 3.099 → 2.108 de test inline. |
| 2 | `refresh.rs` | ~250 | `on_tick`, `refresh_panes`, `after_panes_refresh`. Lo necesitan cuatro de los `mod` de test diferidos. |
| 3 | `gestures.rs` + `suspend.rs` | ~800 | Gestos de panel, línea de comandos, shell, openers y suspensión de terminal. Se lleva `suspend_tests`, `open_tests`, `pane_gestures_tests`, `edit_tests`. |
| 4 | `config_reload.rs` | ~140 | `reload_config`, 12 parámetros. Sale del plan original (estaba mal agrupado con Lua). |
| 5 | **estrechar `run` a error tipado** | — | **Commit propio, y es cambio de firma.** La regla 6 prohíbe `anyhow` en libs y `run` devuelve `anyhow::Result<()>`. El muro son CUATRO líneas: `terminal.size()` ×2, `terminal.draw()` y un `.context("evento de terminal")`. Se paga con un `thiserror` de dos variantes (`Io(std::io::Error)` + evento de terminal) y cuatro `?`. |
| 6 | `dispatch.rs` | ~690 | La tabla comando→efecto ENTERA, sin partir: es un `match` plano de 109 brazos, y este repo ya argumentó por escrito contra trocear tablas planas (`norte-core/src/daemon/server.rs:2360-2364`). |
| 7 | `event_loop.rs` | ~2.500 | `run`. Último. **Este plan no lo parte.** |

Tras 1–7, `main.rs` queda en unas **470 líneas**: cabecera, `main`,
`make_backend`, `build_keymaps` ya no, `open_terminal_or_exit`,
`restore_terminal`, `arm_mouse`, `write_cd_file`, `finish_pick`, `apply_theme`, y
la banda de flags que la regla 6 clava ahí (`start_dir` y `args_or_exit`
devuelven `anyhow::Result`).

De las 178 funciones de producción originales, **solo 5** llevaban
`anyhow::Result`: `start_dir`, `args_or_exit`, `make_backend` (las tres se
quedan), `initial_pane` (ya estrechada a `Result<Pane, Error>`, y era `anyhow`
solo para stringificar un `norte_proto::Error`) y `run`. El muro de la regla 6
era mucho más pequeño de lo que parecía.

## Lo que este plan NO hace, y por qué

- **No parte `run` (2.518 líneas) ni `dispatch` (637).** Partir `run` es
  rediseño: hay que convertir la cadena `else if` de 19 ramas en una tabla de
  precedencia de overlays y decidir la frontera entre «drenar una tarea en
  curso» y «enrutar una tecla», y eso cambia el orden de los `await` dentro de
  un `tokio::select!` de 23 brazos — el tipo de cambio que produce un bug de
  concurrencia que los tests no ven. Ronda siguiente, plan propio, y sobre un
  fichero de 2.500 líneas en vez de 11.612.
- **No toca `app.rs` (7.076 prod) ni `ui.rs` (5.970).** `App` tiene 63 campos y
  `impl App` son 3.577 líneas con 187 métodos, TODOS pequeños (el mayor, 71): el
  problema es anchura, no profundidad, y dentro hay **9 familias de 7 métodos
  copiadas** (`*_push`/`*_pop`/`cancel_*`/`*_confirm`/… para `pack`, `split`,
  `mkdir`, `transfer_dest`, `command_line`, `ai_rename`, `semantic`,
  `mark_pattern`, `transfer_name`) que piden genéricos o macro, no un
  movimiento. `ui.rs` parece el corte más fácil (un `draw_*` por pantalla, cero
  `impl`) y es el más difícil de los tres: sus 18 módulos de test están
  INTERLEAVADOS con la producción, así que necesita un paso previo de
  reordenación.
- **No renombra nada.** `main.rs` mezcla dos idiomas en los identificadores
  (`confirma_el_modal`, `desempaqueta`, `escribe_la_sesion`, `drena_avisos`
  junto a `submit_transfers`, `drain_search`), con híbridos
  (`captura_session`, `SessionOrden`). Los comentarios en español son una
  decisión consistente del proyecto y no se tocan; los nombres de items merecen
  su propia pasada mecánica, en un commit, para no contaminar la verificación
  por compilación.
- **No toca `norte-gui`.** El plan
  `2026-08-19-multi-frontend-tauri-transition.md` lo marca en su Fase 8.3 para
  borrado; sus 11.979 líneas de producción no valen un refactor. Sí sirvió de
  espejo: es lo que permite ver qué lógica de presentación se escribió dos veces
  (`key_meaning` de sync y de compare, el pipeline de `help_render`, `Facts`
  construida con entradas distintas en cada frontend). **Ninguna de esas copias
  tenía un bug detrás** — se verificaron una por una, incluida la que más lo
  parecía: el `submitted` del panel de sync NO es alcanzable en la TUI, porque
  `launch_sync_apply` se espera en línea en la cabecera del bucle, antes del
  `select!`. Subirlas a `norte-frontend` es la decisión D14 del plan Tauri.
- **No toca `norte-proto/src/methods.rs`** (6.705 líneas, cero tests): 132
  structs y ~250 líneas de lógica real; grande por volumen y congelada en el
  wire. Ni `norte-core/src/daemon/server.rs`, cuyo dispatch plano el repo ya
  decidió por escrito que se queda plano.
