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
