+++
id = "columns"
title = "Qué enseña el listado"
tags = ["doing"]
see_also = ["finding", "panes", "remote"]
context = ["dialog.properties"]
commands = [
    "pane.columns",
    "pane.sort-name",
    "pane.sort-ext",
    "pane.sort-size",
    "pane.sort-time",
    "pane.sort-menu",
    "pane.properties",
    "pane.chmod",
    "pane.dir-size",
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

# Los permisos vienen puestos

En un directorio de este disco y en un host SSH sale una columna de
**permisos**, `drwxr-xr-x`, sin que tengas que pedirla. Es `attr:posix.mode`,
una columna de atributo como las de arriba; lo único distinto es quién la
puso.

Y por eso se comporta distinto en un sitio: **cede**. Cuando el panel se
estrecha es la PRIMERA que se va, antes incluso que el tipo, porque una
columna que no pediste no puede ser la que te deje el nombre en `Cap….png`.
Ponla tú en `[ui.columns]` y deja de ceder, como cualquier otra que hayas
elegido.

No aparece hasta que el backend dice que tiene permisos POSIX, así que no la
verás en un bucket, dentro de un `.zip` ni en un Windows: una cabecera «Modo»
sobre doce celdas en blanco es ancho del nombre gastado en no decir nada.

Se puede ordenar por ella, como por cualquier columna de atributo: por el
VALOR, no por lo que se pinta, así que `rwxr-xr-x` y `rw-r--r--` quedan cada
uno con los suyos. Mira «Ordenar» más abajo.

> 💡 Una columna de plugin se rellena de forma asíncrona, con el listado ya en pantalla. Aparece un instante después en un provider lento, y si el listado cambia por debajo se descarta lo que había en vez de enseñar un valor que era del directorio anterior.

# Dueño y grupo

`attr:posix.owner` y `attr:posix.group` enseñan el dueño y el grupo por
**nombre**, como `ls -l`; `attr:posix.uid` y `attr:posix.gid`, por número.
No vienen puestas: enciéndelas en el selector. El nombre se le pregunta al
sistema, que puede estar preguntándoselo a un servidor de directorio, así que
solo se resuelve si la columna está a la vista, y una vez por dueño y minuto.
Un fichero cuyo dueño ya no existe deja la celda del nombre en blanco: el
número sigue en su columna.

Por ahora solo en este disco. Por SSH llegan los números; los nombres esperan
a que norte lea la línea larga del servidor.

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

Una columna de atributo —permisos, UID, GID…— ordena por su valor: números
como números, fechas como fechas. Lo que no trae el atributo va al final en las
dos direcciones. Ese orden vale para la sesión, pero no se guarda en
`norte.toml`: el fichero solo sabe nombrar los órdenes que tienen tecla, y al
aplicar el diálogo se te dice. Una columna de plugin no ordena: sus valores
llegan después del listado, y ordenar por ellos movería las filas bajo el
cursor mientras lees.

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

# Qué es esta entrada, y cuánto ocupa

{{cmd:pane.properties}} abre las propiedades de lo que hay bajo el cursor:
clase, tamaño, fecha, ruta y los atributos que el backend haya reportado. Todo
eso ya está en el listado, así que abrirlo no pide nada.

Menos una cosa, y es justo la que un listado no puede saber: **lo que ocupa una
carpeta**. Un listado dice el tamaño de un fichero; el de una carpeta exige
recorrerla entera, y hacerlo por cada fila convertiría bajar un nivel en una
tormenta de peticiones. Por eso se cuenta cuando lo pides: al abrir las
propiedades de una carpeta el diálogo empieza a contar y lo dice mientras tanto.

{{cmd:pane.dir-size}} cuenta sin abrir nada, y sobre lo MARCADO —o lo que haya
bajo el cursor si no marcaste nada—: la pregunta que contesta es «¿cuánto ocupa
todo esto?», que es la que te haces antes de copiar.

Contar es una tarea como cualquier otra: sale en el panel de tareas y se puede
cancelar. Lo que no se pueda leer no la tumba —una carpeta prohibida en medio de
un árbol de tres horas no puede costarte el recuento entero—, así que el número
es el de lo que se pudo leer.

# Cambiar los permisos

{{cmd:pane.chmod}} es la otra mitad: lo que las propiedades ENSEÑAN, esto lo
cambia. Pide el modo en octal —`755`, `0644`, `4755`— con el campo prellenado
con el que ya tiene lo que hay bajo el cursor, y actúa sobre lo marcado, o sobre
esa misma entrada si no marcaste nada. El título dice sobre cuántas va, porque
teclear un modo creyendo que va sobre una y que vaya sobre cincuenta es el error
que este diálogo tiene que ponerte difícil.

En octal y no con casillas porque es lo que teclea quien sabe lo que quiere, y
es la forma que el propio listado enseña. Los dígitos son del 0 al 7 y como
mucho cuatro: los bits de más arriba dicen de qué CLASE es el nodo, y eso no se
cambia, se es.

Es una mutación como copiar o borrar, con todo lo que eso arrastra: pasa por la
política, deja entrada en el diario y **se puede deshacer** — la reversa son los
permisos que tenía, leídos antes de escribir los nuevos. Cuando no se pueden
leer, el cambio se hace igual y el diario lo apunta como lo que es: algo sin
vuelta atrás.

Solo donde hay permisos POSIX: en un directorio local o en un host SSH sí, en un
bucket de objetos o dentro de un `.zip` no hay nada que cambiar, y se dice.

Por defecto cambia exactamente las entradas que le des: una carpeta cambia la
suya, no la de lo que tiene dentro. Con **`-R`** delante del modo baja por el
árbol, como el `chmod` de siempre. Y como el `chmod` de siempre tiene el mismo
pie de bala: `-R 644` le quita el bit de ejecución a las carpetas, y en una
carpeta sin ese bit no se puede ni entrar. Por eso puedes dar **dos modos**,
`-R 644,755`: el primero para los ficheros y el segundo para las carpetas. Un
segundo modo sin `-R` no significa nada y se te dice.

Un árbol muy grande se corta en un tope y norte dice cuántos nodos no llegó a
visitar, en vez de cambiar la mitad sin avisar. Los enlaces no se tocan tampoco
aquí dentro: `chmod` seguiría el enlace, y lo que se cambiaría es un fichero que
puede estar en cualquier otro sitio. Todo lo que sí cambia entra en el diario
como UNA acción, así que deshacer devuelve el árbol entero.
