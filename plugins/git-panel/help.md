+++
id = "org.norte.git-panel"
title = "Git panel"
+++
Pinta un panel con el estado del repositorio que estás mirando: la rama
actual, el commit al que apunta, y los últimos movimientos del reflog —de
dónde vienes, que es lo que cuesta recordar después de un rato saltando
entre ramas.

Se coloca como cualquier otro hueco: en el selector de disposiciones aparece
como un panel más, y se puede poner al lado del listado, debajo, o en una
pestaña.

Lee la raíz del repositorio a la que el host lo confina —el ancestro que
contiene `.git`— y nada más: `.git/HEAD` para la rama, `.git/logs/HEAD` para
el commit y los movimientos. Nunca escribe, nunca ejecuta `git`, y no sabe
dónde está: el host le da un token y él pide caminos relativos.

Fuera de un repositorio el panel lo dice en una línea, en vez de quedarse en
blanco: un hueco vacío no se distingue de uno roto.

Las zonas pulsables del panel ejecutan comandos de norte, y solo los que
cualquier panel puede ejecutar: moverse entre paneles, abrir otro, cambiar el
tamaño. Un plugin elige la etiqueta y el comando, así que norte no le deja
nombrar nada que tú no pudieras hacer con una tecla mientras el panel tiene
el teclado.
