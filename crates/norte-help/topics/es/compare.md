+++
id = "compare"
title = "Comparar"
tags = ["doing"]
see_also = ["panes", "sync", "copying"]
commands = ["pane.compare-dirs", "pane.compare-files"]
+++
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
