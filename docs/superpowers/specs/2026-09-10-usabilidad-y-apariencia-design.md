# Usabilidad y apariencia: diez huecos de la primera hora

**Fecha:** 2026-09-10. **Estado:** aprobado por Oscar en conversación
(«haz todo, respetando extensible, TUI y GUI, configurable, y ayuda»).

Diez cosas que un lector nota en la primera hora con `ntc`, medidas pilotando
la TUI en tmux (120×36, tema `default`, preset `orthodox`, config aislada).
Ninguna toca el wire del daemon; varias suben el puente de la ventana.

## Reglas transversales

1. **Las dos superficies.** Cada ítem tiene su mitad en la TUI y su mitad en
   la ventana, salvo donde la ventana ya lo tenía (botones de diálogo). La
   lógica va a `norte-frontend` o a `norte-ui-host`; los frontends pintan.
2. **Configurable cuando hay algo que elegir.** Cada clave nueva va en `[ui]`,
   con entrada en el catálogo de ajustes (pantalla de ajustes de los dos
   frontends), Fluent en `en` y `es`, y el golden del schema. Hot-reload en la
   TUI por `reload_config`. Claves nuevas:

   | clave | tipo | defecto | ítem |
   | --- | --- | --- | --- |
   | `key_bar` | bool | `true` | barra F1–F10 |
   | `panel_bar_style` | `letters` \| `names` | `names` | barra de paneles |
   | `pane_footer` | bool | `true` | pie por panel |
   | `date_format` | `relative` \| `smart` \| `iso` | `smart` | fechas |
   | `notice_seconds` | int 0..=600 | `8` | avisos con caducidad |
   | `dialog_buttons` | bool | `true` | botones en los modales |

   El cursor se configura por TEMA (ya existe `[ui] theme` y los ficheros de
   tema); la paleta y el asistente no tienen nada que elegir.
3. **Derivado, no dibujado.** La barra de teclas sale del keymap efectivo de
   la pantalla actual; las etiquetas de la barra de paneles del registro de
   kinds; el pie del listado y de `host.volumes`. Un usuario que reata F5 o
   un plugin que aporta un panel lateral ven su cambio sin tocar nada.
4. **Ayuda.** Topic nuevo `appearance` (en/es) con las seis claves y los tres
   elementos de cromo; `settings.md`, `dialogs.md` y `panes.md` enlazan.

## Los diez

### 1. Barra de teclas F1–F10 (`key_bar`)

Última línea de la TUI, como mc/far/norton; la línea de estado sube una fila.
Diez celdas `N Label` de ancho `width/10`; etiqueta = `menu-item-<cmd>`
(existe para los 93 comandos Live), recortada por celdas. Fuente: el keymap
efectivo de la PANTALLA actual (`browse` / `viewer` / `dialog`), de modo que
en un diálogo F-keys muestra lo que el diálogo ata. Un F sin atadura queda en
blanco. Clic ejecuta el comando (zona de ratón nueva `After::KeyBar`). No es
sensible a modificadores: un terminal no informa de Shift solo.

Ventana: `KeyBarView { cells: Vec<KeyCellView { key, label, command }> }` en
`Screen`, franja inferior de botones; clic → `UiAction::KeyBarActivate`. Sube
`BRIDGE_VERSION`.

### 2. Cursor con contraste (tema)

Rol nuevo `selection-unfocused` (cursor del panel SIN foco). Presets: el
cursor del panel con foco pasa al color del borde activo con texto oscuro
(`default`: bg `#5fafd7` fg `#1c1c1c`); el del panel sin foco conserva el
gris de hoy. Fallback monocromo: `reverse` y `dim`. La ventana recibe
`--selection-unfocused-bg/-fg` por `roles_de_tema`. Los ocho presets (los seis
del selector y los dos `retro-crt`).

### 3. Paleta

Columnas: etiqueta humana primero y entera (mínimo 28 celdas), id atenuado,
chord a la derecha; el recorte por celdas cae sobre la etiqueta, nunca sobre
la línea compuesta. Emparejado: substring plegado como hoy, y si no hay
ninguno, subsecuencia (`cpf` casa `copy path`). Recientes: los cinco últimos
comandos lanzados desde la paleta van arriba con la consulta vacía; viven en
la sesión de la UI (`session.put`), no en config. La ventana comparte
`palette_state` y su CSS pasa a las mismas tres columnas.

### 4. Barra de paneles con nombres (`panel_bar_style`)

`names`: `Places  Viewer  Jobs  Details  Log  Tree`, con la letra de acceso
subrayada; en menos de 60 celdas útiles vuelve a `letters` sola. El nombre
ya existe (`panelbar-<kind>`); solo cambia el pintado en los dos frontends.

### 5. Pie por panel (`pane_footer`)

Título inferior del borde de cada listado: `12 dirs · 84 files · 1.3 GiB`,
`+ 2 marked 4.0 MiB` si hay marcas, `· 120 GiB free` si se conoce. El
espacio libre se cachea en el App/host (`host.volumes` una vez por `cd` y por
`refresh`, en segundo plano como `pending_profile`), nunca por frame. Con
quick search activo el pie es el de quick search (manda el más específico).
Ventana: `PanelView.footer: Option<String>`.

### 6. Botones en los modales (`dialog_buttons`)

`LineKind::Buttons` nuevo: la línea de teclas generada por `dialog_hints`
se pinta como botones `[ Enter  Confirm ] [ Esc  Cancel ]` con `Role::Button`
(nuevo, fallback `reverse`), y cada botón es una zona de ratón que emite el
chord (`After::DialogButton`). `dialog_buttons = false` pinta la línea
`[enter] confirm · [esc] cancel` de hoy. Sin foco por Tab: en `dialog` Tab
es `dialog.pane`, y un foco de botones que compite con el campo de texto es
más error que ayuda. La ventana ya tiene `DialogChoice`; recibe además
`DialogLine.kind` para pintar las jerarquías de ADR 0103.

### 7. Avisos con caducidad (`notice_seconds`)

`app.message` pasa a `Notice { text, since }`. Tras `notice_seconds` (o
con la siguiente tecla, como hoy) sale de la línea de estado y entra en un
anillo de avisos de la sesión; la línea de estado muestra a la derecha una
insignia `⚠ n` mientras haya avisos no vistos, clic → abre `layout.log`
(el panel de registro ya existe y los avisos se anotan en él por `tracing`).
`0` = sin caducidad (comportamiento anterior). Los banners persistentes
(conexión degradada, journal) NO caducan: son estado, no aviso. Ventana:
`StatusView.notices_unread: u32` con la misma regla en `ui-host` (reloj
inyectable; tests deterministas con `hasta`).

### 8. Fechas (`date_format`)

`TimeFormat::Smart`: hoy → `14:02`; este año → `10 sep 14:02` / `Sep 10
14:02`; antes → `2025-09-10`. Ancho fijo 12. Hora LOCAL: `iso` hoy imprime
UTC y pasa a local. Dependencia nueva `jiff` en `norte-frontend` (lee
`/etc/localtime`, sin el fallo de `time::local_offset` multihilo, mantenida,
~400 KB). `[ui] date_format` es el defecto de la columna `mtime` cuando
`[ui.columns]` no fija `fmt`. Ambos frontends formatean en Rust.

### 9. F9 abre el menú

`orthodox`, `cua`, `vim`, `far`, `norton`: `f9` → `app.menu` (mc, far y NC
lo atestiguan). `app.theme` pasa a `alt+9` en los tres diseñados, que es
donde ya estaba en los cuatro importados. `krusader` conserva `f9` = terminal
(atestiguado) y `total-commander` deja `f9` libre (TC no tiene menú en F9);
los dos lo dicen en la cabecera.

### 10. Asistente de primer arranque

Cuando no existe `norte.toml` de usuario, hay TTY y `NORTE_NO_WIZARD` no
está puesta, la TUI abre un overlay de tres pasos: preset (lista de los
siete con una línea cada uno), tema (los del selector, con vista previa
aplicada en vivo) e iconos (si el plugin `file-icons` está instalado: «¿ves
📁?» → `style = emoji|ascii` por `plugin.set_config`). Esc en cualquier paso
= «no volver a preguntar»: escribe un `norte.toml` con solo un comentario.
Rerun: `ntc --setup`. El estado del asistente vive en
`norte_frontend::wizard` (puro) y la ventana lo pinta como diálogo con las
mismas tres páginas (`WizardView`). Escribe el fichero por `norte-config`
(`write_user_config`, que ya existe para los ajustes).

## Fuera de alcance

Arranque en el cwd en vez de la sesión; segundo panel en otro dir; marcado
en amarillo; barra de teclas sensible a modificadores.
