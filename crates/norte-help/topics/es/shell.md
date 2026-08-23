+++
id = "shell"
title = "Salir a un shell"
tags = ["doing"]
see_also = ["panes", "settings"]
commands = ["app.terminal", "app.toggle-panels", "pane.command-line", "pane.copy-path"]
context = ["dialog.command-line"]
+++
Un gestor de ficheros del que no puedes salir es un gestor de ficheros que
dejas de usar. Tres comandos te devuelven la terminal y luego la recuperan.
Los keymaps por defecto no los bindean —sí lo hacen los presets importados de
Krusader, Norton y Far—, así que de fábrica se alcanzan por la paleta de
comandos o bindeándolos tú. Los tres funcionan igual: norte se aparta —suelta la pantalla alternativa, el
modo raw y la captura del ratón—, el programa que has pedido se queda la
terminal entera, y los paneles vuelven cuando termina.

{{cmd:app.terminal}} abre tu shell (`$SHELL`, o `/bin/sh` si eso no dice nada)
en el **directorio del panel activo**. Sal del shell y vuelves al listado, ya
refrescado: lo que hayas cambiado ahí abajo está en pantalla.

{{cmd:pane.command-line}} pide un comando y lo ejecuta en ese mismo
directorio. La línea se le entrega al shell ENTERA, así que las tuberías, las
comillas y los globs significan lo de siempre; norte no la parsea. Al acabar,
su salida se queda en pantalla hasta que pulses una tecla: una salida que
desaparece bajo un listado repintado es como si no se hubiera impreso.

{{cmd:app.toggle-panels}} esconde los paneles y enseña la terminal de debajo
hasta que pulses una tecla. No lanza nada, así que es el único de los tres que
funciona también en un panel remoto.

# Lo que no es

{{cmd:app.toggle-panels}} enseña el **scrollback** de la terminal, no un shell
vivo. mc mantiene un subshell detrás de sus paneles y escribe en él; norte no.
Lo que ves es lo que ya estaba ahí. El de verdad se sigue en la issue #142.

Suspender también entrega la terminal entera, y norte solo puede devolver lo
que se llevó: la pantalla alternativa, el modo raw y la captura del ratón. Un
programa que muere dejando la terminal en un estado suyo queda fuera de lo que
se puede deshacer desde aquí.

Mientras un programa tiene la terminal, norte no está corriendo: ni vigilancia
de directorios, ni tick de tareas, ni respuesta a lo que pida un agente en
segundo plano. Una petición de aprobación que llegue durante una sesión larga
de shell caduca y se deniega —la dirección segura—, pero conviene saberlo
antes de dejar un shell abierto una hora.

Pegar en la línea de comandos se comporta como pegar en una terminal sin
soporte de bracketed paste: el primer salto de línea de lo que pegues
**confirma**, y el resto se descarta en vez de ejecutarse. Escribe el comando,
o pégalo y mira lo que hay en el campo antes de pulsar Enter.

`Ctrl+C` llega al programa, no a norte. `Ctrl+Z` no está contemplado:
suspender norte mientras tiene la terminal cedida no es algo de lo que se
recupere limpiamente, así que evítalo.

# No todos los paneles tienen shell

{{cmd:app.terminal}} y {{cmd:pane.command-line}} necesitan un directorio real
en esta máquina, así que declinan en un host SFTP, en un bucket S3 o dentro de
un archivo comprimido, y dicen de qué panel están hablando. Un shell abierto
«ahí» estaría en silencio en otro sitio —tu directorio personal, casi seguro—,
y eso es peor que una negativa.

Un shell que arrancas así eres **tú**, actuando con tus propios permisos.
norte no anota nada: no hay actor al que atribuirlo ni forma de deshacerlo, y
fingir lo contrario metería en el diario entradas que no se pueden revertir.
Mira [[agents]] para la otra mitad de esa regla: lo que sí queda escrito es lo
que hizo el propio norte.

# De qué norte estás saliendo

Un programa para el que la TUI se suspende hereda `NORTE_LEVEL`, uno más que
el que traía el propio norte. Ejecuta `ntc` dentro de un shell que abriste
desde norte y tendrás dos; la variable está para que tu prompt pueda decirlo,
igual que hace `SHLVL`.

La versión gráfica no siempre puede prometerlo. Una ventana de terminal
servida por una instancia que ya estaba corriendo —GNOME Terminal y Konsole lo
hacen, y macOS también— la arranca en realidad ese servidor, no norte, así que
hereda el entorno del servidor y no el nuestro.

## Copiar la ruta

{{cmd:pane.copy-path}} pone en el portapapeles la ruta de lo marcado —o la de
la entrada bajo el cursor si no hay nada marcado—, una por línea y en su forma
**nativa**: `/casa/notas.txt`, no `file:///casa/notas.txt`. Lo que no está en
este disco no tiene forma nativa, así que viaja como localización completa.

Hay dos caminos y norte dice cuál usó. Si encuentra un helper del escritorio
(`wl-copy`, `xclip`) lo usa, porque ése CONTESTA si funcionó. Si no hay
ninguno —lo normal en una sesión por SSH— manda la secuencia **OSC 52**, que
la interpreta el emulador de terminal que estás mirando y no la máquina donde
corre norte. Esa segunda no se puede confirmar: un terminal que no la soporte
la ignora sin decir nada, y por eso el aviso te pide que lo compruebes
pegando.
