+++
id = "panes"
title = "Dos paneles, un destino"
tags = ["basics"]
see_also = ["selection", "copying", "history"]
commands = [
    "pane.switch",
    "cursor.up",
    "cursor.down",
    "cursor.page-up",
    "cursor.page-down",
    "cursor.top",
    "cursor.bottom",
    "nav.enter",
    "nav.parent",
    "pane.refresh",
    "pane.mirror",
    "pane.mirror-target",
    "pane.pull",
    "pane.swap",
    "nav.back",
    "nav.forward",
    "pane.select-drive",
    "pane.select-drive-left",
    "pane.select-drive-right",
    "pane.compare-dirs",
    "pane.compare-files",
    "pane.sync-dirs",
    "layout.split-h",
    "layout.split-v",
    "layout.focus-next",
    "layout.focus-prev",
    "layout.close-slot",
    "layout.grow",
    "layout.shrink",
    "layout.equalize",
    "layout.set-target",
    "layout.places",
    "layout.preview",
    "layout.processes",
    "layout.metadata",
    "layout.log",
    "layout.disk-map",
    "layout.pick",

    "profile.pick",
    "profile.next",
    "profile.prev",
    "profile.save-as",

    "pane.tree",]
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

# Mover el cursor

{{cmd:cursor.up}} y {{cmd:cursor.down}} avanzan una fila,
{{cmd:cursor.page-up}} y {{cmd:cursor.page-down}} una pantalla, y
{{cmd:cursor.top}} y {{cmd:cursor.bottom}} van a los extremos del listado.

El cursor es del panel, no de la pantalla: cada uno lleva el suyo, y un panel
al que vuelves está donde lo dejaste, en la fila en la que lo dejaste. Eso es
también lo que hace utilizable el segundo panel como destino mientras trabajas
en el primero.

El cursor es a lo que recurre un comando cuando no hay nada marcado: una fila
es un lote de uno, y no necesita tecla propia. De eso va [[selection]].

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

{{cmd:pane.mirror-target}} manda lo que está **bajo el cursor**: si es una
carpeta, el otro panel entra en ella; si no lo es, la ubicación de este panel,
que es lo mismo que {{cmd:pane.mirror}}. Sirve para mirar dentro de un
directorio sin salir de donde estás, y es el gesto que un usuario de Krusader
espera de las flechas con Ctrl. Sobre la fila `..` manda esta ubicación, no la
del padre: esa fila no es el operando de nada.

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

Un paso que no llega se rebobina: no te fuiste, así que el rastro se queda como
estaba. Vale tanto para el paso que **falla** como para el que **abandonas**
con Esc mientras lista: en los dos casos el panel sigue enseñando lo mismo, y
un rastro que diera el paso por bueno te mandaría «adelante» al directorio que
ya está en pantalla. Cuando el motivo es que el directorio **ya no está**,
además sale del rastro, de la rama de delante y del popup de historial, de modo
que la tecla no puede dejarte atrapado en un directorio que se ha demostrado
que no existe. Cualquier otro fallo lo conserva: un host caído o un directorio
que no puedes leer siguen siendo sitios, y pueden responder al siguiente
intento.

El paso que se para a preguntar por la clave desconocida de un host es el único
que ESPERA: ni se da ni se deshace hasta que respondes, porque confiar en la
clave reanuda esa misma navegación. Si confías, el paso se termina; si deniegas,
o si el paso reanudado falla, se rebobina como cualquier otro que no llegó.

# Elegir una unidad

{{cmd:pane.select-drive}} abre un picker de los volúmenes del host para el
panel **con foco**; {{cmd:pane.select-drive-left}} y
{{cmd:pane.select-drive-right}} abren el mismo picker para un **lado** de la
pantalla en su lugar — el panel que haya ahí dibujado, sin importar cuál tenga
el foco. `Alt+F1`/`Alt+F2` de Total Commander funcionan así desde Norton
Commander, y los dos presets que los importan mantienen el mismo reparto.
Enter manda ese panel al punto de montaje resaltado.

Cada fila muestra la etiqueta cuando el filesystem tiene una, el punto de
montaje, el tipo de filesystem, y espacio libre de total — un montaje que el
host no pudo consultar a tiempo aparece como desconocido en vez de como cero,
que se leería como lleno en lugar de sin respuesta. La lista es una foto
tomada al abrir el picker: no crece, no encoge ni vuelve a comprobar el
espacio libre mientras la miras, el mismo contrato que ya cumplen
{{cmd:pane.history}} y {{cmd:pane.hotlist}}. Una tecla dentro del picker
alterna entre la lista de cada día y todos los montajes del host, sistemas de
archivos de sistema incluidos, y el pie dice en cuál de los dos estás.

# Comparar los dos paneles

{{cmd:pane.compare-dirs}} responde a la pregunta para la que existe un gestor
de archivos ortodoxo: **¿son iguales estos dos árboles?** Recorre los dos
paneles a la vez y abre un panel de diferencias donde cada fila es un nombre,
visto desde los dos lados.

No se escribe nada. Esta tecla produce una respuesta y solo una respuesta: ni
copia, ni borra, ni propone un plan. Es también la forma honesta de comprobar
una transferencia recién terminada, que es la pregunta que la gente hace de
verdad después de cada copia.

Cada fila lleva dos marcas, y la segunda es la que merece la pena aprender. La
primera dice QUÉ se decidió: `=` igual, `#` distinto, `<` solo a la izquierda,
`>` solo a la derecha, `T` dos clases distintas bajo un mismo nombre, `A` un
emparejamiento ambiguo, `E` una fila que no se pudo leer. La segunda dice
CUÁNTO vale ese veredicto: `!` lo prueba, `~` lo sugiere, `?` significa que la
ubicación no pudo decirlo.

Esa segunda marca no es adorno. Una fila con `= ~` se llamó *igual* porque las
dos fechas coinciden, y dos ficheros con la misma fecha pueden tener bytes
distintos; una con `= !` la probó un hash, o un tamaño que zanjó la cuestión.
Un archivo comprimido no tiene una fecha de la que fiarse, y responde `?` en
vez de que se le invente algo — que es una respuesta de verdad, no un fallo.

La comparación no lee el contenido de los ficheros salvo que se lo pidas. Los
nombres, las clases, los tamaños y las fechas bastan para casi cualquier
pregunta, y hacer el hash de un terabyte por SFTP porque has pulsado una tecla
no bastaría.

`Tab` cambia el lado desde el que miras, y el pie dice cuál es. Nunca se
infiere de la fila: una fila que solo existe a la izquierda, mirada desde la
derecha, no tiene adónde ir y lo dice, en vez de llevarte en silencio al otro
lado. Hoy el lado decide dónde aterriza el `Enter`; actuar sobre una fila sin
salir del diff —verla, copiarla, borrarla— es trabajo de la spec siguiente, y
hasta entonces la forma de hacer cualquiera de esas cosas es pulsar `Enter` y
usar las teclas que ya conoces una vez allí. Los dígitos `1` a `5` esconden y
enseñan
categorías enteras — iguales, distintas, solo izquierda, solo derecha, y todo
lo que salió mal — y esconder una categoría jamás mueve lo que está
seleccionado. `Enter` deja el diff y te lleva a donde la fila seleccionada vive
de verdad, que es como se abre un directorio que solo existe en un lado: el
recorrido lo cuenta como UNA fila en vez de enumerar un subárbol cuya respuesta
ya conoce. `Esc` cancela una comparación que sigue en marcha, y cierra el panel
cuando ya no lo está.

# Comparar dos FICHEROS

{{cmd:pane.compare-files}} es la otra pregunta: **¿en qué se diferencian estos
dos ficheros?** Actúa sobre dos marcados en el panel con el foco, o sobre el
que hay bajo el cursor aquí y el que hay bajo el cursor en el otro. Dos, y no
se adivina: con tres marcados, con uno solo o con una carpeta de por medio, lo
dice en vez de comparar lo que no elegiste.

La diferencia la enseña otro programa, el que digas en `[ui] diff` —`meld %F`,
`vimdiff %F`, lo que uses—. Sin configurar nada es `diff -u`, y su salida se
queda en pantalla hasta que pulses una tecla. Los dos ficheros tienen que estar
en este sistema: a un programa externo no se le puede dar un `sftp://`, y eso
se dice, como en abrir y en editar.

# Sincronizar los dos paneles

{{cmd:pane.sync-dirs}} es la mitad que escribe. Planifica una sincronización de
un sentido —este panel sobre el otro—, te enseña todos los pasos que daría y no
hace absolutamente nada hasta que la apruebas. Dentro del panel de diferencias
lo mismo es `s`, y `m` planifica un **espejo**, que además borra del destino lo
que el origen no tiene. Allí el sentido lo decide el lado activo del propio
panel de diferencias, el que `Tab` cambia y el pie nombra — no el panel con el
foco. En los dos casos el título del plan lo deletrea con una flecha antes de
que apruebes nada. Letras peladas a propósito: una tecla de función con
modificador no sobrevive a una sesión de `tmux`, y un atajo documentado que no
llega nunca es peor que ninguno.

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

> 💡 Un directorio al que vuelves a menudo merece un favorito: el panel recuerda por dónde ha pasado, y los favoritos son comunes a los dos paneles.

> 💡 Cuando ya no queda rastro hacia atrás, la tecla lo dice. Una tecla que se calla es indistinguible de una rota.

Los paneles se pueden redimensionar y cerrar. {{cmd:layout.grow}} y
{{cmd:layout.shrink}} le dan o le quitan sitio al panel enfocado, y
{{cmd:layout.equalize}} los devuelve a todos al mismo tamaño.
{{cmd:layout.close-slot}} cierra el enfocado, y **se niega a cerrar el
último**: una pantalla sin ningún listado no es un layout, es un cuelgue con
bordes.

{{cmd:layout.focus-next}} y {{cmd:layout.focus-prev}} recorren los paneles.
Con dos hacen lo mismo que {{cmd:pane.switch}}; existen para cuando haya más.
{{cmd:layout.set-target}} fija cuál es el destino de una copia. Con dos
paneles el destino ya es el otro y no cambia nada — es para el día en que
haya más de dos y no se pueda desempatar solo.

{{cmd:layout.split-h}} parte el panel enfocado en dos lado a lado y
{{cmd:layout.split-v}} lo parte arriba y abajo. El panel nuevo nace en el
mismo directorio, ya lleno, y se queda con el foco: partir es pedir sitio para
trabajar en él. A partir de tres paneles, cuál es el destino de una copia deja
de ser obvio — para eso está {{cmd:layout.set-target}}, y el destino designado
se marca en el borde del panel.

{{cmd:layout.places}} abre a la izquierda un panel con tus unidades y tus
favoritos, y `Enter` sobre una fila lleva ahí al **listado enfocado**: es un
mando, no un panel con directorio propio. La segunda pulsación se lleva el
teclado al panel; la tercera lo cierra. Las unidades se piden al abrirlo y al
desplegar su sección, nunca por reloj: preguntarle el espacio libre a cada
filesystem cada pocos segundos se nota en un disco de red.

Un favorito cuya ruta ya no vale sale marcado con `!` y atenuado, no
desaparece — un favorito que se esconde solo es un fallo de configuración que
no puedes ver. El motivo lo dice la barra de estado al pulsarlo.

{{cmd:layout.preview}} abre a la derecha un visor que **sigue al cursor** del
listado activo: mover el cursor cambia lo que enseña, sin pulsar nada. Es el
mismo visor de {{cmd:pane.view}} —mismas teclas, mismos encodings, mismo hex—
metido en un hueco en vez de ocupar la pantalla.

Un directorio no se lee: la caja dice que lo es. Un fichero que no se puede
leer tampoco pregunta nada — el motivo se pinta dentro, porque un panel que
sigue al cursor no puede abrir un diálogo por cada tecla que bajas. Y un visor
acoplado que no se ve —detrás de una pestaña, o sin sitio— no lee NADA.

{{cmd:layout.processes}} abre un panel con una fila por tarea en marcha: su
barra, por dónde va, y cancelar la fila bajo el cursor. La franja de tareas del
pie no se va — el panel es lo que se abre para **actuar** sobre una tarea, no
para mirarla. Toma el teclado al abrirse y la segunda pulsación lo cierra: al
revés que el visor acoplado, y a propósito, porque lo abriste para pulsar algo
dentro.

No hay pausa. El protocolo tiene cancelar y nada más, y un control que no hace
lo que dice es peor que un control que falta.

{{cmd:layout.log}} abre el registro de esta sesión: lo que norte va anotando
mientras trabajas, en la propia terminal. Es lo que contesta «¿y por qué ha
fallado eso?» sin salir a buscar un fichero — una conexión que se cae deja en la
barra un «permiso denegado» que no dice nada, y aquí al lado está el motivo
exacto.

`e`, `w`, `i`, `d` y `t` eligen hasta qué nivel se enseña, de errores a todo; `/`
filtra por texto, y busca también en el nombre del módulo, que es media búsqueda
real. Las flechas y las páginas se despegan del final para que puedas leer
mientras siguen llegando líneas, y `Fin` vuelve a pegarse. `Esc` devuelve el
teclado sin cerrar el panel.

{{cmd:layout.disk-map}} abre el mapa de disco: de qué está hecho el directorio
que estás mirando, con un rectángulo por hijo y del tamaño que ocupa. Es la
respuesta a «¿en qué se me ha ido el sitio?», que un listado ordenado por tamaño
no contesta — ahí un directorio pesa lo que pesa su nodo, no lo que hay dentro.

Las flechas se mueven de rectángulo en rectángulo y {{cmd:nav.enter}} entra en el
elegido, que es como se baja hasta el que ocupa. Un clic hace lo mismo sobre el
rectángulo que pulses. `Esc` devuelve el teclado sin cerrar el panel.

Medir un árbol grande tarda, así que el mapa se va pintando mientras se mide y
dice cuándo ha terminado. Lo que no se pudo leer entero sale marcado con `≈` en
vez de contarse como cero: es una cota inferior y se declara, porque un
rectángulo pequeño que en realidad es enorme es peor que uno que admite no
saberlo. Y si el directorio tiene más hijos de los que caben, los que viajan son
los **más grandes** — los que un mapa existe para enseñar.

Pedir más detalle sube el nivel de verdad, no solo el filtro: los mensajes de
depuración no existen hasta que los pides, así que aparecen de ahí en adelante y
no hacia atrás. Bajarlo otra vez **no** deja de guardarlos, para que ir y volver
no te borre justo el rato que estabas mirando; el título dice qué se está
guardando cuando es más de lo que se enseña, y cerrar el panel lo devuelve a su
sitio. El panel guarda las últimas dos mil líneas y dice cuántas ha tirado.

Con `ntc --socket`, el daemon es **otro proceso**: los providers, el journal, la
política y el motivo por el que una conexión no llegó a abrirse están del otro
lado del socket, y el registro de esta terminal solo tiene lo de esta terminal.
Así que el panel pide también el suyo y los junta por hora, con un filete al
margen en las líneas que vienen de él. `s` recorre las tres vistas —esta
terminal, el daemon, las dos— y solo se ofrece cuando hay un daemon que sirva su
registro; uno que se compiló sin él lo dice, en vez de dejarte creer que la
mitad interesante no ocurre. Subir el nivel se lo pide también a él, y ahí hay
una diferencia que la barra de estado te avisa: su anillo es de **todos** sus
clientes, no baja nunca, y cerrar este panel tampoco lo baja. Las cuentas de
líneas perdidas van por separado —las de aquí y las suyas no significan lo mismo
y no se suman.

El detalle, en cambio, es **solo de norte**. Las bibliotecas que norte usa por
dentro para hablar con un servidor escriben, a ese nivel, el contenido de lo que
mandan — incluida tu contraseña antes de cifrarla. Así que sus mensajes se
quedan siempre en avisos y errores, que es lo que explica un fallo, y ninguna
tecla de este panel puede subirlos. El fichero al que apunta `norte paths` lleva
las de todos a ese nivel, y la misma cota vale al otro lado: el anillo del
daemon la aplica en el proceso que lo tiene, que es donde tiene que estar.

{{cmd:layout.metadata}} abre a la derecha un panel de detalles que también
sigue al cursor: nombre, clase, tamaño, cuándo se modificó y lo que el provider
ya hubiera dicho de la entrada. Para eso no lee **nada** —todo lo que enseña
vino con el listado—, así que bajar por un directorio con él abierto no cuesta
ni una petición.

{{cmd:layout.pick}} lista las disposiciones: las cinco que norte trae
—**orthodox** (los dos listados de siempre), **simple** (un listado),
**krusader** (dos listados y el sidebar de sitios), **explorer** (un listado,
sitios, visor acoplado y procesos) y **full** (todo a la vez)— y las que tengas
guardadas en `layouts/`, dentro de tu directorio de configuración. Cada fila
dibuja cómo quedaría la pantalla, sacado de la propia disposición y no de una
imagen guardada al lado, así que el dibujo no puede quedarse viejo.

El nombre de una disposición y el de un preset de teclas son dos ajustes
distintos. `krusader` es los dos, y elegir la **disposición** mueve paneles sin
cambiar ni una tecla; las teclas son `[keymap] preset`. El diálogo lo dice en
su pie, para que la coincidencia sea una comodidad y no una trampa.

Un fichero tuyo gana al de fábrica con el mismo nombre: `layouts/simple.toml`
es lo que carga `simple`. Borra el fichero y vuelve el original. `--layout
<nombre>` elige una para un solo arranque, sin tocar tu configuración.

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

# El árbol de directorios

{{cmd:pane.tree}} abre una columna a la izquierda con el árbol que cuelga del
directorio que estás mirando. `⏎` sobre una rama la despliega y manda el listado
ahí: ver qué hay dentro y estar dentro son la misma respuesta.

Se lee **por ramas**: abrir una lista ESE directorio y nada más. Un árbol que se
leyera entero tardaría minutos en una carpeta grande y mucho más en un remoto, y
lo que lleva dentro no cambia por plegarlo — así que plegar y volver a abrir no
cuesta otro viaje.

Solo salen directorios. Un árbol con ficheros sería un segundo listado peor que
el que ya tienes al lado; lo que este panel contesta es cómo está organizado
esto.

Tres pulsaciones, como el panel de sitios: la primera abre y se lleva el
teclado, la segunda lo vuelve a coger si lo habías soltado, la tercera cierra.
