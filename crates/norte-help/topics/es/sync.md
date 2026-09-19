+++
id = "sync"
title = "Sincronizar dos carpetas"
tags = ["doing"]
see_also = ["panes", "compare", "copying"]
commands = ["pane.sync-dirs"]
+++
# Sincronizar los dos paneles

{{cmd:pane.sync-dirs}} es la mitad que escribe. Planifica una sincronización de
un sentido —este panel sobre el otro—, te enseña todos los pasos que daría y no
hace absolutamente nada hasta que la apruebas. Dentro del panel de diferencias
de [[compare]] lo mismo es `s`, y `m` planifica un **espejo**, que además borra
del destino lo que el origen no tiene. Allí el sentido lo decide el lado activo
del propio panel de diferencias, el que `Tab` cambia y el pie nombra — no el
panel con el foco. En los dos casos el título del plan lo deletrea con una
flecha antes de que apruebes nada. Letras peladas a propósito: una tecla de
función con modificador no sobrevive a una sesión de `tmux`, y un atajo
documentado que no llega nunca es peor que ninguno.

Nada se planifica dos veces y nada se ejecuta desde la pantalla. Lo que apruebas
es un plan que el daemon tiene guardado, nombrado por su propio resumen
criptográfico, así que lo que corre es byte a byte lo que has leído.

El plan empieza por lo que el deshacer podría devolver, y eso es un hecho del
DESTINO y no de los pasos. La misma lista de copias se revierte entera contra un
destino cuya papelera apunta dónde enterró las cosas, y no revierte nada contra
uno que no tiene papelera — así que el resumen dice cuál de los dos tienes
delante antes que ninguna otra cosa. Un `espejo` que borra árboles, o cualquier
plan que el deshacer no cubra entero, hace una segunda pregunta con el número
dentro.

Cada paso lleva tres marcas: qué hace, cuánto valía la comparación que hay
detrás, y si el deshacer lo devuelve. La tercera es la que obligó al daemon a
decir algo nuevo, y jamás se lee del paso a solas.

Marca filas con `Ins` en el panel de diferencias para sincronizar solo ésas; una
carpeta marcada se lleva su subárbol entero. Sin nada marcado el plan cubre los
dos árboles.

Esto necesita norte contra el daemon. Sincronizar borra y sobrescribe, así que
tiene que quedar en el journal y poder deshacerse, y el motor en proceso no
tiene journal — la tecla lo dice en vez de fallar a medias.
