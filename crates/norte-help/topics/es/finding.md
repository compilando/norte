+++
id = "finding"
title = "Encontrar cosas"
tags = ["doing"]
see_also = ["selection", "panes", "viewer"]
commands = ["pane.quick-search", "pane.search", "pane.toggle-hidden"]
+++
Dos preguntas distintas, dos teclas distintas. *¿Cuál de las filas que tengo
delante es?* es {{cmd:pane.quick-search}}. *¿Está en algún sitio aquí debajo?*
es {{cmd:pane.search}}.

# Estrechar lo que ya está listado

{{cmd:pane.quick-search}} escribe dentro del panel. Cada tecla filtra las filas
a las que contienen lo tecleado, el cursor aterriza en la primera, y salir de
la búsqueda deja el listado exactamente como estaba.

No se lee nada de disco ni se le pide nada a un daemon: las entradas ya están
aquí, y esto solo decide a cuáles estás mirando. Por eso es instantáneo en un
directorio de cien mil entradas, y por eso funciona igual sobre SFTP, en un
bucket o dentro de un `.zip`.

La comparación ignora mayúsculas y normaliza el nombre, así que `café`
encuentra el nombre que macOS guardó descompuesto. Los bytes del disco no los
toca nada de eso: lo indulgente es la comparación, no el nombre.

# Buscar en todo un subárbol

{{cmd:pane.search}} recorre todo lo que cuelga del directorio del panel. Casa
por nombre, con glob (`*.rs`) o con expresión regular, y también sabe buscar
por CONTENIDO: una cadena literal o una expresión regular sobre los ficheros
que el detector lee como texto.

Los resultados aparecen en el panel según se encuentran, así que los primeros
sirven mientras el recorrido sigue. Un hit que vive en otro sitio es una
entrada de verdad: pon el cursor encima y funcionan los comandos de las páginas
vecinas, incluido abrirlo en el visor.

Una búsqueda es una **tarea**, igual que una copia: informa del progreso, no
congela la pantalla, puedes navegar por el otro panel mientras corre y se puede
cancelar. De eso va [[copying]].

> ⚠ Una búsqueda por contenido contra un backend remoto LEE por la red los ficheros candidatos. En SFTP o en almacenamiento de objetos eso es una petición por fichero, y un árbol profundo es lento de una forma en que la misma búsqueda en disco local no lo es. Estrecha antes por nombre.

Lo que se transcodifica es la aguja, no el pajar: una búsqueda por contenido no
decodifica ficheros enteros a texto para mirar dentro, que es lo que le permite
correr sobre un directorio de codificaciones desconocidas sin inventar
contenido que no está.

# Entradas ocultas

{{cmd:pane.toggle-hidden}} enseña o esconde las entradas cuyo nombre empieza
por punto. Es por panel y es una decisión sobre el LISTADO: el provider lista
siempre todo, y ocultar solo aparta lo que ya está aquí — por eso volver a
mostrarlas es instantáneo y jamás relee el directorio.

La consecuencia conviene decirla sin rodeos, porque es la que sorprende: una
fila que no ves es una fila que no puedes marcar, así que un comando actúa
sobre la selección visible. Copiar un DIRECTORIO sigue copiando todo lo que hay
dentro, ocultos incluidos: el filtro está en lo que se te enseña, no en lo que
recorre una operación recursiva.

> 💡 Una búsqueda suspende el filtro. Pedir `.env` por su nombre y que te digan que no hay nada, porque la respuesta estaba oculta, es peor que una fila de más: una petición explícita gana a una preferencia de display.
