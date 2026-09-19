+++
id = "profiles"
title = "Perfiles y sesión"
tags = ["basics"]
see_also = ["panes", "settings", "appearance"]
commands = ["profile.pick", "profile.next", "profile.prev", "profile.save-as"]
+++
# Perfiles

Una disposición reparte la pantalla. Un **perfil** es el espacio de trabajo
entero: su disposición, su teclado, su tema, sus columnas, sus favoritos y
dónde dejaste cada panel. `fotos`, `servidores` y el árbol en el que estás
programando quieren respuestas distintas a todo eso, y un perfil es cómo se
mantienen separadas en vez de recolocar la misma pantalla a mano cada vez que
cambias de tarea.

Un perfil es un directorio dentro de `profiles/`, en tu directorio de
configuración, con la misma forma que tu configuración: un `norte.toml` y, si
quieres, su propio `keymap.toml`, su `openers.toml` y sus `layouts/`. Copiar un
perfil de una máquina a otra es copiar un directorio.

{{cmd:profile.pick}} los lista y marca en cuál estás. {{cmd:profile.next}} y
{{cmd:profile.prev}} giran sin abrir nada, que es lo que quieres cuando tienes
dos. `--profile <nombre>` arranca en uno para una sola vez. Si no dices nada,
norte vuelve al último en el que estuviste.

{{cmd:profile.save-as}} guarda como perfil **lo que ves ahora**: la disposición
tal cual está y el directorio de cada panel, para que su primer arranque te deje
donde lo dejaste. Si estabas en un perfil, el nuevo se lleva también su
`keymap.toml` — guardar como produce algo que se comporta como lo que tenías. El
nombre acaba siendo un directorio, así que se comprueba antes de escribir nada, y
guardar sobre uno que ya existe escribe encima de esas piezas y deja el resto de
sus ficheros intactos.

Lo que un perfil fija pisa a tu propia configuración —para eso lo eliges— y el
`.norte` de un proyecto sigue pisando al perfil. Lo que un perfil **no** puede
es cambiar dónde escucha el daemon, encender la IA, decidir dónde se escriben
los logs, subir los límites de los contenedores ni ejecutar un `init.lua`: un
perfil declara, no ejecuta. Lo que aparezca de eso dentro de uno se ignora y se
dice en voz alta, en vez de aplicarse en silencio.

Si el perfil que nombraste no carga, norte dice qué fichero y no arranca: tú
pediste ése. Si solo era el perfil en el que estabas la última vez, arranca sin
él y te lo dice, para que una errata en un directorio que estabas probando no
te deje nunca fuera del programa.

# La sesión: dónde estaba cada panel

Al cerrar, norte guarda la pantalla —la disposición, qué paneles están
abiertos, el directorio y el historial de cada uno— y al volver a abrir te deja
donde estabas. Eso es la **sesión**, y la guarda **una sola ventana**: la
primera que se conecta al daemon se la queda, y las demás arrancan con la misma
pantalla y a partir de ahí van por su cuenta, sin escribir nada. Dos ventanas
escribiendo la misma sesión se pisarían por turnos, y ninguna de las dos te
dejaría donde la dejaste.

Una ventana que no guarda lo dice con un indicador discreto en la barra de
estado: `sesión sin guardar`. Pulsarlo con el ratón abre esta página. Significa
que **al cerrar esta ventana su pantalla no se recordará**; los ficheros no
tienen nada que ver con esto y no corren ningún riesgo. Pasa en tres casos.
Lo normal es que ya hubiera otra ventana de norte abierta —el terminal o la
gráfica, da igual— y sea ella la que guarda; cuando la cierres, la siguiente
que pregunte se queda con la sesión. También pasa mientras el daemon cambia de
manos (una actualización): la sesión queda libre un momento y esta ventana la
vuelve a pedir sola. Y pasa si la sesión guardada la escribió una versión de
norte MÁS NUEVA que ésta: no se toca, para no estropearla, y esta ventana
arranca de su configuración.

El indicador se va solo en cuanto la ventana vuelve a ser la que guarda. Lo que
un perfil dice de dónde abre cada panel es una semilla para los paneles que la
sesión no conoce; lo que la sesión recuerda gana.
