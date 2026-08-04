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
]
+++
Un comando actúa sobre las entradas marcadas o, si no hay ninguna marcada,
sobre la que está bajo el cursor. No hay un tercer caso, y por eso un lote de
uno no necesita tecla propia.

- {{cmd:mark.toggle}} invierte la marca de la entrada bajo el cursor y baja una fila, así que manteniéndola pulsada barres un rango — y volver a barrer hacia atrás las desmarca
- {{cmd:mark.all}} marca todo lo que el listado esté mostrando
- {{cmd:mark.invert}} invierte las marcas de lo que se ve y deja el resto como estaba
- {{cmd:mark.clear}} las quita todas
- {{cmd:mark.pattern-add}} marca por glob y {{cmd:mark.pattern-remove}} desmarca por glob

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
