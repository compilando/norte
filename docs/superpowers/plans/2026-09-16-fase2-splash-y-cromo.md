# Fase 2 — Splash, procesos y cromo (plan)

Spec: `docs/superpowers/specs/2026-09-15-historia-y-wow-design.md`, sección
«Fase 2». Rama `feat/splash-y-cromo`. **Sube el puente de la ventana UNA vez**
(splash, ritmo/ETA por tarea y progreso por fila); el wire del daemon no se
toca.

Gate: `just t <crate>` en el bucle; `just ci-fast` + `just gui-ci` a mitad y al
cerrar; `just ci` una vez antes de fusionar.

## Lo que el mapeo dejó claro

- La ventana **no pinta nada** hasta que `main.ts` arranca: no hay marcado de
  carga en `index.html`. Un splash de cliente puro se vería ANTES que el primer
  listado, pero no podría decir dónde estuviste — eso lo sabe el host. Por eso
  el splash es del host, con su `Option<SplashView>` como el resto de capas.
- `alternar_hueco` (ventana) y `toggle_processes` (TUI) son INTERRUPTORES. La
  apertura automática necesita las dos mitades por separado, o reabre el panel
  que el lector acaba de cerrar.
- `TaskProgress` no trae ritmo: lo calcula quien mira, con el reloj del
  pintado. Ya está hecho y compartido (`norte_frontend::tasks::Rate`).
- Atenuar el panel sin foco es CSS del renderer: hoy solo hay regla para
  `[data-role="active"]`, y el resto cae en `.slot` a secas.

## T1 — Compartido (`norte-frontend`, `norte-config`)

1. **HECHO**: `[ui] splash` (`brief|off|home`), `[ui] processes_panel`
   (`auto|manual`), `[ui] dir_indicator` (`auto|slash|none`), con fila de
   ajustes en los dos idiomas y golden del schema.
2. **HECHO**: `tasks::Rate` (media exponencial sobre dos fotos, descarta
   contador que retrocede), `human_rate`, `human_eta`.
3. `splash.rs`: `SplashView { art, version, revision, daemon, sections }`,
   `SplashSection { title_key, rows }`, `SplashRow { label, detail, command,
   arg }`. Las secciones salen de un REGISTRO: `fn sections(fuentes: &[&dyn
   SplashSource]) -> Vec<SplashSection>`, con `SplashSource { fn section(&self)
   -> Option<SplashSection> }`. Fuentes de esta fase: populares, favoritos y
   perfiles. Arte: una brújula ASCII con la N, `const ART: &[&str]`.
   Tests: el registro respeta el orden y se salta las fuentes vacías; el arte
   mide lo mismo en todas sus filas.
4. `processes::progress_for(tablero, ruta) -> Option<u8>`: el porcentaje de la
   tarea cuyo operando ES esa fila. Una sola respuesta para las dos
   superficies; sin tarea, `None`.

## T2 — TUI

1. Splash: `App.splash: Option<SplashView>`; se abre en `main.rs` junto a la
   puerta del asistente (`--no-splash`, `NORTE_NO_SPLASH`, y el asistente
   GANA); `brief` se quita con cualquier tecla o cuando llega el primer listado
   y pasa 1,2 s del reloj inyectado; `home` se queda hasta una tecla y `1..9`
   ejecuta su fila. Se pinta en `ui::draw` como capa, antes del modal.
2. Procesos `auto`: `App::processes_auto` recuerda si lo abrió el automático y
   si el lector lo tocó durante la tarea; el bucle abre al aparecer la primera
   fila y cierra cuando el tablero se vacía. `toggle_processes` se parte en
   `open_processes`/`close_processes`.
3. Ritmo y ETA: `TaskRow.rate: Rate`, alimentado en `TaskBoard::tick` con el
   reloj del pintado; la fila del panel pinta `human_rate` y `human_eta`.
4. Barra en la fila del listado: el nombre se pinta sobre un fondo proporcional
   (`progress_for`), sin tocar el ancho de nada.
5. Cromo: `dir_indicator` en `kind_glyph`; el pie del panel con el rol del
   borde de SU panel; `cell_text` con un espacio entre número y etiqueta
   cuando la celda da para número + espacio + tres celdas.

## T3 — Ventana (puente +1)

1. `SplashView` en `ViewSnapshot` + `ViewChange::Splash`; el controlador lo
   abre en el arranque con la misma regla que la TUI (el asistente gana) y lo
   cierra con cualquier tecla o clic. Renderer: `paintSplash` en su propia
   raíz, creada como la del asistente (el orden del DOM es el apilado).
2. `TaskView.rate`/`eta` (cadenas ya formateadas por `human_rate`/`human_eta`:
   el host formatea, el renderer pinta) y `RowView.progress: Option<u8>`, que
   el renderer aplica como variable CSS de una barra bajo el nombre.
3. Procesos `auto` con las dos mitades de `alternar_hueco`, sobre la misma
   condición que ya alimenta la insignia de la barra de paneles.
4. CSS: `.slot:not([data-role="active"])` atenuado con `--inactive-dim`,
   constante del renderer y no color de tema.
5. Vitest: splash pintado y quitado, barra de la fila, panel atenuado.

## T4 — Ayuda, ADR y cierre

- Topic `appearance` (en/es) con las tres claves nuevas; `settings.md` enlaza.
- ADR 0115: por qué el splash es del host y no del webview, por qué la
  apertura automática no reutiliza el interruptor, y por qué el ritmo se
  calcula en el cliente.
- CHANGELOG, memoria, `just link` y `just link-gui`.
- Revisores antes de commitear: `rust-reviewer` (estado del automático y del
  splash) y `protocol-guardian` NO hace falta (no se toca el wire), pero sí una
  revisión del PUENTE en la misma pasada.

## T5 — Gate

`just ci-fast` + `just gui-ci` tras T2 y al cerrar; `just ci` antes de
fusionar.
