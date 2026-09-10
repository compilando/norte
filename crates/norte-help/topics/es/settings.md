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
