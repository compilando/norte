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
SSH, un bucket, el interior de un archivo comprimido. {{cmd:pane.move}} es la
misma operación y borra el origen una vez que la copia ha aterrizado.

{{cmd:pane.delete}} usa la papelera si el backend tiene una, y borra
definitivamente si no la tiene. {{cmd:pane.delete-permanent}} no usa la
papelera nunca. Los dos piden confirmación, y la confirmación dice cuál de las
dos cosas va a pasar.

# Toda transferencia es una tarea

Una copia no es una pantalla congelada: informa del progreso, sigue trabajando
mientras navegas por otro sitio y {{cmd:task.cancel}} detiene la más reciente.

Cancelar es el caso interesante, porque un fichero a medio escribir con el
nombre que esperabas es peor que no tener fichero. Los bytes van primero a un
fichero de trabajo y el nombre de destino no existe hasta que la copia
termina, así que cancelar aquí deja el destino **limpio**: nada a medias, nada
que recoger.

La excepción son las copias reanudables, que se piden a propósito desde la
línea de comandos. Ahí lo que deja una copia cancelada es un fichero con
`.norte-partial` en el nombre — inconfundible de un vistazo, y el punto por
donde sigue el siguiente intento:

```sh
norte cp --resume sftp://host/big.iso ./big.iso
```

# Cuando el nombre ya está ocupado

Cada colisión se pregunta, de una en una, y la respuesta es tuya:

| Respuesta   | Qué pasa                                             |
|-------------|------------------------------------------------------|
| Sobrescribir| se reemplaza lo que hay en el destino                |
| Omitir      | esa entrada se queda como está y las demás siguen    |
| Renombrar   | la copia aterriza al lado, con un sufijo (2)         |
| Más nueva   | se reemplaza solo si el origen es más reciente       |

No hay un «aplicar a todo», y Enter no es una respuesta: un diálogo que puede
destruir datos no tiene una opción inocua en la que apoyarse.

> ⚠ Las colisiones por mayúsculas se juzgan contra el **destino**, no contra el origen: `README` y `readme` conviven sin problema en Linux y caen sobre el mismo fichero en macOS o Windows, y quien decide es el destino.
