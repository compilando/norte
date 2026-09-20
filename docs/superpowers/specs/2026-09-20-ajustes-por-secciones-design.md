# Los ajustes, por secciones (diseño)

*2026-09-20. Sucede a `2026-07-24-settings-ui-cursor-memory-design.md`, que
dejó los ajustes en «una lista curada con buscador» — v1, y explícitamente
sin secciones.*

## Qué se rompió y por qué hay que rehacerlo

Oscar abrió los ajustes y describió tres cosas en una frase:

1. «Veo General y luego Tema» — *General* es la única cabecera que hay y
   *Tema* es la primera fila. No son dos secciones: son la cabecera y su
   primera fila, y desde fuera se leen como una jerarquía que no existe.
2. «Si bajo hasta abajo y subo, General no vuelve a subir» — un bug de
   verdad, ya arreglado aparte (`fix/ajustes-cabecera-no-vuelve`): los dos
   frontends anclaban la ventana a la FILA, y la primera fila de una sección
   vive una línea por debajo de su cabecera.
3. «Además está ahora todo en General» — las 33 entradas del catálogo llevan
   `section: Section::General`. El `enum Section` tiene dos variantes y una
   de ellas (`Plugins`) no aparece en el catálogo.

Y una cuarta, la petición: «quiero unos ajustes cool y wow como los de
VSCode».

Lo que hace que los ajustes de VSCode funcionen no es que sean bonitos: es
que **un ajuste se encuentra de tres maneras distintas** —bajando por un
índice, leyendo una sección, o escribiendo dos letras en el buscador— y que
**se ve de un vistazo qué has tocado tú**. Eso es lo que se copia aquí. No se
copia el aspecto.

## Alcance

- Las dos superficies: el overlay de la terminal (S3) y la vista F11 de la
  ventana (S4). La regla de la casa es paridad, y aquí además comparten el
  modelo entero.
- El modelo compartido `norte-frontend::settings` (1825 líneas) crece: es
  donde vive el catálogo y la máquina de estados, y donde tiene que vivir
  todo lo que las dos superficies necesitan contar igual.
- `norte-config` gana `persist_unset`: hoy no hay forma de quitar una clave.

No entra: derivar el catálogo del esquema JSON (sigue siendo curado y
localizado, por el mismo motivo de 2026-07-24), ni tocar el esquema de
configuración, ni el protocolo del daemon — nada de esto cruza el socket.

## Decisiones

### D1 — Siete secciones, y el reparto es del catálogo

`Section` pasa de `{General, Plugins}` a:

| variante | clave Fluent | qué entra |
| --- | --- | --- |
| `Appearance` | `settings-section-appearance` | `ui.theme`, `ui.theme-light`, `ui.theme-dark`, `ui.font`, `ui.mono-font`, `ui.font-size`, `ui.reduce-motion`, `ui.row-stripes`, `ui.images` |
| `Panes` | `settings-section-panes` | `ui.show-hidden`, `ui.parent-entry`, `ui.dir-indicator`, `ui.pane-footer`, `ui.date-format`, `ui.panel-bar`, `ui.panel-bar-style`, `ui.menu-bar`, `ui.key-bar`, `ui.splash`, `ui.processes-panel` |
| `OpenWith` | `settings-section-open-with` | `ui.editor`, `ui.editor-detached`, `ui.diff`, `ui.diff-detached` |
| `Input` | `settings-section-input` | `keymap.preset`, `ui.mouse`, `ui.alt-menu`, `ui.quick-search` |
| `Behavior` | `settings-section-behavior` | `ui.confirm-quit`, `ui.dialog-buttons`, `ui.notice-seconds`, `ui.history-size`, `ui.lang` |
| `Plugins` | `settings-section-plugins` | lo que ya genera `plugin_summary_rows` |
| `Paths` | `settings-section-paths` | las ubicaciones, hoy solo en la ventana |

`Section::General` **desaparece**. Su clave Fluent (`settings-section-general`)
se queda un ciclo como alias sin usar para que una traducción a medias no
deje una cabecera vacía, y se borra en el siguiente.

El orden de las secciones es el de la tabla, y es deliberado: lo que se toca
el primer día arriba, lo que es diagnóstico abajo. Dentro de cada sección el
orden sigue siendo el del catálogo (agrupado por sección de `norte.toml`),
no alfabético.

`Paths` es una sección del MODELO aunque sus filas no salgan del catálogo: es
lo que permite que la terminal la gane sin copiar la proyección de la
ventana, y que el índice la liste como una más.

### D2 — `Row` lleva su sección, y si está tocado

```rust
pub struct Row {
    // …lo que ya hay…
    /// La sección bajo la que se pinta. Del catálogo para una entrada
    /// curada, `Plugins` para un resumen de plugin, `Paths` para una
    /// ubicación.
    pub section: Section,
    /// El valor efectivo NO es el de fábrica.
    pub modified: bool,
}
```

`modified` se calcula con el mismo `current_value` con el que se pinta el
valor, pasándole un `FrontendConfig::default()`: dos lecturas de la misma
función, no una tabla de defectos escrita a mano que se desincronice del
esquema. Una fila de `Plugins` o de `Paths` nunca está modificada.

### D3 — El índice es una proyección, no una segunda lista

```rust
/// Una sección tal y como la pinta el índice: cuántas filas VISIBLES tiene
/// con el filtro puesto, y en cuál empieza.
pub struct SectionView {
    pub section: Section,
    pub title: String,
    pub visible: usize,
    pub first_row: Option<usize>,
}

impl SettingsState {
    pub fn sections(&self) -> Vec<SectionView>;
    pub fn jump_to(&mut self, section: Section);
}
```

Una sección que el filtro deja a cero se pinta **apagada**, no desaparece: un
índice que cambia de largo mientras escribes es un índice que no se puede
usar como mapa. `jump_to` sobre una sección vacía no mueve el cursor.

### D4 — El buscador manda, con dos operadores y una cuenta

El filtro actual pliega `id + nombre + descripción`. Crece:

- `@modified` — solo lo que difiere del valor de fábrica.
- `@section:<lo que sea>` — pliega contra el nombre **traducido** de la
  sección y contra su clave estable en inglés, así que `@section:apariencia`
  y `@section:appearance` valen las dos. Un idioma no puede ser la diferencia
  entre encontrar algo y no encontrarlo.
- Los operadores se combinan con el resto del texto (`@modified fuente`).
- Un `@` que no abre operador conocido es texto normal: nadie tiene que
  escapar nada para buscar una arroba.
- La cuenta —«7 de 33»— se pinta siempre, también sin filtro.

### D5 — Restablecer dice la verdad sobre las capas

`persist_unset(dir, section, key)` quita la clave de la capa de ESCRITURA
(la misma que elige `dir_de_escritura`, honrando el perfil activo — la
lección del BLOCKER de 2026-09-09). Nace en `norte-config` junto a
`persist_keymap_unbind`, que ya resuelve el mismo problema para una tecla.

Lo incómodo, y va escrito en la pantalla: **quitar tu clave no siempre
devuelve el valor de fábrica.** Si el sistema, el perfil o el proyecto fijan
esa clave, el valor cambia y sigue sin ser el defecto. No se inventa
maquinaria de procedencia para explicarlo: como `modified` se calcula contra
el defecto y las filas se reconstruyen tras escribir, el punto **se queda
encendido**, y quien restableció mira ese punto y lo anuncia — `settings-reset-done`
si volvió al valor de fábrica, `settings-still-set-elsewhere` si no. Es
exactamente lo que ha pasado, dicho con lo que ya se sabe y en el momento en
el que importa. Una marca permanente en la fila pediría recordar «esto se
restableció» a través de una reconstrucción que no conserva identidad, y eso
sí sería maquinaria nueva.

Restablecer una fila que no está modificada no escribe nada.

### D6 — La terminal: índice a la izquierda, cabecera clavada arriba

- El modal ensancha a `clamp(30, 100)`. Con ancho interior ≥ 60 se parte:
  índice de 20 celdas | lista. Por debajo, solo lista — la misma degradación
  que ya hacen las columnas del panel.
- La cabecera de la sección en la que está el cursor se pinta **fuera** del
  `Paragraph` que scrollea, en la primera línea del área de lista. Es
  pegajosa de verdad, y con ella el bug de la cabecera deja de poder existir
  (el arreglo del ancla se queda igual: sigue siendo la regla correcta para
  las cabeceras que sí scrollean).
- Teclas, todas **locales del overlay**: `tab` cambia entre índice y lista,
  `[` y `]` van a la sección anterior/siguiente, `ctrl+r` restablece la fila.
  No son comandos del catálogo a propósito: el overlay se come todo
  imprimible para filtrar, y un comando nuevo obliga a bindear en los siete
  presets, a escribir dos `help-cmd-*`, a tocar el golden del CLI y a
  justificarlo en cuatro transcripciones. Lo que se paga a cambio es
  documentarlas: van en la línea de pie y en el tema de ayuda.
- El punto de modificado es `•` delante del nombre; en una terminal sin
  Unicode fiable, `*`. Nunca color a secas: el color no es información.

### D7 — La ventana: dos paneles y un buscador de verdad

- Dos paneles: `<nav>` con las secciones, `<main>` con la lista. Cabeceras
  `position: sticky`.
- La lista deja de recrearse entera en cada pintada para las partes que no
  cambian, o el scroll de la rueda se pierde en cada frame (lo hace hoy).
  Como mínimo: conservar `scrollTop` al repintar.
- El buscador es un `<input>` real. Las teclas imprimibles no llegan al host
  —por eso esta ventana nunca tuvo filtro—, así que el texto viaja como
  acción del puente.
- Tres acciones nuevas y **una subida de versión del puente**:
  `settings_query { text }`, `settings_jump_section { section }`,
  `settings_reset { row }`.
- El cursor de la ventana deja de ser plano sobre `filas ++ rutas`: con
  filtro, un índice plano miente. Pasa a contar filas VISIBLES, como el de la
  terminal (hay un `debug_assert` en `norte-ui-host/src/settings.rs:181` que
  dice justo que lo plano solo vale porque no se filtra: se cae con esta
  fase).

## Fases

| fase | qué | dónde |
| --- | --- | --- |
| F1 | `Section` ×7, `Row.section`, `Row.modified`, `sections()`, `jump_to`, i18n ×2 | `norte-frontend`, `norte-i18n` |
| F2 | `@modified`, `@section:`, la cuenta | `norte-frontend` |
| F3 | `persist_unset` + `PendingWrite::Unset` + `settings-still-set-elsewhere` | `norte-config`, `norte-frontend` |
| F4 | Terminal: índice, cabecera clavada, `tab`/`[`/`]`/`ctrl+r`, punto | `norte-tui` |
| F5 | Ventana: dos paneles, sticky, buscador, botón restablecer, puente +1 | `norte-ui-host`, `norte-gui-tauri` |
| F6 | ADR, tema de ayuda ×2 locales, changelog, memoria | `docs/`, `crates/norte-help` |

F1–F3 son puras y se prueban sin pintar nada. F4 y F5 son independientes
entre sí y pueden ir en paralelo si hace falta — crates disjuntos, que es la
única forma de reparto que el presupuesto de esta casa aprueba.

## Cómo se prueba

- **Puro** (`norte-frontend`): cada entrada del catálogo tiene sección; cada
  sección tiene sus dos claves Fluent en los dos locales (el test de
  cobertura que ya existe, extendido a las cabeceras); `modified` es falso
  sobre la config por defecto y verdadero al cambiar un campo; `@modified` y
  `@section:` en los dos idiomas; `jump_to` sobre una sección vacía no mueve
  nada; `sections()` cuenta lo VISIBLE.
- **Terminal** (snapshots): la cabecera clavada sobrevive al scroll; el
  índice desaparece por debajo de 60 columnas; `[`/`]` mueven el cursor a la
  primera fila de la sección; `ctrl+r` sobre una fila no modificada no emite
  escritura.
- **Host** (`norte-ui-host`): el cursor filtrado apunta a la fila que se ve;
  `settings_reset` de una fila con otra capa por debajo deja el punto
  encendido y la línea puesta.
- **Renderer** (vitest): una sección por cada `SectionView`, `aria-current`
  en la del cursor, el buscador manda `settings_query`, el `scrollTop`
  sobrevive a un repintado.
- **Ida y vuelta de fichero**: escribir un ajuste y restablecerlo deja el
  `norte.toml` como estaba, byte a byte salvo el orden que `toml_edit`
  conserva.

## Lo que se decide no hacer

- **No** se deriva el catálogo del esquema JSON. Sigue en pie el motivo de
  2026-07-24: las descripciones del esquema son rustdoc en inglés.
- **No** hay ajustes por espacio de trabajo tipo VSCode. norte ya tiene
  cuatro capas y un selector de perfiles; una quinta noción de alcance
  confunde más de lo que ordena.
- **No** se edita el `norte.toml` a mano desde la pantalla. Hay un tema de
  ayuda que dice dónde está el fichero, y la sección de rutas lo señala.
- **No** se pliegan las secciones en la ventana. El índice ya resuelve
  «llévame ahí», y un plegado que no recuerda su estado entre aperturas es
  una tecla que no sirve para nada.
