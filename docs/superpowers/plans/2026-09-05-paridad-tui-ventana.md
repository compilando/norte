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

1. **`Enter` sobre un archivo comprimido o un symlink.** El terminal entra en
   el `zip+file://`; la ventana se lo da a `xdg-open`. Un symlink a
   directorio: el terminal navega, la ventana lo trata como fichero. Y el
   comentario de la ventana afirma que hace «la misma decisión que el TUI».
   La ventana ya conoce `archive_root_for`: lo usa para desempaquetar.
2. **Aviso de espacio y de confinamiento antes de copiar.** El terminal dice
   «no cabe» y «no puedo confinar» antes de que confirmes. La ventana no
   tiene esas líneas: te enteras por una task fallida.
3. **Borrado permanente.** «⚠ aquí NO hay papelera: esto no se deshace» es
   solo del terminal. La ventana compensa con un botón destructivo; ninguna
   de las dos tiene la señal de la otra.
4. **Una búsqueda que FALLÓ se lee como una terminada con 0 resultados.** El
   host marca cualquier estado terminal como «no viva» y pinta
   `search-status-done`. Es una afirmación falsa sobre el disco.
5. **`availability::Facts`: 6 de 8 campos distintos.** El peor par:
   `source_read_only`/`dest_read_only` están cableados a `false` en la
   ventana, así que dentro de un ZIP el terminal apaga F5/F8 y la ventana los
   ofrece encendidos. El host ya recibe `capabilities` y tira todo menos
   `fold_mode`.
6. **Entradas omitidas.** El terminal: «⚠ N omitidas (nombres hostiles /
   límites)». La ventana: «N entradas se saltaron», sin ⚠ y **también cuando
   N es 0**.
7. **La marca persistente de reinterpretación de nombres** solo existe en el
   terminal. La ventana transcribe y no lo dice más allá del mensaje del
   toggle.
8. **Decirte que está esperando.** El terminal: nada antes de 250 ms, luego
   spinner, a dónde va y «Esc cancela». La ventana: `aria-busy="true"` y
   **ninguna regla CSS que lo pinte**. Un SFTP lento no da señal ninguna.
9. **Listas de ficheros en los diálogos:** basename/tope 10/«y 2 más» contra
   ruta entera/tope 16/«mostrando 16 de 200».
10. **El diálogo de colisión pierde la insignia de hostil** (`display_lossy`
    ya metió U+FFFD, así que `display_name` lo declara fiel) **y la
    reinterpretación del panel** (en un panel cp866 el terminal pregunta por
    `Папка` y la ventana por `??????`).
11. **La marca de destino `→`** se enciende siempre en la ventana con dos
    paneles; el terminal la reserva para tres o más, que es lo que el crate
    compartido documenta.
12. **El panel de registro habla tres vocabularios**: `TRACE` / `trace` /
    `traza` — y los tres a la vez en pantalla, porque los botones de nivel de
    la ventana usan el catálogo y su chip usa el nombre de cable. Hay tests
    en los dos lados FIJANDO la divergencia.
13. **El plan de renombrado de la IA** es aplicable en el terminal e inerte
    en la ventana hasta que bajas hasta el final.
14. **El diálogo de aprobación de un agente**: el TTL solo lo enseña la
    ventana; la insignia de hostil en las rutas ocultas solo el terminal.
15. **La fila de un volumen** en la barra de sitios: tres implementaciones,
    una de ellas dentro del propio host, y la de la ventana pierde el dato
    que sí tiene cuando el total es desconocido.
16. **Instrucción de IA vacía**: el terminal deja el error dentro del modal
    con lo tecleado; la ventana ya se comió el diálogo y lo dice en la barra.
    El caso gemelo (consulta semántica) se arregló a conciencia tres ficheros
    más allá.
17. **Marcas de cabecera que la ventana no tiene**: «rellenando, N por
    ahora», «no listado» (#235), quick-search parcial, y el resumen de lo
    marcado. Las cuatro escritas bajo la regla «un listado incompleto jamás
    es silencioso».

## D. Fugas de idioma en la ventana

`norte-ui-host` está limpio: sus 131 llamadas pasan `self.lang`. Todas las
fugas son helpers compartidos que traducen con el global.

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
2. **El arnés de paridad compara FRONTEND con FRONTEND.** Hoy compara el host
   contra las primitivas con una regla que es la del host. Escenario
   semántico → se corre contra `norte-tui` y contra `norte-ui-host` → se
   comparan las respuestas. Lista de excepciones nombradas, como
   `paridad.rs::NO_APLICA`.
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
