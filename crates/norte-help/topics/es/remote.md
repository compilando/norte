+++
id = "remote"
title = "SFTP, FTP y almacenamiento de objetos"
tags = ["remote"]
see_also = ["copying", "archives"]
commands = ["pane.hotlist", "pane.history", "pane.refresh"]
+++
Un panel sostiene un sitio remoto igual que sostiene un directorio. La
dirección es una URL, y su esquema dice quién contesta:

| Esquema | A dónde llega                              |
|---------|--------------------------------------------|
| file    | esta máquina                               |
| sftp    | transferencia de ficheros sobre SSH        |
| ftp     | FTP a secas                                |
| s3      | almacenamiento de objetos compatible con S3|

# Cómo llegar

Un remoto vive en tus favoritos. Añádelo a `norte.toml`:

```toml
[[hotlist]]
name = "trabajo"
path = "sftp://tu@host/srv/datos"
```

A partir de ahí {{cmd:pane.hotlist}} abre la lista y Enter lleva ese panel
allí. {{cmd:pane.history}} te devuelve a donde el panel ya ha estado durante
esta sesión. Desde ese momento todos los comandos de esta ayuda funcionan
igual — {{cmd:pane.refresh}} sobre todo, porque un directorio remoto no se
vigila y no se entera solo de que algo ha cambiado.

La primera vez que llegas a un host SSH desconocido, norte te enseña su huella
y pregunta. Compárala por otro canal antes de darla por buena: ese diálogo es
el único momento en que alguien puede notar una máquina intermedia, y por eso
Enter no responde por ti.

# Los secretos

La contraseña o la clave de acceso viven en el **llavero del sistema**. La
configuración guarda una referencia, jamás el secreto, así que el directorio
de configuración se puede copiar, respaldar o subir a un repositorio sin
filtrar nada.

Por lo mismo la contraseña no va en la URL: una dirección tipo
`sftp://usuario:clave@host` se rechaza en vez de aceptarse sin decir nada,
porque una URL acaba en el historial, en los logs y en pantalla.

> ⚠ El FTP a secas no cifra nada: ni la contraseña ni los ficheros. Para cualquier cosa que salga de tu propia red, mejor `sftp://`.

> ⚠ En el almacenamiento de objetos **no hay directorios**. Una carpeta es el prefijo común de las claves que cuelgan de ella, así que una carpeta vacía solo existe si alguien escribió un objeto marcador, y borrar la última clave de un prefijo hace desaparecer la carpeta. Renombrarla es copiar todas las claves y luego borrarlas todas, no una operación instantánea.
