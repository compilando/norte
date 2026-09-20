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

## Estrechar la búsqueda

Un nombre y un contenido rara vez bastan en un árbol grande, así que el
diálogo tiene además siete campos y cuatro interruptores. Todos son
opcionales y todos van en la misma dirección: quitar de en medio lo que no
estás buscando.

**Saltar carpetas** es el que más se nota. Nombres separados por comas
—`target, node_modules, .git`— y se saltan en CUALQUIER nivel, que es como
aparecen. Es un nombre y no una ruta justamente por eso: la carpeta que sobra
está en cien sitios que no sabes de antemano. Frena el DESCENSO y nada más,
así que la carpeta sigue pudiendo salir como resultado si su nombre casa.

**Al menos** y **como mucho** acotan el tamaño, escrito como se dice: `1M`,
`500k`, `2.5G`, o los bytes a pelo. **Cambiado hace** toma un número de días.
Y **busco** (`F6`) recorre «cualquier cosa», «ficheros» y «carpetas».

Un filtro SOLO ya es una búsqueda: «todo lo que pese más de un giga» no
necesita ningún nombre, y es de las preguntas que más se hacen.

**Palabra entera** (`F4`) es para el contenido: sin ella, buscar `set` en
código devuelve `offset`, `settings` y `subset`. Cuesta algo —obliga a
decodificar en vez de comparar bytes— y por eso no viene puesta.

**Subcarpetas** (`F5`) se puede apagar cuando la pregunta es «qué hay AQUÍ» y
no «qué hay aquí debajo».

**Leer el texto como** fuerza una codificación para el contenido. Vacío es lo
normal y lo que acierta casi siempre; esto es para cuando no acierta, igual
que el visor deja forzar la suya. Un nombre que no se reconoce se te dice, en
vez de caer a la automática y devolverte resultados creíbles leídos con otro
alfabeto.

> ⚠ Un campo que no se entiende PARA la búsqueda y te lleva a él. No es celo: lanzarla ignorando un `1 gigabyte` mal escrito devuelve el árbol entero, y un árbol entero se lee exactamente igual que un resultado.

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
