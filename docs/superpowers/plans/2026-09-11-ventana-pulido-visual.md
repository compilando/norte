# La ventana: pulido visual — plan

> **For agentic workers:** REQUIRED SUB-SKILL: `superpowers:executing-plans`.

**Goal:** los ítems de
`docs/superpowers/specs/2026-09-11-ventana-pulido-visual-design.md`, en el
orden que Oscar aprobó: fallos → tipografía → cromo → plugins → miniaturas.

**Architecture:** cromo en `crates/norte-gui-tauri/ui` (CSS + renderer) y
`norte-ui-host` (DTO/acciones); plugins WASM en `plugins/` con el kit;
`thumbnail` estrena un paquete WIT propio. El wire del daemon solo gana un
método (`plugin.thumbnail`, 0.73.0).

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

### V0: los cuatro fallos — HECHO (`4e9826d7`)
- [x] `.panelbar-button` con `--fg` atenuado; abierto/foco sin atenuar.
- [x] `.keybar-cell` a la izquierda.
- [x] `pedir_volumenes_de_pie` también en el arranque del host.
- [x] Anotar el «recorte»: geometría de ventana, no renderer.
- [x] `just link-gui`, captura, `just gui-ci`.

### V1: tipografía empaquetada — HECHO (`1df1ae17`)
- [x] `@fontsource/jetbrains-mono` + `@fontsource/inter` (OFL); `@font-face`
      con `font-display: swap`; pila de respaldo.
- [x] 14 px / `--cell-h: 22px` / `tabular-nums` / sin ligaduras; Inter en cromo.
- [x] `[ui] font`/`mono_font`/`font_size` siguen mandando (test del renderer).
- [x] Las dos dependencias npm justificadas en el commit.

### V2: teclas, cabeceras, columnas, cursor — HECHO (`ed6c416f`, `1593a2dc`)
- [x] Keycaps en `.keybar-cell`.
- [x] Cabeceras en versalitas + marca; arrastre de borde → `resize_column`
      → `persist_column_width` (`width = { fixed = N }`) y cabeceras de todos
      los huecos (puente 64). La regla 2 del reparto (descartar por la
      derecha) corre en el renderer, que es quien sabe los píxeles útiles.
- [x] Cursor con acento; casilla de marca al hover; transición 80 ms salvo
      `reduce_motion`.

### V3: `file-icons` nerd — HECHO (`e026c4af`, `1593a2dc`)
- [x] Enum `style` gana `nerd`; `dir-icon`, `unknown-icon` (no `hidden_dim`:
      el hueco de icono no lleva rol).
- [x] Symbols Nerd Font Mono RECORTADA a 18 glifos (3,8 KB, MIT) como
      fichero en `assets/` — incrustada como `data:` la CSP la bloqueaba.
- [x] Reinstalado en el sandbox y aprobado por la TUI en tmux; captura.

### V4: `size-bar` y `age` — HECHO (`e026c4af`)
- [x] `size-bar` (columns): `scale`, `width`, `relative-to`; test e2e.
- [x] `age` (columns, no decorator: un decorador no ve el mtime):
      `thresholds`, `glyphs`, `format`; test e2e.
- [x] `git-status`: `glyphs` (letters|symbols) e `ignored` — no `mode`: un
      componente exporta UN world, y git-status es de columnas.

### V5: migas, toasts, píldora, indicador — HECHO (`7be287a1`)
- [x] `path_segments` + `breadcrumb_activate { slot_id, depth }` (puente 65).
- [x] `StatusView.message` como toast; banners como píldora.
- [x] `used_ratio` → barra de 2 px en el pie.

### V6: tema automático y desenfoque — HECHO (`46367a1d`)
- [x] `[ui] theme_light` / `theme_dark` (schema, load, catálogo de ajustes,
      i18n, docs) → `HostCatalog.theme_light/theme_dark`; el renderer
      escucha `prefers-color-scheme`.
- [x] `[effects] backdrop` → `--dialog-backdrop`; `backdrop-filter` en
      `#dialogs`; el preset `default` lo pide.

### V7: `thumbnail` — EN CIERRE
- [x] ADR 0107: paquete `norte:thumbnail@0.1.0` (no bump de `norte:plugin`),
      `plugin.thumbnail` (proto 0.73.0), verificación en el plugin-host.
- [x] Plugin `image-thumb`; el visor de la ventana lo pinta con «via».
- [ ] Gate verde, `protocol-guardian` + `security-reviewer`, `just plugins force`.

### Cierre
- [x] Ayuda `appearance` (en/es); changelog; memoria.
- [ ] `just ci` y `just gui-ci` una vez; revisión; merge a `main`; `link-gui`.
