# Panel de terminal empotrado

Krusader tiene un panel de terminal acoplado: se ve a la vez que los
listados, se teclea dentro, y sigue ahí cuando cambias de carpeta. norte no
lo tiene. Lo que sí tiene es **mejor en una cosa y distinto en todo lo
demás**, y conviene decirlo antes de empezar porque decide el alcance.

| Krusader | norte hoy |
| --- | --- |
| panel de terminal acoplado | — |
| línea de órdenes fija abajo | `pane.command-line`: un modal de un disparo |
| — | `app.toggle-panels`: shell VIVO persistente que sigue al panel (ADR 0084) |
| — | `app.terminal`: un shell en el directorio activo |

Lo que falta es el **panel**, y su dificultad no es el pty —ya lo tenemos—
sino las dos cosas que lo rodean: **emular un terminal** y **quitarle las
teclas al keymap**.

## Lo que ya está hecho y se reutiliza

- **`portable-pty 0.9`** ya es dependencia de `norte-tui`, justificada bajo la
  regla 8 en su `Cargo.toml` (ADR 0084). Sin arista nueva para la terminal.
- **`crates/norte-tui/src/subshell.rs`** (786 líneas) es pty con E/S, y
  **`crates/norte-frontend/src/subshell.rs`** (829) es su mitad pura: marcador
  de prompt, nonce, construcción de `cd` a prueba de bytes. **Ese reparto es
  el modelo a copiar**, no el código: lo que se comparte es la disciplina de
  dejar sin E/S todo lo que se pueda probar.
- **El registro de kinds es una tabla ABIERTA de cadenas** (`KindId` es
  `String`, ADR 0058 D2/D4), así que un kind `terminal` no toca el protocolo
  ni el puente. Ojo: `crates/norte-frontend/src/layout/kinds.rs:307` usa
  literalmente `"terminal"` como ejemplo de kind NO registrado en un test;
  hay que cambiarle el ejemplo.
- **`suspend.rs`** ya sabe ceder y recuperar la terminal, y **ADR 0082** ya
  fijó cómo se lanza un programa ajeno (ruta absoluta ANTES del `cwd`).

## Lo que no existe y hay que construir

**Un emulador VT.** No hay `vt100`, `vte`, `alacritty_terminal`, `tui-term`
ni `xterm.js` en el árbol. Lo que `subshell.rs` hace con los bytes del pty es
COPIARLOS a la terminal de verdad; un panel tiene que PARSEARLOS a una
rejilla de celdas que norte pinta. Son dos cosas distintas.

**Que el panel se quede las teclas de verdad.** Los kinds que
`takes_keys` consumen comandos del catálogo; un terminal consume **bytes**, y
eso incluye los acordes que hoy son de norte. Es el cambio de diseño del
plan, no un detalle: hay que decidir qué tecla SALE del panel y garantizar
que esa no se la coma nunca el pty, exactamente por el mismo motivo por el
que `app.toggle-panels` tiene que atarse a una tecla suelta y no a una
secuencia (ADR 0084).

## Fases

### T1 — El modelo, sin E/S ni toolkit

Crate nuevo `norte-term` (MIT OR Apache-2.0) o módulo en `norte-frontend`:
rejilla de celdas, cursor, atributos, `resize`, y `alimentar(&[u8])`. Una
dependencia de parseo VT con su justificación de regla 8. Sin pty, sin ratatui,
sin DOM.

Las tres candidatas, mirado el 2026-09-23 (la elección se cierra en T1, con
`cargo tree` delante, que es el dato que decide y el único que no se puede
mirar sin añadirla):

| crate | versión | licencia | señal |
| --- | --- | --- | --- |
| `vt100` | 0.16.2 | MIT | pura y pequeña, la que el plan proponía. **Pero tiene al menos cuatro forks publicados** (`panoptes-vt100`, `term-wm-vt100`, `atuin-vt100`, `patch4`), y eso se lee como upstream quieto: quien la quería la bifurcó |
| `alacritty_terminal` | 0.26.0 | Apache-2.0 | el emulador de un terminal que se usa de verdad, mantenido. Más superficie, y `rust-version` 1.85 |
| `termwiz` | 0.23.3 | MIT | de wezterm, **el mismo proyecto que `portable-pty` que ya está en el árbol** por ADR 0084: sin ecosistema nuevo. A cambio es la más gorda de las tres |

La licencia de la dependencia no cambia la nuestra (MIT OR Apache-2.0), pero
`just release-check` audita licencias, así que la elegida pasa por ahí antes de
darla por buena.

Tests: secuencias reales contra la rejilla esperada, incluidas las del corpus
hostil — un programa dentro del panel es contenido AJENO, así que lo que pinte
pasa por el mismo enmascarado que un nombre de fichero.

### T2 — El kind y la disposición

`decl("terminal", (20, 4), true, true, true, SIN_ROLES)` — se enfoca, toma
teclas, y `multi: true` (dos terminales son dos terminales, al revés que el
árbol o el registro). Ningún rol: nadie copia dentro de un terminal.
Cambiar el ejemplo del test de `kinds.rs:307`.

Comando `layout.terminal` en el catálogo, en los siete presets, i18n en los
dos idiomas, tema de ayuda, golden del CLI.

### T3 — El pty en la TUI

Un pty por panel, con el reparto de `subshell.rs`. El bucle de eventos
alimenta la rejilla y repinta. Cierre: el panel muere con su shell y el shell
con el panel.

**La decisión, tomada el 2026-09-23: un acorde PROPIO**, comando nuevo
(`layout.terminal-escape`) atado en los siete presets, y no el `detach_chord`
del subshell que este plan proponía.

Reutilizarlo sale gratis y es lo que se descarta: ese acorde ya significa
«cédeme la terminal ENTERA para el subshell», así que dentro del panel
significaría una segunda cosa distinta y el lector no puede predecir cuál le
toca — depende de dónde esté el foco. Un acorde con dos sentidos es peor que un
acorde más que aprender.

Lo que SÍ se copia de `detach_chord` es la parte que importa, y es una regla,
no una comodidad: **solo sirve un acorde SUELTO**. Una secuencia de dos pide
robarle al shell la primera tecla justo donde el lector la está tecleando. Si
un preset ata la salida a una secuencia, o no la ata, **el panel no toma las
teclas** —se queda mirando, con el shell vivo detrás— en vez de tomarlas sin
salida. El acorde concreto se elige en T2, mirando el catálogo y los siete
presets para que no choque con nada de `browse` y para que el terminal pueda
entregarlo (`shift+<carácter>` es una tecla muerta; la familia que este
repositorio ya usa para lo nuevo es `ctrl+alt+…`).

### T4 — El panel en la ventana

Segunda implementación, y no es opcional: la regla del proyecto es que una
función extensible está en los dos frontends.

**Decidido el 2026-09-23: la rejilla de T1 cruza el puente como filas de
spans**, la forma que el puente ya mueve para los listados y el visor. Una sola
emulación sirve a los dos frontends y la paridad sale por construcción.

Se descarta `xterm.js` en el renderer, y el motivo no es la arista de npm: son
DOS emuladores que pueden pintar distinto el mismo byte, y entonces la paridad
deja de ser una propiedad y pasa a ser una cosa que hay que comprobar a mano
para siempre. Además el enmascarado de T1 no protege lo que pinte el otro, así
que pediría su propia revisión de seguridad.

El precio que sí se acepta: cada repintado del shell es tráfico por el puente.
Se acota mandando lo que CAMBIA —la disciplina que ADR 0142 ya dejó escrita
para las capas de la ventana— y se mide con algo que repinte sin parar (un
`htop` dentro del panel) antes de dar T4 por hecha.

### T5 — Revisión de seguridad

`security-reviewer`, obligatorio: se está arrancando un shell con el entorno
y el `cwd` de alguien, dentro de un panel que pinta lo que ese shell escriba.
Las preguntas: qué entorno hereda, qué pasa con `NORTE_LEVEL`, qué se
enmascara al pintar, y si un panel de terminal puede existir sobre un pane
remoto (la respuesta de `app.terminal` es que no, y debería ser la misma).

## Lo que NO entra

- **Un plugin no puede ser un terminal.** La WIT de paneles
  (`norte:panel@0.1.0`) pinta spans y recibe comandos del catálogo, nunca
  bytes, y no importa ni `exec` ni pty. Darle esa capacidad es otra decisión,
  con su ADR, y probablemente un «no».
- La línea de órdenes fija abajo. `pane.command-line` ya contesta esa
  pregunta, y una segunda forma de teclear un comando no la contesta mejor.

## Coste, sin adornos

Es la pieza más cara de la batida del 2026-09-20: dependencia nueva,
emulador nuevo, un cambio en quién manda en el teclado, dos frontends y una
revisión de seguridad. No se hace en una tirada con otras cosas, y hacerla a
medias es peor que no hacerla — un terminal que se come un acorde de norte, o
que pinta lo que el shell escriba sin enmascarar, es un problema y no una
función a medio hacer.
