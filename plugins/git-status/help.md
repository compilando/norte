+++
id = "org.norte.git-status"
title = "Git status"
+++
Añade una columna que marca qué ficheros cambiaron respecto al índice de
git: `M` modificado, `D` borrado, `?` sin rastrear, `!` ignorado. Un
fichero limpio no lleva marca, y un directorio lleva la más fuerte de lo
que tiene dentro.

Lee el índice y los `.gitignore` bajo la raíz del repositorio a la que el
host lo confina; nunca escribe.

Dos ajustes. `glyphs` cambia las letras por símbolos de una celda (`●`
modificado, `✖` borrado, `+` nuevo, `·` ignorado). `ignored`, apagado, deja
sin marca a los ficheros ignorados: con `.gitignore` grandes la marca se
repite en media pantalla y deja de decir nada.
