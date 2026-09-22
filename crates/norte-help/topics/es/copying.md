+++
id = "copying"
title = "Copiar, mover, renombrar y borrar"
tags = ["doing"]
see_also = ["selection", "remote", "archives"]
commands = [
    "pane.copy",
    "pane.move",
    "pane.rename",
    "pane.rename-batch",
    "pane.checksum",
    "pane.checksum-verify",
    "pane.mkdir",
    "pane.delete",
    "pane.delete-permanent",
    "task.cancel",
    "task.pause",
    "task.retry",
    "task.retry",
    "dialog.overwrite",
    "dialog.skip",
    "dialog.rename",
    "dialog.newer",
]
context = [
    "dialog.confirm",
    "dialog.collision",
    "dialog.transfer-name",
    "dialog.transfer-dest",
    "dialog.mkdir",
]
+++
Marca lo que quieras en el panel con el foco y pulsa {{cmd:pane.copy}}. El
destino es el otro panel, tenga lo que tenga: un directorio local, un host
SSH, un bucket. Lo único que jamás puede ser destino es un archivo comprimido,
que solo sirve como origen; de eso va [[archives]].

{{cmd:pane.move}} mueve. Dentro de un mismo backend eso es un renombrado: no
se lee ni se escribe un solo byte, y da igual lo que ocupe aquello que estás
moviendo. Solo cuando los dos extremos son backends distintos —o sistemas de
ficheros distintos de esta máquina— degrada a copiar y luego borrar el origen.

{{cmd:pane.delete}} usa la papelera si el backend tiene una, y borra
definitivamente si no la tiene. {{cmd:pane.delete-permanent}} no usa la
papelera nunca. Los dos piden confirmación, y la confirmación dice cuál de las
dos cosas va a pasar.

# Poner nombres

La confirmación de una copia o un movimiento lleva el NOMBRE de destino, y es
editable. Déjalo y la entrada conserva el nombre que tiene; escribe encima y la
misma operación aterriza con el nombre que has tecleado. Copiar algo con otro
nombre no es una segunda función: es este campo.

{{cmd:pane.rename}} abre ese mismo diálogo con los dos extremos en el
directorio actual, que es lo que renombrar es: un movimiento que no va a ningún
sitio. Dentro de un mismo backend no cuesta nada, ocupe lo que ocupe.

{{cmd:pane.rename-batch}} renombra MUCHOS de una vez con una plantilla: `[N]`
es el nombre sin extensión, `[E]` la extensión, `[C]` un contador —`[C3]` lo
acolcha con ceros— y lo demás es texto tal cual. Actúa sobre lo marcado, o
sobre lo que hay bajo el cursor si no hay nada marcado, que es el operando de
siempre.

Lo que sale no se aplica: sale un **plan** —el nombre viejo y el nuevo, par a
par— con las colisiones ya señaladas, y no se toca nada hasta que lo aceptas.
Es exactamente la misma revisión, el mismo diario y el mismo deshacer que el
renombrado con IA, porque lo que hace segura la operación no es de dónde
salieron los nombres. Una plantilla que deja todo igual lo dice en vez de
enseñarte una lista vacía.

{{cmd:pane.mkdir}} pide un nombre y crea un directorio en el panel con el foco.
Es lo único de esta página que crea en vez de mover, y está aquí porque todo lo
que viene después —la colisión, la papelera, el deshacer— también le aplica.

> ⚠ Un nombre son bytes, y lo que se te enseña es una representación de ellos. Cuando el texto en pantalla no es lo que hay en disco —bytes que no decodifican, un override que reordena lo que lees— va MARCADO. Vuelve a teclear un nombre que traiga el carácter de reemplazo en vez de confirmarlo: confirmar llamaría al fichero como lo que viste, no como lo que había.

# Toda transferencia es una tarea

Una copia no es una pantalla congelada: informa del progreso, sigue trabajando
mientras navegas por otro sitio y {{cmd:task.cancel}} detiene una: la del
cursor si el panel de procesos tiene el foco, y si no la más reciente.

Lo que cancelar promete es por **fichero**: los bytes van a un fichero de
staging y el nombre de destino no aparece hasta que ese fichero está entero,
así que nunca queda uno a medias con el nombre que esperabas.

Cancelar un **árbol** es otra cosa. Los ficheros ya copiados se quedan donde
aterrizaron —cada uno completo, ninguno a medias—, pero el directorio está ahí
y está incompleto. Nadie lo recoge por ti.

> ⚠ Cancelar una copia HACIA almacenamiento de objetos es el caso que se queda ambiguo: el servidor puede terminar una copia que ya había empezado, así que el objeto puede aparecer después de que canceles, y las partes sueltas de un multipart siguen costando dinero hasta que las barra una regla de ciclo de vida.

# Pausar

{{cmd:task.pause}} pausa la misma tarea que se cancelaría, y si ya está
pausada la reanuda. La pausa es cooperativa, como la cancelación: una copia se
para al acabar el trozo que estaba escribiendo, con el fichero de staging
abierto, y sigue donde iba. Una copia que no tiene trozos —de servidor a
servidor, o la copia rápida del núcleo en un mismo disco— se para al acabar el
fichero en curso, y hasta entonces la barra dice «pausando…» y no «pausada».

Solo se pausan copiar, mover y borrar: son las que miran si se les ha pedido
parar. Sobre una búsqueda o una suma se te dice que no, en vez de aceptar una
pausa que no va a ocurrir.

Cancelar una tarea pausada funciona igual que cancelar una en marcha. Contra un
daemon anterior a la 0.82 pausar no se puede, y se dice.

Las copias reanudables se piden a propósito, desde la línea de comandos. Ahí
lo que deja una copia cancelada es un fichero con `.norte-partial` en el
nombre, inconfundible de un vistazo, y por donde sigue el siguiente intento:

```sh
norte cp --resume sftp://host/big.iso ./big.iso
```

# Repetir lo que falló

{{cmd:task.retry}} vuelve a lanzar la transferencia fallida más reciente con
las MISMAS opciones y el mismo verbo: una copia se repite como copia. Sirve
para lo que no fue culpa tuya —una red que se cayó, un destino que se llenó—
sin tener que rehacer la operación a mano. Si vuelve a chocar con un nombre
ocupado, se te vuelve a preguntar como la primera vez.

Y en la ventana, mientras algo está llegando a un panel, su borde inferior
lleva una línea fina que se llena: dice que ahí está entrando trabajo sin
robarle una fila al listado.

# Repetir lo que falló

{{cmd:task.retry}} vuelve a lanzar la transferencia fallida más reciente con
las MISMAS opciones y el mismo verbo: una copia se repite como copia. Sirve
para lo que no fue culpa tuya —una red que se cayó, un destino que se llenó—
sin tener que rehacer la operación a mano. Si vuelve a chocar con un nombre
ocupado, se te vuelve a preguntar como la primera vez.

Y en la ventana, mientras algo está llegando a un panel, su borde inferior
lleva una línea fina que se llena: dice que ahí está entrando trabajo sin
robarle una fila al listado.

# Cuando el nombre ya está ocupado

La primera colisión detiene la transferencia y pregunta. Tu respuesta no se
aplica solo a esa entrada: la operación entera se reenvía con esa política, de
modo que una sola respuesta gobierna todas las colisiones que queden.

| Respuesta    | Qué pasa                                                      |
|--------------|---------------------------------------------------------------|
| sobrescribir | se reemplaza lo que haya en el destino                        |
| saltar       | las entradas que chocan se quedan como están                  |
| renombrar    | la copia aterriza al lado: informe.txt pasa a informe (1).txt |
| más nuevo    | se reemplaza solo donde el origen sea más reciente            |

Esas cuatro son comandos como cualquier otro —{{cmd:dialog.overwrite}},
{{cmd:dialog.skip}}, {{cmd:dialog.rename}} y {{cmd:dialog.newer}}—, así que las
teclas son tuyas para reasignarlas y esta página dice las que elegiste.

Responde con la tecla que enseña el diálogo, no con una por defecto: no la
hay, porque a un diálogo que puede destruir datos no debería podérsele
contestar de carrerilla.

> ⚠ **más nuevo** no adivina. Si a cualquiera de los dos lados le falta una fecha utilizable, esa entrada se detiene y vuelve a preguntar, en vez de reemplazarse o saltarse por corazonada.

> ⚠ Las colisiones por mayúsculas se juzgan contra el **destino**, no contra el origen: `README` y `readme` conviven sin problema en Linux y caen sobre el mismo fichero en macOS o Windows, y quien decide es el destino.

Cuando no hay otro panel

Una disposición de un solo listado —`simple`— no tiene otro panel que sea el
destino, y una de tres tampoco: cuál sería no es evidente. En los dos casos
{{cmd:pane.copy}} **pregunta** en vez de fallar: abre un prompt con la
dirección de destino, prellenada con la de este panel y en la misma forma que
acepta `[[hotlist]]` (`file:///home/tu/trabajo`, `sftp://host/srv`). Edítale la
cola y pulsa ⏎; a partir de ahí es la confirmación de siempre, con las mismas
colisiones y el mismo undo.

Un destino jamás se adivina. Copiar hacia un panel que no tenías en la cabeza
es pérdida de datos silenciosa, y preguntar una vez cuesta menos que
descubrirlo después.

# Comprobar que llegó entero

Una copia que termina sin error dice que los bytes salieron y entraron. No dice
que sean los mismos: un disco que miente, una red que remienda mal, un
almacenamiento de objetos que reensambla un multipart. Para eso están las sumas.

{{cmd:pane.checksum}} calcula el sha256 de lo marcado —o de lo que hay bajo el
cursor, el operando de siempre— y enseña la lista. Es una tarea como cualquier
otra: informa del progreso, se cancela con {{cmd:task.cancel}} y no bloquea el
panel mientras trabaja. Al confirmar, la lista se copia al portapapeles en el
formato de `sha256sum` —`digest␣␣nombre`, una línea por fichero—, que es lo que
se pega en un `SHA256SUMS` y lo que entiende cualquier otra herramienta.

{{cmd:pane.checksum-verify}} hace el camino de vuelta: sobre un fichero de sumas
—el que esté bajo el cursor— lee sus líneas, calcula lo que hay de verdad en el
disco y enseña un veredicto por línea: **correcto**, **no cuadra**, **falta**,
**no es un fichero** o **nombre imposible aquí**. Son cinco y no dos porque se
arreglan de formas distintas. Los nombres se resuelven contra el directorio del
FICHERO DE SUMAS, no contra el del panel: un `SHA256SUMS` habla de lo que tiene
al lado.

Lo que no se entiende **se cuenta**. Una línea rota no tumba las demás, pero con
una sola que se caiga el resumen ya no puede decir «todos correctos»: la que se
cayó es justo la del nombre raro. Y un lote que se cancela a medias no se compara
con nada — decir «no cuadra» de un fichero que nadie llegó a leer sería peor que
no decir nada.

Un directorio no tiene suma, y pedirla no falla la operación: esa entrada sale
sin digest y lo dice. Sumar «un árbol» sería otra pregunta —un manifiesto, con
su formato y su orden— y contestarla a medias daría un número que no significa
nada comprobable.

> ⚠ Un lote se RECHAZA por encima de 4096 rutas en vez de recortarse. Una lista recortada en silencio se lee como «todo comprobado» sobre ficheros que nadie miró, y comprobar es justo para lo que esto existe.
