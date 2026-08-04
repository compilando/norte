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
tenga: {{cmd:nav.enter}} entra en la entrada bajo el cursor,
{{cmd:nav.parent}} sube al directorio padre y {{cmd:pane.refresh}} vuelve a
leer el directorio actual.

Ningún comando pregunta *hacia dónde*. Por eso a un gestor ortodoxo le bastan
tan pocas teclas, y por eso el segundo panel no es una preferencia de diseño
que se pueda negociar.

# El destino es un panel, no un disco

El panel inactivo puede ser un host SFTP, un bucket de S3 o el interior de un
archivo comprimido. Copiar no cambia de comportamiento por eso; de ello va
[[copying]].

Cada panel guarda su propio historial de directorios y su propio orden, así
que el lado remoto puede no parecerse en nada al local sin que ninguno de los
dos tenga que ceder.

> 💡 Un directorio al que vuelves a menudo merece un favorito: el panel recuerda por dónde ha pasado, y los favoritos son comunes a los dos paneles.
