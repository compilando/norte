+++
id = "plugins"
title = "Extensiones"
tags = ["extensions"]
see_also = ["settings", "remote", "agents"]
commands = ["app.extensions"]
context = ["dialog.trust-lua"]
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
| decorator   | un badge en las filas de un listado                    |
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

> 💡 `norte doctor` informa de qué le pasa a una extensión instalada: un manifiesto que no parsea, un digest que ya no cuadra, una página de ayuda por encima del tope de tamaño.

# Un proyecto que trae su propio script

Un directorio puede llevar un `init.lua` —un script, no un ajuste— y norte no
lo ejecutará hasta que tú lo digas. La pregunta sale la primera vez que
aterrizas ahí, y contestarla es una tecla.

La decisión se recuerda para **el contenido de ese fichero**, no para su ruta.
Edita el script y se te vuelve a preguntar, porque aprobar un script no es un
cheque en blanco para lo que ese nombre guarde más tarde. Lo que se evalúa son
los bytes que se leyeron cuando se te preguntó —jamás una relectura posterior,
que sería la ventana por la que otro script se colaría entre tu respuesta y la
ejecución.

La configuración de un directorio de proyecto sigue la misma regla y está en
[[settings]].
