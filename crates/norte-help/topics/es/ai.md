+++
id = "ai"
title = "Renombrado con IA y búsqueda semántica"
tags = ["doing"]
see_also = ["finding", "agents", "settings"]
commands = ["pane.ai-rename", "pane.semantic-search"]
context = ["dialog.ai-rename", "dialog.semantic-search"]
+++
Las dos están APAGADAS hasta que las enciendes, y las dos van en dos pasos:
pides, te dan algo que mirar, y solo entonces pasa algo. Esa forma es lo
importante: un modelo es un motor de sugerencias, y una sugerencia que no
puedes inspeccionar antes de que aterrice es simplemente una acción que no
tomaste tú.

# Renombrar un directorio entero

{{cmd:pane.ai-rename}} pide una instrucción en tus palabras —*numéralos por
fecha*, *quítales el sufijo de descarga*— y vuelve con un **plan**: el nombre
viejo y el nombre propuesto, par a par.

A esas alturas no se ha renombrado nada. Aceptar el plan envía los movimientos,
cada uno una operación normal con su camino de confirmación, su entrada de
diario y su deshacer. Descartarlo no cuesta nada y deja el directorio intacto.
Un plan que no propone nada lo dice, en vez de enseñarte una lista vacía que
tengas que interpretar.

> ⚠ Lo que sale de tu máquina es la instrucción y los nombres de ese directorio. El contenido de los ficheros no; pero un nombre de fichero suele ser lo más revelador de los dos.

El plan se valida antes de que lo veas: los nombres que propone son segmentos
sueltos, no rutas, así que un plan no puede salirse del directorio por el que
se preguntó — y el que lo intenta se rechaza entero, jamás a medias.

# Buscar por significado

{{cmd:pane.semantic-search}} acepta una pregunta en vez de un patrón y contesta
desde el índice local: lo que se haya indexado y solo eso. Los hits vuelven
ordenados por afinidad, y elegir uno te lleva a donde vive.

Complementa a la búsqueda de siempre, no la sustituye: `*.rs` es trabajo de un
glob, y *aquello del backoff de reintentos* no lo es. De eso va [[finding]].

# Qué está encendido, y qué no sale nunca

Tres ajustes, y no son la misma perilla:

| Ajuste            | Qué decide                                              |
|-------------------|---------------------------------------------------------|
| activado          | apagado por defecto: sin él la IA no hace nada          |
| solo local        | rechaza cualquier proveedor que no esté en esta máquina |
| prefijos negados  | subárboles cuyos nombres y contenidos no llegan a un proveedor |

*Solo local* es una barrera dura en el core, no una cortesía del proveedor: uno
que se declare local sin serlo se rechaza ahí. *Prefijos negados* compara
segmentos de ruta y no prefijos de cadena — negar `~/.ssh` no niega de rebote
`~/.sshfs`, y lo que cuelga de un subárbol negado queda negado también.

Los tres viven en `norte.toml` bajo `[ai]`; de eso va [[settings]]. Un rechazo
dice cuál de los tres paró la petición, porque «la IA falló» no es algo sobre
lo que puedas actuar.

> 💡 Esto no es lo mismo que un agente pilotando norte desde fuera. Aquello es otra puerta, con sus propias aprobaciones y su propio deshacer, y está en [[agents]].
