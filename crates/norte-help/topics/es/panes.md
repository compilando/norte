+++
id = "panes"
title = "Dos paneles, un destino"
tags = ["basics"]
see_also = ["selection", "copying"]
commands = [
    "pane.switch",
    "nav.enter",
    "nav.parent",
    "pane.refresh",
    "pane.mirror",
    "pane.pull",
    "pane.swap",
    "nav.back",
    "nav.forward",
]
context = ["browse"]
+++
En pantalla hay siempre dos paneles. Uno tiene el foco: es donde se mueve el
cursor y de donde lee cualquier comando. El otro es el destino.

{{cmd:pane.switch}} le pasa el foco al otro panel. Lo demás sale de quién lo
tenga: {{cmd:nav.enter}} entra en la entrada bajo el cursor y
{{cmd:nav.parent}} sube al padre, dejando el cursor sobre el directorio del
que acabas de salir.

{{cmd:pane.refresh}} relee el listado, y relee **los dos** paneles, no solo el
que tiene el foco: un cambio hecho fuera de norte rara vez respeta cuál
estabas mirando. Los directorios locales se vigilan y se refrescan solos; un
remoto o un archivo comprimido no, así que esa es la tecla que te dice la
verdad sobre ellos.

Ningún comando pregunta *hacia dónde*. Por eso a un gestor ortodoxo le bastan
tan pocas teclas, y por eso el segundo panel no es una preferencia de
disposición con la que se pueda discutir.

# El destino es un panel, no un disco

El panel inactivo puede ser un host SFTP, un bucket de S3 o el interior de un
archivo comprimido. Copiar no cambia de comportamiento por eso; de ello va
[[copying]].

Cada panel guarda su propio historial de directorios y su propio orden, así
que el lado remoto puede no parecerse en nada al local sin que ninguno de los
dos tenga que ceder.

# Mandar una ubicación al otro lado

{{cmd:pane.mirror}} manda el **otro** panel a donde está este, y el foco no se
mueve. Es la forma más rápida de preparar una copia: {{cmd:pane.copy}} no
pregunta hacia dónde, así que preparar una transferencia *es* poner el otro
panel en su sitio, y esto lo pone sin que tengas que abandonar el origen.
{{cmd:pane.pull}} es el mismo gesto al revés: el panel con el foco se va a
donde está el otro.

Ninguno de los dos dice nada cuando los dos paneles ya están en el mismo sitio.
No ha fallado nada que hubieras pedido, y relistar un panel para nada le
movería el listado por debajo del cursor.

{{cmd:pane.swap}} los intercambia, que es como se invierte el sentido de una
copia sin navegar a ningún sitio. No toca el disco: no se relee ningún listado,
no hay nada que pueda fallar, y las marcas, el filtro, el orden, el cursor y el
historial de cada panel viajan con él, porque lo que se mueve es el panel
entero y no un listado reconstruido. El foco se queda en el mismo **lado** de
la pantalla a propósito: llevarlo con el contenido te dejaría mirando
exactamente el mismo listado y llamándolo intercambio.

Reflejar hacia un host en el que no has estado conecta y pregunta por su clave
igual que lo haría llegar andando. La pregunta es del panel que VIAJA, que con
un espejo no es en el que estás sentado. Si al destino no se llega, el panel se
queda donde estaba y el motivo sale por la barra de estado.

Un panel que enseña los resultados de una búsqueda viva no tiene ubicación que
dar: el directorio que hay detrás es la raíz por la que anduvo la búsqueda, no
la lista que estás leyendo, así que el gesto se rechaza y lo dice, en vez de
adivinar. El veto es solo del panel del que sale la ubicación: mandarle una
ubicación ENCIMA a un panel de resultados sí vale, y el listado real que llega
lo saca del modo búsqueda.

# Volver por donde viniste

{{cmd:nav.back}} devuelve el panel con el foco a donde estaba, y
{{cmd:nav.forward}} deshace ese paso. Cada panel recorre su propio rastro, y
ninguna de las dos teclas mueve el foco.

Es un RASTRO, no una lista. De un directorio a un segundo y de ahí a un
tercero, dos veces atrás llega al primero. Una lista de los últimos visitados,
recorrida como si fuera un rastro, oscilaría entre los dos más recientes para
siempre; por eso «dónde estaba hace un momento» y «por dónde ha pasado este
panel» son dos preguntas distintas: la segunda es el popup de
{{cmd:pane.history}}, y volver atrás nunca le añade nada.

Navegar a un sitio nuevo desde la mitad del rastro olvida la rama de la que te
saliste, igual que en un navegador. Ofrecer un «adelante» hacia una historia
que ya has abandonado es el fallo que todo el mundo conoce.

Un paso que falla se rebobina: no llegaste a irte, así que el rastro se queda
como estaba. Cuando el motivo es que el directorio **ya no está**, además sale
del rastro, de la rama de delante y del popup de historial, de modo que la
tecla no puede dejarte atrapado en un directorio que se ha demostrado que no
existe. Cualquier otro fallo lo conserva: un host caído o un directorio que no
puedes leer siguen siendo sitios, y pueden responder al siguiente intento.

> 💡 Un directorio al que vuelves a menudo merece un favorito: el panel recuerda por dónde ha pasado, y los favoritos son comunes a los dos paneles.

> 💡 Cuando ya no queda rastro hacia atrás, la tecla lo dice. Una tecla que se calla es indistinguible de una rota.
