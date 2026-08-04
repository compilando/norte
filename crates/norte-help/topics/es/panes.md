+++
id = "panes"
title = "Dos paneles, un destino"
tags = ["basics"]
see_also = ["selection", "copying"]
commands = ["pane.switch", "nav.enter", "nav.parent", "pane.refresh"]
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

> 💡 Un directorio al que vuelves a menudo merece un favorito: el panel recuerda por dónde ha pasado, y los favoritos son comunes a los dos paneles.
