# Paridad TUI ↔ ventana: inventario y plan

- Fecha: 2026-09-05
- ADR: 0097 (las reglas), 0077 (la regla original), 0096 (lo que destapó esto)
- Estado: inventario cerrado, fases sin empezar

## Cómo leer esto

El inventario salió de una auditoría de cuatro ejes hecha a propósito, después
de que el trabajo de la fila `..` (ADR 0096) se tropezara con cuatro
divergencias por accidente en una tarde.

Cada ítem lleva una marca de **confianza**:

- **V** — verificado a mano en esta sesión, leyendo los dos lados o viéndolo
  en pantalla.
- **A** — lo trajo la auditoría con `file:line` en los dos lados y no lo he
  vuelto a comprobar. Fiable, pero no es lo mismo.

Nada de esto está arreglado. Lo que sí se arregló ya, en la rama
`fix/paridad-fila-de-subir-y-layout`, es el hueco por el que se entró: la hoja
de atributos compartida, `or_preset`, la fila `..` en la sesión, la sonda de
la hoja, y el embudo del operando que se abría con el quick search.

---

## A. Claves de configuración que solo honra un frontend

La clase más barata de arreglar y la más visible: el usuario escribe algo en
`norte.toml` y en una de las dos superficies no pasa nada.

| clave | terminal | ventana | conf |
| --- | --- | --- | --- |
| ~~`openers.toml` (entero)~~ | sí, con recarga en caliente | ~~**no lo lee nadie**: siempre `xdg-open`~~ **HECHO** | **V** |
| ~~`[ui] editor` / `editor_detached`~~ | sí (F4 lanza tu editor) | ~~**no**: `pane.edit` es `pane.open`~~ **HECHO** (`$EDITOR` sigue fuera, y es deliberado) | A |
| ~~`[ui] quick_search`~~ | sí | ~~**no**: `Filter` a fuego~~ **HECHO** | A |
| ~~`[ui] confirm_quit`~~ | sí | ~~**no**: la X cierra sin preguntar~~ **HECHO** | A |
| ~~`[ui] theme` como RUTA~~ | sí (ADR 0020) | **HECHO en el arranque**; al cambiar de PERFIL sigue siendo solo presets (pide I/O fuera del actor) | A |
| ~~`[ui] lang` vs `NORTE_LANG`~~ | gana el ENTORNO | ~~gana la CONFIG~~ **HECHO**: manda la regla del terminal | **V** |
| ~~`[ui.columns]` estilo por columna~~ | sí (`style_for_id`) | ~~**no**: `default_for_id` en celdas Y cabeceras~~ **HECHO** | **V** |
| ~~`[DIR]` de la línea de órdenes~~ | gana a la sesión (`pin_start_dir`) | ~~**la sesión lo pisa**~~ **HECHO** (gana en el panel activo) | **V** |
| recarga en caliente (todas) | sí, `norte_config::watch` | **no hay watcher**: todo es de arranque | A |
| `[ui] font`, `mono_font`, `font_size`, `reduce_motion` | — | — | **muertas en los dos** | A |
| `[profile.start]` | la escribe | la escribe | **no la lee nadie**, y dos ficheros prometen que sí | **V** |
| `profile_warnings` | — | — | no se enseñan; `load.rs` argumenta largo que callarlas sería el fallo grave | A |

Dos comentarios de `schema.rs` (`menu_bar`, `panel_bar`) dicen que la ventana
ignora esas claves. Las lee. Un comentario que miente es lo que la próxima
auditoría se creerá.

## B. Estado que se queda congelado en la ventana

Un parche de filas escribe `generation`, `first_visible`, `rows` y `cursor`.
**Nada más.** Todo lo demás de `BrowserSlotView` solo viaja en la foto entera.
Y la disposición por defecto es `orthodox`: sin visor y sin hoja, o sea sin
NINGUNA sonda al final del bucle.

| qué | se congela cuando | qué se ve | conf |
| --- | --- | --- | --- |
| `total_rows` | el drenaje paginado (la 1.ª página son 100, los lotes 500) | **es la altura del canvas de scroll y el `aria-rowcount`**: un directorio de 5.000 ficheros se queda topado en la fila 100 para la rueda | **V** (mecanismo) |
| `hidden_note` + `total_rows` | `pane.toggle-hidden` | el chip «N ocultos» se queda con el número de antes y el scroll con la extensión de antes | A |
| `path_display` | `pane.names-encoding` | las filas se retranscriben y la cabecera no — que es exactamente el medio arreglo que #57 y #293 dicen que no puede pasar | A |
| `Processes.cursor` | una task progresa o caduca | fila resaltada que ya no es la que se cancelaría | A |
| `marks` | cualquier gesto de marcado | hoy nada: el renderer no pinta contador. Es el ítem 1 el día que se pinte | A |

`total_rows` puede ser una regresión de #252: antes, un drenaje grande
desbordaba el canal, el renderer recibía `Lagged` y el `resync` reparaba el
número por accidente. Los tests no lo ven porque todos llaman a `Resync` en
cada vuelta — y hay un comentario en `tests/controller.rs:18674` que ya lo
dice: «ni `total_rows` ni `path_display` viajan en un parche».

**Pendiente de comprobar en pantalla.** Quise abrir la ventana sobre
`/usr/bin` y se me fue el rato aislando el daemon (la sesión vive en su
estado, no en `XDG_CONFIG_HOME`; y el socket tiene que caber en `SUN_LEN` y
estar en un directorio 0700). El camino que funciona: daemon propio con las
tres XDG en el sandbox y socket en `/run/user/1000/…`.

## C. Decisiones escritas dos veces, y que YA discrepan

Ordenadas por lo que se nota. Todas con `file:line` en los dos lados en el
informe original; **A** salvo donde se diga.

1. ~~**`Enter` sobre un archivo comprimido o un symlink.**~~ **HECHO**: la
   decisión vive en `norte_frontend::nav::enter_target` y la llaman los dos.
   Y el doble de test ya sabe fabricar un symlink (`Falso::pon_kind`), que es
   lo que faltaba para poder escribirlo.
2. ~~**Aviso de espacio y de confinamiento antes de copiar.**~~ **HECHO**: el
   diálogo nace sin ellos y una task los rellena, que es el reparto del
   terminal. La regla del total —todo o nada— vive ahora en
   `norte_frontend::space::total_to_write`, donde estaba a medias: los dos
   helpers de las frases ya eran compartidos y solo el cálculo era privado
   del TUI. `DialogView.warnings` es el campo nuevo del bridge.
3. ~~**Borrado permanente.**~~ **HECHO**: la ventana lo saca de la caché de
   capacidades del hueco. Con TRES estados, no dos — «no consta» no es «no hay
   papelera», y convertir lo uno en lo otro borraba de verdad en un sitio que
   sí la tiene.
4. ~~**Una búsqueda que FALLÓ se lee como una terminada con 0 resultados.**~~
   **HECHO**: `Busqueda.viva: bool` pasa a un `Desenlace` de cuatro estados y
   la frase sale de la familia del terminal, fallo incluido. De paso: una
   búsqueda que ni llegaba a encolarse se quedaba diciendo «buscando…» para
   siempre, porque sin Task no hay progreso que traiga el desenlace.
5. ~~**`availability::Facts`: 6 de 8 campos distintos.**~~ **HECHO** el peor
   par y uno más. `source_read_only`/`dest_read_only` salen ya de las
   capacidades del hueco, por `availability::read_only`, que es de los dos;
   y `enterable` de `nav::enter_target`, también de los dos. De paso salieron
   dos bugs que nadie buscaba: el listado de ARRANQUE no pedía capacidades
   —o sea que el primer directorio de cada hueco estaba a ciegas, y con él el
   plegado de #268— y las capacidades se BORRABAN al pedir otras, tres líneas
   antes de que el aterrizaje re-congelara los hechos de la ayuda. Quedan los
   campos que no son un impedimento real (`degraded`, `journalled`).
6. ~~**Entradas omitidas.**~~ y 7. ~~**La marca de reinterpretación de
   nombres.**~~ **HECHOS**, con 17, en `norte_frontend::notes`: las SEIS
   frases que dicen que un listado no está completo se redactaban una vez por
   frontend y ahora se redactan una vez. La cabecera de la ventana gana
   `names_note`, `filling_note`, `pruned_note` y `marked_note` (puente 55).
8. ~~**Decirte que está esperando.**~~ **HECHO**, salvo el «Esc cancela», que
   se deja fuera A PROPÓSITO: la ventana no tiene camino para abortar un
   listado en vuelo, y el repo tiene esa doctrina escrita tres veces —jamás
   una affordance falsa—. El verbo sale del vocabulario cerrado compartido
   (`busy::BusyKind`: «conectando» no es «cargando») y el umbral viaja en el
   catálogo desde `busy::THRESHOLD`, en vez de ser un número en el CSS.
9. **Listas de ficheros en los diálogos:** basename/tope 10/«y 2 más» contra
   ruta entera/tope 16/«mostrando 16 de 200». **Se queda, y es una decisión.**
   Es el único de los diecisiete en el que ninguna de las dos superficies
   afirma nada falso: las dos frases son completas —10 + «y 190 más» y «se
   enseñan 16 de 200» dicen lo mismo— y los dos topes son de LEGIBILIDAD,
   sobre superficies de anchos distintos. Unificarlo costaría un campo de
   puente y veinticuatro sitios de construcción para que dos pantallas que ya
   dicen la verdad la digan con las mismas palabras. Si algún día se toca, lo
   que hay que compartir es la FRASE —para que no puedan divergir hacia decir
   cosas distintas— y no el tope.
10. ~~**El diálogo de colisión pierde la insignia de hostil y la
    reinterpretación del panel.**~~ **HECHO**. Y la codificación se captura AL
    LANZAR, no al llegar: la colisión aparece asíncrona y entre el envío y la
    pregunta cabe cambiar de hueco — el terminal lo lleva así en su
    `RetrySpec` desde #98.
11. ~~**La marca de destino `→`**~~ **HECHO**:
    `layout::target_worth_marking`, y la aplica el RENDERER — el rol del DTO
    es el modelo y decirle al host que mienta rompía tres tests que lo leen
    como tal. Que el rol EXISTA y que se PINTE son dos preguntas.
12. ~~**El panel de registro habla tres vocabularios.**~~ **HECHO**: el id de
    cable se COMPARA y la etiqueta se LEE, y viajan las dos. `TRACE` no se
    traduce —es lo que se escribe en `RUST_LOG`—; los botones de nivel sí,
    porque son un mando y el terminal no tiene ninguno con el que discrepar.
13. ~~**El plan de renombrado de la IA.**~~ **HECHO**, y moviendo el
    TERMINAL: la regla estricta era la buena. Una firma sobre algo que no se
    ha leído no es una firma, y con doscientos renombrados los que importan
    pueden estar en la fila ciento ochenta. `approval_ready` lo decide para
    los dos, sobre una marca de agua ALTA: volver arriba no des-lee lo ya
    leído.
14. **El diálogo de aprobación de un agente**: el TTL solo lo enseña la
    ventana; la insignia de hostil en las rutas ocultas solo el terminal.
15. ~~**La fila de un volumen.**~~ **HECHO**: una sola redacción
    (`PlacesState::volume_detail`). Las tres decían «desconocido» cuando lo
    único que faltaba era el TOTAL, tirando el dato que sí había — y cuánto
    queda es la mitad que se mira antes de copiar.
16. ~~**Instrucción de IA vacía.**~~ **HECHO**: el campo vuelve, como en su
    caso gemelo.
17. ~~**Marcas de cabecera que la ventana no tiene**~~ **HECHO** con 6 y 7,
    salvo «no listado» (#235): la ventana ya lo dice por `SlotState::Error`,
    que es su forma de la misma frase. `norte_frontend::notes::unlisted`
    queda escrita para quien la necesite.

## D. Fugas de idioma en la ventana

**HECHA.** `norte-ui-host` ya estaba limpio —sus 131 llamadas pasan
`self.lang`— y las cinco fugas eran helpers COMPARTIDOS que traducían con el
global. Cada uno gana su variante `_in(lang)` y la ambiente delega, que es el
patrón que `header_label` ya usaba en este mismo crate; la ventana pasa el
suyo. Lo que queda abajo es el inventario de lo que había.

| helper | qué se ve | conf |
| --- | --- | --- |
| `columns::format_mtime` | **cada celda de fecha del listado** en el idioma del proceso, bajo una cabecera en el del host | **V** |
| `settings::build_rows` | pantalla de Ajustes: títulos de sección en un idioma, nombre y descripción de cada opción en otro | A |
| `keymap::unavailable_message` | la barra de estado cambia de idioma según qué mensaje toque | A |
| `palette::plugin_rows` | prefijos `[Extensión]`/`[Renamer]` en la paleta | A |
| `columns::styled_cell` (kind, bool) | `Carpeta`/`Folder`, `Sí`/`Yes` | A |

`format_mtime` no se puede esquivar con configuración por el ítem de la tabla
A: la ventana ignora `time-format`, así que la rama relativa está siempre
viva.

---

## Plan por fases

Cada fase es una rama. El orden no es por tamaño: primero lo que impide que
la lista vuelva a crecer, después lo que el usuario nota.

### F1 — Los guardas (ADR 0097, decisiones 1 y 2)

Sin esto, arreglar la lista es barrer hacia la puerta.

1. **Exhaustividad de config en la ventana.** Un destructuring de
   `CommonConfig` sin `..` en el lado del host/arranque, como el que ya tiene
   el terminal en `App::desde_config`. Cada clave: la lee, la ignora a
   propósito (con motivo), o es de otro proceso.
2. ~~**El arnés de paridad compara FRONTEND con FRONTEND.**~~ **HECHO**:
   `crates/norte-ui-host/tests/parity.rs` corre cada escenario TRES veces
   —primitivas, host, `norte-tui`— y compara el estado semántico paso a paso.
   La tercera pata es la que guarda algo: las dos primeras medían el host
   contra un arnés escrito con las reglas del host.

   Cerrado después el hueco que su propia cabecera nombraba: el árbol de
   prueba solo tenía directorios y ficheros, así que no podía tocar la
   divergencia número uno (`Enter` sobre un `.zip` o un symlink). Ahora trae
   `cosas.zip` y `atajo`, y las tres patas preguntan por `nav::enter_target`.

   **Un arnés de paridad caza divergencia, no error compartido.** Al añadir
   un escenario, sabotea UNA pata y compruébalo rojo: estrechar la primitiva
   estrecha las tres y sale verde igual.
3. **Un test que enumere los huecos con sonda.** Cada kind que sigue al
   cursor tiene sonda o está en una lista de «solo foto, a propósito».

### F2 — Lo que se queda congelado (clase B)

Empezar por `total_rows`: comprobarlo en pantalla, y decidir entre meterlo en
el parche de filas o darle sonda. `hidden_note`, `path_display` y el cursor
de procesos salen por el mismo camino. Con F1.3 puesto, esta fase no vuelve.

### F3 — Las claves que solo honra un frontend (clase A)

`openers.toml` y `[ui] editor` primero: son features documentadas enteras.
Luego `[DIR]` contra la sesión (el terminal ya tiene `pin_start_dir`; es
portarlo), el tema como ruta, `quick_search`, `confirm_quit`, y el estilo por
columna. Decidir de una vez la precedencia de `[ui] lang` y escribirla en los
dos comentarios, que hoy se contradicen.

Aparte y explícito: borrar `font`/`mono_font`/`font_size`/`reduce_motion` del
catálogo de ajustes o implementarlas; e implementar `[profile.start]` o quitar
las dos promesas que la mencionan.

### F4 — Las decisiones ya divergidas (clase C)

Por impacto: 5 (read-only en un ZIP, que ofrece escrituras imposibles), 1
(Enter en un archivo), 2 (avisos antes de copiar), 4 (búsqueda fallida que
miente), 10 (colisión sin insignia). El resto en lotes. Cada una acaba en una
función compartida, no en dos arreglos.

### F5 — Idioma (clase D)

Mecánica: `_in` en los cinco helpers y el host pasando `self.lang`. Se puede
cerrar en una rama. Y un test que prohíba las formas globales en helpers que
el host llama.

## Lo que NO entra aquí

Si la ventana debe recargar la configuración en caliente. Hoy no lo hace y no
es un defecto de paridad: es una pregunta de diseño sobre qué le debe una
ventana de escritorio a un fichero de configuración. ADR aparte.
