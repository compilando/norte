# Theming

norte colorea la TUI mediante TEMAS (ADR 0020). El modelo vive en el crate
`norte-theme` y lo comparten la TUI y (a futuro) la GUI.

## Elegir un tema

En `norte.toml`, sección `[ui]`:

```toml
[ui]
theme = "catppuccin-mocha"
```

`theme` acepta:

- El **nombre** de un preset embebido: `default`, `catppuccin-mocha`,
  `gruvbox-dark`, `nord`.
- Una **ruta** a un fichero `.toml` propio.

Sin `theme`, se aplica el preset `default` (neutro). Si el tema falla al cargar
(ruta inexistente, TOML inválido), norte degrada al `default` y avisa en la
barra de estado — nunca revienta. El tema es **hot-reloadable**: al guardar la
config, el cambio se aplica en caliente.

## Escribir un tema propio

Un tema es un TOML con tres partes, todas opcionales (lo que falte hereda un
fallback monocromo sensato):

```toml
name = "mi-tema"

# 1) Roles semánticos de la UI.
[roles]
regular          = { fg = "#d0d0d0" }
selection        = { fg = "#ffffff", bg = "#3a3a3a" }
border-focus     = { fg = "#5fafd7", bold = true }
border-unfocused = { fg = "#6c6c6c", dim = true }
modal-border     = { fg = "#af87d7" }
status-bar       = { fg = "#1c1c1c", bg = "#5fafd7" }
title            = { fg = "#5fafd7", bold = true }
hostile-badge    = { fg = "#d75f5f", bold = true }
error            = { fg = "#d75f5f" }
warning          = { fg = "#d7af5f" }
info             = { fg = "#5fafd7" }
match            = { fg = "#1c1c1c", bg = "#d7af5f" }

# 2) Colores por TIPO de nodo.
[files.kind]
dir        = { fg = "#5fafd7", bold = true }
symlink    = { fg = "#5fafaf" }
# executable / fifo / socket / block-device / char-device también existen.

# 3) Colores por EXTENSIÓN (ganan al kind).
[files.ext]
rs  = { fg = "#d7875f" }
zip = { fg = "#d75f5f" }
png = { fg = "#af87d7" }
```

**Colores**: `#rrggbb` o la forma corta `#rgb`. **Atributos** de un estilo:
`bold`, `dim`, `italic`, `underline`, `reverse` (booleanos).

Un rol definido en el tema **reemplaza** su fallback por completo (tú controlas
el rol entero); un rol ausente conserva el aspecto monocromo de siempre.

## Profundidad de color

norte detecta la capacidad del terminal y **degrada** el color:

- `COLORTERM=truecolor` (o `24bit`) → 24 bits, el color va tal cual.
- `TERM` con `256` → paleta xterp-256 (color más cercano).
- En otro caso → 16 colores ANSI (color más cercano).

Así un mismo tema se ve razonable desde kitty/wezterm hasta un `xterm` básico.

## Efectos de GPU

Un tema puede llevar una sección `[effects]` (gradientes, glow, animación…).
La **TUI la ignora** (un terminal no tiene GPU); queda reservada para la GUI
(hito M5). Incluirla no rompe nada.
