+++
id = "tabs"
title = "Pestañas en un panel"
tags = ["basics"]
see_also = ["panes", "selection"]
commands = [
    "tab.new",
    "tab.close",
    "tab.next",
    "tab.prev",
    "tab.move-left",
    "tab.move-right",
    "tab.goto-1",
    "tab.goto-2",
    "tab.goto-3",
    "tab.goto-4",
    "tab.goto-5",
    "tab.goto-6",
    "tab.goto-7",
    "tab.goto-8",
    "tab.goto-9",
]
+++
Un panel puede llevar varias pestañas, y cada una es un listado entero: su
directorio, su cursor, sus marcas y su historial. Cambiar de pestaña no
recuerda nada porque no ha olvidado nada.

{{cmd:tab.new}} abre una pestaña nueva junto a la que tienes delante, en el
mismo directorio y ya llena — es lo mismo que estabas mirando, así que no hay
nada que volver a leer. A partir de ahí las dos se mueven por su cuenta.

{{cmd:tab.next}} y {{cmd:tab.prev}} recorren el grupo dando la vuelta.
{{cmd:tab.goto-1}} hasta {{cmd:tab.goto-9}} van directas a una.
{{cmd:tab.move-left}} y {{cmd:tab.move-right}} reordenan la pestaña actual, y
se paran en el borde: una pestaña que salta del final al principio por una
pulsación de más no es lo que nadie quería.

{{cmd:tab.close}} cierra la actual. Cuando solo queda una, el grupo
desaparece y el panel vuelve a ser un panel — una barra de pestañas con una
sola pestaña no dice nada. Cerrar el último panel de un lado **no** es esto:
para eso está `layout.close-slot`.

Una pestaña que no se ve no gasta: no vigila su directorio ni pide nada. Al
volver a ella se pone al día.
