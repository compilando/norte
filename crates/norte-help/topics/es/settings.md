+++
id = "settings"
title = "Ajustes y temas"
tags = ["basics"]
see_also = ["appearance", "dialogs", "mouse", "help"]
commands = ["app.settings", "app.theme"]
+++
{{cmd:app.settings}} abre la pantalla de ajustes: todas las opciones de norte,
con su valor actual y una línea que dice para qué sirven. Cambiar una la
escribe en `norte.toml`.

Ese fichero es la interfaz de verdad; la pantalla es una forma de editarlo sin
acordarse de los nombres de las claves. Editarlo con un editor de texto está
igual de soportado, y guardar se recoge sin reiniciar.

# Dónde vive el fichero

Tres capas, cada una pisando a la anterior:

| Capa     | Dónde                          | Cuándo aplica                |
|----------|--------------------------------|------------------------------|
| sistema  | `/etc/norte`                   | siempre                      |
| usuario  | `~/.config/norte`              | siempre                      |
| proyecto | `./.norte` junto a tu trabajo  | solo después de darle confianza |

Una capa de proyecto es un fichero de un directorio que quizá acabas de clonar,
así que no hace nada hasta que tú lo digas. La misma cautela vale para el
`init.lua` de un proyecto, que es un script y no un ajuste.

La pantalla de ajustes escribe en tu capa de USUARIO. Un valor que pisa una
capa de proyecto en la que confías sigue pisado después de cambiarlo: la
pantalla escribió lo que le pediste, y el fichero más específico sigue ganando.

> 💡 Unas pocas opciones solo hacen efecto al reiniciar —una fuente, un idioma—. Su fila lo dice, en vez de fingir que el cambio aterrizó.

# Moverse por la pantalla

Los ajustes están repartidos en secciones: apariencia, paneles y listado,
abrir con, teclado y ratón, comportamiento, plugins, y dónde vive cada cosa.
El rótulo de la sección en la que estás se queda clavado arriba mientras te
desplazas, y a la izquierda hay un índice con cuántas opciones se ven de cada
una — una sección que tu búsqueda vació sigue en el índice, apagada.

`tab` pasa el teclado al índice y de vuelta a la lista. Con el teclado en el
índice, las flechas cambian de **sección** y la lista sigue; el cursor del
lado que no tiene el teclado se pinta apagado, para que siempre se vea dónde
estás sin dudar cuál de los dos manda.

| Tecla     | Qué hace                                            |
|-----------|-----------------------------------------------------|
| tab       | cambia de lado: índice ↔ lista                      |
| [ y ]     | sección anterior / siguiente, saltándose las vacías |
| ctrl+r    | restablecer la opción del cursor                    |
| ctrl+k    | el editor de atajos                                 |

Escribir filtra. Dos operadores estrechan más: `@modified` deja solo lo que no
está en su valor de fábrica, y `@section:apariencia` (o `@section:appearance`,
que vale igual) se queda con una sección. Se combinan entre ellos y con el
texto. Una arroba que no abre operador es texto normal.

En la ventana, el índice se pincha y el buscador es una caja de texto.

# La ventana tiene controles

Lo que en la terminal es una palabra que gira con Intro, en la ventana es el
control que le toca: un interruptor para lo que se enciende y se apaga, un
desplegable para una lista —con los temas que tengas instalados dentro—, y
un campo numérico con sus topes. El texto y las líneas de órdenes se
escriben en la fila y se guardan al salir del campo o con Intro; `esc` deja
el campo como estaba.

Cada ajuste enseña su descripción siempre, y una barra a la izquierda marca
lo que no está en su valor de fábrica. Un campo vacío enseña ese valor de
fábrica: si no enseña nada es que norte no fija ninguno y manda el sistema.

Lo que un ajuste admite lo decide el mismo catálogo en las dos superficies,
y lo valida el mismo editor: un número fuera de rango se rechaza igual
tecleado en la ventana que en la terminal.

# Restablecer, y lo que no puede hacer

Un punto delante del nombre significa «esto no es el valor de fábrica».
`ctrl+r` —o el botón de la fila, en la ventana— **quita la clave de tu
fichero**, que no es lo mismo que poner el valor por defecto: si una capa de
abajo fija la misma clave, el valor cambia y sigue sin ser el de fábrica. El
punto se queda encendido y la barra te lo dice, en vez de dejarte creyendo
que no funcionó.

# Temas

{{cmd:app.theme}} lista los temas y previsualiza el resaltado según te mueves:
lo que estás mirando mientras eliges es lo que estás eligiendo. Confirmar lo
fija y lo escribe; cancelar devuelve el que tenías.

El selector lista los temas que vienen con norte. Un tema tuyo es un fichero
TOML, y `theme` en `[ui]` acepta una ruta igual que acepta un nombre; lo que no
hará es salir en la lista, porque la lista es lo que va incrustado.

Cada superficie toma sus colores del tema vigente: los paneles, los diálogos,
esta ayuda. Una página que ignorara tu tema sería la única pantalla que no
parece del programa.

# Los dos ajustes que la gente busca primero

- **Ratón.** `mouse = false` en `[ui]` le devuelve al terminal su propia selección. En [[mouse]] está lo que cuesta la captura y el truco con Shift, que no necesita ajuste ninguno.
- **Confirmar al salir.** `auto` pregunta solo si hay trabajo pendiente, `always` pregunta siempre, `never` cierra directamente. De eso va [[dialogs]].
