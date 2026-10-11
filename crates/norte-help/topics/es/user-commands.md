+++
id = "user-commands"
title = "Tus propios comandos en una tecla"
tags = ["doing"]
see_also = ["shell", "settings", "panes"]
commands = ["app.palette"]
context = ["dialog.user-command"]
+++
Tarde o temprano hay un programa que ejecutas sobre un archivo cada día:
optimizar una imagen, convertir un documento, abrir una carpeta en tu editor.
Un comando de usuario es ese programa, escrito una sola vez en
`commands.toml` y lanzado desde una tecla, desde el cuadro de búsqueda o
desde cualquiera de los dos frontends, sobre los archivos que marcaste o
sobre el archivo bajo el cursor.

## Declarar uno

Ponlo en `commands.toml`, junto a tu `norte.toml`:

```toml
[[command]]
id = "optimize-png"
label = "Optimizar PNG"
argv = ["oxipng", "-o", "4", "%F"]
```

Cada comando es una tabla `[[command]]`. Las claves:

- `id` (obligatoria): letras minúsculas, dígitos y guiones, 64 como máximo,
  empezando por una letra o un dígito. Es lo que enlazas a una tecla.
- `label` (obligatoria): el texto de su fila en el cuadro de búsqueda.
- `argv` (obligatoria): el programa y después sus argumentos.
- `description`: una línea que `F1` muestra sobre su fila en el cuadro de
  búsqueda.
- `cwd`: `"pane"` (por defecto, la carpeta del panel activo) o una ruta
  absoluta.
- `run_in`: `"background"` (por defecto) o `"terminal"`.
- `confirm`: `false` por defecto. Con `true`, norte pregunta antes de
  ejecutarlo.
- `refresh`: `true` por defecto. Cuando termina un comando en segundo plano,
  los paneles vuelven a listar sus carpetas.

## Qué va en `argv`

`argv` es un programa y sus argumentos, y norte ejecuta exactamente eso. No
hay shell: ni tuberías, ni comodines, ni `~`, ni `$VARIABLES`. El primer
elemento es el programa, se busca en tu `PATH` al ejecutar el comando y nunca
se expande. Si el programa no está, se avisa al ejecutar el comando y no al
cargar el archivo, para que un mismo archivo viaje entre máquinas.

Se rellenan tres códigos, y solo como elemento completo de `argv`:

- `%f` es el archivo bajo el cursor, o el primero de los marcados.
- `%F` son todos los archivos marcados, o el archivo bajo el cursor cuando no
  hay ninguno marcado. Se convierte en un argumento por archivo, así que los
  nombres con espacios o bytes raros llegan intactos.
- `%d` es la carpeta del panel activo.

Cualquier otro `%x` como elemento completo es un error al cargar el archivo.
Un comando que usa `%f` o `%F` y no tiene sobre qué trabajar se rechaza con
su motivo; uno que solo usa `%d` se ejecuta sin selección.

Cuando sí quieras una shell, escríbela tú y pasa los archivos como
argumentos, nunca pegados dentro del script:

```toml
[[command]]
id = "name-of-first"
label = "Mostrar el primer nombre"
argv = ["sh", "-c", 'echo "$(basename "$1")"', "sh", "%F"]
```

Aquí `$1` es el primer archivo y `"$@"` son todos. Con muchísimos archivos la
línea de comandos puede superar lo que el sistema acepta; norte entonces lo
rechaza indicando cuántos son y no ejecuta nada. Desde `sh -c`, `xargs` es la
manera de evitarlo.

## Ponerlo en una tecla

Enlázalo en `keymap.toml`:

```toml
[pane]
prepend_keymap = [{ on = ["ctrl+alt+o"], run = "user:optimize-png" }]
```

O encuéntralo sin tecla: abre el cuadro de búsqueda con {{cmd:app.palette}},
o escribe `>` en él, y mira en la categoría Usuario. `F1` sobre una fila
muestra su `description`. Un enlace a un `id` que `commands.toml` no define
no es fatal: el archivo se carga, y la barra de estado lo avisa al arrancar y
al cambiar de perfil en la ventana, y al arrancar y en cada recarga en el
terminal. `norte doctor` lo muestra como `user:<id>`, y al pulsar la tecla se
dice que ese comando no existe.

## Dónde se ejecuta y adónde va su salida

En segundo plano (lo habitual) el programa va por su cuenta, sin acceso a tu
teclado. Su salida va al panel de registro, y cómo terminó va a la barra de
estado. Un comando en segundo plano no puede preguntarte nada: un programa
que pide una contraseña o una confirmación, como `ssh`, `sudo` o `gpg`, se
detiene en cuanto intenta leer la respuesta. Para los programas que
preguntan, usa `run_in = "terminal"`.

Si al cerrar norte un comando en segundo plano sigue escribiendo salida, se
detiene en su siguiente escritura; para trabajos largos usa `run_in =
"terminal"` o redirige su salida dentro de `sh -c`. Un comando que nunca
termina lo detienes tú.

Con `run_in = "terminal"`, norte ejecuta el propio programa, sin shell de por
medio, como una entrada nueva del panel de terminal, para que lo veas y
puedas contestarle. La entrada se cierra cuando el programa termina bien, y
se queda abierta para que la leas cuando falla.

Los comandos de usuario solo se ejecutan en carpetas locales. En un panel
remoto se rechazan con su motivo. Una ventana abierta en solo lectura tampoco
los ejecuta.

norte no puede deshacer lo que hace un comando de usuario: el programa es
tuyo, y sus cambios en tus archivos no quedan en el historial de norte. Pon
`confirm = true` en los comandos que quieras pensarte dos veces.

## Qué archivos se leen

`commands.toml` se lee de todas las capas de configuración salvo la del
proyecto: la del sistema, la de usuario y la del perfil activo. El
`.norte/commands.toml` de un proyecto, y cualquier enlace `user:` en el mapa
de teclas de un proyecto, se ignoran: una carpeta que te acabas de descargar
no debe decidir qué se ejecuta con tus permisos. Un archivo que no se puede
leer, o con un comando no válido, es un error de configuración, y norte
conserva la configuración que ya tenía.

El terminal recarga `commands.toml` al guardarlo. La ventana lo lee al
cambiar de perfil, al cambiar los ajustes o al reiniciar; no vigila el
archivo.

## Los agentes no los ven nunca

Un comando de usuario ejecuta un programa cualquiera con tus permisos, así que
no es una herramienta del puente de agentes y una sesión de agente no puede
llamarlo. En la página de agentes está lo que un agente sí puede hacer.
