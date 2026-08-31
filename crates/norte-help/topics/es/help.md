+++
id = "help"
title = "Cómo se lee esta ayuda"
tags = ["basics"]
see_also = ["index", "panes"]
commands = [
    "app.help",
    "app.palette",
    "dialog.filter",
    "dialog.pane",
    "dialog.back",
    "app.menu",
]
+++
{{cmd:app.help}} abre esta ayuda desde cualquier sitio. La lista de la
izquierda son todas las páginas; el panel de la derecha es la página en la que
estás.

{{cmd:dialog.pane}} salta de una a otra. En la izquierda, arriba y abajo
cambian de página, y el cursor ABRE aquello sobre lo que cae: recorrer la lista
con las flechas ya es leerla, no elegir qué leer. En la derecha recorren las
filas ejecutables y los enlaces del final. Enter sobre una fila ejecutable
cierra la ayuda y lanza el comando igual que lo lanzaría su tecla: la misma
confirmación, la misma política, la misma entrada en el diario. Enter sobre un
enlace lo sigue, y {{cmd:dialog.back}} te devuelve a la página de la que venías.

Las filas de los verbos del propio overlay —los tres de esta página— son la
excepción. Salen para que puedas consultar su tecla, pero solo significan algo
dentro de un overlay, así que un pane no tiene nada que lanzar: Enter sobre una
de ellas te lo dice y deja la ayuda abierta.

{{cmd:dialog.filter}} empieza a filtrar la lista de la izquierda. Busca en los
títulos, en los ids de las páginas y en los comandos que cada una documenta —el
comando, por el principio del id o de cualquiera de sus partes separadas por
puntos—, así que `copy` saca todas las páginas que documentan `pane.copy`, no
solo la que se llama así. Salir de la caja de búsqueda no borra lo escrito;
para vaciarla está el borrado.

# Las teclas que salen aquí son las tuyas

En estas páginas no hay ni una tecla escrita en el texto. Cada una se consulta
en tu keymap efectivo al dibujar la página, así que reasignar un comando cambia
la prosa. Un comando sin tecla ninguna enseña su nombre en vez de inventarse
una.

La última entrada de la lista, *keys*, es el camino contrario: el keymap
efectivo entero, generado, con los verbos `dialog.*` que los pies de los
overlays se dejan fuera por falta de ancho.

> 💡 {{cmd:app.palette}} es la versión rápida del mismo modelo: escribes, Enter, y fuera. Esta ayuda es la versión que explica.

{{cmd:app.menu}} abre una barra de menús con las mismas órdenes ordenadas por
tema. No añade nada que el teclado no pueda: añade una forma de ENCONTRARLO —
la paleta pide saber el nombre de lo que buscas y esta ayuda pide leer,
mientras que un menú se recorre. Izquierda y derecha pasan de un menú a otro,
arriba y abajo recorren sus órdenes, `Enter` ejecuta y `Esc` cierra.

La barra se queda fijada en la fila de arriba salvo que la apagues
(`[ui] menu_bar`), y ahí dice a su derecha con qué tecla se abre. Con la barra
a la vista, pulsar un título con el ratón abre ese menú directamente, sin
pasar por la tecla.

Justo debajo hay otra fila con una letra por panel —Sitios, Visor, Procesos,
Detalles, Árbol, Registro—, que existe por el mismo motivo: los paneles se
abren con su tecla, desde el menú o desde la paleta, y los tres caminos exigen
saber que el panel está ahí. Cada letra dice además si su panel está abierto,
si tiene el teclado y si tiene algo que contar; pulsarla lo abre. También
cuesta una fila, y `[ui] panel_bar` la devuelve.
