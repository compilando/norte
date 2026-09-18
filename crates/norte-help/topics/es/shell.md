+++
id = "shell"
title = "Salir a un shell"
tags = ["doing"]
see_also = ["panes", "settings"]
commands = ["app.terminal", "app.toggle-panels", "pane.command-line", "pane.copy-path", "app.handoff"]
context = ["dialog.command-line"]
+++
Un gestor de ficheros del que no puedes salir es un gestor de ficheros que
dejas de usar. Tres comandos te devuelven la terminal y luego la recuperan.
Los keymaps por defecto no los bindean —sí lo hacen los presets importados de
Krusader, Norton y Far—, así que de fábrica se alcanzan por la paleta de
comandos o bindeándolos tú. Los tres empiezan igual: norte se aparta —suelta la pantalla alternativa, el
modo raw y la captura del ratón— y el programa que has pedido se queda la
terminal entera. Lo que cambia es cómo vuelves: dos de ellos esperan a que el
programa termine; el tercero te deja un shell VIVO al que vuelves con la misma
tecla.

{{cmd:app.terminal}} abre tu shell (`$SHELL`, o `/bin/sh` si eso no dice nada)
en el **directorio del panel activo**. Sal del shell y vuelves al listado, ya
refrescado: lo que hayas cambiado ahí abajo está en pantalla.

{{cmd:pane.command-line}} pide un comando y lo ejecuta en ese mismo
directorio. La línea se le entrega al shell ENTERA, así que las tuberías, las
comillas y los globs significan lo de siempre; norte no la parsea. Al acabar,
su salida se queda en pantalla hasta que pulses una tecla: una salida que
desaparece bajo un listado repintado es como si no se hubiera impreso.

{{cmd:app.toggle-panels}} esconde los paneles y le entrega la terminal a un
shell que **se queda vivo** detrás. Púlsalo otra vez y vuelves al listado;
púlsalo una tercera y vuelves al MISMO shell, con su historial, sus variables y
la línea a medio escribir que dejaste. Es el único de los tres en el que puedes
dejar un `make` corriendo.

El panel y ese shell se siguen el uno al otro. Al entrar, el shell se va al
directorio del panel activo; al volver, si te moviste con `cd`, el panel se va
adonde acabaste. El shell dice dónde está imprimiendo un marcador en su prompt,
que norte le instala TECLEÁNDOSELO: no se toca ningún fichero tuyo, y el arreglo
desaparece con el shell. bash, zsh y fish son los tres que sabe preparar; con
cualquier otro la tecla te sigue dando el shell, pero nada sigue a nada.

Al shell solo se le manda a un sitio si está **parado en su prompt**. Deja una
línea a medio escribir, o un `make` corriendo, o un `vim` abierto, y norte no
teclea nada: tu línea sigue siendo tuya. Por eso a veces el shell no sigue al
panel, y es la dirección segura — la alternativa es que norte le pegue un `cd`
detrás a una orden que habías decidido no ejecutar.

Arranca en la primera pulsación, no al abrir norte: si nunca pulsas la tecla,
no se forkea ningún shell. Escribe `exit` y la siguiente pulsación arranca uno
nuevo. Muere cuando muere norte.

La tecla que devuelve los paneles es **la misma que se los llevó**, y por eso un
preset tiene que atar {{cmd:app.toggle-panels}} a una tecla suelta. Una
secuencia no sirve: su primer acorde es del shell en el que estás tecleando.
Atado a una secuencia, el comando lo dice y no entrega nada, en vez de entregar
la terminal sin salida.

# Lo que no es

El shell es un shell, no un panel de norte. No sabe nada de las marcas, y
{{cmd:app.terminal}} sigue siendo el que quieres cuando lo que buscas es un
shell que se acaba al salir de él.

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

Los tres necesitan un directorio real
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

## Seguir en la otra ventana

{{cmd:app.handoff}} entrega la pantalla al OTRO frontend: la terminal se la
pasa a la ventana, y la ventana a la terminal. Lo que viaja es lo que estabas
mirando —las pestañas, los directorios, el cursor, por dónde has pasado— y
además lo que tenías **marcado**, que es lo único que no se rehace con un
`cd`.

Sólo funciona **con el daemon**, y por una razón que se puede decir en una
frase: la pantalla la guarda él. Sin daemon no hay nada que entregar, y por
SSH no hay ventana donde ponerla; en los dos casos el comando se anuncia no
disponible con ese motivo en vez de fallar después.

Cuando lo pides, el que se va escribe la pantalla, la **suelta** y lanza al
otro. Si el otro no arranca, no pasa nada grave: la pantalla está guardada y
el que se iba sigue donde estaba. Lo peor que puede ocurrir es que te lo diga.

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
