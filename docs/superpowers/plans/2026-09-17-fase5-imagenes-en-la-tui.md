# Fase 5 — Imágenes de verdad en la TUI

> **Para quien ejecute esto:** SUB-SKILL OBLIGATORIA: usa
> `superpowers:subagent-driven-development` (recomendado) o
> `superpowers:executing-plans` para implementar tarea a tarea. Los pasos
> llevan casilla (`- [ ]`) para ir marcándolos.

**Objetivo:** que el visor de la TUI enseñe una imagen como imagen —píxeles
de verdad en los terminales que saben, medios bloques en los que no— en vez
de caer a hexview.

**Arquitectura:** el visor ya distingue una imagen (`Viewer.image:
Option<ImageFmt>`) y su propio comentario dice que «frontends sin render de
imagen (TUI) caen a hexview». Esta fase le da ese render por dos caminos que
ya existen y no se tocan entre sí: los BYTES salen del kind `thumbnail`
(`backend.plugin_thumbnail`, proto 0.73.0, ADR 0107), que hoy sólo usa la
ventana; el PINTADO va por el protocolo de gráficos de kitty, escrito al tty
después del frame de ratatui, porque un APC no cabe en una celda. Sin kitty,
la caída es el previewer `image-ansi` que ya está construido y verificado.

**Stack:** Rust, ratatui/crossterm, `norte-config`, `norte-i18n`,
`norte-help`; sin dependencias nuevas.

**Spec:** `docs/superpowers/specs/2026-09-15-historia-y-wow-design.md`,
sección «Fase 5 — Imágenes de verdad en la TUI».

## Lo que el piloto dejó comprobado (2026-09-17)

Esto se verificó pilotando `ntc` en un perfil aislado, no leyendo código, y
cambia el alcance de la fase:

- **Los medios bloques YA FUNCIONAN.** `image-ansi` instalado, aprobado y
  activado pinta `icon.png` en el visor: 4352 medios bloques (`▀`), 16
  colores de frente y 18 de fondo, con la cabecera «via Image preview».
  **Esta fase no construye la caída a bloques: ya está.** Lo que falta es
  kitty encima, y decidir cuál se usa.
- **Sin previewer aprobado, F3 sobre un PNG da hexview** (`89 50 4e 47 …
  IHDR`). Ése es el estado de partida de cualquiera que no haya aprobado nada.
- **Aprobar es un gesto de dos pasos y en este orden**: `y`
  (`dialog.approve`) abre el modal de capacidades, que se confirma con `y`; y
  sólo entonces `e` (`dialog.toggle-enabled`). Activar algo no aprobado se
  rechaza. El estado queda en `plugins-state.toml` anclado a
  `approval_anchor()` — sha256 de `norte-plugin-approval:v2` + digest del
  manifiesto + digest del wasm: **escribir ese fichero a mano no vale**,
  reemplazar el binario revoca el consentimiento.

## Restricciones globales

- **Nada de rutas como texto.** `VPath`/`OsString`; `to_str().unwrap()` es
  motivo de rechazo (regla 1).
- **Nada de I/O bloqueante en async** fuera de `spawn_blocking` (regla 2).
  La sonda del terminal corre ANTES del runtime de eventos, en `main`, que es
  donde ya corre la de kitty-teclado.
- **Errores tipados, sin `unwrap`/`expect`** fuera de tests (regla 6).
- **Un fallo de esto NUNCA impide ver el fichero.** Es el contrato que ya
  tiene el visor (ADR 0037): preview de plugin con estilo → preview plano →
  vista cruda. Una sonda que falla, un plugin roto, un terminal que miente:
  todos caen hacia abajo en esa cadena, ninguno es un error.
- **`[ui] images` es de TERMINAL.** La ventana pinta imágenes por su webview
  y no lee esta clave; se documenta así, igual que `[ui] mouse` y `[ui]
  alt_menu` dicen que la ventana los ignora.
- **Presupuesto del gate:** `just t <crate>` sin límite; UN `just ci-fast`
  cada ~3 tareas; UN `just ci` antes de fusionar. Nunca `just ci` como
  depurador, y nunca leer su código tras una tubería.

## Riesgo que esta fase asume por delante

**La sonda de kitty es la pieza con incertidumbre real, y va primero por
eso.** `crossterm` no expone una API para preguntar por el protocolo de
GRÁFICOS —sólo `supports_keyboard_enhancement()` para el de teclado— y no
parsea respuestas APC: una contestación `\x1b_G…\x1b\\` la descartaría. Así
que T1 escribe la sonda y su lectura cruda a mano, y **T1 es una puerta**: si
la respuesta no se puede leer de forma fiable, el plan cae al respaldo por
entorno descrito en T1 paso 6, y el resto del plan no cambia. No se sigue a
T2 sin haber cerrado T1 de una de las dos formas.

**tmux.** Un APC no atraviesa tmux sin `allow-passthrough`. La sonda lo verá
como «no soporta» y caerá a bloques, que es lo correcto: nadie ve un error, y
el piloto confirma que los bloques se ven bien ahí. Se documenta en la ayuda.

---

## Estructura de ficheros

| fichero | responsabilidad |
| --- | --- |
| `crates/norte-tui/src/kitty_graphics.rs` (nuevo) | sonda una vez, parseo de la respuesta, y los escapes de colocar/borrar |
| `crates/norte-config/src/schema.rs` (modificar) | el campo crudo `[ui] images` |
| `crates/norte-config/src/load.rs` (modificar) | `Images` validado + `ui_chrome.images()` |
| `crates/norte-frontend/src/settings.rs` (modificar) | `SettingDef` de `ui.images` |
| `crates/norte-i18n/i18n/{en,es}.ftl` (modificar) | nombre y descripción del ajuste |
| `crates/norte-tui/src/viewer_open.rs` (modificar) | pedir la miniatura cuando el visor abre sobre una imagen |
| `crates/norte-tui/src/app/mod.rs` (modificar) | `App.viewer_imagen: Option<ImagenColocada>` |
| `crates/norte-tui/src/ui/geometry.rs` (modificar) | `rect_del_visor`, compartido por el pintor y el bucle |
| `crates/norte-tui/src/ui/panels.rs` (modificar) | `draw_viewer` deja el hueco en blanco cuando hay imagen |
| `crates/norte-tui/src/event_loop.rs` (modificar) | colocar y borrar tras el frame |
| `crates/norte-help/topics/{en,es}/viewer.md` (modificar) | qué hace falta para ver una imagen |
| `docs/adr/0118-imagenes-en-la-tui.md` (nuevo) | la decisión |

---

### Task 1 — La sonda de kitty (PUERTA)

**Ficheros:**
- Crear: `crates/norte-tui/src/kitty_graphics.rs`
- Modificar: `crates/norte-tui/src/lib.rs` (declarar el módulo)
- Modificar: `crates/norte-tui/src/main.rs:453` (llamarla junto a la de teclado)
- Test: dentro del propio módulo (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produce: `pub fn consultar_soporte() -> bool`, `pub fn soportado() -> bool`,
  `fn respuesta_dice_si(bytes: &[u8]) -> bool`.

- [ ] **Paso 1: el test del parseo, que es lo único puro que hay aquí**

En `crates/norte-tui/src/kitty_graphics.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::respuesta_dice_si;

    #[test]
    fn una_respuesta_de_kitty_es_que_si() {
        // kitty contesta al query con OK para el id que se le mandó.
        assert!(respuesta_dice_si(b"\x1b_Gi=31;OK\x1b\\\x1b[?62;c"));
    }

    #[test]
    fn solo_la_respuesta_de_da1_es_que_no() {
        // Un terminal que no habla el protocolo ignora el APC y sólo
        // contesta a DA1. Es el caso de xterm, de VTE y de tmux sin
        // passthrough, y es la razón de mandar DA1 detrás: sin él no habría
        // nada que esperar y la sonda colgaría hasta el plazo.
        assert!(!respuesta_dice_si(b"\x1b[?62;c"));
    }

    #[test]
    fn un_ok_de_otro_id_no_cuenta() {
        // Si la respuesta es de otra consulta (un id que no es el nuestro),
        // no dice nada de nuestra pregunta.
        assert!(!respuesta_dice_si(b"\x1b_Gi=99;OK\x1b\\\x1b[?62;c"));
    }

    #[test]
    fn un_error_declarado_es_que_no() {
        assert!(!respuesta_dice_si(b"\x1b_Gi=31;ENOTSUPPORTED\x1b\\"));
    }

    #[test]
    fn nada_es_que_no() {
        assert!(!respuesta_dice_si(b""));
    }
}
```

- [ ] **Paso 2: correr el test y verlo fallar**

Ejecuta: `just t norte-tui`
Esperado: FALLA — `respuesta_dice_si` no existe.

- [ ] **Paso 3: el parseo mínimo**

```rust
/// El id con el que se pregunta. Arbitrario y sólo nuestro: una respuesta
/// con otro id contesta a otra pregunta y no dice nada de la nuestra.
const ID_SONDA: &str = "i=31";

/// ¿La contestación del terminal dice que sabe pintar gráficos?
///
/// Se busca la respuesta APC del protocolo (`\x1b_G…;OK\x1b\\`) CON NUESTRO
/// ID. Cualquier otra cosa —sólo la respuesta de DA1, un error declarado,
/// nada en absoluto— es «no»: quien no sabe, calla.
fn respuesta_dice_si(bytes: &[u8]) -> bool {
    let Ok(texto) = std::str::from_utf8(bytes) else {
        return false;
    };
    texto
        .split("\x1b_G")
        .skip(1)
        .any(|resto| match resto.split_once("\x1b\\") {
            Some((cuerpo, _)) => cuerpo.contains(ID_SONDA) && cuerpo.ends_with(";OK"),
            None => false,
        })
}
```

- [ ] **Paso 4: correr el test y verlo pasar**

Ejecuta: `just t norte-tui`
Esperado: PASA.

- [ ] **Paso 5: la sonda de I/O, sin test y dicho por qué**

Calcada de `alt_menu::consultar_soporte` (misma casa, mismas costumbres:
`OnceLock`, una vez al arrancar, `is_terminal` primero, error = «no»).

```rust
use std::io::{self, IsTerminal, Read, Write};
use std::sync::OnceLock;

/// Lo que contestó el terminal, preguntado UNA vez.
static SOPORTE: OnceLock<bool> = OnceLock::new();

/// Pregunta al terminal si sabe pintar gráficos, y guarda la respuesta.
///
/// Se llama al ARRANCAR, con raw mode ya puesto y antes de que el bucle
/// levante su lector de eventos, por los dos motivos que ya documenta
/// `alt_menu::consultar_soporte`: con el lector vivo, ese hilo tiene el lock
/// y la pregunta se rinde; y bajo `--pick` stdout es la tubería de datos de
/// quien llama, así que sin terminal en stdout no se pregunta.
///
/// NO tiene test: lo que hace es escribir en la terminal de control y leerla
/// con un plazo. Lo testeable es [`respuesta_dice_si`], que sí lo está. Un
/// test de esto necesitaría un pty falso que contestara como kitty, y eso es
/// probar el pty.
///
/// Se manda el query APC y DETRÁS un DA1: un terminal que no habla el
/// protocolo ignora el primero en silencio, y sin el segundo no habría nada
/// que esperar — la sonda agotaría el plazo siempre, y el arranque pagaría
/// ese plazo en cada terminal que no lo soporta.
pub fn consultar_soporte() -> bool {
    *SOPORTE.get_or_init(|| {
        if !io::stdout().is_terminal() {
            return false;
        }
        preguntar().unwrap_or(false)
    })
}

/// La respuesta de [`consultar_soporte`], sin preguntar. Si no se preguntó,
/// «no».
#[must_use]
pub fn soportado() -> bool {
    SOPORTE.get().copied().unwrap_or(false)
}
```

El cuerpo de `preguntar()` escribe en `/dev/tty`
(`std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty")`):

```
\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c
```

y lee con un plazo corto (200 ms) hasta ver la `c` que cierra DA1, o hasta
agotarlo. Devuelve `respuesta_dice_si(&leido)`.

- [ ] **Paso 6: LA PUERTA — comprobarlo en terminales de verdad**

Esto no lo decide un test. Pilota en tmux (con `-x 130 -y 40`) y, si hay,
en kitty o foot:

```
tmux new-session -d -s pgfx -x 130 -y 40 '<repo>/target/debug/ntc --no-splash <ruta-con-un-png>'
```

Comprueba en el log (`tracing::debug!`) qué contestó cada uno.

**Si la lectura resulta no ser fiable** —se come la respuesta, cuelga, o
ensucia la pantalla— el respaldo es leer el entorno en vez de preguntar:
`TERM=xterm-kitty`, `KITTY_WINDOW_ID`, `TERM_PROGRAM=ghostty`, `TERM=foot`.
Es peor (no ve un terminal nuevo) pero no tiene I/O que falle. Escribe cuál
de los dos quedó, y por qué, en la ADR de T7 — es la decisión más cara de
esta fase.

**Mata la sesión al terminar: `tmux kill-session -t pgfx`.** Un piloto
olvidado retuvo el lock de sesión de este proyecto siete días.

- [ ] **Paso 7: llamarla al arrancar**

En `crates/norte-tui/src/main.rs`, junto a la línea 453:

```rust
let _ = norte_tui::alt_menu::consultar_soporte();
// La misma pregunta para los GRÁFICOS, en el mismo sitio y por los mismos
// dos motivos: el lector de eventos todavía no existe y stdout sigue siendo
// la terminal.
let _ = norte_tui::kitty_graphics::consultar_soporte();
```

- [ ] **Paso 8: commit**

```bash
git add crates/norte-tui/src/kitty_graphics.rs crates/norte-tui/src/lib.rs crates/norte-tui/src/main.rs
git commit -F <mensaje>
```

Mensaje: `feat(tui): preguntar al terminal si sabe pintar gráficos`

---

### Task 2 — `[ui] images`

**Ficheros:**
- Modificar: `crates/norte-config/src/schema.rs` (junto a `processes_panel`, :438)
- Modificar: `crates/norte-config/src/load.rs` (:1369, :1414, :2030)
- Modificar: `crates/norte-frontend/src/settings.rs` (:302, :455)
- Modificar: `crates/norte-i18n/i18n/en.ftl`, `crates/norte-i18n/i18n/es.ftl`
- Test: `crates/norte-config/src/load.rs` (módulo de tests, junto a :3336)

**Interfaces:**
- Consume: nada de T1 todavía.
- Produce: `pub enum Images { Auto, Kitty, Blocks, Off }` con
  `Default = Auto`, y `UiChrome::images(self) -> Images`.

- [ ] **Paso 1: el test, calcado del de `processes_panel`**

En el módulo de tests de `load.rs`:

```rust
#[test]
fn images_se_lee_y_se_valida() {
    let c = cargar_ui("images = \"kitty\"\n");
    assert_eq!(c.images(), Images::Kitty);
}

#[test]
fn images_ausente_es_auto() {
    let vacio = cargar_ui("");
    assert_eq!(vacio.images(), Images::Auto);
}

#[test]
fn images_invalido_se_rechaza_con_motivo() {
    // Un valor que no es de la lista NO se ignora en silencio: quien
    // escribió "si" quería algo, y arrancar como si no hubiera escrito nada
    // convierte su error en una preferencia que no eligió.
    let err = cargar_ui_err("images = \"si\"");
    assert!(err.contains("images"), "el motivo nombra la clave: {err}");
}
```

Usa los mismos ayudantes que los tests de `processes_panel` de alrededor
(:3336–:3381); si se llaman de otra forma, cópiales el nombre.

- [ ] **Paso 2: correr y ver fallar**

Ejecuta: `just t norte-config`
Esperado: FALLA — `Images` no existe.

- [ ] **Paso 3: el enum, el campo y la validación**

`schema.rs`, junto a `processes_panel`:

```rust
/// `[ui] images`: how the TUI shows an image file in the viewer. Absent =
/// `auto`.
///
/// `auto` uses the terminal's graphics protocol when it has one and falls
/// back to an approved `previewer` plugin otherwise; `kitty` and `blocks`
/// force one of the two; `off` leaves the viewer on hexview. The GUI
/// ignores this key: a window paints images by itself.
#[serde(default)]
pub images: Option<String>,
```

`load.rs`:

```rust
/// `[ui] images`, validado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Images {
    /// El protocolo del terminal si lo hay; si no, el previewer.
    #[default]
    Auto,
    /// El protocolo del terminal, aunque la sonda dijera que no.
    Kitty,
    /// Medios bloques por el previewer, aunque el terminal supiera más.
    Blocks,
    /// Ni uno ni otro: el visor se queda en hexview.
    Off,
}
```

Y la validación, con la misma forma que la de `processes_panel` (:2030):

```rust
if let Some(raw) = &ui.images {
    acc.images = Some(match raw.as_str() {
        "auto" => Images::Auto,
        "kitty" => Images::Kitty,
        "blocks" => Images::Blocks,
        "off" => Images::Off,
        _ => {
            return Err(invalido(
                "[ui] images inválido: sólo se admite «auto», «kitty», «blocks» u «off»",
            ));
        }
    });
}
```

(`invalido(...)` es el ayudante que ya usa `processes_panel`; cópiale la
forma exacta a la línea 2036.)

- [ ] **Paso 4: correr y ver pasar**

Ejecuta: `just t norte-config`
Esperado: PASA.

- [ ] **Paso 5: la pantalla de ajustes y los dos idiomas**

`settings.rs`, junto a `ui.processes-panel`:

```rust
SettingDef {
    id: "ui.images",
    section: Section::General,
    kind: SettingKind::Enum(&["auto", "kitty", "blocks", "off"]),
    applies_live: true,
},
```

y su lectura (junto a :455):

```rust
"ui.images" => cfg.common.ui_chrome.images().as_str().to_owned(),
```

`en.ftl`:

```
setting-ui-images-name = Images in the viewer
setting-ui-images-desc = auto paints an image with the terminal's own graphics when it has them, and falls back to coloured half-blocks from an approved image previewer otherwise; kitty and blocks force one of the two; off leaves the viewer showing the bytes. The window ignores this key.
```

`es.ftl`:

```
setting-ui-images-name = Imágenes en el visor
setting-ui-images-desc = «auto» pinta la imagen con los gráficos del propio terminal cuando los tiene, y si no cae a medios bloques de un previewer de imagen aprobado; «kitty» y «blocks» fuerzan uno de los dos; «off» deja el visor enseñando los bytes. La ventana ignora esta clave.
```

- [ ] **Paso 6: regenerar el schema de configuración**

Ejecuta: `NORTE_UPDATE_SCHEMA=1 cargo test -p norte-config`
(si esa no es la variable de esta crate, mírala en el test que compara
`docs/schema/norte.schema.json` y usa la que diga).
Comprueba con `git diff docs/schema/norte.schema.json` que el único cambio
es la clave nueva.

- [ ] **Paso 7: commit**

```bash
git add crates/norte-config crates/norte-frontend/src/settings.rs crates/norte-i18n/i18n docs/schema/norte.schema.json
git commit -F <mensaje>
```

Mensaje: `feat(config): [ui] images elige cómo se ve una imagen en la TUI`

---

### Task 3 — El visor pide la miniatura

**Ficheros:**
- Modificar: `crates/norte-tui/src/viewer_open.rs:82` (`viewer_for_width`)
- Modificar: `crates/norte-tui/src/app/mod.rs` (campo nuevo en `App`)
- Test: `crates/norte-tui/tests/viewer_imagen.rs` (nuevo)

**Interfaces:**
- Consume: `Images` (T2), `kitty_graphics::soportado` (T1).
- Produce:
  ```rust
  pub struct ImagenColocada {
      pub path: VPath,
      pub bytes: Vec<u8>,
      pub width: u32,
      pub height: u32,
      pub id: u32,
      pub puesta_en: Option<ratatui::layout::Rect>,
  }
  ```
  y `pub fn modo_efectivo(cfg: Images, soporta: bool) -> Modo`, con
  `pub enum Modo { Kitty, Bloques, Nada }`.

- [ ] **Paso 1: el test de la decisión, que es lo que se puede probar**

`crates/norte-tui/tests/viewer_imagen.rs`:

```rust
use norte_config::Images;
use norte_tui::viewer_open::{Modo, modo_efectivo};

#[test]
fn auto_usa_kitty_solo_si_el_terminal_sabe() {
    assert_eq!(modo_efectivo(Images::Auto, true), Modo::Kitty);
    assert_eq!(modo_efectivo(Images::Auto, false), Modo::Bloques);
}

#[test]
fn kitty_forzado_manda_aunque_la_sonda_dijera_que_no() {
    // La sonda puede equivocarse —un multiplexor con passthrough, un
    // terminal que no contesta pero sabe— y forzar es para eso. Si de
    // verdad no sabe, lo que se ve es basura en pantalla, y por eso no es
    // el valor por defecto.
    assert_eq!(modo_efectivo(Images::Kitty, false), Modo::Kitty);
}

#[test]
fn blocks_no_usa_kitty_aunque_el_terminal_sepa() {
    assert_eq!(modo_efectivo(Images::Blocks, true), Modo::Bloques);
}

#[test]
fn off_no_pinta_nada_y_deja_el_visor_como_estaba() {
    assert_eq!(modo_efectivo(Images::Off, true), Modo::Nada);
}
```

- [ ] **Paso 2: correr y ver fallar**

Ejecuta: `just t norte-tui`
Esperado: FALLA — `modo_efectivo` no existe.

- [ ] **Paso 3: la decisión**

En `viewer_open.rs`:

```rust
/// Cómo se va a enseñar esta imagen, ya resueltos la clave y el terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modo {
    /// Píxeles por el protocolo del terminal.
    Kitty,
    /// Medios bloques, que los pone un previewer aprobado.
    Bloques,
    /// Nada: el visor se queda con los bytes.
    Nada,
}

/// Resuelve `[ui] images` contra lo que contestó la sonda.
///
/// `Bloques` NO es una rama que haga nada: es «no hagas nada especial», y
/// el previewer de imagen —si está aprobado y activado— ya pinta. Por eso
/// `Bloques` y `Nada` se parecen tanto aquí y se distinguen en la ayuda:
/// con `off` el lector pidió hexview; con `blocks` pidió medios bloques y
/// lo que falta es aprobar el plugin.
#[must_use]
pub fn modo_efectivo(cfg: norte_config::Images, soporta: bool) -> Modo {
    match cfg {
        norte_config::Images::Off => Modo::Nada,
        norte_config::Images::Blocks => Modo::Bloques,
        norte_config::Images::Kitty => Modo::Kitty,
        norte_config::Images::Auto if soporta => Modo::Kitty,
        norte_config::Images::Auto => Modo::Bloques,
    }
}
```

- [ ] **Paso 4: correr y ver pasar**

Ejecuta: `just t norte-tui`
Esperado: PASA.

- [ ] **Paso 5: pedir los bytes cuando toca**

En `viewer_for_width`, DESPUÉS de construir el `viewer` y sólo si
`viewer.is_image()` y el modo es `Modo::Kitty`:

```rust
// El lado mayor en PÍXELES que cabe en el hueco. Una celda de terminal es
// aproximadamente 8x16 px y no hay forma portable de preguntarlo, así que
// se estima: pasarse sólo cuesta que el terminal la encoja, quedarse corto
// se ve borroso.
let max_edge = columns.unwrap_or(80).saturating_mul(8).clamp(64, 1920);
let thumb = backend.plugin_thumbnail(path, max_edge).await.ok().flatten();
```

Un `Err` o un `None` NO es un fallo: cae a lo de siempre, que es el
contrato del visor. La miniatura se guarda en `App.viewer_imagen`, no en
`Viewer`: `Viewer` es de `norte-frontend` y lo comparten los dos frontends,
y la ventana ya tiene su propio camino a las miniaturas.

- [ ] **Paso 6: correr la crate entera**

Ejecuta: `just t norte-tui`
Esperado: PASA, sin tocar ningún test que ya existía.

- [ ] **Paso 7: commit**

```bash
git add crates/norte-tui
git commit -F <mensaje>
```

Mensaje: `feat(tui): el visor pide la miniatura cuando va a pintar píxeles`

---

### Task 4 — Colocar y borrar

**Ficheros:**
- Modificar: `crates/norte-tui/src/ui/geometry.rs` (`rect_del_visor`)
- Modificar: `crates/norte-tui/src/ui/panels.rs:122` (dejar el hueco vacío)
- Modificar: `crates/norte-tui/src/event_loop.rs:384` (tras el frame)
- Modificar: `crates/norte-tui/src/kitty_graphics.rs` (los escapes)
- Test: `crates/norte-tui/tests/viewer_imagen.rs` (ampliar)

**Interfaces:**
- Consume: `ImagenColocada` (T3), `rect_del_visor`.
- Produce: `pub fn escape_colocar(id: u32, bytes: &[u8], rect: Rect) -> String`
  y `pub fn escape_borrar(id: u32) -> String`.

- [ ] **Paso 1: el test de los escapes**

```rust
use norte_tui::kitty_graphics::{escape_borrar, escape_colocar};
use ratatui::layout::Rect;

#[test]
fn colocar_lleva_el_id_el_tamano_y_base64() {
    let esc = escape_colocar(7, b"PNGFALSO", Rect::new(1, 2, 40, 20));
    assert!(esc.starts_with("\x1b_G"), "empieza por APC: {esc}");
    assert!(esc.contains("i=7"), "lleva el id: {esc}");
    assert!(esc.contains("f=100"), "PNG, que es lo que da el kind thumbnail");
    assert!(esc.contains("c=40") && esc.contains("r=20"), "el hueco: {esc}");
    assert!(esc.ends_with("\x1b\\"), "cierra el APC: {esc}");
    // Los bytes van en base64 y NO en crudo: un APC se termina con
    // `\x1b\\`, y un PNG contiene esa pareja de bytes con toda normalidad.
    assert!(esc.contains("UE5HRkFMU08"), "base64 del contenido: {esc}");
}

#[test]
fn un_contenido_grande_se_trocea() {
    // Una miniatura de verdad no cabe en un solo APC, así que hay que
    // trocearla: todos los trozos menos el último llevan `m=1` y el último
    // `m=0`. Sin este test, los de arriba pasan con un `escape_colocar` que
    // no sabe trocear — 8 bytes nunca llegan al tope.
    let grande = vec![0u8; 12 * 1024];
    let esc = escape_colocar(7, &grande, Rect::new(1, 2, 40, 20));
    let trozos: Vec<&str> = esc.split("\x1b_G").skip(1).collect();
    assert!(trozos.len() > 1, "una imagen grande va en varios trozos: {}", trozos.len());
    let (ultimo, previos) = trozos.split_last().expect("hay al menos uno");
    for t in previos {
        assert!(t.contains("m=1"), "un trozo que no es el último sigue: {t}");
    }
    assert!(ultimo.contains("m=0"), "el último cierra: {ultimo}");
}

#[test]
fn borrar_nombra_solo_ese_id() {
    // `d=i` borra POR ID. Sin el id se borrarían las imágenes de todo el
    // terminal, incluidas las de otro programa en otra pestaña.
    let esc = escape_borrar(7);
    assert!(esc.contains("a=d") && esc.contains("d=i") && esc.contains("i=7"), "{esc}");
}
```

- [ ] **Paso 2: correr y ver fallar**

Ejecuta: `just t norte-tui`
Esperado: FALLA — no existen.

- [ ] **Paso 3: escribirlos**

```rust
/// El escape que coloca la imagen en el hueco del visor.
///
/// `f=100` es PNG, que es lo que devuelve el kind `thumbnail`. Los bytes
/// van en base64 porque un APC termina en `\x1b\\` y un PNG contiene esa
/// pareja con toda normalidad: mandarlo crudo cortaría la imagen por la
/// mitad y dejaría el resto escrito en la pantalla como texto.
///
/// `c`/`r` son celdas, no píxeles: se le dice al terminal el HUECO y él
/// encaja, que es lo que mantiene la imagen dentro del marco cuando el
/// terminal tiene celdas de otro tamaño del que supusimos.
#[must_use]
pub fn escape_colocar(id: u32, bytes: &[u8], rect: ratatui::layout::Rect) -> String { … }

/// El escape que borra SÓLO esta imagen.
#[must_use]
pub fn escape_borrar(id: u32) -> String {
    format!("\x1b_Ga=d,d=i,i={id}\x1b\\")
}
```

Usa el base64 que ya esté en el árbol (`norte-proto` codifica así las
miniaturas del wire — mira `thumb_wire` en `methods.rs:8180`); no añadas
dependencia.

Si el PNG pasa de ~4 KiB hay que trocear el APC en trozos con `m=1`/`m=0`,
que es lo normal: escríbelo troceado desde el principio, porque una
miniatura de 1920 px de lado nunca cabe en un trozo.

- [ ] **Paso 4: correr y ver pasar**

Ejecuta: `just t norte-tui`
Esperado: PASA.

- [ ] **Paso 5: dejar el hueco y colocar**

En `panels.rs:130`, cuando hay imagen colocada, las `lines` son vacías: el
terminal va a pintar encima y cualquier texto ahí se vería DEBAJO o
parpadearía. El marco, el título y las barras siguen igual.

En `event_loop.rs`, justo después de `terminal.draw(...)` (:386) y antes de
`turn::after_frame`:

```rust
// Los píxeles van DESPUÉS del frame y por fuera de ratatui: un APC no
// cabe en una celda, y ratatui pinta celdas. Se borra lo de antes y se
// coloca lo de ahora en el mismo sitio, de una vez, para que no haya un
// frame con la imagen vieja sobre el marco nuevo.
```

El rect sale de `geometry::rect_del_visor(app, painted.area)` — la MISMA
función que usa `draw_viewer`, no una copia: dos cuentas del mismo hueco
divergen en silencio, que es exactamente lo que dice la memoria de
`funcion-compartida-no-basta`.

Se escribe con `terminal.backend_mut().writer()` (el backend es
`CrosstermBackend<TtyOut>`), y el error se traga con un `tracing::debug!`:
una imagen que no se pinta no tumba la TUI.

- [ ] **Paso 6: borrar cuando toca**

Hay que borrar en CUATRO momentos, y olvidar uno deja una imagen pegada en
la pantalla sobre cosas que no son el visor:

1. al cerrar el visor,
2. al mover el visor a otro fichero,
3. al suspender o ceder la terminal (junto a `alt_menu::ceder`, `suspend.rs`),
4. al salir (junto a `tty::restore`).

- [ ] **Paso 7: pilotar, que esto no lo ve un test**

```
tmux kill-session -t pimg 2>/dev/null
tmux new-session -d -s pimg -x 130 -y 40 \
  -e NORTE_CONFIG_DIR=<perfil-aislado>/norte \
  -e XDG_STATE_HOME=<perfil-aislado>/state \
  '<repo>/target/debug/ntc --no-splash <repo>/crates/norte-gui-tauri/icons'
```

F3 sobre `icon.png`, `Esc`, F3 sobre otro, `Esc`, suspender con `ctrl+z` y
volver. Comprueba que no queda ninguna imagen pegada.
**`tmux kill-session -t pimg` al terminar.**

- [ ] **Paso 8: commit**

```bash
git add crates/norte-tui
git commit -F <mensaje>
```

Mensaje: `feat(tui): la imagen se coloca tras el frame y se borra al irse`

- [ ] **Paso 9: el gate intermedio (UNO)**

Ejecuta: `just ci-fast > /tmp/cifast.log 2>&1; echo $status`
Lee el CÓDIGO, no la última línea: en fish no hay `$PIPESTATUS`, y leer un
cero que era del `echo` ya mandó a `main` un gate rojo en este proyecto.

---

### Task 5 — Que se note que falta aprobar el plugin

**Ficheros:**
- Modificar: `crates/norte-tui/src/viewer_open.rs`
- Modificar: `crates/norte-i18n/i18n/{en,es}.ftl`
- Test: `crates/norte-tui/tests/viewer_imagen.rs`

El piloto encontró el agujero de usabilidad de verdad: **sin previewer
aprobado, un PNG se ve como hexview y nada dice por qué.** Alguien que pone
`images = "blocks"` y no ha aprobado `image-ansi` ve exactamente lo mismo
que antes, sin pista.

- [ ] **Paso 1: el test**

```rust
#[test]
fn en_bloques_sin_previewer_el_visor_lo_dice() {
    // Un hexview silencioso es indistinguible de «norte no sabe hacerlo».
    let aviso = norte_tui::viewer_open::aviso_de_imagen(Modo::Bloques, false);
    assert!(aviso.is_some(), "hay que decir que falta aprobar el plugin");
}

#[test]
fn con_previewer_no_se_avisa_de_nada() {
    assert!(norte_tui::viewer_open::aviso_de_imagen(Modo::Bloques, true).is_none());
}

#[test]
fn en_off_no_se_avisa_porque_lo_pidio_el_lector() {
    assert!(norte_tui::viewer_open::aviso_de_imagen(Modo::Nada, false).is_none());
}
```

- [ ] **Paso 2: correr y ver fallar**

Ejecuta: `just t norte-tui`

- [ ] **Paso 3: el aviso**

`aviso_de_imagen` devuelve `Some(t("viewer-image-needs-previewer"))` sólo en
`Modo::Bloques` sin previewer. Va en la barra del visor, con rol `Info`,
donde ya va el «via …».

`en.ftl`: `viewer-image-needs-previewer = an image previewer extension is not approved yet — F12 to approve one`
`es.ftl`: `viewer-image-needs-previewer = no hay ninguna extensión de vista previa de imagen aprobada — F12 para aprobar una`

- [ ] **Paso 4: correr y ver pasar**

Ejecuta: `just t norte-tui`

- [ ] **Paso 5: commit**

Mensaje: `feat(tui): decir que falta aprobar el previewer en vez de callar`

---

### Task 6 — La ayuda

**Ficheros:**
- Modificar: `crates/norte-help/topics/en/viewer.md`
- Modificar: `crates/norte-help/topics/es/viewer.md`

- [ ] **Paso 1: escribir la sección**

En los dos idiomas, y que diga las tres cosas que el piloto tuvo que
descubrir a mano:

- `[ui] images` y sus cuatro valores;
- que los medios bloques necesitan una extensión `previewer` **aprobada y
  activada**, en ese orden, y que se hace con F12;
- que dentro de tmux los píxeles no pasan sin `allow-passthrough`, y que
  por eso ahí se ven medios bloques.

- [ ] **Paso 2: el corpus de ayuda**

Un tema que crece no cambia `DOCUMENTED`, pero un COMANDO nuevo sí — y esta
fase no añade ninguno. Si el gate de ayuda se queja, el motivo es otro:
léelo antes de tocar el array.

Ejecuta: `just t norte-help`

- [ ] **Paso 3: commit**

Mensaje: `docs(help): cómo se ve una imagen en la TUI`

---

### Task 7 — ADR, changelog, memoria y cierre

**Ficheros:**
- Crear: `docs/adr/0118-imagenes-en-la-tui.md`
- Modificar: `CHANGELOG.md`
- Crear: memoria en el directorio de memorias

- [ ] **Paso 1: la ADR**

Que conteste lo que un lector futuro preguntará:

- **Por qué el kind `thumbnail` y no un previewer nuevo:** un plugin sirve a
  las dos superficies, y la ventana ya lo usa. Un kind nuevo por frontend es
  la divergencia que el ADR 0077 existe para evitar.
- **Por qué los píxeles se escriben FUERA de ratatui:** un APC no cabe en
  una celda. De ahí que el rect salga de una función compartida y que el
  borrado tenga cuatro llamadores.
- **Por qué sixel queda fuera:** codificarlo pide un cuantizador y no hay
  caso real (lo dice ya la spec).
- **Qué quedó de T1:** sonda de verdad o respaldo por entorno, y por qué.
- **Por qué `blocks` no construye nada:** ya existía; esta fase lo que
  añadió fue decir que hace falta aprobar el plugin.

- [ ] **Paso 2: changelog**

- [ ] **Paso 3: memoria**

Una memoria de tipo `project` con lo que no está en el código: que los medios
bloques ya funcionaban antes de esta fase, que aprobar es `y` → `y` → `e` en
ese orden, y que `plugins-state.toml` no se puede escribir a mano porque el
ancla es un sha256 sobre los digests. Enlázala con
[[plugins-estado-2026-09]] y [[tui-harness-tmux]], y apúntala en
`MEMORY.md`.

- [ ] **Paso 4: el gate de cierre (UNO)**

Ejecuta: `just ci > /tmp/ci.log 2>&1; echo $status`
Esperado: `0`. Lee el código, nunca tras una tubería.

- [ ] **Paso 5: commit y fusión**

Mensaje: `docs: la fase 5 cierra — la TUI enseña imágenes`

---

## Autorrevisión del plan

**Cobertura de la spec**, punto por punto:

| la spec pide | dónde |
| --- | --- |
| sonda APC `a=q` + DA1 al arrancar | T1 |
| `[ui] images = auto/kitty/blocks/off` | T2 |
| fuente: el kind `thumbnail` (PNG), un plugin para las dos superficies | T3 |
| kitty `f=100`, placement por id en el rect, borrado al repintar | T4 |
| sin kitty, el previewer `image-ansi` | T3 (`Modo::Bloques`) + T5, y ya funcionaba |
| sixel fuera | T7, en la ADR |

**Huecos que asumo a propósito, y por qué:**

- **T1 puede no salir.** Es la única incertidumbre técnica real y por eso es
  una puerta con respaldo escrito, no un paso que se da por hecho.
- **La estimación de 8 px por celda es una suposición.** No hay forma
  portable de preguntarlo; se acota (`clamp(64, 1920)`) y el terminal encaja
  por celdas (`c`/`r`), que es lo que absorbe el error.
- **El troceado del APC** (paso 4.3) es donde esto se rompería con una
  imagen grande. Va escrito desde el principio, no como optimización.
