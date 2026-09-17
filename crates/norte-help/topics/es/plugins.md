+++
id = "plugins"
title = "Extensiones"
tags = ["extensions"]
see_also = ["settings", "remote", "agents"]
commands = ["app.extensions"]
context = ["dialog.trust-lua", "dialog.plugin-approval"]
+++
{{cmd:app.extensions}} lista lo que hay instalado y, de cada cosa, dos hechos
separados: si la has APROBADO y si está ACTIVADA. Nada corre hasta que la
apruebas, y aprobar no es lo mismo que encender: puedes aprobar una extensión y
dejarla apagada, o apagar una sin retirarle la aprobación.

Una extensión es WebAssembly. No ve tu sistema de ficheros, no abre un socket
ni ejecuta un programa por su cuenta: tiene exactamente las capacidades que
pide su manifiesto, y eso es lo que apruebas al aprobarla. No hay forma de que
un plugin ejecute nada en absoluto, y por eso darle un fichero a un programa
externo es configuración; de eso va [[viewer]].

Ese aislamiento sostiene peso, no decora: el backend FTP de [[remote]] es un
plugin, y llega a la red solo por un socket que le abre el host. Un protocolo
entero vive dentro de los mismos muros que una extensión de una línea.

Lo que un manifiesto puede declarar, y por tanto lo que concede aprobarlo:

| Clase       | Qué añade                                              |
|-------------|--------------------------------------------------------|
| provider    | un backend, direccionado por su propio scheme de URL   |
| previewer   | una forma de pintar un fichero en el visor             |
| command     | un verbo en la paleta                                  |
| decorator   | un icono a la izquierda del nombre, o un badge a la derecha, en cada fila |
| columns     | un valor por entrada en el listado                     |

# Sus páginas, y cómo leerlas

Una extensión puede traer su propia página de ayuda, y aparece en este grupo,
al lado de esta. Todas dicen en su cara que las escribió un plugin, y esa línea
está tanto si el plugin declaró algo como si no: una página que pudiera pasar
por prosa de norte es una página que podría decirte que aprobarla es seguro.

El texto es de terceros de principio a fin y se trata como tal: acotado,
decodificado y enmascarado antes de llegar a tu pantalla, así que un override
bidi en un titular no puede reordenar lo que lees. La fila de una extensión que
no está aprobada y activada sale atenuada y dice cuál de las dos cosas falta,
que es justo la respuesta que buscabas si estás leyendo esa página para decidir
si la enciendes.

Un provider es la única clase que responde a una URL: instala uno que declare
`webdav`, apruébalo y enciéndelo, y `webdav://host` se abre a través de él. El
scheme que reclama sale entre sus capacidades como `provider:webdav`, porque
eso es lo que concede aprobarlo. Los schemes que norte sirve por sí mismo
—`file`, `sftp`, `ftp`, `s3`— no los puede reclamar una extensión, así que
aprobar una jamás la pone delante de un backend propio.

La pantalla es la misma en el terminal y en la ventana: la lista a la
izquierda y, a la derecha, la extensión elegida con su estado, sus
capacidades y una fila de botones: encender o apagar, aprobar o revocar,
ajustes, desinstalar, ayuda. Cada botón hace exactamente lo que hace su
tecla; las teclas están en el pie. Pulsar una fila la elige, y pulsar la fila
ya elegida abre sus ajustes, como `Intro`.

Sin ratón: `Tab` mueve el foco entre la lista y los botones, uno a uno, y al
llegar al último vuelve a la lista. El botón que lo tiene se enciende, y el
cursor de la lista se apaga para que no haya dudas sobre a dónde van las
teclas; `Intro` dispara el botón enfocado. Mover el cursor por la lista
devuelve el foco a ella, porque los botones son los de la extensión elegida.
`Tab` no está en el pie —ya va lleno— y no hace nada si la ventana es
demasiado estrecha para pintar la ficha, que es cuando no hay ningún botón a
dónde ir.

Las extensiones entran desde la línea de comandos: `norte plugin install
<dir>` trae una sin aprobar, y `norte plugin list` enseña los mismos dos hechos
que esta pantalla. Salen por cualquiera de los dos lados. La tecla de quitar
(y, en la ventana, el botón) de esta pantalla pregunta antes, porque
desinstalar borra los ficheros de la extensión **y su aprobación** —un plugin
instalado después con el mismo id empieza de cero. `norte plugin uninstall
<id>` hace lo mismo sin preguntar, y un daemon que ya está en marcha no se
entera hasta que reinicia.

> 💡 `norte doctor` informa de qué le pasa a una extensión instalada: un manifiesto que no parsea, un digest que ya no cuadra, una página de ayuda por encima del tope de tamaño.

# Un proyecto que trae su propio script

Un directorio puede llevar un `init.lua` —un script, no un ajuste— y norte no
lo ejecutará hasta que tú lo digas. La pregunta sale la primera vez que
aterrizas ahí, y contestarla es una tecla.

Los scripts Lua solo se ejecutan en el frontend de terminal, `ntc`. La ventana
no los ejecuta, y una tecla ligada a un comando `lua:` dice allí que no está
disponible.

La decisión se recuerda para **el contenido de ese fichero**, no para su ruta.
Edita el script y se te vuelve a preguntar, porque aprobar un script no es un
cheque en blanco para lo que ese nombre guarde más tarde. Lo que se evalúa son
los bytes que se leyeron cuando se te preguntó —jamás una relectura posterior,
que sería la ventana por la que otro script se colaría entre tu respuesta y la
ejecución.

La configuración de un directorio de proyecto sigue la misma regla y está en
[[settings]].

## Aprobar una extensión

Aprobar es LA decisión de seguridad de este sistema: una extensión aprobada
actúa en tu nombre con las capacidades que declara —leer bajo una ubicación,
salir a la red—. Por eso norte **pregunta** y la pregunta las
enumera una por línea, cada una marcada aparte si su texto no es lo que
parece. `Enter` no concede: hace falta la tecla de aprobar, igual que con la
operación de un agente.

**Revocar no pregunta**, y encender lo que no está aprobado no se puede.
Apagar sí se puede siempre, aunque la aprobación se haya revocado por el
camino: apagar va en la dirección segura.

Tras conceder o revocar, la lista se vuelve a pedir al núcleo. Lo que ves es
lo que el núcleo cree, no lo que esta pantalla esperaba que pasara.

## Una extensión que pinta un panel entero

Algunas extensiones aportan un PANEL: un hueco de la pantalla cuyo contenido
describen ellas. Sale en el selector de disposiciones como cualquier otro
panel, y lo colocas donde quieras — al lado de un listado, debajo, o en una
pestaña.

La extensión no dibuja. Describe líneas de texto, y norte las pinta dentro de
un marco suyo, con su título y su borde de foco. Una extensión no puede pintar
ese marco, ni escribir en ese título, ni hacer que su panel parezca otro.

Un panel puede ofrecer zonas pulsables. Una zona ejecuta un comando de norte,
nunca algo propio de la extensión, y solo del conjunto pequeño que cualquier
panel puede nombrar: moverse entre paneles, abrir o cerrar otro, cambiar el
tamaño. La extensión elige la etiqueta y el comando, y nada ata la una al
otro — así que norte rehúsa cualquier cosa que no te dejaría hacer con una
tecla mientras ese panel tiene el teclado.

Lo que un panel recuerda entre repintados es un dato suyo que norte guarda y
le devuelve tal cual, sin leerlo. El permiso de leer el disco no va en eso: se
acuña para cada repintado y se retira al acabar. Un panel cuya extensión
apagues deja de existir para la disposición, y uno que deje de contestar se
queda con lo último que pintó en vez de parpadear en blanco.
