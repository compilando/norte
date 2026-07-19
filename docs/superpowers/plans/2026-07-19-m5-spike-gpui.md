# Spike GPUI (M5 hito 1) — plan de implementación

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** un binario GPUI (`crates/norte-gui`, EXCLUIDO del workspace) que pinta un dir real del daemon con los colores de norte-theme, a la vez que la TUI contra el mismo socket, + ADR 0027 con la medición y la decisión go/no-go.

**Architecture:** crate binario excluido del workspace default (como `examples-wasm`); habla SOLO `norte-proto` vía `RemoteBackend` (regla 7); read-only (`fs.list`). GPUI de API desconocida a priori — **este spike la descubre**: cada task que toca GPUI trata un fallo de build/API como DATO del criterio 4, no como bloqueo. Spec: `docs/superpowers/specs/2026-07-19-m5-spike-gpui-design.md`.

**Tech Stack:** GPUI (Zed; versión/pin a decidir en T1), norte-core `RemoteBackend`, norte-theme, norte-proto.

**Naturaleza especial (léelo):** es un SPIKE, no producción. La MEDICIÓN es el entregable. TDD formal SOLO en la pieza reutilizable (T3, mapeo de color). El resto es exploración con puntos de decisión. Firmas de norte-* son EXACTAS (verificadas); el código GPUI es CONTRATO + «ajusta contra la doc de la versión pineada» — la API real la descubre el implementador y la REGISTRA (es parte del criterio 4). Convenciones vivas: español en comentarios; commits convencionales `feat(gui):`/`docs(gui):`; el crate NO entra en `just ci` (excluido), se verifica con `cargo build -p norte-gui` desde su dir.

**Datos verificados (úsalos literales):**
- `RemoteBackend::connect(socket: PathBuf, spawn_cmd: Option<Vec<std::ffi::OsString>>, client_info: norte_proto::methods::ClientInfo) -> Result<Self, Error>` (backend.rs:926). La TUI pasa `spawn_cmd = Some(...)` para autoarrancar el daemon; el spike puede pasar `None` (exige daemon ya corriendo) — más simple, coherente con «va siempre por daemon».
- `Backend::Remote(RemoteBackend)` envuelve; `Backend::list(&VPath) -> Result<Vec<Entry>, Error>` (backend.rs:174). O usa `RemoteBackend` directo si `Backend` estorba.
- `ClientInfo { name: String, version: String }`.
- `Entry` (norte-proto): `path: VPath`, `kind: EntryKind{File,Dir,Symlink,...}`, `size: Option<u64>`, `mtime_ms: Option<...>` (verifica los campos exactos en `norte-proto/src/entry.rs`).
- `VPath::file_name() -> Option<&Segment>`; `Segment::as_bytes()`. Display lossy: replica el criterio de `norte-tui::app::display_name(bytes) -> (String, bool)` (mask de controles/bidi; el spike puede copiar una versión mínima o depender de `norte-encoding::mask_terminal_hazards` + `String::from_utf8_lossy` — el segundo es mejor, ya es la fuente única #80/liveSearch).
- `norte_theme::Color::resolve(ColorDepth::Truecolor) -> ResolvedColor::Rgb(r: u8, g: u8, b: u8)` (color.rs:107).
- `norte_theme::{Role, FileKind, FileColors, Theme}`; `FileColors::style_for(name: &[u8], kind: FileKind) -> Option<Style>`; `Theme::preset_default()`. `Style` tiene `fg`/`bg: Color` (verifica en `norte-theme/src/style.rs`).
- `EntryKind` de proto ↔ `FileKind` de theme: mapea (Dir→FileKind::Dir, etc.; verifica los variantes reales de ambos — pueden no ser 1:1).

---

### Task 1: scaffold `norte-gui` excluido + dep GPUI + ¿compila? (PUNTO DE DECISIÓN)

**Files:**
- Create: `crates/norte-gui/Cargo.toml`, `crates/norte-gui/src/main.rs`, `crates/norte-gui/rust-toolchain.toml` (solo si GPUI exige nightly)
- Modify: `Cargo.toml` (workspace `exclude`)

- [ ] **Step 1: excluir del workspace** — en el `Cargo.toml` raíz, añade `"crates/norte-gui"` a `[workspace] exclude` (junto a `examples-wasm`). NO a `members`. Comentario: «norte-gui (M5 spike): EXCLUIDO — GPUI puede exigir nightly/deps GPU pesadas; jamás debe contaminar el gate del core (regla 7: solo habla norte-proto)».

- [ ] **Step 2: Cargo.toml del crate** — binario `norte-gui`. Deps: `norte-core` (por path, para `RemoteBackend`), `norte-proto`, `norte-theme` (por path), `tokio` (multi-thread, para el runtime del backend async), `anyhow`. GPUI: **investiga la forma de consumo correcta** — GPUI de Zed NO está en crates.io como `gpui` estable; suele consumirse como `gpui = { git = "https://github.com/zed-industries/zed", rev = "<pin>" }`. Elige un `rev` reciente y PÍNEALO (registra el rev + fecha en un comentario — es dato del criterio 4). Licencia: GPUI es Apache-2.0 (verifica) — anótalo (el crate excluido no pasa por `cargo deny`, pero el ADR lo registra).

- [ ] **Step 3: main.rs mínimo** — un `fn main()` que imprima «norte-gui spike» y salga (aún sin GPUI en el cuerpo). Objetivo de este step: que el crate RESUELVA las deps (incluido el árbol de GPUI) y compile el esqueleto.

- [ ] **Step 4: ¿compila? — EL PUNTO DE DECISIÓN.** Corre `cargo build -p norte-gui` (o `cargo build` dentro de `crates/norte-gui/` si el excluir lo saca del `-p` del raíz — prueba ambos y registra cuál funciona).
  - **Compila con la stable pineada (1.96.1):** perfecto, sigue a T2. Registra tiempo de build en frío.
  - **Exige nightly:** crea `crates/norte-gui/rust-toolchain.toml` con `channel = "nightly-YYYY-MM-DD"` (fecha que compile), documenta la fecha. Sigue a T2. (No bloquea: el crate está excluido, su toolchain es propia.)
  - **No compila con nada razonable (libs de sistema faltan, error irrecuperable):** NO improvises horas. Registra el error EXACTO, las libs que pide, y **escala al controller/oscar** — puede ser dato de NO-GO temprano (fricción de build en Linux = criterio 4). El spike puede terminar aquí con un ADR no-go si GPUI no arranca en el entorno.

- [ ] **Step 5: Commit** — `feat(gui): scaffold norte-gui excluido del workspace + dep GPUI pineada (M5 spike T1)`. Incluye en el mensaje: rev de GPUI, toolchain requerida, tiempo de build en frío.

---

### Task 2: ventana GPUI mínima (valida render en Linux)

**Files:** Modify `crates/norte-gui/src/main.rs`

- [ ] **Step 1: hello-window** — el cuerpo mínimo GPUI: `Application::new().run(...)` (o el equivalente de la versión pineada) que abre UNA ventana con un texto estático «norte-gui». **La API exacta la descubres contra la doc/ejemplos de GPUI del rev pineado** (mira `crates/gpui/examples/` en el repo de Zed del rev). Contrato: una ventana visible con texto; nada más.

- [ ] **Step 2: correr** — `cargo run -p norte-gui`. Verifica visualmente que la ventana abre y pinta. En un Linux headless (sin display) esto FALLA por falta de compositor/GPU: registra si aplica (dato del criterio 4 — «requiere display; en CI headless no corre»). Si hay display, mide cold start (tiempo desde `cargo run` ya compilado hasta ventana visible — cronométralo a mano, es un spike).

- [ ] **Step 3: Commit** — `feat(gui): ventana GPUI mínima (M5 spike T2)`. Registra: ¿abrió?, cold start aprox, libs de sistema que pidió al ejecutar.

---

### Task 3: mapeo `norte_theme::Color → gpui color` (CON TEST — pieza reutilizable)

**Files:** Create `crates/norte-gui/src/theme_map.rs` (+ `mod theme_map;` en main.rs)

Es la ÚNICA pieza con TDD: sobrevive al MVP, es determinista y no necesita GPU.

- [ ] **Step 1: test rojo** (mod tests en theme_map.rs):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use norte_theme::Color;

    #[test]
    fn color_a_rgba_gpui_conserva_los_bytes() {
        // Color::parse de un hex conocido → resolve Truecolor → rgba gpui.
        let c = Color::parse("#3b82f6").expect("hex válido");
        let g = to_gpui_rgba(c);
        // gpui::Rgba tiene r/g/b/a en f32 [0,1]; 0x3b=59, 0x82=130, 0xf6=246.
        assert!((g.r - 59.0 / 255.0).abs() < 1e-4, "r");
        assert!((g.g - 130.0 / 255.0).abs() < 1e-4, "g");
        assert!((g.b - 246.0 / 255.0).abs() < 1e-4, "b");
        assert!((g.a - 1.0).abs() < 1e-4, "alpha opaco");
    }
}
```
(Ajusta el TIPO de retorno y los nombres de campo al de la versión pineada de GPUI: puede ser `gpui::Rgba{r,g,b,a: f32}` o `gpui::Hsla`. Si es Hsla, el test compara tras convertir; documenta cuál usa el rev.)

- [ ] **Step 2: rojo** — `cargo test -p norte-gui theme_map` → FAIL (fn no existe).

- [ ] **Step 3: implementación**:
```rust
//! Puente norte-theme → color de GPUI. Determinista, testeable sin GPU;
//! sobrevive al MVP (M5 hito 2). Truecolor SIEMPRE (la GUI es la capa GPU
//! que MT reservó): jamás degrada a 256/16.
use norte_theme::{Color, ColorDepth, ResolvedColor};

/// `norte_theme::Color` → `gpui::Rgba` opaco (alpha 1.0).
pub fn to_gpui_rgba(c: Color) -> gpui::Rgba {
    let ResolvedColor::Rgb(r, g, b) = c.resolve(ColorDepth::Truecolor) else {
        // Truecolor SIEMPRE resuelve a Rgb; las otras ramas son inalcanzables.
        return gpui::Rgba { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };
    };
    gpui::Rgba {
        r: f32::from(r) / 255.0,
        g: f32::from(g) / 255.0,
        b: f32::from(b) / 255.0,
        a: 1.0,
    }
}
```
(Si GPUI usa Hsla como tipo primario, `to_gpui_rgba` devuelve el tipo que consuma el render; el test se adapta. El contrato: bytes conservados, opaco.)

- [ ] **Step 4: verde** — `cargo test -p norte-gui theme_map` → PASS.
- [ ] **Step 5: Commit** — `feat(gui): mapeo norte-theme→gpui con test (M5 spike T3)`

---

### Task 4: conectar al daemon + pintar el listado (criterio 1)

**Files:** Modify `crates/norte-gui/src/main.rs`; Create `crates/norte-gui/src/backend_task.rs` (async: conectar+listar, fuera del hilo de render)

- [ ] **Step 1: cargar el listado** (`backend_task.rs`): función async que, sobre un runtime tokio, `RemoteBackend::connect(socket, None, ClientInfo{name:"norte-gui", version:env!("CARGO_PKG_VERSION")})` y `backend.list(&dir)`; devuelve `Vec<Entry>` (o el error tipado). El socket y el dir salen de args/env (`NORTE_SOCKET`, o el default del OS que use `norte daemon run` — busca cómo la TUI resuelve el socket por defecto: `grep -rn "default_socket\|socket_path" crates/norte-core/src/daemon crates/norte-tui`). Dir default: `file://` del cwd (usa `norte_vfs_local::vpath_from_native` como la TUI, o pásalo por env `NORTE_DIR`).
  - GPUI + tokio: GPUI trae su propio executor; el backend async corre en un runtime tokio aparte y el resultado cruza al hilo GPUI por un canal (`std::sync::mpsc` o el `AsyncApp` de GPUI). **Patrón exacto = descúbrelo contra la doc de GPUI del rev** (cómo hacer trabajo async y actualizar la UI). Contrato: el listado se obtiene sin bloquear el render.

- [ ] **Step 2: pintar** (main.rs): la ventana muestra una lista vertical de las entradas. Por entrada: `display_name(entry.path.file_name())` — nombre lossy saneado (usa `String::from_utf8_lossy` + `norte_encoding::mask_terminal_hazards`; los bytes no-UTF8 no rompen, regla 1) + un indicador de tipo (texto «/» para dir, nada para file, «@» para symlink — algo mínimo). Color por defecto del tema (el color por tipo llega en T5).
  - Estado inicial: «conectando…»; al llegar el `Vec<Entry>`, re-render con la lista; error (daemon caído) → texto de error en la ventana, NO panic.

- [ ] **Step 3: correr contra un daemon** — en una terminal: `cargo build -p norte-cli && ./target/debug/norte daemon run` (o el comando real — `grep -rn "daemon run" crates/norte-cli`). En otra: `cargo run -p norte-gui`. Verifica que la ventana pinta las entradas del dir real. Si el daemon no está: verifica el mensaje de error (no panic).

- [ ] **Step 4: Commit** — `feat(gui): listado real del daemon pintado en la ventana (M5 spike T4, criterio 1)`

---

### Task 5: norte-theme por tipo de archivo (criterio 3)

**Files:** Modify `crates/norte-gui/src/main.rs` (usa `theme_map` de T3)

- [ ] **Step 1: color por entrada** — al pintar cada entrada, resuelve su color: `Theme::preset_default()` → `theme.file_colors()` (o cómo exponga los FileColors — `grep -n "file_colors\|FileColors\|files:" crates/norte-theme/src/theme.rs`) → `style_for(name_bytes, file_kind)` donde `file_kind` mapea de `entry.kind` (EntryKind→FileKind; verifica los variantes). El `Style.fg` (Color) → `to_gpui_rgba` → color del texto de esa fila. Sin match → color de rol por defecto (`Role::Text` o similar del tema).
  - Documenta el mapeo EntryKind↔FileKind (pueden no ser 1:1: proto tiene Symlink, theme quizá lo trata distinto).

- [ ] **Step 2: correr** — la lista pinta con los colores por tipo (dirs de un color, ejecutables/imágenes/etc. de otro si el nombre casa una extensión conocida). Verifica visualmente que el theming cruzó a la GPU.

- [ ] **Step 3: Commit** — `feat(gui): color por tipo de archivo con norte-theme (M5 spike T5, criterio 3)`

---

### Task 6: GUI+TUI simultáneas (criterio 2) + doc de arranque

**Files:** Create `crates/norte-gui/README.md`

- [ ] **Step 1: verificar simultaneidad** — con un solo `norte daemon run`, arranca a la vez: (a) la TUI en modo daemon (`norte-tui --daemon` o el flag real — `grep -rn "\-\-daemon" crates/norte-tui/src/main.rs`), (b) `norte-gui`. Ambas contra el mismo socket, apuntando al mismo dir. Verifica que ambas listan lo mismo. (El modelo multi-conexión ya está probado con TUI+agente en M3; esto lo confirma con un frontend gráfico.)

- [ ] **Step 2: README** del crate: cómo arrancar el trío (daemon + tui + gui), env vars (`NORTE_SOCKET`/`NORTE_DIR`), toolchain requerida (de T1), y que es un SPIKE excluido del workspace (no `just ci`).

- [ ] **Step 3: Commit** — `docs(gui): README del spike + verificación GUI+TUI simultáneas (M5 spike T6, criterio 2)`

---

### Task 7: medición (criterio 4) + ADR 0027 go/no-go

**Files:** Create `docs/adr/0027-gui-gpui-go-no-go.md`; Modify la spec (estado + resultado)

- [ ] **Step 1: recolectar la medición** — corre y anota:
  - Arranque: cold start (primer run tras build) y warm start (segundo run), a mano, ~ms.
  - Binario: `cargo build -p norte-gui --release` → `ls -lh target/release/norte-gui` (MiB).
  - Deps: `cargo tree -p norte-gui | wc -l` (nº líneas ≈ crates) + los pesados (`cargo tree -p norte-gui | grep -iE "blade|wgpu|skia|naga|cosmic"`).
  - Toolchain: de T1 (stable 1.96.1 / nightly-fecha).
  - Build en frío: `cargo clean -p norte-gui && time cargo build -p norte-gui`.
  - API GPUI: rev pineado, fecha, ¿crates.io o git?, señales de inestabilidad (issues/breaking recientes — una búsqueda rápida, no exhaustiva).
  - Libs de sistema Linux requeridas (de T2/T4, lo que pidió al build/run).

- [ ] **Step 2: ADR 0027** (formato MADR como `docs/adr/0026-*.md`): título `# 0027 — GUI: GPUI go/no-go (M5 spike)`. Contexto (spec §18.3, este spike). Opciones (GPUI / Tauri). Los NÚMEROS de la medición en una tabla. **Decisión**: go o no-go, JUSTIFICADA con los números.
  - **go** → «GPUI confirmado; arranca el plan del MVP (M5 hito 2)»; nota el coste aceptado (toolchain, deps).
  - **no-go** → «pivote a Tauri»; qué número lo mató (build inviable, nightly inaceptable, binario/deps desproporcionados); el spike GPUI queda como referencia.

- [ ] **Step 3: cerrar la spec** — en `docs/superpowers/specs/2026-07-19-m5-spike-gpui-design.md`: estado → COMPLETO + una línea con el resultado (go/no-go) + link al ADR.

- [ ] **Step 4: Commit + push** — `docs(gui): medición del spike + ADR 0027 go/no-go — cierra M5 hito 1 (M5 spike T7)`. Push de todo el rango.

---

## Self-review del plan (hecho)

- Cobertura spec: crate excluido (T1), ventana GPUI (T2), mapeo theme con test (T3), listado real por daemon = criterio 1 (T4), norte-theme aplicado = criterio 3 (T5), GUI+TUI simultáneas = criterio 2 (T6), medición = criterio 4 + ADR go/no-go = entregable (T7). Read-only y errores-sin-panic anclados en T4. Fuera de alcance (dual-pane, mutación, keymap…) respetado: ninguna task los toca.
- Riesgo GPUI tratado como DATO, no bloqueo: T1 step 4 y T2 step 2 tienen ramas explícitas (compila stable / nightly / no-go temprano con escalada). El spike puede terminar en T7 con un ADR no-go sin haber completado T4-T6 si GPUI no arranca — documentado.
- Tipos consistentes: `to_gpui_rgba` (T3) usado en T5; `RemoteBackend::connect`/`list`/`ClientInfo`/`Entry`/`display_name`/`Color::resolve`/`FileColors::style_for` — firmas verificadas contra el código real. El código GPUI es contrato explícito «ajusta contra el rev» por ser API desconocida — decisión consciente de un spike, no placeholder.
- Sin placeholders de trabajo: los `grep` que el plan pide son para RESOLVER detalles verificables (socket default, flags, campos de Style), no huecos de diseño.
