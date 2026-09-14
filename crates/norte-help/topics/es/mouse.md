+++
id = "mouse"
title = "Usar el ratón"
tags = ["basics"]
see_also = ["selection", "panes", "copying"]
commands = ["nav.enter", "mark.toggle", "pane.copy", "pane.move"]
+++
norte escucha el ratón en los dos frontends, y los dos se comportan igual
porque las reglas viven en un solo sitio. En el terminal eso tiene un precio, y
el precio está al final de esta página: léelo antes de preguntarte por qué ya
no puedes seleccionar texto.

- un click izquierdo le da el foco a ese panel y pone el cursor en la fila pulsada
- un doble click sobre una fila hace exactamente lo que {{cmd:nav.enter}}: entra en un directorio, en un archivo comprimido o en un remoto, y deja en paz a los ficheros
- la rueda desplaza el listado **que hay bajo el puntero**, tenga el foco o no, así que puedes leer un panel mientras trabajas en el otro. Mueve el cursor de ese panel, que es también sobre lo que actúa allí un comando cuando no hay nada marcado
- ctrl y un click cambian la marca de una fila, la misma marca que pone {{cmd:mark.toggle}} desde el teclado
- mayús y un click marcan el rango entre el cursor y la fila pulsada, y suman a lo que ya estuviera marcado
- arrastrar por encima de varias filas marca lo que barre, y volver sobre tus pasos las suelta otra vez
- un click derecho abre un menú con las operaciones para las que ya tienes teclas
- arrastrar el borde de la cabecera de una columna cambia su ancho, y el ancho se guarda solo. En la ventana el borde está a la derecha de cada cabecera; en el terminal es el separador que abre cada columna detrás del nombre, porque el nombre es el que crece. Un click en el borde sin moverte no cambia nada

Un click a secas nunca marca. Marcar es siempre un modificador o un arrastre,
así que pasearse por un listado a ver qué hay no puede cambiar sobre qué va a
actuar el siguiente comando.

# Arrastrar entre paneles

Un arrastre al otro panel **copia**. Con mayús pulsado, **mueve**. La decisión
se lee al SOLTAR, no al pulsar, así que un arrastre que empezaste como copia
sigue siendo una copia hasta el momento en que mayús esté pulsado — y puedes
cambiar de idea en los dos sentidos a mitad del gesto. Sea lo que sea, soltar
abre la misma confirmación que {{cmd:pane.copy}} y {{cmd:pane.move}}, y se
deshace igual: un drop es una operación normal, no una más silenciosa.

Qué viaja depende de la fila donde empezaste, que es como un solo gesto hace
dos trabajos:

- un arrastre que empieza sobre una fila **ya marcada** se lleva las marcas: todas, estén donde estén en ese listado
- un arrastre que empieza sobre una fila **sin marcar** se lleva esa fila sola, y solo desde el momento en que el puntero cruza al otro panel. Hasta entonces el mismo gesto sigue marcando lo que barre

A esa segunda regla se le llama promoción, y existe porque coger un fichero y
tirarlo al otro panel es el arrastre más común que hay. Cambia lo que el gesto
*hace*, jamás lo que está seleccionado: la fila que pulsaste no queda marcada
por él, y las filas que el barrido marcó de camino **se devuelven** en cuanto
el puntero sale del panel. Si vuelves, el barrido sigue desde el mismo ancla
sin haber perdido nada.

Dos formas de abortar, y las dos dejan la selección exactamente como estaba:
soltar sobre el panel de origen (soltar en casa no hace nada) o soltar donde no
haya fila — un borde, una cabecera, la barra de estado. Un destino no se
adivina nunca.

Como el gesto significa una cosa sobre su propio panel y otra sobre el de
enfrente, lo dice antes de que sueltes: cuántos elementos, a qué directorio, y
si copia o mueve. En el terminal ese renglón es la barra de estado. Sale del
mismo estado que lee el drop, así que no puede prometer una cosa y hacer otra
— pero el terminal solo reporta el teclado junto a un evento de ratón, así que
pulsar mayús sin moverte se refleja en la siguiente fila que cruces.

# El menú del botón derecho

El frontend gráfico abre un menú en el puntero con las operaciones que ya
tienen tecla: abrir ({{cmd:nav.enter}}), {{cmd:pane.view}}, {{cmd:pane.copy}},
{{cmd:pane.move}}, {{cmd:pane.rename}}, {{cmd:pane.delete}} y copiar la ruta al
portapapeles. Cada entrada ejecuta el MISMO comando que el teclado — no hay una
segunda forma de copiar un fichero. Las entradas que ahora mismo no pueden
correr (un listado de solo lectura, un remoto sin la capability) se pintan
apagadas con el motivo en vez de desaparecer.

Sobre qué actúa el menú lo decide la fila donde hiciste click derecho:

- si esa fila está **marcada**, el menú actúa sobre las marcas, y su cabecera dice cuántas son
- si **no** lo está, el menú actúa sobre esa fila sola

Eso tiene un precio, y conviene saberlo antes de que te sorprenda: hacer click
derecho sobre una fila sin marcar **suelta las marcas de ese panel**. No queda
otra, porque todos los comandos prefieren las marcas cuando las hay — dejarlas
haría que el menú dijera «1» y la copia se llevara once. La selección
descartada no se recupera, ni cerrando el menú con Esc. El intercambio es
deliberado: el fallo que evita es silencioso, y este se ve en el instante en
que el menú aparece.

# Devolverle el ratón al terminal

Mientras norte captura el ratón, tu emulador de terminal no ve los botones que
usa para su propia selección de texto. Seleccionar y pegar deja de funcionar
como lo tienes aprendido, y eso sorprende bastante más de lo que gusta el
soporte de ratón.

Dos salidas, y ninguna necesita reiniciar:

- mantén **Mayús** mientras arrastras. Casi todos los emuladores (xterm, GNOME Terminal, Konsole, Alacritty, kitty, WezTerm, Windows Terminal) leen Mayús+arrastre como «esta va por mí» y seleccionan texto con normalidad. Mayús hace aquí dos papeles: el emulador se lo queda, así que norte no lo ve y no se dispara ni el marcado de rango ni el mover-en-vez-de-copiar de arriba. En un terminal que sí deje pasar Mayús+arrastre pasa lo contrario: te llevas el gesto de norte y ninguna selección
- pon `mouse = false` bajo `[ui]` en `norte.toml`, o desactiva *Ratón* en la pantalla de ajustes. La captura se suelta en cuanto se guarda el fichero

Esa misma liberación ocurre cada vez que norte le cede el terminal a otro
programa —un editor, un paginador— y se recupera cuando ese programa termina.
Un programa lanzado desde norte jamás hereda un terminal en modo ratón.

> 💡 Todo lo que hace el ratón aquí ya lo hacía el teclado. Si echas en falta un gesto, la tecla existe: el ratón es una segunda entrada, nunca la única.
