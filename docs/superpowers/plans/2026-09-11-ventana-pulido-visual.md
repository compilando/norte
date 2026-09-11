# La ventana: pulido visual — plan

> **For agentic workers:** REQUIRED SUB-SKILL: `superpowers:executing-plans`.

**Goal:** los ítems de
`docs/superpowers/specs/2026-09-11-ventana-pulido-visual-design.md`, en el
orden que Oscar aprobó: fallos → tipografía → cromo → plugins → miniaturas.

**Architecture:** cromo en `crates/norte-gui-tauri/ui` (CSS + renderer) y
`norte-ui-host` (DTO/acciones); plugins WASM en `plugins/` con el kit;
`thumbnail` sube el WIT. Cero cambios al wire del daemon.

**Rama:** `feat/ventana-pulido-visual`. Gate: `just gui-ci` es el de la
ventana y NO entra en `just ci`: correrlo tras cada bloque V.

## Global Constraints

- Cada estilo que cambia con el tema se lee de una variable `--x` que
  `roles_de_tema` emite; nada de colores literales fuera de `:root`.
- Un parámetro de plugin es `[config.<k>]` en `plugin.toml` con tipo, y sale
  en la pantalla de ajustes sin código extra.
- Capturas con `norte-gui` en `DISPLAY=:1` bajo config aislada
  (`NORTE_NO_WIZARD=1`, daemon del sandbox levantado antes) y `magick
  import -window`; se lee la imagen, no se supone.

---

### V0: los cuatro fallos
- [ ] `.panelbar-button` con `--fg` atenuado; abierto/foco sin atenuar.
- [ ] `.keybar-cell` a la izquierda.
- [ ] `pedir_volumenes_de_pie` también en el arranque del host.
- [ ] Anotar el «recorte»: geometría de ventana, no renderer.
- [ ] `just link-gui`, captura, `just gui-ci`.

### V1: tipografía empaquetada
- [ ] `npm i @fontsource/jetbrains-mono @fontsource/inter` (OFL); `@font-face`
      en `style.css` con `font-display: swap`; pila de respaldo.
- [ ] 14 px / `--cell-h: 22px` / `font-variant-numeric: tabular-nums` /
      `font-variant-ligatures: none` en listados; Inter en cromo.
- [ ] `[ui] font`/`mono_font`/`font_size` siguen mandando (test del renderer).
- [ ] Justificar las dos dependencias npm en el commit (tamaño, licencia).

### V2: teclas, cabeceras, columnas, cursor
- [ ] Keycaps en `.keybar-cell` (insignia del número, hover con `title`).
- [ ] Cabeceras en versalitas + chevron; arrastre de borde → `UiAction::
      ResizeColumn { slot_id, id, cells }` → `persist_column_width` (existe
      `WidthChoice`) y patch de cabeceras.
- [ ] Cursor con borde de acento; casilla de marca al hover; transición
      80 ms salvo `reduce_motion`.

### V3: `file-icons` nerd
- [ ] Enum `style` gana `nerd`; tabla de glifos PUA por kind/extensión en el
      plugin; parámetros `dir_icon`, `hidden_dim`.
- [ ] Symbols Nerd Font Mono empaquetada (OFL/MIT) en el webview; `@font-face`
      solo para el hueco de icono.
- [ ] Reinstalar el plugin (digest nuevo) y reaprobar; captura.

### V4: `size-bar` y `age`
- [ ] Plugin `size-bar` (columns) desde `plugins/template`: parámetros
      `scale`, `width`, `relative_to`; test WASM del kit.
- [ ] Plugin `age` (decorator, badge): `thresholds`, `role`.
- [ ] `git-status`: `mode = badge|column|both`.

### V5: migas, toasts, píldora, indicador
- [ ] `BrowserSlotView.path_segments` (con `RowKey`-like índice) →
      `UiAction::BreadcrumbActivate { slot_id, depth }`.
- [ ] `StatusView.message` como toast con `notice_seconds`; banners como
      píldora.
- [ ] `BrowserSlotView.free_ratio: Option<f32>` → barra de 2 px en el pie.

### V6: tema automático y desenfoque
- [ ] `[ui] theme_light` / `theme_dark` (schema, load, catálogo de ajustes,
      docs) → `HostCatalog.theme_pair`; renderer escucha
      `prefers-color-scheme`.
- [ ] `[effects] backdrop` en el tema; `backdrop-filter` en `#dialogs`.

### V7: `thumbnail`
- [ ] ADR: kind `thumbnail` (bytes + mime, cota), WIT 0.11, `plugin.thumbnail`
      en el wire del plugin-host (no del daemon).
- [ ] Plugin `image-thumb`; el panel Preview de la ventana lo pinta.

### Cierre
- [ ] Ayuda `appearance` (en/es) con lo nuevo; changelog; memoria; `just ci` y
      `just gui-ci` una vez; revisión `rust-reviewer` + `security-reviewer`
      (V7 toca el plugin-host).
