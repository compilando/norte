+++
id = "dialogs"
title = "Contestar a un diálogo"
tags = ["basics"]
see_also = ["copying", "help", "panes"]
commands = [
    "dialog.confirm",
    "dialog.cancel",
    "dialog.up",
    "dialog.down",
    "dialog.page-up",
    "dialog.page-down",
    "dialog.top",
    "dialog.bottom",
    "dialog.section-prev",
    "dialog.section-next",
    "app.quit",
]
context = ["dialog.quit"]
+++
Todos los overlays de norte —una confirmación, un selector, una lista, esta
misma ayuda— hablan los mismos seis verbos:

- {{cmd:dialog.confirm}} acepta lo que el diálogo está enseñando
- {{cmd:dialog.cancel}} lo cierra sin cambiar nada
- {{cmd:dialog.up}} y {{cmd:dialog.down}} se mueven por él
- {{cmd:dialog.page-up}} y {{cmd:dialog.page-down}} se mueven de pantalla en pantalla

En esta ayuda, además, {{cmd:dialog.top}} y {{cmd:dialog.bottom}} van al
principio y al final del índice o de la página, {{cmd:dialog.section-prev}} y
{{cmd:dialog.section-next}} saltan a la sección anterior o siguiente de la
página, y en el texto las flechas lo desplazan línea a línea hasta que hay una
acción a la vista.

Cada diálogo admite el subconjunto que significa algo en él, y el pie se GENERA
a partir de ese subconjunto en vez de escribirse a mano. Lo que el pie ofrece es
lo que el diálogo acepta: no hay tecla que se trague en silencio ni tecla que
anuncie y luego ignore.

Reasigna cualquiera de ellos y todos los overlays le siguen, esta página
incluida: las teclas de esta ayuda se consultan en tu keymap al dibujarla. De
eso va [[help]]. Una excepción, en la ventana: las teclas que desplazan el
texto de esta ayuda (página, principio y final, secciones) siguen siendo allí
las de serie.

# La regla que importa

**Un diálogo que puede destruir datos no tiene respuesta por defecto.**
Confirmar no está atado a una tecla que pulsarías de carrerilla para pasar de
algo, y una colisión no se resuelve sola hacia `sobrescribir` porque te apoyes
en `⏎`. Se contesta con la tecla que enseña el diálogo, habiendo leído lo que
dice.

Esa regla da forma a los que más te vas a encontrar:

| Diálogo        | Qué está preguntando                                    |
|----------------|---------------------------------------------------------|
| confirmación   | ¿toco estos ficheros?, y cuántos son                    |
| colisión       | el nombre del destino ya está ocupado                   |
| aprobación     | un agente quiere actuar; aprobar no es nunca el default |
| clave de host  | este host es nuevo, o su clave cambió                   |

Un overlay que solo cambia tu propia configuración es la excepción, y lo dice
comportándose distinto: el selector de columnas y el de temas aplican con `⏎`,
porque lo peor que puede pasar es un listado que no querías y una tecla más
para deshacerlo.

# Salir

{{cmd:app.quit}} es la única confirmación que no muta nada, y por eso es un
buen sitio donde aprender la gramática: pregunta, `⏎` acepta y cancelar te
devuelve donde estabas.

Por defecto solo pregunta cuando hay algo pendiente —una tarea corriendo,
marcas que no has usado— y cierra directamente cuando no hay nada que perder.
Puedes hacer que pregunte siempre, o que no pregunte nunca, desde los ajustes.
De eso va [[settings]].

> ⚠ Salir cancela las tareas que sigan corriendo. Una copia cancelada deja entero cada fichero que terminó y a medias el directorio que estaba llenando; nadie lo recoge por ti.

Una salida de emergencia, si tu keymap tiene una, se salta la pregunta entera.
Es deliberado: un atajo cuyo propósito es sacarte de una pantalla atascada no
puede ser el atajo que abre otro diálogo.
