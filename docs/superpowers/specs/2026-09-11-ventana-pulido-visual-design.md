# La ventana: pulido visual y plugins parametrizados

**Fecha:** 2026-09-11. **Estado:** aprobado por Oscar en conversación
(«empieza por los fallos pero quiero hacer todo»).

Sobre una captura real de `norte-gui` (tema `retro-crt-amber`) el 2026-09-11.
Continúa la ola de usabilidad (ADR 0106): mismo reparto —el cromo es de la
ventana y se parametriza por `[ui]` o por tema; lo que es POR FILA es un
plugin y se parametriza por `[config]` del plugin—.

## Regla de reparto

- **GUI (cromo)**: migas de pan, cursor/marcas, cabeceras y arrastre de
  columnas, indicador de espacio libre, teclas físicas, toasts y píldora,
  diálogos con desenfoque, tema automático, tipografía empaquetada.
- **Plugin (por fila, con parámetros)**: iconos (`file-icons`, estilo
  `nerd`), peso visual del tamaño (`size-bar`, columna), edad de la fecha
  (`age`, decorador), git (`git-status`, `mode`), miniaturas (`thumbnail`,
  kind nuevo, WIT 0.11, con ADR).

## Fallos vistos en la captura (V0)

1. Botones de la barra de paneles invisibles: `--status-fg` (texto oscuro)
   sobre el fondo del panel. Pasan a `--fg` atenuados; abierto/con teclado,
   sin atenuar.
2. Barra F: el `<button>` centra su contenido; número y etiqueta se separan.
   Todo a la izquierda.
3. El pie no dice el espacio libre: el arranque del host no pasa por
   `aterrizar_listado` y nunca pedía `host.volumes`.
4. «Recorte a la izquierda»: la ventana medida por xdotool es 1200×800 en
   (1480, 630); el contenido no está desplazado. Es la posición de la
   ventana en la pantalla (o la captura), no el renderer. Se anota; no hay
   código que tocar.

## Tipografía (V1)

Dos fuentes OFL EMPAQUETADAS en el webview (Tauri incrusta `ui/dist`):
JetBrains Mono (listados, visor, barra F) e Inter (menú, diálogos, ajustes,
ayuda). 14 px, fila de 22 px (`--cell-h`), numerales tabulares, sin
ligaduras en listados. `[ui] font` / `mono_font` / `font_size` siguen
mandando cuando están; la pila del sistema es el respaldo. Máxima
compatibilidad = no depender de qué fuentes tenga la máquina.

## Cromo (V2, V5, V6)

- V2 Teclas físicas: número en insignia, hover con el id del comando.
  Cabeceras en versalitas con tracking y chevron. Arrastrar el borde de una
  columna cambia su ancho y persiste en `[ui.columns] width` (ya existe
  `WidthChoice`). Cursor con borde de acento (`--selection-bg` + borde
  izquierdo) y marcas con casilla al pasar el ratón; transiciones de 80 ms
  salvo `reduce_motion`.
- V5 Migas de pan en el título del panel (cada tramo navega). Toasts para
  el mensaje efímero (abajo a la derecha, se van con `notice_seconds`) y
  píldora con icono para el aviso persistente. Indicador de espacio libre
  como barra de 2 px en el pie.
- V6 `[ui] theme_light` / `theme_dark` con `prefers-color-scheme`; `[effects]
  backdrop = "blur"|"none"` en el tema para los diálogos.

## Plugins (V3, V4, V7)

- V3 `file-icons`: `style = nerd` (tercer valor del enum) y el webview
  empaqueta Symbols Nerd Font Mono. Parámetros nuevos: `dir_icon`
  (string), `hidden_dim` (bool). La TUI lo pinta con la fuente del terminal.
- V4 `size-bar` (columns): `▂▄▆█` por tramo, parámetros `scale`
  (`linear|log`), `width` (int 3–8), `relative_to` (`page|dir`). `age`
  (decorator, hueco `badge`): parámetros `thresholds` (string `1,7,30`),
  `role`. `git-status`: `mode = badge|column|both`.
- V7 `thumbnail`: kind nuevo que devuelve bytes de imagen para el panel de
  vista previa; WIT 0.11 y ADR propia. El último, y el único que toca el WIT.

## Fuera de alcance

Cambiar el core o el wire del daemon. La TUI no cambia salvo por lo que un
plugin nuevo le dé.
