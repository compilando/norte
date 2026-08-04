+++
id = "copying"
title = "Copiar, mover y borrar"
tags = ["doing"]
see_also = ["selection", "remote", "archives"]
commands = [
    "pane.copy",
    "pane.move",
    "pane.delete",
    "pane.delete-permanent",
    "task.cancel",
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

Responde con la tecla que enseña el diálogo, no con una por defecto: no la
hay, porque a un diálogo que puede destruir datos no debería podérsele
contestar de carrerilla.

> ⚠ **más nuevo** no adivina. Si a cualquiera de los dos lados le falta una fecha utilizable, esa entrada se detiene y vuelve a preguntar, en vez de reemplazarse o saltarse por corazonada.

> ⚠ Las colisiones por mayúsculas se juzgan contra el **destino**, no contra el origen: `README` y `readme` conviven sin problema en Linux y caen sobre el mismo fichero en macOS o Windows, y quien decide es el destino.
