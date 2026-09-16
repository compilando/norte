+++
id = "appearance"
title = "Lo que la pantalla enseña alrededor del listado"
tags = ["basics"]
see_also = ["settings", "dialogs", "panes", "help"]
commands = ["app.settings", "app.menu", "layout.log"]
+++
Alrededor de los dos listados norte pinta unas filas de cromo, y cada una es
un ajuste bajo `[ui]` en `norte.toml` — o una fila de la pantalla de ajustes
({{cmd:app.settings}}), que es lo mismo sin acordarse de los nombres de las
claves. Apagar una devuelve su fila.

La barra de teclas
------------------

La última fila de la pantalla nombra las diez teclas de función y qué hace
cada una en la pantalla que tiene el teclado: el listado, o el visor. Se lee
de tu keymap, así que reatar F5 cambia su etiqueta, y una tecla que aquí no
hace nada enseña solo su número. Con un diálogo delante la fila se queda en
blanco: ningún preset pone una tecla de función en un diálogo. Pulsar una
celda con el ratón es pulsar la tecla. `key_bar = false` quita la fila.

La barra de paneles
-------------------

Bajo el menú hay una fila con un botón por panel lateral — Sitios, Visor,
Procesos, Detalles, Árbol, Registro —, cada uno con su nombre y la letra de
acceso subrayada, si está abierto, si tiene el teclado y si tiene algo que
contar. `panel_bar_style = "letters"` la deja en las letras solas, y los
nombres pasan solos a letras cuando no caben todos. `panel_bar = false` quita
la fila.

El pie del panel
----------------

El borde inferior de cada listado cuenta lo que hay — directorios, ficheros,
bytes —, luego lo marcado, y luego el espacio libre del volumen donde vive el
directorio. Cuando el borde se queda corto cae primero el espacio libre y
después la cuenta: lo que acabas de marcar es lo último en ceder.
`pane_footer = false` deja el borde limpio.

Fechas
------

La columna de modificación pinta la hora si el fichero cambió hoy, el día y
la hora si cambió este año, y la fecha si no (`date_format = "smart"`, el de
serie). `"relative"` pinta hace cuánto, y `"iso"` la fecha y la hora
completas. Los tres en tu hora local. Un ajuste de columna (`[ui.columns]`)
sigue mandando para esa columna.

Avisos
------

Un mensaje en la línea de estado se queda `notice_seconds` segundos (ocho
de serie), luego pasa al registro y deja una insignia `!n` a la derecha de la
línea hasta que abras el panel de registro ({{cmd:layout.log}}) — pulsar la
insignia lo abre. `0` mantiene un mensaje hasta la siguiente tecla, como
antes. Los avisos persistentes — una conexión degradada, un journal que no
abre — no caducan: son estado, no aviso.

La pantalla de arranque
-----------------------

Antes del primer listado, norte enseña qué build corre y contra qué core
habla, y se quita con la primera tecla o sola en poco más de un segundo
(`splash = "brief"`, el de serie). `"home"` la convierte en una pantalla de
inicio que se queda hasta que la toques, con los directorios a los que más
vas y tus favoritos, cada uno abierto por su número (1-9); un clic en la fila
hace lo mismo. `"off"` no enseña nada. El asistente de primer arranque le
gana: si aún no tienes `norte.toml`, se pregunta antes y la pantalla no sale.

El panel de procesos
--------------------

Con `processes_panel = "auto"` (el de serie) el panel se abre solo en cuanto
hay trabajo en marcha —una copia, un movimiento, un borrado— y se cierra solo
unos segundos después de que la última fila acabe, sin llevarse el teclado:
sigues en tu listado. Esos segundos son los que la fila terminada se queda en el
tablero, y son a propósito: un panel que desapareciera en el instante del
desenlace se llevaría por delante el único sitio donde se lee que algo falló. Solo cierra lo
que abrió él; uno que abriste tú se queda. Buscar, comparar o sumar no lo
abren: eso tiene su propia pantalla, y taparla con el panel sería decir dos
veces lo mismo. `"manual"` lo deja como estaba, abierto y cerrado por ti.

Directorios
-----------

Un directorio se distingue por su icono y su color, y además lleva una barra
al final del nombre cuando no hay iconos que mirar (`dir_indicator = "auto"`,
el de serie). `"slash"` la pone siempre y `"none"` nunca.

Botones en los diálogos
-----------------------

La línea de teclas de un diálogo se pinta como botones que se pueden pulsar,
cada uno con su tecla. `dialog_buttons = false` pinta la línea de teclas de
antes.

El cursor
---------

La fila del cursor del panel que tiene el teclado lleva el color de acento
del tema; el cursor del otro panel se queda en gris, para que dos cursores no
compitan por decir a dónde van las teclas. Los dos son roles del tema
(`selection` y `selection-unfocused`), y un tema propio puede fijarlos.

El primer arranque
------------------

Sin un `norte.toml` tuyo todavía, norte pregunta tres cosas una vez: qué
gestor de ficheros tienes en los dedos (las teclas lo siguen), qué tema, y si
la fuente del terminal pinta iconos. Esc deja lo de serie y no vuelve a
preguntar. `ntc --setup` vuelve a preguntar; `NORTE_NO_WIZARD=1` lo mantiene
cerrado. F9 abre el menú en todos los presets menos el de Krusader, donde
sigue siendo el terminal.

La ventana
----------

La ventana lleva su propia tipografía: JetBrains Mono para todo lo que se
alinea en celdas e Inter para menús y diálogos, 14 px en filas de 22 px,
empaquetadas para que se vea igual en cualquier máquina. `font`,
`mono_font` y `font_size` bajo `[ui]` siguen mandando cuando están.

Las cabeceras de columna van en versalitas, y arrastrar el borde derecho de
una fija el ancho de esa columna: se escribe en `[ui.columns]` como
`width`, que el terminal también lee. Cuando las columnas fijas dejarían al
nombre menos de diez celdas, la ventana descarta columnas desde la derecha
hasta que quepan, como hace el terminal.

El título del panel es una fila de migas — cada tramo es un botón que va
allí — y el pie lleva un indicador de dos píxeles con lo ocupado del
volumen, en aviso pasado el 75 % y en error pasado el 90 %. Un aviso sale
como un toast abajo a la derecha durante `notice_seconds`; los avisos
persistentes son píldoras.

`theme_light` y `theme_dark` nombran el tema que la ventana pinta cuando el
escritorio prefiere un esquema claro u oscuro; `theme` cubre el que no esté
puesto. Un tema puede pedir `backdrop = "blur"` bajo `[effects]` para
desenfocar lo que hay detrás de un diálogo; el terminal ignora ese bloque.

Un tema propio es un fichero en `themes/` dentro del directorio de
configuración, y se ofrece por su nombre allí donde se elige un tema.
`norte theme import` hace uno a partir de un tema de colores de Visual Studio
Code.

Cada fila enseña una casilla de marca al pasar el ratón; pulsarla alterna la
marca sin Ctrl. La extensión `file-icons` puede pintar glifos Nerd Font de
una celda (`style = "nerd"`), o los iconos Seti de Visual Studio Code, uno
por lenguaje (`style = "seti"`): la ventana lleva los glifos que necesita, un
terminal necesita una fuente Nerd parcheada.

Cuando el visor no tiene imagen propia — una foto que no cabe en su tope, un
formato que la ventana no decodifica — una extensión `thumbnail` puede
dársela: `image-thumb` lo hace con los ficheros de imagen, y el visor dice
«via» de quién es.

> 💡 Cada fila de la pantalla de ajustes dice qué hace y aplica al momento.
> El fichero es la interfaz de verdad; la pantalla es una forma de editarlo.
