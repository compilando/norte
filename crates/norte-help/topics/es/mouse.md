+++
id = "mouse"
title = "Usar el ratón"
tags = ["basics"]
see_also = ["selection", "panes"]
commands = ["nav.enter", "mark.toggle", "pane.copy", "pane.move"]
+++
norte escucha el ratón en los dos frontends. En el terminal eso tiene un
precio, y el precio está al final de esta página: léelo antes de preguntarte
por qué ya no puedes seleccionar texto.

- un click izquierdo le da el foco a ese panel y pone el cursor en la fila pulsada
- un doble click sobre una fila hace exactamente lo que {{cmd:nav.enter}}: entra en un directorio, en un archivo comprimido o en un remoto, y deja en paz a los ficheros
- la rueda desplaza el listado **que hay bajo el puntero**, tenga el foco o no, así que puedes leer un panel mientras trabajas en el otro. Mueve el cursor de ese panel, que es también sobre lo que actúa allí un comando cuando no hay nada marcado
- ctrl y un click cambian la marca de una fila, la misma marca que pone {{cmd:mark.toggle}} desde el teclado
- mayús y un click marcan el rango entre el cursor y la fila pulsada, y suman a lo que ya estuviera marcado
- arrastrar por encima de varias filas marca lo que barre, y volver sobre tus pasos las suelta otra vez

Un click a secas nunca marca. Marcar es siempre un modificador o un arrastre,
así que pasearse por un listado a ver qué hay no puede cambiar sobre qué va a
actuar el siguiente comando.

# Arrastrar entre paneles

Un arrastre que empieza sobre una fila **ya marcada** significa «llévate
esto», no «marca un poco más»: un solo gesto, dos trabajos, distinguidos por
el estado de la fila donde empezaste.

En este frontend de terminal ese gesto todavía no tiene dónde aterrizar:
soltar sobre el otro panel no hace nada, y lo dice en la barra de estado en
vez de fallar en silencio. Marca lo que quieras y pulsa {{cmd:pane.copy}} o
{{cmd:pane.move}}; el destino es el otro panel en cualquier caso.

# Devolverle el ratón al terminal

Mientras norte captura el ratón, tu emulador de terminal no ve los botones que
usa para su propia selección de texto. Seleccionar y pegar deja de funcionar
como lo tienes aprendido, y eso sorprende bastante más de lo que gusta el
soporte de ratón.

Dos salidas, y ninguna necesita reiniciar:

- mantén **Mayús** mientras arrastras. Casi todos los emuladores (xterm, GNOME Terminal, Konsole, Alacritty, kitty, WezTerm, Windows Terminal) leen Mayús+arrastre como «esta va por mí» y seleccionan texto con normalidad. Mayús hace aquí dos papeles: el emulador se lo queda, así que norte no lo ve y el marcado de rango de arriba no se dispara. En un terminal que sí deje pasar Mayús+arrastre pasa lo contrario — marcas el rango y no seleccionas nada
- pon `mouse = false` bajo `[ui]` en `norte.toml`, o desactiva *Ratón* en la pantalla de ajustes. La captura se suelta en cuanto se guarda el fichero

Esa misma liberación ocurre cada vez que norte le cede el terminal a otro
programa —un editor, un paginador— y se recupera cuando ese programa termina.
Un programa lanzado desde norte jamás hereda un terminal en modo ratón.

> 💡 Todo lo que hace el ratón aquí ya lo hacía el teclado. Si echas en falta un gesto, la tecla existe: el ratón es una segunda entrada, nunca la única.
