+++
id = "viewer"
title = "Leer un fichero sin salir"
tags = ["doing"]
see_also = ["panes", "archives", "mouse"]
commands = [
    "pane.view",
    "viewer.close",
    "viewer.up",
    "viewer.down",
    "viewer.page-up",
    "viewer.page-down",
    "viewer.top",
    "viewer.bottom",
    "viewer.encoding",
    "viewer.encoding-auto",
    "viewer.hex",
    "pane.open",
    "pane.edit",
    "pane.edit-new",
]
context = ["viewer"]
+++
{{cmd:pane.view}} abre la entrada bajo el cursor y {{cmd:viewer.close}} la
cierra. No se escribe nada, no se descomprime nada y el fichero jamás se abre
para escritura: el visor lee.

Y lo que lee son los **primeros 256 KiB**, no el fichero. Por eso un log de
40 GB se abre tan rápido como una nota, y por eso la barra de estado lo dice
cuando hay más: lo que tienes delante es la cabecera, y no finge otra cosa.

{{cmd:viewer.up}} y {{cmd:viewer.down}} mueven una línea,
{{cmd:viewer.page-up}} y {{cmd:viewer.page-down}} una pantalla, y
{{cmd:viewer.top}} y {{cmd:viewer.bottom}} van a los extremos de lo leído.

Las mismas teclas valen para un fichero dentro de un `.zip` o al otro lado de
SFTP. El panel sostiene una ubicación, el visor lee lo que esa ubicación le dé,
y ninguno de los dos tiene un caso especial por backend; de eso va
[[archives]].

# Que sea texto es una decisión, no un hecho

Un fichero son bytes. Si esos bytes son texto, y en qué codificación, se
DETECTA —mirando los bytes, jamás la extensión— y la respuesta está en la barra
de estado.

{{cmd:viewer.encoding}} lleva la contraria al detector: cicla los candidatos
plausibles y recarga los mismos bytes como UTF-8, como alguna de las
codificaciones de 8 bits, como UTF-16. {{cmd:viewer.encoding-auto}} le devuelve
la decisión.

> 💡 Recargar no cambia nada en disco. Decodificar es una lectura de unos bytes que no se mueven, así que una hipótesis equivocada cuesta una pantalla de mojibake y una tecla.

Dos cosas se ven siempre sin pedirlas: los bytes que no decodifican llegan como
`�` en vez de desaparecer, y los caracteres de control, los overrides bidi y
los separadores invisibles se enmascaran antes de pintarse. Un terminal que
ejecuta lo que muestra es un terminal que un fichero puede pilotar, así que
aquí no se pinta nada en crudo.

# Cuando no es texto en absoluto

{{cmd:viewer.hex}} cambia al volcado hexadecimal: offset, dieciséis bytes y la
columna imprimible al lado. Es la vista honesta de lo que nunca fue texto, y es
donde aterrizas cuando el detector dice binario.

Las imágenes se reconocen por sus bytes mágicos —PNG, JPEG, GIF, BMP, WebP—,
otra vez por contenido y no por nombre. El frontend gráfico las pinta; el
terminal enseña el hex, porque es lo que un terminal puede enseñar sin mentir.

Una extensión también puede aportar una vista previa: un plugin que entiende un
formato lo convierte en texto o en líneas con estilo, y su salida va acotada y
enmascarada como cualquier otro texto de terceros. Un plugin que falla, que
está desactivado o que tarda demasiado no bloquea el fichero: te quedas con la
vista cruda, que es la que ibas a tener de todos modos.

# Darle el fichero a otro programa

{{cmd:pane.open}} no usa el visor: lanza un programa externo sobre la entrada
bajo el cursor, elegido por `openers.toml` según el mimetype del fichero y este
sistema operativo. `bat` para código, `xdg-open` para un PDF, lo que tú pongas
ahí.

Es el único comando de esta página que se sale de norte. Los openers son
CONFIGURACIÓN y no plugins, a propósito: un plugin WebAssembly no tiene forma
de ejecutar nada, así que esta es la única puerta de salida, y es una que abres
escribiendo tú un fichero.

> ⚠ Lo que lances corre con TUS permisos, fuera de toda política que norte aplique. Norte entrega la ruta y se aparta; lo que el programa haga con ella lo decide él.

Mientras un programa externo tiene el terminal, norte no lo tiene: lo recupera
cuando ese programa termina, y un programa lanzado desde aquí nunca hereda un
terminal en modo ratón. De eso va [[mouse]].

# Editar

{{cmd:pane.edit}} abre lo que hay bajo el cursor **en tu editor**: el de
`$VISUAL`, o el de `$EDITOR`, o `vi` si no has puesto ninguno. norte no trae
editor propio y no piensa traerlo — lo suyo es mover ficheros, y el que ya usas
sabe más de editar que cualquier cosa que cupiera aquí.

Mientras el editor está delante, norte se aparta: le devuelve la terminal
entera, igual que con {{cmd:app.terminal}}. Al salir del editor vuelves a los
paneles y el listado se recarga, así que lo que hayas guardado ya se ve.

{{cmd:pane.edit-new}} abre el editor con un buffer VACÍO en el directorio que
estás mirando. El nombre lo pides al guardar, que es donde tu editor sabe
preguntarlo.

Dos cosas que no hace, y las dos a propósito: no edita una carpeta (para entrar
está `⏎`) y no edita en un panel remoto. Un editor abre un fichero del sistema;
bajarlo, editarlo y volver a subirlo es otra cosa —con su conflicto y su
reversa— y norte prefiere decírtelo a hacerlo a medias.
