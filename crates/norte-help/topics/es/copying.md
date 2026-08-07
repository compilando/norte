+++
id = "copying"
title = "Copiar, mover, renombrar y borrar"
tags = ["doing"]
see_also = ["selection", "remote", "archives"]
commands = [
    "pane.copy",
    "pane.move",
    "pane.rename",
    "pane.mkdir",
    "pane.delete",
    "pane.delete-permanent",
    "task.cancel",
    "dialog.overwrite",
    "dialog.skip",
    "dialog.rename",
    "dialog.newer",
]
context = [
    "dialog.confirm",
    "dialog.collision",
    "dialog.transfer-name",
    "dialog.mkdir",
]
+++
Marca lo que quieras en el panel con el foco y pulsa {{cmd:pane.copy}}. El
destino es el otro panel, tenga lo que tenga: un directorio local, un host
SSH, un bucket. Lo único que jamás puede ser destino es un archivo comprimido,
que solo sirve como origen; de eso va [[archives]].

{{cmd:pane.move}} mueve. Dentro de un mismo backend eso es un renombrado: no
se lee ni se escribe un solo byte, y da igual lo que ocupe aquello que estás
moviendo. Solo cuando los dos extremos son backends distintos —o sistemas de
ficheros distintos de esta máquina— degrada a copiar y luego borrar el origen.

{{cmd:pane.delete}} usa la papelera si el backend tiene una, y borra
definitivamente si no la tiene. {{cmd:pane.delete-permanent}} no usa la
papelera nunca. Los dos piden confirmación, y la confirmación dice cuál de las
dos cosas va a pasar.

# Poner nombres

La confirmación de una copia o un movimiento lleva el NOMBRE de destino, y es
editable. Déjalo y la entrada conserva el nombre que tiene; escribe encima y la
misma operación aterriza con el nombre que has tecleado. Copiar algo con otro
nombre no es una segunda función: es este campo.

{{cmd:pane.rename}} abre ese mismo diálogo con los dos extremos en el
directorio actual, que es lo que renombrar es: un movimiento que no va a ningún
sitio. Dentro de un mismo backend no cuesta nada, ocupe lo que ocupe.

{{cmd:pane.mkdir}} pide un nombre y crea un directorio en el panel con el foco.
Es lo único de esta página que crea en vez de mover, y está aquí porque todo lo
que viene después —la colisión, la papelera, el deshacer— también le aplica.

> ⚠ Un nombre son bytes, y lo que se te enseña es una representación de ellos. Cuando el texto en pantalla no es lo que hay en disco —bytes que no decodifican, un override que reordena lo que lees— va MARCADO. Vuelve a teclear un nombre que traiga el carácter de reemplazo en vez de confirmarlo: confirmar llamaría al fichero como lo que viste, no como lo que había.

# Toda transferencia es una tarea

Una copia no es una pantalla congelada: informa del progreso, sigue trabajando
mientras navegas por otro sitio y {{cmd:task.cancel}} detiene la más reciente.

Lo que cancelar promete es por **fichero**: los bytes van a un fichero de
staging y el nombre de destino no aparece hasta que ese fichero está entero,
así que nunca queda uno a medias con el nombre que esperabas.

Cancelar un **árbol** es otra cosa. Los ficheros ya copiados se quedan donde
aterrizaron —cada uno completo, ninguno a medias—, pero el directorio está ahí
y está incompleto. Nadie lo recoge por ti.

> ⚠ Cancelar una copia HACIA almacenamiento de objetos es el caso que se queda ambiguo: el servidor puede terminar una copia que ya había empezado, así que el objeto puede aparecer después de que canceles, y las partes sueltas de un multipart siguen costando dinero hasta que las barra una regla de ciclo de vida.

Las copias reanudables se piden a propósito, desde la línea de comandos. Ahí
lo que deja una copia cancelada es un fichero con `.norte-partial` en el
nombre, inconfundible de un vistazo, y por donde sigue el siguiente intento:

```sh
norte cp --resume sftp://host/big.iso ./big.iso
```

# Cuando el nombre ya está ocupado

La primera colisión detiene la transferencia y pregunta. Tu respuesta no se
aplica solo a esa entrada: la operación entera se reenvía con esa política, de
modo que una sola respuesta gobierna todas las colisiones que queden.

| Respuesta    | Qué pasa                                                      |
|--------------|---------------------------------------------------------------|
| sobrescribir | se reemplaza lo que haya en el destino                        |
| saltar       | las entradas que chocan se quedan como están                  |
| renombrar    | la copia aterriza al lado: informe.txt pasa a informe (1).txt |
| más nuevo    | se reemplaza solo donde el origen sea más reciente            |

Esas cuatro son comandos como cualquier otro —{{cmd:dialog.overwrite}},
{{cmd:dialog.skip}}, {{cmd:dialog.rename}} y {{cmd:dialog.newer}}—, así que las
teclas son tuyas para reasignarlas y esta página dice las que elegiste.

Responde con la tecla que enseña el diálogo, no con una por defecto: no la
hay, porque a un diálogo que puede destruir datos no debería podérsele
contestar de carrerilla.

> ⚠ **más nuevo** no adivina. Si a cualquiera de los dos lados le falta una fecha utilizable, esa entrada se detiene y vuelve a preguntar, en vez de reemplazarse o saltarse por corazonada.

> ⚠ Las colisiones por mayúsculas se juzgan contra el **destino**, no contra el origen: `README` y `readme` conviven sin problema en Linux y caen sobre el mismo fichero en macOS o Windows, y quien decide es el destino.
