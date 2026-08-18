+++
id = "panes"
title = "Dos paneles, un destino"
tags = ["basics"]
see_also = ["selection", "copying"]
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
    "pane.pull",
    "pane.swap",
    "nav.back",
    "nav.forward",
    "pane.select-drive",
    "pane.select-drive-left",
    "pane.select-drive-right",
    "pane.compare-dirs",
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
    "layout.pick",

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
