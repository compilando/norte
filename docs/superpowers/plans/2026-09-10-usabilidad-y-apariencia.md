# Usabilidad y apariencia: plan

> **For agentic workers:** REQUIRED SUB-SKILL: `superpowers:executing-plans`.
> Los pasos llevan checkbox (`- [ ]`).

**Goal:** los diez ítems de
`docs/superpowers/specs/2026-09-10-usabilidad-y-apariencia-design.md`, en
las dos superficies, configurables y documentados.

**Architecture:** lógica en `norte-frontend` (puro) o `norte-ui-host`; la
TUI y la ventana pintan. Cero cambios de wire; el puente sube UNA vez al
final de los ítems que lo tocan (T7: `KeyBarView`, `footer`, `notices_unread`,
`DialogLine.kind`, `WizardView`), no una por ítem.

**Tech Stack:** Rust, ratatui, TypeScript + Tauri, Fluent, `jiff` (nuevo).

**Rama:** `feat/usabilidad-y-apariencia`. Gate: `just t <crate>` en el
bucle; `just ci-fast` tras T4 y T8; `just ci` al cerrar.

**Estado: ejecutado el 2026-09-10** (ADR 0106). Lo que salió distinto del
plan:

- **T8, mitad de la ventana (`DialogLine.kind`) descartada**: la ventana ya
  tiene `DialogChoice` y los campos estructurales; el papel de línea tocaba
  doce inicializadores por un dato que nada pintaría distinto.
- **T2: `iso` sigue UTC**; lo fijan modales, metadata y sync como libre de
  locale. `smart` es el local. El defecto de `ColumnsSettings` en tests queda
  `relative` (la hora local depende de la máquina); los frontends encadenan
  `with_date_format`.
- **T7: el puente subió en T4**, no aquí: el guarda de forma del corpus lo
  exige con el primer campo nuevo. Un solo bump (63) para toda la ola.
- **T8: la regla de botones no es «igual a lo generado» sino «la ÚLTIMA
  línea del cuerpo que parsea como `[tecla] verbo`»**: el piloto enseñó que
  el modal de copia de un fichero lleva su línea de teclas en prosa Fluent.
- **T11: `first_run` viaja en el catálogo de la ventana + acción
  `wizard_open` del renderer**, no en `UiHostOptions` (33 inicializadores de
  test). La `Cli` de la ventana no tiene `--setup`.
- **Fuera**: barra de teclas sensible a modificadores (un terminal no
  informa de Shift solo).
- Tres arreglos que solo vio el piloto de tmux: corte por la cola en la
  barra de teclas, pie por prioridad de tramos, botones en pistas de prosa.

## Global Constraints

- Cada clave `[ui]` nueva: `schema.rs::UiSection` → `load.rs::CommonConfig`
  (+ `merge_ui_flags`, acumulador, ensamblado) → `config_cobertura.rs` y
  `profile.rs:162` (destructuran sin `..`) → `settings.rs::CATALOG` +
  `current_value` → Fluent `setting-*` en/es → `docs/schema/norte.schema.json`
  (`just` recipe del schema) → `main.rs` arranque → `config_reload.rs`.
- Nombres como bytes; recortes por CELDAS (`norte_frontend::cells`).
- Ningún `sleep` en tests; el reloj de los avisos se inyecta.
- Toda clave nueva sale en el topic `appearance` (en/es), y `help-cmd-*`
  para comandos nuevos; golden `norte-cli` con `NORTE_UPDATE_GOLDEN=1`.

---

### T1: claves de configuración

- [ ] `UiSection`: `key_bar`, `panel_bar_style`, `pane_footer`,
      `date_format`, `notice_seconds`, `dialog_buttons` (todas `Option`).
- [ ] `CommonConfig` + merge + sweeps + `CATALOG` (Bool/Enum/Int) +
      `current_value` + Fluent `setting-ui-*-name/-desc` en/es.
- [ ] Schema golden regenerado. `just t norte-config norte-frontend
      norte-ui-host`.

### T2: fechas locales y `smart`

- [ ] `jiff` en `norte-frontend` (justificado en el commit).
- [ ] `TimeFormat::Smart`; `format_mtime_in` con offset local; `iso` local.
      Tests con `now_ms` fijo Y offset fijo (inyectar `jiff::tz::TimeZone`
      en una `_with_tz` para no depender de la máquina).
- [ ] `[ui] date_format` como defecto de `mtime` en `ColumnsSettings::resolve`.

### T3: cursor con contraste

- [ ] `Role::SelectionUnfocused` (+`Button` para T8), fallback, `ALL`,
      kebab; ocho presets: `selection` = borde activo/fondo oscuro,
      `selection-unfocused` = valor anterior.
- [ ] `draw_pane`: `highlight_style` según `focused`.
- [ ] `roles_de_tema` emite `selection-unfocused-*`; `style.css` defaults;
      renderer usa la var en `.pane:not(.focused) .row.cursor`.

### T4: paleta

- [ ] `palette_state`: subsecuencia como fallback; `recent: Vec<String>`
      con `note_run(key)` (cap 5) y orden con consulta vacía.
- [ ] Persistir recientes en la sesión de UI (`session.put`), TUI y host.
- [ ] `draw_palette`: etiqueta ≥ 28 celdas primero, id `DIM`, chord derecha;
      recorte sobre la etiqueta. Snapshot nuevo.
- [ ] `menus.ts` `paintPalette`: mismo orden de columnas; CSS.
- [ ] **`just ci-fast`** (uno).

### T5: barra de paneles con nombres

- [ ] `panelbar::PanelButton` gana `name` (ya lo calcula `nombre_con`);
      `draw_panel_bar` pinta nombre con letra subrayada si `names` y caben
      (`ANCHO_BOTON` pasa a variable por estilo); zonas de ratón siguen.
- [ ] `PanelButtonView.name`; renderer pinta nombre.

### T6: pie por panel

- [ ] `norte_frontend::footer::pane_footer(counts, marked, free, lang)`.
- [ ] TUI: `app.volumes` cacheado (patrón `pending_profile`: pedir tras
      `settle_cd`/`refresh`, aterrizar en el bucle); `draw_pane`
      `title_bottom` cuando no hay quick search y `pane_footer`.
- [ ] Host: mismo cache en el controller; `PanelView.footer`.

### T7: barra de teclas + puente

- [ ] `norte_frontend::keybar::cells(eff, screen, lang) -> [KeyCell; 10]`
      (etiqueta `menu-item-<cmd>` o vacía).
- [ ] TUI: fila última; `geometry.rs` reserva; `draw_key_bar`; zona
      `After::KeyBar(n)` → despacha el comando; `config_reload`.
- [ ] Host: `KeyBarView` en la pantalla; `UiAction::KeyBarActivate{index}`;
      fixture golden; `types.ts`; renderer `paintKeyBar`; CSS.
- [ ] Sube `BRIDGE_VERSION` 62 → 63 con TODOS los campos de T5–T10 en el
      changelog rustdoc (`footer`, `name`, `KeyBarView`, `notices_unread`,
      `DialogLine.kind`, `WizardView`).

### T8: botones en los modales

- [ ] `LineKind::Buttons`; `modal_title_body` emite `Buttons` para la línea
      de `dialog_hints` cuando `dialog_buttons`; `draw_modal` pinta
      `[ Enter  Confirm ]` con `Role::Button`, zonas `After::DialogButton(chord)`.
- [ ] `DialogLine.kind` hacia la ventana; `dialogs.ts` estilos por kind.
- [ ] **`just ci-fast`** (dos).

### T9: avisos con caducidad

- [ ] TUI: `app.message: Option<Notice{text, since: Instant}>`; `expired()`
      en el bucle con `notice_seconds`; anillo `app.notices_unread`;
      insignia en `compose` con zona `After::Notices` → `layout.log`.
- [ ] Host: `StatusView.notices_unread`; reloj inyectado.

### T10: F9 = menú

- [ ] Cinco presets `f9` → `app.menu`; `alt+9` → `app.theme` en los tres
      diseñados; cabeceras de `krusader` y `total-commander`. Goldens de
      keymap si los hay.

### T11: asistente de primer arranque

- [ ] `norte_frontend::wizard::{Wizard, Step, Answer}` puro, con tests.
- [ ] Condición en `main.rs` (no `norte.toml` de usuario ∧ TTY ∧
      `NORTE_NO_WIZARD` ausente, o `--setup`). Overlay TUI (`Modal::Wizard`),
      tema en vivo, escritura por `config::persist_set`; iconos por
      `plugin.set_config` si `file-icons` está.
- [ ] Host: `WizardView` + `UiAction::WizardAnswer`; renderer.
- [ ] Piloto tmux con sandbox vacío.

### T12: ayuda, ADR, changelog, memoria

- [ ] `topics/{en,es}/appearance.md`; enlaces desde `settings`, `dialogs`,
      `panes`; tablas `EN`/`ES` de `corpus.rs`; golden CLI.
- [ ] ADR 0106 «el cromo se deriva del keymap y del catálogo».
- [ ] `CHANGELOG.md`; memoria.
- [ ] **`just ci`** (uno). Revisores: `rust-reviewer` sobre el rango.
