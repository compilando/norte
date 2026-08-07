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
]
context = ["dialog.trust-host"]
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

El access key id de S3 no está en esa lista, porque no es un secreto: es un
identificador, y va en claro en `connections.toml`. La secret access key que
lo acompaña sí pasa por el resolver.

Por lo mismo la contraseña no va nunca en la URL: una dirección tipo
`sftp://usuario:clave@host` se rechaza en vez de aceptarse sin decir nada,
porque una URL acaba en el historial, en los logs y en pantalla.

> ⚠ El FTP va en claro. No «salvo que actives TLS»: todavía no hay FTPS, así que ese ajuste no significa nada y la contraseña y todos los bytes de todos los ficheros cruzan la red a la vista. Cada conexión FTP te lo avisa. Fuera de tu propia red, usa `sftp://`.

> ⚠ En el almacenamiento de objetos **no hay directorios**. Una carpeta es el prefijo común de las claves que cuelgan de ella, así que una carpeta vacía solo existe si alguien escribió un objeto marcador, y borrar la última clave de un prefijo hace desaparecer la carpeta. Renombrarla es copiar todas las claves y luego borrarlas todas, no una operación instantánea.
