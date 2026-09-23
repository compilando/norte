+++
id = "remote"
title = "SFTP, FTP y almacenamiento de objetos"
tags = ["remote"]
see_also = ["copying", "archives"]
commands = [
    "pane.hotlist",
    "pane.history",
    "pane.refresh",
    "dialog.add",
    "dialog.remove",

    "pane.connect",
    "pane.disconnect",]
context = ["dialog.trust-host", "dialog.ask-secret"]
+++
Un panel sostiene un sitio remoto igual que sostiene un directorio. La
dirección es una URL, y su esquema dice quién contesta:

| Esquema | A dónde llega                               | Lo sirve             |
|---------|---------------------------------------------|----------------------|
| file    | esta máquina                                | el propio norte      |
| sftp    | transferencia de ficheros sobre SSH         | el propio norte      |
| ftp     | FTP a secas                                 | un plugin aislado    |
| s3      | almacenamiento de objetos compatible con S3 | el propio norte      |

El FTP es la excepción: lo sirve un plugin WebAssembly que viaja dentro de
norte y que solo toca la red por un socket que le abre el anfitrión, hacia una
dirección que el anfitrión ha resuelto y filtrado antes. Es un sitio a
propósito para el más viejo y menos fiable de los cuatro.

# Cómo llegar

Un remoto vive en tus favoritos. Añádelo a `norte.toml`:

```toml
[[hotlist]]
name = "trabajo"
path = "sftp://tu@host/srv/datos"
```

A partir de ahí {{cmd:pane.hotlist}} abre la lista y al confirmar el panel
viaja allí. {{cmd:pane.history}} te devuelve a donde el panel ya ha estado
durante esta sesión. Desde ese momento todos los comandos de esta ayuda
funcionan igual — {{cmd:pane.refresh}} sobre todo, porque un directorio remoto
no se vigila y no se entera solo de que algo ha cambiado.

No hace falta editar el fichero para mantener esa lista: dentro del popup de
favoritos, {{cmd:dialog.add}} añade la ubicación actual del panel con el nombre
que teclees y {{cmd:dialog.remove}} quita la resaltada. Los dos escriben en
`norte.toml`, que es la misma lista que habrías editado a mano. El popup de
historial no admite ninguno de los dos: no hay nada que nombrar en un sitio por
el que simplemente pasaste, ni nada que borrar de un registro de esta sesión.

Un favorito es solo una dirección. Cómo se autentica una conexión, y qué se le
permite hacer, vive aparte en `connections.toml`: compartir un favorito no
regala más que una ruta.

La primera vez que llegas a un host SSH desconocido, norte te enseña su huella
y pregunta. Compárala con el host por otro canal antes de aceptarla: ese
diálogo es el único momento en que alguien puede notar una máquina en medio, y
a propósito no tiene respuesta por defecto.

# Los secretos

`connections.toml` guarda referencias, jamás secretos. Cuando una conexión
necesita uno, se busca en tres sitios y por este orden: la variable de entorno
`NORTE_SECRET_` de esa conexión, después el **llavero del sistema** y por
último un fichero `secrets.age` cifrado. Gana el primero que acierte, así que
una máquina sin llavero —un servidor, un contenedor— sigue funcionando con los
otros dos.

Si ninguno de los tres lo tiene, la conexión falla — que es lo correcto en una
máquina sin nadie delante. Cuando sí hay alguien, añade `secret = "prompt"` a
la entrada y norte lo PIDE en vez de fallar: un diálogo que no enseña lo que
tecleas y que solo aparece cuando los tres sitios de arriba han quedado
vacíos. El diálogo dice el nombre de la conexión **y a dónde se conecta**, que
es lo que hace la pregunta contestable: el nombre lo eligió el fichero, y un
fichero se puede haber editado.

Solo funciona con `auth = "password"` y `auth = "access-key"`. Con `agent` no
hay secreto que pedir, y con `key` el secreto es la contraseña de la clave,
donde «vacío» y «no hay» son lo mismo — preguntar ahí sacaría un diálogo cada
vez que usas una clave sin cifrar. `norte doctor` te avisa si has puesto la
clave donde no hace nada.

Lo que escribas vive en memoria mientras el daemon siga en pie, y no se
escribe en el llavero, ni en `secrets.age`, ni en `connections.toml`. Al parar
norte desaparece y la próxima sesión vuelve a preguntar; para no teclearla cada
vez, la variable de entorno o el llavero siguen siendo el sitio. Si te
equivocas al teclearla, no te quedas atrapado: cuando el servidor la rechaza,
norte la olvida y te vuelve a preguntar.

El access key id de S3 no está en esa lista, porque no es un secreto: es un
identificador, y va en claro en `connections.toml`. La secret access key que
lo acompaña sí pasa por el resolver.

Por lo mismo la contraseña no va nunca en la URL: una dirección tipo
`sftp://usuario:clave@host` se rechaza en vez de aceptarse sin decir nada,
porque una URL acaba en el historial, en los logs y en pantalla.

Las claves SSH son ed25519. Una clave RSA se rechaza, porque firmar con ella
pasa por una debilidad de temporización conocida en la biblioteca que usa
norte. Cuando un servidor te da una clave RSA y nada más, añade
`allow_rsa = true` a la entrada de esa conexión. Solo esa entrada acepta RSA,
y solo con firmas SHA-2. Cada autenticación con RSA queda como aviso en el
log del daemon, o en la terminal cuando `norte` corre sin daemon.
`norte doctor` te lo recuerda mientras esté puesto. Pide una clave ed25519
cuando puedas.

> ⚠ El FTP va en claro. No «salvo que actives TLS»: todavía no hay FTPS, así que ese ajuste no significa nada y la contraseña y todos los bytes de todos los ficheros cruzan la red a la vista. Cada conexión FTP te lo avisa. Fuera de tu propia red, usa `sftp://`.

> ⚠ En el almacenamiento de objetos **no hay directorios**. Una carpeta es el prefijo común de las claves que cuelgan de ella, así que una carpeta vacía solo existe si alguien escribió un objeto marcador, y borrar la última clave de un prefijo hace desaparecer la carpeta. Renombrarla es copiar todas las claves y luego borrarlas todas, no una operación instantánea.

# Abrir y cerrar una conexión

{{cmd:pane.connect}} enseña las conexiones que tienes en `connections.toml` y
lleva el panel a la que elijas. La lista sale del fichero, así que lo que ves
aquí es lo que escribiste ahí — nombre y dirección, nunca una contraseña: las
credenciales se referencian, no se guardan (por eso hay un llavero).

{{cmd:pane.disconnect}} hace las dos cosas que su nombre promete: **suelta la
sesión** —el socket se cierra ahora, no cuando venza sola— y saca al panel de
ahí. A dónde va es el último sitio de su rastro que no esté en esa máquina:
volver a otra carpeta del mismo servidor reabriría la conexión que acabas de
pedir cerrar. Si todo su rastro es esa máquina, se cae a tu carpeta personal.
Sobre un panel local no hay nada que cerrar y te lo dice, en vez de contestar
«hecho» a algo que no ha hecho nada.

Cerrar no prohíbe: la siguiente vez que navegues ahí, norte vuelve a conectar
por el camino de siempre.
