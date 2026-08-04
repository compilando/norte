+++
id = "archives"
title = "Dentro de un archivo comprimido"
tags = ["remote"]
see_also = ["copying", "remote"]
commands = ["nav.enter", "nav.parent", "pane.view", "pane.names-encoding"]
+++
{{cmd:nav.enter}} sobre un archivo comprimido entra en él. El panel lista lo
que hay dentro, el cursor se mueve como siempre, {{cmd:pane.view}} abre una
entrada en el visor y {{cmd:nav.parent}} vuelve a salir. No hay paso de
descompresión ni directorio temporal en ninguna parte.

Hay tres contenedores direccionables, reconocidos por la extensión:

| Formato | Extensión       |
|---------|-----------------|
| zip     | .zip            |
| tar     | .tar            |
| tar+gz  | .tar.gz, .tgz   |

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

> ⚠ Un archivo comprimido es de **solo lectura**. Copiar hacia fuera es una copia normal y vale con cualquier destino; copiar hacia dentro se rechaza.

# Nombres que no son UTF-8

Una entrada de zip solo promete UTF-8 cuando lo dice el bit 11 de su
cabecera. Los archivos antiguos vienen tan a menudo en CP437, CP866 o la
página de códigos de la máquina que los escribió que no hay forma de saberlo
mirando el fichero.

norte se queda con los bytes crudos y se niega a adivinar en silencio. Cuando
los nombres se ven mal, {{cmd:pane.names-encoding}} los relee con otra
codificación *solo para mostrarlos* — cp437, cp866, Shift-JIS, GBK,
windows-1252 — y los bytes del archivo siguen intactos, igual que lo que
escribe una copia al otro lado.

Una entrada cuyo nombre no puede ser una ruta, por ejemplo porque lleva `..` o
porque es absoluta, se queda fuera del listado en vez de acabar donde nadie
quería ponerla. El panel dice cuántas ha descartado.
