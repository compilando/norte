+++
id = "archives"
title = "Dentro de un archivo comprimido"
tags = ["remote"]
see_also = ["copying", "remote"]
commands = [
    "nav.enter",
    "nav.parent",
    "pane.view",
    "pane.names-encoding",
    "pane.pack",
    "pane.unpack",
    "pane.test-archive",
    "pane.split-file",
    "pane.combine-files",
]
+++
{{cmd:nav.enter}} sobre un archivo comprimido entra en él. El panel lista lo
que hay dentro, el cursor se mueve como siempre, {{cmd:pane.view}} abre una
entrada en el visor y {{cmd:nav.parent}} vuelve a salir. No se descomprime
nada en una carpeta que luego tengas que encontrar y borrar.

Hay tres contenedores direccionables, reconocidos por la extensión:

| Formato | Extensión       | Cómo se lee                        |
|---------|-----------------|------------------------------------|
| zip     | .zip            | saltando directo a la entrada      |
| tar     | .tar            | saltando directo a la entrada      |
| tar+gz  | .tar.gz, .tgz   | descomprimiendo; ver más abajo     |

Un `.tar.gz` no se puede saltar: llegar a la última entrada obliga a
descomprimir todo lo anterior. Curiosear en uno sale barato, pero sacar varias
entradas del mismo significaría empezar de cero cada vez, así que a partir de
la segunda lectura de un mismo archivo norte lo descomprime una vez en un
fichero temporal y sirve el resto desde ahí. Ese fichero es anónimo y está
desenlazado, o sea que no aparece en ningún listado y el espacio vuelve solo
en cuanto norte lo suelta; pero mientras existe ocupa tanto como el archivo
descomprimido. Por encima de un gigabyte ni se construye, y las lecturas se
quedan por el camino lento.

# El ! del camino

Una ruta de archivo nombra a la vez el contenedor y la entrada de dentro, con
un segmento `!` entre los dos:

```
zip+file:///home/tu/fotos.zip/!/2019/espana.jpg
```

El esquema crece con el formato y el `!` marca la frontera. Todo lo que queda
a su izquierda es un fichero corriente en el backend exterior, que a su vez
puede ser remoto: `zip+sftp://` es una dirección real, y leer un zip en un
host SSH no exige descargarlo antes.

> ⚠ Un archivo comprimido es de **solo lectura** por dentro: copiar hacia fuera es una copia normal y vale con cualquier destino, copiar hacia *dentro* se rechaza. Hacer uno **nuevo** es otra cosa, y está aquí abajo.

# Hacer uno

{{cmd:pane.pack}} escribe un archivo nuevo con lo que tengas marcado, o con la
entrada bajo el cursor. Pregunta el nombre, y el nombre decide el formato —
`.zip`, `.tar`, `.tar.gz` o `.tgz`; el diálogo dice cuál va a escribir antes de
que pulses Enter. `.rar` no está en esa lista: norte lee rar delegando en un
programa externo, y a ese programa no se le pide que escriba.

Los nombres que se guardan dentro son los que ves en pantalla, relativos al
panel desde el que empaquetas. Cancelar no deja nada — ni un fichero a medias
con pinta de archivo.

{{cmd:pane.unpack}} es lo contrario y no pregunta nada: copia el contenido del
archivo al otro panel, que es una copia normal con las preguntas normales sobre
colisiones y con el deshacer de siempre.

{{cmd:pane.test-archive}} lee cada entrada hasta el final y dice qué ha fallado.
Lo que significa «pasa» depende del formato, y el informe lo dice: un zip lleva
un CRC por entrada, un `.tar.gz` una suma de todo el flujo, y un tar pelado no
lleva ninguna — ahí lo único que se puede comprobar es que cada tamaño
declarado se alcanza.

# Partir un fichero grande

{{cmd:pane.split-file}} corta un fichero en trozos numerados — `nombre.001`,
`nombre.002` — en el otro panel, del tamaño que le pidas (`10M`, `700M`,
`4096`). Más de 999 trozos se rehúsa antes de escribir nada: un conjunto que se
queda sin números a la mitad no lo vuelve a juntar nadie.

{{cmd:pane.combine-files}} los junta, empezando por el `.001`. Un hueco en la
numeración, o un trozo intermedio más corto que el primero, lo para: un fichero
mal unido es un fichero corrupto con buena pinta.

# Nombres que no son UTF-8

Una entrada de zip solo promete UTF-8 cuando lo dice el bit 11 de su cabecera.
Los archivos antiguos vienen una y otra vez en CP437, CP866 o la página de
códigos que usara la máquina que los escribió, y nada dentro del fichero dice
cuál de ellas es.

norte se queda con los bytes crudos y se niega a adivinar en silencio. Cuando
los nombres se ven mal, {{cmd:pane.names-encoding}} los relee con otra
codificación *solo para mostrarlos* — cp437, cp866, Shift-JIS, GBK,
windows-1252 — y los bytes del archivo siguen intactos, igual que lo que
escribe una copia al otro lado.

Una entrada cuyo nombre no puede ser una ruta, por ejemplo porque lleva `..` o
porque es absoluta, se queda fuera del listado en vez de acabar donde nadie
quería ponerla. El panel dice cuántas ha descartado.
