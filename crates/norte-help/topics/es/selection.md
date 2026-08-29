+++
id = "selection"
title = "Marcar sobre qué se actúa"
tags = ["basics"]
see_also = ["panes", "copying"]
commands = [
    "mark.toggle",
    "mark.all",
    "mark.invert",
    "mark.clear",
    "mark.pattern-add",
    "mark.pattern-remove",
    "mark.extension-add",
    "mark.extension-remove",
    "mark.files",
    "mark.dirs",
    "mark.restore",
    "app.pick-accept",
]
context = ["dialog.mark-pattern"]
+++
Un comando actúa sobre las entradas marcadas o, si no hay ninguna marcada,
sobre la que está bajo el cursor. No hay un tercer caso, y por eso un lote de
uno no necesita tecla propia.

- {{cmd:mark.toggle}} invierte la marca de la entrada bajo el cursor y baja una fila, así que manteniéndola pulsada barres un rango — y volver a barrer hacia atrás las desmarca
- {{cmd:mark.all}} marca todo lo que el listado esté mostrando
- {{cmd:mark.invert}} invierte las marcas de lo que se ve y deja el resto como estaba
- {{cmd:mark.clear}} las quita todas
- {{cmd:mark.pattern-add}} marca por glob y {{cmd:mark.pattern-remove}} desmarca por glob
- {{cmd:mark.extension-add}} marca las que comparten extensión con la de debajo del cursor, y {{cmd:mark.extension-remove}} las desmarca
- {{cmd:mark.files}} marca los ficheros y {{cmd:mark.dirs}} las carpetas, sumándose a lo que ya hubiera marcado
- {{cmd:mark.restore}} devuelve la selección de ANTES del último gesto en bloque

La extensión es la cola tras el último punto, así que un `.bashrc` no tiene
extensión: tiene nombre, y marcarlo no marca a los demás ocultos. Es la misma
regla que usa el renombrado por plantilla, y no por casualidad — dos
definiciones distintas marcarían un conjunto y renombrarían otro.

Restaurar guarda UNA foto por panel, la de antes del último gesto en bloque, y
va y vuelve: lo que rescata a quien pulsó «quitar todas» sin querer tiene que
rescatar también a quien pulsó «restaurar» sin querer. Un cambio de directorio
se la lleva, porque esas rutas ya no nombran nada de lo que estás viendo.

# Qué es «lo que el listado muestra»

Una marca en bloque alcanza lo que de verdad estás viendo. Con un filtro
rápido activo marca el subconjunto filtrado, no el directorio entero; y
mientras un listado largo todavía está llegando alcanza lo que ha llegado
hasta ahora, cosa que el panel avisa mientras ocurre.

Marcar por glob es la misma regla desde el otro lado: el patrón se compara
contra los nombres que tienes delante, así que `*.log` dos veces seguidas
marca el mismo conjunto dos veces, no lo desmarca. Antes de comparar se
ignoran mayúsculas y se normaliza el nombre, de modo que `*LEEME*` encuentra
`leeme` y un nombre escrito en NFD por macOS casa con lo que has tecleado.

Si un refresco descubre que una entrada marcada ya no existe, su marca se cae
con ella y la barra de estado dice cuántas se han perdido. Podarlas en
silencio cambiaría a hurtadillas sobre qué actúa el siguiente comando.

> ⚠ Las marcas son de **un** listado. Cambiar de directorio las borra, y una operación las consume en cuanto se envía el lote: así una selección nunca queda a medio gastar, pendiente de qué tarea acabó antes.

# Entregar la selección a otro programa

Arrancado como `ntc --pick`, norte responde en vez de actuar:
{{cmd:app.pick-accept}} escribe las entradas marcadas —o la que está bajo el
cursor, la misma regla de siempre— en la salida estándar, cada ruta terminada
en NUL en vez de en salto de línea, y sale. Un shell la canaliza directo a otra herramienta, por
ejemplo `ntc --pick | xargs -0 vim`.

Enter la ejecuta siempre que el cursor no esté sobre algo que de otro modo se
entraría, así que navegar a un directorio sigue funcionando; Ctrl+Enter
acepta pase lo que pase bajo el cursor. Salir de cualquier otra forma termina
sin escribir nada — un script distingue "no se eligió nada" de "norte falló"
por el código de salida, no analizando la salida.
