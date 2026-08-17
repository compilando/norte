+++
id = "tabs"
title = "Pestañas en un panel"
tags = ["basics"]
see_also = ["panes", "selection"]
commands = [
    "pane.tab-new",
    "pane.tab-close",
    "pane.tab-next",
    "pane.tab-prev",
    "pane.tab-move-left",
    "pane.tab-move-right",
    "pane.tab-goto-1",
    "pane.tab-goto-2",
    "pane.tab-goto-3",
    "pane.tab-goto-4",
    "pane.tab-goto-5",
    "pane.tab-goto-6",
    "pane.tab-goto-7",
    "pane.tab-goto-8",
    "pane.tab-goto-9",
]
+++
Un panel puede llevar varias pestañas, y cada una es un listado entero: su
directorio, su cursor, sus marcas y su historial. Cambiar de pestaña no
recuerda nada porque no ha olvidado nada.

{{cmd:pane.tab-new}} abre una pestaña nueva junto a la que tienes delante, en el
mismo directorio y ya llena — es lo mismo que estabas mirando, así que no hay
nada que volver a leer. A partir de ahí las dos se mueven por su cuenta.

{{cmd:pane.tab-next}} y {{cmd:pane.tab-prev}} recorren el grupo dando la vuelta.
{{cmd:pane.tab-goto-1}} hasta {{cmd:pane.tab-goto-9}} van directas a una.
{{cmd:pane.tab-move-left}} y {{cmd:pane.tab-move-right}} reordenan la pestaña actual, y
se paran en el borde: una pestaña que salta del final al principio por una
pulsación de más no es lo que nadie quería.

{{cmd:pane.tab-close}} cierra la actual. Cuando solo queda una, el grupo
desaparece y el panel vuelve a ser un panel — una barra de pestañas con una
sola pestaña no dice nada. Cerrar el último panel de un lado **no** es esto:
para eso está `layout.close-slot`.

Una pestaña que no se ve no gasta: no vigila su directorio ni pide nada. Al
volver a ella se pone al día.

Con el ratón: pulsa una pestaña para ir a ella, `[+]` para abrir otra y
`[x]` para cerrar la que estás viendo. Pulsar la barra de un panel le da el
foco antes de hacer nada — pulsar en un lado y que la orden la reciba el otro
sería lo contrario de lo que dijo el dedo.
