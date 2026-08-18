+++
id = "columns"
title = "Qué enseña el listado"
tags = ["doing"]
see_also = ["finding", "panes", "remote"]
commands = [
    "pane.columns",
    "pane.sort-name",
    "pane.sort-ext",
    "pane.sort-size",
    "pane.sort-time",
    "pane.sort-menu",
    "dialog.toggle-enabled",
    "dialog.move-up",
    "dialog.move-down",
    "dialog.sort",
    "dialog.cycle-format",
]
+++
{{cmd:pane.columns}} abre el selector: todas las columnas que norte sabe
enseñar, cuáles están puestas y en qué orden salen.

- {{cmd:dialog.toggle-enabled}} pone o quita la columna resaltada
- {{cmd:dialog.move-up}} y {{cmd:dialog.move-down}} la mueven de sitio
- {{cmd:dialog.sort}} ordena el listado por ella
- {{cmd:dialog.cycle-format}} cambia cómo se escriben sus valores

Confirmar aplica la elección y la guarda; cancelar deja el listado como estaba.
Es uno de los pocos diálogos donde `⏎` es seguro por construcción: escribe tu
propia configuración y no toca ningún fichero.

# No todas las columnas las pone norte

Tres clases de columna comparten el selector, y se comportan distinto por
buenas razones:

| Clase    | De dónde sale el valor                    | Ejemplo            |
|----------|-------------------------------------------|--------------------|
| builtin  | la propia entrada                         | nombre, tamaño     |
| attr     | un atributo que reporta el provider       | `attr:posix.mode`  |
| plugin   | una extensión que lo calcula por entrada  | el estado de `git` |

Una columna que un backend no sabe contestar se queda **vacía**. Es el
resultado honesto: el almacenamiento de objetos no tiene dueño ni modo, y
pintar un `-` verosímil sería inventarse una respuesta. De eso va [[remote]].

> 💡 Una columna de plugin se rellena de forma asíncrona, con el listado ya en pantalla. Aparece un instante después en un provider lento, y si el listado cambia por debajo se descarta lo que había en vez de enseñar un valor que era del directorio anterior.

# Formatos

El tamaño y la fecha se pueden escribir de más de una manera —bytes o unidades
IEC, fecha absoluta o relativa— y {{cmd:dialog.cycle-format}} recorre las
opciones de la columna bajo el cursor. Una columna de atributo ofrece el ciclo
que permita la pista de su provider, y no ofrece ninguno cuando el valor es
opaco.

Hay un caso deliberadamente inerte: una columna cuyo formato fija una regla por
scheme de tu configuración enseña ese formato y se niega a ciclar. El selector
solo escribe el ajuste GLOBAL, y dejarte ciclar un valor que la regla más
específica va a seguir tapando es un diálogo que te miente sobre lo que acaba
de hacer.

# Ordenar

{{cmd:dialog.sort}} ordena por la columna resaltada; pulsarlo otra vez invierte
la dirección. El orden por defecto es por nombre, ascendente, directorios
primero. Una columna sin orden posible —el tipo de una entrada, por ejemplo— no
hace nada al pulsarla, en vez de inventarse un ranking.

El orden es por panel y se conserva mientras el panel viva, así que los dos
pueden estar ordenados distinto: que es justo lo que quieres cuando uno es un
listado que estás leyendo y el otro un destino que estás llenando.

# Ordenar sin abrir nada

Cuatro teclas ordenan el panel con el foco sin pasar por el diálogo:
{{cmd:pane.sort-name}}, {{cmd:pane.sort-ext}}, {{cmd:pane.sort-size}} y
{{cmd:pane.sort-time}}. Pulsar la que ya está activa invierte la dirección,
igual que hacer clic dos veces en una cabecera. {{cmd:pane.sort-menu}} abre el
diálogo de columnas, que es donde viven la dirección y el «directorios
primero»: ninguna tecla de orden los toca, porque son preferencias tuyas y no
criterios de una columna.

Ordenar por extensión mira lo que va después del ÚLTIMO punto. `.TXT` y `.txt`
caen juntas —agruparlas es de lo que va ordenar por extensión— aunque sigan
siendo nombres distintos para todo lo demás. Un nombre que empieza por punto no
tiene extensión: `.bashrc` es un nombre entero. Lo que no tiene extensión va al
final, en las dos direcciones, igual que un tamaño que el backend no sabe
decir.
